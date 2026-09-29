//! Panopto recordings for Canvas courses, as notebook sources (their caption transcripts).
//!
//! A course's Panopto folder is found, in order, from:
//!   1. a folder you picked (pasted folder link, saved per course)
//!   2. recordings linked or embedded in the course's cached Canvas content
//!   3. searching Panopto for the course code, keeping the folder whose name contains the
//!      Canvas course name (schools often name folders like "<Term> - <Canvas course name>")

use std::collections::HashMap;
use std::sync::Arc;

use once_cell::sync::Lazy;
use regex::Regex;
use rusqlite::OptionalExtension;
use serde_json::{Value, json};

use crate::engine::Engine;
use crate::panopto::{LINK_RE, Panopto, srt_to_paragraphs};
use crate::{Error, Result};

pub struct Recordings {
    pub engine: Arc<Engine>,
    pub client: Panopto,
}

pub fn norm(s: &str) -> String {
    static NON: Lazy<Regex> = Lazy::new(|| Regex::new(r"[^a-z0-9]+").unwrap());
    NON.replace_all(&s.to_lowercase(), " ").trim().to_string()
}

/// Most common first, ties in first-seen order (like collections.Counter.most_common).
fn most_common(items: impl Iterator<Item = (String, String)>) -> Vec<((String, String), usize)> {
    let mut order: Vec<(String, String)> = Vec::new();
    let mut counts: HashMap<(String, String), usize> = HashMap::new();
    for it in items {
        if !counts.contains_key(&it) {
            order.push(it.clone());
        }
        *counts.entry(it).or_default() += 1;
    }
    let mut out: Vec<_> = order.into_iter().map(|k| {
        let n = counts[&k];
        (k, n)
    }).collect();
    out.sort_by(|a, b| b.1.cmp(&a.1)); // stable
    out
}

impl Recordings {
    pub fn new(engine: Arc<Engine>) -> Arc<Recordings> {
        engine.store.exec("CREATE TABLE IF NOT EXISTS panopto_folders (course_id TEXT PRIMARY KEY, folder_id TEXT, folder_name TEXT)", &[]);
        Arc::new(Recordings { engine, client: Panopto::from_env() })
    }

    fn course(&self, cid: &str) -> Value {
        let e = &self.engine;
        e.cached_list("courses", &[]).into_iter().chain(e.cached_list("past_courses", &[])).find(|c| c["id"].to_string() == cid).unwrap_or(json!({}))
    }

    pub fn saved_folder(&self, cid: &str) -> Option<Value> {
        self.engine.store.with(|db| {
            db.query_row("SELECT folder_id, folder_name FROM panopto_folders WHERE course_id = ?", [cid], |r| {
                Ok(json!({"id": r.get::<_, Option<String>>(0)?, "name": r.get::<_, Option<String>>(1)?, "via": "chosen"}))
            })
            .optional()
            .ok()
            .flatten()
        })
    }

    pub fn save_folder(&self, cid: &str, folder_id: Option<&str>, folder_name: Option<&str>) {
        match folder_id {
            Some(fid) => self.engine.store.exec(
                "INSERT OR REPLACE INTO panopto_folders (course_id, folder_id, folder_name) VALUES (?,?,?)",
                &[&cid, &fid, &folder_name],
            ),
            None => self.engine.store.exec("DELETE FROM panopto_folders WHERE course_id = ?", &[&cid]),
        }
    }

    /// Recording ids linked/embedded anywhere in the course's cached Canvas content.
    pub fn linked_ids(&self, cid: &str) -> Vec<String> {
        let e = &self.engine;
        let c = [cid.to_string()];
        let mut blobs: Vec<Value> = ["modules", "groups", "syllabus", "course_announcements", "discussions"].iter().filter_map(|n| e.cached(n, &c)).collect();
        for p in e.cached_list("pages", &c) {
            if let Some(url) = p["url"].as_str() {
                if let Some(b) = e.cached("page", &[cid.to_string(), url.to_string()]) {
                    blobs.push(b);
                }
            }
        }
        let blobs: Vec<Value> = blobs.into_iter().filter(|b| crate::util::truthy(b)).collect();
        let text = serde_json::to_string(&blobs).unwrap_or_default();
        let mut out: Vec<String> = Vec::new();
        for m in LINK_RE.captures_iter(&text) {
            let id = m[1].to_lowercase();
            if !out.contains(&id) {
                out.push(id);
            }
        }
        out
    }

