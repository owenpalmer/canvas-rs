//! Documents: an assignment, a discussion or announcement, a page, a file, and the inbox.

use egui::{Rect, Sense, Stroke, Ui, pos2, vec2};
use serde_json::{Value, json};

use super::{RowSpec, actions, course_info, dashboard_crumb, detail_meta, ext_link, h1, head, list, row, sub_pill};
use crate::app::{App, Pane};
use crate::data::{Lazy, Need};
use crate::fmt;
use crate::html;
use crate::theme::t;
use crate::widgets::{self as w, Btn, ButtonOpts, Ts};

fn detail_crumbs(c: &Value, cid: &str, tab: &str, label: &str) -> Vec<(String, Option<String>)> {
    let code = c["course_code"].as_str().filter(|x| !x.is_empty()).or(c["name"].as_str()).unwrap_or("").to_string();
    vec![dashboard_crumb(), (code, Some(format!("#/c/{cid}/modules"))), (label.to_string(), Some(format!("#/c/{cid}/{tab}")))]
}

/// A comment: who, when, and what they wrote (.comment).
fn comment(app: &mut App, ui: &mut Ui, first: bool, who: &str, when: &str, body: impl FnOnce(&mut App, &mut Ui)) {
    let tk = t();
    if !first {
        let y = ui.cursor().min.y;
        let w = ui.available_width();
        ui.painter().hline(ui.cursor().min.x..=ui.cursor().min.x + w, y + 0.5, Stroke::new(1.0, tk.line));
    }
    ui.add_space(12.0);
    let mut rich = w::Rich::new();
    rich.push(who, Ts::new(13.0, 600, tk.text));
    rich.push("  ", Ts::new(8.0, 400, tk.faint));
    rich.push(when, Ts::new(12.0, 400, tk.faint));
    let g = rich.lay(ui);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 21.0), Sense::hover());
    ui.painter().galley(pos2(r.min.x, r.center().y - g.size().y / 2.0), g, tk.text);
    body(app, ui);
    ui.add_space(12.0);
}

/// Text with its line breaks kept (white-space: pre-wrap).
fn pre_wrap(ui: &mut Ui, text: &str) {
    w::text_block(ui, text, Ts::body());
}

