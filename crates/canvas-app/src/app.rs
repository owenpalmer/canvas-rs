//! The app: its state, the per-frame loop, and the plumbing between the UI thread and the
//! background tasks (the engine, Canvas, Anki, Claude, NotebookLM run on a Tokio runtime and send
//! their results back as messages).

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use canvas_mcp::engine::Event;
use canvas_mcp::services::Services;
use egui::{Context, Rect};
use serde_json::{Value, json};

use crate::data::{Data, Loaded};
use crate::prefs::Prefs;
use crate::route::{self, Route, View};
use crate::theme::{self, Theme};

static CTX: OnceLock<Context> = OnceLock::new();

/// Ask for a frame (from any thread).
pub fn wake() {
    if let Some(c) = CTX.get() {
        c.request_repaint();
    }
}

pub type Apply = Box<dyn FnOnce(&mut App) + Send>;

pub enum Msg {
    Data(Loaded),
    Event(Event),
    Apply(Apply),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Pane {
    Sidebar,
    Main,
    Viewer,
}

/// What a drawn view said about itself.
#[derive(Default, Clone)]
pub struct Out {
    pub title: String,
    pub crumbs: Vec<(String, Option<String>)>,
    pub crumb_pill: Option<String>,
    pub tab_label: Option<String>,
}

/// A thing j/k can move to, registered while drawing.
#[derive(Clone, Debug)]
pub struct Item {
    pub key: String,
    pub rect: Rect,
    pub href: Option<String>,
}

/// One pane's drawing state: what it wants to show, and what it last showed.
pub struct PaneDraw {
    pub want: String,
    pub since: Instant,
    pub shown: Option<String>,
    pub deps: HashSet<String>,
    pub out: Out,
    pub items: Vec<Item>,
    pub cursor: Option<String>,
    pub scroll_to: Option<f32>,
    pub scroll_to_rect: Option<Rect>,
    pub anchor: Option<String>,
    pub anchors: HashMap<String, f32>,
    pub scroll: f32,
    pub viewport_h: f32,
    pub content_top: f32,
    pub trace: Option<crate::debug::Trace>,
}

impl PaneDraw {
    pub fn new(want: &str) -> PaneDraw {
        PaneDraw {
            want: want.to_string(),
            since: Instant::now(),
            shown: None,
            deps: HashSet::new(),
            out: Out::default(),
            items: Vec::new(),
            cursor: None,
            scroll_to: None,
            scroll_to_rect: None,
            anchor: None,
            anchors: HashMap::new(),
            scroll: 0.0,
            viewport_h: 0.0,
            content_top: 0.0,
            trace: None,
        }
    }
}

pub struct Toast {
    pub text: String,
    pub bad: bool,
    pub at: Instant,
}

pub struct App {
    pub svc: Arc<Services>,
    pub rt: tokio::runtime::Handle,
    pub tx: Sender<Msg>,
    rx: Receiver<Msg>,
    pub d: Data,
    pub status: Value,
    pub prefs: Prefs,
    pub theme: Theme,
    pub nav: crate::nav::Nav,
    pub main: PaneDraw,
    pub viewer: PaneDraw,
    pub panes: crate::panes::Panes,
    pub sidebar: crate::sidebar::SidebarState,
    pub palette: crate::palette::Palette,
    pub keys_open: bool,
    pub toast: Option<Toast>,
    pub setup: crate::setup::Setup,
    pub nb: crate::notebooks::NbState,
    pub anki: crate::anki::AnkiState,
    pub cp: crate::checkpoints::Cp,
    pub pdf: crate::pdf::Pdfs,
    pub images: crate::images::Images,
    pub vines: crate::vines::Vines,
    pub debug: crate::debug::Debug,
    pub html: crate::html::HtmlCache,
    pub math: crate::math::MathCache,
    pub settings: crate::settings::SettingsState,
    pub intro: Option<Instant>,
    pub started: Instant,
    pub first_frame: bool,
    pub title: String,
    pub search_index: Option<Arc<Vec<Value>>>,
    pub openers: HashMap<String, Option<String>>,
    pub prefetched: HashMap<String, Instant>,
    pub screenshot: Option<crate::Screenshot>,
    pub frame_no: u64,
    pub modal_focus: bool,
    pub now: f64,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, svc: Arc<Services>, rt: tokio::runtime::Handle, screenshot: Option<crate::Screenshot>) -> App {
        let _ = CTX.set(cc.egui_ctx.clone());
        cc.egui_ctx.set_fonts(theme::fonts());
        if let Some(rs) = cc.wgpu_render_state.as_ref() {
            crate::paper::install(rs);
        }
        let (tx, rx) = std::sync::mpsc::channel();
        // Engine events come back as messages.
        let mut events = svc.engine.subscribe();
        let etx = tx.clone();
        rt.spawn(async move {
            loop {
                match events.recv().await {
                    Ok(e) => {
                        let _ = etx.send(Msg::Event(e));
                        wake();
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        });
        // What the first screen needs, straight from the cache, so the first frame is complete.
        let mut boot = HashMap::new();
        for k in ["self", "courses", "past_courses", "colors", "planner", "announcements", "inbox"] {
            if let Some((text, _)) = svc.engine.store.get(k) {
                if let Ok(v) = serde_json::from_str(&text) {
                    boot.insert(k.to_string(), v);
                }
            }
        }
        let prefs = Prefs::load();
        let system_dark = crate::system_dark(&cc.egui_ctx);
        let theme = Theme::new(&prefs.str("theme"), system_dark);
        let status = svc.engine.status();
        let d = Data::new(svc.clone(), rt.clone(), tx.clone(), boot);
        let panes = crate::panes::Panes::load(&prefs);
        let start = crate::nav::start_hash(&status, &d);
        let mut app = App {
            svc,
            rt,
            tx,
            rx,
            d,
            status,
            theme,
            nav: crate::nav::Nav::new(&start),
            main: PaneDraw::new(&start),
            viewer: PaneDraw::new(""),
            panes,
            sidebar: crate::sidebar::SidebarState::load(&prefs),
            palette: crate::palette::Palette::default(),
            keys_open: false,
            toast: None,
            setup: crate::setup::Setup::default(),
            nb: crate::notebooks::NbState::default(),
            anki: crate::anki::AnkiState::default(),
            cp: crate::checkpoints::Cp::load(&prefs),
            pdf: crate::pdf::Pdfs::new(),
            images: crate::images::Images::default(),
            vines: crate::vines::Vines::load(&prefs),
            debug: crate::debug::Debug::load(&prefs),
            html: crate::html::HtmlCache::default(),
            math: crate::math::MathCache::default(),
            settings: crate::settings::SettingsState::default(),
            intro: None,
            started: Instant::now(),
            first_frame: true,
            title: String::new(),
            search_index: None,
            openers: HashMap::new(),
            prefetched: HashMap::new(),
            screenshot,
            frame_no: 0,
            modal_focus: false,
            now: 0.0,
            prefs,
        };
        crate::nav::land(&mut app, &start);
        crate::notebooks::init(&mut app);
        crate::anki::init(&mut app);
        app
    }

    // --- background work -------------------------------------------------------------------------
    /// Run a future on the runtime and hand its result to `then` on the UI thread.
    pub fn spawn<T: Send + 'static>(&self, fut: impl std::future::Future<Output = T> + Send + 'static, then: impl FnOnce(&mut App, T) + Send + 'static) {
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let v = fut.await;
            let _ = tx.send(Msg::Apply(Box::new(move |app: &mut App| then(app, v))));
            wake();
        });
    }

