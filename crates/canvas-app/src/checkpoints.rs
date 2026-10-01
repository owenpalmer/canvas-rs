//! Checkpoint mode for PDFs in the viewer, ported from CheckpointReader (github: pdf-checkpoints).
//! Split a PDF anywhere, and Claude writes retrieval-practice questions about the passage above
//! the split (back to the previous checkpoint, at most two pages); send the good ones to Anki.
//!
//! A PDF's checkpoints always show; checkpoint mode (M, or the Checkpoints button) is for adding
//! them: J/K step through the text sentence by sentence and C splits after the highlighted one, or
//! A turns on click-to-place. Space shows an answer and ←/→ switch questions, whenever one is on
//! screen. Writing questions, the API key and saving are canvas_mcp::checkpoints.
//!
//! A place in a PDF is page index + fraction of the page's height ("abs"), so a passage can span
//! page breaks. Checkpoints are saved per PDF in CheckpointReader's format.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use canvas_mcp::checkpoints::GenError;
use egui::{Color32, CursorIcon, Id, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use serde_json::{Value, json};
use unicode_segmentation::UnicodeSegmentation;

use crate::app::{App, Msg, Pane};
use crate::html::{Env, Flavor};
use crate::pdf::{Info, PageText};
use crate::theme::{self, t};
use crate::widgets::{self as w, Rich, Ts, cr};

const MAX_REGION_PAGES: f32 = 2.0; // how far back a checkpoint's passage reaches
const CONTEXT_PAGES: f32 = 1.5; // background text sent from before the passage
pub const CP_DECK: &str = "Canvas checkpoints"; // for courses without a linked Anki deck
const OPEN_SECS: f32 = 1.5;

// ---------- state ----------
#[derive(Clone)]
pub struct Card {
    pub uid: u64,
    pub q: String,
    pub a: Option<String>,
    pub revealed: bool,
    pub anki: Value,
    pub editing: bool,
    pub loading: bool,
    pub edit_q: String,
    pub edit_a: String,
}

impl Card {
    fn new(q: String, a: Option<String>) -> Card {
        Card { uid: uid(), q, a, revealed: false, anki: Value::Null, editing: false, loading: false, edit_q: String::new(), edit_a: String::new() }
    }
}

fn uid() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(1);
    N.fetch_add(1, Ordering::Relaxed)
}

pub struct Cut {
    pub id: String,
    pub page: usize,
    pub y: f32,
    pub cards: Vec<Card>,
    pub idx: usize,
    pub status: String,
    pub error: Option<String>,
    pub range: Option<(f32, f32)>,
    pub mock: Value,
    pub hint_file: Option<String>,
    pub streaming: bool,
    pub opened: Option<Instant>,
    pub slide: Option<(f32, Instant)>,
    pub adding_anki: bool,
    pub allowing: bool,
}

impl Cut {
    fn abs(&self) -> f32 {
        self.page as f32 + self.y
    }
}

#[derive(Clone)]
struct Rects {
    page: usize,
    x0: f32,
    x1: f32,
    top: f32,
    bottom: f32,
}

struct Sentence {
    rects: Vec<Rects>,
}

pub struct DocSt {
    pub cuts: Vec<Cut>,
    sentences: Option<Vec<Sentence>>,
    sent: i64,
    pending_cut: Option<(usize, f32)>,
    active: Option<String>,
    fid: String,
    name: String,
    course: Option<String>,
    pages: usize,
    save_at: Option<Instant>,
    /// the PDF's chapters and sections (empty when it has no bookmarks)
    outline: Arc<Vec<crate::pdf::OutlineItem>>,
    /// the pages the reading sentences were built from (long books: around where you are)
    sent_window: (usize, usize),
}

/// Something waiting for pages' text before it goes on.
struct Wait {
    fid: String,
    pages: Vec<usize>,
    then: Option<Box<dyn FnOnce(&mut App)>>,
}

pub struct Cp {
    pub mode: bool,
    pub adding: bool,
    docs: HashMap<String, DocSt>,
    /// The PDF in the viewer (its key, and the frame it was drawn in).
    current: Option<(String, u64)>,
    waits: Vec<Wait>,
    heights: HashMap<String, f32>,
    rects: HashMap<String, Rect>,
    view: Rect,
    origin: f32,
    auto_scroll_until: Option<Instant>,
    key_info: Option<Value>,
    key_loading: bool,
    key_editing: bool,
    key_note: String,
    key_text: String,
    key_busy: bool,
    prompt_text: Option<String>,
    deck_text: Option<String>,
}

impl Cp {
    pub fn load(p: &crate::prefs::Prefs) -> Cp {
        Cp {
            mode: p.bool("cpMode", false),
            adding: false,
            docs: HashMap::new(),
            current: None,
            waits: Vec::new(),
            heights: HashMap::new(),
            rects: HashMap::new(),
            view: Rect::NOTHING,
            origin: 0.0,
            auto_scroll_until: None,
            key_info: None,
            key_loading: false,
            key_editing: false,
            key_note: String::new(),
            key_text: String::new(),
            key_busy: false,
            prompt_text: None,
            deck_text: None,
        }
    }
}

fn fmt_pos(abs: f32) -> String {
    format!("p.{} {}%", abs.floor() as i64 + 1, ((abs % 1.0) * 100.0).round() as i64)
}

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

// ---------- the mode ----------
pub fn set_mode(app: &mut App, on: bool) {
    app.cp.mode = on;
    app.set_pref("cpMode", json!(on));
    if !on {
        app.cp.adding = false;
        if let Some(st) = current_doc(app) {
            st.sent = -1;
            st.pending_cut = None;
        }
    }
}

fn current_key(app: &App) -> Option<String> {
    app.cp.current.as_ref().filter(|(_, f)| *f + 2 >= app.frame_no).map(|(k, _)| k.clone())
}

fn current_doc(app: &mut App) -> Option<&mut DocSt> {
    let k = current_key(app)?;
    app.cp.docs.get_mut(&k)
}

// ---------- a PDF's checkpoints ----------
fn card_from(v: &Value) -> Card {
    let mut c = Card::new(v["q"].as_str().unwrap_or("").to_string(), v["a"].as_str().map(String::from));
    c.revealed = v["revealed"].as_bool().unwrap_or(false);
    c.anki = v["anki"].clone();
    c
}

fn cut_from(v: &Value) -> Option<Cut> {
    Some(Cut {
        id: v["id"].as_str().map(String::from).unwrap_or_else(new_id),
        page: v["page"].as_u64()? as usize,
        y: v["y"].as_f64()? as f32,
        cards: v["cards"].as_array().map(|a| a.iter().map(card_from).collect()).unwrap_or_default(),
        idx: v["idx"].as_u64().unwrap_or(0) as usize,
        status: match v["status"].as_str().unwrap_or("idle") {
            "loading" => "idle".into(),
            s => s.to_string(),
        },
        error: v["error"].as_str().map(String::from),
        range: v["range"].as_array().and_then(|r| Some((r.first()?.as_f64()? as f32, r.get(1)?.as_f64()? as f32))),
        mock: v["mock"].clone(),
        hint_file: v["hintFile"].as_str().map(String::from),
        streaming: false,
        opened: None,
        slide: None,
        adding_anki: false,
        allowing: false,
    })
}

fn cut_json(c: &Cut) -> Value {
    let mut v = json!({
        "id": c.id, "page": c.page, "y": c.y, "idx": c.idx,
        "status": if c.status == "loading" { "idle" } else { c.status.as_str() },
        "cards": c.cards.iter().map(|k| json!({"q": k.q, "a": k.a, "revealed": k.revealed, "anki": k.anki})).collect::<Vec<_>>(),
    });
    if let Some(e) = &c.error {
        v["error"] = json!(e);
    }
    if let Some((a, b)) = c.range {
        v["range"] = json!([a, b]);
    }
    if !c.mock.is_null() {
        v["mock"] = c.mock.clone();
    }
    if let Some(h) = &c.hint_file {
        v["hintFile"] = json!(h);
    }
    v
}

fn new_id() -> String {
    const A: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    (0..8).map(|_| A[rand::random::<u32>() as usize % A.len()] as char).collect()
}

/// pdf.rs calls this as it draws a PDF: its checkpoints load once, then stay in memory.
pub fn attach(app: &mut App, fid: &str, key: &str, name: &str, cid: Option<&str>, info: &Info) {
    if !app.cp.docs.contains_key(key) {
        let saved = canvas_mcp::checkpoints::load_state(key);
        let cuts = saved.as_array().map(|a| a.iter().filter_map(cut_from).collect()).unwrap_or_default();
        app.cp.docs.insert(
            key.to_string(),
            DocSt { cuts, sentences: None, sent: -1, pending_cut: None, active: None, fid: String::new(), name: String::new(), course: None, pages: 0, save_at: None, outline: Arc::new(vec![]), sent_window: (0, 0) },
        );
    }
    let st = app.cp.docs.get_mut(key).unwrap();
    st.fid = fid.to_string();
    st.name = name.to_string();
    st.course = cid.map(String::from);
    st.pages = info.pages.len();
    st.outline = info.outline.clone();
    app.cp.current = Some((key.to_string(), app.frame_no));
    if app.cp.mode {
        prepare_reading(app, key);
    }
}

fn save_cuts(app: &mut App, key: &str) {
    if let Some(st) = app.cp.docs.get_mut(key) {
        st.save_at = Some(Instant::now() + Duration::from_millis(300));
    }
    if let Some(c) = app.ctx() {
        c.request_repaint_after(Duration::from_millis(320));
    }
}

/// Every frame: debounced saves, and work waiting for pages' text.
pub fn tick(app: &mut App) {
    let now = Instant::now();
    let due: Vec<String> = app.cp.docs.iter().filter(|(_, s)| s.save_at.map(|t| t <= now).unwrap_or(false)).map(|(k, _)| k.clone()).collect();
    for k in due {
        let st = app.cp.docs.get_mut(&k).unwrap();
        st.save_at = None;
        let data = Value::Array(st.cuts.iter().map(cut_json).collect());
        let key = k.clone();
        app.spawn(async move { tokio::task::spawn_blocking(move || canvas_mcp::checkpoints::save_state(&key, &data).is_ok()).await.unwrap_or(false) }, |app, ok| {
            if !ok {
                app.toast("Couldn't save the checkpoints", true);
            }
        });
    }
    if app.cp.waits.is_empty() {
        return;
    }
    let mut waits = std::mem::take(&mut app.cp.waits);
    let tx = app.tx.clone();
    for wt in waits.iter_mut() {
        let mut ready = true;
        for &p in &wt.pages {
            if app.pdf.text(&tx, &wt.fid, p).is_none() {
                ready = false;
            }
        }
        if !app.pdf.docs.contains_key(&wt.fid) {
            wt.then = None; // the PDF closed
            continue;
        }
        if ready {
            if let Some(f) = wt.then.take() {
                f(app);
            }
        }
    }
    waits.retain(|wt| wt.then.is_some());
    waits.append(&mut app.cp.waits);
    app.cp.waits = waits;
    if !app.cp.waits.is_empty() {
        if let Some(c) = app.ctx() {
            c.request_repaint_after(Duration::from_millis(30));
        }
    }
}

fn wait_text(app: &mut App, fid: &str, pages: Vec<usize>, then: impl FnOnce(&mut App) + 'static) {
    app.cp.waits.push(Wait { fid: fid.to_string(), pages, then: Some(Box::new(then)) });
}

