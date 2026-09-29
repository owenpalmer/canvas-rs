//! Opt-in launch timing: with CANVAS_TIMING=1, the app appends one JSON line per launch to
//! ~/.canvas-mcp/launch-timing.log, with the milliseconds from process start to each mark.

use std::io::Write;
use std::sync::Mutex;
use std::time::Instant;

use once_cell::sync::Lazy;

static ENABLED: Lazy<bool> = Lazy::new(|| std::env::var("CANVAS_TIMING").map(|v| v.trim() == "1").unwrap_or(false));
static ORIGIN: Lazy<Instant> = Lazy::new(Instant::now);
static MARKS: Mutex<Vec<(String, f64)>> = Mutex::new(Vec::new());
static WRITTEN: Mutex<bool> = Mutex::new(false);

pub fn mark(name: &str) {
    Lazy::force(&ORIGIN);
    if *ENABLED {
        let ms = (ORIGIN.elapsed().as_secs_f64() * 10000.0).round() / 10.0;
        MARKS.lock().unwrap().push((name.to_string(), ms));
    }
}

/// Append this launch's marks to the log, once.
pub fn write() {
    if !*ENABLED {
        return;
    }
    let mut w = WRITTEN.lock().unwrap();
    if *w {
        return;
    }
    *w = true;
    let marks: serde_json::Map<String, serde_json::Value> = MARKS.lock().unwrap().iter().map(|(k, v)| (k.clone(), serde_json::json!(v))).collect();
    let record = serde_json::json!({
        "at": chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
        "frozen": true,
        "launcher": false,
        "native": "rust",
        "marks": marks,
    });
    let log = canvas_mcp::config::home().join(".canvas-mcp").join("launch-timing.log");
    if let Some(dir) = log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
        let _ = writeln!(f, "{record}");
    }
}
