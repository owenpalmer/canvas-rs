//! Gemini Notebook integration: notebook list, the source builder, and live upload progress.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::Duration;

use egui::{CursorIcon, Id, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use once_cell::sync::Lazy as OnceLazy;
use regex::Regex;
use serde_json::{Value, json};

use crate::app::{App, Pane};
use crate::data::{Lazy, Need};
use crate::fmt;
use crate::setup::{self, OnDone, Phase};
use crate::theme::{self, t};
use crate::views::{RowSpec, dashboard_crumb, detail_meta, h1, head, list, row};
use crate::widgets::{self as w, Pill, Ts, cr, lay};

const SOURCE_LIMIT: usize = 50;
const VERIFY: Duration = Duration::from_millis(1800);
static FILE_OK: OnceLazy<Regex> = OnceLazy::new(|| Regex::new(r"(?i)\.(pdf|txt|md|docx|pptx|csv|png|jpe?g|webp|mp3|wav|m4a|ogg)$").unwrap());
static PANOPTO_ID: OnceLazy<Regex> = OnceLazy::new(|| Regex::new(r"(?i)panopto\.com/[^\s']*?[?&](?:id|sessionId|deliveryId)=([0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12})").unwrap());
static EXT: OnceLazy<Regex> = OnceLazy::new(|| Regex::new(r"\.\w+$").unwrap());

fn kind_label(k: &str) -> &'static str {
    match k {
        "page" => "Page",
        "assignment" => "Assignment",
        "discussion" => "Discussion",
        "file" => "File",
        "url" => "Link",
        "syllabus" => "Syllabus",
        "recording" => "Recording",
        _ => "",
    }
}

#[derive(Default)]
pub struct Builder {
    pub id: Option<String>,
    pub title: String,
    pub selected: BTreeSet<String>,
    pub filter: String,
    pub q: String,
    pub busy: bool,
    pub error: Option<String>,
    pub titles: HashMap<String, String>,
}

#[derive(Default)]
pub struct NbState {
    pub jobs: HashMap<String, Value>,
    pub builder: Builder,
    pub pan_open: HashSet<String>,
    pub pan_link: String,
    pub pan_busy: bool,
}

pub fn init(app: &mut App) {
    for j in app.svc.notebooks.all_jobs() {
        let id = fmt::s(&j["id"]);
        app.nb.jobs.insert(id, j);
    }
}

pub fn on_job(app: &mut App, job: Value) {
    let id = fmt::s(&job["id"]);
    app.nb.jobs.insert(id, job);
}

pub fn running(app: &App) -> bool {
    app.nb.jobs.values().any(|j| !j["done"].as_bool().unwrap_or(false))
}

#[derive(Clone)]
struct Item {
    key: Option<String>,
    kind: String,
    title: String,
    meta: String,
    why: String,
}

struct Group {
    id: String,
    name: String,
    items: Vec<Item>,
    recordings: Option<RecState>,
}

enum RecState {
    Setup,
    Signin,
    Loading,
    Error,
    Folder(Value),
}

fn module_item_source(cid: &str, it: &Value, file_names: &HashMap<String, String>) -> Option<Item> {
    let s = |k: &str| fmt::id(&it[k]);
    let mk = |key: Option<String>, kind: &str, why: &str| Some(Item { key, kind: kind.into(), title: String::new(), meta: String::new(), why: why.into() });
    match it["type"].as_str().unwrap_or("") {
        "Page" => mk(Some(format!("page:{cid}:{}", s("page_url"))), "page", ""),
        "Assignment" => mk(Some(format!("assignment:{cid}:{}", s("content_id"))), "assignment", ""),
        "Discussion" => mk(Some(format!("discussion:{cid}:{}", s("content_id"))), "discussion", ""),
        "File" => {
            let name = file_names.get(&s("content_id")).cloned().unwrap_or_else(|| fmt::s(&it["title"]));
            if FILE_OK.is_match(&name) {
                mk(Some(format!("file:{}", s("content_id"))), "file", "")
            } else {
                let ext = EXT.find(&name).map(|m| m.as_str().to_string()).unwrap_or_else(|| "this type".into());
                mk(None, "file", &format!("Gemini can't read {ext} files"))
            }
        }
        t @ ("ExternalUrl" | "ExternalTool") => {
            let url = it["external_url"].as_str().or(it["url"].as_str()).unwrap_or("");
            if let Some(m) = PANOPTO_ID.captures(url) {
                return mk(Some(format!("recording:{cid}:{}", m[1].to_lowercase())), "recording", "");
            }
            if t == "ExternalTool" {
                return mk(None, "other", "External tools can't be exported");
            }
            let ext = it["external_url"].as_str().unwrap_or("");
            if ext.starts_with("http://") || ext.starts_with("https://") { mk(Some(format!("url:{ext}")), "url", "") } else { mk(None, "url", "Not a web link") }
        }
        "Quiz" => mk(None, "other", "Quizzes can't be exported"),
        _ => None,
    }
}