fn find_cut<'a>(app: &'a mut App, key: &str, id: &str) -> Option<&'a mut Cut> {
    app.cp.docs.get_mut(key)?.cuts.iter_mut().find(|c| c.id == id)
}

/// The checkpoints on a page, top to bottom: (id, y).
pub fn cuts_on(app: &App, key: &str, page: usize) -> Vec<(String, f32)> {
    let Some(st) = app.cp.docs.get(key) else { return vec![] };
    let mut v: Vec<(String, f32)> = st.cuts.iter().filter(|c| c.page == page).map(|c| (c.id.clone(), c.y)).collect();
    v.sort_by(|a, b| a.1.total_cmp(&b.1));
    v
}

pub fn add_cut(app: &mut App, key: &str, page: usize, y: f32) {
    if !(0.01..=0.995).contains(&y) {
        return;
    }
    let Some(st) = app.cp.docs.get_mut(key) else { return };
    if st.cuts.iter().any(|c| c.page == page && (c.y - y).abs() < 0.01) {
        return;
    }
    let id = new_id();
    st.cuts.push(Cut {
        id: id.clone(),
        page,
        y,
        cards: vec![],
        idx: 0,
        status: "idle".into(),
        error: None,
        range: None,
        mock: Value::Null,
        hint_file: None,
        streaming: false,
        opened: Some(Instant::now()),
        slide: None,
        adding_anki: false,
        allowing: false,
    });
    save_cuts(app, key); // kept even if the app closes before its questions are written
    generate(app, key, &id);
}

fn remove_cut(app: &mut App, key: &str, id: &str) {
    if let Some(st) = app.cp.docs.get_mut(key) {
        st.cuts.retain(|c| c.id != id);
    }
    save_cuts(app, key);
}

/// The passage a checkpoint asks about: back to the previous checkpoint, at most two pages. In a
/// PDF with chapters and sections, back to the start of the section it's in instead (so a
/// checkpoint in 10.1 starts at 10.1, not in chapter 9), at most MAX_SECTION_PAGES.
fn region_for(app: &App, st: &DocSt, id: &str) -> (f32, f32) {
    let end = st.cuts.iter().find(|c| c.id == id).map(|c| c.abs()).unwrap_or(0.0);
    let prev = st.cuts.iter().map(|c| c.abs()).filter(|a| *a < end).fold(0.0f32, f32::max);
    passage_bounds(prev, section_start(app, st, end), end)
}

/// A passage from the last checkpoint before `end` (`prev`, or 0) and the start of the section
/// `end` is in (when the PDF has sections).
fn passage_bounds(prev: f32, section: Option<f32>, end: f32) -> (f32, f32) {
    match section {
        Some(start) => (prev.max(start).max(end - MAX_SECTION_PAGES), end),
        None => (prev.max(end - MAX_REGION_PAGES), end),
    }
}

/// How far back a checkpoint's passage reaches in a book with sections.
const MAX_SECTION_PAGES: f32 = 8.0;
/// Pictures sent of a passage: its last pages.
const MAX_IMAGES: usize = 4;

/// Where the section containing `pos` starts (its heading, found on the page when its text is
/// loaded, else the top of the page), in a PDF with an outline.
fn section_start(app: &App, st: &DocSt, pos: f32) -> Option<f32> {
    let last = crate::pdf::section_at(&st.outline, pos - 0.001)?;
    // Bookmarks point at the top of the heading's page; the heading itself may be lower, below
    // `pos`, and then `pos` is still in the section before it.
    for i in (0..=last).rev().take(4) {
        let it = &st.outline[i];
        let start = it.page as f32 + heading_y(app, &st.fid, it).unwrap_or(it.y);
        if start < pos {
            return Some(start);
        }
    }
    let it = &st.outline[last];
    Some(it.page as f32 + it.y)
}

/// Where an outline entry's heading is on its page (the top of its line), when the page's text is
/// loaded: the first line that reads like the title, preferring larger type.
pub fn heading_y(app: &App, fid: &str, it: &crate::pdf::OutlineItem) -> Option<f32> {
    let text = app.pdf.docs.get(fid)?.text.get(&it.page)?.clone();
    find_heading(&text, &it.title)
}

/// Where a heading with this title is on a page (fraction from the top), if it's there.
pub fn find_heading(text: &PageText, title: &str) -> Option<f32> {
    let words = |s: &str| canvas_mcp::fulltext::norm(s).split(' ').filter(|w| !w.is_empty() && !w.chars().all(|c| c.is_ascii_digit())).map(String::from).collect::<Vec<_>>();
    let want: Vec<String> = words(title).into_iter().take(4).collect();
    if want.is_empty() {
        return None;
    }
    // the page's lines: runs on the same baseline
    let mut lines: Vec<(String, f32, f32)> = Vec::new(); // (text, top, height)
    for item in &text.items {
        match lines.last_mut() {
            Some(l) if ((l.1 + l.2) - item.y).abs() < 0.5 * l.2.max(item.h) => {
                l.0.push(' ');
                l.0.push_str(&item.s);
                l.2 = l.2.max(item.h);
            }
            _ => lines.push((item.s.clone(), item.y - item.h, item.h)),
        }
    }
    let mut hs: Vec<f32> = lines.iter().map(|l| l.2).collect();
    hs.sort_by(f32::total_cmp);
    let median = hs.get(hs.len() / 2).copied().unwrap_or(0.0);
    let line_words: Vec<Vec<String>> = lines.iter().map(|l| words(&l.0)).collect();
    // the title's first words on one line; a heading that wraps has fewer of them there, so then
    // fewer words will do, but only in heading-sized type
    for n in (want.len().min(2)..=want.len()).rev() {
        let w = &want[..n];
        let found: Vec<usize> = (0..lines.len()).filter(|&i| line_words[i].windows(n).any(|x| x == w)).collect();
        let big = found.iter().copied().find(|&i| lines[i].2 > median * 1.15);
        let pick = if n == want.len() { big.or(found.first().copied()) } else { big };
        if let Some(i) = pick {
            return Some((lines[i].1 - 0.004).max(0.0));
        }
    }
    None
}

#[derive(Clone, Copy, Debug)]
struct Seg {
    page: usize,
    y0: f32,
    y1: f32,
}

/// [a, b) split into per-page pieces.
fn segments(pages: usize, a: f32, b: f32) -> Vec<Seg> {
    let mut out = Vec::new();
    if pages == 0 {
        return out;
    }
    let (fa, fb) = (a.floor() as usize, b.floor() as usize);
    for pi in fa..=fb.min(pages - 1) {
        let y0 = if pi == fa { a - pi as f32 } else { 0.0 };
        let y1 = if pi == fb { b - pi as f32 } else { 1.0 };
        if y1 - y0 > 0.004 {
            out.push(Seg { page: pi, y0, y1 });
        }
    }
    out
}

fn text_between(app: &App, fid: &str, pages: usize, a: f32, b: f32) -> String {
    let mut s = String::new();
    let Some(d) = app.pdf.docs.get(fid) else { return s };
    for seg in segments(pages, a, b) {
        if let Some(t_) = d.text.get(&seg.page) {
            for it in &t_.items {
                if it.y >= seg.y0 && it.y < seg.y1 {
                    s += &it.s;
                    s.push(if it.eol { '\n' } else { ' ' });
                }
            }
        }
        s.push('\n');
    }
    let re = regex::Regex::new(r"[ \t]+").unwrap();
    re.replace_all(&s, " ").trim().to_string()
}

fn seg_pages(pages: usize, a: f32, b: f32) -> Vec<usize> {
    segments(pages, a, b).iter().map(|s| s.page).collect()
}

// ---------- pictures of the passage ----------
/// Each piece of the passage as a picture `width` pixels wide, on white.
fn render_segments(app: &mut App, fid: &str, segs: Vec<Seg>, width: u32, done: impl FnOnce(&mut App, Vec<image::RgbaImage>) + Send + 'static) {
    type Done = Box<dyn FnOnce(&mut App, Vec<image::RgbaImage>) + Send>;
    let n = segs.len();
    let slots: Arc<Mutex<(Vec<Option<image::RgbaImage>>, Option<Done>)>> = Arc::new(Mutex::new((vec![None; n], Some(Box::new(done)))));
    if n == 0 {
        let d = slots.lock().unwrap().1.take().unwrap();
        d(app, vec![]);
        return;
    }
    let mut pages: Vec<usize> = segs.iter().map(|s| s.page).collect();
    pages.dedup();
    for p in pages {
        let (slots, segs, tx) = (slots.clone(), segs.clone(), app.tx.clone());
        let tx2 = app.tx.clone();
        app.pdf.render_plain(
            &tx2,
            fid,
            p,
            width,
            Box::new(move |img| {
                let full = img.map(|ci| {
                    let [w_, h_] = ci.size;
                    let raw: Vec<u8> = ci.pixels.iter().flat_map(|c| c.to_array()).collect();
                    image::RgbaImage::from_raw(w_ as u32, h_ as u32, raw).unwrap_or_else(|| image::RgbaImage::new(1, 1))
                });
                let mut g = slots.lock().unwrap();
                for (i, s) in segs.iter().enumerate() {
                    if s.page != p {
                        continue;
                    }
                    let crop = match &full {
                        Some(m) => {
                            let sy = (s.y0 * m.height() as f32).round() as u32;
                            let sh = ((s.y1 - s.y0) * m.height() as f32).round().max(1.0) as u32;
                            let sy = sy.min(m.height().saturating_sub(1));
                            let sh = sh.min(m.height() - sy).max(1);
                            image::imageops::crop_imm(m, 0, sy, m.width(), sh).to_image()
                        }
                        None => image::RgbaImage::from_pixel(width, 1, image::Rgba([255, 255, 255, 255])),
                    };
                    g.0[i] = Some(crop);
                }
                if g.0.iter().all(|x| x.is_some()) {
                    if let Some(d) = g.1.take() {
                        let parts: Vec<image::RgbaImage> = g.0.iter_mut().map(|x| x.take().unwrap()).collect();
                        let _ = tx.send(Msg::Apply(Box::new(move |app: &mut App| d(app, parts))));
                        crate::app::wake();
                    }
                }
            }),
        );
    }
}

pub fn png_b64(img: &image::RgbaImage) -> String {
    let mut buf = Cursor::new(Vec::new());
    let _ = image::DynamicImage::ImageRgba8(img.clone()).to_rgb8().write_to(&mut buf, image::ImageFormat::Png);
    base64::engine::general_purpose::STANDARD.encode(buf.into_inner())
}

/// The whole passage as one image, its pages stacked on grey.
fn stack(parts: &[image::RgbaImage]) -> image::RgbaImage {
    let w_ = parts.iter().map(|p| p.width()).max().unwrap_or(1);
    let h_ = (parts.iter().map(|p| p.height() + 8).sum::<u32>()).saturating_sub(8).max(1);
    let mut out = image::RgbaImage::from_pixel(w_, h_, image::Rgba([0xdd, 0xdd, 0xdd, 255]));
    let mut y = 0;
    for p in parts {
        image::imageops::overlay(&mut out, p, 0, y as i64);
        y += p.height() + 8;
    }
    out
}

// ---------- writing questions ----------
#[derive(Clone, Copy)]
enum Target {
    Fill,
    Replace(u64),
}

