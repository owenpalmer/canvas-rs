//! A course's sections: modules, assignments, grades, announcements, discussions, pages, files,
//! syllabus.

use egui::{Id, Ui, vec2};
use serde_json::Value;

use super::{RowSpec, course_info, course_shell, empty_row, group_head, h2_anchor, list, row, sub_pill};
use crate::app::{App, Pane};
use crate::data::Need;
use crate::fmt;
use crate::theme::t;
use crate::widgets::{self as w, Pill, Ts};

fn module_item_link(cid: &str, it: &Value) -> Option<String> {
    let s = |k: &str| fmt::id(&it[k]);
    Some(match it["type"].as_str().unwrap_or("") {
        "Page" => format!("#/c/{cid}/p/{}", s("page_url")),
        "Assignment" => format!("#/c/{cid}/a/{}", s("content_id")),
        "Discussion" => format!("#/c/{cid}/d/{}", s("content_id")),
        "File" => format!("#/c/{cid}/f/{}", s("content_id")),
        "ExternalUrl" => return it["external_url"].as_str().map(String::from),
        _ => return it["html_url"].as_str().map(String::from),
    })
}

pub fn modules(app: &mut App, ui: &mut Ui, pane: Pane, cid: &str) -> Result<(), Need> {
    let mods = app.d.need1("modules", cid)?;
    let info = course_info(app, cid)?;
    let tabs = app.d.need1("tabs", cid).ok();
    course_shell(app, ui, pane, cid, "modules", &info, tabs.as_deref());
    let tk = t();
    let mods = mods.as_array().cloned().unwrap_or_default();
    if mods.is_empty() {
        w::empty(ui, "This course has no modules. Try Pages or the Syllabus.");
        return Ok(());
    }
    ui.add_space(-20.0); // the tabs' margin collapses into the first heading's
    for m in &mods {
        let mid = fmt::id(&m["id"]);
        let anchor = format!("m{mid}");
        let y = ui.cursor().min.y + 28.0;
        let mut nb_click = false;
        group_head(ui, &fmt::s(&m["name"]), |ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let r = w::text_button(ui, "+ Notebook", Ts::new(12.0, 500, tk.faint), Ts::new(12.0, 500, tk.accent)).on_hover_text("Create a Gemini notebook from this module");
            nb_click = r.clicked();
            match m["state"].as_str() {
                Some("completed") => {
                    w::pill(ui, Pill::Ok, "Done");
                }
                Some("locked") => {
                    w::pill(ui, Pill::Plain, "Locked");
                }
                _ => {}
            }
        });
        let st = crate::panes::state(app, pane);
        let off = y - st.content_top - 14.0;
        st.anchors.insert(anchor, off.max(0.0));
        if nb_click {
            crate::nav::go(app, &format!("#/notebooks/new?c={cid}&m=m{mid}"));
        }
        let items = m["items"].as_array().cloned().unwrap_or_default();
        list(ui, |ui| {
            if items.is_empty() {
                empty_row(ui, "No items");
            }
            for (i, it) in items.iter().enumerate() {
                let indent = it["indent"].as_f64().unwrap_or(0.0) as f32 * 20.0;
                if it["type"] == "SubHeader" {
                    row(app, ui, pane, RowSpec { kind: Some(String::new()), indent, title: fmt::s(&it["title"]), subheader: true, first: i == 0, ..Default::default() });
                    continue;
                }
                let d = &it["content_details"];
                let mut meta = Vec::new();
                if super::truthy(&d["due_at"]) {
                    meta.push(format!("Due {}", fmt::fmt_date(&d["due_at"])));
                }
                if !d["points_possible"].is_null() {
                    meta.push(format!("{} pts", fmt::num(&d["points_possible"])));
                }
                let kind = fmt::s(&it["type"]).replace("ExternalUrl", "Link").replace("ExternalTool", "Tool");
                let done = it["completion_requirement"]["completed"].as_bool().unwrap_or(false);
                let href = module_item_link(cid, it);
                row(app, ui, pane, RowSpec {
                    key: Some(format!("mi:{}", fmt::id(&it["id"]))),
                    href,
                    kind: Some(kind),
                    indent,
                    title: fmt::s(&it["title"]),
                    meta: (!meta.is_empty()).then(|| meta.join(" · ")),
                    locked: d["locked_for_user"].as_bool().unwrap_or(false),
                    trailing: done.then(|| (Pill::Ok, "✓".to_string())),
                    first: i == 0,
                    ..Default::default()
                });
            }
        });
    }
    let _ = h2_anchor;
    Ok(())
}

