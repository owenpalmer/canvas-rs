//! The pages: each gathers what it needs first (so a page that's still loading draws nothing), then
//! draws. Shared here: dispatch, breadcrumbs, the list row, course sections.

mod canvas;
mod course;
mod detail;

use egui::{Align2, Color32, CursorIcon, Id, Rect, Sense, Stroke, Ui, pos2, vec2};
use serde_json::Value;

use crate::app::{App, Item, Pane};
use crate::data::{Json, Need};
use crate::fmt::{self};
use crate::route::{Route, View};
use crate::theme::{self, t};
use crate::widgets::{self as w, Pill, Ts, cr, lay};

pub use canvas::course_info;

/// Draw a route's page into `ui` (nothing is drawn if it returns an error).
pub fn draw(app: &mut App, ui: &mut Ui, r: &Route, pane: Pane) -> Result<(), Need> {
    match &r.view {
        View::Dashboard => canvas::dashboard(app, ui, pane),
        View::Inbox => detail::inbox(app, ui, pane),
        View::Conversation(id) => detail::conversation(app, ui, pane, id),
        View::Modules(c) => course::modules(app, ui, pane, c),
        View::Assignments(c) => course::assignments(app, ui, pane, c),
        View::Assignment(c, a) => detail::assignment(app, ui, pane, c, a),
        View::Grades(c) => course::grades(app, ui, pane, c),
        View::Announcements(c) => course::announcements(app, ui, pane, c),
        View::Discussions(c) => course::discussions(app, ui, pane, c),
        View::Topic(c, tid) => detail::topic(app, ui, pane, c, tid),
        View::Pages(c) => course::pages(app, ui, pane, c),
        View::Page(c, slug) => detail::page(app, ui, pane, c, slug),
        View::Files(c) => course::files(app, ui, pane, c),
        View::File(c, f) => detail::file(app, ui, pane, c.as_deref(), f),
        View::Syllabus(c) => course::syllabus(app, ui, pane, c),
        View::Welcome => crate::setup::welcome(app, ui, pane),
        View::Settings => crate::settings::view(app, ui, pane),
        View::Notebooks => crate::notebooks::list_view(app, ui, pane),
        View::NotebookNew => crate::notebooks::builder(app, ui, pane, None, &r.query),
        View::NotebookAdd(id) => crate::notebooks::builder(app, ui, pane, Some(id), &r.query),
        View::Notebook(id) => crate::notebooks::detail(app, ui, pane, id),
        View::Anki => crate::anki::decks_view(app, ui, pane),
        View::AnkiImport => crate::anki::import_view(app, ui, pane),
        View::AnkiDeck(d) => crate::anki::review_view(app, ui, pane, *d),
        View::NotFound => {
            out(app, pane).title = "Not found".into();
            h1(ui, pane, "Not found");
            ui.add_space(14.0);
            if w::linklike(ui, "Back to dashboard", 14.0).clicked() {
                crate::nav::go(app, "#/");
            }
            Ok(())
        }
    }
}

/// Warm the resources a page needs (hovering a link to it).
pub fn prefetch(app: &mut App, r: &Route) {
    let d = &app.d;
    let _ = match &r.view {
        View::Dashboard => {
            for n in ["self", "courses", "colors", "planner", "announcements"] {
                let _ = d.need0(n);
            }
            Ok(())
        }
        View::Inbox => d.need0("inbox").map(|_| ()),
        View::Conversation(id) => {
            let _ = d.need0("inbox");
            d.need1("conversation", id).map(|_| ())
        }
        View::Modules(c) | View::Assignments(c) | View::Grades(c) | View::Announcements(c) | View::Discussions(c) | View::Pages(c) | View::Files(c) | View::Syllabus(c) => {
            let _ = (d.need0("courses"), d.need0("colors"), d.need1("tabs", c));
            let name = match &r.view {
                View::Modules(_) => "modules",
                View::Assignments(_) | View::Grades(_) => "groups",
                View::Announcements(_) => "course_announcements",
                View::Discussions(_) => "discussions",
                View::Pages(_) => "pages",
                View::Files(_) => "files",
                _ => "syllabus",
            };
            d.need1(name, c).map(|_| ())
        }
        View::Assignment(c, _) => d.need1("groups", c).map(|_| ()),
        View::Page(c, slug) => d.need("page", &[c.clone(), slug.clone()]).map(|_| ()),
        View::Topic(c, tid) => {
            let _ = (d.need1("course_announcements", c), d.need1("discussions", c));
            d.need("topic", &[c.clone(), tid.clone()]).map(|_| ())
        }
        View::File(c, f) => match c {
            Some(c) => d.need1("files", c).map(|_| ()),
            None => d.need1("file", f).map(|_| ()),
        },
        _ => Ok(()),
    };
}

