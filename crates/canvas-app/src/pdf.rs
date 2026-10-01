//! PDFs in the viewer, drawn with PDFium on a worker thread. Every page spans the pane's width and
//! is drawn when it comes near the screen, sharp at the screen's pixel density, with selectable
//! text and working links. Pages far off screen let their drawings go, so long readings stay light,
//! and all of them redraw when the pane is resized.
//!
//! A page is a column of slices: one, or with checkpoints (checkpoints.rs), one more per checkpoint
//! on it, with the checkpoint between. Positions in a page are fractions of its height, 0 to 1.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Instant;

use egui::{Color32, ColorImage, CursorIcon, Id, Rect, Sense, TextureHandle, TextureOptions, Ui, pos2, vec2};
use md5::{Digest, Md5};
use pdfium_render::prelude::*;

use crate::app::{App, Msg, Pane};
use crate::theme::{self, t};
use crate::widgets::{Ts, cr};

/// A character with its box, as fractions of the page (y down).
#[derive(Clone, Debug)]
pub struct Ch {
    pub c: char,
    pub x0: f32,
    pub x1: f32,
    pub top: f32,
    pub bottom: f32,
    pub base: f32,
    pub h: f32,
}

/// A run of text on one line (like pdf.js's text items): y is the baseline.
#[derive(Clone, Debug)]
pub struct TItem {
    pub s: String,
    pub eol: bool,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

pub struct PageText {
    pub chars: Vec<Ch>,
    pub items: Vec<TItem>,
    pub w: f32,
    pub h: f32,
}

#[derive(Clone, Debug)]
pub struct Link {
    pub rect: [f32; 4], // x0, y0, x1, y1 fractions
    pub url: Option<String>,
    pub dest: Option<(usize, f32)>,
}

#[derive(Clone)]
pub struct Info {
    pub pages: Vec<(f32, f32)>,
    pub fingerprint: String,
    pub outline: Arc<Vec<OutlineItem>>,
}

enum Job {
    Open { fid: String, path: PathBuf },
    Render { fid: String, page: usize, width: u32, plain: bool, reply: Box<dyn FnOnce(Option<ColorImage>) + Send> },
    Text { fid: String, page: usize },
    Links { fid: String, page: usize },
    Close { fid: String },
    /// every page's text of a PDF on disk (for the search index), a few pages per turn
    Extract { path: PathBuf, from: usize, acc: Vec<String>, reply: Box<dyn FnOnce(Option<Vec<String>>) + Send> },
}

/// The PDF's fingerprint as pdf.js computes it: the file's /ID, else an MD5 of its first 1KB.
pub fn fingerprint(bytes: &[u8]) -> String {
    let tail = &bytes[bytes.len().saturating_sub(4096)..];
    let text = String::from_utf8_lossy(tail);
    let re = regex::Regex::new(r"/ID\s*\[\s*<([0-9A-Fa-f]+)>").unwrap();
    if let Some(m) = re.captures_iter(&text).last() {
        let id = m[1].to_lowercase();
        if !id.chars().all(|c| c == '0') && !id.is_empty() {
            return id;
        }
    }
    hex::encode(Md5::digest(&bytes[..bytes.len().min(1024)]))
}

fn library() -> Result<Pdfium, String> {
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf()));
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = &exe_dir {
        dirs.push(d.clone());
        dirs.push(d.join("lib"));
        dirs.push(d.join("../lib"));
    }
    let arch = if cfg!(windows) { "win-x64" } else { "linux-x64" };
    dirs.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vendor/pdfium").join(arch).join(if cfg!(windows) { "bin" } else { "lib" }));
    for d in dirs {
        let p = Pdfium::pdfium_platform_library_name_at_path(&d);
        if p.exists() {
            if let Ok(b) = Pdfium::bind_to_library(&p) {
                return Ok(Pdfium::new(b));
            }
        }
    }
    Pdfium::bind_to_system_library().map(Pdfium::new).map_err(|e| {
        log::warn!("PDFium: {e}");
        "PDFium isn't installed (scripts/fetch-pdfium.sh gets it)".to_string()
    })
}

fn page_text(page: &PdfPage) -> PageText {
    let (pw, ph) = (page.width().value, page.height().value);
    let mut chars = Vec::new();
    if let Ok(text) = page.text() {
        for c in text.chars().iter() {
            let Some(ch) = c.unicode_char() else { continue };
            let b = c.loose_bounds().unwrap_or(PdfRect::ZERO);
            let base = c.origin_y().map(|y| y.value).unwrap_or(b.bottom().value);
            let size = c.scaled_font_size().value;
            chars.push(Ch {
                c: ch,
                x0: b.left().value / pw,
                x1: b.right().value / pw,
                top: 1.0 - b.top().value / ph,
                bottom: 1.0 - b.bottom().value / ph,
                base: 1.0 - base / ph,
                h: size / ph,
            });
        }
    }
    // runs on a line (like pdf.js's text items); a newline ends one (eol)
    let mut items: Vec<TItem> = Vec::new();
    let mut cur: Option<TItem> = None;
    let aspect = ph / pw.max(1.0);
    for c in &chars {
        if c.c == '\r' || c.c == '\n' {
            if let Some(mut it) = cur.take() {
                it.eol = true;
                items.push(it);
            } else if let Some(last) = items.last_mut() {
                last.eol = true;
            }
            continue;
        }
        let joins = match &cur {
            Some(it) => {
                let gap = c.x0 - (it.x + it.w);
                (it.y - c.base).abs() < 0.5 * it.h.max(c.h) && gap > -0.01 && gap < 1.5 * c.h.max(it.h) * aspect
            }
            None => false,
        };
        if joins {
            let it = cur.as_mut().unwrap();
            it.s.push(c.c);
            it.w = it.w.max(c.x1 - it.x);
            it.h = it.h.max(c.h);
        } else {
            if let Some(it) = cur.take() {
                items.push(it);
            }
            if !c.c.is_whitespace() {
                cur = Some(TItem { s: c.c.to_string(), eol: false, x: c.x0, y: c.base, w: (c.x1 - c.x0).max(0.0), h: c.h });
            }
        }
    }
    if let Some(it) = cur {
        items.push(it);
    }
    for it in items.iter_mut() {
        let t = it.s.trim_end().len();
        it.s.truncate(t);
    }
    PageText { chars, items, w: pw, h: ph }
}