fn all_assignments(groups: &Value) -> Vec<(Value, String)> {
    groups.as_array().into_iter().flatten().flat_map(|g| g["assignments"].as_array().cloned().unwrap_or_default().into_iter().map(move |a| (a, fmt::s(&g["name"])))).collect()
}

fn assignment_row(app: &mut App, ui: &mut Ui, pane: Pane, cid: &str, a: &Value, group: &str, first: bool) {
    let mut meta = if super::truthy(&a["due_at"]) { format!("Due {}", fmt::fmt_date(&a["due_at"])) } else { "No due date".into() };
    if !a["points_possible"].is_null() {
        meta += &format!(" · {} pts", fmt::num(&a["points_possible"]));
    }
    if !group.is_empty() {
        meta += &format!(" · {group}");
    }
    let pill = sub_pill(&a["submission"], a);
    row(app, ui, pane, RowSpec {
        href: Some(format!("#/c/{cid}/a/{}", fmt::id(&a["id"]))),
        title: fmt::s(&a["name"]),
        meta: Some(meta),
        right: pill.map(|p| vec![(vec![p], String::new())]).unwrap_or_default(),
        first,
        ..Default::default()
    });
}

pub fn assignments(app: &mut App, ui: &mut Ui, pane: Pane, cid: &str) -> Result<(), Need> {
    let groups = app.d.need1("groups", cid)?;
    let info = course_info(app, cid)?;
    let tabs = app.d.need1("tabs", cid).ok();
    course_shell(app, ui, pane, cid, "assignments", &info, tabs.as_deref());
    let all = all_assignments(&groups);
    if all.is_empty() {
        w::empty(ui, "No assignments.");
        return Ok(());
    }
    let now = chrono::Local::now();
    let due = |a: &Value| fmt::iso(&a["due_at"]);
    let mut upcoming: Vec<&(Value, String)> = all.iter().filter(|(a, _)| due(a).map(|d| d >= now).unwrap_or(false)).collect();
    upcoming.sort_by_key(|(a, _)| fmt::s(&a["due_at"]));
    let undated: Vec<&(Value, String)> = all.iter().filter(|(a, _)| !super::truthy(&a["due_at"])).collect();
    let mut past: Vec<&(Value, String)> = all.iter().filter(|(a, _)| due(a).map(|d| d < now).unwrap_or(false)).collect();
    past.sort_by_key(|(a, _)| std::cmp::Reverse(fmt::s(&a["due_at"])));
    ui.add_space(-20.0);
    for (title, items) in [("Upcoming", upcoming), ("Undated", undated), ("Past", past)] {
        if items.is_empty() {
            continue;
        }
        w::h2(ui, title);
        list(ui, |ui| {
            for (i, (a, g)) in items.iter().enumerate() {
                assignment_row(app, ui, pane, cid, a, g, i == 0);
            }
        });
    }
    Ok(())
}