    /// Run a future for its side effects only.
    pub fn fire(&self, fut: impl std::future::Future<Output = ()> + Send + 'static) {
        self.rt.spawn(fut);
    }

    fn process(&mut self) {
        let mut n = 0;
        while let Ok(msg) = self.rx.try_recv() {
            n += 1;
            match msg {
                Msg::Data(l) => {
                    self.d.loaded(l);
                }
                Msg::Event(Event::Status(s)) => {
                    self.status = s;
                    crate::setup::status_changed(self);
                }
                Msg::Event(Event::Changed(key)) => {
                    self.search_index = None;
                    self.d.changed(&key);
                }
                Msg::Event(Event::Job(job)) => crate::notebooks::on_job(self, job),
                Msg::Apply(f) => f(self),
            }
            if n > 500 {
                wake();
                break;
            }
        }
    }

    // --- small helpers the views share ---------------------------------------------------------------
    pub fn toast(&mut self, text: impl Into<String>, bad: bool) {
        self.toast = Some(Toast { text: text.into(), bad, at: Instant::now() });
    }

    pub fn allowed(&self, perm: &str) -> bool {
        self.status["demo"].as_bool().unwrap_or(false) || self.status["permissions"].as_array().map(|a| a.iter().any(|p| p == perm)).unwrap_or(false)
    }

