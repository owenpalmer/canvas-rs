//! Canvas REST API client authenticated with the session cookies from your Firefox.

use std::sync::{Mutex, RwLock};
use std::time::Duration;

use once_cell::sync::Lazy;
use regex::Regex;
use reqwest::header::{ACCEPT, CONTENT_TYPE, COOKIE, HeaderMap, HeaderValue, LOCATION};
use serde_json::Value;

use crate::cookies::{self, cookie_header};
use crate::demo::DemoClient;
use crate::{Error, Result, config};

pub const MAX_PAGES: usize = 20;
pub type Params = Vec<(String, String)>;

/// Build query parameters: `params![("per_page", 100), ("include[]", "term")]`.
#[macro_export]
macro_rules! params {
    ($(($k:expr, $v:expr)),* $(,)?) => { vec![$(($k.to_string(), $v.to_string())),*] };
}

pub fn host_of(url: &str) -> String {
    url::Url::parse(url).ok().and_then(|u| u.host_str().map(|h| h.to_lowercase())).unwrap_or_default()
}

/// A downloaded file: its bytes and content type.
pub struct Download {
    pub bytes: Vec<u8>,
    pub content_type: String,
}

struct Conn {
    http: reqwest::Client,
}

pub struct CanvasClient {
    base: RwLock<(String, String)>, // (base, host)
    conn: Mutex<Option<std::sync::Arc<Conn>>>,
}

fn is_redirect(s: reqwest::StatusCode) -> bool {
    matches!(s.as_u16(), 301 | 302 | 303 | 307 | 308)
}

/// Canvas's JSON-hijacking guard on cookie-auth responses.
pub fn parse_json(text: &str) -> Result<Value> {
    let text = text.strip_prefix("while(1);").unwrap_or(text);
    Ok(serde_json::from_str(text)?)
}

static LINK_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"<([^>]*)>\s*((?:;\s*[^;,]+)*)"#).unwrap());

/// The rel="next" URL of a Link header.
pub fn next_link(headers: &HeaderMap) -> Option<String> {
    for v in headers.get_all("link") {
        let s = v.to_str().ok()?;
        for m in LINK_RE.captures_iter(s) {
            let params = m.get(2).map(|p| p.as_str()).unwrap_or("");
            if params.split(';').any(|p| {
                let p = p.trim();
                p.strip_prefix("rel=").map(|r| r.trim_matches('"').split_whitespace().any(|x| x == "next")).unwrap_or(false)
            }) {
                return Some(m[1].to_string());
            }
        }
    }
    None
}

pub fn status_error(status: reqwest::StatusCode, url: &str) -> Error {
    let kind = if status.is_client_error() { "Client error" } else if status.is_server_error() { "Server error" } else { "Error" };
    Error::Status {
        status: status.as_u16(),
        message: format!("{kind} '{} {}' for url '{url}'", status.as_u16(), status.canonical_reason().unwrap_or("")),
    }
}

impl CanvasClient {
    pub fn new(base: Option<&str>) -> CanvasClient {
        let base = base.map(String::from).unwrap_or_else(config::canvas_url).trim_end_matches('/').to_string();
        let host = host_of(&base);
        CanvasClient { base: RwLock::new((base, host)), conn: Mutex::new(None) }
    }

    pub fn base(&self) -> String {
        self.base.read().unwrap().0.clone()
    }
    pub fn host(&self) -> String {
        self.base.read().unwrap().1.clone()
    }

    pub fn set_base(&self, base: &str) {
        let base = base.trim_end_matches('/').to_string();
        let host = host_of(&base);
        *self.base.write().unwrap() = (base, host);
        *self.conn.lock().unwrap() = None; // the next request connects with this host's cookies
    }

