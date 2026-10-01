//! Everything the desktop app talks to, in one place: the engine and the services around it, plus
//! the operations the old local web backend offered (setup and signing in, files, the search
//! index, Anki, checkpoints, notebooks), as async methods the UI calls directly.
//!
//! Errors come back as ApiErr: a status and a JSON body {"error": kind, "message": …} shaped like the
//! web backend's, so the UI can tell a missing permission from an expired session from the rest.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{Value, json};
use sha1::{Digest, Sha1};

use crate::anki::Anki;
use crate::client::{Client, host_of};
use crate::engine::{Engine, ResourceExt};
use crate::files::{file_path, warm_files};
use crate::notebooks::Notebooks;
use crate::recordings::Recordings;
use crate::signin::{self, Watcher};
use crate::store::Store;
use crate::util::s;
use crate::{Error, Result, anki_setup, checkpoints, config, cookies};

#[derive(Clone, Debug)]
pub struct ApiErr {
    pub status: u16,
    pub body: Value,
}

impl ApiErr {
    pub fn new(status: u16, kind: &str, message: impl Into<String>) -> ApiErr {
        ApiErr { status, body: json!({"error": kind, "message": message.into()}) }
    }
    pub fn kind(&self) -> &str {
        self.body["error"].as_str().unwrap_or("")
    }
    pub fn message(&self) -> String {
        self.body["message"].as_str().map(String::from).unwrap_or_else(|| format!("HTTP {}", self.status))
    }
}

impl std::fmt::Display for ApiErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

/// The web backend's _err(): permission 403, setup 409, session 401, anything else 502.
pub fn err(e: &Error) -> ApiErr {
    match e {
        Error::NeedsPermission(m) => ApiErr::new(403, "permission", m.clone()),
        Error::NotConfigured(m) => ApiErr::new(409, "setup", m.clone()),
        Error::SessionExpired(m) => ApiErr::new(401, "session", m.clone()),
        other => ApiErr::new(502, "fetch", other.to_string()),
    }
}

pub type ApiResult = std::result::Result<Value, ApiErr>;

struct Ext {
    recordings: Weak<Recordings>,
    notebooks: Weak<Notebooks>,
}

impl ResourceExt for Ext {
    fn ttl(&self, name: &str) -> Option<f64> {
        Some(match name {
            "recordings" => 30.0 * 60.0,
            "recording_info" => 24.0 * 60.0 * 60.0,
            "notebooks" => 2.0 * 60.0,
            "nb_links" => 0.0,
            _ => return None,
        })
    }

    fn fetch(&self, name: &str, args: Vec<String>) -> BoxFuture<'static, Result<Value>> {
        let (rec, nbs) = (self.recordings.upgrade(), self.notebooks.upgrade());
        let name = name.to_string();
        async move {
            let first = args.first().cloned().unwrap_or_default();
            match name.as_str() {
                "recordings" => rec.ok_or(Error::Other("shutting down".into()))?.recordings(&first).await,
                "recording_info" => rec.ok_or(Error::Other("shutting down".into()))?.client.delivery_info(&first).await,
                "notebooks" => nbs.ok_or(Error::Other("shutting down".into()))?.notebooks_resource().await,
                "nb_links" => Ok(Value::Array(nbs.ok_or(Error::Other("shutting down".into()))?.links())),
                _ => Err(Error::NotFound(format!("unknown resource {name}"))),
            }
        }
        .boxed()
    }
}

pub struct Services {
    pub engine: Arc<Engine>,
    pub recordings: Arc<Recordings>,
    pub anki: Arc<Anki>,
    pub notebooks: Arc<Notebooks>,
    pub watcher: Watcher,
    openers: Mutex<HashMap<String, Option<String>>>,
}

impl Services {
    /// The engine (demo or real, from CANVAS_DEMO) with every service around it.
    pub fn new() -> Result<Arc<Services>> {
        let engine = Engine::new(Store::open_default()?, Client::from_env());
        let demo = config::is_demo();
        engine.init_status(vec![
            ("base", json!(engine.client.base())),
            ("configured", json!(!engine.client.base().is_empty())),
            ("demo", json!(demo)),
            ("permissions", json!(config::permissions())),
            ("panopto_host", json!(config::panopto_host())),
            ("notebooks", json!(true)),
        ]);
        let recordings = Recordings::new(engine.clone());
        let notebooks = Notebooks::new(engine.clone(), recordings.clone());
        engine.set_ext(Arc::new(Ext { recordings: Arc::downgrade(&recordings), notebooks: Arc::downgrade(&notebooks) }));
        let warm = engine.clone();
        engine.add_after_sync(Arc::new(move || warm_files(warm.clone()).boxed()));
        // search: fetch what isn't cached yet (page bodies, transcripts), then index it all
        let (fe, fr) = (engine.clone(), recordings.clone());
        engine.add_after_sync(Arc::new(move || {
            let (e, r) = (fe.clone(), fr.clone());
            async move {
                crate::fulltext::warm(e.clone(), r).await;
                let n = tokio::task::spawn_blocking(move || crate::fulltext::refresh(&e)).await.unwrap_or(0);
                log::info!("search index: {n} documents updated");
            }
            .boxed()
        }));
        let anki = Anki::new(engine.clone());
        Ok(Arc::new(Services { engine, recordings, anki, notebooks, watcher: Watcher::default(), openers: Mutex::new(HashMap::new()) }))
    }

