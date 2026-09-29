//! Anki: deck list, linking course decks, and a review screen, through the AnkiConnect add-on of
//! a running Anki (canvas_mcp::anki). Anki does the scheduling, so reviews count exactly as they
//! do in Anki. Nothing talks to Anki until you allow it.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use canvas_mcp::services::ApiErr;
use egui::{Color32, Id, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use serde_json::{Value, json};

use crate::app::{App, Pane};
use crate::data::Need;
use crate::fmt;
use crate::html::{Env, Flavor, TAlign};
use crate::theme::{self, t};
use crate::views::{RowSpec, actions, group_head, h1, head, list, row};
use crate::widgets::{self as w, Pill, Ts, cr};

const EASE_LABELS: [&str; 4] = ["Again", "Hard", "Good", "Easy"];

#[derive(Default)]
pub struct Review {
    did: i64,
    data: Option<Value>,
    error: Option<ApiErr>,
    loading: bool,
    shown: bool,
    busy: bool,
    reviewed: u32,
}

#[derive(Default)]
pub struct AnkiState {
    pub due: Option<i64>,
    decks: Option<Result<Value, ApiErr>>,
    setup: Option<Value>,
    loading: bool,
    last_drawn: u64,
    /// course id -> "" (not linked) | "new" | deck id, only for changed rows
    choice: HashMap<i64, String>,
    imported: Vec<String>,
    review: Review,
    /// a button whose action is running, and what it says meanwhile
    busy: Option<&'static str>,
    badge_at: Option<Instant>,
}

pub fn init(app: &mut App) {
    // the sidebar's due count: a few seconds after launch, then every 10 minutes
    app.anki.badge_at = Some(Instant::now() + Duration::from_secs(3));
}

pub fn tick(app: &mut App) {
    if let Some(at) = app.anki.badge_at {
        if Instant::now() >= at {
            app.anki.badge_at = Some(Instant::now() + Duration::from_secs(600));
            refresh_badge(app);
        } else if let Some(c) = app.ctx() {
            c.request_repaint_after(at - Instant::now());
        }
    }
}

fn due_count(d: &Value) -> i64 {
    ["new", "learn", "review"].iter().map(|k| d[*k].as_i64().unwrap_or(0)).sum()
}

fn top_due(decks: &Value) -> i64 {
    decks.as_array().into_iter().flatten().filter(|d| !fmt::s(&d["name"]).contains("::")).map(due_count).sum()
}

fn leaf(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
}

fn refresh_badge(app: &mut App) {
    if !app.allowed("anki") {
        return;
    }
    let svc = app.svc.clone();
    app.spawn(async move { svc.anki_decks().await }, |app, r| {
        app.anki.due = r.ok().map(|d| top_due(&d["decks"])).filter(|n| *n > 0);
    });
}

/// Load the decks (and what's set up), fresh each time the deck list or import page opens.
fn reload(app: &mut App) {
    if app.anki.loading {
        return;
    }
    app.anki.loading = true;
    let svc = app.svc.clone();
    app.spawn(
        async move {
            let decks = svc.anki_decks().await;
            let setup = svc.anki_setup_status().await.unwrap_or(json!({}));
            (decks, setup)
        },
        |app, (decks, setup)| {
            app.anki.loading = false;
            app.anki.due = decks.as_ref().ok().map(|d| top_due(&d["decks"])).filter(|n| *n > 0);
            app.anki.decks = Some(decks.map(|d| d["decks"].clone()));
            app.anki.setup = Some(setup);
        },
    );
}

/// Reload when the page has just been opened (it wasn't drawn last frame).
fn entering(app: &mut App) -> bool {
    let fresh = app.anki.last_drawn + 1 < app.frame_no;
    app.anki.last_drawn = app.frame_no;
    fresh
}

/// Run a background action for a button, then reload.
fn act<T: Send + 'static>(app: &mut App, label: &'static str, fut: impl std::future::Future<Output = Result<T, ApiErr>> + Send + 'static, then: impl FnOnce(&mut App, T) + Send + 'static) {
    app.anki.busy = Some(label);
    app.spawn(fut, move |app, r| {
        app.anki.busy = None;
        match r {
            Ok(v) => then(app, v),
            Err(e) => app.toast(e.message(), true),
        }
        app.anki.decks = None;
        app.anki.review.data = None;
        app.anki.review.error = None;
        app.anki.review.loading = false;
        reload(app);
    });
}

fn btn(app: &App, ui: &mut Ui, label: &str, busy_label: &'static str, primary: bool) -> bool {
    let busy = app.anki.busy == Some(busy_label);
    let text = if busy { busy_label } else { label };
    w::button_if(ui, text, primary, app.anki.busy.is_some()).clicked()
}

// ---------- the setup screens ----------
fn intro(ui: &mut Ui, pane: Pane) {
    h1(ui, pane, "Anki");
    w::sub(ui, "Review your Anki decks here. Anki does the scheduling, so reviews count exactly as they do in Anki.");
}

