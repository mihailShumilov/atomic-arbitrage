//! Exit codes of the real binary (task 012 item 5): a used-up `--max-calls`
//! exits with 75 (EX_TEMPFAIL, `SuccessExitStatus=75` in the systemd unit),
//! any other error with 1. Only the local mock on 127.0.0.1 is called.
#![cfg(unix)]

mod common;

use std::process::{Command, Stdio};

use common::{scratch, start, Behavior};

fn run_bin(args: &[&str]) -> (Option<i32>, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_enricher"))
        .args(args)
        .env_remove("RPC_URL")
        .env_remove("ENRICHER_OUT_DIR")
        .env_remove("ENRICHER_LOGS_OUT_DIR")
        .env("RUST_LOG", "info")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    // tracing logs to stdout, `Error: ...` of main goes to stderr.
    let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    (o.status.code(), text)
}

#[tokio::test(flavor = "multi_thread")]
async fn max_calls_exhausted_exits_75_and_keeps_finished_files() {
    let m = start(Behavior::default()).await;
    let d = scratch("exit75");
    let out = d.join("blocks");
    let gaps = d.join("gaps.tsv");
    // Two gap files of 4 blocks = 8 calls each; budget 12 = 1 eth_chainId +
    // 8 for the first file; the 3 left cannot pay for the second file, so it
    // is not started (task 020: no calls wasted on a file that cannot finish).
    std::fs::write(&gaps, "100\t103\t1\n200\t203\t2\n").unwrap();
    let stats = d.join("stats.json");
    let (code, err) = run_bin(&[
        "--rpc-url",
        &m.url,
        "--gaps",
        gaps.to_str().unwrap(),
        "--rps",
        "0",
        "--batch",
        "2",
        "--concurrency",
        "1",
        "--max-calls",
        "12",
        "--out-dir",
        out.to_str().unwrap(),
        "--stats-json",
        stats.to_str().unwrap(),
    ]);
    assert_eq!(code, Some(75), "{err}");
    assert!(err.contains("call budget exhausted"), "{err}");
    assert!(out.join("blocks-100-103.jsonl.zst").exists());
    assert!(!out.join("blocks-200-203.jsonl.zst").exists());
    let filled = std::fs::read_to_string(out.join("filled.tsv")).unwrap();
    assert_eq!(filled.lines().count(), 1, "{filled}");
    assert!(stats.exists(), "stats are still written");
    assert_eq!(m.calls.lock().unwrap().len(), 9);
    assert!(!m.blocks_requested().contains(&200), "second file not started");
}

fn budget_args<'a>(url: &'a str, gaps: &'a str, out: &'a str, max_calls: &'a str) -> Vec<&'a str> {
    vec![
        "--rpc-url",
        url,
        "--gaps",
        gaps,
        "--rps",
        "0",
        "--batch",
        "2",
        "--concurrency",
        "1",
        "--max-calls",
        max_calls,
        "--out-dir",
        out,
    ]
}

/// Task 020 item 5 (review I9): a failed `--stats-json` write is a WARN and
/// does not turn 75 into 1.
#[tokio::test(flavor = "multi_thread")]
async fn stats_json_failure_does_not_mask_exit_75() {
    let m = start(Behavior::default()).await;
    let d = scratch("exit75-stats");
    let out = d.join("blocks");
    let gaps = d.join("gaps.tsv");
    std::fs::write(&gaps, "100\t103\t1\n200\t203\t2\n").unwrap();
    let mut a = budget_args(&m.url, gaps.to_str().unwrap(), out.to_str().unwrap(), "12");
    // A directory: the write fails.
    a.extend(["--stats-json", d.to_str().unwrap()]);
    let (code, err) = run_bin(&a);
    assert_eq!(code, Some(75), "{err}");
    assert!(err.contains("the exit code is not affected"), "{err}");
    assert!(out.join("blocks-100-103.jsonl.zst").exists());
}

