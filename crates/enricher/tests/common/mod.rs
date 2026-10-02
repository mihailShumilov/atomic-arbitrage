//! Tiny hand-rolled JSON-RPC mock over HTTP/1.1 (tokio TcpListener), so the
//! tests need no extra crates. One request per connection (`Connection: close`).
//! Synthesises consistent blocks/receipts/logs for any block number.
#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Clone, Default)]
pub struct Behavior {
    /// First N HTTP requests get HTTP 429.
    pub http_429_first: usize,
    /// `Retry-After` header value sent with those 429s.
    pub retry_after: Option<&'static str>,
    /// The next M requests (after the 429s) get HTTP 200 with a JSON-RPC rate-limit error.
    pub rpc_rate_limit_next: usize,
    /// Every request: HTTP 429 forever.
    pub always_429: bool,
    /// Sleep before answering each request.
    pub delay: Duration,
    /// eth_getLogs over more blocks than this returns "more than 10000 results".
    pub logs_max_range: Option<u64>,
    /// The next K requests (after the 429s and rate-limit errors) answer
    /// eth_getBlockByNumber with `result: null` (node lag: a transient retry).
    pub null_block_next: usize,
    /// `eth_chainId` answer; default `0x1237` (Robinhood Chain, 4663).
    pub chain_id: Option<&'static str>,
}

pub struct Mock {
    pub url: String,
    /// HTTP requests except the `eth_chainId` check (counted separately).
    pub requests: Arc<AtomicUsize>,
    /// HTTP requests that only carried `eth_chainId`. They are always answered
    /// normally and do not use up the 429 / rate-limit / delay behaviours, so
    /// those keep applying to the data requests they were written for.
    pub chain_id_requests: Arc<AtomicUsize>,
    /// (method, params) of every JSON-RPC call received, incl. retried ones.
    pub calls: Arc<Mutex<Vec<(String, Value)>>>,
}

impl Mock {
    pub fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
    pub fn chain_id_requests(&self) -> usize {
        self.chain_id_requests.load(Ordering::SeqCst)
    }
    pub fn calls_of(&self, method: &str) -> Vec<Value> {
        self.calls.lock().unwrap().iter().filter(|(m, _)| m == method).map(|(_, p)| p.clone()).collect()
    }
    /// Block numbers requested via eth_getBlockByNumber.
    pub fn blocks_requested(&self) -> Vec<u64> {
        self.calls_of("eth_getBlockByNumber").iter().map(|p| hex(&p[0])).collect()
    }
}

pub fn hex(v: &Value) -> u64 {
    hood_core::hex::parse_quantity(v.as_str().unwrap()).unwrap()
}

pub fn block_hash(n: u64) -> String {
    format!("0x{:064x}", n)
}

fn tx_hash(n: u64, i: u64) -> String {
    format!("0x{:048x}{:016x}", n, i)
}

fn answer(b: &Behavior, call: &Value) -> Value {
    let id = call["id"].clone();
    let method = call["method"].as_str().unwrap_or("");
    let p = &call["params"];
    let result = match method {
        "eth_chainId" => json!(b.chain_id.unwrap_or("0x1237")),
        "eth_getBlockByNumber" => {
            let n = hex(&p[0]);
            json!({"number": p[0], "hash": block_hash(n), "parentHash": block_hash(n.wrapping_sub(1)),
                   "transactions": [{"hash": tx_hash(n, 0), "from": "0x01", "value": "0x0"}]})
        }
        "eth_getBlockReceipts" => {
            let n = hex(&p[0]);
            json!([{"blockHash": block_hash(n), "blockNumber": p[0], "transactionHash": tx_hash(n, 0),
                    "status": "0x1", "gasUsed": "0x5208", "logs": []}])
        }
        "eth_getLogs" => {
            let f = &p[0];
            let (a, z) = (hex(&f["fromBlock"]), hex(&f["toBlock"]));
            if b.logs_max_range.is_some_and(|m| z - a + 1 > m) {
                return json!({"jsonrpc": "2.0", "id": id,
                              "error": {"code": -32005, "message": "query returned more than 10000 results"}});
            }
            let t0 = f["topics"][0][0].clone();
            Value::Array(
                (a..=z)
                    .map(|n| {
                        json!({"blockNumber": hood_core::hex::quantity(n), "blockHash": block_hash(n),
                                    "logIndex": "0x0", "transactionIndex": "0x0", "transactionHash": tx_hash(n, 0),
                                    "address": "0x02", "topics": [t0], "data": "0x", "removed": false})
                    })
                    .collect(),
            )
        }
        _ => return json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "method not found"}}),
    };
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    let header_end = loop {
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_ascii_lowercase();
    let len: usize =
        head.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse().ok()).unwrap_or(0);
    while buf.len() < header_end + len {
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    Some(buf[header_end..header_end + len].to_vec())
}

