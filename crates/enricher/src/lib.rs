//! Phase-1 enricher: the feed has no receipts, logs or revert status, so this
//! fetches them over read-only JSON-RPC.
//!
//! Modes:
//! - `blocks` (default): `eth_getBlockByNumber(full=true)` + `eth_getBlockReceipts`
//!   → `<out>/blocks-<from>-<to>.jsonl.zst` (format unchanged, see `blocks`).
//!   Every committed file is appended to `<out>/filled.tsv`.
//! - `logs`: `eth_getLogs` by topic0 with an adaptive window
//!   → `<logs-out>/logs-<from>-<to>.jsonl.zst` (see `logs`). No reverted txs.
//! - `--gaps <gaps.tsv>`: fills every recorder gap not yet in `filled.tsv`, in
//!   `blocks` mode; a rerun downloads nothing that is already closed.
//!
//! All output is atomic (`*.partial` → fsync → rename). Calls are rate
//! limited (`--rps`, per JSON-RPC call), honour `Retry-After`, back off with
//! jitter (a 429 pauses all in-flight tasks), stop the run after
//! `--max-attempts` instead of skipping a block, and never exceed `--max-calls`.
//!
//! Not here yet (separate tasks): `--follow`, loading into ClickHouse.
//! Known limitation: ETH moved by contracts (internal transfers) is invisible
//! without traces (debug_traceBlock*), which depends on the provider.

pub mod atomic;
pub mod blocks;
pub mod logs;
pub mod ranges;
pub mod rpc;
pub mod stats;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};
use clap::{Parser, ValueEnum};
use tracing::{info, warn};

use crate::atomic::OutDirLock;
use crate::ranges::Range;
use crate::rpc::{redact_url, RetryPolicy, Rpc};
use crate::stats::{Stats, Summary};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    Blocks,
    Logs,
}

#[derive(Parser, Debug, Clone)]
#[command(about = "Fetch blocks+receipts or logs for L2 block ranges or feed gaps (read-only RPC)")]
pub struct Args {
    /// Use a provider endpoint. The public RPC is rate-limited and not for production.
    #[arg(long, env = "RPC_URL", default_value = hood_core::PUBLIC_RPC_URL, hide_env_values = true)]
    pub rpc_url: String,
    #[arg(long, value_enum, default_value_t = Mode::Blocks)]
    pub mode: Mode,
    #[arg(long, required_unless_present = "gaps", conflicts_with = "gaps")]
    pub from: Option<u64>,
    /// Inclusive.
    #[arg(long, required_unless_present = "gaps", conflicts_with = "gaps")]
    pub to: Option<u64>,
    /// Recorder gaps file(s) (`from \t to \t recv_ns`); fill them in blocks mode. Repeatable.
    #[arg(long)]
    pub gaps: Vec<PathBuf>,
    /// Output dir for blocks mode (also holds filled.tsv).
    #[arg(long, env = "ENRICHER_OUT_DIR", default_value = "data/blocks")]
    pub out_dir: PathBuf,
    #[arg(long, env = "ENRICHER_LOGS_OUT_DIR", default_value = "data/logs")]
    pub logs_out_dir: PathBuf,
    /// Blocks per JSON-RPC batch (each block = 2 calls, one HTTP request per batch).
    #[arg(long, default_value_t = 20)]
    pub batch: u64,
    /// Concurrent batches in flight.
    #[arg(long, default_value_t = 4)]
    pub concurrency: usize,
    /// Max JSON-RPC calls per second, retries included. A batch of N calls
    /// consumes N (blocks mode: 2 calls per block). 0 = unlimited.
    #[arg(long, default_value_t = 4.0)]
    pub rps: f64,
    /// Hard budget of JSON-RPC calls for the whole run (retries included);
    /// the run stops with an error instead of exceeding it.
    #[arg(long)]
    pub max_calls: Option<u64>,
    /// Attempts per request before the run stops with an error.
    #[arg(long, default_value_t = 8)]
    pub max_attempts: u32,
    /// First backoff step in ms (doubles per attempt, equal jitter, capped at 60 s).
    #[arg(long, default_value_t = 500)]
    pub backoff_ms: u64,
    #[arg(long, default_value_t = 30)]
    pub timeout_secs: u64,
    /// Max blocks per output file. Default: whole range in range mode, 1000 in --gaps mode.
    #[arg(long)]
    pub chunk: Option<u64>,
    /// Refuse to start if more than this many blocks would be downloaded.
    #[arg(long)]
    pub max_blocks: Option<u64>,
    /// --gaps: print the plan and exit without RPC calls.
    #[arg(long)]
    pub dry_run: bool,
    /// logs mode: topic0 filter (repeatable). Default: all topics known to crates/decoders.
    #[arg(long = "topic0")]
    pub topics: Vec<String>,
    /// logs mode: initial window in blocks.
    #[arg(long, default_value_t = 500)]
    pub logs_window: u64,
    /// logs mode: max window in blocks.
    #[arg(long, default_value_t = 5000)]
    pub logs_window_max: u64,
    /// Write the final counters as JSON to this file.
    #[arg(long)]
    pub stats_json: Option<PathBuf>,
}