pub fn grades(app: &mut App, ui: &mut Ui, pane: Pane, cid: &str) -> Result<(), Need> {
    let groups = app.d.need1("groups", cid)?;
    let info = course_info(app, cid)?;
    let tabs = app.d.need1("tabs", cid).ok();
    course_shell(app, ui, pane, cid, "grades", &info, tabs.as_deref());
    let tk = t();
    let e = info.c["enrollments"].as_array().and_then(|a| a.iter().find(|x| x["type"] == "student")).cloned().unwrap_or(Value::Null);
    let weighted = groups.as_array().map(|a| a.iter().any(|g| g["group_weight"].as_f64().unwrap_or(0.0) != 0.0)).unwrap_or(false);
    // .grade-summary: big score, and "Current score · grade"
    let big = if e["computed_current_score"].is_null() { "–".to_string() } else { format!("{}%", fmt::num(&e["computed_current_score"])) };
    w::text_line(ui, &big, Ts::new(34.0, 650, tk.text));
    let grade = e["computed_current_grade"].as_str().filter(|g| !g.is_empty()).map(|g| format!(" · {g}")).unwrap_or_default();
    w::sub(ui, &format!("Current score{grade}"));
    for g in groups.as_array().cloned().unwrap_or_default() {
        let assignments = g["assignments"].as_array().cloned().unwrap_or_default();
        let graded: Vec<&Value> = assignments.iter().filter(|a| !a["submission"]["score"].is_null() && !a["submission"]["excused"].as_bool().unwrap_or(false) && !a["omit_from_final_grade"].as_bool().unwrap_or(false)).collect();
        let got: f64 = graded.iter().map(|a| a["submission"]["score"].as_f64().unwrap_or(0.0)).sum();
        let poss: f64 = graded.iter().map(|a| a["points_possible"].as_f64().unwrap_or(0.0)).sum();
        let left = format!("{}{}", fmt::s(&g["name"]), if weighted { format!(" · {}%", fmt::num(&g["group_weight"])) } else { String::new() });
        let right = if poss != 0.0 { format!("{} / {} ({}%)", fmt::num_f(got), fmt::num_f(poss), fmt::num_f(100.0 * got / poss)) } else { String::new() };
        group_head(ui, &left, |ui| {
            if !right.is_empty() {
                w::text_line(ui, &right, w::h2_ts());
            }
        });
        list(ui, |ui| {
            if assignments.is_empty() {
                empty_row(ui, "No assignments");
            }
            for (i, a) in assignments.iter().enumerate() {
                let s = &a["submission"];
                let score = if s["excused"].as_bool().unwrap_or(false) {
                    "Excused".to_string()
                } else if !s["score"].is_null() {
                    format!("{} / {}", fmt::num(&s["score"]), fmt::num(&a["points_possible"]))
                } else {
                    format!("– / {}", fmt::num(&a["points_possible"]))
                };
                let mut right = vec![(vec![], score)];
                if s["missing"].as_bool().unwrap_or(false) {
                    right.push((vec![(Pill::Bad, "Missing".into())], String::new()));
                } else if s["late"].as_bool().unwrap_or(false) {
                    right.push((vec![(Pill::Warn, "Late".into())], String::new()));
                }
                row(app, ui, pane, RowSpec {
                    href: Some(format!("#/c/{cid}/a/{}", fmt::id(&a["id"]))),
                    title: fmt::s(&a["name"]),
                    meta: Some(if super::truthy(&a["due_at"]) { format!("Due {}", fmt::fmt_date(&a["due_at"])) } else { "No due date".into() }),
                    right,
                    right_score: true,
                    first: i == 0,
                    ..Default::default()
                });
            }
        });
    }
    Ok(())
}

