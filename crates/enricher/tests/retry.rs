//! 429 / rate-limit handling against the mock server.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{args, read_jsonl_zst, scratch, start, Behavior};
use enricher::stats::Stats;

#[tokio::test]
async fn retries_after_429_honouring_retry_after() {
    let m = start(Behavior { http_429_first: 2, retry_after: Some("1"), rpc_rate_limit_next: 1, ..Default::default() })
        .await;
    let d = scratch("retry-ok");
    let out = d.join("blocks");
    let a = args(&[
        "--rpc-url",
        &m.url,
        "--from",
        "100",
        "--to",
        "109",
        "--batch",
        "10",
        "--rps",
        "0",
        "--backoff-ms",
        "10",
        "--out-dir",
        out.to_str().unwrap(),
    ]);
    let stats = Arc::new(Stats::default());
    let t = Instant::now();
    enricher::run(&a, stats.clone()).await.unwrap();
    let el = t.elapsed();

    let c = stats.counters();
    assert_eq!(c.http_429, 2);
    assert_eq!(c.rpc_rate_limited, 1, "JSON-RPC rate-limit error inside HTTP 200 is retried too");
    assert_eq!(c.retries, 3);
    assert_eq!(c.global_pauses, 3, "every rate limit pauses all tasks");
    // 4 data requests + 1 eth_chainId check (task 020).
    assert_eq!(c.http_requests, 5);
    assert_eq!(m.requests(), 4);
    assert_eq!(m.chain_id_requests(), 1);
    assert_eq!(c.calls["eth_chainId"], 1);
    // Two Retry-After: 1 waits; the backoff alone (10 ms base) would be far shorter.
    assert!(el >= Duration::from_millis(1900), "Retry-After not honoured: {el:?}");
    assert_eq!(c.calls["eth_getBlockByNumber"], 40);
    assert_eq!(c.calls["eth_getBlockReceipts"], 40);

    let lines = read_jsonl_zst(&out.join("blocks-100-109.jsonl.zst"));
    let nums: Vec<u64> = lines.iter().map(|l| l["number"].as_u64().unwrap()).collect();
    assert_eq!(nums, (100..=109).collect::<Vec<_>>());
    let filled = std::fs::read_to_string(out.join("filled.tsv")).unwrap();
    assert!(filled.starts_with("100\t109\tblocks-100-109.jsonl.zst\t"));
    let sum = stats.summary();
    assert_eq!(sum.sizes["eth_getBlockReceipts"].results, 10, "sizes count only accepted results");
}

/// Task 019: `Retry-After` as an HTTP-date is honoured (before 019 the
/// enricher ignored it and retried after the 10 ms backoff).
#[tokio::test]
async fn retry_after_http_date_is_honoured() {
    let when = chrono::Utc::now() + chrono::Duration::seconds(3);
    let date: &'static str = Box::leak(when.format("%a, %d %b %Y %H:%M:%S GMT").to_string().into_boxed_str());
    let m = start(Behavior { http_429_first: 1, retry_after: Some(date), ..Default::default() }).await;
    let d = scratch("retry-date");
    let out = d.join("blocks");
    let a = args(&[
        "--rpc-url",
        &m.url,
        "--from",
        "100",
        "--to",
        "101",
        "--rps",
        "0",
        "--backoff-ms",
        "10",
        "--out-dir",
        out.to_str().unwrap(),
    ]);
    let stats = Arc::new(Stats::default());
    let t = Instant::now();
    enricher::run(&a, stats.clone()).await.unwrap();
    let el = t.elapsed();
    assert_eq!(stats.counters().http_429, 1);
    assert_eq!(m.requests(), 2);
    // The date has 1 s resolution: the wait is 2..3 s.
    assert!(el >= Duration::from_millis(1900), "HTTP-date Retry-After not honoured: {el:?}");
    assert!(el < Duration::from_secs(10), "{el:?}");
}