pub fn assignment(app: &mut App, ui: &mut Ui, pane: Pane, cid: &str, aid: &str) -> Result<(), Need> {
    let info = course_info(app, cid)?;
    let groups = app.d.need1("groups", cid)?;
    let a = groups.as_array().into_iter().flatten().flat_map(|g| g["assignments"].as_array().cloned().unwrap_or_default()).find(|x| fmt::id(&x["id"]) == aid);
    let crumbs = detail_crumbs(&info.c, cid, "assignments", "Assignments");
    let Some(a) = a else {
        head(app, ui, pane, "", crumbs, None);
        w::empty(ui, "Assignment not found (it may be hidden or unpublished).");
        return Ok(());
    };
    let sub = match app.d.lazy("submission", &[cid.to_string(), aid.to_string()]) {
        Lazy::Ready(s) => (*s).clone(),
        _ => a["submission"].clone(),
    };
    let sub = if sub.is_object() { sub } else { json!({}) };
    let name = fmt::s(&a["name"]);
    head(app, ui, pane, &name, crumbs, None);
    h1(ui, pane, &name);
    let types = a["submission_types"].as_array().map(|t| t.iter().map(|x| fmt::s(x).replace('_', " ")).collect::<Vec<_>>().join(", ")).unwrap_or_default();
    let mut parts = vec![vec![("Due ".to_string(), false), (if super::truthy(&a["due_at"]) { fmt::fmt_date(&a["due_at"]) } else { "No due date".into() }, true)]];
    if !a["points_possible"].is_null() {
        parts.push(vec![(fmt::num(&a["points_possible"]), true), (" points".into(), false)]);
    }
    if !types.is_empty() {
        parts.push(vec![("Submit: ".into(), false), (types, true)]);
    }
    if super::truthy(&a["lock_at"]) {
        parts.push(vec![("Closes ".into(), false), (fmt::fmt_date(&a["lock_at"]), true)]);
    }
    detail_meta(ui, parts, sub_pill(&sub, &a).into_iter().collect());
    ui.add_space(-22.0);
    let html_url = fmt::s(&a["html_url"]);
    actions(ui, |ui| {
        let label = if super::truthy(&sub["submitted_at"]) { "View submission in Canvas ↗" } else { "Submit in Canvas ↗" };
        ext_link(app, ui, &html_url, label);
    });
    let desc = fmt::s(&a["description"]);
    if desc.is_empty() {
        w::empty(ui, "No description.");
    } else {
        html::content(app, ui, &desc, pane);
    }
    let rubric = a["rubric"].as_array().cloned().unwrap_or_default();
    let assess = sub["rubric_assessment"].clone();
    if !rubric.is_empty() {
        w::h2(ui, "Rubric");
        list(ui, |ui| {
            for (i, cr) in rubric.iter().enumerate() {
                let got = &assess[fmt::s(&cr["id"])]["points"];
                let score = format!("{}{}", if got.is_null() { String::new() } else { format!("{} / ", fmt::num(got)) }, fmt::num(&cr["points"]));
                row(app, ui, pane, RowSpec {
                    title: fmt::s(&cr["description"]),
                    meta: cr["long_description"].as_str().filter(|x| !x.is_empty()).map(String::from),
                    right: vec![(vec![], score)],
                    right_score: true,
                    first: i == 0,
                    ..Default::default()
                });
            }
        });
    }
    if super::truthy(&sub["submitted_at"]) || !sub["score"].is_null() {
        w::h2(ui, "Your submission");
        let mut parts = Vec::new();
        if super::truthy(&sub["submitted_at"]) {
            parts.push(vec![("Submitted ".into(), false), (fmt::fmt_date(&sub["submitted_at"]), true)]);
        }
        if super::truthy(&sub["attempt"]) {
            parts.push(vec![("Attempt ".into(), false), (fmt::s(&sub["attempt"]), true)]);
        }
        if !sub["score"].is_null() {
            parts.push(vec![("Score ".into(), false), (format!("{} / {}", fmt::num(&sub["score"]), fmt::num(&a["points_possible"])), true)]);
        }
        if let Some(g) = sub["grade"].as_str().filter(|g| !g.is_empty() && *g != fmt::s(&sub["score"])) {
            parts.push(vec![("Grade ".into(), false), (g.to_string(), true)]);
        }
        ui.add_space(-10.0);
        detail_meta(ui, parts, vec![]);
    }
    let comments = sub["submission_comments"].as_array().cloned().unwrap_or_default();
    if !comments.is_empty() {
        w::h2(ui, "Comments");
        w::boxed(ui, egui::Margin { left: 26, right: 26, top: 22, bottom: 22 }, 8.0, |ui| {
            for (i, cm) in comments.iter().enumerate() {
                comment(app, ui, i == 0, &fmt::s(&cm["author_name"]), &fmt::fmt_date(&cm["created_at"]), |_, ui| pre_wrap(ui, &fmt::s(&cm["comment"])));
            }
        });
    }
    Ok(())
}

