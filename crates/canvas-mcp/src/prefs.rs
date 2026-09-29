//! The app's preferences: one JSON object in settings.json, merged by patch().
//! The same file (and keys) the web UI used, so preferences carry over.

use serde_json::{Map, Value};

use crate::config;

pub fn path() -> std::path::PathBuf {
    config::data_dir().join("settings.json")
}

pub fn load() -> Map<String, Value> {
    std::fs::read_to_string(path()).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v.as_object().cloned()).unwrap_or_default()
}

/// Merge `patch` into settings.json: null removes a key.
pub fn patch(patch: &Map<String, Value>) -> std::io::Result<()> {
    let mut data = load();
    for (k, v) in patch {
        if v.is_null() {
            data.remove(k);
        } else {
            data.insert(k.clone(), v.clone());
        }
    }
    std::fs::create_dir_all(config::data_dir())?;
    let tmp = path().with_extension("tmp");
    let mut buf = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(b"  ");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, fmt);
    serde::Serialize::serialize(&Value::Object(data), &mut ser).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, buf)?;
    std::fs::rename(tmp, path())
}