    pub fn demo(&self) -> bool {
        config::is_demo()
    }

    // --- resources -----------------------------------------------------------------------------
    /// {"fetched_at", "data"} for a resource (the web backend's /r/<name>/<args>).
    pub async fn resource(&self, name: &str, args: &[String], force: bool) -> std::result::Result<(Value, f64), ApiErr> {
        match self.engine.get(name, args, force).await {
            Ok((text, at)) => Ok((serde_json::from_str(&text).map_err(|e| ApiErr::new(502, "fetch", e.to_string()))?, at)),
            Err(Error::NotFound(m)) if m == "unknown resource" => Err(ApiErr::new(404, "unknown resource", "unknown resource")),
            Err(e) => Err(err(&e)),
        }
    }

    // --- search index (built only from cache, never hits the network) ---------------------------
    pub fn search_index(&self) -> Vec<Value> {
        let e = &self.engine;
        let mut out = Vec::new();
        let active = e.cached_list("courses", &[]);
        let active_ids: Vec<String> = active.iter().map(|c| c["id"].to_string()).collect();
        let past: Vec<Value> = e.cached_list("past_courses", &[]).into_iter().filter(|c| !active_ids.contains(&c["id"].to_string())).collect();
        let n_active = active.len();
        let courses: Vec<Value> = active.into_iter().chain(past).collect();
        let names: HashMap<String, String> = courses
            .iter()
            .map(|c| (c["id"].to_string(), c["course_code"].as_str().filter(|x| !x.is_empty()).map(String::from).unwrap_or_else(|| s(&c["name"]))))
            .collect();
        for (i, c) in courses.iter().enumerate() {
            let cid = c["id"].to_string();
            let a = [cid.clone()];
            let term = if i < n_active { String::new() } else { format!("Past · {}", s(&c["term"]["name"])).trim_end_matches([' ', '·']).to_string() };
            let name = &names[&cid];
            out.push(json!({"t": s(&c["name"]), "k": "course", "c": term, "h": format!("#/c/{cid}")}));
            for g in e.cached_list("groups", &a) {
                for x in g["assignments"].as_array().into_iter().flatten() {
                    out.push(json!({"t": s(&x["name"]), "k": "assignment", "c": name, "h": format!("#/c/{cid}/a/{}", x["id"])}));
                }
            }
            for p in e.cached_list("pages", &a) {
                out.push(json!({"t": s(&p["title"]), "k": "page", "c": name, "h": format!("#/c/{cid}/p/{}", s(&p["url"]))}));
            }
            if let Some(files) = e.cached("files", &a) {
                for f in files["files"].as_array().into_iter().flatten() {
                    out.push(json!({"t": s(&f["display_name"]), "k": "file", "c": name, "h": format!("#/c/{cid}/f/{}", f["id"])}));
                }
            }
            for d in e.cached_list("discussions", &a) {
                out.push(json!({"t": s(&d["title"]), "k": "discussion", "c": name, "h": format!("#/c/{cid}/d/{}", d["id"])}));
            }
            for m in e.cached_list("modules", &a) {
                out.push(json!({"t": s(&m["name"]), "k": "module", "c": name, "h": format!("#/c/{cid}/modules#m{}", m["id"])}));
            }
        }
        for a in e.cached_list("announcements", &[]) {
            let cid = s(&a["context_code"]).split('_').nth(1).unwrap_or("").to_string();
            out.push(json!({"t": s(&a["title"]), "k": "announcement", "c": names.get(&cid).cloned().unwrap_or_default(), "h": format!("#/c/{cid}/d/{}", a["id"])}));
        }
        for b in crate::textbooks::list() {
            out.push(json!({"t": s(&b["title"]), "k": "textbook", "c": "Textbook", "h": format!("#/t/{}", s(&b["id"]))}));
        }
        for m in e.cached_list("inbox", &[]) {
            let subject = m["subject"].as_str().filter(|x| !x.is_empty()).unwrap_or("(no subject)");
            out.push(json!({"t": subject, "k": "message", "c": s(&m["context_name"]), "h": format!("#/inbox/{}", m["id"])}));
        }
        out
    }