    fn connect(&self) -> Result<std::sync::Arc<Conn>> {
        if self.base().is_empty() {
            // set up (in the app) after this process started
            self.set_base(&config::require_canvas_url()?);
        }
        let host = self.host();
        let cookies = match cookies::load_cookies(&host, None) {
            Ok(c) => c,
            Err(Error::NotPermitted(site)) => return Err(Error::NeedsPermission(Error::NotPermitted(site).to_string())),
            Err(e) => return Err(e),
        };
        if !cookies.contains_key("canvas_session") && !cookies.contains_key("_legacy_normandy_session") {
            let hint = cookies::restore_hint(None);
            let msg = format!("No Canvas session in Firefox. Log into {} in Firefox and try again.", self.base());
            return Err(Error::SessionExpired(if hint.is_empty() { msg } else { format!("{msg} {hint}") }));
        }
        let csrf = cookies.get("_csrf_token").map(|t| percent_encoding::percent_decode_str(t).decode_utf8_lossy().into_owned()).unwrap_or_default();
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        if let Ok(v) = HeaderValue::from_str(&csrf) {
            headers.insert("X-CSRF-Token", v);
        }
        if let Ok(mut v) = HeaderValue::from_str(&cookie_header(&cookies)) {
            v.set_sensitive(true);
            headers.insert(COOKIE, v);
        }
        let http = reqwest::Client::builder()
            .user_agent(crate::util::USER_AGENT)
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| Error::Fetch(e.to_string()))?;
        let conn = std::sync::Arc::new(Conn { http });
        *self.conn.lock().unwrap() = Some(conn.clone());
        Ok(conn)
    }

    fn absolute(&self, url: &str) -> String {
        if url.starts_with("http://") || url.starts_with("https://") { url.to_string() } else { format!("{}/{}", self.base(), url.trim_start_matches('/')) }
    }

    /// GET with your session. On an auth failure, re-read cookies from Firefox once: you may
    /// have logged in again since. Redirects are returned, not followed.
    async fn request(&self, url: &str, params: Option<&Params>) -> Result<reqwest::Response> {
        for _ in 0..2 {
            let existing = self.conn.lock().unwrap().clone();
            let conn = match existing {
                Some(c) => c,
                None => self.connect()?,
            };
            let full = self.absolute(url);
            let mut req = conn.http.get(&full);
            if let Some(p) = params {
                req = req.query(p);
            }
            let resp = req.send().await?;
            let status = resp.status();
            let location = resp.headers().get(LOCATION).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
            let logged_out = status.as_u16() == 401 || (is_redirect(status) && location.contains("/login"));
            if !logged_out {
                if !is_redirect(status) && !status.is_success() {
                    return Err(status_error(status, resp.url().as_str()));
                }
                return Ok(resp);
            }
            *self.conn.lock().unwrap() = None;
        }
        Err(Error::SessionExpired(format!(
            "Canvas session expired. Open {} in Firefox (log in if asked) and try again.",
            self.base()
        )))
    }

    /// GET one resource, or all pages of a list (up to MAX_PAGES, or until `limit` items).
    pub async fn get(&self, path: &str, params: &Params, limit: Option<usize>) -> Result<Value> {
        let resp = self.request(&format!("/api/v1/{}", path.trim_start_matches('/')), Some(params)).await?;
        let mut next = next_link(resp.headers());
        let data = parse_json(&resp.text().await?)?;
        let Value::Array(mut items) = data else { return Ok(data) };
        let mut pages = 1;
        while let Some(url) = next.take() {
            if pages >= MAX_PAGES || limit.map(|l| items.len() >= l).unwrap_or(false) {
                break;
            }
            let resp = self.request(&url, None).await?;
            next = next_link(resp.headers());
            if let Value::Array(more) = parse_json(&resp.text().await?)? {
                items.extend(more);
            }
            pages += 1;
        }
        if let Some(l) = limit {
            items.truncate(l);
        }
        Ok(Value::Array(items))
    }

    /// Download a file, following Canvas's redirect to its file storage.
    pub async fn download(&self, url: &str) -> Result<Download> {
        let mut resp = self.request(url, None).await?;
        let mut next = String::new();
        let mut left_canvas = false;
        for _ in 0..5 {
            if !is_redirect(resp.status()) {
                let ct = resp.headers().get(CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("application/octet-stream").to_string();
                return Ok(Download { bytes: resp.bytes().await?.to_vec(), content_type: ct });
            }
            let loc = resp.headers().get(LOCATION).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
            next = resp.url().join(&loc).map(|u| u.to_string()).unwrap_or(loc);
            if host_of(&next) != self.host() {
                left_canvas = true;
                break;
            }
            resp = self.request(&next, None).await?;
        }
        if !left_canvas {
            return Err(Error::Fetch("Too many redirects".into()));
        }
        // Signed storage URL: don't send Canvas cookies to a third-party host.
        let http = reqwest::Client::builder().user_agent(crate::util::USER_AGENT).timeout(Duration::from_secs(120)).build().map_err(|e| Error::Fetch(e.to_string()))?;
        let resp = http.get(&next).send().await?;
        if !resp.status().is_success() {
            return Err(status_error(resp.status(), &next));
        }
        let ct = resp.headers().get(CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("application/octet-stream").to_string();
        Ok(Download { bytes: resp.bytes().await?.to_vec(), content_type: ct })
    }
}

/// The Canvas client, or the demo's fake one.
pub enum Client {
    Canvas(CanvasClient),
    Demo(DemoClient),
}

impl Client {
    /// The demo's client with CANVAS_DEMO set, else the real one.
    pub fn from_env() -> Client {
        if config::is_demo() { Client::Demo(DemoClient::new()) } else { Client::Canvas(CanvasClient::new(None)) }
    }
    pub fn base(&self) -> String {
        match self {
            Client::Canvas(c) => c.base(),
            Client::Demo(_) => crate::demo::BASE.into(),
        }
    }
    pub fn host(&self) -> String {
        match self {
            Client::Canvas(c) => c.host(),
            Client::Demo(_) => "canvas.example.edu".into(),
        }
    }
    pub fn set_base(&self, base: &str) {
        if let Client::Canvas(c) = self {
            c.set_base(base)
        }
    }
    pub async fn get(&self, path: &str, params: Params) -> Result<Value> {
        self.get_limit(path, params, None).await
    }
    pub async fn get_limit(&self, path: &str, params: Params, limit: Option<usize>) -> Result<Value> {
        match self {
            Client::Canvas(c) => c.get(path, &params, limit).await,
            Client::Demo(d) => d.get(path, &params).await,
        }
    }
    pub async fn download(&self, url: &str) -> Result<Download> {
        match self {
            Client::Canvas(c) => c.download(url).await,
            Client::Demo(d) => d.download(url).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_header_next() {
        let mut h = HeaderMap::new();
        h.insert(
            "link",
            HeaderValue::from_static(r#"<https://x.edu/api/v1/courses?page=1&per_page=100>; rel="current",<https://x.edu/api/v1/courses?page=2&per_page=100>; rel="next",<https://x.edu/api/v1/courses?page=1>; rel="first""#),
        );
        assert_eq!(next_link(&h).unwrap(), "https://x.edu/api/v1/courses?page=2&per_page=100");
        let mut h = HeaderMap::new();
        h.insert("link", HeaderValue::from_static(r#"<https://x.edu/a?page=1>; rel="current""#));
        assert!(next_link(&h).is_none());
    }

    #[test]
    fn json_guard() {
        assert_eq!(parse_json("while(1);[1,2]").unwrap(), serde_json::json!([1, 2]));
        assert_eq!(parse_json("{\"a\":1}").unwrap(), serde_json::json!({"a": 1}));
    }
}
