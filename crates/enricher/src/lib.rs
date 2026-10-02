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
//!   `blocks` mode; a rerun downloads nothing that is already closed (and
//!   makes no call at all). `gaps.tsv` is read strictly, `filled.tsv`
//!   leniently (see `ranges`).
//!
//! Before the first download every run checks `eth_chainId` against
//! `hood_core::CHAIN_ID` (one call, part of `--max-calls`); `--dry-run` and a
//! run with nothing to do make no call. Exit codes: see `exit`.
//!
//! Plan checks (both `blocks` modes, before the lock and any call): more
//! than `--max-blocks` blocks, or a `--max-calls` that cannot pay for the
//! largest file plus the chain id check, is a configuration error (exit 1);
//! for ranges since task 026, before it such a run spent one call and exited
//! 75 on every start. `logs` mode makes a data-dependent number of calls, so
//! its budget is only enforced while running.
//!
//! `--dry-run` (task 026: also in range mode, where it used to be ignored and
//! the range was downloaded) logs the plan, runs the plan checks and exits
//! with no RPC call. In range mode it touches nothing on disk: no out-dir,
//! no lock, no `*.partial` cleanup (the plan does not depend on what is
//! there). In `--gaps` mode it takes the lock, because the plan subtracts
//! `filled.tsv`.
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
pub mod exit;
pub mod logs;
pub mod ranges;
pub mod rpc;
pub mod stats;
#[cfg(test)]
mod testdir;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};
use clap::{Parser, ValueEnum};
use tracing::{info, warn};

use hood_core::hex::quantity;
use hood_core::ranges::{self as hr, Range};

use crate::atomic::OutDirLock;
use crate::rpc::{redact_url, RetryPolicy, Rpc, M_CHAIN_ID};
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
    /// An unterminated last line (no `\n`, being written) is ignored with a WARN.
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
    /// Hard budget of JSON-RPC calls for the whole run (retries and the one
    /// `eth_chainId` check included); the run stops instead of exceeding it,
    /// the binary with exit code 75. A file the rest of the budget cannot pay
    /// for is not started. Blocks mode (range or `--gaps`): less than the
    /// largest file + 1 is an error (exit 1).
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
    /// Print the plan, check it and exit without RPC calls (range and --gaps).
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

/// Default `--chunk` in `--gaps` mode (blocks per file).
pub const GAPS_DEFAULT_CHUNK: u64 = 1000;

impl Args {
    /// Every argument check that needs no IO, before anything is done.
    pub fn validate(&self) -> Result<()> {
        ensure!(self.batch >= 1, "--batch must be >= 1");
        if let Some(c) = self.chunk {
            ensure!(c >= 1, "--chunk must be >= 1");
        }
        ensure!(self.rps >= 0.0 && self.rps.is_finite(), "--rps must be >= 0");
        ensure!(self.max_attempts >= 1, "--max-attempts must be >= 1");
        if !self.gaps.is_empty() {
            ensure!(self.mode == Mode::Blocks, "--gaps always fills in blocks mode; do not combine with --mode logs");
        }
        Ok(())
    }
}

fn make_rpc(a: &Args, stats: Arc<Stats>) -> Result<Rpc> {
    let policy = RetryPolicy {
        max_attempts: a.max_attempts,
        base: Duration::from_millis(a.backoff_ms.max(1)),
        ..RetryPolicy::default()
    };
    Ok(Rpc::new(&a.rpc_url, a.rps, Duration::from_secs(a.timeout_secs.max(1)), policy, stats)?
        .with_max_calls(a.max_calls))
}

/// Build the client and make sure the endpoint is Robinhood Chain (task 020
/// item 2): one `eth_chainId` call, counted in `--max-calls`, before the first
/// download. Another network is an error (exit 1) and nothing is written:
/// its blocks would pass every internal consistency check and `filled.tsv`
/// would mark the gaps closed.
async fn connect(a: &Args, stats: Arc<Stats>) -> Result<Rpc> {
    let rpc = make_rpc(a, stats)?;
    let got = rpc.chain_id().await.context("check the chain id of the RPC endpoint")?;
    ensure!(
        got == hood_core::CHAIN_ID,
        "wrong network: the RPC endpoint {} reports chain id {got} ({}), expected Robinhood Chain {} ({}); \
         check RPC_URL / --rpc-url. Nothing was downloaded",
        redact_url(&a.rpc_url),
        quantity(got),
        hood_core::CHAIN_ID,
        quantity(hood_core::CHAIN_ID),
    );
    info!(chain_id = got, "chain id ok");
    Ok(rpc)
}

