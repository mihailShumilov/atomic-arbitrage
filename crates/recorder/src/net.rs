//! Feed connection: TCP + TLS + WebSocket upgrade, frame loop, and the
//! connection log `<out>/connections.tsv` (task 002, items 4 and 5).
//!
//! yawc reports a failed upgrade only as `InvalidStatusCode(u16)` and drops
//! the response headers, so `Retry-After` would be lost. We therefore do the
//! TCP/TLS part ourselves (same steps as `WebSocket::connect`) and wrap the
//! stream in [`HeadTap`], which keeps a copy of the HTTP response head. This
//! costs no extra request: one connection, one upgrade attempt.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use futures::SinkExt;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::{rustls, TlsConnector};
use tracing::{info, warn};
use yawc::close::CloseCode;
use yawc::{
    frame::OpCode, CompressionLevel, Frame, HttpRequest, MaybeTlsStream, Options, WebSocket,
    WebSocketError,
};

use crate::backoff::{parse_retry_after, EndKind};
use crate::resume::Backlog;
use crate::route::{route_opaque, route_text, Line};

pub const CONNECTIONS_FILE: &str = "connections.tsv";
const HEAD_CAP: usize = 16 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// Client-initiated close (shutdown, idle timeout, writer gone): how long to
/// wait for the server's Close after sending ours.
pub const CLOSE_REPLY_WAIT: Duration = Duration::from_secs(2);
/// Client-initiated close: bound on the final TLS close_notify / TCP FIN.
const CLOSE_SHUTDOWN_WAIT: Duration = Duration::from_millis(500);

pub fn now_ns() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

/// TLS with the OS certificate store (works on normal servers and behind
/// TLS-intercepting proxies) and HTTP/1.1 ALPN.
pub fn tls_connector() -> Result<TlsConnector> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(cert);
    }
    anyhow::ensure!(
        !roots.is_empty(),
        "no OS root certificates found (install ca-certificates)"
    );
    let mut cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth();
    // WebSocket upgrade over HTTP/2 through Cloudflare returned 520 in tests.
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsConnector::from(Arc::new(cfg)))
}

// ---------------------------------------------------------------- HeadTap ---

/// Transparent stream wrapper that copies the first bytes read (up to the
/// end of the HTTP response head) into a shared buffer.
pub struct HeadTap<S> {
    inner: S,
    head: Arc<Mutex<Vec<u8>>>,
    capturing: bool,
}

impl<S> HeadTap<S> {
    pub fn new(inner: S) -> (Self, Arc<Mutex<Vec<u8>>>) {
        let head = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                inner,
                head: head.clone(),
                capturing: true,
            },
            head,
        )
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for HeadTap<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let r = Pin::new(&mut self.inner).poll_read(cx, buf);
        if self.capturing {
            if let Poll::Ready(Ok(())) = &r {
                let new = &buf.filled()[before..];
                let mut h = self.head.lock().unwrap_or_else(|e| e.into_inner());
                let room = HEAD_CAP.saturating_sub(h.len());
                h.extend_from_slice(&new[..new.len().min(room)]);
                let done = h.len() >= HEAD_CAP || h.windows(4).any(|w| w == b"\r\n\r\n");
                drop(h);
                if done || new.is_empty() {
                    self.capturing = false;
                }
            }
        }
        r
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for HeadTap<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        b: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, b)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }
    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

/// Parsed HTTP response head: status code, `Retry-After`, status line.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HttpHead {
    pub status: Option<u16>,
    pub retry_after: Option<String>,
    pub status_line: String,
}

pub fn parse_http_head(bytes: &[u8]) -> HttpHead {
    let end = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or(bytes.len());
    let text = String::from_utf8_lossy(&bytes[..end]);
    let mut lines = text.split("\r\n");
    let status_line = lines.next().unwrap_or("").trim().to_string();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok());
    let retry_after = lines.find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case("retry-after")
            .then(|| v.trim().to_string())
    });
    HttpHead {
        status,
        retry_after,
        status_line,
    }
}