/// .anki-setup: a panel with a big line, text, and buttons.
fn setup_panel(ui: &mut Ui, big: &str, add: impl FnOnce(&mut Ui)) {
    let tk = t();
    ui.add_space(8.0);
    let wdt = ui.available_width().min(640.0);
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(ui.cursor().min, vec2(wdt, f32::INFINITY))).layout(egui::Layout::top_down(egui::Align::Min)));
    w::boxed(&mut c, egui::Margin { left: 22, right: 22, top: 18, bottom: 18 }, 8.0, |ui| {
        w::text_block(ui, big, Ts::new(17.0, 650, tk.text));
        ui.add_space(4.0);
        add(ui);
    });
    let h = c.min_rect().height();
    ui.allocate_space(vec2(wdt, h));
}

fn para(ui: &mut Ui, text: &str) {
    ui.add_space(6.0);
    w::text_block(ui, text, Ts::body());
    ui.add_space(6.0);
}

fn permission(app: &mut App, ui: &mut Ui, pane: Pane) {
    head(app, ui, pane, "Anki", vec![], None);
    intro(ui, pane);
    let mut allow = false;
    let busy = app.anki.busy == Some("Allowing…");
    setup_panel(ui, "Use Anki from this app?", |ui| {
        para(ui, "The Anki page talks to Anki on this computer through its AnkiConnect add-on: it reads your decks and records your reviews. When you ask it to, it can also install AnkiConnect and a small helper add-on, and start or quit Anki.");
        ui.add_space(4.0);
        allow = w::button_if(ui, if busy { "Allowing…" } else { "Allow" }, true, busy).clicked();
    });
    if allow {
        let svc = app.svc.clone();
        app.anki.busy = Some("Allowing…");
        app.spawn(async move { svc.setup_allow("anki").is_ok() }, |app, ok| {
            app.anki.busy = None;
            if ok {
                app.grant("anki");
            }
            app.anki.decks = None;
            reload(app);
            refresh_badge(app);
        });
    }
}

/// What went wrong talking to Anki, and what to do about it.
fn problem(app: &mut App, ui: &mut Ui, pane: Pane, e: &ApiErr) {
    match e.kind() {
        "permission" => return permission(app, ui, pane),
        "port_taken" => return port_taken(app, ui, pane, e),
        "offline" => {}
        _ => {
            head(app, ui, pane, "Anki", vec![], None);
            h1(ui, pane, "Anki");
            crate::setup::notice_pub(ui, &e.message(), Pill::Warn);
            if actions(ui, |ui| w::button(ui, "Try again").clicked()) {
                app.anki.decks = None;
                reload(app);
            }
            return;
        }
    }
    head(app, ui, pane, "Anki", vec![], None);
    intro(ui, pane);
    let s = app.anki.setup.clone().unwrap_or(json!({}));
    let mut todo: Option<&str> = None;
    if s["launcher"] == false {
        crate::setup::notice_pub(ui, "Anki doesn't seem to be installed.", Pill::Warn);
        actions(ui, |ui| {
            if w::primary(ui, "Get Anki ↗").clicked() {
                todo = Some("get");
            }
            if w::button(ui, "Try again").clicked() {
                todo = Some("retry");
            }
        });
    } else if s["ankiconnect"] == false {
        let running = s["running"].as_bool().unwrap_or(false);
        let busy = app.anki.busy == Some("Setting up…");
        setup_panel(ui, "Set up Anki", |ui| {
            para(ui, "This adds two add-ons to Anki:");
            w::text_block(ui, "•  AnkiConnect, from AnkiWeb, which lets this app read your decks and record your reviews. It gets its own API key, so other programs and web pages can't use it.", Ts::body());
            ui.add_space(4.0);
            w::text_block(ui, "•  A small canvas-mcp helper that lets this app open Anki in the tray, without its window. Opening Anki yourself works as before.", Ts::body());
            para(ui, if running { "Anki will restart to load them." } else { "Anki then starts in the tray." });
            if w::button_if(ui, if busy { "Setting up…" } else { "Set up Anki" }, true, busy).clicked() {
                todo = Some("setup");
            }
            ui.add_space(8.0);
            w::text_block(ui, "Or install AnkiConnect yourself: in Anki, Tools → Add-ons → Get Add-ons…, code 2055492159, then restart Anki.", Ts::faint(12.0));
        });
    } else if s["running"] == true {
        crate::setup::notice_pub(ui, "Anki is open, but AnkiConnect isn't answering. Restarting Anki usually fixes this.", Pill::Warn);
        actions(ui, |ui| {
            if btn(app, ui, "Restart Anki", "Setting up…", true) {
                todo = Some("setup");
            }
            if w::button(ui, "Try again").clicked() {
                todo = Some("retry");
            }
        });
    } else {
        crate::setup::notice_pub(ui, "Anki isn't running.", Pill::Warn);
        let companion = s["companion"].as_bool().unwrap_or(false);
        actions(ui, |ui| {
            let r = w::button_if(ui, if app.anki.busy == Some("Opening Anki…") { "Opening Anki…" } else { "Open in tray" }, true, !companion || app.anki.busy.is_some());
            if !companion {
                r.clone().on_hover_text("Needs the canvas-mcp helper add-on");
            }
            if r.clicked() {
                todo = Some("tray");
            }
            if w::button_if(ui, "Open Anki", false, app.anki.busy.is_some()).clicked() {
                todo = Some("window");
            }
            if w::button(ui, "Try again").clicked() {
                todo = Some("retry");
            }
        });
    }
    let svc = app.svc.clone();
    match todo {
        Some("get") => app.open_external("https://apps.ankiweb.net"),
        Some("retry") => {
            app.anki.decks = None;
            app.anki.review = Review::default();
            reload(app);
        }
        Some("setup") => act(app, "Setting up…", async move { svc.anki_setup_run().await }, |_, _| {}),
        Some("tray") | Some("window") => {
            let tray = todo == Some("tray");
            act(app, "Opening Anki…", async move { svc.anki_launch(tray).await }, |_, _| {})
        }
        _ => {}
    }
}

