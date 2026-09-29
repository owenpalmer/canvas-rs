//! Full-text search over everything cached: page bodies, assignment descriptions, announcements,
//! discussions, the syllabus, messages, lecture transcripts, and PDFs' text (which the app extracts
//! and hands over). Kept in an SQLite FTS5 table in cache.db, one row per section (a paragraph, a
//! transcript passage, a PDF page), so a hit can open its document at that place.
//!
//! The index is brought up to date after each sync; a document whose text hasn't changed is skipped.

use std::collections::HashSet;
use std::sync::Arc;

use rusqlite::params;
use serde_json::{Value, json};
use sha1::{Digest, Sha1};

use crate::engine::Engine;
use crate::util::s;

/// Words, lowercased, separated by single spaces: how the index and the app compare text, so a
/// section can be found again in the rendered document whatever its markup was.
pub fn norm(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = true;
    for c in text.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
            space = false;
        } else if !space {
            out.push(' ');
            space = true;
        }
    }
    out.trim_end().to_string()
}

/// The first words of a section: enough to find it again in the rendered document.
pub fn needle(section: &str) -> String {
    norm(section).split(' ').take(8).collect::<Vec<_>>().join(" ")
}

fn ensure(e: &Engine) {
    e.store.with(|db| {
        let _ = db.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(
                body, key UNINDEXED, sec UNINDEXED, kind UNINDEXED, title UNINDEXED, course UNINDEXED, href UNINDEXED,
                tokenize = 'unicode61 remove_diacritics 2');
             CREATE TABLE IF NOT EXISTS fts_docs (key TEXT PRIMARY KEY, hash TEXT NOT NULL);",
        );
    });
}

/// A document to index: its sections in order.
pub struct Doc {
    pub key: String,
    pub kind: &'static str,
    pub title: String,
    pub course: String,
    pub href: String,
    pub sections: Vec<String>,
}

/// Put documents in the index (replacing their old rows); unchanged ones are skipped. Returns how
/// many changed.
pub fn put(e: &Engine, docs: &[Doc]) -> usize {
    ensure(e);
    e.store.with(|db| {
        let mut changed = 0;
        let Ok(tx) = db.unchecked_transaction() else { return 0 };
        for d in docs {
            let mut h = Sha1::new();
            h.update(d.title.as_bytes());
            h.update(d.href.as_bytes());
            for s in &d.sections {
                h.update(s.as_bytes());
                h.update([0]);
            }
            let hash = hex::encode(h.finalize());
            let old: Option<String> = tx.query_row("SELECT hash FROM fts_docs WHERE key = ?", [&d.key], |r| r.get(0)).ok();
            if old.as_deref() == Some(hash.as_str()) {
                continue;
            }
            let _ = tx.execute("DELETE FROM fts WHERE key = ?", [&d.key]);
            for (i, sec) in d.sections.iter().enumerate() {
                if sec.trim().is_empty() {
                    continue;
                }
                let _ = tx.execute(
                    "INSERT INTO fts (body, key, sec, kind, title, course, href) VALUES (?, ?, ?, ?, ?, ?, ?)",
                    params![sec, d.key, i as i64, d.kind, d.title, d.course, d.href],
                );
            }
            let _ = tx.execute("INSERT OR REPLACE INTO fts_docs (key, hash) VALUES (?, ?)", params![d.key, hash]);
            changed += 1;
        }
        let _ = tx.commit();
        changed
    })
}

/// Whether a document is indexed with this exact hash tag (for PDFs: the file's version).
pub fn has(e: &Engine, key: &str, tag: &str) -> bool {
    ensure(e);
    e.store.with(|db| db.query_row("SELECT 1 FROM fts_docs WHERE key = ? AND hash = ?", params![key, tag], |_| Ok(())).is_ok())
}

