//! Canvas HTML (and Anki cards), shown natively: parsed into blocks and inline runs, then laid out
//! the way the old stylesheet did it (.content: paragraphs, headings, lists, tables, code, quotes,
//! images). Nothing in it runs: scripts, styles, forms and handlers are dropped; links to Canvas
//! become links inside the app; images load through your Canvas session; embedded pages (videos,
//! tools) can't be shown without a browser engine, so they become links that open outside.

use std::collections::HashMap;
use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat};
use egui::{Align, Color32, CursorIcon, Galley, Id, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use scraper::{ElementRef, Html, Node};

use crate::app::{App, Pane};
use crate::images::Src;
use crate::theme::{self, t};
use crate::widgets::cr;

// --- the model ---------------------------------------------------------------------------------------
#[derive(Clone, Copy, Default, PartialEq)]
pub struct Sty {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub code: bool,
    pub sup: bool,
    pub sub: bool,
    pub mark: bool,
    pub scale: f32,
    pub link: Option<u32>,
}

impl Sty {
    fn scale(&self) -> f32 {
        if self.scale == 0.0 { 1.0 } else { self.scale }
    }
}

#[derive(Clone, Debug)]
pub struct ImgRef {
    pub src: Option<Src>,
    pub alt: String,
    pub w: Option<f32>,
    pub h: Option<f32>,
    pub class: String,
}

#[derive(Clone)]
pub enum Inl {
    Text(String, Sty),
    Br,
    Img(ImgRef, Sty),
    Math(String, Sty),
    Smiles(String),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum TAlign {
    Left,
    Center,
    Right,
}

#[derive(Clone)]
pub struct Cell {
    pub header: bool,
    pub colspan: usize,
    pub blocks: Vec<Block>,
    pub align: Option<TAlign>,
}

#[derive(Clone)]
pub enum Block {
    Para { inl: Vec<Inl>, align: Option<TAlign> },
    Heading { level: u8, inl: Vec<Inl>, align: Option<TAlign> },
    List { ordered: bool, start: i64, items: Vec<Vec<Block>> },
    Quote(Vec<Block>),
    Pre(String),
    Table(Vec<Vec<Cell>>),
    Hr { answer: bool },
    Image(ImgRef, Option<u32>, Option<TAlign>),
    Embed { url: String, label: String },
    Hint { label: String, blocks: Vec<Block>, key: String },
    Div { blocks: Vec<Block>, align: Option<TAlign>, margin: (f32, f32), indent: f32, class: String },
    Math(String),
}

pub struct Doc {
    pub blocks: Vec<Block>,
    pub links: Vec<String>,
    /// Anki card CSS we honor: .card { font-size, text-align }
    pub card_size: Option<f32>,
    pub card_align: Option<TAlign>,
}

/// How to treat links and images: Canvas content, or an Anki card.
#[derive(Clone, Copy, PartialEq)]
pub enum Flavor {
    Canvas,
    Anki,
}

#[derive(Default)]
pub struct HtmlCache {
    docs: HashMap<(u64, bool, String), Arc<Doc>>,
    pub hints: std::collections::HashSet<String>,
}

fn hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

impl HtmlCache {
    pub fn doc(&mut self, html: &str, flavor: Flavor, base: &str) -> Arc<Doc> {
        let key = (hash(html), flavor == Flavor::Anki, base.to_string());
        if let Some(d) = self.docs.get(&key) {
            return d.clone();
        }
        let d = Arc::new(parse(html, flavor, base));
        if self.docs.len() > 400 {
            self.docs.clear();
        }
        self.docs.insert(key, d.clone());
        d
    }
}

// --- links ---------------------------------------------------------------------------------------------
fn host(url: &str) -> String {
    canvas_mcp::client::host_of(url)
}

pub fn absolute(href: &str, base: &str) -> Option<String> {
    let b = url::Url::parse(&format!("{}/", base.trim_end_matches('/'))).ok()?;
    b.join(href.trim()).ok().map(|u| u.to_string())
}

/// A Canvas URL as an address in the app (a page, assignment, discussion, file, course section).
pub fn app_link_for(href: &str, base: &str) -> Option<String> {
    let u = url::Url::parse(&absolute(href, base)?).ok()?;
    if u.host_str().map(|h| h.to_lowercase()) != Some(host(base)) {
        return None;
    }
    let p = u.path().trim_end_matches('/').to_string();
    let re = |s: &str| regex::Regex::new(s).unwrap();
    if let Some(m) = re(r"^/courses/(\d+)/pages/([^/]+)$").captures(&p) {
        return Some(format!("#/c/{}/p/{}", &m[1], &m[2]));
    }
    if let Some(m) = re(r"^/courses/(\d+)/assignments/(\d+)$").captures(&p) {
        return Some(format!("#/c/{}/a/{}", &m[1], &m[2]));
    }
    if let Some(m) = re(r"^/courses/(\d+)/(?:discussion_topics|announcements)/(\d+)$").captures(&p) {
        return Some(format!("#/c/{}/d/{}", &m[1], &m[2]));
    }
    if let Some(m) = re(r"^/courses/(\d+)/files/(\d+)(?:/(?:download|preview))?$").captures(&p) {
        return Some(format!("#/c/{}/f/{}", &m[1], &m[2]));
    }
    if let Some(m) = re(r"^/files/(\d+)(?:/(?:download|preview))?$").captures(&p) {
        return Some(format!("#/f/{}", &m[1]));
    }
    if let Some(m) = re(r"^/courses/(\d+)(?:/(modules|assignments|grades|announcements|discussion_topics|pages|files|assignments/syllabus|wiki))?$").captures(&p) {
        let tab = match m.get(2).map(|x| x.as_str()) {
            Some("discussion_topics") => "discussions",
            Some("assignments/syllabus") => "syllabus",
            Some("wiki") => "pages",
            Some(x) => x,
            None => "modules",
        };
        return Some(format!("#/c/{}/{tab}", &m[1]));
    }
    None
}

pub fn is_canvas_url(src: &str, base: &str) -> bool {
    absolute(src, base).map(|u| host(&u) == host(base)).unwrap_or(false)
}

fn bad_scheme(v: &str) -> bool {
    let l = v.trim_start().to_lowercase();
    l.starts_with("javascript") || l.starts_with("vbscript") || l.starts_with("data:text")
}

// --- parsing -------------------------------------------------------------------------------------------
struct P<'a> {
    flavor: Flavor,
    base: &'a str,
    links: Vec<String>,
    hint_n: usize,
}

fn attr<'a>(e: &'a ElementRef, n: &str) -> Option<&'a str> {
    e.value().attr(n)
}

fn style_of(e: &ElementRef) -> HashMap<String, String> {
    let mut m = HashMap::new();
    if let Some(s) = attr(e, "style") {
        for decl in s.split(';') {
            if let Some((k, v)) = decl.split_once(':') {
                m.insert(k.trim().to_lowercase(), v.trim().trim_end_matches("!important").trim().to_lowercase());
            }
        }
    }
    m
}

fn align_of(e: &ElementRef) -> Option<TAlign> {
    let st = style_of(e);
    let v = st.get("text-align").cloned().or_else(|| attr(e, "align").map(|a| a.to_lowercase()));
    match v.as_deref() {
        Some("center") => Some(TAlign::Center),
        Some("right") | Some("end") => Some(TAlign::Right),
        Some("left") | Some("start") | Some("justify") => Some(TAlign::Left),
        _ => None,
    }
}

