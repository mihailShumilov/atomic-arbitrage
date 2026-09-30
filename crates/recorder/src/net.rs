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
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::{rustls, TlsConnector};
use tracing::{info, warn};
use yawc::{frame::OpCode, CompressionLevel, MaybeTlsStream, Options, WebSocket, WebSocketError};

use crate::backoff::{parse_retry_after, EndKind};
use crate::route::{route_opaque, route_text, Line};

pub const CONNECTIONS_FILE: &str = "connections.tsv";
const HEAD_CAP: usize = 16 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

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

/// Connect and upgrade. On failure returns a classified [`ConnEnd`].
async fn connect(url_str: &str, tls: &TlsConnector) -> std::result::Result<FeedWs, ConnEnd> {
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
    let hs = tokio::time::timeout(CONNECT_TIMEOUT, WebSocket::handshake(url, tap, opts)).await;
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

/// One connection: connect, stream frames into `tx` until it ends.
/// `on_connected` is called right after a successful upgrade.
pub async fn run_connection(
    url: &str,
    tls: &TlsConnector,
    idle: Duration,
    tx: &SyncSender<Line>,
    mut on_connected: impl FnMut(),
) -> ConnEnd {
    let mut ws = match connect(url, tls).await {
        Ok(ws) => ws,
        Err(end) => return end,
    };
    info!(url, "connected");
    on_connected();
    let started = Instant::now();
    let mut envelopes: u64 = 0;
    let mut last_log = Instant::now();
    let mut close_info: Option<String> = None;

    let end = |kind: EndKind, detail: String, envelopes: u64| ConnEnd {
        kind,
        http_status: Some(101),
        retry_after_raw: None,
        retry_after: None,
        session: started.elapsed(),
        envelopes,
        detail,
    };

    loop {
        let frame = match tokio::time::timeout(idle, ws.next_frame()).await {
            Err(_) => return end(EndKind::Idle, format!("no frame for {idle:?}"), envelopes),
            Ok(Err(e)) => {
                let d = match &close_info {
                    Some(c) => format!("{c}; then {e}"),
                    None => e.to_string(),
                };
                return end(EndKind::ServerClosed, d, envelopes);
            }
            Ok(Ok(f)) => f,
        };
        let recv_ns = now_ns();
        let op = frame.opcode();
        let line = match op {
            OpCode::Text => match std::str::from_utf8(frame.payload()) {
                Ok(s) => route_text(recv_ns, s.to_owned()),
                Err(_) => route_opaque(recv_ns, "text_invalid_utf8", frame.payload()),
            },
            other => {
                if other == OpCode::Close {
                    let code = frame.close_code().map(u16::from);
                    let reason = frame
                        .close_reason()
                        .ok()
                        .flatten()
                        .unwrap_or("")
                        .to_string();
                    close_info = Some(format!("close frame code={code:?} reason={reason:?}"));
                    warn!(?code, reason, "server sent close frame");
                }
                route_opaque(recv_ns, opcode_name(other), frame.payload())
            }
        };
        let seq_last = line.seq_last;
        let sequenced = line.has_seq();
        if tx.send(line).is_err() {
            return end(EndKind::NetError, "writer gone".into(), envelopes);
        }
        if sequenced {
            envelopes += 1;
        }
        if last_log.elapsed() >= Duration::from_secs(60) {
            info!(envelopes, last_seq = seq_last, "alive");
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

    /// The pause chosen at the last `disconnected` event, if the log has one.
    pub fn last_pause(&self) -> Option<PendingPause> {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(&self.path).ok()?;
        let len = f.metadata().ok()?.len();
        let from = len.saturating_sub(64 * 1024);
        f.seek(SeekFrom::Start(from)).ok()?;
        let mut buf = String::new();
        f.read_to_string(&mut buf).ok()?;
        buf.lines().rev().find_map(parse_pause_row)
    }
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
