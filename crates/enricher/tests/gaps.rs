//! `--gaps`: fills recorder gaps, records them in filled.tsv, never re-downloads closed ranges.

mod common;

use std::sync::Arc;

use common::{args, read_jsonl_zst, scratch, start, Behavior};
use enricher::stats::Stats;

#[tokio::test]
async fn gaps_fill_once_and_skip_closed_ranges() {
    let m = start(Behavior::default()).await;
    let d = scratch("gaps");
    let out = d.join("blocks");
    let gaps = d.join("gaps.tsv");
    // Recorder format: from \t to \t recv_ns. Overlapping gaps get merged.
    std::fs::write(&gaps, "100\t104\t1\n200\t202\t2\n103\t105\t3\n").unwrap();
    let base = [
        "--rpc-url",
        &m.url,
        "--gaps",
        gaps.to_str().unwrap(),
        "--rps",
        "0",
        "--batch",
        "2",
        "--chunk",
        "4",
        "--out-dir",
        out.to_str().unwrap(),
    ];

    enricher::run(&args(&base), Arc::new(Stats::default())).await.unwrap();
    let mut got = m.blocks_requested();
    got.sort();
    assert_eq!(got, vec![100, 101, 102, 103, 104, 105, 200, 201, 202]);
    // 100..=105 in chunks of 4 → two files; 200..=202 → one file.
    for f in ["blocks-100-103.jsonl.zst", "blocks-104-105.jsonl.zst", "blocks-200-202.jsonl.zst"] {
        assert!(out.join(f).exists(), "{f}");
    }
    let lines = read_jsonl_zst(&out.join("blocks-104-105.jsonl.zst"));
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["block"]["hash"], common::block_hash(104));
    let filled = std::fs::read_to_string(out.join("filled.tsv")).unwrap();
    assert_eq!(filled.lines().count(), 3);

    // Rerun: everything is closed → zero RPC requests.
    let before = m.requests();
    enricher::run(&args(&base), Arc::new(Stats::default())).await.unwrap();
    assert_eq!(m.requests(), before, "rerun must not download closed ranges");

    // A new, partly overlapping gap: only the uncovered part is fetched.
    std::fs::write(&gaps, "100\t104\t1\n200\t202\t2\n103\t105\t3\n201\t207\t4\n").unwrap();
    let calls_before = m.blocks_requested().len();
    enricher::run(&args(&base), Arc::new(Stats::default())).await.unwrap();
    let mut new: Vec<u64> = m.blocks_requested()[calls_before..].to_vec();
    new.sort();
    assert_eq!(new, vec![203, 204, 205, 206, 207]);
    assert!(out.join("blocks-203-206.jsonl.zst").exists());
    assert!(out.join("blocks-207-207.jsonl.zst").exists());
}

#[tokio::test]
async fn gaps_dry_run_and_budget_make_no_calls() {
    let m = start(Behavior::default()).await;
    let d = scratch("gaps-dry");
    let out = d.join("blocks");
    let gaps = d.join("gaps.tsv");
    std::fs::write(&gaps, "1\t500\t1\n").unwrap();
    let common = ["--rpc-url", &m.url, "--gaps", gaps.to_str().unwrap(), "--out-dir", out.to_str().unwrap()];
    let mut dry = common.to_vec();
    dry.push("--dry-run");
    enricher::run(&args(&dry), Arc::new(Stats::default())).await.unwrap();
    let mut capped = common.to_vec();
    capped.extend(["--max-blocks", "100"]);
    let err = enricher::run(&args(&capped), Arc::new(Stats::default())).await.unwrap_err();
    assert!(format!("{err:#}").contains("--max-blocks 100"));
    assert_eq!(m.requests(), 0);
}

