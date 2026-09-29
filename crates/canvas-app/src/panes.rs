//! Three panes: the sidebar and main for getting around, and the viewer, where pages, files,
//! assignments, discussions and messages open, in tabs. Moving through a list with the keyboard
//! shows the item in a preview tab, drawn in italics; opening it (Enter, a click) keeps it.
//! Keys (nav.rs): H/L move between panes, J/K resize the one you're in, z hides it.

use std::collections::HashSet;
use std::time::Instant;

use egui::{Color32, CursorIcon, Id, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use serde_json::{Value, json};

use crate::app::{App, Pane, PaneDraw};
use crate::data::Need;
use crate::prefs::Prefs;
use crate::route::{self, is_doc, path_of};
use crate::theme::{self, t};
use crate::widgets::{Ts, cr, lay};

pub const RESIZE_STEP: f32 = 40.0;
pub const SIDEBAR_MIN: f32 = 160.0;
pub const SIDEBAR_MAX: f32 = 420.0;
pub const VIEWER_MIN: f32 = 280.0;
pub const MAIN_MIN: f32 = 320.0;
pub const SIDEBAR_DEFAULT: f32 = 248.0;
pub const VIEWER_DEFAULT: f32 = 480.0;

pub struct Tab {
    pub id: u64,
    pub stack: Vec<String>,
    pub i: usize,
    pub pinned: bool,
    pub title: String,
    pub scroll: f32,
}

impl Tab {
    pub fn href(&self) -> &str {
        &self.stack[self.i]
    }
}

pub struct Panes {
    pub sidebar_w: f32,
    pub viewer_w: f32,
    pub hidden: HashSet<String>,
    pub open_sidebar: bool,
    pub open_main: bool,
    pub open_viewer: bool,
    pub focused: Pane,
    pub tabs: Vec<Tab>,
    pub active: Option<u64>,
    pub seq: u64,
    pub side_w_now: f32,
    pub view_w_now: f32,
    pub preview_at: Option<(String, Instant)>,
    pub titling: HashSet<String>,
}

impl Panes {
    pub fn load(prefs: &Prefs) -> Panes {
        let mut p = Panes {
            sidebar_w: prefs.f32("sidebarWidth", SIDEBAR_DEFAULT),
            viewer_w: prefs.f32("viewerWidth", VIEWER_DEFAULT),
            hidden: prefs.strings("hiddenPanes").into_iter().collect(),
            open_sidebar: true,
            open_main: true,
            open_viewer: false,
            focused: Pane::Main,
            tabs: Vec::new(),
            active: None,
            seq: 0,
            side_w_now: SIDEBAR_DEFAULT,
            view_w_now: 0.0,
            preview_at: None,
            titling: HashSet::new(),
        };
        for t in prefs.get("viewerTabs").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
            let h = t["h"].as_str().unwrap_or("").to_string();
            if is_doc(&h) {
                p.seq += 1;
                p.tabs.push(Tab { id: p.seq, stack: vec![h], i: 0, pinned: true, title: t["t"].as_str().unwrap_or("").to_string(), scroll: 0.0 });
            }
        }
        let a = prefs.f32("viewerActive", 0.0) as usize;
        p.active = p.tabs.get(a.min(p.tabs.len().saturating_sub(1))).map(|t| t.id);
        p
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.active.and_then(|id| self.tabs.iter().find(|t| t.id == id))
    }
    pub fn active_mut(&mut self) -> Option<&mut Tab> {
        let id = self.active?;
        self.tabs.iter_mut().find(|t| t.id == id)
    }
    fn index_of(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }
    fn new_tab(&mut self, href: &str, pinned: bool) -> Tab {
        self.seq += 1;
        Tab { id: self.seq, stack: vec![href.to_string()], i: 0, pinned, title: String::new(), scroll: 0.0 }
    }
}

pub fn save_tabs(app: &mut App) {
    let kept: Vec<&Tab> = app.panes.tabs.iter().filter(|t| t.pinned).collect();
    let tabs: Vec<Value> = kept.iter().map(|t| json!({"h": t.href(), "t": t.title})).collect();
    let active = kept.iter().position(|t| Some(t.id) == app.panes.active).unwrap_or(0);
    app.set_pref("viewerTabs", Value::Array(tabs));
    app.set_pref("viewerActive", json!(active));
}

