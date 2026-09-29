//! Panopto client authenticated with your Firefox Panopto session (like client.rs for Canvas).
//!
//! Uses the same endpoints as Panopto's web player and recording browser:
//!   DeliveryInfo.aspx           recording details (+ Panopto's AI summary/chapters)
//!   GenerateSRT.ashx            captions as SRT
//!   Data.svc/GetSessions        recordings in a folder, or matching a search (each with its folder)
//!
//! There is no folder listing for students (GetFoldersList is 404), so a Canvas course's folder is
//! found from a recording linked in the course, or by searching recordings for the course code.

use std::sync::Mutex;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use once_cell::sync::Lazy;
use regex::Regex;
use reqwest::header::{COOKIE, HeaderMap, HeaderValue, LOCATION};
use serde_json::{Value, json};

use crate::cookies::{self, cookie_header};
use crate::demo;
use crate::{Error, Result, config};

pub const GUID: &str = r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}";
/// Panopto recording links/embeds in Canvas content: Viewer.aspx?id=…, Embed.aspx?id=…, LTI launches with id/sessionId.
pub static LINK_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(&format!(r#"(?i)panopto\.com/[^\s"'<>]*?(?:[?&](?:id|sessionId|deliveryId)=)({GUID})"#)).unwrap());
const WINDOWS_EPOCH_OFFSET: f64 = 11644473600.0; // seconds from 1601-01-01 to 1970-01-01

pub fn viewer_url(rid: &str, host: &str) -> String {
    format!("https://{host}/Panopto/Pages/Viewer.aspx?id={rid}")
}

fn iso(secs: f64) -> String {
    let ms = (secs * 1000.0).round() as i64;
    Utc.timestamp_millis_opt(ms).single().map(|d| {
        if ms % 1000 == 0 { d.format("%Y-%m-%dT%H:%M:%S+00:00").to_string() } else { d.format("%Y-%m-%dT%H:%M:%S%.6f+00:00").to_string() }
    }).unwrap_or_default()
}

/// Panopto returns ISO strings, .NET '/Date(1696000000000)/' values, or (DeliveryInfo) plain
/// seconds since 1601-01-01, the Windows epoch.
pub fn parse_date(v: &Value) -> Value {
    static NET: Lazy<Regex> = Lazy::new(|| Regex::new(r"/Date\((-?\d+)").unwrap());
    match v {
        Value::Null => Value::Null,
        Value::Bool(false) => Value::Null,
        Value::Number(n) => {
            let mut f = n.as_f64().unwrap_or(0.0);
            if f == 0.0 {
                return Value::Null;
            }
            if f > 1e10 {
                f -= WINDOWS_EPOCH_OFFSET; // seconds since 1601, not 1970
            }
            json!(iso(f))
        }
        Value::String(s) if s.is_empty() => Value::Null,
        Value::String(s) => match NET.captures(s) {
            Some(m) => json!(iso(m[1].parse::<f64>().unwrap_or(0.0) / 1000.0)),
            None => json!(s),
        },
        other => json!(other.to_string()),
    }
}

fn unescape(v: &Value) -> String {
    html_unescape(v.as_str().unwrap_or(""))
}

/// Python's html.unescape for the entities Panopto uses.
pub fn html_unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    static ENT: Lazy<Regex> = Lazy::new(|| Regex::new(r"&(#[0-9]+|#[xX][0-9a-fA-F]+|[a-zA-Z]+);").unwrap());
    ENT.replace_all(s, |m: &regex::Captures| {
        let e = &m[1];
        if let Some(num) = e.strip_prefix('#') {
            let code = if let Some(hex) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) { u32::from_str_radix(hex, 16).ok() } else { num.parse().ok() };
            return code.and_then(char::from_u32).map(String::from).unwrap_or_else(|| m[0].to_string());
        }
        match e {
            "amp" => "&", "lt" => "<", "gt" => ">", "quot" => "\"", "apos" => "'", "nbsp" => "\u{a0}", "ndash" => "–", "mdash" => "—",
            "rsquo" => "’", "lsquo" => "‘", "rdquo" => "”", "ldquo" => "“", "hellip" => "…",
            _ => return m[0].to_string(),
        }
        .to_string()
    })
    .into_owned()
}

pub struct PanoptoClient {
    pub host: String,
    http: Mutex<Option<reqwest::Client>>,
}

impl PanoptoClient {
    pub fn new(host: &str) -> PanoptoClient {
        PanoptoClient { host: host.to_string(), http: Mutex::new(None) }
    }