    fn matches_course(folder_name: &str, course: &Value) -> bool {
        let name = norm(course["name"].as_str().unwrap_or(""));
        !name.is_empty() && format!(" {} ", norm(folder_name)).contains(&format!(" {name} "))
    }

    async fn find_folder(&self, cid: &str, course: &Value, linked: &[Value]) -> Result<Option<Value>> {
        if let Some(saved) = self.saved_folder(cid) {
            return Ok(Some(saved));
        }
        let s = |v: &Value| v.as_str().unwrap_or("").to_string();
        // From linked recordings: prefer a folder named after this course, else the most common one.
        let folders = most_common(linked.iter().filter(|i| crate::util::truthy(&i["folder_id"])).map(|i| (s(&i["folder_id"]), s(&i["folder_name"]))));
        for ((fid, fname), _) in &folders {
            if Self::matches_course(fname, course) {
                return Ok(Some(json!({"id": fid, "name": fname, "via": "linked"})));
            }
        }
        // Search recordings for the course code; their results carry folder ids and names.
        static CODE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^([A-Z&]+(?: [A-Z&]+)*) ?(\d{3,4})").unwrap());
        let code = course["course_code"].as_str().filter(|c| !c.is_empty()).or(course["name"].as_str()).unwrap_or("");
        if let Some(m) = CODE.captures(code) {
            let hits = self.client.search_sessions(&format!("{} {}", &m[1], &m[2])).await?;
            for ((fid, fname), _) in most_common(hits.iter().map(|h| (s(&h["folder_id"]), s(&h["folder_name"])))) {
                if Self::matches_course(&fname, course) {
                    return Ok(Some(json!({"id": fid, "name": fname, "via": "search"})));
                }
            }
        }
        if let Some(((fid, fname), _)) = folders.first() {
            return Ok(Some(json!({"id": fid, "name": fname, "via": "linked"})));
        }
        Ok(None)
    }

    /// The "recordings" resource: a course's folder and its recordings (plus ones linked from elsewhere).
    pub async fn recordings(&self, cid: &str) -> Result<Value> {
        let course = self.course(cid);
        let ids: Vec<String> = self.linked_ids(cid).into_iter().take(15).collect();
        let infos: Vec<Value> = futures::future::join_all(ids.iter().map(|i| self.client.delivery_info(i))).await.into_iter().filter_map(|r| r.ok()).collect();
        let folder = self.find_folder(cid, &course, &infos).await?;
        let mut items = match &folder {
            Some(f) => self.client.folder_sessions(f["id"].as_str().unwrap_or("")).await?,
            None => vec![],
        };
        let in_folder: Vec<String> = items.iter().map(|i| i["id"].as_str().unwrap_or("").to_lowercase()).collect();
        for i in &infos {
            // linked from Canvas but living in another folder (e.g. an older year's recording)
            if !in_folder.contains(&i["id"].as_str().unwrap_or("").to_lowercase()) {
                let mut o = serde_json::Map::new();
                for k in ["id", "title", "duration", "start", "has_captions", "folder_name", "url"] {
                    o.insert(k.into(), i.get(k).cloned().unwrap_or(Value::Null));
                }
                o.insert("linked".into(), json!(true));
                items.push(Value::Object(o));
            }
        }
        Ok(json!({"folder": folder, "items": items}))
    }