    // --- full-text search -------------------------------------------------------------------------
    /// Sections of cached documents, transcripts and PDFs matching a query (fulltext::search).
    pub async fn search_content(&self, q: &str, limit: usize) -> Vec<Value> {
        let (e, q) = (self.engine.clone(), q.to_string());
        tokio::task::spawn_blocking(move || crate::fulltext::search(&e, &q, limit)).await.unwrap_or_default()
    }

    /// Bring the search index up to date with the cache now.
    pub async fn refresh_search(&self) {
        let e = self.engine.clone();
        let _ = tokio::task::spawn_blocking(move || crate::fulltext::refresh(&e)).await;
    }

    /// A recording's transcript {title, text}: saved after the first fetch (captions don't change).
    pub async fn transcript(&self, cid: &str, rid: &str) -> ApiResult {
        let key = format!("transcript:{cid}:{rid}");
        if let Some((v, _)) = self.engine.store.get(&key) {
            if let Ok(v) = serde_json::from_str(&v) {
                return Ok(v);
            }
        }
        let label = self.engine.cached_list("courses", &[]).into_iter().chain(self.engine.cached_list("past_courses", &[])).find(|c| c["id"].to_string() == cid).map(|c| s(&c["course_code"])).unwrap_or_default();
        let (title, text) = self.recordings.transcript(rid, &label).await.map_err(|e| err(&e))?;
        let v = json!({"title": title, "text": text});
        self.engine.store.put(&key, &v.to_string());
        let e = self.engine.clone();
        tokio::task::spawn_blocking(move || crate::fulltext::refresh(&e));
        Ok(v)
    }

    /// Downloaded PDFs whose text isn't in the search index yet: {fid, cid, name, course, path, tag}.
    pub fn pdfs_to_index(&self) -> Vec<Value> {
        let e = &self.engine;
        let mut out = Vec::new();
        for c in e.cached_list("courses", &[]).into_iter().chain(e.cached_list("past_courses", &[])) {
            let cid = c["id"].to_string();
            let course = c["course_code"].as_str().filter(|x| !x.is_empty()).map(String::from).unwrap_or_else(|| s(&c["name"]));
            let Some(files) = e.cached("files", &[cid.clone()]) else { continue };
            for f in files["files"].as_array().into_iter().flatten() {
                if f["content-type"] != "application/pdf" {
                    continue;
                }
                let fid = f["id"].to_string();
                let path = crate::files::blob_path(&fid, f);
                let tag = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                if path.exists() && !crate::fulltext::has(e, &format!("pdf:{fid}"), &tag) {
                    out.push(json!({"fid": fid, "href": format!("#/c/{cid}/f/{fid}"), "name": s(&f["display_name"]), "course": course, "path": path.to_string_lossy(), "tag": tag}));
                }
            }
        }
        for b in crate::textbooks::list() {
            let path = PathBuf::from(s(&b["path"]));
            let Ok(m) = std::fs::metadata(&path) else { continue };
            let stamp = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
            let tag = format!("{}-{stamp}", m.len());
            let fid = format!("tb-{}", s(&b["id"]));
            if !crate::fulltext::has(e, &format!("pdf:{fid}"), &tag) {
                out.push(json!({"fid": fid, "href": format!("#/t/{}", s(&b["id"])), "name": s(&b["title"]), "course": "Textbook", "path": path.to_string_lossy(), "tag": tag}));
            }
        }
        out
    }

    /// Put a PDF's page texts (extracted by the app) in the search index.
    pub fn index_pdf(&self, item: &Value, pages: Vec<String>) {
        crate::fulltext::put_pdf(&self.engine, &s(&item["fid"]), &s(&item["tag"]), &s(&item["name"]), &s(&item["course"]), &s(&item["href"]), pages);
    }

    // --- textbooks -----------------------------------------------------------------------------------
    pub fn textbooks(&self) -> Vec<Value> {
        crate::textbooks::list()
    }

    pub fn textbook(&self, id: &str) -> Option<Value> {
        crate::textbooks::get(id)
    }

    pub fn textbook_add(&self, path: &str) -> ApiResult {
        crate::textbooks::add(path).map_err(|e| ApiErr::new(400, "textbook", e.to_string()))
    }

