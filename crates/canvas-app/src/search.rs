//! Full-text search on the app's side: the palette's content hits (canvas_mcp::fulltext), opening
//! a hit in the viewer scrolled to its section with the section briefly highlighted, the
//! transcript view that transcript hits open, and feeding PDFs' text to the index.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use canvas_mcp::fulltext::norm;
use canvas_mcp::services::ApiErr;
use egui::{Rect, Ui};
use serde_json::Value;

use crate::app::{App, Pane};
use crate::data::Need;
use crate::fmt;

/// How long a found section stays highlighted (s), fading out over the last part.
const HIGHLIGHT: f32 = 2.6;
/// How long to keep looking for a section while its document loads (s).
const PATIENCE: f32 = 8.0;

/// A hit being opened: its document, and the section to scroll to.
pub struct Find {
    pub href: String,
    pub kind: String,
    /// the section's first words, normalized
    needle: String,
    /// the query's words, normalized
    words: Vec<String>,
    /// the section number (a PDF's page)
    pub sec: usize,
    started: Instant,
    /// this frame's matches: by the section's words, else by the query's
    primary: Option<Rect>,
    fallback: Option<Rect>,
    /// once scrolled there: what's highlighted, since when
    found: Option<(String, Instant)>,
    pending_text: Option<String>,
    pdf_hl: Vec<(usize, [f32; 4])>,
}

#[derive(Default)]
pub struct Search {
    pub find: Option<Find>,
    /// the palette's content hits: (query, hits)
    pub hits: Option<(String, Vec<Value>)>,
    wanted: Option<(String, Instant)>,
    asked: Option<String>,
    transcripts: HashMap<String, Result<Value, ApiErr>>,
    loading: HashSet<String>,
    pdf_at: Option<Instant>,
    pdf_busy: bool,
}

// ---------- the palette ----------
/// The palette's query changed: look it up in the index once typing pauses.
pub fn want(app: &mut App, q: &str) {
    let q = q.trim().to_string();
    if q.chars().count() < 2 {
        app.search.wanted = None;
        app.search.hits = None;
        return;
    }
    if app.search.wanted.as_ref().map(|w| w.0 != q).unwrap_or(true) {
        app.search.wanted = Some((q, Instant::now()));
    }
}

/// Every frame the palette is open: run the pending query.
pub fn tick_palette(app: &mut App) {
    let Some((q, at)) = app.search.wanted.clone() else { return };
    if app.search.asked.as_deref() == Some(q.as_str()) {
        return;
    }
    if at.elapsed() < Duration::from_millis(120) {
        if let Some(c) = app.ctx() {
            c.request_repaint_after(Duration::from_millis(130));
        }
        return;
    }
    app.search.asked = Some(q.clone());
    let svc = app.svc.clone();
    app.spawn(
        {
            let q = q.clone();
            async move { svc.search_content(&q, 30).await }
        },
        move |app, hits| {
            if app.search.wanted.as_ref().map(|w| w.0 == q).unwrap_or(false) {
                app.search.hits = Some((q, hits));
                app.palette.refresh();
            }
        },
    );
}

/// The content hits for the palette's current query, if they've arrived.
pub fn hits_for(app: &App, q: &str) -> Vec<Value> {
    match &app.search.hits {
        Some((hq, h)) if hq == q.trim() => h.clone(),
        _ => vec![],
    }
}

/// Open a hit in the viewer, at its section.
pub fn open_hit(app: &mut App, hit: &Value) {
    let href = fmt::s(&hit["h"]);
    app.search.find = Some(Find {
        href: href.clone(),
        kind: fmt::s(&hit["k"]),
        needle: fmt::s(&hit["needle"]),
        words: fmt::s(&hit["q"]).split(' ').filter(|w| !w.is_empty()).map(String::from).collect(),
        sec: hit["sec"].as_u64().unwrap_or(0) as usize,
        started: Instant::now(),
        primary: None,
        fallback: None,
        found: None,
        pending_text: None,
        pdf_hl: vec![],
    });
    crate::panes::open_doc(app, &href);
    app.panes.focused = Pane::Viewer;
}

