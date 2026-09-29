//! Settings, from environment variables or ~/.canvas-mcp/config.toml (environment wins).
//!
//!   config.toml key    environment variable   meaning
//!   canvas_url         CANVAS_BASE_URL        your school's Canvas, e.g. "https://canvas.example.edu" (required)
//!   panopto_host       PANOPTO_HOST           your school's Panopto, e.g. "example.hosted.panopto.com" (optional)
//!   firefox_profile    FIREFOX_PROFILE        Firefox profile folder (default: the one Firefox opens)
//!   download_dir       CANVAS_DOWNLOAD_DIR    where download_file saves (default: ~/Downloads/canvas)
//!   google_authuser    GOOGLE_AUTHUSER        which signed-in Google account NotebookLM uses (default: 0)
//!   anki_url           ANKI_CONNECT_URL       AnkiConnect's address (default: the port in AnkiConnect's settings)
//!   anki_api_key       ANKI_CONNECT_KEY       AnkiConnect's apiKey, if you set one in its config (optional)
//!   anki_parent_deck   ANKI_PARENT_DECK       the deck course decks are created under (default: "Canvas")
//!   permissions        -                      what you've allowed the app to use (see below)
//!
//! Nothing reads your Firefox cookies for a site, or talks to Anki, until you allow it. Each allowed
//! thing is one entry in `permissions`: a site's host name (your Canvas and Panopto hosts), "google"
//! (the Google cookies NotebookLM needs), "anki", or "claude". The app asks when it first needs one,
//! and Settings → Permissions takes them back; `canvas-check` asks on the command line.
//!
//! The data folder itself is ~/.canvas-mcp, or CANVAS_MCP_DIR.

use std::path::{Path, PathBuf};
use std::sync::RwLock;

use once_cell::sync::Lazy;
use serde_json::{Map, Value};

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

pub fn expand_user(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        home().join(rest)
    } else if p == "~" {
        home()
    } else {
        PathBuf::from(p)
    }
}

/// The data folder: CANVAS_MCP_DIR or ~/.canvas-mcp. Read once, so set CANVAS_MCP_DIR (e.g. for
/// the demo) before anything touches the configuration.
pub static DATA_DIR: Lazy<PathBuf> = Lazy::new(|| match std::env::var("CANVAS_MCP_DIR") {
    Ok(d) if !d.is_empty() => expand_user(&d),
    _ => home().join(".canvas-mcp"),
});

pub fn data_dir() -> &'static Path {
    &DATA_DIR
}

pub fn config_path() -> PathBuf {
    DATA_DIR.join("config.toml")
}

pub fn blob_dir() -> PathBuf {
    DATA_DIR.join("blobs")
}

pub fn is_demo() -> bool {
    std::env::var("CANVAS_DEMO").map(|v| !v.is_empty()).unwrap_or(false)
}

fn toml_to_json(v: toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::String(s),
        toml::Value::Integer(i) => Value::from(i),
        toml::Value::Float(f) => Value::from(f),
        toml::Value::Boolean(b) => Value::Bool(b),
        toml::Value::Datetime(d) => Value::String(d.to_string()),
        toml::Value::Array(a) => Value::Array(a.into_iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => Value::Object(t.into_iter().map(|(k, v)| (k, toml_to_json(v))).collect()),
    }
}

/// config.toml as JSON values, in file order (empty when missing or unreadable).
pub fn read_file() -> Map<String, Value> {
    let Ok(text) = std::fs::read_to_string(config_path()) else { return Map::new() };
    match text.parse::<toml::Table>() {
        Ok(t) => t.into_iter().map(|(k, v)| (k, toml_to_json(v))).collect(),
        Err(e) => {
            log::warn!("couldn't read {}: {e}", config_path().display());
            Map::new()
        }
    }
}

/// Python's truthiness, for `os.environ.get(env) or file.get(key) or default`.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn get_str(file: &Map<String, Value>, key: &str, env_name: &str) -> Option<String> {
    if let Some(v) = env(env_name) {
        return Some(v);
    }
    match file.get(key) {
        Some(v) if truthy(v) => Some(match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        }),
        _ => None,
    }
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub canvas_url: String,
    pub panopto_host: String,
    pub firefox_profile: Option<String>,
    pub download_dir: PathBuf,
    pub google_authuser: i64,
    pub anki_url: Option<String>,
    pub anki_api_key: Option<String>,
    pub anki_parent_deck: String,
}

