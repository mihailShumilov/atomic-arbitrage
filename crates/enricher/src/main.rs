//! CLI entry point; see the crate docs in lib.rs for modes and file formats
//! and `enricher::exit` for the exit codes.

use std::sync::Arc;

use clap::Parser;
use enricher::exit::{self, Outcome};
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
            Err(e) => {
                warn!("cannot install the SIGTERM handler ({e}); only SIGINT stops the run cleanly");
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
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let a = Args::parse();
    let stats = Arc::new(Stats::default());

    // On a signal the `run` future is dropped: the open AtomicZstdFile is
    // dropped with it and deletes its `*.partial`, so no final-named file
    // appears for an unfinished range and filled.tsv is not touched.
    let outcome = tokio::select! {
        r = run(&a, stats.clone()) => match r {
            Ok(()) => Outcome::Done,
            Err(e) => Outcome::Failed(e),
        },
        sig = shutdown_signal() => Outcome::Interrupted(sig),
    };
    // Reporting never changes the exit code (task 020 item 5): a failed
    // --stats-json write must not turn 75/130 into 1.
    if let Err(e) = report(&stats.summary(), a.stats_json.as_deref()) {
        warn!("{e:#}; the exit code is not affected");
    }
    let code = exit::code(&outcome);
    match &outcome {
        Outcome::Done => {}
        Outcome::Interrupted(sig) => warn!("interrupted by {sig}; unfinished file discarded"),
        // Task 012 item 5: a used-up --max-calls is not a failure of the
        // run; finished files are already in filled.tsv. systemd treats 75
        // as success via SuccessExitStatus=75 (deploy/enricher-gaps.service).
        Outcome::Failed(e) if code == exit::BUDGET_EXHAUSTED => {
            warn!("{e:#}; stopping with exit code {code}, the next run continues");
        }
        // Same text and stream as `main() -> anyhow::Result` printed before.
        Outcome::Failed(e) => eprintln!("Error: {e:?}"),
    }
    // process::exit as before: the run future is already dropped (partial
    // files removed), and a pending DNS lookup must not delay the exit.
    std::process::exit(i32::from(code));
}
