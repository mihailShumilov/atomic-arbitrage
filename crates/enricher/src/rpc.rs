//! JSON-RPC over HTTP with a request-rate limit, `Retry-After`, exponential
//! backoff with jitter, and a hard attempt limit. A request that keeps failing
//! ends the run with an error that names the range; nothing is skipped.
//!
//! Retry decisions use the typed [`FailKind`] and the pure [`next_step`];
//! error texts are only logged. The `--max-calls` budget is [`CallBudget`].
//!
//! Read-only: the enricher only calls `eth_*` getters. No signing, no keys.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, Context};
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::{json, Value};
use tokio::time::Instant;
use tracing::warn;

use hood_core::hex::parse_quantity;
use hood_core::http::parse_retry_after;
use hood_core::jitter::rand01;

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
    if has(&[
        "rate limit",
        "rate-limit",
        "ratelimit",
        "too many requests",
        "request limit",
        "capacity",
        "throughput",
        "credits",
        "compute units",
        "exceeded the quota",
    ]) {
        return ErrClass::RateLimit;
    }
    if has(&[
        "too many results",
        "more than",
        "results",
        "block range",
        "range too",
        "range is too",
        "too large",
        "too big",
        "response size",
        "size exceeded",
        "limit exceeded",
        "exceed max",
        "query timeout",
        "timed out",
        "timeout",
    ]) {
        return ErrClass::TooMuchData;
    }
    if code == -32005 || code == 429 {
        return ErrClass::RateLimit;
    }
    ErrClass::Other
}

/// A JSON-RPC error object.
#[derive(Debug, Clone, Deserialize)]
pub struct RpcError {
    /// JSON-RPC error code.
    pub code: i64,
    /// Provider message (may be empty).
    #[serde(default)]
    pub message: String,
}

/// One JSON-RPC response of a batch; `result` is kept as raw bytes.
#[derive(Debug, Deserialize)]
pub(crate) struct Item {
    #[serde(default)]
    pub id: Value,
    #[serde(default)]
    pub result: Option<Box<RawValue>>,
    #[serde(default)]
    pub error: Option<RpcError>,
}

/// One JSON-RPC call of a batch.
#[derive(Debug, Clone)]
pub(crate) struct Call {
    pub id: u64,
    pub method: &'static str,
    pub params: Value,
}

/// `eth_chainId`: checked once per run before the first download (task 020).
pub const M_CHAIN_ID: &str = "eth_chainId";

/// What [`Rpc::call`] does when an HTTP request times out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OnTimeout {
    /// Count it as a failed attempt and retry (blocks mode, chain id).
    Retry,
    /// Return [`CallError::Timeout`] at once (logs mode shrinks its window).
    Return,
}

/// Error of [`Rpc::call`].
#[derive(Debug)]
pub enum CallError {
    /// HTTP timeout, returned only with [`OnTimeout::Return`].
    Timeout,
    /// Sending the next request would exceed `--max-calls` (task 012 item 5:
    /// the binary exits with [`crate::exit::BUDGET_EXHAUSTED`], not 1).
    Budget(BudgetExhausted),
    /// Attempts used up or a non-retryable problem.
    Failed(anyhow::Error),
}