impl Settings {
    fn load() -> Settings {
        let f = read_file();
        Settings {
            canvas_url: get_str(&f, "canvas_url", "CANVAS_BASE_URL").unwrap_or_default().trim_end_matches('/').to_string(),
            panopto_host: get_str(&f, "panopto_host", "PANOPTO_HOST").unwrap_or_default(),
            firefox_profile: get_str(&f, "firefox_profile", "FIREFOX_PROFILE").or_else(|| env("CANVAS_FIREFOX_PROFILE")),
            download_dir: get_str(&f, "download_dir", "CANVAS_DOWNLOAD_DIR")
                .map(|d| expand_user(&d))
                .unwrap_or_else(|| home().join("Downloads").join("canvas")),
            google_authuser: get_str(&f, "google_authuser", "GOOGLE_AUTHUSER").and_then(|v| v.trim().parse().ok()).unwrap_or(0),
            anki_url: get_str(&f, "anki_url", "ANKI_CONNECT_URL"),
            anki_api_key: get_str(&f, "anki_api_key", "ANKI_CONNECT_KEY"),
            anki_parent_deck: get_str(&f, "anki_parent_deck", "ANKI_PARENT_DECK").unwrap_or_else(|| "Canvas".into()),
        }
    }
}

static SETTINGS: Lazy<RwLock<Settings>> = Lazy::new(|| RwLock::new(Settings::load()));

/// The settings as this process last read or saved them.
pub fn settings() -> Settings {
    SETTINGS.read().unwrap().clone()
}

pub fn canvas_url() -> String {
    SETTINGS.read().unwrap().canvas_url.clone()
}
pub fn panopto_host() -> String {
    SETTINGS.read().unwrap().panopto_host.clone()
}
pub fn google_authuser() -> i64 {
    SETTINGS.read().unwrap().google_authuser
}

/// Read from the file each time, so a change made in the app applies to a running MCP server.
pub fn permissions() -> Vec<String> {
    match read_file().get("permissions") {
        Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
        _ => Vec::new(),
    }
}

pub fn allowed(what: &str) -> bool {
    permissions().iter().any(|p| p == what)
}

pub fn allow(what: &str) {
    if !allowed(what) {
        let mut p = permissions();
        p.push(what.to_string());
        let _ = save(vec![("permissions", Value::from(p))]);
    }
}

pub fn revoke(what: &str) {
    if allowed(what) {
        let p: Vec<String> = permissions().into_iter().filter(|p| p != what).collect();
        let _ = save(vec![("permissions", Value::from(p))]);
    }
}

/// Write settings to config.toml (keeping the others) and apply them to this process.
pub fn save(values: Vec<(&str, Value)>) -> std::io::Result<()> {
    let mut file = read_file();
    for (k, v) in &values {
        file.insert((*k).to_string(), v.clone());
    }
    let mut lines = vec!["# canvas-mcp settings (see canvas_mcp/config.py)".to_string()];
    for (k, v) in &file {
        // A JSON string, number or list of strings is valid TOML.
        lines.push(format!("{k} = {}", serde_json::to_string(v).unwrap_or_default()));
    }
    std::fs::create_dir_all(&*DATA_DIR)?;
    let tmp = config_path().with_extension("tmp");
    std::fs::write(&tmp, lines.join("\n") + "\n")?;
    std::fs::rename(&tmp, config_path())?;
    *SETTINGS.write().unwrap() = Settings::load();
    Ok(())
}

/// The Canvas base URL, re-reading the file in case it was set up (in the app) since we started.
pub fn require_canvas_url() -> crate::Result<String> {
    let mut url = canvas_url();
    if url.is_empty() && env("CANVAS_BASE_URL").is_none() {
        url = match read_file().get("canvas_url") {
            Some(Value::String(s)) => s.trim_end_matches('/').to_string(),
            _ => String::new(),
        };
        if !url.is_empty() {
            SETTINGS.write().unwrap().canvas_url = url.clone();
        }
    }
    if url.is_empty() {
        return Err(crate::Error::NotConfigured(format!(
            "Canvas isn't set up yet. Open the Canvas app to set it up, or add canvas_url = \"https://canvas.your-school.edu\" to {} (or set CANVAS_BASE_URL).",
            config_path().display()
        )));
    }
    Ok(url)
}

/// Make a file or folder readable by you only (no-op on Windows, where the profile folder is private).
pub fn private(path: &Path, dir: bool) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(if dir { 0o700 } else { 0o600 }));
    }
    #[cfg(not(unix))]
    {
        let _ = (path, dir);
    }
}
