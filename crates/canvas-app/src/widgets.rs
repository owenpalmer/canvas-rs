//! The design's building blocks, painted to match the old stylesheet: text styles, headings,
//! lists and rows, pills, buttons, chips, cards, tabs, breadcrumbs, keycaps, spinners, inputs.

use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Align2, Color32, CornerRadius, CursorIcon, FontId, Galley, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

use crate::theme::{self, t};

pub fn cr(r: f32) -> CornerRadius {
    CornerRadius::same(r.round().clamp(0.0, 255.0) as u8)
}

/// Text style: size, weight, color, and a few CSS touches.
#[derive(Clone, Copy)]
pub struct Ts {
    pub size: f32,
    pub weight: u16,
    pub color: Color32,
    pub italic: bool,
    /// letter-spacing in em
    pub spacing: f32,
    pub upper: bool,
    pub underline: bool,
    pub mono: bool,
    /// line-height as a multiple of the size
    pub lh: f32,
}

impl Ts {
    pub fn new(size: f32, weight: u16, color: Color32) -> Ts {
        Ts { size, weight, color, italic: false, spacing: 0.0, upper: false, underline: false, mono: false, lh: 1.5 }
    }
    pub fn body() -> Ts {
        Ts::new(14.0, 400, t().text)
    }
    pub fn muted(size: f32) -> Ts {
        Ts::new(size, 400, t().muted)
    }
    pub fn faint(size: f32) -> Ts {
        Ts::new(size, 400, t().faint)
    }
    pub fn w(mut self, weight: u16) -> Ts {
        self.weight = weight;
        self
    }
    pub fn c(mut self, color: Color32) -> Ts {
        self.color = color;
        self
    }
    pub fn it(mut self, on: bool) -> Ts {
        self.italic = on;
        self
    }
    pub fn sp(mut self, em: f32) -> Ts {
        self.spacing = em;
        self
    }
    pub fn up(mut self) -> Ts {
        self.upper = true;
        self
    }
    pub fn ul(mut self, on: bool) -> Ts {
        self.underline = on;
        self
    }
    pub fn mono(mut self) -> Ts {
        self.mono = true;
        self
    }
    pub fn lh(mut self, lh: f32) -> Ts {
        self.lh = lh;
        self
    }
    pub fn font(&self) -> FontId {
        if self.mono {
            theme::mono(self.size)
        } else if self.italic {
            theme::italic(self.size, self.weight)
        } else {
            theme::font(self.size, self.weight)
        }
    }
    pub fn format(&self) -> TextFormat {
        TextFormat {
            font_id: self.font(),
            extra_letter_spacing: self.spacing * self.size,
            color: self.color,
            underline: if self.underline { Stroke::new(1.0, self.color) } else { Stroke::NONE },
            line_height: Some(self.line_px()),
            ..Default::default()
        }
    }
    pub fn line_px(&self) -> f32 {
        (self.size * self.lh).round()
    }
    /// Where text sits in its CSS line box: half the leading above it.
    pub fn top_pad(&self) -> f32 {
        ((self.line_px() - self.size * 1.2109) / 2.0).max(0.0)
    }
}

fn prep(text: &str, ts: &Ts) -> String {
    if ts.upper { text.to_uppercase() } else { text.to_string() }
}

/// Laid-out text: one line, or wrapped at `width`, or cut to one line with an ellipsis.
pub fn lay(ui: &Ui, text: &str, ts: Ts, width: Option<f32>, elide: bool) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(prep(text, &ts), ts.format());
    job.wrap = TextWrapping {
        max_width: width.unwrap_or(f32::INFINITY),
        max_rows: if elide { 1 } else { usize::MAX },
        break_anywhere: elide,
        overflow_character: if elide { Some('…') } else { None },
    };
    ui.painter().layout_job(job)
}

/// A LayoutJob with several styled runs.
pub struct Rich {
    pub job: LayoutJob,
}