/// Where a destination points: (page index, fraction of the page's height from the top).
fn dest_pos(doc: &PdfDocument, d: &PdfDestination) -> Option<(usize, f32)> {
    let idx = d.page_index().ok()? as usize;
    let h = doc.pages().get(idx as PdfPageIndex).map(|p| p.height().value).ok()?;
    let top = match d.view_settings().ok()? {
        PdfDestinationViewSettings::SpecificCoordinatesAndZoom(_, y, _) => y.map(|y| y.value),
        PdfDestinationViewSettings::FitPageHorizontallyToWindow(y) | PdfDestinationViewSettings::FitBoundsHorizontallyToWindow(y) => y.map(|y| y.value),
        PdfDestinationViewSettings::FitPageToRectangle(r) => Some(r.top().value),
        _ => None,
    };
    Some((idx, top.map(|y| (1.0 - y / h).clamp(0.0, 1.0)).unwrap_or(0.0)))
}

/// An entry in a PDF's outline (its bookmarks: chapters, sections, the glossary…), in document
/// order; an entry's children are the entries after it that are deeper.
#[derive(Clone, Debug)]
pub struct OutlineItem {
    pub title: String,
    pub depth: usize,
    pub page: usize,
    pub y: f32,
}

/// The whole outline, flattened in reading order (entries without a destination are kept, at the
/// place of their first child, so the tree stays whole).
fn outline(doc: &PdfDocument) -> Vec<OutlineItem> {
    fn walk(doc: &PdfDocument, first: Option<PdfBookmark>, depth: usize, out: &mut Vec<OutlineItem>, budget: &mut usize) {
        let mut cur = first;
        while let Some(b) = cur {
            if *budget == 0 || depth > 12 {
                return;
            }
            *budget -= 1;
            let pos = b.destination().and_then(|d| dest_pos(doc, &d)).or_else(|| b.action().and_then(|a| a.as_local_destination_action().and_then(|d| d.destination().ok().and_then(|d| dest_pos(doc, &d)))));
            let title = b.title().unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ");
            let at = out.len();
            out.push(OutlineItem { title, depth, page: pos.map(|p| p.0).unwrap_or(usize::MAX), y: pos.map(|p| p.1).unwrap_or(0.0) });
            walk(doc, b.first_child(), depth + 1, out, budget);
            // no destination of its own: where its first child is
            if out[at].page == usize::MAX {
                let (page, y) = out.get(at + 1).filter(|c| c.depth > depth && c.page != usize::MAX).map(|c| (c.page, c.y)).unwrap_or((0, 0.0));
                out[at].page = page;
                out[at].y = y;
            }
            cur = b.next_sibling();
        }
    }
    let mut out = Vec::new();
    let mut budget = 5000;
    walk(doc, doc.bookmarks().root(), 0, &mut out, &mut budget);
    out
}

fn page_links(doc: &PdfDocument, page: &PdfPage) -> Vec<Link> {
    let (pw, ph) = (page.width().value, page.height().value);
    let mut out = Vec::new();
    for l in page.links().iter() {
        let Ok(r) = l.rect() else { continue };
        let rect = [r.left().value / pw, 1.0 - r.top().value / ph, r.right().value / pw, 1.0 - r.bottom().value / ph];
        let url = l.action().and_then(|a| a.as_uri_action().and_then(|u| u.uri().ok())).filter(|u| u.starts_with("http://") || u.starts_with("https://"));
        let dest_of = |d: PdfDestination| dest_pos(doc, &d);
        let dest = l.destination().and_then(dest_of).or_else(|| l.action().and_then(|a| a.as_local_destination_action().and_then(|d| d.destination().ok().and_then(dest_of))));
        if url.is_some() || dest.is_some() {
            out.push(Link { rect, url, dest });
        }
    }
    out
}

/// Pages of text extracted per turn of the worker.
const EXTRACT_CHUNK: usize = 25;