/// All selectable things in a course, grouped: one group per module, then everything else.
fn source_groups(app: &mut App, cid: &str) -> Result<Vec<Group>, Need> {
    let d = &app.d;
    let mods = d.need1("modules", cid)?;
    let files = match d.need1("files", cid) {
        Ok(f) => (*f).clone(),
        Err(Need::Pending) => return Err(Need::Pending),
        Err(_) => json!({"files": [], "folders": []}),
    };
    let soft = |r: Result<crate::data::Json, Need>, default: Value| -> Result<Value, Need> {
        match r {
            Ok(v) => Ok((*v).clone()),
            Err(Need::Pending) => Err(Need::Pending),
            Err(_) => Ok(default),
        }
    };
    let pages = soft(d.need1("pages", cid), json!([]))?;
    let groups = soft(d.need1("groups", cid), json!([]))?;
    let syllabus = soft(d.need1("syllabus", cid), json!(""))?;
    let file_names: HashMap<String, String> = files["files"].as_array().into_iter().flatten().map(|f| (fmt::id(&f["id"]), fmt::s(&f["display_name"]))).collect();
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for m in mods.as_array().into_iter().flatten() {
        let mut items = Vec::new();
        for it in m["items"].as_array().into_iter().flatten() {
            let Some(mut s) = module_item_source(cid, it, &file_names) else { continue };
            s.title = if it["type"] == "File" { file_names.get(&fmt::id(&it["content_id"])).cloned().unwrap_or_else(|| fmt::s(&it["title"])) } else { fmt::s(&it["title"]) };
            if crate::views::truthy(&it["content_details"]["due_at"]) {
                s.meta = format!("Due {}", fmt::fmt_date(&it["content_details"]["due_at"]));
            }
            if let Some(k) = &s.key {
                seen.insert(k.clone());
            }
            items.push(s);
        }
        out.push(Group { id: format!("m{}", fmt::id(&m["id"])), name: fmt::s(&m["name"]), items, recordings: None });
    }
    out.push(recordings_group(app, cid, &seen));
    let mut extra = Vec::new();
    if crate::views::truthy(&syllabus) {
        extra.push(Item { key: Some(format!("syllabus:{cid}")), kind: "syllabus".into(), title: "Syllabus".into(), meta: String::new(), why: String::new() });
    }
    for p in pages.as_array().into_iter().flatten() {
        let key = format!("page:{cid}:{}", fmt::s(&p["url"]));
        if !seen.contains(&key) {
            extra.push(Item { key: Some(key), kind: "page".into(), title: fmt::s(&p["title"]), meta: String::new(), why: String::new() });
        }
    }
    for a in groups.as_array().into_iter().flatten().flat_map(|g| g["assignments"].as_array().cloned().unwrap_or_default()) {
        let key = format!("assignment:{cid}:{}", fmt::id(&a["id"]));
        if !seen.contains(&key) {
            let meta = if crate::views::truthy(&a["due_at"]) { format!("Due {}", fmt::fmt_date(&a["due_at"])) } else { String::new() };
            extra.push(Item { key: Some(key), kind: "assignment".into(), title: fmt::s(&a["name"]), meta, why: String::new() });
        }
    }
    for f in files["files"].as_array().into_iter().flatten() {
        let key = format!("file:{}", fmt::id(&f["id"]));
        if seen.contains(&key) {
            continue;
        }
        let name = fmt::s(&f["display_name"]);
        if FILE_OK.is_match(&name) {
            extra.push(Item { key: Some(key), kind: "file".into(), title: name, meta: fmt::fmt_size(&f["size"]), why: String::new() });
        } else {
            let ext = EXT.find(&name).map(|m| m.as_str().to_string()).unwrap_or_else(|| "this type".into());
            extra.push(Item { key: None, kind: "file".into(), title: name, meta: String::new(), why: format!("Gemini can't read {ext} files") });
        }
    }
    if !extra.is_empty() {
        let has_mods = mods.as_array().map(|a| !a.is_empty()).unwrap_or(false);
        out.push(Group { id: "other".into(), name: if has_mods { "Not in a module".into() } else { "Course content".into() }, items: extra, recordings: None });
    }
    Ok(out)
}

/// Panopto recordings for the course; loads in the background so the builder never waits on Panopto.
fn recordings_group(app: &mut App, cid: &str, seen: &HashSet<String>) -> Group {
    let mut g = Group { id: "rec".into(), name: "Recordings (Panopto)".into(), items: Vec::new(), recordings: None };
    let host = fmt::s(&app.status["panopto_host"]);
    if !app.demo() && host.is_empty() {
        g.recordings = Some(RecState::Setup);
        return g;
    }
    if !app.allowed(&host) {
        g.recordings = Some(RecState::Signin); // nothing is read before you allow it
        return g;
    }
    match app.d.lazy("recordings", &[cid.to_string()]) {
        Lazy::Pending => g.recordings = Some(RecState::Loading),
        Lazy::Failed => g.recordings = Some(RecState::Error),
        Lazy::Ready(rec) => {
            g.recordings = Some(RecState::Folder(rec["folder"].clone()));
            for x in rec["items"].as_array().into_iter().flatten() {
                let key = format!("recording:{cid}:{}", fmt::s(&x["id"]).to_lowercase());
                if seen.contains(&key) {
                    continue;
                }
                let when = fmt::iso(&x["start"]).map(|d| fmt::fmt_day(&d)).unwrap_or_default();
                let mut meta = Vec::new();
                if !when.is_empty() {
                    meta.push(when);
                }
                if let Some(d) = x["duration"].as_f64().filter(|d| *d != 0.0) {
                    meta.push(format!("{} min", (d / 60.0).round() as i64));
                }
                if x["linked"].as_bool().unwrap_or(false) {
                    meta.push("linked from Canvas".into());
                }
                let caps = x["has_captions"].as_bool().unwrap_or(false);
                g.items.push(Item {
                    key: caps.then_some(key),
                    kind: "recording".into(),
                    title: x["title"].as_str().filter(|t| !t.is_empty()).unwrap_or("Untitled recording").into(),
                    meta: meta.join(" · "),
                    why: if caps { String::new() } else { "No captions yet".into() },
                });
            }
        }
    }
    g
}

