//! `blocks` mode: `eth_getBlockByNumber(full=true)` + `eth_getBlockReceipts`.
//!
//! Output format (unchanged since bootstrap, see references/data-model.md):
//! `<dir>/blocks-<from>-<to>.jsonl.zst`, one JSON object per block, in order:
//!   {"number": N, "block": {...}, "receipts": [...]}
//! This is the only mode that sees reverted transactions (receipt `status`).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use futures::{stream, StreamExt, TryStreamExt};
use serde_json::{json, Value};
use tracing::info;

use crate::atomic::{append_line_synced, AtomicZstdFile};
use crate::ranges::Range;
use crate::rpc::{Call, Check, Item, Rpc};

pub const M_BLOCK: &str = "eth_getBlockByNumber";
pub const M_RECEIPTS: &str = "eth_getBlockReceipts";
pub const FILLED_TSV: &str = "filled.tsv";

pub fn file_name(r: Range) -> String {
    format!("blocks-{}-{}.jsonl.zst", r.from, r.to)
}

/// Consistency checks between a block and its receipts. A failure is
/// treated as transient (retried, then the run stops) — never skipped.
pub fn validate_block(n: u64, block: &Value, receipts: &Value) -> Result<(), String> {
    let want = format!("0x{n:x}");
    if block["number"].as_str() != Some(want.as_str()) {
        return Err(format!("block {n}: number field is {}", block["number"]));
    }
    let hash = block["hash"].as_str().ok_or_else(|| format!("block {n}: no hash"))?;
    let txs = block["transactions"].as_array().ok_or_else(|| format!("block {n}: no transactions array"))?;
    let rs = receipts.as_array().ok_or_else(|| format!("block {n}: receipts is not an array"))?;
    if rs.len() != txs.len() {
        return Err(format!("block {n}: {} receipts for {} transactions", rs.len(), txs.len()));
    }
    for (i, (r, t)) in rs.iter().zip(txs).enumerate() {
        if r["blockHash"].as_str() != Some(hash) {
            return Err(format!("block {n}: receipt {i} blockHash {} != {hash}", r["blockHash"]));
        }
        if r["transactionHash"] != t["hash"] {
            return Err(format!("block {n}: receipt {i} tx hash {} != {}", r["transactionHash"], t["hash"]));
        }
    }
    Ok(())
}

fn calls_for(from: u64, to: u64) -> Vec<Call> {
    let mut calls = Vec::with_capacity(((to - from + 1) * 2) as usize);
    for n in from..=to {
        let tag = format!("0x{n:x}");
        calls.push(Call { id: n * 2, method: M_BLOCK, params: json!([tag, true]) });
        calls.push(Call { id: n * 2 + 1, method: M_RECEIPTS, params: json!([tag]) });
    }
    calls
}

fn check_items(from: u64, items: &[Item]) -> Check {
    for (i, pair) in items.chunks(2).enumerate() {
        let n = from + i as u64;
        for (it, m) in pair.iter().zip([M_BLOCK, M_RECEIPTS]) {
            if let Some(e) = &it.error {
                return Check::Retry(format!("block {n}: {m} error {} {}", e.code, e.message));
            }
            if it.result.is_none() {
                return Check::Retry(format!("block {n}: {m} returned null (not available yet?)"));
            }
        }
        let parse = |it: &Item| serde_json::from_str::<Value>(it.result.as_ref().unwrap().get());
        match (parse(&pair[0]), parse(&pair[1])) {
            (Ok(b), Ok(r)) => {
                if let Err(e) = validate_block(n, &b, &r) {
                    return Check::Retry(e);
                }
            }
            _ => return Check::Retry(format!("block {n}: invalid JSON in result")),
        }
    }
    Check::Accept
}

/// Fetch `from..=to` in one JSON-RPC batch. Returns one output line per block.
pub async fn fetch_batch(rpc: &Rpc, from: u64, to: u64) -> Result<Vec<Value>> {
    let calls = calls_for(from, to);
    let what = format!("blocks {from}..={to}");
    let items = rpc.call(&calls, &what, false, |items| check_items(from, items)).await?;
    let mut out = Vec::with_capacity(items.len() / 2);
    for (i, pair) in items.chunks(2).enumerate() {
        let n = from + i as u64;
        let (rb, rr) = (pair[0].result.as_ref().unwrap(), pair[1].result.as_ref().unwrap());
        rpc.stats.record_result(M_BLOCK, rb.get());
        rpc.stats.record_result(M_RECEIPTS, rr.get());
        let b: Value = serde_json::from_str(rb.get())?;
        let r: Value = serde_json::from_str(rr.get())?;
        out.push(json!({"number": n, "block": b, "receipts": r}));
    }
    Ok(out)
}

pub struct BlocksOpts {
    pub batch: u64,
    pub concurrency: usize,
}

/// Download one range into `<dir>/blocks-<from>-<to>.jsonl.zst` atomically.
pub async fn write_range(rpc: &Rpc, dir: &Path, r: Range, o: &BlocksOpts) -> Result<PathBuf> {
    let path = dir.join(file_name(r));
    let mut f = AtomicZstdFile::create(&path, 3)?;
    let batches: Vec<(u64, u64)> = (r.from..=r.to)
        .step_by(o.batch as usize)
        .map(|s| (s, (s + o.batch - 1).min(r.to)))
        .collect();
    info!(from = r.from, to = r.to, batches = batches.len(), file = %path.display(), "range start");

    // `buffered` keeps output in block order while fetching concurrently.
    let mut s = stream::iter(batches).map(|(a, b)| fetch_batch(rpc, a, b)).buffered(o.concurrency.max(1));
    let mut expect = r.from;
    while let Some(batch) = s.try_next().await? {
        for v in batch {
            if v["number"].as_u64() != Some(expect) {
                bail!("internal: expected block {expect}, got {}", v["number"]);
            }
            serde_json::to_writer(&mut f, &v)?;
            f.write_all(b"\n")?;
            expect += 1;
            if (expect - r.from).is_multiple_of(1000) {
                info!(done = expect - r.from, of = r.blocks(), "progress");
            }
        }
    }
    if expect != r.to + 1 {
        bail!("internal: range {}..={} ended at {}", r.from, r.to, expect - 1);
    }
    let path = f.commit()?;
    info!(file = %path.display(), blocks = r.blocks(), "range done");
    Ok(path)
}

/// Record a committed file in `filled.tsv`. Called only after `commit`.
pub fn mark_filled(dir: &Path, r: Range, file: &Path) -> Result<()> {
    let name = file.file_name().and_then(|n| n.to_str()).context("file name")?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    append_line_synced(&dir.join(FILLED_TSV), &format!("{}\t{}\t{name}\t{now}", r.from, r.to))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_catches_mismatches() {
        let b = json!({"number":"0xa","hash":"0xh","transactions":[{"hash":"0xt1"}]});
        let ok = json!([{"blockHash":"0xh","transactionHash":"0xt1"}]);
        assert!(validate_block(10, &b, &ok).is_ok());
        assert!(validate_block(11, &b, &ok).unwrap_err().contains("number"));
        assert!(validate_block(10, &b, &json!([])).unwrap_err().contains("0 receipts for 1"));
        let wrong_hash = json!([{"blockHash":"0xother","transactionHash":"0xt1"}]);
        assert!(validate_block(10, &b, &wrong_hash).unwrap_err().contains("blockHash"));
        let wrong_tx = json!([{"blockHash":"0xh","transactionHash":"0xt2"}]);
        assert!(validate_block(10, &b, &wrong_tx).unwrap_err().contains("tx hash"));
    }
}