    pub fn textbook_remove(&self, id: &str) -> ApiResult {
        crate::textbooks::remove(id).map_err(|e| ApiErr::new(500, "textbook", e.to_string()))?;
        // its text leaves the search index too
        let key = format!("pdf:tb-{id}");
        self.engine.store.exec("DELETE FROM fts WHERE key = ?", &[&key]);
        self.engine.store.exec("DELETE FROM fts_docs WHERE key = ?", &[&key]);
        Ok(json!({"ok": true}))
    }

    pub fn textbook_update(&self, id: &str, patch: &Value) {
        let _ = crate::textbooks::update(id, patch);
    }

    pub fn textbook_suggestions(&self) -> Vec<Value> {
        crate::textbooks::suggestions()
    }

    // --- files and images ------------------------------------------------------------------------
    /// A Canvas file on disk (downloaded on first use), with its metadata.
    pub async fn file(&self, fid: &str) -> std::result::Result<(PathBuf, Value), ApiErr> {
        file_path(&self.engine, fid, None).await.map_err(|e| err(&e))
    }

    pub async fn file_open(&self, fid: &str) -> ApiResult {
        let (path, _) = self.file(fid).await?;
        signin::open_default(&path.to_string_lossy());
        Ok(json!({"ok": true}))
    }

    pub fn open_external(&self, url: &str) {
        let scheme = url.split(':').next().unwrap_or("").to_lowercase();
        if scheme == "http" || scheme == "https" || scheme == "mailto" {
            signin::open_default(url);
        }
    }

    /// Name of the app that "Open" launches for a content type, e.g. "Document Viewer".
    pub async fn opener(&self, ctype: &str) -> Option<String> {
        if let Some(v) = self.openers.lock().unwrap().get(ctype) {
            return v.clone();
        }
        static MIME: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[\w.+-]+/[\w.+-]+$").unwrap());
        let mut name = None;
        #[cfg(all(unix, not(target_os = "macos")))]
        if MIME.is_match(ctype) {
            if let Ok(out) = tokio::process::Command::new("xdg-mime").args(["query", "default", ctype]).stderr(std::process::Stdio::null()).output().await {
                let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !id.is_empty() {
                    name = desktop_name(&id);
                }
            }
        }
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        let _ = &*MIME;
        self.openers.lock().unwrap().insert(ctype.to_string(), name.clone());
        name
    }

    /// Fetch an image or other embedded resource from Canvas with your session, cached on disk.
    /// Returns (bytes, content type).
    pub async fn proxy(&self, u: &str) -> std::result::Result<(Vec<u8>, String), ApiErr> {
        let base = self.engine.client.base();
        let url = url::Url::parse(&format!("{base}/")).and_then(|b| b.join(u)).map(|x| x.to_string()).unwrap_or_default();
        if host_of(&url) != self.engine.client.host() {
            return Err(ApiErr::new(400, "fetch", "not a Canvas URL"));
        }
        let digest = hex::encode(Sha1::digest(url.as_bytes()));
        let dir = config::blob_dir().join("proxy");
        let (path, ctype_path) = (dir.join(&digest), dir.join(format!("{digest}.type")));
        if !path.exists() {
            let resp = self.engine.client.download(&url).await.map_err(|e| err(&e))?;
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(&path, &resp.bytes);
            let _ = std::fs::write(&ctype_path, &resp.content_type);
            return Ok((resp.bytes, safe_media_type(&resp.content_type)));
        }
        let ctype = std::fs::read_to_string(&ctype_path).unwrap_or_else(|_| "application/octet-stream".into());
        Ok((std::fs::read(&path).map_err(|e| ApiErr::new(502, "fetch", e.to_string()))?, safe_media_type(&ctype)))
    }

    // --- Panopto --------------------------------------------------------------------------------
    /// Set (or with an empty link, clear) the Panopto folder for a course from a pasted folder link.
    pub async fn panopto_folder(&self, cid: &str, link: &str) -> ApiResult {
        let link = link.trim();
        if link.is_empty() {
            self.recordings.save_folder(cid, None, None);
        } else {
            static FOLDER: Lazy<Regex> = Lazy::new(|| Regex::new(r"folderID(?:=|%3D|%22%3A%22)(?:%22)?([0-9a-fA-F-]{36})").unwrap());
            static GUID: Lazy<Regex> = Lazy::new(|| Regex::new(r"([0-9a-fA-F]{8}-(?:[0-9a-fA-F]{4}-){3}[0-9a-fA-F]{12})").unwrap());
            let Some(fid) = FOLDER.captures(link).or_else(|| GUID.captures(link)).map(|m| m[1].to_string()) else {
                return Err(ApiErr::new(400, "folder", "That doesn't look like a Panopto folder link"));
            };
            let sessions = self.recordings.client.folder_sessions(&fid).await.map_err(|e| err(&e))?;
            let name = sessions.iter().find_map(|x| x["folder_name"].as_str().filter(|n| !n.is_empty()).map(String::from)).unwrap_or_else(|| "Panopto folder".into());
            self.recordings.save_folder(cid, Some(&fid), Some(&name));
        }
        self.engine.refresh_bg("recordings", &[cid.to_string()]);
        Ok(json!({"ok": true}))
    }

