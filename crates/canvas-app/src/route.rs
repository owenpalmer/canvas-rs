//! Places in the app, as the old hash URLs ("#/c/101/modules"), which also name viewer tabs,
//! recent pages and search results, so they're kept as the app's addresses.

use std::collections::BTreeMap;

use once_cell::sync::Lazy;
use regex::Regex;

#[derive(Clone, Debug, PartialEq)]
pub enum View {
    Dashboard,
    Inbox,
    Conversation(String),
    Modules(String),
    Assignments(String),
    Assignment(String, String),
    Grades(String),
    Announcements(String),
    Discussions(String),
    Topic(String, String),
    Pages(String),
    Page(String, String),
    Files(String),
    File(Option<String>, String),
    Syllabus(String),
    Welcome,
    Settings,
    Notebooks,
    NotebookNew,
    NotebookAdd(String),
    Notebook(String),
    Anki,
    AnkiImport,
    AnkiDeck(i64),
    NotFound,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Route {
    pub view: View,
    pub path: String,
    pub anchor: Option<String>,
    pub query: BTreeMap<String, String>,
    pub hash: String,
}

fn decode(s: &str) -> String {
    percent_encoding::percent_decode_str(s).decode_utf8_lossy().into_owned()
}

pub fn parse_query(qs: &str) -> BTreeMap<String, String> {
    qs.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (decode(&k.replace('+', " ")), decode(&v.replace('+', " ")))
        })
        .collect()
}

static ROUTES: Lazy<Vec<(Regex, &'static str)>> = Lazy::new(|| {
    [
        (r"^/$", "dashboard"),
        (r"^/inbox$", "inbox"),
        (r"^/inbox/(\d+)$", "conversation"),
        (r"^/c/(\d+)/?$", "modules"),
        (r"^/c/(\d+)/modules$", "modules"),
        (r"^/c/(\d+)/assignments$", "assignments"),
        (r"^/c/(\d+)/a/(\d+)$", "assignment"),
        (r"^/c/(\d+)/grades$", "grades"),
        (r"^/c/(\d+)/announcements$", "announcements"),
        (r"^/c/(\d+)/discussions$", "discussions"),
        (r"^/c/(\d+)/d/(\d+)$", "topic"),
        (r"^/c/(\d+)/pages$", "pages"),
        (r"^/c/(\d+)/p/(.+)$", "page"),
        (r"^/c/(\d+)/files$", "files"),
        (r"^/c/(\d+)/f/(\d+)$", "cfile"),
        (r"^/f/(\d+)$", "file"),
        (r"^/c/(\d+)/syllabus$", "syllabus"),
        (r"^/welcome$", "welcome"),
        (r"^/settings$", "settings"),
        (r"^/notebooks$", "notebooks"),
        (r"^/notebooks/new$", "nbnew"),
        (r"^/notebooks/([\w-]+)/add$", "nbadd"),
        (r"^/notebooks/([\w-]+)$", "notebook"),
        (r"^/anki$", "anki"),
        (r"^/anki/import$", "ankiimport"),
        (r"^/anki/deck/(\d+)$", "ankideck"),
    ]
    .into_iter()
    .map(|(p, n)| (Regex::new(p).unwrap(), n))
    .collect()
});

pub fn parse(hash: &str) -> Route {
    let h = hash.trim_start_matches('#');
    let h = if h.is_empty() { "/" } else { h };
    let (path_query, anchor) = match h.split_once('#') {
        Some((a, b)) => (a, Some(b.to_string())),
        None => (h, None),
    };
    let (path, qs) = path_query.split_once('?').unwrap_or((path_query, ""));
    let path = if path.is_empty() { "/" } else { path };
    let query = parse_query(qs);
    let mut view = View::NotFound;
    for (re, name) in ROUTES.iter() {
        if let Some(m) = re.captures(path) {
            let g = |i: usize| m.get(i).map(|x| x.as_str().to_string()).unwrap_or_default();
            view = match *name {
                "dashboard" => View::Dashboard,
                "inbox" => View::Inbox,
                "conversation" => View::Conversation(g(1)),
                "modules" => View::Modules(g(1)),
                "assignments" => View::Assignments(g(1)),
                "assignment" => View::Assignment(g(1), g(2)),
                "grades" => View::Grades(g(1)),
                "announcements" => View::Announcements(g(1)),
                "discussions" => View::Discussions(g(1)),
                "topic" => View::Topic(g(1), g(2)),
                "pages" => View::Pages(g(1)),
                "page" => View::Page(g(1), decode(&g(2))),
                "files" => View::Files(g(1)),
                "cfile" => View::File(Some(g(1)), g(2)),
                "file" => View::File(None, g(1)),
                "syllabus" => View::Syllabus(g(1)),
                "welcome" => View::Welcome,
                "settings" => View::Settings,
                "notebooks" => View::Notebooks,
                "nbnew" => View::NotebookNew,
                "nbadd" => View::NotebookAdd(g(1)),
                "notebook" => View::Notebook(g(1)),
                "anki" => View::Anki,
                "ankiimport" => View::AnkiImport,
                "ankideck" => View::AnkiDeck(g(1).parse().unwrap_or(0)),
                _ => View::NotFound,
            };
            break;
        }
    }
    Route { view, path: path.to_string(), anchor, query, hash: format!("#{h}") }
}

/// The path part of an href ("#/c/1/p/x?y" → "/c/1/p/x").
pub fn path_of(href: &str) -> String {
    href.trim_start_matches('#').split(['?', '#']).next().unwrap_or("").to_string()
}

static DOC_ROUTES: Lazy<Vec<Regex>> = Lazy::new(|| {
    [r"^/c/\d+/[adf]/\d+$", r"^/c/\d+/p/.+$", r"^/f/\d+$", r"^/inbox/\d+$"].iter().map(|p| Regex::new(p).unwrap()).collect()
});

/// Documents (pages, files, assignments, discussions, messages) open in the viewer.
pub fn is_doc(href: &str) -> bool {
    href.starts_with("#/") && DOC_ROUTES.iter().any(|re| re.is_match(&path_of(href)))
}

/// Encode a path segment the way encodeURIComponent does.
pub fn enc(s: &str) -> String {
    const SET: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'.').remove(b'!').remove(b'~').remove(b'*').remove(b'\'').remove(b'(').remove(b')');
    percent_encoding::utf8_percent_encode(s, SET).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        assert_eq!(parse("#/").view, View::Dashboard);
        assert_eq!(parse("").view, View::Dashboard);
        assert_eq!(parse("#/c/101/modules#m12").anchor.as_deref(), Some("m12"));
        assert_eq!(parse("#/c/101/p/heaps-overview").view, View::Page("101".into(), "heaps-overview".into()));
        assert_eq!(parse("#/notebooks/new?c=101&m=all").query.get("m").unwrap(), "all");
        assert_eq!(parse("#/notebooks/abc-1/add").view, View::NotebookAdd("abc-1".into()));
        assert_eq!(parse("#/nope").view, View::NotFound);
        assert!(is_doc("#/c/1/a/2") && is_doc("#/f/9") && is_doc("#/inbox/3") && !is_doc("#/c/1/files"));
    }
}
