//! Read-only Canvas LMS MCP server, authenticated with your Firefox session.
//!
//! Reads go through the same cache as the desktop app (engine.rs, ~/.canvas-mcp/cache.db), so
//! anything either one has fetched is shared. Data older than its TTL is refreshed first (waiting a
//! few seconds); if Canvas can't be reached, the cached copy is returned with a note saying how old it is.
//!
//! The protocol is JSON-RPC 2.0 over stdio, one message per line.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::engine::Engine;
use crate::files::{file_path, safe_name};
use crate::markdown::md;
use crate::store::now;
use crate::util::{pick, pretty, s, truthy};
use crate::{Error, Result, config, params};

const PLANNER_DAYS: i64 = 60; // the cached planner covers 14 days back to 60 days ahead (resources.rs)
const INSTRUCTIONS: &str = "Read-only access to the user's Canvas LMS (courses, assignments, grades, announcements, \
modules, pages, files, inbox). Course IDs come from list_courses. Results may start with a \
note that Canvas couldn't be reached and the data is cached from some time ago; mention that \
when it matters. If a tool reports the session expired, ask the user to open Canvas in \
Firefox and log in, then retry.";

pub struct Server {
    engine: Arc<Engine>,
}

/// One tool call's reads: notes when any of them had to fall back to stale data.
struct Call<'a> {
    engine: &'a Arc<Engine>,
    stale: Mutex<Vec<(String, f64)>>,
}

impl Call<'_> {
    async fn res(&self, name: &str, args: &[String]) -> Result<Value> {
        let (data, fetched_at, why) = self.engine.read(name, args, Duration::from_secs(5)).await?;
        if let Some(why) = why {
            self.stale.lock().unwrap().push((why, fetched_at));
        }
        Ok(data)
    }
    async fn list(&self, name: &str, args: &[String]) -> Result<Vec<Value>> {
        Ok(self.res(name, args).await?.as_array().cloned().unwrap_or_default())
    }
}

fn ago(ts: f64) -> String {
    let s = now() - ts;
    if s < 3600.0 {
        format!("{} min ago", ((s / 60.0).round() as i64).max(1))
    } else if s < 86400.0 {
        format!("{} h ago", (s / 3600.0).round() as i64)
    } else {
        format!("{} days ago", (s / 86400.0).round() as i64)
    }
}

pub fn when(iso: &Value) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(iso.as_str()?).ok().map(|d| d.with_timezone(&Utc))
}

fn obj(m: Map<String, Value>) -> Value {
    Value::Object(m)
}

fn merge(mut a: Map<String, Value>, b: Map<String, Value>) -> Map<String, Value> {
    a.extend(b);
    a
}

fn int_arg(args: &Value, k: &str) -> Result<i64> {
    let v = &args[k];
    v.as_i64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())).ok_or_else(|| Error::Invalid(format!("{k} must be an integer")))
}

fn opt_int(args: &Value, k: &str) -> Result<Option<i64>> {
    if args[k].is_null() { Ok(None) } else { int_arg(args, k).map(Some) }
}

fn opt_str(args: &Value, k: &str) -> Option<String> {
    args[k].as_str().filter(|s| !s.is_empty()).map(String::from)
}

fn submitted(sub: &Value) -> bool {
    truthy(&sub["submitted_at"]) || ["submitted", "graded", "pending_review"].contains(&sub["workflow_state"].as_str().unwrap_or(""))
}

const BUCKETS: &[&str] = &["past", "overdue", "undated", "ungraded", "unsubmitted", "upcoming", "future"];

fn in_bucket(a: &Value, bucket: &str, now: DateTime<Utc>) -> bool {
    let sub = if a["submission"].is_object() { a["submission"].clone() } else { json!({}) };
    let due = when(&a["due_at"]);
    let types: Vec<String> = a["submission_types"].as_array().into_iter().flatten().map(s).collect();
    let needs = !types.iter().any(|t| ["none", "on_paper", "not_graded"].contains(&t.as_str()));
    match bucket {
        "past" => due.map(|d| d < now).unwrap_or(false),
        "overdue" => due.map(|d| d < now).unwrap_or(false) && needs && !submitted(&sub),
        "undated" => due.is_none(),
        "ungraded" => submitted(&sub) && sub["workflow_state"] != "graded",
        "unsubmitted" => !submitted(&sub),
        "upcoming" => due.map(|d| now <= d && d <= now + chrono::Duration::days(7)).unwrap_or(false),
        "future" => due.map(|d| d >= now).unwrap_or(false),
        _ => false,
    }
}