/// Task 020 item 2 (review I6): an endpoint of another network is refused
/// before any download: exit 1, one call, nothing written.
#[tokio::test(flavor = "multi_thread")]
async fn wrong_chain_id_exits_1_and_writes_nothing() {
    let m = start(Behavior { chain_id: Some("0xa4b1"), ..Default::default() }).await;
    let d = scratch("exit-chain");
    let out = d.join("blocks");
    let gaps = d.join("gaps.tsv");
    std::fs::write(&gaps, "100\t103\t1\n").unwrap();
    let (code, err) = run_bin(&budget_args(&m.url, gaps.to_str().unwrap(), out.to_str().unwrap(), "100"));
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("wrong network") && err.contains("42161") && err.contains("4663"), "{err}");
    assert_eq!(m.chain_id_requests(), 1);
    assert_eq!(m.requests(), 0, "no block was requested");
    assert!(!out.join("filled.tsv").exists());
    assert!(!out.join("blocks-100-103.jsonl.zst").exists());
}

/// Task 020 item 3 (review I7): `--max-calls` below one file + the chain id
/// check would make every run exit 75 without progress; it is a
/// configuration error (exit 1) found while planning, before any call.
#[tokio::test(flavor = "multi_thread")]
async fn gaps_budget_below_one_file_is_a_config_error() {
    let m = start(Behavior::default()).await;
    let d = scratch("exit-plan");
    let out = d.join("blocks");
    let gaps = d.join("gaps.tsv");
    // 1000 blocks, default --chunk 1000: one file needs 2000 + 1 calls.
    std::fs::write(&gaps, "100\t1099\t1\n").unwrap();
    let (code, err) = run_bin(&budget_args(&m.url, gaps.to_str().unwrap(), out.to_str().unwrap(), "1500"));
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("--max-calls 1500") && err.contains("2001"), "{err}");
    assert!(err.contains("lower --chunk to <= 749"), "{err}");
    // Exactly enough is fine.
    let (code, err) = run_bin(&budget_args(&m.url, gaps.to_str().unwrap(), out.to_str().unwrap(), "2001"));
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(m.calls.lock().unwrap().len(), 2001);
    assert!(out.join("blocks-100-1099.jsonl.zst").exists());
}

/// Task 026 item 2: the same plan check in range mode, exit 1 and no call
/// (before 026: one `eth_chainId` call and exit 75 on every run).
#[tokio::test(flavor = "multi_thread")]
async fn range_budget_below_one_file_is_a_config_error() {
    let m = start(Behavior::default()).await;
    let d = scratch("exit-range-plan");
    let out = d.join("blocks");
    let (code, err) = run_bin(&[
        "--rpc-url",
        &m.url,
        "--from",
        "100",
        "--to",
        "109",
        "--rps",
        "0",
        "--max-calls",
        "20",
        "--out-dir",
        out.to_str().unwrap(),
    ]);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("--max-calls 20") && err.contains("21"), "{err}");
    assert_eq!((m.requests(), m.chain_id_requests()), (0, 0));
    assert!(!out.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn other_errors_still_exit_1() {
    // Retries exhausted (HTTP 429 forever, 1 attempt): not the budget.
    let m = start(Behavior { always_429: true, ..Default::default() }).await;
    let d = scratch("exit1");
    let (code, err) = run_bin(&[
        "--rpc-url",
        &m.url,
        "--from",
        "1",
        "--to",
        "2",
        "--rps",
        "0",
        "--max-attempts",
        "1",
        "--max-calls",
        "100",
        "--out-dir",
        d.join("blocks").to_str().unwrap(),
    ]);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("giving up after 1 attempts"), "{err}");
    // Missing gaps file.
    let (code, err) = run_bin(&[
        "--rpc-url",
        "http://127.0.0.1:9",
        "--gaps",
        d.join("nope.tsv").to_str().unwrap(),
        "--out-dir",
        d.join("b2").to_str().unwrap(),
    ]);
    assert_eq!(code, Some(1), "{err}");
}