/// Index a PDF's pages (text extracted by the app), tagged with the file's version.
pub fn put_pdf(e: &Engine, fid: &str, tag: &str, title: &str, course: &str, href: &str, pages: Vec<String>) {
    let key = format!("pdf:{fid}");
    ensure(e);
    e.store.with(|db| {
        let Ok(tx) = db.unchecked_transaction() else { return };
        let _ = tx.execute("DELETE FROM fts WHERE key = ?", [&key]);
        for (i, p) in pages.iter().enumerate() {
            let text = p.split_whitespace().collect::<Vec<_>>().join(" ");
            if !text.is_empty() {
                let _ = tx.execute(
                    "INSERT INTO fts (body, key, sec, kind, title, course, href) VALUES (?, ?, ?, 'pdf', ?, ?, ?)",
                    params![text, key, i as i64, title, course, href],
                );
            }
        }
        let _ = tx.execute("INSERT OR REPLACE INTO fts_docs (key, hash) VALUES (?, ?)", params![key, tag]);
        let _ = tx.commit();
    });
}

/// HTML as plain-text sections: one per paragraph, heading, list item or table row.
pub fn html_sections(html: &str) -> Vec<String> {
    let md = crate::markdown::md(html);
    md_sections(&md)
}

static LINK: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| regex::Regex::new(r"!?\[([^\]]*)\]\([^)]*\)").unwrap());

/// Markdown as plain-text sections (paragraphs, and each list item or heading on its own).
pub fn md_sections(md: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        let t = cur.split_whitespace().collect::<Vec<_>>().join(" ");
        if t.chars().filter(|c| c.is_alphanumeric()).count() >= 3 {
            out.push(t);
        }
        cur.clear();
    };
    for line in md.lines() {
        let l = line.trim();
        if l.is_empty() {
            flush(&mut cur, &mut out);
            continue;
        }
        // headings and list items start sections of their own
        let is_item = l.starts_with('#') || l.starts_with("- ") || l.starts_with("* ") || l.starts_with("+ ") || l.starts_with('|') || l.split_once(". ").map(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())).unwrap_or(false);
        if is_item {
            flush(&mut cur, &mut out);
        }
        let l = LINK.replace_all(l, "$1");
        let l = l.trim_start_matches(|c: char| c == '#' || c == '>' || c == '-' || c == '*' || c == '+' || c == '|' || c.is_whitespace());
        let l = l.split_once(". ").filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())).map(|(_, r)| r).unwrap_or(l);
        let clean: String = l.chars().filter(|c| !matches!(c, '*' | '_' | '`' | '\\')).map(|c| if c == '|' { ' ' } else { c }).collect();
        cur.push_str(&clean);
        cur.push(' ');
        if is_item {
            flush(&mut cur, &mut out);
        }
    }
    flush(&mut cur, &mut out);
    out
}

fn course_labels(e: &Engine) -> Vec<(String, String)> {
    let active = e.cached_list("courses", &[]);
    let past = e.cached_list("past_courses", &[]);
    let mut seen = HashSet::new();
    active
        .into_iter()
        .chain(past)
        .filter_map(|c| {
            let id = c["id"].to_string();
            seen.insert(id.clone()).then(|| {
                let code = s(&c["course_code"]);
                (id, if code.is_empty() { s(&c["name"]) } else { code })
            })
        })
        .collect()
}