    /// (source title, markdown text) for a recording; fails if it has no captions.
    pub async fn transcript(&self, rid: &str, course_label: &str) -> Result<(String, String)> {
        let info = self.engine.get_json("recording_info", &[rid.to_string()]).await?;
        if !info["has_captions"].as_bool().unwrap_or(false) {
            return Err(Error::NotFound("This recording has no captions yet".into()));
        }
        let mut langs: Vec<i64> = info["languages"].as_array().into_iter().flatten().filter_map(|v| v.as_i64()).collect();
        if !langs.contains(&0) {
            langs.push(0);
        }
        let mut srt = String::new();
        for lang in langs {
            srt = self.client.captions_srt(rid, lang).await?;
            if srt.contains("-->") {
                break;
            }
        }
        let body = srt_to_paragraphs(&srt, 60);
        if body.is_empty() {
            return Err(Error::NotFound("Panopto returned empty captions".into()));
        }
        let date: String = info["start"].as_str().unwrap_or("").chars().take(10).collect();
        let name = info["title"].as_str().filter(|t| !t.is_empty()).unwrap_or("Recording");
        let mut title = if norm(name).starts_with(&norm(course_label)) { name.to_string() } else { format!("{course_label} · {name}") };
        if !date.is_empty() && !name.contains(&date) {
            title += &format!(" ({date})");
        }
        let minutes = (info["duration"].as_f64().unwrap_or(0.0) / 60.0).round() as i64;
        let mut parts = vec![
            format!("# {}", info["title"].as_str().unwrap_or("None")),
            String::new(),
            format!("Course: {course_label}"),
            format!("Recorded: {} · {minutes} min", if date.is_empty() { "unknown" } else { &date }),
            format!("Panopto: {}", info["url"].as_str().unwrap_or("")),
            String::new(),
        ];
        for (heading, key) in [("Summary", "summary"), ("Chapters", "chapters"), ("Key points", "key_points")] {
            let txt = ai_text(&info[key]);
            if !txt.is_empty() {
                parts.extend([format!("## {heading} (Panopto AI)"), String::new(), txt, String::new()]);
            }
        }
        parts.extend(["## Transcript".to_string(), String::new(), body]);
        Ok((title, parts.join("\n")))
    }
}

fn clock(sec: f64) -> String {
    let s = sec as i64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60) } else { format!("{:02}:{:02}", s / 60, s % 60) }
}

/// Panopto AI fields vary in shape (string, list of strings, list of {title/text, time}); flatten them.
pub fn ai_text(v: &Value) -> String {
    if !crate::util::truthy(v) {
        return String::new();
    }
    match v {
        Value::String(s) => s.trim().to_string(),
        Value::Object(o) => {
            for k in ["Summary", "Text", "Content", "Value", "Items", "Chapters", "KeyPoints"] {
                if let Some(x) = o.get(k) {
                    return ai_text(x);
                }
            }
            o.iter()
                .filter(|(_, x)| matches!(x, Value::String(_) | Value::Array(_) | Value::Object(_)) && crate::util::truthy(x))
                .map(|(k, x)| format!("- {k}: {}", ai_text(x)))
                .collect::<Vec<_>>()
                .join("\n")
        }
        Value::Array(a) => {
            let mut lines = Vec::new();
            for x in a {
                if let Value::Object(o) = x {
                    let label = ["Title", "Text", "Caption", "Content", "Summary", "Name"].iter().find_map(|k| o.get(*k).filter(|v| crate::util::truthy(v))).cloned().unwrap_or(json!(""));
                    let t = ["Time", "StartTime", "Start", "Seconds", "Offset"].iter().find_map(|k| o.get(*k).filter(|v| !v.is_null()));
                    let stamp = match t {
                        Some(Value::Number(n)) => format!("[{}] ", clock(n.as_f64().unwrap_or(0.0))),
                        _ => String::new(),
                    };
                    let label = match &label {
                        Value::String(s) => s.clone(),
                        other => ai_text(other),
                    };
                    lines.push(format!("- {stamp}{label}"));
                } else {
                    lines.push(format!("- {}", ai_text(x)));
                }
            }
            lines.into_iter().filter(|l| !l.trim_matches(|c| c == '-' || c == ' ').is_empty()).collect::<Vec<_>>().join("\n")
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_fields() {
        assert_eq!(ai_text(&json!("  hi ")), "hi");
        assert_eq!(ai_text(&json!([{"Title": "Heap property", "Time": 0}, {"Title": "Percolate down", "Time": 1260}])), "- [00:00] Heap property\n- [21:00] Percolate down");
        assert_eq!(ai_text(&json!(["a", "b"])), "- a\n- b");
    }

    #[test]
    fn counter_order() {
        let got = most_common(vec![("a".into(), "A".into()), ("b".into(), "B".into()), ("b".into(), "B".into())].into_iter());
        assert_eq!(got[0].0.0, "b");
        assert_eq!(norm("Autumn 2026 - Data Structures!"), "autumn 2026 data structures");
    }
}