/// Questions stream in and are added as each is written, so the first shows before the rest exist.
fn request_cards(app: &mut App, key: &str, id: &str, count: Option<usize>, avoid: Vec<String>, target: Target) {
    let Some(st) = app.cp.docs.get(key) else { return };
    let Some(cut) = st.cuts.iter().find(|c| c.id == id) else { return };
    let (a, b) = cut.range.unwrap_or_else(|| region_for(app, st, id));
    let (fid, pages, name) = (st.fid.clone(), st.pages, st.name.clone());
    // background from before the passage, but not from an earlier section
    let floor = section_start(app, st, a + 0.0005).filter(|f| *f <= a).unwrap_or(0.0);
    let c0 = (a - CONTEXT_PAGES).max(0.0).max(floor);
    let mut need = seg_pages(pages, c0, a);
    need.extend(seg_pages(pages, a, b));
    need.sort();
    need.dedup();
    let (key, id) = (key.to_string(), id.to_string());
    wait_text(app, &fid.clone(), need, move |app| {
        let section = text_between(app, &fid, pages, a, b);
        let context = if c0 < a { text_between(app, &fid, pages, c0, a) } else { String::new() };
        let chars: Vec<char> = context.chars().collect();
        let context: String = chars[chars.len().saturating_sub(4000)..].iter().collect();
        // pictures of the passage's last pages
        let segs = segments(pages, a, b);
        let segs = segs[segs.len().saturating_sub(MAX_IMAGES)..].to_vec();
        render_segments(app, &fid, segs, 1100, move |app, parts| {
            app.prefs.flush(true); // the model settings are read from the file
            let tx = app.tx.clone();
            app.fire(async move {
                let images = tokio::task::spawn_blocking(move || parts.iter().map(png_b64).collect::<Vec<_>>()).await.unwrap_or_default();
                let mut body = json!({
                    "doc_title": name, "location": format!("{} → {}", fmt_pos(a), fmt_pos(b)),
                    "section_text": section, "context_text": context, "images": images,
                });
                if let Some(n) = count {
                    body["count"] = json!(n);
                }
                if !avoid.is_empty() {
                    body["avoid"] = json!(avoid);
                }
                let mut rx = canvas_mcp::checkpoints::generate(body);
                let mut done = false;
                let mut err = None;
                while let Some(ev) = rx.recv().await {
                    match ev {
                        Ok(v) => {
                            if v["done"] == true {
                                done = true;
                            }
                            let (k, i) = (key.clone(), id.clone());
                            let _ = tx.send(Msg::Apply(Box::new(move |app: &mut App| on_event(app, &k, &i, target, v))));
                            crate::app::wake();
                        }
                        Err(e) => {
                            err = Some(e);
                            break;
                        }
                    }
                }
                if err.is_none() && !done {
                    err = Some(GenError { kind: "generate", status: 502, message: "The connection closed before the questions were finished.".into() });
                }
                let _ = tx.send(Msg::Apply(Box::new(move |app: &mut App| finish(app, &key, &id, target, err))));
                crate::app::wake();
            });
        });
    });
}

fn on_event(app: &mut App, key: &str, id: &str, target: Target, v: Value) {
    let Some(cut) = find_cut(app, key, id) else { return };
    let i = v["i"].as_u64().map(|i| i as usize);
    if v["done"] == true {
        cut.mock = v["mock"].clone();
    }
    match target {
        Target::Fill => {
            if let (Some(i), Some(card)) = (i, v.get("card")) {
                let c = Card::new(card["question"].as_str().unwrap_or("").to_string(), card["answer"].as_str().map(String::from));
                while cut.cards.len() < i {
                    cut.cards.push(Card::new(String::new(), None));
                }
                if i < cut.cards.len() {
                    cut.cards[i] = c;
                } else {
                    cut.cards.push(c);
                }
                cut.status = "ready".into();
            }
            if let (Some(i), Some(a)) = (i, v["answer"].as_str()) {
                if let Some(c) = cut.cards.get_mut(i) {
                    c.a = Some(a.to_string());
                }
            }
        }
        Target::Replace(u) => {
            if i == Some(0) {
                if let Some(c) = cut.cards.iter_mut().find(|c| c.uid == u) {
                    if let Some(card) = v.get("card") {
                        c.q = card["question"].as_str().unwrap_or("").to_string();
                        c.a = card["answer"].as_str().map(String::from);
                        c.revealed = false;
                        c.anki = Value::Null;
                    }
                    if let Some(a) = v["answer"].as_str() {
                        c.a = Some(a.to_string());
                    }
                }
            }
        }
    }
}

fn finish(app: &mut App, key: &str, id: &str, target: Target, err: Option<GenError>) {
    let mut toast = None;
    if let Some(cut) = find_cut(app, key, id) {
        match target {
            Target::Fill => {
                cut.status = "ready".into();
                if let Some(e) = err {
                    // Questions that already arrived stay; the error only replaces an empty widget.
                    if !cut.cards.is_empty() {
                        toast = Some(e.message);
                    } else if e.kind == "key" {
                        cut.status = "nokey".into();
                    } else if e.kind == "permission" {
                        cut.status = "perm".into();
                    } else {
                        cut.status = "error".into();
                        cut.error = Some(e.message);
                    }
                }
                cut.streaming = false;
            }
            Target::Replace(u) => {
                if let Some(e) = err {
                    toast = Some(e.message);
                }
                if let Some(c) = cut.cards.iter_mut().find(|c| c.uid == u) {
                    c.loading = false;
                }
            }
        }
    }
    if let Some(m) = toast {
        app.toast(m, true);
    }
    save_cuts(app, key);
}

fn generate(app: &mut App, key: &str, id: &str) {
    let Some(st) = app.cp.docs.get_mut(key) else { return };
    let Some(cut) = st.cuts.iter_mut().find(|c| c.id == id) else { return };
    let end = cut.abs();
    cut.status = "loading".into();
    cut.error = None;
    cut.cards.clear();
    cut.idx = 0;
    cut.streaming = true;
    // the section's heading page needs its text to find where the section starts
    let pages: Vec<usize> = match crate::pdf::section_at(&st.outline, end - 0.001) {
        Some(last) => {
            let mut v: Vec<usize> = (0..=last).rev().take(4).map(|i| st.outline[i].page).collect();
            v.dedup();
            v
        }
        None => vec![],
    };
    let fid = st.fid.clone();
    let (k, i) = (key.to_string(), id.to_string());
    wait_text(app, &fid, pages, move |app| {
        let Some(st) = app.cp.docs.get(&k) else { return };
        let range = region_for(app, st, &i);
        if let Some(cut) = find_cut(app, &k, &i) {
            cut.range = Some(range);
        }
        request_cards(app, &k, &i, None, vec![], Target::Fill);
    });
}

/// Replace one question, steering away from the ones already there.
fn regenerate_card(app: &mut App, key: &str, id: &str, u: u64) {
    let Some(cut) = find_cut(app, key, id) else { return };
    let avoid = cut.cards.iter().map(|c| c.q.clone()).collect();
    if let Some(c) = cut.cards.iter_mut().find(|c| c.uid == u) {
        c.loading = true;
    }
    request_cards(app, key, id, Some(1), avoid, Target::Replace(u));
}

/// Checkpoints waiting for a key or the permission write their questions once there is one.
pub fn retry_waiting(app: &mut App) {
    let todo: Vec<(String, String)> = app.cp.docs.iter().flat_map(|(k, st)| st.cuts.iter().filter(|c| c.status == "nokey" || c.status == "perm").map(|c| (k.clone(), c.id.clone())).collect::<Vec<_>>()).collect();
    for (k, id) in todo {
        generate(app, &k, &id);
    }
}

// ---------- Anki ----------
/// Anki can't draw molecules, so each goes to Anki's media folder as a PNG.
fn anki_field(text: &str, media: &mut Vec<Value>) -> String {
    let mut html = esc(text);
    let re = regex::Regex::new(r"\\smiles\{([^}]+)\}").unwrap();
    let found: Vec<(String, String)> = re.captures_iter(&html).map(|c| (c[0].to_string(), c[1].to_string())).collect();
    for (whole, smi) in found {
        let mut hash: i32 = 0;
        for u in smi.encode_utf16() {
            hash = hash.wrapping_mul(31).wrapping_add(u as i32);
        }
        let filename = format!("pdfcp_mol_{}.png", to_base36(hash as u32));
        match crate::smiles::png(smi.trim(), 480, 340) {
            Some(bytes) => {
                if !media.iter().any(|m| m["filename"] == filename.as_str()) {
                    media.push(json!({"filename": filename, "data": base64::engine::general_purpose::STANDARD.encode(bytes)}));
                }
                html = html.replacen(&whole, &format!("<img class=\"mol\" src=\"{filename}\">"), 1);
            }
            None => html = html.replacen(&whole, &format!("<code>{smi}</code>"), 1),
        }
    }
    html
}

fn to_base36(mut n: u32) -> String {
    if n == 0 {
        return "0".into();
    }
    let mut s = Vec::new();
    while n > 0 {
        s.push(b"0123456789abcdefghijklmnopqrstuvwxyz"[(n % 36) as usize]);
        n /= 36;
    }
    s.reverse();
    String::from_utf8(s).unwrap()
}

/// Into the course's Anki deck (the one linked on the Anki page), with the passage as the hint.
fn send_to_anki(app: &mut App, key: &str, id: &str, u: u64) {
    if !app.allowed("anki") {
        app.toast("Allow the app to use Anki first, on the Anki page.", true);
        return;
    }
    let Some(st) = app.cp.docs.get(key) else { return };
    let Some(cut) = st.cuts.iter().find(|c| c.id == id) else { return };
    let Some(card) = cut.cards.iter().find(|c| c.uid == u).cloned() else { return };
    let (a, b) = cut.range.unwrap_or_else(|| region_for(app, st, id));
    let hint_file = cut.hint_file.clone().unwrap_or_else(|| format!("pdfcp_{}.png", cut.id));
    let need_hint = cut.hint_file.is_none();
    let (fid, pages, name, course) = (st.fid.clone(), st.pages, st.name.clone(), st.course.clone());
    let code = course.as_ref().and_then(|c| app.d.peek("courses").and_then(|v| v.as_array().and_then(|a| a.iter().find(|x| crate::fmt::id(&x["id"]) == *c).map(|x| crate::fmt::s(&x["course_code"])))));
    let deck = { let d = app.prefs.str("cpDeck"); if d.trim().is_empty() { CP_DECK.to_string() } else { d } };
    if let Some(c) = find_cut(app, key, id) {
        c.adding_anki = true;
    }
    let (key, id) = (key.to_string(), id.to_string());
    let segs = if need_hint { segments(pages, a, b) } else { vec![] };
    render_segments(app, &fid, segs, 900, move |app, parts| {
        let svc = app.svc.clone();
        let hint = hint_file.clone();
        let fut = async move {
            let (notes, media) = tokio::task::spawn_blocking(move || {
                let mut media = vec![];
                if need_hint {
                    media.push(json!({"filename": hint, "data": png_b64(&stack(&parts))}));
                }
                let source = [code.unwrap_or_default(), name, format!("{} → {}", fmt_pos(a), fmt_pos(b))].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
                let note = json!({
                    "front": anki_field(&card.q, &mut media), "back": anki_field(card.a.as_deref().unwrap_or(""), &mut media),
                    "hint": format!("<img src=\"{hint}\">"), "source": esc(&source),
                });
                (vec![note], media)
            })
            .await
            .unwrap_or_default();
            svc.checkpoint_anki(course.and_then(|c| c.parse().ok()), &deck, notes, media).await
        };
        app.spawn(fut, move |app, r| {
            if let Some(c) = find_cut(app, &key, &id) {
                c.adding_anki = false;
            }
            match r {
                Ok(data) => {
                    if let Some(c) = find_cut(app, &key, &id) {
                        c.hint_file = Some(hint_file);
                        if let Some(k) = c.cards.iter_mut().find(|k| k.uid == u) {
                            k.anki = data["ids"][0].clone();
                            if k.anki.is_null() {
                                k.anki = json!(true);
                            }
                        }
                        // on to the next question that isn't in Anki yet
                        if let Some(next) = c.cards.iter().enumerate().position(|(i, k)| i > c.idx && k.anki.is_null()) {
                            go_to(c, next);
                        }
                    }
                    let deck = data["deck"].as_str().unwrap_or("").to_string();
                    app.toast(format!("Added the card to “{deck}”"), false);
                    save_cuts(app, &key);
                }
                Err(e) => app.toast(if e.message().is_empty() { "Couldn't add the cards to Anki.".to_string() } else { e.message() }, true),
            }
        });
    });
}