// --- documents: what opens in the viewer ------------------------------------------------------------
/// The list a document belongs in, for main to show beside it.
pub fn doc_parent(app: &App, href: &str) -> Option<String> {
    let p = path_of(href);
    let re = regex::Regex::new(r"^/c/(\d+)/([adpf])/").unwrap();
    if let Some(m) = re.captures(&p) {
        let (cid, k) = (&m[1], &m[2]);
        if k != "d" {
            let tab = match k {
                "a" => "assignments",
                "p" => "pages",
                _ => "files",
            };
            return Some(format!("#/c/{cid}/{tab}"));
        }
        let tid = p.rsplit('/').next().unwrap_or("");
        let ann = app.d.peek(&format!("course_announcements:{cid}")).map(|v| v.as_array().map(|a| a.iter().any(|x| crate::fmt::id(&x["id"]) == tid)).unwrap_or(false)).unwrap_or(false);
        return Some(format!("#/c/{cid}/{}", if ann { "announcements" } else { "discussions" }));
    }
    p.starts_with("/inbox/").then(|| "#/inbox".to_string())
}

fn activate(app: &mut App, id: u64) {
    if app.panes.active == Some(id) {
        return;
    }
    let scroll = app.viewer.scroll;
    if let Some(t) = app.panes.active_mut() {
        t.scroll = scroll;
    }
    app.panes.active = Some(id);
    save_tabs(app);
}

/// Open an item and keep it (Enter, a click): its preview tab stays, or it gets a new tab.
pub fn open_doc(app: &mut App, href: &str) {
    if app.panes.hidden.contains("viewer") {
        set_pane_hidden(app, "viewer", false);
    }
    let p = &mut app.panes;
    let id = match p.tabs.iter().find(|t| t.href() == href) {
        Some(t) => t.id,
        None => {
            let t = p.new_tab(href, true);
            let id = t.id;
            let at = p.active.and_then(|a| p.index_of(a)).map(|i| i + 1).unwrap_or(p.tabs.len());
            p.tabs.insert(at, t);
            id
        }
    };
    if let Some(t) = p.tabs.iter_mut().find(|t| t.id == id) {
        t.pinned = true;
    }
    if app.panes.active == Some(id) {
        save_tabs(app);
        return;
    }
    activate(app, id);
}

pub fn close_tab(app: &mut App, id: Option<u64>) {
    let Some(id) = id.or(app.panes.active) else { return };
    let Some(i) = app.panes.index_of(id) else { return };
    app.panes.tabs.remove(i);
    if app.panes.active == Some(id) {
        app.panes.active = app.panes.tabs.get(i).or_else(|| i.checked_sub(1).and_then(|j| app.panes.tabs.get(j))).map(|t| t.id);
        app.viewer.shown = None;
    }
    if app.panes.tabs.is_empty() {
        if app.panes.hidden.contains("main") {
            set_pane_hidden(app, "main", false);
        }
        if app.panes.focused == Pane::Viewer {
            focus_pane(app, Pane::Main);
        }
    }
    save_tabs(app);
}

pub fn switch_tab(app: &mut App, d: i64) {
    let n = app.panes.tabs.len() as i64;
    if n == 0 {
        return;
    }
    let i = app.panes.active.and_then(|a| app.panes.index_of(a)).unwrap_or(0) as i64;
    let id = app.panes.tabs[((i + d + n) % n) as usize].id;
    activate(app, id);
}

/// A link inside the viewer opens in the same tab, which keeps its own back and forward.
pub fn navigate_tab(app: &mut App, href: &str) {
    let scroll = app.viewer.scroll;
    let Some(t) = app.panes.active_mut() else { return open_doc(app, href) };
    t.scroll = scroll;
    t.stack.truncate(t.i + 1);
    t.stack.push(href.to_string());
    t.i += 1;
    t.pinned = true;
    save_tabs(app);
}

pub fn tab_history(app: &mut App, d: i64) {
    let Some(t) = app.panes.active_mut() else { return };
    let j = t.i as i64 + d;
    if j < 0 || j as usize >= t.stack.len() {
        return;
    }
    t.scroll = 0.0;
    t.i = j as usize;
    save_tabs(app);
}