fn worker(rx: Receiver<Job>, tx: Sender<Msg>, me: Sender<Job>) {
    let pdfium = match library() {
        Ok(p) => Box::leak(Box::new(p)),
        Err(e) => {
            for job in rx {
                match job {
                    Job::Open { fid, .. } => {
                        let e = e.clone();
                        let _ = tx.send(Msg::Apply(Box::new(move |app: &mut App| app.pdf.opened(&fid, Err(e)))));
                        crate::app::wake();
                    }
                    Job::Extract { reply, .. } => reply(None),
                    Job::Render { reply, .. } => reply(None),
                    _ => {}
                }
            }
            return;
        }
    };
    let mut docs: HashMap<String, PdfDocument<'static>> = HashMap::new();
    let mut extracting: HashMap<PathBuf, PdfDocument<'static>> = HashMap::new();
    let send = |f: Box<dyn FnOnce(&mut App) + Send>| {
        let _ = tx.send(Msg::Apply(f));
        crate::app::wake();
    };
    for job in rx {
        match job {
            Job::Open { fid, path } => {
                let res = std::fs::read(&path).map_err(|e| e.to_string()).and_then(|bytes| {
                    let fp = fingerprint(&bytes);
                    let doc = pdfium.load_pdf_from_byte_vec(bytes, None).map_err(|e| format!("{e}"))?;
                    let pages: Vec<(f32, f32)> = doc.pages().iter().map(|p| (p.width().value, p.height().value)).collect();
                    let outline = Arc::new(outline(&doc));
                    docs.insert(fid.clone(), doc);
                    Ok(Info { pages, fingerprint: fp, outline })
                });
                send(Box::new(move |app: &mut App| app.pdf.opened(&fid, res)));
            }
            Job::Render { fid, page, width, plain, reply } => {
                let img = docs.get(&fid).and_then(|d| d.pages().get(page as PdfPageIndex).ok()).and_then(|p| {
                    let cfg = PdfRenderConfig::new().set_target_width(width.max(1) as Pixels).render_form_data(true).set_clear_color(PdfColor::WHITE);
                    let cfg = if plain { cfg } else { cfg.use_lcd_text_rendering(false) };
                    let bm = p.render_with_config(&cfg).ok()?;
                    let (w, h) = (bm.width() as usize, bm.height() as usize);
                    Some(ColorImage::from_rgba_unmultiplied([w, h], &bm.as_rgba_bytes()))
                });
                reply(img);
            }
            Job::Text { fid, page } => {
                let text = docs.get(&fid).and_then(|d| d.pages().get(page as PdfPageIndex).ok()).map(|p| Arc::new(page_text(&p)));
                send(Box::new(move |app: &mut App| {
                    if let Some(d) = app.pdf.docs.get_mut(&fid) {
                        d.text.insert(page, text.unwrap_or_else(|| Arc::new(PageText { chars: vec![], items: vec![], w: 1.0, h: 1.0 })));
                    }
                }));
            }
            Job::Links { fid, page } => {
                let links = docs.get(&fid).and_then(|d| d.pages().get(page as PdfPageIndex).ok().map(|p| page_links(d, &p))).unwrap_or_default();
                send(Box::new(move |app: &mut App| {
                    if let Some(d) = app.pdf.docs.get_mut(&fid) {
                        d.links.insert(page, links);
                    }
                }));
            }
            Job::Close { fid } => {
                docs.remove(&fid);
            }
            Job::Extract { path, from, mut acc, reply } => {
                if !extracting.contains_key(&path) {
                    match pdfium.load_pdf_from_file(&path, None) {
                        Ok(d) => {
                            extracting.insert(path.clone(), d);
                        }
                        Err(_) => {
                            reply(None);
                            continue;
                        }
                    }
                }
                let doc = &extracting[&path];
                let n = doc.pages().len() as usize;
                let to = (from + EXTRACT_CHUNK).min(n);
                for i in from..to {
                    acc.push(doc.pages().get(i as PdfPageIndex).ok().and_then(|p| p.text().ok().map(|t| t.all())).unwrap_or_default());
                }
                if to >= n {
                    extracting.remove(&path);
                    reply(Some(acc));
                } else {
                    // to the back of the queue: pages being read get drawn in between
                    let _ = me.send(Job::Extract { path, from: to, acc, reply });
                }
            }
        }
    }
}

pub struct PageTex {
    pub tex: TextureHandle,
    pub width: u32,
}

pub struct Doc {
    pub state: Result<Info, String>,
    pub loading: bool,
    pub tex: HashMap<usize, PageTex>,
    pub rendering: HashMap<usize, u32>,
    pub text: HashMap<usize, Arc<PageText>>,
    pub text_asked: std::collections::HashSet<usize>,
    pub links: HashMap<usize, Vec<Link>>,
    pub links_asked: std::collections::HashSet<usize>,
    /// Where each page was drawn last frame (content y in the viewer's scroll, height).
    pub page_y: HashMap<usize, (f32, f32)>,
    pub slices: HashMap<usize, Vec<(f32, f32, Rect)>>,
    pub trace: Option<crate::debug::Trace>,
    pub first_drawn: bool,
    pub opened_at: Instant,
}

pub struct Sel {
    pub fid: String,
    pub anchor: (usize, usize),
    pub focus: (usize, usize),
    pub dragging: bool,
}

pub struct Pdfs {
    tx: Option<Sender<Job>>,
    pub docs: HashMap<String, Doc>,
    order: Vec<String>,
    pub sel: Option<Sel>,
    pub scroll_to: Option<(String, usize, f32, bool)>,
    /// the Contents panel is open (for PDFs that have an outline)
    pub contents: bool,
    /// per PDF: the outline entries opened in the Contents panel
    pub expanded: HashMap<String, std::collections::HashSet<usize>>,
    /// per PDF: a place to go back to once it's drawn (a textbook's last position)
    pub restore: HashMap<String, (usize, f32)>,
    /// fullscreen: pages no wider than this, centered
    pub max_w: Option<f32>,
}

const KEEP: usize = 4;

impl Pdfs {
    pub fn new() -> Pdfs {
        Pdfs { tx: None, docs: HashMap::new(), order: Vec::new(), sel: None, scroll_to: None, contents: false, expanded: HashMap::new(), restore: HashMap::new(), max_w: None }
    }

    fn send(&mut self, app_tx: &Sender<Msg>, job: Job) {
        if self.tx.is_none() {
            let (tx, rx) = std::sync::mpsc::channel();
            let (atx, me) = (app_tx.clone(), tx.clone());
            std::thread::Builder::new().name("pdfium".into()).spawn(move || worker(rx, atx, me)).ok();
            self.tx = Some(tx);
        }
        if let Some(t) = &self.tx {
            let _ = t.send(job);
        }
    }

    fn opened(&mut self, fid: &str, res: Result<Info, String>) {
        if let Some(d) = self.docs.get_mut(fid) {
            d.state = res;
            d.loading = false;
            crate::debug::step(&mut d.trace, "first page");
        }
    }

    pub fn info(&self, fid: &str) -> Option<&Info> {
        self.docs.get(fid).and_then(|d| d.state.as_ref().ok())
    }

    pub fn text(&mut self, app_tx: &Sender<Msg>, fid: &str, page: usize) -> Option<Arc<PageText>> {
        let d = self.docs.get_mut(fid)?;
        if let Some(t) = d.text.get(&page) {
            return Some(t.clone());
        }
        if d.text_asked.insert(page) {
            let job = Job::Text { fid: fid.to_string(), page };
            self.send(app_tx, job);
        }
        None
    }

    /// Extract every page's text from a PDF file (in the worker, between drawing jobs).
    pub fn extract(&mut self, app_tx: &Sender<Msg>, path: PathBuf, reply: Box<dyn FnOnce(Option<Vec<String>>) + Send>) {
        self.send(app_tx, Job::Extract { path, from: 0, acc: Vec::new(), reply });
    }

