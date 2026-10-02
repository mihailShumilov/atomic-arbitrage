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

use hood_core::fsutil::append_line_synced;
use hood_core::hex::quantity;
use hood_core::ranges::{FilledRow, Range};

use crate::atomic::AtomicZstdFile;
use crate::rpc::{Call, Item, OnTimeout, Rpc};

pub const M_BLOCK: &str = "eth_getBlockByNumber";
pub const M_RECEIPTS: &str = "eth_getBlockReceipts";
pub const FILLED_TSV: &str = "filled.tsv";
/// `eth_getBlockByNumber` + `eth_getBlockReceipts`.
pub const CALLS_PER_BLOCK: u64 = 2;

pub fn file_name(r: Range) -> String {
    format!("blocks-{}-{}.jsonl.zst", r.from, r.to)
}

/// Consistency checks between a block and its receipts. A failure is
/// treated as transient (retried, then the run stops) — never skipped.
pub fn validate_block(n: u64, block: &Value, receipts: &Value) -> Result<(), String> {
    let want = quantity(n);
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
    let mut calls = Vec::with_capacity(((to - from + 1) * CALLS_PER_BLOCK) as usize);
    for n in from..=to {
        let tag = quantity(n);
        calls.push(Call { id: n * 2, method: M_BLOCK, params: json!([tag, true]) });
        calls.push(Call { id: n * 2 + 1, method: M_RECEIPTS, params: json!([tag]) });
    }
    calls
}

/// The result of one call as JSON, or why the batch must be retried.
fn parse_result(n: u64, method: &str, it: &Item) -> Result<Value, String> {
    if let Some(e) = &it.error {
        return Err(format!("block {n}: {method} error {} {}", e.code, e.message));
    }
    let raw = it.result.as_ref().ok_or_else(|| format!("block {n}: {method} returned null (not available yet?)"))?;
    serde_json::from_str(raw.get()).map_err(|_| format!("block {n}: invalid JSON in {method} result"))
}

/// Turn a complete batch response (pairs block, receipts per block from
/// `from` on) into output lines, parsing each result once. `Err` = retry.
fn accept_batch(from: u64, items: &[Item]) -> Result<Vec<Value>, String> {
    let mut out = Vec::with_capacity(items.len() / 2);
    for (i, pair) in items.chunks(2).enumerate() {
        let n = from + i as u64;
        let [b, r] = pair else { return Err(format!("block {n}: receipts response missing")) };
        let block = parse_result(n, M_BLOCK, b)?;
        let receipts = parse_result(n, M_RECEIPTS, r)?;
        validate_block(n, &block, &receipts)?;
        // Same Value construction as before task 020: identical output bytes.
        out.push(json!({"number": n, "block": block, "receipts": receipts}));
    }
    Ok(out)
}

/// Fetch `from..=to` in one JSON-RPC batch. Returns one output line per block.
pub(crate) async fn fetch_batch(rpc: &Rpc, from: u64, to: u64) -> Result<Vec<Value>> {
    let calls = calls_for(from, to);
    let what = format!("blocks {from}..={to}");
    Ok(rpc.call(&calls, &what, OnTimeout::Retry, |items| accept_batch(from, items)).await?)
}

/// JSON-RPC calls needed to fetch `r` once (no retries).
pub fn calls_needed(r: Range) -> u64 {
    r.blocks().saturating_mul(CALLS_PER_BLOCK)
}

pub struct BlocksOpts {
    pub batch: u64,
    pub concurrency: usize,
}

/// Download one range into `<dir>/blocks-<from>-<to>.jsonl.zst` atomically.
pub async fn write_range(rpc: &Rpc, dir: &Path, r: Range, o: &BlocksOpts) -> Result<PathBuf> {
    let path = dir.join(file_name(r));
    let mut f = AtomicZstdFile::create(&path, 3)?;
    let batches: Vec<(u64, u64)> =
        (r.from..=r.to).step_by(o.batch as usize).map(|s| (s, (s + o.batch - 1).min(r.to))).collect();
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
    let file_name = file.file_name().and_then(|n| n.to_str()).context("file name")?;
    let filled_unix_s = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let row = FilledRow { range: r, file_name, filled_unix_s };
    append_line_synced(&dir.join(FILLED_TSV), &row.to_string())?;
    Ok(())
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

    fn items(json: &str) -> Vec<Item> {
        serde_json::from_str(json).unwrap()
    }

    /// Task 020 item 7: one parse per result, and the output line keeps the
    /// exact bytes of the pre-020 format (serde_json without preserve_order:
    /// keys sorted, `block` < `number` < `receipts`).
    #[test]
    fn accepted_batch_gives_the_same_line_bytes() {
        let ok = items(
            r#"[{"id":20,"result":{"number":"0xa","hash":"0xh","transactions":[{"hash":"0xt1"}]}},
                {"id":21,"result":[{"transactionHash":"0xt1","blockHash":"0xh","status":"0x1"}]}]"#,
        );
        let lines = accept_batch(10, &ok).unwrap();
        assert_eq!(
            serde_json::to_string(&lines[0]).unwrap(),
            r#"{"block":{"hash":"0xh","number":"0xa","transactions":[{"hash":"0xt1"}]},"number":10,"receipts":[{"blockHash":"0xh","status":"0x1","transactionHash":"0xt1"}]}"#
        );
        let null = items(r#"[{"id":20,"result":null},{"id":21,"result":[]}]"#);
        assert!(accept_batch(10, &null).unwrap_err().contains("returned null"));
        let err = items(r#"[{"id":20,"error":{"code":-32000,"message":"x"}},{"id":21,"result":[]}]"#);
        assert!(accept_batch(10, &err).unwrap_err().contains("eth_getBlockByNumber error -32000"));
        assert!(accept_batch(10, &ok[..1]).unwrap_err().contains("receipts response missing"));
        assert_eq!(calls_needed(Range { from: 5, to: 9 }), 10);
    }
}