impl Server {
    pub fn new(engine: Arc<Engine>) -> Server {
        Server { engine }
    }

    async fn find_course(&self, c: &Call<'_>, course_id: i64) -> Result<Value> {
        for name in ["courses", "past_courses"] {
            for x in c.list(name, &[]).await? {
                if x["id"].as_i64() == Some(course_id) {
                    return Ok(x);
                }
            }
        }
        Ok(json!({}))
    }

    async fn assignments_of(&self, c: &Call<'_>, course_id: i64) -> Result<Vec<Value>> {
        let groups = c.list("groups", &[course_id.to_string()]).await?;
        let mut items: Vec<Value> = groups.iter().flat_map(|g| g["assignments"].as_array().cloned().unwrap_or_default()).collect();
        items.sort_by(|a, b| {
            let ka = (a["due_at"].is_null(), s(&a["due_at"]));
            let kb = (b["due_at"].is_null(), s(&b["due_at"]));
            ka.cmp(&kb)
        });
        Ok(items)
    }

    async fn tool(&self, c: &Call<'_>, name: &str, args: &Value) -> Result<String> {
        let out = |v: Value| Ok(pretty(&v));
        let engine = &self.engine;
        match name {
            "whoami" => out(obj(pick(&c.res("self", &[]).await?, &["id", "name", "short_name", "primary_email", "login_id"]))),
            "list_courses" => {
                let mut courses = c.list("courses", &[]).await?;
                if args["include_past"].as_bool().unwrap_or(false) {
                    courses.extend(c.list("past_courses", &[]).await?);
                }
                let result: Vec<Value> = courses
                    .iter()
                    .map(|x| {
                        let enr = x["enrollments"].as_array().into_iter().flatten().find(|e| e["type"] == "student").cloned().unwrap_or(json!({}));
                        let mut m = pick(x, &["id", "name", "course_code"]);
                        m.insert("term".into(), x["term"]["name"].clone());
                        obj(merge(m, pick(&enr, &["computed_current_score", "computed_current_grade"])))
                    })
                    .collect();
                out(Value::Array(result))
            }
            "upcoming" => {
                let days = opt_int(args, "days")?.unwrap_or(14).min(PLANNER_DAYS);
                let now = Utc::now();
                let end = now + chrono::Duration::days(days);
                let base = engine.client.base();
                let mut result = Vec::new();
                for it in c.list("planner", &[]).await? {
                    let Some(due) = when(&it["plannable_date"]) else { continue };
                    if !(now <= due && due <= end) {
                        continue;
                    }
                    let p = &it["plannable"];
                    let subs = &it["submissions"];
                    let sub = |k: &str| if truthy(subs) { subs.get(k).cloned().unwrap_or(Value::Null) } else { Value::Null };
                    result.push(json!({
                        "course": it["context_name"], "course_id": it["course_id"], "type": it["plannable_type"], "id": it["plannable_id"],
                        "title": if truthy(&p["title"]) { p["title"].clone() } else { p["name"].clone() },
                        "due": it["plannable_date"], "points": p["points_possible"],
                        "submitted": sub("submitted"), "graded": sub("graded"), "missing": sub("missing"),
                        "url": if truthy(&it["html_url"]) { json!(format!("{base}{}", s(&it["html_url"]))) } else { Value::Null },
                    }));
                }
                out(Value::Array(result))
            }
            "list_assignments" => {
                let cid = int_arg(args, "course_id")?;
                let bucket = opt_str(args, "bucket");
                if let Some(b) = &bucket {
                    if !BUCKETS.contains(&b.as_str()) {
                        return Err(Error::Invalid(format!("bucket must be one of: {}", BUCKETS.join(", "))));
                    }
                }
                let now = Utc::now();
                let mut result = Vec::new();
                for a in self.assignments_of(c, cid).await? {
                    if bucket.as_deref().map(|b| !in_bucket(&a, b, now)).unwrap_or(false) {
                        continue;
                    }
                    let sub = if a["submission"].is_object() { a["submission"].clone() } else { json!({}) };
                    let mut m = pick(&a, &["id", "name", "due_at", "points_possible", "submission_types"]);
                    for (k, v) in pick(&sub, &["workflow_state", "score", "grade", "submitted_at", "late", "missing"]) {
                        m.insert(format!("submission_{k}"), v);
                    }
                    result.push(obj(m));
                }
                out(Value::Array(result))
            }
            "get_assignment" => {
                let (cid, aid) = (int_arg(args, "course_id")?, int_arg(args, "assignment_id")?);
                let a = self
                    .assignments_of(c, cid)
                    .await?
                    .into_iter()
                    .find(|x| x["id"].as_i64() == Some(aid))
                    .ok_or_else(|| Error::Invalid(format!("No assignment {aid} in course {cid} (see list_assignments)")))?;
                let sub = c.res("submission", &[cid.to_string(), aid.to_string()]).await?;
                let mut m = pick(&a, &["id", "name", "due_at", "unlock_at", "lock_at", "points_possible", "submission_types", "allowed_extensions", "html_url"]);
                m.insert("description".into(), json!(md(&s(&a["description"]))));
                if let Some(r) = a["rubric"].as_array().filter(|r| !r.is_empty()) {
                    m.insert("rubric".into(), Value::Array(r.iter().map(|x| obj(pick(x, &["description", "long_description", "points"]))).collect()));
                }
                m.insert("submission".into(), obj(pick(&sub, &["workflow_state", "score", "grade", "submitted_at", "late", "missing", "attempt"])));
                m.insert(
                    "comments".into(),
                    Value::Array(sub["submission_comments"].as_array().into_iter().flatten().map(|x| obj(pick(x, &["author_name", "created_at", "comment"]))).collect()),
                );
                out(obj(m))
            }
            "grades" => {
                let cid = int_arg(args, "course_id")?;
                let course = self.find_course(c, cid).await?;
                let enr = course["enrollments"].as_array().into_iter().flatten().find(|e| e["type"] == "student").cloned().unwrap_or(json!({}));
                let groups = c.list("groups", &[cid.to_string()]).await?;
                let gs: Vec<Value> = groups
                    .iter()
                    .map(|g| {
                        let mut m = pick(g, &["name", "group_weight"]);
                        let assignments: Vec<Value> = g["assignments"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|a| {
                                let mut x = Map::new();
                                x.insert("assignment".into(), a["name"].clone());
                                x.insert("assignment_id".into(), a["id"].clone());
                                x.insert("points_possible".into(), a["points_possible"].clone());
                                let sub = if a["submission"].is_object() { a["submission"].clone() } else { json!({}) };
                                obj(merge(x, pick(&sub, &["score", "grade", "workflow_state", "late", "missing", "excused"])))
                            })
                            .collect();
                        m.insert("assignments".into(), Value::Array(assignments));
                        obj(m)
                    })
                    .collect();
                out(json!({
                    "course_grade": obj(pick(&enr, &["computed_current_score", "computed_current_grade", "computed_final_score", "computed_final_grade"])),
                    "groups": gs,
                }))
            }
            "announcements" => {
                let cid = opt_int(args, "course_id")?;
                let days = opt_int(args, "days")?.unwrap_or(14);
                let items = match cid {
                    Some(cid) => c.list("course_announcements", &[cid.to_string()]).await?,
                    None => c.list("announcements", &[]).await?,
                };
                let cutoff = Utc::now() - chrono::Duration::days(days);
                let result: Vec<Value> = items
                    .iter()
                    .filter(|a| when(&a["posted_at"]).map(|p| p >= cutoff).unwrap_or(true))
                    .map(|a| {
                        let mut m = pick(a, &["id", "title", "posted_at", "user_name", "html_url"]);
                        let course: i64 = s(&a["context_code"]).split('_').nth(1).and_then(|x| x.parse().ok()).unwrap_or(0);
                        m.insert("course_id".into(), json!(course));
                        m.insert("message".into(), json!(md(&s(&a["message"]))));
                        obj(m)
                    })
                    .collect();
                out(Value::Array(result))
            }
            "list_modules" => {
                let cid = int_arg(args, "course_id")?;
                let mods = c.list("modules", &[cid.to_string()]).await?;
                out(Value::Array(
                    mods.iter()
                        .map(|m| {
                            let mut x = pick(m, &["id", "name", "state"]);
                            x.insert(
                                "items".into(),
                                Value::Array(m["items"].as_array().into_iter().flatten().map(|i| obj(pick(i, &["id", "title", "type", "content_id", "page_url", "external_url"]))).collect()),
                            );
                            obj(x)
                        })
                        .collect(),
                ))
            }
            "get_page" => {
                let cid = int_arg(args, "course_id")?;
                let mut page_url = opt_str(args, "page_url").ok_or_else(|| Error::Invalid("page_url is required".into()))?;
                if page_url == "front_page" {
                    let front = c.list("pages", &[cid.to_string()]).await?.into_iter().find(|p| p["front_page"].as_bool().unwrap_or(false));
                    match front {
                        None => {
                            let p = engine.client.get(&format!("courses/{cid}/front_page"), vec![]).await?;
                            return Ok(format!("# {}\n\n{}", s(&p["title"]), md(&s(&p["body"]))));
                        }
                        Some(f) => page_url = s(&f["url"]),
                    }
                }
                let p = c.res("page", &[cid.to_string(), page_url]).await?;
                Ok(format!("# {}\n\n{}", if p["title"].is_null() { "None".into() } else { s(&p["title"]) }, md(&s(&p["body"]))))
            }
            "list_pages" => {
                let cid = int_arg(args, "course_id")?;
                let mut pages = c.list("pages", &[cid.to_string()]).await?;
                if let Some(q) = opt_str(args, "search") {
                    let q = q.to_lowercase();
                    pages.retain(|p| s(&p["title"]).to_lowercase().contains(&q));
                }
                out(Value::Array(pages.iter().map(|p| obj(pick(p, &["title", "url", "updated_at"]))).collect()))
            }
            "syllabus" => {
                let cid = int_arg(args, "course_id")?;
                let body = c.res("syllabus", &[cid.to_string()]).await?;
                let course = self.find_course(c, cid).await?;
                let name = course["name"].as_str().filter(|n| !n.is_empty()).map(String::from).unwrap_or_else(|| format!("Course {cid}"));
                let text = md(&s(&body));
                Ok(format!("# {name} syllabus\n\n{}", if text.is_empty() { "(empty; check list_modules or list_files)".to_string() } else { text }))
            }
            "list_files" => {
                let cid = int_arg(args, "course_id")?;
                let mut files = c.res("files", &[cid.to_string()]).await?["files"].as_array().cloned().unwrap_or_default();
                if let Some(q) = opt_str(args, "search") {
                    let q = q.to_lowercase();
                    files.retain(|f| s(&f["display_name"]).to_lowercase().contains(&q));
                }
                out(Value::Array(files.iter().map(|f| obj(pick(f, &["id", "display_name", "size", "content-type", "updated_at"]))).collect()))
            }
            "download_file" => {
                let fid = int_arg(args, "file_id")?;
                let (src, meta) = file_path(engine, &fid.to_string(), None).await?; // shared on-disk cache: a file downloaded once is reused
                let name = meta["display_name"].as_str().filter(|n| !n.is_empty()).map(String::from).unwrap_or_else(|| format!("file_{fid}"));
                let dir = config::settings().download_dir;
                std::fs::create_dir_all(&dir)?;
                let path = dir.join(safe_name(&name));
                std::fs::copy(&src, &path)?;
                let size = std::fs::metadata(&path)?.len();
                out(json!({"path": path.to_string_lossy(), "size": size, "content_type": meta["content-type"]}))
            }
            "discussions" => {
                let cid = int_arg(args, "course_id")?;
                match opt_int(args, "topic_id")? {
                    None => {
                        let topics = c.list("discussions", &[cid.to_string()]).await?;
                        out(Value::Array(topics.iter().map(|t| obj(pick(t, &["id", "title", "posted_at", "due_at", "discussion_subentry_count"]))).collect()))
                    }
                    Some(tid) => {
                        let d = c.res("topic", &[cid.to_string(), tid.to_string()]).await?;
                        let (t, view) = (&d["topic"], &d["view"]);
                        let names: std::collections::HashMap<String, Value> =
                            view["participants"].as_array().into_iter().flatten().map(|p| (p["id"].to_string(), p["display_name"].clone())).collect();
                        fn entry(e: &Value, names: &std::collections::HashMap<String, Value>) -> Value {
                            let mut m = Map::new();
                            m.insert("author".into(), names.get(&e["user_id"].to_string()).cloned().unwrap_or(Value::Null));
                            m.insert("at".into(), e["created_at"].clone());
                            m.insert("message".into(), json!(md(&s(&e["message"]))));
                            if let Some(r) = e["replies"].as_array().filter(|r| !r.is_empty()) {
                                m.insert("replies".into(), Value::Array(r.iter().filter(|x| !x["deleted"].as_bool().unwrap_or(false)).map(|x| entry(x, names)).collect()));
                            }
                            Value::Object(m)
                        }
                        let mut m = pick(t, &["title", "posted_at", "due_at"]);
                        m.insert("prompt".into(), json!(md(&s(&t["message"]))));
                        m.insert(
                            "entries".into(),
                            Value::Array(view["view"].as_array().into_iter().flatten().filter(|e| !e["deleted"].as_bool().unwrap_or(false)).map(|e| entry(e, &names)).collect()),
                        );
                        out(obj(m))
                    }
                }
            }
            "inbox" => {
                let limit = opt_int(args, "limit")?.unwrap_or(20).max(0) as usize;
                let scope = opt_str(args, "scope").unwrap_or_else(|| "inbox".into());
                let convs: Vec<Value> = if scope == "inbox" && limit <= 50 {
                    c.list("inbox", &[]).await?.into_iter().take(limit).collect() // the cached inbox holds the latest 50
                } else {
                    engine.client.get_limit("conversations", params![("scope", scope), ("per_page", limit.min(100))], Some(limit)).await?.as_array().cloned().unwrap_or_default()
                };
                out(Value::Array(
                    convs
                        .iter()
                        .map(|x| {
                            let mut m = pick(x, &["id", "subject", "last_message_at", "last_message", "workflow_state", "context_name"]);
                            m.insert("participants".into(), Value::Array(x["participants"].as_array().into_iter().flatten().map(|p| p["name"].clone()).collect()));
                            obj(m)
                        })
                        .collect(),
                ))
            }
            "get_conversation" => {
                let id = int_arg(args, "conversation_id")?;
                let conv = c.res("conversation", &[id.to_string()]).await?;
                let names: std::collections::HashMap<String, Value> = conv["participants"].as_array().into_iter().flatten().map(|p| (p["id"].to_string(), p["name"].clone())).collect();
                let mut m = pick(&conv, &["subject", "context_name"]);
                let mut msgs: Vec<Value> = conv["messages"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|x| json!({"from": names.get(&x["author_id"].to_string()).cloned().unwrap_or(Value::Null), "at": x["created_at"], "body": x["body"]}))
                    .collect();
                msgs.reverse();
                m.insert("messages".into(), Value::Array(msgs));
                out(obj(m))
            }
            "api_get" => {
                let path = opt_str(args, "path").ok_or_else(|| Error::Invalid("path is required".into()))?;
                let mut p = Vec::new();
                if let Some(o) = args["params"].as_object() {
                    for (k, v) in o {
                        match v {
                            Value::Array(a) => p.extend(a.iter().map(|x| (k.clone(), s(x)))),
                            Value::Bool(b) => p.push((k.clone(), if *b { "true".into() } else { "false".into() })),
                            other => p.push((k.clone(), s(other))),
                        }
                    }
                }
                out(engine.client.get(&path, p).await?)
            }
            _ => Err(Error::Invalid(format!("Unknown tool: {name}"))),
        }
    }