pub async fn start(b: Behavior) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(AtomicUsize::new(0));
    let chain_id_requests = Arc::new(AtomicUsize::new(0));
    let calls: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
    let (rq, cq, cl) = (requests.clone(), chain_id_requests.clone(), calls.clone());
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            let (b, rq, cq, cl) = (b.clone(), rq.clone(), cq.clone(), cl.clone());
            tokio::spawn(async move {
                let Some(body) = read_request(&mut sock).await else { return };
                let req: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let batch: Vec<Value> = match &req {
                    Value::Array(v) => v.clone(),
                    other => vec![other.clone()],
                };
                for c in &batch {
                    cl.lock().unwrap().push((c["method"].as_str().unwrap_or("").to_owned(), c["params"].clone()));
                }
                let chain_only = !batch.is_empty() && batch.iter().all(|c| c["method"] == "eth_chainId");
                let k = if chain_only {
                    cq.fetch_add(1, Ordering::SeqCst);
                    usize::MAX
                } else {
                    rq.fetch_add(1, Ordering::SeqCst)
                };
                if !chain_only && !b.delay.is_zero() {
                    tokio::time::sleep(b.delay).await;
                }
                let (status, extra, body) = if chain_only {
                    let v: Vec<Value> = batch.iter().map(|c| answer(&b, c)).collect();
                    ("200 OK", String::new(), serde_json::to_vec(&v).unwrap())
                } else if b.always_429 || k < b.http_429_first {
                    let ra = b.retry_after.map(|v| format!("Retry-After: {v}\r\n")).unwrap_or_default();
                    ("429 Too Many Requests", ra, b"{\"error\":\"rate limited\"}".to_vec())
                } else if k < b.http_429_first + b.rpc_rate_limit_next {
                    let v: Vec<Value> = batch
                        .iter()
                        .map(|c| json!({"jsonrpc":"2.0","id":c["id"],"error":{"code":-32005,"message":"rate limit exceeded"}}))
                        .collect();
                    ("200 OK", String::new(), serde_json::to_vec(&v).unwrap())
                } else if k < b.http_429_first + b.rpc_rate_limit_next + b.null_block_next {
                    let v: Vec<Value> = batch
                        .iter()
                        .map(|c| match c["method"].as_str() {
                            Some("eth_getBlockByNumber") => json!({"jsonrpc":"2.0","id":c["id"],"result":null}),
                            _ => answer(&b, c),
                        })
                        .collect();
                    ("200 OK", String::new(), serde_json::to_vec(&v).unwrap())
                } else {
                    let v: Vec<Value> = batch.iter().map(|c| answer(&b, c)).collect();
                    ("200 OK", String::new(), serde_json::to_vec(&v).unwrap())
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(&body).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    Mock { url, requests, chain_id_requests, calls }
}

#[path = "../../src/testdir.rs"]
mod testdir;
pub use testdir::TestDir as Scratch;

/// Fresh empty `$TMPDIR/enricher-it-<name>-<pid>`, removed when dropped.
pub fn scratch(name: &str) -> Scratch {
    Scratch::new(&format!("it-{name}"))
}

/// Decompress a jsonl.zst file into parsed lines.
pub fn read_jsonl_zst(p: &std::path::Path) -> Vec<Value> {
    let raw = zstd::decode_all(std::fs::File::open(p).unwrap()).unwrap();
    String::from_utf8(raw).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

pub fn args(extra: &[&str]) -> enricher::Args {
    use clap::Parser;
    let mut v = vec!["enricher"];
    v.extend_from_slice(extra);
    enricher::Args::try_parse_from(v).unwrap()
}