// --- layout ------------------------------------------------------------------------------------------
pub fn set_pane_hidden(app: &mut App, p: &str, hide: bool) {
    if hide {
        app.panes.hidden.insert(p.to_string());
    } else {
        app.panes.hidden.remove(p);
    }
    let v: Vec<Value> = app.panes.hidden.iter().map(|x| json!(x)).collect();
    app.set_pref("hiddenPanes", Value::Array(v));
}

fn pane_name(p: Pane) -> &'static str {
    match p {
        Pane::Sidebar => "sidebar",
        Pane::Main => "main",
        Pane::Viewer => "viewer",
    }
}

pub fn is_open(app: &App, p: Pane) -> bool {
    match p {
        Pane::Sidebar => app.panes.open_sidebar,
        Pane::Main => app.panes.open_main,
        Pane::Viewer => app.panes.open_viewer,
    }
}

/// z: hide the pane you're in (never the last one showing a page), and move to its neighbour.
pub fn toggle_pane(app: &mut App, p: Pane) {
    if !is_open(app, p) {
        set_pane_hidden(app, pane_name(p), false);
        focus_pane(app, p);
        return;
    }
    if p == Pane::Main && !app.panes.open_viewer {
        return;
    }
    if p == Pane::Viewer && app.panes.tabs.is_empty() {
        return;
    }
    set_pane_hidden(app, pane_name(p), true);
    if app.panes.focused == p {
        focus_pane(app, if p == Pane::Main { Pane::Viewer } else { Pane::Main });
    }
}

/// H / L: the pane to the left or right, showing it again if it was hidden.
pub fn move_pane(app: &mut App, d: i64) {
    let order = [Pane::Sidebar, Pane::Main, Pane::Viewer];
    let i = order.iter().position(|p| *p == app.panes.focused).unwrap_or(1) as i64 + d;
    if i < 0 || i > 2 {
        return;
    }
    let next = order[i as usize];
    if next == Pane::Viewer && app.panes.tabs.is_empty() {
        return;
    }
    if !is_open(app, next) {
        set_pane_hidden(app, pane_name(next), false);
    }
    focus_pane(app, next);
}

/// J / K: narrower and wider. Main grows by taking from the viewer (or the sidebar when there's none).
pub fn resize_pane(app: &mut App, p: Pane, d: f32, screen_w: f32) {
    let (side, view) = (app.panes.side_w_now, app.panes.view_w_now);
    match p {
        Pane::Sidebar => set_sidebar_width(app, side + d, true),
        Pane::Viewer => {
            if app.panes.open_main {
                set_viewer_width(app, view + d, true, screen_w)
            }
        }
        Pane::Main => {
            if app.panes.open_viewer {
                set_viewer_width(app, view - d, true, screen_w)
            } else if app.panes.open_sidebar {
                set_sidebar_width(app, side - d, true)
            }
        }
    }
}

pub fn set_sidebar_width(app: &mut App, w: f32, save: bool) {
    app.panes.sidebar_w = w.round().clamp(SIDEBAR_MIN, SIDEBAR_MAX);
    if save {
        let w = app.panes.sidebar_w;
        app.set_pref("sidebarWidth", json!(w));
    }
}

pub fn set_viewer_width(app: &mut App, w: f32, save: bool, screen_w: f32) {
    let side = if app.panes.open_sidebar { app.panes.sidebar_w } else { 0.0 };
    app.panes.viewer_w = w.round().clamp(VIEWER_MIN, (screen_w - side - MAIN_MIN).max(VIEWER_MIN));
    if save {
        let w = app.panes.viewer_w;
        app.set_pref("viewerWidth", json!(w));
    }
}

pub fn focus_pane(app: &mut App, p: Pane) {
    app.panes.focused = p;
    if p == Pane::Sidebar && app.sidebar.cursor.is_none() {
        app.sidebar.cursor = app.sidebar.active_href.clone();
    }
}

