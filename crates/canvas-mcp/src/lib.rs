//! canvas-mcp: your Canvas LMS as a local cache, an MCP server for LLMs, and the shared core of
//! the desktop app, signed in with your Firefox session.

pub mod anki;
pub mod check;
pub mod mcp;
pub mod anki_setup;
pub mod checkpoints;
pub mod client;
pub mod config;
pub mod cookies;
pub mod demo;
pub mod engine;
pub mod error;
pub mod files;
pub mod markdown;
pub mod notebooklm;
pub mod notebooks;
pub mod panopto;
pub mod prefs;
pub mod recordings;
pub mod resources;
pub mod services;
pub mod signin;
pub mod store;
pub mod util;

pub use error::{Error, Result};