    fn connect(&self) -> Result<reqwest::Client> {
        if self.host.is_empty() {
            return Err(Error::PanoptoSession(format!(
                "Panopto isn't set up: add panopto_host = \"your-school.hosted.panopto.com\" to {}",
                config::config_path().display()
            )));
        }
        let cookies = cookies::load_cookies(&self.host, None).map_err(|e| Error::PanoptoSession(e.to_string()))?;
        if !cookies.contains_key(".ASPXAUTH") {
            let hint = cookies::restore_hint(None);
            let msg = format!("No Panopto session in Firefox. Open any recording on {} in Firefox, then retry.", self.host);
            return Err(Error::PanoptoSession(if hint.is_empty() { msg } else { format!("{msg} {hint}") }));
        }
        let mut headers = HeaderMap::new();
        if let Some(t) = cookies.get("csrfToken").filter(|t| !t.is_empty()) {
            if let Ok(v) = HeaderValue::from_str(t) {
                headers.insert("X-CSRF-Token", v);
            }
        }
        if let Ok(mut v) = HeaderValue::from_str(&cookie_header(&cookies)) {
            v.set_sensitive(true);
            headers.insert(COOKIE, v);
        }
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| Error::Fetch(e.to_string()))?;
        *self.http.lock().unwrap() = Some(http.clone());
        Ok(http)
    }

    /// On a login redirect/401, re-read cookies from Firefox once.
    async fn request(&self, build: impl Fn(&reqwest::Client, String) -> reqwest::RequestBuilder, path: &str) -> Result<reqwest::Response> {
        for _ in 0..2 {
            let existing = self.http.lock().unwrap().clone();
            let http = match existing {
                Some(h) => h,
                None => self.connect()?,
            };
            let resp = build(&http, format!("https://{}{path}", self.host)).send().await?;
            let status = resp.status();
            let loc = resp.headers().get(LOCATION).and_then(|v| v.to_str().ok()).unwrap_or("").to_lowercase();
            let redirect = matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308);
            if !(status.as_u16() == 401 || (redirect && loc.contains("login"))) {
                if !status.is_success() {
                    return Err(crate::client::status_error(status, resp.url().as_str()));
                }
                return Ok(resp);
            }
            *self.http.lock().unwrap() = None;
        }
        Err(Error::PanoptoSession(format!("Panopto session expired. Open any recording on {} in Firefox, then retry.", self.host)))
    }

    pub async fn delivery_info(&self, rid: &str) -> Result<Value> {
        let resp = self
            .request(
                |h, url| h.post(url).form(&[("deliveryId", rid), ("isEmbed", "true"), ("responseType", "json")]),
                "/Panopto/Pages/Viewer/DeliveryInfo.aspx",
            )
            .await?;
        let info: Value = serde_json::from_str(&resp.text().await?)?;
        if info.get("ErrorCode").map(|v| !v.is_null() && v != &json!(0) && v != &json!(false)).unwrap_or(false) {
            let msg = info.get("ErrorMessage").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).unwrap_or("recording unavailable");
            return Err(Error::NotFound(format!("Panopto: {msg}")));
        }
        let d = info.get("Delivery").cloned().unwrap_or(json!({}));
        let langs = d.get("AvailableLanguages").filter(|v| v.as_array().map(|a| !a.is_empty()).unwrap_or(false)).cloned().unwrap_or(json!([0]));
        Ok(json!({
            "id": rid,
            "title": unescape(&d["SessionName"]),
            "duration": d.get("Duration").cloned().unwrap_or(Value::Null),
            "start": parse_date(d.get("SessionStartTime").unwrap_or(&Value::Null)),
            "has_captions": d.get("HasCaptions").map(truthy).unwrap_or(false),
            "languages": langs,
            "folder_id": d.get("SessionGroupPublicID").cloned().unwrap_or(Value::Null),
            "folder_name": unescape(&d["SessionGroupLongName"]),
            "summary": d.get("AISummary").cloned().unwrap_or(Value::Null),
            "chapters": d.get("AIChapters").cloned().unwrap_or(Value::Null),
            "key_points": d.get("AIKeyPoints").cloned().unwrap_or(Value::Null),
            "abstract": d.get("SessionAbstract").cloned().unwrap_or(Value::Null),
            "url": viewer_url(rid, &self.host),
        }))
    }

    pub async fn captions_srt(&self, rid: &str, language: i64) -> Result<String> {
        let lang = language.to_string();
        let resp = self
            .request(|h, url| h.get(url).query(&[("id", rid), ("language", lang.as_str())]), "/Panopto/Pages/Transcription/GenerateSRT.ashx")
            .await?;
        Ok(resp.text().await?)
    }

    async fn data(&self, method: &str, body: &Value) -> Result<Value> {
        let resp = self
            .request(
                |h, url| h.post(url).header("Content-Type", "application/json; charset=utf-8").body(body.to_string()),
                &format!("/Panopto/Services/Data.svc/{method}"),
            )
            .await?;
        let v: Value = serde_json::from_str(&resp.text().await?)?;
        Ok(v.get("d").cloned().unwrap_or(Value::Null))
    }

    async fn sessions(&self, folder_id: Option<&str>, query: Option<&str>, max_pages: usize) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut page = 0;
        loop {
            let d = self
                .data(
                    "GetSessions",
                    &json!({"queryParameters": {
                        "query": query, "sortColumn": 1, "sortAscending": false, "maxResults": 100, "page": page,
                        "startDate": null, "endDate": null, "folderID": folder_id, "bookmarked": false, "getFolderData": true,
                        "isSharedWithMe": false, "isSubscriptionsPage": false, "includeArchived": true,
                        "includeArchivedStateCount": true, "sessionListOnlyArchived": false, "includePlaylists": true,
                    }}),
                )
                .await?;
            let results = d.get("Results").and_then(|r| r.as_array()).cloned().unwrap_or_default();
            for s in &results {
                if let Some(rid) = s.get("DeliveryID").and_then(|v| v.as_str()).filter(|v| !v.is_empty()) {
                    out.push(json!({
                        "id": rid, "title": unescape(&s["SessionName"]), "duration": s.get("Duration").cloned().unwrap_or(Value::Null),
                        "start": parse_date(s.get("StartTime").unwrap_or(&Value::Null)), "has_captions": s.get("HasCaptions").map(truthy).unwrap_or(false),
                        "folder_id": s.get("FolderID").cloned().unwrap_or(Value::Null), "folder_name": unescape(&s["FolderName"]),
                        "url": viewer_url(rid, &self.host),
                    }));
                }
            }
            let total = d.get("TotalNumber").and_then(|v| v.as_u64()).filter(|t| *t > 0).map(|t| t as usize).unwrap_or(out.len());
            page += 1;
            if results.is_empty() || out.len() >= total || page >= max_pages {
                return Ok(out);
            }
        }
    }

    pub async fn folder_sessions(&self, folder_id: &str) -> Result<Vec<Value>> {
        self.sessions(Some(folder_id), None, 20).await
    }

    /// Recordings you can see whose metadata matches `query`, across all folders.
    pub async fn search_sessions(&self, query: &str) -> Result<Vec<Value>> {
        self.sessions(None, Some(query), 3).await
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Null => false,
        _ => true,
    }
}

