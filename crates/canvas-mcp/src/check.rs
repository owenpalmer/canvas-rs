//! Command-line checks: that your Firefox Canvas session works with the API (canvas-check), where
//! Canvas cookies are in every profile (--diagnose), that Panopto works (panopto), and calling
//! every MCP tool (smoke). Prints cookie names only, never values.

use std::io::Write;
use std::sync::Arc;
use std::time::SystemTime;

use regex::Regex;
use serde_json::{Value, json};

use crate::client::host_of;
use crate::cookies::{self, RESTORE_HINT, find_profile, firefox_roots, load_cookies, session_restore_enabled};
use crate::{config, mcp};

fn names(set: impl IntoIterator<Item = String>) -> String {
    let mut v: Vec<String> = set.into_iter().collect();
    v.sort();
    v.dedup();
    if v.is_empty() { "(none)".into() } else { v.join(", ") }
}

fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>, depth: usize) {
    if depth > 4 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk(&p, out, depth + 1);
        } else if p.file_name().and_then(|n| n.to_str()) == Some("cookies.sqlite") {
            out.push(p);
        }
    }
}

/// For every Firefox profile, show where Canvas cookies are (counts and names only).
pub fn diagnose() {
    let host = host_of(&config::canvas_url());
    let profile = find_profile();
    let restore = profile.as_ref().ok().and_then(|p| session_restore_enabled(Some(p)));
    let label = match restore {
        Some(true) => "on",
        Some(false) => "OFF",
        None => "unknown (prefs.js unreadable)",
    };
    match &profile {
        Ok(p) => println!("Default profile picked: {}", p.display()),
        Err(e) => println!("Default profile picked: none ({e})"),
    }
    println!("Session restore (\"Open previous windows and tabs\"): {label}\n");
    for root in firefox_roots() {
        if !root.exists() {
            continue;
        }
        let mut dbs = Vec::new();
        walk(&root, &mut dbs, 0);
        for db in dbs {
            let profile = db.parent().unwrap().to_path_buf();
            let age_h = db.metadata().and_then(|m| m.modified()).ok().and_then(|t| SystemTime::now().duration_since(t).ok()).map(|d| d.as_secs_f64() / 3600.0).unwrap_or(0.0);
            let running = ["lock", ".parentlock", "parent.lock"].iter().any(|f| profile.join(f).exists());
            let sql = match cookies::from_sqlite(&profile, &[&host]) {
                Ok(c) => names(c.into_iter().map(|c| c.name)),
                Err(e) => format!("<error: {e}>"),
            };
            let sess = match cookies::from_sessionstore(&profile, &[&host]) {
                Ok(c) => names(c.into_iter().map(|c| c.name)),
                Err(e) => format!("<error: {e}>"),
            };
            println!("{}", profile.display());
            println!("  cookies.sqlite last written {age_h:.1} h ago; Firefox using it now: {}", if running { "True" } else { "False" });
            println!("  Canvas cookies in cookies.sqlite: {sql}");
            println!("  Canvas cookies in session store:  {sess}");
        }
    }
}

/// Reading cookies for a site needs your say-so, once (config permissions).
pub fn ask_permission(host: &str) -> bool {
    if config::allowed(host) {
        return true;
    }
    println!("canvas-mcp signs in to {host} with your Firefox login: it reads Firefox's cookies for {host}");
    println!("(and no other site) and sends them only to it.");
    print!("Allow it to use your {host} login? [y/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    let _ = std::io::stdin().read_line(&mut answer);
    if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
        println!("Not allowed; nothing was read.");
        return false;
    }
    config::allow(host);
    true
}

pub async fn check_main(args: &[String]) -> i32 {
    if args.first().map(|a| a == "panopto").unwrap_or(false) {
        return panopto_check(&args[1..]).await;
    }
    if let Err(e) = config::require_canvas_url() {
        println!("{e}");
        return 1;
    }
    let host = host_of(&config::canvas_url());
    if !ask_permission(&host) {
        return 1;
    }
    if args.iter().any(|a| a == "--diagnose") {
        diagnose();
        return 0;
    }
    let profile = match find_profile() {
        Ok(p) => p,
        Err(e) => {
            println!("{e}");
            return 1;
        }
    };
    let cookies = match load_cookies(&host, Some(&profile)) {
        Ok(c) => c,
        Err(e) => {
            println!("{e}");
            return 1;
        }
    };
    println!("Profile: {}", profile.display());
    println!("Canvas cookies found: {}", names(cookies.keys().cloned()));
    if cookies.is_empty() {
        if session_restore_enabled(Some(&profile)) == Some(false) {
            println!("{RESTORE_HINT}");
        } else {
            println!("Log into Canvas in Firefox, then run this again.");
        }
        return 1;
    }
    let csrf = cookies.get("_csrf_token").map(|t| percent_encoding::percent_decode_str(t).decode_utf8_lossy().into_owned()).unwrap_or_default();
    let resp = reqwest::Client::builder()
        .user_agent(crate::util::USER_AGENT)
        .build()
        .expect("http client")
        .get(format!("{}/api/v1/users/self", config::canvas_url()))
        .header("Accept", "application/json")
        .header("X-CSRF-Token", csrf)
        .header("Cookie", cookies::cookie_header(&cookies))
        .send()
        .await;
    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            println!("API call failed: {e}");
            return 1;
        }
    };
    if resp.status().as_u16() != 200 {
        println!("API call failed: HTTP {}. Session may be expired; reload Canvas in Firefox and retry.", resp.status().as_u16());
        return 1;
    }
    let user = crate::client::parse_json(&resp.text().await.unwrap_or_default()).unwrap_or(json!({}));
    println!("API works. Logged in as {} (id {}).", crate::util::s(&user["name"]), user["id"]);
    0
}