    /// Render a page at a width in pixels (white background), for checkpoints' pictures.
    pub fn render_plain(&mut self, app_tx: &Sender<Msg>, fid: &str, page: usize, width: u32, reply: Box<dyn FnOnce(Option<ColorImage>) + Send>) {
        self.send(app_tx, Job::Render { fid: fid.to_string(), page, width, plain: true, reply });
    }
}

/// Open (or re-use) a PDF: download the file, then parse it on the worker.
fn ensure(app: &mut App, fid: &str) {
    if let Some(i) = app.pdf.order.iter().position(|f| f == fid) {
        app.pdf.order.remove(i);
        app.pdf.order.push(fid.to_string());
        return;
    }
    app.pdf.order.push(fid.to_string());
    while app.pdf.order.len() > KEEP {
        let old = app.pdf.order.remove(0);
        app.pdf.docs.remove(&old);
        let tx = app.tx.clone();
        app.pdf.send(&tx, Job::Close { fid: old });
    }
    let trace = app.viewer.trace.as_ref().and_then(|_| crate::debug::sub(&app.viewer.trace, "pdf", fid, "load"));
    app.pdf.docs.insert(
        fid.to_string(),
        Doc {
            state: Err(String::new()),
            loading: true,
            tex: HashMap::new(),
            rendering: HashMap::new(),
            text: HashMap::new(),
            text_asked: Default::default(),
            links: HashMap::new(),
            links_asked: Default::default(),
            page_y: HashMap::new(),
            slices: HashMap::new(),
            trace,
            first_drawn: false,
            opened_at: Instant::now(),
        },
    );
    let (svc, f) = (app.svc.clone(), fid.to_string());
    app.spawn(async move {
        match f.strip_prefix("tb-") {
            Some(id) => svc.textbook(id).map(|b| PathBuf::from(canvas_mcp::util::s(&b["path"]))).ok_or_else(|| canvas_mcp::services::ApiErr::new(404, "textbook", "This textbook isn't in your library any more")),
            None => svc.file(&f).await.map(|(p, _)| p),
        }
    }, {
        let f = fid.to_string();
        move |app, r| match r {
            Ok(path) => {
                let tx = app.tx.clone();
                app.pdf.send(&tx, Job::Open { fid: f, path });
            }
            Err(e) => app.pdf.opened(&f, Err(e.message())),
        }
    });
}

fn request_render(app: &mut App, ctx: &egui::Context, fid: &str, page: usize, width: u32) {
    let Some(d) = app.pdf.docs.get_mut(fid) else { return };
    if d.rendering.get(&page) == Some(&width) || d.tex.get(&page).map(|t| t.width == width).unwrap_or(false) {
        return;
    }
    d.rendering.insert(page, width);
    let (tx, ctx2, f) = (app.tx.clone(), ctx.clone(), fid.to_string());
    let reply: Box<dyn FnOnce(Option<ColorImage>) + Send> = Box::new(move |img| {
        let tex = img.map(|i| ctx2.load_texture(format!("pdf-{f}-{page}"), i, TextureOptions::LINEAR));
        let _ = tx.send(Msg::Apply(Box::new(move |app: &mut App| {
            let mut done = None;
            if let Some(d) = app.pdf.docs.get_mut(&f) {
                if d.rendering.get(&page) == Some(&width) {
                    d.rendering.remove(&page);
                    if let Some(t) = tex {
                        d.tex.insert(page, PageTex { tex: t, width });
                    }
                }
                if !d.first_drawn {
                    d.first_drawn = true;
                    done = d.trace.take();
                }
            }
            if let Some(tr) = done {
                crate::debug::end(app, tr, &format!("page {}", page + 1));
            }
        })));
        crate::app::wake();
    });
    let tx = app.tx.clone();
    app.pdf.send(&tx, Job::Render { fid: fid.to_string(), page, width, plain: false, reply });
}

/// The selected text, in reading order.
fn selected_text(app: &App) -> Option<String> {
    let s = app.pdf.sel.as_ref()?;
    let d = app.pdf.docs.get(&s.fid)?;
    let (a, b) = if s.anchor <= s.focus { (s.anchor, s.focus) } else { (s.focus, s.anchor) };
    let mut out = String::new();
    for p in a.0..=b.0 {
        let Some(t) = d.text.get(&p) else { continue };
        let from = if p == a.0 { a.1 } else { 0 };
        let to = if p == b.0 { b.1 } else { t.chars.len() };
        for c in t.chars.iter().take(to.min(t.chars.len())).skip(from) {
            out.push(if c.c == '\r' { '\n' } else { c.c });
        }
        if p != b.0 {
            out.push('\n');
        }
    }
    Some(out.replace("\n\n", "\n"))
}

/// The character nearest a point in a page (fractions).
fn char_at(t: &PageText, fx: f32, fy: f32) -> usize {
    let mut best = (f32::INFINITY, 0usize);
    for (i, c) in t.chars.iter().enumerate() {
        if c.c == '\r' || c.c == '\n' {
            continue;
        }
        let dy = if fy < c.top { c.top - fy } else if fy > c.bottom { fy - c.bottom } else { 0.0 };
        let dx = if fx < c.x0 { c.x0 - fx } else if fx > c.x1 { fx - c.x1 } else { 0.0 };
        let d = dy * 4.0 + dx;
        if d < best.0 {
            best = (d, i + if fx > (c.x0 + c.x1) / 2.0 { 1 } else { 0 });
        }
    }
    best.1
}

