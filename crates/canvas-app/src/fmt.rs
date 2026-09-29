//! Formatting: dates the way the web UI showed them (en-US), sizes, scores, course colors.

use chrono::{DateTime, Datelike, Local, NaiveDate, Timelike};
use egui::Color32;
use serde_json::Value;

pub fn parse_iso(s: &str) -> Option<DateTime<Local>> {
    DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&Local))
}

pub fn iso(v: &Value) -> Option<DateTime<Local>> {
    parse_iso(v.as_str()?)
}

pub fn today() -> NaiveDate {
    Local::now().date_naive()
}

const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const WEEKDAYS_LONG: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const MONTHS_LONG: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// "Today", "Tomorrow", "Yesterday", else "Wed, Sep 30" (with the year when it's another year).
pub fn fmt_day_date(d: NaiveDate) -> String {
    let diff = (d - today()).num_days();
    match diff {
        0 => "Today".into(),
        1 => "Tomorrow".into(),
        -1 => "Yesterday".into(),
        _ => {
            let wd = WEEKDAYS[d.weekday().num_days_from_monday() as usize];
            let m = MONTHS[d.month0() as usize];
            if d.year() != today().year() { format!("{wd}, {m} {}, {}", d.day(), d.year()) } else { format!("{wd}, {m} {}", d.day()) }
        }
    }
}

pub fn fmt_day(d: &DateTime<Local>) -> String {
    fmt_day_date(d.date_naive())
}

/// "11:59 PM"
pub fn fmt_time(d: &DateTime<Local>) -> String {
    let (pm, h) = d.hour12();
    format!("{h}:{:02} {}", d.minute(), if pm { "PM" } else { "AM" })
}

/// "Wed, Sep 30, 11:59 PM" ("" for no date).
pub fn fmt_date(v: &Value) -> String {
    iso(v).map(|d| format!("{}, {}", fmt_day(&d), fmt_time(&d))).unwrap_or_default()
}

pub fn fmt_date_str(s: &str) -> String {
    parse_iso(s).map(|d| format!("{}, {}", fmt_day(&d), fmt_time(&d))).unwrap_or_default()
}

/// "Monday, September 28"
pub fn long_date(d: &DateTime<Local>) -> String {
    format!("{}, {} {}", WEEKDAYS_LONG[d.weekday().num_days_from_monday() as usize], MONTHS_LONG[d.month0() as usize], d.day())
}

/// "just now", "5m ago", "3h ago", "2d ago" for a Unix time in seconds.
pub fn ago(ts: f64) -> String {
    let s = canvas_mcp::store::now() - ts;
    if s < 45.0 {
        "just now".into()
    } else if s < 3600.0 {
        format!("{}m ago", (s / 60.0).round() as i64)
    } else if s < 86400.0 {
        format!("{}h ago", (s / 3600.0).round() as i64)
    } else {
        format!("{}d ago", (s / 86400.0).round() as i64)
    }
}

/// "482 KB", "1.2 MB"
pub fn fmt_size(v: &Value) -> String {
    let Some(n) = v.as_f64() else { return String::new() };
    if n < 1024.0 {
        format!("{} B", n as i64)
    } else if n < 1_048_576.0 {
        format!("{:.0} KB", n / 1024.0)
    } else {
        format!("{:.1} MB", n / 1_048_576.0)
    }
}

/// A score: "38", "91.4", "84.25"; "–" for none.
pub fn num(v: &Value) -> String {
    match v.as_f64() {
        None => "–".into(),
        Some(x) => num_f(x),
    }
}

pub fn num_f(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        return format!("{}", x as i64);
    }
    let s = format!("{x:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

pub const PALETTE: [u32; 8] = [0xb5462f, 0x2f6db5, 0x3d8b5a, 0x8a4fb0, 0xc07a1a, 0x1f8a8a, 0xb0406e, 0x5b6b7d];

pub fn hex_color(s: &str) -> Option<Color32> {
    let h = s.trim().trim_start_matches('#');
    let v = u32::from_str_radix(h, 16).ok()?;
    match h.len() {
        6 => Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)),
        3 => {
            let (r, g, b) = ((v >> 8) & 0xf, (v >> 4) & 0xf, v & 0xf);
            Some(Color32::from_rgb((r * 17) as u8, (g * 17) as u8, (b * 17) as u8))
        }
        _ => None,
    }
}

/// A course's color: yours from Canvas, else one from the palette by its id.
pub fn color_for(cid: &str, colors: Option<&Value>) -> Color32 {
    if let Some(c) = colors.and_then(|c| c.get(format!("course_{cid}"))).and_then(|v| v.as_str()).and_then(hex_color) {
        return c;
    }
    let tail: String = cid.chars().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect();
    let n: i64 = tail.parse().unwrap_or(0);
    let c = PALETTE[(n.unsigned_abs() as usize) % PALETTE.len()];
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

/// Canvas leaves posted_at null on some announcements (delayed or imported ones).
pub fn posted_at(a: &Value) -> String {
    for k in ["posted_at", "delayed_post_at", "created_at"] {
        if let Some(s) = a[k].as_str().filter(|s| !s.is_empty()) {
            return s.to_string();
        }
    }
    String::new()
}

/// A string field ("" when missing).
pub fn s(v: &Value) -> String {
    canvas_mcp::util::s(v)
}

/// The id of a JSON value as a string (numbers written out).
pub fn id(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(num_f(38.0), "38");
        assert_eq!(num_f(91.4), "91.4");
        assert_eq!(num_f(84.25), "84.25");
        assert_eq!(num_f(28.000000001), "28");
        assert_eq!(fmt_size(&serde_json::json!(482113)), "471 KB");
        assert_eq!(fmt_size(&serde_json::json!(1202)), "1 KB");
        assert_eq!(fmt_size(&serde_json::json!(530002)), "518 KB");
    }

    #[test]
    fn colors() {
        let colors = serde_json::json!({"course_101": "#2f6db5"});
        assert_eq!(color_for("101", Some(&colors)), Color32::from_rgb(0x2f, 0x6d, 0xb5));
        assert_eq!(color_for("103", None), Color32::from_rgb(0x5b, 0x6b, 0x7d)); // 103 % 8 == 7
    }
}
