//! One error type for the whole library. It is Clone, so a refresh that several callers wait on
//! can hand each of them the same error.

#[derive(Clone, Debug, thiserror::Error)]
pub enum Error {
    /// You haven't allowed the app to use your Canvas sign-in from Firefox yet (a kind of SessionExpired).
    #[error("{0}")]
    NeedsPermission(String),
    /// No usable Canvas session: logged out in Firefox, or the login expired.
    #[error("{0}")]
    SessionExpired(String),
    /// Canvas isn't set up (no canvas_url).
    #[error("{0}")]
    NotConfigured(String),
    /// Reading a site's Firefox cookies needs your permission (the site: a host, or "google").
    #[error("canvas-mcp needs your permission to use {} from Firefox. Allow it in the Canvas app, or run canvas-check.", what_site(.0))]
    NotPermitted(String),
    /// No Panopto session (or Panopto isn't set up).
    #[error("{0}")]
    PanoptoSession(String),
    /// Anki isn't running, or AnkiConnect isn't installed.
    #[error("{0}")]
    AnkiOffline(String),
    /// Something other than AnkiConnect answers on its port.
    #[error("Another program is using Anki's address ({0}), so the app can't reach Anki.")]
    AnkiPortTaken(String),
    /// AnkiConnect answered with an error.
    #[error("{0}")]
    Anki(String),
    /// Something wasn't found (LookupError).
    #[error("{0}")]
    NotFound(String),
    /// Bad input (ValueError).
    #[error("{0}")]
    Invalid(String),
    /// An HTTP status error.
    #[error("{message}")]
    Status { status: u16, message: String },
    /// The network, a timeout, or an unexpected response.
    #[error("{0}")]
    Fetch(String),
    #[error("{0}")]
    Io(String),
    /// A sign-in to Google/NotebookLM failed (the caller may retry with fresh cookies).
    #[error("{0}")]
    Auth(String),
    #[error("{0}")]
    Other(String),
}

fn what_site(site: &str) -> String {
    if site == "google" { "your Google sign-in".into() } else { format!("your sign-in for {site}") }
}

impl Error {
    /// Session problems of any kind (SessionExpired and its subclass NeedsPermission).
    pub fn is_session(&self) -> bool {
        matches!(self, Error::SessionExpired(_) | Error::NeedsPermission(_))
    }

    /// The error's name as the Python version reported it, for "Couldn't reach Canvas (X)".
    pub fn type_name(&self) -> &'static str {
        match self {
            Error::NeedsPermission(_) => "NeedsPermission",
            Error::SessionExpired(_) => "SessionExpired",
            Error::NotConfigured(_) => "NotConfigured",
            Error::NotPermitted(_) => "NotPermitted",
            Error::PanoptoSession(_) => "PanoptoSessionExpired",
            Error::AnkiOffline(_) => "AnkiOffline",
            Error::AnkiPortTaken(_) => "AnkiPortTaken",
            Error::Anki(_) => "AnkiError",
            Error::NotFound(_) => "LookupError",
            Error::Invalid(_) => "ValueError",
            Error::Status { .. } => "HTTPStatusError",
            Error::Fetch(_) => "ConnectError",
            Error::Io(_) => "OSError",
            Error::Auth(_) => "AuthError",
            Error::Other(_) => "Error",
        }
    }

    /// The UI's error kind: permission, setup, session or fetch.
    pub fn kind(&self) -> &'static str {
        match self {
            Error::NeedsPermission(_) => "permission",
            Error::NotConfigured(_) => "setup",
            Error::SessionExpired(_) => "session",
            _ => "fetch",
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        if let Some(s) = e.status() {
            return Error::Status { status: s.as_u16(), message: e.to_string() };
        }
        if e.is_timeout() {
            return Error::Fetch("the request timed out".into());
        }
        Error::Fetch(e.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Fetch(format!("invalid JSON: {e}"))
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Io(format!("database: {e}"))
    }
}

pub type Result<T> = std::result::Result<T, Error>;
