//! Fake Canvas client with sample data, for developing the UI without a real session.
//!
//! Enable with CANVAS_DEMO=1 (the app's --demo also points the cache at a separate demo folder).

use std::time::Duration;

use chrono::{Local, Timelike, Utc};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{Value, json};

use crate::client::{Download, Params};
use crate::{Error, Result};

pub const BASE: &str = "https://canvas.example.edu";

/// An ISO time `days` from today at hour:minute local time, in UTC ("…Z").
pub fn at(days: f64, hour: u32, minute: u32) -> String {
    let now = Local::now();
    let base = now.with_hour(hour).and_then(|d| d.with_minute(minute)).and_then(|d| d.with_second(0)).and_then(|d| d.with_nanosecond(0)).unwrap_or(now);
    let t = base.fixed_offset() + chrono::Duration::milliseconds((days * 86_400_000.0).round() as i64);
    t.with_timezone(&Utc).format("%Y-%m-%dT%H:%M:%SZ").to_string()
}
fn at_(days: f64) -> String {
    at(days, 23, 59)
}

fn assignment(cid: i64, aid: i64, name: &str, due: Option<String>, pts: f64, sub: Option<Value>, desc: Option<&str>) -> Value {
    let description = desc.map(String::from).unwrap_or_else(|| {
        format!(
            "<p>Complete <strong>{name}</strong>. See the <a href=\"{BASE}/courses/{cid}/pages/syllabus-notes\">course notes</a>.</p><p><img src=\"/courses/{cid}/files/9001/preview\" alt=\"diagram\"></p>"
        )
    });
    let rubric = if pts != 0.0 {
        json!([{"id": "r1", "description": "Correctness", "points": pts * 0.7}, {"id": "r2", "description": "Style", "long_description": "Readable, commented code.", "points": pts * 0.3}])
    } else {
        Value::Null
    };
    json!({
        "id": aid, "name": name, "due_at": due, "points_possible": pts, "submission_types": ["online_upload"],
        "html_url": format!("{BASE}/courses/{cid}/assignments/{aid}"),
        "description": description,
        "submission": sub.unwrap_or_else(|| json!({"workflow_state": "unsubmitted", "missing": false})),
        "rubric": rubric,
    })
}

pub struct Fixtures {
    pub courses: Value,
    pub past_courses: Value,
    pub groups: Value,   // {cid: [groups]}
    pub modules: Value,  // {cid: [modules]}
    pub pages: Value,    // {cid: [pages]}
    pub files: Value,    // {cid: {files, folders}}
    pub announcements: Value,
    pub discussions: Value, // {cid: [topics]}
    pub conversations: Value,
    pub recordings: Value,
}

fn int(v: f64) -> Value {
    if v.fract() == 0.0 { json!(v as i64) } else { json!(v) }
}