impl Rich {
    pub fn new() -> Rich {
        Rich { job: LayoutJob::default() }
    }
    pub fn add(mut self, text: &str, ts: Ts) -> Rich {
        self.job.append(&prep(text, &ts), 0.0, ts.format());
        self
    }
    pub fn push(&mut self, text: &str, ts: Ts) {
        self.job.append(&prep(text, &ts), 0.0, ts.format());
    }
    pub fn wrap(mut self, width: f32) -> Rich {
        self.job.wrap.max_width = width;
        self
    }
    pub fn elide(mut self, width: f32) -> Rich {
        self.job.wrap = TextWrapping { max_width: width, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
        self
    }
    pub fn lay(self, ui: &Ui) -> Arc<Galley> {
        ui.painter().layout_job(self.job)
    }
}

impl Default for Rich {
    fn default() -> Self {
        Rich::new()
    }
}

/// Paint a galley whose top-left CSS line box is at `pos`.
pub fn paint_at(ui: &Ui, pos: Pos2, g: &Arc<Galley>, ts: &Ts) {
    ui.painter().galley(pos + vec2(0.0, ts.top_pad()), g.clone(), ts.color);
}

/// Allocate and draw a block of text (wrapping at the available width).
pub fn text_block(ui: &mut Ui, text: &str, ts: Ts) -> Response {
    let w = ui.available_width();
    let g = lay(ui, text, ts, Some(w), false);
    let h = (g.rows.len().max(1) as f32) * ts.line_px();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
    paint_at(ui, rect.min, &g, &ts);
    resp
}

/// One line of text, cut with an ellipsis to the available width.
pub fn text_line(ui: &mut Ui, text: &str, ts: Ts) -> Response {
    let w = ui.available_width();
    let g = lay(ui, text, ts, Some(w), true);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, ts.line_px()), Sense::hover());
    paint_at(ui, rect.min, &g, &ts);
    resp
}

/// One line of text taking only its own width (for horizontal rows).
pub fn text_line_fixed(ui: &mut Ui, text: &str, ts: Ts) -> Response {
    let g = lay(ui, text, ts, None, false);
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x, ts.line_px()), Sense::hover());
    paint_at(ui, rect.min, &g, &ts);
    resp
}

// --- headings ----------------------------------------------------------------------------------------
pub fn h1_ts() -> Ts {
    Ts::new(24.0, 650, t().text).sp(-0.01)
}

pub fn h2_ts() -> Ts {
    Ts::new(13.0, 600, t().muted).sp(0.05).up()
}

/// h2: 13px/600 uppercase, margin 28px 0 8px.
pub fn h2(ui: &mut Ui, text: &str) {
    ui.add_space(28.0);
    text_line(ui, text, h2_ts());
    ui.add_space(8.0);
}

/// .sub: muted, margin-bottom 18px.
pub fn sub(ui: &mut Ui, text: &str) {
    text_block(ui, text, Ts::muted(14.0));
    ui.add_space(18.0);
}

pub fn empty(ui: &mut Ui, text: &str) {
    ui.add_space(16.0);
    text_block(ui, text, Ts::faint(14.0));
    ui.add_space(16.0);
}

pub fn skeleton(ui: &mut Ui) {
    ui.add_space(40.0);
    text_line(ui, "Loading…", Ts::faint(14.0));
    ui.add_space(40.0);
}

// --- pills -------------------------------------------------------------------------------------------
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pill {
    Ok,
    Warn,
    Bad,
    Plain,
}

fn pill_colors(kind: Pill) -> (Color32, Color32) {
    let tk = t();
    match kind {
        Pill::Ok => (tk.ok_soft, tk.ok),
        Pill::Warn => (tk.warn_soft, tk.warn),
        Pill::Bad => (tk.bad_soft, tk.bad),
        Pill::Plain => (tk.hover, tk.muted),
    }
}

pub fn pill_size(ui: &Ui, text: &str) -> Vec2 {
    let g = lay(ui, text, Ts::new(11.0, 550, Color32::WHITE), None, false);
    vec2(g.size().x + 14.0, 18.0)
}

/// .pill at `rect.min` (11px/550, padding 1px 7px, radius 10).
pub fn paint_pill(ui: &Ui, pos: Pos2, kind: Pill, text: &str) -> Rect {
    let (bg, fg) = pill_colors(kind);
    let ts = Ts::new(11.0, 550, fg);
    let g = lay(ui, text, ts, None, false);
    let rect = Rect::from_min_size(pos, vec2(g.size().x + 14.0, 18.0));
    ui.painter().rect_filled(rect, cr(10.0), bg);
    ui.painter().galley(pos2(rect.min.x + 7.0, rect.center().y - g.size().y / 2.0), g, fg);
    rect
}

pub fn pill(ui: &mut Ui, kind: Pill, text: &str) -> Response {
    let size = pill_size(ui, text);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    paint_pill(ui, rect.min, kind, text);
    resp
}

