//! Offline scan of block files for Uniswap v3/v4 swaps: decoder counters, `hood.swaps` rows and
//! per-venue / per-skip-reason counters, optional TSV of the rows and the audit TSV of every
//! decoded swap. Reads `blocks-*.jsonl.zst` (multi-frame zstd) or plain `.jsonl`, in the given
//! order (pass files in ascending block order: pools seen in v4 `Initialize` logs apply to later
//! swaps only). No network, no ClickHouse. TSV columns and formatting come from
//! `decoders::rows::SwapRow` and `decoders::swaps::PoolSwap`.
//!
//! cargo run -p decoders --example swaps_scan -- [--pools pools.tsv]... [--tokens tokens.tsv]...
//!     [--no-builtin-tokens] [--v4-manager ADDR=observed|verified]... [--min-status verified|observed]
//!     [--rows-out swaps.tsv] [--pool-swaps-out pool_swaps.tsv] FILE...
//!
//! Registry formats: `decoders::pools::PoolRegistry::parse_tsv`, `TokenRegistry::parse_tsv`.
//! Nothing beyond the `verified` built-ins (native ETH, L2 WETH) is assumed: without `--pools` /
//! `--v4-manager` every swap is counted as `no_pool_meta` and no row is written.

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read};

use anyhow::{bail, ensure, Context, Result};
use decoders::parse_block_line;
use decoders::pools::{InitOutcome, PoolRegistry, PoolSource, TokenRegistry};
use decoders::registry::RegistryStatus;
use decoders::rows::{SwapRow, Venue};
use decoders::swap_rows::{swap_row, RowInputs, SkipReason, SwapRowCounters};
use decoders::swaps::{decode_block, PoolSwap, SwapCounters, SwapEvent};

struct Args {
    pools: PoolRegistry,
    tokens: TokenRegistry,
    min_status: RegistryStatus,
    rows_out: Option<String>,
    pool_swaps_out: Option<String>,
    files: Vec<String>,
}

fn status(s: &str) -> Result<RegistryStatus> {
    RegistryStatus::parse(s).with_context(|| format!("status {s:?} (verified|observed)"))
}

fn read(path: &str) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("read {path}"))
}

fn parse_args() -> Result<Args> {
    let mut args = std::env::args().skip(1);
    let mut pools = PoolRegistry::default();
    let mut extra_tokens = Vec::new();
    let mut builtin_tokens = true;
    let mut min_status = RegistryStatus::Verified;
    let (mut rows_out, mut pool_swaps_out, mut files) = (None, None, Vec::new());
    while let Some(a) = args.next() {
        match a.as_str() {
            "--pools" => {
                let p = args.next().context("--pools PATH")?;
                pools.extend(PoolRegistry::parse_tsv(&read(&p)?).with_context(|| p.clone())?)?;
            }
            "--tokens" => extra_tokens.push(args.next().context("--tokens PATH")?),
            "--no-builtin-tokens" => builtin_tokens = false,
            "--v4-manager" => {
                let v = args.next().context("--v4-manager ADDR=STATUS")?;
                let (addr, st) = v.split_once('=').context("--v4-manager ADDR=observed|verified")?;
                let addr: alloy_primitives::Address = addr.parse().with_context(|| format!("address {addr:?}"))?;
                pools.allow_v4_manager(addr, status(st)?)?;
            }
            "--min-status" => min_status = status(&args.next().context("--min-status STATUS")?)?,
            "--rows-out" => rows_out = Some(args.next().context("--rows-out PATH")?),
            "--pool-swaps-out" => pool_swaps_out = Some(args.next().context("--pool-swaps-out PATH")?),
            s if s.starts_with("--") => bail!("unknown flag {s}"),
            _ => files.push(a),
        }
    }
    let mut tokens = if builtin_tokens { TokenRegistry::builtin() } else { TokenRegistry::default() };
    for p in &extra_tokens {
        tokens.extend(TokenRegistry::parse_tsv(&read(p)?).with_context(|| p.clone())?)?;
    }
    ensure!(!files.is_empty(), "no input files");
    Ok(Args { pools, tokens, min_status, rows_out, pool_swaps_out, files })
}

fn writer<F>(path: Option<&String>, header: F) -> Result<Option<BufWriter<File>>>
where
    F: FnOnce(&mut BufWriter<File>) -> std::io::Result<()>,
{
    path.map(|p| {
        let mut w = BufWriter::new(File::create(p).with_context(|| format!("create {p}"))?);
        header(&mut w)?;
        Ok(w)
    })
    .transpose()
}