// ------------------------------------------------------------- connection ---

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
    /// idle timeout, writer gone): outcome of our Close handshake.
    pub client_close: Option<Box<CloseOutcome>>,
    /// Statistics of the first burst; None if the upgrade failed.
    pub backlog: Option<Box<Backlog>>,
}

/// Result of a client-initiated close handshake.
#[derive(Debug, Clone)]
pub struct CloseOutcome {
    /// `server_replied`, `no_reply`, `stream_ended` or `send_failed`.
    pub reason: &'static str,
    /// From sending our Close to the end of the wait.
    pub took: Duration,
    pub detail: String,
}

impl ConnEnd {
    fn failed(kind: EndKind, detail: String) -> Self {
        ConnEnd {
            kind,
            http_status: None,
            retry_after_raw: None,
            retry_after: None,
            session: Duration::ZERO,
            envelopes: 0,
            detail,
            client_close: None,
            backlog: None,
        }
    }
}

fn classify_upgrade_error(e: &WebSocketError, head: &HttpHead) -> EndKind {
    let status = match e {
        WebSocketError::InvalidStatusCode(c) => Some(*c),
        _ => head.status.filter(|&c| c != 101),
    };
    match (status, e) {
        (Some(429), _) => EndKind::RateLimited,
        (Some(c), _) if (400..500).contains(&c) => EndKind::Forbidden,
        (Some(c), _) if c >= 500 => EndKind::HttpError,
        (_, WebSocketError::InvalidUpgradeHeader)
        | (_, WebSocketError::InvalidConnectionHeader)
        | (_, WebSocketError::Redirected { .. }) => EndKind::Forbidden,
        _ => EndKind::NetError,
    }
}

type FeedWs = WebSocket<HeadTap<MaybeTlsStream<TcpStream>>>;

/// Handshake request: with `requested`, the two headers the Nitro feed client
/// sends (task 005: `broadcastclient.go` v3.11.4 L231-L234); without it, no
/// extra headers (the behaviour of tasks 001-008).
fn handshake_request(requested: Option<u64>) -> yawc::HttpRequestBuilder {
    let b = HttpRequest::builder();
    match requested {
        Some(n) => b
            .header("Arbitrum-Feed-Client-Version", "2")
            .header("Arbitrum-Requested-Sequence-Number", n.to_string()),
        None => b,
    }
}

/// Connect and upgrade. On failure returns a classified [`ConnEnd`].
async fn connect(
    url_str: &str,
    tls: &TlsConnector,
    requested: Option<u64>,
) -> std::result::Result<FeedWs, ConnEnd> {
    let url: url::Url = url_str
        .parse()
        .map_err(|e| ConnEnd::failed(EndKind::NetError, format!("bad url: {e}")))?;
    let host = url.host_str().unwrap_or_default().to_string();
    let port = url.port_or_known_default().unwrap_or(443);
    let res = tokio::time::timeout(CONNECT_TIMEOUT, async {
        let tcp = TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|e| ConnEnd::failed(EndKind::NetError, format!("tcp: {e}")))?;
        let _ = tcp.set_nodelay(true);
        // ws:// is only for local mock-feed tests; the real feed is wss://.
        let stream = if url.scheme() == "ws" {
            MaybeTlsStream::Plain(tcp)
        } else {
            let name = ServerName::try_from(host.clone())
                .map_err(|e| ConnEnd::failed(EndKind::NetError, format!("server name: {e}")))?;
            let tls_stream = tls
                .connect(name, tcp)
                .await
                .map_err(|e| ConnEnd::failed(EndKind::NetError, format!("tls: {e}")))?;
            MaybeTlsStream::Tls(tls_stream)
        };
        let (tap, head) = HeadTap::new(stream);
        Ok::<_, ConnEnd>((tap, head))
    })
    .await;
    let (tap, head_buf) = match res {
        Err(_) => return Err(ConnEnd::failed(EndKind::NetError, "tcp/tls timeout".into())),
        Ok(r) => r?,
    };
    // Server negotiates no_context_takeover both ways; deflate is mandatory.
    let opts = Options::default().with_compression_level(CompressionLevel::fast());
    let hs = tokio::time::timeout(
        CONNECT_TIMEOUT,
        WebSocket::handshake_with_request(url, tap, opts, handshake_request(requested)),
    )
    .await;
    let captured = head_buf.lock().map(|h| h.clone()).unwrap_or_default();
    let head = parse_http_head(&captured);
    match hs {
        Ok(Ok(ws)) => Ok(ws),
        Ok(Err(e)) => {
            let kind = classify_upgrade_error(&e, &head);
            let status = match &e {
                WebSocketError::InvalidStatusCode(c) => Some(*c),
                _ => head.status,
            };
            let now_unix = (now_ns() / 1_000_000_000) as i64;
            Err(ConnEnd {
                kind,
                http_status: status,
                retry_after: head
                    .retry_after
                    .as_deref()
                    .and_then(|v| parse_retry_after(v, now_unix)),
                retry_after_raw: head.retry_after.clone(),
                session: Duration::ZERO,
                envelopes: 0,
                detail: format!("upgrade: {e}; {}", head.status_line),
                client_close: None,
                backlog: None,
            })
        }
        Err(_) => Err(ConnEnd {
            http_status: head.status,
            ..ConnEnd::failed(EndKind::NetError, "upgrade timeout".into())
        }),
    }
}