pub fn topic(app: &mut App, ui: &mut Ui, pane: Pane, cid: &str, tid: &str) -> Result<(), Need> {
    let info = course_info(app, cid)?;
    let anns = app.d.need1("course_announcements", cid).ok();
    let topics = app.d.need1("discussions", cid).ok();
    let in_list = |l: &Option<crate::data::Json>| l.as_ref().and_then(|v| v.as_array().and_then(|a| a.iter().find(|x| fmt::id(&x["id"]) == tid).cloned()));
    let from_list = in_list(&anns).or_else(|| in_list(&topics));
    let full = app.d.lazy("topic", &[cid.to_string(), tid.to_string()]);
    let t_ = full.ready().map(|f| f["topic"].clone()).filter(|x| x.is_object()).or(from_list);
    let is_ann = in_list(&anns).is_some() || t_.as_ref().map(|x| x["is_announcement"].as_bool().unwrap_or(false)).unwrap_or(false);
    let crumbs = if is_ann { detail_crumbs(&info.c, cid, "announcements", "Announcements") } else { detail_crumbs(&info.c, cid, "discussions", "Discussions") };
    let Some(tp) = t_ else {
        head(app, ui, pane, "", crumbs, None);
        w::skeleton(ui);
        return Ok(());
    };
    let title = fmt::s(&tp["title"]);
    head(app, ui, pane, &title, crumbs, None);
    h1(ui, pane, &title);
    let who = tp["user_name"].as_str().filter(|x| !x.is_empty()).or(tp["author"]["display_name"].as_str()).unwrap_or("").to_string();
    let mut parts = vec![vec![(who, false)], vec![(fmt::fmt_date_str(&fmt::posted_at(&tp)), false)]];
    if super::truthy(&tp["assignment"]["due_at"]) {
        parts.push(vec![("Due ".into(), false), (fmt::fmt_date(&tp["assignment"]["due_at"]), true)]);
    }
    detail_meta(ui, parts, vec![]);
    ui.add_space(-22.0);
    let url = fmt::s(&tp["html_url"]);
    actions(ui, |ui| ext_link(app, ui, &url, if is_ann { "Open in Canvas ↗" } else { "Reply in Canvas ↗" }));
    html::content(app, ui, &fmt::s(&tp["message"]), pane);
    let entries = full.ready().map(|f| f["view"]["view"].as_array().cloned().unwrap_or_default()).unwrap_or_default();
    if !is_ann || !entries.is_empty() {
        w::h2(ui, "Replies");
        match &full {
            Lazy::Ready(f) => {
                let names: std::collections::HashMap<String, String> = f["view"]["participants"].as_array().into_iter().flatten().map(|p| (fmt::id(&p["id"]), fmt::s(&p["display_name"]))).collect();
                if entries.is_empty() {
                    w::empty(ui, "No replies yet.");
                }
                fn entries_ui(app: &mut App, ui: &mut Ui, pane: Pane, list: &[Value], names: &std::collections::HashMap<String, String>) {
                    let tk = t();
                    let mut first = true;
                    for e in list {
                        if e["deleted"].as_bool().unwrap_or(false) {
                            continue;
                        }
                        let who = names.get(&fmt::id(&e["user_id"])).cloned().unwrap_or_else(|| "Someone".into());
                        comment(app, ui, first, &who, &fmt::fmt_date(&e["created_at"]), |app, ui| {
                            html::content(app, ui, &fmt::s(&e["message"]), pane);
                            let replies = e["replies"].as_array().cloned().unwrap_or_default();
                            if !replies.is_empty() {
                                // .replies: margin-left 22, border-left 2px, padding-left 14
                                let top = ui.cursor().min;
                                let w = ui.available_width();
                                let mut c = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(top + vec2(22.0 + 2.0 + 14.0, 0.0), vec2(w - 38.0, f32::INFINITY))).layout(egui::Layout::top_down(egui::Align::Min)));
                                entries_ui(app, &mut c, pane, &replies, names);
                                let h = c.min_rect().height();
                                ui.painter().rect_filled(Rect::from_min_size(top + vec2(22.0, 0.0), vec2(2.0, h)), 0.0, tk.line);
                                ui.allocate_space(vec2(w, h));
                            }
                        });
                        first = false;
                    }
                }
                entries_ui(app, ui, pane, &entries, &names);
            }
            Lazy::Failed => w::empty(ui, "Couldn't load replies."),
            Lazy::Pending => w::empty(ui, "Loading replies…"),
        }
    }
    Ok(())
}

pub fn page(app: &mut App, ui: &mut Ui, pane: Pane, cid: &str, slug: &str) -> Result<(), Need> {
    let info = course_info(app, cid)?;
    let p = app.d.need("page", &[cid.to_string(), slug.to_string()])?;
    let title = fmt::s(&p["title"]);
    head(app, ui, pane, &title, detail_crumbs(&info.c, cid, "pages", "Pages"), None);
    h1(ui, pane, &title);
    detail_meta(ui, vec![vec![(format!("Updated {}", fmt::fmt_date(&p["updated_at"])), false)]], vec![]);
    html::content(app, ui, &fmt::s(&p["body"]), pane);
    Ok(())
}

