//! Read cookies from your everyday Firefox profile, for Canvas, Panopto, and Google.
//!
//! Firefox keeps persistent cookies in cookies.sqlite (recent writes may still be in the -wal file),
//! and session cookies (no expiry) only in the session store: sessionstore-backups/recovery.jsonlz4
//! while Firefox runs, sessionstore.jsonlz4 after a clean shutdown. The database is copied so it can
//! be read without touching Firefox's lock. Only the default cookie jar is used (not container tabs
//! or partitioned third-party cookies), and only cookies for the domains asked for are returned.
//!
//!   load_cookie_records(domains)  full cookies (domain, path, expiry, flags), e.g. for NotebookLM
//!   load_cookies(host)            {name: value} of the cookies Firefox would send to host
//!
//! Both refuse (NotPermitted) unless you've allowed that site (config::allow): a site's own host
//! name, or "google" for the Google domains NotebookLM needs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use serde::Serialize;
use serde_json::Value;

use crate::{Error, Result, config};

pub const GOOGLE_DOMAINS: &[&str] = &["google.com", "googleusercontent.com", "notebooklm.google", "youtube.com"];

pub fn firefox_roots() -> Vec<PathBuf> {
    let home = config::home();
    let appdata = std::env::var("APPDATA").map(PathBuf::from).unwrap_or_else(|_| home.join("AppData/Roaming"));
    vec![
        home.join(".mozilla/firefox"),
        home.join("snap/firefox/common/.mozilla/firefox"),
        home.join(".var/app/org.mozilla.firefox/.mozilla/firefox"),
        appdata.join("Mozilla/Firefox"), // Windows
        home.join("Library/Application Support/Firefox"), // macOS
    ]
}

fn samesite(v: i64) -> &'static str {
    match v {
        1 => "Lax",
        2 => "Strict",
        _ => "None",
    }
}

/// A cookie as Playwright's storage state writes it (what the NotebookLM client reads).
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub expires: i64,
    #[serde(rename = "httpOnly")]
    pub http_only: bool,
    pub secure: bool,
    #[serde(rename = "sameSite")]
    pub same_site: String,
}

/// The permission that covers reading cookies for a domain.
pub fn permission_for(domain: &str) -> String {
    if GOOGLE_DOMAINS.contains(&domain) { "google".into() } else { domain.to_string() }
}

pub fn require_permission(domains: &[&str]) -> Result<()> {
    let granted = config::permissions();
    for d in domains {
        let p = permission_for(d);
        if !granted.contains(&p) {
            return Err(Error::NotPermitted(p));
        }
    }
    Ok(())
}

/// A minimal INI reader for profiles.ini: sections in order, keys as written.
fn parse_ini(text: &str) -> Vec<(String, BTreeMap<String, String>)> {
    let mut out: Vec<(String, BTreeMap<String, String>)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            out.push((line[1..line.len() - 1].to_string(), BTreeMap::new()));
        } else if let Some((k, v)) = line.split_once('=').or_else(|| line.split_once(':')) {
            if let Some((_, sec)) = out.last_mut() {
                sec.insert(k.trim().to_lowercase(), v.trim().to_string());
            }
        }
    }
    out
}

pub fn find_profile() -> Result<PathBuf> {
    if let Some(p) = config::settings().firefox_profile {
        return Ok(config::expand_user(&p));
    }
    // Each Firefox install (regular, snap, flatpak) has its default profile; a machine can have
    // several, e.g. an old ~/.mozilla left behind after switching to the snap. The one in use is
    // the one whose cookies were written last.
    let found: Vec<PathBuf> = firefox_roots().iter().filter_map(|r| default_profile(r)).collect();
    found
        .into_iter()
        .max_by_key(|p| cookies_written(p))
        .ok_or_else(|| Error::Io(format!("No Firefox profile found; set firefox_profile in {}", config::config_path().display())))
}

/// A Firefox install's default profile, from its profiles.ini.
fn default_profile(root: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(root.join("profiles.ini")).ok()?;
    let ini = parse_ini(&text);
    // [Install*] Default= is the profile Firefox actually opens; fall back to Default=1.
    for (name, sec) in &ini {
        if name.starts_with("Install") {
            if let Some(d) = sec.get("default").filter(|d| !d.is_empty()) {
                return Some(root.join(d));
            }
        }
    }
    for (_, sec) in &ini {
        if sec.get("default").map(|d| d == "1").unwrap_or(false) {
            if let Some(p) = sec.get("path").filter(|p| !p.is_empty()) {
                return Some(root.join(p));
            }
        }
    }
    None
}

