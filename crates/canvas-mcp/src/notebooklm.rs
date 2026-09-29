//! A small client for NotebookLM's private web API (what notebooklm-py did for the Python version):
//! the batchexecute RPCs for notebooks and sources, and the resumable upload for files.
//!
//! It signs in with your Google cookies from Firefox (held in memory only). This API is unofficial
//! and can change without notice; the request shapes follow notebooklm-py 0.8.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use regex::Regex;
use reqwest::cookie::Jar;
use serde_json::{Value, json};

use crate::cookies::Cookie;
use crate::{Error, Result};

pub const BASE_URL: &str = "https://notebook.google.com";
const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/138.0.0.0 Safari/537.36";

const LIST_NOTEBOOKS: &str = "wXbhsf";
const CREATE_NOTEBOOK: &str = "CCqFvf";
const GET_NOTEBOOK: &str = "rLM1Ne";
const ADD_SOURCE: &str = "izAoDd";
const ADD_SOURCE_FILE: &str = "o4cbdc";
const DELETE_SOURCE: &str = "tGMBJ";

const STATUS_READY: i64 = 2;
const STATUS_ERROR: i64 = 3;

#[derive(Clone, Debug)]
pub struct Notebook {
    pub id: String,
    pub title: String,
    pub sources_count: usize,
    pub created_at: Option<f64>,
}

fn template_block() -> Value {
    json!([2, null, null, [1, null, null, null, null, null, null, null, null, null, [1]]])
}

fn jar_for(cookies: &[Cookie]) -> Arc<Jar> {
    let jar = Arc::new(Jar::default());
    for c in cookies {
        let host = c.domain.trim_start_matches('.');
        let Ok(url) = url::Url::parse(&format!("https://{host}{}", c.path)) else { continue };
        let domain = if c.domain.starts_with('.') { format!("; Domain={}", c.domain) } else { String::new() };
        jar.add_cookie_str(&format!("{}={}{domain}; Path={}{}", c.name, c.value, c.path, if c.secure { "; Secure" } else { "" }), &url);
    }
    jar
}

fn http_with(cookies: &[Cookie]) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .cookie_provider(jar_for(cookies))
        .user_agent(UA)
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| Error::Fetch(e.to_string()))
}

fn is_login(url: &reqwest::Url) -> bool {
    url.host_str() == Some("accounts.google.com") || url.path().contains("ServiceLogin")
}