/// NotebookLM needs your Google login from Firefox: None when connected; else draws the card.
fn google_gate(app: &mut App, ui: &mut Ui, pane: Pane, title: &str, sub: &str) -> Result<bool, Need> {
    let allowed = app.allowed("google");
    let s = setup::state(app, "google").clone();
    if s.phase == Phase::Idle && allowed && !s.quiet_checked {
        setup::quiet_check(app, "google"); // one quick look, so someone signed in never sees the card
        return Err(Need::Pending);
    }
    if s.phase == Phase::Checking && s.quiet_checked && s.next_poll.is_none() && allowed && !s.polling && s.message.is_none() {
        // still the quiet look
        if s.at.elapsed() < Duration::from_secs(10) && !matches!(s.info, Value::Object(_)) {
            return Err(Need::Pending);
        }
    }
    if s.phase == Phase::Done {
        if s.quiet || s.at.elapsed() >= VERIFY {
            return Ok(false);
        }
        ui.ctx().request_repaint_after(VERIFY - s.at.elapsed() + Duration::from_millis(50));
    }
    setup::when_signed_in(app, "google", OnDone::Google);
    crate::views::out(app, pane).title = title.to_string();
    h1(ui, pane, title);
    w::sub(ui, sub);
    ui.add_space(-18.0 + 8.0);
    let wdt = ui.available_width().min(640.0);
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(ui.cursor().min, vec2(wdt, f32::INFINITY))).layout(egui::Layout::top_down(egui::Align::Min)));
    w::boxed(&mut c, egui::Margin { left: 22, right: 22, top: 18, bottom: 18 }, 8.0, |ui| {
        w::text_line(ui, if s.phase == Phase::Done { "NotebookLM connected" } else { "Connect NotebookLM" }, Ts::new(17.0, 650, t().text));
        ui.add_space(6.0 - 8.0);
        setup::signin_card_bare(app, ui, "google");
    });
    let h = c.min_rect().height();
    ui.allocate_space(vec2(wdt, h));
    Ok(true)
}

fn account_line(app: &mut App, ui: &mut Ui) {
    let info = setup::state(app, "google").info.clone();
    let accounts = info["accounts"].as_array().cloned().unwrap_or_default();
    if accounts.is_empty() || app.demo() {
        return;
    }
    let tk = t();
    let au = info["authuser"].as_i64();
    let cur = accounts.iter().find(|a| a["authuser"].as_i64() == au).or(accounts.first()).cloned().unwrap_or(json!({}));
    ui.add_space(-10.0);
    if accounts.len() == 1 {
        let g = w::Rich::new().add("Using Google account ", Ts::muted(13.0)).add(&fmt::s(&cur["email"]), Ts::new(13.0, 700, tk.muted)).lay(ui);
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 19.5), Sense::hover());
        ui.painter().galley(pos2(r.min.x, r.center().y - g.size().y / 2.0), g, tk.muted);
    } else {
        let mut pick = None;
        ui.horizontal(|ui| {
            w::text_line_fixed(ui, "Using Google account ", Ts::muted(13.0));
            let opts: Vec<(String, String)> = accounts.iter().map(|a| (fmt::id(&a["authuser"]), fmt::s(&a["email"]))).collect();
            pick = w::select(ui, Id::new("nb-account"), &fmt::id(&cur["authuser"]), &opts, 240.0, 13.0);
        });
        if let Some(v) = pick {
            let n: i64 = v.parse().unwrap_or(0);
            setup::state(app, "google").info["authuser"] = json!(n);
            let svc = app.svc.clone();
            app.spawn(async move { svc.setup_google_account(n).await }, |app, _| {
                app.d.forget("notebooks");
            });
        }
    }
    ui.add_space(14.0);
}

fn status_pill(s: &str) -> (Pill, String) {
    match s {
        "queued" => (Pill::Plain, "Queued".into()),
        "preparing" => (Pill::Plain, "Preparing…".into()),
        "uploading" => (Pill::Warn, "Uploading…".into()),
        "done" => (Pill::Ok, "Added".into()),
        "skipped" => (Pill::Plain, "Already there".into()),
        "failed" => (Pill::Bad, "Failed".into()),
        other => (Pill::Plain, other.into()),
    }
}