fn font_scale(v: &str) -> Option<f32> {
    let v = v.trim();
    let num = |s: &str| s.trim().parse::<f32>().ok();
    if let Some(px) = v.strip_suffix("px") {
        return num(px).map(|p| p / 14.0);
    }
    if let Some(pt) = v.strip_suffix("pt") {
        return num(pt).map(|p| p * 4.0 / 3.0 / 14.0);
    }
    if let Some(em) = v.strip_suffix("rem").or_else(|| v.strip_suffix("em")) {
        return num(em);
    }
    if let Some(pc) = v.strip_suffix('%') {
        return num(pc).map(|p| p / 100.0);
    }
    match v {
        "xx-small" => Some(0.6),
        "x-small" => Some(0.75),
        "small" | "smaller" => Some(0.89),
        "medium" => Some(1.0),
        "large" | "larger" => Some(1.2),
        "x-large" => Some(1.5),
        "xx-large" => Some(2.0),
        _ => None,
    }
}

fn hidden(e: &ElementRef) -> bool {
    style_of(e).get("display").map(|d| d.starts_with("none")).unwrap_or(false) || attr(e, "hidden").is_some()
}

const SKIP: &[&str] = &["script", "style", "link", "meta", "base", "form", "object", "embed", "noscript", "head", "title", "template", "svg", "button", "input", "select", "textarea"];
const BLOCKS: &[&str] = &[
    "p", "div", "h1", "h2", "h3", "h4", "h5", "h6", "ul", "ol", "li", "blockquote", "pre", "table", "hr", "section", "article", "header", "footer", "main", "aside", "nav", "figure",
    "figcaption", "center", "dl", "dt", "dd", "address", "details", "summary", "iframe", "video", "audio", "tbody", "thead", "tr", "td", "th", "body", "html",
];

/// Text split into plain runs, \( … \) / \[ … \] math, and \smiles{…}.
pub fn split_math(text: &str) -> Vec<(u8, String)> {
    let mut out: Vec<(u8, String)> = Vec::new();
    let mut rest = text;
    loop {
        let cands = [("\\(", "\\)", 1u8), ("\\[", "\\]", 2u8), ("\\smiles{", "}", 3u8)];
        let next = cands.iter().filter_map(|(o, c, k)| rest.find(o).map(|i| (i, *o, *c, *k))).min_by_key(|x| x.0);
        let Some((i, open, close, kind)) = next else {
            if !rest.is_empty() {
                out.push((0, rest.to_string()));
            }
            return out;
        };
        let after = &rest[i + open.len()..];
        let Some(j) = after.find(close) else {
            out.push((0, rest.to_string()));
            return out;
        };
        if i > 0 {
            out.push((0, rest[..i].to_string()));
        }
        out.push((kind, after[..j].trim().to_string()));
        rest = &after[j + close.len()..];
    }
}

