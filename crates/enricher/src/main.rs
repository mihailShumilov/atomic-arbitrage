//! Phase-1 enricher: the feed has no receipts, logs or revert status, so for
//! each L2 block we pull `eth_getBlockByNumber(full=true)` (tx value, input,
//! from/to) and `eth_getBlockReceipts` (status, gasUsed, gasUsedForL1, logs).
//!
//! Output: <out>/blocks-<from>-<to>.jsonl.zst, one JSON object per block:
//!   {"number": N, "block": {...}, "receipts": [...]}
//!
//! Range mode only. TODO(claude-code): `--gaps <gaps.tsv>` mode, `--follow`
//! mode, and direct load into ClickHouse (see sql/001_schema.sql).
//!
//! Known limitation: ETH moved by contracts (internal transfers) is invisible
//! without traces. Check whether the provider supports debug_traceBlock*.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::Parser;
use futures::{stream, StreamExt, TryStreamExt};
use serde_json::{json, Value};
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(about = "Fetch blocks + receipts for an L2 block range")]
struct Args {
    /// Use a provider endpoint. The public RPC is rate-limited and not for production.
    #[arg(long, env = "RPC_URL", default_value = hood_core::PUBLIC_RPC_URL)]
    rpc_url: String,
    #[arg(long)]
    from: u64,
    /// Inclusive.
    #[arg(long)]
    to: u64,
    #[arg(long, env = "ENRICHER_OUT_DIR", default_value = "data/blocks")]
    out_dir: PathBuf,
    /// Blocks per JSON-RPC batch (each block = 2 calls).
    #[arg(long, default_value_t = 20)]
    batch: u64,
    /// Concurrent batches in flight.
    #[arg(long, default_value_t = 4)]
    concurrency: usize,
}

async fn fetch_batch(client: &reqwest::Client, url: &str, from: u64, to: u64) -> Result<Vec<Value>> {
    let mut calls = Vec::new();
    for n in from..=to {
        let tag = format!("0x{n:x}");
        calls.push(json!({"jsonrpc":"2.0","id": n * 2,     "method":"eth_getBlockByNumber","params":[tag, true]}));
        calls.push(json!({"jsonrpc":"2.0","id": n * 2 + 1, "method":"eth_getBlockReceipts","params":[format!("0x{n:x}")]}));
    }
    let mut attempt = 0u32;
    let resp: Vec<Value> = loop {
        attempt += 1;
        match client.post(url).json(&calls).send().await {
            Ok(r) if r.status().is_success() => break r.json().await.context("decode batch")?,
            Ok(r) => warn!(status = %r.status(), from, to, attempt, "rpc http error"),
            Err(e) => warn!(error = format!("{e:#}"), from, to, attempt, "rpc transport error"),
        }
        if attempt >= 6 {
            bail!("giving up on {from}..={to}");
        }
        tokio::time::sleep(Duration::from_millis(250 * 2u64.pow(attempt))).await;
    };

    let mut blocks = vec![Value::Null; (to - from + 1) as usize];
    let mut receipts = vec![Value::Null; (to - from + 1) as usize];
    for item in resp {
        let id = item["id"].as_u64().context("missing id")?;
        if let Some(err) = item.get("error") {
            bail!("rpc error for id {id}: {err}");
        }
        let n = id / 2;
        let idx = (n - from) as usize;
        if id % 2 == 0 { blocks[idx] = item["result"].clone() } else { receipts[idx] = item["result"].clone() }
    }
    let mut out = Vec::with_capacity(blocks.len());
    for (i, (b, r)) in blocks.into_iter().zip(receipts).enumerate() {
        let n = from + i as u64;
        if b.is_null() || r.is_null() {
            bail!("block {n}: missing block or receipts (not yet available?)");
        }
        out.push(json!({"number": n, "block": b, "receipts": r}));
    }
    Ok(out)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let a = Args::parse();
    anyhow::ensure!(a.to >= a.from, "--to must be >= --from");
    std::fs::create_dir_all(&a.out_dir)?;
    let path = a.out_dir.join(format!("blocks-{}-{}.jsonl.zst", a.from, a.to));
    let mut enc = zstd::Encoder::new(std::fs::File::create(&path)?, 3)?;
    let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?;

    let ranges: Vec<(u64, u64)> = (a.from..=a.to)
        .step_by(a.batch as usize)
        .map(|s| (s, (s + a.batch - 1).min(a.to)))
        .collect();
    info!(batches = ranges.len(), file = %path.display(), "start");

    // `buffered` keeps output in block order while fetching concurrently.
    let mut s = stream::iter(ranges)
        .map(|(f, t)| {
            let c = client.clone();
            let u = a.rpc_url.clone();
            async move { fetch_batch(&c, &u, f, t).await }
        })
        .buffered(a.concurrency);

    let mut done = 0u64;
    while let Some(batch) = s.try_next().await? {
        for v in batch {
            serde_json::to_writer(&mut enc, &v)?;
            enc.write_all(b"\n")?;
            done += 1;
        }
        if done % 1000 < a.batch {
            info!(done, "progress");
        }
    }
    enc.finish()?;
    info!(done, file = %path.display(), "finished");
    Ok(())
}