pub fn announcements(app: &mut App, ui: &mut Ui, pane: Pane, cid: &str) -> Result<(), Need> {
    let anns = app.d.need1("course_announcements", cid)?;
    let info = course_info(app, cid)?;
    let tabs = app.d.need1("tabs", cid).ok();
    course_shell(app, ui, pane, cid, "announcements", &info, tabs.as_deref());
    let mut anns: Vec<Value> = anns.as_array().cloned().unwrap_or_default();
    anns.sort_by_key(|a| std::cmp::Reverse(fmt::posted_at(a)));
    if anns.is_empty() {
        w::empty(ui, "No announcements.");
        return Ok(());
    }
    list(ui, |ui| {
        for (i, a) in anns.iter().enumerate() {
            let who = a["user_name"].as_str().filter(|x| !x.is_empty()).or(a["author"]["display_name"].as_str()).unwrap_or("").to_string();
            let text: String = crate::html::text_of(&fmt::s(&a["message"])).chars().take(160).collect();
            row(app, ui, pane, RowSpec {
                href: Some(format!("#/c/{cid}/d/{}", fmt::id(&a["id"]))),
                title: fmt::s(&a["title"]),
                meta: Some(format!("{who} · {text}")),
                right: vec![(vec![], fmt::parse_iso(&fmt::posted_at(a)).map(|d| fmt::fmt_day(&d)).unwrap_or_default())],
                first: i == 0,
                ..Default::default()
            });
        }
    });
    Ok(())
}

pub fn discussions(app: &mut App, ui: &mut Ui, pane: Pane, cid: &str) -> Result<(), Need> {
    let topics = app.d.need1("discussions", cid)?;
    let info = course_info(app, cid)?;
    let tabs = app.d.need1("tabs", cid).ok();
    course_shell(app, ui, pane, cid, "discussions", &info, tabs.as_deref());
    let topics = topics.as_array().cloned().unwrap_or_default();
    if topics.is_empty() {
        w::empty(ui, "No discussions.");
        return Ok(());
    }
    list(ui, |ui| {
        for (i, t) in topics.iter().enumerate() {
            let mut meta = format!("{} replies", t["discussion_subentry_count"].as_i64().unwrap_or(0));
            if super::truthy(&t["last_reply_at"]) {
                meta += &format!(" · last {}", fmt::fmt_date(&t["last_reply_at"]));
            }
            if super::truthy(&t["assignment"]["due_at"]) {
                meta += &format!(" · due {}", fmt::fmt_date(&t["assignment"]["due_at"]));
            }
            let unread = t["unread_count"].as_i64().unwrap_or(0);
            row(app, ui, pane, RowSpec {
                href: Some(format!("#/c/{cid}/d/{}", fmt::id(&t["id"]))),
                title: fmt::s(&t["title"]),
                meta: Some(meta),
                right: if unread > 0 { vec![(vec![(Pill::Warn, format!("{unread} new"))], String::new())] } else { vec![] },
                first: i == 0,
                ..Default::default()
            });
        }
    });
    Ok(())
}

pub fn pages(app: &mut App, ui: &mut Ui, pane: Pane, cid: &str) -> Result<(), Need> {
    let pages = app.d.need1("pages", cid)?;
    let info = course_info(app, cid)?;
    let tabs = app.d.need1("tabs", cid).ok();
    course_shell(app, ui, pane, cid, "pages", &info, tabs.as_deref());
    let pages = pages.as_array().cloned().unwrap_or_default();
    if pages.is_empty() {
        w::empty(ui, "No pages.");
        return Ok(());
    }
    list(ui, |ui| {
        for (i, p) in pages.iter().enumerate() {
            let pills = if p["front_page"].as_bool().unwrap_or(false) { vec![(Pill::Plain, "Front page".to_string())] } else { vec![] };
            let day = fmt::iso(&p["updated_at"]).map(|d| fmt::fmt_day(&d)).unwrap_or_default();
            row(app, ui, pane, RowSpec {
                href: Some(format!("#/c/{cid}/p/{}", crate::route::enc(&fmt::s(&p["url"])))),
                title: fmt::s(&p["title"]),
                right: vec![(pills, day)],
                first: i == 0,
                ..Default::default()
            });
        }
    });
    Ok(())
}