/// The PDF, in the viewer: pages as slices, checkpoints between them.
pub fn view(app: &mut App, ui: &mut Ui, pane: Pane, fid: &str, name: &str, cid: Option<&str>) {
    let _tk = t();
    ensure(app, fid);
    let ctx = ui.ctx().clone();
    let (loading, err) = match app.pdf.docs.get(fid) {
        Some(d) if d.loading => (true, None),
        Some(d) => (false, d.state.as_ref().err().cloned()),
        None => (true, None),
    };
    if loading {
        ui.add_space(24.0);
        crate::widgets::text_line(ui, "   Loading the PDF…", Ts::faint(14.0));
        return;
    }
    if let Some(e) = err {
        ui.add_space(24.0);
        crate::widgets::text_block(ui, &format!("Couldn't show this PDF: {e}."), Ts::faint(14.0));
        return;
    }
    let info = app.pdf.info(fid).cloned().unwrap();
    let key = format!("pdfcp:{}", info.fingerprint);
    crate::checkpoints::attach(app, fid, &key, name, cid, &info);
    // .pdf-view: margin 0 -16px, pages 8px apart
    // the pages span the viewer's visible width, whatever the bar above them did to the layout
    let vis = ui.clip_rect();
    // in fullscreen, pages no wider than reading width, centered
    let width = app.pdf.max_w.map(|m| vis.width().min(m)).unwrap_or(vis.width());
    let x0 = vis.center().x - width / 2.0;
    let avail = (width - 32.0).max(1.0);
    let ppp = ctx.pixels_per_point();
    let width_px = (width * ppp.min(3.0)).round() as u32;
    let clip = ui.clip_rect();
    let view_h = clip.height();
    let near = Rect::from_min_max(pos2(clip.min.x, clip.min.y - view_h), pos2(clip.max.x, clip.max.y + view_h));
    let far = Rect::from_min_max(pos2(clip.min.x, clip.min.y - 4.0 * view_h), pos2(clip.max.x, clip.max.y + 4.0 * view_h));
    let adding = app.cp.adding;
    // content coordinates: y from the top of the viewer's scrolling content
    let origin = clip.min.y - app.viewer.scroll;
    crate::checkpoints::set_view(app, clip, origin);
    let copy = ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)));
    for (pi, &(pw, ph)) in info.pages.iter().enumerate() {
        let page_h = width * ph / pw;
        let top = ui.cursor().min.y;
        let cuts = crate::checkpoints::cuts_on(app, &key, pi);
        let mut ys = vec![0.0f32];
        ys.extend(cuts.iter().map(|c| c.1));
        ys.push(1.0);
        let mut slice_rects = Vec::new();
        let page_rect_guess = Rect::from_min_size(pos2(x0, top), vec2(width, page_h));
        let visible_near = page_rect_guess.intersects(near);
        let visible_far = page_rect_guess.intersects(far);
        if visible_near {
            request_render(app, &ctx, fid, pi, width_px);
            if let Some(d) = app.pdf.docs.get_mut(fid) {
                if d.links_asked.insert(pi) {
                    let tx = app.tx.clone();
                    app.pdf.send(&tx, Job::Links { fid: fid.to_string(), page: pi });
                }
            }
            let tx = app.tx.clone();
            let _ = app.pdf.text(&tx, fid, pi);
        } else if !visible_far {
            if let Some(d) = app.pdf.docs.get_mut(fid) {
                d.tex.remove(&pi);
                d.rendering.remove(&pi);
            }
        }
        for si in 0..ys.len() - 1 {
            let (y0, y1) = (ys[si], ys[si + 1]);
            let h = (page_h * (y1 - y0)).max(1.0);
            let (row, _) = ui.allocate_exact_size(vec2(avail, h), Sense::hover());
            let r = Rect::from_min_size(pos2(x0, row.min.y), vec2(width, h));
            slice_rects.push((y0, y1, r));
            let p = ui.painter();
            // .pdf-page: shadow 0 1px 4px; the slice: white
            let round = egui::CornerRadius { nw: if si == 0 { 2 } else { 0 }, ne: if si == 0 { 2 } else { 0 }, sw: if si == ys.len() - 2 { 2 } else { 0 }, se: if si == ys.len() - 2 { 2 } else { 0 } };
            if r.intersects(clip) {
                let shadow = egui::Shadow { offset: [0, 1], blur: 4, spread: 0, color: Color32::from_black_alpha(71) };
                p.add(shadow.as_shape(r, round));
                p.rect_filled(r, round, Color32::WHITE);
                if let Some(pt) = app.pdf.docs.get(fid).and_then(|d| d.tex.get(&pi)) {
                    p.image(pt.tex.id(), r, Rect::from_min_max(pos2(0.0, y0), pos2(1.0, y1)), Color32::WHITE);
                }
                slice_overlays(app, ui, fid, &key, pi, y0, y1, r, adding);
            }
            if si < cuts.len() {
                crate::checkpoints::widget(app, ui, pane, &key, &cuts[si].0, x0, width);
            }
        }
        if let Some(d) = app.pdf.docs.get_mut(fid) {
            let bottom = ui.cursor().min.y;
            d.page_y.insert(pi, (top - origin, bottom - top));
            d.slices.insert(pi, slice_rects.iter().map(|(a, b, r)| (*a, *b, r.translate(vec2(0.0, -origin)))).collect());
        }
        ui.add_space(8.0);
    }
    if copy {
        if let Some(text) = selected_text(app).filter(|s| !s.is_empty()) {
            ctx.copy_text(text);
        }
    }
    // a textbook opens where you left it
    if let Some((page, y)) = app.pdf.restore.get(fid).copied() {
        if content_y(app, fid, page, y).is_some() {
            app.pdf.restore.remove(fid);
            scroll_pdf_to(app, fid, page, y, READ_LINE);
        }
    }
    // a search hit in this PDF
    crate::search::pdf(app, fid);
    // a link to a place in this PDF
    if let Some((f, page, y, start)) = app.pdf.scroll_to.take() {
        if f == fid {
            scroll_pdf_to(app, fid, page, y, if start { 12.0 } else { app.viewer.viewport_h * 0.3 });
        }
    }
}

/// Scroll the viewer so a place in the PDF sits `offset` below the top.
pub fn scroll_pdf_to(app: &mut App, fid: &str, page: usize, y: f32, offset: f32) {
    if let Some(cy) = content_y(app, fid, page, y) {
        app.viewer.scroll_to = Some((cy - offset).max(0.0));
    }
}

/// A place in the PDF in the viewer's content coordinates (from where it was drawn last).
pub fn content_y(app: &App, fid: &str, page: usize, y: f32) -> Option<f32> {
    let d = app.pdf.docs.get(fid)?;
    let slices = d.slices.get(&page)?;
    let (y0, y1, r) = slices.iter().find(|(a, b, _)| y >= *a && y <= *b)?;
    Some(r.min.y + (y - y0) / (y1 - y0) * r.height())
}

