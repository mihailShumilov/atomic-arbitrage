//! Loader: raw files on disk -> local ClickHouse (`hood.*`) over HTTP (task 032).
//!
//! Inputs (all read-only):
//! - `--blocks`: `blocks-*.jsonl.zst` of the enricher (a directory or files; `*.partial` is never
//!   loaded) and other files of the same line format (`data/samples/hourly-*.jsonl.zst`) ->
//!   `hood.blocks`, `hood.txs`, `hood.logs`, `hood.funding_edges` (L1 inflows decoder);
//! - `--feed-dir`: copies of recorder out-dirs -> `blocks.feed_recv_ns` ([`feed`]);
//! - `--gaps`: recorder `gaps.tsv` files -> `hood.feed_gaps` ([`load::load_gaps`]).
//!
//! Guarantees (task 032, "Требования из ревью"): every file is decoded whole (zstd checksum) and
//! every row validated before the first INSERT ([`zst`], [`tsv`], [`blocks_file`]); a file goes in
//! with all of its tables or not at all ([`load`]); explicit column lists in `COLUMNS` order with
//! `TabSeparatedWithNames`; repeated loads give the same result after `FINAL`. The schema is
//! checked against the loader's column lists before loading.
//!
//! Connection: [`config`] (password from the environment or `.env`, never argv; loopback only).
//! `--dry-run` reads and validates everything without connecting.
//!
//! Modules: [`config`], [`ch`] (HTTP client), [`zst`], [`tsv`], [`rows`], [`blocks_file`],
//! [`feed`], [`load`].

pub mod blocks_file;
pub mod ch;
pub mod config;
pub mod feed;
pub mod load;
pub mod rows;
pub mod tsv;
pub mod zst;

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::Parser;
use decoders::l1_inflows::GatewayRegistry;
use hood_core::ranges::{self, Range};
use tracing::{info, warn};

use crate::blocks_file::{range_from_name, read_blocks_file};
use crate::ch::Client;
use crate::config::ChConfig;
use crate::feed::FeedIndex;

/// Command line.
#[derive(Debug, Clone, Parser)]
#[command(name = "loader", about = "Load raw blocks, feed timing and feed gaps into the local ClickHouse")]
pub struct Args {
    /// Blocks file or directory (directory: every `blocks-*.jsonl.zst` in it). Repeatable.
    #[arg(long = "blocks", value_name = "PATH")]
    pub blocks: Vec<PathBuf>,
    /// Recorder out-dir copy for `feed_recv_ns` (`YYYY/MM/DD/feed-*.tsv.zst`). Repeatable.
    /// A copy of closed hours only: the open hour of a live recorder ends in an unfinished zstd
    /// frame and fails the run.
    #[arg(long = "feed-dir", value_name = "DIR")]
    pub feed_dirs: Vec<PathBuf>,
    /// Recorder `gaps.tsv` -> `hood.feed_gaps` (loaded after the blocks). Repeatable.
    #[arg(long = "gaps", value_name = "FILE")]
    pub gaps: Vec<PathBuf>,
    /// `.env` with CLICKHOUSE_PASSWORD (and optionally CLICKHOUSE_URL / _HTTP_PORT / _USER);
    /// the environment wins over it.
    #[arg(long, value_name = "FILE", default_value = ".env")]
    pub env_file: PathBuf,
    /// Read and validate everything, connect to nothing.
    #[arg(long)]
    pub dry_run: bool,
}

/// Totals of one run.
#[derive(Debug, Clone, Default)]
pub struct Summary {
    pub files: usize,
    pub files_unchecked: usize,
    pub blocks: usize,
    pub txs: usize,
    pub logs: usize,
    pub edges: usize,
    pub dropped_unregistered_token_rows: usize,
    pub unaccounted: BTreeMap<&'static str, usize>,
    pub already_loaded: usize,
    pub with_feed_time: usize,
    pub inserted: BTreeMap<&'static str, usize>,
    pub gaps: Option<load::GapsOutcome>,
}

