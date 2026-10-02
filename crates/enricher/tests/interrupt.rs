//! Interrupting the real binary mid-range never leaves a final-named file.
#![cfg(unix)]

mod common;

use std::process::{Command, Stdio};
use std::time::Duration;

use common::{scratch, start, Behavior};

fn spawn(url: &str, out: &std::path::Path, from: u64, to: u64) -> std::process::Child {
    Command::new(env!("CARGO_BIN_EXE_enricher"))
        .args([
            "--rpc-url",
            url,
            "--from",
            &from.to_string(),
            "--to",
            &to.to_string(),
            "--batch",
            "5",
            "--concurrency",
            "1",
            "--rps",
            "0",
            "--out-dir",
            out.to_str().unwrap(),
        ])
        .env("RUST_LOG", "warn")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn kill(pid: u32, sig: &str) {
    assert!(Command::new("kill").args([sig, &pid.to_string()]).status().unwrap().success());
}

#[tokio::test(flavor = "multi_thread")]
async fn sigint_mid_range_leaves_no_final_file() {
    // 40 batches × 200 ms ≈ 8 s; interrupt after ~1.5 s.
    let m = start(Behavior { delay: Duration::from_millis(200), ..Default::default() }).await;
    let d = scratch("sigint");
    let out = d.join("blocks");
    let mut child = spawn(&m.url, &out, 1, 200);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(out.join("blocks-1-200.jsonl.zst.partial").exists(), "partial exists while running");
    assert!(!out.join("blocks-1-200.jsonl.zst").exists());
    kill(child.id(), "-INT");
    let status = tokio::task::spawn_blocking(move || child.wait().unwrap()).await.unwrap();
    assert_eq!(status.code(), Some(130));
    assert!(m.requests() > 0 && m.requests() < 40, "stopped mid-range: {} requests", m.requests());
    assert!(!out.join("blocks-1-200.jsonl.zst").exists(), "no final-named file after Ctrl-C");
    assert!(!out.join("blocks-1-200.jsonl.zst.partial").exists(), "partial removed on Ctrl-C");
    assert!(!out.join("filled.tsv").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn sigkill_leaves_only_partial_which_next_run_removes() {
    let m = start(Behavior { delay: Duration::from_millis(200), ..Default::default() }).await;
    let d = scratch("sigkill");
    let out = d.join("blocks");
    let mut child = spawn(&m.url, &out, 1, 200);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    kill(child.id(), "-KILL");
    tokio::task::spawn_blocking(move || child.wait().unwrap()).await.unwrap();
    assert!(!out.join("blocks-1-200.jsonl.zst").exists(), "no final-named file after kill -9");
    assert!(out.join("blocks-1-200.jsonl.zst.partial").exists(), "kill -9 cannot clean up");

    // Next run in the same dir removes the leftover and completes its own range.
    let fast = start(Behavior::default()).await;
    let mut child = spawn(&fast.url, &out, 300, 304);
    let status = tokio::task::spawn_blocking(move || child.wait().unwrap()).await.unwrap();
    assert!(status.success());
    assert!(!out.join("blocks-1-200.jsonl.zst.partial").exists());
    assert!(out.join("blocks-300-304.jsonl.zst").exists());
    let filled = std::fs::read_to_string(out.join("filled.tsv")).unwrap();
    assert_eq!(filled.lines().count(), 1);
}