// ---------- finding the section in a drawn document ----------
fn pane_shows(app: &App, pane: Pane, href: &str) -> bool {
    let want = if pane == Pane::Viewer { &app.viewer.want } else { &app.main.want };
    want == href || want.ends_with(&format!(":{href}"))
}

fn has_words(text: &str, words: &[String]) -> bool {
    !words.is_empty() && words.iter().all(|w| text.split(' ').any(|t| t.starts_with(w.as_str())))
}

/// A paragraph was laid out at `rect` in `pane`: note it if it's the section being looked for.
/// Returns the highlight's strength if it's the section found.
pub fn para(app: &mut App, pane: Pane, text: &str, rect: Rect) -> Option<f32> {
    let f = app.search.find.as_ref()?;
    if f.kind == "pdf" || !pane_shows(app, pane, &f.href) {
        return None;
    }
    let f = app.search.find.as_mut().unwrap();
    let t_ = norm(text);
    if let Some((hl, since)) = &f.found {
        if *hl == t_ {
            let e = since.elapsed().as_secs_f32();
            if e < HIGHLIGHT {
                if let Some(c) = app.ctx() {
                    c.request_repaint();
                }
                return Some(if e < HIGHLIGHT - 0.8 { 1.0 } else { (HIGHLIGHT - e) / 0.8 });
            }
        }
        return None;
    }
    if f.primary.is_none() && !f.needle.is_empty() && t_.contains(&f.needle) {
        f.primary = Some(rect);
        f.found_text(t_);
    } else if f.fallback.is_none() && has_words(&t_, &f.words) {
        f.fallback = Some(rect);
        if f.primary.is_none() {
            f.found_text(t_);
        }
    }
    None
}

impl Find {
    // the text of the best match so far, kept until the frame resolves
    fn found_text(&mut self, t_: String) {
        self.pdf_hl.clear();
        self.pending_text = Some(t_);
    }
}

/// After a pane's frame: scroll to the section, if it was drawn. `top` is the content's top on
/// screen, `view_h` the pane's visible height.
pub fn resolve(app: &mut App, pane: Pane, top: f32, view_h: f32) {
    let Some(f) = app.search.find.as_ref() else { return };
    if f.kind == "pdf" || !pane_shows(app, pane, &f.href) {
        return;
    }
    let f = app.search.find.as_mut().unwrap();
    if f.started.elapsed().as_secs_f32() > PATIENCE && f.found.is_none() {
        app.search.find = None;
        return;
    }
    if f.found.is_some() {
        if f.found.as_ref().map(|(_, s)| s.elapsed().as_secs_f32() > HIGHLIGHT).unwrap_or(false) {
            app.search.find = None;
        }
        return;
    }
    let target = f.primary.or(f.fallback);
    f.primary = None;
    f.fallback = None;
    let Some(r) = target else { return };
    let text = f.pending_text.take().unwrap_or_default();
    f.found = Some((text, Instant::now()));
    let st = crate::panes::state(app, pane);
    // `top` is where the content starts on screen, so this is the section's place in the content
    st.scroll_to = Some((r.min.y - top - view_h * 0.25).max(0.0));
    if let Some(c) = app.ctx() {
        c.request_repaint();
    }
}

