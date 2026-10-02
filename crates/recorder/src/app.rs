//! The recorder process: start-up (recovery, wait), the reconnect loop, and
//! shutdown with its exit code (task 021 item 3; moved out of `main.rs`).
//!
//! Exit codes (also in data-model.md): 0 — stopped by a signal;
//! [`EXIT_WRITER_ERROR`] (2) — fatal writer error (disk, fsync), after a
//! WebSocket Close; 1 — error before recording starts (`run` returns `Err`,
//! e.g. the out-dir cannot be created or recovery fails).

use std::path::PathBuf;
use std::sync::mpsc::{sync_channel, SyncSender};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio_rustls::TlsConnector;
use tracing::{error, info, warn};

use crate::backoff::{session_end_ns, startup_wait, Ladder, SessionEndSource};
use crate::connlog::{gaps_skipped_events, ConnEvent, ConnLog, PendingPause};
use crate::layout::newest_data_mtime_ns;
use crate::now_ns;
use crate::recovery::recover;
use crate::resume::{requested_seq, Backlog};
use crate::route::Line;
use crate::seqtrack::SeqTracker;
use crate::session::{run_connection, stop_reason, stopped, ConnParams, Sink, Stop, SHUTDOWN_REASON};
use crate::transport::tls_connector;
use crate::writer::{self, FeedWriter};

/// Exit code after a fatal writer error.
pub const EXIT_WRITER_ERROR: i32 = 2;

/// Lines between the network task and the writer thread. At ~9 lines/s
/// (assumed: a block every ~113 ms plus unsequenced lines) this is hours of
/// feed; it only fills if the writer is stuck on the disk, and then the
/// network task blocks in `send` (also for SIGTERM; systemd kills it after
/// `TimeoutStopSec=30`).
const CHANNEL_CAP: usize = 200_000;
/// Bound on the network loop's close handshake after a stop (<= 2 s for the
/// server's reply + 0.5 s stream shutdown); only guards against a bug.
const STOP_WAIT: Duration = Duration::from_secs(5);

/// Everything the process needs from the command line.
#[derive(Debug, Clone)]
pub struct Config {
    pub url: String,
    pub out_dir: PathBuf,
    pub idle: Duration,
    pub zstd_level: i32,
    pub frame: Duration,
    pub ignore_pending_pause: bool,
    pub min_interval: Duration,
    pub block_idle: Duration,
    /// Send `Arbitrum-Requested-Sequence-Number` (off: `--no-requested-seq`).
    pub requested_seq: bool,
}

/// How the process ended (other than an `Err` before recording).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Stopped by a signal, everything committed.
    Stopped,
    /// The writer failed; exit with [`EXIT_WRITER_ERROR`].
    WriterFailed,
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

/// Honour a pause (e.g. Retry-After of a ban) chosen before a restart and
/// the minimum interval since the end of the previous session (tasks 008,
/// 012). Returns false if stopped while waiting.
/// `pending` is read from the journal before this run writes any row
/// (finding Б1 of the 021 data audit).
async fn startup_wait_phase(
    cfg: &Config,
    conn_log: &ConnLog,
    pending: Option<PendingPause>,
    session_end: Option<(u128, SessionEndSource)>,
    ladder: &mut Ladder,
    stop: &mut Stop,
) -> bool {
    if cfg.ignore_pending_pause {
        return true;
    }
    // The strikes counter is restored as before task 021 (Р6 of the review
    // is Mihail's decision).
    if let Some(p) = pending {
        ladder.strikes = p.strikes;
    }
    let now = now_ns();
    let Some((wait, why)) =
        startup_wait(now, pending.map(|p| p.not_before_ns), session_end.map(|e| e.0), cfg.min_interval)
    else {
        return true;
    };
    let strikes = pending.map_or(0, |p| p.strikes);
    warn!(wait_s = wait.as_secs_f64(), reason = why.as_str(), strikes, "waiting before the first connect");
    conn_log.event(ConnEvent::startup_wait(why, wait, strikes, now, session_end, cfg.min_interval));
    tokio::select! {
        _ = tokio::time::sleep(wait) => true,
        _ = stopped(stop) => false,
    }
}

/// Connect, record, pause, reconnect, until `stop` is set. `seq` is the
/// highest seq handed to the writer: at start-up the highest seq in the
/// data (recover), not last_seq.txt (task 009, item 1).
async fn net_loop(
    p: &ConnParams<'_>,
    cfg: &Config,
    conn_log: &ConnLog,
    tx: &SyncSender<Line>,
    mut seq: SeqTracker,
    mut ladder: Ladder,
    stop: &mut Stop,
) {
    loop {
        let before = seq.last();
        let (requested, mode) = requested_seq(before, cfg.requested_seq);
        let mut sink = Sink::new(tx, &mut seq, Backlog::new(requested, before));
        let strikes = ladder.strikes;
        let log_backlog = |b: &Backlog, reason: &'static str| {
            info!(detail = %b.detail(), "backlog");
            conn_log.event(ConnEvent::backlog(b, reason));
        };
        let end = run_connection(
            p,
            requested,
            &mut sink,
            stop,
            || conn_log.event(ConnEvent::connected(&cfg.url, requested, mode, strikes)),
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
            conn_log.event(ConnEvent::client_close(c, end.session, end.envelopes));
        }
        if stop_reason(stop).is_some() {
            return;
        }
        let (pause, rule) = ladder.next_pause(end.kind, end.retry_after, end.session, rand01());
        warn!(
            reason = end.kind.as_str(), http = ?end.http_status, retry_after = ?end.retry_after_raw,
            session_s = end.session.as_secs_f64(), envelopes = end.envelopes,
            pause_s = pause.as_secs_f64(), rule = rule.as_str(), detail = %end.detail,
            "connection ended"
        );
        conn_log.event(ConnEvent::disconnected(&end, pause, rule, ladder.strikes));
        tokio::select! {
            _ = tokio::time::sleep(pause) => {}
            _ = stopped(stop) => return,
        }
    }
}

