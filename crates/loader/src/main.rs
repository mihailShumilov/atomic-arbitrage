//! CLI entry point; see the crate docs in lib.rs.
//!
//! Exit codes: 0 — done; 1 — any error (the failing file left nothing behind, files before it
//! stay loaded; re-running is safe).

use clap::Parser;
use loader::{report, run, Args};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let a = Args::parse();
    match run(&a).await {
        Ok(s) => report(&s),
        Err(e) => {
            eprintln!("Error: {e:?}");
            std::process::exit(1);
        }
    }
}