// --- keycaps -----------------------------------------------------------------------------------------
pub fn kbd_size(ui: &Ui, text: &str, size: f32) -> Vec2 {
    let g = lay(ui, text, Ts::new(size, 400, t().muted).mono(), None, false);
    vec2(g.size().x + 12.0, size + 7.0)
}

/// kbd: 11px mono, padding 1px 5px, 1px border, radius 4.
pub fn paint_kbd(ui: &Ui, pos: Pos2, text: &str, size: f32, fg: Option<Color32>, bg: Option<Color32>, border: Option<Color32>) -> Rect {
    let tk = t();
    let fg = fg.unwrap_or(tk.muted);
    let g = lay(ui, text, Ts::new(size, 400, fg).mono(), None, false);
    let rect = Rect::from_min_size(pos, vec2(g.size().x + 12.0, size + 7.0));
    ui.painter().rect_filled(rect, cr(4.0), bg.unwrap_or(tk.panel));
    ui.painter().rect_stroke(rect, cr(4.0), Stroke::new(1.0, border.unwrap_or(tk.line)), StrokeKind::Inside);
    ui.painter().galley(pos2(rect.min.x + 6.0, rect.center().y - g.size().y / 2.0), g, fg);
    rect
}

// --- spinner -----------------------------------------------------------------------------------------
/// A ring with one colored quarter, turning once every .8s.
pub fn paint_spinner(ui: &Ui, center: Pos2, size: f32, width: f32, ring: Color32, head: Color32) {
    let t = ui.input(|i| i.time) as f32;
    let a0 = (t / 0.8) * std::f32::consts::TAU;
    let r = size / 2.0 - width / 2.0;
    ui.painter().circle_stroke(center, r, Stroke::new(width, ring));
    let n = 16;
    let pts: Vec<Pos2> = (0..=n).map(|i| {
        let a = a0 - std::f32::consts::FRAC_PI_2 - std::f32::consts::FRAC_PI_4 + (i as f32 / n as f32) * std::f32::consts::FRAC_PI_2;
        center + vec2(a.cos(), a.sin()) * r
    }).collect();
    ui.painter().add(egui::Shape::line(pts, Stroke::new(width, head)));
    ui.ctx().request_repaint();
}

pub fn spinner(ui: &mut Ui, size: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    let tk = t();
    paint_spinner(ui, rect.center(), size, 2.0, tk.line, tk.accent);
    resp
}

/// .cp .spin: a soft accent ring with an accent head.
pub fn spinner_soft(ui: &mut Ui, size: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    let tk = t();
    paint_spinner(ui, rect.center(), size, 2.0, tk.accent_soft, tk.accent);
    resp
}

// --- buttons -----------------------------------------------------------------------------------------
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Btn {
    Normal,
    Primary,
}

pub struct ButtonOpts {
    pub kind: Btn,
    pub disabled: bool,
    pub size: f32,
    pub pad: Vec2,
    pub pressed: bool,
}

impl Default for ButtonOpts {
    fn default() -> Self {
        ButtonOpts { kind: Btn::Normal, disabled: false, size: 13.0, pad: vec2(12.0, 6.0), pressed: false }
    }
}

/// .btn: 13px, padding 6px 12px, radius 6, 1px border, panel background. Returns clicked.
pub fn button_ex(ui: &mut Ui, text: &str, o: ButtonOpts) -> Response {
    let tk = t();
    let ts = Ts::new(o.size, 400, tk.text);
    let g = lay(ui, text, ts, None, false);
    let h = (o.size * 1.5).round() + o.pad.y * 2.0 + 2.0;
    let size = vec2(g.size().x + o.pad.x * 2.0 + 2.0, h);
    let (rect, resp) = ui.allocate_exact_size(size, if o.disabled { Sense::hover() } else { Sense::click() });
    paint_button(ui, rect, text, &o, resp.hovered());
    if !o.disabled {
        resp.clone().on_hover_cursor(CursorIcon::PointingHand);
    }
    resp
}