pub static FIX: Lazy<Fixtures> = Lazy::new(|| {
    let courses = json!([
        {"id": 101, "name": "Data Structures and Algorithms", "course_code": "CSE 373", "is_favorite": true,
         "enrollments": [{"type": "student", "computed_current_score": 91.4, "computed_current_grade": "A-"}], "term": {"name": "Autumn 2026"}},
        {"id": 102, "name": "Introduction to Linear Algebra", "course_code": "MATH 208", "is_favorite": true, "default_view": "wiki",
         "enrollments": [{"type": "student", "computed_current_score": 84.25, "computed_current_grade": null}], "term": {"name": "Autumn 2026"}},
        {"id": 103, "name": "Writing in the Sciences", "course_code": "ENGL 298", "is_favorite": false,
         "enrollments": [{"type": "student", "computed_current_score": null}], "term": {"name": "Autumn 2026"}},
    ]);
    let past_courses = json!([
        {"id": 91, "name": "Foundations of Computing I", "course_code": "CSE 311", "term": {"name": "Spring 2026", "start_at": "2026-03-30T00:00:00Z"},
         "enrollments": [{"type": "student", "computed_current_score": 88.0, "computed_current_grade": "B+"}]},
        {"id": 92, "name": "Calculus III", "course_code": "MATH 126", "term": {"name": "Spring 2026", "start_at": "2026-03-30T00:00:00Z"},
         "enrollments": [{"type": "student", "computed_current_score": 93.1}]},
        {"id": 93, "name": "Intro to Programming II", "course_code": "CSE 123", "term": {"name": "Winter 2026", "start_at": "2026-01-05T00:00:00Z"},
         "enrollments": [{"type": "student", "computed_current_score": 96.4, "computed_current_grade": "A"}]},
    ]);
    let a = |cid, aid, name: &str, due: Option<String>, pts: f64, sub: Option<Value>| {
        let mut v = assignment(cid, aid, name, due, pts, sub, None);
        v["points_possible"] = int(pts);
        if let Some(r) = v["rubric"].as_array_mut() {
            for c in r {
                let p = c["points"].as_f64().unwrap_or(0.0);
                c["points"] = json!(p);
            }
        }
        v
    };
    let groups = json!({
        "101": [
            {"id": 1, "name": "Projects", "group_weight": 50, "assignments": [
                a(101, 5001, "P1: Deques", Some(at_(-10.0)), 40.0, Some(json!({"workflow_state": "graded", "score": 38, "submitted_at": at_(-11.0)}))),
                a(101, 5002, "P2: Heaps and Priority Queues", Some(at_(2.0)), 40.0, None),
                a(101, 5003, "P3: Graph Search", Some(at_(16.0)), 40.0, None),
            ]},
            {"id": 2, "name": "Exercises", "group_weight": 30, "assignments": [
                a(101, 5010, "EX1: Asymptotic Analysis", Some(at_(-6.0)), 10.0, Some(json!({"workflow_state": "graded", "score": 9, "submitted_at": at_(-6.2)}))),
                a(101, 5011, "EX2: Recurrences", Some(at(0.0, 22, 59)), 10.0, Some(json!({"workflow_state": "submitted", "submitted_at": at_(-0.2), "late": false}))),
                a(101, 5012, "EX3: Hashing", Some(at_(5.0)), 10.0, None),
            ]},
            {"id": 3, "name": "Exams", "group_weight": 20, "assignments": [a(101, 5020, "Midterm", Some(at(20.0, 10, 30)), 100.0, None)]},
        ],
        "102": [
            {"id": 4, "name": "Homework", "group_weight": 0, "assignments": [
                a(102, 6001, "Homework 1", Some(at_(-3.0)), 20.0, Some(json!({"workflow_state": "unsubmitted", "missing": true}))),
                a(102, 6002, "Homework 2", Some(at(1.0, 17, 0)), 20.0, None),
                a(102, 6003, "Homework 3", Some(at(8.0, 17, 0)), 20.0, None),
            ]},
        ],
        "91": [{"id": 9, "name": "Problem sets", "group_weight": 0, "assignments": [
            a(91, 9101, "PS1: Propositional logic", Some(at_(-150.0)), 30.0, Some(json!({"workflow_state": "graded", "score": 27, "submitted_at": at_(-151.0)})))]}],
        "103": [
            {"id": 5, "name": "Essays", "group_weight": 0, "assignments": [
                a(103, 7001, "Reading response", None, 5.0, None),
                a(103, 7002, "Lab report draft", Some(at_(3.0)), 25.0, None),
            ]},
        ],
    });
    let modules = json!({
        "101": [
            {"id": 11, "name": "Week 1: Introduction", "state": "completed", "items": [
                {"id": 1, "type": "SubHeader", "title": "Readings", "indent": 0},
                {"id": 2, "type": "Page", "title": "Course notes", "page_url": "syllabus-notes", "indent": 1, "completion_requirement": {"completed": true}},
                {"id": 3, "type": "File", "title": "lecture01.pdf", "content_id": 9001, "indent": 1},
                {"id": 4, "type": "ExternalUrl", "title": "Visualgo", "external_url": "https://visualgo.net", "indent": 1},
                {"id": 5, "type": "Assignment", "title": "P1: Deques", "content_id": 5001, "indent": 0, "content_details": {"due_at": at_(-10.0), "points_possible": 40}},
            ]},
            {"id": 12, "name": "Week 2: Heaps", "state": "started", "items": [
                {"id": 6, "type": "Page", "title": "Heaps overview", "page_url": "heaps-overview", "indent": 0},
                {"id": 7, "type": "Assignment", "title": "P2: Heaps and Priority Queues", "content_id": 5002, "indent": 0, "content_details": {"due_at": at_(2.0), "points_possible": 40}},
                {"id": 8, "type": "Discussion", "title": "Week 2 discussion: amortized analysis", "content_id": 8001, "indent": 0},
                {"id": 10, "type": "ExternalTool", "title": "Lecture 6 recording", "indent": 0,
                 "external_url": "https://demo.hosted.panopto.com/Panopto/Pages/Viewer.aspx?id=11111111-0000-0000-0000-000000000001"},
                {"id": 9, "type": "Quiz", "title": "Quiz 2", "html_url": format!("{BASE}/courses/101/quizzes/12"), "indent": 0, "content_details": {"locked_for_user": true}},
            ]},
        ],
        "102": [{"id": 21, "name": "Chapter 1", "state": "started", "items": [
            {"id": 20, "type": "Assignment", "title": "Homework 2", "content_id": 6002, "indent": 0, "content_details": {"due_at": at(1.0, 17, 0), "points_possible": 20}}]}],
        "103": [],
        "91": [{"id": 91, "name": "Week 1: Logic", "state": "completed", "items": [
            {"id": 910, "type": "Page", "title": "Logic cheat sheet", "page_url": "logic-cheat-sheet", "indent": 0},
            {"id": 911, "type": "Assignment", "title": "PS1: Propositional logic", "content_id": 9101, "indent": 0}]}],
    });
    let pages = json!({
        "101": [
            {"url": "syllabus-notes", "title": "Course notes", "updated_at": at(-12.0, 9, 59), "front_page": true,
             "body": "<h2>Welcome</h2><p>Office hours are <span style=\"color:#000000\">Mon 2–3pm</span>.</p><ul><li>Project spec: <a href=\"/courses/101/assignments/5002\">P2</a></li><li><a href=\"https://docs.python.org\">Python docs</a></li></ul><iframe src=\"https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ\" width=\"560\" height=\"315\"></iframe><script>alert(1)</script><img src=x onerror=alert(1)>"},
            {"url": "heaps-overview", "title": "Heaps overview", "updated_at": at(-2.0, 9, 59),
             "body": "<p>A heap is a complete binary tree with the heap property.</p><pre><code>def percolate_up(i): ...</code></pre><table><tr><th>Op</th><th>Cost</th></tr><tr><td>insert</td><td>O(log n)</td></tr></table>"},
        ],
        "102": [{"url": "front", "title": "Welcome to MATH 208", "updated_at": at(-20.0, 9, 59), "front_page": true, "body": "<p>Textbook: Lay, <em>Linear Algebra</em>.</p>"}],
        "103": [],
        "91": [{"url": "logic-cheat-sheet", "title": "Logic cheat sheet", "updated_at": at(-160.0, 9, 59), "body": "<p>p → q ≡ ¬p ∨ q</p>"}],
    });
    let files = json!({
        "101": {"files": [
            {"id": 9001, "display_name": "lecture01.pdf", "size": 482113, "content-type": "application/pdf", "updated_at": at_(-12.0), "folder_id": 1},
            {"id": 9002, "display_name": "lecture02.pdf", "size": 530002, "content-type": "application/pdf", "updated_at": at_(-5.0), "folder_id": 1},
            {"id": 9003, "display_name": "p2-starter.zip", "size": 22130, "content-type": "application/zip", "updated_at": at_(-4.0), "folder_id": 2},
            {"id": 9004, "display_name": "heap-diagram.png", "size": 1202, "content-type": "image/png", "updated_at": at_(-2.0), "folder_id": 2},
        ], "folders": [{"id": 1, "full_name": "course files/Lectures"}, {"id": 2, "full_name": "course files/Projects"}, {"id": 3, "full_name": "course files"}]},
        "102": {"files": [], "folders": []},
        "103": {"files": [], "folders": []},
    });
    let announcements = json!([
        {"id": 8101, "context_code": "course_101", "title": "P2 released", "posted_at": at(-1.0, 10, 59), "user_name": "Prof. Rivera",
         "message": "<p>Project 2 is out. Start early! Starter code is in <a href=\"/courses/101/files/9003\">p2-starter.zip</a>.</p>", "html_url": format!("{BASE}/courses/101/discussion_topics/8101")},
        {"id": 8102, "context_code": "course_102", "title": "Quiz section moved to Thursday", "posted_at": at(-3.0, 15, 59), "user_name": "TA Kim",
         "message": "<p>This week only, quiz section meets Thursday in room 110.</p>", "html_url": format!("{BASE}/courses/102/discussion_topics/8102")},
    ]);
    let discussions = json!({
        "101": [{"id": 8001, "title": "Week 2 discussion: amortized analysis", "discussion_subentry_count": 3, "unread_count": 2, "last_reply_at": at_(-0.5),
               "message": "<p>Explain amortized analysis in your own words, with an example.</p>", "posted_at": at(-4.0, 9, 59), "user_name": "Prof. Rivera",
               "html_url": format!("{BASE}/courses/101/discussion_topics/8001"), "assignment": {"due_at": at(4.0, 12, 0)}}],
        "102": [], "103": [],
    });
    let conversations = json!([
        {"id": 301, "subject": "Question about P1 regrade", "workflow_state": "unread", "last_message": "Sure, I took another look and updated your score.",
         "last_message_at": at(-0.3, 14, 59), "context_name": "CSE 373", "participants": [{"id": 1, "name": "You"}, {"id": 2, "name": "Prof. Rivera"}],
         "messages": [{"author_id": 2, "created_at": at(-0.3, 14, 59), "body": "Sure, I took another look and updated your score."},
                      {"author_id": 1, "created_at": at(-1.0, 9, 59), "body": "Hi, could you re-check test 4 on P1?\n\nThanks!"}]},
        {"id": 302, "subject": "Study group", "workflow_state": "read", "last_message": "Library 3rd floor at 6?",
         "last_message_at": at(-2.0, 18, 59), "context_name": "MATH 208", "participants": [{"id": 1, "name": "You"}, {"id": 3, "name": "Sam Lee"}],
         "messages": [{"author_id": 3, "created_at": at(-2.0, 18, 59), "body": "Library 3rd floor at 6?"}]},
    ]);
    let folder = ("aaaa0000-0000-0000-0000-000000000373", "Autumn 2026 - Data Structures and Algorithms");
    let recs: Vec<Value> = [("CSE 373 Lecture 6 - Heaps", 3010, true), ("CSE 373 Lecture 5 - Hashing", 2950, true), ("CSE 373 Lecture 4 - Recurrences", 3100, true), ("Section 2 walkthrough", 1500, false)]
        .iter()
        .enumerate()
        .map(|(i, (t, d, cap))| {
            let i = i + 1;
            json!({"id": format!("11111111-0000-0000-0000-00000000000{i}"), "title": t, "duration": d, "start": at(-(i as f64) * 2.0, 10, 30),
                   "has_captions": cap, "folder_id": folder.0, "folder_name": folder.1})
        })
        .collect();
    Fixtures { courses, past_courses, groups, modules, pages, files, announcements, discussions, conversations, recordings: Value::Array(recs) }
});

