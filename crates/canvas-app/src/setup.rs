//! First-run setup and signing in through Firefox: the welcome screen, and the sign-in card that
//! Canvas, Panopto and NotebookLM (Google) share. Each site's cookies are read only after you allow
//! it here; the card's first button is that permission.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use egui::{Align2, Color32, Context, CursorIcon, Id, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use serde_json::{Value, json};

use crate::app::{App, Pane};
use crate::data::Need;
use crate::route::View;
use crate::theme::{self, t};
use crate::widgets::{self as w, Pill, Rich, Ts, cr, lay};

const POLL: Duration = Duration::from_millis(1500);
const GIVE_UP: Duration = Duration::from_secs(10 * 60);
const FIREFOX_DOWNLOAD: &str = "https://www.mozilla.org/firefox/new/";

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Idle,
    Checking,
    Waiting,
    NoFirefox,
    Done,
    Error,
}

#[derive(Clone)]
pub struct Signin {
    pub phase: Phase,
    pub message: Option<String>,
    pub info: Value,
    pub quiet: bool,
    pub at: Instant,
    pub since: Instant,
    pub next_poll: Option<Instant>,
    pub quiet_checked: bool,
    pub polling: bool,
}

impl Default for Signin {
    fn default() -> Self {
        Signin { phase: Phase::Idle, message: None, info: Value::Null, quiet: false, at: Instant::now(), since: Instant::now(), next_poll: None, quiet_checked: false, polling: false }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum OnDone {
    Welcome,
    Banner,
    Panopto,
    Google,
}

#[derive(Default)]
pub struct Welcome {
    pub step: Option<String>,
    pub address: String,
    pub found: Option<String>,
    pub find_error: Option<String>,
    pub finding: bool,
    pub find_seq: u64,
    pub find_at: Option<Instant>,
    pub saving: bool,
    pub advance_at: Option<(Instant, &'static str)>,
}

#[derive(Default)]
pub struct Setup {
    pub signins: HashMap<String, Signin>,
    pub welcome: Welcome,
    pub banner_flash_until: Option<Instant>,
    pub on_done: HashMap<String, Vec<OnDone>>,
}

fn host_of(url: &str) -> String {
    url::Url::parse(url).ok().and_then(|u| u.host_str().map(|h| match u.port() {
        Some(p) => format!("{h}:{p}"),
        None => h.to_string(),
    })).unwrap_or_else(|| url.to_string())
}

pub fn perm_for(app: &App, site: &str) -> String {
    match site {
        "canvas" => host_of(&crate::fmt::s(&app.status["base"])),
        "panopto" => crate::fmt::s(&app.status["panopto_host"]),
        other => other.to_string(),
    }
}

fn label_for(app: &App, site: &str) -> String {
    match site {
        "canvas" => host_of(&crate::fmt::s(&app.status["base"])),
        "panopto" => crate::fmt::s(&app.status["panopto_host"]),
        _ => "Google".into(),
    }
}

fn what_for(site: &str) -> &'static str {
    match site {
        "canvas" => "Canvas",
        "panopto" => "Panopto",
        _ => "NotebookLM",
    }
}

fn explain(app: &App, site: &str) -> Rich {
    let tk = t();
    let m = Ts::muted(14.0);
    let b = Ts::new(14.0, 600, tk.text);
    let host = label_for(app, site);
    let mut r = Rich::new();
    match site {
        "canvas" => {
            r.push("The app uses your Canvas login from Firefox, like another Firefox tab would. It reads Firefox's cookies for ", m);
            r.push(&host, b);
            r.push(&format!(" and no other site, keeps them in memory, and sends them only to {host}. It never sees your password."), m);
        }
        "panopto" => {
            r.push("Lecture recordings come from Panopto. The app reads Firefox's cookies for ", m);
            r.push(&host, b);
            r.push(" and no other site, and sends them only there.", m);
        }
        _ => {
            r.push("NotebookLM needs your Google login from Firefox. The app reads only the Google cookies NotebookLM uses (google.com, googleusercontent.com, notebooklm.google, youtube.com) and keeps them in memory. That login covers your whole Google account, and the NotebookLM client is unofficial, so allow this only if you're comfortable with both.", m);
        }
    }
    r
}

pub fn state<'a>(app: &'a mut App, site: &str) -> &'a mut Signin {
    app.setup.signins.entry(site.to_string()).or_default()
}

pub fn when_signed_in(app: &mut App, site: &str, what: OnDone) {
    let v = app.setup.on_done.entry(site.to_string()).or_default();
    if !v.contains(&what) {
        v.push(what);
    }
}

fn check(app: &mut App, site: &str, then: impl FnOnce(&mut App, Value) + Send + 'static) {
    let (svc, s) = (app.svc.clone(), site.to_string());
    app.spawn(async move { svc.setup_check(&s).await }, then);
}