// --- the pane buttons ---------------------------------------------------------------------------------
/// One icon per pane, each the window's three columns with its own marked.
fn pane_icon(ui: &Ui, rect: Rect, p: Pane, off: bool, color: Color32) {
    let s = rect.width() / 16.0;
    let at = |x: f32, y: f32| rect.min + vec2(x * s, y * s);
    let stroke = Stroke::new(1.3, color);
    let painter = ui.painter();
    let frame = Rect::from_min_max(at(1.75, 2.75), at(14.25, 13.25));
    painter.rect_stroke(frame, cr(2.0 * s), stroke, StrokeKind::Middle);
    let v = |x: f32| painter.line_segment([at(x, 2.75), at(x, 13.25)], stroke);
    let chev = |pts: [(f32, f32); 3], flip_about: f32| {
        let p: Vec<egui::Pos2> = pts.iter().map(|(x, y)| if off { at(2.0 * flip_about - x, *y) } else { at(*x, *y) }).collect();
        painter.add(egui::Shape::line(p, stroke));
    };
    match p {
        Pane::Sidebar => {
            v(6.25);
            chev([(10.5, 6.5), (9.0, 8.0), (10.5, 9.5)], 9.75);
        }
        Pane::Main => {
            v(5.25);
            v(10.75);
            painter.rect_filled(Rect::from_min_size(at(6.5, 4.5), vec2(3.0 * s, 7.0 * s)), cr(0.6 * s), theme::alpha(color, 0.45));
        }
        Pane::Viewer => {
            v(9.75);
            chev([(5.5, 6.5), (7.0, 8.0), (5.5, 9.5)], 6.25);
        }
    }
}

/// The buttons that live in `host`'s corner: its own, and those of hidden neighbours.
pub fn pane_buttons(app: &mut App, ui: &mut Ui, host: Pane) {
    let exists = |app: &App, p: Pane| p == Pane::Sidebar || !app.panes.tabs.is_empty();
    let neighbours = |p: Pane| match p {
        Pane::Sidebar => [Pane::Main, Pane::Viewer],
        Pane::Main => [Pane::Viewer, Pane::Sidebar],
        Pane::Viewer => [Pane::Main, Pane::Sidebar],
    };
    let host_of = |app: &App, p: Pane| if is_open(app, p) { Some(p) } else { neighbours(p).into_iter().find(|q| is_open(app, *q)) };
    let here: Vec<Pane> = [Pane::Sidebar, Pane::Main, Pane::Viewer].into_iter().filter(|p| exists(app, *p) && host_of(app, *p) == Some(host)).collect();
    let tk = t();
    for p in here {
        let open = is_open(app, p);
        let stuck = p == Pane::Main && open && !app.panes.open_viewer; // something has to show a page
        let name = match p {
            Pane::Sidebar => "the sidebar",
            Pane::Main => "the main view",
            Pane::Viewer => "the viewer",
        };
        let label = if stuck { "The main view hides only while the viewer is open".to_string() } else { format!("{} {name}{}", if open { "Hide" } else { "Show" }, if open { " (z)" } else { "" }) };
        let (rect, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), if stuck { Sense::hover() } else { Sense::click() });
        let hovered = resp.hovered() && !stuck;
        if hovered {
            ui.painter().rect_filled(rect, cr(5.0), tk.hover);
        }
        let mut color = if hovered { tk.text } else { tk.faint };
        if !open {
            color = theme::alpha(color, 0.6);
        }
        if stuck {
            color = theme::alpha(color, 0.3);
        }
        // the chevron turns over .15s
        let flip = ui.ctx().animate_bool_with_time(Id::new(("pane-chev", pane_name(p))), !open, 0.15) > 0.5;
        pane_icon(ui, Rect::from_center_size(rect.center(), vec2(15.0, 15.0)), p, flip, color);
        let resp = resp.on_hover_text(label);
        if resp.clicked() {
            if open {
                app.panes.focused = p;
                toggle_pane(app, p);
            } else {
                set_pane_hidden(app, pane_name(p), false);
                focus_pane(app, p);
            }
        }
        if !stuck {
            resp.on_hover_cursor(CursorIcon::PointingHand);
        }
        ui.add_space(1.0);
    }
}

// --- drawing a pane's page ----------------------------------------------------------------------------
fn render_route(app: &mut App, ui: &mut Ui, hash: &str, pane: Pane) -> Result<(), Need> {
    let route = route::parse(hash);
    crate::views::draw(app, ui, &route, pane)
}

