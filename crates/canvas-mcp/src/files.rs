//! Canvas files cached on disk, shared by the app, the MCP server and notebook uploads.

use std::path::PathBuf;
use std::sync::Arc;

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;
use sha1::{Digest, Sha1};

use crate::engine::Engine;
use crate::{Result, config};

/// The background sync downloads course files up to this size.
pub const WARM_MAX_BYTES: i64 = 50 * 1024 * 1024;

static UNSAFE: Lazy<Regex> = Lazy::new(|| Regex::new(r"[^\w.\- ()]").unwrap());

/// A file name that's safe on every platform (at most 150 characters).
pub fn safe_name(name: &str) -> String {
    UNSAFE.replace_all(name, "_").chars().take(150).collect()
}

fn py_str(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "None".into(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// A file's metadata, from any cached course file list if possible (no network).
pub async fn file_meta(engine: &Arc<Engine>, fid: &str) -> Result<Value> {
    let courses = engine.cached_list("courses", &[]).into_iter().chain(engine.cached_list("past_courses", &[]));
    for c in courses {
        let cid = c["id"].to_string();
        if let Some(files) = engine.cached("files", &[cid]) {
            for f in files["files"].as_array().into_iter().flatten() {
                if f["id"].to_string() == fid && f.get("url").map(|u| !u.is_null()).unwrap_or(false) {
                    return Ok(f.clone());
                }
            }
        }
    }
    engine.get_json("file", &[fid.to_string()]).await
}

/// Where a file with this metadata is (or will be) kept.
pub fn blob_path(fid: &str, meta: &Value) -> PathBuf {
    let stamp = hex::encode(Sha1::digest(py_str(meta.get("updated_at")).as_bytes()));
    let name = meta.get("display_name").and_then(|v| v.as_str()).unwrap_or("file");
    config::blob_dir().join("files").join(format!("{fid}-{}-{}", &stamp[..8], safe_name(name)))
}

/// Local path of a Canvas file (downloaded on first use) and its metadata.
pub async fn file_path(engine: &Arc<Engine>, fid: &str, meta: Option<Value>) -> Result<(PathBuf, Value)> {
    let meta = match meta {
        Some(m) => m,
        None => file_meta(engine, fid).await?,
    };
    let path = blob_path(fid, &meta);
    if !path.exists() {
        let url = meta.get("url").and_then(|u| u.as_str()).unwrap_or_default().to_string();
        let resp = engine.client.download(&url).await?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = PathBuf::from(format!("{}.part", path.display()));
        std::fs::write(&tmp, &resp.bytes)?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok((path, meta))
}

/// Download the active courses' files, so opening one never waits on the network.
pub async fn warm_files(engine: Arc<Engine>) {
    let sem = Arc::new(tokio::sync::Semaphore::new(3));
    let mut metas = Vec::new();
    for c in engine.cached_list("courses", &[]) {
        if let Some(files) = engine.cached("files", &[c["id"].to_string()]) {
            for f in files["files"].as_array().into_iter().flatten() {
                if f.get("url").map(|u| !u.is_null()).unwrap_or(false) && f["size"].as_i64().unwrap_or(0) <= WARM_MAX_BYTES {
                    metas.push(f.clone());
                }
            }
        }
    }
    let jobs = metas.into_iter().map(|meta| {
        let (engine, sem) = (engine.clone(), sem.clone());
        async move {
            let _permit = sem.acquire().await;
            let fid = meta["id"].to_string();
            if let Err(e) = file_path(&engine, &fid, Some(meta)).await {
                log::info!("couldn't warm file {fid}: {e}");
            }
        }
    });
    futures::future::join_all(jobs).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_names() {
        assert_eq!(safe_name("lecture 01 (v2).pdf"), "lecture 01 (v2).pdf");
        assert_eq!(safe_name("a/b\\c:d?.pdf"), "a_b_c_d_.pdf");
        assert_eq!(safe_name("Übung.pdf"), "Übung.pdf");
    }

    #[test]
    fn stamp_matches_python() {
        // hashlib.sha1(b"None").hexdigest()[:8] == "6eef6648"
        let p = blob_path("5", &serde_json::json!({"display_name": "x.pdf"}));
        assert!(p.to_string_lossy().ends_with("5-6eef6648-x.pdf"), "{}", p.display());
    }
}