fn check_max_blocks(a: &Args, blocks: u64) -> Result<()> {
    if let Some(max) = a.max_blocks {
        if blocks > max {
            bail!("plan needs {blocks} blocks, more than --max-blocks {max}");
        }
    }
    Ok(())
}

/// Calls one run needs at least: the chain id check plus the largest file
/// (a file is all or nothing). `None` for an empty plan (no call is made).
fn min_calls_per_run(chunks: &[Range]) -> Option<u64> {
    let largest = chunks.iter().map(|c| blocks::calls_needed(*c)).max()?;
    Some(largest.saturating_add(CHAIN_ID_CALLS))
}

/// `eth_chainId` calls per run (see [`connect`]).
const CHAIN_ID_CALLS: u64 = 1;

/// Task 020 item 3 (review I7): with `--max-calls` below what one file
/// needs, every `--gaps` run would spend its budget on a file it cannot
/// commit and exit 75 ("success" for systemd) without progress. That is a
/// configuration error: exit 1, so `OnFailure=` notifies. Range mode
/// (`--mode blocks`) uses the same check since task 026.
fn check_plan_fits_budget(a: &Args, chunks: &[Range]) -> Result<()> {
    let (Some(max), Some(need)) = (a.max_calls, min_calls_per_run(chunks)) else { return Ok(()) };
    if need <= max {
        return Ok(());
    }
    let largest = chunks.iter().map(Range::blocks).max().unwrap_or(0);
    let max_chunk = max.saturating_sub(CHAIN_ID_CALLS) / blocks::CALLS_PER_BLOCK;
    let fix = if max_chunk >= 1 {
        format!("lower --chunk to <= {max_chunk} or raise --max-calls to >= {need}")
    } else {
        format!("raise --max-calls to >= {need}")
    };
    bail!(
        "--max-calls {max} is less than the {need} calls a run needs to commit its largest file \
         ({largest} blocks x {} calls + {CHAIN_ID_CALLS} {M_CHAIN_ID}): no run could make progress; {fix}",
        blocks::CALLS_PER_BLOCK
    )
}

/// Download `chunks` one file each, recording every committed file in
/// `filled.tsv`. A file the remaining `--max-calls` cannot pay for is not
/// started: the run stops with the budget error (exit 75) instead of
/// spending calls on a file that would be discarded.
async fn fill_blocks(rpc: &Rpc, a: &Args, chunks: &[Range]) -> Result<()> {
    let opts = blocks::BlocksOpts { batch: a.batch, concurrency: a.concurrency };
    for &c in chunks {
        rpc.ensure_budget(blocks::calls_needed(c), &format!("blocks {}..={}", c.from, c.to))?;
        let path = blocks::write_range(rpc, &a.out_dir, c, &opts).await?;
        blocks::mark_filled(&a.out_dir, c, &path)?;
        info!(from = c.from, to = c.to, "file filled");
    }
    Ok(())
}

/// Run the selected mode. `stats` is filled as it goes so the caller can
/// print counters even if this future is dropped (Ctrl-C) or fails.
pub async fn run(a: &Args, stats: Arc<Stats>) -> Result<()> {
    a.validate()?;
    info!(rpc = %redact_url(&a.rpc_url), mode = ?a.mode, rps = a.rps, max_calls = ?a.max_calls, batch = a.batch, concurrency = a.concurrency, "enricher");

    if !a.gaps.is_empty() {
        return run_gaps(a, stats).await;
    }
    let (from, to) = (a.from.context("--from")?, a.to.context("--to")?);
    let range = Range::new(from, to).context("--to must be >= --from")?;
    // Before chunking: a huge range with a small --chunk is refused without
    // building (and logging) its file list.
    check_max_blocks(a, range.blocks())?;
    let chunks = hr::chunk(&[range], a.chunk.unwrap_or(u64::MAX));
    log_range_plan(a, range, &chunks);

    match a.mode {
        Mode::Blocks => {
            // Task 026 item 2: same plan check as --gaps (exit 1, no call).
            check_plan_fits_budget(a, &chunks)?;
            if a.dry_run {
                info!("dry run: plan only, no RPC call, nothing written");
                return Ok(());
            }
            let lock = OutDirLock::acquire(&a.out_dir)?;
            report_partials(&lock);
            let rpc = connect(a, stats).await?;
            fill_blocks(&rpc, a, &chunks).await?;
        }
        Mode::Logs => {
            let topics = if a.topics.is_empty() {
                logs::default_topics()
            } else {
                a.topics.iter().map(|t| logs::validate_topic(t)).collect::<Result<_>>()?
            };
            if a.dry_run {
                info!(topics = topics.len(), "dry run: plan only, no RPC call, nothing written");
                return Ok(());
            }
            let lock = OutDirLock::acquire(&a.logs_out_dir)?;
            report_partials(&lock);
            let rpc = connect(a, stats).await?;
            let opts = logs::LogsOpts { topics, window: a.logs_window, window_max: a.logs_window_max };
            for r in chunks {
                logs::write_range(&rpc, &a.logs_out_dir, r, &opts).await?;
            }
        }
    }
    Ok(())
}

