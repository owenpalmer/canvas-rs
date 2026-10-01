//! Getting around: the back/forward history, recently visited pages, and the keyboard shortcuts.

use std::time::{Duration, Instant};

use egui::{Context, Event, Key, Modifiers, PointerButton, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use serde_json::{Value, json};

use crate::app::{App, Pane};
use crate::panes::{self, RESIZE_STEP};
use crate::route::{self, View, is_doc, path_of};
use crate::theme::t;
use crate::widgets::{Ts, cr, lay, paint_kbd};

pub struct Nav {
    pub stack: Vec<String>,
    pub pos: usize,
    pub g_prefix: Option<Instant>,
}

impl Nav {
    pub fn new(start: &str) -> Nav {
        Nav { stack: vec![start.to_string()], pos: 0, g_prefix: None }
    }
    pub fn can_back(&self) -> bool {
        self.pos > 0
    }
    pub fn can_forward(&self) -> bool {
        self.pos + 1 < self.stack.len()
    }
    pub fn current(&self) -> &str {
        &self.stack[self.pos]
    }
}

/// Where the app opens: the dashboard, or the welcome screen before setup is finished.
pub fn start_hash(status: &Value, d: &crate::data::Data) -> String {
    let demo = status["demo"].as_bool().unwrap_or(false);
    if !demo && (!status["configured"].as_bool().unwrap_or(false) || !d.has("self")) {
        return "#/welcome".into();
    }
    std::env::var("CANVAS_APP_ROUTE").ok().filter(|r| r.starts_with("#/")).unwrap_or_else(|| "#/".into())
}

/// Show `hash` in main (no history entry).
fn set_main(app: &mut App, hash: &str) {
    if app.main.want == hash {
        return;
    }
    app.main.want = hash.to_string();
    app.main.since = Instant::now();
    app.main.cursor = None;
    app.main.anchor = route::parse(hash).anchor;
    app.main.trace = crate::debug::start(app, "main", hash, "data");
    app.d.clear_errors();
    if app.panes.hidden.contains("main") {
        panes::set_pane_hidden(app, "main", false); // a new page in main brings it back
    }
}

/// The first page: a document goes to the viewer with its list in main.
pub fn land(app: &mut App, hash: &str) {
    if is_doc(hash) {
        panes::open_doc(app, hash);
        let parent = panes::doc_parent(app, hash).unwrap_or_else(|| "#/".into());
        app.nav.stack = vec![parent.clone()];
        app.nav.pos = 0;
        app.main.want = parent;
    }
}

/// Go somewhere: a document opens in the viewer (its list in main); anything else, in main.
pub fn go(app: &mut App, href: &str) {
    if is_doc(href) {
        panes::open_doc(app, href);
        if let Some(parent) = panes::doc_parent(app, href) {
            if parent != app.nav.current() {
                push(app, &parent);
            }
        }
        return;
    }
    push(app, href);
}

fn push(app: &mut App, hash: &str) {
    if app.nav.current() == hash {
        return;
    }
    let pos = app.nav.pos;
    app.nav.stack.truncate(pos + 1);
    app.nav.stack.push(hash.to_string());
    app.nav.pos += 1;
    set_main(app, hash);
}

pub fn back(app: &mut App) {
    if app.nav.can_back() {
        app.nav.pos -= 1;
        let h = app.nav.current().to_string();
        set_main(app, &h);
    }
}

pub fn forward(app: &mut App) {
    if app.nav.can_forward() {
        app.nav.pos += 1;
        let h = app.nav.current().to_string();
        set_main(app, &h);
    }
}

/// Up to the parent page: the last link in the breadcrumbs.
pub fn go_up(app: &mut App) {
    if let Some(h) = app.main.out.crumbs.iter().rev().find_map(|(_, h)| h.clone().filter(|h| h.starts_with("#/"))) {
        go(app, &h);
    }
}

/// Follow a link from a pane: outside links open in the browser, documents in the viewer (in the
/// same tab when clicked inside the viewer), anything else in main.
pub fn follow(app: &mut App, href: &str, from: Pane) {
    let lower = href.to_lowercase();
    if lower.starts_with("http:") || lower.starts_with("https:") || lower.starts_with("mailto:") {
        app.open_external(href);
        return;
    }
    if !href.starts_with("#/") {
        return;
    }
    if is_doc(href) {
        if from == Pane::Viewer { panes::navigate_tab(app, href) } else { panes::open_doc(app, href) }
        return;
    }
    go(app, href);
}

// --- recently visited ------------------------------------------------------------------------------
const RECENT_MAX: usize = 12;

pub fn recent(app: &App) -> Vec<Value> {
    app.prefs.get("recent").and_then(|v| v.as_array()).cloned().unwrap_or_default()
}

pub fn record_recent(app: &mut App, hash: &str, title: &str, where_: &str) {
    if title.is_empty() || ["Not found", "Couldn't load this", "Sign in to see this"].contains(&title) {
        return;
    }
    let p = path_of(hash);
    if p == "/welcome" || p.starts_with("/notebooks/new") || p.ends_with("/add") {
        return;
    }
    let list = recent(app);
    if list.first().map(|x| x["h"] == hash && x["t"] == title).unwrap_or(false) {
        return; // a re-render of the same page
    }
    let mut out = vec![json!({"h": hash, "t": title, "c": where_})];
    out.extend(list.into_iter().filter(|x| x["h"] != hash));
    out.truncate(RECENT_MAX);
    app.set_pref("recent", Value::Array(out));
}

/// A page just appeared in a pane (the first draw of a new address).
pub fn after_render(app: &mut App, pane: Pane, hash: &str) {
    let st = panes::state(app, pane);
    let out = st.out.clone();
    let deps: Vec<String> = st.deps.iter().cloned().collect();
    let crumbs_text = out.crumbs.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>().join(" / ");
    let where_ = crumbs_text.trim_start_matches("Dashboard").trim_start_matches(" / ").trim().to_string();
    match pane {
        Pane::Viewer => {
            let href = hash.split_once(':').map(|x| x.1).unwrap_or(hash).to_string();
            let title = if out.title.is_empty() { "Untitled".to_string() } else { out.title.clone() };
            record_recent(app, &href, &title, &where_);
            let mut changed = false;
            if let Some(t) = app.panes.active_mut() {
                if t.title != title {
                    t.title = title;
                    changed = t.pinned;
                }
            }
            if changed {
                panes::save_tabs(app);
            }
            if let Some(tr) = app.viewer.trace.take() {
                crate::debug::end(app, tr, &out.title);
            }
        }
        _ => {
            let route = route::parse(hash);
            // Scroll: to the anchor, else the top.
            app.main.scroll_to = Some(route.anchor.as_ref().and_then(|a| app.main.anchors.get(a).copied()).unwrap_or(0.0));
            let label = out.tab_label.clone().unwrap_or_else(|| out.title.clone());
            record_recent(app, hash, &label, &where_);
            // Navigating to a past course opens its (otherwise collapsed) sidebar section.
            if let Some(cid) = regex::Regex::new(r"^/c/(\d+)").unwrap().captures(&route.path).map(|m| m[1].to_string()) {
                if !app.sidebar.past_open && crate::sidebar::is_past(app, &cid) {
                    crate::sidebar::set_past_open(app, true);
                }
            }
            if !app.sidebar.editing {
                app.panes.focused = if app.panes.open_main { Pane::Main } else { app.panes.focused };
            }
            if let Some(tr) = app.main.trace.take() {
                let what = [out.title.clone(), out.tab_label.clone().unwrap_or_default()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
                crate::debug::end(app, tr, &what);
            }
        }
    }
    app.d.revalidate(deps.iter(), false);
}

/// Hovering a link warms every resource its view needs.
pub fn prefetch(app: &mut App, href: &str) {
    if let Some(t) = app.prefetched.get(href) {
        if t.elapsed() < Duration::from_secs(30) {
            return;
        }
    }
    app.prefetched.insert(href.to_string(), Instant::now());
    let r = route::parse(href);
    if matches!(r.view, View::NotFound | View::Welcome | View::NotebookNew | View::NotebookAdd(_) | View::Anki | View::AnkiImport | View::AnkiDeck(_)) {
        return; // views with state
    }
    crate::views::prefetch(app, &r);
}

// --- shortcuts -------------------------------------------------------------------------------------
pub const SHORTCUTS: &[(&str, &[(&[&str], &str)])] = &[
    (
        "Anywhere",
        &[
            (&["Ctrl K", "/"], "Search, or run a command"),
            (&["?"], "Show these shortcuts"),
            (&["Alt ←", "Alt →"], "Back and forward"),
            (&["u"], "Up to the parent page"),
            (&["g d"], "Dashboard"),
            (&["g i"], "Inbox"),
            (&["g n"], "Gemini Notebooks"),
            (&["g a"], "Anki"),
            (&["g b"], "Textbooks"),
            (&["g s"], "Settings"),
            (&["r"], "Refresh this page from Canvas"),
            (&["t"], "Switch theme"),
        ],
    ),
    (
        "Panes: sidebar, main, viewer",
        &[
            (&["H", "L"], "Move to the pane on the left or right (shows it if hidden)"),
            (&["J", "K"], "Make the pane narrower or wider"),
            (&["z"], "Hide the pane (H or L brings it back)"),
            (&["j", "k"], "Move through the sidebar or a list; scroll the viewer"),
            (&["h", "l"], "Course sections in main; viewer tabs in the viewer; l opens a sidebar link"),
            (&["Enter"], "Open the item; a page or file stays as a viewer tab"),
            (&["f"], "Filter the list, where there is one"),
            (&["Esc"], "Leave a text field"),
        ],
    ),
    (
        "Viewer tabs",
        &[
            (&["j", "k"], "In a list, show the item in the preview tab (italic)"),
            (&["Ctrl Tab"], "Next tab (with Shift, previous), from anywhere"),
            (&["x", "Ctrl W"], "Close the tab"),
            (&["Alt ←", "Alt →"], "Back and forward within the tab"),
        ],
    ),
    (
        "PDFs and textbooks in the viewer",
        &[
            (&["o"], "Contents: the chapters and sections, to jump to"),
            (&["F11"], "Fullscreen (Esc leaves it)"),
            (&["m"], "Checkpoint mode on or off (for adding checkpoints)"),
            (&["j", "k"], "In checkpoint mode: next and previous sentence"),
            (&["c"], "In checkpoint mode: a checkpoint after the sentence (c again to add it)"),
            (&["a"], "In checkpoint mode: click anywhere to place a checkpoint"),
            (&["Space"], "Show the answer"),
            (&["←", "→"], "Previous and next question"),
            (&["Esc"], "Cancel, then clear the highlight"),
        ],
    ),
    (
        "Editing the course list (✎ in the sidebar)",
        &[(&["↑", "↓"], "Move between courses"), (&["Alt ↑", "Alt ↓"], "Move the course up or down (or J, K)"), (&["Space"], "Hide or show the course"), (&["Esc"], "Done")],
    ),
    ("Reviewing Anki cards", &[(&["Space"], "Show the answer, then answer Good"), (&["1", "2", "3", "4"], "Again, Hard, Good, Easy")]),
];

/// The shortcuts in two columns (.keys-groups), for the dialog and Settings.
pub fn shortcuts_table(ui: &mut egui::Ui) {
    let tk = t();
    let w = ui.available_width();
    let cols = if w >= 628.0 { 2 } else { 1 };
    let col_w = (w - 28.0 * (cols as f32 - 1.0)) / cols as f32;
    // Balance the groups over the columns by height (CSS columns).
    let heights: Vec<f32> = SHORTCUTS.iter().map(|(_, rows)| 12.0 + 18.0 + rows.len() as f32 * 26.0).collect();
    let total: f32 = heights.iter().sum();
    let mut assign = vec![0usize; SHORTCUTS.len()];
    if cols == 2 {
        let mut acc = 0.0;
        for (i, h) in heights.iter().enumerate() {
            if acc + h / 2.0 > total / 2.0 {
                assign[i] = 1;
            }
            acc += h;
        }
    }
    let top = ui.cursor().min.y;
    let mut bottom = top;
    for c in 0..cols {
        let x = ui.cursor().min.x + c as f32 * (col_w + 28.0);
        let mut y = top;
        for (gi, (group, rows)) in SHORTCUTS.iter().enumerate() {
            if assign[gi] != c {
                continue;
            }
            y += 12.0;
            let head = Ts::new(11.0, 400, tk.faint).up().sp(0.05);
            let g = lay(ui, group, head, Some(col_w), true);
            ui.painter().galley(pos2(x, y + head.top_pad()), g, head.color);
            y += head.line_px() + 4.0;
            for (keys, what) in rows.iter() {
                let mut kx = x;
                let mut ky = y + 3.0;
                for k in keys.iter() {
                    let size = crate::widgets::kbd_size(ui, k, 11.0);
                    if kx + size.x > x + 104.0 {
                        kx = x;
                        ky += size.y + 3.0;
                    }
                    paint_kbd(ui, pos2(kx, ky), k, 11.0, None, None, None);
                    kx += size.x + 3.0;
                }
                let ts = Ts::new(13.0, 400, tk.text);
                let g = lay(ui, what, ts, Some(col_w - 114.0), false);
                let th = g.rows.len() as f32 * ts.line_px();
                ui.painter().galley(pos2(x + 114.0, y + 3.0 + ts.top_pad()), g, tk.text);
                y += (th + 6.0).max(ky + 18.0 - y + 3.0);
            }
        }
        bottom = bottom.max(y);
    }
    ui.allocate_rect(Rect::from_min_max(pos2(ui.cursor().min.x, top), pos2(ui.cursor().min.x + w, bottom)), Sense::hover());
}

/// The shortcuts dialog (#keys).
pub fn draw_keys(app: &mut App, ctx: &Context) {
    if !app.keys_open {
        return;
    }
    let tk = t();
    let screen = ctx.content_rect();
    let dim = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("keys-dim")));
    dim.rect_filled(screen, 0.0, egui::Color32::from_black_alpha(64));
    let w = (screen.width() * 0.92).min(720.0);
    let top = screen.min.y + screen.height() * 0.08;
    let mut close = false;
    let area = egui::Area::new(egui::Id::new("keys")).order(egui::Order::Foreground).fixed_pos(pos2(screen.center().x - w / 2.0, top)).show(ctx, |ui| {
        ui.set_width(w);
        let max_h = screen.height() * 0.84;
        let frame_rect_idx = ui.painter().add(egui::Shape::Noop);
        let inner = egui::ScrollArea::vertical().max_height(max_h).show(ui, |ui| {
            ui.set_width(w);
            let mut c = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(ui.cursor().min + vec2(20.0, 16.0), vec2(w - 40.0, 10_000.0))));
            // title row
            let row = c.allocate_exact_size(vec2(w - 40.0, 22.0), Sense::hover()).0;
            crate::widgets::painter_text(&c, row.left_center(), egui::Align2::LEFT_CENTER, "Keyboard shortcuts", Ts::new(15.0, 650, tk.text));
            let bsize = crate::widgets::kbd_size(&c, "Esc", 11.0);
            let brect = Rect::from_min_size(pos2(row.max.x - bsize.x, row.center().y - bsize.y / 2.0), bsize);
            let resp = c.interact(brect, egui::Id::new("keys-close"), Sense::click());
            paint_kbd(&c, brect.min, "Esc", 11.0, None, Some(egui::Color32::TRANSPARENT), None);
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                close = true;
            }
            c.add_space(8.0);
            shortcuts_table(&mut c);
            c.add_space(20.0);
            ui.allocate_space(vec2(w, c.min_rect().height() + 16.0));
        });
        let r = Rect::from_min_size(inner.inner_rect.min, vec2(w, inner.inner_rect.height()));
        let shadow = egui::Shadow { offset: [0, 20], blur: 60, spread: 0, color: egui::Color32::from_black_alpha(77) };
        ui.painter().set(frame_rect_idx, egui::Shape::Vec(vec![shadow.as_shape(r, cr(12.0)).into(), egui::Shape::rect_filled(r, cr(12.0), tk.panel_solid), egui::Shape::rect_stroke(r, cr(12.0), Stroke::new(1.0, tk.line), StrokeKind::Inside)]));
        r
    });
    let clicked_outside = ctx.input(|i| i.pointer.any_pressed()) && ctx.input(|i| i.pointer.interact_pos()).map(|p| !area.inner.contains(p)).unwrap_or(false);
    if close || clicked_outside {
        app.keys_open = false;
    }
}