/// A small, real PDF of lecture-like pages (Helvetica, so no fonts embedded), for the viewer.
pub fn pdf(title: &str, pages: usize) -> Vec<u8> {
    let esc = |s: &str| s.replace('\\', "\\\\").replace('(', "\\(").replace(')', "\\)");
    let mut objs: Vec<String> = vec!["<< /Type /Catalog /Pages 2 0 R >>".into(), String::new(), "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into()];
    let mut kids = Vec::new();
    for n in 1..=pages {
        let mut lines = vec![format!("Slide {n}: {}", if n == 1 { "Overview".to_string() } else { format!("Part {}", n - 1) })];
        for i in 1..9 {
            lines.push(format!("{i}. Amortized analysis bounds the average cost per operation, point {n}.{i}."));
        }
        let mut text = format!("BT /F1 26 Tf 60 700 Td ({}) Tj ET BT /F1 18 Tf 60 650 Td ({}) Tj ET ", esc(title), esc(&lines[0]));
        text += &lines[1..].iter().enumerate().map(|(i, line)| format!("BT /F1 13 Tf 72 {} Td ({}) Tj ET", 610 - 30 * i as i64, esc(line))).collect::<Vec<_>>().join(" ");
        text += &format!(" 0.2 0.43 0.71 rg 60 90 492 6 re f BT /F1 10 Tf 60 70 Td (CSE 373 - page {n} of {pages}) Tj ET");
        objs.push(format!("<< /Length {} >>\nstream\n{text}\nendstream", text.len()));
        objs.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
            objs.len()
        ));
        kids.push(format!("{} 0 R", objs.len()));
    }
    objs[1] = format!("<< /Type /Pages /Kids [{}] /Count {pages} >>", kids.join(" "));
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{body}\nendobj\n", i + 1).bytes());
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).bytes());
    for o in offsets {
        out.extend(format!("{o:010} 00000 n \n").bytes());
    }
    out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).bytes());
    out
}

