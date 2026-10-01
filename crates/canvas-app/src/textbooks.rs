//! Textbooks: PDFs on this computer, read in the viewer like a course's PDFs (checkpoints, the
//! Contents panel, fullscreen, search), opening where you left off. The library is
//! canvas_mcp::textbooks; adding is by the system's file dialog, a suggestion from your files, or
//! dropping a PDF on the window.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use egui::{Id, Rect, Sense, Stroke, Ui, pos2, vec2};
use serde_json::{Value, json};

use crate::app::{App, Pane};
use crate::data::Need;
use crate::fmt;
use crate::theme::{self, t};
use crate::views::{RowSpec, group_head, h1, head, list, row};
use crate::widgets::{self as w, Ts, cr};

#[derive(Default)]
pub struct Books {
    list: Option<Vec<Value>>,
    suggestions: Vec<Value>,
    loading: bool,
    picking: bool,
    last_drawn: u64,
    /// books opened this session (their place restored once)
    opened: HashSet<String>,
    /// per book: the position last saved, and when
    saved: HashMap<String, (f32, Instant)>,
}

fn reload(app: &mut App) {
    if app.books.loading {
        return;
    }
    app.books.loading = true;
    let svc = app.svc.clone();
    app.spawn(
        async move { tokio::task::spawn_blocking(move || (svc.textbooks(), svc.textbook_suggestions())).await.unwrap_or_default() },
        |app, (list, sugg)| {
            app.books.loading = false;
            app.books.list = Some(list);
            app.books.suggestions = sugg;
        },
    );
}

fn add(app: &mut App, path: String) {
    let svc = app.svc.clone();
    app.spawn(async move { tokio::task::spawn_blocking(move || svc.textbook_add(&path)).await.unwrap_or_else(|_| Err(canvas_mcp::services::ApiErr::new(500, "textbook", "Couldn't add it"))) }, |app, r| {
        match r {
            Ok(b) => {
                app.toast(format!("Added “{}”", fmt::s(&b["title"])), false);
                app.books.loading = false;
                reload(app);
                app.search_index = None;
            }
            Err(e) => app.toast(e.message(), true),
        }
    });
}

fn pick(app: &mut App) {
    if app.books.picking {
        return;
    }
    app.books.picking = true;
    let start = canvas_mcp::config::home().join("Downloads");
    app.spawn(
        async move {
            rfd::AsyncFileDialog::new().set_title("Add a textbook").add_filter("PDF", &["pdf", "PDF"]).set_directory(start).pick_file().await.map(|f| f.path().to_string_lossy().to_string())
        },
        |app, path| {
            app.books.picking = false;
            if let Some(p) = path {
                add(app, p);
            }
        },
    );
}

fn remove(app: &mut App, id: String) {
    let svc = app.svc.clone();
    app.spawn(async move { tokio::task::spawn_blocking(move || svc.textbook_remove(&id)).await.ok() }, |app, _| {
        app.books.loading = false;
        reload(app);
        app.search_index = None;
    });
}

/// "page 63 of 2,036" from a saved position.
fn progress(b: &Value) -> Option<String> {
    let pos = b["position"].as_f64()?;
    let pages = b["pages"].as_u64();
    Some(match pages {
        Some(n) => format!("page {} of {}", pos as u64 + 1, fmt::num(&json!(n))),
        None => format!("page {}", pos as u64 + 1),
    })
}

fn home_short(path: &str) -> String {
    let home = canvas_mcp::config::home().to_string_lossy().to_string();
    match path.strip_prefix(&home) {
        Some(rest) => format!("~{rest}"),
        None => path.to_string(),
    }
}