// ---------- reading sentence by sentence ----------
/// Pages of sentences built at a time in a long PDF: a little before where you are, more after.
const READ_BEHIND: usize = 3;
const READ_AHEAD: usize = 12;

fn prepare_reading(app: &mut App, key: &str) {
    let Some(st) = app.cp.docs.get(key) else { return };
    if st.pages == 0 {
        return;
    }
    let (fid, n) = (st.fid.clone(), st.pages);
    // a short PDF is read whole; a long one around the page you're on
    let here = crate::pdf::current_pos(app, &fid).map(|p| p as usize).unwrap_or(0);
    let window = if n <= READ_BEHIND + READ_AHEAD + 1 { (0, n) } else { (here.saturating_sub(READ_BEHIND), (here + READ_AHEAD).min(n)) };
    if st.sentences.is_some() {
        let (a, b) = st.sent_window;
        // rebuilt once you've read past the window's start or near its end (not at the book's end)
        let inside = here >= a && (here + 3 <= b || b == n);
        if inside {
            return;
        }
    }
    let tx = app.tx.clone();
    let mut texts = Vec::with_capacity(window.1 - window.0);
    for p in window.0..window.1 {
        match app.pdf.text(&tx, &fid, p) {
            Some(t_) => texts.push((p, t_)),
            None => return, // asked; next frame
        }
    }
    let s = build_sentences_at(&texts);
    let st = app.cp.docs.get_mut(key).unwrap();
    // keep the highlighted sentence highlighted in the rebuilt list
    let was = usize::try_from(st.sent).ok().and_then(|i| st.sentences.as_ref()?.get(i)).map(|x| (x.rects[0].page, x.rects[0].top));
    st.sent = was.and_then(|(p, top)| s.iter().position(|x| x.rects[0].page == p && (x.rects[0].top - top).abs() < 1e-4)).map(|i| i as i64).unwrap_or(-1);
    st.sentences = Some(s);
    st.sent_window = window;
}

/// Sentences of some pages (their page numbers given): like build_sentences, numbered right.
fn build_sentences_at(pages: &[(usize, Arc<PageText>)]) -> Vec<Sentence> {
    let texts: Vec<Arc<PageText>> = pages.iter().map(|(_, t_)| t_.clone()).collect();
    let first = pages.first().map(|p| p.0).unwrap_or(0);
    let mut out = build_sentences(&texts);
    for s in out.iter_mut() {
        for r in s.rects.iter_mut() {
            r.page += first;
        }
    }
    out
}

/// The whole document's text as one string (remembering where each character came from), split
/// into sentences, each turned back into highlight boxes, one per line of text.
fn build_sentences(pages: &[Arc<PageText>]) -> Vec<Sentence> {
    struct Span {
        page: usize,
        it: usize,
        start: usize,
        end: usize,
    }
    let mut text = String::new();
    let mut spans: Vec<Span> = Vec::new();
    for (pi, p) in pages.iter().enumerate() {
        let mut prev: Option<&crate::pdf::TItem> = None;
        let mut line_start: Option<&crate::pdf::TItem> = None;
        for (ii, it) in p.items.iter().enumerate() {
            if let Some(pv) = prev {
                let ls = line_start.unwrap();
                let new_line = pv.eol || (it.y - pv.y).abs() > 0.5 * it.h.max(pv.h);
                if new_line && it.y - ls.y > 1.6 * ls.h {
                    text += "\n\n";
                } else if new_line {
                    if !text.ends_with('-') {
                        text.push(' ');
                    }
                } else if (it.x - pv.x - pv.w) * p.w > 0.12 * it.h * p.h {
                    text.push(' ');
                }
                if new_line {
                    line_start = Some(it);
                }
            } else {
                line_start = Some(it);
                text += "\n\n"; // never let a sentence run across a page break
            }
            if !it.s.is_empty() {
                let start = text.len();
                text += &it.s;
                spans.push(Span { page: pi, it: ii, start, end: text.len() });
            }
            prev = Some(it);
        }
    }
    let word = regex::Regex::new(r"[\p{L}\p{N}]{2}").unwrap();
    let mut out = Vec::new();
    let mut k = 0;
    for (s, seg) in text.split_sentence_bound_indices() {
        let e = s + seg.len();
        if !word.is_match(seg) {
            continue; // stray page numbers, bullets
        }
        while k < spans.len() && spans[k].end <= s {
            k += 1;
        }
        let mut rects: Vec<Rects> = Vec::new();
        let mut j = k;
        while j < spans.len() && spans[j].start < e {
            let sp = &spans[j];
            j += 1;
            let it = &pages[sp.page].items[sp.it];
            let a = s.max(sp.start) - sp.start;
            let b = e.min(sp.end) - sp.start;
            let part = it.s.get(a..b).unwrap_or("");
            if part.trim().is_empty() {
                continue;
            }
            // positions by character count, as pdf.js's string offsets
            let n = it.s.chars().count().max(1) as f32;
            let ca = it.s[..a].chars().count() as f32;
            let cb = it.s[..b].chars().count() as f32;
            let r = Rects { page: sp.page, x0: it.x + it.w * ca / n, x1: it.x + it.w * cb / n, top: it.y - it.h * 0.85, bottom: it.y + it.h * 0.25 };
            let same_line = rects.last().map(|l| l.page == r.page && l.bottom.min(r.bottom) - l.top.max(r.top) > 0.5 * (r.bottom - r.top)).unwrap_or(false);
            if same_line {
                let l = rects.last_mut().unwrap();
                l.x0 = l.x0.min(r.x0);
                l.x1 = l.x1.max(r.x1);
                l.top = l.top.min(r.top);
                l.bottom = l.bottom.max(r.bottom);
            } else {
                rects.push(r);
            }
        }
        if !rects.is_empty() {
            out.push(Sentence { rects });
        }
    }
    out
}

fn screen_y(app: &App, fid: &str, page: usize, y: f32) -> Option<f32> {
    crate::pdf::content_y(app, fid, page, y).map(|cy| cy + app.cp.origin)
}

fn hl_bounds(app: &App, key: &str) -> Option<(f32, f32)> {
    let st = app.cp.docs.get(key)?;
    let s = st.sentences.as_ref()?.get(usize::try_from(st.sent).ok()?)?;
    let mut top = f32::INFINITY;
    let mut bottom = f32::NEG_INFINITY;
    for r in &s.rects {
        if let (Some(a), Some(b)) = (screen_y(app, &st.fid, r.page, r.top.max(0.0)), screen_y(app, &st.fid, r.page, r.bottom.min(1.0))) {
            top = top.min(a);
            bottom = bottom.max(b);
        }
    }
    (top.is_finite()).then_some((top, bottom))
}

fn step_sentence(app: &mut App, key: &str, d: i64) {
    let Some(st) = app.cp.docs.get_mut(key) else { return };
    let Some(n) = st.sentences.as_ref().map(|s| s.len() as i64) else { return };
    if n == 0 {
        return;
    }
    st.pending_cut = None;
    let sent = st.sent;
    let fid = st.fid.clone();
    let view = app.cp.view;
    let cur = if sent >= 0 { hl_bounds(app, key) } else { None };
    let auto = app.cp.auto_scroll_until.map(|t| Instant::now() < t).unwrap_or(false);
    let i = if cur.map(|(t0, b0)| auto || (b0 > view.min.y && t0 < view.max.y)).unwrap_or(false) {
        (sent + d).clamp(0, n - 1)
    } else {
        // nothing highlighted on screen: start at the first sentence in sight
        let st = &app.cp.docs[key];
        st.sentences.as_ref().unwrap().iter().position(|s| screen_y(app, &fid, s.rects[0].page, s.rects[0].top).map(|y| y >= view.min.y).unwrap_or(false)).unwrap_or(0) as i64
    };
    app.cp.docs.get_mut(key).unwrap().sent = i;
    if let Some((t0, b0)) = hl_bounds(app, key) {
        if t0 < view.min.y + 24.0 || b0 > view.max.y - 48.0 {
            app.viewer.scroll_to = Some((app.viewer.scroll + t0 - view.min.y - view.height() * 0.3).max(0.0));
            app.cp.auto_scroll_until = Some(Instant::now() + Duration::from_millis(800));
        }
    }
}

/// Just after the highlighted sentence: in the gap below its last line, so the split doesn't cut
/// through the next line of text.
fn after_sentence(app: &App, key: &str) -> Option<(usize, f32)> {
    let st = app.cp.docs.get(key)?;
    let s = st.sentences.as_ref()?.get(usize::try_from(st.sent).ok()?)?;
    let last = s.rects.last()?;
    let text = app.pdf.docs.get(&st.fid)?.text.get(&last.page)?;
    let tops: Vec<f32> = text.items.iter().map(|it| it.y - it.h * 0.85).filter(|t_| *t_ >= last.bottom - 0.001).collect();
    let y = if tops.is_empty() { last.bottom + 0.01 } else { (last.bottom + tops.iter().cloned().fold(f32::INFINITY, f32::min)) / 2.0 };
    Some((last.page, y.clamp(0.011, 0.99)))
}

/// C: show where the checkpoint would go; C again adds it.
fn checkpoint_here(app: &mut App, key: &str) {
    let Some(st) = app.cp.docs.get_mut(key) else { return };
    if let Some((page, y)) = st.pending_cut.take() {
        add_cut(app, key, page, y);
    } else {
        let p = after_sentence(app, key);
        app.cp.docs.get_mut(key).unwrap().pending_cut = p;
    }
}

// ---------- drawing on the PDF's slices ----------
/// The highlighted sentence, and a checkpoint waiting to be confirmed (C twice), on a slice.
pub fn overlays(app: &mut App, ui: &mut Ui, key: &str, page: usize, y0: f32, y1: f32, r: Rect) {
    let Some(st) = app.cp.docs.get(key) else { return };
    let to_y = |y: f32| r.min.y + (y - y0) / (y1 - y0) * r.height();
    if app.cp.mode {
        if let Some(s) = st.sentences.as_ref().and_then(|s| s.get(usize::try_from(st.sent).ok()?)) {
            for rr in s.rects.iter().filter(|x| x.page == page) {
                let (t0, b0) = (rr.top.max(y0), rr.bottom.min(y1));
                if b0 <= t0 {
                    continue;
                }
                let hl = Rect::from_min_max(pos2(r.min.x + (rr.x0 - 0.003) * r.width(), to_y(t0)), pos2(r.min.x + (rr.x1 + 0.003) * r.width(), to_y(b0)));
                ui.painter().rect_filled(hl, cr(2.0), Color32::from_rgba_unmultiplied(255, 196, 0, 82));
            }
        }
    }
    if let Some((pp, py)) = st.pending_cut {
        if pp == page && py >= y0 && py <= y1 {
            // .cp-guide.pending: dashed, pulsing
            let tm = ui.input(|i| i.time) as f32;
            let ph = (tm / 1.2) % 2.0;
            let x = if ph < 1.0 { ph } else { 2.0 - ph };
            let op = 1.0 - 0.45 * crate::anim::ease_in_out(x);
            guide(ui, r, to_y(py), if app.touch_mode() { 2 } else { 1 }, op);
            ui.ctx().request_repaint();
        }
    }
}