fn opcode_name(op: OpCode) -> &'static str {
    match op {
        OpCode::Text => "text",
        OpCode::Binary => "binary",
        OpCode::Ping => "ping",
        OpCode::Pong => "pong",
        OpCode::Close => "close",
        OpCode::Continuation => "continuation",
    }
}

/// Frame -> raw line. Also logs a server close frame; returns its summary.
fn route_frame(frame: &Frame, recv_ns: u128) -> (Line, Option<String>) {
    let op = frame.opcode();
    match op {
        OpCode::Text => match std::str::from_utf8(frame.payload()) {
            Ok(s) => (route_text(recv_ns, s.to_owned()), None),
            Err(_) => (
                route_opaque(recv_ns, "text_invalid_utf8", frame.payload()),
                None,
            ),
        },
        other => {
            let mut close = None;
            if other == OpCode::Close {
                let code = frame.close_code().map(u16::from);
                let reason = frame
                    .close_reason()
                    .ok()
                    .flatten()
                    .unwrap_or("")
                    .to_string();
                close = Some(format!("close frame code={code:?} reason={reason:?}"));
            }
            (
                route_opaque(recv_ns, opcode_name(other), frame.payload()),
                close,
            )
        }
    }
}

/// Resolves once `stop` is true (or its sender is gone).
pub async fn stopped(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|s| *s).await;
}

/// Where received frames go: the writer channel, plus the bookkeeping the
/// network side needs for the next handshake (task 009): the highest seq
/// handed to the writer (`last_seq`, survives reconnects inside the process)
/// and the burst statistics of the current connection.
pub struct Sink<'a> {
    tx: &'a SyncSender<Line>,
    last_seq: &'a mut Option<u64>,
    backlog: Backlog,
    envelopes: u64,
    /// Set when the burst ended; the caller logs it once.
    backlog_ready: bool,
}

impl<'a> Sink<'a> {
    pub fn new(tx: &'a SyncSender<Line>, last_seq: &'a mut Option<u64>, backlog: Backlog) -> Self {
        Self {
            tx,
            last_seq,
            backlog,
            envelopes: 0,
            backlog_ready: false,
        }
    }

    /// Hand one line to the writer. Err if the writer is gone.
    fn push(&mut self, line: Line) -> std::result::Result<(), ()> {
        if line.has_seq() {
            // Same rule as FeedWriter::accept: a frame whose seqs are all
            // <= last_seq is skipped there (dup_skipped).
            let stale = self.last_seq.is_some_and(|l| line.seq_max <= l);
            if self.backlog.observe(
                line.recv_ns,
                line.seq_first,
                line.seq_max,
                line.kind3_ts,
                stale,
            ) {
                self.backlog_ready = true;
            }
            self.envelopes += 1;
            *self.last_seq = Some(self.last_seq.map_or(line.seq_max, |s| s.max(line.seq_max)));
        }
        self.tx.send(line).map_err(|_| ())
    }
}