fn finish(app: &mut App, site: &str, info: Value, quiet: bool) {
    let s = state(app, site);
    s.phase = Phase::Done;
    s.info = info.clone();
    s.quiet = quiet;
    s.at = Instant::now();
    s.next_poll = None;
    if !quiet {
        // Bring the app back from Firefox so you see who you signed in as.
        if let Some(ctx) = app.ctx() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }
    let fns = app.setup.on_done.remove(site).unwrap_or_default();
    for f in fns {
        match f {
            OnDone::Welcome => {
                if app.path_is(&View::Welcome) {
                    app.setup.welcome.advance_at = Some((Instant::now() + Duration::from_millis(1100), "ready"));
                }
            }
            OnDone::Banner => {
                app.setup.banner_flash_until = Some(Instant::now() + Duration::from_millis(3500));
                app.d.clear_all(); // everything may have changed while signed out
                let deps: Vec<String> = app.main.deps.iter().cloned().collect();
                app.d.revalidate(deps.iter(), true);
            }
            OnDone::Panopto => app.d.forget_prefix("recordings:"),
            OnDone::Google => app.d.forget("notebooks"),
        }
    }
}

/// The button press: allow reading this site's cookies, then sign in (only opening Firefox if needed).
pub fn start(app: &mut App, site: &str) {
    let s = state(app, site);
    s.phase = Phase::Checking;
    s.message = None;
    s.next_poll = None;
    let perm = perm_for(app, site);
    let site_s = site.to_string();
    if !app.allowed(&perm) {
        let svc = app.svc.clone();
        let s2 = site_s.clone();
        app.spawn(async move { svc.setup_allow(&s2) }, move |app, r| match r {
            Err(e) => {
                let s = state(app, &site_s);
                s.phase = Phase::Error;
                s.message = Some(e.message());
            }
            Ok(_) => {
                let perm = perm_for(app, &site_s);
                app.grant(&perm);
                after_allowed(app, &site_s);
            }
        });
    } else {
        after_allowed(app, site);
    }
}

fn after_allowed(app: &mut App, site: &str) {
    let s = site.to_string();
    check(app, site, move |app, r| match r["state"].as_str() {
        Some("signed_in") => finish(app, &s, r, false),
        Some("error") => {
            let st = state(app, &s);
            st.phase = Phase::Error;
            st.message = r["message"].as_str().map(String::from);
        }
        // 'no_firefox' may only mean Firefox was never opened (no profile yet): opening it makes one.
        _ => open_login(app, &s),
    });
}

pub fn open_login(app: &mut App, site: &str) {
    let (svc, s) = (app.svc.clone(), site.to_string());
    let s2 = s.clone();
    app.spawn(async move { svc.setup_open(&s2).await }, move |app, r| {
        let st = state(app, &s);
        match r {
            Ok(_) => {
                st.phase = Phase::Waiting;
                st.since = Instant::now();
                st.next_poll = Some(Instant::now() + POLL);
            }
            Err(e) if e.kind() == "no_firefox" => st.phase = Phase::NoFirefox,
            Err(e) => {
                st.phase = Phase::Error;
                st.message = Some(e.message());
            }
        }
    });
}

pub fn cancel(app: &mut App, site: &str) {
    let s = state(app, site);
    s.phase = Phase::Idle;
    s.message = None;
    s.next_poll = None;
}

/// Already allowed and signed in? Find out without opening Firefox.
pub fn quiet_check(app: &mut App, site: &str) {
    let perm = perm_for(app, site);
    let allowed = app.allowed(&perm);
    let s = state(app, site);
    if s.phase != Phase::Idle || s.quiet_checked || !allowed {
        return;
    }
    s.quiet_checked = true;
    s.phase = Phase::Checking;
    let site_s = site.to_string();
    check(app, site, move |app, r| {
        if r["state"] == "signed_in" {
            finish(app, &site_s, r, true);
        } else {
            state(app, &site_s).phase = if r["state"] == "no_firefox" { Phase::NoFirefox } else { Phase::Idle };
        }
    });
}

