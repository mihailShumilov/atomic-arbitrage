//! JSON-RPC over HTTP with a request-rate limit, `Retry-After`, exponential
//! backoff with jitter, and a hard attempt limit. A request that keeps failing
//! ends the run with an error that names the range; nothing is skipped.
//!
//! Read-only: the enricher only calls `eth_*` getters. No signing, no keys.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context};
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::{json, Value};
use tracing::warn;

use crate::stats::Stats;

#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Total attempts per request (first try included).
    pub max_attempts: u32,
    pub base: Duration,
    pub cap: Duration,
    /// Upper bound for an honoured `Retry-After`.
    pub max_retry_after: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 8,
            base: Duration::from_millis(500),
            cap: Duration::from_secs(60),
            max_retry_after: Duration::from_secs(600),
        }
    }
}

/// Delay before retry number `attempt` (1-based): exponential `base·2^(attempt-1)`
/// capped at `cap`, with "equal jitter" — uniformly in [exp/2, exp]. `u` is a
/// random number in [0, 1).
pub fn backoff_delay(attempt: u32, base: Duration, cap: Duration, u: f64) -> Duration {
    let exp = base.saturating_mul(1u32 << attempt.saturating_sub(1).min(20)).min(cap);
    let half = exp / 2;
    half + half.mul_f64(u.clamp(0.0, 1.0))
}