/// The line that follows the mouse while placing a checkpoint.
pub fn follow_guide(ui: &mut Ui, r: Rect, y: f32) {
    guide(ui, r, y, 0, 1.0);
}

/// The checkpoint line across a slice: `kind` 0 follows the mouse, 1 is a proposed checkpoint
/// (keys), 2 a proposed one in touch mode.
fn guide(ui: &Ui, r: Rect, y: f32, kind: u8, op: f32) {
    let pending = kind > 0;
    let tk = t();
    let p = ui.painter().with_clip_rect(ui.clip_rect());
    let c = theme::alpha(tk.accent, op);
    if pending {
        let mut x = r.min.x;
        while x < r.max.x {
            p.line_segment([pos2(x, y + 1.0), pos2((x + 6.0).min(r.max.x), y + 1.0)], Stroke::new(2.0, c));
            x += 10.0;
        }
    } else {
        p.line_segment([pos2(r.min.x, y + 1.0), pos2(r.max.x, y + 1.0)], Stroke::new(2.0, c));
    }
    // the label: a pill at the right end
    let white = theme::alpha(Color32::WHITE, op);
    let ts = Ts::new(12.0, 600, white);
    let parts: Vec<(&str, bool)> = if pending {
        if kind == 2 {
            vec![("Tap ✓ to add the checkpoint here", false)]
        } else {
            vec![("Press ", false), ("C", true), (" again to add the checkpoint here · ", false), ("Esc", true), (" cancels", false)]
        }
    } else {
        vec![("＋ Check understanding up to here", false)]
    };
    let widths: Vec<f32> = parts.iter().map(|(s, k)| if *k { w::kbd_size(ui, s, 10.0).x + 2.0 } else { w::lay(ui, s, ts, None, false).size().x }).collect();
    let total: f32 = widths.iter().sum::<f32>() + 20.0;
    let h = 24.0;
    let pill = Rect::from_min_size(pos2(r.max.x - 8.0 - total, y - 13.0 + 1.0), vec2(total, h));
    let sh = egui::Shadow { offset: [0, 2], blur: 6, spread: 0, color: Color32::from_black_alpha((51.0 * op) as u8) };
    p.add(sh.as_shape(pill, cr(12.0)));
    p.rect_filled(pill, cr(12.0), c);
    let mut x = pill.min.x + 10.0;
    for ((s, k), wd) in parts.iter().zip(widths) {
        if *k {
            let ks = w::kbd_size(ui, s, 10.0);
            w::paint_kbd(ui, pos2(x + 1.0, pill.center().y - ks.y / 2.0), s, 10.0, Some(white), Some(theme::alpha(Color32::WHITE, 0.2 * op)), Some(theme::alpha(Color32::WHITE, 0.5 * op)));
        } else {
            let g = w::lay(ui, s, ts, None, false);
            p.galley(pos2(x, pill.center().y - g.size().y / 2.0), g, white);
        }
        x += wd;
    }
}

// ---------- the checkpoint widget ----------
/// Lucide icons (ISC license), as strokes in a 24px box.
fn lucide(ui: &Ui, rect: Rect, name: &str, color: Color32) {
    let s = rect.width() / 24.0;
    let at = |x: f32, y: f32| rect.min + vec2(x * s, y * s);
    let stroke = Stroke::new(2.0 * s, color);
    let line = |pts: Vec<Pos2>| {
        ui.painter().add(egui::Shape::line(pts, stroke));
    };
    let arc = |cx: f32, cy: f32, r: f32, a0: f32, a1: f32| -> Vec<Pos2> {
        let n = 24;
        (0..=n).map(|i| {
            let a = (a0 + (a1 - a0) * i as f32 / n as f32).to_radians();
            at(cx + r * a.cos(), cy + r * a.sin())
        }).collect()
    };
    match name {
        "left" => line(vec![at(15.0, 18.0), at(9.0, 12.0), at(15.0, 6.0)]),
        "right" => line(vec![at(9.0, 18.0), at(15.0, 12.0), at(9.0, 6.0)]),
        "x" => {
            line(vec![at(18.0, 6.0), at(6.0, 18.0)]);
            line(vec![at(6.0, 6.0), at(18.0, 18.0)]);
        }
        "edit" => {
            let mut pts = arc(19.0, 5.0, 2.83, -135.0, 45.0);
            pts.extend([at(7.5, 20.5), at(2.0, 22.0), at(3.5, 16.5), at(17.0, 3.0)]);
            line(pts);
        }
        "regen" => {
            let mut pts = arc(12.0, 12.0, 9.0, 0.0, 270.0 + 45.0);
            pts.push(at(21.0, 8.0));
            line(pts);
            line(vec![at(21.0, 3.0), at(21.0, 8.0), at(16.0, 8.0)]);
        }
        "trash" => {
            line(vec![at(3.0, 6.0), at(21.0, 6.0)]);
            line(vec![at(19.0, 6.0), at(19.0, 20.0), at(18.4, 21.4), at(17.0, 22.0), at(7.0, 22.0), at(5.6, 21.4), at(5.0, 20.0), at(5.0, 6.0)]);
            line(vec![at(8.0, 6.0), at(8.0, 4.0), at(8.6, 2.6), at(10.0, 2.0), at(14.0, 2.0), at(15.4, 2.6), at(16.0, 4.0), at(16.0, 6.0)]);
        }
        _ => {}
    }
}

/// .cp .icon: a 16px icon with 5px padding; accent on hover.
fn icon_btn(ui: &mut Ui, id: Id, name: &str, title: &str) -> bool {
    let tk = t();
    let (r, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click());
    let _ = id;
    let hov = resp.hovered();
    if hov {
        ui.painter().rect_filled(r, cr(6.0), tk.accent_soft);
    }
    lucide(ui, Rect::from_center_size(r.center(), vec2(16.0, 16.0)), name, if hov { tk.accent } else { tk.muted });
    resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(title).clicked()
}

/// .cp .primary: a small accent pill.
fn pill_btn(ui: &mut Ui, text: &str, disabled: bool) -> bool {
    let ts = Ts::new(13.0, 500, Color32::WHITE);
    let g = w::lay(ui, text, ts, None, false);
    let (r, resp) = ui.allocate_exact_size(vec2(g.size().x + 24.0, 27.0), if disabled { Sense::hover() } else { Sense::click() });
    let mut bg = t().accent;
    if resp.hovered() && !disabled {
        bg = theme::mix(bg, Color32::WHITE, 0.9);
    }
    if disabled {
        bg = theme::alpha(bg, 0.6);
    }
    ui.painter().rect_filled(r, cr(999.0), bg);
    ui.painter().galley(r.center() - g.size() / 2.0, g, Color32::WHITE);
    ui.add_space(6.0);
    !disabled && resp.on_hover_cursor(CursorIcon::PointingHand).clicked()
}

/// .cp .link
fn link_btn(ui: &mut Ui, text: &str) -> bool {
    let tk = t();
    w::text_button(ui, text, Ts::new(13.0, 600, tk.accent), Ts::new(13.0, 600, tk.accent).ul(true)).clicked()
}

enum Act {
    Prev,
    Next,
    Go(usize),
    Reveal(u64),
    Edit(u64),
    Cancel(u64),
    Save(u64),
    Drop,
    Regen(u64),
    RegenAll,
    Remove,
    Settings,
    Allow,
    Anki(u64),
}

fn go_to(cut: &mut Cut, i: usize) -> bool {
    if i >= cut.cards.len() || i == cut.idx {
        return false;
    }
    cut.slide = Some((if i > cut.idx { 1.0 } else { -1.0 }, Instant::now()));
    cut.idx = i;
    true
}

/// A checkpoint between two slices: it opens when added (the row grows from 0 over 1.5s).
pub fn widget(app: &mut App, ui: &mut Ui, pane: Pane, key: &str, id: &str, x0: f32, width: f32) {
    let tk = t();
    let natural = app.cp.heights.get(id).copied().unwrap_or(0.0);
    let Some(cut) = app.cp.docs.get(key).and_then(|s| s.cuts.iter().find(|c| c.id == id)) else { return };
    let (grow, op) = match cut.opened {
        Some(t0) => {
            let x = (t0.elapsed().as_secs_f32() / OPEN_SECS).min(1.0);
            if x < 1.0 {
                ui.ctx().request_repaint();
            }
            (crate::anim::cubic_bezier(0.22, 0.8, 0.26, 1.0, x), crate::anim::ease(x))
        }
        None => (1.0, 1.0),
    };
    let h = (natural * grow).round();
    let avail = ui.available_width();
    let (row, _) = ui.allocate_exact_size(vec2(avail, h), Sense::hover());
    let rect = Rect::from_min_size(pos2(x0, row.min.y), vec2(width, h));
    let clip = rect.intersect(ui.clip_rect());
    app.cp.rects.insert(id.to_string(), rect);
    // off screen: keep the height, skip the drawing
    if !rect.expand(200.0).intersects(ui.clip_rect()) && natural > 0.0 {
        return;
    }
    let p = ui.painter();
    p.rect_filled(rect, 0.0, theme::alpha(tk.panel_solid, op));
    p.hline(rect.x_range(), rect.min.y + 0.5, Stroke::new(1.0, theme::alpha(tk.line, op)));
    p.hline(rect.x_range(), rect.max.y - 0.5, Stroke::new(1.0, theme::alpha(tk.line, op)));
    let inner = Rect::from_min_size(rect.min, vec2(width, f32::INFINITY));
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)).id_salt(("cp", id)));
    c.set_clip_rect(clip);
    c.multiply_opacity(op);
    if app.cp.docs[key].active.as_deref() != Some(id) && c.rect_contains_pointer(rect) {
        app.cp.docs.get_mut(key).unwrap().active = Some(id.to_string());
    }
    let acts = draw_contents(app, &mut c, key, id);
    let used = c.min_rect().height().ceil();
    if (used - natural).abs() > 0.5 {
        app.cp.heights.insert(id.to_string(), used);
        ui.ctx().request_repaint();
    }
    for a in acts {
        act(app, pane, key, id, a);
    }
}