/// Poll the sites being signed in to; run delayed steps. Called every frame.
fn tick(app: &mut App) {
    let due: Vec<String> = app.setup.signins.iter().filter(|(_, s)| s.phase == Phase::Waiting && !s.polling && s.next_poll.map(|t| Instant::now() >= t).unwrap_or(false)).map(|(k, _)| k.clone()).collect();
    for site in due {
        let s = state(app, &site);
        if s.since.elapsed() > GIVE_UP {
            s.phase = Phase::Idle;
            s.message = Some("Still not signed in. Try again when you're ready.".into());
            continue;
        }
        s.polling = true;
        let site2 = site.clone();
        check(app, &site, move |app, r| {
            let s = state(app, &site2);
            s.polling = false;
            if s.phase != Phase::Waiting {
                return; // cancelled meanwhile
            }
            if r["state"] == "signed_in" {
                finish(app, &site2, r, false);
            } else {
                state(app, &site2).next_poll = Some(Instant::now() + POLL);
            }
        });
    }
    if app.setup.signins.values().any(|s| s.phase == Phase::Waiting || s.phase == Phase::Checking) {
        if let Some(ctx) = app.ctx() {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }
    if let Some((at, step)) = app.setup.welcome.advance_at {
        if Instant::now() >= at {
            app.setup.welcome.advance_at = None;
            if step == "ready" {
                app.setup.welcome.step = Some("ready".into());
                let fragile = state(app, "canvas").info["fragile"].as_bool().unwrap_or(false);
                if !fragile {
                    app.setup.welcome.advance_at = Some((Instant::now() + Duration::from_millis(1600), "finish"));
                }
            } else if step == "finish" && app.setup.welcome.step.as_deref() == Some("ready") && app.path_is(&View::Welcome) {
                finish_welcome(app);
            }
        } else if let Some(ctx) = app.ctx() {
            ctx.request_repaint_after(at - Instant::now());
        }
    }
    // The school finder, 450ms after typing stops.
    if let Some(at) = app.setup.welcome.find_at {
        if Instant::now() >= at {
            app.setup.welcome.find_at = None;
            let (svc, addr, seq) = (app.svc.clone(), app.setup.welcome.address.clone(), app.setup.welcome.find_seq);
            app.spawn(async move { svc.setup_find(&addr).await }, move |app, r| {
                let wl = &mut app.setup.welcome;
                if seq != wl.find_seq {
                    return; // typed more since
                }
                wl.finding = false;
                match r {
                    Ok(v) => wl.found = v["url"].as_str().map(String::from),
                    Err(e) => wl.find_error = Some(e.message()),
                }
            });
        } else if let Some(ctx) = app.ctx() {
            ctx.request_repaint_after(at - Instant::now());
        }
    }
}

pub fn status_changed(app: &mut App) {
    let _ = app;
}

fn finish_welcome(app: &mut App) {
    app.setup.welcome.step = None;
    crate::nav::go(app, "#/");
}

// --- the sign-in card ------------------------------------------------------------------------------------
fn avatar(app: &mut App, ui: &Ui, rect: Rect, info: &Value) {
    let tk = t();
    let u = &info["user"];
    if let Some(av) = u["avatar"].as_str().filter(|a| !a.ends_with("avatar-50.png") && a.starts_with("https:")) {
        let base = crate::fmt::s(&app.status["base"]);
        let src = if host_of(av) == host_of(&base) { crate::images::Src::Proxy(av.to_string()) } else { crate::images::Src::Web(av.to_string()) };
        if let Some((tex, _)) = app.image(ui.ctx(), &src) {
            // a circle: the image drawn as a textured circle mesh
            let mut mesh = egui::Mesh::with_texture(tex);
            let n = 48;
            let c = rect.center();
            let r = rect.width() / 2.0;
            mesh.vertices.push(egui::epaint::Vertex { pos: c, uv: pos2(0.5, 0.5), color: Color32::WHITE });
            for i in 0..=n {
                let a = i as f32 / n as f32 * std::f32::consts::TAU;
                mesh.vertices.push(egui::epaint::Vertex { pos: c + vec2(a.cos(), a.sin()) * r, uv: pos2(0.5 + a.cos() * 0.5, 0.5 + a.sin() * 0.5), color: Color32::WHITE });
                if i > 0 {
                    mesh.indices.extend([0, i as u32, i as u32 + 1]);
                }
            }
            ui.painter().add(mesh);
            return;
        }
    }
    let name = u["name"].as_str().map(String::from).or_else(|| {
        let au = info["authuser"].as_i64();
        info["accounts"].as_array().and_then(|a| a.iter().find(|x| x["authuser"].as_i64() == au).and_then(|x| x["email"].as_str().map(String::from)))
    });
    let name = name.unwrap_or_else(|| "?".into());
    let initials: String = name.split(|c: char| c.is_whitespace() || c == '@' || c == '.').filter(|x| !x.is_empty()).take(2).filter_map(|w| w.chars().next()).flat_map(|c| c.to_uppercase()).collect();
    ui.painter().circle_filled(rect.center(), rect.width() / 2.0, tk.accent_soft);
    w::centered_text(ui, rect, &initials, Ts::new(17.0, 650, tk.accent));
}

fn signed_in_as(_app: &App, site: &str, info: &Value) -> String {
    if site == "google" {
        let au = info["authuser"].as_i64();
        let accts = info["accounts"].as_array();
        return accts.and_then(|a| a.iter().find(|x| x["authuser"].as_i64() == au).or(a.first())).and_then(|x| x["email"].as_str().map(String::from)).unwrap_or_else(|| "your Google account".into());
    }
    info["user"]["name"].as_str().filter(|n| !n.is_empty()).map(String::from).unwrap_or_else(|| format!("your {} account", what_for(site)))
}

/// "Signed in to … as …": the avatar, with a check that pops in.
pub fn verified(app: &mut App, ui: &mut Ui, site: &str, info: &Value, at: Instant) {
    let tk = t();
    let el = at.elapsed().as_secs_f32();
    // .verified: rise .3s ease-out
    let rise = crate::anim::ease_out((el / 0.3).min(1.0));
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::hover());
    let row = row.translate(vec2(0.0, 8.0 * (1.0 - rise)));
    let op = rise;
    let av = Rect::from_min_size(pos2(row.min.x, row.center().y - 24.0), vec2(48.0, 48.0));
    ui.scope(|ui| {
        ui.multiply_opacity(op);
        avatar(app, ui, av, info);
        // the tick: pops in (.45s, overshooting) after .15s, then its check draws (.3s after .45s)
        let pop = crate::anim::cubic_bezier(0.3, 1.6, 0.5, 1.0, crate::anim::progress(at, 0.15, 0.45));
        let tick = Rect::from_center_size(av.max - vec2(7.0, 7.0), vec2(22.0, 22.0) * pop.max(0.0));
        if pop > 0.0 {
            ui.painter().circle_filled(tick.center(), 11.0 * pop, tk.ok);
            ui.painter().circle_stroke(tick.center(), 11.0 * pop, Stroke::new(2.0, tk.panel_solid));
            let draw = crate::anim::ease_out(crate::anim::progress(at, 0.45, 0.3));
            if draw > 0.0 {
                let s = tick.width() / 22.0 * (13.0 / 16.0);
                let o = tick.center() - vec2(6.5, 6.5) * (tick.width() / 22.0);
                let pts = [(3.5, 8.5), (6.5, 11.5), (12.5, 4.5)];
                let lens = [((3.0f32).powi(2) * 2.0).sqrt(), ((6.0f32).powi(2) + 7.0f32.powi(2)).sqrt()];
                let total = lens[0] + lens[1];
                let mut left = draw * total;
                let p = |i: usize| o + vec2(pts[i].0, pts[i].1) * s;
                let mut line = vec![p(0)];
                for i in 0..2 {
                    if left <= 0.0 {
                        break;
                    }
                    let f = (left / lens[i]).min(1.0);
                    line.push(p(i) + (p(i + 1) - p(i)) * f);
                    left -= lens[i];
                }
                ui.painter().add(egui::Shape::line(line, Stroke::new(2.4 * s, Color32::WHITE)));
            }
        }
        let x = av.max.x + 14.0;
        let label = format!("Signed in to {} as", label_for(app, site));
        ui.painter().galley(pos2(x, row.center().y - 21.0), lay(ui, &label, Ts::muted(12.0), None, false), tk.muted);
        let name = signed_in_as(app, site, info);
        ui.painter().galley(pos2(x, row.center().y - 3.0), lay(ui, &name, Ts::new(17.0, 650, tk.text), None, false), tk.text);
    });
    if el < 1.0 {
        ui.ctx().request_repaint();
    }
}