pub fn files(app: &mut App, ui: &mut Ui, pane: Pane, cid: &str) -> Result<(), Need> {
    let listing = app.d.need1("files", cid)?;
    let info = course_info(app, cid)?;
    let tabs = app.d.need1("tabs", cid).ok();
    course_shell(app, ui, pane, cid, "files", &info, tabs.as_deref());
    let files = listing["files"].as_array().cloned().unwrap_or_default();
    if files.is_empty() {
        w::empty(ui, "No files (or the Files tab is hidden in this course).");
        return Ok(());
    }
    let folder_name = |fid: &Value| -> String {
        listing["folders"].as_array().into_iter().flatten().find(|f| f["id"] == *fid).map(|f| fmt::s(&f["full_name"])).map(|n| n.trim_start_matches("course files").trim_start_matches('/').to_string()).unwrap_or_default()
    };
    let mut sorted = files.clone();
    sorted.sort_by(|a, b| folder_name(&a["folder_id"]).cmp(&folder_name(&b["folder_id"])).then_with(|| fmt::s(&a["display_name"]).cmp(&fmt::s(&b["display_name"]))));
    let mut by_folder: Vec<(String, Vec<Value>)> = Vec::new();
    for f in sorted {
        let k = folder_name(&f["folder_id"]);
        match by_folder.iter_mut().find(|(n, _)| *n == k) {
            Some((_, v)) => v.push(f),
            None => by_folder.push((k, vec![f])),
        }
    }
    // the filter: a field styled like the search button
    let id = Id::new(("files-filter", cid));
    let mut q = app.settings.filters.get(cid).cloned().unwrap_or_default();
    if pane == Pane::Main && std::mem::take(&mut app.settings.focus_filter) {
        ui.memory_mut(|m| m.request_focus(id));
    }
    let resp = w::input(ui, id, &mut q, &format!("Filter {} files…", files.len()), w::InputOpts { pad: vec2(8.0, 6.0), focus_ring: false, ..Default::default() });
    if resp.changed() {
        app.settings.filters.insert(cid.to_string(), q.clone());
    }
    let ql = q.trim().to_lowercase();
    for (folder, fs) in by_folder {
        let shown: Vec<&Value> = fs.iter().filter(|f| ql.is_empty() || fmt::s(&f["display_name"]).to_lowercase().contains(&ql)).collect();
        if shown.is_empty() {
            continue;
        }
        w::h2(ui, if folder.is_empty() { "Top level" } else { &folder });
        list(ui, |ui| {
            for (i, f) in shown.iter().enumerate() {
                let day = fmt::iso(&f["updated_at"]).map(|d| fmt::fmt_day(&d)).unwrap_or_default();
                row(app, ui, pane, RowSpec {
                    href: Some(format!("#/c/{cid}/f/{}", fmt::id(&f["id"]))),
                    title: fmt::s(&f["display_name"]),
                    right: vec![(vec![], format!("{} · {day}", fmt::fmt_size(&f["size"])))],
                    first: i == 0,
                    ..Default::default()
                });
            }
        });
    }
    Ok(())
}

/// Home: the course's front page.
pub fn home(app: &mut App, ui: &mut Ui, pane: Pane, cid: &str) -> Result<(), Need> {
    let p = app.d.need1("front_page", cid)?;
    let info = course_info(app, cid)?;
    let tabs = app.d.need1("tabs", cid).ok();
    course_shell(app, ui, pane, cid, "home", &info, tabs.as_deref());
    let body = fmt::s(&p["body"]);
    if body.trim().is_empty() {
        w::empty(ui, "The home page is empty.");
    } else {
        crate::html::content(app, ui, &body, pane);
    }
    Ok(())
}

pub fn syllabus(app: &mut App, ui: &mut Ui, pane: Pane, cid: &str) -> Result<(), Need> {
    let body = app.d.need1("syllabus", cid)?;
    let info = course_info(app, cid)?;
    let tabs = app.d.need1("tabs", cid).ok();
    course_shell(app, ui, pane, cid, "syllabus", &info, tabs.as_deref());
    let html = fmt::s(&body);
    if html.is_empty() {
        w::empty(ui, "No syllabus text. Check Modules or Files.");
    } else {
        crate::html::content(app, ui, &html, pane);
    }
    Ok(())
}