    /// Run a tool; prefix its output with a note if any data it used was stale.
    pub async fn call_tool(&self, name: &str, args: &Value) -> Result<String> {
        let c = Call { engine: &self.engine, stale: Mutex::new(Vec::new()) };
        let text = self.tool(&c, name, args).await?;
        let notes = c.stale.into_inner().unwrap();
        if let Some((why, oldest)) = notes.iter().min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)) {
            return Ok(format!("Note: {why}, so this is cached data from {}.\n\n{text}", ago(*oldest)));
        }
        Ok(text)
    }

    async fn handle(&self, msg: Value) -> Option<Value> {
        let id = msg.get("id").cloned();
        let method = msg["method"].as_str().unwrap_or("");
        let ok = |result: Value| id.clone().map(|id| json!({"jsonrpc": "2.0", "id": id, "result": result}));
        let fail = |code: i64, message: String| id.clone().map(|id| json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}));
        match method {
            "initialize" => {
                let requested = msg["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18");
                let supported = ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];
                let version = if supported.contains(&requested) { requested } else { "2025-06-18" };
                ok(json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "canvas", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": INSTRUCTIONS,
                }))
            }
            "ping" => ok(json!({})),
            "tools/list" => ok(json!({"tools": tools()})),
            "tools/call" => {
                let name = msg["params"]["name"].as_str().unwrap_or("");
                if !tools().iter().any(|t| t["name"] == name) {
                    return fail(-32602, format!("Unknown tool: {name}"));
                }
                let args = msg["params"].get("arguments").cloned().unwrap_or(json!({}));
                match self.call_tool(name, &args).await {
                    Ok(text) => ok(json!({"content": [{"type": "text", "text": text}], "isError": false})),
                    Err(e) => ok(json!({"content": [{"type": "text", "text": format!("Error executing tool {name}: {e}")}], "isError": true})),
                }
            }
            "resources/list" => ok(json!({"resources": []})),
            "resources/templates/list" => ok(json!({"resourceTemplates": []})),
            "prompts/list" => ok(json!({"prompts": []})),
            m if m.starts_with("notifications/") => None,
            _ => fail(-32601, format!("Method not found: {method}")),
        }
    }

    /// Serve MCP over stdin/stdout until stdin closes.
    pub async fn run_stdio(self: Arc<Self>) {
        let out = Arc::new(tokio::sync::Mutex::new(tokio::io::stdout()));
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        let mut pending = tokio::task::JoinSet::new();
        while let Ok(Some(line)) = lines.next_line().await {
            while pending.try_join_next().is_some() {}
            if line.trim().is_empty() {
                continue;
            }
            let msg: Value = match serde_json::from_str(&line) {
                Ok(m) => m,
                Err(e) => {
                    let resp = json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("Parse error: {e}")}});
                    let mut o = out.lock().await;
                    let _ = o.write_all(format!("{resp}\n").as_bytes()).await;
                    let _ = o.flush().await;
                    continue;
                }
            };
            let (this, out) = (self.clone(), out.clone());
            pending.spawn(async move {
                let replies: Vec<Value> = match msg {
                    Value::Array(batch) => {
                        let mut r = Vec::new();
                        for m in batch {
                            if let Some(x) = this.handle(m).await {
                                r.push(x);
                            }
                        }
                        r
                    }
                    m => this.handle(m).await.into_iter().collect(),
                };
                let mut o = out.lock().await;
                for r in replies {
                    let _ = o.write_all(format!("{r}\n").as_bytes()).await;
                }
                let _ = o.flush().await;
            });
        }
        // stdin closed: finish answering what was asked.
        while pending.join_next().await.is_some() {}
    }
}