/// When a profile's cookies were last written (cookies.sqlite or its write-ahead log).
fn cookies_written(profile: &Path) -> std::time::SystemTime {
    ["cookies.sqlite", "cookies.sqlite-wal"]
        .iter()
        .filter_map(|f| std::fs::metadata(profile.join(f)).and_then(|m| m.modified()).ok())
        .max()
        .unwrap_or(std::time::UNIX_EPOCH)
}

// Firefox's "Open previous windows and tabs" (Settings → General → Startup) maps to the pref
// browser.startup.page = 3. Canvas/SSO login cookies are session cookies (no expiry), and with this
// off Firefox drops them when it closes, so an at-rest read of cookies.sqlite finds nothing.
static STARTUP_RESTORE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"user_pref\(\s*"browser\.startup\.page"\s*,\s*(\d+)\s*\)"#).unwrap());

pub const RESTORE_HINT: &str = "Firefox is set to clear session cookies when it closes, so your Canvas login does not persist. \
Turn on Settings → General → Startup → \"Open previous windows and tabs\", then log into Canvas again.";

/// Is Firefox set to restore the previous session (browser.startup.page = 3)? None when prefs.js
/// can't be read. The Firefox default (no pref written) doesn't persist session cookies: false.
pub fn session_restore_enabled(profile: Option<&Path>) -> Option<bool> {
    let profile = match profile {
        Some(p) => p.to_path_buf(),
        None => find_profile().ok()?,
    };
    let bytes = std::fs::read(profile.join("prefs.js")).ok()?;
    let prefs = String::from_utf8_lossy(&bytes);
    Some(STARTUP_RESTORE.captures(&prefs).map(|m| &m[1] == "3").unwrap_or(false))
}

/// The "turn on session restore" message when that setting is off, else "".
pub fn restore_hint(profile: Option<&Path>) -> &'static str {
    if session_restore_enabled(profile) == Some(false) { RESTORE_HINT } else { "" }
}

/// A cookie belongs to `domain` if it's for that domain, a subdomain of it, or a parent of it
/// (an .example.edu cookie is sent to canvas.example.edu).
fn related(cookie_host: &str, domain: &str) -> bool {
    let h = cookie_host.trim_start_matches('.');
    h == domain || h.ends_with(&format!(".{domain}")) || domain.ends_with(&format!(".{h}"))
}

fn wanted(cookie_host: &str, domains: &[&str]) -> bool {
    domains.iter().any(|d| related(cookie_host, d))
}

pub fn from_sqlite(profile: &Path, domains: &[&str]) -> Result<Vec<Cookie>> {
    let src = profile.join("cookies.sqlite");
    if !src.exists() {
        return Ok(Vec::new());
    }
    let tmp = tempfile::tempdir()?;
    config::private(tmp.path(), true);
    for suffix in ["", "-wal", "-shm"] {
        let from = profile.join(format!("cookies.sqlite{suffix}"));
        if from.exists() {
            std::fs::copy(&from, tmp.path().join(format!("cookies.sqlite{suffix}")))?;
        }
    }
    let con = rusqlite::Connection::open(tmp.path().join("cookies.sqlite"))?;
    let mut stmt = con.prepare("SELECT host, name, value, path, expiry, isSecure, isHttpOnly, sameSite, originAttributes FROM moz_cookies")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0).unwrap_or_default(),
            r.get::<_, String>(1).unwrap_or_default(),
            r.get::<_, String>(2).unwrap_or_default(),
            r.get::<_, Option<String>>(3).unwrap_or_default(),
            r.get::<_, Option<i64>>(4).unwrap_or_default(),
            r.get::<_, Option<i64>>(5).unwrap_or_default(),
            r.get::<_, Option<i64>>(6).unwrap_or_default(),
            r.get::<_, Option<i64>>(7).unwrap_or_default(),
            r.get::<_, Option<String>>(8).unwrap_or_default(),
        ))
    })?;
    let mut out = Vec::new();
    for row in rows.flatten() {
        let (host, name, value, path, expiry, secure, http_only, same_site, origin) = row;
        if origin.as_deref().map(|o| !o.is_empty()).unwrap_or(false) || !wanted(&host, domains) {
            continue;
        }
        let mut expiry = expiry.unwrap_or(0);
        if expiry > 100_000_000_000 {
            expiry /= 1000; // some Firefox versions store ms
        }
        out.push(Cookie {
            name,
            value,
            domain: host,
            path: path.filter(|p| !p.is_empty()).unwrap_or_else(|| "/".into()),
            expires: if expiry != 0 { expiry } else { -1 },
            http_only: http_only.unwrap_or(0) != 0,
            secure: secure.unwrap_or(0) != 0,
            same_site: samesite(same_site.unwrap_or(0)).into(),
        });
    }
    drop(stmt);
    drop(con);
    Ok(out)
}