/// 1x1 PNG
pub const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00,
    0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0x00, 0x05, 0x00, 0x02, 0x01, 0xa5, 0xf3, 0xb3, 0xc8,
    0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

fn codes(params: &Params) -> Vec<String> {
    params.iter().filter(|(k, _)| k == "context_codes[]").map(|(_, v)| v.clone()).collect()
}

fn strip(v: &Value, key: &str) -> Value {
    let mut v = v.clone();
    if let Some(o) = v.as_object_mut() {
        o.remove(key);
    }
    v
}

fn find_by_id<'a>(list: &'a Value, id: &str) -> Option<&'a Value> {
    list.as_array()?.iter().find(|x| x["id"].to_string() == id)
}

pub struct DemoClient;

static ROUTES: Lazy<Vec<(Regex, &'static str)>> = Lazy::new(|| {
    [
        (r"users/self", "self"),
        (r"users/self/colors", "colors"),
        (r"courses", "courses"),
        (r"planner/items", "planner"),
        (r"announcements", "announcements"),
        (r"conversations", "conversations"),
        (r"conversations/(\d+)", "conversation"),
        (r"courses/(\d+)/assignment_groups", "groups"),
        (r"courses/(\d+)/assignments/(\d+)/submissions/self", "submission"),
        (r"courses/(\d+)/modules", "modules"),
        (r"courses/(\d+)/pages", "pages"),
        (r"courses/(\d+)/pages/(.+)", "page"),
        (r"courses/(\d+)/front_page", "front_page"),
        (r"courses/(\d+)/files", "files"),
        (r"courses/(\d+)/folders", "folders"),
        (r"files/(\d+)", "file"),
        (r"courses/(\d+)/discussion_topics", "discussions"),
        (r"courses/(\d+)/discussion_topics/(\d+)", "topic"),
        (r"courses/(\d+)/discussion_topics/(\d+)/view", "view"),
        (r"courses/(\d+)", "course"),
        (r"courses/(\d+)/tabs", "tabs"),
    ]
    .into_iter()
    .map(|(p, n)| (Regex::new(&format!("^(?:{p})$")).unwrap(), n))
    .collect()
});