/// Client-initiated close (task 008 item 3 for shutdown; task 009 item 6 for
/// idle timeout and a lost writer): send Close 1000 with `reason`, keep
/// recording whatever still arrives until the server's Close or
/// [`CLOSE_REPLY_WAIT`], then shut the stream down (TLS close_notify, FIN).
/// Before 008 the socket was dropped on SIGTERM, before 009 on idle timeout,
/// both without a Close.
async fn close_gracefully(ws: &mut FeedWs, sink: &mut Sink<'_>, reason: &str) -> CloseOutcome {
    let t0 = Instant::now();
    let deadline = tokio::time::Instant::now() + CLOSE_REPLY_WAIT;
    let sent = tokio::time::timeout_at(
        deadline,
        ws.send(Frame::close(CloseCode::Normal, reason.as_bytes())),
    )
    .await;
    let mut out = match sent {
        Err(_) => CloseOutcome {
            reason: "send_failed",
            took: t0.elapsed(),
            detail: "timeout sending close frame".into(),
        },
        Ok(Err(e)) => CloseOutcome {
            reason: "send_failed",
            took: t0.elapsed(),
            detail: format!("sending close frame: {e}"),
        },
        Ok(Ok(())) => loop {
            match tokio::time::timeout_at(deadline, ws.next_frame()).await {
                Err(_) => {
                    break CloseOutcome {
                        reason: "no_reply",
                        took: t0.elapsed(),
                        detail: format!("no close frame from server within {CLOSE_REPLY_WAIT:?}"),
                    }
                }
                Ok(Err(e)) => {
                    break CloseOutcome {
                        reason: "stream_ended",
                        took: t0.elapsed(),
                        detail: format!("stream ended before the server's close: {e}"),
                    }
                }
                Ok(Ok(frame)) => {
                    let (line, close) = route_frame(&frame, now_ns());
                    // Nothing is dropped, also during the close handshake.
                    // If the writer is gone there is nowhere to put it.
                    let _ = sink.push(line);
                    if let Some(c) = close {
                        break CloseOutcome {
                            reason: "server_replied",
                            took: t0.elapsed(),
                            detail: c,
                        };
                    }
                }
            }
        },
    };
    out.detail = format!("close 1000 {reason:?}: {}", out.detail);
    match tokio::time::timeout(CLOSE_SHUTDOWN_WAIT, ws.close()).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => out.detail = format!("{}; stream shutdown: {e}", out.detail),
        Err(_) => out.detail = format!("{}; stream shutdown timed out", out.detail),
    }
    out
}

/// Report the end of the first burst once (also if it ended during the
/// close handshake).
fn flush_backlog(sink: &mut Sink<'_>, on_backlog: &mut impl FnMut(&Backlog)) {
    if sink.backlog_ready {
        sink.backlog_ready = false;
        on_backlog(&sink.backlog);
    }
}