fn draw_contents(app: &mut App, ui: &mut Ui, key: &str, id: &str) -> Vec<Act> {
    let tk = t();
    let mut acts = Vec::new();
    let top = ui.cursor().min;
    let width = ui.available_width();
    ui.add_space(12.0);
    let cut = app.cp.docs[key].cuts.iter().find(|c| c.id == id).unwrap();
    let status = cut.status.clone();
    let msg = |ui: &mut Ui, add: &mut dyn FnMut(&mut Ui)| {
        let r = Rect::from_min_size(ui.cursor().min + vec2(40.0, 4.0), vec2(width - 80.0, f32::INFINITY));
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(r).layout(egui::Layout::top_down(egui::Align::Min)));
        c.spacing_mut().item_spacing = vec2(10.0, 6.0);
        c.horizontal_wrapped(|ui| add(ui));
        let hh = c.min_rect().height();
        ui.allocate_space(vec2(width, hh + 8.0));
    };
    match status.as_str() {
        "loading" => {
            msg(ui, &mut |ui| {
                w::spinner_soft(ui, 14.0);
                w::text_line(ui, "Writing questions about the passage above…", Ts::muted(14.0));
            });
        }
        "nokey" => msg(ui, &mut |ui| {
            w::text_line(ui, "Claude needs an Anthropic API key to write questions.", Ts::muted(14.0));
            if link_btn(ui, "Add one in Settings") {
                acts.push(Act::Settings);
            }
        }),
        "perm" => {
            let allowing = cut.allowing;
            msg(ui, &mut |ui| {
                let wd = ui.available_width();
                let g = w::lay(ui, "To write questions, the app sends this passage (a picture of it and its text) to Anthropic's Claude, with your API key. Nothing else from your courses is sent.", Ts::muted(13.0), Some(wd), false);
                let (r, _) = ui.allocate_exact_size(vec2(wd, g.size().y), Sense::hover());
                ui.painter().galley(r.min, g, tk.muted);
                if pill_btn(ui, "Allow and write questions", allowing) {
                    acts.push(Act::Allow);
                }
            })
        }
        "error" => {
            let e = cut.error.clone().unwrap_or_else(|| "Something went wrong.".into());
            msg(ui, &mut |ui| {
                let wd = ui.available_width();
                let g = w::lay(ui, &e, Ts::new(14.0, 400, tk.bad), Some(wd - 80.0), false);
                let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                ui.painter().galley(r.min, g, tk.bad);
                if link_btn(ui, "Try again") {
                    acts.push(Act::RegenAll);
                }
            })
        }
        _ if cut.cards.is_empty() => msg(ui, &mut |ui| {
            w::text_line(ui, "No questions left.", Ts::muted(14.0));
            if link_btn(ui, "Write more") {
                acts.push(Act::RegenAll);
            }
        }),
        _ => cards(app, ui, key, id, width, &mut acts),
    }
    ui.add_space(10.0);
    // the close button, top right
    let close = Rect::from_min_size(pos2(top.x + width - 8.0 - 18.0, top.y + 4.0), vec2(18.0, 18.0));
    let resp = ui.interact(close, Id::new(("cp-close", id)), Sense::click());
    let col = if resp.hovered() { tk.text } else { theme::alpha(tk.muted, 0.7) };
    lucide(ui, Rect::from_center_size(close.center(), vec2(14.0, 14.0)), "x", col);
    if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Remove this checkpoint").clicked() {
        acts.push(Act::Remove);
    }
    acts
}

fn answer_color() -> Color32 {
    let tk = t();
    theme::mix(tk.ok, tk.text, 0.45)
}

/// All cards share one cell, so the widget is as tall as its tallest card: switching cards never
/// changes its height, and nothing below jumps.
fn cards(app: &mut App, ui: &mut Ui, key: &str, id: &str, width: f32, acts: &mut Vec<Act>) {
    let tk = t();
    let cut = app.cp.docs.get_mut(key).unwrap().cuts.iter_mut().find(|c| c.id == id).unwrap();
    let n = cut.cards.len();
    cut.idx = cut.idx.min(n - 1);
    let idx = cut.idx;
    let streaming = cut.streaming;
    let slide = cut.slide.filter(|(_, t0)| t0.elapsed().as_secs_f32() < 0.3);
    let cards_: Vec<Card> = cut.cards.clone();
    let top = ui.cursor().min;
    let stack_x = top.x + 40.0;
    let stack_w = width - 80.0;
    // measure every card's body; draw the current one
    let mut tallest = 0.0f32;
    let mut cur_h = 0.0;
    for (i, c) in cards_.iter().enumerate() {
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(pos2(stack_x, top.y), vec2(stack_w, f32::INFINITY))).layout(egui::Layout::top_down(egui::Align::Min)).id_salt(("cp-card", id, c.uid)));
        if i != idx {
            child.set_invisible();
        } else if let Some((dir, t0)) = slide {
            let x = crate::anim::ease_out(t0.elapsed().as_secs_f32() / 0.3);
            child.multiply_opacity(x);
            ui.ctx().request_repaint();
            let _ = dir;
        }
        card_body(app, &mut child, key, id, c, streaming, i == idx, acts);
        let hh = child.min_rect().height();
        tallest = tallest.max(hh);
        if i == idx {
            cur_h = hh;
        }
    }
    // .card-row: margin-top auto, padding-top 10, min-height 36
    let row_top = top.y + tallest.max(cur_h) + 10.0;
    let row = Rect::from_min_size(pos2(stack_x, row_top), vec2(stack_w, 36.0));
    let c = &cards_[idx];
    let mut left = ui.new_child(egui::UiBuilder::new().max_rect(row).layout(egui::Layout::left_to_right(egui::Align::Center)).id_salt(("cp-left", id)));
    left.spacing_mut().item_spacing.x = 2.0;
    if c.loading {
    } else if c.editing {
        if pill_btn(&mut left, "Save", false) {
            acts.push(Act::Save(c.uid));
        }
        if link_btn(&mut left, "Cancel") {
            acts.push(Act::Cancel(c.uid));
        }
    } else {
        if !c.revealed && pill_btn(&mut left, "Show answer", false) {
            acts.push(Act::Reveal(c.uid));
        }
        if icon_btn(&mut left, Id::new(("e", id)), "edit", "Edit") {
            acts.push(Act::Edit(c.uid));
        }
        if icon_btn(&mut left, Id::new(("r", id)), "regen", "New question") {
            acts.push(Act::Regen(c.uid));
        }
        if icon_btn(&mut left, Id::new(("d", id)), "trash", "Discard") {
            acts.push(Act::Drop);
        }
    }
    // the dots, centered
    if n > 1 || streaming {
        let dots_w = n as f32 * 7.0 + (n.saturating_sub(1)) as f32 * 6.0 + if streaming { 14.0 } else { 0.0 };
        let mut x = row.center().x - dots_w / 2.0;
        for (i, k) in cards_.iter().enumerate() {
            let r = Rect::from_min_size(pos2(x, row.center().y - 3.5), vec2(7.0, 7.0));
            let resp = ui.interact(r.expand(3.0), Id::new(("cp-dot", id, i)), Sense::click());
            let col = if i == idx { tk.accent } else if !k.anki.is_null() { tk.ok } else { tk.line };
            ui.painter().circle_filled(r.center(), 3.5, col);
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                acts.push(Act::Go(i));
            }
            x += 13.0;
        }
        if streaming {
            w::paint_spinner(ui, pos2(x + 4.0, row.center().y), 8.0, 1.5, tk.accent_soft, tk.accent);
        }
    }
    // right: Anki
    let mut right = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(row.min, pos2(row.max.x - 12.0, row.max.y))).layout(egui::Layout::right_to_left(egui::Align::Center)).id_salt(("cp-right", id)));
    if !(c.loading || c.editing || c.a.is_none()) {
        if !c.anki.is_null() {
            w::text_line(&mut right, "✓ Anki", Ts::new(13.0, 600, tk.ok));
        } else if cut_adding(app, key, id) {
            w::text_line(&mut right, "Adding…", Ts::new(13.0, 600, theme::alpha(tk.accent, 0.6)));
        } else if link_btn(&mut right, "＋ Anki") {
            acts.push(Act::Anki(c.uid));
        }
    }
    // the side arrows, the full height
    let total = tallest + 10.0 + 36.0;
    for (dir, x, disabled, icon, label) in [(-1i32, top.x, idx == 0, "left", "Previous question (←)"), (1, top.x + width - 40.0, idx + 1 >= n, "right", "Next question (→)")] {
        let r = Rect::from_min_size(pos2(x, top.y), vec2(40.0, total));
        let resp = ui.interact(r, Id::new(("cp-side", id, dir)), if disabled { Sense::hover() } else { Sense::click() });
        let col = if disabled { theme::alpha(tk.muted, 0.2) } else if resp.hovered() { tk.accent } else { tk.muted };
        lucide(ui, Rect::from_center_size(r.center(), vec2(22.0, 22.0)), icon, col);
        if !disabled && resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(label).clicked() {
            acts.push(if dir < 0 { Act::Prev } else { Act::Next });
        }
    }
    ui.allocate_space(vec2(width, total));
}

fn cut_adding(app: &App, key: &str, id: &str) -> bool {
    app.cp.docs.get(key).and_then(|s| s.cuts.iter().find(|c| c.id == id)).map(|c| c.adding_anki).unwrap_or(false)
}

/// Questions and answers with math, as cards show them (#/mathtest; for checking the layout).
const MATH_SAMPLES: &[&str] = &[
    r"Each half keeps the same values of the intensive properties (T, P, ρ) as the original system. It has half the values of the extensive properties (\(\tfrac12 m\), \(\tfrac12 V\)).",
    r"What is a 'specific property,' and how is specific volume \(v\) defined in terms of extensive volume \(V\) and mass \(m\)?",
    r"The continuum idealization is valid when the system's characteristic length is much larger than the mean free path of the molecules: \(L \gg \lambda\).",
    r"For oxygen at 1 atm and 20°C it is about \(6.3 \times 10^{-8}\) m, roughly 200 times the molecule's diameter.",
    "\\[ \\mathrm{SG} = \\frac{\\rho}{\\rho_{\\mathrm{H_2O}}} \\]\nSpecific gravity is the dimensionless ratio of a substance's density to the density of water at 4°C.\n\nIt equals the density in g/cm³ because water at 4°C has \\(\\rho = 1\\ \\text{g/cm}^3 = 1\\ \\text{kg/L} = 1000\\ \\text{kg/m}^3\\).",
    r"Its density is \(\rho = \text{SG}\times\rho_{\ce{H2O}} = 13.6 \times 1000 = 13{,}600\ \text{kg/m}^3\), which is 13.6 g/cm³ (this is mercury).",
    r"Why does \(\frac{dP}{dz} = -\rho g\) imply that pressure grows with depth, and what is \(\int_0^h \rho g\,dz\) for constant \(\rho\)?",
    r"The Reynolds number \(\mathrm{Re} = \frac{\rho V D}{\mu}\) compares inertial to viscous forces; flow in a pipe is laminar when \(\mathrm{Re} \lesssim 2300\) and \(\sqrt{\frac{\tau_w}{\rho}}\) is the friction velocity.",
    r"Bernoulli: \(P_1 + \tfrac{1}{2}\rho V_1^2 + \rho g z_1 = P_2 + \tfrac{1}{2}\rho V_2^2 + \rho g z_2\) along a streamline.",
];

pub fn math_test(app: &mut App, ui: &mut Ui, pane: Pane) -> Result<(), crate::data::Need> {
    crate::views::head(app, ui, pane, "Math test", vec![], None);
    for (i, s) in MATH_SAMPLES.iter().enumerate() {
        let width = if i % 2 == 0 { 420.0 } else { 560.0 };
        ui.allocate_ui(vec2(width, 0.0), |ui| {
            ui.set_max_width(width);
            let r = ui.max_rect();
            crate::html::bare(app, ui, &rich_html(s), Flavor::Canvas, Env { weight: 500, ..Env::content(pane) });
            ui.painter().rect_stroke(r.with_max_y(ui.min_rect().max.y), 0.0, egui::Stroke::new(0.5, t().faint), egui::StrokeKind::Outside);
        });
        ui.add_space(14.0);
    }
    Ok(())
}