fn notice(ui: &mut Ui, text: &str, kind: Pill) {
    let tk = t();
    let (bg, fg) = match kind {
        Pill::Ok => (tk.ok_soft, tk.ok),
        Pill::Plain => (tk.hover, tk.muted),
        _ => (tk.warn_soft, tk.warn),
    };
    ui.add_space(12.0);
    let wdt = ui.available_width();
    let g = lay(ui, text, Ts::new(14.0, 400, fg), Some(wdt - 28.0), false);
    let (r, _) = ui.allocate_exact_size(vec2(wdt, g.size().y + 20.0), Sense::hover());
    ui.painter().rect_filled(r, cr(8.0), bg);
    ui.painter().galley(r.min + vec2(14.0, 10.0), g, fg);
    ui.add_space(12.0);
}

pub fn notice_pub(ui: &mut Ui, text: &str, kind: Pill) {
    notice(ui, text, kind)
}

/// The sign-in card for a site, in its current phase.
pub fn signin_card(app: &mut App, ui: &mut Ui, site: &str, compact: bool) {
    let tk = t();
    let s = state(app, site).clone();
    let perm = perm_for(app, site);
    let allowed = app.allowed(&perm);
    let label = label_for(app, site);
    let what = what_for(site);
    let pad = if compact { egui::Margin { left: 14, right: 14, top: 10, bottom: 10 } } else { egui::Margin { left: 20, right: 20, top: 16, bottom: 16 } };
    ui.add_space(if compact { 4.0 } else { 8.0 });
    let max_w = ui.available_width().min(640.0);
    let mut act: Option<&str> = None;
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(ui.cursor().min, vec2(max_w, f32::INFINITY))).layout(egui::Layout::top_down(egui::Align::Min)));
    w::boxed(&mut child, pad, 8.0, |ui| match s.phase {
        Phase::Checking => {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                w::spinner(ui, 14.0);
                w::text_line_fixed(ui, &format!("Checking Firefox for your {what} login…"), Ts::body());
            });
        }
        Phase::Waiting => {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                ui.add_space(0.0);
                ui.vertical(|ui| {
                    ui.add_space(3.0);
                    w::spinner(ui, 14.0);
                });
                ui.vertical(|ui| {
                    w::text_line_fixed(ui, &format!("Log in to {label} in Firefox."), Ts::new(14.0, 700, tk.text));
                    w::text_block(ui, "This updates as soon as you're in. Firefox can take up to 15 seconds to save a new login.", Ts::faint(12.0));
                });
            });
            ui.add_space(12.0);
            w::hwrap(ui, vec2(8.0, 8.0), |ui| {
                if w::button(ui, "Open the login page again").clicked() {
                    act = Some("open");
                }
                if w::button(ui, "Cancel").clicked() {
                    act = Some("cancel");
                }
            });
        }
        Phase::NoFirefox => {
            notice(ui, "Firefox isn't installed, or hasn't been opened yet. This app signs in with your Firefox login.", Pill::Warn);
            w::hwrap(ui, vec2(8.0, 8.0), |ui| {
                if w::primary(ui, "Get Firefox ↗").clicked() {
                    act = Some("get");
                }
                if w::button(ui, "Try again").clicked() {
                    act = Some("start");
                }
            });
        }
        Phase::Done => verified(app, ui, site, &s.info, s.at),
        _ => {
            if !allowed {
                let g = explain(app, site).wrap(ui.available_width()).lay(ui);
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), g.size().y), Sense::hover());
                ui.painter().galley(r.min, g, tk.muted);
            }
            if s.message.is_some() || s.phase == Phase::Error {
                notice(ui, s.message.as_deref().unwrap_or("Something went wrong."), Pill::Warn);
            } else {
                ui.add_space(12.0);
            }
            w::hwrap(ui, vec2(8.0, 8.0), |ui| {
                if w::primary(ui, if allowed { "Sign in with Firefox" } else { "Allow and sign in with Firefox" }).clicked() {
                    act = Some("start");
                }
            });
        }
    });
    let h = child.min_rect().height();
    ui.allocate_space(vec2(max_w, h));
    ui.add_space(if compact { 4.0 } else { 8.0 });
    match act {
        Some("start") => {
            if site == "canvas" {
                when_signed_in(app, "canvas", if app.path_is(&View::Welcome) { OnDone::Welcome } else { OnDone::Banner });
            }
            start(app, site);
        }
        Some("open") => open_login(app, site),
        Some("cancel") => cancel(app, site),
        Some("get") => app.open_external(FIREFOX_DOWNLOAD),
        _ => {}
    }
}