/// A WIZ_global_data value from the page.
fn wiz_field(html: &str, key: &str) -> Option<String> {
    let k = regex::escape(key);
    for pat in [format!(r#""{k}"\s*:\s*"([^"\\]*(?:\\.[^"\\]*)*)""#), format!(r"'{k}'\s*:\s*'([^'\\]*(?:\\.[^'\\]*)*)'")] {
        if let Some(m) = Regex::new(&pat).ok()?.captures(html) {
            return Some(m[1].to_string());
        }
    }
    None
}

/// The signed-in account's email from a NotebookLM page.
fn email_from_html(html: &str) -> Option<String> {
    static EMAIL: Lazy<Regex> = Lazy::new(|| Regex::new(r#""([A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,})""#).unwrap());
    const LOCALS: &[&str] = &["abuse", "feedback", "info", "mail-noreply", "googlemail-noreply", "no-reply", "noreply", "press", "privacy", "support"];
    const DOMAINS: &[&str] = &["google.com", "accounts.google.com", "gmail.com"];
    for m in EMAIL.captures_iter(html) {
        let email = m[1].to_string();
        let (local, domain) = email.split_once('@').unwrap_or(("", ""));
        if LOCALS.contains(&local.to_lowercase().as_str()) && DOMAINS.contains(&domain.to_lowercase().as_str()) {
            continue;
        }
        return Some(email);
    }
    None
}

/// The Google accounts signed in to Firefox, as NotebookLM sees them: [{authuser, email}].
pub async fn enumerate_accounts(cookies: &[Cookie]) -> Result<Vec<Value>> {
    let http = http_with(cookies)?;
    let probe = |n: i64| {
        let http = http.clone();
        async move {
            let resp = http.get(format!("{BASE_URL}/?authuser={n}")).header("Accept", "text/html,*/*").send().await?;
            if resp.status().as_u16() != 200 || is_login(resp.url()) {
                return Ok::<Option<String>, Error>(None);
            }
            Ok(email_from_html(&resp.text().await?))
        }
    };
    let Some(default) = probe(0).await? else { return Ok(vec![]) };
    let mut out = vec![json!({"authuser": 0, "email": default})];
    for n in 1..=9 {
        match probe(n).await? {
            Some(e) if e != default => out.push(json!({"authuser": n, "email": e})),
            _ => break,
        }
    }
    Ok(out)
}

/// batchexecute responses: ")]}'" then chunks; the result is the JSON string in the "wrb.fr" frame.
fn decode(raw: &str, rpc_id: &str) -> Result<Value> {
    let body = raw.strip_prefix(")]}'").unwrap_or(raw);
    let mut result = Value::Null;
    let mut saw = false;
    for line in body.lines() {
        let line = line.trim();
        if !line.starts_with('[') {
            continue;
        }
        let Ok(chunk) = serde_json::from_str::<Value>(line) else { continue };
        let items: Vec<Value> = match &chunk {
            Value::Array(a) if a.first().map(|f| f.is_array()).unwrap_or(false) => a.clone(),
            other => vec![other.clone()],
        };
        for item in items {
            let Some(it) = item.as_array() else { continue };
            if it.len() < 3 || it[1] != rpc_id {
                continue;
            }
            if it[0] == "er" {
                return Err(Error::Fetch(format!("NotebookLM refused the request ({})", it[2])));
            }
            if it[0] == "wrb.fr" {
                saw = true;
                if let Some(s) = it[2].as_str() {
                    if let Ok(v) = serde_json::from_str::<Value>(s) {
                        if !v.is_null() {
                            result = v;
                        }
                    }
                } else if it.len() > 5 && !it[5].is_null() && result.is_null() {
                    let code = it[5].get(0).cloned().unwrap_or(Value::Null);
                    if code == json!(16) || code == json!(7) {
                        return Err(Error::Auth("NotebookLM says you're not signed in".into()));
                    }
                    return Err(Error::Fetch(format!("NotebookLM couldn't do that (status {code})")));
                }
            }
        }
    }
    if !saw {
        return Err(Error::Fetch("NotebookLM sent an unexpected response".into()));
    }
    Ok(result)
}

static UUID: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$").unwrap());

/// The first source id (a UUID) in a response, depth first.
fn first_uuid(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if UUID.is_match(s) => Some(s.clone()),
        Value::Array(a) => a.iter().find_map(first_uuid),
        _ => None,
    }
}

fn youtube_id(url: &str) -> Option<String> {
    static YT: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?:youtube(?:-nocookie)?\.com/(?:watch\?(?:.*&)?v=|embed/|shorts/|live/|v/)|youtu\.be/)([A-Za-z0-9_-]{11})").unwrap());
    YT.captures(url).map(|m| m[1].to_string())
}

pub struct NotebookLm {
    http: reqwest::Client,
    authuser: i64,
    csrf: String,
    sid: String,
}

fn row_notebook(row: &Value) -> Option<Notebook> {
    let a = row.as_array()?;
    let id = a.get(2)?.as_str()?.to_string();
    let title = a.first().and_then(|t| t.as_str()).unwrap_or("").replace("thought\n", "").trim().to_string();
    let sources_count = a.get(1).and_then(|s| s.as_array()).map(|s| s.len()).unwrap_or(0);
    let created_at = a.get(5).and_then(|m| m.get(8)).and_then(|t| t.get(0)).and_then(|t| t.as_f64());
    Some(Notebook { id, title, sources_count, created_at })
}

impl NotebookLm {
    /// Sign in with Google cookies: fetch the page tokens the RPCs need.
    pub async fn connect(cookies: &[Cookie], authuser: i64) -> Result<NotebookLm> {
        let http = http_with(cookies)?;
        let url = if authuser != 0 { format!("{BASE_URL}/?authuser={authuser}") } else { format!("{BASE_URL}/") };
        let resp = http.get(&url).timeout(Duration::from_secs(30)).send().await?;
        if is_login(resp.url()) {
            return Err(Error::Auth("Your Google sign-in expired. Sign in to NotebookLM in Firefox, then try again.".into()));
        }
        if !resp.status().is_success() {
            return Err(crate::client::status_error(resp.status(), &url));
        }
        let html = resp.text().await?;
        let csrf = wiz_field(&html, "SNlM0e").ok_or_else(|| Error::Auth("Couldn't sign in to NotebookLM (no CSRF token on its page)".into()))?;
        let sid = wiz_field(&html, "FdrFJe").ok_or_else(|| Error::Auth("Couldn't sign in to NotebookLM (no session id on its page)".into()))?;
        Ok(NotebookLm { http, authuser, csrf, sid })
    }

    async fn rpc(&self, rpc_id: &str, params: Value, source_path: &str) -> Result<Value> {
        let mut q = vec![("rpcids", rpc_id.to_string()), ("source-path", source_path.to_string()), ("f.sid", self.sid.clone()), ("hl", "en".into()), ("rt", "c".into())];
        if self.authuser != 0 {
            q.push(("authuser", self.authuser.to_string()));
        }
        let f_req = json!([[[rpc_id, params.to_string(), null, "generic"]]]).to_string();
        let resp = self
            .http
            .post(format!("{BASE_URL}/_/LabsTailwindUi/data/batchexecute"))
            .query(&q)
            .header("Content-Type", "application/x-www-form-urlencoded;charset=UTF-8")
            .form(&[("f.req", f_req.as_str()), ("at", self.csrf.as_str())])
            .send()
            .await?;
        let status = resp.status().as_u16();
        if status == 401 || status == 403 {
            return Err(Error::Auth(format!("NotebookLM answered HTTP {status}; sign in to Google in Firefox again")));
        }
        if !resp.status().is_success() {
            return Err(Error::Fetch(format!("NotebookLM answered HTTP {status}")));
        }
        decode(&resp.text().await?, rpc_id)
    }

    pub async fn list(&self) -> Result<Vec<Notebook>> {
        let r = self.rpc(LIST_NOTEBOOKS, json!([null, 1, null, [2]]), "/").await?;
        Ok(r.get(0).and_then(|x| x.as_array()).into_iter().flatten().filter_map(row_notebook).collect())
    }

    pub async fn create(&self, title: &str) -> Result<Notebook> {
        let r = self.rpc(CREATE_NOTEBOOK, json!([title, null, null, template_block()]), "/").await?;
        row_notebook(&r).or_else(|| first_uuid(&r).map(|id| Notebook { id, title: title.to_string(), sources_count: 0, created_at: None }))
            .ok_or_else(|| Error::Fetch("NotebookLM didn't return the new notebook".into()))
    }

    /// The source's processing status (2 ready, 3 failed), from the notebook.
    async fn source_status(&self, nb: &str, source_id: &str) -> Result<Option<i64>> {
        let r = self.rpc(GET_NOTEBOOK, json!([nb, null, template_block(), null, 0]), &format!("/notebook/{nb}")).await?;
        let row = r.get(0).cloned().unwrap_or(Value::Null);
        for s in row.get(1).and_then(|x| x.as_array()).into_iter().flatten() {
            if first_uuid(&s[0]).as_deref() == Some(source_id) {
                return Ok(s.get(3).and_then(|b| b.get(1)).and_then(|v| v.as_i64()));
            }
        }
        Ok(None)
    }

    async fn wait_ready(&self, nb: &str, source_id: &str, timeout: Duration) -> Result<()> {
        let start = Instant::now();
        let mut delay = Duration::from_secs(1);
        loop {
            match self.source_status(nb, source_id).await? {
                Some(STATUS_READY) => return Ok(()),
                Some(STATUS_ERROR) => return Err(Error::Fetch("NotebookLM couldn't process this source".into())),
                _ => {}
            }
            if start.elapsed() > timeout {
                return Err(Error::Fetch("NotebookLM is still processing this source (timed out waiting)".into()));
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 3 / 2).min(Duration::from_secs(5));
        }
    }

    async fn added(&self, nb: &str, r: Value, wait: Duration) -> Result<String> {
        let id = first_uuid(&r).ok_or_else(|| Error::Fetch("NotebookLM didn't return the new source".into()))?;
        self.wait_ready(nb, &id, wait).await?;
        Ok(id)
    }

    pub async fn add_text(&self, nb: &str, title: &str, content: &str, wait: Duration) -> Result<String> {
        let params = json!([[[null, [title, content], null, 2, null, null, null, null, null, null, 1]], nb, template_block()]);
        let r = self.rpc(ADD_SOURCE, params, &format!("/notebook/{nb}")).await?;
        self.added(nb, r, wait).await
    }

    pub async fn add_url(&self, nb: &str, url: &str, wait: Duration) -> Result<String> {
        let spec = if youtube_id(url).is_some() {
            json!([null, null, null, null, null, null, null, [url], null, null, 1])
        } else {
            json!([null, null, [url], null, null, null, null, null, null, null, 1])
        };
        let r = self.rpc(ADD_SOURCE, json!([[spec], nb, template_block()]), &format!("/notebook/{nb}")).await?;
        self.added(nb, r, wait).await
    }

    pub async fn add_file(&self, nb: &str, path: &Path, mime: Option<&str>, title: &str, wait: Duration) -> Result<String> {
        let bytes = tokio::fs::read(path).await?;
        let filename = if title.is_empty() { path.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string() } else { title.to_string() };
        // 1. register the source, 2. start a resumable upload, 3. send the bytes.
        let r = self.rpc(ADD_SOURCE_FILE, json!([[[filename]], nb, template_block()]), &format!("/notebook/{nb}")).await?;
        let source_id = first_uuid(&r).ok_or_else(|| Error::Fetch("NotebookLM didn't register the file".into()))?;
        let origin = BASE_URL;
        let start = self
            .http
            .post(format!("{BASE_URL}/upload/_/?authuser={}", self.authuser))
            .header("Accept", "*/*")
            .header("Content-Type", "application/x-www-form-urlencoded;charset=UTF-8")
            .header("Origin", origin)
            .header("Referer", format!("{origin}/"))
            .header("x-goog-authuser", self.authuser.to_string())
            .header("x-goog-upload-command", "start")
            .header("x-goog-upload-header-content-length", bytes.len().to_string())
            .header("x-goog-upload-header-content-type", mime.unwrap_or("application/octet-stream"))
            .header("x-goog-upload-protocol", "resumable")
            .body(json!({"PROJECT_ID": nb, "SOURCE_NAME": filename, "SOURCE_ID": source_id}).to_string())
            .send()
            .await?;
        if !start.status().is_success() {
            return Err(Error::Fetch(format!("NotebookLM refused the upload (HTTP {})", start.status().as_u16())));
        }
        let upload_url = start.headers().get("x-goog-upload-url").and_then(|v| v.to_str().ok()).map(String::from)
            .ok_or_else(|| Error::Fetch("NotebookLM didn't start the upload".into()))?;
        let host = crate::client::host_of(&upload_url);
        if !(host.ends_with(".google.com") || host == "google.com") {
            return Err(Error::Fetch("NotebookLM sent an unexpected upload address".into()));
        }
        let done = self
            .http
            .post(&upload_url)
            .header("Accept", "*/*")
            .header("Content-Type", "application/x-www-form-urlencoded;charset=utf-8")
            .header("x-goog-authuser", self.authuser.to_string())
            .header("Origin", origin)
            .header("Referer", format!("{origin}/"))
            .header("x-goog-upload-command", "upload, finalize")
            .header("x-goog-upload-offset", "0")
            .timeout(Duration::from_secs(600))
            .body(bytes)
            .send()
            .await?;
        if !done.status().is_success() {
            return Err(Error::Fetch(format!("The upload to NotebookLM failed (HTTP {})", done.status().as_u16())));
        }
        self.wait_ready(nb, &source_id, wait).await?;
        Ok(source_id)
    }

    pub async fn delete_source(&self, nb: &str, source_id: &str) -> Result<()> {
        self.rpc(DELETE_SOURCE, json!([[[source_id]]]), &format!("/notebook/{nb}")).await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_frames() {
        let raw = ")]}'\n\n123\n[[\"wrb.fr\",\"wXbhsf\",\"[[[\\\"My notebook\\\",[[1],[2]],\\\"8a1b2c3d-0000-4000-8000-000000000001\\\"]]]\",null,null,null,\"generic\"]]\n25\n[[\"di\",42],[\"af.httprm\",41,\"x\",1]]\n";
        let r = decode(raw, "wXbhsf").unwrap();
        let nb = row_notebook(&r[0][0]).unwrap();
        assert_eq!(nb.title, "My notebook");
        assert_eq!(nb.id, "8a1b2c3d-0000-4000-8000-000000000001");
        assert_eq!(nb.sources_count, 2);
    }

    #[test]
    fn page_tokens() {
        let html = r#"<script>window.WIZ_global_data = {"FdrFJe":"-123","SNlM0e":"AF1_QpN-x\"y","cfb2h":"b"};</script> "someone@school.edu" "#;
        assert_eq!(wiz_field(html, "SNlM0e").unwrap(), "AF1_QpN-x\\\"y");
        assert_eq!(wiz_field(html, "FdrFJe").unwrap(), "-123");
        assert_eq!(email_from_html(r#""support@google.com" "me@uw.edu""#).unwrap(), "me@uw.edu");
    }

    #[test]
    fn misc() {
        assert_eq!(youtube_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ").unwrap(), "dQw4w9WgXcQ");
        assert_eq!(youtube_id("https://youtu.be/dQw4w9WgXcQ").unwrap(), "dQw4w9WgXcQ");
        assert!(youtube_id("https://visualgo.net").is_none());
        assert_eq!(first_uuid(&json!([[["8a1b2c3d-0000-4000-8000-000000000001"], "t"]])).unwrap(), "8a1b2c3d-0000-4000-8000-000000000001");
    }
}