pub fn paint_button(ui: &Ui, rect: Rect, text: &str, o: &ButtonOpts, hovered: bool) {
    let tk = t();
    let (mut bg, mut border, mut fg) = match o.kind {
        Btn::Primary => (tk.accent, tk.accent, Color32::WHITE),
        Btn::Normal => (tk.panel, if hovered { tk.faint } else { tk.line }, tk.text),
    };
    if o.pressed {
        // .cp-mode-btn[aria-pressed="true"]
        bg = tk.accent_soft;
        border = tk.accent;
        fg = tk.accent;
    }
    let alpha = if o.disabled { 0.5 } else { 1.0 };
    let p = ui.painter();
    p.rect_filled(rect, cr(6.0), theme::alpha(bg, alpha));
    p.rect_stroke(rect, cr(6.0), Stroke::new(1.0, theme::alpha(border, alpha)), StrokeKind::Inside);
    let ts = Ts::new(o.size, 400, theme::alpha(fg, alpha));
    let g = lay(ui, text, ts, None, false);
    p.galley(rect.center() - g.size() / 2.0, g, fg);
}

pub fn button(ui: &mut Ui, text: &str) -> Response {
    button_ex(ui, text, ButtonOpts::default())
}

pub fn primary(ui: &mut Ui, text: &str) -> Response {
    button_ex(ui, text, ButtonOpts { kind: Btn::Primary, ..Default::default() })
}

pub fn button_if(ui: &mut Ui, text: &str, primary: bool, disabled: bool) -> Response {
    button_ex(ui, text, ButtonOpts { kind: if primary { Btn::Primary } else { Btn::Normal }, disabled, ..Default::default() })
}

/// .chip: 12px, padding 3px 11px, radius 14; "on" is inverted.
pub fn chip(ui: &mut Ui, text: &str, on: bool) -> Response {
    chip_sized(ui, text, on, 12.0, vec2(11.0, 3.0))
}

pub fn chip_sized(ui: &mut Ui, text: &str, on: bool, size: f32, pad: Vec2) -> Response {
    let tk = t();
    let ts = Ts::new(size, 400, if on { tk.bg } else { tk.muted });
    let g = lay(ui, text, ts, None, false);
    let h = (size * 1.5).round() + pad.y * 2.0 + 2.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + pad.x * 2.0 + 2.0, h), Sense::click());
    let (bg, border) = if on { (tk.text, tk.text) } else { (tk.panel, tk.line) };
    ui.painter().rect_filled(rect, cr(14.0), bg);
    ui.painter().rect_stroke(rect, cr(14.0), Stroke::new(1.0, border), StrokeKind::Inside);
    ui.painter().galley(rect.center() - g.size() / 2.0, g, ts.color);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// .linklike: accent, underlined, no box.
pub fn linklike(ui: &mut Ui, text: &str, size: f32) -> Response {
    let tk = t();
    let ts = Ts::new(size, 400, tk.accent).ul(true);
    let g = lay(ui, text, ts, None, false);
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x, ts.line_px()), Sense::click());
    paint_at(ui, rect.min, &g, &ts);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// A link-styled text button (.cp .link, .mini): no underline until hovered.
pub fn text_button(ui: &mut Ui, text: &str, ts: Ts, hover: Ts) -> Response {
    let g = lay(ui, text, ts, None, false);
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x, ts.line_px()), Sense::click());
    let style = if resp.hovered() { hover } else { ts };
    let g = if resp.hovered() { lay(ui, text, style, None, false) } else { g };
    paint_at(ui, rect.min, &g, &style);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

// --- surfaces ----------------------------------------------------------------------------------------
/// .list / .content / .set-panel: panel background, 1px line border, radius 8.
pub fn panel(ui: &Ui, rect: Rect, radius: f32) {
    let tk = t();
    ui.painter().rect_filled(rect, cr(radius), tk.panel);
    ui.painter().rect_stroke(rect, cr(radius), Stroke::new(1.0, tk.line), StrokeKind::Inside);
}

/// A box drawn around content laid out inside it: `pad` inside, border and background behind.
pub fn boxed<R>(ui: &mut Ui, pad: egui::Margin, radius: f32, add: impl FnOnce(&mut Ui) -> R) -> (R, Rect) {
    let bg_idx = ui.painter().add(egui::Shape::Noop);
    let border_idx = ui.painter().add(egui::Shape::Noop);
    let outer_w = ui.available_width();
    let inner = Rect::from_min_size(ui.cursor().min + vec2(pad.left as f32, pad.top as f32), vec2(outer_w - (pad.left + pad.right) as f32, f32::INFINITY));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)));
    let r = add(&mut child);
    let used = child.min_rect();
    let rect = Rect::from_min_max(ui.cursor().min, pos2(ui.cursor().min.x + outer_w, used.max.y.max(inner.min.y) + pad.bottom as f32));
    ui.allocate_rect(rect, Sense::hover());
    let tk = t();
    ui.painter().set(bg_idx, egui::Shape::rect_filled(rect, cr(radius), tk.panel));
    ui.painter().set(border_idx, egui::Shape::rect_stroke(rect, cr(radius), Stroke::new(1.0, tk.line), StrokeKind::Inside));
    (r, rect)
}