pub fn list_view(app: &mut App, ui: &mut Ui, pane: Pane) -> Result<(), Need> {
    let (notebooks, err) = match app.d.need0("notebooks") {
        Ok(n) => ((*n).clone(), None),
        Err(Need::Pending) if setup::state(app, "google").phase == Phase::Done => return Err(Need::Pending),
        Err(Need::Err(e)) => (json!([]), Some(e.message())),
        Err(Need::Pending) => (json!([]), None),
    };
    let links = app.d.need0("nb_links")?;
    if google_gate(app, ui, pane, "Gemini Notebooks", "Turn your course material into Gemini notebooks (NotebookLM)")? {
        return Ok(());
    }
    if app.d.need0("notebooks").is_err() && err.is_none() {
        return Err(Need::Pending);
    }
    let mut from_canvas: HashMap<String, i64> = HashMap::new();
    for l in links.as_array().into_iter().flatten() {
        if l["status"] == "done" {
            *from_canvas.entry(fmt::s(&l["notebook_id"])).or_default() += 1;
        }
    }
    let running_ids: HashSet<String> = app.nb.jobs.values().filter(|j| !j["done"].as_bool().unwrap_or(false)).map(|j| fmt::s(&j["notebook_id"])).collect();
    let all = notebooks.as_array().cloned().unwrap_or_default();
    let mine: Vec<&Value> = all.iter().filter(|n| from_canvas.contains_key(&fmt::s(&n["id"])) || running_ids.contains(&fmt::s(&n["id"]))).collect();
    let others: Vec<&Value> = all.iter().filter(|n| !mine.iter().any(|m| m["id"] == n["id"])).collect();
    head(app, ui, pane, "Gemini Notebooks", vec![], None);
    h1(ui, pane, "Gemini Notebooks");
    w::sub(ui, "Gemini notebooks built from your Canvas courses");
    account_line(app, ui);
    crate::views::actions(ui, |ui| {
        if w::primary(ui, "New notebook").clicked() {
            crate::nav::go(app, "#/notebooks/new");
        }
    });
    if let Some(e) = &err {
        setup::notice_pub(ui, &format!("Couldn't reach Gemini Notebook: {e}. Make sure you're signed into Google in Firefox."), Pill::Warn);
    }
    let draw_rows = |app: &mut App, ui: &mut Ui, list_: &[&Value]| {
        list(ui, |ui| {
            for (i, n) in list_.iter().enumerate() {
                let id = fmt::s(&n["id"]);
                let mut meta = format!("{} sources", n["sources_count"].as_i64().unwrap_or(0));
                if let Some(k) = from_canvas.get(&id) {
                    meta += &format!(" · {k} from Canvas");
                }
                if let Some(d) = fmt::iso(&n["created_at"]) {
                    meta += &format!(" · created {}", fmt::fmt_day(&d));
                }
                row(app, ui, pane, RowSpec {
                    href: Some(format!("#/notebooks/{id}")),
                    title: n["title"].as_str().filter(|s| !s.is_empty()).unwrap_or("Untitled notebook").into(),
                    meta: Some(meta),
                    right: if running_ids.contains(&id) { vec![(vec![(Pill::Warn, "Uploading…".into())], String::new())] } else { vec![] },
                    first: i == 0,
                    ..Default::default()
                });
            }
        });
    };
    if !mine.is_empty() {
        w::h2(ui, "From Canvas");
        draw_rows(app, ui, &mine);
    }
    if !others.is_empty() {
        w::h2(ui, if mine.is_empty() { "Your notebooks" } else { "Other notebooks" });
        draw_rows(app, ui, &others);
    }
    if err.is_none() && all.is_empty() {
        w::empty(ui, "No notebooks yet.");
    }
    Ok(())
}