const PANOPTO_USAGE: &str = "Check that your Firefox Panopto session can read a recording's info and captions.
Prints cookie names only (never values), the recording's title, and the first caption lines.

Usage: canvas-check panopto <viewer URL or recording id> [--folders]
  --folders  also list the recording's folder and the folders you can see (titles and counts only)";

async fn panopto_check(args: &[String]) -> i32 {
    let default_host = config::panopto_host();
    if default_host.is_empty() {
        println!("Panopto isn't set up: add panopto_host = \"your-school.hosted.panopto.com\" to {}", config::config_path().display());
        return 1;
    }
    let arg = args.iter().find(|a| !a.starts_with("--")).cloned().unwrap_or_default();
    let re = Regex::new(r"(?i)[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}").unwrap();
    let Some(m) = re.find(&arg) else {
        println!("{PANOPTO_USAGE}");
        return 2;
    };
    let rid = m.as_str().to_string();
    let host = Some(host_of(&arg)).filter(|h| !h.is_empty()).unwrap_or(default_host);
    if !ask_permission(&host) {
        return 1;
    }
    let cookies = load_cookies(&host, None).unwrap_or_default();
    println!("Panopto cookies for {host}: {}", names(cookies.keys().cloned()));
    if cookies.is_empty() {
        println!("No Panopto session. Open the recording in Firefox (sign in if asked), then run this again.");
        return 1;
    }
    let client = crate::panopto::PanoptoClient::new(&host);
    match client.delivery_info(&rid).await {
        Ok(info) => {
            println!("\nDeliveryInfo: OK");
            println!("  title: {}", crate::util::s(&info["title"]));
            println!("  duration: {} min", (info["duration"].as_f64().unwrap_or(0.0) / 60.0).round());
            println!("  has captions: {}", info["has_captions"]);
            let mut langs: Vec<i64> = info["languages"].as_array().into_iter().flatten().filter_map(|v| v.as_i64()).collect();
            if !langs.contains(&0) {
                langs.push(0);
            }
            for lang in langs {
                let text = client.captions_srt(&rid, lang).await.unwrap_or_default();
                let cues: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty() && !l.trim().chars().all(|c| c.is_ascii_digit()) && !l.contains("-->")).collect();
                println!("\nCaptions (language {lang}): {} caption lines", cues.len());
                for l in cues.iter().take(5) {
                    println!("  | {}", l.chars().take(100).collect::<String>());
                }
                if !cues.is_empty() {
                    break;
                }
            }
            if args.iter().any(|a| a == "--folders") {
                let fid = crate::util::s(&info["folder_id"]);
                println!("\nRecording's folder: {} ({fid})", crate::util::s(&info["folder_name"]));
                match client.folder_sessions(&fid).await {
                    Ok(items) => {
                        println!("GetSessions: {} recordings", items.len());
                        for s in items.iter().take(5) {
                            println!("  - {}  ({} min, {})", crate::util::s(&s["title"]), (s["duration"].as_f64().unwrap_or(0.0) / 60.0).round(), crate::util::s(&s["start"]));
                        }
                    }
                    Err(e) => println!("GetSessions failed: {e}"),
                }
            }
            0
        }
        Err(e) => {
            println!("\nDeliveryInfo failed: {e}");
            1
        }
    }
}

/// Call each MCP tool and report OK/FAIL (verbose prints output). Real Canvas unless CANVAS_DEMO=1.
pub async fn smoke(server: Arc<mcp::Server>, verbose: bool) -> i32 {
    let mut failures = 0;
    let mut call = async |name: &str, tool: &str, args: Value| -> Option<String> {
        match server.call_tool(tool, &args).await {
            Ok(text) => {
                println!("OK    {name}  ({} chars)", text.chars().count());
                if verbose {
                    let head: String = text.chars().take(600).collect();
                    println!("      {}{}", head.replace('\n', "\n      "), if text.chars().count() > 600 { "\n      ..." } else { "" });
                }
                Some(text)
            }
            Err(e) => {
                println!("FAIL  {name}: {}: {e}", e.type_name());
                failures += 1;
                None
            }
        }
    };
    call("whoami", "whoami", json!({})).await;
    let courses = call("list_courses", "list_courses", json!({})).await;
    call("upcoming", "upcoming", json!({"days": 14})).await;
    call("announcements (all courses)", "announcements", json!({})).await;
    call("inbox", "inbox", json!({"limit": 3})).await;
    let courses: Vec<Value> = courses.and_then(|t| serde_json::from_str(&t).ok()).and_then(|v: Value| v.as_array().cloned()).unwrap_or_default();
    let Some(cid) = courses.first().map(|c| c["id"].clone()) else {
        println!("No active courses; skipping per-course tools.");
        return if failures > 0 { 1 } else { 0 };
    };
    println!("--- per-course tools on course {cid}");
    let assignments = call("list_assignments", "list_assignments", json!({"course_id": cid})).await;
    if let Some(first) = assignments.and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v.get(0).cloned()) {
        call("get_assignment", "get_assignment", json!({"course_id": cid, "assignment_id": first["id"]})).await;
    }
    call("grades", "grades", json!({"course_id": cid})).await;
    call("list_modules", "list_modules", json!({"course_id": cid})).await;
    call("list_pages", "list_pages", json!({"course_id": cid})).await;
    call("syllabus", "syllabus", json!({"course_id": cid})).await;
    call("list_files", "list_files", json!({"course_id": cid})).await;
    call("discussions", "discussions", json!({"course_id": cid})).await;
    call("api_get", "api_get", json!({"path": format!("courses/{cid}/tabs")})).await;
    if failures > 0 { 1 } else { 0 }
}
