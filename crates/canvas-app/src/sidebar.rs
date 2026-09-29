//! The sidebar: the app's places, your courses (in your order, some hidden), past courses by term,
//! and the sync status. ✎ switches the course list to editing: drag (or Alt+↑/↓) to reorder,
//! Space or the eye to hide.

use std::collections::HashSet;

use egui::{Align2, Color32, CursorIcon, Id, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use serde_json::{Value, json};

use crate::app::{App, Pane};
use crate::fmt::{self, color_for};
use crate::route::View;
use crate::theme::{self, t};
use crate::widgets::{Ts, cr, lay, paint_kbd};

pub struct SidebarState {
    pub editing: bool,
    pub past_open: bool,
    pub cursor: Option<String>,
    pub active_href: Option<String>,
    pub items: Vec<(String, Option<String>)>,
    pub drag: Option<(String, Vec<String>)>,
    pub edit_focus: Option<String>,
    pub search_hover: bool,
}

impl SidebarState {
    pub fn load(prefs: &crate::prefs::Prefs) -> SidebarState {
        SidebarState {
            editing: false,
            past_open: prefs.bool("pastOpen", false),
            cursor: None,
            active_href: None,
            items: Vec::new(),
            drag: None,
            edit_focus: None,
            search_hover: false,
        }
    }
}

pub fn hidden_courses(app: &App) -> HashSet<String> {
    app.prefs.strings("hiddenCourses").into_iter().collect()
}

/// The sidebar's course order: yours, then any course you haven't placed yet, favorites first.
/// Hidden courses leave the sidebar, the dashboard cards and J/K.
pub fn ordered_courses(app: &App, courses: &[Value], include_hidden: bool) -> Vec<Value> {
    let order = app.prefs.strings("courseOrder");
    let rank = |c: &Value| order.iter().position(|id| *id == fmt::id(&c["id"])).map(|i| i as f64).unwrap_or(f64::INFINITY);
    let hidden = hidden_courses(app);
    let key = |c: &Value| c["course_code"].as_str().filter(|x| !x.is_empty()).or(c["name"].as_str()).unwrap_or("").to_string();
    let mut v: Vec<Value> = courses.to_vec();
    v.sort_by(|a, b| {
        rank(a)
            .partial_cmp(&rank(b))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| (b["is_favorite"].as_bool().unwrap_or(false) as i32).cmp(&(a["is_favorite"].as_bool().unwrap_or(false) as i32)))
            .then_with(|| key(a).cmp(&key(b)))
    });
    v.into_iter().filter(|c| include_hidden || !hidden.contains(&fmt::id(&c["id"]))).collect()
}

pub fn is_past(app: &App, cid: &str) -> bool {
    app.d.peek("past_courses").and_then(|p| p.as_array().map(|a| a.iter().any(|c| fmt::id(&c["id"]) == cid))).unwrap_or(false)
}

pub fn set_past_open(app: &mut App, open: bool) {
    app.sidebar.past_open = open;
    app.set_pref("pastOpen", json!(open));
}

pub fn set_editing(app: &mut App, on: bool) {
    app.sidebar.editing = on;
    if on {
        app.panes.focused = Pane::Sidebar;
        app.sidebar.edit_focus = None;
    } else if app.panes.focused == Pane::Sidebar {
        app.panes.focused = Pane::Main;
    }
}

fn save_order(app: &mut App, order: Vec<String>) {
    app.set_pref("courseOrder", json!(order));
}

fn toggle_hidden(app: &mut App, id: &str) {
    let mut hidden = hidden_courses(app);
    if !hidden.remove(id) {
        hidden.insert(id.to_string());
    }
    let v: Vec<String> = hidden.into_iter().collect();
    app.set_pref("hiddenCourses", json!(v));
}

fn edit_order(app: &App) -> Vec<String> {
    let courses = app.d.peek("courses").and_then(|v| v.as_array().cloned()).unwrap_or_default();
    ordered_courses(app, &courses, true).iter().map(|c| fmt::id(&c["id"])).collect()
}

