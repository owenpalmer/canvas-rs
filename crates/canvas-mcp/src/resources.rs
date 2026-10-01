//! Named Canvas resources the UI reads. Each is cached under "name:arg1:arg2".
//!
//! A resource is an async function (client, args) -> JSON data, with a TTL after which the
//! cached copy is refreshed in the background (the stale copy is still served instantly).
//! Resources that need more than the Canvas client (recordings, notebooks) come from the
//! engine's extension (engine::ResourceExt).

use chrono::{Duration, Utc};
use serde_json::{Value, json};

use crate::client::Client;
use crate::{Error, Result, params};

pub const MIN: f64 = 60.0;

/// The built-in resources' TTLs in seconds.
pub fn ttl(name: &str) -> Option<f64> {
    Some(match name {
        "self" => 24.0 * 60.0 * MIN,
        "colors" => 60.0 * MIN,
        "courses" => 5.0 * MIN, // every sync, so newly published courses show up
        "past_courses" => 24.0 * 60.0 * MIN,
        "planner" | "announcements" | "course_announcements" | "groups" | "submission" | "modules" | "pages" | "discussions" | "topic" => 5.0 * MIN,
        "inbox" | "conversation" => 3.0 * MIN,
        "page" | "front_page" => 30.0 * MIN,
        "files" => 15.0 * MIN,
        "file" => 60.0 * MIN,
        "syllabus" => 60.0 * MIN,
        "tabs" => 24.0 * 60.0 * MIN,
        _ => return None,
    })
}

/// Midnight UTC today plus some days, as an ISO time.
fn iso(delta_days: i64) -> String {
    let day = Utc::now().date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc();
    (day + Duration::days(delta_days)).format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
}

fn named(list: Value) -> Value {
    match list {
        Value::Array(a) => Value::Array(a.into_iter().filter(|x| x.get("name").is_some()).collect()),
        other => other,
    }
}

fn arg(args: &[String], i: usize) -> Result<&str> {
    args.get(i).map(|s| s.as_str()).ok_or_else(|| Error::Invalid("missing argument".into()))
}

pub async fn fetch(c: &Client, name: &str, args: &[String]) -> Result<Value> {
    Ok(match name {
        "self" => c.get("users/self", vec![]).await?,
        "colors" => c.get("users/self/colors", vec![]).await?.get("custom_colors").cloned().unwrap_or(json!({})),
        "courses" => named(
            c.get("courses", params![("include[]", "term"), ("include[]", "total_scores"), ("include[]", "favorites"), ("enrollment_state", "active"), ("per_page", 100)]).await?,
        ),
        "past_courses" => named(c.get("courses", params![("include[]", "term"), ("include[]", "total_scores"), ("enrollment_state", "completed"), ("per_page", 100)]).await?),
        "planner" => c.get("planner/items", params![("start_date", iso(-14)), ("end_date", iso(60)), ("per_page", 100)]).await?,
        "announcements" => {
            let courses = named(c.get("courses", params![("enrollment_state", "active"), ("per_page", 100)]).await?);
            let ids: Vec<String> = courses.as_array().into_iter().flatten().map(|x| x["id"].to_string()).collect();
            if ids.is_empty() {
                return Ok(json!([]));
            }
            let mut p: Vec<(String, String)> = ids.iter().map(|i| ("context_codes[]".to_string(), format!("course_{i}"))).collect();
            p.extend(params![("start_date", iso(-180)), ("end_date", iso(1)), ("per_page", 100)]);
            c.get("announcements", p).await?
        }
        // Explicit wide range: by default Canvas only returns the last 14 days.
        "course_announcements" => {
            c.get("announcements", params![("context_codes[]", format!("course_{}", arg(args, 0)?)), ("start_date", "2000-01-01"), ("end_date", iso(1)), ("per_page", 100)]).await?
        }
        "inbox" => c.get_limit("conversations", params![("scope", "inbox"), ("per_page", 50)], Some(50)).await?,
        "conversation" => c.get(&format!("conversations/{}", arg(args, 0)?), params![("auto_mark_as_read", "false")]).await?,
        // Assignment groups (with weights) containing assignments and your submissions.
        "groups" => {
            c.get(
                &format!("courses/{}/assignment_groups", arg(args, 0)?),
                params![("include[]", "assignments"), ("include[]", "submission"), ("include[]", "discussion_topic"), ("per_page", 100)],
            )
            .await?
        }
        "submission" => {
            c.get(
                &format!("courses/{}/assignments/{}/submissions/self", arg(args, 0)?, arg(args, 1)?),
                params![("include[]", "submission_comments"), ("include[]", "rubric_assessment")],
            )
            .await?
        }
        "modules" => c.get(&format!("courses/{}/modules", arg(args, 0)?), params![("include[]", "items"), ("include[]", "content_details"), ("per_page", 100)]).await?,
        "pages" => c.get(&format!("courses/{}/pages", arg(args, 0)?), params![("sort", "title"), ("per_page", 100)]).await?,
        "page" => c.get(&format!("courses/{}/pages/{}", arg(args, 0)?, arg(args, 1)?), vec![]).await?,
        // the course's home page, when its home is a page ("wiki")
        "front_page" => c.get(&format!("courses/{}/front_page", arg(args, 0)?), vec![]).await?,
        "files" => {
            let cid = arg(args, 0)?;
            let (fp, dp) = (format!("courses/{cid}/files"), format!("courses/{cid}/folders"));
            let (files, folders) =
                futures::try_join!(c.get(&fp, params![("sort", "updated_at"), ("order", "desc"), ("per_page", 100)]), c.get(&dp, params![("per_page", 100)]))?;
            json!({"files": files, "folders": folders})
        }
        "file" => c.get(&format!("files/{}", arg(args, 0)?), vec![]).await?,
        "discussions" => c.get(&format!("courses/{}/discussion_topics", arg(args, 0)?), params![("per_page", 100)]).await?,
        "topic" => {
            let (cid, tid) = (arg(args, 0)?, arg(args, 1)?);
            let (tp, vp) = (format!("courses/{cid}/discussion_topics/{tid}"), format!("courses/{cid}/discussion_topics/{tid}/view"));
            let (topic, view) = futures::try_join!(c.get(&tp, vec![]), c.get(&vp, vec![]))?;
            json!({"topic": topic, "view": view})
        }
        "syllabus" => {
            let course = c.get(&format!("courses/{}", arg(args, 0)?), params![("include[]", "syllabus_body")]).await?;
            match course.get("syllabus_body") {
                Some(Value::String(s)) if !s.is_empty() => Value::String(s.clone()),
                _ => Value::String(String::new()),
            }
        }
        "tabs" => c.get(&format!("courses/{}/tabs", arg(args, 0)?), vec![]).await?,
        _ => return Err(Error::NotFound(format!("unknown resource {name}"))),
    })
}
