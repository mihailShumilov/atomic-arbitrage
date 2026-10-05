//! One feed connection: connect, frame loop into the writer channel, and the
//! client-initiated close (task 002 items 4, 5; task 009; task 012 item 3).
//! Since task 021 every way the recorder ends a connection itself goes
//! through one code path ([`ClientEnd`]) instead of four copies.

use std::sync::mpsc::SyncSender;
use std::time::{Duration, Instant};

use hood_core::redact::redact_url;
use tokio::sync::watch;
use tokio_rustls::TlsConnector;
use tracing::{info, warn};
use yawc::{frame::OpCode, Frame};

use crate::backoff::EndKind;
use crate::now_ns;
use crate::resume::Backlog;
use crate::route::{route_opaque, route_text, Line};
use crate::seqtrack::{Seen, SeqTracker};
use crate::transport::{close_handshake, close_summary, connect, opcode_name, CloseOutcome, ConnectError, FeedWs};

/// Stop signal for the network loop: `Some(close reason)` once the process
/// is shutting down (signal: `"recorder shutdown"`, fatal writer error:
/// `"recorder writer error"`, task 012 item 2).
pub type Stop = watch::Receiver<Option<&'static str>>;

/// Close reason when the stop signal carries none (sender gone).
pub const SHUTDOWN_REASON: &str = "recorder shutdown";

/// Resolves once a stop reason is set (or its sender is gone).
pub async fn stopped(stop: &mut Stop) {
    let _ = stop.wait_for(|s| s.is_some()).await;
}

/// Close reason of the stop signal, if stopping.
pub fn stop_reason(stop: &Stop) -> Option<&'static str> {
    *stop.borrow()
}

/// How a connection attempt / session ended.
#[derive(Debug, Clone)]
pub struct ConnEnd {
    pub kind: EndKind,
    /// HTTP status of the upgrade response (101 on success), if seen.
    pub http_status: Option<u16>,
    pub retry_after_raw: Option<String>,
    pub retry_after: Option<Duration>,
    /// Time spent upgraded; zero if the upgrade failed.
    pub session: Duration,
    pub envelopes: u64,
    pub detail: String,
    /// Set when the connection was ended by us after the upgrade (shutdown,
    /// idle timeout, block idle, writer gone): outcome of our Close handshake.
    pub client_close: Option<Box<CloseOutcome>>,
    /// Statistics of the first burst; None if the upgrade failed.
    pub backlog: Option<Box<Backlog>>,
}

impl From<ConnectError> for ConnEnd {
    fn from(e: ConnectError) -> Self {
        ConnEnd {
            kind: e.kind,
            http_status: e.http_status,
            retry_after_raw: e.retry_after_raw,
            retry_after: e.retry_after,
            session: Duration::ZERO,
            envelopes: 0,
            detail: e.detail,
            client_close: None,
            backlog: None,
        }
    }
}

/// Frame -> raw line, plus the summary of a server Close frame.
pub fn route_frame(frame: &Frame, recv_ns: u128) -> (Line, Option<String>) {
    let op = frame.opcode();
    match op {
        OpCode::Text => match std::str::from_utf8(frame.payload()) {
            Ok(s) => (route_text(recv_ns, s.to_owned()), None),
            Err(_) => (route_opaque(recv_ns, "text_invalid_utf8", frame.payload()), None),
        },
        other => (route_opaque(recv_ns, opcode_name(other), frame.payload()), close_summary(frame)),
    }
}

/// Where received frames go: the writer channel, plus the bookkeeping the
/// network side needs for the next handshake (task 009): the highest seq
/// handed to the writer (`seq`, survives reconnects inside the process)
/// and the burst statistics of the current connection.
pub struct Sink<'a> {
    tx: &'a SyncSender<Line>,
    seq: &'a mut SeqTracker,
    backlog: Backlog,
    envelopes: u64,
    /// Set when the burst ended; the caller logs it once.
    backlog_ready: bool,
}

impl<'a> Sink<'a> {
    /// Sink for one connection.
    pub fn new(tx: &'a SyncSender<Line>, seq: &'a mut SeqTracker, backlog: Backlog) -> Self {
        Self { tx, seq, backlog, envelopes: 0, backlog_ready: false }
    }