/// Keys while editing the course list. Returns true when used.
pub fn edit_key(app: &mut App, key: &str, m: &egui::Modifiers) -> bool {
    let order = edit_order(app);
    let Some(id) = app.sidebar.edit_focus.clone().or_else(|| order.first().cloned()) else { return false };
    let i = order.iter().position(|x| *x == id).unwrap_or(0);
    let up = key == "ArrowUp" || key == "K" || key == "k";
    let down = key == "ArrowDown" || key == "J" || key == "j";
    if (m.alt && (key == "ArrowUp" || key == "ArrowDown")) || key == "J" || key == "K" {
        let j = if up { i as i64 - 1 } else { i as i64 + 1 };
        if j >= 0 && (j as usize) < order.len() {
            let mut o = order.clone();
            o.swap(i, j as usize);
            save_order(app, o);
        }
        app.sidebar.edit_focus = Some(id);
        return true;
    }
    if up || down {
        let j = if up { i.saturating_sub(1) } else { (i + 1).min(order.len() - 1) };
        app.sidebar.edit_focus = Some(order[j].clone());
        return true;
    }
    if key == " " || key == "Enter" || key == "x" {
        toggle_hidden(app, &id);
        app.sidebar.edit_focus = Some(id);
        return true;
    }
    if key == "Escape" {
        set_editing(app, false);
        return true;
    }
    false
}

/// j / k through the sidebar's links (and the past courses toggle).
pub fn step(app: &mut App, d: i64) {
    let items = &app.sidebar.items;
    if items.is_empty() {
        return;
    }
    let cur = app.sidebar.cursor.as_ref().and_then(|c| items.iter().position(|(k, _)| k == c));
    let active = app.sidebar.active_href.as_ref().and_then(|a| items.iter().position(|(_, h)| h.as_ref() == Some(a)));
    let i = match cur.or(active) {
        None => 0,
        Some(i) => (i as i64 + if cur.is_some() { d } else { 0 }).clamp(0, items.len() as i64 - 1) as usize,
    };
    app.sidebar.cursor = Some(items[i].0.clone());
    app.panes.focused = Pane::Sidebar;
}

pub fn open_cursor(app: &mut App) {
    let Some(c) = app.sidebar.cursor.clone() else { return };
    match app.sidebar.items.iter().find(|(k, _)| *k == c).and_then(|(_, h)| h.clone()) {
        Some(h) => crate::nav::go(app, &h),
        None if c == "past-toggle" => {
            let open = !app.sidebar.past_open;
            set_past_open(app, open);
        }
        None => {}
    }
}

fn active_href(app: &App) -> Option<String> {
    let r = app.route();
    let cm = regex::Regex::new(r"^/c/(\d+)").unwrap().captures(&r.path).map(|m| m[1].to_string());
    if let Some(cid) = cm {
        return Some(format!("#/c/{cid}/modules"));
    }
    Some(match r.view {
        View::Dashboard => "#/",
        View::Inbox | View::Conversation(_) => "#/inbox",
        View::Notebooks | View::Notebook(_) | View::NotebookNew | View::NotebookAdd(_) => "#/notebooks",
        View::Anki | View::AnkiImport | View::AnkiDeck(_) => "#/anki",
        View::Settings => "#/settings",
        _ => return None,
    }
    .to_string())
}

/// A sidebar link row (#sidebar nav a): 5px 8px padding, radius 6, muted until hovered or active.
fn nav_link(app: &mut App, ui: &mut Ui, key: &str, href: &str, dot: Option<Color32>, label: &str, tooltip: Option<&str>, badge: impl FnOnce(&mut App, &Ui, Rect)) {
    let tk = t();
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 31.0), Sense::click());
    let active = app.sidebar.active_href.as_deref() == Some(href);
    let cursor = app.panes.focused == Pane::Sidebar && app.sidebar.cursor.as_deref() == Some(key);
    let hovered = resp.hovered();
    if active || hovered || cursor {
        ui.painter().rect_filled(rect, cr(6.0), tk.hover);
    }
    if cursor {
        ui.painter().rect_filled(Rect::from_min_size(rect.min, vec2(2.0, rect.height())), egui::CornerRadius { nw: 6, sw: 6, ne: 0, se: 0 }, tk.accent);
    }
    let color = if active || hovered || cursor { tk.text } else { tk.muted };
    let mut x = rect.min.x + 8.0;
    if let Some(c) = dot {
        ui.painter().circle_filled(pos2(x + 4.0, rect.center().y), 4.0, c);
        x += 16.0;
    }
    let ts = Ts::new(14.0, if active { 550 } else { 400 }, color);
    let g = lay(ui, label, ts, Some(rect.max.x - 8.0 - x - 28.0), true);
    ui.painter().galley(pos2(x, rect.center().y - g.size().y / 2.0), g, color);
    badge(app, ui, rect);
    let resp = match tooltip {
        Some(tip) => resp.on_hover_text(tip),
        None => resp,
    };
    if resp.hovered() {
        crate::nav::prefetch(app, href);
    }
    if resp.clicked() {
        app.sidebar.cursor = None;
        crate::nav::go(app, href);
    }
    resp.on_hover_cursor(CursorIcon::PointingHand);
    app.sidebar.items.push((key.to_string(), Some(href.to_string())));
}