/// The error pages: a sign-in card for an expired session, else what went wrong.
pub fn error_page(app: &mut App, ui: &mut Ui, e: &canvas_mcp::services::ApiErr, pane: Pane) {
    if e.kind() == "session" || e.kind() == "permission" {
        out(app, pane).title = "Sign in to see this".into();
        h1(ui, pane, "Sign in to see this");
        w::sub(ui, "It isn't saved on this computer yet.");
        crate::setup::signin_card(app, ui, "canvas", false);
    } else {
        out(app, pane).title = "Couldn't load this".into();
        h1(ui, pane, "Couldn't load this");
        w::sub(ui, &e.message());
    }
}

pub fn out(app: &mut App, pane: Pane) -> &mut crate::app::Out {
    &mut crate::panes::state(app, pane).out
}

/// h1 (21px in the viewer).
pub fn h1(ui: &mut Ui, pane: Pane, text: &str) -> Rect {
    let ts = if pane == Pane::Viewer { w::h1_ts().c(t().text) } else { w::h1_ts() };
    let ts = if pane == Pane::Viewer { Ts { size: 21.0, ..ts } } else { ts };
    let r = w::text_block(ui, text, ts).rect;
    ui.add_space(4.0);
    r
}

/// Set the page's title and breadcrumbs; in the viewer the crumbs are drawn in the page, in main
/// they go up into the bar beside the arrows.
pub fn head(app: &mut App, ui: &mut Ui, pane: Pane, title: &str, crumbs: Vec<(String, Option<String>)>, pill: Option<String>) {
    let o = out(app, pane);
    o.title = title.to_string();
    o.crumbs = crumbs;
    o.crumb_pill = pill;
    if pane == Pane::Viewer && !o.crumbs.is_empty() {
        let o = o.clone();
        crumbs_line(app, ui, &o, pane);
        ui.add_space(6.0);
    }
}

/// .crumbs: 13px muted, links underline on hover.
pub fn crumbs_line(app: &mut App, ui: &mut Ui, o: &crate::app::Out, pane: Pane) {
    if o.crumbs.is_empty() {
        return;
    }
    let tk = t();
    let mut go: Option<String> = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let n = o.crumbs.len();
        for (i, (label, href)) in o.crumbs.iter().enumerate() {
            let ts = Ts::new(13.0, 400, tk.muted);
            match href {
                Some(h) => {
                    let resp = w::text_button(ui, label, ts, ts.c(tk.text).ul(true));
                    if resp.hovered() {
                        crate::nav::prefetch(app, h);
                    }
                    if resp.clicked() {
                        go = Some(h.clone());
                    }
                }
                None => {
                    let g = lay(ui, label, ts, None, false);
                    let (r, _) = ui.allocate_exact_size(vec2(g.size().x, 19.5), Sense::hover());
                    ui.painter().galley(pos2(r.min.x, r.center().y - g.size().y / 2.0), g, tk.muted);
                }
            }
            if i + 1 < n {
                let g = lay(ui, " / ", ts, None, false);
                let (r, _) = ui.allocate_exact_size(vec2(g.size().x, 19.5), Sense::hover());
                ui.painter().galley(pos2(r.min.x, r.center().y - g.size().y / 2.0), g, tk.muted);
            }
        }
        if let Some(p) = &o.crumb_pill {
            ui.add_space(4.0);
            w::pill(ui, Pill::Plain, p);
        }
    });
    if let Some(h) = go {
        crate::nav::follow(app, &h, if pane == Pane::Viewer { Pane::Viewer } else { Pane::Main });
    }
}

pub fn dashboard_crumb() -> (String, Option<String>) {
    ("Dashboard".into(), Some("#/".into()))
}

