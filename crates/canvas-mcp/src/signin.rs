//! First-run setup and signing in: find your school's Canvas, open Firefox where you log in, and
//! notice when you have (by watching Firefox's cookies), for Canvas and for Google (NotebookLM).
//!
//! Nothing here types or stores a password: you log in in Firefox as usual, and the app reads the
//! resulting cookies the same way it always does (cookies.rs).
//!
//! Firefox writes persistent cookies to cookies.sqlite within a second or so, but session cookies
//! (most schools' Canvas logins) only reach its session store every 15 seconds, so a sign-in can take
//! that long to show up here.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{Value, json};

use crate::cookies::{self, Cookie, GOOGLE_DOMAINS};
use crate::{Result, config};

pub const CANVAS_SESSION_COOKIES: &[&str] = &["canvas_session", "_legacy_normandy_session"];
pub const GOOGLE_SESSION_COOKIES: &[&str] = &["SID", "__Secure-1PSID", "__Secure-3PSID"];
pub const GOOGLE_SIGNIN_URL: &str = "https://notebooklm.google.com/";
pub const FIREFOX_DOWNLOAD: &str = "https://www.mozilla.org/firefox/new/";
const FLATPAK_ID: &str = "org.mozilla.firefox";

// --- finding Canvas ----------------------------------------------------------------------------------
/// Canvas addresses to try for what someone typed: a URL, a host, or a school's short name
/// ("uw" → canvas.uw.edu, uw.instructure.com).
pub fn candidates(text: &str) -> Vec<String> {
    static HOST: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[a-z0-9-]+(\.[a-z0-9-]+)*$").unwrap());
    let text = text.trim().trim_matches('/');
    if text.is_empty() {
        return vec![];
    }
    let full = if text.contains("://") { text.to_string() } else { format!("https://{text}") };
    let host = url::Url::parse(&full).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_default();
    let host = host.to_lowercase().trim_end_matches('.').to_string();
    if !HOST.is_match(&host) {
        return vec![];
    }
    if !host.contains('.') {
        return vec![format!("https://canvas.{host}.edu"), format!("https://{host}.instructure.com")];
    }
    if host.starts_with("canvas.") || host.ends_with(".instructure.com") {
        return vec![format!("https://{host}")];
    }
    vec![format!("https://{host}"), format!("https://canvas.{host}")] // "uw.edu" → canvas.uw.edu
}

/// The Canvas base URL if `url` is a Canvas site, following a redirect to its real address.
pub async fn probe(url: &str, http: &reqwest::Client) -> Option<String> {
    let resp = http.get(format!("{url}/api/v1/users/self")).header("Accept", "application/json").send().await.ok()?;
    // Signed out, Canvas answers 401 {"status": "unauthenticated"} with its x-canvas-meta header;
    // an unknown *.instructure.com name answers 404 "domain not found".
    let final_url = resp.url().clone();
    let status = resp.status().as_u16();
    let meta = resp.headers().contains_key("x-canvas-meta");
    let text = resp.text().await.unwrap_or_default();
    if status == 401 && (meta || text.contains("unauthenticated")) {
        let host = final_url.host_str()?;
        let port = final_url.port().map(|p| format!(":{p}")).unwrap_or_default();
        return Some(format!("{}://{host}{port}", final_url.scheme()));
    }
    None
}

pub async fn find_canvas(text: &str) -> Option<String> {
    let http = reqwest::Client::builder().user_agent(crate::util::USER_AGENT).timeout(Duration::from_secs(8)).build().ok()?;
    for url in candidates(text) {
        if let Some(found) = probe(&url, &http).await {
            return Some(found);
        }
    }
    None
}

// --- Firefox -----------------------------------------------------------------------------------------
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT").unwrap_or(".EXE;.CMD;.BAT".into()).split(';').map(|s| s.to_lowercase()).collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let p = dir.join(format!("{name}{ext}"));
            if p.is_file() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if p.metadata().map(|m| m.permissions().mode() & 0o111 == 0).unwrap_or(true) {
                        continue;
                    }
                }
                return Some(p);
            }
        }
    }
    None
}

