//! The MCP server: read-only access to your Canvas for an LLM, over stdio.
//!
//! Usage: canvas-mcp                 serve MCP on stdin/stdout
//!        canvas-mcp check [...]     same as canvas-check
//!        canvas-mcp smoke [-v]      call every tool once and report (CANVAS_DEMO=1 for sample data)

use std::sync::Arc;

use canvas_mcp::client::Client;
use canvas_mcp::engine::Engine;
use canvas_mcp::store::Store;
use canvas_mcp::{check, mcp};

fn server() -> Arc<mcp::Server> {
    let store = match Store::open_default() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("canvas-mcp: couldn't open the cache: {e}");
            std::process::exit(1);
        }
    };
    Arc::new(mcp::Server::new(Engine::new(store, Client::from_env())))
}

#[tokio::main]
async fn main() {
    // Logs go to stderr: stdout is the protocol.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).target(env_logger::Target::Stderr).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("check") => std::process::exit(check::check_main(&args[1..]).await),
        Some("smoke") => std::process::exit(check::smoke(server(), args.iter().any(|a| a == "-v")).await),
        Some("--version") => println!("canvas-mcp {}", env!("CARGO_PKG_VERSION")),
        Some("-h" | "--help") => println!("Usage: canvas-mcp [check [--diagnose] | smoke [-v]]\nWith no arguments, serves MCP over stdio."),
        _ => server().run_stdio().await,
    }
}
