//! Send Canvas items to Gemini Notebook (NotebookLM) notebooks and remember what went where.
//!
//! Google can't open Canvas URLs (they need your Canvas login), so pages, assignments and
//! discussions are uploaded as markdown text and files as file uploads. Only public links from
//! modules are added as URLs.
//!
//! Items are identified by keys shared with the UI:
//!   page:<course>:<slug>   assignment:<course>:<id>   discussion:<course>:<id>
//!   syllabus:<course>      file:<id>                  url:<url>
//!   recording:<course>:<panopto id>   (uploaded as its caption transcript)
//!
//! Each (notebook, item) upload is recorded with a fingerprint of what was sent, so re-adding an
//! unchanged item is skipped and a changed one replaces its old source.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use sha1::{Digest, Sha1};

use crate::cookies::GOOGLE_DOMAINS;
use crate::engine::{Engine, Event};
use crate::files::file_path;
use crate::markdown::md;
use crate::notebooklm::{Notebook, NotebookLm};
use crate::recordings::Recordings;
use crate::store::now;
use crate::util::s;
use crate::{Error, Result, config, cookies};

pub const UPLOAD_CONCURRENCY: usize = 3;
const WAIT_TIMEOUT: Duration = Duration::from_secs(300);
pub const FILE_TYPES: &[&str] = &[".pdf", ".txt", ".md", ".docx", ".pptx", ".csv", ".png", ".jpg", ".jpeg", ".webp", ".mp3", ".wav", ".m4a", ".ogg"];
pub const NOTEBOOK_URL: &str = "https://notebooklm.google.com/notebook/";

pub enum Kind {
    Text(String),
    File(PathBuf, Option<String>),
    Url(String),
}

pub struct Payload {
    pub kind: Kind,
    pub title: String,
    pub fingerprint: String,
}

fn fp(parts: &[&str]) -> String {
    hex::encode(Sha1::digest(parts.join("\x1f").as_bytes()))
}

fn suffix(name: &str) -> String {
    let file = name.rsplit(['/', '\\']).next().unwrap_or(name);
    match file.rfind('.') {
        Some(i) if i > 0 => file[i..].to_string(),
        _ => String::new(),
    }
}

// --- the fake NotebookLM, so demo mode never touches a real Google account --------------------------
pub struct DemoNotebookLm {
    books: Mutex<Vec<Notebook>>,
    n: Mutex<u32>,
}

impl DemoNotebookLm {
    fn new() -> DemoNotebookLm {
        DemoNotebookLm {
            books: Mutex::new(vec![Notebook { id: "demo-nb-1".into(), title: "CSE 373 midterm review".into(), sources_count: 4, created_at: Some(now()) }]),
            n: Mutex::new(0),
        }
    }
    async fn add(&self, nb: &str, title: &str, delay: u64) -> Result<String> {
        tokio::time::sleep(Duration::from_millis(delay)).await;
        if title.contains("zip") {
            return Err(Error::Invalid("unsupported file type".into()));
        }
        let mut n = self.n.lock().unwrap();
        *n += 1;
        if let Some(b) = self.books.lock().unwrap().iter_mut().find(|b| b.id == nb) {
            b.sources_count += 1;
        }
        Ok(format!("src-{n}"))
    }
}

enum Backend {
    Real(NotebookLm),
    Demo(DemoNotebookLm),
}

pub struct Notebooks {
    pub engine: Arc<Engine>,
    recordings: Arc<Recordings>,
    client: tokio::sync::Mutex<Option<Arc<Backend>>>,
    pub jobs: Mutex<HashMap<String, Arc<Mutex<Value>>>>,
    job_order: Mutex<Vec<String>>,
}