// --- the key handler ---------------------------------------------------------------------------------
/// The key as the web UI named it: Shift+j reads as J (and Shift+/ as ?).
fn key_name(key: Key, m: &Modifiers) -> String {
    let base = match key {
        Key::Escape => "Escape".to_string(),
        Key::Enter => "Enter".to_string(),
        Key::Space => " ".to_string(),
        Key::Tab => "Tab".to_string(),
        Key::ArrowLeft => "ArrowLeft".to_string(),
        Key::ArrowRight => "ArrowRight".to_string(),
        Key::ArrowUp => "ArrowUp".to_string(),
        Key::ArrowDown => "ArrowDown".to_string(),
        Key::Slash => "/".to_string(),
        Key::Questionmark => "?".to_string(),
        Key::Num1 => "1".into(),
        Key::Num2 => "2".into(),
        Key::Num3 => "3".into(),
        Key::Num4 => "4".into(),
        k => k.name().to_lowercase(),
    };
    if m.shift {
        if base.len() == 1 && base.chars().all(|c| c.is_ascii_lowercase()) {
            return base.to_uppercase();
        }
        if base == "/" {
            return "?".into();
        }
    }
    base
}

pub fn keys(app: &mut App, ctx: &Context) {
    let events = ctx.input(|i| i.events.clone());
    let typing = ctx.egui_wants_keyboard_input();
    for ev in events {
        match ev {
            Event::PointerButton { button: PointerButton::Extra1, pressed: true, .. } => back(app),
            Event::PointerButton { button: PointerButton::Extra2, pressed: true, .. } => forward(app),
            Event::Key { key, pressed: true, modifiers, .. } => {
                if handle_key(app, ctx, key, &modifiers, typing) {
                    ctx.input_mut(|i| i.consume_key(modifiers, key));
                }
            }
            _ => {}
        }
    }
    if let Some(t) = app.nav.g_prefix {
        if t.elapsed() > Duration::from_millis(800) {
            app.nav.g_prefix = None;
        }
    }
}