    // --- Gemini notebooks -------------------------------------------------------------------------
    pub async fn nb_create(&self, title: &str, course_id: Option<i64>, items: Vec<Value>) -> ApiResult {
        let title = if title.trim().is_empty() { "Untitled notebook" } else { title.trim() };
        self.notebooks.create(title, course_id, items).await.map_err(|e| ApiErr::new(502, "notebook", format!("Couldn't create the notebook: {e}")))
    }

    pub fn nb_add(&self, nid: &str, title: &str, items: Vec<Value>) -> Value {
        self.notebooks.start_job(nid, title, items)
    }

    pub fn nb_retry(&self, jid: &str) -> ApiResult {
        self.notebooks.retry(jid).map_err(|_| ApiErr::new(404, "unknown job", "unknown job"))
    }

    // --- setup and signing in ----------------------------------------------------------------------
    /// (permission, login URL) for a site, or None if it isn't set up.
    fn site(&self, name: &str) -> Option<(String, String)> {
        let c = &self.engine.client;
        match name {
            "canvas" if !c.base().is_empty() => Some((c.host(), format!("{}/login", c.base()))),
            "panopto" if !config::panopto_host().is_empty() => Some((config::panopto_host(), format!("https://{}/", config::panopto_host()))),
            "google" => Some(("google".into(), signin::GOOGLE_SIGNIN_URL.into())),
            "anki" => Some(("anki".into(), String::new())),
            "claude" => Some(("claude".into(), String::new())), // sending passages of PDFs to Anthropic
            _ => None,
        }
    }

    fn setup_changed(&self) {
        let c = &self.engine.client;
        self.engine.set_status(vec![("configured", json!(!c.base().is_empty())), ("base", json!(c.base())), ("permissions", json!(config::permissions()))]);
    }

    pub async fn setup_find(&self, address: &str) -> ApiResult {
        if signin::candidates(address).is_empty() {
            return Err(ApiErr::new(400, "invalid", "Type your school's Canvas address, like canvas.school.edu"));
        }
        match signin::find_canvas(address).await {
            Some(url) => Ok(json!({"url": url})),
            None => Err(ApiErr::new(404, "not_found", format!("Couldn't find Canvas at {}", address.trim()))),
        }
    }

    /// Use this Canvas (checked again here, so only a real Canvas site gets saved).
    pub async fn setup_canvas(&self, url: &str) -> ApiResult {
        let Some(url) = signin::find_canvas(url).await else { return Err(ApiErr::new(404, "not_found", "That isn't a Canvas site")) };
        if url != self.engine.client.base() {
            if !self.engine.client.base().is_empty() {
                self.engine.store.clear(); // another school: its cached courses don't belong to this one
            }
            let _ = config::save(vec![("canvas_url", json!(url))]);
            self.engine.client.set_base(&url);
            self.watcher.forget("canvas");
            self.setup_changed();
        }
        Ok(json!({"url": url}))
    }

    pub fn setup_allow(&self, site: &str) -> ApiResult {
        let Some((perm, _)) = self.site(site) else { return Err(ApiErr::new(400, "setup", format!("{site} isn't set up"))) };
        config::allow(&perm);
        self.watcher.forget(site);
        self.setup_changed();
        Ok(json!({"ok": true}))
    }

    pub async fn setup_revoke(&self, site: &str) -> ApiResult {
        let Some((perm, _)) = self.site(site) else { return Err(ApiErr::new(400, "setup", format!("{site} isn't set up"))) };
        config::revoke(&perm);
        self.watcher.forget(site);
        if site == "google" {
            let _ = std::fs::remove_file(config::data_dir().join("google_storage_state.json")); // left by the Python version
            self.notebooks.reset_client().await;
        } else if site == "canvas" {
            self.engine.client.set_base(&self.engine.client.base()); // drop the connection holding its cookies
            self.engine.set_status(vec![("session", json!("permission"))]);
        }
        self.setup_changed();
        Ok(json!({"ok": true}))
    }

