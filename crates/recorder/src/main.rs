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
//! out a pending pause / the minimum connect interval, counted from the end
//! of the previous session (task 012 item 1). Every connection asks
//! the feed to resume at `last_seq + 1` (`Arbitrum-Requested-Sequence-Number`,
//! task 009; `last_seq` from memory, at start-up from the data), unless there
//! is no data yet or `--no-requested-seq` is given. Whenever the recorder ends
//! a connection itself (SIGINT/SIGTERM, idle timeout, no block for
//! `--block-idle-timeout-secs`, fatal writer error): WebSocket Close 1000,
//! wait <= 2 s for the reply, then drop TCP; on shutdown drain and commit.
//! A fatal writer error (disk full, fsync) exits with code 2 after the Close
//! (task 012 item 2).
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

use crate::backoff::{session_end_ns, startup_wait, Ladder, StartupWaitReason};
use crate::net::{
    now_ns, run_connection, stop_reason, stopped, tls_connector, ConnEvent, ConnLog, Sink,
    SHUTDOWN_REASON,
};
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
    /// Minimum time between the end of the previous session and the first
    /// connect after a (re)start. End = last row of connections.tsv after the
    /// last `connected`, or the mtime of the newest hourly data file if that
    /// is later (kill -9 leaves no row). The 403 + Retry-After: 3600 ban of
    /// 2026-09-30 followed a reconnect 11 s after kill -9; the cause is
    /// unknown, so restarts are spaced out.
    #[arg(long, default_value_t = 120)]
    min_connect_interval_secs: u64,
    /// Reconnect (Close 1000, then resume at last_seq + 1) if no frame with a
    /// block (seq > 0) arrives for this many seconds, even if pings and
    /// confirmations keep the stream alive. Blocks are ~100 ms apart. 0
    /// disables.
    #[arg(long, default_value_t = 30)]
    block_idle_timeout_secs: u64,
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
    let conn_log = ConnLog::new(&args.out_dir);

    // End of the previous session (task 012 item 1). Read before recovery
    // and before this run writes any row: a torn-tail repair rewrites the
    // newest file (mtime) and start-up rows would look like a later end.
    let last_session = conn_log.last_session();
    let session_end = session_end_ns(
        last_session.map(|s| s.connected_ns),
        last_session.and_then(|s| s.last_row_ns),
        writer::newest_data_mtime_ns(&args.out_dir),
    );

    // Crash recovery before anything is written.
    let rec = writer::recover(&args.out_dir)?;
    info!(
        state = ?rec.state_seq, data = ?rec.data_seq, resume = ?rec.resume_seq,
        torn_repairs = rec.repairs.len(), "recovery done"
    );
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
    // A fatal writer error (disk full, fsync) is reported here; `run` has
    // already dropped `rx`, so the network side cannot block on a full
    // channel. The main task then closes the connection politely and exits
    // with code 2 (task 012 item 2; before: exit(2) right in this thread,
    // without a WebSocket Close).
    let (writer_err_tx, mut writer_err_rx) = tokio::sync::oneshot::channel::<String>();
    let writer = std::thread::spawn(move || match writer::run(rx, w) {
        Ok(_) => Ok(()),
        Err(e) => {
            let msg = format!("{e:#}");
            error!(error = %msg, "writer failed");
            let _ = writer_err_tx.send(msg.clone());
            Err(msg)
        }
    });

    let tls = tls_connector()?;
    let idle = Duration::from_secs(args.idle_timeout_secs);
    let block_idle = Duration::from_secs(args.block_idle_timeout_secs);
    let min_interval = Duration::from_secs(args.min_connect_interval_secs);
    let (stop_tx, stop_rx) = tokio::sync::watch::channel::<Option<&'static str>>(None);
    let mut writer_failed: Option<String> = None;
    // The network future borrows `tx`; it lives in this block so that `tx`
    // can be dropped after it.
    {
        let net = async {
            let mut stop = stop_rx.clone();
            let mut ladder = Ladder::default();
            // Honour a pause (e.g. Retry-After of a ban) chosen before a restart
            // and the minimum interval since the end of the previous session.
            if !args.ignore_pending_pause {
                let pending = conn_log.last_pause();
                if let Some(p) = pending {
                    ladder.strikes = p.strikes;
                }
                let now = now_ns();
                let wait = startup_wait(
                    now,
                    pending.map(|p| p.not_before_ns),
                    session_end.map(|e| e.0),
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
                        StartupWaitReason::MinConnectInterval => {
                            let (end, src) = session_end.expect("interval wait needs an end");
                            format!(
                                "previous session ended {:.3}s ago (end={}), min interval {}s",
                                now.saturating_sub(end) as f64 / 1e9,
                                src.as_str(),
                                args.min_connect_interval_secs
                            )
                        }
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
                    block_idle,
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
                if stop_reason(&stop).is_some() {
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

        let (sig, close_reason) = tokio::select! {
            _ = &mut net => unreachable!("network loop only returns after stop"),
            s = shutdown_signal() => (s, SHUTDOWN_REASON),
            Ok(msg) = &mut writer_err_rx => {
                writer_failed = Some(msg);
                ("writer_error", "recorder writer error")
            }
        };
        match &writer_failed {
            None => info!(
                signal = sig,
                "shutting down: closing connection, draining queue, closing frame, fsync"
            ),
            Some(e) => error!(
                error = %e,
                "writer failed: closing the connection (Close 1000), then exit 2"
            ),
        }
        conn_log.event(ConnEvent {
            event: "shutdown",
            reason: sig,
            detail: writer_failed.as_deref().unwrap_or(""),
            ..Default::default()
        });
        // Let the network loop finish the WebSocket close handshake (<= 2 s for
        // the server's reply + 0.5 s for the stream shutdown). The outer bound
        // only guards against a bug; systemd gives us TimeoutStopSec=30.
        let _ = stop_tx.send(Some(close_reason));
        if tokio::time::timeout(Duration::from_secs(5), &mut net)
            .await
            .is_err()
        {
            warn!("network loop did not stop in 5 s, dropping the connection");
        }
    }
    // Dropping the sender lets the writer drain everything queued and commit.
    drop(tx);
    match writer.join() {
        Err(_) => {
            error!("writer thread panicked");
            std::process::exit(2);
        }
        Ok(Err(e)) => {
            // Failed during the run (Close already sent above) or in the
            // final commit after a signal.
            if writer_failed.is_none() {
                conn_log.event(ConnEvent {
                    event: "writer_error",
                    reason: "final_commit",
                    detail: &e,
                    ..Default::default()
                });
            }
            error!(error = %e, "writer failed, exit 2");
            std::process::exit(2);
        }
        Ok(Ok(())) => {}
    }
    info!("stopped");
    Ok(())
}