/// Returns true when the key was used.
fn handle_key(app: &mut App, ctx: &Context, key: Key, m: &Modifiers, typing: bool) -> bool {
    let name = key_name(key, m);
    let cmd = m.ctrl || m.command || m.mac_cmd;
    if cmd && key == Key::K {
        app.keys_open = false;
        crate::palette::open(app);
        return true;
    }
    if app.palette.open {
        return crate::palette::key(app, key, m);
    }
    if app.keys_open {
        if key == Key::Escape || name == "?" {
            app.keys_open = false;
            return true;
        }
        return false;
    }
    // Back and forward (in the viewer, within the tab).
    if m.alt && (key == Key::ArrowLeft || key == Key::ArrowRight) {
        let d = if key == Key::ArrowLeft { -1 } else { 1 };
        if app.panes.focused == Pane::Viewer {
            panes::tab_history(app, d);
        } else if d < 0 {
            back(app);
        } else {
            forward(app);
        }
        return true;
    }
    // Viewer tabs from anywhere, like a browser's.
    if m.ctrl && key == Key::Tab {
        if !app.panes.tabs.is_empty() {
            panes::switch_tab(app, if m.shift { -1 } else { 1 });
            return true;
        }
        return false;
    }
    if cmd && key == Key::W && app.panes.active.is_some() {
        panes::close_tab(app, None);
        return true;
    }
    // The sidebar's course editing keys.
    if app.sidebar.editing && app.panes.focused == Pane::Sidebar && !typing {
        if crate::sidebar::edit_key(app, &name, m) {
            return true;
        }
    }
    // Fullscreen reading: F11 toggles it, Esc leaves it (after the palette and dialogs above).
    if key == Key::F11 {
        let on = !app.panes.fullscreen;
        panes::set_fullscreen(app, on);
        return true;
    }
    if key == Key::Escape && app.panes.fullscreen && !typing {
        panes::set_fullscreen(app, false);
        return true;
    }
    // o: a PDF's Contents panel
    if name == "o" && !typing && !cmd && !m.alt && app.panes.focused == Pane::Viewer {
        let has = app.panes.active_tab().map(|t| t.href().to_string()).and_then(|h| crate::pdf::fid_of(app, &h)).and_then(|f| app.pdf.info(&f).map(|i| !i.outline.is_empty())).unwrap_or(false);
        if has {
            let on = !app.pdf.contents;
            crate::pdf::set_contents(app, on);
            return true;
        }
    }
    // In the viewer, a PDF's checkpoints get the keys first.
    if app.panes.focused == Pane::Viewer && app.nav.g_prefix.is_none() && !typing && !cmd && !m.alt && crate::checkpoints::key(app, &name) {
        return true;
    }
    // Reviewing Anki cards.
    if !typing && !cmd && !m.alt && crate::anki::key(app, &name) {
        return true;
    }
    if key == Key::Escape {
        if typing {
            ctx.memory_mut(|mem| {
                if let Some(id) = mem.focused() {
                    mem.surrender_focus(id);
                }
            });
            app.panes.focused = Pane::Main;
        } else if app.sidebar.editing {
            crate::sidebar::set_editing(app, false);
        }
        return !typing || true;
    }
    if typing || cmd || m.alt {
        return false;
    }
    if app.nav.g_prefix.take().is_some() {
        let dest = match name.as_str() {
            "d" => "#/",
            "i" => "#/inbox",
            "a" => "#/anki",
            "b" => "#/textbooks",
            "n" => "#/notebooks",
            "s" => "#/settings",
            _ => return true,
        };
        go(app, dest);
        return true;
    }
    let pane = app.panes.focused;
    let screen_w = ctx.content_rect().width();
    // hjkl within the pane you're in.
    let within = match (pane, name.as_str()) {
        (Pane::Sidebar, "j") => Some(crate::sidebar::step(app, 1)),
        (Pane::Sidebar, "k") => Some(crate::sidebar::step(app, -1)),
        (Pane::Sidebar, "l") | (Pane::Sidebar, "Enter") => Some(crate::sidebar::open_cursor(app)),
        (Pane::Main, "j") => Some(step_item(app, 1)),
        (Pane::Main, "k") => Some(step_item(app, -1)),
        (Pane::Main, "h") => Some(step_tab(app, -1)),
        (Pane::Main, "l") => Some(step_tab(app, 1)),
        (Pane::Main, "Enter") => Some(open_item(app)),
        (Pane::Viewer, "j") => Some(scroll_viewer(app, 80.0)),
        (Pane::Viewer, "k") => Some(scroll_viewer(app, -80.0)),
        (Pane::Viewer, "h") => Some(panes::switch_tab(app, -1)),
        (Pane::Viewer, "l") => Some(panes::switch_tab(app, 1)),
        (Pane::Viewer, "x") => Some(panes::close_tab(app, None)),
        _ => None,
    };
    if within.is_some() {
        return true;
    }
    match name.as_str() {
        "/" => crate::palette::open(app),
        "?" => app.keys_open = true,
        "g" => app.nav.g_prefix = Some(Instant::now()),
        "r" => {
            let deps: Vec<String> = panes::state(app, pane).deps.iter().cloned().collect();
            app.d.clear_errors();
            app.d.revalidate(deps.iter(), true);
        }
        "t" => toggle_theme(app),
        "u" => go_up(app),
        "H" => panes::move_pane(app, -1),
        "L" => panes::move_pane(app, 1),
        "J" => panes::resize_pane(app, pane, -RESIZE_STEP, screen_w),
        "K" => panes::resize_pane(app, pane, RESIZE_STEP, screen_w),
        "z" => panes::toggle_pane(app, pane),
        "f" => app.settings.focus_filter = true,
        _ => return false,
    }
    true
}