    /// Open the site's login page (or Firefox's settings) in Firefox.
    pub async fn setup_open(&self, site: &str) -> ApiResult {
        let target = if site == "firefox_settings" { "about:preferences".to_string() } else { self.site(site).map(|x| x.1).unwrap_or_default() };
        if target.is_empty() {
            return Err(ApiErr::new(400, "setup", "Nothing to open"));
        }
        let ok = tokio::task::spawn_blocking(move || signin::open_in_firefox(&target)).await.unwrap_or(false);
        if !ok {
            return Err(ApiErr { status: 404, body: json!({"error": "no_firefox", "message": "Firefox isn't installed", "download": signin::FIREFOX_DOWNLOAD}) });
        }
        Ok(json!({"ok": true}))
    }

    async fn check_canvas(&self) -> Value {
        let host = self.engine.client.host();
        let state = match tokio::task::spawn_blocking(move || signin::canvas_cookie_state(&host)).await {
            Ok(Ok(s)) => s,
            _ => return json!({"state": "signed_out"}),
        };
        if !state["cookie"].as_bool().unwrap_or(false) {
            return json!({"state": "signed_out"});
        }
        let was = self.engine.status()["session"].clone();
        match self.engine.refresh("self", &[]).await {
            Err(e) if e.is_session() => return json!({"state": "signed_out"}), // an old login Canvas no longer accepts
            Err(e) => return json!({"state": "error", "message": format!("Couldn't reach Canvas ({})", e.type_name())}),
            Ok(()) => {}
        }
        if was != "ok" || self.engine.status()["last_sync"].is_null() {
            let e = self.engine.clone();
            tokio::spawn(async move { e.sync_once().await }); // just signed in: fetch everything now
        }
        let me = self.engine.cached("self", &[]).unwrap_or(json!({}));
        json!({"state": "signed_in", "fragile": state["fragile"], "user": {"name": me["name"], "short_name": me["short_name"], "avatar": me["avatar_url"]}})
    }

    async fn check_panopto(&self) -> Value {
        let host = config::panopto_host();
        let cookies = tokio::task::spawn_blocking(move || cookies::load_cookies(&host, None)).await.ok().and_then(|r| r.ok()).unwrap_or_default();
        json!({"state": if cookies.contains_key(".ASPXAUTH") { "signed_in" } else { "signed_out" }})
    }

    async fn check_google(&self) -> Value {
        let jar = tokio::task::spawn_blocking(signin::google_cookie_jar).await.ok().and_then(|r| r.ok()).flatten();
        let Some(jar) = jar else { return json!({"state": "signed_out"}) };
        let accounts = match crate::notebooklm::enumerate_accounts(&jar).await {
            Ok(a) => a,
            Err(e) => return json!({"state": "error", "message": format!("Couldn't reach Google ({})", e.type_name())}),
        };
        if accounts.is_empty() {
            return json!({"state": "signed_out"});
        }
        let want = config::google_authuser();
        let authuser = if accounts.iter().any(|a| a["authuser"].as_i64() == Some(want)) { want } else { 0 };
        json!({"state": "signed_in", "accounts": accounts, "authuser": authuser})
    }

    /// Is the site signed in, in Firefox? Polled by the setup screens while you log in.
    pub async fn setup_check(&self, site: &str) -> Value {
        let Some((perm, _)) = self.site(site) else { return json!({"state": "not_setup"}) };
        if site == "anki" {
            return json!({"state": "not_setup"});
        }
        if self.demo() {
            return json!({"state": "signed_in", "user": {"name": "Demo Student"}, "accounts": [{"authuser": 0, "email": "demo@example.edu"}], "authuser": 0});
        }
        if !config::allowed(&perm) {
            return json!({"state": "permission"});
        }
        let (hit, stamp) = match self.watcher.cached(site) {
            Ok(x) => x,
            Err(_) => return json!({"state": "no_firefox", "download": signin::FIREFOX_DOWNLOAD}),
        };
        if let Some(hit) = hit {
            return hit;
        }
        let result = match site {
            "canvas" => self.check_canvas().await,
            "panopto" => self.check_panopto().await,
            _ => self.check_google().await,
        };
        if result["state"] != "error" {
            self.watcher.remember(site, stamp, result.clone());
        }
        if site == "google" && result["state"] == "signed_in" {
            self.notebooks.reset_client().await; // pick up the new cookies
        }
        result
    }

    pub async fn setup_google_account(&self, authuser: i64) -> ApiResult {
        let _ = config::save(vec![("google_authuser", json!(authuser))]);
        self.notebooks.reset_client().await;
        self.watcher.forget("google");
        Ok(json!({"ok": true}))
    }