/// .count: accent pill with a number, at the right of a link.
fn count_badge(ui: &Ui, rect: Rect, n: i64, bg: Color32) {
    let ts = Ts::new(11.0, 400, Color32::WHITE);
    let g = lay(ui, &n.to_string(), ts, None, false);
    let r = Rect::from_min_size(pos2(rect.max.x - 8.0 - g.size().x - 12.0, rect.center().y - 8.5), vec2(g.size().x + 12.0, 17.0));
    ui.painter().rect_filled(r, cr(9.0), bg);
    crate::widgets::centered_caps(ui, r, &n.to_string(), ts);
}

pub fn draw(app: &mut App, ui: &mut Ui, rect: Rect) {
    let tk = t();
    app.sidebar.items.clear();
    app.sidebar.active_href = active_href(app);
    if ui.rect_contains_pointer(rect) && ui.input(|i| i.pointer.any_pressed()) {
        app.panes.focused = Pane::Sidebar;
    }
    egui::ScrollArea::vertical().id_salt("sidebar-scroll").auto_shrink([false, false]).show(ui, |ui| {
        let w = ui.available_width();
        let inner_w = w - 20.0;
        let top = ui.cursor().min.y;
        let min_h = ui.clip_rect().height();
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(ui.cursor().min + vec2(10.0, 14.0), vec2(inner_w, f32::INFINITY))).layout(egui::Layout::top_down(egui::Align::Min)));
        content(app, &mut c);
        let body_bottom = c.min_rect().max.y;
        // footer: margin-top auto, padding 10px 8px 0, gap 6
        let footer_h = 10.0 + 18.0 + 6.0 + 18.0;
        let footer_top = (body_bottom).max(top + min_h - 14.0 - footer_h);
        let mut f = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(pos2(ui.cursor().min.x + 18.0, footer_top + 10.0), vec2(inner_w - 16.0, footer_h))).layout(egui::Layout::top_down(egui::Align::Min)));
        footer(app, &mut f);
        ui.allocate_space(vec2(w, (footer_top + footer_h + 14.0 - top).max(0.0)));
    });
    let _ = tk;
}