/// "Open in Document Viewer" (the app the system opens this type with), else "Open".
fn open_label(app: &mut App, ctype: &str) -> String {
    if let Some(v) = app.openers.get(ctype) {
        return v.as_ref().map(|n| format!("Open in {n}")).unwrap_or_else(|| "Open".into());
    }
    app.openers.insert(ctype.to_string(), None);
    let (svc, ct) = (app.svc.clone(), ctype.to_string());
    app.spawn(async move { svc.opener(&ct).await }, {
        let ct = ctype.to_string();
        move |app, name| {
            app.openers.insert(ct, name);
        }
    });
    "Open".into()
}

fn open_button(app: &mut App, ui: &mut Ui, fid: &str, label: &str, small: bool) {
    let busy = app.settings.opening.contains(fid);
    let text = if busy { "Opening…" } else { label };
    let o = ButtonOpts { kind: Btn::Primary, size: if small { 12.0 } else { 13.0 }, pad: if small { vec2(9.0, 3.0) } else { vec2(12.0, 6.0) }, ..Default::default() };
    if w::button_ex(ui, text, o).clicked() && !busy {
        app.settings.opening.insert(fid.to_string());
        let (svc, f) = (app.svc.clone(), fid.to_string());
        app.spawn(async move { svc.file_open(&f).await }, {
            let f = fid.to_string();
            move |app, r| {
                app.settings.opening.remove(&f);
                if let Err(e) = r {
                    app.toast(e.message(), true);
                }
            }
        });
    }
}

