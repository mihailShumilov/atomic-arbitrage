//! `logs` mode: adaptive window and a file that cannot be confused with full blocks.

mod common;

use std::sync::Arc;

use common::{args, read_jsonl_zst, scratch, start, Behavior};
use enricher::stats::Stats;

#[tokio::test]
async fn logs_window_shrinks_and_grows_and_covers_every_block() {
    let m = start(Behavior { logs_max_range: Some(30), ..Default::default() }).await;
    let d = scratch("logs");
    let out = d.join("logs");
    let a = args(&[
        "--rpc-url",
        &m.url,
        "--mode",
        "logs",
        "--from",
        "1000",
        "--to",
        "1099",
        "--rps",
        "0",
        "--logs-window",
        "100",
        "--logs-window-max",
        "1000",
        "--logs-out-dir",
        out.to_str().unwrap(),
    ]);
    let stats = Arc::new(Stats::default());
    enricher::run(&a, stats.clone()).await.unwrap();

    // Windows asked: 100 (too much) → 50 (too much) → 25 ok → 50 (too much) → 25 ok → …
    let spans: Vec<u64> = m
        .calls_of("eth_getLogs")
        .iter()
        .map(|p| common::hex(&p[0]["toBlock"]) - common::hex(&p[0]["fromBlock"]) + 1)
        .collect();
    assert_eq!(&spans[..4], &[100, 50, 25, 50]);
    assert_eq!(spans.iter().filter(|s| **s <= 30).sum::<u64>(), 100, "{spans:?}");
    // Default filter = every topic0 of crates/decoders.
    let topics = &m.calls_of("eth_getLogs")[0][0]["topics"][0];
    assert_eq!(topics.as_array().unwrap().len(), decoders::ALL_TOPIC0.len());

    let lines = read_jsonl_zst(&out.join("logs-1000-1099.jsonl.zst"));
    assert_eq!(lines.len(), 101);
    let meta = &lines[0]["meta"];
    assert_eq!(meta["mode"], "logs");
    assert_eq!(meta["format"], "hood-logs-v1");
    assert_eq!(meta["not_full_blocks"], true);
    assert!(meta["reverted_txs"].as_str().unwrap().starts_with("absent"));
    for (i, l) in lines[1..].iter().enumerate() {
        assert_eq!(l["number"].as_u64().unwrap(), 1000 + i as u64);
        assert_eq!(l["logs"].as_array().unwrap().len(), 1);
        assert!(l.get("block").is_none() && l.get("receipts").is_none());
    }
    assert!(!out.join("filled.tsv").exists(), "logs never count as filled blocks");
}

#[tokio::test]
async fn logs_custom_topic_filter() {
    let m = start(Behavior::default()).await;
    let d = scratch("logs-topic");
    let out = d.join("logs");
    let t = "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67";
    let a = args(&[
        "--rpc-url",
        &m.url,
        "--mode",
        "logs",
        "--from",
        "1",
        "--to",
        "3",
        "--rps",
        "0",
        "--topic0",
        t,
        "--logs-out-dir",
        out.to_str().unwrap(),
    ]);
    enricher::run(&a, Arc::new(Stats::default())).await.unwrap();
    let calls = m.calls_of("eth_getLogs");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0][0]["topics"], serde_json::json!([[t]]));
}