fn content(app: &mut App, ui: &mut Ui) {
    let tk = t();
    let w = ui.available_width();
    // .brand: gap 9, 650 15px, padding 2px 8px 12px; pane buttons first
    let brand = ui.allocate_exact_size(vec2(w, 2.0 + 24.0 + 12.0), Sense::hover()).0;
    let mut b = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(brand.min + vec2(2.0, 2.0), vec2(w - 8.0, 24.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
    crate::panes::pane_buttons(app, &mut b, Pane::Sidebar);
    b.add_space(9.0);
    let (lr, _) = b.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
    b.painter().rect_filled(lr, cr(5.0), tk.accent);
    b.add_space(9.0);
    crate::widgets::text_line(&mut b, "Canvas", Ts::new(15.0, 650, tk.text));

    // .search-btn
    let (sr, sresp) = ui.allocate_exact_size(vec2(w, 33.0), Sense::click());
    ui.painter().rect_filled(sr, cr(6.0), tk.panel);
    ui.painter().rect_stroke(sr, cr(6.0), Stroke::new(1.0, if sresp.hovered() { tk.faint } else { tk.line }), StrokeKind::Inside);
    crate::widgets::painter_text(ui, pos2(sr.min.x + 9.0, sr.center().y), Align2::LEFT_CENTER, "Search", Ts::new(14.0, 400, tk.muted));
    let ks = crate::widgets::kbd_size(ui, "Ctrl K", 11.0);
    paint_kbd(ui, pos2(sr.max.x - 9.0 - ks.x, sr.center().y - ks.y / 2.0), "Ctrl K", 11.0, None, None, None);
    if sresp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
        crate::palette::open(app);
    }
    ui.add_space(10.0);

    let unread = app.d.peek("inbox").and_then(|v| v.as_array().map(|a| a.iter().filter(|m| m["workflow_state"] == "unread").count() as i64)).unwrap_or(0);
    let running = crate::notebooks::running(app);
    let due = app.anki.due;
    nav_link(app, ui, "nav-home", "#/", None, "Dashboard", None, |_, _, _| {});
    nav_link(app, ui, "nav-inbox", "#/inbox", None, "Inbox", None, |_, ui, r| {
        if unread > 0 {
            count_badge(ui, r, unread, t().accent)
        }
    });
    nav_link(app, ui, "nav-nb", "#/notebooks", None, "Gemini Notebooks", None, |_, ui, r| {
        if running {
            // .running: 7px dot, pulsing
            let time = ui.input(|i| i.time) as f32;
            let a = 0.3 + 0.7 * ((time % 2.0 - 1.0).abs());
            ui.painter().circle_filled(pos2(r.max.x - 8.0 - 3.5, r.center().y), 3.5, theme::alpha(t().warn, 1.0 - a + 0.3));
            ui.ctx().request_repaint();
        }
    });
    nav_link(app, ui, "nav-anki", "#/anki", None, "Anki", None, |_, ui, r| {
        if let Some(n) = due.filter(|n| *n > 0) {
            count_badge(ui, r, n, t().ok)
        }
    });
    nav_link(app, ui, "nav-settings", "#/settings", None, "Settings", None, |_, _, _| {});

    // .section-label: 11px uppercase .06em faint, padding 14px 8px 4px, with ✎ at the right
    ui.add_space(14.0);
    let (lr, lresp) = ui.allocate_exact_size(vec2(w, 18.0), Sense::hover());
    crate::widgets::painter_text(ui, pos2(lr.min.x + 8.0, lr.center().y), Align2::LEFT_CENTER, "Courses", Ts::new(11.0, 400, tk.faint).up().sp(0.06));
    let editing = app.sidebar.editing;
    let label = if editing { "Done" } else { "✎" };
    let ets = Ts::new(12.0, if editing { 600 } else { 400 }, if editing { tk.accent } else { tk.faint });
    let eg = lay(ui, label, ets, None, false);
    let er = Rect::from_min_size(pos2(lr.max.x - 8.0 - eg.size().x - 10.0, lr.min.y), vec2(eg.size().x + 10.0, 18.0));
    let eresp = ui.interact(er, Id::new("edit-courses"), Sense::click());
    let show = editing || lresp.hovered() || eresp.hovered() || ui.rect_contains_pointer(lr);
    if show {
        if eresp.hovered() {
            ui.painter().rect_filled(er, cr(4.0), tk.hover);
        }
        let color = if editing { tk.accent } else if eresp.hovered() { tk.text } else { tk.faint };
        ui.painter().galley(er.center() - eg.size() / 2.0, eg, color);
    }
    let eresp = eresp.on_hover_text(if editing { "Done (Esc)" } else { "Reorder or hide courses" });
    if eresp.clicked() {
        set_editing(app, !editing);
    }
    ui.add_space(4.0);

    let courses = app.d.need0("courses").ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
    let colors = app.d.need0("colors").ok();
    let _ = app.d.need0("inbox");
    if app.sidebar.editing {
        edit_list(app, ui, &courses, colors.as_deref());
    } else {
        for c in ordered_courses(app, &courses, false) {
            let id = fmt::id(&c["id"]);
            let name = c["course_code"].as_str().filter(|x| !x.is_empty()).or(c["name"].as_str()).unwrap_or("").to_string();
            let full = fmt::s(&c["name"]);
            nav_link(app, ui, &format!("course-{id}"), &format!("#/c/{id}/modules"), Some(color_for(&id, colors.as_deref())), &name, Some(&full), |_, _, _| {});
        }
    }
    past_section(app, ui, colors.as_deref());
}

fn edit_list(app: &mut App, ui: &mut Ui, courses: &[Value], colors: Option<&Value>) {
    let tk = t();
    let w = ui.available_width();
    let hidden = hidden_courses(app);
    let mut order: Vec<Value> = ordered_courses(app, courses, true);
    if let Some((_, live)) = &app.sidebar.drag {
        order.sort_by_key(|c| live.iter().position(|id| *id == fmt::id(&c["id"])).unwrap_or(usize::MAX));
    }
    let mut rects: Vec<(String, Rect)> = Vec::new();
    let mut toggled: Option<String> = None;
    let mut drag_started: Option<String> = None;
    let mut drag_ended = false;
    for c in &order {
        let id = fmt::id(&c["id"]);
        let name = c["course_code"].as_str().filter(|x| !x.is_empty()).or(c["name"].as_str()).unwrap_or("").to_string();
        let is_hidden = hidden.contains(&id);
        let (rect, resp) = ui.allocate_exact_size(vec2(w, 29.0), Sense::click_and_drag());
        rects.push((id.clone(), rect));
        let focused = app.sidebar.edit_focus.as_deref() == Some(id.as_str()) && app.panes.focused == Pane::Sidebar;
        let dragging = app.sidebar.drag.as_ref().map(|(d, _)| *d == id).unwrap_or(false);
        let hovered = resp.hovered();
        let alpha = if dragging { 0.4 } else { 1.0 };
        if hovered || focused {
            ui.painter().rect_filled(rect, cr(6.0), theme::alpha(tk.hover, alpha));
        }
        if focused {
            ui.painter().rect_stroke(rect, cr(6.0), Stroke::new(1.5, tk.accent), StrokeKind::Inside);
        }
        let text_c = if hovered || focused { tk.text } else { tk.muted };
        let mut x = rect.min.x + 2.0;
        crate::widgets::centered_text(ui, Rect::from_min_size(pos2(x, rect.min.y), vec2(10.0, rect.height())), "⠿", Ts::new(12.0, 400, theme::alpha(tk.faint, alpha)));
        x += 18.0;
        let fade = if is_hidden { 0.4 } else { 1.0 } * alpha;
        ui.painter().circle_filled(pos2(x + 4.0, rect.center().y), 4.0, theme::alpha(color_for(&id, colors), fade));
        x += 16.0;
        let ts = Ts::new(14.0, 400, theme::alpha(text_c, fade)).ul(false);
        let g = lay(ui, &name, ts, Some(rect.max.x - 6.0 - 22.0 - 8.0 - x), true);
        let gpos = pos2(x, rect.center().y - g.size().y / 2.0);
        if is_hidden {
            let y = gpos.y + g.size().y / 2.0;
            ui.painter().line_segment([pos2(gpos.x, y), pos2(gpos.x + g.size().x, y)], Stroke::new(1.0, theme::alpha(text_c, fade)));
        }
        ui.painter().galley(gpos, g, ts.color);
        // the eye: its slash draws in when the course is hidden
        let eye = Rect::from_min_size(pos2(rect.max.x - 6.0 - 22.0, rect.center().y - 10.0), vec2(22.0, 20.0));
        let eresp = ui.interact(eye, Id::new(("eye", &id)), Sense::click());
        if eresp.hovered() {
            ui.painter().rect_filled(eye, cr(4.0), tk.hover);
        }
        let ec = if eresp.hovered() { tk.text } else if is_hidden { tk.faint } else { tk.muted };
        let k = ui.ctx().animate_bool_with_time(Id::new(("eye-slash", &id)), is_hidden, 0.18);
        eye_icon(ui, Rect::from_center_size(eye.center(), vec2(15.0, 15.0)), ec, k);
        let eresp = eresp.on_hover_text(if is_hidden { "Show (Space)" } else { "Hide (Space)" });
        if eresp.clicked() {
            toggled = Some(id.clone());
        } else if resp.drag_started() {
            drag_started = Some(id.clone());
        } else if resp.clicked() {
            app.sidebar.edit_focus = Some(id.clone());
            app.panes.focused = Pane::Sidebar;
        }
        if resp.drag_stopped() {
            drag_ended = true;
        }
        let _ = resp.on_hover_text("Drag, or Alt+↑/↓ to move. Space hides or shows.").on_hover_cursor(CursorIcon::Grab);
    }
    if let Some(id) = toggled {
        toggle_hidden(app, &id);
    }
    if let Some(id) = drag_started {
        app.sidebar.drag = Some((id, order.iter().map(|c| fmt::id(&c["id"])).collect()));
    }
    // While dragging, the row moves to where the pointer is.
    if let (Some((did, live)), Some(p)) = (app.sidebar.drag.clone(), ui.ctx().pointer_interact_pos()) {
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        if let Some((over, r)) = rects.iter().find(|(_, r)| p.y >= r.min.y && p.y < r.max.y) {
            if *over != did {
                let mut l = live.clone();
                let from = l.iter().position(|x| *x == did).unwrap();
                l.remove(from);
                let to = l.iter().position(|x| x == over).unwrap();
                let at = if p.y < r.center().y { to } else { to + 1 };
                l.insert(at.min(l.len()), did.clone());
                app.sidebar.drag = Some((did, l));
            }
        }
    }
    if drag_ended || (app.sidebar.drag.is_some() && !ui.input(|i| i.pointer.any_down())) {
        if let Some((_, live)) = app.sidebar.drag.take() {
            save_order(app, live);
        }
    }
}

fn eye_icon(ui: &Ui, rect: Rect, color: Color32, slash: f32) {
    let s = rect.width() / 16.0;
    let at = |x: f32, y: f32| rect.min + vec2(x * s, y * s);
    let stroke = Stroke::new(1.4, color);
    // M1.5 8S4 3.5 8 3.5 14.5 8 14.5 8 12 12.5 8 12.5 1.5 8 1.5 8z
    let mut pts = Vec::new();
    for i in 0..=16 {
        let a = std::f32::consts::PI * i as f32 / 16.0;
        pts.push(at(8.0 - 6.5 * a.cos(), 8.0 - 4.5 * a.sin()));
    }
    for i in 0..=16 {
        let a = std::f32::consts::PI * i as f32 / 16.0;
        pts.push(at(8.0 + 6.5 * a.cos(), 8.0 + 4.5 * a.sin()));
    }
    ui.painter().add(egui::Shape::closed_line(pts, stroke));
    ui.painter().circle_stroke(at(8.0, 8.0), 2.0 * s, stroke);
    if slash > 0.0 {
        let a = at(2.5, 2.5);
        let b = at(13.5, 13.5);
        ui.painter().line_segment([a, a + (b - a) * slash], stroke);
    }
}

fn past_section(app: &mut App, ui: &mut Ui, colors: Option<&Value>) {
    let tk = t();
    let past = match app.d.need0("past_courses") {
        Ok(p) => p.as_array().cloned().unwrap_or_default(),
        Err(_) => return,
    };
    if past.is_empty() {
        return;
    }
    let w = ui.available_width();
    ui.add_space(10.0);
    // .section-toggle
    let (r, resp) = ui.allocate_exact_size(vec2(w, 30.0), Sense::click());
    let cursor = app.panes.focused == Pane::Sidebar && app.sidebar.cursor.as_deref() == Some("past-toggle");
    if resp.hovered() || cursor {
        ui.painter().rect_filled(r, cr(6.0), tk.hover);
    }
    if cursor {
        ui.painter().rect_filled(Rect::from_min_size(r.min, vec2(2.0, r.height())), 0.0, tk.accent);
    }
    let color = if resp.hovered() { tk.muted } else { tk.faint };
    let open = app.sidebar.past_open;
    crate::widgets::painter_text(ui, pos2(r.min.x + 8.0, r.center().y), Align2::LEFT_CENTER, if open { "▾" } else { "▸" }, Ts::new(10.0, 400, color));
    crate::widgets::painter_text(ui, pos2(r.min.x + 8.0 + 16.0, r.center().y), Align2::LEFT_CENTER, "Past courses", Ts::new(11.0, 400, color).up().sp(0.06));
    crate::widgets::painter_text(ui, pos2(r.max.x - 8.0, r.center().y), Align2::RIGHT_CENTER, &past.len().to_string(), Ts::new(11.0, 400, color));
    if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
        set_past_open(app, !open);
    }
    app.sidebar.items.push(("past-toggle".into(), None));
    if !open {
        return;
    }
    let mut sorted = past.clone();
    let start = |c: &Value| c["term"]["start_at"].as_str().or(c["start_at"].as_str()).unwrap_or("").to_string();
    sorted.sort_by_key(|c| std::cmp::Reverse(start(c)));
    let mut terms: Vec<(String, Vec<Value>)> = Vec::new();
    for c in sorted {
        let term = c["term"]["name"].as_str().filter(|x| !x.is_empty()).unwrap_or("Other").to_string();
        match terms.iter_mut().find(|(t, _)| *t == term) {
            Some((_, v)) => v.push(c),
            None => terms.push((term, vec![c])),
        }
    }
    for (term, cs) in terms {
        // .term-label: 11px faint, padding 8px 8px 2px 24px
        ui.add_space(8.0);
        let (tr, _) = ui.allocate_exact_size(vec2(w, 16.0), Sense::hover());
        crate::widgets::painter_text(ui, pos2(tr.min.x + 24.0, tr.min.y + 2.0), Align2::LEFT_TOP, &term, Ts::new(11.0, 400, tk.faint));
        ui.add_space(2.0);
        for c in cs {
            let id = fmt::id(&c["id"]);
            let name = c["course_code"].as_str().filter(|x| !x.is_empty()).or(c["name"].as_str()).unwrap_or("").to_string();
            let full = fmt::s(&c["name"]);
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(ui.cursor().min + vec2(16.0, 0.0), vec2(w - 16.0, 31.0))));
            nav_link(app, &mut child, &format!("course-{id}"), &format!("#/c/{id}/modules"), Some(color_for(&id, colors)), &name, Some(&full), |_, _, _| {});
            ui.allocate_space(vec2(w, 31.0));
        }
    }
}

