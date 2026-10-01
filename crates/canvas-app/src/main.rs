//! The Canvas desktop app: a native window drawn with wgpu, over the same local cache as the MCP
//! server. Closing the window quits.
//!
//! Usage: canvas-app [--demo] [--dev]
//!   --demo     use built-in sample data instead of your Canvas account
//!   --dev      log more (RUST_LOG=debug)
//!
//! For checking the UI: --screenshot out.png [--route '#/c/101/modules'] [--wait 2.5] [--size 1280x860]
//! draws the app, saves a picture of the window after `wait` seconds, and exits.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod anim;
mod anki;
mod app;
mod checkpoints;
mod data;
mod debug;
mod fmt;
mod html;
mod images;
mod math;
mod search;
mod textbooks;
mod nav;
mod notebooks;
mod palette;
mod panes;
mod paper;
mod pdf;
mod prefs;
mod route;
mod settings;
mod setup;
mod shell;
mod sidebar;
mod smiles;
mod theme;
mod timing;
mod views;
mod vines;
mod widgets;
#[cfg(windows)]
mod win;

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

pub struct Screenshot {
    pub path: String,
    pub wait: f32,
    pub requested: bool,
    pub start: Instant,
    pub actions: Vec<String>,
    /// scripted key presses, delivered at the start of the next frame
    pub keys: Vec<egui::Event>,
}