// --- lists and rows -------------------------------------------------------------------------------------
/// A .list: rows in a panel with a border and rounded corners.
pub fn list<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let (r, rect) = w::boxed(ui, egui::Margin::ZERO, 8.0, |ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        add(ui)
    });
    let _ = rect;
    r
}

#[derive(Default)]
pub struct RowSpec {
    pub href: Option<String>,
    pub key: Option<String>,
    pub bar: Option<Color32>,
    pub kind: Option<String>,
    pub indent: f32,
    pub title: String,
    pub bold: bool,
    pub meta: Option<String>,
    pub meta_color: Option<Color32>,
    /// right column, one entry per line: pills then text
    pub right: Vec<(Vec<(Pill, String)>, String)>,
    pub right_score: bool,
    pub trailing: Option<(Pill, String)>,
    pub locked: bool,
    pub first: bool,
    pub subheader: bool,
    pub changed: bool,
    pub title_color: Option<Color32>,
    /// right column as colored numbers (.anki-counts): (text, color, weight, underlined)
    pub counts: Vec<(String, Color32, u16, bool)>,
}

fn counts_width(ui: &Ui, c: &[(String, Color32, u16, bool)]) -> f32 {
    if c.is_empty() {
        return 0.0;
    }
    c.iter().map(|(t_, col, wt, _)| lay(ui, t_, Ts::new(13.0, *wt, *col), None, false).size().x.max(18.0)).sum::<f32>() + 10.0 * (c.len() - 1) as f32
}

fn right_size(ui: &Ui, lines: &[(Vec<(Pill, String)>, String)]) -> egui::Vec2 {
    let mut wmax: f32 = 0.0;
    let mut h = 0.0;
    for (pills, text) in lines {
        let mut lw = 0.0;
        for (_, p) in pills {
            lw += w::pill_size(ui, p).x + 4.0;
        }
        if !text.is_empty() {
            lw += lay(ui, text, Ts::muted(12.0), None, false).size().x;
        } else if lw > 0.0 {
            lw -= 4.0;
        }
        wmax = wmax.max(lw);
        h += 18.0;
    }
    vec2(wmax, h)
}