/// One connection: connect (with the resume header if `requested` is set),
/// stream frames into the sink until it ends or `stop` becomes true.
/// `on_connected` is called right after a successful upgrade, `on_backlog`
/// once when the first burst ends (see `resume.rs`). Whenever we end the
/// connection ourselves after the upgrade (stop, idle timeout, writer gone)
/// the close handshake is done and reported in [`ConnEnd::client_close`].
#[allow(clippy::too_many_arguments)]
pub async fn run_connection(
    url: &str,
    tls: &TlsConnector,
    idle: Duration,
    requested: Option<u64>,
    sink: &mut Sink<'_>,
    stop: &mut watch::Receiver<bool>,
    mut on_connected: impl FnMut(),
    mut on_backlog: impl FnMut(&Backlog),
) -> ConnEnd {
    let connected = tokio::select! {
        biased;
        _ = stopped(stop) => {
            return ConnEnd::failed(EndKind::NetError, "stopped before upgrade".into());
        }
        r = connect(url, tls, requested) => r,
    };
    let mut ws = match connected {
        Ok(ws) => ws,
        Err(end) => return end,
    };
    info!(url, requested = ?requested, "connected");
    on_connected();
    let started = Instant::now();
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

    loop {
        let next = tokio::select! {
            biased;
            _ = stopped(stop) => None,
            r = tokio::time::timeout(idle, ws.next_frame()) => Some(r),
        };
        let Some(next) = next else {
            let outcome = close_gracefully(&mut ws, sink, "recorder shutdown").await;
            flush_backlog(sink, &mut on_backlog);
            info!(
                reason = outcome.reason, took_ms = outcome.took.as_millis() as u64,
                detail = %outcome.detail, "client close handshake done"
            );
            let mut e = end(EndKind::ServerClosed, "client shutdown".into(), sink);
            e.client_close = Some(Box::new(outcome));
            return e;
        };
        let frame = match next {
            Err(_) => {
                // Task 009 item 6: close politely instead of dropping TCP.
                warn!(idle = ?idle, "no frame within idle timeout, closing");
                let outcome = close_gracefully(&mut ws, sink, "idle timeout").await;
                flush_backlog(sink, &mut on_backlog);
                info!(
                    reason = outcome.reason, took_ms = outcome.took.as_millis() as u64,
                    detail = %outcome.detail, "client close handshake done"
                );
                let mut e = end(EndKind::Idle, format!("no frame for {idle:?}"), sink);
                e.client_close = Some(Box::new(outcome));
                return e;
            }
            Ok(Err(e)) => {
                let d = match &close_info {
                    Some(c) => format!("{c}; then {e}"),
                    None => e.to_string(),
                };
                return end(EndKind::ServerClosed, d, sink);
            }
            Ok(Ok(f)) => f,
        };
        let (line, close) = route_frame(&frame, now_ns());
        if let Some(c) = close {
            warn!(detail = %c, "server sent close frame");
            close_info = Some(c);
        }
        let seq_last = line.seq_last;
        if sink.push(line).is_err() {
            let outcome = close_gracefully(&mut ws, sink, "recorder writer gone").await;
            flush_backlog(sink, &mut on_backlog);
            let mut e = end(EndKind::NetError, "writer gone".into(), sink);
            e.client_close = Some(Box::new(outcome));
            return e;
        }
        flush_backlog(sink, &mut on_backlog);
        if last_log.elapsed() >= Duration::from_secs(60) {
            info!(envelopes = sink.envelopes, last_seq = seq_last, "alive");
            last_log = Instant::now();
        }
    }
}

// -------------------------------------------------------- connections.tsv ---

pub const CONNECTIONS_HEADER: &str = "# ts_utc\tts_unix_ns\tevent\treason\thttp_status\tretry_after\tpause_s\tsession_s\tenvelopes\tstrikes\tdetail";

/// One row of connections.tsv. `None` fields are written as `-`.
#[derive(Debug, Default)]
pub struct ConnEvent<'a> {
    pub event: &'a str,
    pub reason: &'a str,
    pub http_status: Option<u16>,
    pub retry_after: Option<&'a str>,
    pub pause: Option<Duration>,
    pub session: Option<Duration>,
    pub envelopes: Option<u64>,
    pub strikes: Option<u32>,
    pub detail: &'a str,
}

/// Pause decided before the previous process exited (from connections.tsv).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PendingPause {
    /// Unix ns before which we must not reconnect.
    pub not_before_ns: u128,
    pub strikes: u32,
}

fn dash<T: ToString>(v: Option<T>) -> String {
    v.map(|x| x.to_string()).unwrap_or_else(|| "-".into())
}

/// Append-only event log of connects/disconnects and chosen pauses. It also
/// carries the pause across restarts, so a restarted process (systemd
/// `Restart=always`) does not reconnect in the middle of a ban.
pub struct ConnLog {
    path: PathBuf,
}

impl ConnLog {
    pub fn new(out: &Path) -> Self {
        Self {
            path: out.join(CONNECTIONS_FILE),
        }
    }

