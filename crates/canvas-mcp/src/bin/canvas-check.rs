//! Check that your Firefox's Canvas session works with the API. Prints cookie names only, never values.
//!
//! Usage: canvas-check [--diagnose]
//!        canvas-check panopto <viewer URL or recording id> [--folders]

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(canvas_mcp::check::check_main(&args).await);
}