/// A round × (or a word) at the right end of a row, its own touch target.
fn row_button(ui: &mut Ui, row: Rect, id: Id, label: &str, tip: &str) -> bool {
    let tk = t();
    let ts = Ts::new(13.0, 600, tk.accent);
    let wdt = if label == "×" { 36.0 } else { w::lay(ui, label, ts, None, false).size().x + 26.0 };
    let r = Rect::from_center_size(pos2(row.max.x - 10.0 - wdt / 2.0, row.center().y), vec2(wdt, 36.0));
    let resp = ui.interact(r, id, Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(r, cr(18.0), if label == "×" { tk.hover } else { tk.accent_soft });
    }
    if label == "×" {
        let c = r.center();
        let col = if resp.hovered() { tk.text } else { tk.faint };
        ui.painter().line_segment([c + vec2(-5.0, -5.0), c + vec2(5.0, 5.0)], Stroke::new(1.5, col));
        ui.painter().line_segment([c + vec2(5.0, -5.0), c + vec2(-5.0, 5.0)], Stroke::new(1.5, col));
    } else {
        ui.painter().rect_stroke(r, cr(18.0), Stroke::new(1.0, theme::alpha(tk.accent, 0.5)), egui::StrokeKind::Inside);
        w::centered_text(ui, r, label, ts);
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip).clicked()
}

pub fn list_view(app: &mut App, ui: &mut Ui, pane: Pane) -> Result<(), Need> {
    let tk = t();
    if app.books.last_drawn + 1 < app.frame_no {
        reload(app);
    }
    app.books.last_drawn = app.frame_no;
    // a PDF dropped on the window
    let dropped: Vec<String> = ui.ctx().input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| p.extension().map(|e| e.eq_ignore_ascii_case("pdf")).unwrap_or(false)).map(|p| p.to_string_lossy().to_string()).collect());
    for p in dropped {
        add(app, p);
    }
    let Some(books) = app.books.list.clone() else { return Err(Need::Pending) };
    head(app, ui, pane, "Textbooks", vec![], None);
    h1(ui, pane, "Textbooks");
    w::sub(ui, "PDFs on this computer, read with checkpoints, chapters and search. The files stay where they are. Add one here, or drop it on the window.");
    crate::views::actions(ui, |ui| {
        let picking = app.books.picking;
        if w::button_ex(ui, if picking { "Choosing…" } else { "Add a PDF…" }, w::ButtonOpts { kind: w::Btn::Primary, size: 14.0, pad: vec2(16.0, 8.0), disabled: picking, ..Default::default() }).clicked() {
            pick(app);
        }
    });
    if books.is_empty() {
        w::empty(ui, "No textbooks yet.");
    } else {
        let mut gone: Option<String> = None;
        list(ui, |ui| {
            for (i, b) in books.iter().enumerate() {
                let id = fmt::s(&b["id"]);
                let exists = b["exists"].as_bool().unwrap_or(true);
                let mut meta = vec![];
                if let Some(n) = b["pages"].as_u64() {
                    meta.push(format!("{} pages", fmt::num(&json!(n))));
                }
                meta.push(fmt::fmt_size(&b["size"]));
                if let Some(p) = progress(b) {
                    meta.push(format!("read to {p}"));
                }
                if !exists {
                    meta = vec![format!("Not found at {}", home_short(&fmt::s(&b["path"])))];
                }
                let resp = row(app, ui, pane, RowSpec {
                    href: exists.then(|| format!("#/t/{id}")),
                    key: Some(format!("#/t/{id}")),
                    title: fmt::s(&b["title"]),
                    meta: Some(meta.join(" · ")),
                    meta_color: (!exists).then_some(tk.bad),
                    first: i == 0,
                    counts: vec![(" ".repeat(10), egui::Color32::TRANSPARENT, 400, false)], // room for ×
                    ..Default::default()
                });
                if row_button(ui, resp.rect, Id::new(("book-remove", &id)), "×", "Remove from Textbooks (the file stays)") {
                    gone = Some(id);
                }
            }
        });
        if let Some(id) = gone {
            remove(app, id);
        }
    }
    let sugg = app.books.suggestions.clone();
    if !sugg.is_empty() {
        group_head(ui, "From your files", |_| {});
        w::text_block(ui, "Large PDFs in Downloads, Documents and on the Desktop.", Ts::faint(13.0));
        ui.add_space(8.0);
        let mut take: Option<String> = None;
        list(ui, |ui| {
            for (i, s) in sugg.iter().enumerate() {
                let path = fmt::s(&s["path"]);
                let resp = row(app, ui, pane, RowSpec {
                    title: fmt::s(&s["title"]),
                    meta: Some(format!("{} · {}", fmt::fmt_size(&s["size"]), home_short(&path))),
                    first: i == 0,
                    counts: vec![(" ".repeat(14), egui::Color32::TRANSPARENT, 400, false)], // room for Add
                    ..Default::default()
                });
                if row_button(ui, resp.rect, Id::new(("book-add", &path)), "Add", "Add to Textbooks") {
                    take = Some(path);
                }
            }
        });
        if let Some(p) = take {
            add(app, p);
        }
    }
    Ok(())
}