#[tokio::test]
async fn gives_up_with_clear_error_and_writes_nothing() {
    let m = start(Behavior { always_429: true, ..Default::default() }).await;
    let d = scratch("retry-fail");
    let out = d.join("blocks");
    let a = args(&[
        "--rpc-url",
        &m.url,
        "--from",
        "5",
        "--to",
        "9",
        "--rps",
        "0",
        "--max-attempts",
        "3",
        "--backoff-ms",
        "5",
        "--out-dir",
        out.to_str().unwrap(),
    ]);
    let stats = Arc::new(Stats::default());
    let err = enricher::run(&a, stats.clone()).await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("blocks 5..=9") && msg.contains("giving up after 3 attempts") && msg.contains("429"), "{msg}");
    assert_eq!(m.requests(), 3);
    assert!(!out.join("blocks-5-9.jsonl.zst").exists());
    assert!(!out.join("blocks-5-9.jsonl.zst.partial").exists());
    assert!(!out.join("filled.tsv").exists(), "nothing is marked filled on failure");
}

#[tokio::test]
async fn rps_limit_counts_calls_not_requests() {
    let m = start(Behavior::default()).await;
    let d = scratch("rps");
    let out = d.join("blocks");
    // 4 batches × 2 calls at 5 calls/s: slots at 0, 0.4, 0.8, 1.2 s.
    let a = args(&[
        "--rpc-url",
        &m.url,
        "--from",
        "1",
        "--to",
        "4",
        "--batch",
        "1",
        "--concurrency",
        "4",
        "--rps",
        "5",
        "--out-dir",
        out.to_str().unwrap(),
    ]);
    let t = Instant::now();
    enricher::run(&a, Arc::new(Stats::default())).await.unwrap();
    assert!(t.elapsed() >= Duration::from_millis(1180), "{:?}", t.elapsed());
    assert_eq!(m.requests(), 4);
}

#[tokio::test]
async fn call_budget_is_never_exceeded() {
    let m = start(Behavior { always_429: true, ..Default::default() }).await;
    let d = scratch("budget");
    let out = d.join("blocks");
    // Each attempt = 10 blocks = 20 calls; the 3rd attempt would make 60 > 50.
    let a = args(&[
        "--rpc-url",
        &m.url,
        "--from",
        "1",
        "--to",
        "10",
        "--batch",
        "10",
        "--rps",
        "0",
        "--backoff-ms",
        "5",
        "--max-attempts",
        "8",
        "--max-calls",
        "50",
        "--out-dir",
        out.to_str().unwrap(),
    ]);
    let stats = Arc::new(Stats::default());
    let err = enricher::run(&a, stats.clone()).await.unwrap_err();
    assert!(format!("{err:#}").contains("call budget exhausted"), "{err:#}");
    // Task 012 item 5: typed, so the binary can exit with 75.
    assert!(enricher::rpc::is_budget_exhausted(&err), "{err:#}");
    assert_eq!(m.requests(), 2);
    // 1 eth_chainId + 2 attempts x 20; the third (61 > 50) is never sent.
    assert_eq!(m.calls.lock().unwrap().len(), 41);
    assert!(!out.join("blocks-1-10.jsonl.zst").exists());
}

/// Task 020 item 1 (review I1): the global pause follows the error kind,
/// not the text. A null block result for block 77429001 has "429" in its
/// message; before 020 that paused every task. Now it is an ordinary retry.
#[tokio::test]
async fn transient_error_with_429_in_text_does_not_pause_all() {
    let m = start(Behavior { null_block_next: 1, ..Default::default() }).await;
    let d = scratch("retry-429-text");
    let out = d.join("blocks");
    let a = args(&[
        "--rpc-url",
        &m.url,
        "--from",
        "77429001",
        "--to",
        "77429002",
        "--rps",
        "0",
        "--backoff-ms",
        "10",
        "--out-dir",
        out.to_str().unwrap(),
    ]);
    let stats = Arc::new(Stats::default());
    enricher::run(&a, stats.clone()).await.unwrap();
    let c = stats.counters();
    assert_eq!(c.retries, 1);
    assert_eq!(c.global_pauses, 0, "a transient retry must not pause every task");
    assert_eq!(c.rpc_rate_limited, 0);
    assert_eq!(m.requests(), 2);
    assert!(out.join("blocks-77429001-77429002.jsonl.zst").exists());
}