/// A horizontal run of widgets with a gap, wrapping when there isn't room.
pub fn hwrap<R>(ui: &mut Ui, gap: Vec2, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = gap;
        ui.horizontal_wrapped(add).inner
    })
    .inner
}

/// A 1px line.
pub fn hline(ui: &Ui, y: f32, x0: f32, x1: f32, color: Color32) {
    ui.painter().hline(x0..=x1, y + 0.5, Stroke::new(1.0, color));
}

// --- text fields ---------------------------------------------------------------------------------
pub struct InputOpts {
    pub size: f32,
    pub pad: Vec2,
    pub radius: f32,
    pub password: bool,
    pub width: Option<f32>,
    pub multiline: Option<usize>,
    pub focus_ring: bool,
    pub bg: Option<Color32>,
}

impl Default for InputOpts {
    fn default() -> Self {
        InputOpts { size: 14.0, pad: vec2(10.0, 6.0), radius: 6.0, password: false, width: None, multiline: None, focus_ring: true, bg: None }
    }
}

/// A text field in the design's style: panel background, 1px line border (accent when focused,
/// with a soft accent ring), muted placeholder.
pub fn input(ui: &mut Ui, id: egui::Id, text: &mut String, placeholder: &str, o: InputOpts) -> Response {
    let tk = t();
    let w = o.width.unwrap_or_else(|| ui.available_width());
    let line = (o.size * 1.5).round();
    let rows = o.multiline.unwrap_or(1);
    let h = line * rows as f32 + o.pad.y * 2.0 + 2.0;
    let (rect, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
    let focused = ui.memory(|m| m.has_focus(id));
    let bg_idx = ui.painter().add(egui::Shape::Noop);
    let inner = rect.shrink2(o.pad + vec2(1.0, 1.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
    let font = theme::font(o.size, 400);
    let mut edit = if o.multiline.is_some() { egui::TextEdit::multiline(text) } else { egui::TextEdit::singleline(text) };
    edit = edit
        .id(id)
        .frame(egui::Frame::NONE)
        .font(font.clone())
        .text_color(tk.text)
        .desired_width(inner.width())
        .margin(egui::Margin::ZERO)
        .password(o.password)
        .hint_text(egui::RichText::new(placeholder).color(tk.muted).font(font));
    if let Some(r) = o.multiline {
        edit = edit.desired_rows(r);
    }
    let resp = child.add(edit);
    let p = ui.painter();
    let mut shapes = vec![egui::Shape::rect_filled(rect, cr(o.radius), o.bg.unwrap_or(tk.panel))];
    shapes.push(egui::Shape::rect_stroke(rect, cr(o.radius), Stroke::new(1.0, if focused && o.focus_ring { tk.accent } else { tk.line }), StrokeKind::Inside));
    if focused && o.focus_ring {
        shapes.push(egui::Shape::rect_stroke(rect.expand(1.0), cr(o.radius + 1.0), Stroke::new(2.0, tk.accent_soft), StrokeKind::Outside));
    }
    p.set(bg_idx, egui::Shape::Vec(shapes));
    resp.on_hover_cursor(CursorIcon::Text)
}

/// A <select>: the current choice in a field, a menu of options when clicked. Returns the new
/// value when it changes. Options are (value, label); labels starting with "§" are group headings.
pub fn select(ui: &mut Ui, id: egui::Id, value: &str, options: &[(String, String)], width: f32, size: f32) -> Option<String> {
    let tk = t();
    let current = options.iter().find(|(v, l)| v == value && !l.starts_with('§')).map(|(_, l)| l.clone()).unwrap_or_default();
    let mut out = None;
    let h = (size * 1.5).round() + 10.0 + 2.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(width, h), Sense::click());
    let open = egui::Popup::is_id_open(ui.ctx(), id);
    ui.painter().rect_filled(rect, cr(6.0), tk.panel);
    ui.painter().rect_stroke(rect, cr(6.0), Stroke::new(1.0, if open { tk.accent } else if resp.hovered() { tk.faint } else { tk.line }), StrokeKind::Inside);
    let g = lay(ui, &current, Ts::new(size, 400, tk.text), Some(width - 34.0), true);
    ui.painter().galley(pos2(rect.min.x + 8.0, rect.center().y - g.size().y / 2.0), g, tk.text);
    // the arrow
    let c = pos2(rect.max.x - 14.0, rect.center().y);
    ui.painter().add(egui::Shape::line(vec![c + vec2(-4.0, -2.0), c + vec2(0.0, 2.0), c + vec2(4.0, -2.0)], Stroke::new(1.3, tk.muted)));
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    egui::Popup::menu(&resp).id(id).width(width.max(200.0)).show(|ui| {
        ui.set_min_width(width.max(200.0));
        egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
            for (v, label) in options {
                if let Some(head) = label.strip_prefix('§') {
                    ui.add_space(4.0);
                    text_line(ui, head, Ts::new(12.0, 600, tk.muted));
                    continue;
                }
                let on = v == value;
                let g = lay(ui, label, Ts::new(size, if on { 600 } else { 400 }, tk.text), Some(ui.available_width() - 16.0), true);
                let (r, rr) = ui.allocate_exact_size(vec2(ui.available_width(), g.size().y + 8.0), Sense::click());
                if rr.hovered() || on {
                    ui.painter().rect_filled(r, cr(4.0), tk.hover);
                }
                ui.painter().galley(pos2(r.min.x + 8.0, r.center().y - g.size().y / 2.0), g, tk.text);
                if rr.clicked() {
                    if !on {
                        out = Some(v.clone());
                    }
                    ui.close();
                }
            }
        });
    });
    out
}

