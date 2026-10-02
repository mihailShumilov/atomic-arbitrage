//! `logs` mode: `eth_getLogs` over a block range, filtered by topic0, with an
//! adaptive window.
//!
//! Output: `<dir>/logs-<from>-<to>.jsonl.zst`. It can never be mistaken for
//! full blocks: different directory (`data/logs`), different prefix, a meta
//! header line, and lines carry `logs` instead of `block`/`receipts`:
//!   line 1: {"meta": {"format": "hood-logs-v1", "mode": "logs", ...}}
//!   then one line per block of the range, in order, empty blocks included:
//!           {"number": N, "logs": [<raw eth_getLogs objects>]}
//! Reverted transactions emit no logs, so they are absent here by
//! construction; only `blocks` mode shows them.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tracing::{info, warn};

use hood_core::hex::{parse_quantity, quantity};
use hood_core::ranges::Range;

use crate::atomic::AtomicZstdFile;
use crate::rpc::{classify, Call, CallError, ErrClass, Item, OnTimeout, Rpc};

pub const M_LOGS: &str = "eth_getLogs";
pub const FORMAT: &str = "hood-logs-v1";

pub fn file_name(r: Range) -> String {
    format!("logs-{}-{}.jsonl.zst", r.from, r.to)
}

/// Default filter: every topic0 known to `crates/decoders`.
pub fn default_topics() -> Vec<String> {
    decoders::ALL_TOPIC0.iter().map(|t| format!("{t}")).collect()
}

pub fn validate_topic(t: &str) -> Result<String> {
    let t = t.trim().to_ascii_lowercase();
    if t.len() != 66 || !t.starts_with("0x") || !t[2..].bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("bad topic0 {t:?}: need 0x + 64 hex chars");
    }
    Ok(t)
}

/// Adaptive block window: halve on "too much data"/timeout, double on success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub cur: u64,
    pub max: u64,
}

impl Window {
    pub fn new(start: u64, max: u64) -> Self {
        let max = max.max(1);
        Self { cur: start.clamp(1, max), max }
    }
    pub fn grow(&mut self) {
        self.cur = self.cur.saturating_mul(2).min(self.max);
    }
    /// Returns false if the window is already a single block.
    pub fn shrink(&mut self) -> bool {
        if self.cur == 1 {
            return false;
        }
        self.cur = (self.cur / 2).max(1);
        true
    }
}

pub struct LogsOpts {
    pub topics: Vec<String>,
    pub window: u64,
    pub window_max: u64,
}

enum WindowResult {
    Logs(Vec<Value>),
    TooMuch(String),
}

async fn get_logs(rpc: &Rpc, a: u64, b: u64, topics: &[String]) -> Result<WindowResult> {
    let call = Call {
        id: 1,
        method: M_LOGS,
        params: json!([{"fromBlock": quantity(a), "toBlock": quantity(b), "topics": [topics]}]),
    };
    let what = format!("logs {a}..={b}");
    // Too much data (error) or the log array; anything else is retried.
    let accept = |items: &[Item]| -> Result<WindowResult, String> {
        let [it] = items else { return Err(format!("{M_LOGS}: {} responses for 1 call", items.len())) };
        match (&it.error, &it.result) {
            (Some(e), _) if classify(e.code, &e.message) == ErrClass::TooMuchData => {
                Ok(WindowResult::TooMuch(format!("{} {}", e.code, e.message)))
            }
            (Some(e), _) => Err(format!("{M_LOGS} error {} {}", e.code, e.message)),
            (None, None) => Err(format!("{M_LOGS} returned null")),
            (None, Some(raw)) => serde_json::from_str::<Vec<Value>>(raw.get())
                .map(WindowResult::Logs)
                .map_err(|e| format!("{M_LOGS} result is not an array of logs: {e}")),
        }
    };
    match rpc.call(std::slice::from_ref(&call), &what, OnTimeout::Return, accept).await {
        Ok(w) => Ok(w),
        Err(CallError::Timeout) => Ok(WindowResult::TooMuch("timeout".into())),
        Err(e) => Err(e.into()),
    }
}