/// A .row: padding 9px 14px, gap 12, min-height 42; hover and keyboard highlight; a link when
/// it has an href (click follows it, hovering warms it).
pub fn row(app: &mut App, ui: &mut Ui, pane: Pane, s: RowSpec) -> egui::Response {
    let tk = t();
    let width = ui.available_width();
    let pad_l = 14.0 + if s.indent > 0.0 { 0.0 } else { 0.0 };
    let mut rs = right_size(ui, &s.right);
    let cw = counts_width(ui, &s.counts);
    if cw > 0.0 {
        rs = vec2(rs.x.max(cw), rs.y.max(19.5));
    }
    let trailing_w = s.trailing.as_ref().map(|(_, p)| w::pill_size(ui, p).x + 12.0).unwrap_or(0.0);
    let mut x_main = pad_l;
    if s.bar.is_some() {
        x_main += 4.0 + 12.0;
    }
    if s.kind.is_some() {
        x_main += 76.0 + 12.0;
    }
    x_main += s.indent;
    let right_w = if rs.x > 0.0 { rs.x + 12.0 } else { 0.0 };
    let main_w = (width - x_main - 14.0 - right_w - trailing_w).max(20.0);
    let title_ts = if s.subheader { Ts::new(13.0, 600, tk.text) } else { Ts::new(14.0, if s.bold { 600 } else { 400 }, s.title_color.unwrap_or(tk.text)) };
    let tg = lay(ui, &s.title, title_ts, Some(main_w), true);
    let mg = s.meta.as_ref().map(|m| lay(ui, m, Ts::new(12.0, 400, s.meta_color.unwrap_or(tk.muted)), Some(main_w), true));
    let main_h: f32 = 21.0 + if mg.is_some() { 18.0 } else { 0.0 };
    let min_h = if s.subheader { 34.0 } else { 42.0 };
    let border = if s.first { 0.0 } else { 1.0 };
    let h = (main_h.max(rs.y) + 18.0 + border).max(min_h);
    let clickable = s.href.is_some();
    let (rect, resp) = ui.allocate_exact_size(vec2(width, h), if clickable { Sense::click() } else { Sense::hover() });
    let body = Rect::from_min_max(pos2(rect.min.x, rect.min.y + border), rect.max);
    if border > 0.0 {
        ui.painter().hline(rect.x_range(), rect.min.y + 0.5, Stroke::new(1.0, tk.line));
    }
    let key = s.key.clone().or_else(|| s.href.clone());
    let cursor = key.is_some() && crate::panes::state(app, pane).cursor == key && app.panes.focused == pane;
    if s.subheader {
        ui.painter().rect_filled(body, 0.0, tk.hover);
    }
    if s.changed {
        ui.painter().rect_filled(body, 0.0, tk.accent_soft);
    }
    if (clickable && resp.hovered()) || cursor {
        ui.painter().rect_filled(body, 0.0, tk.hover);
    }
    if cursor {
        ui.painter().rect_filled(Rect::from_min_size(body.min, vec2(3.0, body.height())), 0.0, tk.accent);
    }
    let alpha = if s.locked { 0.55 } else { 1.0 };
    let cy = body.center().y;
    let mut x = rect.min.x + pad_l;
    if let Some(c) = s.bar {
        // .bar: 4px, radius 2, stretches (margin -2px 0)
        ui.painter().rect_filled(Rect::from_min_max(pos2(x, body.min.y + 9.0 - 2.0), pos2(x + 4.0, body.max.y - 9.0 + 2.0)), cr(2.0), theme::alpha(c, alpha));
        x += 16.0;
    }
    if let Some(k) = &s.kind {
        let g = lay(ui, k, Ts::new(11.0, 400, theme::alpha(tk.faint, alpha)), Some(76.0), true);
        ui.painter().galley(pos2(x, cy - g.size().y / 2.0), g, tk.faint);
        x += 88.0;
    }
    x += s.indent;
    let top = cy - main_h / 2.0;
    ui.painter().galley(pos2(x, top + title_ts.top_pad()), tg, theme::alpha(title_ts.color, alpha));
    if let Some(g) = mg {
        ui.painter().galley(pos2(x, top + 21.0 + 2.2), g, theme::alpha(tk.muted, alpha));
    }
    if let Some((p, text)) = &s.trailing {
        let ps = w::pill_size(ui, text);
        w::paint_pill(ui, pos2(rect.max.x - 14.0 - right_w - ps.x, cy - ps.y / 2.0), *p, text);
    }
    // .anki-counts: numbers, each at least 18px, right-aligned
    if !s.counts.is_empty() {
        let mut xr = rect.max.x - 14.0;
        for (text, col, wt, ul) in s.counts.iter().rev() {
            let ts = Ts::new(13.0, *wt, *col).ul(*ul);
            let g = lay(ui, text, ts, None, false);
            let gw = g.size().x;
            ui.painter().galley(pos2(xr - gw, cy - g.size().y / 2.0), g, *col);
            xr -= gw.max(18.0) + 10.0;
        }
    }
    // right: each line right-aligned
    let mut y = cy - rs.y / 2.0;
    for (pills, text) in &s.right {
        let mut xr = rect.max.x - 14.0;
        if !text.is_empty() {
            let ts = if s.right_score { Ts::new(12.0, 400, tk.muted) } else { Ts::muted(12.0) };
            let g = lay(ui, text, ts, None, false);
            xr -= g.size().x;
            ui.painter().galley(pos2(xr, y + 9.0 - g.size().y / 2.0), g, theme::alpha(tk.muted, alpha));
            xr -= 4.0;
        }
        for (p, label) in pills.iter().rev() {
            let ps = w::pill_size(ui, label);
            xr -= ps.x;
            w::paint_pill(ui, pos2(xr, y + 9.0 - ps.y / 2.0), *p, label);
            xr -= 4.0;
        }
        y += 18.0;
    }
    if let Some(k) = &key {
        crate::panes::state(app, pane).items.push(Item { key: k.clone(), rect: rect.translate(vec2(0.0, 0.0)), href: s.href.clone() });
    }
    if let Some(h) = &s.href {
        if resp.hovered() {
            crate::nav::prefetch(app, h);
        }
        if resp.clicked() {
            crate::panes::state(app, pane).cursor = None;
            crate::nav::follow(app, h, pane);
        }
        return resp.on_hover_cursor(CursorIcon::PointingHand);
    }
    resp
}