/// Run the recorder until a signal (`Stopped`) or a fatal writer error
/// (`WriterFailed`). `Err`: failed before recording (exit 1).
pub async fn run(cfg: Config) -> Result<Outcome> {
    std::fs::create_dir_all(&cfg.out_dir).with_context(|| format!("create {}", cfg.out_dir.display()))?;
    let conn_log = ConnLog::new(&cfg.out_dir);

    // End of the previous session (task 012 item 1) and the pending pause.
    // Both are read before recovery and before this run writes any row: a
    // torn-tail repair rewrites the newest file (mtime), start-up rows would
    // look like a later end, and many start-up rows could push the
    // `disconnected 403` row out of the 64 KiB tail that is read (finding Б1
    // of the 021 data audit: the recorder then reconnected during a ban).
    let last_session = conn_log.last_session();
    let pending = conn_log.last_pause();
    let session_end = session_end_ns(
        last_session.map(|s| s.connected_ns),
        last_session.and_then(|s| s.last_row_ns),
        newest_data_mtime_ns(&cfg.out_dir),
    );

    // Crash recovery before anything is written.
    let rec = recover(&cfg.out_dir)?;
    info!(
        state = ?rec.state_seq, data = ?rec.data_seq, resume = ?rec.resume_seq,
        torn_repairs = rec.repairs.len(), "recovery done"
    );
    for e in gaps_skipped_events(&rec.skipped_gap_lines) {
        conn_log.event(e);
    }
    for g in &rec.reconciled {
        conn_log.event(ConnEvent::gap_reconciled(g));
    }
    for r in &rec.repairs {
        conn_log.event(ConnEvent::torn_repair(r));
    }

    let (tx, rx) = sync_channel::<Line>(CHANNEL_CAP);
    let w = FeedWriter::new(cfg.out_dir.clone(), cfg.zstd_level, cfg.frame.max(Duration::from_secs(1)), rec.resume_seq);
    // A fatal writer error (disk full, fsync) is reported here; `run` has
    // already dropped `rx`, so the network side cannot block on a full
    // channel. The main task then closes the connection politely and exits
    // with code 2 (task 012 item 2).
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

    let tls: TlsConnector = tls_connector()?;
    let params = ConnParams { url: &cfg.url, tls: &tls, idle: cfg.idle, block_idle: cfg.block_idle };
    let (stop_tx, stop_rx) = tokio::sync::watch::channel::<Option<&'static str>>(None);
    let mut writer_failed: Option<String> = None;
    // The network future borrows `tx`; it lives in this block so that `tx`
    // can be dropped after it.
    {
        let net = async {
            let mut stop = stop_rx.clone();
            let mut ladder = Ladder::default();
            if startup_wait_phase(&cfg, &conn_log, pending, session_end, &mut ladder, &mut stop).await {
                net_loop(&params, &cfg, &conn_log, &tx, SeqTracker::new(rec.data_seq), ladder, &mut stop).await;
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
            None => info!(signal = sig, "shutting down: closing connection, draining queue, closing frame, fsync"),
            Some(e) => error!(error = %e, "writer failed: closing the connection (Close 1000), then exit 2"),
        }
        conn_log.event(ConnEvent::shutdown(sig, writer_failed.as_deref().unwrap_or("")));
        // Let the network loop finish the WebSocket close handshake.
        // systemd gives us TimeoutStopSec=30.
        let _ = stop_tx.send(Some(close_reason));
        if tokio::time::timeout(STOP_WAIT, &mut net).await.is_err() {
            warn!("network loop did not stop in 5 s, dropping the connection");
        }
    }
    // Dropping the sender lets the writer drain everything queued and commit.
    drop(tx);
    match writer.join() {
        Err(_) => {
            error!("writer thread panicked");
            Ok(Outcome::WriterFailed)
        }
        Ok(Err(e)) => {
            // Failed during the run (Close already sent above) or in the
            // final commit after a signal.
            if writer_failed.is_none() {
                conn_log.event(ConnEvent::writer_error_final(&e));
            }
            error!(error = %e, "writer failed, exit 2");
            Ok(Outcome::WriterFailed)
        }
        Ok(Ok(())) => {
            info!("stopped");
            Ok(Outcome::Stopped)
        }
    }
}
