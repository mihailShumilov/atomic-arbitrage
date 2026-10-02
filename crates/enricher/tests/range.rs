//! Range mode (`--from/--to`), task 026: the budget plan check and
//! `--dry-run` work as in `--gaps`, and every output file carries the zstd
//! content checksum. Only the local mock on 127.0.0.1 is called.

mod common;

use std::sync::Arc;

use common::{args, read_jsonl_zst, scratch, start, Behavior};
use enricher::stats::Stats;

/// zstd frame header descriptor of the first frame: bit 2 = content checksum.
fn has_checksum_flag(p: &std::path::Path) -> bool {
    let b = std::fs::read(p).unwrap();
    assert_eq!(b[..4], [0x28, 0xb5, 0x2f, 0xfd], "zstd magic in {}", p.display());
    b[4] & 0b100 != 0
}

/// Task 026 item 2: `--dry-run` in range mode is plan only: no RPC call
/// (not even `eth_chainId`) and nothing on disk, not even the out-dir.
#[tokio::test]
async fn range_dry_run_makes_no_call_and_touches_nothing() {
    let m = start(Behavior::default()).await;
    let d = scratch("range-dry");
    let out = d.join("blocks");
    let logs_out = d.join("logs");
    for mode in ["blocks", "logs"] {
        let a = args(&[
            "--rpc-url",
            &m.url,
            "--mode",
            mode,
            "--from",
            "100",
            "--to",
            "199",
            "--chunk",
            "30",
            "--out-dir",
            out.to_str().unwrap(),
            "--logs-out-dir",
            logs_out.to_str().unwrap(),
            "--dry-run",
        ]);
        enricher::run(&a, Arc::new(Stats::default())).await.unwrap();
    }
    // Plan errors are still reported by a dry run.
    let a = args(&["--rpc-url", &m.url, "--from", "100", "--to", "199", "--max-blocks", "99", "--dry-run"]);
    let e = format!("{:#}", enricher::run(&a, Arc::new(Stats::default())).await.unwrap_err());
    assert!(e.contains("--max-blocks 99"), "{e}");
    let a = args(&["--rpc-url", &m.url, "--mode", "logs", "--from", "1", "--to", "2", "--topic0", "0x12", "--dry-run"]);
    let e = format!("{:#}", enricher::run(&a, Arc::new(Stats::default())).await.unwrap_err());
    assert!(e.contains("bad topic0"), "{e}");
    assert_eq!(m.requests(), 0);
    assert_eq!(m.chain_id_requests(), 0, "--dry-run makes no call");
    assert!(!out.exists() && !logs_out.exists(), "a range dry run creates nothing");
}

/// Task 026 item 2 (review 020 Р2/З4): a range file larger than
/// `--max-calls` can pay for is a configuration error found while planning
/// (exit 1 in the binary, see `exit_codes.rs`), before the lock and any
/// call. Before 026 every run spent the `eth_chainId` call and exited 75.
#[tokio::test]
async fn range_budget_below_one_file_is_a_config_error() {
    let m = start(Behavior::default()).await;
    let d = scratch("range-plan");
    let out = d.join("blocks");
    let base = ["--rpc-url", &m.url, "--from", "100", "--to", "109", "--rps", "0", "--out-dir", out.to_str().unwrap()];
    // 10 blocks in one file: 20 + 1 calls.
    for extra in [&["--max-calls", "20"][..], &["--max-calls", "20", "--dry-run"][..]] {
        let mut v = base.to_vec();
        v.extend_from_slice(extra);
        let e = enricher::run(&args(&v), Arc::new(Stats::default())).await.unwrap_err();
        assert!(!enricher::rpc::is_budget_exhausted(&e), "a config error (exit 1), not exit 75");
        let e = format!("{e:#}");
        assert!(e.contains("--max-calls 20") && e.contains("21") && e.contains("lower --chunk to <= 9"), "{e}");
    }
    assert_eq!((m.requests(), m.chain_id_requests()), (0, 0));
    assert!(!out.exists(), "refused before the lock: out-dir not created");

    // Smaller files fit: 2 files of 5 blocks, 1 + 10 + 10 calls.
    let mut v = base.to_vec();
    v.extend(["--max-calls", "21", "--chunk", "5"]);
    enricher::run(&args(&v), Arc::new(Stats::default())).await.unwrap();
    assert_eq!(m.calls.lock().unwrap().len(), 21);
    assert!(out.join("blocks-100-104.jsonl.zst").exists() && out.join("blocks-105-109.jsonl.zst").exists());
}

/// Task 026 item 1: `blocks-*` and `logs-*` are written with the zstd
/// content checksum, the content reads back unchanged, and a corrupted
/// byte makes reading fail instead of returning wrong data.
#[tokio::test]
async fn output_files_carry_the_zstd_checksum() {
    let m = start(Behavior::default()).await;
    let d = scratch("range-checksum");
    let out = d.join("blocks");
    let logs_out = d.join("logs");
    let a =
        args(&["--rpc-url", &m.url, "--from", "10", "--to", "59", "--rps", "0", "--out-dir", out.to_str().unwrap()]);
    enricher::run(&a, Arc::new(Stats::default())).await.unwrap();
    let a = args(&[
        "--rpc-url",
        &m.url,
        "--mode",
        "logs",
        "--from",
        "10",
        "--to",
        "59",
        "--rps",
        "0",
        "--logs-out-dir",
        logs_out.to_str().unwrap(),
    ]);
    enricher::run(&a, Arc::new(Stats::default())).await.unwrap();

    let blocks = out.join("blocks-10-59.jsonl.zst");
    let logs = logs_out.join("logs-10-59.jsonl.zst");
    for p in [&blocks, &logs] {
        assert!(has_checksum_flag(p), "{}", p.display());
    }
    let lines = read_jsonl_zst(&blocks);
    assert_eq!(lines.len(), 50);
    assert_eq!(lines[0]["number"], 10);
    assert_eq!(read_jsonl_zst(&logs).len(), 51, "meta + one line per block");

    // Corrupt one byte of the compressed data (not the 4-byte checksum).
    let mut b = std::fs::read(&blocks).unwrap();
    let i = b.len() / 2;
    b[i] ^= 0x01;
    std::fs::write(&blocks, b).unwrap();
    assert!(zstd::decode_all(std::fs::File::open(&blocks).unwrap()).is_err(), "corruption must be reported");
}