/// A row that just says there's nothing (".row.empty").
pub fn empty_row(ui: &mut Ui, text: &str) {
    let tk = t();
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, 42.0), Sense::hover());
    w::painter_text(ui, pos2(rect.min.x + 14.0, rect.center().y), Align2::LEFT_CENTER, text, Ts::faint(14.0));
    let _ = tk;
}

/// An h2 that also marks an anchor (module headings).
pub fn h2_anchor(app: &mut App, ui: &mut Ui, pane: Pane, text: &str, anchor: Option<&str>) -> Rect {
    ui.add_space(28.0);
    let y = ui.cursor().min.y;
    let r = w::text_line(ui, text, w::h2_ts()).rect;
    ui.add_space(8.0);
    if let Some(a) = anchor {
        let st = crate::panes::state(app, pane);
        let off = y - st.content_top - 14.0;
        st.anchors.insert(a.to_string(), off.max(0.0));
    }
    r
}

/// h2.group-head: the name on the left, something on the right.
pub fn group_head(ui: &mut Ui, left: &str, right: impl FnOnce(&mut Ui)) -> Rect {
    ui.add_space(28.0);
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, 19.5), Sense::hover());
    let ts = w::h2_ts();
    let g = lay(ui, left, ts, Some(w * 0.7), true);
    ui.painter().galley(pos2(rect.min.x, rect.min.y + ts.top_pad()), g, ts.color);
    let mut r = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::right_to_left(egui::Align::Center)));
    right(&mut r);
    ui.add_space(8.0);
    rect
}

/// A course's tabs (.tabs): the sections Canvas shows for it.
pub const COURSE_TABS: [(&str, &str, &str); 8] = [
    ("modules", "Modules", "modules"),
    ("assignments", "Assignments", "assignments"),
    ("grades", "Grades", "grades"),
    ("announcements", "Announcements", "announcements"),
    ("discussions", "Discussions", "discussions"),
    ("pages", "Pages", "pages"),
    ("files", "Files", "files"),
    ("syllabus", "Syllabus", "syllabus"),
];

/// The course's header (crumbs, title, New notebook) and section tabs; the body goes below.
pub fn course_shell(app: &mut App, ui: &mut Ui, pane: Pane, cid: &str, active: &str, info: &canvas::CourseInfo, tabs: Option<&Value>) {
    let tk = t();
    let c = &info.c;
    let code = fmt::s(&c["course_code"]);
    let pill = info.past.then(|| format!("Past · {}", c["term"]["name"].as_str().unwrap_or("completed")));
    let name = fmt::s(&c["name"]);
    head(app, ui, pane, &name, vec![dashboard_crumb(), (code.clone(), None)], pill);
    // .title-row: h1 with the course color, New notebook on the right
    let wdt = ui.available_width();
    let btn_w = w::lay(ui, "New notebook", Ts::new(13.0, 400, tk.text), None, false).size().x + 26.0;
    let top = ui.cursor().min;
    let mut left = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(top, vec2(wdt - btn_w - 16.0, f32::INFINITY))).layout(egui::Layout::top_down(egui::Align::Min)));
    h1(&mut left, pane, &name);
    let h = left.min_rect().height();
    let mut right = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(pos2(top.x + wdt - btn_w, top.y + (h - 4.0 - 33.0) / 2.0), vec2(btn_w, 33.0))));
    let resp = w::button(&mut right, "New notebook").on_hover_text("Create a Gemini notebook from this course");
    if resp.clicked() {
        crate::nav::go(app, &format!("#/notebooks/new?c={cid}&m=all"));
    }
    ui.allocate_space(vec2(wdt, h));
    // tabs: shown if Canvas shows them (Grades always, and the one you're on)
    let visible: Option<Vec<String>> = tabs.and_then(|t| t.as_array()).map(|a| a.iter().filter(|x| !x["hidden"].as_bool().unwrap_or(false)).map(|x| fmt::s(&x["id"])).collect());
    let shown: Vec<&(&str, &str, &str)> = COURSE_TABS.iter().filter(|(slug, _, id)| visible.as_ref().map(|v| v.iter().any(|x| x == id)).unwrap_or(true) || *id == "grades" || *slug == active).collect();
    ui.add_space(14.0);
    let (bar, _) = ui.allocate_exact_size(vec2(wdt, 36.0), Sense::hover());
    ui.painter().hline(bar.x_range(), bar.max.y - 0.5, Stroke::new(1.0, tk.line));
    let mut x = bar.min.x;
    let mut tabs_list = Vec::new();
    for (slug, label, _) in shown {
        let on = *slug == active;
        let ts = Ts::new(14.0, if on { 550 } else { 400 }, if on { tk.text } else { tk.muted });
        let g = lay(ui, label, ts, None, false);
        let r = Rect::from_min_size(pos2(x, bar.min.y), vec2(g.size().x + 24.0, 36.0));
        if r.max.x > bar.max.x + 1.0 {
            break;
        }
        let href = format!("#/c/{cid}/{slug}");
        let resp = ui.interact(r, Id::new(("ctab", cid, *slug)), Sense::click());
        let color = if on || resp.hovered() { tk.text } else { tk.muted };
        ui.painter().galley(pos2(r.min.x + 12.0, r.center().y - g.size().y / 2.0), g, color);
        if on {
            ui.painter().rect_filled(Rect::from_min_max(pos2(r.min.x, r.max.y - 2.0), r.max), 0.0, tk.accent);
        }
        if resp.hovered() {
            crate::nav::prefetch(app, &href);
        }
        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            crate::nav::go(app, &href);
        }
        tabs_list.push((href, on));
        if on {
            out(app, pane).tab_label = Some(label.to_string());
        }
        x = r.max.x + 2.0;
    }
    if pane == Pane::Main {
        app.settings.course_tabs = tabs_list;
    }
    ui.add_space(20.0);
}

