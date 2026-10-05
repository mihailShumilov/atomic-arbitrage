//! Rollback decisions of `load::load_file` against a mock ClickHouse HTTP server on 127.0.0.1
//! (review 032 architect, В1). The INSERT into `hood.blocks` (the completion marker, last table)
//! fails in three ways:
//! - the server took the body and closed the connection without an answer, while the marker was
//!   committed ("insert committed but the client saw an error"): nothing may be deleted;
//! - the server answered 500 and the marker is absent: the rows of txs/logs/funding_edges are
//!   deleted again;
//! - the server answered 500 but the marker is present on re-read: nothing is deleted.
//!
//! Input: the task-017 fixture (real blocks 77285521, 77285531, 77300695, 77312169; see
//! `crates/decoders/tests/l1_inflows.rs`), which yields rows in all four tables.

use std::path::Path;
use std::sync::{Arc, Mutex};

use decoders::l1_inflows::GatewayRegistry;
use loader::blocks_file::read_blocks_file;
use loader::ch::Client;
use loader::config::ChConfig;
use loader::feed::FeedIndex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone, Copy, PartialEq, Eq)]
enum MarkerInsert {
    /// Read the body, commit the marker, close without an answer.
    CommitThenDrop,
    /// Answer 500, marker absent.
    Fail500,
    /// Answer 500, marker present on re-read.
    Fail500ButPresent,
}

struct State {
    mode: MarkerInsert,
    marker_present: bool,
    /// `block_number \t block_hash \t \N` lines returned once the marker is present.
    marker_rows: String,
    sqls: Vec<String>,
}

fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
                i += 2;
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8(out).unwrap()
}

async fn serve_one(mut sock: TcpStream, state: Arc<Mutex<State>>) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 65536];
    let head_end = loop {
        let n = sock.read(&mut tmp).await.unwrap();
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let len: usize = head
        .lines()
        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap()))
        .unwrap_or(0);
    while buf.len() < head_end + len {
        let n = sock.read(&mut tmp).await.unwrap();
        assert!(n > 0, "short body");
        buf.extend_from_slice(&tmp[..n]);
    }
    let body = String::from_utf8_lossy(&buf[head_end..head_end + len]).to_string();
    let target = head.split_whitespace().nth(1).unwrap_or_default();
    let query = target
        .split_once('?')
        .map(|(_, q)| q)
        .unwrap_or_default()
        .split('&')
        .find_map(|kv| kv.strip_prefix("query="))
        .map(url_decode);
    let sql = query.unwrap_or(body);

    let (status, answer) = {
        let mut st = state.lock().unwrap();
        st.sqls.push(sql.clone());
        if sql.starts_with("SELECT block_number, block_hash, feed_recv_ns FROM hood.blocks") {
            (200, if st.marker_present { st.marker_rows.clone() } else { String::new() })
        } else if sql.starts_with("INSERT INTO hood.blocks") {
            match st.mode {
                MarkerInsert::CommitThenDrop => {
                    st.marker_present = true;
                    return; // drop the socket: no answer at all
                }
                MarkerInsert::Fail500 => (500, "Code: 469. DB::Exception: mock failure".to_owned()),
                MarkerInsert::Fail500ButPresent => {
                    st.marker_present = true;
                    (500, "Code: 469. DB::Exception: mock failure".to_owned())
                }
            }
        } else {
            (200, String::new())
        }
    };
    let resp = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}", answer.len());
    let _ = sock.write_all(resp.as_bytes()).await;
}

/// Runs one load of the fixture against the mock; returns (error text, every SQL received).
async fn run(mode: MarkerInsert) -> (String, Vec<String>) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../decoders/tests/fixtures/l1-inflows-blocks.jsonl");
    let rows = read_blocks_file(&fixture, &GatewayRegistry::builtin()).unwrap();
    let marker_rows: String =
        rows.blocks.iter().map(|b| format!("{}\t{:#x}\t\\N\n", b.block_number, b.block_hash)).collect();
    let state = Arc::new(Mutex::new(State { mode, marker_present: false, marker_rows, sqls: Vec::new() }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let st = state.clone();
    tokio::spawn(async move {
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            tokio::spawn(serve_one(sock, st.clone()));
        }
    });
    let env = move |k: &str| match k {
        "CLICKHOUSE_URL" => Some(format!("http://127.0.0.1:{port}/")),
        "CLICKHOUSE_PASSWORD" => Some("mock".to_owned()),
        _ => None,
    };
    let ch = Client::new(ChConfig::resolve(env, "").unwrap()).unwrap();
    let err = loader::load::load_file(&ch, &FeedIndex::default(), rows).await.unwrap_err();
    let sqls = state.lock().unwrap().sqls.clone();
    (format!("{err:#}"), sqls)
}

fn deletes(sqls: &[String]) -> Vec<&str> {
    sqls.iter().filter(|s| s.starts_with("DELETE")).map(String::as_str).collect()
}

fn inserts(sqls: &[String]) -> Vec<&str> {
    sqls.iter().filter_map(|s| s.strip_prefix("INSERT INTO ")).map(|s| s.split(' ').next().unwrap()).collect()
}

#[tokio::test]
async fn committed_marker_without_answer_deletes_nothing() {
    let (err, sqls) = run(MarkerInsert::CommitThenDrop).await;
    assert_eq!(inserts(&sqls), ["hood.txs", "hood.logs", "hood.funding_edges", "hood.blocks"]);
    assert!(deletes(&sqls).is_empty(), "{sqls:?}");
    assert!(err.contains("INSERT OUTCOME UNKNOWN"), "{err}");
    assert!(!err.contains("rolled back"), "{err}");
}

#[tokio::test]
async fn server_error_with_marker_absent_rolls_back() {
    let (err, sqls) = run(MarkerInsert::Fail500).await;
    let d = deletes(&sqls);
    assert_eq!(d.len(), 3, "{sqls:?}");
    for (sql, table) in d.iter().zip(["hood.txs", "hood.logs", "hood.funding_edges"]) {
        assert!(
            sql.starts_with(&format!(
                "DELETE FROM {table} WHERE block_number IN (77285521,77285531,77300695,77312169)"
            )),
            "{sql}"
        );
    }
    assert!(
        err.contains("rolled back: rows of 4 new blocks deleted from hood.txs, hood.logs, hood.funding_edges"),
        "{err}"
    );
    assert!(err.contains("ClickHouse HTTP 500"), "{err}");
}

#[tokio::test]
async fn server_error_with_marker_present_deletes_nothing() {
    let (err, sqls) = run(MarkerInsert::Fail500ButPresent).await;
    assert!(deletes(&sqls).is_empty(), "{sqls:?}");
    assert!(err.contains("nothing rolled back: all 4 blocks are in hood.blocks"), "{err}");
}