/// Blocks files named by `paths`: files as given, directories expanded to their
/// `blocks-*.jsonl.zst` (sorted). A `*.partial` file is an error.
///
/// # Errors
/// Unreadable directory, a `*.partial` file, nothing found.
pub fn expand_blocks_paths(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            let mut v: Vec<PathBuf> = std::fs::read_dir(p)
                .with_context(|| format!("read dir {}", p.display()))?
                .map(|e| e.map(|e| e.path()))
                .collect::<std::io::Result<_>>()?;
            v.retain(|f| range_from_name(f).is_some());
            v.sort_by_key(|f| range_from_name(f));
            out.extend(v);
        } else {
            if p.to_string_lossy().ends_with(".partial") {
                bail!("{}: *.partial files are always incomplete, never loaded", p.display());
            }
            out.push(p.clone());
        }
    }
    if out.is_empty() && !paths.is_empty() {
        bail!("no blocks files found in {paths:?}");
    }
    Ok(out)
}

/// Feed index limit: the block ranges from the file names, or `None` (index everything) if some
/// file name has no range.
fn wanted_ranges(files: &[PathBuf]) -> Option<Vec<Range>> {
    let v: Option<Vec<Range>> = files.iter().map(|f| range_from_name(f)).collect();
    v.map(ranges::merge)
}

/// Reads every `gaps.tsv` (strict; an unterminated last line is skipped with a WARN).
fn read_gaps(paths: &[PathBuf]) -> Result<Vec<ranges::GapRow>> {
    let mut rows = Vec::new();
    for p in paths {
        let text = std::fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?;
        let parsed = ranges::parse_gaps_file(&text).with_context(|| format!("{}", p.display()))?;
        if let Some(t) = parsed.unterminated {
            warn!("{}: ignoring unterminated last line {t:?}", p.display());
        }
        rows.extend(parsed.rows);
    }
    Ok(rows)
}

/// Runs the loader.
///
/// # Errors
/// The first error: nothing of a failing file stays in ClickHouse (see [`load`]); files loaded
/// before it stay loaded (re-running is safe).
pub async fn run(a: &Args) -> Result<Summary> {
    let files = expand_blocks_paths(&a.blocks)?;
    if files.is_empty() && a.gaps.is_empty() {
        bail!("nothing to do: give --blocks and/or --gaps");
    }
    let gaps = read_gaps(&a.gaps)?;
    let wanted = wanted_ranges(&files);
    let feed_dirs = a.feed_dirs.clone();
    let feed = tokio::task::spawn_blocking(move || FeedIndex::from_dirs(&feed_dirs, wanted.as_deref()))
        .await
        .context("feed index task")??;
    info!(
        "feed: {} files ({} without checksum), {} blocks indexed, {} repeats, {} out of range",
        feed.stats.files,
        feed.stats.files_unchecked,
        feed.len(),
        feed.stats.repeats,
        feed.stats.messages_out_of_range
    );

    let ch = if a.dry_run {
        None
    } else {
        let ch = Client::new(ChConfig::load(&a.env_file)?)?;
        let v = ch.query("SELECT version() FORMAT TabSeparated").await?;
        info!("ClickHouse {} version {}", ch.url(), v.trim());
        load::check_schema(&ch).await?;
        Some(ch)
    };

    let mut s = Summary::default();
    for path in files {
        let p = path.clone();
        let rows = tokio::task::spawn_blocking(move || read_blocks_file(&p, &GatewayRegistry::builtin()))
            .await
            .context("read task")??;
        let st = rows.stats.clone();
        s.files += 1;
        if st.frames > 0 && st.frames != st.frames_with_checksum {
            s.files_unchecked += 1;
            warn!(
                "{}: {} of {} zstd frames without content checksum",
                path.display(),
                st.frames - st.frames_with_checksum,
                st.frames
            );
        }
        s.blocks += st.blocks;
        s.txs += st.txs;
        s.logs += st.logs;
        s.edges += st.edges_l1_eth + st.edges_l1_token;
        s.dropped_unregistered_token_rows += st.dropped_unregistered_token_rows;
        for (k, v) in &st.unaccounted {
            *s.unaccounted.entry(k).or_default() += v;
        }
        let Some(ch) = &ch else {
            info!(
                "{}: valid: blocks={} txs={} logs={} edges={}",
                path.display(),
                st.blocks,
                st.txs,
                st.logs,
                st.edges_l1_eth + st.edges_l1_token
            );
            continue;
        };
        let o = load::load_file(ch, &feed, rows).await.with_context(|| format!("load {}", path.display()))?;
        info!(
            "{}: loaded blocks={} txs={} logs={} edges={} (already loaded {}, feed time {})",
            path.display(),
            st.blocks,
            st.txs,
            st.logs,
            st.edges_l1_eth + st.edges_l1_token,
            o.already_loaded,
            o.with_feed_time
        );
        s.already_loaded += o.already_loaded;
        s.with_feed_time += o.with_feed_time;
        for (k, v) in o.inserted {
            *s.inserted.entry(k).or_default() += v;
        }
    }
    if let Some(ch) = &ch {
        if !gaps.is_empty() {
            s.gaps = Some(load::load_gaps(ch, &gaps).await?);
        }
    } else if !gaps.is_empty() {
        let d = load::dedupe_gaps(&gaps)?;
        info!("gaps: valid: {} rows, {} distinct", gaps.len(), d.len());
    }
    Ok(s)
}