/// The logo: an accent rounded square, `n` pixels a side (RGBA).
fn logo(n: u32) -> Vec<u8> {
    let k = n as f32 / 64.0;
    let (r, m) = (14.0 * k, 2.0 * k);
    let mut rgba = Vec::with_capacity((n * n * 4) as usize);
    for y in 0..n {
        for x in 0..n {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let (cx, cy) = (fx.clamp(r + m, n as f32 - r - m), fy.clamp(r + m, n as f32 - r - m));
            let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt() - r;
            let a = (0.5 - d).clamp(0.0, 1.0);
            rgba.extend([0xb5, 0x46, 0x2f, (a * 255.0) as u8]);
        }
    }
    rgba
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

// --- the system's light/dark setting ----------------------------------------------------------------
static SYSTEM_DARK: AtomicU8 = AtomicU8::new(2); // 0 light, 1 dark, 2 unknown

#[cfg(all(unix, not(target_os = "macos")))]
fn read_system_dark() -> Option<bool> {
    // GNOME and most portals' setting; KDE sets the GTK theme name.
    let out = std::process::Command::new("gsettings").args(["get", "org.gnome.desktop.interface", "color-scheme"]).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    if s.contains("prefer-dark") {
        return Some(true);
    }
    if s.contains("default") || s.contains("prefer-light") {
        let out = std::process::Command::new("gsettings").args(["get", "org.gnome.desktop.interface", "gtk-theme"]).output().ok()?;
        return Some(String::from_utf8_lossy(&out.stdout).to_lowercase().contains("-dark"));
    }
    None
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
fn read_system_dark() -> Option<bool> {
    None
}

fn watch_system_theme() {
    std::thread::spawn(|| {
        loop {
            if let Some(d) = read_system_dark() {
                let v = d as u8;
                if SYSTEM_DARK.swap(v, Ordering::Relaxed) != v {
                    app::wake();
                }
            }
            std::thread::sleep(Duration::from_secs(3));
        }
    });
}

/// Does the system prefer dark? (winit's answer when it has one, else the desktop's setting.)
pub fn system_dark(ctx: &egui::Context) -> bool {
    if let Ok(t) = std::env::var("CANVAS_SYSTEM_THEME") {
        return t == "dark";
    }
    match SYSTEM_DARK.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => ctx.system_theme().map(|t| t == egui::Theme::Dark).unwrap_or(false),
    }
}

/// With --screenshot: after the wait, capture the window and exit.
pub fn screenshot_tick(app: &mut app::App, ctx: &egui::Context) {
    let Some(s) = app.screenshot.as_mut() else { return };
    let shots: Vec<std::sync::Arc<egui::ColorImage>> = ctx.input(|i| {
        i.events.iter().filter_map(|e| if let egui::Event::Screenshot { image, .. } = e { Some(image.clone()) } else { None }).collect()
    });
    if let Some(img) = shots.first() {
        let [w, h] = img.size;
        let mut buf = image::RgbaImage::new(w as u32, h as u32);
        for (i, p) in img.pixels.iter().enumerate() {
            buf.put_pixel((i % w) as u32, (i / w) as u32, image::Rgba(p.to_srgba_unmultiplied()));
        }
        let _ = buf.save(&s.path);
        eprintln!("saved {}", s.path);
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        return;
    }
    // Scripted steps: "key:j", "click:x,y", "wait" (each after 0.6s).
    let elapsed = s.start.elapsed().as_secs_f32();
    if !s.actions.is_empty() && elapsed > 1.0 {
        let act = s.actions.remove(0);
        s.start = Instant::now() - Duration::from_millis(400);
        s.wait += 0.0;
        let _ = act;
        ctx.request_repaint();
        let act2 = act.clone();
        crate::views::script(app, ctx, &act2);
        return;
    }
    let s = app.screenshot.as_mut().unwrap();
    if !s.requested && s.actions.is_empty() && s.start.elapsed().as_secs_f32() >= s.wait {
        s.requested = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
    }
    ctx.request_repaint_after(Duration::from_millis(50));
}

fn main() {
    timing::mark("main");
    let args: Vec<String> = std::env::args().skip(1).collect();
    // For launcher entries: the logo as a 256px PNG.
    if let Some(path) = arg(&args, "--write-icon") {
        let n = 256;
        let ok = image::RgbaImage::from_raw(n, n, logo(n)).map(|img| img.save(&path).is_ok()).unwrap_or(false);
        std::process::exit(if ok { 0 } else { 1 });
    }
    let demo = args.iter().any(|a| a == "--demo");
    let dev = args.iter().any(|a| a == "--dev");
    // Started from the app's folder (a shortcut): anything we start (Firefox for signing in)
    // inherits the working directory and would hold the folder open.
    let _ = std::env::set_current_dir(canvas_mcp::config::home());
    if demo {
        unsafe {
            std::env::set_var("CANVAS_DEMO", "1");
            if std::env::var("CANVAS_MCP_DIR").is_err() {
                std::env::set_var("CANVAS_MCP_DIR", canvas_mcp::config::home().join(".canvas-mcp").join("demo"));
            }
        }
    }
    if let Some(r) = arg(&args, "--route") {
        unsafe { std::env::set_var("CANVAS_APP_ROUTE", r) };
    }
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(if dev { "info,wgpu_core=warn,wgpu_hal=warn,naga=warn" } else { "warn,wgpu_core=error,wgpu_hal=error" })).init();

    #[cfg(windows)]
    if !dev && !demo && arg(&args, "--screenshot").is_none() {
        win::set_app_id();
        // One app window: a second launch brings the running one forward.
        if !win::take_single_instance() {
            win::focus_existing();
            return;
        }
    }

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().worker_threads(4).thread_name("canvas-worker").build().expect("tokio runtime");
    let svc = match canvas_mcp::services::Services::new() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("canvas-app: couldn't start: {e}");
            std::process::exit(1);
        }
    };
    timing::mark("services");
    // The background sync, every few minutes.
    {
        let _g = rt.enter();
        let engine = svc.engine.clone();
        rt.spawn(engine.sync_loop());
    }
    watch_system_theme();

    let screenshot = arg(&args, "--screenshot").map(|path| Screenshot {
        path,
        wait: arg(&args, "--wait").and_then(|w| w.parse().ok()).unwrap_or(2.5),
        requested: false,
        start: Instant::now(),
        keys: Vec::new(),
        actions: arg(&args, "--actions").map(|a| a.split(';').map(String::from).filter(|s| !s.is_empty()).collect()).unwrap_or_default(),
    });
    let size = arg(&args, "--size").and_then(|s| {
        let (w, h) = s.split_once('x')?;
        Some([w.parse().ok()?, h.parse().ok()?])
    });

    let icon = egui::IconData { rgba: logo(64), width: 64, height: 64 };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Canvas")
            .with_app_id("canvas-desktop")
            .with_inner_size(size.unwrap_or([1280.0, 860.0]))
            .with_min_inner_size([480.0, 360.0])
            // Linux: the app draws its own touch-sized window buttons
            .with_decorations(!shell::OWN_CONTROLS)
            .with_icon(icon),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    let handle = rt.handle().clone();
    let res = eframe::run_native("Canvas", options, Box::new(move |cc| {
        timing::mark("window");
        Ok(Box::new(app::App::new(cc, svc, handle, screenshot)))
    }));
    timing::write();
    if let Err(e) = res {
        eprintln!("canvas-app: {e}");
        std::process::exit(1);
    }
    rt.shutdown_timeout(Duration::from_millis(500));
}