// --- welcome: school → sign in → ready ---------------------------------------------------------------
fn theme_picker(app: &mut App, ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (v, label) in [("", "System"), ("light", "Light"), ("dark", "Dark")] {
            if w::chip(ui, label, app.theme.pref == v).clicked() {
                crate::nav::set_theme(app, v);
            }
        }
    });
}

pub fn theme_picker_pub(app: &mut App, ui: &mut Ui) {
    theme_picker(app, ui)
}

pub fn welcome(app: &mut App, ui: &mut Ui, pane: Pane) -> Result<(), Need> {
    tick(app);
    let tk = t();
    if app.setup.welcome.step.is_none() {
        app.setup.welcome.step = Some(if app.status["configured"].as_bool().unwrap_or(false) { "signin".into() } else { "school".into() });
    }
    let step = app.setup.welcome.step.clone().unwrap();
    // The sign-in step asks Firefox right away when the permission was already given.
    if step == "signin" && state(app, "canvas").phase == Phase::Idle && app.allowed(&perm_for(app, "canvas")) {
        when_signed_in(app, "canvas", OnDone::Welcome);
        quiet_check(app, "canvas");
    }
    let host = host_of(&crate::fmt::s(&app.status["base"]));
    let title = match step.as_str() {
        "school" => "Which school's Canvas do you use?".to_string(),
        "signin" => if state(app, "canvas").phase == Phase::Done { "You're signed in".into() } else { "Sign in to Canvas".into() },
        _ => "You're signed in".into(),
    };
    crate::views::out(app, pane).title = title.clone();
    // .welcome: centered in the window; .welcome-card: up to 520px
    let screen_h = ui.ctx().content_rect().height();
    let avail = ui.available_width();
    let card_w = avail.min(520.0);
    let x0 = ui.cursor().min.x + (avail - card_w) / 2.0;
    let est_h = 420.0;
    let top_pad = ((screen_h - 108.0 - est_h) / 2.0).max(0.0);
    ui.add_space(top_pad);
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(pos2(x0, ui.cursor().min.y), vec2(card_w, f32::INFINITY))).layout(egui::Layout::top_down(egui::Align::Min)));
    // top: brand and the theme picker
    let (top, _) = c.allocate_exact_size(vec2(card_w, 28.0), Sense::hover());
    let lr = Rect::from_min_size(pos2(top.min.x, top.center().y - 9.0), vec2(18.0, 18.0));
    c.painter().rect_filled(lr, cr(5.0), tk.accent);
    w::painter_text(&c, pos2(lr.max.x + 9.0, top.center().y), Align2::LEFT_CENTER, "Canvas", Ts::new(15.0, 650, tk.text));
    let mut tp = c.new_child(egui::UiBuilder::new().max_rect(top).layout(egui::Layout::right_to_left(egui::Align::Center)));
    tp.horizontal(|ui| {
        // right-aligned: draw the chips from their total width
        let _ = ui;
    });
    let chips_w = 3.0 * 4.0 + ["System", "Light", "Dark"].iter().map(|l| lay(&c, l, Ts::new(12.0, 400, tk.muted), None, false).size().x + 24.0).sum::<f32>();
    let mut tp = c.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(pos2(top.max.x - chips_w, top.min.y), vec2(chips_w, 28.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
    theme_picker(app, &mut tp);
    c.add_space(22.0);
    // .steps
    let idx = ["school", "signin", "ready"].iter().position(|s| *s == step).unwrap_or(0);
    let (sr, _) = c.allocate_exact_size(vec2(card_w, 18.0), Sense::hover());
    let mut x = sr.min.x;
    for (i, label) in ["Your school", "Sign in", "Ready"].iter().enumerate() {
        if i > 0 {
            c.painter().hline(x..=x + 18.0, sr.center().y, Stroke::new(1.0, tk.line));
            x += 18.0 + 6.0;
        }
        let ts = if i < idx { Ts::new(12.0, 400, tk.ok) } else if i == idx { Ts::new(12.0, 600, tk.text) } else { Ts::new(12.0, 400, tk.faint) };
        let g = lay(&c, label, ts, None, false);
        let gw = g.size().x;
        c.painter().galley(pos2(x, sr.center().y - g.size().y / 2.0), g, ts.color);
        x += gw + 6.0;
    }
    c.add_space(22.0);
    let h1s = Ts::new(26.0, 650, tk.text).sp(-0.01);
    w::text_block(&mut c, &title, h1s);
    c.add_space(4.0);
    match step.as_str() {
        "school" => {
            w::sub(&mut c, "Type its Canvas address, or just your school's short name.");
            c.add_space(-18.0 + 6.0);
            let id = Id::new("school");
            let mut addr = app.setup.welcome.address.clone();
            let btn_w = lay(&c, "Continue", Ts::new(14.0, 400, tk.text), None, false).size().x + 34.0;
            let (row, _) = c.allocate_exact_size(vec2(card_w, 45.0), Sense::hover());
            let mut ic = c.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(row.min, vec2(card_w - btn_w - 8.0, 45.0))));
            if app.settings.autofocus_school {
                app.settings.autofocus_school = false;
                ic.memory_mut(|m| m.request_focus(id));
            }
            let resp = w::input(&mut ic, id, &mut addr, "canvas.school.edu", w::InputOpts { size: 16.0, pad: vec2(12.0, 9.0), focus_ring: false, ..Default::default() });
            if resp.changed() {
                let wl = &mut app.setup.welcome;
                wl.address = addr.clone();
                wl.found = None;
                wl.find_error = None;
                wl.find_seq += 1;
                wl.finding = addr.trim().chars().count() >= 2;
                wl.find_at = if wl.finding { Some(Instant::now() + Duration::from_millis(450)) } else { None };
            }
            let enter = resp.lost_focus() && c.input(|i| i.key_pressed(egui::Key::Enter));
            let mut bc = c.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(pos2(row.max.x - btn_w, row.min.y), vec2(btn_w, 45.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
            let found = app.setup.welcome.found.clone();
            let saving = app.setup.welcome.saving;
            let b = w::button_ex(&mut bc, if saving { "Saving…" } else { "Continue" }, w::ButtonOpts { kind: w::Btn::Primary, disabled: found.is_none() || saving, size: 14.0, pad: vec2(16.0, 8.0), pressed: false });
            if (b.clicked() || enter) && found.is_some() && !saving {
                choose_school(app, found.unwrap());
            }
            c.add_space(8.0);
            // .find-status
            let wl = &app.setup.welcome;
            let (fr, _) = c.allocate_exact_size(vec2(card_w, 22.0), Sense::hover());
            if wl.finding {
                w::paint_spinner(&c, pos2(fr.min.x + 7.0, fr.center().y), 14.0, 2.0, tk.line, tk.accent);
                w::painter_text(&c, pos2(fr.min.x + 22.0, fr.center().y), Align2::LEFT_CENTER, "Looking for Canvas…", Ts::muted(13.0));
            } else if let Some(u) = &wl.found {
                let g = Rich::new().add("✓ Found Canvas at ", Ts::new(13.0, 400, tk.ok)).add(&host_of(u), Ts::new(13.0, 700, tk.ok)).lay(&c);
                c.painter().galley(pos2(fr.min.x, fr.center().y - g.size().y / 2.0), g, tk.ok);
            } else if let Some(e) = &wl.find_error {
                w::painter_text(&c, pos2(fr.min.x, fr.center().y), Align2::LEFT_CENTER, e, Ts::new(13.0, 400, tk.bad));
            }
            if app.status["configured"].as_bool().unwrap_or(false) && app.d.has("self") {
                c.add_space(14.0);
                if w::linklike(&mut c, &format!("Keep using {host}"), 14.0).clicked() {
                    finish_welcome(app);
                }
            }
        }
        "signin" => {
            let mut change = false;
            c.horizontal(|ui| {
                w::text_line_fixed(ui, &format!("{host} · "), Ts::muted(14.0));
                change = w::linklike(ui, "change school", 14.0).clicked();
            });
            c.add_space(18.0);
            if change {
                cancel(app, "canvas");
                let base = crate::fmt::s(&app.status["base"]);
                let wl = &mut app.setup.welcome;
                wl.step = Some("school".into());
                wl.address = host_of(&base);
                wl.found = Some(base);
            }
            signin_card(app, &mut c, "canvas", false);
        }
        _ => {
            w::sub(&mut c, &host);
            let s = state(app, "canvas").clone();
            let fragile = s.info["fragile"].as_bool().unwrap_or(false);
            w::boxed(&mut c, egui::Margin { left: 20, right: 20, top: 16, bottom: 16 }, 8.0, |ui| verified(app, ui, "canvas", &s.info, s.at));
            if fragile {
                notice(&mut c, "Firefox is set to forget this login when it closes, so you'd have to log in again every time. Turn on Settings → General → Startup → Open previous windows and tabs in Firefox to keep it.", Pill::Warn);
                if w::button(&mut c, "Open Firefox settings").clicked() {
                    let svc = app.svc.clone();
                    app.fire(async move {
                        let _ = svc.setup_open("firefox_settings").await;
                    });
                }
            }
            c.add_space(12.0);
            if w::primary(&mut c, if fragile { "Continue anyway" } else { "Go to my courses" }).clicked() {
                finish_welcome(app);
            }
            c.add_space(12.0);
            let syncing = app.status["syncing"].as_bool().unwrap_or(false);
            w::text_line(&mut c, &format!("Loading your courses in the background{}", if syncing { "…" } else { "." }), Ts::faint(12.0));
        }
    }
    let h = c.min_rect().height();
    ui.allocate_space(vec2(avail, h + top_pad));
    Ok(())
}

fn choose_school(app: &mut App, url: String) {
    app.setup.welcome.saving = true;
    let svc = app.svc.clone();
    app.spawn(async move { svc.setup_canvas(&url).await }, |app, r| {
        app.setup.welcome.saving = false;
        match r {
            Err(e) => {
                app.setup.welcome.found = None;
                app.setup.welcome.find_error = Some(e.message());
            }
            Ok(v) => {
                app.status["base"] = v["url"].clone();
                app.status["configured"] = json!(true);
                app.setup.signins.insert("canvas".into(), Signin::default());
                app.setup.welcome.step = Some("signin".into());
            }
        }
    });
}

// --- the banner: Canvas login expired, or not yet allowed ---------------------------------------------
pub fn banner(app: &mut App, ctx: &Context) {
    tick(app);
    if app.path_is(&View::Welcome) {
        return;
    }
    let tk = t();
    let s = state(app, "canvas").clone();
    let host = host_of(&crate::fmt::s(&app.status["base"]));
    let session = crate::fmt::s(&app.status["session"]);
    enum Part {
        Spin,
        Text(Rich),
        Btn(&'static str, &'static str),
    }
    let mut parts: Vec<Part> = Vec::new();
    let inv = Ts::new(13.0, 400, tk.bg);
    if s.phase == Phase::Done && app.setup.banner_flash_until.map(|t| Instant::now() < t).unwrap_or(false) {
        parts.push(Part::Text(Rich::new().add("✓ Signed in as ", inv).add(&signed_in_as(app, "canvas", &s.info), Ts::new(13.0, 650, tk.bg))));
        ctx.request_repaint_after(Duration::from_millis(250));
    } else if session == "permission" || session == "expired" {
        match s.phase {
            Phase::Waiting => {
                parts.push(Part::Spin);
                parts.push(Part::Text(Rich::new().add(&format!("Log in to {host} in Firefox. This goes away once you're in."), inv)));
                parts.push(Part::Btn("Cancel", "cancel"));
            }
            Phase::Checking => {
                parts.push(Part::Spin);
                parts.push(Part::Text(Rich::new().add("Checking Firefox…", inv)));
            }
            Phase::NoFirefox => {
                parts.push(Part::Text(Rich::new().add("Firefox isn't installed. This app signs in with your Firefox login.", inv)));
                parts.push(Part::Btn("Get Firefox", "get"));
            }
            _ if session == "permission" => {
                parts.push(Part::Text(Rich::new().add(&format!("Showing saved data. Allow the app to use your Firefox login for {host} to get new data."), inv)));
                parts.push(Part::Btn("Allow", "signin"));
            }
            _ => {
                parts.push(Part::Text(Rich::new().add("Your Canvas login expired, so this is saved data.", inv)));
                parts.push(Part::Btn("Sign in with Firefox", "signin"));
            }
        }
    }
    if parts.is_empty() {
        return;
    }
    let screen = ctx.content_rect();
    let mut act: Option<&str> = None;
    let max_text = (screen.width() * 0.9 - 200.0).max(200.0);
    egui::Area::new(Id::new("banner")).order(egui::Order::Foreground).anchor(Align2::CENTER_BOTTOM, vec2(0.0, -18.0)).show(ctx, |ui| {
        let bg = ui.painter().add(egui::Shape::Noop);
        let start = ui.cursor().min;
        let inner = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            ui.add_space(2.0);
            for p in &parts {
                match p {
                    Part::Spin => {
                        let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                        w::paint_spinner(ui, r.center(), 14.0, 2.0, theme::mix(tk.bg, Color32::TRANSPARENT, 0.35), tk.bg);
                    }
                    Part::Text(r) => {
                        let g = Rich { job: r.job.clone() }.wrap(max_text).lay(ui);
                        let (rr, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                        ui.painter().galley(rr.min, g, tk.bg);
                    }
                    Part::Btn(label, a) => {
                        let g = lay(ui, label, Ts::new(13.0, 400, tk.bg), None, false);
                        let (r, resp) = ui.allocate_exact_size(g.size() + vec2(18.0, 6.0), Sense::click());
                        ui.painter().rect_stroke(r, cr(5.0), Stroke::new(1.0, tk.bg), StrokeKind::Inside);
                        ui.painter().galley(r.center() - g.size() / 2.0, g, tk.bg);
                        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                            act = Some(a);
                        }
                    }
                }
            }
            ui.add_space(2.0);
        });
        let r = Rect::from_min_max(start, inner.response.rect.max).expand2(vec2(14.0, 10.0));
        let shadow = egui::Shadow { offset: [0, 6], blur: 24, spread: 0, color: Color32::from_black_alpha(51) };
        ui.painter().set(bg, egui::Shape::Vec(vec![shadow.as_shape(r, cr(8.0)).into(), egui::Shape::rect_filled(r, cr(8.0), tk.text)]));
    });
    match act {
        Some("cancel") => cancel(app, "canvas"),
        Some("get") => app.open_external(FIREFOX_DOWNLOAD),
        Some("signin") => {
            when_signed_in(app, "canvas", OnDone::Banner);
            start(app, "canvas");
        }
        _ => {}
    }
}