/// `Retry-After` in delta-seconds form (integer or decimal). The HTTP-date
/// form is not supported and falls back to the exponential backoff.
pub fn parse_retry_after(v: &str) -> Option<Duration> {
    let x: f64 = v.trim().parse().ok()?;
    (x.is_finite() && x >= 0.0).then(|| Duration::from_secs_f64(x))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrClass {
    /// Back off and resend the same request.
    RateLimit,
    /// The request asks for too much (eth_getLogs range / result limit): shrink it.
    TooMuchData,
    Other,
}

/// Classify a JSON-RPC error. Message patterns win over codes because
/// providers reuse codes (e.g. Infura's -32005 means both "rate limited" and
/// "query returned more than 10000 results").
pub fn classify(code: i64, msg: &str) -> ErrClass {
    let m = msg.to_ascii_lowercase();
    let has = |p: &[&str]| p.iter().any(|x| m.contains(x));
    if has(&["rate limit", "rate-limit", "ratelimit", "too many requests", "request limit", "capacity", "throughput", "credits", "compute units", "exceeded the quota"]) {
        return ErrClass::RateLimit;
    }
    if has(&["too many results", "more than", "results", "block range", "range too", "range is too", "too large", "too big", "response size", "size exceeded", "limit exceeded", "exceed max", "query timeout", "timed out", "timeout"]) {
        return ErrClass::TooMuchData;
    }
    if code == -32005 || code == 429 {
        return ErrClass::RateLimit;
    }
    ErrClass::Other
}

#[derive(Debug, Clone, Deserialize)]
pub struct RpcError {
    pub code: i64,
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct Item {
    #[serde(default)]
    pub id: Value,
    #[serde(default)]
    pub result: Option<Box<RawValue>>,
    #[serde(default)]
    pub error: Option<RpcError>,
}

#[derive(Debug, Clone)]
pub struct Call {
    pub id: u64,
    pub method: &'static str,
    pub params: Value,
}

/// Verdict of the caller-supplied check on a complete batch response.
pub enum Check {
    Accept,
    /// Transient problem (null result, node lag…): counts as a failed attempt.
    Retry(String),
}

#[derive(Debug)]
pub enum CallError {
    /// HTTP timeout, returned only when the caller asked for it (logs mode shrinks its window).
    Timeout,
    Failed(anyhow::Error),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::Timeout => write!(f, "request timed out"),
            CallError::Failed(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for CallError {}

/// Spaces JSON-RPC calls at least `1/rps` apart across all concurrent tasks.
/// A batch of N calls consumes N slots: providers (and, as observed on
/// 2026-09-30, the public RPC) limit and bill per call, not per HTTP request.
pub struct RateLimiter {
    interval: Duration,
    next: tokio::sync::Mutex<Instant>,
}

impl RateLimiter {
    pub fn new(rps: f64) -> Self {
        let interval = if rps > 0.0 { Duration::from_secs_f64(1.0 / rps) } else { Duration::ZERO };
        Self { interval, next: tokio::sync::Mutex::new(Instant::now()) }
    }

    /// Wait for a slot for `weight` calls.
    pub async fn acquire(&self, weight: u32) {
        let slot = {
            let mut next = self.next.lock().await;
            let slot = (*next).max(Instant::now());
            *next = slot + self.interval * weight.max(1);
            slot
        };
        tokio::time::sleep_until(slot.into()).await;
    }

    /// Push the next free slot to at least `now + d`, so after a 429 every
    /// concurrent task pauses, not only the one that was rejected.
    pub async fn pause_all(&self, d: Duration) {
        let mut next = self.next.lock().await;
        *next = (*next).max(Instant::now() + d);
    }
}

fn jitter() -> f64 {
    static STATE: AtomicU64 = AtomicU64::new(0);
    let mut x = STATE.load(Ordering::Relaxed);
    if x == 0 {
        x = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1) | 1;
    }
    // xorshift64
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    STATE.store(x, Ordering::Relaxed);
    (x >> 11) as f64 / (1u64 << 53) as f64
}

/// Keep only scheme and host: provider URLs often carry the API key in the path.
pub fn redact_url(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => {
            let host = rest.split(['/', '?']).next().unwrap_or("");
            let host = host.rsplit('@').next().unwrap_or(host);
            format!("{scheme}://{host}/…")
        }
        None => "<rpc>".into(),
    }
}

pub struct Rpc {
    client: reqwest::Client,
    url: String,
    limiter: RateLimiter,
    pub policy: RetryPolicy,
    pub stats: Arc<Stats>,
    /// Hard cap on JSON-RPC calls sent by this run (retries included).
    max_calls: Option<u64>,
    calls_sent: AtomicU64,
}

impl Rpc {
    pub fn new(url: &str, rps: f64, timeout: Duration, policy: RetryPolicy, stats: Arc<Stats>) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder().timeout(timeout).build()?;
        Ok(Self {
            client,
            url: url.to_owned(),
            limiter: RateLimiter::new(rps),
            policy,
            stats,
            max_calls: None,
            calls_sent: AtomicU64::new(0),
        })
    }

    /// Stop the run (with an error) instead of sending more than `n` calls in total.
    pub fn with_max_calls(mut self, n: Option<u64>) -> Self {
        self.max_calls = n;
        self
    }

    /// Send `calls` as one JSON-RPC batch and return the responses in call
    /// order. Retries (with rate limit, `Retry-After`, backoff) on transport
    /// errors, non-2xx, malformed/incomplete responses, JSON-RPC rate-limit
    /// errors and whatever `check` rejects. Other per-call JSON-RPC errors are
    /// passed to `check`, which decides.
    pub async fn call<F>(&self, calls: &[Call], what: &str, return_timeout: bool, check: F) -> Result<Vec<Item>, CallError>
    where
        F: Fn(&[Item]) -> Check,
    {
        let body: Vec<Value> = calls
            .iter()
            .map(|c| json!({"jsonrpc": "2.0", "id": c.id, "method": c.method, "params": c.params}))
            .collect();
        let body = serde_json::to_vec(&body).map_err(|e| CallError::Failed(e.into()))?;
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let n = calls.len() as u64;
            if let Some(max) = self.max_calls {
                let sent = self.calls_sent.fetch_add(n, Ordering::SeqCst);
                if sent + n > max {
                    self.calls_sent.fetch_sub(n, Ordering::SeqCst);
                    return Err(CallError::Failed(anyhow!(
                        "{what}: call budget exhausted: {sent} calls sent, {n} more would exceed --max-calls {max}"
                    )));
                }
            }
            self.limiter.acquire(n.min(u32::MAX as u64) as u32).await;
            self.stats.update(|s| {
                s.http_requests += 1;
                for c in calls {
                    *s.calls.entry(c.method.to_owned()).or_default() += 1;
                }
            });
            let (reason, retry_after) = match self.send(&body, calls, &check).await {
                Ok(items) => return Ok(items),
                Err(Failure::Timeout) if return_timeout => return Err(CallError::Timeout),
                Err(Failure::Timeout) => ("timeout".to_owned(), None),
                Err(Failure::Retry(r, ra)) => (r, ra),
            };
            if attempt >= self.policy.max_attempts {
                return Err(CallError::Failed(anyhow!(
                    "{what}: giving up after {attempt} attempts, last error: {reason}"
                )));
            }
            let delay = match retry_after {
                Some(d) => d.min(self.policy.max_retry_after),
                None => backoff_delay(attempt, self.policy.base, self.policy.cap, jitter()),
            };
            self.stats.update(|s| s.retries += 1);
            if reason.contains("429") || reason.starts_with("rpc rate limit") {
                self.limiter.pause_all(delay).await;
            }
            warn!(what, attempt, reason = %reason, delay_ms = delay.as_millis() as u64, retry_after = retry_after.is_some(), "retrying");
            tokio::time::sleep(delay).await;
        }
    }

    async fn send<F>(&self, body: &[u8], calls: &[Call], check: &F) -> Result<Vec<Item>, Failure>
    where
        F: Fn(&[Item]) -> Check,
    {
        let resp = self
            .client
            .post(&self.url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_vec())
            .send()
            .await
            .map_err(|e| self.transport(e))?;
        let status = resp.status();
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_retry_after);
        let bytes = resp.bytes().await.map_err(|e| self.transport(e))?;
        self.stats.update(|s| s.http_body_bytes += bytes.len() as u64);
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            self.stats.update(|s| s.http_429 += 1);
            return Err(Failure::Retry("http 429".into(), retry_after));
        }
        if !status.is_success() {
            self.stats.update(|s| s.http_other_status += 1);
            return Err(Failure::Retry(format!("http {status}"), retry_after));
        }
        let items = parse_batch(&bytes, calls).map_err(|r| {
            if r.starts_with("rpc rate limit") {
                self.stats.update(|s| s.rpc_rate_limited += 1);
            }
            Failure::Retry(r, None)
        })?;
        if let Some(e) = items
            .iter()
            .filter_map(|i| i.error.as_ref())
            .find(|e| classify(e.code, &e.message) == ErrClass::RateLimit)
        {
            self.stats.update(|s| s.rpc_rate_limited += 1);
            return Err(Failure::Retry(format!("rpc rate limit: {} {}", e.code, e.message), None));
        }
        match check(&items) {
            Check::Accept => Ok(items),
            Check::Retry(r) => Err(Failure::Retry(r, None)),
        }
    }

    fn transport(&self, e: reqwest::Error) -> Failure {
        if e.is_timeout() {
            self.stats.update(|s| s.timeouts += 1);
            Failure::Timeout
        } else {
            self.stats.update(|s| s.transport_errors += 1);
            Failure::Retry(format!("transport: {e:#}"), None)
        }
    }
}