/// The demo's fake Panopto.
pub struct DemoPanopto;

impl DemoPanopto {
    fn recs() -> Vec<Value> {
        demo::FIX.recordings.as_array().cloned().unwrap_or_default()
    }
    pub async fn search_sessions(&self, query: &str) -> Result<Vec<Value>> {
        tokio::time::sleep(Duration::from_millis(300)).await;
        Ok(if query.contains("373") { Self::recs() } else { vec![] })
    }
    pub async fn folder_sessions(&self, folder_id: &str) -> Result<Vec<Value>> {
        tokio::time::sleep(Duration::from_millis(300)).await;
        Ok(Self::recs()
            .into_iter()
            .filter(|r| r["folder_id"] == folder_id)
            .map(|mut r| {
                r["url"] = json!(demo::demo_recording_url(r["id"].as_str().unwrap_or("")));
                r
            })
            .collect())
    }
    pub async fn delivery_info(&self, rid: &str) -> Result<Value> {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let mut r = Self::recs().into_iter().find(|x| x["id"] == rid).ok_or_else(|| Error::NotFound(format!("no recording {rid}")))?;
        r["languages"] = json!([0]);
        r["url"] = json!(demo::demo_recording_url(rid));
        r["summary"] = json!("Covers binary heaps, percolate up/down, and heapify in O(n).");
        r["chapters"] = json!([{"Title": "Heap property", "Time": 0}, {"Title": "Percolate down", "Time": 1260}]);
        r["key_points"] = json!(["Heaps are complete trees", "Build-heap is linear"]);
        Ok(r)
    }
    pub async fn captions_srt(&self, _rid: &str, _language: i64) -> Result<String> {
        tokio::time::sleep(Duration::from_millis(200)).await;
        Ok("1\n00:00:01,000 --> 00:00:04,000\nAlright, let's get started with heaps.\n\n2\n00:01:05,000 --> 00:01:09,000\nA heap is a complete binary tree.\n".into())
    }
}

/// Panopto, or the demo's fake one.
pub enum Panopto {
    Real(PanoptoClient),
    Demo(DemoPanopto),
}