// --- Settings: your school, and what the app may use -------------------------------------------------
pub fn permissions_section(app: &mut App, ui: &mut Ui) {
    if app.demo() {
        return;
    }
    let tk = t();
    let host = host_of(&crate::fmt::s(&app.status["base"]));
    let panopto = crate::fmt::s(&app.status["panopto_host"]);
    w::h2(ui, "School");
    let mut change = false;
    panel_rows(ui, |ui| {
        perm_row(ui, &[(if host.is_empty() { "Not set up" } else { &host }, true)], "Your school's Canvas", |ui| {
            change = w::button(ui, "Change school").clicked();
        });
    });
    if change {
        let base = crate::fmt::s(&app.status["base"]);
        let wl = &mut app.setup.welcome;
        wl.step = Some("school".into());
        wl.address = host_of(&base);
        wl.found = Some(base);
        wl.find_error = None;
        crate::nav::go(app, "#/welcome");
    }
    w::h2(ui, "Permissions");
    w::text_block(ui, "What this app may use. It asks the first time it needs each one; revoking stops it until you allow it again.", Ts::muted(14.0));
    ui.add_space(16.0);
    let mut rows: Vec<(&str, &str, String, String, &str)> = vec![("canvas", "Canvas", host.clone(), host.clone(), "Your Canvas login from Firefox")];
    if !panopto.is_empty() {
        rows.push(("panopto", "Panopto", panopto.clone(), panopto.clone(), "Your Panopto login from Firefox, for lecture recordings"));
    }
    rows.push(("google", "Google", "NotebookLM".into(), "google".into(), "Your Google login from Firefox, for NotebookLM export"));
    rows.push(("anki", "Anki", "on this computer".into(), "anki".into(), "Reading your decks and recording your reviews"));
    rows.push(("claude", "Claude", "Anthropic".into(), "claude".into(), "Sending passages of PDFs you add checkpoints to, so Claude can write questions"));
    let mut revoke: Option<String> = None;
    panel_rows(ui, |ui| {
        for (i, (site, name, where_, perm, what)) in rows.iter().enumerate() {
            if perm.is_empty() {
                continue;
            }
            if i > 0 {
                let y = ui.cursor().min.y;
                ui.painter().hline(ui.cursor().min.x..=ui.cursor().min.x + ui.available_width(), y + 0.5, Stroke::new(1.0, tk.line));
            }
            let allowed = app.allowed(perm);
            perm_row(ui, &[(name, true), (" ", false), (where_, false)], what, |ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                if allowed {
                    if w::button(ui, "Revoke").clicked() {
                        revoke = Some(site.to_string());
                    }
                    w::pill(ui, Pill::Ok, "Allowed");
                } else {
                    w::pill(ui, Pill::Plain, "Not allowed");
                }
            });
        }
    });
    if let Some(site) = revoke {
        let (svc, s) = (app.svc.clone(), site.clone());
        app.spawn(async move { svc.setup_revoke(&s).await }, move |app, r| {
            if r.is_ok() {
                let perm = perm_for(app, &site);
                if let Some(a) = app.status["permissions"].as_array_mut() {
                    a.retain(|p| *p != perm.as_str());
                }
                app.setup.signins.insert(site.clone(), Signin::default());
                if site == "anki" {
                    app.anki.due = None;
                }
            }
        });
    }
}

