//! The window: its three panes (sidebar | main | viewer), their backgrounds (paper, vignette,
//! vines), the resize handles, and what floats above (banner, search palette, shortcuts, toast,
//! load-time panel).

use std::time::Instant;

use egui::{Color32, CursorIcon, Id, Rect, Sense, Stroke, StrokeKind, Ui, Visuals, pos2, vec2};

use crate::app::{App, Pane};
use crate::panes::{self, MAIN_MIN, SIDEBAR_MAX, SIDEBAR_MIN, VIEWER_MIN};
use crate::theme::{self, Tokens, t};
use crate::widgets::cr;

/// egui's own widgets (text fields, dropdowns, scrollbars, selection) in the theme's colors.
pub fn style(ctx: &egui::Context, tk: &Tokens) {
    let mut v = if tk.is_dark() { Visuals::dark() } else { Visuals::light() };
    v.override_text_color = Some(tk.text);
    v.panel_fill = Color32::TRANSPARENT;
    v.window_fill = tk.panel_solid;
    v.extreme_bg_color = tk.panel;
    v.faint_bg_color = tk.hover;
    v.code_bg_color = tk.bg;
    v.hyperlink_color = tk.accent;
    v.selection.bg_fill = theme::alpha(tk.accent, 0.3);
    v.selection.stroke = Stroke::new(1.0, tk.text);
    v.window_stroke = Stroke::new(1.0, tk.line);
    v.window_corner_radius = cr(8.0);
    v.menu_corner_radius = cr(8.0);
    v.window_shadow = egui::Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(64) };
    v.popup_shadow = egui::Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(40) };
    v.text_cursor.stroke = Stroke::new(1.5, tk.text);
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.bg_fill = tk.panel;
        w.weak_bg_fill = tk.panel;
        w.bg_stroke = Stroke::new(1.0, tk.line);
        w.fg_stroke = Stroke::new(1.0, tk.text);
        w.corner_radius = cr(6.0);
        w.expansion = 0.0;
    }
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, tk.faint);
    v.widgets.hovered.weak_bg_fill = tk.hover;
    v.widgets.active.bg_stroke = Stroke::new(1.0, tk.accent);
    v.widgets.open.bg_stroke = Stroke::new(1.0, tk.accent);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, tk.text);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, tk.line);
    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = vec2(0.0, 0.0);
        s.spacing.button_padding = vec2(8.0, 4.0);
        s.spacing.interact_size = vec2(20.0, 20.0);
        s.spacing.scroll = egui::style::ScrollStyle::floating();
        s.spacing.scroll.floating_width = 8.0;
        s.spacing.scroll.bar_inner_margin = 2.0;
        s.spacing.scroll.bar_outer_margin = 2.0;
        s.interaction.selectable_labels = false;
        s.url_in_tooltip = false;
        s.animation_time = 0.12;
        s.text_styles.insert(egui::TextStyle::Body, theme::font(14.0, 400));
        s.text_styles.insert(egui::TextStyle::Button, theme::font(13.0, 400));
        s.text_styles.insert(egui::TextStyle::Small, theme::font(12.0, 400));
        s.text_styles.insert(egui::TextStyle::Monospace, theme::mono(13.0));
    });
}

/// The panes' rectangles this frame.
pub struct Layout {
    pub sidebar: Option<Rect>,
    pub main: Option<Rect>,
    pub viewer: Option<Rect>,
    pub bare: bool,
}

fn layout(app: &mut App, screen: Rect) -> Layout {
    let bare = matches!(app.route().view, crate::route::View::Welcome);
    let w = screen.width();
    let p = &mut app.panes;
    let has_viewer = !p.tabs.is_empty() && !bare;
    p.open_sidebar = !p.hidden.contains("sidebar") && !bare;
    p.open_viewer = has_viewer && !p.hidden.contains("viewer");
    p.open_main = !p.hidden.contains("main") || !p.open_viewer;
    let side_w = if p.open_sidebar { p.sidebar_w.clamp(SIDEBAR_MIN, SIDEBAR_MAX.min(w - MAIN_MIN).max(SIDEBAR_MIN)) } else { 0.0 };
    let view_w = p.viewer_w.clamp(VIEWER_MIN, (w - side_w - MAIN_MIN).max(VIEWER_MIN));
    let x0 = screen.min.x;
    let sidebar = p.open_sidebar.then(|| Rect::from_min_max(pos2(x0, screen.min.y), pos2(x0 + side_w, screen.max.y)));
    let (main, viewer) = match (p.open_main, p.open_viewer) {
        (true, true) => (
            Some(Rect::from_min_max(pos2(x0 + side_w, screen.min.y), pos2(screen.max.x - view_w, screen.max.y))),
            Some(Rect::from_min_max(pos2(screen.max.x - view_w, screen.min.y), screen.max)),
        ),
        (true, false) => (Some(Rect::from_min_max(pos2(x0 + side_w, screen.min.y), screen.max)), None),
        (false, true) => (None, Some(Rect::from_min_max(pos2(x0 + side_w, screen.min.y), screen.max))),
        (false, false) => (Some(Rect::from_min_max(pos2(x0 + side_w, screen.min.y), screen.max)), None),
    };
    p.side_w_now = side_w;
    p.view_w_now = viewer.map(|r| r.width()).unwrap_or(0.0);
    Layout { sidebar, main, viewer, bare }
}