impl Notebooks {
    pub fn new(engine: Arc<Engine>, recordings: Arc<Recordings>) -> Arc<Notebooks> {
        engine.store.exec("CREATE TABLE IF NOT EXISTS nb_notebooks (id TEXT PRIMARY KEY, title TEXT, course_id INTEGER, created_at REAL, rule TEXT)", &[]);
        engine.store.exec(
            "CREATE TABLE IF NOT EXISTS nb_links (notebook_id TEXT, item_key TEXT, source_id TEXT, title TEXT, \
             fingerprint TEXT, status TEXT, error TEXT, updated_at REAL, PRIMARY KEY (notebook_id, item_key))",
            &[],
        );
        Arc::new(Notebooks { engine, recordings, client: tokio::sync::Mutex::new(None), jobs: Mutex::new(HashMap::new()), job_order: Mutex::new(Vec::new()) })
    }

    // --- NotebookLM client (Google session read from Firefox) ------------------------------------
    async fn client(&self) -> Result<Arc<Backend>> {
        let mut c = self.client.lock().await;
        if let Some(b) = c.as_ref() {
            return Ok(b.clone());
        }
        let backend = if config::is_demo() {
            Backend::Demo(DemoNotebookLm::new())
        } else {
            let records = tokio::task::spawn_blocking(|| cookies::load_cookie_records(GOOGLE_DOMAINS, None)).await.map_err(|e| Error::Other(e.to_string()))??;
            Backend::Real(NotebookLm::connect(&records, config::google_authuser()).await?)
        };
        let b = Arc::new(backend);
        *c = Some(b.clone());
        Ok(b)
    }

    pub async fn reset_client(&self) {
        *self.client.lock().await = None;
    }