/// The newest session store: the live recovery file, or the one written at a clean shutdown.
fn session_file(profile: &Path) -> Option<PathBuf> {
    [
        profile.join("sessionstore-backups").join("recovery.jsonlz4"),
        profile.join("sessionstore-backups").join("recovery.baklz4"),
        profile.join("sessionstore.jsonlz4"),
    ]
    .into_iter()
    .filter_map(|p| p.metadata().and_then(|m| m.modified()).ok().map(|t| (t, p)))
    .max_by_key(|(t, _)| *t)
    .map(|(_, p)| p)
}

/// Decode a mozLz4 file (Firefox's "mozLz40\0" + size + LZ4 block).
pub fn read_mozlz4(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 12 || &data[..8] != b"mozLz40\0" {
        return None;
    }
    lz4_flex::block::decompress_size_prepended(&data[8..]).ok()
}

pub fn from_sessionstore(profile: &Path, domains: &[&str]) -> Result<Vec<Cookie>> {
    let Some(path) = session_file(profile) else { return Ok(Vec::new()) };
    let data = std::fs::read(&path)?;
    let Some(json) = read_mozlz4(&data) else { return Ok(Vec::new()) };
    let session: Value = serde_json::from_slice(&json)?;
    let mut out = Vec::new();
    for c in session.get("cookies").and_then(|c| c.as_array()).into_iter().flatten() {
        let host = c.get("host").and_then(|v| v.as_str()).unwrap_or("");
        let ctx = c.get("originAttributes").and_then(|o| o.get("userContextId")).and_then(|v| v.as_i64()).unwrap_or(0);
        if !wanted(host, domains) || ctx != 0 {
            continue;
        }
        let flag = |k: &str| c.get(k).map(|v| v.as_bool().unwrap_or_else(|| v.as_i64().unwrap_or(0) != 0)).unwrap_or(false);
        out.push(Cookie {
            name: c.get("name").and_then(|v| v.as_str()).unwrap_or("").into(),
            value: c.get("value").and_then(|v| v.as_str()).unwrap_or("").into(),
            domain: host.into(),
            path: c.get("path").and_then(|v| v.as_str()).unwrap_or("/").into(),
            expires: -1,
            http_only: flag("httponly"),
            secure: flag("secure"),
            same_site: samesite(c.get("sameSite").and_then(|v| v.as_i64()).unwrap_or(0)).into(),
        });
    }
    Ok(out)
}

/// Full cookie records for the given domain(s), session cookies included.
pub fn load_cookie_records(domains: &[&str], profile: Option<&Path>) -> Result<Vec<Cookie>> {
    require_permission(domains)?;
    let profile = match profile {
        Some(p) => p.to_path_buf(),
        None => find_profile()?,
    };
    // sqlite is written more often; prefer it. Insertion order as Python's dict update.
    let mut keys: Vec<(String, String, String)> = Vec::new();
    let mut merged: std::collections::HashMap<(String, String, String), Cookie> = Default::default();
    for c in from_sessionstore(&profile, domains).unwrap_or_default().into_iter().chain(from_sqlite(&profile, domains)?) {
        let k = (c.domain.clone(), c.path.clone(), c.name.clone());
        if !merged.contains_key(&k) {
            keys.push(k.clone());
        }
        merged.insert(k, c);
    }
    Ok(keys.into_iter().filter_map(|k| merged.remove(&k)).collect())
}