fn main() -> Result<()> {
    let Args { mut pools, tokens, min_status, rows_out, pool_swaps_out, files } = parse_args()?;
    let mut rows_w = writer(rows_out.as_ref(), SwapRow::write_tsv_header)?;
    let mut swaps_w = writer(pool_swaps_out.as_ref(), PoolSwap::write_tsv_header)?;

    let mut decoded = SwapCounters::default();
    let mut rows = SwapRowCounters::default();
    let mut inits: std::collections::BTreeMap<InitOutcome, u64> = std::collections::BTreeMap::new();
    let (mut blocks, mut min_block, mut max_block) = (0u64, u64::MAX, 0u64);
    for path in &files {
        let f = File::open(path).with_context(|| format!("open {path}"))?;
        let zst = std::path::Path::new(path).extension().is_some_and(|e| e.eq_ignore_ascii_case("zst"));
        let reader: Box<dyn Read> = if zst { Box::new(zstd::stream::read::Decoder::new(f)?) } else { Box::new(f) };
        let mut file_decoded = SwapCounters::default();
        let mut file_rows = 0u64;
        for (i, line) in BufReader::new(reader).lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let block = parse_block_line(&line).with_context(|| format!("{path}:{}", i + 1))?;
            blocks += 1;
            min_block = min_block.min(block.number);
            max_block = max_block.max(block.number);
            // Pools first: a pool is initialized before its first swap, possibly in the same block.
            for log in block.txs.iter().flat_map(|t| &t.logs) {
                match pools.observe_log(log) {
                    InitOutcome::NotInitialize => {}
                    o => *inits.entry(o).or_default() += 1,
                }
            }
            let r = decode_block(&block);
            file_decoded.merge(&r.counters);
            for m in &r.malformed {
                println!("malformed swap: block {} tx {} log {}: {}", m.block_number, m.tx_index, m.log_index, m.error);
            }
            let inputs = RowInputs { pools: &pools, tokens: &tokens, min_status };
            for s in &r.swaps {
                if let Some(w) = swaps_w.as_mut() {
                    s.write_tsv(w)?;
                }
                let out = swap_row(s, &inputs);
                rows.record(s, &out);
                if let Ok(m) = &out {
                    file_rows += 1;
                    if let Some(w) = rows_w.as_mut() {
                        m.row.write_tsv(w)?;
                    }
                }
            }
        }
        println!(
            "file {path}: logs={} v3={} v4={} malformed={} rows={file_rows}",
            file_decoded.logs, file_decoded.v3, file_decoded.v4, file_decoded.malformed
        );
        decoded.merge(&file_decoded);
    }
    ensure!(rows.is_balanced(), "counter invariant broken: swaps != rows + skipped");

    println!("== total: {} files, blocks={blocks} (range {min_block}..{max_block})", files.len());
    println!(
        "registry: pools={} (tsv {}, initialize {}), v4 managers={}, tokens={}, min_status={}",
        pools.len(),
        pools.count_by_source(PoolSource::Registry),
        pools.count_by_source(PoolSource::Initialize),
        pools.v4_managers().len(),
        tokens.len(),
        min_status.as_str()
    );
    for (m, s) in pools.v4_managers() {
        println!("  v4 manager {m:#x} {}", s.as_str());
    }
    let inits: Vec<String> = inits.iter().map(|(k, v)| format!("{}={v}", k.as_str())).collect();
    println!("initialize: {}", if inits.is_empty() { "none".to_owned() } else { inits.join(" ") });
    println!(
        "decoded: logs={} v3={} v4={} malformed={} swaps={}",
        decoded.logs,
        decoded.v3,
        decoded.v4,
        decoded.malformed,
        decoded.v3 + decoded.v4
    );
    let total_rows: u64 = rows.rows.values().sum();
    println!("rows: {total_rows}");
    for v in Venue::ALL {
        if let Some(n) = rows.rows.get(&v) {
            println!("  venue {:<14} {n}", v.as_str());
        }
    }
    println!("skipped: {}", rows.skipped.values().sum::<u64>());
    for ev in [SwapEvent::V3, SwapEvent::V4] {
        for reason in SkipReason::ALL {
            if let Some(n) = rows.skipped.get(&(ev, reason)) {
                println!("  {} {:<17} {n}", ev.as_str(), reason.as_str());
            }
        }
    }
    println!(
        "flags price_unknown={} fee_unknown={} v4_fee_zero={} v4_rows_with_hooks={} v4_rows_hooks_unknown={}",
        rows.price_unknown, rows.fee_unknown, rows.v4_fee_zero, rows.v4_rows_with_hooks, rows.v4_rows_hooks_unknown
    );
    for w in [rows_w, swaps_w].into_iter().flatten() {
        w.into_inner().map_err(std::io::IntoInnerError::into_error)?.sync_all()?;
    }
    Ok(())
}