pub fn planner() -> Value {
    let f = &*FIX;
    let mut items = Vec::new();
    for c in f.courses.as_array().unwrap() {
        let cid = c["id"].as_i64().unwrap();
        let code = c["course_code"].as_str().unwrap();
        for g in f.groups[cid.to_string()].as_array().into_iter().flatten() {
            for a in g["assignments"].as_array().unwrap() {
                if a["due_at"].is_null() {
                    continue;
                }
                let s = &a["submission"];
                items.push(json!({
                    "course_id": cid, "context_name": code, "plannable_type": "assignment", "plannable_id": a["id"],
                    "plannable_date": a["due_at"], "plannable": {"title": a["name"], "points_possible": a["points_possible"]},
                    "html_url": format!("/courses/{cid}/assignments/{}", a["id"]),
                    "submissions": {"submitted": s.get("submitted_at").map(|v| !v.is_null()).unwrap_or(false), "graded": s["workflow_state"] == "graded", "missing": s.get("missing").cloned().unwrap_or(json!(false))},
                }));
            }
        }
    }
    items.push(json!({"course_id": 101, "context_name": "CSE 373", "plannable_type": "discussion_topic", "plannable_id": 8001,
                      "plannable_date": at(4.0, 12, 0), "plannable": {"title": "Week 2 discussion: amortized analysis"},
                      "html_url": "/courses/101/discussion_topics/8001", "submissions": {"submitted": false}}));
    Value::Array(items)
}