/// One-line totals for the log.
pub fn report(s: &Summary) {
    info!(
        "total: files={} (without checksum {}) blocks={} txs={} logs={} edges={} dropped_unregistered_token_rows={} \
         already_loaded={} with_feed_time={}",
        s.files,
        s.files_unchecked,
        s.blocks,
        s.txs,
        s.logs,
        s.edges,
        s.dropped_unregistered_token_rows,
        s.already_loaded,
        s.with_feed_time
    );
    for (k, v) in &s.inserted {
        info!("inserted {k}: {v} rows");
    }
    for (k, v) in &s.unaccounted {
        info!("unaccounted (not loaded) {k}: {v}");
    }
    if let Some(g) = &s.gaps {
        info!("feed_gaps: rows read {}, distinct {}, filled {}", g.rows_read, g.gaps, g.filled);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../decoders/tests/fixtures/l1-inflows-blocks.jsonl")
    }

    fn args(blocks: Vec<PathBuf>, gaps: Vec<PathBuf>) -> Args {
        Args { blocks, feed_dirs: vec![], gaps, env_file: PathBuf::from("/nonexistent/.env"), dry_run: true }
    }

    /// `--dry-run` reads and validates without any connection (no `.env`, no server).
    #[tokio::test]
    async fn dry_run_validates_offline() {
        let dir = std::env::temp_dir().join(format!("loader-dry-run-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let gaps = dir.join("gaps.tsv");
        std::fs::write(&gaps, "10\t20\t5\n10\t20\t3\n30\t30\t1\n40\t4").unwrap();
        let s = run(&args(vec![fixture()], vec![gaps.clone()])).await.unwrap();
        assert_eq!((s.files, s.blocks, s.txs, s.edges), (1, 4, 11, 3));
        assert!(s.inserted.is_empty() && s.gaps.is_none());

        std::fs::write(&gaps, "10\t20\t5\n10\t21\t3\n").unwrap();
        assert!(run(&args(vec![fixture()], vec![gaps.clone()])).await.is_err(), "conflicting to_seq");
        let partial = dir.join("blocks-1-2.jsonl.zst.partial");
        assert!(run(&args(vec![partial], vec![])).await.is_err(), "*.partial is never loaded");
        assert!(run(&args(vec![], vec![])).await.is_err(), "nothing to do");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