pub fn make_rpc(a: &Args, stats: Arc<Stats>) -> Result<Rpc> {
    ensure!(a.rps >= 0.0 && a.rps.is_finite(), "--rps must be >= 0");
    ensure!(a.max_attempts >= 1, "--max-attempts must be >= 1");
    let policy = RetryPolicy {
        max_attempts: a.max_attempts,
        base: Duration::from_millis(a.backoff_ms.max(1)),
        ..RetryPolicy::default()
    };
    Ok(Rpc::new(&a.rpc_url, a.rps, Duration::from_secs(a.timeout_secs.max(1)), policy, stats)?.with_max_calls(a.max_calls))
}

fn check_budget(a: &Args, blocks: u64) -> Result<()> {
    if let Some(max) = a.max_blocks {
        if blocks > max {
            bail!("plan needs {blocks} blocks, more than --max-blocks {max}");
        }
    }
    Ok(())
}

/// Run the selected mode. `stats` is filled as it goes so the caller can
/// print counters even if this future is dropped (Ctrl-C) or fails.
pub async fn run(a: &Args, stats: Arc<Stats>) -> Result<()> {
    ensure!(a.batch >= 1, "--batch must be >= 1");
    if let Some(c) = a.chunk {
        ensure!(c >= 1, "--chunk must be >= 1");
    }
    info!(rpc = %redact_url(&a.rpc_url), mode = ?a.mode, rps = a.rps, max_calls = ?a.max_calls, batch = a.batch, concurrency = a.concurrency, "enricher");

    if !a.gaps.is_empty() {
        ensure!(a.mode == Mode::Blocks, "--gaps always fills in blocks mode; do not combine with --mode logs");
        return run_gaps(a, stats).await;
    }
    let (from, to) = (a.from.context("--from")?, a.to.context("--to")?);
    let range = Range::new(from, to).context("--to must be >= --from")?;
    check_budget(a, range.blocks())?;
    let rpc = make_rpc(a, stats)?;

    match a.mode {
        Mode::Blocks => {
            let lock = OutDirLock::acquire(&a.out_dir)?;
            report_partials(&lock);
            let opts = blocks::BlocksOpts { batch: a.batch, concurrency: a.concurrency };
            for r in ranges::chunk(&[range], a.chunk.unwrap_or(u64::MAX)) {
                let path = blocks::write_range(&rpc, &a.out_dir, r, &opts).await?;
                blocks::mark_filled(&a.out_dir, r, &path)?;
            }
        }
        Mode::Logs => {
            let lock = OutDirLock::acquire(&a.logs_out_dir)?;
            report_partials(&lock);
            let topics = if a.topics.is_empty() {
                logs::default_topics()
            } else {
                a.topics.iter().map(|t| logs::validate_topic(t)).collect::<Result<_>>()?
            };
            let opts = logs::LogsOpts { topics, window: a.logs_window, window_max: a.logs_window_max };
            for r in ranges::chunk(&[range], a.chunk.unwrap_or(u64::MAX)) {
                logs::write_range(&rpc, &a.logs_out_dir, r, &opts).await?;
            }
        }
    }
    Ok(())
}