impl DemoClient {
    pub fn new() -> DemoClient {
        DemoClient
    }

    fn find_assignment(c: &str, a: &str) -> Option<Value> {
        FIX.groups[c].as_array()?.iter().flat_map(|g| g["assignments"].as_array().cloned().unwrap_or_default()).find(|x| x["id"].to_string() == a)
    }

    pub async fn get(&self, path: &str, params: &Params) -> Result<Value> {
        tokio::time::sleep(Duration::from_millis(250)).await; // pretend network latency, so caching is visible
        let p = path.trim_matches('/');
        let f = &*FIX;
        let param = |k: &str| params.iter().find(|(pk, _)| pk == k).map(|(_, v)| v.as_str());
        let no = || Error::NotFound(format!("demo: no fixture for {path}"));
        for (re, name) in ROUTES.iter() {
            let Some(m) = re.captures(p) else { continue };
            let g = |i: usize| m.get(i).map(|x| x.as_str()).unwrap_or("");
            let v = match *name {
                "self" => json!({"id": 1, "name": "Demo Student", "short_name": "Demo"}),
                "colors" => json!({"custom_colors": {"course_101": "#2f6db5", "course_102": "#3d8b5a"}}),
                "courses" => if param("enrollment_state") == Some("completed") { f.past_courses.clone() } else { f.courses.clone() },
                "planner" => planner(),
                "announcements" => {
                    let codes = codes(params);
                    Value::Array(f.announcements.as_array().unwrap().iter().filter(|a| codes.iter().any(|c| a["context_code"] == c.as_str())).cloned().collect())
                }
                "conversations" => Value::Array(f.conversations.as_array().unwrap().iter().map(|c| strip(c, "messages")).collect()),
                "conversation" => find_by_id(&f.conversations, g(1)).cloned().ok_or_else(no)?,
                "groups" => f.groups.get(g(1)).cloned().unwrap_or(json!([])),
                "submission" => {
                    let mut s = Self::find_assignment(g(1), g(2)).ok_or_else(no)?["submission"].clone();
                    s["submission_comments"] = if g(2) == "5001" {
                        json!([{"author_name": "TA Kim", "created_at": at(-5.0, 12, 59), "comment": "Nice work on the resize logic."}])
                    } else {
                        json!([])
                    };
                    s
                }
                "modules" => f.modules.get(g(1)).cloned().unwrap_or(json!([])),
                "pages" => Value::Array(f.pages.get(g(1)).and_then(|v| v.as_array()).into_iter().flatten().map(|p| strip(p, "body")).collect()),
                "page" => f.pages.get(g(1)).and_then(|v| v.as_array()).into_iter().flatten().find(|x| x["url"] == g(2)).cloned().ok_or_else(no)?,
                "front_page" => f.pages.get(g(1)).and_then(|v| v.as_array()).into_iter().flatten().find(|x| x["front_page"] == true).cloned().ok_or_else(no)?,
                "files" => f.files.get(g(1)).map(|v| v["files"].clone()).unwrap_or(json!([])),
                "folders" => f.files.get(g(1)).map(|v| v["folders"].clone()).unwrap_or(json!([])),
                "file" => {
                    let mut x = f.files.as_object().unwrap().values().flat_map(|v| v["files"].as_array().cloned().unwrap_or_default()).find(|x| x["id"].to_string() == g(1)).ok_or_else(no)?;
                    x["url"] = json!(format!("{BASE}/files/{}/download", g(1)));
                    x
                }
                "discussions" => f.discussions.get(g(1)).cloned().unwrap_or(json!([])),
                "topic" => {
                    let list = f.discussions.get(g(1)).and_then(|v| v.as_array()).cloned().unwrap_or_default();
                    list.iter().chain(f.announcements.as_array().unwrap()).find(|x| x["id"].to_string() == g(2)).cloned().ok_or_else(no)?
                }
                "view" => {
                    if g(2) == "8001" {
                        json!({"participants": [{"id": 2, "display_name": "Prof. Rivera"}, {"id": 4, "display_name": "Alex Chen"}, {"id": 5, "display_name": "Jordan P."}],
                               "view": [{"user_id": 4, "created_at": at(-2.0, 20, 59), "message": "<p>It's the average cost per operation over a worst-case sequence.</p>",
                                         "replies": [{"user_id": 2, "created_at": at(-1.0, 9, 59), "message": "<p>Good. Now give an example!</p>"}]},
                                        {"user_id": 5, "created_at": at(-0.5, 11, 59), "message": "<p>ArrayList resizing: doubling makes append O(1) amortized.</p>"}]})
                    } else {
                        json!({"participants": [{"id": 2, "display_name": "Prof. Rivera"}, {"id": 4, "display_name": "Alex Chen"}, {"id": 5, "display_name": "Jordan P."}], "view": []})
                    }
                }
                "course" => {
                    let all: Vec<Value> = f.courses.as_array().unwrap().iter().chain(f.past_courses.as_array().unwrap()).cloned().collect();
                    let mut c = all.into_iter().find(|x| x["id"].to_string() == g(1)).ok_or_else(no)?;
                    c["syllabus_body"] = json!(if g(1) == "101" { "<h2>Grading</h2><p>Projects 50%, exercises 30%, exams 20%.</p>" } else { "" });
                    c
                }
                "tabs" => {
                    let ids: &[&str] = if g(1) == "101" {
                        &["home", "modules", "assignments", "grades", "announcements", "discussions", "pages", "files", "syllabus"]
                    } else {
                        &["home", "modules", "assignments", "grades", "announcements"]
                    };
                    Value::Array(ids.iter().map(|t| json!({"id": t})).collect())
                }
                _ => return Err(no()),
            };
            return Ok(v);
        }
        Err(no())
    }