#[tokio::test]
async fn missing_gaps_file_is_an_error() {
    let d = scratch("gaps-missing");
    let a = args(&[
        "--rpc-url",
        "http://127.0.0.1:9",
        "--gaps",
        d.join("nope.tsv").to_str().unwrap(),
        "--out-dir",
        d.join("b").to_str().unwrap(),
    ]);
    assert!(enricher::run(&a, Arc::new(Stats::default())).await.is_err());
}

/// Task 012 item 5: the recorder may be appending the last line of gaps.tsv
/// while `--gaps` reads it. An unterminated last line is ignored (WARN), the
/// complete ones are filled; once the line is terminated the next run fills it.
#[tokio::test]
async fn unterminated_last_gaps_line_is_ignored_until_complete() {
    let m = start(Behavior::default()).await;
    let d = scratch("gaps-torn");
    let out = d.join("blocks");
    let gaps = d.join("gaps.tsv");
    let base = [
        "--rpc-url",
        &m.url,
        "--gaps",
        gaps.to_str().unwrap(),
        "--rps",
        "0",
        "--batch",
        "5",
        "--out-dir",
        out.to_str().unwrap(),
    ];
    for tail in ["300", "300\t", "300\t302", "300\t302\t17908"] {
        std::fs::write(&gaps, format!("100\t102\t1\n{tail}")).unwrap();
        enricher::run(&args(&base), Arc::new(Stats::default())).await.unwrap();
    }
    let mut got = m.blocks_requested();
    got.sort();
    assert_eq!(got, vec![100, 101, 102], "only the complete line is filled, once");
    std::fs::write(&gaps, "100\t102\t1\n300\t302\t17908000\n").unwrap();
    enricher::run(&args(&base), Arc::new(Stats::default())).await.unwrap();
    let mut got = m.blocks_requested();
    got.sort();
    assert_eq!(got, vec![100, 101, 102, 300, 301, 302]);
    // A broken line that does end with a newline is still an error.
    std::fs::write(&gaps, "100\t102\t1\n5\tx\n").unwrap();
    assert!(enricher::run(&args(&base), Arc::new(Stats::default())).await.is_err());
}

/// Finding F1 of the 019 data audit, enricher side: filled.tsv ends with a
/// torn row (no `\n`). The run ignores it (WARN), fills that range again and
/// appends its own row on a new line (the fragment gets its `\n` first) instead
/// of gluing the two. A rerun downloads nothing.
#[tokio::test]
async fn torn_last_filled_row_is_not_glued_to_the_next_one() {
    let m = start(Behavior::default()).await;
    let d = scratch("gaps-torn-filled");
    let out = d.join("blocks");
    std::fs::create_dir_all(&out).unwrap();
    let gaps = d.join("gaps.tsv");
    std::fs::write(&gaps, "100\t103\t1\n").unwrap();
    std::fs::write(out.join("filled.tsv"), "100\t103\tblocks-100-103.jsonl.zst\t17").unwrap();
    let base =
        ["--rpc-url", &m.url, "--gaps", gaps.to_str().unwrap(), "--rps", "0", "--out-dir", out.to_str().unwrap()];

    enricher::run(&args(&base), Arc::new(Stats::default())).await.unwrap();
    let mut got = m.blocks_requested();
    got.sort();
    assert_eq!(got, vec![100, 101, 102, 103], "torn row does not count as filled");
    let filled = std::fs::read_to_string(out.join("filled.tsv")).unwrap();
    let lines: Vec<&str> = filled.lines().collect();
    assert_eq!(lines.len(), 2, "{filled:?}");
    assert_eq!(lines[0], "100\t103\tblocks-100-103.jsonl.zst\t17");
    assert!(lines[1].starts_with("100\t103\tblocks-100-103.jsonl.zst\t"), "{filled:?}");
    assert_eq!(lines[1].split('\t').count(), 4, "{filled:?}");
    assert!(filled.ends_with('\n'));

    let before = m.requests();
    enricher::run(&args(&base), Arc::new(Stats::default())).await.unwrap();
    assert_eq!(m.requests(), before, "rerun must not download closed ranges");
}