/// Another program answers where AnkiConnect should.
fn port_taken(app: &mut App, ui: &mut Ui, pane: Pane, e: &ApiErr) {
    head(app, ui, pane, "Anki", vec![], None);
    h1(ui, pane, "Anki");
    let url = fmt::s(&e.body["url"]);
    let port = url.rsplit(':').next().unwrap_or("").to_string();
    let can_move = e.body["can_move"].as_bool().unwrap_or(false);
    let mut todo = None;
    setup_panel(ui, "Another program is using Anki's port", |ui| {
        para(ui, &format!("The app reaches Anki through AnkiConnect at {url}, but a different program on this computer is answering there{}.", if port.is_empty() { String::new() } else { format!(" on port {port}") }));
        if can_move {
            para(ui, "Anki can use a free port instead. Anki restarts in the tray to pick it up. Other tools that talk to AnkiConnect (like Yomitan) would need the new port too.");
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if btn(app, ui, "Use a different port", "Moving Anki…", true) {
                    todo = Some("move");
                }
                if w::button(ui, "Try again").clicked() {
                    todo = Some("retry");
                }
            });
        } else {
            para(ui, "Close that program, or change anki_url in config.toml to where AnkiConnect listens.");
            if w::button(ui, "Try again").clicked() {
                todo = Some("retry");
            }
        }
    });
    let svc = app.svc.clone();
    match todo {
        Some("move") => act(app, "Moving Anki…", async move { svc.anki_move_port().await }, |app, _| refresh_badge(app)),
        Some("retry") => {
            app.anki.decks = None;
            app.anki.review = Review::default();
            reload(app);
        }
        _ => {}
    }
}

// ---------- deck list ----------
fn counts(d: &Value, active: Option<&str>) -> Vec<(String, Color32, u16, bool)> {
    let tk = t();
    [("new", tk.blue), ("learn", tk.bad), ("review", tk.ok)]
        .iter()
        .map(|(k, c)| {
            let n = d[*k].as_i64().unwrap_or(0);
            let (col, wt) = if n == 0 { (tk.faint, 400) } else { (*c, 600) };
            (n.to_string(), col, wt, active == Some(*k))
        })
        .collect()
}

/// The deck list, or `Err(Pending)` while it loads.
fn decks_or_problem(app: &mut App, ui: &mut Ui, pane: Pane) -> Result<Option<Value>, Need> {
    if !app.allowed("anki") {
        permission(app, ui, pane);
        return Ok(None);
    }
    if entering(app) {
        reload(app);
    }
    match app.anki.decks.clone() {
        None => Err(Need::Pending),
        Some(Err(e)) => {
            problem(app, ui, pane, &e);
            Ok(None)
        }
        Some(Ok(d)) => Ok(Some(d)),
    }
}

fn courses_by_id(app: &App) -> (Vec<Value>, Vec<Value>, Option<Value>) {
    let courses = app.d.need0("courses").ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
    let past = app.d.need0("past_courses").ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
    let colors = app.d.need0("colors").ok().map(|v| (*v).clone());
    (courses, past, colors)
}

