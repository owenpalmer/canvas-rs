//! The search palette (Ctrl K or /): everything in the cache by name, recent pages, and commands.

use std::sync::Arc;

use egui::{Color32, Context, CursorIcon, Id, Key, Modifiers, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use serde_json::Value;

use crate::app::{App, Pane};
use crate::theme::t;
use crate::widgets::{Rich, Ts, cr, lay, paint_kbd};

#[derive(Clone, Debug)]
pub struct PItem {
    pub k: String,
    pub t: String,
    pub c: String,
    pub h: Option<String>,
    pub cmd: Option<usize>,
    /// a content hit: its snippet (matches between \u{1} and \u{2}) and where it goes
    pub snippet: Option<String>,
    pub hit: Option<Value>,
}

#[derive(Default)]
pub struct Palette {
    pub open: bool,
    pub query: String,
    pub sel: usize,
    pub items: Vec<PItem>,
    last_query: Option<String>,
    loading: bool,
    scroll_sel: bool,
    focus: bool,
    trace: Option<crate::debug::Trace>,
}

const COMMANDS: &[(&str, &str)] = &[
    ("Settings", "g s"),
    ("Toggle theme", "t"),
    ("Edit courses: reorder or hide", ""),
    ("Keyboard shortcuts", "?"),
    ("Hide or show the sidebar", ""),
    ("Hide or show the viewer", ""),
    ("Close the viewer tab", "x"),
    ("Sync now", ""),
    ("Refresh this page", "r"),
    ("Show or hide load times (debug)", ""),
    ("Back", "Alt ←"),
    ("Forward", "Alt →"),
];

fn run_command(app: &mut App, i: usize) {
    match i {
        0 => crate::nav::go(app, "#/settings"),
        1 => crate::nav::toggle_theme(app),
        2 => crate::sidebar::set_editing(app, true),
        3 => app.keys_open = true,
        4 => crate::panes::toggle_pane(app, Pane::Sidebar),
        5 => crate::panes::toggle_pane(app, Pane::Viewer),
        6 => crate::panes::close_tab(app, None),
        7 => crate::sidebar::sync_now(app),
        8 => {
            let deps: Vec<String> = app.main.deps.iter().cloned().collect();
            app.d.revalidate(deps.iter(), true);
        }
        9 => {
            let on = !app.debug.on;
            crate::debug::set_on(app, on);
        }
        10 => crate::nav::back(app),
        11 => crate::nav::forward(app),
        _ => {}
    }
}

fn kind_order(k: &str) -> f32 {
    match k {
        "assignment" => 1.0,
        "page" => 2.0,
        "announcement" => 3.0,
        "discussion" => 4.0,
        "module" => 5.0,
        "file" => 6.0,
        "message" => 7.0,
        _ => 0.0,
    }
}

fn score(it: &PItem, tokens: &[String]) -> f32 {
    let t = it.t.to_lowercase();
    let all = format!("{t} {} {}", it.c.to_lowercase(), it.k);
    let mut s = 0.0;
    for tok in tokens {
        if let Some(i) = t.find(tok.as_str()) {
            let prev = t[..i].chars().last();
            s += if i == 0 || prev.map(|c| !c.is_alphanumeric() && c != '_').unwrap_or(true) { 0.0 } else { 2.0 };
        } else if all.contains(tok.as_str()) {
            s += 5.0;
        } else {
            return -1.0;
        }
    }
    s + kind_order(&it.k) * 0.3 + t.chars().count() as f32 / 200.0
}

pub fn open(app: &mut App) {
    app.palette.open = true;
    app.palette.query.clear();
    app.palette.last_query = None;
    app.palette.focus = true;
    app.palette.trace = crate::debug::start(app, "search", "Search palette", "index");
    if app.search_index.is_none() && !app.palette.loading {
        app.palette.loading = true;
        let svc = app.svc.clone();
        app.spawn(async move { tokio::task::spawn_blocking(move || svc.search_index()).await.unwrap_or_default() }, |app, idx| {
            app.search_index = Some(Arc::new(idx));
            app.palette.loading = false;
            app.palette.last_query = None;
        });
    }
}

pub fn close(app: &mut App) {
    app.palette.open = false;
}

impl Palette {
    /// Recompute the results (new content hits arrived).
    pub fn refresh(&mut self) {
        self.last_query = None;
    }
}

/// A content hit's kind, as the palette shows it.
fn hit_kind(k: &str) -> &str {
    match k {
        "pdf" => "in PDF",
        "transcript" => "transcript",
        "page" => "in page",
        "assignment" => "in assignment",
        "announcement" => "in announcement",
        "discussion" => "in discussion",
        "syllabus" => "in syllabus",
        "message" => "in message",
        k => k,
    }
}

fn item(v: &Value, k: Option<&str>) -> PItem {
    PItem {
        k: k.map(String::from).unwrap_or_else(|| crate::fmt::s(&v["k"])),
        t: crate::fmt::s(&v["t"]),
        c: crate::fmt::s(&v["c"]),
        h: v["h"].as_str().map(String::from),
        cmd: None,
        snippet: None,
        hit: None,
    }
}

fn update(app: &mut App) {
    let raw = app.palette.query.clone();
    crate::search::want(app, &raw);
    let q = app.palette.query.trim().to_lowercase();
    if app.palette.last_query.as_deref() == Some(q.as_str()) {
        return;
    }
    app.palette.last_query = Some(q.clone());
    let tokens: Vec<String> = q.split_whitespace().map(String::from).collect();
    let idx = app.search_index.clone().unwrap_or_default();
    let here = app.main.want.clone();
    let items: Vec<PItem> = if tokens.is_empty() {
        // recent pages, then the courses in sidebar order
        let mut out: Vec<PItem> = crate::nav::recent(app).iter().filter(|r| r["h"] != here.as_str()).take(6).map(|r| item(r, Some("recent"))).collect();
        let by_hash: std::collections::HashMap<String, PItem> = idx.iter().filter(|x| x["k"] == "course").map(|x| (crate::fmt::s(&x["h"]), item(x, None))).collect();
        match app.d.peek("courses").and_then(|c| c.as_array().cloned()) {
            Some(courses) => {
                for c in crate::sidebar::ordered_courses(app, &courses, false) {
                    let h = format!("#/c/{}/modules", crate::fmt::id(&c["id"]));
                    let short = format!("#/c/{}", crate::fmt::id(&c["id"]));
                    out.push(by_hash.get(&short).or(by_hash.get(&h)).cloned().unwrap_or(PItem { k: "course".into(), t: crate::fmt::s(&c["name"]), c: crate::fmt::s(&c["course_code"]), h: Some(h), cmd: None, snippet: None, hit: None }));
                }
            }
            None => out.extend(by_hash.into_values()),
        }
        out
    } else {
        let mut scored: Vec<(f32, PItem)> = COMMANDS
            .iter()
            .enumerate()
            .map(|(i, (t, c))| PItem { k: "command".into(), t: t.to_string(), c: c.to_string(), h: None, cmd: Some(i), snippet: None, hit: None })
            .chain(idx.iter().map(|x| item(x, None)))
            .map(|it| (score(&it, &tokens), it))
            .filter(|(s, _)| *s >= 0.0)
            .collect();
        scored.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut out: Vec<PItem> = scored.into_iter().take(30).map(|(_, i)| i).collect();
        // then what's inside documents, transcripts and PDFs
        for h in crate::search::hits_for(app, &raw) {
            out.push(PItem {
                k: hit_kind(h["k"].as_str().unwrap_or("")).to_string(),
                t: crate::fmt::s(&h["t"]),
                c: crate::fmt::s(&h["c"]),
                h: h["h"].as_str().map(String::from),
                cmd: None,
                snippet: h["s"].as_str().map(String::from),
                hit: Some(h),
            });
        }
        out
    };
    app.palette.items = items;
    app.palette.sel = 0;
    if let Some(tr) = app.palette.trace.take() {
        if app.search_index.is_some() {
            crate::debug::end(app, tr, "Search palette");
        } else {
            app.palette.trace = Some(tr);
        }
    }
}

fn move_sel(app: &mut App, d: i64) {
    let n = app.palette.items.len() as i64;
    if n == 0 {
        return;
    }
    app.palette.sel = ((app.palette.sel as i64 + d + n) % n) as usize;
    app.palette.scroll_sel = true;
    if let Some(h) = app.palette.items[app.palette.sel].h.clone() {
        crate::nav::prefetch(app, &h);
    }
}

fn choose(app: &mut App, i: usize) {
    let Some(it) = app.palette.items.get(i).cloned() else { return };
    close(app);
    if let Some(h) = &it.hit {
        crate::search::open_hit(app, h);
    } else if let Some(c) = it.cmd {
        run_command(app, c);
    } else if let Some(h) = it.h {
        crate::nav::go(app, &h);
    }
}

/// Keys while the palette is open. Returns true when used.
pub fn key(app: &mut App, key: Key, m: &Modifiers) -> bool {
    match key {
        Key::Escape => close(app),
        Key::ArrowDown => move_sel(app, 1),
        Key::ArrowUp => move_sel(app, -1),
        Key::N if m.ctrl => move_sel(app, 1),
        Key::P if m.ctrl => move_sel(app, -1),
        Key::Enter => {
            let s = app.palette.sel;
            choose(app, s);
        }
        _ => return false,
    }
    true
}

/// A content hit's snippet: the matches (between \u{1} and \u{2}) marked.
fn snippet(s: &str) -> Rich {
    let tk = t();
    let mut rich = Rich::new();
    let mut on = false;
    for part in s.split(['\u{1}', '\u{2}']) {
        if !part.is_empty() {
            rich.push(part, if on { Ts::new(12.5, 600, tk.text) } else { Ts::new(12.5, 400, tk.muted) });
        }
        on = !on;
    }
    rich
}

/// The text with each search word marked (mark: accent, 600).
fn highlighted(text: &str, tokens: &[String]) -> Rich {
    let tk = t();
    let lower = text.to_lowercase();
    let mut marks = vec![false; text.len()];
    for tok in tokens.iter().filter(|t| !t.is_empty()) {
        let mut start = 0;
        while let Some(i) = lower[start..].find(tok.as_str()) {
            let a = start + i;
            for m in marks.iter_mut().skip(a).take(tok.len()) {
                *m = true;
            }
            start = a + tok.len().max(1);
            if start >= lower.len() {
                break;
            }
        }
    }
    let mut rich = Rich::new();
    let mut run = String::new();
    let mut cur = false;
    for (i, ch) in text.char_indices() {
        let m = marks.get(i).copied().unwrap_or(false) && lower.len() == text.len();
        if m != cur && !run.is_empty() {
            rich.push(&run, if cur { Ts::new(14.0, 600, tk.accent) } else { Ts::new(14.0, 400, tk.text) });
            run.clear();
        }
        cur = m;
        run.push(ch);
    }
    if !run.is_empty() {
        rich.push(&run, if cur { Ts::new(14.0, 600, tk.accent) } else { Ts::new(14.0, 400, tk.text) });
    }
    rich
}

pub fn draw(app: &mut App, ctx: &Context) {
    if !app.palette.open {
        return;
    }
    update(app);
    crate::search::tick_palette(app);
    let tk = t();
    let screen = ctx.content_rect();
    let dim = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, Id::new("palette-dim")));
    dim.rect_filled(screen, 0.0, Color32::from_black_alpha(64));
    let w = (screen.width() * 0.92).min(640.0);
    let top = screen.min.y + screen.height() * 0.12;
    let tokens: Vec<String> = app.palette.query.trim().to_lowercase().split_whitespace().map(String::from).collect();
    let mut chosen: Option<usize> = None;
    let area = egui::Area::new(Id::new("palette")).order(egui::Order::Foreground).fixed_pos(pos2(screen.center().x - w / 2.0, top)).show(ctx, |ui| {
        ui.set_width(w);
        let bg = ui.painter().add(egui::Shape::Noop);
        let start = ui.cursor().min;
        // the input: 16px, padding 14px 16px, border-bottom
        let id = Id::new("palette-input");
        let (ir, _) = ui.allocate_exact_size(vec2(w, 24.0 + 28.0), Sense::hover());
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(ir.shrink2(vec2(16.0, 14.0))));
        let font = crate::theme::font(16.0, 400);
        let resp = c.add(
            egui::TextEdit::singleline(&mut app.palette.query)
                .id(id)
                .frame(egui::Frame::NONE)
                .font(font.clone())
                .text_color(tk.text)
                .desired_width(w - 32.0)
                .margin(egui::Margin::ZERO)
                .hint_text(egui::RichText::new("Search everything: titles, pages, PDFs, transcripts…").color(tk.muted).font(font)),
        );
        if std::mem::take(&mut app.palette.focus) || !resp.has_focus() {
            resp.request_focus();
        }
        ui.painter().hline(ir.x_range(), ir.max.y - 0.5, Stroke::new(1.0, tk.line));
        // results: max 50vh, padding 6
        let max_h = screen.height() * 0.5;
        let n = app.palette.items.len();
        egui::ScrollArea::vertical().max_height(max_h).id_salt("palette-results").show(ui, |ui| {
            ui.add_space(6.0);
            if n == 0 {
                let (r, _) = ui.allocate_exact_size(vec2(w, 35.0), Sense::hover());
                crate::widgets::painter_text(ui, pos2(r.min.x + 16.0, r.center().y), egui::Align2::LEFT_CENTER, if app.palette.loading { "Loading…" } else { "No matches" }, Ts::new(14.0, 400, tk.faint));
            }
            for i in 0..n {
                let it = app.palette.items[i].clone();
                let tall = it.snippet.is_some();
                let (r, resp) = ui.allocate_exact_size(vec2(w, if tall { 54.0 } else { 35.0 }), Sense::click());
                let row = r.shrink2(vec2(6.0, 0.0));
                // a content hit: the title line on top, the snippet under it
                let line = if tall { Rect::from_min_size(row.min, vec2(row.width(), 32.0)) } else { row };
                if i == app.palette.sel {
                    ui.painter().rect_filled(row, cr(6.0), tk.hover);
                    if app.palette.scroll_sel {
                        ui.scroll_to_rect(r, None);
                    }
                }
                let x0 = row.min.x + 10.0;
                let k = lay(ui, &it.k, Ts::new(11.0, 400, tk.faint), Some(82.0), true);
                ui.painter().galley(pos2(x0, line.center().y - k.size().y / 2.0), k, tk.faint);
                let cg = lay(ui, &it.c, Ts::new(12.0, 400, tk.muted), Some(w * 0.4), true);
                let cw = cg.size().x;
                ui.painter().galley(pos2(row.max.x - 10.0 - cw, line.center().y - cg.size().y / 2.0), cg, tk.muted);
                let tw = row.max.x - 10.0 - cw - 10.0 - (x0 + 92.0);
                let tg = highlighted(&it.t, &tokens).elide(tw.max(20.0)).lay(ui);
                ui.painter().galley(pos2(x0 + 92.0, line.center().y - tg.size().y / 2.0), tg, tk.text);
                if let Some(sn) = &it.snippet {
                    let g = snippet(sn).elide(row.max.x - 10.0 - (x0 + 92.0)).lay(ui);
                    ui.painter().galley(pos2(x0 + 92.0, line.max.y - 4.0), g, tk.muted);
                }
                if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    chosen = Some(i);
                }
            }
            ui.add_space(6.0);
        });
        app.palette.scroll_sel = false;
        // .palette-hint
        let (hr, _) = ui.allocate_exact_size(vec2(w, 34.0), Sense::hover());
        ui.painter().hline(hr.x_range(), hr.min.y + 0.5, Stroke::new(1.0, tk.line));
        let mut x = hr.min.x + 14.0;
        let y = hr.center().y;
        for (keys, label) in [(&["↑", "↓"][..], "move"), (&["Enter"][..], "open"), (&["Esc"][..], "close")] {
            for k in keys {
                let r = paint_kbd(ui, pos2(x, y - 9.0), k, 11.0, None, None, None);
                x = r.max.x + 6.0;
            }
            let g = lay(ui, label, Ts::new(12.0, 400, tk.faint), None, false);
            let gw = g.size().x;
            ui.painter().galley(pos2(x, y - g.size().y / 2.0), g, tk.faint);
            x += gw + 6.0;
        }
        let rect = Rect::from_min_max(start, pos2(start.x + w, ui.cursor().min.y));
        let shadow = egui::Shadow { offset: [0, 20], blur: 60, spread: 0, color: Color32::from_black_alpha(77) };
        ui.painter().set(bg, egui::Shape::Vec(vec![shadow.as_shape(rect, cr(12.0)).into(), egui::Shape::rect_filled(rect, cr(12.0), tk.panel_solid), egui::Shape::rect_stroke(rect, cr(12.0), Stroke::new(1.0, tk.line), StrokeKind::Inside)]));
        rect
    });
    if let Some(i) = chosen {
        choose(app, i);
        return;
    }
    let outside = ctx.input(|i| i.pointer.any_pressed()) && ctx.input(|i| i.pointer.interact_pos()).map(|p| !area.inner.contains(p)).unwrap_or(false);
    if outside {
        close(app);
    }
}