fn rich_html(text: &str) -> String {
    // text with \( \) math and \smiles{}; html.rs splits those out
    format!("<p>{}</p>", esc(text).replace('\n', "<br>"))
}

fn card_body(app: &mut App, ui: &mut Ui, key: &str, id: &str, c: &Card, streaming: bool, current: bool, acts: &mut Vec<Act>) {
    let tk = t();
    let pane = Pane::Viewer;
    if c.loading {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            w::spinner_soft(ui, 14.0);
            w::text_line(ui, "Writing a new question…", Ts::muted(14.0));
        });
        return;
    }
    if c.editing {
        if !current {
            // an invisible card being edited keeps its size
            w::input(ui, Id::new(("cp-eq-m", id, c.uid)), &mut c.edit_q.clone(), "Question", w::InputOpts { multiline: Some(2), size: 14.0, pad: vec2(8.0, 5.0), ..Default::default() });
            ui.add_space(4.0);
            w::input(ui, Id::new(("cp-ea-m", id, c.uid)), &mut c.edit_a.clone(), "Answer", w::InputOpts { multiline: Some(2), size: 14.0, pad: vec2(8.0, 5.0), ..Default::default() });
            return;
        }
        let Some(card) = app.cp.docs.get_mut(key).and_then(|s| s.cuts.iter_mut().find(|x| x.id == id)).and_then(|x| x.cards.iter_mut().find(|k| k.uid == c.uid)) else { return };
        let o = || w::InputOpts { multiline: Some(2), size: 14.0, pad: vec2(8.0, 5.0), ..Default::default() };
        w::input(ui, Id::new(("cp-eq", id, c.uid)), &mut card.edit_q, "Question", o());
        ui.add_space(4.0);
        w::input(ui, Id::new(("cp-ea", id, c.uid)), &mut card.edit_a, "Answer", o());
        ui.add_space(4.0);
        return;
    }
    crate::html::bare(app, ui, &rich_html(&c.q), Flavor::Canvas, Env { weight: 500, ..Env::content(pane) });
    if c.revealed {
        ui.add_space(6.0);
        match &c.a {
            Some(a) => crate::html::bare(app, ui, &rich_html(a), Flavor::Canvas, Env { color: answer_color(), ..Env::content(pane) }),
            None if streaming => {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    w::spinner_soft(ui, 14.0);
                    w::text_line(ui, "Writing the answer…", Ts::muted(14.0));
                });
            }
            None => {
                w::text_line(ui, "No answer was written. Edit to add one.", Ts::muted(14.0));
            }
        }
    }
    let _ = (acts, tk);
}

fn act(app: &mut App, _pane: Pane, key: &str, id: &str, a: Act) {
    if let Some(st) = app.cp.docs.get_mut(key) {
        st.active = Some(id.to_string());
    }
    let Some(cut) = find_cut(app, key, id) else { return };
    let card = |cut: &mut Cut, u: u64| -> Option<usize> { cut.cards.iter().position(|c| c.uid == u) };
    match a {
        Act::Prev => {
            let i = cut.idx;
            if i == 0 || !go_to(cut, i - 1) {
                return;
            }
        }
        Act::Next => {
            let i = cut.idx;
            if !go_to(cut, i + 1) {
                return;
            }
        }
        Act::Go(i) => {
            if !go_to(cut, i) {
                return;
            }
        }
        Act::Reveal(u) => {
            if let Some(i) = card(cut, u) {
                cut.cards[i].revealed = true;
            }
        }
        Act::Edit(u) => {
            if let Some(i) = card(cut, u) {
                let c = &mut cut.cards[i];
                c.editing = true;
                c.edit_q = c.q.clone();
                c.edit_a = c.a.clone().unwrap_or_default();
            }
        }
        Act::Cancel(u) => {
            if let Some(i) = card(cut, u) {
                cut.cards[i].editing = false;
            }
        }
        Act::Save(u) => {
            if let Some(i) = card(cut, u) {
                let c = &mut cut.cards[i];
                c.q = c.edit_q.trim().to_string();
                c.a = Some(c.edit_a.trim().to_string());
                c.editing = false;
                c.revealed = true;
            }
        }
        Act::Drop => {
            if cut.idx < cut.cards.len() {
                let i = cut.idx;
                cut.cards.remove(i);
            }
        }
        Act::Regen(u) => return regenerate_card(app, key, id, u),
        Act::RegenAll => return generate(app, key, id),
        Act::Remove => return remove_cut(app, key, id),
        Act::Settings => {
            crate::nav::go(app, "#/settings");
            return;
        }
        Act::Allow => {
            cut.allowing = true;
            let svc = app.svc.clone();
            let (k, i) = (key.to_string(), id.to_string());
            app.spawn(async move { svc.setup_allow("claude").is_ok() }, move |app, ok| {
                if let Some(c) = find_cut(app, &k, &i) {
                    c.allowing = false;
                }
                if ok {
                    app.grant("claude");
                }
                retry_waiting(app);
            });
            return;
        }
        Act::Anki(u) => return send_to_anki(app, key, id, u),
    }
    save_cuts(app, key);
}

// ---------- keys (nav.rs asks here first while you're in the viewer) ----------
/// The checkpoint keys act on: the last one pointed at, if it's still on screen, else the one on
/// screen closest to the middle of the viewer.
fn active_cut(app: &App, key: &str) -> Option<String> {
    let st = app.cp.docs.get(key)?;
    let view = app.cp.view;
    let on_screen = |c: &Cut| app.cp.rects.get(&c.id).map(|r| r.max.y > view.min.y && r.min.y < view.max.y && r.height() > 0.0).unwrap_or(false) && c.status == "ready" && !c.cards.is_empty();
    if let Some(a) = &st.active {
        if st.cuts.iter().any(|c| &c.id == a && on_screen(c)) {
            return Some(a.clone());
        }
    }
    let mid = view.center().y;
    st.cuts.iter().filter(|c| on_screen(c)).min_by(|a, b| {
        let d = |c: &Cut| (app.cp.rects[&c.id].center().y - mid).abs();
        d(a).total_cmp(&d(b))
    }).map(|c| c.id.clone())
}

/// Returns true when it handled the key.
pub fn key(app: &mut App, k: &str) -> bool {
    let Some(key_) = current_key(app) else { return false };
    if !app.cp.docs.contains_key(&key_) {
        return false;
    }
    if k == "m" {
        let on = !app.cp.mode;
        set_mode(app, on);
        return true;
    }
    if app.cp.mode {
        let (sent, pending) = { let st = &app.cp.docs[&key_]; (st.sent, st.pending_cut.is_some()) };
        if k == "j" || k == "k" {
            step_sentence(app, &key_, if k == "j" { 1 } else { -1 });
            return true;
        }
        if k == "a" {
            app.cp.adding = !app.cp.adding;
            return true;
        }
        if k == "c" && sent >= 0 {
            checkpoint_here(app, &key_);
            return true;
        }
        if k == "Escape" && (app.cp.adding || pending || sent >= 0) {
            if app.cp.adding {
                app.cp.adding = false;
            } else {
                let st = app.cp.docs.get_mut(&key_).unwrap();
                // the first Esc cancels the checkpoint, the next clears the highlight
                if st.pending_cut.is_some() {
                    st.pending_cut = None;
                } else {
                    st.sent = -1;
                }
            }
            return true;
        }
    }
    let Some(id) = active_cut(app, &key_) else { return false };
    let cut = find_cut(app, &key_, &id).unwrap();
    let Some(card) = cut.cards.get_mut(cut.idx) else { return false };
    if card.editing || card.loading {
        return false;
    }
    if k == " " && !card.revealed {
        card.revealed = true; // once shown, Space scrolls again
    } else if k == "ArrowLeft" || k == "ArrowRight" {
        let i = cut.idx as i64 + if k == "ArrowRight" { 1 } else { -1 };
        if i < 0 || !go_to(cut, i as usize) {
            return false;
        }
    } else {
        return false;
    }
    app.cp.docs.get_mut(&key_).unwrap().active = Some(id);
    save_cuts(app, &key_);
    true
}

/// pdf.rs reports where the viewer's viewport and content are each frame.
pub fn set_view(app: &mut App, view: Rect, origin: f32) {
    app.cp.view = view;
    app.cp.origin = origin;
}