    pub fn grant(&mut self, perm: &str) {
        if let Some(a) = self.status["permissions"].as_array_mut() {
            if !a.iter().any(|p| p == perm) {
                a.push(json!(perm));
            }
        } else {
            self.status["permissions"] = json!([perm]);
        }
    }

    pub fn ctx(&self) -> Option<Context> {
        CTX.get().cloned()
    }

    pub fn demo(&self) -> bool {
        self.status["demo"].as_bool().unwrap_or(false)
    }

    pub fn base(&self) -> String {
        let b = self.status["base"].as_str().unwrap_or("").to_string();
        if b.is_empty() { "https://canvas.instructure.com".into() } else { b }
    }

    pub fn open_external(&self, url: &str) {
        self.svc.open_external(url);
    }

    pub fn set_pref(&mut self, k: &str, v: Value) {
        self.prefs.set(k, v);
    }

    /// The window's frame: draw everything.
    fn frame(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = &ui.ctx().clone();
        self.frame_no += 1;
        self.now = ctx.input(|i| i.time);
        self.process();
        crate::checkpoints::tick(self);
        crate::anki::tick(self);
        // The theme: the system's changes, then this frame's tokens (mid-fade, maybe).
        let sys_dark = crate::system_dark(ctx);
        self.theme.set_system_dark(sys_dark);
        let tokens = self.theme.current();
        theme::set(tokens);
        crate::shell::style(ctx, &tokens);
        if self.theme.fading() {
            ctx.request_repaint();
        }
        crate::debug::note_input(self, ctx);
        if let Some(keys) = self.screenshot.as_mut().map(|s| std::mem::take(&mut s.keys)) {
            ctx.input_mut(|i| i.events.extend(keys));
        }
        crate::nav::keys(self, ctx);
        crate::shell::draw(self, ui, frame);
        self.prefs.flush(false);
        if self.prefs.waiting() {
            ctx.request_repaint_after(Duration::from_millis(320));
        }
        // "Synced 3m ago" stays current.
        ctx.request_repaint_after(Duration::from_secs(30));
        if self.first_frame {
            self.first_frame = false;
            self.intro = Some(Instant::now());
            #[cfg(windows)]
            crate::win::mark_window();
            crate::timing::mark("first frame");
            crate::timing::write();
        }
        crate::screenshot_tick(self, ctx);
    }

    pub fn route(&self) -> Route {
        route::parse(&self.main.want)
    }

    pub fn path_is(&self, view: &View) -> bool {
        &self.route().view == view
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.frame(ui, frame);
    }

    fn on_exit(&mut self) {
        crate::panes::save_tabs(self);
        self.prefs.flush(true);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        let c = theme::t().bg;
        [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0, 1.0]
    }
}