    /// Hand one line to the writer. Err if the writer is gone.
    ///
    /// The blocking `send` runs in the async task on purpose: the channel
    /// holds 200 000 lines (hours of feed); if it is full the writer is stuck
    /// on the disk anyway (see `app.rs`).
    fn push(&mut self, line: Line) -> Result<(), ()> {
        if line.has_seq() {
            // The writer's rule (shared SeqTracker): a frame whose seqs are
            // all <= last_seq is skipped there (dup_skipped).
            let stale = self.seq.observe(line.seq_first, line.seq_max, &line.intra_gaps) == Seen::Stale;
            if self.backlog.observe(line.recv_ns, line.seq_first, line.seq_max, line.kind3_ts, stale) {
                self.backlog_ready = true;
            }
            self.envelopes += 1;
        }
        self.tx.send(line).map_err(|_| ())
    }
}

/// Report the end of the first burst once (also if it ended during the
/// close handshake).
fn flush_backlog(sink: &mut Sink<'_>, on_backlog: &mut impl FnMut(&Backlog)) {
    if sink.backlog_ready {
        sink.backlog_ready = false;
        on_backlog(&sink.backlog);
    }
}

/// Close 1000 with `reason`; frames that still arrive go to the writer.
async fn close_gracefully(ws: &mut FeedWs, sink: &mut Sink<'_>, reason: &str) -> CloseOutcome {
    close_handshake(ws, reason, |frame| {
        // If the writer is gone there is nowhere to put it.
        let _ = sink.push(route_frame(frame, now_ns()).0);
    })
    .await
}

/// Fixed parameters of every connection.
#[derive(Clone, Copy)]
pub struct ConnParams<'a> {
    pub url: &'a str,
    pub tls: &'a TlsConnector,
    /// No frame for this long: idle timeout.
    pub idle: Duration,
    /// No frame with seq > 0 for this long: block idle (zero disables).
    pub block_idle: Duration,
}

/// Why the recorder ends a connection itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientEnd {
    /// Shutdown (signal or fatal writer error); the close reason.
    Stop(&'static str),
    /// No block for `block_idle` (task 012 item 3).
    BlockIdle,
    /// No frame for `idle` (task 009 item 6).
    Idle,
    /// The writer channel is closed.
    WriterGone,
}

impl ClientEnd {
    /// Close reason, end kind and detail of the `disconnected` row. The
    /// kinds are those of tasks 009/012 (a stop logs no `disconnected`).
    fn parts(self, idle: Duration, block_idle: Duration) -> (&'static str, EndKind, String) {
        match self {
            ClientEnd::Stop(reason) => (reason, EndKind::ServerClosed, "client shutdown".into()),
            ClientEnd::BlockIdle => {
                ("block idle timeout", EndKind::BlockIdle, format!("no block (seq > 0) for {block_idle:?}"))
            }
            ClientEnd::Idle => ("idle timeout", EndKind::Idle, format!("no frame for {idle:?}")),
            ClientEnd::WriterGone => ("recorder writer gone", EndKind::NetError, "writer gone".into()),
        }
    }
}

/// Outcome of one wait in the frame loop.
enum Next<F> {
    Stop,
    BlockIdle,
    Frame(F),
}