pub fn detail(app: &mut App, ui: &mut Ui, pane: Pane, nb_id: &String) -> Result<(), Need> {
    let notebooks = app.d.need0("notebooks").map(|n| (*n).clone()).or_else(|e| if matches!(e, Need::Pending) { Err(e) } else { Ok(json!([])) })?;
    let links = app.d.need0("nb_links")?;
    let mut my_jobs: Vec<Value> = app.nb.jobs.values().filter(|j| fmt::s(&j["notebook_id"]) == *nb_id).cloned().collect();
    my_jobs.sort_by(|a, b| b["started_at"].as_f64().partial_cmp(&a["started_at"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));
    let nb = notebooks.as_array().and_then(|a| a.iter().find(|n| fmt::s(&n["id"]) == *nb_id).cloned()).unwrap_or(json!({"id": nb_id, "title": my_jobs.first().map(|j| j["title"].clone()).unwrap_or(json!("Notebook")), "sources_count": 0}));
    let running: Vec<&Value> = my_jobs.iter().filter(|j| !j["done"].as_bool().unwrap_or(false)).collect();
    // latest status per item: live job state wins over the saved record
    let mut order: Vec<String> = Vec::new();
    let mut items: HashMap<String, (String, String, String)> = HashMap::new();
    for l in links.as_array().into_iter().flatten().filter(|l| fmt::s(&l["notebook_id"]) == *nb_id) {
        let k = fmt::s(&l["key"]);
        if !items.contains_key(&k) {
            order.push(k.clone());
        }
        items.insert(k.clone(), (l["title"].as_str().filter(|s| !s.is_empty()).map(String::from).unwrap_or(k), fmt::s(&l["status"]), fmt::s(&l["error"])));
    }
    for j in my_jobs.iter().rev() {
        for it in j["items"].as_array().into_iter().flatten() {
            let k = fmt::s(&it["key"]);
            let prev = items.get(&k).cloned();
            if it["status"] == "skipped" && prev.as_ref().map(|p| p.1 == "done").unwrap_or(false) {
                continue;
            }
            if !items.contains_key(&k) {
                order.push(k.clone());
            }
            let title = it["title"].as_str().filter(|s| !s.is_empty()).map(String::from).or(prev.map(|p| p.0)).unwrap_or_else(|| k.clone());
            items.insert(k, (title, fmt::s(&it["status"]), fmt::s(&it["error"])));
        }
    }
    let failed_job = my_jobs.iter().find(|j| j["done"].as_bool().unwrap_or(false) && j["items"].as_array().map(|a| a.iter().any(|i| i["status"] == "failed")).unwrap_or(false)).map(|j| fmt::s(&j["id"]));
    let done = items.values().filter(|i| i.1 == "done").count();
    let last_course = order.iter().map(|k| k.split(':').collect::<Vec<_>>()).find(|p| p[0] != "file" && p[0] != "url" && p.len() > 1).map(|p| p[1].to_string());
    let title = nb["title"].as_str().filter(|s| !s.is_empty()).unwrap_or("Untitled notebook").to_string();
    head(app, ui, pane, &title, vec![dashboard_crumb(), ("Gemini Notebooks".into(), Some("#/notebooks".into()))], None);
    h1(ui, pane, &title);
    let mut parts = vec![vec![(nb["sources_count"].as_i64().unwrap_or(0).to_string(), true), (" sources".into(), false)]];
    if done > 0 {
        parts.push(vec![(done.to_string(), true), (" from Canvas".into(), false)]);
    }
    let left: usize = running.iter().map(|j| j["items"].as_array().map(|a| a.iter().filter(|i| !["done", "failed", "skipped"].contains(&i["status"].as_str().unwrap_or(""))).count()).unwrap_or(0)).sum();
    let pills = if running.is_empty() { vec![] } else { vec![(Pill::Warn, format!("Uploading {left} left"))] };
    detail_meta(ui, parts, pills);
    ui.add_space(-22.0);
    crate::views::actions(ui, |ui| {
        if w::primary(ui, "Open in Gemini ↗").clicked() {
            app.open_external(&format!("https://notebooklm.google.com/notebook/{nb_id}"));
        }
        if w::button(ui, "Add sources").clicked() {
            let q = last_course.as_ref().map(|c| format!("?c={c}")).unwrap_or_default();
            crate::nav::go(app, &format!("#/notebooks/{nb_id}/add{q}"));
        }
        if let Some(jid) = &failed_job {
            if w::button(ui, "Retry failed").clicked() {
                if let Ok(job) = app.svc.nb_retry(jid) {
                    on_job(app, job);
                }
            }
        }
    });
    if items.is_empty() {
        w::empty(ui, "No sources added from Canvas yet.");
        return Ok(());
    }
    w::h2(ui, "Sources from Canvas");
    list(ui, |ui| {
        for (i, k) in order.iter().enumerate() {
            let (title, status, error) = &items[k];
            let (p, label) = status_pill(status);
            row(app, ui, pane, RowSpec {
                title: title.clone(),
                meta: (!error.is_empty() && status != "skipped").then(|| error.clone()),
                right: vec![(vec![(p, label)], String::new())],
                first: i == 0,
                ..Default::default()
            });
        }
    });
    Ok(())
}

fn visible(b: &Builder, it: &Item) -> bool {
    (b.filter == "all" || it.kind == b.filter) && (b.q.is_empty() || it.title.to_lowercase().contains(&b.q.to_lowercase()))
}

pub fn builder(app: &mut App, ui: &mut Ui, pane: Pane, nb_id: Option<&String>, query: &BTreeMap<String, String>) -> Result<(), Need> {
    let courses = app.d.need0("courses")?;
    let past = app.d.need0("past_courses").map(|v| (*v).clone()).unwrap_or(json!([]));
    let notebooks = app.d.need0("notebooks").ok();
    let links = app.d.need0("nb_links")?;
    if google_gate(app, ui, pane, if nb_id.is_some() { "Add sources" } else { "New notebook" }, "Connect NotebookLM first, then pick what to add.")? {
        return Ok(());
    }
    let cid = query.get("c").cloned().or_else(|| courses.as_array().and_then(|a| a.first()).map(|c| fmt::id(&c["id"])));
    let target = nb_id.and_then(|id| notebooks.as_ref().and_then(|n| n.as_array().and_then(|a| a.iter().find(|x| fmt::s(&x["id"]) == *id).cloned())));
    let mut in_nb: HashMap<String, HashSet<String>> = HashMap::new();
    for l in links.as_array().into_iter().flatten().filter(|l| l["status"] == "done") {
        in_nb.entry(fmt::s(&l["key"])).or_default().insert(fmt::s(&l["notebook_id"]));
    }
    let groups = match &cid {
        Some(c) => source_groups(app, c)?,
        None => Vec::new(),
    };
    let code = match &cid {
        Some(c) => {
            let info = crate::views::course_info(app, c)?;
            info.c["course_code"].as_str().filter(|x| !x.is_empty()).or(info.c["name"].as_str()).unwrap_or("").to_string()
        }
        None => String::new(),
    };
    let m = query.get("m").cloned().unwrap_or_default();
    let already = |key: &str| nb_id.map(|n| in_nb.get(key).map(|s| s.contains(n)).unwrap_or(false)).unwrap_or(false);
    // New builder session: reset selections and default title.
    let sid = format!("{}|{}|{m}", nb_id.cloned().unwrap_or_else(|| "new".into()), cid.clone().unwrap_or_default());
    if app.nb.builder.id.as_deref() != Some(sid.as_str()) {
        let b = &mut app.nb.builder;
        *b = Builder { id: Some(sid), filter: "all".into(), titles: std::mem::take(&mut b.titles), ..Default::default() };
        for g in groups.iter().filter(|g| m == "all" || g.id == m) {
            for it in &g.items {
                // a whole course can have dozens of recordings: only preselected for one module
                if let Some(k) = &it.key {
                    if (m != "all" || it.kind != "recording") && !already(k) {
                        b.selected.insert(k.clone());
                    }
                }
            }
        }
        let module = groups.iter().find(|g| g.id == m);
        b.title = match module {
            Some(g) => format!("{code} · {}", g.name),
            None if m == "all" => format!("{code} study notebook"),
            None => format!("{code} notebook"),
        };
    }
    for g in &groups {
        for it in &g.items {
            if let Some(k) = &it.key {
                app.nb.builder.titles.insert(k.clone(), it.title.clone());
            }
        }
    }
    let existing = target.as_ref().and_then(|t| t["sources_count"].as_i64()).unwrap_or(0) as usize;
    let tk = t();
    let mut crumbs = vec![dashboard_crumb(), ("Gemini Notebooks".into(), Some("#/notebooks".into()))];
    if let (Some(tg), Some(id)) = (&target, nb_id) {
        crumbs.push((fmt::s(&tg["title"]), Some(format!("#/notebooks/{id}"))));
    }
    let title = match nb_id {
        Some(_) => format!("Add sources to “{}”", target.as_ref().map(|t| fmt::s(&t["title"])).unwrap_or_else(|| "notebook".into())),
        None => "New notebook".into(),
    };
    head(app, ui, pane, &title, crumbs, None);
    h1(ui, pane, &title);
    // .builder-form: title and course
    ui.add_space(16.0);
    let mut course_pick: Option<String> = None;
    w::hwrap(ui, vec2(14.0, 10.0), |ui| {
        if nb_id.is_none() {
            ui.vertical(|ui| {
                ui.set_width(282.0);
                w::text_line(ui, "Title", Ts::muted(12.0));
                ui.add_space(4.0);
                let mut title = app.nb.builder.title.clone();
                if w::input(ui, Id::new("nb-title"), &mut title, "", w::InputOpts { width: Some(282.0), ..Default::default() }).changed() {
                    app.nb.builder.title = title;
                }
            });
        }
        ui.vertical(|ui| {
            ui.set_width(282.0);
            w::text_line(ui, "Course", Ts::muted(12.0));
            ui.add_space(4.0);
            let mut opts: Vec<(String, String)> = courses.as_array().into_iter().flatten().map(|c| (fmt::id(&c["id"]), c["course_code"].as_str().filter(|x| !x.is_empty()).or(c["name"].as_str()).unwrap_or("").to_string())).collect();
            if past.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
                opts.push((String::new(), "§Past courses".into()));
                for c in past.as_array().into_iter().flatten() {
                    let name = c["course_code"].as_str().filter(|x| !x.is_empty()).or(c["name"].as_str()).unwrap_or("").to_string();
                    let term = c["term"]["name"].as_str().map(|t| format!(" ({t})")).unwrap_or_default();
                    opts.push((fmt::id(&c["id"]), format!("{name}{term}")));
                }
            }
            course_pick = w::select(ui, Id::new("nb-course"), cid.as_deref().unwrap_or(""), &opts, 282.0, 14.0);
        });
    });
    if let Some(c) = course_pick {
        let path = crate::route::path_of(&app.main.want);
        crate::nav::go(app, &format!("#{path}?c={c}"));
        return Ok(());
    }
    ui.add_space(10.0);
    // .toolbar: kind chips, and the filter
    let mut chip_pick = None;
    w::hwrap(ui, vec2(6.0, 8.0), |ui| {
        for (k, label) in [("all", "All"), ("file", "Files"), ("page", "Pages"), ("assignment", "Assignments"), ("discussion", "Discussions"), ("recording", "Recordings")] {
            if w::chip(ui, label, app.nb.builder.filter == k).clicked() {
                chip_pick = Some(k.to_string());
            }
        }
        ui.add_space(12.0);
        let id = Id::new("nb-q");
        if std::mem::take(&mut app.settings.focus_filter) {
            ui.memory_mut(|m| m.request_focus(id));
        }
        let mut q = app.nb.builder.q.clone();
        if w::input(ui, id, &mut q, "Filter…", w::InputOpts { width: Some(220.0), pad: vec2(8.0, 6.0), ..Default::default() }).changed() {
            app.nb.builder.q = q;
        }
    });
    if let Some(k) = chip_pick {
        app.nb.builder.filter = k;
    }
    ui.add_space(4.0);
    if groups.is_empty() {
        w::empty(ui, "Nothing in this course can be added yet.");
    }
    let mut toggles: Vec<(String, bool)> = Vec::new();
    let mut group_toggles: Vec<(Vec<String>, bool)> = Vec::new();
    for g in &groups {
        let selectable: Vec<String> = g.items.iter().filter(|it| it.key.as_ref().map(|k| !already(k)).unwrap_or(false)).map(|it| it.key.clone().unwrap()).collect();
        let n = selectable.iter().filter(|k| app.nb.builder.selected.contains(*k)).count();
        let any_visible = g.items.iter().any(|it| visible(&app.nb.builder, it)) || (g.recordings.is_some() && ["all", "recording"].contains(&app.nb.builder.filter.as_str()) && app.nb.builder.q.is_empty());
        if !any_visible {
            continue;
        }
        // h2.group-head with a pick-all checkbox
        ui.add_space(28.0);
        let wdt = ui.available_width();
        let (hr, _) = ui.allocate_exact_size(vec2(wdt, 19.5), Sense::hover());
        let ts = w::h2_ts();
        let name_g = lay(ui, &g.name, ts, Some(wdt * 0.7), true);
        let lab = Rect::from_min_size(hr.min, vec2(15.0 + 8.0 + name_g.size().x, 19.5));
        let resp = ui.interact(lab, Id::new(("pick-all", &g.id)), if selectable.is_empty() { Sense::hover() } else { Sense::click() });
        w::checkbox(ui, Rect::from_min_size(pos2(hr.min.x, hr.center().y - 7.5), vec2(15.0, 15.0)), !selectable.is_empty() && n == selectable.len(), n > 0 && n < selectable.len(), selectable.is_empty(), tk.accent, resp.hovered());
        ui.painter().galley(pos2(hr.min.x + 23.0, hr.min.y + ts.top_pad()), name_g, ts.color);
        if n > 0 {
            w::painter_text(ui, pos2(hr.max.x, hr.center().y), egui::Align2::RIGHT_CENTER, &format!("{n} selected"), ts);
        }
        if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            let on = n < selectable.len();
            // respect the current filter
            let keys: Vec<String> = g.items.iter().filter(|it| visible(&app.nb.builder, it)).filter_map(|it| it.key.clone()).filter(|k| !already(k)).collect();
            group_toggles.push((keys, on));
        }
        ui.add_space(8.0);
        if let Some(rs) = &g.recordings {
            recordings_note(app, ui, rs, cid.as_deref().unwrap_or(""));
        }
        let shown: Vec<&Item> = g.items.iter().filter(|it| visible(&app.nb.builder, it)).collect();
        if !shown.is_empty() {
            list(ui, |ui| {
                for (i, it) in shown.iter().enumerate() {
                    if let Some(t) = pick_row(app, ui, pane, it, &in_nb, nb_id, i == 0) {
                        toggles.push(t);
                    }
                }
            });
        } else if g.recordings.is_none() && g.items.is_empty() {
            list(ui, |ui| crate::views::empty_row(ui, "No items"));
        }
    }
    for (k, on) in toggles {
        if on {
            app.nb.builder.selected.insert(k);
        } else {
            app.nb.builder.selected.remove(&k);
        }
    }
    for (keys, on) in group_toggles {
        for k in keys {
            if on {
                app.nb.builder.selected.insert(k);
            } else {
                app.nb.builder.selected.remove(&k);
            }
        }
    }
    builder_bar(app, ui, nb_id, existing, cid.as_deref());
    Ok(())
}