    pub async fn download(&self, url: &str) -> Result<Download> {
        tokio::time::sleep(Duration::from_millis(300)).await;
        if url.contains("9004") || url.contains("preview") {
            return Ok(Download { bytes: PNG.to_vec(), content_type: "image/png".into() });
        }
        let (title, n) = if url.contains("9002") { ("Lecture 2: Heaps", 12) } else { ("Lecture 1: Amortized analysis", 6) };
        Ok(Download { bytes: pdf(title, n), content_type: "application/pdf".into() })
    }
}

// --- fake Panopto ----------------------------------------------------------------------------------
pub const PANOPTO_HOST: &str = "demo.hosted.panopto.com";

pub fn demo_recording_url(rid: &str) -> String {
    format!("https://{PANOPTO_HOST}/Panopto/Pages/Viewer.aspx?id={rid}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_is_well_formed() {
        let bytes = pdf("T", 2);
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.starts_with("%PDF-1.4"));
        assert!(s.contains("/Count 2"));
        assert!(s.trim_end().ends_with("%%EOF"));
    }

    #[tokio::test]
    async fn routes() {
        let c = DemoClient::new();
        let me = c.get("users/self", &vec![]).await.unwrap();
        assert_eq!(me["name"], "Demo Student");
        let past = c.get("courses", &crate::params![("enrollment_state", "completed")]).await.unwrap();
        assert_eq!(past.as_array().unwrap().len(), 3);
        let anns = c.get("announcements", &crate::params![("context_codes[]", "course_101")]).await.unwrap();
        assert_eq!(anns.as_array().unwrap().len(), 1);
        let page = c.get("courses/101/pages/heaps-overview", &vec![]).await.unwrap();
        assert_eq!(page["title"], "Heaps overview");
        let sub = c.get("courses/101/assignments/5001/submissions/self", &vec![]).await.unwrap();
        assert_eq!(sub["score"], 38);
        assert!(c.get("nope", &vec![]).await.is_err());
    }
}