/// .set-panel.perm-panel: max 640px, padding 4px 16px.
pub fn panel_rows(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let w = ui.available_width().min(640.0);
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(ui.cursor().min, vec2(w, f32::INFINITY))).layout(egui::Layout::top_down(egui::Align::Min)));
    w::boxed(&mut c, egui::Margin { left: 16, right: 16, top: 4, bottom: 4 }, 8.0, add);
    let h = c.min_rect().height();
    ui.allocate_space(vec2(w, h));
}

/// .perm-row: a title (bold parts) with a hint under it on the left; controls on the right.
pub fn perm_row(ui: &mut Ui, title: &[(&str, bool)], hint: &str, right: impl FnOnce(&mut Ui)) {
    let tk = t();
    let w = ui.available_width();
    let top = ui.cursor().min;
    // lay out the right side first to know its width
    let mut rc = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(top + vec2(0.0, 10.0), vec2(w, 40.0))).layout(egui::Layout::right_to_left(egui::Align::Center)).sizing_pass());
    rc.set_invisible();
    let dummy = |ui: &mut Ui| {
        let _ = ui;
    };
    dummy(&mut rc);
    let right_w = w * 0.45;
    let mut rich = Rich::new();
    for (t_, bold) in title {
        rich.push(t_, if *bold { Ts::new(14.0, 700, tk.text) } else { Ts::new(13.0, 400, tk.muted) });
    }
    let tg = rich.wrap(w - right_w - 16.0).lay(ui);
    let hg = lay(ui, hint, Ts::faint(12.0), Some(w - right_w - 16.0), false);
    let left_h = tg.size().y.max(21.0) + if hint.is_empty() { 0.0 } else { hg.rows.len() as f32 * 18.0 };
    let h = left_h.max(33.0) + 20.0;
    let (r, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
    let ly = r.center().y - left_h / 2.0;
    ui.painter().galley(pos2(r.min.x, ly + 1.0), tg.clone(), tk.text);
    if !hint.is_empty() {
        ui.painter().galley(pos2(r.min.x, ly + tg.size().y.max(21.0)), hg, tk.faint);
    }
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(pos2(r.max.x - right_w, r.min.y), r.max)).layout(egui::Layout::right_to_left(egui::Align::Center)));
    right(&mut c);
}

/// The card's contents without its own box (inside .connect-card).
pub fn signin_card_bare(app: &mut App, ui: &mut Ui, site: &str) {
    signin_card(app, ui, site, true);
}