/// label.row.pick: a checkbox row. Returns (key, checked) when toggled.
fn pick_row(app: &mut App, ui: &mut Ui, pane: Pane, it: &Item, in_nb: &HashMap<String, HashSet<String>>, nb_id: Option<&String>, first: bool) -> Option<(String, bool)> {
    let tk = t();
    let already = it.key.as_ref().map(|k| nb_id.map(|n| in_nb.get(k).map(|s| s.contains(n)).unwrap_or(false)).unwrap_or(false)).unwrap_or(false);
    let disabled = it.key.is_none() || already;
    let checked = already || it.key.as_ref().map(|k| app.nb.builder.selected.contains(k)).unwrap_or(false);
    let in_others = it.key.as_ref().map(|k| in_nb.get(k).map(|s| s.iter().filter(|n| Some(*n) != nb_id).count()).unwrap_or(0)).unwrap_or(0);
    let right = if already {
        vec![(vec![(Pill::Ok, "Added".to_string())], String::new())]
    } else if in_others > 0 {
        vec![(vec![(Pill::Plain, format!("In {in_others} notebook{}", if in_others > 1 { "s" } else { "" }))], String::new())]
    } else {
        vec![]
    };
    let key = it.key.clone().map(|k| format!("pick:{k}")).unwrap_or_else(|| format!("pick:{}", it.title));
    let top = ui.cursor().min;
    let resp = row(app, ui, pane, RowSpec {
        key: Some(key.clone()),
        kind: Some(format!("      {}", kind_label(&it.kind))),
        title: it.title.clone(),
        meta: if !it.why.is_empty() { Some(it.why.clone()) } else if !it.meta.is_empty() { Some(it.meta.clone()) } else { None },
        right,
        locked: disabled,
        first,
        ..Default::default()
    });
    let r = resp.rect;
    let _ = top;
    let resp = ui.interact(r, Id::new(("pick", &key)), if disabled { Sense::hover() } else { Sense::click() });
    if resp.hovered() && !disabled {
        ui.painter().rect_filled(Rect::from_min_max(pos2(r.min.x, r.min.y + if first { 0.0 } else { 1.0 }), r.max), 0.0, theme::alpha(tk.hover, 1.0));
    }
    w::checkbox(ui, Rect::from_min_size(pos2(r.min.x + 14.0, r.center().y - 7.5), vec2(15.0, 15.0)), checked, false, disabled, tk.accent, resp.hovered());
    let cursor = crate::panes::state(app, pane).cursor.as_deref() == Some(key.as_str());
    let toggle_key = cursor && std::mem::take(&mut app.settings.toggle_cursor);
    if !disabled && (resp.on_hover_cursor(CursorIcon::PointingHand).clicked() || toggle_key) {
        return it.key.clone().map(|k| (k, !checked));
    }
    None
}