pub fn flatpak_has(app: &str) -> bool {
    which("flatpak").is_some()
        && Command::new("flatpak").args(["info", app]).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

#[cfg(windows)]
fn windows_firefox() -> Option<String> {
    for var in ["PROGRAMFILES", "PROGRAMFILES(X86)", "LOCALAPPDATA"] {
        if let Ok(base) = std::env::var(var) {
            let exe = PathBuf::from(base).join("Mozilla Firefox").join("firefox.exe");
            if exe.exists() {
                return Some(exe.to_string_lossy().into_owned());
            }
        }
    }
    None
}

pub fn firefox_command() -> Option<Vec<String>> {
    #[cfg(windows)]
    {
        return windows_firefox().map(|e| vec![e]);
    }
    #[cfg(target_os = "macos")]
    {
        return PathBuf::from("/Applications/Firefox.app").exists().then(|| vec!["open".into(), "-a".into(), "Firefox".into()]);
    }
    #[allow(unreachable_code)]
    {
        for name in ["firefox", "firefox-esr"] {
            if let Some(exe) = which(name) {
                return Some(vec![exe.to_string_lossy().into_owned()]);
            }
        }
        if flatpak_has(FLATPAK_ID) {
            return Some(vec!["flatpak".into(), "run".into(), FLATPAK_ID.into()]);
        }
        None
    }
}

/// Start a program detached from this one (its own session; no console window on Windows).
pub fn spawn_detached(cmd: &[String]) -> std::io::Result<std::process::Child> {
    let mut c = Command::new(&cmd[0]);
    c.args(&cmd[1..]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        c.creation_flags(DETACHED_PROCESS);
    }
    c.current_dir(config::home());
    c.spawn()
}

/// Open url in Firefox (a new tab if it's already running). False if Firefox isn't installed.
pub fn open_in_firefox(url: &str) -> bool {
    let Some(mut cmd) = firefox_command() else { return false };
    cmd.push(url.to_string());
    spawn_detached(&cmd).is_ok()
}

/// Open a file or URL with the system's default app.
pub fn open_default(target: &str) {
    #[cfg(windows)]
    let cmd = vec!["cmd".to_string(), "/C".into(), "start".into(), String::new(), target.to_string()];
    #[cfg(target_os = "macos")]
    let cmd = vec!["open".to_string(), target.to_string()];
    #[cfg(all(unix, not(target_os = "macos")))]
    let cmd = vec!["xdg-open".to_string(), target.to_string()];
    if let Err(e) = spawn_detached(&cmd) {
        log::warn!("couldn't open {target}: {e}");
    }
}

// --- noticing a sign-in --------------------------------------------------------------------------------
/// Changes whenever Firefox writes cookies, so an unchanged profile needn't be read again.
pub fn cookie_files_stamp() -> Result<Vec<(String, u128, u64)>> {
    let profile = cookies::find_profile()?;
    let mut stamp = Vec::new();
    for rel in ["cookies.sqlite", "cookies.sqlite-wal", "sessionstore-backups/recovery.jsonlz4", "sessionstore.jsonlz4"] {
        if let Ok(m) = profile.join(rel).metadata() {
            let t = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos()).unwrap_or(0);
            stamp.push((rel.to_string(), t, m.len()));
        }
    }
    Ok(stamp)
}

type Stamp = Vec<(String, u128, u64)>;

/// Keeps the last answer per site until Firefox's cookie files change (or a minute passes, as a
/// session can also end on the server), so polling while someone logs in doesn't re-read the
/// profile or ask Canvas or Google when nothing has happened.
#[derive(Default)]
pub struct Watcher {
    last: Mutex<HashMap<String, (Stamp, Instant, Value)>>,
}

impl Watcher {
    const MAX_AGE: Duration = Duration::from_secs(60);

    pub fn cached(&self, key: &str) -> Result<(Option<Value>, Stamp)> {
        let stamp = cookie_files_stamp()?;
        let last = self.last.lock().unwrap();
        let hit = last.get(key).filter(|(s, t, _)| *s == stamp && t.elapsed() < Self::MAX_AGE).map(|(_, _, v)| v.clone());
        Ok((hit, stamp))
    }

    pub fn remember(&self, key: &str, stamp: Stamp, result: Value) {
        self.last.lock().unwrap().insert(key.to_string(), (stamp, Instant::now(), result));
    }

    pub fn forget(&self, key: &str) {
        self.last.lock().unwrap().remove(key);
    }
}

/// Whether Firefox has a Canvas session for host, and whether it will survive Firefox closing.
pub fn canvas_cookie_state(host: &str) -> Result<Value> {
    let records: Vec<Cookie> = cookies::load_cookie_records(&[host], None)?.into_iter().filter(|c| CANVAS_SESSION_COOKIES.contains(&c.name.as_str())).collect();
    if records.is_empty() {
        return Ok(json!({"cookie": false}));
    }
    // A login without an expiry is dropped when Firefox closes, unless it restores the session.
    let session_only = records.iter().all(|c| c.expires == -1 || c.expires == 0);
    Ok(json!({"cookie": true, "fragile": session_only && cookies::session_restore_enabled(None) == Some(false)}))
}

/// Google's cookies from Firefox, if signed in to Google there.
pub fn google_cookie_jar() -> Result<Option<Vec<Cookie>>> {
    let records = cookies::load_cookie_records(GOOGLE_DOMAINS, None)?;
    let signed_in = records.iter().any(|c| GOOGLE_SESSION_COOKIES.contains(&c.name.as_str()) && c.domain.trim_start_matches('.') == "google.com");
    Ok(signed_in.then_some(records))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn school_candidates() {
        assert_eq!(candidates("uw"), vec!["https://canvas.uw.edu", "https://uw.instructure.com"]);
        assert_eq!(candidates("canvas.uw.edu"), vec!["https://canvas.uw.edu"]);
        assert_eq!(candidates("https://canvas.uw.edu/courses/1"), vec!["https://canvas.uw.edu"]);
        assert_eq!(candidates("uw.edu"), vec!["https://uw.edu", "https://canvas.uw.edu"]);
        assert_eq!(candidates("school.instructure.com"), vec!["https://school.instructure.com"]);
        assert!(candidates("").is_empty());
        assert!(candidates("not a host!").is_empty());
    }
}
