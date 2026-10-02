//! Offline scan of block files for L1 inflows: counters, optional TSV of funding edges and of
//! unaccounted flows. Reads `blocks-*.jsonl.zst` (multi-frame zstd) or plain `.jsonl`.
//! No network, no ClickHouse. TSV columns and formatting come from `decoders::rows`.
//!
//! cargo run -p decoders --example l1_inflows_scan -- [--gateways extra.tsv] [--no-builtin]
//!     [--edges-out edges.tsv] [--unaccounted-out unaccounted.tsv] FILE...

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};

use alloy_primitives::U256;
use anyhow::{bail, Context, Result};
use decoders::l1_inflows::{decode_block, Counters, GatewayRegistry, UnaccountedFlow};
use decoders::parse_block_line;
use decoders::rows::FundingEdge;

fn eth(v: U256) -> String {
    // Display only; sums are reported in wei as well.
    let s = v.to_string();
    let f: f64 = s.parse().unwrap_or(f64::NAN);
    format!("{:.6}", f / 1e18)
}

fn opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map(|x| x.to_string()).unwrap_or_default()
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut registry = GatewayRegistry::builtin();
    let mut extra: Option<String> = None;
    let mut edges_out: Option<String> = None;
    let mut unacc_out: Option<String> = None;
    let mut files = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--gateways" => extra = Some(args.next().context("--gateways PATH")?),
            "--no-builtin" => registry = GatewayRegistry::default(),
            "--edges-out" => edges_out = Some(args.next().context("--edges-out PATH")?),
            "--unaccounted-out" => unacc_out = Some(args.next().context("--unaccounted-out PATH")?),
            s if s.starts_with("--") => bail!("unknown flag {s}"),
            _ => files.push(a),
        }
    }
    if let Some(p) = extra {
        let text = std::fs::read_to_string(&p).with_context(|| format!("read {p}"))?;
        registry.extend(GatewayRegistry::parse_tsv(&text)?)?;
    }
    if files.is_empty() {
        bail!("no input files");
    }
    let mut edges = match &edges_out {
        Some(p) => {
            let mut w = BufWriter::new(File::create(p)?);
            FundingEdge::write_tsv_header(&mut w)?;
            Some(w)
        }
        None => None,
    };
    let mut unacc = match &unacc_out {
        Some(p) => {
            let mut w = BufWriter::new(File::create(p)?);
            UnaccountedFlow::write_tsv_header(&mut w)?;
            Some(w)
        }
        None => None,
    };

    let mut total = Counters::default();
    let (mut min_block, mut max_block) = (u64::MAX, 0u64);
    for path in &files {
        let f = File::open(path).with_context(|| format!("open {path}"))?;
        let zst = std::path::Path::new(path).extension().is_some_and(|e| e.eq_ignore_ascii_case("zst"));
        let reader: Box<dyn Read> = if zst { Box::new(zstd::stream::read::Decoder::new(f)?) } else { Box::new(f) };
        let mut file_counters = Counters::default();
        for (i, line) in BufReader::new(reader).lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let block = parse_block_line(&line).with_context(|| format!("{path}:{}", i + 1))?;
            min_block = min_block.min(block.number);
            max_block = max_block.max(block.number);
            let r = decode_block(&block, &registry).with_context(|| format!("{path}:{}", i + 1))?;
            file_counters.merge(&r.counters);
            if let Some(w) = edges.as_mut() {
                for row in &r.inflows {
                    row.funding_edge().write_tsv(w)?;
                }
            }
            if let Some(w) = unacc.as_mut() {
                for u in &r.unaccounted {
                    u.write_tsv(w)?;
                }
            }
        }
        println!(
            "file {path}: blocks={} txs={} deposit_rows={} retry_eth_rows={} token_rows={}",
            file_counters.blocks,
            file_counters.txs,
            file_counters.deposit_rows.n,
            file_counters.retry_eth_rows.n,
            file_counters.token_rows_registered.n + file_counters.token_rows_unregistered.n
        );
        total.merge(&file_counters);
    }

    let c = &total;
    println!(
        "== total: {} files, blocks={} (range {}..{}), txs={}",
        files.len(),
        c.blocks,
        min_block,
        max_block,
        c.txs
    );
    println!("registry: {} entries", registry.len());
    for e in registry.entries() {
        println!(
            "  gateway {:#x} token {} {}",
            e.gateway,
            opt(e.l2_token.map(|a| format!("{a:#x}"))),
            e.status.as_str()
        );
    }
    let types: Vec<String> = c.tx_types.iter().map(|(k, v)| format!("0x{k:02x}={v}")).collect();
    println!("tx types: {}", types.join(" "));
    println!(
        "rows  eth_deposit(0x64)   n={} sum={} ETH ({} wei)",
        c.deposit_rows.n,
        eth(c.deposit_rows.sum),
        c.deposit_rows.sum
    );
    println!(
        "rows  retry_eth(0x68)     n={} sum={} ETH ({} wei)",
        c.retry_eth_rows.n,
        eth(c.retry_eth_rows.sum),
        c.retry_eth_rows.sum
    );
    println!("rows  token registered    n={} sum_raw={}", c.token_rows_registered.n, c.token_rows_registered.sum);
    println!("rows  token unregistered  n={} sum_raw={}", c.token_rows_unregistered.n, c.token_rows_unregistered.sum);
    println!(
        "flags token_l2_missing={} token_registry_mismatch={} deposit_finalized_foreign={} retry_zero_value={} refund_identity_anomaly={}",
        c.token_l2_missing, c.token_registry_mismatch, c.deposit_finalized_foreign, c.retry_zero_value, c.refund_identity_anomaly
    );
    // Printed only when non-zero, so the report of a normal run is unchanged.
    if c.token_sum_overflow > 0 {
        println!("flags token_sum_overflow={}", c.token_sum_overflow);
    }
    println!("ctx   0x69 ok n={} depositValue sum={} ETH", c.submit_ok.n, eth(c.submit_ok.sum));
    println!("unaccounted (not in funding_edges):");
    if c.unaccounted.is_empty() {
        println!("  none");
    }
    for (k, a) in &c.unaccounted {
        println!("  {:<30} n={} sum={} ETH ({} wei)", k.as_str(), a.n, eth(a.sum), a.sum);
    }
    // Printed only when set, so the report of a normal run is unchanged.
    let named = [
        ("deposit_rows", &c.deposit_rows),
        ("retry_eth_rows", &c.retry_eth_rows),
        ("token_rows_registered", &c.token_rows_registered),
        ("token_rows_unregistered", &c.token_rows_unregistered),
        ("submit_ok", &c.submit_ok),
    ];
    let unaccounted = c.unaccounted.iter().map(|(k, a)| (k.as_str(), a));
    for (name, a) in named.into_iter().chain(unaccounted) {
        if a.overflowed {
            println!("overflow {name}: sum={} (lower bound: overflowed)", a.sum);
        }
    }
    if let Some(mut w) = edges {
        w.flush()?;
    }
    if let Some(mut w) = unacc {
        w.flush()?;
    }
    Ok(())
}
