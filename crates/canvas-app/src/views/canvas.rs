//! The dashboard, and looking up a course.

use chrono::{Local, Timelike};
use egui::{CursorIcon, Id, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use serde_json::{Value, json};

use super::{RowSpec, h1, head, list, row};
use crate::app::{App, Pane};
use crate::data::{Json, Need};
use crate::fmt::{self, color_for};
use crate::theme::{self, t};
use crate::widgets::{self as w, Pill, Ts, cr, lay};

pub struct CourseInfo {
    pub c: Value,
    pub past: bool,
}

/// A course (active or past) with its color; a placeholder when it isn't in either list.
pub fn course_info(app: &App, cid: &str) -> Result<CourseInfo, Need> {
    let courses = app.d.need0("courses")?;
    app.d.need0("colors")?; // the course's color is ready with it
    if let Some(c) = courses.as_array().and_then(|a| a.iter().find(|x| fmt::id(&x["id"]) == cid)) {
        return Ok(CourseInfo { c: c.clone(), past: false });
    }
    let past = app.d.need0("past_courses").ok();
    if let Some(c) = past.as_ref().and_then(|p| p.as_array()).and_then(|a| a.iter().find(|x| fmt::id(&x["id"]) == cid)) {
        return Ok(CourseInfo { c: c.clone(), past: true });
    }
    Ok(CourseInfo { c: json!({"id": cid, "name": format!("Course {cid}"), "course_code": ""}), past: false })
}

pub fn planner_link(app: &App, it: &Value) -> String {
    let p = &it["plannable"];
    let cid = fmt::id(&it["course_id"]);
    let base = app.base();
    let html = |it: &Value| if let Some(u) = it["html_url"].as_str() { format!("{base}{u}") } else { "#/".into() };
    match it["plannable_type"].as_str().unwrap_or("") {
        "assignment" => format!("#/c/{cid}/a/{}", fmt::id(&it["plannable_id"])),
        "quiz" => if p["assignment_id"].is_null() { html(it) } else { format!("#/c/{cid}/a/{}", fmt::id(&p["assignment_id"])) },
        "discussion_topic" | "announcement" => format!("#/c/{cid}/d/{}", fmt::id(&it["plannable_id"])),
        "wiki_page" => if let Some(u) = p["url"].as_str() { format!("#/c/{cid}/p/{u}") } else { html(it) },
        _ => html(it),
    }
}

fn done(it: &Value) -> bool {
    let s = &it["submissions"];
    s.is_object() && (super::truthy(&s["submitted"]) || super::truthy(&s["graded"]) || super::truthy(&s["excused"]))
}

fn planner_row(app: &mut App, ui: &mut Ui, pane: Pane, it: &Value, colors: &Json, first: bool) {
    let s = &it["submissions"];
    let st = if super::truthy(&s["graded"]) {
        Some((Pill::Ok, "Graded"))
    } else if super::truthy(&s["submitted"]) {
        Some((Pill::Ok, "Submitted"))
    } else if super::truthy(&s["missing"]) {
        Some((Pill::Bad, "Missing"))
    } else if super::truthy(&s["late"]) {
        Some((Pill::Warn, "Late"))
    } else {
        None
    };
    let p = &it["plannable"];
    let title = p["title"].as_str().filter(|x| !x.is_empty()).or(p["name"].as_str()).filter(|x| !x.is_empty()).unwrap_or("Untitled").to_string();
    let kind = it["plannable_type"].as_str().unwrap_or("").replacen('_', " ", 1);
    let pts = if p["points_possible"].is_null() { String::new() } else { format!(" · {} pts", fmt::num(&p["points_possible"])) };
    let when = fmt::iso(&it["plannable_date"]).map(|d| fmt::fmt_time(&d)).unwrap_or_default();
    let mut right = Vec::new();
    if let Some((k, l)) = st {
        right.push((vec![(k, l.to_string())], String::new()));
    }
    right.push((vec![], when));
    let cid = fmt::id(&it["course_id"]);
    row(app, ui, pane, RowSpec {
        href: Some(planner_link(app, it)),
        key: Some(format!("planner:{}:{}", fmt::s(&it["plannable_type"]), fmt::id(&it["plannable_id"]))),
        bar: Some(color_for(&cid, Some(colors))),
        title,
        meta: Some(format!("{} · {kind}{pts}", fmt::s(&it["context_name"]))),
        right,
        first,
        ..Default::default()
    });
}

pub fn dashboard(app: &mut App, ui: &mut Ui, pane: Pane) -> Result<(), Need> {
    let d = &app.d;
    let (me, courses, colors, planner, anns) = (d.need0("self")?, d.need0("courses")?, d.need0("colors")?, d.need0("planner")?, d.need0("announcements")?);
    let tk = t();
    let now = Local::now();
    let today = now.date_naive();
    let items: Vec<&Value> = planner.as_array().into_iter().flatten().filter(|it| it["plannable_type"] != "announcement" && super::truthy(&it["plannable_date"])).collect();
    let missing: Vec<&Value> = items.iter().copied().filter(|it| fmt::iso(&it["plannable_date"]).map(|d| d < now).unwrap_or(false) && super::truthy(&it["submissions"]["missing"]) && !done(it)).collect();
    let mut upcoming: Vec<&Value> = items
        .iter()
        .copied()
        .filter(|it| fmt::iso(&it["plannable_date"]).map(|d| d.date_naive() >= today && d.date_naive() < today + chrono::Duration::days(14)).unwrap_or(false))
        .collect();
    upcoming.sort_by_key(|it| fmt::s(&it["plannable_date"]));
    let mut by_day: Vec<(chrono::NaiveDate, Vec<&Value>)> = Vec::new();
    for it in upcoming {
        let k = fmt::iso(&it["plannable_date"]).unwrap().date_naive();
        match by_day.iter_mut().find(|(d, _)| *d == k) {
            Some((_, v)) => v.push(it),
            None => by_day.push((k, vec![it])),
        }
    }
    let hour = now.hour();
    let hello = if hour < 12 { "Good morning" } else if hour < 18 { "Good afternoon" } else { "Good evening" };
    let who = me["short_name"].as_str().filter(|x| !x.is_empty()).or(me["name"].as_str()).unwrap_or("").to_string();
    let title = format!("{hello}, {who}");
    head(app, ui, pane, &title, vec![], None);
    h1(ui, pane, &title);
    w::sub(ui, &fmt::long_date(&now));

    if !missing.is_empty() {
        w::h2(ui, "Missing");
        list(ui, |ui| {
            for (i, it) in missing.iter().enumerate() {
                planner_row(app, ui, pane, it, &colors, i == 0);
            }
        });
    }
    w::h2(ui, "Coming up");
    if by_day.is_empty() {
        w::empty(ui, "Nothing due in the next two weeks.");
    }
    for (day, its) in &by_day {
        // .day-head: 13px 600 muted (today accent), margin 18px 0 6px
        ui.add_space(18.0 - 8.0 * (day == &by_day[0].0) as i32 as f32 * 0.0);
        w::text_line(ui, &fmt::fmt_day_date(*day), Ts::new(13.0, 600, if *day == today { tk.accent } else { tk.muted }));
        ui.add_space(6.0);
        list(ui, |ui| {
            for (i, it) in its.iter().enumerate() {
                planner_row(app, ui, pane, it, &colors, i == 0);
            }
        });
    }

    w::h2(ui, "Courses");
    let courses_v: Vec<Value> = courses.as_array().cloned().unwrap_or_default();
    let ordered = crate::sidebar::ordered_courses(app, &courses_v, false);
    cards(app, ui, pane, &ordered, &colors);

    w::h2(ui, "Recent announcements");
    let cutoff = now - chrono::Duration::days(14);
    let mut recent: Vec<&Value> = anns.as_array().into_iter().flatten().filter(|a| fmt::parse_iso(&fmt::posted_at(a)).map(|d| d > cutoff).unwrap_or(false)).collect();
    recent.sort_by_key(|a| std::cmp::Reverse(fmt::posted_at(a)));
    recent.truncate(6);
    if recent.is_empty() {
        w::empty(ui, "No announcements in the last two weeks.");
    } else {
        list(ui, |ui| {
            for (i, a) in recent.iter().enumerate() {
                let cid = fmt::s(&a["context_code"]).split('_').nth(1).unwrap_or("").to_string();
                let code = courses_v.iter().find(|c| fmt::id(&c["id"]) == cid).map(|c| fmt::s(&c["course_code"])).unwrap_or_default();
                let text: String = crate::html::text_of(&fmt::s(&a["message"])).chars().take(140).collect();
                let when = fmt::parse_iso(&fmt::posted_at(a)).map(|d| fmt::fmt_day(&d)).unwrap_or_default();
                row(app, ui, pane, RowSpec {
                    href: Some(format!("#/c/{cid}/d/{}", fmt::id(&a["id"]))),
                    bar: Some(color_for(&cid, Some(&colors))),
                    title: fmt::s(&a["title"]),
                    meta: Some(format!("{code} · {text}")),
                    right: vec![(vec![], when)],
                    first: i == 0,
                    ..Default::default()
                });
            }
        });
    }
    Ok(())
}

/// .cards: auto-fill columns of at least 220px, 12px apart; each card is a link to the course.
fn cards(app: &mut App, ui: &mut Ui, pane: Pane, courses: &[Value], colors: &Json) {
    let tk = t();
    let width = ui.available_width();
    let n = (((width + 12.0) / (220.0 + 12.0)).floor() as usize).max(1);
    let col_w = (width - 12.0 * (n as f32 - 1.0)) / n as f32;
    for chunk in courses.chunks(n) {
        // measure: every card in a grid row is as tall as the tallest
        let mut heights = Vec::new();
        let mut laid = Vec::new();
        for c in chunk {
            let inner = col_w - 28.0 - 2.0;
            let code = lay(ui, &fmt::s(&c["course_code"]), Ts::muted(12.0), Some(inner), true);
            let name = lay(ui, &fmt::s(&c["name"]), Ts::new(14.0, 600, tk.text).lh(1.3), Some(inner), false);
            let e = c["enrollments"].as_array().and_then(|a| a.iter().find(|x| x["type"] == "student")).cloned().unwrap_or(json!({}));
            let h = 4.0 + 14.0 + 18.0 + 2.0 + name.rows.len() as f32 * (14.0f32 * 1.3).round() + 10.0 + 33.0 + 14.0 + 1.0;
            heights.push(h);
            laid.push((code, name, e));
        }
        let row_h = heights.iter().cloned().fold(0.0, f32::max);
        let (row_rect, _) = ui.allocate_exact_size(vec2(width, row_h), Sense::hover());
        for (i, (c, (code, name, e))) in chunk.iter().zip(laid).enumerate() {
            let cid = fmt::id(&c["id"]);
            let color = color_for(&cid, Some(colors));
            let r = Rect::from_min_size(pos2(row_rect.min.x + i as f32 * (col_w + 12.0), row_rect.min.y), vec2(col_w, row_h));
            let href = crate::views::course_href(app, &cid);
            let resp = ui.interact(r, Id::new(("card", &cid)), Sense::click());
            let key = format!("card:{cid}");
            let cursor = crate::panes::state(app, pane).cursor.as_deref() == Some(key.as_str()) && app.panes.focused == pane;
            let p = ui.painter();
            p.rect_filled(r, cr(8.0), tk.panel);
            p.rect_stroke(r, cr(8.0), Stroke::new(1.0, if resp.hovered() { tk.faint } else { tk.line }), StrokeKind::Inside);
            // border-top: 4px in the course's color, following the corners
            p.with_clip_rect(Rect::from_min_size(r.min, vec2(r.width(), 4.0))).rect_filled(r, cr(8.0), color);
            if cursor {
                w::focus_ring(ui, r, 8.0);
            }
            let x = r.min.x + 15.0;
            let mut y = r.min.y + 4.0 + 14.0;
            p.galley(pos2(x, y + 2.0), code, tk.muted);
            y += 18.0 + 2.0;
            let nh = name.rows.len() as f32 * (14.0f32 * 1.3).round();
            p.galley(pos2(x, y + 1.0), name, tk.text);
            y += nh + 10.0;
            // the grade: "91.4% A-" or "No grade yet"
            let mut rich = w::Rich::new();
            if e["computed_current_score"].is_null() {
                rich.push("No grade yet", Ts::new(13.0, 500, tk.muted));
            } else {
                rich.push(&format!("{}%", fmt::num(&e["computed_current_score"])), Ts::new(22.0, 650, tk.text));
                if let Some(g) = e["computed_current_grade"].as_str().filter(|g| !g.is_empty()) {
                    rich.push(" ", Ts::new(22.0, 650, tk.text));
                    rich.push(g, Ts::new(13.0, 500, tk.muted));
                }
            }
            let g = rich.lay(ui);
            p.galley(pos2(x, y + 33.0 - g.size().y - 2.0), g, tk.text);
            crate::panes::state(app, pane).items.push(crate::app::Item { key: key.clone(), rect: r, href: Some(href.clone()) });
            if resp.hovered() {
                crate::nav::prefetch(app, &href);
            }
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                crate::nav::go(app, &href);
            }
        }
        ui.add_space(12.0);
    }
    ui.add_space(-12.0);
    let _ = theme::t();
}