    pub fn event(&self, e: ConnEvent<'_>) {
        let ns = now_ns();
        let ts = chrono::DateTime::from_timestamp_nanos(ns as i64).format("%Y-%m-%dT%H:%M:%S%.3fZ");
        let clean = |s: &str| {
            let c = s.replace(['\t', '\n', '\r'], " ");
            if c.is_empty() {
                "-".to_string()
            } else {
                c
            }
        };
        let row = format!(
            "{ts}\t{ns}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            clean(e.event),
            clean(e.reason),
            dash(e.http_status),
            e.retry_after.map(clean).unwrap_or_else(|| "-".into()),
            dash(e.pause.map(|p| format!("{:.3}", p.as_secs_f64()))),
            dash(e.session.map(|p| format!("{:.3}", p.as_secs_f64()))),
            dash(e.envelopes),
            dash(e.strikes),
            clean(e.detail),
        );
        let res = (|| -> std::io::Result<()> {
            let new = !self.path.exists();
            let mut f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?;
            if new {
                writeln!(f, "{CONNECTIONS_HEADER}")?;
            }
            writeln!(f, "{row}")
        })();
        if let Err(e) = res {
            warn!(error = %e, "cannot append to connections.tsv");
        }
    }

    /// Last 64 KiB of the log (the log grows by a few rows per connect).
    fn tail(&self) -> Option<String> {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(&self.path).ok()?;
        let len = f.metadata().ok()?.len();
        let from = len.saturating_sub(64 * 1024);
        f.seek(SeekFrom::Start(from)).ok()?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).ok()?;
        Some(String::from_utf8_lossy(&buf).into_owned())
    }

    /// The pause chosen at the last `disconnected` event, if the log has one.
    pub fn last_pause(&self) -> Option<PendingPause> {
        self.tail()?.lines().rev().find_map(parse_pause_row)
    }

    /// Unix ns of the last `connected` event, if the log has one.
    pub fn last_connected_ns(&self) -> Option<u128> {
        self.tail()?.lines().rev().find_map(parse_connected_row)
    }
}

/// Timestamp of a `connected` row. Works for both column layouts seen so far
/// (with and without the `strikes` column): ts_unix_ns and event come first.
pub fn parse_connected_row(line: &str) -> Option<u128> {
    let mut c = line.split('\t');
    let _ts = c.next()?;
    let ns = c.next()?;
    (c.next()? == "connected").then(|| ns.parse().ok())?
}