fn prop(t: &str, desc: &str) -> Value {
    json!({"type": t, "description": desc})
}

/// The tools, with their input schemas.
pub fn tools() -> Vec<Value> {
    let course = prop("integer", "Course ID (from list_courses)");
    let t = |name: &str, desc: &str, props: Value, required: &[&str]| {
        json!({"name": name, "description": desc, "inputSchema": {"type": "object", "properties": props, "required": required}})
    };
    vec![
        t("whoami", "The logged-in Canvas user.", json!({}), &[]),
        t(
            "list_courses",
            "List your courses with IDs and current grades. Set include_past=True for completed terms too.",
            json!({"include_past": {"type": "boolean", "default": false, "description": "Include completed terms"}}),
            &[],
        ),
        t(
            "upcoming",
            "Everything due or scheduled in the next `days` days (up to 60) across all courses\n(assignments, quizzes, discussions, calendar events), with whether you've submitted it.",
            json!({"days": {"type": "integer", "default": 14}}),
            &[],
        ),
        t(
            "list_assignments",
            "Assignments in a course with due dates, points, and your submission status/score.\nbucket (optional): past, overdue, undated, ungraded, unsubmitted, upcoming (next 7 days), future.",
            json!({"course_id": course, "bucket": {"type": ["string", "null"], "enum": ["past", "overdue", "undated", "ungraded", "unsubmitted", "upcoming", "future", null], "default": null}}),
            &["course_id"],
        ),
        t(
            "get_assignment",
            "Full assignment details: instructions (as markdown), rubric, due/lock dates, your submission and feedback.",
            json!({"course_id": course, "assignment_id": {"type": "integer"}}),
            &["course_id", "assignment_id"],
        ),
        t("grades", "Your current course grade, the assignment groups (with weights), and every assignment's score in a course.", json!({"course_id": course}), &["course_id"]),
        t(
            "announcements",
            "Announcements from the last `days` days (up to 180 across all courses), for one course or all active courses.",
            json!({"course_id": {"type": ["integer", "null"], "default": null}, "days": {"type": "integer", "default": 14}}),
            &[],
        ),
        t("list_modules", "A course's modules and their items (pages, files, assignments, links) in order.", json!({"course_id": course}), &["course_id"]),
        t(
            "get_page",
            "A course wiki page as markdown. page_url is the page's slug (from list_modules or list_pages), or 'front_page'.",
            json!({"course_id": course, "page_url": {"type": "string"}}),
            &["course_id", "page_url"],
        ),
        t(
            "list_pages",
            "List a course's wiki pages (optionally matching `search`).",
            json!({"course_id": course, "search": {"type": ["string", "null"], "default": null}}),
            &["course_id"],
        ),
        t("syllabus", "A course's syllabus as markdown.", json!({"course_id": course}), &["course_id"]),
        t(
            "list_files",
            "List files in a course, newest first (optionally matching `search`).",
            json!({"course_id": course, "search": {"type": ["string", "null"], "default": null}}),
            &["course_id"],
        ),
        t("download_file", "Download a Canvas file to the local downloads folder and return its path.", json!({"file_id": {"type": "integer"}}), &["file_id"]),
        t(
            "discussions",
            "Without topic_id: list a course's discussion topics. With topic_id: the prompt and all replies.",
            json!({"course_id": course, "topic_id": {"type": ["integer", "null"], "default": null}}),
            &["course_id"],
        ),
        t(
            "inbox",
            "Recent Canvas inbox conversations. scope: inbox, unread, starred, sent, archived.",
            json!({"limit": {"type": "integer", "default": 20}, "scope": {"type": "string", "default": "inbox"}}),
            &[],
        ),
        t("get_conversation", "All messages in one Canvas inbox conversation.", json!({"conversation_id": {"type": "integer"}}), &["conversation_id"]),
        t(
            "api_get",
            "Escape hatch: GET any Canvas REST API path under /api/v1 (e.g. 'courses/123/quizzes').\nAlways live (not cached). See https://canvas.instructure.com/doc/api/ for endpoints. Read-only.",
            json!({"path": {"type": "string"}, "params": {"type": ["object", "null"], "default": null}}),
            &["path"],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets() {
        let now = Utc::now();
        let past = (now - chrono::Duration::days(2)).to_rfc3339();
        let soon = (now + chrono::Duration::days(2)).to_rfc3339();
        let a = json!({"due_at": past, "submission_types": ["online_upload"], "submission": {"workflow_state": "unsubmitted"}});
        assert!(in_bucket(&a, "past", now) && in_bucket(&a, "overdue", now) && in_bucket(&a, "unsubmitted", now));
        let b = json!({"due_at": soon, "submission_types": ["on_paper"], "submission": {"submitted_at": "x", "workflow_state": "submitted"}});
        assert!(in_bucket(&b, "upcoming", now) && in_bucket(&b, "future", now) && in_bucket(&b, "ungraded", now) && !in_bucket(&b, "overdue", now));
        assert!(in_bucket(&json!({"due_at": null}), "undated", now));
    }

    #[test]
    fn tool_list() {
        let names: Vec<String> = tools().iter().map(|t| s(&t["name"])).collect();
        assert_eq!(names.len(), 17);
        assert!(names.contains(&"api_get".to_string()));
    }
}