/// Draw what the pane wants; while that's loading, what it showed before (up to 150ms), then
/// "Loading…". Returns true when the wanted page is on screen.
fn render_pane(app: &mut App, ui: &mut Ui, pane: Pane) -> bool {
    let want = match pane {
        Pane::Viewer => app.panes.active_tab().map(|t| t.href().to_string()).unwrap_or_default(),
        _ => app.main.want.clone(),
    };
    let outer = app.d.begin_deps();
    let saved_out = std::mem::take(&mut state(app, pane).out);
    state(app, pane).items.clear();
    let res = render_route(app, ui, &want, pane);
    let deps = app.d.end_deps(outer);
    match res {
        Ok(()) => {
            let st = state(app, pane);
            st.deps = deps;
            let first = st.shown.as_deref() != Some(want.as_str());
            if first {
                st.shown = Some(want.clone());
                crate::nav::after_render(app, pane, &want);
            }
            true
        }
        Err(Need::Pending) => {
            state(app, pane).out = saved_out;
            let since = state(app, pane).since;
            let shown = state(app, pane).shown.clone();
            let early = since.elapsed().as_millis() < 150;
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(40));
            if let (true, Some(old)) = (early, shown.filter(|s| *s != want)) {
                let outer = app.d.begin_deps();
                let ok = render_route(app, ui, &old, pane).is_ok();
                app.d.end_deps(outer);
                if ok {
                    return false;
                }
            }
            crate::widgets::skeleton(ui);
            false
        }
        Err(Need::Err(e)) => {
            state(app, pane).deps = deps;
            if e.kind() == "setup" && pane == Pane::Main {
                crate::nav::go(app, "#/welcome");
                return false;
            }
            crate::views::error_page(app, ui, &e, pane);
            let st = state(app, pane);
            if st.shown.as_deref() != Some(want.as_str()) {
                st.shown = Some(want.clone());
                crate::nav::after_render(app, pane, &want);
            }
            true
        }
    }
}

pub fn state(app: &mut App, pane: Pane) -> &mut PaneDraw {
    if pane == Pane::Viewer { &mut app.viewer } else { &mut app.main }
}

/// A scrolling pane body with programmatic scrolling (restores, anchors, j/k).
fn scroll_body(app: &mut App, ui: &mut Ui, pane: Pane, id: &str, add: impl FnOnce(&mut App, &mut Ui)) {
    let st = state(app, pane);
    let mut area = egui::ScrollArea::vertical().id_salt(id).auto_shrink([false, false]);
    if let Some(y) = st.scroll_to.take() {
        area = area.vertical_scroll_offset(y.max(0.0));
    }
    let out = area.show(ui, |ui| {
        let top = ui.min_rect().min.y;
        add(app, ui);
        top
    });
    let st = state(app, pane);
    st.scroll = out.state.offset.y;
    st.viewport_h = out.inner_rect.height();
    st.content_top = out.inner;
    let (top, view_h) = (out.inner, out.inner_rect.height());
    crate::search::resolve(app, pane, top, view_h);
    let st = state(app, pane);
    if let Some(r) = st.scroll_to_rect.take() {
        // scrollIntoView({block: "nearest"})
        let view = out.inner_rect;
        let y = if r.min.y < view.min.y { Some(st.scroll + r.min.y - view.min.y - 16.0) } else if r.max.y > view.max.y { Some(st.scroll + r.max.y - view.max.y + 16.0) } else { None };
        if let Some(y) = y {
            st.scroll_to = Some(y.max(0.0));
            ui.ctx().request_repaint();
        }
    }
}

pub fn draw_main(app: &mut App, ui: &mut Ui, rect: Rect, bare: bool, scale: f32) {
    let _ = scale;
    let pane_rect = rect;
    if ui.rect_contains_pointer(pane_rect) && ui.input(|i| i.pointer.any_pressed()) {
        app.panes.focused = Pane::Main;
    }
    scroll_body(app, ui, Pane::Main, "main-scroll", |app, ui| {
        let w = ui.available_width();
        let col = w.min(920.0);
        let x0 = ui.min_rect().min.x + (w - col) / 2.0;
        // #topbar: padding 10px 40px 0 34px
        if !bare {
            let top = ui.cursor().min.y;
            let bar = Rect::from_min_size(pos2(x0 + 34.0, top + 10.0), vec2(col - 74.0, 26.0));
            let mut bui = ui.new_child(egui::UiBuilder::new().max_rect(bar).layout(egui::Layout::left_to_right(egui::Align::Center)));
            topbar(app, &mut bui);
            ui.allocate_space(vec2(w, 36.0));
        }
        // #view: padding 14px 40px 80px
        let top = ui.cursor().min.y + 14.0;
        let inner = Rect::from_min_size(pos2(x0 + 40.0, top), vec2(col - 80.0, f32::INFINITY));
        let mut vui = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)).id_salt("view"));
        render_pane(app, &mut vui, Pane::Main);
        let used = vui.min_rect();
        ui.allocate_space(vec2(w, (used.max.y - ui.cursor().min.y).max(0.0) + 80.0));
    });
}