pub fn file(app: &mut App, ui: &mut Ui, pane: Pane, cid: Option<&str>, fid: &str) -> Result<(), Need> {
    let mut meta: Option<Value> = None;
    if let Some(c) = cid {
        if let Ok(listed) = app.d.need1("files", c) {
            meta = listed["files"].as_array().and_then(|a| a.iter().find(|f| fmt::id(&f["id"]) == fid).cloned());
        } else if let Err(Need::Pending) = app.d.need1("files", c) {
            return Err(Need::Pending);
        }
    }
    let meta = match meta {
        Some(m) => m,
        None => (*app.d.need1("file", fid)?).clone(),
    };
    let crumbs = match cid {
        Some(c) => detail_crumbs(&course_info(app, c)?.c, c, "files", "Files"),
        None => vec![dashboard_crumb()],
    };
    let ct = fmt::s(&meta["content-type"]);
    let name = fmt::s(&meta["display_name"]);
    let label = open_label(app, &ct);
    let canvas_url = format!("{}/files/{fid}", app.base());
    let tk = t();
    // A PDF gets as much of the pane as possible: one line for its name and buttons, then its pages.
    if ct == "application/pdf" {
        // the crumbs are hidden in the PDF view (.pdf-doc .crumbs)
        let o = super::out(app, pane);
        o.title = name.clone();
        o.crumbs = crumbs;
        let size = fmt::fmt_size(&meta["size"]);
        crate::pdf::reader_bar(app, ui, fid, &name, &size, Some(&canvas_url));
        crate::pdf::view(app, ui, pane, fid, &name, cid);
        return Ok(());
    }
    head(app, ui, pane, &name, crumbs, None);
    h1(ui, pane, &name);
    detail_meta(ui, vec![vec![(fmt::fmt_size(&meta["size"]), false)], vec![(ct.clone(), false)], vec![(format!("Updated {}", fmt::fmt_date(&meta["updated_at"])), false)]], vec![]);
    ui.add_space(-22.0);
    actions(ui, |ui| {
        open_button(app, ui, fid, &label, false);
        if w::button(ui, "Open in Canvas ↗").clicked() {
            app.open_external(&canvas_url);
        }
    });
    let screen_h = ui.ctx().content_rect().height();
    let max_h = (screen_h - if pane == Pane::Viewer { 250.0 } else { 220.0 }).max(240.0);
    if ct.starts_with("image/") && !ct.contains("svg") {
        let src = crate::images::Src::File(fid.to_string());
        let wdt = ui.available_width();
        match app.image(ui.ctx(), &src) {
            Some((tex, size)) => {
                // width 100%, height auto, capped; contained in a bordered panel
                let mut sz = vec2(wdt, wdt * size.y / size.x.max(1.0));
                if sz.y > max_h {
                    sz = vec2(wdt, max_h);
                }
                let (r, _) = ui.allocate_exact_size(sz, Sense::hover());
                w::panel(ui, r, 8.0);
                let fit = (r.width() / size.x).min(r.height() / size.y);
                let img = Rect::from_center_size(r.center(), size * fit);
                ui.painter().image(tex, img, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), egui::Color32::WHITE);
            }
            None => {
                let (r, _) = ui.allocate_exact_size(vec2(wdt, 120.0), Sense::hover());
                w::panel(ui, r, 8.0);
                if app.image_failed(&src) {
                    w::centered_text(ui, r, "Couldn't show this image.", Ts::faint(14.0));
                } else {
                    w::paint_spinner(ui, r.center(), 14.0, 2.0, tk.line, tk.accent);
                }
            }
        }
    } else if ct.starts_with("text/") || ["json", "xml", "javascript", "csv"].iter().any(|k| ct.contains(k)) {
        // shown as plain text (never run)
        let text = app.settings.texts.get(fid).cloned();
        let wdt = ui.available_width();
        let (r, _) = ui.allocate_exact_size(vec2(wdt, max_h), Sense::hover());
        w::panel(ui, r, 8.0);
        match text {
            Some(t) => {
                let mut c = ui.new_child(egui::UiBuilder::new().max_rect(r.shrink(10.0)));
                egui::ScrollArea::both().id_salt(("text", fid)).show(&mut c, |ui| {
                    let g = w::lay(ui, &t, Ts::new(13.0, 400, tk.text).mono(), None, false);
                    let (rr, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                    ui.painter().galley(rr.min, g, tk.text);
                });
            }
            None => {
                app.settings.texts.insert(fid.to_string(), String::new());
                let (svc, f) = (app.svc.clone(), fid.to_string());
                app.spawn(
                    async move {
                        match svc.file(&f).await {
                            Ok((path, _)) => tokio::fs::read(path).await.map(|b| String::from_utf8_lossy(&b[..b.len().min(2_000_000)]).into_owned()).unwrap_or_default(),
                            Err(e) => e.message(),
                        }
                    },
                    {
                        let f = fid.to_string();
                        move |app, text| {
                            app.settings.texts.insert(f, text);
                        }
                    },
                );
            }
        }
    } else if ct.starts_with("video/") || ct.starts_with("audio/") {
        let wdt = ui.available_width();
        let (r, _) = ui.allocate_exact_size(vec2(wdt, if ct.starts_with("video/") { max_h.min(wdt * 9.0 / 16.0) } else { 64.0 }), Sense::hover());
        w::panel(ui, r, 8.0);
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_center_size(r.center(), vec2(260.0, 36.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
        let play = format!("▶  Play in {}", label.trim_start_matches("Open in ").trim_start_matches("Open"));
        let play = if play.trim_end().ends_with("in") { "▶  Play".to_string() } else { play };
        if w::button(&mut c, &play).clicked() {
            open_button_action(app, fid);
        }
    } else {
        w::empty(ui, &format!("No preview for this file type. Use “{label}”."));
    }
    Ok(())
}

fn open_button_action(app: &mut App, fid: &str) {
    let (svc, f) = (app.svc.clone(), fid.to_string());
    app.spawn(async move { svc.file_open(&f).await }, |app, r| {
        if let Err(e) = r {
            app.toast(e.message(), true);
        }
    });
}

pub fn inbox(app: &mut App, ui: &mut Ui, pane: Pane) -> Result<(), Need> {
    let convs = app.d.need0("inbox")?;
    let convs = convs.as_array().cloned().unwrap_or_default();
    head(app, ui, pane, "Inbox", vec![], None);
    h1(ui, pane, "Inbox");
    w::sub(ui, &format!("{} recent conversations", convs.len()));
    if convs.is_empty() {
        w::empty(ui, "Inbox is empty.");
        return Ok(());
    }
    list(ui, |ui| {
        for (i, m) in convs.iter().enumerate() {
            let who: Vec<String> = m["participants"].as_array().into_iter().flatten().take(4).map(|p| fmt::s(&p["name"])).collect();
            let subject = m["subject"].as_str().filter(|s| !s.is_empty()).unwrap_or("(no subject)").to_string();
            let day = fmt::iso(&m["last_message_at"]).map(|d| fmt::fmt_day(&d)).unwrap_or_default();
            row(app, ui, pane, RowSpec {
                href: Some(format!("#/inbox/{}", fmt::id(&m["id"]))),
                title: subject,
                bold: m["workflow_state"] == "unread",
                meta: Some(format!("{} · {}", who.join(", "), fmt::s(&m["last_message"]))),
                right: vec![(vec![], fmt::s(&m["context_name"])), (vec![], day)],
                first: i == 0,
                ..Default::default()
            });
        }
    });
    Ok(())
}

pub fn conversation(app: &mut App, ui: &mut Ui, pane: Pane, id: &str) -> Result<(), Need> {
    let convs = app.d.need0("inbox")?;
    let summary = convs.as_array().and_then(|a| a.iter().find(|c| fmt::id(&c["id"]) == id).cloned()).unwrap_or(json!({}));
    let full = app.d.lazy("conversation", &[id.to_string()]);
    let f = full.ready().map(|v| (**v).clone());
    let subject = summary["subject"].as_str().filter(|s| !s.is_empty()).or(f.as_ref().and_then(|f| f["subject"].as_str()).filter(|s| !s.is_empty())).unwrap_or("(no subject)").to_string();
    head(app, ui, pane, &subject, vec![dashboard_crumb(), ("Inbox".into(), Some("#/inbox".into()))], None);
    h1(ui, pane, &subject);
    let ctx_name = summary["context_name"].as_str().filter(|s| !s.is_empty()).or(f.as_ref().and_then(|f| f["context_name"].as_str())).unwrap_or("").to_string();
    detail_meta(ui, vec![vec![(ctx_name, false)]], vec![]);
    ui.add_space(-22.0);
    let url = format!("{}/conversations#filter=type=inbox", app.base());
    actions(ui, |ui| ext_link(app, ui, &url, "Reply in Canvas ↗"));
    let participants = f.as_ref().map(|f| f["participants"].clone()).filter(|p| p.is_array()).unwrap_or(summary["participants"].clone());
    let names: std::collections::HashMap<String, String> = participants.as_array().into_iter().flatten().map(|p| (fmt::id(&p["id"]), fmt::s(&p["name"]))).collect();
    let mut go: Option<String> = None;
    w::boxed(ui, egui::Margin { left: 26, right: 26, top: 22, bottom: 22 }, 8.0, |ui| match &f {
        Some(f) => {
            let mut msgs = f["messages"].as_array().cloned().unwrap_or_default();
            msgs.reverse();
            for (i, m) in msgs.iter().enumerate() {
                let who = names.get(&fmt::id(&m["author_id"])).cloned().unwrap_or_else(|| "Someone".into());
                comment(app, ui, i == 0, &who, &fmt::fmt_date(&m["created_at"]), |_, ui| {
                    pre_wrap(ui, &fmt::s(&m["body"]));
                    for at in m["attachments"].as_array().into_iter().flatten() {
                        if w::linklike(ui, &format!("📎 {}", fmt::s(&at["display_name"])), 14.0).clicked() {
                            go = Some(format!("#/f/{}", fmt::id(&at["id"])));
                        }
                    }
                });
            }
        }
        None => w::empty(ui, if matches!(full, Lazy::Failed) { "Couldn't load this conversation." } else { "Loading…" }),
    });
    if let Some(h) = go {
        crate::nav::follow(app, &h, pane);
    }
    Ok(())
}
