//! Textbooks: PDFs on this computer that you read in the app, with checkpoints, a contents sidebar
//! and full-text search. The library is textbooks.json in the data folder; the files stay where
//! they are.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Value, json};
use sha1::{Digest, Sha1};

use crate::util::s;
use crate::{Error, Result, config};

static LOCK: Mutex<()> = Mutex::new(());

/// PDFs at least this big are suggested from Downloads, Documents and the Desktop.
const SUGGEST_MIN_BYTES: u64 = 3 * 1024 * 1024;

fn file() -> PathBuf {
    config::data_dir().join("textbooks.json")
}

fn load() -> Vec<Value> {
    std::fs::read_to_string(file()).ok().and_then(|t| serde_json::from_str::<Vec<Value>>(&t).ok()).unwrap_or_default()
}

fn save(books: &[Value]) -> Result<()> {
    std::fs::create_dir_all(config::data_dir()).map_err(|e| Error::Io(e.to_string()))?;
    let tmp = file().with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(books).unwrap_or_default()).map_err(|e| Error::Io(e.to_string()))?;
    std::fs::rename(&tmp, file()).map_err(|e| Error::Io(e.to_string()))
}

fn now() -> f64 {
    crate::store::now()
}

/// A title from a file name: "fluid-mechanics_fundamentals.pdf" → "fluid mechanics fundamentals".
pub fn title_from(path: &Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    stem.replace(['_', '-'], " ").split_whitespace().collect::<Vec<_>>().join(" ")
}

fn id_for(path: &Path) -> String {
    hex::encode(Sha1::digest(path.to_string_lossy().as_bytes()))[..10].to_string()
}

fn is_pdf(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 5];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut head)).map(|_| &head == b"%PDF-").unwrap_or(false)
}

/// The library, newest-opened first, each with whether its file is still there.
pub fn list() -> Vec<Value> {
    let _g = LOCK.lock().unwrap();
    let mut books = load();
    for b in books.iter_mut() {
        let p = PathBuf::from(s(&b["path"]));
        b["exists"] = json!(p.exists());
    }
    books.sort_by(|a, b| {
        let key = |v: &Value| v["opened"].as_f64().unwrap_or(0.0).max(v["added"].as_f64().unwrap_or(0.0));
        key(b).total_cmp(&key(a))
    });
    books
}

pub fn get(id: &str) -> Option<Value> {
    let _g = LOCK.lock().unwrap();
    load().into_iter().find(|b| b["id"] == id)
}

/// Add a PDF (or return the one already added from that file).
pub fn add(path: &str) -> Result<Value> {
    let given = config::expand_user(path.trim());
    // the same file by any path (Downloads may be a link elsewhere) is one book
    let canon = given.canonicalize().map_err(|_| Error::NotFound(format!("There's no file at {}", given.display())))?;
    if !canon.is_file() || !is_pdf(&canon) {
        return Err(Error::Invalid(format!("{} isn't a PDF", given.display())));
    }
    let p = if given.is_absolute() { given } else { canon.clone() };
    let _g = LOCK.lock().unwrap();
    let mut books = load();
    let id = id_for(&canon);
    if let Some(b) = books.iter().find(|b| b["id"] == id.as_str()) {
        return Ok(b.clone());
    }
    let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
    let book = json!({"id": id, "path": p.to_string_lossy(), "title": title_from(&p), "size": size, "added": now()});
    books.push(book.clone());
    save(&books)?;
    Ok(book)
}

pub fn remove(id: &str) -> Result<()> {
    let _g = LOCK.lock().unwrap();
    let mut books = load();
    books.retain(|b| b["id"] != id);
    save(&books)
}

/// Merge fields into a book: its page count, reading position, when it was last opened, and the
/// course it's for (a course id; null for none), whose Anki deck its checkpoint cards go to.
pub fn update(id: &str, patch: &Value) -> Result<()> {
    let _g = LOCK.lock().unwrap();
    let mut books = load();
    let Some(b) = books.iter_mut().find(|b| b["id"] == id) else { return Err(Error::NotFound("No such textbook".into())) };
    if let (Some(o), Some(p)) = (b.as_object_mut(), patch.as_object()) {
        for (k, v) in p {
            if ["pages", "position", "opened", "title", "course"].contains(&k.as_str()) {
                o.insert(k.clone(), v.clone());
            }
        }
    }
    save(&books)
}

/// Large PDFs in Downloads, Documents and on the Desktop (two levels deep) that aren't in the
/// library yet, biggest first.
pub fn suggestions() -> Vec<Value> {
    let have: Vec<String> = load().iter().filter_map(|b| PathBuf::from(s(&b["path"])).canonicalize().ok()).map(|p| p.to_string_lossy().to_string()).collect();
    let home = config::home();
    let mut found = Vec::new();
    for dir in ["Downloads", "Documents", "Desktop"] {
        walk(&home.join(dir), 2, &mut found);
    }
    // the same book saved twice (same name and size) is suggested once
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<(u64, Value)> = found
        .into_iter()
        .filter_map(|p| {
            let m = std::fs::metadata(&p).ok()?;
            let canon = p.canonicalize().ok()?;
            if m.len() < SUGGEST_MIN_BYTES || have.contains(&canon.to_string_lossy().to_string()) {
                return None;
            }
            if !seen.insert((p.file_name()?.to_os_string(), m.len())) {
                return None;
            }
            // the path as you know it (Downloads may be a link elsewhere)
            Some((m.len(), json!({"path": p.to_string_lossy(), "title": title_from(&p), "size": m.len()})))
        })
        .collect();
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out.into_iter().take(12).map(|x| x.1).collect()
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if p.is_dir() {
            if depth > 0 {
                walk(&p, depth - 1, out);
            }
        } else if name.to_lowercase().ends_with(".pdf") {
            out.push(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("CANVAS_MCP_DIR", dir.path()) };
        // the data folder is read once per process; only run when it's ours
        if config::data_dir() != dir.path() {
            return;
        }
        let pdf = dir.path().join("Fluid_mechanics-3rd.pdf");
        std::fs::write(&pdf, b"%PDF-1.7\n...").unwrap();
        let b = add(pdf.to_str().unwrap()).unwrap();
        assert_eq!(b["title"], "Fluid mechanics 3rd");
        assert_eq!(add(pdf.to_str().unwrap()).unwrap()["id"], b["id"]); // once
        update(b["id"].as_str().unwrap(), &json!({"position": 12.5, "path": "/elsewhere"})).unwrap();
        let got = get(b["id"].as_str().unwrap()).unwrap();
        assert_eq!(got["position"], 12.5);
        assert_eq!(got["path"], pdf.to_string_lossy().as_ref()); // not patchable
        update(b["id"].as_str().unwrap(), &json!({"course": "101"})).unwrap();
        assert_eq!(get(b["id"].as_str().unwrap()).unwrap()["course"], "101");
        update(b["id"].as_str().unwrap(), &json!({"course": null})).unwrap();
        assert!(get(b["id"].as_str().unwrap()).unwrap()["course"].is_null());
        let txt = dir.path().join("notes.pdf");
        std::fs::write(&txt, b"hello").unwrap();
        assert!(add(txt.to_str().unwrap()).is_err());
        remove(b["id"].as_str().unwrap()).unwrap();
        assert!(list().is_empty());
    }

    #[test]
    fn titles() {
        assert_eq!(title_from(Path::new("/x/fluid-mechanics-fundaments-and-applications.pdf")), "fluid mechanics fundaments and applications");
    }
}