pub fn book_view(app: &mut App, ui: &mut Ui, pane: Pane, id: &str) -> Result<(), Need> {
    let tk = t();
    if app.books.list.is_none() {
        reload(app);
        return Err(Need::Pending);
    }
    let Some(b) = app.books.list.as_ref().and_then(|l| l.iter().find(|b| b["id"] == id)).cloned() else {
        // added in another window, maybe: look again once
        if !app.books.loading && app.books.last_drawn + 1 < app.frame_no {
            reload(app);
        }
        app.books.last_drawn = app.frame_no;
        crate::views::out(app, pane).title = "Not found".into();
        h1(ui, pane, "This textbook isn't in your library");
        return Ok(());
    };
    let title = fmt::s(&b["title"]);
    let crumbs = vec![("Textbooks".to_string(), Some("#/textbooks".to_string()))];
    head(app, ui, pane, &title, crumbs, None);
    if !b["exists"].as_bool().unwrap_or(true) {
        h1(ui, pane, &title);
        w::text_block(ui, &format!("The file isn't at {} any more. Moved it? Add it again from its new place.", home_short(&fmt::s(&b["path"]))), Ts::new(14.0, 400, tk.bad));
        return Ok(());
    }
    let fid = format!("tb-{id}");
    // the first time this session: back to where you were
    if app.books.opened.insert(id.to_string()) {
        if let Some(pos) = b["position"].as_f64() {
            app.pdf.restore.insert(fid.clone(), (pos as usize, (pos.fract()) as f32));
        }
        let svc = app.svc.clone();
        let i = id.to_string();
        app.fire(async move {
            let _ = tokio::task::spawn_blocking(move || svc.textbook_update(&i, &json!({"opened": canvas_mcp::store::now()}))).await;
        });
    }
    crate::pdf::reader_bar(app, ui, &fid, &title, &fmt::fmt_size(&b["size"]), None);
    crate::pdf::view(app, ui, pane, &fid, &title, None);
    save_place(app, id, &fid, &b);
    Ok(())
}

/// Every few seconds while reading: where you are (and the page count, once known).
fn save_place(app: &mut App, id: &str, fid: &str, b: &Value) {
    let Some(pos) = crate::pdf::current_pos(app, fid) else { return };
    // not while a saved place is still being gone back to
    if app.pdf.restore.contains_key(fid) {
        return;
    }
    let pages = app.pdf.info(fid).map(|i| i.pages.len());
    let last = app.books.saved.get(id).copied();
    let moved = last.map(|(p, _)| (p - pos).abs() > 0.05).unwrap_or(true);
    let due = last.map(|(_, at)| at.elapsed() > Duration::from_secs(3)).unwrap_or(true);
    if !(moved && due) {
        return;
    }
    app.books.saved.insert(id.to_string(), (pos, Instant::now()));
    let mut patch = json!({"position": (pos * 1000.0).round() / 1000.0});
    if let Some(n) = pages.filter(|n| b["pages"].as_u64() != Some(*n as u64)) {
        patch["pages"] = json!(n);
    }
    // the library shown elsewhere stays current too
    if let Some(l) = app.books.list.as_mut() {
        if let Some(x) = l.iter_mut().find(|x| x["id"] == id) {
            x["position"] = patch["position"].clone();
            if let Some(n) = pages {
                x["pages"] = json!(n);
            }
        }
    }
    let svc = app.svc.clone();
    let i = id.to_string();
    app.fire(async move {
        let _ = tokio::task::spawn_blocking(move || svc.textbook_update(&i, &patch)).await;
    });
}
