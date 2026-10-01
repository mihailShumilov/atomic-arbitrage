//! Phase-1 feed recorder.
//!
//! Stores every feed frame verbatim, one per line, in hourly zstd files:
//!   <out>/YYYY/MM/DD/feed-YYYYMMDD-HH.tsv.zst
//! Line format (tab-separated, unchanged):
//!   recv_unix_ns \t seq_first \t seq_last \t <raw envelope JSON>
//! Lines without sequence numbers (JSON envelopes without `messages`;
//! non-text frames and text that is not JSON, wrapped as
//! `{"recorderFrame":...}`) have `seq_first = seq_last = 0`.
//!
//! Start-up: repair torn zstd tails, add holes missing from gaps.tsv, wait
//! out a pending pause / the minimum connect interval. Every connection asks
//! the feed to resume at `last_seq + 1` (`Arbitrum-Requested-Sequence-Number`,
//! task 009; `last_seq` from memory, at start-up from the data), unless there
//! is no data yet or `--no-requested-seq` is given. Whenever the recorder ends
//! a connection itself (SIGINT/SIGTERM, idle timeout): WebSocket Close 1000,
//! wait <= 2 s for the reply, then drop TCP; on shutdown drain and commit.
//!
//! Side files in <out>:
//!   gaps.tsv         `from \t to \t recv_ns` of missing L2 blocks (for RPC backfill)
//!   last_seq.txt     highest seq that is fsynced to disk (atomic replace)
//!   connections.tsv  connect/backlog/disconnect/startup/shutdown events and chosen pauses
//!   _torn/           torn zstd tails cut off after a crash (kept for analysis)
//!
//! Deliberately NOT done here: decoding l2Msg, signature checks, anything
//! latency-critical. Raw first, decode later. Exactly one feed connection.

mod backoff;
mod net;
mod resume;
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