async fn run_gaps(a: &Args, stats: Arc<Stats>) -> Result<()> {
    let lock = OutDirLock::acquire(&a.out_dir)?;
    report_partials(&lock);
    let mut gaps = Vec::new();
    for g in &a.gaps {
        ensure!(g.exists(), "gaps file {} does not exist", g.display());
        let v = ranges::read_ranges_file(g, "gaps")?;
        info!(file = %g.display(), gaps = v.len(), "read gaps");
        gaps.extend(v);
    }
    let gaps = ranges::merge(gaps);
    let filled = ranges::read_ranges_file(&a.out_dir.join(blocks::FILLED_TSV), "filled")?;
    let todo = ranges::subtract(gaps.clone(), filled);
    let gap_blocks: u64 = gaps.iter().map(Range::blocks).sum();
    let todo_blocks: u64 = todo.iter().map(Range::blocks).sum();
    let chunks = ranges::chunk(&todo, a.chunk.unwrap_or(1000));
    info!(
        gap_ranges = gaps.len(),
        gap_blocks,
        already_filled_blocks = gap_blocks - todo_blocks,
        todo_ranges = todo.len(),
        todo_blocks,
        min_calls = todo_blocks * 2,
        files = chunks.len(),
        "gaps plan"
    );
    for c in &chunks {
        info!(from = c.from, to = c.to, blocks = c.blocks(), "to fill");
    }
    check_budget(a, todo_blocks)?;
    if a.dry_run || chunks.is_empty() {
        if chunks.is_empty() {
            info!("nothing to fill: every gap is already in filled.tsv");
        }
        return Ok(());
    }
    let rpc = make_rpc(a, stats)?;
    let opts = blocks::BlocksOpts { batch: a.batch, concurrency: a.concurrency };
    for c in chunks {
        let path = blocks::write_range(&rpc, &a.out_dir, c, &opts).await?;
        blocks::mark_filled(&a.out_dir, c, &path)?;
        info!(from = c.from, to = c.to, "gap chunk filled");
    }
    Ok(())
}

fn report_partials(lock: &OutDirLock) {
    for p in &lock.removed_partials {
        warn!(file = %p.display(), "removed leftover partial file from an interrupted run");
    }
}

/// Log the final counters (and write them as JSON if asked).
pub fn report(sum: &Summary, stats_json: Option<&std::path::Path>) -> Result<()> {
    let c = &sum.counters;
    info!(
        elapsed_s = format!("{:.1}", sum.elapsed_s),
        http_requests = c.http_requests,
        http_body_bytes = c.http_body_bytes,
        http_429 = c.http_429,
        http_other_status = c.http_other_status,
        transport_errors = c.transport_errors,
        timeouts = c.timeouts,
        rpc_rate_limited = c.rpc_rate_limited,
        retries = c.retries,
        "stats"
    );
    for (m, n) in &c.calls {
        info!(method = %m, calls = n, "stats calls");
    }
    for (m, s) in &sum.sizes {
        info!(
            method = %m,
            results = s.results,
            raw_bytes = s.raw_bytes,
            raw_avg = format!("{:.0}", s.raw_avg),
            zstd_bytes = s.zstd_bytes,
            zstd_avg = format!("{:.0}", s.zstd_avg),
            "stats size per result (per block for block/receipts)"
        );
    }
    if let Some(p) = stats_json {
        std::fs::write(p, serde_json::to_vec_pretty(sum)?).with_context(|| format!("write {}", p.display()))?;
    }
    Ok(())
}
