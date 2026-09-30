//! Phase-1 feed recorder.
//!
//! Stores every feed frame verbatim, one per line, in hourly zstd files:
//!   <out>/YYYY/MM/DD/feed-YYYYMMDD-HH.tsv.zst
//! Line format (tab-separated, unchanged):
//!   recv_unix_ns \t seq_first \t seq_last \t <raw envelope JSON>
//! Lines without sequence numbers (envelopes without `messages`, non-text
//! frames wrapped as `{"recorderFrame":...}`) have `seq_first = seq_last = 0`.
//!
//! Side files in <out>:
//!   gaps.tsv         `from \t to \t recv_ns` of missing L2 blocks (for RPC backfill)
//!   last_seq.txt     highest seq that is fsynced to disk (atomic replace)
//!   connections.tsv  connect/disconnect events and chosen reconnect pauses
//!   _torn/           torn zstd tails cut off after a crash (kept for analysis)
//!
//! Deliberately NOT done here: decoding l2Msg, signature checks, anything
//! latency-critical. Raw first, decode later. Exactly one feed connection.

mod backoff;
mod net;
mod route;
mod writer;

use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::sync_channel;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use hood_core::FEED_URL;
use tracing::{error, info, warn};

use crate::backoff::Ladder;
use crate::net::{now_ns, run_connection, tls_connector, ConnEvent, ConnLog};
use crate::route::Line;
use crate::writer::FeedWriter;

#[derive(Parser, Debug)]
#[command(about = "Record the Robinhood Chain sequencer feed to hourly zstd files")]
struct Args {
    #[arg(long, env = "FEED_URL", default_value = FEED_URL)]
    url: String,
    #[arg(long, env = "RECORDER_OUT_DIR", default_value = "data/feed")]
    out_dir: PathBuf,
    /// Reconnect if no frame arrives for this many seconds
    /// (blocks are ~100 ms, silence means a broken stream).
    #[arg(long, default_value_t = 10)]
    idle_timeout_secs: u64,
    #[arg(long, default_value_t = 3)]
    zstd_level: i32,
    /// Close the zstd frame and fsync at least this often. Bounds the data
    /// lost on kill -9 / power loss.
    #[arg(long, default_value_t = 60)]
    frame_secs: u64,
    /// Do not wait out a reconnect pause (e.g. Retry-After of a ban) that a
    /// previous run recorded in connections.tsv. Use only if you know the
    /// ban is over; the default is to honour it.
    #[arg(long)]
    ignore_pending_pause: bool,
}

/// Cheap jitter source in [0, 1) without an RNG dependency.
fn rand01() -> f64 {
    let n = now_ns() as u64;
    // splitmix64 finaliser
    let mut z = n.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 11) as f64 / (1u64 << 53) as f64
}

async fn shutdown_signal() -> &'static str {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "cannot install SIGTERM handler, only ctrl-c works");
                let _ = tokio::signal::ctrl_c().await;
                return "SIGINT";
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => "SIGINT",
            _ = term.recv() => "SIGTERM",
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
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    fs::create_dir_all(&args.out_dir)?;

    // Crash recovery before anything is written.
    let rec = writer::recover(&args.out_dir)?;
    info!(
        state = ?rec.state_seq, data = ?rec.data_seq, resume = ?rec.resume_seq,
        torn_repairs = rec.repairs.len(), "recovery done"
    );
    let conn_log = ConnLog::new(&args.out_dir);
    for r in &rec.repairs {
        let detail = format!(
            "{} kept={} torn={} saved={}",
            r.file.display(),
            r.kept_bytes,
            r.torn_bytes,
            r.saved_to.display()
        );
        conn_log.event(ConnEvent {
            event: "torn_repair",
            reason: "startup",
            detail: &detail,
            ..Default::default()
        });
    }

    let (tx, rx) = sync_channel::<Line>(200_000);
    let w = FeedWriter::new(
        args.out_dir.clone(),
        args.zstd_level,
        Duration::from_secs(args.frame_secs.max(1)),
        rec.resume_seq,
    );
    let writer = std::thread::spawn(move || {
        if let Err(e) = writer::run(rx, w) {
            error!(error = %e, "writer failed");
            std::process::exit(2);
        }
    });

    let tls = tls_connector()?;
    let idle = Duration::from_secs(args.idle_timeout_secs);
    let net = async {
        let mut ladder = Ladder::default();
        // Honour a pause (e.g. Retry-After of a ban) chosen before a restart.
        if let Some(p) = conn_log.last_pause().filter(|_| !args.ignore_pending_pause) {
            ladder.strikes = p.strikes;
            let now = now_ns();
            if p.not_before_ns > now {
                let wait = Duration::from_nanos((p.not_before_ns - now) as u64);
                warn!(
                    wait_s = wait.as_secs_f64(),
                    strikes = p.strikes,
                    "pause from before restart still active, waiting"
                );
                let detail = format!("remaining pause from previous run, strikes={}", p.strikes);
                conn_log.event(ConnEvent {
                    event: "startup_wait",
                    reason: "pending_pause",
                    pause: Some(wait),
                    strikes: Some(p.strikes),
                    detail: &detail,
                    ..Default::default()
                });
                tokio::time::sleep(wait).await;
            }
        }
        loop {
            let end = run_connection(&args.url, &tls, idle, &tx, || {
                conn_log.event(ConnEvent {
                    event: "connected",
                    reason: "-",
                    http_status: Some(101),
                    strikes: Some(ladder.strikes),
                    detail: &args.url,
                    ..Default::default()
                });
            })
            .await;
            let (pause, rule) = ladder.next_pause(end.kind, end.retry_after, end.session, rand01());
            warn!(
                reason = end.kind.as_str(), http = ?end.http_status, retry_after = ?end.retry_after_raw,
                session_s = end.session.as_secs_f64(), envelopes = end.envelopes,
                pause_s = pause.as_secs_f64(), rule = rule.as_str(), detail = %end.detail,
                "connection ended"
            );
            let detail = format!("rule={} {}", rule.as_str(), end.detail);
            conn_log.event(ConnEvent {
                event: "disconnected",
                reason: end.kind.as_str(),
                http_status: end.http_status,
                retry_after: end.retry_after_raw.as_deref(),
                pause: Some(pause),
                session: Some(end.session),
                envelopes: Some(end.envelopes),
                strikes: Some(ladder.strikes),
                detail: &detail,
            });
            tokio::time::sleep(pause).await;
        }
    };

    let sig = tokio::select! {
        _ = net => unreachable!("network loop never returns"),
        s = shutdown_signal() => s,
    };
    info!(
        signal = sig,
        "shutting down: draining queue, closing frame, fsync"
    );
    conn_log.event(ConnEvent {
        event: "shutdown",
        reason: sig,
        ..Default::default()
    });
    // Dropping the sender lets the writer drain everything queued and commit.
    drop(tx);
    if writer.join().is_err() {
        error!("writer thread panicked");
        std::process::exit(2);
    }
    info!("stopped");
    Ok(())
}