fn recordings_note(app: &mut App, ui: &mut Ui, rs: &RecState, cid: &str) {
    let tk = t();
    let m = Ts::muted(13.0);
    ui.add_space(-2.0);
    let form = |app: &mut App, ui: &mut Ui, reset: bool| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let mut link = app.nb.pan_link.clone();
            if w::input(ui, Id::new("pan-folder"), &mut link, "Paste a Panopto folder link…", w::InputOpts { size: 13.0, width: Some(300.0), pad: vec2(8.0, 4.0), ..Default::default() }).changed() {
                app.nb.pan_link = link;
            }
            let busy = app.nb.pan_busy;
            if w::button_if(ui, "Use folder", false, busy).clicked() {
                let l = app.nb.pan_link.clone();
                set_folder(app, cid, &l);
            }
            if reset && w::linklike(ui, "use automatic", 13.0).clicked() {
                set_folder(app, cid, "");
            }
        });
    };
    match rs {
        RecState::Setup => {
            w::text_block(ui, "To add lecture recordings, set panopto_host in config.toml.", m);
        }
        RecState::Loading => {
            w::text_block(ui, "Loading recordings from Panopto…", m);
        }
        RecState::Signin | RecState::Error => {
            setup::when_signed_in(app, "panopto", OnDone::Panopto);
            if matches!(rs, RecState::Error) && setup::state(app, "panopto").phase == Phase::Idle {
                w::text_block(ui, "Couldn't load recordings from Panopto. ", m);
            }
            setup::signin_card(app, ui, "panopto", true);
        }
        RecState::Folder(f) if f.is_null() => {
            w::text_block(ui, "Couldn't find this course's Panopto folder. Open it on Panopto and paste its link:", m);
            ui.add_space(4.0);
            form(app, ui, false);
        }
        RecState::Folder(f) => {
            let chosen = f["via"] == "chosen";
            let mut change = false;
            ui.horizontal(|ui| {
                let g = w::Rich::new().add("From ", m).add(&fmt::s(&f["name"]), Ts::new(13.0, 700, tk.muted)).add(if chosen { " (chosen by you) · " } else { " · " }, m).lay(ui);
                let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                ui.painter().galley(r.min, g, tk.muted);
                if !app.nb.pan_open.contains(cid) {
                    change = w::linklike(ui, "change folder", 13.0).clicked();
                }
            });
            if change {
                app.nb.pan_open.insert(cid.to_string());
            }
            if app.nb.pan_open.contains(cid) {
                ui.add_space(4.0);
                form(app, ui, chosen);
            }
        }
    }
    ui.add_space(8.0);
}