/// Back and forward, then the view's breadcrumbs.
fn topbar(app: &mut App, ui: &mut Ui) {
    pane_buttons(app, ui, Pane::Main);
    ui.add_space(4.0);
    let tk = t();
    for (label, back) in [("←", true), ("→", false)] {
        let enabled = if back { app.nav.can_back() } else { app.nav.can_forward() };
        let (rect, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), if enabled { Sense::click() } else { Sense::hover() });
        if resp.hovered() && enabled {
            ui.painter().rect_filled(rect, cr(6.0), tk.hover);
        }
        let color = if !enabled { theme::alpha(tk.faint, 0.45) } else if resp.hovered() { tk.text } else { tk.muted };
        crate::widgets::centered_text(ui, rect, label, Ts::new(15.0, 400, color));
        let resp = resp.on_hover_text(if back { "Back (Alt ←)" } else { "Forward (Alt →)" });
        if resp.clicked() {
            if back { crate::nav::back(app) } else { crate::nav::forward(app) }
        }
        if enabled {
            resp.on_hover_cursor(CursorIcon::PointingHand);
        }
        ui.add_space(2.0);
    }
    ui.add_space(8.0);
    let out = app.main.out.clone();
    crate::views::crumbs_line(app, ui, &out, Pane::Main);
}

pub fn draw_viewer(app: &mut App, ui: &mut Ui, rect: Rect) {
    let tk = t();
    if ui.rect_contains_pointer(rect) && ui.input(|i| i.pointer.any_pressed()) {
        app.panes.focused = Pane::Viewer;
    }
    // .viewer-head: padding 8px 10px 0 8px, border-bottom 1px line
    let head_h = 8.0 + 30.0;
    let head = Rect::from_min_size(rect.min, vec2(rect.width(), head_h));
    // the pane buttons sit 5px above the border; the tabs stand on it
    let btns = Rect::from_min_max(pos2(head.min.x + 8.0, head.max.y - 5.0 - 24.0), pos2(head.max.x - 10.0, head.max.y - 5.0));
    let mut bui = ui.new_child(egui::UiBuilder::new().max_rect(btns).layout(egui::Layout::left_to_right(egui::Align::Center)).id_salt("viewer-btns"));
    pane_buttons(app, &mut bui, Pane::Viewer);
    let tabs_x = bui.min_rect().max.x + 4.0;
    let strip = Rect::from_min_max(pos2(tabs_x, head.max.y - 30.0), pos2(head.max.x - 10.0, head.max.y));
    tabs_strip(app, ui, strip);
    // #viewer-body: padding 14px 24px 60px
    let body = Rect::from_min_max(pos2(rect.min.x, head.max.y), rect.max);
    let mut bui = ui.new_child(egui::UiBuilder::new().max_rect(body).id_salt("viewer-body"));
    bui.set_clip_rect(body);
    if app.panes.active_tab().is_none() {
        ui.painter().hline(rect.x_range(), head.max.y - 0.5, Stroke::new(1.0, tk.line));
        return;
    }
    // Switching tabs puts back where you were in that tab.
    let want = app.panes.active_tab().map(|t| format!("{}:{}", t.id, t.href())).unwrap_or_default();
    if app.viewer.want != want {
        app.viewer.want = want.clone();
        app.viewer.since = Instant::now();
        app.viewer.scroll_to = Some(app.panes.active_tab().map(|t| t.scroll).unwrap_or(0.0));
    }
    scroll_body(app, &mut bui, Pane::Viewer, "viewer-scroll", |app, ui| {
        let w = ui.available_width();
        let inner = Rect::from_min_size(ui.cursor().min + vec2(24.0, 14.0), vec2(w - 48.0, f32::INFINITY));
        let mut vui = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)).id_salt("vview"));
        render_pane(app, &mut vui, Pane::Viewer);
        let used = vui.min_rect();
        ui.allocate_space(vec2(w, (used.max.y - ui.cursor().min.y).max(0.0) + 60.0));
    });
    // the header's border, over whatever scrolled under it
    ui.painter().hline(rect.x_range(), head.max.y - 0.5, Stroke::new(1.0, tk.line));
    // A tab gets its title when drawn; a pinned one waiting in the background looks it up.
    crate::views::fill_titles(app);
}