    // --- Anki (through the AnkiConnect add-on) ------------------------------------------------------
    /// Nothing talks to Anki (or looks at its add-ons) until you allow it.
    fn anki_allowed(&self) -> std::result::Result<(), ApiErr> {
        if !self.demo() && !config::allowed("anki") {
            return Err(ApiErr::new(403, "permission", "Allow the app to use Anki first."));
        }
        Ok(())
    }

    fn anki_err(e: Error) -> ApiErr {
        let e = match e {
            // Whatever answers there, it isn't Anki: set Anki up (which picks a free port for it).
            Error::AnkiPortTaken(_) if config::settings().anki_url.is_none() && !anki_setup::status()["ankiconnect"].as_bool().unwrap_or(false) => {
                Error::AnkiOffline("AnkiConnect isn't installed".into())
            }
            other => other,
        };
        match e {
            Error::AnkiPortTaken(url) => ApiErr {
                status: 409,
                body: json!({"error": "port_taken", "message": Error::AnkiPortTaken(url.clone()).to_string(), "url": url, "can_move": config::settings().anki_url.is_none()}),
            },
            Error::AnkiOffline(m) => ApiErr { status: 503, body: json!({"error": "offline", "message": m, "can_launch": anki_setup::launcher().is_some()}) },
            other => ApiErr::new(502, "anki", other.to_string()),
        }
    }

    pub async fn anki_decks(&self) -> ApiResult {
        self.anki_allowed()?;
        self.anki.decks().await.map(|d| json!({"decks": d})).map_err(Self::anki_err)
    }

    pub async fn anki_links(&self, links: Vec<(i64, Value)>) -> ApiResult {
        self.anki_allowed()?;
        self.anki.set_course_decks(links).await.map(|c| json!({"created": c})).map_err(Self::anki_err)
    }

    pub async fn anki_next(&self, did: i64) -> ApiResult {
        self.anki_allowed()?;
        self.anki.next_card(did).await.map_err(Self::anki_err)
    }

    pub async fn anki_answer(&self, did: i64, card: i64, ease: i64) -> ApiResult {
        self.anki_allowed()?;
        self.anki.answer(card, ease).await.map_err(Self::anki_err)?;
        self.anki.next_card(did).await.map_err(Self::anki_err)
    }

    pub async fn anki_media(&self, name: &str) -> std::result::Result<Option<(Vec<u8>, String)>, ApiErr> {
        self.anki_allowed()?;
        self.anki.media(name).await.map_err(Self::anki_err)
    }

    pub async fn anki_add(&self, did: i64) -> ApiResult {
        self.anki_allowed()?;
        self.anki.add_cards(did).await.map(|_| json!({"ok": true})).map_err(Self::anki_err)
    }

    pub async fn anki_sync(&self) -> ApiResult {
        self.anki_allowed()?;
        self.anki.sync().await.map(|_| json!({"ok": true})).map_err(Self::anki_err)
    }

    async fn anki_start(&self, tray: bool) -> ApiResult {
        if !anki_setup::launch(tray) {
            return Err(ApiErr::new(404, "anki", "Couldn't find Anki. Is it installed?"));
        }
        match self.anki.wait_online(30.0).await {
            Err(e) => Err(Self::anki_err(e)),
            Ok(false) => Err(ApiErr::new(503, "offline", "Anki started, but AnkiConnect isn't answering. Is the add-on installed?")),
            Ok(true) => Ok(json!({"ok": true})),
        }
    }

    pub async fn anki_launch(&self, tray: bool) -> ApiResult {
        self.anki_allowed()?;
        self.anki_start(tray).await
    }

    /// Launching Anki while it runs brings its window forward (the companion un-hides it).
    pub fn anki_show(&self) -> ApiResult {
        self.anki_allowed()?;
        if !anki_setup::launch(false) {
            return Err(ApiErr::new(404, "anki", "Couldn't find Anki. Is it installed?"));
        }
        Ok(json!({"ok": true}))
    }

    pub async fn anki_quit(&self) -> ApiResult {
        self.anki_allowed()?;
        self.anki.quit().await;
        // AnkiConnect closes it after ~1s; else SIGTERM
        if !tokio::task::spawn_blocking(|| anki_setup::stop(Duration::from_secs(20))).await.unwrap_or(false) {
            return Err(ApiErr::new(502, "anki", "Anki didn't quit"));
        }
        Ok(json!({"ok": true}))
    }