    /// Run f(client); on a sign-in failure re-read Google cookies from Firefox and retry once.
    /// Only auth errors are retried: other failures may happen after Google already saved the
    /// source, and retrying those would add it twice.
    async fn call<T, F, Fut>(&self, f: F) -> Result<T>
    where
        F: Fn(Arc<Backend>) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        match f(self.client().await?).await {
            Err(Error::Auth(e)) => {
                log::info!("notebooklm sign-in failed ({e}); retrying with fresh cookies");
                self.reset_client().await;
                f(self.client().await?).await
            }
            other => other,
        }
    }

    pub async fn list(&self) -> Result<Vec<Notebook>> {
        self.call(|c| async move {
            match &*c {
                Backend::Real(n) => n.list().await,
                Backend::Demo(d) => {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    Ok(d.books.lock().unwrap().clone())
                }
            }
        })
        .await
    }

    // --- database ----------------------------------------------------------------------------------
    fn link(&self, nb: &str, key: &str) -> Option<(Option<String>, Option<String>, Option<String>)> {
        self.engine.store.with(|db| {
            db.query_row("SELECT source_id, fingerprint, status FROM nb_links WHERE notebook_id=? AND item_key=?", [nb, key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).ok()
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn save_link(&self, nb: &str, key: &str, source_id: Option<&str>, title: Option<&str>, fingerprint: Option<&str>, status: &str, error: Option<&str>) {
        self.engine.store.exec(
            "INSERT INTO nb_links (notebook_id, item_key, source_id, title, fingerprint, status, error, updated_at) \
             VALUES (?,?,?,?,?,?,?,?) ON CONFLICT(notebook_id, item_key) DO UPDATE SET \
             source_id=COALESCE(excluded.source_id, source_id), title=COALESCE(excluded.title, title), \
             fingerprint=COALESCE(excluded.fingerprint, fingerprint), status=excluded.status, error=excluded.error, \
             updated_at=excluded.updated_at",
            &[&nb, &key, &source_id, &title, &fingerprint, &status, &error, &now()],
        );
    }

    pub fn links(&self) -> Vec<Value> {
        self.engine.store.with(|db| {
            let mut stmt = db.prepare("SELECT notebook_id, item_key, source_id, title, status, error, updated_at FROM nb_links ORDER BY updated_at DESC").unwrap();
            stmt.query_map([], |r| {
                Ok(json!({
                    "notebook_id": r.get::<_, Option<String>>(0)?, "key": r.get::<_, Option<String>>(1)?, "source_id": r.get::<_, Option<String>>(2)?,
                    "title": r.get::<_, Option<String>>(3)?, "status": r.get::<_, Option<String>>(4)?, "error": r.get::<_, Option<String>>(5)?,
                    "updated_at": r.get::<_, Option<f64>>(6)?,
                }))
            })
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
        })
    }

    // --- Canvas item → notebook source -------------------------------------------------------------
    fn course(&self, cid: &str) -> Value {
        let e = &self.engine;
        e.cached_list("courses", &[]).into_iter().chain(e.cached_list("past_courses", &[])).find(|c| c["id"].to_string() == cid).unwrap_or(json!({"name": format!("Course {cid}")}))
    }

    fn header(&self, cid: &str, title: &str, url: &str) -> String {
        let c = self.course(cid);
        format!("# {title}\n\nCourse: {} {}\nCanvas: {url}\n\n", s(&c["course_code"]), s(&c["name"]))
    }

    fn label(&self, cid: &str, title: &str) -> String {
        let c = self.course(cid);
        let code = c["course_code"].as_str().filter(|x| !x.is_empty()).map(String::from).unwrap_or_else(|| s(&c["name"]));
        format!("{code} · {title}")
    }

    async fn get(&self, name: &str, args: &[&str]) -> Result<Value> {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        self.engine.get_json(name, &args).await
    }

    pub async fn resolve(&self, key: &str) -> Result<Payload> {
        let (kind, rest) = key.split_once(':').unwrap_or((key, ""));
        let base = self.engine.client.base();
        match kind {
            "page" => {
                let (cid, slug) = rest.split_once(':').ok_or_else(|| Error::Invalid(format!("unknown item {key}")))?;
                let p = self.get("page", &[cid, slug]).await?;
                let text = self.header(cid, &s(&p["title"]), &format!("{base}/courses/{cid}/pages/{slug}")) + &md(&s(&p["body"]));
                Ok(Payload { title: self.label(cid, &s(&p["title"])), fingerprint: fp(&[&text]), kind: Kind::Text(text) })
            }
            "assignment" => {
                let (cid, aid) = rest.split_once(':').ok_or_else(|| Error::Invalid(format!("unknown item {key}")))?;
                let groups = self.get("groups", &[cid]).await?;
                let a = groups.as_array().into_iter().flatten().flat_map(|g| g["assignments"].as_array().cloned().unwrap_or_default()).find(|x| x["id"].to_string() == aid);
                let a = a.ok_or_else(|| Error::NotFound("assignment not found (hidden or unpublished?)".into()))?;
                let mut lines = vec![
                    format!("Due: {}", a["due_at"].as_str().unwrap_or("no due date")),
                    format!("Points: {}", if a["points_possible"].is_null() { "None".into() } else { py_num(&a["points_possible"]) }),
                ];
                if let Some(t) = a["submission_types"].as_array().filter(|t| !t.is_empty()) {
                    lines.push(format!("Submission: {}", t.iter().map(s).collect::<Vec<_>>().join(", ")));
                }
                let mut text = self.header(cid, &s(&a["name"]), &s(&a["html_url"])) + &lines.join("\n") + "\n\n" + &md(&s(&a["description"]));
                if let Some(r) = a["rubric"].as_array().filter(|r| !r.is_empty()) {
                    text += "\n\n## Rubric\n\n";
                    text += &r
                        .iter()
                        .map(|c| {
                            let long = c["long_description"].as_str().filter(|l| !l.is_empty()).map(|l| format!(": {l}")).unwrap_or_default();
                            format!("- {} ({} pts){long}", s(&c["description"]), py_num(&c["points"]))
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                }
                Ok(Payload { title: self.label(cid, &s(&a["name"])), fingerprint: fp(&[&text]), kind: Kind::Text(text) })
            }
            "discussion" => {
                let (cid, tid) = rest.split_once(':').ok_or_else(|| Error::Invalid(format!("unknown item {key}")))?;
                let d = self.get("topic", &[cid, tid]).await?;
                let (t, view) = (&d["topic"], &d["view"]);
                let names: HashMap<String, String> = view["participants"].as_array().into_iter().flatten().map(|p| (p["id"].to_string(), s(&p["display_name"]))).collect();
                fn entries(es: &Value, depth: usize, names: &HashMap<String, String>, out: &mut Vec<String>) {
                    for e in es.as_array().into_iter().flatten() {
                        if e["deleted"].as_bool().unwrap_or(false) {
                            continue;
                        }
                        let who = names.get(&e["user_id"].to_string()).cloned().unwrap_or_else(|| "Someone".into());
                        out.push(format!("{}- **{who}**: {}", "  ".repeat(depth), md(&s(&e["message"]))));
                        entries(&e["replies"], depth + 1, names, out);
                    }
                }
                let mut text = self.header(cid, &s(&t["title"]), &s(&t["html_url"])) + &md(&s(&t["message"]));
                let mut reps = Vec::new();
                entries(&view["view"], 0, &names, &mut reps);
                if !reps.is_empty() {
                    text += &format!("\n\n## Replies\n\n{}", reps.join("\n"));
                }
                Ok(Payload { title: self.label(cid, &s(&t["title"])), fingerprint: fp(&[&text]), kind: Kind::Text(text) })
            }
            "syllabus" => {
                let cid = rest;
                let body = self.get("syllabus", &[cid]).await?;
                if !crate::util::truthy(&body) {
                    return Err(Error::NotFound("this course has no syllabus text".into()));
                }
                let text = self.header(cid, "Syllabus", &format!("{base}/courses/{cid}/assignments/syllabus")) + &md(&s(&body));
                Ok(Payload { title: self.label(cid, "Syllabus"), fingerprint: fp(&[&text]), kind: Kind::Text(text) })
            }
            "file" => {
                let (path, meta) = file_path(&self.engine, rest, None).await?;
                let name = meta["display_name"].as_str().map(String::from).unwrap_or_else(|| path.file_name().unwrap_or_default().to_string_lossy().into_owned());
                let suf = suffix(&name).to_lowercase();
                if !FILE_TYPES.contains(&suf.as_str()) {
                    return Err(Error::Invalid(format!("Gemini Notebook can't read {} files", if suf.is_empty() { "this" } else { &suf })));
                }
                let stamp = [rest.to_string(), py_str(&meta["updated_at"]), py_str(&meta["size"])];
                Ok(Payload {
                    title: name,
                    fingerprint: fp(&[&stamp[0], &stamp[1], &stamp[2]]),
                    kind: Kind::File(path, meta["content-type"].as_str().map(String::from)),
                })
            }
            "recording" => {
                let (cid, rid) = rest.split_once(':').ok_or_else(|| Error::Invalid(format!("unknown item {key}")))?;
                let c = self.course(cid);
                let label = c["course_code"].as_str().filter(|x| !x.is_empty()).map(String::from).unwrap_or_else(|| s(&c["name"]));
                let (title, text) = self.recordings.transcript(rid, &label).await?;
                Ok(Payload { title, fingerprint: fp(&[&text]), kind: Kind::Text(text) })
            }
            "url" => Ok(Payload { title: rest.to_string(), fingerprint: fp(&[rest]), kind: Kind::Url(rest.to_string()) }),
            _ => Err(Error::Invalid(format!("unknown item {key}"))),
        }
    }

    async fn upload(&self, nb: &str, p: &Payload) -> Result<String> {
        self.call(|c| async move {
            match (&*c, &p.kind) {
                (Backend::Real(n), Kind::Text(text)) => n.add_text(nb, &p.title, text, WAIT_TIMEOUT).await,
                (Backend::Real(n), Kind::File(path, mime)) => n.add_file(nb, path, mime.as_deref(), &p.title, WAIT_TIMEOUT).await,
                (Backend::Real(n), Kind::Url(u)) => n.add_url(nb, u, WAIT_TIMEOUT).await,
                (Backend::Demo(d), Kind::Text(_)) => d.add(nb, &p.title, 1000).await,
                (Backend::Demo(d), Kind::File(..)) => d.add(nb, &p.title, 2000).await,
                (Backend::Demo(d), Kind::Url(u)) => d.add(nb, u, 1500).await,
            }
        })
        .await
    }

    // --- jobs ------------------------------------------------------------------------------------------
    pub async fn create(self: &Arc<Self>, title: &str, course_id: Option<i64>, items: Vec<Value>) -> Result<Value> {
        let nb = self
            .call(|c| async move {
                match &*c {
                    Backend::Real(n) => n.create(title).await,
                    Backend::Demo(d) => {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        let mut books = d.books.lock().unwrap();
                        let nb = Notebook { id: format!("demo-nb-{}", books.len() + 1), title: title.to_string(), sources_count: 0, created_at: Some(now()) };
                        books.push(nb.clone());
                        Ok(nb)
                    }
                }
            })
            .await?;
        self.engine.store.exec("INSERT OR REPLACE INTO nb_notebooks (id, title, course_id, created_at) VALUES (?,?,?,?)", &[&nb.id, &nb.title, &course_id, &now()]);
        self.engine.refresh_bg("notebooks", &[]);
        Ok(self.start_job(&nb.id, &nb.title, items))
    }

    /// items: keys, or {"key", "title"} objects (the title is just a label until the item is resolved).
    pub fn start_job(self: &Arc<Self>, notebook_id: &str, title: &str, items: Vec<Value>) -> Value {
        let mut order: Vec<String> = Vec::new();
        let mut labels: HashMap<String, Value> = HashMap::new();
        for it in items {
            let (key, label) = match &it {
                Value::String(k) => (k.clone(), Value::Null),
                other => (s(&other["key"]), other.get("title").cloned().unwrap_or(Value::Null)),
            };
            if !labels.contains_key(&key) {
                order.push(key.clone());
                labels.insert(key, label);
            }
        }
        let id: String = hex::encode(rand::random::<[u8; 5]>());
        let job = json!({
            "id": id, "notebook_id": notebook_id, "title": title, "started_at": now(), "done": false,
            "items": order.iter().map(|k| json!({"key": k, "title": labels[k], "status": "queued", "error": null})).collect::<Vec<_>>(),
        });
        let shared = Arc::new(Mutex::new(job.clone()));
        self.jobs.lock().unwrap().insert(id.clone(), shared.clone());
        self.job_order.lock().unwrap().push(id);
        let this = self.clone();
        tokio::spawn(async move { this.run(shared).await });
        self.emit(&job);
        job
    }

    pub fn retry(self: &Arc<Self>, job_id: &str) -> Result<Value> {
        let old = self.jobs.lock().unwrap().get(job_id).cloned().ok_or_else(|| Error::NotFound("unknown job".into()))?;
        let old = old.lock().unwrap().clone();
        let items: Vec<Value> = old["items"].as_array().into_iter().flatten().filter(|i| i["status"] == "failed").map(|i| json!({"key": i["key"], "title": i["title"]})).collect();
        Ok(self.start_job(&s(&old["notebook_id"]), &s(&old["title"]), items))
    }

    pub fn all_jobs(&self) -> Vec<Value> {
        let jobs = self.jobs.lock().unwrap();
        self.job_order.lock().unwrap().iter().filter_map(|id| jobs.get(id).map(|j| j.lock().unwrap().clone())).collect()
    }

    fn emit(&self, job: &Value) {
        self.engine.broadcast(Event::Job(job.clone()));
    }

    fn update(&self, job: &Arc<Mutex<Value>>, i: usize, f: impl FnOnce(&mut Value)) {
        let snapshot = {
            let mut j = job.lock().unwrap();
            f(&mut j["items"][i]);
            j.clone()
        };
        self.emit(&snapshot);
    }

    async fn run(self: Arc<Self>, job: Arc<Mutex<Value>>) {
        let sem = Arc::new(tokio::sync::Semaphore::new(UPLOAD_CONCURRENCY));
        let (nb_id, n) = {
            let j = job.lock().unwrap();
            (s(&j["notebook_id"]), j["items"].as_array().map(|a| a.len()).unwrap_or(0))
        };
        let tasks = (0..n).map(|i| {
            let (this, job, sem, nb_id) = (self.clone(), job.clone(), sem.clone(), nb_id.clone());
            async move {
                let _permit = sem.acquire().await;
                let key = s(&job.lock().unwrap()["items"][i]["key"]);
                let result: Result<()> = async {
                    this.update(&job, i, |it| it["status"] = json!("preparing"));
                    let p = this.resolve(&key).await?;
                    job.lock().unwrap()["items"][i]["title"] = json!(p.title);
                    let prev = this.link(&nb_id, &key);
                    if let Some((_, Some(fprint), Some(status))) = &prev {
                        if status == "done" && *fprint == p.fingerprint {
                            let mut j = job.lock().unwrap();
                            j["items"][i]["status"] = json!("skipped");
                            j["items"][i]["error"] = json!("already in this notebook");
                            return Ok(());
                        }
                    }
                    this.update(&job, i, |it| it["status"] = json!("uploading"));
                    if let Some((Some(old), _, _)) = &prev {
                        // changed since last upload: replace the old source
                        let r = this
                            .call(|c| {
                                let (nb, old) = (nb_id.clone(), old.clone());
                                async move {
                                    match &*c {
                                        Backend::Real(n) => n.delete_source(&nb, &old).await,
                                        Backend::Demo(_) => {
                                            tokio::time::sleep(Duration::from_millis(200)).await;
                                            Ok(())
                                        }
                                    }
                                }
                            })
                            .await;
                        if let Err(e) = r {
                            log::info!("couldn't delete old source {old}: {e}");
                        }
                    }
                    let src = this.upload(&nb_id, &p).await?;
                    this.save_link(&nb_id, &key, Some(&src), Some(&p.title), Some(&p.fingerprint), "done", None);
                    job.lock().unwrap()["items"][i]["status"] = json!("done");
                    Ok(())
                }
                .await;
                if let Err(e) = result {
                    let msg = if e.to_string().is_empty() { e.type_name().to_string() } else { e.to_string() };
                    let title = {
                        let mut j = job.lock().unwrap();
                        j["items"][i]["status"] = json!("failed");
                        j["items"][i]["error"] = json!(msg);
                        j["items"][i]["title"].as_str().map(String::from)
                    };
                    this.save_link(&nb_id, &key, None, title.as_deref(), None, "failed", Some(&msg));
                }
                let snapshot = job.lock().unwrap().clone();
                this.emit(&snapshot);
            }
        });
        futures::future::join_all(tasks).await;
        let snapshot = {
            let mut j = job.lock().unwrap();
            j["done"] = json!(true);
            j.clone()
        };
        self.emit(&snapshot);
        self.engine.refresh_bg("nb_links", &[]);
        self.engine.refresh_bg("notebooks", &[]);
    }

    /// The "notebooks" resource.
    pub async fn notebooks_resource(&self) -> Result<Value> {
        let nbs = self.list().await?;
        Ok(Value::Array(
            nbs.into_iter()
                .map(|n| {
                    let created = n.created_at.and_then(|t| chrono::DateTime::from_timestamp(t as i64, 0)).map(|d| d.format("%Y-%m-%dT%H:%M:%S+00:00").to_string());
                    json!({"id": n.id, "title": n.title, "sources_count": n.sources_count, "url": format!("{NOTEBOOK_URL}{}", n.id), "created_at": created})
                })
                .collect(),
        ))
    }
}

fn py_num(v: &Value) -> String {
    match v {
        Value::Number(n) if n.is_f64() => {
            let f = n.as_f64().unwrap();
            if f.fract() == 0.0 { format!("{f:.1}") } else { f.to_string() }
        }
        other => s(other),
    }
}

fn py_str(v: &Value) -> String {
    if v.is_null() { "None".into() } else { s(v) }
}

#[cfg(test)]
mod tests {
    #[test]
    fn suffixes() {
        assert_eq!(super::suffix("notes.PDF"), ".PDF");
        assert_eq!(super::suffix("README"), "");
        assert_eq!(super::suffix(".bashrc"), "");
        assert_eq!(super::fp(&["a", "b"]), super::fp(&["a", "b"]));
    }
}