/// One connection: connect (with the resume header if `requested` is set),
/// stream frames into the sink until it ends or `stop` is set.
/// `on_connected` is called right after a successful upgrade, `on_backlog`
/// once when the first burst ends (see `resume.rs`). Whenever we end the
/// connection ourselves after the upgrade (stop, idle timeout, block idle
/// timeout, writer gone) the close handshake is done and reported in
/// [`ConnEnd::client_close`]. `block_idle` (task 012 item 3): no frame with
/// seq > 0 for that long (pings and confirmations do not count; zero
/// disables) ends the session like an idle timeout, so the reconnect asks
/// for `last_seq + 1`.
pub async fn run_connection(
    p: &ConnParams<'_>,
    requested: Option<u64>,
    sink: &mut Sink<'_>,
    stop: &mut Stop,
    on_connected: impl FnOnce(),
    mut on_backlog: impl FnMut(&Backlog),
) -> ConnEnd {
    let connected = tokio::select! {
        biased;
        _ = stopped(stop) => {
            return ConnectError {
                kind: EndKind::NetError, http_status: None, retry_after_raw: None, retry_after: None,
                detail: "stopped before upgrade".into(),
            }.into();
        }
        r = connect(p.url, p.tls, requested) => r,
    };
    let mut ws = match connected {
        Ok(ws) => ws,
        Err(e) => return e.into(),
    };
    info!(url = %redact_url(p.url), requested = ?requested, "connected");
    on_connected();
    let started = Instant::now();
    // Last frame with a block; the session start counts as one.
    let mut last_block = tokio::time::Instant::now();
    let mut last_log = Instant::now();
    let mut close_info: Option<String> = None;

    let end = |kind: EndKind, detail: String, sink: &Sink<'_>| ConnEnd {
        kind,
        http_status: Some(101),
        retry_after_raw: None,
        retry_after: None,
        session: started.elapsed(),
        envelopes: sink.envelopes,
        detail,
        client_close: None,
        backlog: Some(Box::new(sink.backlog.clone())),
    };

    let why = loop {
        let block_deadline = (!p.block_idle.is_zero()).then(|| last_block + p.block_idle);
        let next = tokio::select! {
            biased;
            _ = stopped(stop) => Next::Stop,
            _ = async {
                match block_deadline {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => std::future::pending().await,
                }
            } => Next::BlockIdle,
            r = tokio::time::timeout(p.idle, ws.next_frame()) => Next::Frame(r),
        };
        let frame = match next {
            Next::Stop => break ClientEnd::Stop(stop_reason(stop).unwrap_or(SHUTDOWN_REASON)),
            Next::BlockIdle => {
                // Task 012 item 3: frames keep coming (pings), blocks do not.
                warn!(block_idle = ?p.block_idle, "no block within block idle timeout, closing");
                break ClientEnd::BlockIdle;
            }
            Next::Frame(Err(_)) => {
                // Task 009 item 6: close politely instead of dropping TCP.
                warn!(idle = ?p.idle, "no frame within idle timeout, closing");
                break ClientEnd::Idle;
            }
            Next::Frame(Ok(Err(e))) => {
                let d = match &close_info {
                    Some(c) => format!("{c}; then {e}"),
                    None => e.to_string(),
                };
                return end(EndKind::ServerClosed, d, sink);
            }
            Next::Frame(Ok(Ok(f))) => f,
        };
        let (line, close) = route_frame(&frame, now_ns());
        if let Some(c) = close {
            warn!(detail = %c, "server sent close frame");
            close_info = Some(c);
        }
        let seq_last = line.seq_last;
        if line.has_seq() {
            last_block = tokio::time::Instant::now();
        }
        if sink.push(line).is_err() {
            break ClientEnd::WriterGone;
        }
        flush_backlog(sink, &mut on_backlog);
        if last_log.elapsed() >= Duration::from_secs(60) {
            info!(envelopes = sink.envelopes, last_seq = seq_last, "alive");
            last_log = Instant::now();
        }
    };

    // The recorder ends the connection: one path for all four reasons.
    let (close_reason, kind, detail) = why.parts(p.idle, p.block_idle);
    let outcome = close_gracefully(&mut ws, sink, close_reason).await;
    flush_backlog(sink, &mut on_backlog);
    info!(
        reason = outcome.reply.as_str(), took_ms = outcome.took.as_millis() as u64,
        detail = %outcome.detail, "client close handshake done"
    );
    let mut e = end(kind, detail, sink);
    e.client_close = Some(Box::new(outcome));
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_end_parts_keep_the_logged_strings() {
        let (idle, block_idle) = (Duration::from_secs(10), Duration::from_secs(30));
        let parts = |w: ClientEnd| w.parts(idle, block_idle);
        let stop = ("recorder shutdown", EndKind::ServerClosed, "client shutdown".to_string());
        assert_eq!(parts(ClientEnd::Stop("recorder shutdown")), stop);
        let block = ("block idle timeout", EndKind::BlockIdle, "no block (seq > 0) for 30s".to_string());
        assert_eq!(parts(ClientEnd::BlockIdle), block);
        assert_eq!(parts(ClientEnd::Idle), ("idle timeout", EndKind::Idle, "no frame for 10s".to_string()));
        let gone = ("recorder writer gone", EndKind::NetError, "writer gone".to_string());
        assert_eq!(parts(ClientEnd::WriterGone), gone);
    }

    #[test]
    fn sink_marks_stale_frames_by_the_shared_rule() {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Line>(8);
        let mut seq = SeqTracker::new(Some(100));
        let mut sink = Sink::new(&tx, &mut seq, Backlog::new(Some(101), Some(100)));
        let env = |s: u64| route_text(1, format!(r#"{{"version":1,"messages":[{{"sequenceNumber":{s}}}]}}"#));
        sink.push(env(99)).unwrap();
        sink.push(env(101)).unwrap();
        sink.push(env(101)).unwrap();
        sink.push(route_opaque(1, "ping", b"")).unwrap();
        assert_eq!(sink.backlog.stale_frames, 2);
        assert_eq!(sink.envelopes, 3);
        assert_eq!(seq.last(), Some(101));
        drop(tx);
        assert_eq!(rx.iter().count(), 4); // stale frames still go to the writer
    }
}