/// The tab strip: click to switch, middle-click or × to close, double-click to keep a preview.
fn tabs_strip(app: &mut App, ui: &mut Ui, strip: Rect) {
    let tk = t();
    let mut sui = ui.new_child(egui::UiBuilder::new().max_rect(strip).layout(egui::Layout::left_to_right(egui::Align::Min)).id_salt("viewer-tabs"));
    sui.set_clip_rect(strip.intersect(ui.clip_rect()));
    let mut act: Option<(u64, &str)> = None;
    egui::ScrollArea::horizontal().id_salt("vtabs").scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(&mut sui, |ui| {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing = vec2(2.0, 0.0);
            let ids: Vec<(u64, String, bool, bool)> = app.panes.tabs.iter().map(|t| (t.id, t.title.clone(), t.pinned, Some(t.id) == app.panes.active)).collect();
            for (id, title, pinned, on) in ids {
                let title = if title.is_empty() { "Loading…".to_string() } else { title };
                let ts = Ts::new(12.5, 400, if on { tk.text } else { tk.muted }).it(!pinned);
                let g = lay(ui, &title, ts, Some(200.0 - 10.0 - 4.0 - 22.0), true);
                let w = (g.size().x + 10.0 + 4.0 + 22.0 + 4.0).min(200.0);
                let (rect, resp) = ui.allocate_exact_size(vec2(w, 30.0), Sense::click());
                let hovered = resp.hovered();
                if on || hovered {
                    ui.painter().rect_filled(rect, egui::CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }, tk.hover);
                }
                let color = if on || hovered { tk.text } else { tk.muted };
                ui.painter().galley(pos2(rect.min.x + 10.0, rect.center().y - g.size().y / 2.0 - 0.5), g, color);
                if on {
                    ui.painter().rect_filled(Rect::from_min_max(pos2(rect.min.x, rect.max.y - 2.0), rect.max), 0.0, tk.accent);
                }
                // × button
                let x_rect = Rect::from_center_size(pos2(rect.max.x - 14.0, rect.center().y), vec2(18.0, 18.0));
                let x_resp = ui.interact(x_rect, Id::new(("vtab-x", id)), Sense::click());
                if on || hovered || x_resp.hovered() {
                    if x_resp.hovered() {
                        ui.painter().rect_filled(x_rect, cr(4.0), tk.hover);
                    }
                    crate::widgets::centered_text(ui, x_rect, "×", Ts::new(14.0, 400, if x_resp.hovered() { tk.text } else { tk.faint }));
                }
                let tip = if pinned { title.clone() } else { format!("{title} (preview: open it to keep it)") };
                let resp = resp.on_hover_text(tip);
                if x_resp.clicked() || resp.middle_clicked() {
                    act = Some((id, "close"));
                } else if resp.double_clicked() {
                    act = Some((id, "pin"));
                } else if resp.clicked() {
                    act = Some((id, "activate"));
                }
                // a tab that just became active scrolls into view
                if on {
                    let last = Id::new("vtabs-shown");
                    if ui.data(|d| d.get_temp::<u64>(last)) != Some(id) {
                        ui.data_mut(|d| d.insert_temp(last, id));
                        ui.scroll_to_rect(rect, None);
                    }
                }
            }
        });
    });
    match act {
        Some((id, "close")) => close_tab(app, Some(id)),
        Some((id, "pin")) => {
            if let Some(t) = app.panes.tabs.iter_mut().find(|t| t.id == id) {
                t.pinned = true;
            }
            save_tabs(app);
        }
        Some((id, _)) => {
            activate(app, id);
            app.panes.focused = Pane::Viewer;
        }
        None => {}
    }
}
