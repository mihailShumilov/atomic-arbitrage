//! Run counters: calls per method, bytes received, 429s, and per-method
//! response sizes (raw JSON and zstd-3 stream) for decision 0001.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::sync::Mutex;
use std::time::Instant;

use serde::Serialize;

#[derive(Debug, Default, Clone, Serialize)]
pub struct Counters {
    /// HTTP requests sent (a JSON-RPC batch is one request), incl. retries.
    pub http_requests: u64,
    /// Response body bytes received over HTTP (all responses, incl. errors).
    pub http_body_bytes: u64,
    pub http_429: u64,
    /// Non-2xx other than 429.
    pub http_other_status: u64,
    pub transport_errors: u64,
    pub timeouts: u64,
    /// JSON-RPC level rate-limit errors inside an HTTP 200 response.
    pub rpc_rate_limited: u64,
    /// Requests re-sent after a failure.
    pub retries: u64,
    /// JSON-RPC calls sent per method, incl. retries (what a provider bills).
    pub calls: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MethodSize {
    /// Accepted non-null results.
    pub results: u64,
    /// Raw JSON bytes of the `result` values exactly as received.
    pub raw_bytes: u64,
    pub raw_avg: f64,
    /// Size after zstd level 3 over the stream of results of this method.
    pub zstd_bytes: u64,
    pub zstd_avg: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub elapsed_s: f64,
    #[serde(flatten)]
    pub counters: Counters,
    pub sizes: BTreeMap<String, MethodSize>,
}

#[derive(Default)]
struct CountingSink(u64);

impl Write for CountingSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0 += buf.len() as u64;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Sizer {
    results: u64,
    raw: u64,
    enc: zstd::Encoder<'static, CountingSink>,
}

pub struct Stats {
    started: Instant,
    c: Mutex<Counters>,
    sizes: Mutex<BTreeMap<String, Sizer>>,
}

impl Default for Stats {
    fn default() -> Self {
        Self { started: Instant::now(), c: Mutex::default(), sizes: Mutex::default() }
    }
}

impl Stats {
    pub fn update(&self, f: impl FnOnce(&mut Counters)) {
        f(&mut self.c.lock().unwrap())
    }

    pub fn counters(&self) -> Counters {
        self.c.lock().unwrap().clone()
    }

    /// Record one accepted result of `method` with its raw JSON text.
    pub fn record_result(&self, method: &str, raw: &str) {
        let mut m = self.sizes.lock().unwrap();
        let s = m.entry(method.to_owned()).or_insert_with(|| Sizer {
            results: 0,
            raw: 0,
            enc: zstd::Encoder::new(CountingSink::default(), 3).expect("zstd encoder"),
        });
        s.results += 1;
        s.raw += raw.len() as u64;
        // Newline-separated, like the jsonl files.
        let _ = s.enc.write_all(raw.as_bytes());
        let _ = s.enc.write_all(b"\n");
    }

    /// Final numbers. Finishes the zstd sizers, so call once at the end.
    pub fn summary(&self) -> Summary {
        let mut sizes = BTreeMap::new();
        for (k, s) in std::mem::take(&mut *self.sizes.lock().unwrap()) {
            let z = s.enc.finish().map(|c| c.0).unwrap_or(0);
            let avg = |x: u64| if s.results == 0 { 0.0 } else { x as f64 / s.results as f64 };
            sizes.insert(
                k,
                MethodSize { results: s.results, raw_bytes: s.raw, raw_avg: avg(s.raw), zstd_bytes: z, zstd_avg: avg(z) },
            );
        }
        Summary { elapsed_s: self.started.elapsed().as_secs_f64(), counters: self.counters(), sizes }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_counted() {
        let s = Stats::default();
        s.record_result("eth_getBlockReceipts", "[1,2,3]");
        s.record_result("eth_getBlockReceipts", "[]");
        s.update(|c| *c.calls.entry("eth_getBlockReceipts".into()).or_default() += 2);
        let sum = s.summary();
        let m = &sum.sizes["eth_getBlockReceipts"];
        assert_eq!(m.results, 2);
        assert_eq!(m.raw_bytes, 9);
        assert!((m.raw_avg - 4.5).abs() < 1e-9);
        assert!(m.zstd_bytes > 0);
        assert_eq!(sum.counters.calls["eth_getBlockReceipts"], 2);
    }
}