/// In a PDF: once the hit's page has its text, scroll to the first match on it and mark the
/// matching runs.
pub fn pdf(app: &mut App, fid: &str) {
    let Some(f) = app.search.find.as_ref() else { return };
    if f.kind != "pdf" || !f.href.ends_with(&format!("/f/{fid}")) {
        return;
    }
    if f.found.is_some() {
        if f.found.as_ref().map(|(_, s)| s.elapsed().as_secs_f32() > HIGHLIGHT).unwrap_or(false) {
            app.search.find = None;
        } else if let Some(c) = app.ctx() {
            c.request_repaint();
        }
        return;
    }
    if f.started.elapsed().as_secs_f32() > PATIENCE {
        app.search.find = None;
        return;
    }
    let (page, words) = (f.sec, f.words.clone());
    let tx = app.tx.clone();
    let Some(text) = app.pdf.text(&tx, fid, page) else { return };
    // the whole phrase where it's on the page; else each word
    let mut hits = marks(&text.chars, &words.join(" "));
    if hits.is_empty() {
        for w in &words {
            hits.extend(marks(&text.chars, w));
        }
    }
    let y = hits.first().map(|r| r[1]).unwrap_or(0.0);
    let Some(cy) = crate::pdf::content_y(app, fid, page, y) else { return };
    app.viewer.scroll_to = Some((cy - app.viewer.viewport_h * 0.25).max(0.0));
    let f = app.search.find.as_mut().unwrap();
    f.pdf_hl = hits.into_iter().map(|r| (page, r)).collect();
    f.found = Some((String::new(), Instant::now()));
}

/// Where `needle` (normalized words) is on a page: one box per line it covers.
fn marks(chars: &[crate::pdf::Ch], needle: &str) -> Vec<[f32; 4]> {
    let target: Vec<char> = needle.chars().collect();
    if target.is_empty() {
        return vec![];
    }
    // the page's text normalized like the index's, remembering each character's source
    let mut text: Vec<char> = Vec::new();
    let mut src: Vec<usize> = Vec::new();
    let mut space = true;
    for (i, c) in chars.iter().enumerate() {
        if c.c.is_alphanumeric() {
            for l in c.c.to_lowercase() {
                text.push(l);
                src.push(i);
            }
            space = false;
        } else if !space {
            text.push(' ');
            src.push(i);
            space = true;
        }
    }
    let mut out: Vec<[f32; 4]> = Vec::new();
    let mut at = 0;
    while at + target.len() <= text.len() {
        let Some(k) = text[at..].windows(target.len()).position(|w| w == target.as_slice()) else { break };
        let (a, b) = (src[at + k], src[at + k + target.len() - 1]);
        let mut line: Option<[f32; 4]> = None;
        for c in &chars[a..=b] {
            if c.x1 <= c.x0 {
                continue;
            }
            match &mut line {
                Some(l) if (c.top - l[1]).abs() < 0.5 * (l[3] - l[1]) => {
                    l[0] = l[0].min(c.x0);
                    l[2] = l[2].max(c.x1);
                    l[1] = l[1].min(c.top);
                    l[3] = l[3].max(c.bottom);
                }
                _ => {
                    if let Some(l) = line.take() {
                        out.push(l);
                    }
                    line = Some([c.x0, c.top, c.x1, c.bottom]);
                }
            }
        }
        out.extend(line);
        at += k + target.len();
    }
    out
}

/// A PDF page's highlighted runs (fractions of the page) and their strength.
pub fn pdf_marks(app: &App, fid: &str, page: usize) -> (Vec<[f32; 4]>, f32) {
    let Some(f) = app.search.find.as_ref() else { return (vec![], 0.0) };
    if f.kind != "pdf" || !f.href.ends_with(&format!("/f/{fid}")) {
        return (vec![], 0.0);
    }
    let Some((_, since)) = &f.found else { return (vec![], 0.0) };
    let e = since.elapsed().as_secs_f32();
    let a = if e < HIGHLIGHT - 0.8 { 1.0 } else { ((HIGHLIGHT - e) / 0.8).max(0.0) };
    (f.pdf_hl.iter().filter(|(p, _)| *p == page).map(|(_, r)| *r).collect(), a)
}