/// Parse a `disconnected` row into the pending pause it defines.
pub fn parse_pause_row(line: &str) -> Option<PendingPause> {
    let c: Vec<&str> = line.split('\t').collect();
    if c.len() < 10 || c[2] != "disconnected" {
        return None;
    }
    let ts: u128 = c[1].parse().ok()?;
    let pause: f64 = c[6].parse().ok()?;
    let strikes: u32 = c[9].parse().unwrap_or(0);
    Some(PendingPause {
        not_before_ns: ts + (pause * 1e9) as u128,
        strikes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_429_head_with_retry_after() {
        let raw = b"HTTP/1.1 429 Too Many Requests\r\nDate: Wed, 30 Sep 2026 12:00:00 GMT\r\nContent-Type: text/plain\r\nretry-after: 120\r\nServer: cloudflare\r\n\r\nerror code: 1015";
        let h = parse_http_head(raw);
        assert_eq!(h.status, Some(429));
        assert_eq!(h.retry_after.as_deref(), Some("120"));
        assert_eq!(h.status_line, "HTTP/1.1 429 Too Many Requests");
    }

    #[test]
    fn parses_head_without_retry_after_and_partial() {
        let h = parse_http_head(b"HTTP/1.1 403 Forbidden\r\nServer: cloudflare\r\n\r\n");
        assert_eq!((h.status, h.retry_after), (Some(403), None));
        let h = parse_http_head(b"HTTP/1.1 101 Switching Protocols\r\nUpgr");
        assert_eq!(h.status, Some(101));
        assert_eq!(parse_http_head(b"").status, None);
    }

    #[test]
    fn classifies_upgrade_failures() {
        let none = HttpHead::default();
        let c = |code| classify_upgrade_error(&WebSocketError::InvalidStatusCode(code), &none);
        assert_eq!(c(429), EndKind::RateLimited);
        assert_eq!(c(403), EndKind::Forbidden);
        assert_eq!(c(404), EndKind::Forbidden);
        assert_eq!(c(520), EndKind::HttpError);
        assert_eq!(
            classify_upgrade_error(&WebSocketError::InvalidUpgradeHeader, &none),
            EndKind::Forbidden
        );
        assert_eq!(
            classify_upgrade_error(&WebSocketError::ConnectionClosed, &none),
            EndKind::NetError
        );
    }

    #[test]
    fn pause_survives_restart_via_connections_log() {
        let dir = std::env::temp_dir().join(format!(
            "recorder-connlog-{}-{}",
            std::process::id(),
            now_ns()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let log = ConnLog::new(&dir);
        assert_eq!(log.last_pause(), None);
        log.event(ConnEvent {
            event: "connected",
            reason: "-",
            http_status: Some(101),
            ..Default::default()
        });
        assert_eq!(log.last_pause(), None);
        let before = now_ns();
        log.event(ConnEvent {
            event: "disconnected",
            reason: "forbidden",
            http_status: Some(403),
            retry_after: Some("3600"),
            pause: Some(Duration::from_secs(3600)),
            session: Some(Duration::ZERO),
            envelopes: Some(0),
            strikes: Some(1),
            detail: "upgrade: Invalid status code: 403;\tHTTP/1.1 403 Forbidden",
        });
        log.event(ConnEvent {
            event: "shutdown",
            reason: "SIGTERM",
            ..Default::default()
        });
        let p = log.last_pause().unwrap();
        assert_eq!(p.strikes, 1);
        let wait = p.not_before_ns - before;
        assert!(
            (3_599_000_000_000..=3_601_000_000_000).contains(&wait),
            "{wait}"
        );
        let text = std::fs::read_to_string(dir.join(CONNECTIONS_FILE)).unwrap();
        assert!(text.starts_with(CONNECTIONS_HEADER));
        assert!(text.lines().all(|l| l.split('\t').count() == 11), "{text}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn last_connected_from_both_log_layouts() {
        // Rows copied from data/feed-test-002/connections.tsv (task 002): the
        // first run wrote 10 columns, later runs 11 (with `strikes`).
        let old = "2026-09-30T12:52:21.202Z\t1790772741202619000\tconnected\t-\t101\t-\t-\t-\t-\twss://feed.mainnet.chain.robinhood.com";
        let new = "2026-09-30T13:57:32.226Z\t1790776652226292000\tconnected\t-\t101\t-\t-\t-\t-\t0\twss://feed.mainnet.chain.robinhood.com";
        assert_eq!(parse_connected_row(old), Some(1790772741202619000));
        assert_eq!(parse_connected_row(new), Some(1790776652226292000));
        assert_eq!(parse_connected_row(CONNECTIONS_HEADER), None);
        assert_eq!(
            parse_connected_row("2026-09-30T14:03:00.329Z\t1790776980329123000\tshutdown\tSIGTERM"),
            None
        );
        let dir = std::env::temp_dir().join(format!(
            "recorder-connlog2-{}-{}",
            std::process::id(),
            now_ns()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let log = ConnLog::new(&dir);
        assert_eq!(log.last_connected_ns(), None);
        std::fs::write(
            dir.join(CONNECTIONS_FILE),
            format!("{CONNECTIONS_HEADER}\n{old}\n{new}\n2026-09-30T14:03:00.329Z\t1790776980329123000\tshutdown\tSIGTERM\t-\t-\t-\t-\t-\t-\t-\n"),
        )
        .unwrap();
        assert_eq!(log.last_connected_ns(), Some(1790776652226292000));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn head_tap_captures_response_head() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut server, client) = tokio::io::duplex(64);
        let (mut tap, head) = HeadTap::new(client);
        tokio::spawn(async move {
            server
                .write_all(b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 7\r\n\r\nbody")
                .await
                .unwrap();
        });
        let mut out = Vec::new();
        tap.read_to_end(&mut out).await.unwrap();
        let h = parse_http_head(&head.lock().unwrap());
        assert_eq!((h.status, h.retry_after.as_deref()), (Some(429), Some("7")));
        assert!(out.ends_with(b"body"));
    }
}