/// The launch intro: the first screen fades in over 200ms (and the view settles from 99%).
fn intro(app: &App) -> (f32, f32) {
    let Some(start) = app.intro else { return (1.0, 1.0) };
    if app.settings.reduced_motion {
        return (1.0, 1.0);
    }
    let p = crate::anim::progress(start, 0.0, 0.2);
    let e = crate::anim::cubic_bezier(0.2, 0.8, 0.2, 1.0, p);
    (e, 0.99 + 0.01 * e)
}

pub fn draw(app: &mut App, root: &mut Ui, frame: &mut eframe::Frame) {
    let ctx = root.ctx().clone();
    let screen = ctx.content_rect();
    let tk = t();
    let ppp = ctx.pixels_per_point();
    let lay = layout(app, screen);
    let (fade, scale) = intro(app);
    if fade < 1.0 {
        ctx.request_repaint();
    }
    let painter = root.painter().clone();
    // Backgrounds: the body's paper everywhere, the sidebar's own over it.
    let mut paper = crate::paper::Paper::new(ppp);
    if crate::paper::available(frame) {
        paper.paper(&painter, screen, tk.bg, &tk, 1.0);
        if let Some(r) = lay.sidebar {
            paper.paper(&painter, r, tk.sidebar, &tk, fade);
        }
    } else {
        painter.rect_filled(screen, 0.0, tk.bg);
        if let Some(r) = lay.sidebar {
            painter.rect_filled(r, 0.0, theme::alpha(tk.sidebar, fade));
        }
    }
    if let Some(r) = lay.main {
        paper.vignette(&painter, r, tk.vignette, fade);
    }
    // Vines, behind each region's content, pinned to its visible area.
    crate::vines::paint(app, root, lay.sidebar, lay.main, fade);

    if let Some(r) = lay.sidebar {
        let mut ui = root.new_child(egui::UiBuilder::new().max_rect(r).id_salt("sidebar"));
        ui.set_clip_rect(r);
        ui.multiply_opacity(fade);
        crate::sidebar::draw(app, &mut ui, r);
        // border-right: 1px solid line
        painter.vline(r.max.x - 0.5, r.y_range(), Stroke::new(1.0, theme::alpha(tk.line, fade)));
    }
    if let Some(r) = lay.main {
        let mut ui = root.new_child(egui::UiBuilder::new().max_rect(r).id_salt("main"));
        ui.set_clip_rect(r);
        ui.multiply_opacity(fade);
        panes::draw_main(app, &mut ui, r, lay.bare, scale);
    }
    if let Some(r) = lay.viewer {
        let mut ui = root.new_child(egui::UiBuilder::new().max_rect(r).id_salt("viewer"));
        ui.set_clip_rect(r);
        panes::draw_viewer(app, &mut ui, r);
        painter.vline(r.min.x + 0.5, r.y_range(), Stroke::new(1.0, tk.line));
    }
    // The pane the keys act on: a thin outline inside its edge, over its content.
    let focused = match app.panes.focused {
        Pane::Sidebar => lay.sidebar,
        Pane::Main => lay.main,
        Pane::Viewer => lay.viewer,
    };
    if let (Some(r), false) = (focused, lay.bare) {
        let fg = root.ctx().layer_painter(egui::LayerId::new(egui::Order::Middle, Id::new("pane-focus")));
        fg.rect_stroke(r.shrink(0.0), 0.0, Stroke::new(1.0, theme::mix(tk.accent, Color32::TRANSPARENT, 0.8)), StrokeKind::Inside);
    }
    if !lay.bare {
        resize_handles(app, root, &lay, screen);
    }
    crate::setup::banner(app, &ctx);
    crate::palette::draw(app, &ctx);
    crate::nav::draw_keys(app, &ctx);
    toast(app, &ctx);
    crate::debug::hud(app, &ctx);
    // Window title: the page's heading.
    let title = if app.main.out.title.is_empty() { "Canvas".to_string() } else { format!("{} · Canvas", app.main.out.title) };
    if title != app.title {
        app.title = title.clone();
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }
}