// ---------- transcripts ----------
fn transcript(app: &mut App, cid: &str, rid: &str) -> Result<Value, Need> {
    let key = format!("{cid}:{rid}");
    if let Some(r) = app.search.transcripts.get(&key) {
        return r.clone().map_err(Need::Err);
    }
    if app.search.loading.insert(key.clone()) {
        let (svc, c, r) = (app.svc.clone(), cid.to_string(), rid.to_string());
        app.spawn(async move { svc.transcript(&c, &r).await }, move |app, res| {
            app.search.loading.remove(&key);
            app.search.transcripts.insert(key, res);
        });
    }
    Err(Need::Pending)
}

/// The transcript's markdown as simple HTML: a heading or paragraph per line, "- " lines as a
/// list (its title is the page's).
fn md_html(md: &str) -> String {
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let mut out = String::new();
    let mut in_list = false;
    for (i, line) in md.lines().enumerate() {
        let l = line.trim();
        if i == 0 && l.starts_with("# ") {
            continue;
        }
        let item = l.strip_prefix("- ");
        if in_list && item.is_none() {
            out.push_str("</ul>");
            in_list = false;
        }
        if l.is_empty() {
            continue;
        }
        if let Some(it) = item {
            if !in_list {
                out.push_str("<ul>");
                in_list = true;
            }
            out.push_str(&format!("<li>{}</li>", esc(it)));
        } else if let Some(h) = l.strip_prefix("## ") {
            out.push_str(&format!("<h2>{}</h2>", esc(h)));
        } else {
            out.push_str(&format!("<p>{}</p>", esc(l)));
        }
    }
    if in_list {
        out.push_str("</ul>");
    }
    out
}

pub fn recording_view(app: &mut App, ui: &mut Ui, pane: Pane, cid: &str, rid: &str) -> Result<(), Need> {
    let info = crate::views::canvas::course_info(app, cid)?;
    let t_ = transcript(app, cid, rid)?;
    let title = fmt::s(&t_["title"]);
    let code = info.c["course_code"].as_str().filter(|x| !x.is_empty()).or(info.c["name"].as_str()).unwrap_or("").to_string();
    let crumbs = vec![crate::views::dashboard_crumb(), (code, Some(format!("#/c/{cid}/modules"))), ("Recording".into(), None)];
    crate::views::head(app, ui, pane, &title, crumbs, None);
    crate::views::h1(ui, pane, &title);
    crate::html::content(app, ui, &md_html(t_["text"].as_str().unwrap_or("")), pane);
    Ok(())
}

// ---------- PDFs into the index ----------
/// Every few minutes: extract the text of downloaded PDFs not yet indexed, one at a time.
pub fn tick(app: &mut App) {
    let due = app.search.pdf_at.get_or_insert_with(|| Instant::now() + Duration::from_secs(4));
    if Instant::now() < *due || app.search.pdf_busy {
        return;
    }
    app.search.pdf_at = Some(Instant::now() + Duration::from_secs(600));
    app.search.pdf_busy = true;
    let svc = app.svc.clone();
    app.spawn(async move { tokio::task::spawn_blocking(move || svc.pdfs_to_index()).await.unwrap_or_default() }, |app, todo| {
        next_pdf(app, todo);
    });
}

fn next_pdf(app: &mut App, mut todo: Vec<Value>) {
    let Some(item) = todo.pop() else {
        app.search.pdf_busy = false;
        return;
    };
    let path = std::path::PathBuf::from(fmt::s(&item["path"]));
    let (tx, reply_tx) = (app.tx.clone(), app.tx.clone());
    app.pdf.extract(&tx, path, Box::new(move |pages| {
        let tx = reply_tx;
        let _ = tx.send(crate::app::Msg::Apply(Box::new(move |app: &mut App| {
            if let Some(pages) = pages {
                let svc = app.svc.clone();
                let it = item.clone();
                app.fire(async move {
                    let _ = tokio::task::spawn_blocking(move || svc.index_pdf(&it, pages)).await;
                });
            }
            next_pdf(app, todo);
        })));
        crate::app::wake();
    }));
}