    /// Another program has AnkiConnect's port: move AnkiConnect to a free one and restart Anki there.
    /// Anki must be closed while its settings change (it rewrites them from memory when it quits).
    pub async fn anki_move_port(&self) -> ApiResult {
        self.anki_allowed()?;
        if config::settings().anki_url.is_some() {
            return Err(ApiErr::new(400, "anki", format!("anki_url is set in {}; change the port there.", config::config_path().display())));
        }
        if !anki_setup::anki_pids().is_empty() && !tokio::task::spawn_blocking(|| anki_setup::stop(Duration::from_secs(20))).await.unwrap_or(false) {
            return Err(ApiErr::new(502, "anki", "Anki didn't close. Quit Anki, then try again."));
        }
        let port = anki_setup::spare_port().and_then(|p| anki_setup::set_port(p, None).map(|_| p)).map_err(|e| ApiErr::new(502, "anki", format!("Couldn't change AnkiConnect's port: {e}")))?;
        self.anki_start(true).await?;
        Ok(json!({"port": port}))
    }

    pub async fn anki_setup_status(&self) -> ApiResult {
        self.anki_allowed()?;
        Ok(tokio::task::spawn_blocking(anki_setup::status).await.unwrap_or(json!({})))
    }

    /// Install the add-ons that are missing, then (re)start Anki in the tray so it loads them.
    pub async fn anki_setup_run(&self) -> ApiResult {
        self.anki_allowed()?;
        let installed = anki_setup::install().await.map_err(|e| ApiErr::new(502, "setup", format!("Couldn't install the Anki add-ons: {e}")))?;
        if !installed.is_empty() || anki_setup::anki_pids().is_empty() {
            if !anki_setup::anki_pids().is_empty() {
                self.anki.quit().await;
                if !tokio::task::spawn_blocking(|| anki_setup::stop(Duration::from_secs(20))).await.unwrap_or(false) {
                    return Err(ApiErr::new(502, "setup", "Installed, but Anki didn't close. Restart Anki to finish."));
                }
            }
            self.anki_start(true).await?;
        }
        Ok(json!({"installed": installed}))
    }

    // --- checkpoints -----------------------------------------------------------------------------
    pub async fn checkpoint_key_info(&self) -> Value {
        tokio::task::spawn_blocking(checkpoints::key_info).await.unwrap_or(json!({}))
    }

    pub async fn checkpoint_key_save(&self, key: &str) -> ApiResult {
        let key = key.trim();
        if key.is_empty() {
            return Err(ApiErr::new(400, "key", "Enter an API key."));
        }
        if let Some(e) = checkpoints::save_key(key).await {
            return Err(ApiErr::new(400, "key", e));
        }
        Ok(self.checkpoint_key_info().await)
    }

    pub async fn checkpoint_key_delete(&self) -> Value {
        let _ = tokio::task::spawn_blocking(checkpoints::delete_key).await;
        self.checkpoint_key_info().await
    }

    pub async fn checkpoint_anki(&self, course_id: Option<i64>, deck: &str, notes: Vec<Value>, media: Vec<Value>) -> ApiResult {
        self.anki_allowed()?;
        let deck = if deck.is_empty() { "Canvas checkpoints" } else { deck };
        self.anki.add_checkpoint_notes(course_id, deck, &notes, &media).await.map_err(Self::anki_err)
    }
}

/// Only images, audio and video keep their type; anything else is plain bytes.
fn safe_media_type(ctype: &str) -> String {
    if ctype.starts_with("image/") || ctype.starts_with("audio/") || ctype.starts_with("video/") { ctype.to_string() } else { "application/octet-stream".into() }
}

/// The Name= of a .desktop file (for "Open in <app>").
pub fn desktop_name(desktop_id: &str) -> Option<String> {
    let mut dirs = vec![std::env::var("XDG_DATA_HOME").ok().filter(|d| !d.is_empty()).map(PathBuf::from).unwrap_or_else(|| config::home().join(".local/share"))];
    dirs.extend(std::env::var("XDG_DATA_DIRS").ok().filter(|d| !d.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into()).split(':').map(PathBuf::from));
    static NAME: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^Name=(.+)$").unwrap());
    for d in dirs {
        let mut f = d.join("applications").join(desktop_id);
        if !f.exists() {
            f = d.join("applications").join(desktop_id.replacen('-', "/", 1)); // "foo-bar.desktop" may live at applications/foo/bar.desktop
        }
        let Ok(bytes) = std::fs::read(&f) else { continue };
        let text = String::from_utf8_lossy(&bytes);
        let entry = text.split("[Desktop Entry]").nth(1).unwrap_or(&text);
        let entry = entry.split("\n[").next().unwrap_or(entry);
        if let Some(m) = NAME.captures(entry) {
            return Some(m[1].trim().to_string());
        }
    }
    None
}