pub fn decks_view(app: &mut App, ui: &mut Ui, pane: Pane) -> Result<(), Need> {
    app.anki.review = Review::default(); // opening a deck again starts a fresh session
    let Some(decks) = decks_or_problem(app, ui, pane)? else { return Ok(()) };
    let tk = t();
    let imported = std::mem::take(&mut app.anki.imported);
    let (courses, past, colors) = courses_by_id(app);
    let by_id: HashMap<i64, Value> = past.iter().chain(courses.iter()).filter_map(|c| c["id"].as_i64().map(|i| (i, c.clone()))).collect();
    let all = decks.as_array().cloned().unwrap_or_default();
    let code = |c: &Value| fmt::s(&c["course_code"]);
    let mut course_decks: Vec<&Value> = all.iter().filter(|d| d["course_id"].as_i64().map(|i| by_id.contains_key(&i)).unwrap_or(false)).collect();
    course_decks.sort_by_key(|d| code(&by_id[&d["course_id"].as_i64().unwrap()]));
    let others: Vec<&Value> = all.iter().filter(|d| !d["course_id"].as_i64().map(|i| by_id.contains_key(&i)).unwrap_or(false) && !(d["name"] == "Default" && d["total"].as_i64().unwrap_or(0) == 0)).collect();
    let total = top_due(&decks);
    head(app, ui, pane, "Anki", vec![], None);
    h1(ui, pane, "Anki");
    w::sub(ui, &if total > 0 { format!("{total} card{} to study today", if total == 1 { "" } else { "s" }) } else { "All caught up for today".into() });
    let mut todo: Option<&str> = None;
    actions(ui, |ui| {
        if w::primary(ui, "Import course decks").clicked() {
            todo = Some("import");
        }
        if btn(app, ui, "Sync with AnkiWeb", "Syncing…", false) {
            todo = Some("sync");
        }
        if btn(app, ui, "Show Anki", "Showing…", false) {
            todo = Some("window");
        }
        if btn(app, ui, "Quit Anki", "Quitting…", false) {
            todo = Some("quit");
        }
    });
    if app.anki.setup.as_ref().map(|s| s["companion"] == false).unwrap_or(false) {
        let wdt = ui.available_width();
        let (r, _) = ui.allocate_exact_size(vec2(wdt, 41.0), Sense::hover());
        ui.painter().rect_filled(r, cr(8.0), tk.hover);
        let g = w::lay(ui, "Install the canvas-mcp helper add-on to open Anki in the tray, without its window. ", Ts::muted(14.0), Some(wdt - 240.0), true);
        let gw = g.size().x;
        ui.painter().galley(pos2(r.min.x + 14.0, r.center().y - g.size().y / 2.0), g, tk.muted);
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(pos2(r.min.x + 14.0 + gw, r.min.y), r.max)).layout(egui::Layout::left_to_right(egui::Align::Center)));
        if w::linklike(&mut c, "Install it (restarts Anki)", 14.0).clicked() {
            todo = Some("setup");
        }
        ui.add_space(12.0);
    }
    if !imported.is_empty() {
        crate::setup::notice_pub(ui, &format!("Created {}", imported.join(", ")), Pill::Ok);
    }
    group_head(ui, "Courses", |ui| {
        w::text_line(ui, "new · learning · review", Ts::faint(11.0));
    });
    if course_decks.is_empty() {
        w::empty(ui, "No course decks yet. Use “Import course decks” to link a deck to a course or make a new one.");
    } else {
        list(ui, |ui| {
            for (i, d) in course_decks.iter().enumerate() {
                let c = &by_id[&d["course_id"].as_i64().unwrap()];
                let n = d["total"].as_i64().unwrap_or(0);
                row(app, ui, pane, RowSpec {
                    href: Some(format!("#/anki/deck/{}", fmt::id(&d["id"]))),
                    bar: Some(fmt::color_for(&fmt::id(&c["id"]), colors.as_ref())),
                    title: leaf(&fmt::s(&d["name"])).to_string(),
                    meta: Some(format!("{} · {}", fmt::s(&c["name"]), if n > 0 { format!("{n} card{}", if n == 1 { "" } else { "s" }) } else { "no cards yet".into() })),
                    counts: counts(d, None),
                    first: i == 0,
                    ..Default::default()
                });
            }
        });
    }
    if !others.is_empty() {
        w::h2(ui, "Other decks");
        list(ui, |ui| {
            for (i, d) in others.iter().enumerate() {
                let name = fmt::s(&d["name"]);
                let n = d["total"].as_i64();
                row(app, ui, pane, RowSpec {
                    href: Some(format!("#/anki/deck/{}", fmt::id(&d["id"]))),
                    indent: (name.matches("::").count() as f32) * 20.0,
                    title: leaf(&name).to_string(),
                    meta: n.map(|n| format!("{n} card{}", if n == 1 { "" } else { "s" })),
                    counts: counts(d, None),
                    first: i == 0,
                    ..Default::default()
                });
            }
        });
    }
    let svc = app.svc.clone();
    match todo {
        Some("import") => crate::nav::follow(app, "#/anki/import", pane),
        Some("sync") => act(app, "Syncing…", async move { svc.anki_sync().await.map_err(|e| ApiErr::new(e.status, "anki", format!("Couldn't sync: {}", e.message()))) }, |_, _| {}),
        Some("window") => {
            // Anki's launcher takes a few seconds to hand off to the running Anki
            app.anki.busy = Some("Showing…");
            app.spawn(async move { let r = svc.anki_show(); tokio::time::sleep(Duration::from_secs(5)).await; r }, |app, r| {
                app.anki.busy = None;
                if let Err(e) = r {
                    app.toast(e.message(), true);
                }
            });
        }
        Some("quit") => act(app, "Quitting…", async move { svc.anki_quit().await }, |app, _| app.anki.due = None),
        Some("setup") => act(app, "Setting up…", async move { svc.anki_setup_run().await }, |_, _| {}),
        _ => {}
    }
    Ok(())
}

// ---------- import: link each course to a new deck or one of your existing decks ----------
/// "BIOEN 317 A" and "UW::BIOEN 317" -> "BIOEN 317", so an existing deck can be suggested for a course.
fn course_key(s: &str) -> String {
    let re = regex::Regex::new(r"([A-Za-z&]{2,})\s*(\d{3})").unwrap();
    re.captures(s).map(|c| format!("{} {}", &c[1], &c[2]).to_uppercase()).unwrap_or_default()
}