/// Dragging an edge resizes the sidebar or the viewer; double-click resets.
fn resize_handles(app: &mut App, root: &mut Ui, lay: &Layout, screen: Rect) {
    let tk = t();
    let handle = |which: &str, x: f32| -> Option<(egui::Response, bool)> {
        let r = Rect::from_min_max(pos2(x - 4.0, screen.min.y), pos2(x + 4.0, screen.max.y));
        let id = Id::new(("resize", which));
        let resp = root.interact(r, id, Sense::click_and_drag()).on_hover_cursor(CursorIcon::ResizeColumn);
        let active = resp.hovered() || resp.dragged();
        let fg = root.ctx().layer_painter(egui::LayerId::new(egui::Order::Middle, Id::new("resize-line")));
        // .pane-resize::after: 2px line that turns accent on hover (with a .15s fade)
        let a = root.ctx().animate_bool_with_time(id.with("h"), active, 0.15);
        if a > 0.0 {
            fg.rect_filled(Rect::from_min_size(pos2(x - 1.0, screen.min.y), vec2(2.0, screen.height())), 0.0, theme::alpha(tk.accent, a));
        }
        Some((resp, active))
    };
    if let Some(sr) = lay.sidebar {
        if let Some((resp, _)) = handle("sidebar", sr.max.x) {
            if resp.double_clicked() {
                panes::set_sidebar_width(app, panes::SIDEBAR_DEFAULT, true);
            } else if resp.dragged() {
                if let Some(p) = resp.interact_pointer_pos() {
                    panes::set_sidebar_width(app, p.x - screen.min.x, false);
                }
            } else if resp.drag_stopped() {
                let w = app.panes.sidebar_w;
                panes::set_sidebar_width(app, w, true);
            }
        }
    }
    if let (Some(vr), Some(_)) = (lay.viewer, lay.main) {
        if let Some((resp, _)) = handle("viewer", vr.min.x) {
            if resp.double_clicked() {
                panes::set_viewer_width(app, panes::VIEWER_DEFAULT, true, screen.width());
            } else if resp.dragged() {
                if let Some(p) = resp.interact_pointer_pos() {
                    panes::set_viewer_width(app, screen.max.x - p.x, false, screen.width());
                }
            } else if resp.drag_stopped() {
                let w = app.panes.viewer_w;
                panes::set_viewer_width(app, w, true, screen.width());
            }
        }
    }
}

/// #toast: bottom center, slides up and fades in; 3.5s.
fn toast(app: &mut App, ctx: &egui::Context) {
    let Some(tst) = &app.toast else { return };
    let age = tst.at.elapsed().as_secs_f32();
    if age > 3.7 {
        app.toast = None;
        return;
    }
    ctx.request_repaint();
    let tk = t();
    let show = if age < 3.5 { (age / 0.2).min(1.0) } else { 1.0 - ((age - 3.5) / 0.2).min(1.0) };
    let e = crate::anim::ease(show);
    let screen = ctx.content_rect();
    let layer = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::new("toast")));
    let ts = crate::widgets::Ts::new(13.0, 400, if tst.bad { tk.bad } else { tk.text });
    let g = layer.layout(tst.text.clone(), ts.font(), ts.color, screen.width() * 0.9 - 32.0);
    let size = g.size() + vec2(32.0, 18.0);
    let center = pos2(screen.center().x, screen.max.y - 24.0 - size.y / 2.0 + 20.0 * (1.0 - e));
    let rect = Rect::from_center_size(center, size);
    let alpha = e;
    layer.add(egui::Shape::from(egui::epaint::RectShape::filled(rect, cr(8.0), theme::alpha(tk.panel_solid, alpha)).with_blur_width(0.0)));
    let shadow = egui::Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha((64.0 * alpha) as u8) };
    layer.add(shadow.as_shape(rect, cr(8.0)));
    layer.rect_filled(rect, cr(8.0), theme::alpha(tk.panel_solid, alpha));
    layer.rect_stroke(rect, cr(8.0), Stroke::new(1.0, theme::alpha(if tst.bad { tk.bad } else { tk.line }, alpha)), StrokeKind::Inside);
    layer.galley(rect.min + vec2(16.0, 9.0), g, theme::alpha(ts.color, alpha));
    let _ = Instant::now();
}
