//! Preferences, persisted in settings.json (the same file and keys as before): changes are
//! batched and written 300 ms after the last one, off the UI thread.

use std::time::{Duration, Instant};

use serde_json::{Map, Value};

pub struct Prefs {
    map: Map<String, Value>,
    pending: Map<String, Value>,
    last: Option<Instant>,
}

impl Prefs {
    pub fn load() -> Prefs {
        Prefs { map: canvas_mcp::prefs::load(), pending: Map::new(), last: None }
    }

    pub fn get(&self, k: &str) -> Option<&Value> {
        self.map.get(k)
    }

    pub fn bool(&self, k: &str, default: bool) -> bool {
        self.map.get(k).and_then(|v| v.as_bool()).unwrap_or(default)
    }

    pub fn f32(&self, k: &str, default: f32) -> f32 {
        self.map.get(k).and_then(|v| v.as_f64()).map(|v| v as f32).unwrap_or(default)
    }

    pub fn str(&self, k: &str) -> String {
        self.map.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string()
    }

    pub fn strings(&self, k: &str) -> Vec<String> {
        self.map
            .get(k)
            .and_then(|v| v.as_array())
            .map(|a| a.iter().map(|x| match x {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            }).collect())
            .unwrap_or_default()
    }

    pub fn set(&mut self, k: &str, v: Value) {
        if self.map.get(k) == Some(&v) {
            return;
        }
        self.map.insert(k.to_string(), v.clone());
        self.pending.insert(k.to_string(), v);
        self.last = Some(Instant::now());
    }

    /// Write pending changes once they've settled (or now, with `force`, e.g. on exit).
    pub fn flush(&mut self, force: bool) {
        let due = self.last.map(|t| t.elapsed() >= Duration::from_millis(300)).unwrap_or(false);
        if self.pending.is_empty() || !(due || force) {
            return;
        }
        let patch = std::mem::take(&mut self.pending);
        self.last = None;
        if force {
            let _ = canvas_mcp::prefs::patch(&patch);
        } else {
            std::thread::spawn(move || {
                let _ = canvas_mcp::prefs::patch(&patch);
            });
        }
    }

    pub fn waiting(&self) -> bool {
        !self.pending.is_empty()
    }
}