// ---------- Settings ----------
pub fn settings_section(app: &mut App, ui: &mut Ui) {
    let tk = t();
    if app.cp.key_info.is_none() && !app.cp.key_loading {
        app.cp.key_loading = true;
        let svc = app.svc.clone();
        app.spawn(async move { svc.checkpoint_key_info().await }, |app, v| {
            app.cp.key_loading = false;
            app.cp.key_info = Some(v);
        });
    }
    let Some(k) = app.cp.key_info.clone() else { return };
    if app.cp.prompt_text.is_none() {
        app.cp.prompt_text = Some(app.prefs.str("cpPrompt"));
        app.cp.deck_text = Some(app.prefs.str("cpDeck"));
    }
    w::h2(ui, "Checkpoints");
    // .set-intro
    let mut rich = Rich::new();
    let m = Ts::muted(14.0);
    rich.push("In a PDF, press ", m);
    rich.push(" M ", Ts::new(11.0, 400, tk.muted).mono());
    rich.push(" (or ", m);
    rich.push("Checkpoints", m.w(700));
    rich.push(") to add checkpoints: Claude writes questions about what you just read, and you can send the good ones to Anki. From CheckpointReader.", m);
    let g = rich.wrap(ui.available_width().min(640.0)).lay(ui);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), g.size().y), Sense::hover());
    ui.painter().galley(r.min, g, tk.muted);
    ui.add_space(12.0);
    let source = k["source"].as_str().map(String::from);
    let where_ = match source.as_deref() {
        Some("env") => "From the ANTHROPIC_API_KEY environment variable.",
        Some("keychain") => "Saved in your system keychain.",
        Some("file") => "Saved to a private file in the app's folder (no system keychain was available).",
        _ => "Claude can't write questions without one.",
    };
    let note = if app.cp.key_note.is_empty() { where_.to_string() } else { app.cp.key_note.clone() };
    let models: Vec<(String, String)> = k["models"].as_array().into_iter().flatten().map(|m| (crate::fmt::s(&m["id"]), format!("{} ({})", crate::fmt::s(&m["name"]), crate::fmt::s(&m["note"])))).collect();
    let mut changed: Vec<(&str, String)> = Vec::new();
    let mut key_act: Option<&str> = None;
    crate::setup::panel_rows(ui, |ui| {
        crate::setup::perm_row(ui, &[("Anthropic API key", true)], &note, |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if source.is_some() && !app.cp.key_editing {
                if source.as_deref() != Some("env") {
                    if w::button(ui, "Remove").clicked() {
                        key_act = Some("remove");
                    }
                    if w::button(ui, "Replace").clicked() {
                        key_act = Some("edit");
                    }
                }
                w::text_line(ui, k["hint"].as_str().unwrap_or(""), Ts::new(12.0, 400, tk.text).mono());
            } else {
                if source.is_some() && w::button(ui, "Cancel").clicked() {
                    key_act = Some("cancel");
                }
                let busy = app.cp.key_busy;
                if w::button_if(ui, if busy { "Checking…" } else { "Save" }, true, busy).clicked() {
                    key_act = Some("save");
                }
                let resp = w::input(ui, Id::new("cp-key"), &mut app.cp.key_text, "sk-ant-…", w::InputOpts { size: 13.0, pad: vec2(8.0, 5.0), password: true, width: Some(220.0), ..Default::default() });
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    key_act = Some("save");
                }
            }
        });
        w::hline(ui, ui.cursor().min.y, ui.min_rect().min.x, ui.min_rect().max.x, tk.line);
        crate::setup::perm_row(ui, &[("Question model", true)], "", |ui| {
            if let Some(v) = w::select(ui, Id::new("cp-model"), k["model"].as_str().unwrap_or(""), &models, 300.0f32.min(ui.available_width()), 13.0) {
                changed.push(("cpModel", v));
            }
        });
        w::hline(ui, ui.cursor().min.y, ui.min_rect().min.x, ui.min_rect().max.x, tk.line);
        crate::setup::perm_row(ui, &[("Answer model", true)], "A second model can write the answers.", |ui| {
            let mut opts = vec![(String::new(), "Same as the question model".to_string())];
            opts.extend(models.iter().cloned());
            if let Some(v) = w::select(ui, Id::new("cp-answer-model"), k["answer_model"].as_str().unwrap_or(""), &opts, 300.0f32.min(ui.available_width()), 13.0) {
                changed.push(("cpAnswerModel", v));
            }
        });
        w::hline(ui, ui.cursor().min.y, ui.min_rect().min.x, ui.min_rect().max.x, tk.line);
        // .perm-row.col: the text box under its title
        ui.add_space(10.0);
        w::text_line(ui, "Custom instructions", Ts::new(14.0, 700, tk.text));
        w::text_line(ui, "Added to every request, e.g. “Write the questions in Spanish”.", Ts::faint(12.0));
        ui.add_space(6.0);
        let prompt = app.cp.prompt_text.as_mut().unwrap();
        if w::input(ui, Id::new("cp-prompt"), prompt, "", w::InputOpts { size: 13.0, pad: vec2(8.0, 5.0), multiline: Some(3), ..Default::default() }).changed() {
            changed.push(("cpPrompt", prompt.trim().to_string()));
        }
        ui.add_space(10.0);
        w::hline(ui, ui.cursor().min.y, ui.min_rect().min.x, ui.min_rect().max.x, tk.line);
        crate::setup::perm_row(ui, &[("Anki deck", true)], "For courses without a deck linked on the Anki page.", |ui| {
            let deck = app.cp.deck_text.as_mut().unwrap();
            if w::input(ui, Id::new("cp-deck"), deck, CP_DECK, w::InputOpts { size: 13.0, pad: vec2(8.0, 5.0), width: Some(220.0), ..Default::default() }).changed() {
                changed.push(("cpDeck", deck.trim().to_string()));
            }
        });
    });
    for (k_, v) in changed {
        app.set_pref(k_, json!(v));
        if let Some(info) = app.cp.key_info.as_mut() {
            match k_ {
                "cpModel" => info["model"] = json!(v),
                "cpAnswerModel" => info["answer_model"] = json!(v),
                _ => {}
            }
        }
    }
    match key_act {
        Some("edit") => {
            app.cp.key_editing = true;
            app.cp.key_note.clear();
            if let Some(c) = app.ctx() {
                c.memory_mut(|m| m.request_focus(Id::new("cp-key")));
            }
        }
        Some("cancel") => {
            app.cp.key_editing = false;
            app.cp.key_note.clear();
        }
        Some("remove") => {
            let svc = app.svc.clone();
            app.spawn(async move { svc.checkpoint_key_delete().await }, |app, v| {
                app.cp.key_info = Some(v);
                app.cp.key_note.clear();
            });
        }
        Some("save") => {
            let text = app.cp.key_text.trim().to_string();
            if text.is_empty() || app.cp.key_busy {
                return;
            }
            app.cp.key_busy = true;
            let svc = app.svc.clone();
            app.spawn(
                async move {
                    let r = svc.checkpoint_key_save(&text).await;
                    let info = svc.checkpoint_key_info().await;
                    (r, info)
                },
                |app, (r, info)| {
                    app.cp.key_busy = false;
                    match r {
                        Ok(_) => {
                            app.cp.key_editing = false;
                            app.cp.key_text.clear();
                            app.cp.key_note.clear();
                            app.cp.key_info = Some(info);
                            retry_waiting(app);
                        }
                        Err(e) => {
                            app.cp.key_note = if e.message().is_empty() { "Couldn't save the key.".into() } else { e.message() };
                        }
                    }
                },
            );
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::TItem;

    fn it(s: &str, x: f32, y: f32, eol: bool) -> TItem {
        TItem { s: s.into(), eol, x, y, w: s.len() as f32 * 0.01, h: 0.012 }
    }

    #[test]
    fn passages_stop_at_their_section() {
        // no sections: back to the last checkpoint, at most two pages
        assert_eq!(passage_bounds(0.0, None, 10.5), (8.5, 10.5));
        assert_eq!(passage_bounds(9.0, None, 10.5), (9.0, 10.5));
        // in section 2-2 (starting partway down page 63): not into 2-1 or chapter 1
        assert_eq!(passage_bounds(40.0, Some(62.3), 64.2), (62.3, 64.2));
        // an earlier checkpoint in the same section still comes first
        assert_eq!(passage_bounds(63.0, Some(62.3), 64.2), (63.0, 64.2));
        // a long section: the last eight pages of it
        assert_eq!(passage_bounds(0.0, Some(50.0), 64.0), (56.0, 64.0));
    }

    #[test]
    fn sentences_and_positions() {
        let p = Arc::new(PageText { chars: vec![], items: vec![it("The cell is small. It divides", 0.1, 0.2, true), it("often. 12", 0.1, 0.215, true)], w: 612.0, h: 792.0 });
        let s = build_sentences(&[p]);
        assert_eq!(s.len(), 3); // "12" counts, as in the original
        assert_eq!(s[1].rects.len(), 2); // across a line break
        assert_eq!(fmt_pos(1.25), "p.2 25%");
        let segs = segments(3, 0.5, 2.25);
        assert_eq!(segs.len(), 3);
        assert!((segs[2].y1 - 0.25).abs() < 1e-6);
        assert_eq!(to_base36(35), "z");
        assert_eq!(esc("<a & 'b'>"), "&lt;a &amp; &#39;b&#39;&gt;");
    }
}

// ---------- touch mode: on-screen buttons for reading ----------
/// A round reading button's look (CheckpointReader's .read-ctl): 54px, a shadow, the icon.
fn round_button(ui: &Ui, layer: &egui::Painter, r: Rect, id: Id, icon: &str, accent: bool, filled: bool, enabled: bool) -> bool {
    let tk = t();
    let resp = ui.interact(r, id, if enabled { Sense::click() } else { Sense::hover() });
    let pressed = resp.is_pointer_button_down_on();
    let r = if pressed { r.shrink(r.width() * 0.04) } else { r };
    let alpha: f32 = if enabled { 1.0 } else { 0.4 };
    layer.add(egui::Shadow { offset: [0, 2], blur: 10, spread: 0, color: Color32::from_black_alpha((46.0 * alpha) as u8) }.as_shape(r, cr(r.width() / 2.0)));
    let (bg, border, fg) = if filled {
        (tk.accent, tk.accent, Color32::WHITE)
    } else {
        let hot = resp.hovered() && enabled;
        (tk.panel_solid, if hot { tk.accent } else { tk.line }, if accent || hot { tk.accent } else { tk.text })
    };
    layer.circle_filled(r.center(), r.width() / 2.0, theme::alpha(bg, alpha.max(0.85)));
    layer.circle_stroke(r.center(), r.width() / 2.0 - 0.5, Stroke::new(1.0, theme::alpha(border, alpha)));
    let c = r.center();
    let st = Stroke::new(2.0, theme::alpha(fg, alpha));
    let line = |pts: Vec<Pos2>| {
        layer.add(egui::Shape::line(pts, st));
    };
    match icon {
        "up" => line(vec![c + vec2(-7.0, 3.5), c + vec2(0.0, -3.5), c + vec2(7.0, 3.5)]),
        "down" => line(vec![c + vec2(-7.0, -3.5), c + vec2(0.0, 3.5), c + vec2(7.0, -3.5)]),
        "plus" => {
            line(vec![c + vec2(-8.0, 0.0), c + vec2(8.0, 0.0)]);
            line(vec![c + vec2(0.0, -8.0), c + vec2(0.0, 8.0)]);
        }
        "check" => line(vec![c + vec2(-8.0, 0.5), c + vec2(-2.5, 6.0), c + vec2(8.5, -6.0)]),
        _ => {
            line(vec![c + vec2(-6.5, -6.5), c + vec2(6.5, 6.5)]);
            line(vec![c + vec2(6.5, -6.5), c + vec2(-6.5, 6.5)]);
        }
    }
    let tip = match icon {
        "up" => "Previous sentence",
        "down" => "Next sentence",
        "plus" => "A checkpoint after this sentence",
        "check" => "Add the checkpoint here",
        _ => "Cancel",
    };
    enabled && resp.on_hover_text(tip).clicked()
}

/// In touch mode, while adding checkpoints to the PDF in the viewer: ↑ / ↓ to step through the
/// sentences, + to propose a checkpoint after the highlighted one (✓ to add it, × to cancel), in a
/// bottom corner of the viewer.
pub fn read_controls(app: &mut App, root: &mut Ui, viewer: Rect) {
    if !app.touch_mode() || !app.cp.mode || app.palette.open || app.keys_open {
        return;
    }
    let Some(key) = current_key(app) else { return };
    let Some(st) = app.cp.docs.get(&key) else { return };
    let (sent, pending) = (st.sent, st.pending_cut.is_some());
    let left = app.prefs.str("readControls") == "left";
    let size = 54.0;
    let x = if left { viewer.min.x + 16.0 } else { viewer.max.x - 16.0 - size };
    let layer = root.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, Id::new("read-controls")));
    // bottom up: ↓, ↑, then the checkpoint button (8px further), then ×
    let mut y = viewer.max.y - 20.0 - size;
    let mut act = None;
    if round_button(root, &layer, Rect::from_min_size(pos2(x, y), vec2(size, size)), Id::new("rc-next"), "down", false, false, true) {
        act = Some("next");
    }
    y -= size + 10.0;
    if round_button(root, &layer, Rect::from_min_size(pos2(x, y), vec2(size, size)), Id::new("rc-prev"), "up", false, false, true) {
        act = Some("prev");
    }
    y -= size + 18.0;
    if round_button(root, &layer, Rect::from_min_size(pos2(x, y), vec2(size, size)), Id::new("rc-cut"), if pending { "check" } else { "plus" }, true, pending, sent >= 0) {
        act = Some("cut");
    }
    if pending {
        y -= size + 10.0;
        if round_button(root, &layer, Rect::from_min_size(pos2(x, y), vec2(size, size)), Id::new("rc-cancel"), "x", false, false, true) {
            act = Some("cancel");
        }
    }
    match act {
        Some("next") => step_sentence(app, &key, 1),
        Some("prev") => step_sentence(app, &key, -1),
        Some("cut") => checkpoint_here(app, &key),
        Some("cancel") => {
            if let Some(st) = app.cp.docs.get_mut(&key) {
                st.pending_cut = None;
            }
        }
        _ => {}
    }
}