/// Range mode plan, logged before the checks (like the `--gaps` plan).
/// `logs` mode has no call estimate: the window adapts to the data.
fn log_range_plan(a: &Args, range: Range, chunks: &[Range]) {
    match a.mode {
        Mode::Blocks => info!(
            from = range.from,
            to = range.to,
            blocks = range.blocks(),
            files = chunks.len(),
            planned_calls = planned_calls(chunks),
            "range plan"
        ),
        Mode::Logs => {
            info!(from = range.from, to = range.to, blocks = range.blocks(), files = chunks.len(), "range plan");
        }
    }
    log_files_to_fill(chunks);
}

/// Calls the whole blocks plan makes: every file plus the chain id check
/// (0 for an empty plan, which makes no call). Shared by range and `--gaps`.
fn planned_calls(chunks: &[Range]) -> u64 {
    if chunks.is_empty() {
        return 0;
    }
    chunks.iter().map(|c| blocks::calls_needed(*c)).fold(CHAIN_ID_CALLS, u64::saturating_add)
}

/// One `to fill` line per planned file (range and `--gaps`).
fn log_files_to_fill(chunks: &[Range]) {
    for c in chunks {
        info!(from = c.from, to = c.to, blocks = c.blocks(), "to fill");
    }
}

async fn run_gaps(a: &Args, stats: Arc<Stats>) -> Result<()> {
    let lock = OutDirLock::acquire(&a.out_dir)?;
    report_partials(&lock);
    let mut gaps = Vec::new();
    for g in &a.gaps {
        let read = ranges::read_gaps_file(g)?;
        if let Some(line) = &read.unterminated {
            warn!(file = %g.display(), line = %line.escape_debug(), "ignoring unterminated last line of gaps file (being written?); the next run picks it up");
        }
        info!(file = %g.display(), gaps = read.ranges.len(), "read gaps");
        gaps.extend(read.ranges);
    }
    let gaps = hr::merge(gaps);
    let filled_path = a.out_dir.join(blocks::FILLED_TSV);
    let filled = ranges::read_filled_file(&filled_path)?;
    if let Some(line) = &filled.unterminated {
        // Torn append (the enricher holds the out-dir lock, so nobody is
        // writing it now): that file is not counted and gets downloaded again.
        warn!(file = %filled_path.display(), line = %line.escape_debug(), "ignoring unterminated last line of filled.tsv (torn write?); its range is filled again");
    }
    for e in &filled.broken {
        warn!(file = %filled_path.display(), line_no = e.line_no, line = %e.line.escape_debug(), reason = %e.reason, "ignoring broken line of filled.tsv; its range is filled again if it is still a gap");
    }
    let todo = hr::subtract(&gaps, &filled.ranges);
    let gap_blocks: u64 = gaps.iter().map(Range::blocks).sum();
    let todo_blocks: u64 = todo.iter().map(Range::blocks).sum();
    let chunks = hr::chunk(&todo, a.chunk.unwrap_or(GAPS_DEFAULT_CHUNK));
    info!(
        gap_ranges = gaps.len(),
        gap_blocks,
        already_filled_blocks = gap_blocks - todo_blocks,
        todo_ranges = todo.len(),
        todo_blocks,
        planned_calls = planned_calls(&chunks),
        files = chunks.len(),
        "gaps plan"
    );
    log_files_to_fill(&chunks);
    check_max_blocks(a, todo_blocks)?;
    check_plan_fits_budget(a, &chunks)?;
    if a.dry_run || chunks.is_empty() {
        if chunks.is_empty() {
            info!("nothing to fill: every gap is already in filled.tsv");
        }
        return Ok(());
    }
    let rpc = connect(a, stats).await?;
    fill_blocks(&rpc, a, &chunks).await
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
        global_pauses = c.global_pauses,
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