/// Download logs for `r` into `<dir>/logs-<from>-<to>.jsonl.zst` atomically.
/// Returns the path and the number of logs written.
pub async fn write_range(rpc: &Rpc, dir: &Path, r: Range, o: &LogsOpts) -> Result<(PathBuf, u64)> {
    anyhow::ensure!(!o.topics.is_empty(), "empty topic0 filter");
    let path = dir.join(file_name(r));
    let mut f = AtomicZstdFile::create(&path, 3)?;
    let created = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let meta = json!({"meta": {
        "format": FORMAT,
        "mode": "logs",
        "method": M_LOGS,
        "from": r.from,
        "to": r.to,
        "topic0_any_of": o.topics,
        "reverted_txs": "absent: eth_getLogs only returns logs of successful transactions; use blocks mode for status/gas/reverts",
        "not_full_blocks": true,
        "created_unix": created,
    }});
    serde_json::to_writer(&mut f, &meta)?;
    f.write_all(b"\n")?;
    info!(from = r.from, to = r.to, topics = o.topics.len(), file = %path.display(), "logs start");

    let mut w = Window::new(o.window, o.window_max);
    let mut start = r.from;
    let mut total = 0u64;
    while start <= r.to {
        let end = start.saturating_add(w.cur - 1).min(r.to);
        match get_logs(rpc, start, end, &o.topics).await? {
            WindowResult::TooMuch(why) => {
                let before = w.cur;
                if !w.shrink() {
                    bail!("logs {start}..={end}: single-block window still fails ({why})");
                }
                warn!(from = start, to = end, why = %why, window_before = before, window = w.cur, "shrinking window");
            }
            WindowResult::Logs(logs) => {
                let mut by_block: BTreeMap<u64, Vec<(u64, Value)>> = BTreeMap::new();
                for l in logs {
                    let n = l["blockNumber"].as_str().and_then(parse_quantity).context("log without blockNumber")?;
                    if n < start || n > end {
                        bail!("logs {start}..={end}: node returned a log from block {n}");
                    }
                    if l["removed"].as_bool() == Some(true) {
                        warn!(block = n, "log marked removed=true (kept as-is)");
                    }
                    let idx = l["logIndex"].as_str().and_then(parse_quantity).context("log without logIndex")?;
                    by_block.entry(n).or_default().push((idx, l));
                }
                for n in start..=end {
                    let mut ls = by_block.remove(&n).unwrap_or_default();
                    ls.sort_by_key(|(i, _)| *i);
                    total += ls.len() as u64;
                    let ls: Vec<Value> = ls.into_iter().map(|(_, l)| l).collect();
                    serde_json::to_writer(&mut f, &json!({"number": n, "logs": ls}))?;
                    f.write_all(b"\n")?;
                }
                start = end + 1;
                w.grow();
            }
        }
    }
    let path = f.commit()?;
    info!(file = %path.display(), blocks = r.blocks(), logs = total, "logs done");
    Ok((path, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_adapts() {
        let mut w = Window::new(500, 2000);
        w.grow();
        assert_eq!(w.cur, 1000);
        w.grow();
        w.grow();
        assert_eq!(w.cur, 2000);
        assert!(w.shrink());
        assert_eq!(w.cur, 1000);
        let mut one = Window::new(0, 10);
        assert_eq!(one.cur, 1);
        assert!(!one.shrink());
    }

    #[test]
    fn topics() {
        let d = default_topics();
        assert_eq!(d.len(), decoders::ALL_TOPIC0.len());
        for t in &d {
            assert_eq!(validate_topic(t).unwrap(), *t);
        }
        assert!(d.contains(&"0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef".to_string()));
        assert!(validate_topic("0x12").is_err());
    }
}