pub fn import_view(app: &mut App, ui: &mut Ui, pane: Pane) -> Result<(), Need> {
    let Some(decks) = decks_or_problem(app, ui, pane)? else { return Ok(()) };
    let (courses, past, colors) = courses_by_id(app);
    let all = decks.as_array().cloned().unwrap_or_default();
    let linked: HashMap<i64, String> = all.iter().filter_map(|d| d["course_id"].as_i64().map(|c| (c, fmt::id(&d["id"])))).collect();
    let decks: Vec<Value> = all.into_iter().filter(|d| d["name"] != "Default" || d["total"].as_i64().unwrap_or(0) > 0).collect();
    let current = |c: i64| linked.get(&c).cloned().unwrap_or_default();
    let choice = |app: &App, c: i64| app.anki.choice.get(&c).cloned().unwrap_or_else(|| current(c));
    let crumbs = vec![crate::views::dashboard_crumb(), ("Anki".into(), Some("#/anki".into()))];
    head(app, ui, pane, "Import course decks", crumbs, None);
    h1(ui, pane, "Import course decks");
    w::sub(ui, "Give a course its own new deck, or link it to a deck you already have. Linked decks show under Courses with the course's color.");
    let unlinked: Vec<i64> = courses.iter().filter_map(|c| c["id"].as_i64()).filter(|c| choice(app, *c).is_empty()).collect();
    let mut new_all = false;
    group_head(ui, "Current courses", |ui| {
        if !unlinked.is_empty() && w::linklike(ui, "New deck for every unlinked course", 13.0).clicked() {
            new_all = true;
        }
    });
    if new_all {
        for c in &unlinked {
            app.anki.choice.insert(*c, "new".into());
        }
    }
    let draw = |app: &mut App, ui: &mut Ui, list_: &[Value]| {
        let tk = t();
        list(ui, |ui| {
            for (i, c) in list_.iter().enumerate() {
                let Some(cid) = c["id"].as_i64() else { continue };
                let label = { let code = fmt::s(&c["course_code"]); if code.is_empty() { fmt::s(&c["name"]) } else { code } };
                let key = course_key(&label);
                let suggested: Vec<&Value> = if key.is_empty() { vec![] } else { decks.iter().filter(|d| course_key(leaf(&fmt::s(&d["name"]))) == key && d["course_id"].is_null()).collect() };
                let v = choice(app, cid);
                let changed = app.anki.choice.get(&cid).map(|x| *x != current(cid)).unwrap_or(false);
                let term = c["term"]["name"].as_str().map(|t_| format!(" · {t_}")).unwrap_or_default();
                let top = ui.cursor().min.y;
                let resp = row(app, ui, pane, RowSpec {
                    bar: Some(fmt::color_for(&cid.to_string(), colors.as_ref())),
                    title: label.clone(),
                    meta: Some(format!("{}{term}", fmt::s(&c["name"]))),
                    changed,
                    first: i == 0,
                    counts: vec![(" ".repeat(60), Color32::TRANSPARENT, 400, false)], // room for the select
                    ..Default::default()
                });
                let r = resp.rect;
                let _ = top;
                // the select (and a suggestion) on the right
                let mut opts: Vec<(String, String)> = vec![(String::new(), "Not linked".into())];
                if !linked.contains_key(&cid) {
                    opts.push(("new".into(), "New deck".into()));
                }
                if !suggested.is_empty() {
                    opts.push(("§sug".into(), "§Suggested".into()));
                    opts.extend(suggested.iter().map(|d| (fmt::id(&d["id"]), fmt::s(&d["name"]))));
                }
                opts.push(("§yours".into(), "§Your decks".into()));
                opts.extend(decks.iter().map(|d| (fmt::id(&d["id"]), fmt::s(&d["name"]))));
                let sw = 240.0f32.min(r.width() * 0.45);
                let sel = Rect::from_min_size(pos2(r.max.x - 14.0 - sw, r.center().y - 16.0), vec2(sw, 32.0));
                let mut c2 = ui.new_child(egui::UiBuilder::new().max_rect(sel).layout(egui::Layout::left_to_right(egui::Align::Center)));
                if let Some(nv) = w::select(&mut c2, Id::new(("anki-link", cid)), &v, &opts, sw, 13.0) {
                    app.anki.choice.insert(cid, nv);
                }
                if v.is_empty() {
                    if let Some(d) = suggested.first() {
                        let name = fmt::s(&d["name"]);
                        let label = format!("Link {name}?");
                        let lw = w::lay(ui, &label, Ts::new(13.0, 400, tk.accent), None, false).size().x;
                        let lr = Rect::from_min_size(pos2(sel.min.x - 12.0 - lw, r.center().y - 10.0), vec2(lw, 20.0));
                        let mut c3 = ui.new_child(egui::UiBuilder::new().max_rect(lr));
                        if w::linklike(&mut c3, &label, 13.0).clicked() {
                            app.anki.choice.insert(cid, fmt::id(&d["id"]));
                        }
                    }
                }
            }
        });
    };
    if courses.is_empty() {
        w::empty(ui, "No current courses.");
    } else {
        draw(app, ui, &courses);
    }
    if !past.is_empty() {
        w::h2(ui, "Past courses");
        draw(app, ui, &past);
    }
    // the bar: how many changes, and Save
    let changes: Vec<(i64, String)> = app.anki.choice.iter().filter(|(c, v)| **v != current(**c)).map(|(c, v)| (*c, v.clone())).collect();
    let n = changes.len();
    let mut todo = None;
    bottom_bar(ui, "anki-import-bar", |ui| {
        let mut rich = w::Rich::new();
        rich.push(&n.to_string(), Ts::new(13.0, 700, t().text));
        rich.push(&format!(" change{}", if n == 1 { "" } else { "s" }), Ts::new(13.0, 400, t().text));
        let g = rich.lay(ui);
        let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
        ui.painter().galley(r.min, g, t().text);
    }, |ui| {
        let saving = app.anki.busy == Some("Saving…");
        if w::button_if(ui, if saving { "Saving…" } else { "Save" }, true, n == 0 || saving).clicked() {
            todo = Some("save");
        }
        if w::button(ui, "Cancel").clicked() {
            todo = Some("cancel");
        }
        if n > 0 && w::button(ui, "Reset").clicked() {
            todo = Some("reset");
        }
    });
    match todo {
        Some("reset") => app.anki.choice.clear(),
        Some("cancel") => crate::nav::follow(app, "#/anki", pane),
        Some("save") => {
            let links: Vec<(i64, Value)> = changes.into_iter().map(|(c, v)| (c, if v.is_empty() { Value::Null } else if v == "new" { json!("new") } else { json!(v.parse::<i64>().unwrap_or(0)) })).collect();
            let svc = app.svc.clone();
            app.anki.busy = Some("Saving…");
            app.spawn(async move { svc.anki_links(links).await }, move |app, r| {
                app.anki.busy = None;
                match r {
                    Ok(v) => {
                        app.anki.imported = v["created"].as_array().into_iter().flatten().map(fmt::s).collect();
                        app.anki.choice.clear();
                        app.anki.decks = None;
                        reload(app);
                        crate::nav::follow(app, "#/anki", pane);
                    }
                    Err(e) => app.toast(e.message(), true),
                }
            });
        }
        _ => {}
    }
    Ok(())
}