/// Over one slice: text selection, links, and the checkpoints' highlights and guides.
fn slice_overlays(app: &mut App, ui: &mut Ui, fid: &str, key: &str, page: usize, y0: f32, y1: f32, r: Rect, adding: bool) {
    let tk = t();
    let to_screen = |fx: f32, fy: f32| pos2(r.min.x + fx * r.width(), r.min.y + (fy - y0) / (y1 - y0) * r.height());
    let text = app.pdf.docs.get(fid).and_then(|d| d.text.get(&page).cloned());
    let links = app.pdf.docs.get(fid).and_then(|d| d.links.get(&page).cloned()).unwrap_or_default();
    let resp = ui.interact(r, Id::new(("pdf-slice", fid, page, (y0 * 1000.0) as i32)), Sense::click_and_drag());
    let pointer = resp.hover_pos();
    // selection highlight
    if let (Some(s), Some(t_)) = (&app.pdf.sel, &text) {
        if s.fid == fid {
            let (a, b) = if s.anchor <= s.focus { (s.anchor, s.focus) } else { (s.focus, s.anchor) };
            if page >= a.0 && page <= b.0 {
                let from = if page == a.0 { a.1 } else { 0 };
                let to = if page == b.0 { b.1 } else { t_.chars.len() };
                for c in t_.chars.iter().take(to.min(t_.chars.len())).skip(from) {
                    if c.bottom < y0 || c.top > y1 || c.x1 <= c.x0 {
                        continue;
                    }
                    let rr = Rect::from_min_max(to_screen(c.x0, c.top.max(y0)), to_screen(c.x1, c.bottom.min(y1)));
                    ui.painter().rect_filled(rr, 0.0, theme::alpha(tk.accent, 0.3));
                }
            }
        }
    }
    // links
    let mut over_link = None;
    for (i, l) in links.iter().enumerate() {
        if l.rect[3] < y0 || l.rect[1] > y1 {
            continue;
        }
        let lr = Rect::from_min_max(to_screen(l.rect[0], l.rect[1].max(y0)), to_screen(l.rect[2], l.rect[3].min(y1)));
        if !adding && pointer.map(|p| lr.contains(p)).unwrap_or(false) {
            ui.painter().rect_filled(lr, cr(2.0), theme::mix(tk.accent, Color32::TRANSPARENT, 0.14));
            over_link = Some(i);
        }
    }
    crate::checkpoints::overlays(app, ui, key, page, y0, y1, r);
    // a search hit's words, fading
    let (marks, a) = crate::search::pdf_marks(app, fid, page);
    for m in marks {
        if m[3] < y0 || m[1] > y1 {
            continue;
        }
        let mr = Rect::from_min_max(to_screen(m[0] - 0.003, m[1].max(y0)), to_screen(m[2] + 0.003, m[3].min(y1)));
        ui.painter().rect_filled(mr, cr(2.0), Color32::from_rgba_unmultiplied(255, 196, 0, (110.0 * a) as u8));
    }
    if adding {
        if let Some(p) = pointer {
            ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
            let fy = y0 + (p.y - r.min.y) / r.height() * (y1 - y0);
            crate::checkpoints::follow_guide(ui, r, p.y);
            if resp.clicked() {
                crate::checkpoints::add_cut(app, key, page, fy);
            }
        }
        return;
    }
    if let Some(i) = over_link {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        if resp.clicked() {
            let l = &links[i];
            if let Some(u) = &l.url {
                app.open_external(u);
            } else if let Some((pg, y)) = l.dest {
                app.pdf.scroll_to = Some((fid.to_string(), pg, y, true));
            }
            return;
        }
    } else if pointer.is_some() && text.as_ref().map(|t| !t.chars.is_empty()).unwrap_or(false) {
        ui.ctx().set_cursor_icon(CursorIcon::Text);
    }
    // text selection by dragging
    if let (Some(t_), Some(p)) = (&text, resp.interact_pointer_pos()) {
        let fx = (p.x - r.min.x) / r.width();
        let fy = y0 + (p.y - r.min.y) / r.height() * (y1 - y0);
        let ci = char_at(t_, fx, fy);
        if resp.drag_started() {
            app.pdf.sel = Some(Sel { fid: fid.to_string(), anchor: (page, ci), focus: (page, ci), dragging: true });
        } else if resp.clicked() {
            app.pdf.sel = None;
        }
    }
    if let (Some(s), Some(t_)) = (app.pdf.sel.as_mut(), &text) {
        if s.dragging && s.fid == fid {
            if let Some(p) = ui.ctx().pointer_interact_pos() {
                if r.y_range().contains(p.y) {
                    let fx = (p.x - r.min.x) / r.width();
                    let fy = y0 + (p.y - r.min.y) / r.height() * (y1 - y0);
                    s.focus = (page, char_at(t_, fx, fy));
                }
            }
            if !ui.input(|i| i.pointer.primary_down()) {
                s.dragging = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use pdfium_render::prelude::*;

    /// OUTLINE_PDF=/path/book.pdf cargo test -p canvas-app outline_of -- --nocapture
    #[test]
    fn outline_of() {
        let Ok(path) = std::env::var("OUTLINE_PDF") else { return };
        let pdfium = super::library().unwrap();
        let doc = pdfium.load_pdf_from_file(&path, None).unwrap();
        let o = super::outline(&doc);
        println!("{} pages, {} outline entries", doc.pages().len(), o.len());
        for it in o.iter().take(80) {
            println!("{}{} → p{} {:.2}", "  ".repeat(it.depth), it.title, it.page + 1, it.y);
        }
        // where the sections' headings are on their pages
        for it in o.iter().filter(|i| i.depth == 1).take(12) {
            let page = doc.pages().get(it.page as PdfPageIndex).unwrap();
            let y = crate::checkpoints::find_heading(&super::page_text(&page), &it.title);
            println!("heading {:40} p{} y={:?}", it.title, it.page + 1, y);
        }
        if let Ok(pages) = std::env::var("OUTLINE_PAGES") {
            for p in pages.split(',').filter_map(|x| x.parse::<usize>().ok()) {
                let t = super::page_text(&doc.pages().get((p - 1) as PdfPageIndex).unwrap());
                println!("--- page {p}");
                for it in t.items.iter().take(14) {
                    println!("  y={:.3} h={:.4} {:?}", it.y, it.h, it.s.chars().take(70).collect::<String>());
                }
            }
        }
    }

    #[test]
    fn fingerprints() {
        let bytes = canvas_mcp::demo::pdf("T", 1);
        let fp = super::fingerprint(&bytes);
        assert_eq!(fp.len(), 32);
        let with_id = b"%PDF-1.4\ntrailer\n<< /ID [<ABCDEF0123> <ABCDEF0123>] >>\n%%EOF".to_vec();
        assert_eq!(super::fingerprint(&with_id), "abcdef0123");
    }
}

/// Where "where you're reading" is: this far below the top of the viewer (a sliver of the page
/// before doesn't count).
const READ_LINE: f32 = 40.0;

/// Where the reader is: the page at the top of the viewer, plus how far down it (page index +
/// fraction), from where the pages were drawn last frame.
pub fn current_pos(app: &App, fid: &str) -> Option<f32> {
    let d = app.pdf.docs.get(fid)?;
    let y = app.viewer.scroll + READ_LINE;
    let (page, (top, h)) = d.page_y.iter().filter(|(_, (top, _))| *top <= y).max_by(|a, b| a.1.0.total_cmp(&b.1.0))?;
    Some(*page as f32 + ((y - top) / h.max(1.0)).clamp(0.0, 0.999))
}

/// The PDF behind a viewer address, when it's one that's open: a Canvas file or a textbook.
pub fn fid_of(app: &App, href: &str) -> Option<String> {
    let fid = match crate::route::parse(href).view {
        crate::route::View::File(_, f) => f,
        crate::route::View::Textbook(id) => format!("tb-{id}"),
        _ => return None,
    };
    app.pdf.info(&fid).map(|_| fid)
}

// ---------- the reader's bar and its Contents panel ----------
/// The PDF's one-row bar: its name (cut to fit) and size, then Contents (when it has an outline),
/// Checkpoints, Fullscreen and, for a Canvas file, Canvas ↗.
pub fn reader_bar(app: &mut App, ui: &mut Ui, fid: &str, name: &str, size: &str, canvas_url: Option<&str>) {
    let tk = t();
    let outline = app.pdf.info(fid).map(|i| !i.outline.is_empty()).unwrap_or(false);
    let full = app.panes.fullscreen;
    let cp_on = app.cp.mode;
    let contents_on = app.pdf.contents;
    let opts = |pressed| crate::widgets::ButtonOpts { size: 13.0, pad: vec2(11.0, 5.0), pressed, ..Default::default() };
    let mut labels: Vec<&str> = vec!["Checkpoints", if full { "Exit fullscreen" } else { "Fullscreen" }];
    if outline {
        labels.push("Contents");
    }
    if canvas_url.is_some() {
        labels.push("Canvas ↗");
    }
    let btn_w: f32 = labels.iter().map(|l| crate::widgets::lay(ui, l, Ts::new(13.0, 400, tk.text), None, false).size().x + 24.0 + 6.0).sum();
    ui.add_space(-6.0);
    let avail = ui.available_width();
    let size_w = crate::widgets::lay(ui, size, Ts::faint(12.0), None, false).size().x;
    // narrow: the name on its own line, the buttons under it
    let two_rows = avail - btn_w - 20.0 < 200.0;
    let title_w = if two_rows { avail } else { (avail - btn_w - size_w - 20.0).max(60.0) };
    let (row, _) = ui.allocate_exact_size(vec2(avail, 34.0), Sense::hover());
    let ts = Ts::new(14.0, 650, tk.text).sp(-0.01);
    let g = crate::widgets::lay(ui, name, ts, Some(title_w), true);
    let whole = crate::widgets::lay(ui, name, ts, None, false).size().x <= g.size().x + 1.0;
    let gw = g.size().x;
    let title_rect = Rect::from_min_size(pos2(row.min.x, row.center().y - g.size().y / 2.0), g.size());
    ui.painter().galley(title_rect.min, g, tk.text);
    ui.interact(title_rect, Id::new(("pdf-title", fid)), Sense::hover()).on_hover_text(name);
    if whole && !size.is_empty() && gw + 8.0 + size_w <= avail {
        let sg = crate::widgets::lay(ui, size, Ts::faint(12.0), None, false);
        ui.painter().galley(pos2(row.min.x + gw + 8.0, row.center().y - sg.size().y / 2.0), sg, tk.faint);
    }
    let bar_rect = if two_rows {
        let (r2, _) = ui.allocate_exact_size(vec2(avail, 38.0), Sense::hover());
        r2
    } else {
        row
    };
    let layout = if two_rows { egui::Layout::left_to_right(egui::Align::Center) } else { egui::Layout::right_to_left(egui::Align::Center) };
    let mut bar = ui.new_child(egui::UiBuilder::new().max_rect(bar_rect).layout(layout));
    bar.spacing_mut().item_spacing.x = 6.0;
    let mut order = vec!["contents", "checkpoints", "fullscreen", "canvas"];
    if !two_rows {
        order.reverse(); // laid out from the right
    }
    for b in order {
        match b {
            "contents" if outline => {
                if crate::widgets::button_ex(&mut bar, "Contents", opts(contents_on)).on_hover_text("Chapters and sections (O)").clicked() {
                    set_contents(app, !contents_on);
                }
            }
            "checkpoints" => {
                if crate::widgets::button_ex(&mut bar, "Checkpoints", opts(cp_on)).on_hover_text("Checkpoint mode (M)").clicked() {
                    crate::checkpoints::set_mode(app, !cp_on);
                }
            }
            "fullscreen" => {
                if crate::widgets::button_ex(&mut bar, if full { "Exit fullscreen" } else { "Fullscreen" }, opts(full)).on_hover_text("Fullscreen (F11)").clicked() {
                    crate::panes::set_fullscreen(app, !full);
                }
            }
            "canvas" => {
                if let Some(u) = canvas_url {
                    if crate::widgets::button_ex(&mut bar, "Canvas ↗", opts(false)).on_hover_text("Open in Canvas").clicked() {
                        app.open_external(u);
                    }
                }
            }
            _ => {}
        }
    }
    ui.add_space(8.0);
}

pub fn set_contents(app: &mut App, on: bool) {
    app.pdf.contents = on;
    app.set_pref("pdfContents", serde_json::json!(on));
}

/// Which outline entry the reader is in: the last one starting at or before `pos`.
pub fn section_at(outline: &[OutlineItem], pos: f32) -> Option<usize> {
    outline.iter().enumerate().filter(|(_, it)| it.page as f32 + it.y <= pos + 0.002).map(|(i, _)| i).last()
}

/// The Contents panel: the outline as a tree (chapters open to their sections), the section
/// you're in marked. Tapping a title goes there; the arrow opens or closes it.
pub fn contents(app: &mut App, ui: &mut Ui, fid: &str) {
    let tk = t();
    let Some(info) = app.pdf.info(fid).cloned() else { return };
    let items = info.outline.clone();
    let pos = current_pos(app, fid).unwrap_or(0.0);
    let cur = section_at(&items, pos);
    // the chain of entries leading to the current one stays open
    let mut ancestors = std::collections::HashSet::new();
    if let Some(c) = cur {
        let mut depth = items[c].depth;
        for i in (0..c).rev() {
            if items[i].depth < depth {
                ancestors.insert(i);
                depth = items[i].depth;
            }
        }
    }
    let expanded = app.pdf.expanded.entry(fid.to_string()).or_default().clone();
    let has_kids = |i: usize| items.get(i + 1).map(|n| n.depth > items[i].depth).unwrap_or(false);
    let is_open = |i: usize| expanded.contains(&i) || (ancestors.contains(&i) && !expanded.contains(&(usize::MAX - i)));
    ui.add_space(8.0);
    crate::widgets::text_line(ui, "   Contents", Ts::new(11.0, 600, tk.faint).up().sp(0.06));
    ui.add_space(4.0);
    let w = ui.available_width();
    let mut toggle: Option<usize> = None;
    let mut go: Option<usize> = None;
    let mut hidden_below: Option<usize> = None; // inside a closed entry of this depth
    for (i, it) in items.iter().enumerate() {
        if let Some(d) = hidden_below {
            if it.depth > d {
                continue;
            }
            hidden_below = None;
        }
        let kids = has_kids(i);
        let open = kids && is_open(i);
        if kids && !open {
            hidden_below = Some(it.depth);
        }
        let indent = 8.0 + it.depth as f32 * 16.0;
        let tx_off = indent + 28.0;
        let pg = format!("{}", it.page + 1);
        let pgg = crate::widgets::lay(ui, &pg, Ts::faint(11.5), None, false);
        let pw = pgg.size().x;
        let here = Some(i) == cur;
        let ts = Ts::new(13.5, if it.depth == 0 { 600 } else { 400 }, if here { tk.accent } else { tk.text });
        // titles wrap onto a second line (then end in …)
        let mut job = egui::text::LayoutJob::single_section(it.title.clone(), ts.format());
        job.wrap = egui::text::TextWrapping { max_width: (w - 12.0 - pw - 8.0 - tx_off).max(20.0), max_rows: 2, break_anywhere: false, overflow_character: Some('…') };
        let g = ui.painter().layout_job(job);
        let (r, resp) = ui.allocate_exact_size(vec2(w, (g.size().y + 14.0).max(36.0)), Sense::click());
        if here {
            ui.painter().rect_filled(r.shrink2(vec2(4.0, 1.0)), cr(6.0), tk.accent_soft);
        } else if resp.hovered() {
            ui.painter().rect_filled(r.shrink2(vec2(4.0, 1.0)), cr(6.0), tk.hover);
        }
        // the arrow: its own 32px target
        let arrow = Rect::from_min_size(pos2(r.min.x + indent, r.center().y - 16.0), vec2(28.0, 32.0));
        if kids {
            let ar = ui.interact(arrow, Id::new(("toc-arrow", fid, i)), Sense::click());
            let c = arrow.center();
            let col = if ar.hovered() { tk.text } else { tk.faint };
            let pts = if open { vec![c + vec2(-4.0, -2.0), c + vec2(0.0, 2.5), c + vec2(4.0, -2.0)] } else { vec![c + vec2(-2.0, -4.0), c + vec2(2.5, 0.0), c + vec2(-2.0, 4.0)] };
            ui.painter().add(egui::Shape::line(pts, egui::Stroke::new(1.6, col)));
            if ar.clicked() {
                toggle = Some(i);
            }
        }
        let tx = r.min.x + tx_off;
        ui.painter().galley(pos2(tx, r.center().y - g.size().y / 2.0), g, ts.color);
        ui.painter().galley(pos2(r.max.x - 12.0 - pw, r.center().y - pgg.size().y / 2.0), pgg, tk.faint);
        if resp.clicked() && !(kids && arrow.contains(resp.interact_pointer_pos().unwrap_or_default())) {
            go = Some(i);
        }
        resp.on_hover_cursor(CursorIcon::PointingHand);
    }
    ui.add_space(12.0);
    if let Some(i) = toggle {
        let open = is_open(i);
        let set = app.pdf.expanded.entry(fid.to_string()).or_default();
        if open {
            set.remove(&i);
            // an open ancestor of where you are needs saying "closed" explicitly
            if ancestors.contains(&i) {
                set.insert(usize::MAX - i);
            }
        } else {
            set.insert(i);
            set.remove(&(usize::MAX - i));
        }
    }
    if let Some(i) = go {
        let it = &items[i];
        let y = crate::checkpoints::heading_y(app, fid, it).unwrap_or(it.y);
        scroll_pdf_to(app, fid, it.page, y, 12.0);
        // the page may not be laid out yet (far away): go there once it is
        if content_y(app, fid, it.page, y).is_none() {
            app.pdf.restore.insert(fid.to_string(), (it.page, y));
        }
    }
}