use crate::backoff::{startup_wait, Ladder, StartupWaitReason};
use crate::net::{now_ns, run_connection, stopped, tls_connector, ConnEvent, ConnLog, Sink};
use crate::resume::{requested_seq, Backlog};
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
    /// previous run recorded in connections.tsv, and skip the
    /// --min-connect-interval-secs wait. Use only if you know the ban is
    /// over; the default is to honour both.
    #[arg(long)]
    ignore_pending_pause: bool,
    /// Minimum time between the last successful connect of an earlier run
    /// (from connections.tsv) and the first connect after a (re)start. The
    /// 403 + Retry-After: 3600 ban of 2026-09-30 followed a reconnect 11 s
    /// after kill -9; the cause is unknown, so restarts are spaced out.
    #[arg(long, default_value_t = 120)]
    min_connect_interval_secs: u64,
    /// Do not send `Arbitrum-Feed-Client-Version` /
    /// `Arbitrum-Requested-Sequence-Number` on connect: the stream then
    /// starts at the tip, as before task 009.
    #[arg(long)]
    no_requested_seq: bool,
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
    for g in &rec.reconciled {
        let detail = format!(
            "{}..{} recv_ns={} missing from gaps.tsv, appended",
            g.from, g.to, g.recv_ns
        );
        conn_log.event(ConnEvent {
            event: "gap_reconciled",
            reason: "startup",
            detail: &detail,
            ..Default::default()
        });
    }
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
    let min_interval = Duration::from_secs(args.min_connect_interval_secs);
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    // The network future borrows `tx`; it lives in this block so that `tx`
    // can be dropped after it.
    {
        let net = async {
            let mut stop = stop_rx.clone();
            let mut ladder = Ladder::default();
            // Honour a pause (e.g. Retry-After of a ban) chosen before a restart
            // and the minimum interval since the last successful connect.
            if !args.ignore_pending_pause {
                let pending = conn_log.last_pause();
                if let Some(p) = pending {
                    ladder.strikes = p.strikes;
                }
                let wait = startup_wait(
                    now_ns(),
                    pending.map(|p| p.not_before_ns),
                    conn_log.last_connected_ns(),
                    min_interval,
                );
                if let Some((wait, why)) = wait {
                    let strikes = pending.map_or(0, |p| p.strikes);
                    warn!(
                        wait_s = wait.as_secs_f64(),
                        reason = why.as_str(),
                        strikes,
                        "waiting before the first connect"
                    );
                    let detail = match why {
                        StartupWaitReason::PendingPause => {
                            format!("remaining pause from previous run, strikes={strikes}")
                        }
                        StartupWaitReason::MinConnectInterval => format!(
                            "last connect less than {}s ago",
                            args.min_connect_interval_secs
                        ),
                    };
                    conn_log.event(ConnEvent {
                        event: "startup_wait",
                        reason: why.as_str(),
                        pause: Some(wait),
                        strikes: Some(strikes),
                        detail: &detail,
                        ..Default::default()
                    });
                    tokio::select! {
                        _ = tokio::time::sleep(wait) => {}
                        _ = stopped(&mut stop) => return,
                    }
                }
            }
            // Highest seq handed to the writer; at start-up the highest seq
            // in the data (recover), not last_seq.txt (task 009, item 1).
            let mut last_seq = rec.data_seq;
            loop {
                let before = last_seq;
                let (requested, mode) = requested_seq(before, !args.no_requested_seq);
                let mut sink = Sink::new(&tx, &mut last_seq, Backlog::new(requested, before));
                let strikes = ladder.strikes;
                let log_backlog = |b: &Backlog, reason: &str| {
                    info!(detail = %b.detail(), "backlog");
                    conn_log.event(ConnEvent {
                        event: "backlog",
                        reason,
                        http_status: Some(101),
                        session: Some(b.live_after),
                        envelopes: Some(b.blocks),
                        detail: &b.detail(),
                        ..Default::default()
                    });
                };
                let end = run_connection(
                    &args.url,
                    &tls,
                    idle,
                    requested,
                    &mut sink,
                    &mut stop,
                    || {
                        let detail = format!(
                            "{} requested={} mode={}",
                            args.url,
                            requested.map_or_else(|| "-".to_string(), |n| n.to_string()),
                            mode.as_str()
                        );
                        conn_log.event(ConnEvent {
                            event: "connected",
                            reason: "-",
                            http_status: Some(101),
                            strikes: Some(strikes),
                            detail: &detail,
                            ..Default::default()
                        });
                    },
                    |b| log_backlog(b, "done"),
                )
                .await;
                // Backlog still running when the connection ended: log what we have.
                if let Some(b) = &end.backlog {
                    if !b.done && b.first_seq.is_some() {
                        log_backlog(b, "session_ended");
                    }
                }
                if let Some(c) = &end.client_close {
                    let detail = format!(
                        "sent close 1000, waited {} ms; {}",
                        c.took.as_millis(),
                        c.detail
                    );
                    conn_log.event(ConnEvent {
                        event: "client_close",
                        reason: c.reason,
                        http_status: Some(101),
                        session: Some(end.session),
                        envelopes: Some(end.envelopes),
                        detail: &detail,
                        ..Default::default()
                    });
                }
                if *stop.borrow() {
                    return;
                }
                let (pause, rule) =
                    ladder.next_pause(end.kind, end.retry_after, end.session, rand01());
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
                tokio::select! {
                    _ = tokio::time::sleep(pause) => {}
                    _ = stopped(&mut stop) => return,
                }
            }
        };
        tokio::pin!(net);

        let sig = tokio::select! {
            _ = &mut net => unreachable!("network loop only returns after stop"),
            s = shutdown_signal() => s,
        };
        info!(
            signal = sig,
            "shutting down: closing connection, draining queue, closing frame, fsync"
        );
        conn_log.event(ConnEvent {
            event: "shutdown",
            reason: sig,
            ..Default::default()
        });
        // Let the network loop finish the WebSocket close handshake (<= 2 s for
        // the server's reply + 0.5 s for the stream shutdown). The outer bound
        // only guards against a bug; systemd gives us TimeoutStopSec=30.
        let _ = stop_tx.send(true);
        if tokio::time::timeout(Duration::from_secs(5), &mut net)
            .await
            .is_err()
        {
            warn!("network loop did not stop in 5 s, dropping the connection");
        }
    }
    // Dropping the sender lets the writer drain everything queued and commit.
    drop(tx);
    if writer.join().is_err() {
        error!("writer thread panicked");
        std::process::exit(2);
    }
    info!("stopped");
    Ok(())
}