/// .builder-bar: a bar that sticks to the bottom of the pane while the page scrolls.
fn bottom_bar(ui: &mut Ui, id: &str, left: impl FnOnce(&mut Ui), right: impl FnOnce(&mut Ui)) {
    let tk = t();
    let wdt = ui.available_width();
    ui.add_space(24.0);
    let (natural, _) = ui.allocate_exact_size(vec2(wdt + 24.0, 57.0), Sense::hover());
    let natural = natural.translate(vec2(-12.0, 0.0));
    let bottom = ui.clip_rect().max.y;
    let r = if natural.max.y > bottom { natural.translate(vec2(0.0, bottom - natural.max.y)) } else { natural };
    let layer_id = egui::LayerId::new(egui::Order::Middle, Id::new(id));
    let layer = ui.ctx().layer_painter(layer_id).with_clip_rect(ui.clip_rect());
    let shadow = egui::Shadow { offset: [0, -6], blur: 20, spread: 0, color: Color32::from_black_alpha(31) };
    layer.add(shadow.as_shape(r, cr(10.0)));
    layer.rect_filled(r, cr(10.0), tk.panel_solid);
    layer.rect_filled(r, cr(10.0), tk.panel);
    layer.rect_stroke(r, cr(10.0), Stroke::new(1.0, tk.line), StrokeKind::Inside);
    let mut l = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(r.min + vec2(14.0, 0.0), pos2(r.center().x, r.max.y))).layout(egui::Layout::left_to_right(egui::Align::Center)).layer_id(layer_id));
    left(&mut l);
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(pos2(r.min.x + 14.0, r.min.y), r.max - vec2(14.0, 0.0))).layout(egui::Layout::right_to_left(egui::Align::Center)).layer_id(layer_id));
    c.spacing_mut().item_spacing.x = 6.0;
    right(&mut c);
}

// ---------- review ----------
fn load_next(app: &mut App, did: i64) {
    let rv = &mut app.anki.review;
    if rv.loading {
        return;
    }
    rv.loading = true;
    let svc = app.svc.clone();
    app.spawn(async move { svc.anki_next(did).await }, move |app, r| {
        let rv = &mut app.anki.review;
        if rv.did != did {
            return;
        }
        rv.loading = false;
        match r {
            Ok(v) => rv.data = Some(v),
            Err(e) => rv.error = Some(e),
        }
    });
}

fn show(app: &mut App) {
    let rv = &mut app.anki.review;
    if rv.data.as_ref().map(|d| !d["card"].is_null()).unwrap_or(false) {
        rv.shown = true;
    }
}