enum Failure {
    Timeout,
    Retry(String, Option<Duration>),
}

/// Parse a batch response and reorder it to match `calls`. Every call must
/// have exactly one response; anything else is a (retryable) failure.
fn parse_batch(bytes: &[u8], calls: &[Call]) -> Result<Vec<Item>, String> {
    let items: Vec<Item> = match serde_json::from_slice::<Vec<Item>>(bytes) {
        Ok(v) => v,
        Err(_) => {
            // Some servers answer a whole batch with one error object.
            let one: Item = serde_json::from_slice(bytes)
                .with_context(|| format!("unparseable response: {}", String::from_utf8_lossy(&bytes[..bytes.len().min(200)])))
                .map_err(|e| format!("{e:#}"))?;
            if let Some(e) = &one.error {
                let kind = if classify(e.code, &e.message) == ErrClass::RateLimit { "rpc rate limit" } else { "batch error" };
                return Err(format!("{kind}: {} {}", e.code, e.message));
            }
            vec![one]
        }
    };
    let mut slots: Vec<Option<Item>> = (0..calls.len()).map(|_| None).collect();
    for it in items {
        let id = it.id.as_u64().ok_or_else(|| format!("response without numeric id: {:?}", it.id))?;
        let pos = calls.iter().position(|c| c.id == id).ok_or_else(|| format!("unexpected response id {id}"))?;
        if slots[pos].replace(it).is_some() {
            return Err(format!("duplicate response id {id}"));
        }
    }
    slots
        .into_iter()
        .zip(calls)
        .map(|(s, c)| s.ok_or_else(|| format!("no response for id {} ({})", c.id, c.method)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_is_capped() {
        let b = Duration::from_millis(500);
        let cap = Duration::from_secs(60);
        assert_eq!(backoff_delay(1, b, cap, 0.0), Duration::from_millis(250));
        assert_eq!(backoff_delay(1, b, cap, 0.999_999).as_millis(), 499);
        assert_eq!(backoff_delay(3, b, cap, 0.0), Duration::from_millis(1000));
        assert_eq!(backoff_delay(30, b, cap, 0.0), Duration::from_secs(30));
        assert!(backoff_delay(30, b, cap, 0.999) <= cap);
        for _ in 0..1000 {
            let u = jitter();
            assert!((0.0..1.0).contains(&u));
        }
    }

    #[test]
    fn retry_after_parsing() {
        assert_eq!(parse_retry_after("2"), Some(Duration::from_secs(2)));
        assert_eq!(parse_retry_after(" 0.5 "), Some(Duration::from_millis(500)));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
        assert_eq!(parse_retry_after("-1"), None);
    }

    #[test]
    fn error_classes() {
        assert_eq!(classify(-32005, "rate limit exceeded"), ErrClass::RateLimit);
        assert_eq!(classify(-32005, "query returned more than 10000 results"), ErrClass::TooMuchData);
        assert_eq!(classify(-32000, "block range is too large"), ErrClass::TooMuchData);
        assert_eq!(classify(-32000, "Too Many Requests"), ErrClass::RateLimit);
        assert_eq!(classify(-32005, "whatever"), ErrClass::RateLimit);
        assert_eq!(classify(-32000, "header not found"), ErrClass::Other);
    }

    #[test]
    fn batch_is_reordered_and_checked() {
        let calls = vec![
            Call { id: 10, method: "a", params: json!([]) },
            Call { id: 11, method: "b", params: json!([]) },
        ];
        let r = br#"[{"jsonrpc":"2.0","id":11,"result":{"x":1}},{"jsonrpc":"2.0","id":10,"result":null}]"#;
        let v = parse_batch(r, &calls).unwrap();
        assert!(v[0].result.is_none());
        assert_eq!(v[1].result.as_ref().unwrap().get(), r#"{"x":1}"#);
        assert!(parse_batch(br#"[{"id":10,"result":1}]"#, &calls).unwrap_err().contains("no response for id 11"));
        assert!(parse_batch(br#"{"id":null,"error":{"code":-32005,"message":"rate limit"}}"#, &calls)
            .unwrap_err()
            .starts_with("rpc rate limit"));
    }

    #[tokio::test]
    async fn limiter_weights_and_global_pause() {
        let l = RateLimiter::new(100.0); // 10 ms per call
        let t = Instant::now();
        l.acquire(20).await; // immediate, books 200 ms
        l.acquire(1).await;
        assert!(t.elapsed() >= Duration::from_millis(195), "{:?}", t.elapsed());
        l.pause_all(Duration::from_millis(300)).await;
        let t = Instant::now();
        l.acquire(1).await;
        assert!(t.elapsed() >= Duration::from_millis(290), "{:?}", t.elapsed());
    }

    #[test]
    fn url_redaction() {
        assert_eq!(redact_url("https://x.example.com/v2/SECRETKEY"), "https://x.example.com/…");
        assert_eq!(redact_url("https://user:pw@h.io?key=1"), "https://h.io/…");
    }
}