pub fn set_theme(app: &mut App, pref: &str) {
    let fade = !app.settings.reduced_motion;
    app.theme.set_pref(pref, fade);
    app.set_pref("theme", json!(pref));
}

pub fn toggle_theme(app: &mut App) {
    let next = match app.theme.pref.as_str() {
        "dark" => "light",
        "light" => "",
        _ => "dark",
    };
    set_theme(app, next);
}

fn scroll_viewer(app: &mut App, dy: f32) {
    let y = app.viewer.scroll + dy;
    app.viewer.scroll_to = Some(y.max(0.0));
}

/// j / k: step through the rows and cards of the page, starting from the first one in sight.
fn step_item(app: &mut App, d: i64) {
    let items = app.main.items.clone();
    if items.is_empty() {
        return;
    }
    let cur = app.main.cursor.as_ref().and_then(|c| items.iter().position(|i| &i.key == c));
    let i = match cur {
        Some(i) => (i as i64 + d).clamp(0, items.len() as i64 - 1) as usize,
        None => {
            // the first (or last) one in sight
            let view_top = app.main.content_top + app.main.scroll;
            let view_bottom = view_top + app.main.viewport_h;
            let in_view: Vec<usize> = items.iter().enumerate().filter(|(_, it)| it.rect.max.y > view_top + 36.0 && it.rect.min.y < view_bottom).map(|(i, _)| i).collect();
            if in_view.is_empty() {
                if d > 0 { 0 } else { items.len() - 1 }
            } else if d > 0 {
                in_view[0]
            } else {
                *in_view.last().unwrap()
            }
        }
    };
    let it = &items[i];
    app.main.cursor = Some(it.key.clone());
    app.main.scroll_to_rect = Some(it.rect);
    // The viewer shows it (preview tab), a moment later.
    if let Some(h) = &it.href {
        if is_doc(h) {
            app.panes.preview_at = Some((h.clone(), Instant::now() + Duration::from_millis(60)));
        }
    }
}

fn open_item(app: &mut App) {
    let Some(c) = &app.main.cursor else { return };
    if let Some(href) = app.main.items.iter().find(|i| &i.key == c).and_then(|i| i.href.clone()) {
        follow(app, &href, Pane::Main);
    } else {
        app.settings.toggle_cursor = true; // a checkbox row: toggles it
    }
}

/// h / l: along the course's section tabs.
fn step_tab(app: &mut App, d: i64) {
    let tabs = app.settings.course_tabs.clone();
    let Some(i) = tabs.iter().position(|(_, active)| *active) else { return };
    let j = i as i64 + d;
    if j >= 0 && (j as usize) < tabs.len() {
        let href = tabs[j as usize].0.clone();
        go(app, &href);
    }
}