/// A checkbox in the accent (or given) color, 15px.
pub fn checkbox(ui: &Ui, rect: Rect, checked: bool, indeterminate: bool, disabled: bool, color: Color32, hovered: bool) {
    let tk = t();
    let r = Rect::from_center_size(rect.center(), vec2(15.0, 15.0));
    let alpha = if disabled { 0.45 } else { 1.0 };
    if checked || indeterminate {
        ui.painter().rect_filled(r, cr(3.0), theme::alpha(color, alpha));
        if indeterminate && !checked {
            ui.painter().line_segment([r.left_center() + vec2(4.0, 0.0), r.right_center() - vec2(4.0, 0.0)], Stroke::new(2.0, Color32::WHITE));
        } else {
            let pts = vec![r.min + vec2(3.5, 7.8), r.min + vec2(6.3, 10.5), r.min + vec2(11.5, 4.5)];
            ui.painter().add(egui::Shape::line(pts, Stroke::new(2.0, Color32::WHITE)));
        }
    } else {
        ui.painter().rect_filled(r, cr(3.0), theme::alpha(tk.panel_solid, alpha));
        ui.painter().rect_stroke(r, cr(3.0), Stroke::new(1.0, theme::alpha(if hovered { tk.muted } else { tk.faint }, alpha)), StrokeKind::Inside);
    }
}

// --- the "you are here" keyboard ring -------------------------------------------------------------
pub fn focus_ring(ui: &Ui, rect: Rect, radius: f32) {
    // outline: 2px solid accent, offset 1px
    ui.painter().rect_stroke(rect.expand(1.0), cr(radius + 1.0), Stroke::new(2.0, t().accent), StrokeKind::Outside);
}

/// Text centered on its letters (Inter's cap height, .727em) rather than its line box, so digits
/// and capitals sit in the optical middle of a small pill.
pub fn centered_caps(ui: &Ui, rect: Rect, text: &str, ts: Ts) {
    let g = lay(ui, text, ts, None, false);
    let baseline = g.rows.first().map(|r| r.pos.y + r.row.glyphs.first().map(|gl| gl.pos.y).unwrap_or(0.0)).unwrap_or(g.size().y * 0.8);
    let y = rect.center().y + ts.size * 0.727 / 2.0 - baseline;
    ui.painter().galley(pos2(rect.center().x - g.size().x / 2.0, y), g, ts.color);
}

pub fn centered_text(ui: &Ui, rect: Rect, text: &str, ts: Ts) {
    let g = lay(ui, text, ts, None, false);
    ui.painter().galley(rect.center() - g.size() / 2.0, g, ts.color);
}

pub fn painter_text(ui: &Ui, pos: Pos2, anchor: Align2, text: &str, ts: Ts) -> Rect {
    let g = lay(ui, text, ts, None, false);
    let rect = anchor.anchor_size(pos, g.size());
    ui.painter().galley(rect.min, g, ts.color);
    rect
}