/// A submission's status pill.
pub fn sub_pill(sub: &Value, a: &Value) -> Option<(Pill, String)> {
    if !sub.is_object() {
        return None;
    }
    if sub["excused"].as_bool().unwrap_or(false) {
        return Some((Pill::Plain, "Excused".into()));
    }
    if sub["workflow_state"] == "graded" && !sub["score"].is_null() {
        let score = sub["score"].as_f64().unwrap_or(0.0);
        let pts = a["points_possible"].as_f64().filter(|p| *p != 0.0);
        let pct = pts.map(|p| score / p);
        let kind = match pct {
            None => Pill::Ok,
            Some(p) if p >= 0.7 => Pill::Ok,
            Some(p) if p >= 0.5 => Pill::Warn,
            _ => Pill::Bad,
        };
        return Some((kind, format!("{}{}", fmt::num(&sub["score"]), pts.map(|p| format!(" / {}", fmt::num_f(p))).unwrap_or_default())));
    }
    if sub["missing"].as_bool().unwrap_or(false) {
        return Some((Pill::Bad, "Missing".into()));
    }
    if crate::views::truthy(&sub["submitted_at"]) {
        return Some(if sub["late"].as_bool().unwrap_or(false) { (Pill::Warn, "Late".into()) } else { (Pill::Ok, "Submitted".into()) });
    }
    None
}

pub fn truthy(v: &Value) -> bool {
    canvas_mcp::util::truthy(v)
}

/// .detail-meta: a wrapped row of "label <b>value</b>" parts, 13px muted, margin 10px 0 22px.
pub fn detail_meta(ui: &mut Ui, parts: Vec<Vec<(String, bool)>>, pills: Vec<(Pill, String)>) {
    let tk = t();
    ui.add_space(10.0);
    w::hwrap(ui, vec2(20.0, 8.0), |ui| {
        for p in parts {
            let mut rich = w::Rich::new();
            for (text, bold) in p {
                rich.push(&text, if bold { Ts::new(13.0, 550, tk.text) } else { Ts::muted(13.0) });
            }
            let g = rich.lay(ui);
            let (r, _) = ui.allocate_exact_size(vec2(g.size().x, 19.5), Sense::hover());
            ui.painter().galley(pos2(r.min.x, r.center().y - g.size().y / 2.0), g, tk.muted);
        }
        for (k, p) in pills {
            ui.allocate_ui_with_layout(vec2(w::pill_size(ui, &p).x, 19.5), egui::Layout::left_to_right(egui::Align::Center), |ui| w::pill(ui, k, &p));
        }
    });
    ui.add_space(22.0);
}