/// Details of a refused request: `sent + next > max`.
#[derive(Debug, Clone)]
pub struct BudgetExhausted {
    /// What was about to be fetched (for the message).
    pub what: String,
    /// Calls sent by this run so far.
    pub sent: u64,
    /// Calls the refused request (or file) needs.
    pub next: u64,
    /// `--max-calls`.
    pub max: u64,
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::Timeout => write!(f, "request timed out"),
            CallError::Budget(b) => write!(
                f,
                "{}: call budget exhausted: {} calls sent, {} more would exceed --max-calls {}",
                b.what, b.sent, b.next, b.max
            ),
            CallError::Failed(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for CallError {}

/// True if the run stopped on `--max-calls` (anywhere in the error chain).
pub fn is_budget_exhausted(e: &anyhow::Error) -> bool {
    e.chain().any(|c| matches!(c.downcast_ref::<CallError>(), Some(CallError::Budget(_))))
}

/// Hard per-run cap on JSON-RPC calls (`--max-calls`), retries included.
///
/// `try_reserve` is one atomic compare-and-swap: a request that does not fit
/// never touches the counter, so it cannot make a concurrent request that
/// does fit fail (before task 020 it was `fetch_add` → check → `fetch_sub`,
/// and a too-big batch could briefly inflate the counter and cause a false
/// early exit 75).
#[derive(Debug)]
pub(crate) struct CallBudget {
    max: Option<u64>,
    sent: AtomicU64,
}

impl CallBudget {
    pub(crate) fn new(max: Option<u64>) -> Self {
        Self { max, sent: AtomicU64::new(0) }
    }

    /// Reserve `n` calls, or refuse without changing anything.
    pub(crate) fn try_reserve(&self, n: u64, what: &str) -> Result<(), BudgetExhausted> {
        let Some(max) = self.max else {
            self.sent.fetch_add(n, Ordering::SeqCst);
            return Ok(());
        };
        self.sent
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |s| s.checked_add(n).filter(|t| *t <= max))
            .map(|_| ())
            .map_err(|sent| BudgetExhausted { what: what.to_owned(), sent, next: n, max })
    }

    /// Refuse (without reserving) if fewer than `n` calls are left.
    pub(crate) fn check(&self, n: u64, what: &str) -> Result<(), BudgetExhausted> {
        match self.max {
            Some(max) => {
                let sent = self.sent.load(Ordering::SeqCst);
                if sent.saturating_add(n) > max {
                    return Err(BudgetExhausted { what: what.to_owned(), sent, next: n, max });
                }
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// Calls reserved so far (tests only).
    #[cfg(test)]
    pub(crate) fn sent(&self) -> u64 {
        self.sent.load(Ordering::SeqCst)
    }
}

/// Why an attempt failed, as far as the retry decision is concerned. The
/// accompanying text is for the log only and never inspected (task 020
/// item 1: before, a "429" anywhere in the text, e.g. inside a block number
/// or hash, paused every task).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailKind {
    /// HTTP 429 or a JSON-RPC rate-limit error: every in-flight task pauses.
    RateLimited,
    /// Anything else retryable (timeout, transport, other status, bad or
    /// incomplete response, rejected by the caller): only this request waits.
    Transient,
}

enum Failure {
    Timeout,
    Retry { kind: FailKind, reason: String, retry_after: Option<Duration> },
}

impl Failure {
    fn retry(kind: FailKind, reason: impl Into<String>, retry_after: Option<Duration>) -> Self {
        Failure::Retry { kind, reason: reason.into(), retry_after }
    }
}

/// What to do after failed attempt number `attempt` (1-based).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// Attempts used up: stop the run with an error.
    GiveUp,
    /// Sleep `delay`, then resend; `pause_all` also holds back every other task.
    Wait { delay: Duration, pause_all: bool },
}

/// The retry decision, a pure function (unit-tested without a server or a
/// clock). `Retry-After` wins over backoff and is capped at
/// `max_retry_after`; `u` is a random number in [0, 1) for the jitter.
pub(crate) fn next_step(attempt: u32, kind: FailKind, retry_after: Option<Duration>, p: &RetryPolicy, u: f64) -> Step {
    if attempt >= p.max_attempts {
        return Step::GiveUp;
    }
    let delay = match retry_after {
        Some(d) => d.min(p.max_retry_after),
        None => backoff_delay(attempt, p.base, p.cap, u),
    };
    Step::Wait { delay, pause_all: kind == FailKind::RateLimited }
}

/// Spaces JSON-RPC calls at least `1/rps` apart across all concurrent tasks.
/// A batch of N calls consumes N slots: providers (and, as observed on
/// 2026-09-30, the public RPC) limit and bill per call, not per HTTP request.
/// Uses tokio's clock, so it follows a paused test clock too.
pub struct RateLimiter {
    interval: Duration,
    next: tokio::sync::Mutex<Instant>,
}

impl RateLimiter {
    /// `rps` = 0 means unlimited.
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
        tokio::time::sleep_until(slot).await;
    }

    /// Push the next free slot to at least `now + d`, so after a 429 every
    /// concurrent task pauses, not only the one that was rejected.
    pub async fn pause_all(&self, d: Duration) {
        let mut next = self.next.lock().await;
        *next = (*next).max(Instant::now() + d);
    }
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

/// Read-only JSON-RPC client with rate limit, retries and call budget.
pub struct Rpc {
    client: reqwest::Client,
    url: String,
    limiter: RateLimiter,
    policy: RetryPolicy,
    pub(crate) stats: Arc<Stats>,
    budget: CallBudget,
}

impl Rpc {
    /// Client for `url`; `rps` = 0 means unlimited, `timeout` is per HTTP request.
    pub fn new(url: &str, rps: f64, timeout: Duration, policy: RetryPolicy, stats: Arc<Stats>) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder().timeout(timeout).build()?;
        Ok(Self {
            client,
            url: url.to_owned(),
            limiter: RateLimiter::new(rps),
            policy,
            stats,
            budget: CallBudget::new(None),
        })
    }

    /// Stop the run (with an error) instead of sending more than `n` calls in total.
    pub fn with_max_calls(mut self, n: Option<u64>) -> Self {
        self.budget = CallBudget::new(n);
        self
    }

    /// Fail with [`CallError::Budget`] now, without sending anything, if
    /// fewer than `n` calls are left (a file that cannot be finished is not
    /// started, so its calls are not wasted).
    pub(crate) fn ensure_budget(&self, n: u64, what: &str) -> Result<(), CallError> {
        self.budget.check(n, what).map_err(CallError::Budget)
    }

    /// `eth_chainId` of the endpoint (one call, part of the budget).
    pub async fn chain_id(&self) -> Result<u64, CallError> {
        let call = Call { id: 1, method: M_CHAIN_ID, params: json!([]) };
        self.call(std::slice::from_ref(&call), M_CHAIN_ID, OnTimeout::Retry, |items| {
            let [it] = items else { return Err(format!("{M_CHAIN_ID}: {} responses for 1 call", items.len())) };
            if let Some(e) = &it.error {
                return Err(format!("{M_CHAIN_ID} error {} {}", e.code, e.message));
            }
            let raw = it.result.as_ref().ok_or_else(|| format!("{M_CHAIN_ID} returned null"))?;
            let s: String = serde_json::from_str(raw.get())
                .map_err(|_| format!("{M_CHAIN_ID} returned {}, not a string", raw.get()))?;
            parse_quantity(&s).ok_or_else(|| format!("{M_CHAIN_ID} returned {s:?}, not a hex quantity"))
        })
        .await
    }

    /// Send `calls` as one JSON-RPC batch and turn the responses (in call
    /// order, one per call) into `T` with `accept`. Retries (with rate limit,
    /// `Retry-After`, backoff) on transport errors, non-2xx, malformed or
    /// incomplete responses, JSON-RPC rate-limit errors and whatever `accept`
    /// rejects (`Err(reason)`). Other per-call JSON-RPC errors are passed to
    /// `accept`, which decides. Response sizes of an accepted batch are
    /// recorded in the stats per method.
    pub(crate) async fn call<T, F>(
        &self,
        calls: &[Call],
        what: &str,
        on_timeout: OnTimeout,
        accept: F,
    ) -> Result<T, CallError>
    where
        F: Fn(&[Item]) -> Result<T, String> + Sync,
    {
        let body: Vec<Value> = calls
            .iter()
            .map(|c| json!({"jsonrpc": "2.0", "id": c.id, "method": c.method, "params": c.params}))
            .collect();
        let body = serde_json::to_vec(&body).map_err(|e| CallError::Failed(e.into()))?;
        let n = calls.len() as u64;
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            self.budget.try_reserve(n, what).map_err(CallError::Budget)?;
            self.limiter.acquire(u32::try_from(n).unwrap_or(u32::MAX)).await;
            self.stats.update(|s| {
                s.http_requests += 1;
                for c in calls {
                    *s.calls.entry(c.method.to_owned()).or_default() += 1;
                }
            });
            let (kind, reason, retry_after) = match self.send(&body, calls, &accept).await {
                Ok(v) => return Ok(v),
                Err(Failure::Timeout) if on_timeout == OnTimeout::Return => return Err(CallError::Timeout),
                Err(Failure::Timeout) => (FailKind::Transient, "timeout".to_owned(), None),
                Err(Failure::Retry { kind, reason, retry_after }) => (kind, reason, retry_after),
            };
            match next_step(attempt, kind, retry_after, &self.policy, rand01()) {
                Step::GiveUp => {
                    return Err(CallError::Failed(anyhow!(
                        "{what}: giving up after {attempt} attempts, last error: {reason}"
                    )));
                }
                Step::Wait { delay, pause_all } => {
                    self.stats.update(|s| s.retries += 1);
                    if pause_all {
                        self.stats.update(|s| s.global_pauses += 1);
                        self.limiter.pause_all(delay).await;
                    }
                    warn!(what, attempt, kind = ?kind, reason = %reason, delay_ms = delay.as_millis() as u64, retry_after = retry_after.is_some(), "retrying");
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    async fn send<T, F>(&self, body: &[u8], calls: &[Call], accept: &F) -> Result<T, Failure>
    where
        F: Fn(&[Item]) -> Result<T, String> + Sync,
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
            .and_then(|v| parse_retry_after(v, SystemTime::now()));
        let bytes = resp.bytes().await.map_err(|e| self.transport(e))?;
        self.stats.update(|s| s.http_body_bytes += bytes.len() as u64);
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            self.stats.update(|s| s.http_429 += 1);
            return Err(Failure::retry(FailKind::RateLimited, "http 429", retry_after));
        }
        if !status.is_success() {
            self.stats.update(|s| s.http_other_status += 1);
            return Err(Failure::retry(FailKind::Transient, format!("http {status}"), retry_after));
        }
        let items = parse_batch(&bytes, calls).map_err(|(kind, reason)| {
            if kind == FailKind::RateLimited {
                self.stats.update(|s| s.rpc_rate_limited += 1);
            }
            Failure::retry(kind, reason, None)
        })?;
        if let Some(e) =
            items.iter().filter_map(|i| i.error.as_ref()).find(|e| classify(e.code, &e.message) == ErrClass::RateLimit)
        {
            self.stats.update(|s| s.rpc_rate_limited += 1);
            return Err(Failure::retry(
                FailKind::RateLimited,
                format!("rpc rate limit: {} {}", e.code, e.message),
                None,
            ));
        }
        let v = accept(&items).map_err(|r| Failure::retry(FailKind::Transient, r, None))?;
        for (it, c) in items.iter().zip(calls) {
            if let Some(raw) = &it.result {
                self.stats.record_result(c.method, raw.get());
            }
        }
        Ok(v)
    }

    fn transport(&self, e: reqwest::Error) -> Failure {
        if e.is_timeout() {
            self.stats.update(|s| s.timeouts += 1);
            Failure::Timeout
        } else {
            self.stats.update(|s| s.transport_errors += 1);
            Failure::retry(FailKind::Transient, format!("transport: {e:#}"), None)
        }
    }
}

/// Parse a batch response and reorder it to match `calls`. Every call must
/// have exactly one response; anything else is a (retryable) failure.
fn parse_batch(bytes: &[u8], calls: &[Call]) -> Result<Vec<Item>, (FailKind, String)> {
    let transient = |r: String| (FailKind::Transient, r);
    let items: Vec<Item> = match serde_json::from_slice::<Vec<Item>>(bytes) {
        Ok(v) => v,
        Err(_) => {
            // Some servers answer a whole batch with one error object.
            let one: Item = serde_json::from_slice(bytes)
                .with_context(|| {
                    format!("unparseable response: {}", String::from_utf8_lossy(&bytes[..bytes.len().min(200)]))
                })
                .map_err(|e| transient(format!("{e:#}")))?;
            if let Some(e) = &one.error {
                return Err(if classify(e.code, &e.message) == ErrClass::RateLimit {
                    (FailKind::RateLimited, format!("rpc rate limit: {} {}", e.code, e.message))
                } else {
                    transient(format!("batch error: {} {}", e.code, e.message))
                });
            }
            vec![one]
        }
    };
    let mut slots: Vec<Option<Item>> = (0..calls.len()).map(|_| None).collect();
    for it in items {
        let id = it.id.as_u64().ok_or_else(|| transient(format!("response without numeric id: {:?}", it.id)))?;
        let pos =
            calls.iter().position(|c| c.id == id).ok_or_else(|| transient(format!("unexpected response id {id}")))?;
        if slots[pos].replace(it).is_some() {
            return Err(transient(format!("duplicate response id {id}")));
        }
    }
    slots
        .into_iter()
        .zip(calls)
        .map(|(s, c)| s.ok_or_else(|| transient(format!("no response for id {} ({})", c.id, c.method))))
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
        let calls =
            vec![Call { id: 10, method: "a", params: json!([]) }, Call { id: 11, method: "b", params: json!([]) }];
        let r = br#"[{"jsonrpc":"2.0","id":11,"result":{"x":1}},{"jsonrpc":"2.0","id":10,"result":null}]"#;
        let v = parse_batch(r, &calls).unwrap();
        assert!(v[0].result.is_none());
        assert_eq!(v[1].result.as_ref().unwrap().get(), r#"{"x":1}"#);
        let (kind, msg) = parse_batch(br#"[{"id":10,"result":1}]"#, &calls).unwrap_err();
        assert_eq!(kind, FailKind::Transient);
        assert!(msg.contains("no response for id 11"), "{msg}");
        let (kind, msg) =
            parse_batch(br#"{"id":null,"error":{"code":-32005,"message":"rate limit"}}"#, &calls).unwrap_err();
        assert_eq!(kind, FailKind::RateLimited, "{msg}");
        let (kind, _) =
            parse_batch(br#"{"id":null,"error":{"code":-32000,"message":"block 77429001 missing"}}"#, &calls)
                .unwrap_err();
        assert_eq!(kind, FailKind::Transient, "a 429 inside the text is not a rate limit");
    }

    fn policy(max_attempts: u32) -> RetryPolicy {
        RetryPolicy { max_attempts, base: Duration::from_millis(100), ..RetryPolicy::default() }
    }

    /// Task 020 item 1: only the kind decides about the global pause.
    #[test]
    fn next_step_pauses_all_only_for_rate_limits() {
        let p = policy(3);
        assert_eq!(
            next_step(1, FailKind::RateLimited, None, &p, 0.0),
            Step::Wait { delay: Duration::from_millis(50), pause_all: true }
        );
        assert_eq!(
            next_step(1, FailKind::Transient, None, &p, 0.0),
            Step::Wait { delay: Duration::from_millis(50), pause_all: false }
        );
        assert_eq!(
            next_step(2, FailKind::Transient, None, &p, 0.0),
            Step::Wait { delay: Duration::from_millis(100), pause_all: false }
        );
        assert_eq!(next_step(3, FailKind::RateLimited, None, &p, 0.0), Step::GiveUp);
        assert_eq!(next_step(1, FailKind::Transient, None, &policy(1), 0.0), Step::GiveUp);
    }

    #[test]
    fn next_step_honours_and_caps_retry_after() {
        let p = policy(8);
        assert_eq!(
            next_step(1, FailKind::RateLimited, Some(Duration::from_secs(7)), &p, 0.9),
            Step::Wait { delay: Duration::from_secs(7), pause_all: true }
        );
        assert_eq!(
            next_step(1, FailKind::Transient, Some(Duration::from_secs(99_999)), &p, 0.9),
            Step::Wait { delay: p.max_retry_after, pause_all: false }
        );
    }

    #[test]
    fn budget_reserves_exactly_up_to_max() {
        let b = CallBudget::new(Some(10));
        assert!(b.try_reserve(6, "a").is_ok());
        let e = b.try_reserve(5, "b").unwrap_err();
        assert_eq!((e.sent, e.next, e.max), (6, 5, 10));
        assert_eq!(b.sent(), 6, "a refused reserve leaves the counter alone");
        assert!(b.check(4, "c").is_ok());
        assert!(b.check(5, "c").is_err());
        assert_eq!(b.sent(), 6, "check does not reserve");
        assert!(b.try_reserve(4, "c").is_ok());
        assert!(b.try_reserve(1, "d").is_err());
        let unlimited = CallBudget::new(None);
        assert!(unlimited.try_reserve(u64::MAX / 2, "x").is_ok());
        assert!(unlimited.check(u64::MAX, "x").is_ok());
    }

    /// Task 020 item 4: a request that can never fit, retried in a tight
    /// loop, never makes a concurrent request that fits fail. With the old
    /// `fetch_add` → check → `fetch_sub` the big request briefly inflated the
    /// counter and small ones failed at random (false early exit 75).
    #[test]
    fn refused_reserve_does_not_starve_concurrent_ones() {
        const SMALL_THREADS: u64 = 4;
        const SMALL_EACH: u64 = 25_000;
        let max = SMALL_THREADS * SMALL_EACH + 50;
        let b = Arc::new(CallBudget::new(Some(max)));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let big = {
            let (b, stop) = (b.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut refused = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    assert!(b.try_reserve(max + 1, "big").is_err());
                    refused += 1;
                }
                refused
            })
        };
        let small: Vec<_> = (0..SMALL_THREADS)
            .map(|_| {
                let b = b.clone();
                std::thread::spawn(move || (0..SMALL_EACH).filter(|_| b.try_reserve(1, "small").is_err()).count())
            })
            .collect();
        let refused_small: usize = small.into_iter().map(|h| h.join().unwrap()).sum();
        stop.store(true, Ordering::Relaxed);
        assert!(big.join().unwrap() > 0);
        assert_eq!(refused_small, 0, "a call that fits was refused");
        assert_eq!(b.sent(), SMALL_THREADS * SMALL_EACH);
    }

    /// Paused tokio clock: exact, no real sleeping (review I5).
    #[tokio::test(start_paused = true)]
    async fn limiter_weights_and_global_pause() {
        let l = RateLimiter::new(100.0); // 10 ms per call
        let t = Instant::now();
        l.acquire(20).await; // immediate, books 200 ms
        assert_eq!(t.elapsed(), Duration::ZERO);
        l.acquire(1).await;
        assert_eq!(t.elapsed(), Duration::from_millis(200));
        l.pause_all(Duration::from_millis(300)).await;
        let t = Instant::now();
        l.acquire(1).await;
        assert_eq!(t.elapsed(), Duration::from_millis(300));
    }

    #[test]
    fn url_redaction() {
        assert_eq!(redact_url("https://x.example.com/v2/SECRETKEY"), "https://x.example.com/…");
        assert_eq!(redact_url("https://user:pw@h.io?key=1"), "https://h.io/…");
    }
}