impl Panopto {
    pub fn from_env() -> Panopto {
        if config::is_demo() { Panopto::Demo(DemoPanopto) } else { Panopto::Real(PanoptoClient::new(&config::panopto_host())) }
    }
    pub async fn delivery_info(&self, rid: &str) -> Result<Value> {
        match self {
            Panopto::Real(c) => c.delivery_info(rid).await,
            Panopto::Demo(d) => d.delivery_info(rid).await,
        }
    }
    pub async fn captions_srt(&self, rid: &str, language: i64) -> Result<String> {
        match self {
            Panopto::Real(c) => c.captions_srt(rid, language).await,
            Panopto::Demo(d) => d.captions_srt(rid, language).await,
        }
    }
    pub async fn folder_sessions(&self, folder_id: &str) -> Result<Vec<Value>> {
        match self {
            Panopto::Real(c) => c.folder_sessions(folder_id).await,
            Panopto::Demo(d) => d.folder_sessions(folder_id).await,
        }
    }
    pub async fn search_sessions(&self, query: &str) -> Result<Vec<Value>> {
        match self {
            Panopto::Real(c) => c.search_sessions(query).await,
            Panopto::Demo(d) => d.search_sessions(query).await,
        }
    }
}

fn clock(t: i64) -> String {
    if t >= 3600 { format!("{}:{:02}:{:02}", t / 3600, t % 3600 / 60, t % 60) } else { format!("{:02}:{:02}", t / 60, t % 60) }
}

/// SRT captions → '[mm:ss] text' paragraphs, one per ~`every` seconds.
pub fn srt_to_paragraphs(srt: &str, every: i64) -> String {
    static BLANK: Lazy<Regex> = Lazy::new(|| Regex::new(r"\n\s*\n").unwrap());
    static TS: Lazy<Regex> = Lazy::new(|| Regex::new(r"(\d+):(\d+):(\d+)").unwrap());
    let srt = srt.replace('\r', "");
    let mut paras: Vec<(i64, String)> = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    let mut cur_start: Option<i64> = None;
    for block in BLANK.split(&srt) {
        let lines: Vec<&str> = block.trim().split('\n').filter(|l| !l.trim().is_empty()).collect();
        let ts = lines.iter().position(|l| l.contains("-->"));
        let text = lines
            .iter()
            .enumerate()
            .filter(|(i, l)| Some(*i) != ts && !l.trim().chars().all(|c| c.is_ascii_digit()))
            .map(|(_, l)| l.trim())
            .collect::<Vec<_>>()
            .join(" ");
        let (Some(ts), false) = (ts, text.is_empty()) else { continue };
        let Some(m) = TS.captures(lines[ts]) else { continue };
        let t = m[1].parse::<i64>().unwrap_or(0) * 3600 + m[2].parse::<i64>().unwrap_or(0) * 60 + m[3].parse::<i64>().unwrap_or(0);
        match cur_start {
            None => cur_start = Some(t),
            Some(s) if t - s >= every => {
                paras.push((s, cur.join(" ")));
                cur.clear();
                cur_start = Some(t);
            }
            _ => {}
        }
        cur.push(text);
    }
    if !cur.is_empty() {
        paras.push((cur_start.unwrap_or(0), cur.join(" ")));
    }
    paras.iter().map(|(t, txt)| format!("[{}] {txt}", clock(*t))).collect::<Vec<_>>().join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(parse_date(&json!("/Date(1696000000000)/")), json!("2023-09-29T15:06:40+00:00"));
        assert_eq!(parse_date(&json!(1696000000.0 + WINDOWS_EPOCH_OFFSET)), json!("2023-09-29T15:06:40+00:00"));
        assert_eq!(parse_date(&Value::Null), Value::Null);
        assert_eq!(parse_date(&json!("2024-01-01T00:00:00Z")), json!("2024-01-01T00:00:00Z"));
    }

    #[test]
    fn srt() {
        let s = "1\n00:00:01,000 --> 00:00:04,000\nAlright, let's get started.\n\n2\n00:01:05,000 --> 00:01:09,000\nA heap is a tree.\n";
        assert_eq!(srt_to_paragraphs(s, 60), "[00:01] Alright, let's get started.\n\n[01:05] A heap is a tree.");
    }

    #[test]
    fn links() {
        let t = r#"https://x.hosted.panopto.com/Panopto/Pages/Viewer.aspx?id=11111111-0000-0000-0000-000000000001""#;
        assert_eq!(&LINK_RE.captures(t).unwrap()[1], "11111111-0000-0000-0000-000000000001");
        assert_eq!(html_unescape("A &amp; B &#39;x&#39;"), "A & B 'x'");
    }
}