fn answer(app: &mut App, ease: i64) {
    let rv = &mut app.anki.review;
    let Some(card) = rv.data.as_ref().and_then(|d| d["card"]["id"].as_i64()) else { return };
    if rv.busy || !rv.shown {
        return;
    }
    rv.busy = true;
    let did = rv.did;
    let svc = app.svc.clone();
    app.spawn(async move { svc.anki_answer(did, card, ease).await }, move |app, r| {
        let rv = &mut app.anki.review;
        rv.busy = false;
        match r {
            Ok(v) => {
                rv.data = Some(v);
                rv.shown = false;
                rv.reviewed += 1;
                refresh_badge(app);
            }
            Err(e) => rv.error = Some(e),
        }
    });
}

/// Space/Enter shows the answer, then answers Good; 1–4 answer.
pub fn key(app: &mut App, k: &str) -> bool {
    if !matches!(app.route().view, crate::route::View::AnkiDeck(_)) || app.palette.open {
        return false;
    }
    if app.anki.review.data.as_ref().map(|d| d["card"].is_null()).unwrap_or(true) {
        return false;
    }
    match k {
        " " | "Enter" => {
            if app.anki.review.shown {
                answer(app, 3);
            } else {
                show(app);
            }
            true
        }
        "1" | "2" | "3" | "4" => {
            answer(app, k.parse().unwrap());
            true
        }
        _ => false,
    }
}

/// Card HTML from Anki: audio buttons aren't supported here.
fn card_html(html: &str) -> String {
    let re = regex::Regex::new(r"\[anki:play:[qa]:\d+\]|\[sound:[^\]]*\]").unwrap();
    re.replace_all(html, "").into_owned()
}

pub fn review_view(app: &mut App, ui: &mut Ui, pane: Pane, did: i64) -> Result<(), Need> {
    let tk = t();
    if !app.allowed("anki") {
        permission(app, ui, pane);
        return Ok(());
    }
    if app.anki.review.did != did {
        app.anki.review = Review { did, ..Default::default() };
    }
    if app.anki.review.data.is_none() && app.anki.review.error.is_none() {
        load_next(app, did);
    }
    let crumbs_base = vec![crate::views::dashboard_crumb(), ("Anki".into(), Some("#/anki".into()))];
    if let Some(e) = app.anki.review.error.clone() {
        if app.anki.setup.is_none() {
            reload(app);
        }
        head(app, ui, pane, "Anki", crumbs_base, None);
        problem(app, ui, pane, &e);
        // "Try again" (which reloads the decks) starts the deck over
        if app.anki.decks.is_none() {
            app.anki.review = Review { did, ..Default::default() };
        }
        return Ok(());
    }
    let Some(data) = app.anki.review.data.clone() else { return Err(Need::Pending) };
    let name = fmt::s(&data["deck"]["name"]);
    let mut crumbs = crumbs_base;
    let parts: Vec<&str> = name.split("::").collect();
    if parts.len() > 1 {
        crumbs.push((parts[..parts.len() - 1].join(" / "), None));
    }
    head(app, ui, pane, leaf(&name), crumbs, None);
    let card = data["card"].clone();
    let kind = card["kind"].as_str().map(String::from);
    // .title-row: the deck's name and its counts
    let wdt = ui.available_width();
    let cs = counts(&data["counts"], kind.as_deref());
    let title = leaf(&name).to_string();
    let top = ui.cursor().min;
    h1(ui, pane, &title);
    let mut x = top.x + wdt;
    for (text, col, wt, ul) in cs.iter().rev() {
        let g = w::lay(ui, text, Ts::new(13.0, *wt, *col).ul(*ul), None, false);
        let gw = g.size().x;
        ui.painter().galley(pos2(x - gw, top.y + 12.0), g, *col);
        x -= gw.max(18.0) + 10.0;
    }
    let reviewed = app.anki.review.reviewed;
    if card.is_null() {
        // .anki-done
        ui.add_space(48.0);
        let c = |ui: &mut Ui, text: &str, ts: Ts| {
            let g = w::lay(ui, text, ts, Some(wdt), false);
            let (r, _) = ui.allocate_exact_size(vec2(wdt, g.size().y), Sense::hover());
            ui.painter().galley(pos2(r.center().x - g.size().x / 2.0, r.min.y), g, ts.color);
        };
        c(ui, if reviewed > 0 { "Done for now" } else { "Nothing to study" }, Ts::new(22.0, 650, tk.text));
        ui.add_space(6.0);
        let p = format!("{}No more cards are due in this deck today.", if reviewed > 0 { format!("You reviewed {reviewed} card{}. ", if reviewed == 1 { "" } else { "s" }) } else { String::new() });
        c(ui, &p, Ts::muted(14.0));
        ui.add_space(16.0);
        let bw = 130.0 + 150.0 + 8.0;
        let (r, _) = ui.allocate_exact_size(vec2(wdt, 36.0), Sense::hover());
        let mut row_ = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(pos2(r.center().x - bw / 2.0, r.min.y), vec2(bw + 40.0, 36.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
        row_.spacing_mut().item_spacing.x = 8.0;
        let back = w::primary(&mut row_, "Back to decks").clicked();
        let add = w::button(&mut row_, "Add cards in Anki").clicked();
        if back {
            crate::nav::follow(app, "#/anki", pane);
        }
        if add {
            let svc = app.svc.clone();
            app.spawn(async move { svc.anki_add(did).await }, |app, r| {
                if let Err(e) = r {
                    app.toast(e.message(), true);
                }
            });
        }
        return Ok(());
    }
    // .anki-stage: the card in a panel
    let shown = app.anki.review.shown;
    let html = card_html(card[if shown { "answer" } else { "question" }].as_str().unwrap_or(""));
    ui.add_space(16.0);
    let env = Env { size: 20.0, lh: 1.45, align: TAlign::Center, color: tk.text, ..Env::content(pane) };
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(ui.cursor().min, vec2(wdt, f32::INFINITY))).layout(egui::Layout::top_down(egui::Align::Min)));
    w::boxed(&mut c, egui::Margin { left: 28, right: 28, top: 32, bottom: 32 }, 8.0, |ui| {
        let start = ui.cursor().min.y;
        crate::html::bare(app, ui, &html, Flavor::Anki, env);
        let used = ui.cursor().min.y - start;
        if used < 216.0 {
            ui.add_space(216.0 - used);
        }
    });
    let h = c.min_rect().height();
    ui.allocate_space(vec2(wdt, h));
    // the bar: what kind of card, and the buttons
    let busy = app.anki.review.busy;
    let mut todo: Option<i64> = None;
    bottom_bar(ui, "anki-review-bar", |ui| {
        if reviewed > 0 {
            w::text_line(ui, &format!("{reviewed} reviewed"), Ts::new(13.0, 400, tk.text));
        } else {
            let (label, col) = match kind.as_deref() {
                Some("new") => ("New card", tk.blue),
                Some("learn") => ("Learning", tk.bad),
                _ => ("Review", tk.ok),
            };
            w::text_line(ui, label, Ts::new(12.0, 400, col));
        }
    }, |ui| {
        if shown {
            let cols = [tk.bad, tk.warn, tk.ok, tk.blue];
            for i in (0..4).rev() {
                let next = card["next"][i].as_str().unwrap_or("").to_string();
                if ease_button(ui, &next, EASE_LABELS[i], cols[i], busy) {
                    todo = Some(i as i64 + 1);
                }
            }
        } else if show_button(ui) {
            todo = Some(0);
        }
    });
    match todo {
        Some(0) => show(app),
        Some(e) => answer(app, e),
        None => {}
    }
    Ok(())
}

