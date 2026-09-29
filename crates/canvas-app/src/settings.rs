//! The Settings page, and small bits of UI state shared across pages.

use std::collections::{HashMap, HashSet};

use egui::{CursorIcon, Id, Rect, Sense, Stroke, Ui, pos2, vec2};
use serde_json::Value;

use crate::app::{App, Pane};
use crate::data::Need;
use crate::theme::t;
use crate::widgets::{self as w, Ts, cr, lay};

#[derive(Default)]
pub struct SettingsState {
    pub course_tabs: Vec<(String, bool)>,
    pub focus_filter: bool,
    pub toggle_cursor: bool,
    pub filters: HashMap<String, String>,
    pub opening: HashSet<String>,
    pub texts: HashMap<String, String>,
    pub reduced_motion: bool,
    pub autofocus_school: bool,
    pub adv_open: bool,
    pub key_editing: bool,
    pub key_note: String,
    pub key_input: String,
    pub key_saving: bool,
    pub key_info: Option<Value>,
    pub key_loading: bool,
    pub cp_prompt: Option<String>,
    pub cp_deck: Option<String>,
}

/// .set-toggle: a checkbox with a label and an optional hint under it.
pub fn toggle(ui: &mut Ui, id: Id, on: bool, label: &str, hint: Option<&str>) -> bool {
    let tk = t();
    let wdt = ui.available_width();
    let lg = lay(ui, label, Ts::body(), Some(wdt - 24.0), false);
    let hg = hint.map(|h| lay(ui, h, Ts::faint(12.0), Some(wdt - 24.0), false));
    let h = lg.size().y.max(21.0) + hg.as_ref().map(|g| g.size().y).unwrap_or(0.0) + 10.0;
    let (r, resp) = ui.allocate_exact_size(vec2(wdt, h), Sense::click());
    let cb = Rect::from_min_size(pos2(r.min.x, r.min.y + 5.0 + 3.0), vec2(15.0, 15.0));
    w::checkbox(ui, cb, on, false, false, tk.ok, resp.hovered());
    ui.painter().galley(pos2(r.min.x + 24.0, r.min.y + 5.0 + 1.0), lg.clone(), tk.text);
    if let Some(g) = hg {
        ui.painter().galley(pos2(r.min.x + 24.0, r.min.y + 5.0 + lg.size().y.max(21.0)), g, tk.faint);
    }
    let _ = id;
    resp.on_hover_cursor(CursorIcon::PointingHand).clicked()
}

/// .set-row: a label and its value, with a range slider under them (accent: ok).
pub fn slider(ui: &mut Ui, id: Id, value: &mut f64, min: f64, max: f64, step: f64, label: &str, shown: &str) -> bool {
    let tk = t();
    let wdt = ui.available_width();
    let (r, _) = ui.allocate_exact_size(vec2(wdt, 5.0 + 21.0 + 4.0 + 16.0 + 5.0), Sense::hover());
    w::painter_text(ui, pos2(r.min.x, r.min.y + 5.0 + 10.5), egui::Align2::LEFT_CENTER, label, Ts::body());
    w::painter_text(ui, pos2(r.max.x, r.min.y + 5.0 + 10.5), egui::Align2::RIGHT_CENTER, shown, Ts::muted(12.0));
    let track = Rect::from_min_max(pos2(r.min.x + 8.0, r.min.y + 30.0), pos2(r.max.x - 8.0, r.min.y + 46.0));
    let resp = ui.interact(track.expand2(vec2(8.0, 0.0)), id, Sense::click_and_drag());
    let frac = ((*value - min) / (max - min)).clamp(0.0, 1.0) as f32;
    let cy = track.center().y;
    ui.painter().rect_filled(Rect::from_min_max(pos2(track.min.x, cy - 2.0), pos2(track.max.x, cy + 2.0)), cr(2.0), tk.line);
    let x = track.min.x + frac * track.width();
    ui.painter().rect_filled(Rect::from_min_max(pos2(track.min.x, cy - 2.0), pos2(x, cy + 2.0)), cr(2.0), tk.ok);
    ui.painter().circle_filled(pos2(x, cy), if resp.hovered() || resp.dragged() { 8.0 } else { 7.0 }, tk.ok);
    ui.painter().circle_stroke(pos2(x, cy), 7.0, Stroke::new(1.0, tk.panel_solid));
    let mut changed = false;
    if let Some(p) = resp.interact_pointer_pos() {
        if resp.dragged() || resp.clicked() || resp.drag_started() {
            let f = ((p.x - track.min.x) / track.width()).clamp(0.0, 1.0) as f64;
            let mut v = min + f * (max - min);
            v = (v / step).round() * step;
            v = (v * 1e6).round() / 1e6;
            if (v - *value).abs() > 1e-9 {
                *value = v.clamp(min, max);
                changed = true;
            }
        }
    }
    resp.on_hover_cursor(CursorIcon::PointingHand);
    changed
}

pub fn view(app: &mut App, ui: &mut Ui, pane: Pane) -> Result<(), Need> {
    let tk = t();
    crate::views::out(app, pane).title = "Settings".into();
    crate::views::h1(ui, pane, "Settings");
    w::sub(ui, "Saved on this device.");
    ui.add_space(-18.0 - 28.0 + 18.0);
    w::h2(ui, "Appearance");
    crate::setup::panel_rows(ui, |ui| {
        crate::setup::perm_row(ui, &[("Theme", true)], "System follows your computer's light or dark setting. Press T anywhere to switch.", |ui| {
            let chips_w = 3.0 * 4.0 + ["System", "Light", "Dark"].iter().map(|l| lay(ui, l, Ts::new(12.0, 400, tk.muted), None, false).size().x + 24.0).sum::<f32>();
            ui.allocate_ui_with_layout(vec2(chips_w, 30.0), egui::Layout::left_to_right(egui::Align::Center), |ui| crate::setup::theme_picker_pub(app, ui));
        });
    });
    crate::setup::permissions_section(app, ui);
    crate::checkpoints::settings_section(app, ui);
    w::h2(ui, "Keyboard shortcuts");
    let mut rich = w::Rich::new();
    rich.push("Press ", Ts::muted(14.0));
    rich.push("?", Ts::new(11.0, 400, tk.muted).mono());
    rich.push(" anywhere to see these.", Ts::muted(14.0));
    let g = rich.lay(ui);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 21.0), Sense::hover());
    ui.painter().galley(pos2(r.min.x, r.center().y - g.size().y / 2.0), g, tk.muted);
    ui.add_space(16.0);
    w::boxed(ui, egui::Margin { left: 20, right: 20, top: 12, bottom: 12 }, 8.0, |ui| crate::nav::shortcuts_table(ui));
    // Debug
    w::h2(ui, "Debug");
    let mut flip = false;
    w::boxed(ui, egui::Margin { left: 16, right: 16, top: 12, bottom: 12 }, 8.0, |ui| {
        flip = toggle(ui, Id::new("debug-timings"), app.debug.on, "Show load times", Some("A panel in the corner times each page, document and part of the app from your click, tap or key until it's on screen, split into waiting, loading data, drawing and painting."));
    });
    if flip {
        let on = !app.debug.on;
        crate::debug::set_on(app, on);
    }
    crate::vines::settings(app, ui);
    Ok(())
}
