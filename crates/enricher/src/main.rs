//! CLI entry point; see the crate docs in lib.rs for modes and file formats.

use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use enricher::stats::Stats;
use enricher::{report, run, Args};
use tracing::warn;

async fn shutdown_signal() -> &'static str {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => tokio::select! {
                _ = tokio::signal::ctrl_c() => "SIGINT",
                _ = term.recv() => "SIGTERM",
            },
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                "SIGINT"
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        "SIGINT"
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let a = Args::parse();
    let stats = Arc::new(Stats::default());

    // On a signal the `run` future is dropped: the open AtomicZstdFile is
    // dropped with it and deletes its `*.partial`, so no final-named file
    // appears for an unfinished range and filled.tsv is not touched.
    let outcome = tokio::select! {
        r = run(&a, stats.clone()) => r.map_err(|e| (e, None)),
        sig = shutdown_signal() => Err((anyhow::anyhow!("interrupted by {sig}; unfinished file discarded"), Some(sig))),
    };
    report(&stats.summary(), a.stats_json.as_deref())?;
    match outcome {
        Ok(()) => Ok(()),
        Err((e, Some(_))) => {
            warn!("{e:#}");
            std::process::exit(130);
        }
        Err((e, None)) => Err(e),
    }
}