/// (name, value) of the cookies Firefox would send to `host` (more specific domains win).
pub fn load_cookies(host: &str, profile: Option<&Path>) -> Result<BTreeMap<String, String>> {
    let mut records: Vec<Cookie> = load_cookie_records(&[host], profile)?
        .into_iter()
        .filter(|c| {
            let h = c.domain.trim_start_matches('.');
            host == h || host.ends_with(&format!(".{h}"))
        })
        .collect();
    records.sort_by_key(|c| c.domain.trim_start_matches('.').len());
    Ok(records.into_iter().map(|c| (c.name, c.value)).collect())
}

/// A Cookie header value.
pub fn cookie_header(cookies: &BTreeMap<String, String>) -> String {
    cookies.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ")
}

#[cfg(test)]
mod tests {
    #[test]
    fn picks_the_profile_in_use() {
        // two installs' profiles: the one whose cookies were written last is the one in use
        let dir = tempfile::tempdir().unwrap();
        let (old, live) = (dir.path().join("old"), dir.path().join("live"));
        for p in [&old, &live] {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(old.join("cookies.sqlite"), b"x").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(live.join("cookies.sqlite-wal"), b"x").unwrap();
        let best = [old.clone(), live.clone()].into_iter().max_by_key(|p| cookies_written(p)).unwrap();
        assert_eq!(best, live);
        assert_eq!(cookies_written(&dir.path().join("none")), std::time::UNIX_EPOCH);
    }

    use super::*;

    #[test]
    fn related_domains() {
        assert!(related(".example.edu", "canvas.example.edu"));
        assert!(related("canvas.example.edu", "canvas.example.edu"));
        assert!(related("sub.canvas.example.edu", "canvas.example.edu"));
        assert!(!related("other.edu", "canvas.example.edu"));
        assert!(!related("xexample.edu", "example.edu"));
    }

    #[test]
    fn mozlz4_roundtrip() {
        let body = br#"{"cookies":[{"host":".example.edu","name":"canvas_session","value":"abc","path":"/"}]}"#;
        let mut data = b"mozLz40\0".to_vec();
        data.extend(lz4_flex::block::compress_prepend_size(body));
        assert_eq!(read_mozlz4(&data).unwrap(), body);
    }

    #[test]
    fn ini_default_profile() {
        let ini = parse_ini("[Install4F96D1932A9F858E]\nDefault=abc.default-release\nLocked=1\n\n[Profile0]\nName=default\nIsRelative=1\nPath=xyz.default\nDefault=1\n");
        assert_eq!(ini[0].1.get("default").unwrap(), "abc.default-release");
        assert_eq!(ini[1].1.get("path").unwrap(), "xyz.default");
    }

    #[test]
    fn restore_pref() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("prefs.js"), "user_pref(\"browser.startup.page\", 3);\n").unwrap();
        assert_eq!(session_restore_enabled(Some(dir.path())), Some(true));
        std::fs::write(dir.path().join("prefs.js"), "user_pref(\"other\", 1);\n").unwrap();
        assert_eq!(session_restore_enabled(Some(dir.path())), Some(false));
        assert_eq!(session_restore_enabled(Some(&dir.path().join("missing"))), None);
    }

    #[test]
    fn sqlite_cookies_filtered_and_ms_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let con = rusqlite::Connection::open(dir.path().join("cookies.sqlite")).unwrap();
        con.execute_batch(
            "CREATE TABLE moz_cookies (host TEXT, name TEXT, value TEXT, path TEXT, expiry INTEGER, isSecure INTEGER, isHttpOnly INTEGER, sameSite INTEGER, originAttributes TEXT);
             INSERT INTO moz_cookies VALUES ('.example.edu','canvas_session','s1','/',1900000000000,1,1,1,'');
             INSERT INTO moz_cookies VALUES ('other.com','x','y','/',0,0,0,0,'');
             INSERT INTO moz_cookies VALUES ('.example.edu','container','c','/',0,0,0,0,'^userContextId=2');",
        )
        .unwrap();
        drop(con);
        let got = from_sqlite(dir.path(), &["canvas.example.edu"]).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "canvas_session");
        assert_eq!(got[0].expires, 1_900_000_000);
        assert_eq!(got[0].same_site, "Lax");
    }
}