fn footer(app: &mut App, ui: &mut Ui) {
    let tk = t();
    // #keys-hint: "? Shortcuts"
    let ks = crate::widgets::kbd_size(ui, "?", 11.0);
    let g = lay(ui, "Shortcuts", Ts::new(12.0, 400, tk.faint), None, false);
    let (r, resp) = ui.allocate_exact_size(vec2(ks.x + 6.0 + g.size().x, 18.0), Sense::click());
    let color = if resp.hovered() { tk.muted } else { tk.faint };
    paint_kbd(ui, pos2(r.min.x, r.center().y - ks.y / 2.0), "?", 11.0, None, None, None);
    ui.painter().galley(pos2(r.min.x + ks.x + 6.0, r.center().y - g.size().y / 2.0), g, color);
    if resp.on_hover_text("Keyboard shortcuts").on_hover_cursor(CursorIcon::PointingHand).clicked() {
        app.keys_open = true;
    }
    ui.add_space(6.0);
    // #sync-status
    let syncing = app.status["syncing"].as_bool().unwrap_or(false);
    let text = if syncing {
        "Syncing…".to_string()
    } else if let Some(ts) = app.status["last_sync"].as_f64() {
        format!("Synced {}", fmt::ago(ts))
    } else {
        "Not synced yet".to_string()
    };
    let g = lay(ui, &text, Ts::new(12.0, 400, tk.faint), None, false);
    let (r, resp) = ui.allocate_exact_size(vec2(12.0 + g.size().x, 18.0), Sense::click());
    let dot = if syncing {
        let time = ui.input(|i| i.time) as f32;
        let a = 0.3 + 0.7 * (1.0 - ((time % 2.0) - 1.0).abs());
        ui.ctx().request_repaint();
        theme::alpha(tk.warn, a)
    } else {
        tk.ok
    };
    ui.painter().circle_filled(pos2(r.min.x + 3.0, r.center().y), 3.0, dot);
    ui.painter().galley(pos2(r.min.x + 12.0, r.center().y - g.size().y / 2.0), g, tk.faint);
    if resp.on_hover_text("Click to sync now").on_hover_cursor(CursorIcon::PointingHand).clicked() {
        sync_now(app);
    }
}

pub fn sync_now(app: &App) {
    let e = app.svc.engine.clone();
    app.fire(async move { e.sync_once().await });
}