fn set_folder(app: &mut App, cid: &str, link: &str) {
    app.nb.pan_busy = true;
    let (svc, c, l) = (app.svc.clone(), cid.to_string(), link.to_string());
    app.spawn(async move { svc.panopto_folder(&c, &l).await }, {
        let c = cid.to_string();
        move |app, r| {
            app.nb.pan_busy = false;
            match r {
                Err(e) => app.toast(e.message(), true),
                Ok(_) => {
                    app.nb.pan_open.remove(&c);
                    app.nb.pan_link.clear();
                    app.d.forget(&format!("recordings:{c}"));
                }
            }
        }
    });
}

/// .builder-bar: sticky at the bottom of the pane while the list is taller than it.
fn builder_bar(app: &mut App, ui: &mut Ui, nb_id: Option<&String>, existing: usize, cid: Option<&str>) {
    let tk = t();
    let b = &app.nb.builder;
    let n = b.selected.len();
    let total = existing + n;
    let wdt = ui.available_width();
    ui.add_space(24.0);
    let (natural, _) = ui.allocate_exact_size(vec2(wdt + 24.0, 57.0), Sense::hover());
    let natural = natural.translate(vec2(-12.0, 0.0));
    let visible_bottom = ui.clip_rect().max.y;
    let r = if natural.max.y > visible_bottom { natural.translate(vec2(0.0, visible_bottom - natural.max.y)) } else { natural };
    let layer = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Middle, Id::new("builder-bar")));
    let layer = layer.with_clip_rect(ui.clip_rect());
    let shadow = egui::Shadow { offset: [0, -6], blur: 20, spread: 0, color: egui::Color32::from_black_alpha(31) };
    layer.add(shadow.as_shape(r, cr(10.0)));
    layer.rect_filled(r, cr(10.0), tk.panel_solid);
    layer.rect_filled(r, cr(10.0), tk.panel);
    layer.rect_stroke(r, cr(10.0), Stroke::new(1.0, tk.line), StrokeKind::Inside);
    let mut rich = w::Rich::new();
    rich.push(&n.to_string(), Ts::new(13.0, 700, tk.text));
    rich.push(" selected ", Ts::new(13.0, 400, tk.text));
    rich.push(&format!("· {total} / {SOURCE_LIMIT} sources in notebook"), Ts::new(13.0, 400, if total > SOURCE_LIMIT { tk.bad } else { tk.text }));
    if let Some(e) = &b.error {
        rich.push(&format!(" · {e}"), Ts::new(13.0, 400, tk.bad));
    }
    let g = rich.elide(r.width() * 0.6).lay(ui);
    layer.galley(pos2(r.min.x + 14.0, r.center().y - g.size().y / 2.0), g, tk.text);
    let busy = b.busy;
    let label = if busy { "Working…".to_string() } else if nb_id.is_some() { format!("Add {n} source{}", if n == 1 { "" } else { "s" }) } else { "Create notebook".into() };
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(pos2(r.center().x, r.min.y), r.max - vec2(14.0, 0.0))).layout(egui::Layout::right_to_left(egui::Align::Center)).layer_id(egui::LayerId::new(egui::Order::Middle, Id::new("builder-bar"))));
    c.spacing_mut().item_spacing.x = 8.0;
    let submit = w::button_if(&mut c, &label, true, n == 0 || busy).clicked();
    let clear = n > 0 && w::button(&mut c, "Clear").clicked();
    if clear {
        app.nb.builder.selected.clear();
    }
    if submit && n > 0 && !busy {
        submit_builder(app, nb_id.cloned(), cid.map(String::from));
    }
}

fn submit_builder(app: &mut App, nb_id: Option<String>, cid: Option<String>) {
    let b = &mut app.nb.builder;
    b.busy = true;
    b.error = None;
    let items: Vec<Value> = b.selected.iter().map(|k| json!({"key": k, "title": b.titles.get(k)})).collect();
    let title = b.title.clone();
    let svc = app.svc.clone();
    let course: Option<i64> = cid.and_then(|c| c.parse().ok());
    app.spawn(
        async move {
            match nb_id {
                Some(id) => Ok(svc.nb_add(&id, &title, items)),
                None => svc.nb_create(&title, course, items).await,
            }
        },
        |app, r| {
            app.nb.builder.busy = false;
            match r {
                Ok(job) => {
                    let nb = fmt::s(&job["notebook_id"]);
                    on_job(app, job);
                    app.nb.builder.id = None;
                    crate::nav::go(app, &format!("#/notebooks/{nb}"));
                }
                Err(e) => app.nb.builder.error = Some(e.message()),
            }
        },
    );
}