/// .btn.ease: the next interval small above the label.
fn ease_button(ui: &mut Ui, next: &str, label: &str, color: Color32, disabled: bool) -> bool {
    let tk = t();
    let (r, resp) = ui.allocate_exact_size(vec2(84.0, 44.0), if disabled { Sense::hover() } else { Sense::click() });
    let alpha = if disabled { 0.5 } else { 1.0 };
    ui.painter().rect_filled(r, cr(6.0), theme::alpha(tk.panel, alpha));
    ui.painter().rect_stroke(r, cr(6.0), Stroke::new(1.0, theme::alpha(if resp.hovered() { tk.faint } else { tk.line }, alpha)), StrokeKind::Inside);
    w::painter_text(ui, pos2(r.center().x, r.min.y + 5.0), egui::Align2::CENTER_TOP, next, Ts::new(11.0, 400, theme::alpha(tk.muted, alpha)));
    w::painter_text(ui, pos2(r.center().x, r.max.y - 6.0), egui::Align2::CENTER_BOTTOM, label, Ts::new(13.0, 600, theme::alpha(color, alpha)));
    !disabled && resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

/// The primary "Show answer [Space]" button.
fn show_button(ui: &mut Ui) -> bool {
    let tk = t();
    let ts = Ts::new(13.0, 400, Color32::WHITE);
    let g = w::lay(ui, "Show answer", ts, None, false);
    let ks = w::kbd_size(ui, "Space", 10.0);
    let wd = g.size().x + 8.0 + ks.x + 44.0;
    let (r, resp) = ui.allocate_exact_size(vec2(wd, 38.0), Sense::click());
    let bg = if resp.hovered() { theme::mix(tk.accent, Color32::WHITE, 0.9) } else { tk.accent };
    ui.painter().rect_filled(r, cr(6.0), bg);
    let x = r.min.x + 22.0;
    let gw = g.size().x;
    ui.painter().galley(pos2(x, r.center().y - g.size().y / 2.0), g, Color32::WHITE);
    w::paint_kbd(ui, pos2(x + gw + 8.0, r.center().y - ks.y / 2.0), "Space", 10.0, Some(Color32::WHITE), Some(Color32::TRANSPARENT), Some(theme::alpha(Color32::WHITE, 0.5)));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

#[cfg(test)]
mod tests {
    #[test]
    fn course_keys() {
        assert_eq!(super::course_key("BIOEN 317 A"), "BIOEN 317");
        assert_eq!(super::course_key("UW::bioen317"), "BIOEN 317");
        assert_eq!(super::course_key("Intro"), "");
        assert_eq!(super::leaf("A::B::C"), "C");
    }
}