/// Everything cached, as documents.
pub fn docs_from_cache(e: &Engine) -> Vec<Doc> {
    let mut out = Vec::new();
    let labels = course_labels(e);
    for (cid, label) in &labels {
        let a = [cid.clone()];
        for g in e.cached_list("groups", &a) {
            for x in g["assignments"].as_array().into_iter().flatten() {
                out.push(Doc {
                    key: format!("a:{}", x["id"]),
                    kind: "assignment",
                    title: s(&x["name"]),
                    course: label.clone(),
                    href: format!("#/c/{cid}/a/{}", x["id"]),
                    sections: html_sections(x["description"].as_str().unwrap_or("")),
                });
            }
        }
        for p in e.cached_list("pages", &a) {
            let url = s(&p["url"]);
            if let Some(body) = e.cached("page", &[cid.clone(), url.clone()]) {
                out.push(Doc {
                    key: format!("p:{cid}:{url}"),
                    kind: "page",
                    title: s(&p["title"]),
                    course: label.clone(),
                    href: format!("#/c/{cid}/p/{url}"),
                    sections: html_sections(body["body"].as_str().unwrap_or("")),
                });
            }
        }
        let mut topics: Vec<(Value, &'static str)> = e.cached_list("discussions", &a).into_iter().map(|d| (d, "discussion")).collect();
        topics.extend(e.cached_list("course_announcements", &a).into_iter().map(|d| (d, "announcement")));
        let mut seen = HashSet::new();
        for (d, kind) in topics {
            let id = d["id"].to_string();
            if !seen.insert(id.clone()) {
                continue;
            }
            let mut sections = html_sections(d["message"].as_str().unwrap_or(""));
            if let Some(t) = e.cached("topic", &[cid.clone(), id.clone()]) {
                for v in t["view"]["view"].as_array().into_iter().flatten() {
                    sections.extend(html_sections(v["message"].as_str().unwrap_or("")));
                }
            }
            out.push(Doc { key: format!("d:{id}"), kind, title: s(&d["title"]), course: label.clone(), href: format!("#/c/{cid}/d/{id}"), sections });
        }
        if let Some(Value::String(body)) = e.cached("syllabus", &a) {
            out.push(Doc { key: format!("syl:{cid}"), kind: "syllabus", title: "Syllabus".into(), course: label.clone(), href: format!("#/c/{cid}/syllabus"), sections: html_sections(&body) });
        }
        for (k, v) in e.store.prefix(&format!("transcript:{cid}:")) {
            let Ok(t) = serde_json::from_str::<Value>(&v) else { continue };
            let rid = k.rsplit(':').next().unwrap_or("").to_string();
            out.push(Doc {
                key: format!("t:{rid}"),
                kind: "transcript",
                title: s(&t["title"]),
                course: label.clone(),
                href: format!("#/c/{cid}/rec/{rid}"),
                sections: md_sections(t["text"].as_str().unwrap_or("")),
            });
        }
    }
    for m in e.cached_list("inbox", &[]) {
        let id = m["id"].to_string();
        let mut sections: Vec<String> = Vec::new();
        match e.cached("conversation", &[id.clone()]) {
            Some(c) => {
                for msg in c["messages"].as_array().into_iter().flatten() {
                    sections.extend(md_sections(msg["body"].as_str().unwrap_or("")));
                }
            }
            None => sections.extend(md_sections(m["last_message"].as_str().unwrap_or(""))),
        }
        let subject = m["subject"].as_str().filter(|x| !x.is_empty()).unwrap_or("(no subject)").to_string();
        out.push(Doc { key: format!("m:{id}"), kind: "message", title: subject, course: s(&m["context_name"]), href: format!("#/inbox/{id}"), sections });
    }
    out
}

/// Fetch what isn't cached yet but should be searchable: page bodies, and (with Panopto allowed)
/// the transcripts of each course's recordings. Gently, a few at a time.
pub async fn warm(e: Arc<Engine>, recordings: Arc<crate::recordings::Recordings>) {
    let labels = course_labels(&e);
    let active: HashSet<String> = e.cached_list("courses", &[]).iter().map(|c| c["id"].to_string()).collect();
    let sem = Arc::new(tokio::sync::Semaphore::new(3));
    let mut jobs = Vec::new();
    for (cid, _) in labels.iter().filter(|(c, _)| active.contains(c)) {
        for p in e.cached_list("pages", &[cid.clone()]) {
            let url = s(&p["url"]);
            if url.is_empty() || e.cached("page", &[cid.clone(), url.clone()]).is_some() {
                continue;
            }
            let (e, sem, cid) = (e.clone(), sem.clone(), cid.clone());
            jobs.push(tokio::spawn(async move {
                let _p = sem.acquire().await;
                let _ = e.get("page", &[cid, url], false).await;
            }));
        }
    }
    futures::future::join_all(jobs).await;
    // transcripts: only from courses whose recordings are already known (the Panopto permission
    // and a found folder), and only ones not fetched before
    let host = crate::config::panopto_host();
    if !crate::config::is_demo() && (host.is_empty() || !crate::config::allowed(&host)) {
        return;
    }
    let mut jobs = Vec::new();
    for (cid, label) in labels.iter().filter(|(c, _)| active.contains(c)) {
        // each course's recordings (cached for half an hour, so this is usually free)
        let recs = match e.get_json("recordings", &[cid.clone()]).await {
            Ok(r) => r,
            Err(_) => continue,
        };
        for r in recs["items"].as_array().into_iter().flatten() {
            let rid = s(&r["id"]);
            let key = format!("transcript:{cid}:{rid}");
            if rid.is_empty() || e.store.get(&key).is_some() {
                continue;
            }
            let (e, sem, recordings, label) = (e.clone(), sem.clone(), recordings.clone(), label.clone());
            jobs.push(tokio::spawn(async move {
                let _p = sem.acquire().await;
                match recordings.transcript(&rid, &label).await {
                    Ok((title, text)) => {
                        e.store.put(&key, &json!({"title": title, "text": text}).to_string());
                    }
                    Err(err) => log::info!("no transcript for {rid}: {err}"),
                }
            }));
        }
    }
    futures::future::join_all(jobs).await;
}

/// Bring the index up to date with the cache.
pub fn refresh(e: &Engine) -> usize {
    let docs = docs_from_cache(e);
    let n = put(e, &docs);
    // documents gone from the cache leave the index too (PDFs are the app's to manage)
    let keep: HashSet<String> = docs.iter().map(|d| d.key.clone()).collect();
    e.store.with(|db| {
        let keys: Vec<String> = db.prepare("SELECT key FROM fts_docs").and_then(|mut st| st.query_map([], |r| r.get::<_, String>(0)).map(|r| r.filter_map(|x| x.ok()).collect())).unwrap_or_default();
        for k in keys.into_iter().filter(|k| !k.starts_with("pdf:") && !keep.contains(k)) {
            let _ = db.execute("DELETE FROM fts WHERE key = ?", [&k]);
            let _ = db.execute("DELETE FROM fts_docs WHERE key = ?", [&k]);
        }
    });
    n
}

/// The FTS5 query for what someone typed: every word must appear (as a prefix).
fn fts_query(q: &str) -> Option<String> {
    let words: Vec<String> = norm(q).split(' ').filter(|w| !w.is_empty()).map(|w| format!("\"{w}\"*")).collect();
    (!words.is_empty()).then(|| words.join(" "))
}

/// The same words as a phrase, in order and adjacent (the last one a prefix): "point 2.7" means
/// that, not any page with a 2 and a 7 on it.
fn phrase_query(q: &str) -> Option<String> {
    let n = norm(q);
    (n.contains(' ')).then(|| format!("\"{n}\"*"))
}

/// Search the index: the best sections, at most two per document. Each hit has the document
/// (t, k, c, h), a snippet with the matches between \u{1} and \u{2}, the section number, and a
/// needle (the section's first words) to find it again on screen.
pub fn search(e: &Engine, q: &str, limit: usize) -> Vec<Value> {
    let Some(words) = fts_query(q) else { return vec![] };
    ensure(e);
    // exact phrase hits first, then the rest with every word
    let mut out = match phrase_query(q) {
        Some(p) => run(e, q, &p, limit, &mut Default::default()),
        None => vec![],
    };
    if out.len() < limit {
        let mut seen: std::collections::HashMap<String, usize> = Default::default();
        let mut have: HashSet<(String, i64)> = HashSet::new();
        for h in &out {
            *seen.entry(h["key"].as_str().unwrap_or("").to_string()).or_default() += 1;
            have.insert((h["key"].as_str().unwrap_or("").to_string(), h["sec"].as_i64().unwrap_or(0)));
        }
        for h in run(e, q, &words, limit, &mut seen) {
            if out.len() >= limit {
                break;
            }
            if !have.contains(&(h["key"].as_str().unwrap_or("").to_string(), h["sec"].as_i64().unwrap_or(0))) {
                out.push(h);
            }
        }
    }
    out
}

/// One FTS query: at most two sections per document (counting ones already found).
fn run(e: &Engine, q: &str, query: &str, limit: usize, per_doc: &mut std::collections::HashMap<String, usize>) -> Vec<Value> {
    e.store.with(|db| {
        let Ok(mut st) = db.prepare(
            "SELECT key, sec, kind, title, course, href, snippet(fts, 0, char(1), char(2), '…', 14), body
             FROM fts WHERE fts MATCH ? ORDER BY bm25(fts) LIMIT 200",
        ) else {
            return vec![];
        };
        let rows = st.query_map([query], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
            ))
        });
        let mut out = Vec::new();
        for (key, sec, kind, title, course, href, snip, body) in rows.into_iter().flatten().flatten() {
            let n = per_doc.entry(key.clone()).or_default();
            if *n >= 2 {
                continue;
            }
            *n += 1;
            out.push(json!({"key": key, "t": title, "k": kind, "c": course, "h": href, "s": snip, "sec": sec, "needle": needle(&body), "q": norm(q)}));
            if out.len() >= limit {
                break;
            }
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_and_norm() {
        let s = html_sections("<h2>Office hours</h2><p>Mon <b>2–3pm</b> in <a href='x'>CSE 2</a>.</p><ul><li>Bring questions</li><li>ok</li></ul>");
        assert_eq!(s, vec!["Office hours", "Mon 2–3pm in CSE 2.", "Bring questions"]);
        assert_eq!(norm("Heaps & Priority-Queues!"), "heaps priority queues");
        assert_eq!(fts_query("amortized  analy").unwrap(), "\"amortized\"* \"analy\"*");
        assert_eq!(needle("One two three four five six seven eight nine ten"), "one two three four five six seven eight");
    }

    #[test]
    fn index_and_search() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(&dir.path().join("c.db")).unwrap();
        let e = Engine::new(store, crate::client::Client::Demo(crate::demo::DemoClient));
        let d = Doc { key: "p:1:x".into(), kind: "page", title: "Heaps".into(), course: "CSE 373".into(), href: "#/c/1/p/x".into(), sections: vec!["A binary heap is a complete tree.".into(), "Percolate down after removing the minimum.".into()] };
        assert_eq!(put(&e, std::slice::from_ref(&d)), 1);
        assert_eq!(put(&e, &[d]), 0); // unchanged: skipped
        let hits = search(&e, "percol", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["sec"], 1);
        assert!(hits[0]["s"].as_str().unwrap().contains("\u{1}Percolate\u{2}"));
        assert_eq!(hits[0]["needle"], "percolate down after removing the minimum");
        put_pdf(&e, "9", "v1", "lec.pdf", "CSE 373", "#/c/1/f/9", vec!["intro".into(), "amortized analysis here".into()]);
        assert!(has(&e, "pdf:9", "v1"));
        assert_eq!(search(&e, "amortized", 10)[0]["sec"], 1);
        // a phrase outranks pages that merely have its words
        put_pdf(&e, "10", "v1", "lec2.pdf", "CSE 373", "#/c/1/f/10", vec!["point 7.1 and 2.2".into(), "point 2.7 here".into()]);
        let hits = search(&e, "point 2.7", 10);
        assert_eq!((hits[0]["key"].as_str().unwrap(), hits[0]["sec"].as_i64().unwrap()), ("pdf:10", 1));
    }
}