/// .actions: a row of buttons, margin 12px 0 20px.
pub fn actions<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.add_space(12.0);
    let r = w::hwrap(ui, vec2(8.0, 8.0), add);
    ui.add_space(20.0);
    r
}

/// An "Open in Canvas ↗" style button that opens a URL outside.
pub fn ext_link(app: &mut App, ui: &mut Ui, url: &str, label: &str) {
    if url.is_empty() {
        return;
    }
    if w::button(ui, label).clicked() {
        app.open_external(url);
    }
}

/// Look up titles for background tabs (their pages' headings).
pub fn fill_titles(app: &mut App) {
    let missing: Vec<(u64, String)> = app.panes.tabs.iter().filter(|t| t.title.is_empty() && Some(t.id) != app.panes.active).map(|t| (t.id, t.href().to_string())).collect();
    for (id, href) in missing {
        if let Some(title) = title_for(app, &href) {
            if let Some(t) = app.panes.tabs.iter_mut().find(|t| t.id == id) {
                t.title = title;
            }
        }
    }
}

/// A document's heading from cached data (no drawing).
pub fn title_for(app: &App, href: &str) -> Option<String> {
    let r = crate::route::parse(href);
    let d = &app.d;
    let find = |list: &Json, k: &str, id: &str, field: &str| list.as_array().and_then(|a| a.iter().find(|x| fmt::id(&x[k]) == id).map(|x| fmt::s(&x[field])));
    match r.view {
        View::Assignment(c, a) => d.need1("groups", &c).ok().and_then(|g| g.as_array().and_then(|gs| gs.iter().flat_map(|g| g["assignments"].as_array().cloned().unwrap_or_default()).find(|x| fmt::id(&x["id"]) == a).map(|x| fmt::s(&x["name"])))),
        View::Page(c, slug) => d.need("page", &[c, slug]).ok().map(|p| fmt::s(&p["title"])),
        View::Topic(c, tid) => d.need1("discussions", &c).ok().and_then(|l| find(&l, "id", &tid, "title")).or_else(|| d.need1("course_announcements", &c).ok().and_then(|l| find(&l, "id", &tid, "title"))),
        View::File(c, f) => c.and_then(|c| d.need1("files", c).ok()).and_then(|l| l["files"].as_array().and_then(|a| a.iter().find(|x| fmt::id(&x["id"]) == f).map(|x| fmt::s(&x["display_name"])))),
        View::Conversation(id) => d.need0("inbox").ok().and_then(|l| find(&l, "id", &id, "subject")).map(|s| if s.is_empty() { "(no subject)".into() } else { s }),
        _ => None,
    }
    .filter(|s| !s.is_empty())
}

/// Scripted steps for --screenshot --actions (testing the UI).
pub fn script(app: &mut App, ctx: &egui::Context, act: &str) {
    let (verb, arg) = act.split_once(':').unwrap_or((act, ""));
    match verb {
        "go" => crate::nav::go(app, arg),
        "open" => crate::panes::open_doc(app, arg),
        "theme" => crate::nav::set_theme(app, arg),
        "key" => {
            let key = egui::Key::from_name(arg).or_else(|| egui::Key::from_name(&arg.to_uppercase()));
            let shift = arg.len() == 1 && arg.chars().all(|c| c.is_ascii_uppercase());
            if let Some(k) = key {
                let _ = ctx;
                if let Some(s) = app.screenshot.as_mut() {
                    s.keys.push(egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers { shift, ..Default::default() } });
                }
            }
        }
        "palette" => {
            crate::palette::open(app);
            app.palette.query = arg.to_string();
        }
        "pane" => match arg {
            "viewer" => app.panes.focused = Pane::Viewer,
            "sidebar" => app.panes.focused = Pane::Sidebar,
            _ => app.panes.focused = Pane::Main,
        },
        "keys" => app.keys_open = true,
        "debug" => crate::debug::set_on(app, true),
        "edit" => crate::sidebar::set_editing(app, true),
        "scroll" => {
            let y: f32 = arg.parse().unwrap_or(0.0);
            app.main.scroll_to = Some(y);
        }
        "vscroll" => {
            let y: f32 = arg.parse().unwrap_or(0.0);
            app.viewer.scroll_to = Some(y);
        }
        _ => {}
    }
}