impl P<'_> {
    fn link(&mut self, href: &str) -> Option<u32> {
        if bad_scheme(href) {
            return None;
        }
        let target = if self.flavor == Flavor::Anki {
            if href.to_lowercase().starts_with("http") { href.to_string() } else { return None }
        } else if let Some(a) = app_link_for(href, self.base) {
            a
        } else if href.starts_with('#') {
            return None;
        } else {
            absolute(href, self.base)?
        };
        self.links.push(target);
        Some(self.links.len() as u32 - 1)
    }

    fn img(&mut self, e: &ElementRef) -> ImgRef {
        let src = attr(e, "src").unwrap_or("").trim();
        let dim = |n: &str| attr(e, n).and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok()).filter(|v| *v > 0.0);
        let st = style_of(e);
        let sdim = |n: &str| st.get(n).and_then(|v| v.strip_suffix("px")).and_then(|v| v.trim().parse::<f32>().ok());
        let src = if src.is_empty() || bad_scheme(src) {
            None
        } else if self.flavor == Flavor::Anki {
            if src.starts_with("http") { Some(Src::Web(src.to_string())) } else {
                let name = percent_encoding::percent_decode_str(src).decode_utf8_lossy().into_owned();
                Some(Src::Anki(name))
            }
        } else if src.starts_with("data:image/") {
            Some(Src::Data(src.to_string()))
        } else if is_canvas_url(src, self.base) {
            absolute(src, self.base).map(Src::Proxy)
        } else {
            absolute(src, self.base).filter(|u| u.starts_with("https://")).map(Src::Web)
        };
        ImgRef { src, alt: attr(e, "alt").unwrap_or("").to_string(), w: sdim("width").or_else(|| dim("width")), h: sdim("height").or_else(|| dim("height")), class: attr(e, "class").unwrap_or("").to_string() }
    }

    fn push_text(&mut self, inl: &mut Vec<Inl>, text: &str, sty: Sty, pre: bool) {
        let text = if pre { text.to_string() } else { collapse(text) };
        if text.is_empty() {
            return;
        }
        for (kind, part) in split_math(&text) {
            match kind {
                1 => inl.push(Inl::Math(part, sty)),
                2 => inl.push(Inl::Math(format!("\\displaystyle {part}"), sty)),
                3 => inl.push(Inl::Smiles(part)),
                _ => inl.push(Inl::Text(part, sty)),
            }
        }
    }

    /// Walk children, collecting inline runs into paragraphs and block elements into blocks.
    fn children(&mut self, e: &ElementRef, sty: Sty, out: &mut Vec<Block>, inl: &mut Vec<Inl>, align: Option<TAlign>) {
        let kids: Vec<_> = e.children().collect();
        let mut i = 0;
        while i < kids.len() {
            let node = kids[i];
            match node.value() {
                Node::Text(t) => self.push_text(inl, t, sty, false),
                Node::Element(_) => {
                    let Some(ce) = ElementRef::wrap(node) else {
                        i += 1;
                        continue;
                    };
                    let name = ce.value().name().to_lowercase();
                    // Anki's hint: <a class=hint onclick=…>Show Hint</a><div class=hint style="display: none">…</div>
                    if name == "a" && attr(&ce, "class").map(|c| c.split_whitespace().any(|x| x == "hint")).unwrap_or(false) && attr(&ce, "onclick").is_some() {
                        let next = kids[i + 1..].iter().find_map(|n| ElementRef::wrap(*n));
                        if let Some(div) = next.filter(|d| d.value().name() == "div") {
                            self.flush(out, inl, align);
                            let mut inner = Vec::new();
                            let mut ii = Vec::new();
                            self.children(&div, sty, &mut inner, &mut ii, align);
                            self.flush(&mut inner, &mut ii, align);
                            self.hint_n += 1;
                            out.push(Block::Hint { label: collapse(&ce.text().collect::<String>()).trim().to_string(), blocks: inner, key: format!("hint{}", self.hint_n) });
                            let skip_to = kids[i + 1..].iter().position(|n| n.id() == div.id()).map(|p| i + 1 + p).unwrap_or(i);
                            i = skip_to + 1;
                            continue;
                        }
                    }
                    self.element(&ce, &name, sty, out, inl, align);
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn flush(&mut self, out: &mut Vec<Block>, inl: &mut Vec<Inl>, align: Option<TAlign>) {
        if inl.is_empty() {
            return;
        }
        let mut runs = std::mem::take(inl);
        trim_runs(&mut runs);
        if runs.is_empty() {
            return;
        }
        // A paragraph that's only one big picture is a block image.
        if runs.len() == 1 {
            if let Inl::Img(im, s) = &runs[0] {
                out.push(Block::Image(im.clone(), s.link, align));
                return;
            }
        }
        out.push(Block::Para { inl: runs, align });
    }

    fn block_children(&mut self, e: &ElementRef, sty: Sty, align: Option<TAlign>) -> Vec<Block> {
        let mut out = Vec::new();
        let mut inl = Vec::new();
        self.children(e, sty, &mut out, &mut inl, align);
        self.flush(&mut out, &mut inl, align);
        out
    }

    fn element(&mut self, e: &ElementRef, name: &str, sty: Sty, out: &mut Vec<Block>, inl: &mut Vec<Inl>, align: Option<TAlign>) {
        if SKIP.contains(&name) || hidden(e) {
            return;
        }
        let st = style_of(e);
        let mut s2 = sty;
        if let Some(fs) = st.get("font-size").and_then(|v| font_scale(v)) {
            s2.scale = fs.clamp(0.5, 3.0);
        }
        if st.get("font-weight").map(|w| w == "bold" || w == "bolder" || w.parse::<u32>().map(|n| n >= 600).unwrap_or(false)).unwrap_or(false) {
            s2.bold = true;
        }
        if st.get("font-style").map(|v| v == "italic" || v == "oblique").unwrap_or(false) {
            s2.italic = true;
        }
        if let Some(td) = st.get("text-decoration").or(st.get("text-decoration-line")) {
            if td.contains("underline") {
                s2.underline = true;
            }
            if td.contains("line-through") {
                s2.strike = true;
            }
        }
        let own_align = align_of(e).or(align);
        if BLOCKS.contains(&name) {
            self.flush(out, inl, align);
        }
        match name {
            "br" => inl.push(Inl::Br),
            "b" | "strong" => self.children(e, Sty { bold: true, ..s2 }, out, inl, align),
            "i" | "em" | "cite" | "var" | "dfn" => self.children(e, Sty { italic: true, ..s2 }, out, inl, align),
            "u" | "ins" => self.children(e, Sty { underline: true, ..s2 }, out, inl, align),
            "s" | "strike" | "del" => self.children(e, Sty { strike: true, ..s2 }, out, inl, align),
            "code" | "kbd" | "samp" | "tt" => self.children(e, Sty { code: true, ..s2 }, out, inl, align),
            "sup" => self.children(e, Sty { sup: true, scale: s2.scale() * 0.83, ..s2 }, out, inl, align),
            "sub" => self.children(e, Sty { sub: true, scale: s2.scale() * 0.83, ..s2 }, out, inl, align),
            "small" => self.children(e, Sty { scale: s2.scale() * 0.83, ..s2 }, out, inl, align),
            "big" => self.children(e, Sty { scale: s2.scale() * 1.17, ..s2 }, out, inl, align),
            "mark" => self.children(e, Sty { mark: true, ..s2 }, out, inl, align),
            "a" => {
                let link = attr(e, "href").and_then(|h| self.link(h));
                self.children(e, Sty { link: link.or(s2.link), ..s2 }, out, inl, align);
            }
            "img" => {
                let im = self.img(e);
                inl.push(Inl::Img(im, s2));
            }
            "iframe" | "video" | "audio" => {
                let src = attr(e, "src").map(String::from).or_else(|| e.select(&scraper::Selector::parse("source").unwrap()).find_map(|s| s.value().attr("src").map(String::from))).unwrap_or_default();
                let abs = if src.is_empty() { Some(self.base.to_string()) } else { absolute(&src, self.base) };
                if let Some(url) = abs.filter(|u| !bad_scheme(u)) {
                    let canvas = is_canvas_url(&url, self.base);
                    let label = if name == "iframe" {
                        if canvas { "Open embedded content in Canvas ↗".to_string() } else { format!("Open embedded content from {} ↗", host(&url)) }
                    } else if name == "video" {
                        "Open the video ↗".to_string()
                    } else {
                        "Open the audio ↗".to_string()
                    };
                    out.push(Block::Embed { url, label });
                }
            }
            "p" | "address" | "summary" | "dt" | "figcaption" => {
                let blocks = self.block_children(e, s2, own_align);
                out.push(Block::Div { blocks, align: own_align, margin: if name == "p" { (1.0, 1.0) } else { (0.0, 0.0) }, indent: 0.0, class: "p".into() });
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = name[1..].parse().unwrap_or(2);
                let mut runs = Vec::new();
                let mut inner_blocks = Vec::new();
                self.children(e, s2, &mut inner_blocks, &mut runs, own_align);
                trim_runs(&mut runs);
                if !runs.is_empty() {
                    out.push(Block::Heading { level, inl: runs, align: own_align });
                }
                out.extend(inner_blocks);
            }
            "ul" | "ol" | "menu" => {
                let ordered = name == "ol";
                let start = attr(e, "start").and_then(|s| s.parse().ok()).unwrap_or(1);
                let mut items = Vec::new();
                for c in e.children().filter_map(ElementRef::wrap) {
                    if c.value().name() == "li" && !hidden(&c) {
                        items.push(self.block_children(&c, s2, align_of(&c).or(own_align)));
                    } else if matches!(c.value().name(), "ul" | "ol") {
                        // a nested list directly in a list belongs to the previous item
                        let mut nested = Vec::new();
                        let mut ii = Vec::new();
                        self.element(&c, c.value().name(), s2, &mut nested, &mut ii, own_align);
                        match items.last_mut() {
                            Some(last) => last.extend(nested),
                            None => items.push(nested),
                        }
                    }
                }
                out.push(Block::List { ordered, start, items });
            }
            "li" => {
                let blocks = self.block_children(e, s2, own_align);
                out.push(Block::List { ordered: false, start: 1, items: vec![blocks] });
            }
            "blockquote" => out.push(Block::Quote(self.block_children(e, s2, own_align))),
            "pre" => out.push(Block::Pre(e.text().collect::<String>().trim_end_matches('\n').to_string())),
            "hr" => out.push(Block::Hr { answer: attr(e, "id") == Some("answer") }),
            "table" => {
                let mut rows = Vec::new();
                let sel = scraper::Selector::parse("tr").unwrap();
                for tr in e.select(&sel) {
                    // only rows of this table, not nested ones
                    if tr.ancestors().filter_map(ElementRef::wrap).find(|a| a.value().name() == "table").map(|t| t.id()) != Some(e.id()) {
                        continue;
                    }
                    let mut cells = Vec::new();
                    for c in tr.children().filter_map(ElementRef::wrap) {
                        let n = c.value().name();
                        if n == "td" || n == "th" {
                            let header = n == "th";
                            let a = align_of(&c).or(if header { Some(TAlign::Center) } else { None });
                            let blocks = self.block_children(&c, Sty { bold: header || s2.bold, ..s2 }, a);
                            cells.push(Cell { header, colspan: attr(&c, "colspan").and_then(|x| x.parse().ok()).unwrap_or(1).max(1), blocks, align: a });
                        }
                    }
                    if !cells.is_empty() {
                        rows.push(cells);
                    }
                }
                if !rows.is_empty() {
                    out.push(Block::Table(rows));
                }
            }
            "dd" => {
                let blocks = self.block_children(e, s2, own_align);
                out.push(Block::Div { blocks, align: own_align, margin: (0.0, 0.0), indent: 40.0, class: String::new() });
            }
            "figure" => {
                let blocks = self.block_children(e, s2, own_align);
                out.push(Block::Div { blocks, align: own_align, margin: (1.0, 1.0), indent: 40.0, class: String::new() });
            }
            "dl" => {
                let blocks = self.block_children(e, s2, own_align);
                out.push(Block::Div { blocks, align: own_align, margin: (1.0, 1.0), indent: 0.0, class: String::new() });
            }
            "center" => {
                let blocks = self.block_children(e, s2, Some(TAlign::Center));
                out.push(Block::Div { blocks, align: Some(TAlign::Center), margin: (0.0, 0.0), indent: 0.0, class: String::new() });
            }
            n if BLOCKS.contains(&n) => {
                let class = attr(e, "class").unwrap_or("").to_string();
                let blocks = self.block_children(e, s2, own_align);
                out.push(Block::Div { blocks, align: own_align, margin: (0.0, 0.0), indent: 0.0, class });
            }
            _ => self.children(e, s2, out, inl, align), // span, font, abbr, label, …
        }
    }
}

fn collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut sp = false;
    for c in s.chars() {
        if c == ' ' || c == '\n' || c == '\t' || c == '\r' || c == '\u{c}' {
            if !sp {
                out.push(' ');
            }
            sp = true;
        } else {
            out.push(c);
            sp = false;
        }
    }
    out
}

/// Drop whitespace at the start and end of a paragraph (and doubled spaces between runs).
fn trim_runs(runs: &mut Vec<Inl>) {
    let mut prev_space = true;
    for r in runs.iter_mut() {
        match r {
            Inl::Text(t, _) => {
                if prev_space {
                    *t = t.trim_start_matches(' ').to_string();
                }
                if !t.is_empty() {
                    prev_space = t.ends_with(' ');
                }
            }
            Inl::Br => prev_space = true,
            _ => prev_space = false,
        }
    }
    while let Some(Inl::Text(t, _)) = runs.last_mut() {
        let tt = t.trim_end_matches(' ').to_string();
        if tt.is_empty() {
            runs.pop();
        } else {
            *t = tt;
            break;
        }
    }
    while matches!(runs.last(), Some(Inl::Br)) {
        runs.pop();
    }
    runs.retain(|r| !matches!(r, Inl::Text(t, _) if t.is_empty()));
}

pub fn parse(html: &str, flavor: Flavor, base: &str) -> Doc {
    let frag = Html::parse_fragment(html);
    let mut p = P { flavor, base, links: Vec::new(), hint_n: 0 };
    let root = frag.root_element();
    let blocks = p.block_children(&root, Sty::default(), None);
    let (mut card_size, mut card_align) = (None, None);
    if flavor == Flavor::Anki {
        // The note type's CSS: honor .card's font size and alignment.
        let sel = scraper::Selector::parse("style").unwrap();
        for s in frag.select(&sel) {
            let css: String = s.text().collect();
            for rule in css.split('}') {
                if let Some((sel, body)) = rule.split_once('{') {
                    if sel.split(',').any(|x| x.trim() == ".card") {
                        for decl in body.split(';') {
                            if let Some((k, v)) = decl.split_once(':') {
                                match k.trim() {
                                    "font-size" => card_size = font_scale(v.trim()).map(|x| x * 14.0),
                                    "text-align" => {
                                        card_align = match v.trim() {
                                            "left" => Some(TAlign::Left),
                                            "right" => Some(TAlign::Right),
                                            "center" => Some(TAlign::Center),
                                            _ => card_align,
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Doc { blocks, links: p.links, card_size, card_align }
}

/// Plain text of some HTML, whitespace collapsed (textOf).
pub fn text_of(html: &str) -> String {
    if !html.contains('<') && !html.contains('&') {
        return collapse(html).trim().to_string();
    }
    let frag = Html::parse_fragment(html);
    let mut s = String::new();
    for node in frag.root_element().descendants() {
        if let Node::Text(t) = node.value() {
            let skip = node.ancestors().filter_map(ElementRef::wrap).any(|a| matches!(a.value().name(), "script" | "style"));
            if !skip {
                s.push_str(t);
            }
        }
    }
    collapse(&s).trim().to_string()
}

// --- layout and drawing --------------------------------------------------------------------------------
/// The text environment blocks are drawn in.
#[derive(Clone, Copy)]
pub struct Env {
    pub size: f32,
    pub lh: f32,
    pub color: Color32,
    pub align: TAlign,
    pub pane: Pane,
    /// the paragraphs' weight
    pub weight: u16,
}

impl Env {
    pub fn content(pane: Pane) -> Env {
        Env { size: 14.0, lh: 1.5, color: t().text, align: TAlign::Left, pane, weight: 400 }
    }
}

fn weight(bold: bool, base: u16) -> u16 {
    if bold { 700 } else { base }
}

fn format(env: &Env, s: &Sty, base_weight: u16, doc: &Doc) -> TextFormat {
    let tk = t();
    let size = env.size * s.scale();
    let mut f = TextFormat {
        font_id: if s.code { theme::mono(13.0 * s.scale() * env.size / 14.0) } else if s.italic { theme::italic(size, weight(s.bold, base_weight)) } else { theme::font(size, weight(s.bold, base_weight)) },
        color: env.color,
        line_height: Some((env.size * env.lh).round()),
        ..Default::default()
    };
    if s.code {
        f.background = tk.bg;
    }
    if s.mark {
        f.background = theme::alpha(Color32::from_rgb(255, 214, 0), 0.45);
    }
    if s.sup {
        f.valign = Align::TOP;
    }
    if s.link.is_some() && doc.links.get(s.link.unwrap() as usize).is_some() {
        f.color = tk.accent;
        f.underline = Stroke::new(1.0, tk.accent);
    } else if s.underline {
        f.underline = Stroke::new(1.0, env.color);
    }
    if s.strike {
        f.strikethrough = Stroke::new(1.0, f.color);
    }
    f
}

/// Something drawn over a paragraph's text: an inline picture or formula.
enum Over {
    Img(ImgRef, egui::Vec2, Option<u32>),
    Math(Arc<crate::math::Laid>),
    Smiles(String),
}

/// Lay out a paragraph's runs; pictures and formulas get a gap of their width in the text.
fn para_job(app: &mut App, ui: &Ui, inl: &[Inl], env: &Env, width: f32, base_weight: u16, doc: &Doc, align: TAlign) -> (LayoutJob, Vec<(usize, Over)>) {
    let mut job = LayoutJob::default();
    job.wrap.max_width = width;
    job.halign = match align {
        TAlign::Left => Align::LEFT,
        TAlign::Center => Align::Center,
        TAlign::Right => Align::RIGHT,
    };
    let mut overs = Vec::new();
    let nbsp_w = ui.fonts_mut(|f| f.glyph_width(&theme::font(env.size, 400), '\u{a0}'));
    let lh = (env.size * env.lh).round();
    // A picture or formula is a gap in the text: two no-break spaces (never broken between) with
    // the gap as the second one's letter spacing, which egui puts *before* a glyph, so it always
    // lands between them, even when the first starts a line. The thing is drawn from the first.
    // (Each its own section: append would merge one into the text before it when their formats
    // match, and the thing would be drawn a gap's width to the right.)
    let gap = |job: &mut LayoutJob, f: egui::TextFormat, width: f32| {
        let mut g = f.clone();
        g.extra_letter_spacing = width - 2.0 * nbsp_w;
        for fmt in [f, g] {
            let start = job.text.len();
            job.text.push('\u{a0}');
            job.sections.push(egui::text::LayoutSection { leading_space: 0.0, byte_range: egui::text::ByteIndex(start)..egui::text::ByteIndex(job.text.len()), format: fmt });
        }
    };
    // Text's baseline sits this far below a line's top (Inter's ascent), and the rest below.
    let asc = (env.size * 0.97).round();
    let below = lh - asc;
    let mut has_math = false;
    for r in inl {
        match r {
            Inl::Text(text, s) => job.append(text, 0.0, format(env, s, base_weight, doc)),
            Inl::Br => job.append("\n", 0.0, format(env, &Sty::default(), base_weight, doc)),
            Inl::Img(im, s) => {
                let size = inline_img_size(app, ui, im, width);
                let mut f = format(env, &Sty::default(), base_weight, doc);
                f.line_height = Some(lh.max(size.y + 4.0));
                f.underline = Stroke::NONE;
                overs.push((job.sections.len(), Over::Img(im.clone(), size, s.link)));
                gap(&mut job, f, size.x);
            }
            Inl::Math(tex, _) => {
                let laid = app.math.lay(ui, tex, env.size * 1.21, env.color);
                let mut f = format(env, &Sty::default(), base_weight, doc);
                // lines with math are centered on the text, so a tall formula gets room above and
                // below it (on the text's baseline); this is the line height that makes room
                let need = (laid.height + 2.0 - asc).max(laid.depth + 2.0 - below).max(0.0);
                f.line_height = Some(lh + 2.0 * need);
                f.underline = Stroke::NONE;
                overs.push((job.sections.len(), Over::Math(laid.clone())));
                gap(&mut job, f, laid.width);
                has_math = true;
            }
            Inl::Smiles(smi) => {
                let mut f = format(env, &Sty::default(), base_weight, doc);
                f.line_height = Some(170.0 + 12.0);
                overs.push((job.sections.len(), Over::Smiles(smi.clone())));
                gap(&mut job, f, 240.0);
            }
        }
    }
    if has_math {
        for s in job.sections.iter_mut() {
            s.format.valign = Align::Center;
        }
    }
    (job, overs)
}

fn inline_img_size(app: &mut App, ui: &Ui, im: &ImgRef, max_w: f32) -> egui::Vec2 {
    let natural = im.src.as_ref().and_then(|s| app.image(ui.ctx(), s)).map(|(_, s)| s);
    let (w, h) = match (im.w, im.h, natural) {
        (Some(w), Some(h), _) => (w, h),
        (Some(w), None, Some(n)) => (w, w * n.y / n.x.max(1.0)),
        (None, Some(h), Some(n)) => (h * n.x / n.y.max(1.0), h),
        (None, None, Some(n)) => (n.x, n.y),
        (Some(w), None, None) => (w, 16.0),
        (None, Some(h), None) => (h, h),
        (None, None, None) => (16.0, 16.0),
    };
    if w > max_w {
        egui::vec2(max_w, h * max_w / w)
    } else {
        egui::vec2(w, h)
    }
}

/// Each glyph's section, row by row (one glyph per char; rows may end in an implicit newline).
fn glyph_sections(g: &Galley) -> Vec<Vec<usize>> {
    let mut starts: Vec<(usize, usize)> = Vec::new(); // (first char, section)
    for (i, s) in g.job.sections.iter().enumerate() {
        let c = g.job.text[..s.byte_range.start.0.min(g.job.text.len())].chars().count();
        starts.push((c, i));
    }
    let sec_of = |c: usize| starts.iter().rev().find(|(s, _)| *s <= c).map(|x| x.1).unwrap_or(0);
    let mut out = Vec::new();
    let mut c = 0;
    for row in &g.rows {
        let mut v = Vec::with_capacity(row.row.glyphs.len());
        for _ in &row.row.glyphs {
            v.push(sec_of(c));
            c += 1;
        }
        if row.ends_with_newline {
            c += 1;
        }
        out.push(v);
    }
    out
}

/// The section under a point in a galley (for links).
fn section_at(g: &Galley, p: Pos2) -> Option<usize> {
    let secs = glyph_sections(g);
    for (ri, row) in g.rows.iter().enumerate() {
        let r = row.rect().translate(row.pos.to_vec2());
        if p.y < r.min.y || p.y > r.max.y {
            continue;
        }
        for (gi, gl) in row.row.glyphs.iter().enumerate() {
            let x0 = gl.pos.x + row.pos.x;
            if p.x >= x0 && p.x <= x0 + gl.advance_width {
                return Some(secs[ri][gi]);
            }
        }
    }
    None
}

/// Where each placeholder (its first glyph's section) landed: (section, left x, row top, row
/// bottom, the text's baseline on that row).
fn placeholder_spots(g: &Galley) -> Vec<(usize, f32, f32, f32, f32)> {
    let mut out = Vec::new();
    let secs = glyph_sections(g);
    for (ri, row) in g.rows.iter().enumerate() {
        let top = row.pos.y;
        let bottom = row.pos.y + row.row.size.y;
        // the baseline of the row's text (a placeholder's own may differ: its line is taller)
        let text_base = row.row.glyphs.iter().find(|gl| gl.chr != '\u{a0}').or(row.row.glyphs.first()).map(|gl| top + gl.pos.y).unwrap_or(bottom);
        for (gi, gl) in row.row.glyphs.iter().enumerate() {
            if gl.chr == '\u{a0}' {
                out.push((secs[ri][gi], gl.pos.x + row.pos.x, top, bottom, text_base));
            }
        }
    }
    out
}

pub struct Ctx<'a> {
    pub doc: &'a Doc,
    pub id: Id,
    pub clicked: Option<String>,
    pub hovered: Option<String>,
    pub n: usize,
}

/// Draw a paragraph (or heading): selectable text, links, inline pictures and formulas.
fn para(app: &mut App, ui: &mut Ui, cx: &mut Ctx, inl: &[Inl], env: &Env, base_weight: u16, align: TAlign) {
    let width = ui.available_width();
    let (job, overs) = para_job(app, ui, inl, env, width, base_weight, cx.doc, align);
    let pad = ((env.size * env.lh).round() - env.size * 1.2109).max(0.0) / 2.0;
    let galley = ui.painter().layout_job(job);
    cx.n += 1;
    let size = vec2(width, galley.size().y);
    let (rect, resp) = ui.allocate_exact_size(size + vec2(0.0, 0.0), Sense::click());
    // a search hit being opened: this paragraph may be it, or be highlighted as it
    if let Some(a) = crate::search::para(app, env.pane, &galley.job.text, rect) {
        ui.painter().rect_filled(rect.expand2(vec2(6.0, 3.0)), crate::widgets::cr(4.0), crate::theme::alpha(t().accent, 0.22 * a));
    }
    let origin = rect.min + vec2(0.0, pad * 0.0);
    // text x offset for alignment: galley rect may start left of 0 for centered text
    let gpos = pos2(match align {
        TAlign::Left => rect.min.x,
        TAlign::Center => rect.center().x,
        TAlign::Right => rect.max.x,
    }, origin.y);
    ui.painter().galley(gpos + vec2(0.0, pad), galley.clone(), env.color);
    let galley_origin = gpos + vec2(0.0, pad);
    // Pictures and formulas in their gaps.
    let spots = placeholder_spots(&galley);
    for (sec, over) in &overs {
        let Some((_, x, top, bottom, base)) = spots.iter().find(|s| s.0 == *sec).copied() else { continue };
        match over {
            Over::Img(im, size, link) => {
                let r = Rect::from_min_size(galley_origin + vec2(x, bottom - 4.0 - size.y), *size);
                draw_img(app, ui, im, r);
                if let Some(l) = link {
                    let lr = ui.interact(r, cx.id.with(("il", cx.n, *sec)), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
                    if lr.clicked() {
                        cx.clicked = cx.doc.links.get(*l as usize).cloned();
                    }
                }
            }
            Over::Math(laid) => {
                crate::math::paint(ui, laid, pos2(galley_origin.x + x, galley_origin.y + base));
            }
            Over::Smiles(smi) => {
                let r = Rect::from_min_size(galley_origin + vec2(x, top + 6.0), vec2(240.0, 170.0));
                crate::smiles::show(app, ui, smi, r);
            }
        }
    }
    // Links: pointer over a linked run.
    if let Some(p) = resp.hover_pos() {
        if let Some(sec) = section_at(&galley, (p - galley_origin).to_pos2()) {
            if let Some(Inl::Text(_, s)) = run_of_section(inl, sec) {
                if let Some(l) = s.link {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    let href = cx.doc.links.get(l as usize).cloned();
                    if resp.clicked() {
                        cx.clicked = href.clone();
                    }
                    cx.hovered = href;
                }
            }
        }
    }
}

/// The run that produced a section (runs map 1:1 to sections).
fn run_of_section(inl: &[Inl], sec: usize) -> Option<&Inl> {
    inl.get(sec)
}

fn draw_img(app: &mut App, ui: &Ui, im: &ImgRef, r: Rect) {
    let tk = t();
    match im.src.as_ref().and_then(|s| app.image(ui.ctx(), s)) {
        Some((tex, _)) => {
            ui.painter().image(tex, r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        None => {
            if im.src.as_ref().map(|s| app.image_failed(s)).unwrap_or(true) {
                ui.painter().rect_stroke(r, cr(2.0), Stroke::new(1.0, tk.line), StrokeKind::Inside);
                if !im.alt.is_empty() && r.width() > 30.0 {
                    crate::widgets::centered_text(ui, r, &im.alt, crate::widgets::Ts::new(12.0, 400, tk.faint));
                }
            }
        }
    }
}

fn heading_style(level: u8, env: &Env) -> (f32, u16, f32, f32) {
    // (size, weight, margin-top em, margin-bottom em) per the stylesheet (h1 24/650; .content h2 18/600)
    match level {
        1 => (24.0 * env.size / 14.0, 650, 1.2, 0.5),
        2 => (18.0 * env.size / 14.0, 600, 1.2, 0.5),
        3 => (env.size * 1.17, 700, 1.2, 0.5),
        4 => (env.size, 700, 1.33, 1.33),
        5 => (env.size * 0.83, 700, 1.67, 1.67),
        _ => (env.size * 0.67, 700, 2.33, 2.33),
    }
}

/// A block's (margin-top, margin-bottom) in px, for collapsing between siblings.
fn margins(b: &Block, env: &Env) -> (f32, f32) {
    match b {
        Block::Para { .. } => (0.0, 0.0),
        Block::Heading { level, .. } => {
            let (s, _, t, bm) = heading_style(*level, env);
            (t * s, bm * s)
        }
        Block::List { .. } => (env.size, env.size),
        Block::Quote(_) => (env.size, env.size),
        Block::Pre(_) => (env.size, env.size),
        Block::Table(_) => (12.0, 12.0),
        Block::Hr { answer } => if *answer { (22.0, 22.0) } else { (7.0, 7.0) },
        Block::Div { margin, blocks, .. } => {
            let (inner_t, inner_b) = (blocks.first().map(|b| margins(b, env).0).unwrap_or(0.0), blocks.last().map(|b| margins(b, env).1).unwrap_or(0.0));
            ((margin.0 * env.size).max(inner_t), (margin.1 * env.size).max(inner_b))
        }
        Block::Embed { .. } => (8.0, 8.0),
        Block::Math(_) => (8.0, 8.0),
        _ => (0.0, 0.0),
    }
}

/// Draw blocks top to bottom, collapsing margins between siblings.
pub fn blocks(app: &mut App, ui: &mut Ui, cx: &mut Ctx, list: &[Block], env: &Env, first_top: bool, last_bottom: bool) {
    let mut prev_bottom: Option<f32> = None;
    for (i, b) in list.iter().enumerate() {
        let (mt, mb) = margins(b, env);
        let gap = match prev_bottom {
            None => if first_top { mt } else { 0.0 },
            Some(pb) => pb.max(mt),
        };
        ui.add_space(gap);
        block(app, ui, cx, b, env);
        prev_bottom = Some(mb);
        if i == list.len() - 1 && last_bottom {
            ui.add_space(mb);
        }
    }
}

fn block(app: &mut App, ui: &mut Ui, cx: &mut Ctx, b: &Block, env: &Env) {
    let tk = t();
    match b {
        Block::Para { inl, align } => para(app, ui, cx, inl, env, env.weight, align.unwrap_or(env.align)),
        Block::Heading { level, inl, align } => {
            let (size, w, _, _) = heading_style(*level, env);
            let e2 = Env { size, lh: if *level == 1 { 1.5 } else { 1.5 }, ..*env };
            para(app, ui, cx, inl, &e2, w, align.unwrap_or(env.align));
        }
        Block::List { ordered, start, items } => {
            let depth = ui.data(|d| d.get_temp::<usize>(cx.id.with("list-depth"))).unwrap_or(0);
            ui.data_mut(|d| d.insert_temp(cx.id.with("list-depth"), depth + 1));
            for (i, item) in items.iter().enumerate() {
                let top = ui.cursor().min.y;
                let x0 = ui.cursor().min.x;
                let w = ui.available_width();
                let mut c = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(pos2(x0 + 40.0, top), vec2((w - 40.0).max(20.0), f32::INFINITY))).layout(egui::Layout::top_down(egui::Align::Min)));
                blocks(app, &mut c, cx, item, env, false, false);
                let h = c.min_rect().height().max((env.size * env.lh).round());
                // the marker, on the first line
                let line = (env.size * env.lh).round();
                let mid = top + line / 2.0;
                if *ordered {
                    let label = format!("{}.", start + i as i64);
                    let g = crate::widgets::lay(ui, &label, crate::widgets::Ts::new(env.size, 400, env.color), None, false);
                    ui.painter().galley(pos2(x0 + 40.0 - 6.0 - g.size().x, mid - g.size().y / 2.0), g, env.color);
                } else {
                    let c = pos2(x0 + 40.0 - 14.0, mid);
                    let r = env.size * 0.18;
                    match depth {
                        0 => { ui.painter().circle_filled(c, r, env.color); }
                        1 => { ui.painter().circle_stroke(c, r, Stroke::new(1.0, env.color)); }
                        _ => { ui.painter().rect_filled(Rect::from_center_size(c, vec2(r * 1.8, r * 1.8)), 0.0, env.color); }
                    }
                }
                ui.allocate_space(vec2(w, h));
            }
            ui.data_mut(|d| d.insert_temp(cx.id.with("list-depth"), depth));
        }
        Block::Quote(inner) => {
            let top = ui.cursor().min.y;
            let x0 = ui.cursor().min.x;
            let w = ui.available_width();
            let mut c = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(pos2(x0 + 17.0, top), vec2(w - 17.0, f32::INFINITY))).layout(egui::Layout::top_down(egui::Align::Min)));
            let e2 = Env { color: tk.muted, ..*env };
            blocks(app, &mut c, cx, inner, &e2, false, false);
            let h = c.min_rect().height();
            ui.painter().rect_filled(Rect::from_min_size(pos2(x0, top), vec2(3.0, h)), 0.0, tk.line);
            ui.allocate_space(vec2(w, h));
        }
        Block::Pre(text) => {
            let ts = crate::widgets::Ts::new(13.0, 400, env.color).mono().lh(1.5);
            let g = crate::widgets::lay(ui, text, ts, None, false);
            let w = ui.available_width();
            let h = g.size().y + 20.0;
            let (rect, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
            ui.painter().rect_filled(rect, cr(4.0), tk.bg);
            // overflow-x: auto → scroll sideways when wider
            let inner = rect.shrink2(vec2(12.0, 10.0));
            if g.size().x > inner.width() {
                let mut c = ui.new_child(egui::UiBuilder::new().max_rect(inner));
                egui::ScrollArea::horizontal().id_salt(cx.id.with(("pre", cx.n))).show(&mut c, |ui| {
                    let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                    ui.painter().galley(r.min, g.clone(), env.color);
                });
            } else {
                ui.painter().galley(inner.min, g, env.color);
            }
            cx.n += 1;
        }
        Block::Hr { answer } => {
            let w = ui.available_width();
            let (r, _) = ui.allocate_exact_size(vec2(w, if *answer { 1.0 } else { 2.0 }), Sense::hover());
            ui.painter().hline(r.x_range(), r.min.y + 0.5, Stroke::new(1.0, tk.line));
        }
        Block::Image(im, link, align) => {
            let w = ui.available_width();
            let size = inline_img_size(app, ui, im, w);
            let x = match align.unwrap_or(env.align) {
                TAlign::Left => ui.cursor().min.x,
                TAlign::Center => ui.cursor().min.x + (w - size.x) / 2.0,
                TAlign::Right => ui.cursor().min.x + w - size.x,
            };
            let (row, _) = ui.allocate_exact_size(vec2(w, size.y), Sense::hover());
            let r = Rect::from_min_size(pos2(x, row.min.y), size);
            draw_img(app, ui, im, r);
            if let Some(l) = link {
                let resp = ui.interact(r, cx.id.with(("img", cx.n)), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
                if resp.clicked() {
                    cx.clicked = cx.doc.links.get(*l as usize).cloned();
                }
            }
            cx.n += 1;
        }
        Block::Embed { url, label } => {
            let resp = crate::widgets::linklike(ui, label, env.size);
            if resp.clicked() {
                cx.clicked = Some(url.clone());
            }
        }
        Block::Hint { label, blocks: inner, key } => {
            let open = app.html.hints.contains(key);
            if !open {
                let resp = crate::widgets::linklike(ui, if label.is_empty() { "Show Hint" } else { label }, env.size);
                if resp.clicked() {
                    app.html.hints.insert(key.clone());
                }
            } else {
                blocks(app, ui, cx, inner, env, false, false);
            }
        }
        Block::Div { blocks: inner, align, indent, .. } => {
            let e2 = Env { align: align.unwrap_or(env.align), ..*env };
            if *indent > 0.0 {
                let w = ui.available_width();
                let top = ui.cursor().min;
                let mut c = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(top + vec2(*indent, 0.0), vec2(w - indent, f32::INFINITY))).layout(egui::Layout::top_down(egui::Align::Min)));
                blocks(app, &mut c, cx, inner, &e2, false, false);
                let h = c.min_rect().height();
                ui.allocate_space(vec2(w, h));
            } else {
                blocks(app, ui, cx, inner, &e2, false, false);
            }
        }
        Block::Table(rows) => table(app, ui, cx, rows, env),
        Block::Math(tex) => {
            let laid = app.math.lay(ui, &format!("\\displaystyle {tex}"), env.size * 1.21, env.color);
            let w = ui.available_width();
            let (r, _) = ui.allocate_exact_size(vec2(w, laid.height + laid.depth), Sense::hover());
            crate::math::paint(ui, &laid, pos2(r.center().x - laid.width / 2.0, r.min.y + laid.height));
        }
    }
}

/// Natural (no-wrap) and minimum (longest word) widths of some blocks, roughly.
fn widths(ui: &Ui, list: &[Block], env: &Env) -> (f32, f32) {
    let mut max_w: f32 = 0.0;
    let mut min_w: f32 = 0.0;
    let font = theme::font(env.size, 400);
    let bold = theme::font(env.size, 700);
    let measure = |s: &str, f: &egui::FontId| ui.fonts_mut(|fo| fo.layout_no_wrap(s.to_string(), f.clone(), Color32::WHITE).size().x);
    for b in list {
        match b {
            Block::Para { inl, .. } | Block::Heading { inl, .. } => {
                let mut line = 0.0;
                for r in inl {
                    match r {
                        Inl::Text(t, s) => {
                            let f = if s.bold { &bold } else { &font };
                            line += measure(t, f);
                            for w in t.split_whitespace() {
                                min_w = min_w.max(measure(w, f));
                            }
                        }
                        Inl::Br => {
                            max_w = max_w.max(line);
                            line = 0.0;
                        }
                        Inl::Img(im, _) => {
                            let w = im.w.unwrap_or(40.0);
                            line += w;
                            min_w = min_w.max(w.min(200.0));
                        }
                        Inl::Math(tex, _) => line += tex.len() as f32 * env.size * 0.5,
                        Inl::Smiles(_) => line += 240.0,
                    }
                }
                max_w = max_w.max(line);
            }
            Block::Div { blocks, indent, .. } => {
                let (a, b) = widths(ui, blocks, env);
                max_w = max_w.max(a + indent);
                min_w = min_w.max(b + indent);
            }
            Block::List { items, .. } => {
                for it in items {
                    let (a, b) = widths(ui, it, env);
                    max_w = max_w.max(a + 40.0);
                    min_w = min_w.max(b + 40.0);
                }
            }
            Block::Pre(t) => {
                let w = t.lines().map(|l| measure(l, &theme::mono(13.0))).fold(0.0, f32::max) + 24.0;
                max_w = max_w.max(w);
                min_w = min_w.max(w.min(300.0));
            }
            Block::Image(im, ..) => {
                let w = im.w.unwrap_or(200.0);
                max_w = max_w.max(w);
                min_w = min_w.max(w.min(200.0));
            }
            _ => {
                max_w = max_w.max(100.0);
                min_w = min_w.max(40.0);
            }
        }
    }
    (max_w, min_w)
}

fn table(app: &mut App, ui: &mut Ui, cx: &mut Ctx, rows: &[Vec<Cell>], env: &Env) {
    let tk = t();
    let ncols = rows.iter().map(|r| r.iter().map(|c| c.colspan).sum::<usize>()).max().unwrap_or(1);
    let mut maxw = vec![0.0f32; ncols];
    let mut minw = vec![0.0f32; ncols];
    for r in rows {
        let mut col = 0;
        for c in r {
            if c.colspan == 1 && col < ncols {
                let (a, b) = widths(ui, &c.blocks, &Env { size: env.size, ..*env });
                maxw[col] = maxw[col].max(a + 18.0 + 1.0);
                minw[col] = minw[col].max(b + 18.0 + 1.0);
            }
            col += c.colspan;
        }
    }
    let avail = ui.available_width();
    let sum_max: f32 = maxw.iter().sum();
    let sum_min: f32 = minw.iter().sum();
    let cols: Vec<f32> = if sum_max <= avail {
        maxw.clone()
    } else if sum_min >= avail {
        minw.clone()
    } else {
        let extra = avail - sum_min;
        let spread: f32 = maxw.iter().zip(&minw).map(|(a, b)| a - b).sum::<f32>().max(1e-3);
        minw.iter().zip(&maxw).map(|(mn, mx)| mn + extra * (mx - mn) / spread).collect()
    };
    let total_w: f32 = cols.iter().sum::<f32>() + 1.0;
    let draw = |app: &mut App, ui: &mut Ui, cx: &mut Ctx| {
        let origin = ui.cursor().min;
        let mut y = origin.y;
        for r in rows {
            // lay out each cell, then the row is as tall as its tallest
            let mut col = 0;
            let mut x = origin.x;
            let mut h: f32 = 0.0;
            let mut cells = Vec::new();
            for c in r {
                let w: f32 = cols[col.min(ncols - 1)..(col + c.colspan).min(ncols)].iter().sum();
                let mut cu = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(pos2(x + 9.5, y + 5.5), vec2((w - 19.0).max(4.0), f32::INFINITY))).layout(egui::Layout::top_down(egui::Align::Min)));
                let e2 = Env { align: c.align.unwrap_or(TAlign::Left), ..*env };
                blocks(app, &mut cu, cx, &c.blocks, &e2, false, false);
                h = h.max(cu.min_rect().height() + 11.0);
                cells.push((x, w));
                x += w;
                col += c.colspan;
            }
            for (cx0, w) in cells {
                ui.painter().rect_stroke(Rect::from_min_size(pos2(cx0, y), vec2(w + 1.0, h + 1.0)), 0.0, Stroke::new(1.0, tk.line), StrokeKind::Inside);
            }
            y += h;
        }
        ui.allocate_rect(Rect::from_min_max(origin, pos2(origin.x + total_w, y + 1.0)), Sense::hover());
    };
    if total_w > avail + 1.0 {
        let id = cx.id.with(("table", cx.n));
        cx.n += 1;
        egui::ScrollArea::horizontal().id_salt(id).show(ui, |ui| draw(app, ui, cx));
    } else {
        draw(app, ui, cx);
    }
}

/// Canvas content in its box (.content: panel, 1px border, radius 8, padding 22px 26px).
/// Returns a link that was clicked, if any.
pub fn content(app: &mut App, ui: &mut Ui, html: &str, pane: Pane) {
    let base = app.base();
    let doc = app.html.doc(html, Flavor::Canvas, &base);
    let id = Id::new(("content", hash(html)));
    let mut cx = Ctx { doc: &doc, id, clicked: None, hovered: None, n: 0 };
    crate::widgets::boxed(ui, egui::Margin { left: 26, right: 26, top: 22, bottom: 22 }, 8.0, |ui| {
        let env = Env::content(pane);
        blocks(app, ui, &mut cx, &doc.blocks, &env, false, true);
    });
    handle(app, cx, pane);
}

/// Content without the box (a discussion reply inside .comment, an Anki card).
pub fn bare(app: &mut App, ui: &mut Ui, html: &str, flavor: Flavor, env: Env) {
    let base = app.base();
    let doc = app.html.doc(html, flavor, &base);
    let mut env = env;
    if flavor == Flavor::Anki {
        if let Some(s) = doc.card_size {
            env.size = s;
        }
        if let Some(a) = doc.card_align {
            env.align = a;
        }
    }
    let id = Id::new(("bare", hash(html)));
    let mut cx = Ctx { doc: &doc, id, clicked: None, hovered: None, n: 0 };
    blocks(app, ui, &mut cx, &doc.blocks, &env, false, false);
    let pane = env.pane;
    handle(app, cx, pane);
}

fn handle(app: &mut App, cx: Ctx, pane: Pane) {
    if let Some(h) = &cx.hovered {
        if h.starts_with("#/") {
            crate::nav::prefetch(app, h);
        }
    }
    if let Some(h) = cx.clicked {
        crate::nav::follow(app, &h, pane);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://canvas.example.edu";

    #[test]
    fn app_links() {
        assert_eq!(app_link_for("/courses/101/assignments/5002", BASE).unwrap(), "#/c/101/a/5002");
        assert_eq!(app_link_for("https://canvas.example.edu/courses/101/pages/syllabus-notes", BASE).unwrap(), "#/c/101/p/syllabus-notes");
        assert_eq!(app_link_for("/courses/101/files/9003", BASE).unwrap(), "#/c/101/f/9003");
        assert_eq!(app_link_for("/files/9/download", BASE).unwrap(), "#/f/9");
        assert_eq!(app_link_for("/courses/101/discussion_topics", BASE).unwrap(), "#/c/101/discussions");
        assert_eq!(app_link_for("/courses/101", BASE).unwrap(), "#/c/101/modules");
        assert!(app_link_for("https://docs.python.org", BASE).is_none());
    }

    #[test]
    fn parses_demo_page() {
        let html = "<h2>Welcome</h2><p>Office hours are <span style=\"color:#000000\">Mon 2–3pm</span>.</p><ul><li>Project spec: <a href=\"/courses/101/assignments/5002\">P2</a></li><li><a href=\"https://docs.python.org\">Python docs</a></li></ul><iframe src=\"https://www.youtube-nocookie.com/embed/x\"></iframe><script>alert(1)</script><img src=x onerror=alert(1)>";
        let d = parse(html, Flavor::Canvas, BASE);
        assert!(matches!(d.blocks[0], Block::Heading { level: 2, .. }));
        assert!(d.links.contains(&"#/c/101/a/5002".to_string()));
        assert!(d.links.iter().any(|l| l.starts_with("https://docs.python.org")), "{:?}", d.links);
        assert!(d.blocks.iter().any(|b| matches!(b, Block::Embed { .. })));
        assert!(!d.blocks.iter().any(|b| matches!(b, Block::Para { inl, .. } if inl.iter().any(|r| matches!(r, Inl::Text(t, _) if t.contains("alert"))))));
    }

    #[test]
    fn anki_hint_and_math() {
        let html = r##"Q \(x^2\)<div class="hint"><a class=hint href="#" onclick="this.style.display='none';document.getElementById('hint1').style.display='block';return false;">Show Hint</a><div id="hint1" class=hint style="display: none"><img src="pdfcp_a.png"></div></div>"##;
        let d = parse(html, Flavor::Anki, BASE);
        let has_hint = |bs: &[Block]| bs.iter().any(|b| matches!(b, Block::Hint { .. }) || matches!(b, Block::Div { blocks, .. } if blocks.iter().any(|x| matches!(x, Block::Hint { .. }))));
        assert!(has_hint(&d.blocks));
        assert!(matches!(&d.blocks[0], Block::Para { inl, .. } if inl.iter().any(|r| matches!(r, Inl::Math(..)))));
        assert_eq!(text_of("<p>A <b>b</b>\n c</p>"), "A b c");
    }

    #[test]
    fn math_split() {
        let s = split_math(r"Aspirin \(\ce{C9H8O4}\). It's: \[a\] \smiles{OC(=O)C}");
        assert_eq!(s.iter().map(|x| x.0).collect::<Vec<_>>(), vec![0, 1, 0, 2, 0, 3]);
    }
}


