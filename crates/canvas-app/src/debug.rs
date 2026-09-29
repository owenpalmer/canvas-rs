//! Debug: load times. With it on (Settings → Debug, or the palette), a corner panel shows how long
//! each thing took from the click or key that asked for it until it was on screen: the main view, a
//! viewer tab, a PDF's first page, the search palette. Each time splits into
//!   wait  input → the render started
//!   data  fetching what it shows (n = requests that went to the engine, not the memo)
//!   draw  laying it out
//!   paint until the next frame
//! and a PDF's into the viewer's part, load (the file, page 1's size), first page (drawing it).

use std::time::Instant;

use egui::{Align2, Color32, Context, CursorIcon, Id, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use serde_json::json;

use crate::app::App;
use crate::theme::t;
use crate::widgets::{Ts, cr, lay};

const KEEP: usize = 8;
const INPUT_WINDOW: f32 = 3.0;

#[derive(Clone)]
pub struct Trace {
    pub pane: String,
    pub what: String,
    input: (Instant, String),
    marks: Vec<(String, Instant)>,
    fetched0: u64,
}

pub struct Entry {
    pub pane: String,
    pub what: String,
    pub input: String,
    pub total: f32,
    pub phases: Vec<(String, f32)>,
    pub fetched: u64,
}

pub struct Debug {
    pub on: bool,
    pub open: bool,
    last_input: Option<(Instant, String)>,
    pending: Vec<(Trace, String)>,
    pub log: Vec<Entry>,
    pub hud_rect: Option<Rect>,
}

impl Debug {
    pub fn load(prefs: &crate::prefs::Prefs) -> Debug {
        Debug { on: prefs.bool("debugTimings", false), open: prefs.bool("debugHudOpen", true), last_input: None, pending: Vec::new(), log: Vec::new(), hud_rect: None }
    }
}

/// The latest input: what a render that starts soon after it was asked for by.
pub fn note_input(app: &mut App, ctx: &Context) {
    // Finish traces whose "paint" frame has now happened.
    let now = Instant::now();
    for (mut tr, title) in std::mem::take(&mut app.debug.pending) {
        tr.marks.push(("end".into(), now));
        let phases: Vec<(String, f32)> = tr.marks.windows(2).map(|w| (w[0].0.clone(), (w[1].1 - w[0].1).as_secs_f32() * 1000.0)).collect();
        let total = (now - tr.input.0).as_secs_f32() * 1000.0;
        let fetched = app.d.fetches.get().saturating_sub(tr.fetched0);
        log::debug!("[load] {}: {} — {:.0} ms ({})", tr.pane, title, total, tr.input.1);
        app.debug.log.insert(0, Entry { pane: tr.pane, what: if title.is_empty() { tr.what } else { title }, input: tr.input.1, total, phases, fetched });
        app.debug.log.truncate(KEEP);
    }
    if !app.debug.on {
        return;
    }
    let hud = app.debug.hud_rect;
    let ev = ctx.input(|i| {
        i.events.iter().find_map(|e| match e {
            egui::Event::Key { key, pressed: true, modifiers, .. } => {
                let mut k = String::new();
                if modifiers.ctrl {
                    k += "Ctrl+";
                }
                if modifiers.alt {
                    k += "Alt+";
                }
                Some(format!("key {k}{}", key.name()))
            }
            egui::Event::PointerButton { pressed: true, pos, .. } if !hud.map(|r| r.contains(*pos)).unwrap_or(false) => Some("click".to_string()),
            _ => None,
        })
    });
    if let Some(what) = ev {
        app.debug.last_input = Some((Instant::now(), what));
    }
}

/// A trace from the input (or, before any, the app's launch) to the frame after end().
pub fn start(app: &App, pane: &str, what: &str, first: &str) -> Option<Trace> {
    if !app.debug.on {
        return None;
    }
    let now = Instant::now();
    let input = match &app.debug.last_input {
        None => (app.started, "app launch".to_string()),
        Some((t, w)) if t.elapsed().as_secs_f32() < INPUT_WINDOW => (*t, w.clone()),
        _ => (now, "no input (background)".to_string()),
    };
    Some(Trace { pane: pane.into(), what: what.into(), marks: vec![("wait".into(), input.0), (first.into(), now)], input, fetched0: app.d.fetches.get() })
}

pub fn step(tr: &mut Option<Trace>, name: &str) {
    if let Some(t) = tr {
        t.marks.push((name.into(), Instant::now()));
    }
}

/// A later part of the same load (a PDF's first page), timed from the same input.
pub fn sub(tr: &Option<Trace>, pane: &str, what: &str, first: &str) -> Option<Trace> {
    tr.as_ref().map(|t| Trace { pane: pane.into(), what: what.into(), input: t.input.clone(), marks: vec![(t.pane.clone(), t.input.0), (first.into(), Instant::now())], fetched0: t.fetched0 })
}

pub fn end(app: &mut App, mut tr: Trace, title: &str) {
    tr.marks.push(("paint".into(), Instant::now()));
    app.debug.pending.push((tr, title.to_string()));
    crate::app::wake();
}

pub fn set_on(app: &mut App, on: bool) {
    app.debug.on = on;
    app.set_pref("debugTimings", json!(on));
    if !on {
        app.debug.log.clear();
    }
}

fn fmt_ms(ms: f32) -> String {
    if ms >= 1000.0 { format!("{:.2} s", ms / 1000.0) } else { format!("{} ms", ms.round() as i64) }
}

fn speed(ms: f32) -> Color32 {
    let tk = t();
    if ms < 100.0 {
        tk.ok
    } else if ms < 400.0 {
        tk.warn
    } else {
        tk.bad
    }
}

fn phase_color(name: &str) -> Color32 {
    let tk = t();
    match name {
        "data" | "load" | "index" => tk.accent,
        "draw" | "first page" => tk.warn,
        "paint" => tk.ok,
        "viewer" => tk.line,
        _ => tk.faint,
    }
}

/// The corner panel.
pub fn hud(app: &mut App, ctx: &Context) {
    if !app.debug.on {
        app.debug.hud_rect = None;
        return;
    }
    let tk = t();
    let screen = ctx.content_rect();
    let open = app.debug.open;
    let w = if open { 300.0f32.min(screen.width() - 24.0) } else { 0.0 };
    let head_ts = Ts::new(12.0, 600, tk.muted);
    let top = app.debug.log.first().map(|e| (fmt_ms(e.total), speed(e.total)));
    let collapsed_label = match (&top, open) {
        (Some((ms, _)), false) => format!("Load times · {ms}"),
        _ => "Load times".to_string(),
    };
    let head_w = if open { w } else { lay_width(ctx, &collapsed_label, head_ts) + 20.0 + 30.0 };
    let rows = if open { app.debug.log.len().max(1) } else { 0 };
    let list_h = if !open { 0.0 } else if app.debug.log.is_empty() { 28.0 } else { rows as f32 * 38.0 + 8.0 };
    let size = vec2(head_w.max(120.0), 28.0 + list_h);
    let rect = Rect::from_min_size(pos2(screen.max.x - 12.0 - size.x, screen.max.y - 12.0 - size.y), size);
    app.debug.hud_rect = Some(rect);
    let mut toggle = false;
    let mut off = false;
    egui::Area::new(Id::new("debug-hud")).order(egui::Order::Foreground).fixed_pos(rect.min).show(ctx, |ui| {
        let p = ui.painter();
        let shadow = egui::Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(51) };
        p.add(shadow.as_shape(rect, cr(8.0)));
        p.rect_filled(rect, cr(8.0), tk.panel_solid);
        p.rect_stroke(rect, cr(8.0), Stroke::new(1.0, tk.line), StrokeKind::Inside);
        let head = Rect::from_min_size(rect.min, vec2(rect.width() - 30.0, 28.0));
        let hr = ui.interact(head, Id::new("dh-toggle"), Sense::click());
        let hc = if hr.hovered() { tk.text } else { tk.muted };
        let g = lay(ui, "Load times", head_ts.c(hc), None, false);
        let gw = g.size().x;
        p.galley(pos2(head.min.x + 10.0, head.center().y - g.size().y / 2.0), g, hc);
        if let (Some((ms, color)), false) = (&top, open) {
            let g2 = lay(ui, &format!(" · {ms}"), Ts::new(12.0, 700, *color), None, false);
            p.galley(pos2(head.min.x + 10.0 + gw, head.center().y - g2.size().y / 2.0), g2, *color);
        }
        if hr.on_hover_text(if open { "Collapse" } else { "Expand" }).on_hover_cursor(CursorIcon::PointingHand).clicked() {
            toggle = true;
        }
        let xr = Rect::from_min_size(pos2(rect.max.x - 30.0, rect.min.y), vec2(30.0, 28.0));
        let xresp = ui.interact(xr, Id::new("dh-off"), Sense::click());
        crate::widgets::centered_text(ui, xr, "×", Ts::new(12.0, 400, if xresp.hovered() { tk.text } else { tk.muted }));
        if xresp.on_hover_text("Turn off (Settings → Debug)").clicked() {
            off = true;
        }
        if !open {
            return;
        }
        let x0 = rect.min.x + 10.0;
        let x1 = rect.max.x - 10.0;
        if app.debug.log.is_empty() {
            crate::widgets::painter_text(ui, pos2(x0, rect.min.y + 28.0 + 4.0), Align2::LEFT_TOP, "Click something to time it.", Ts::new(12.0, 400, tk.faint));
            return;
        }
        let mut y = rect.min.y + 28.0;
        for e in &app.debug.log {
            p.hline(x0..=x1, y + 0.5, Stroke::new(1.0, tk.line));
            let row = Rect::from_min_max(pos2(x0, y), pos2(x1, y + 38.0));
            crate::widgets::painter_text(ui, pos2(x0, y + 5.0 + 9.0), Align2::LEFT_CENTER, &e.pane.to_uppercase(), Ts::new(10.0, 400, tk.faint).sp(0.04));
            let total = fmt_ms(e.total);
            let tg = lay(ui, &total, Ts::new(12.0, 700, speed(e.total)), None, false);
            let tw = tg.size().x;
            p.galley(pos2(x1 - tw, y + 5.0 + 9.0 - tg.size().y / 2.0), tg, speed(e.total));
            let wg = lay(ui, &e.what, Ts::new(12.0, 400, tk.text), Some(x1 - tw - 6.0 - (x0 + 48.0)), true);
            p.galley(pos2(x0 + 48.0, y + 5.0 + 9.0 - wg.size().y / 2.0), wg, tk.text);
            // the phases as a bar
            let bar = Rect::from_min_size(pos2(x0, y + 27.0), vec2(x1 - x0, 4.0));
            p.rect_filled(bar, cr(2.0), tk.line);
            let sum: f32 = e.phases.iter().map(|(_, v)| v.max(0.0)).sum::<f32>().max(1e-3);
            let mut bx = bar.min.x;
            for (k, v) in &e.phases {
                let bw = (v.max(0.0) / sum * bar.width()).max(1.0);
                p.rect_filled(Rect::from_min_size(pos2(bx, bar.min.y), vec2(bw.min(bar.max.x - bx), 4.0)), 0.0, phase_color(k));
                bx += bw;
            }
            let tip = format!("{}\n{}\n{}{}", e.what, e.input, e.phases.iter().map(|(k, v)| format!("{k} {}", fmt_ms(*v))).collect::<Vec<_>>().join("\n"), if e.fetched > 0 { format!("\n{} fetched", e.fetched) } else { String::new() });
            ui.interact(row, Id::new(("dh-row", y as i64)), Sense::hover()).on_hover_text(tip);
            y += 38.0;
        }
    });
    if toggle {
        app.debug.open = !app.debug.open;
        let o = app.debug.open;
        app.set_pref("debugHudOpen", json!(o));
    }
    if off {
        set_on(app, false);
    }
}

fn lay_width(ctx: &Context, text: &str, ts: Ts) -> f32 {
    ctx.fonts_mut(|f| f.layout_no_wrap(text.to_string(), ts.font(), ts.color).size().x)
}
