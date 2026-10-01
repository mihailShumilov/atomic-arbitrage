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
    // Two gap files of 4 blocks = 8 calls each; budget 12: the first file is
    // committed, the second runs out.
    std::fs::write(&gaps, "100\t103\t1\n200\t203\t2\n").unwrap();
    let stats = d.join("stats.json");
    let (code, err) = run_bin(&["--rpc-url", &m.url, "--gaps", gaps.to_str().unwrap(), "--rps", "0", "--batch", "2",
                                "--concurrency", "1", "--max-calls", "12", "--out-dir", out.to_str().unwrap(),
                                "--stats-json", stats.to_str().unwrap()]);
    assert_eq!(code, Some(75), "{err}");
    assert!(err.contains("call budget exhausted"), "{err}");
    assert!(out.join("blocks-100-103.jsonl.zst").exists());
    assert!(!out.join("blocks-200-203.jsonl.zst").exists());
    let filled = std::fs::read_to_string(out.join("filled.tsv")).unwrap();
    assert_eq!(filled.lines().count(), 1, "{filled}");
    assert!(stats.exists(), "stats are still written");
    assert_eq!(m.calls.lock().unwrap().len(), 12);
}

#[tokio::test(flavor = "multi_thread")]
async fn other_errors_still_exit_1() {
    // Retries exhausted (HTTP 429 forever, 1 attempt): not the budget.
    let m = start(Behavior { always_429: true, ..Default::default() }).await;
    let d = scratch("exit1");
    let (code, err) = run_bin(&["--rpc-url", &m.url, "--from", "1", "--to", "2", "--rps", "0", "--max-attempts", "1",
                                "--max-calls", "100", "--out-dir", d.join("blocks").to_str().unwrap()]);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("giving up after 1 attempts"), "{err}");
    // Missing gaps file.
    let (code, err) = run_bin(&["--rpc-url", "http://127.0.0.1:9", "--gaps", d.join("nope.tsv").to_str().unwrap(),
                                "--out-dir", d.join("b2").to_str().unwrap()]);
    assert_eq!(code, Some(1), "{err}");
}
