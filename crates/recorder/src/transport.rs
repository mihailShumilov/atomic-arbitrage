//! TCP + TLS + WebSocket upgrade with the HTTP response head kept, and the
//! client side of the WebSocket close handshake (task 002 item 4, task 008
//! item 3, task 009 items 1 and 6). Shared by the recorder and
//! `examples/feed_probe.rs` (task 021 item 6).
//!
//! yawc reports a failed upgrade only as `InvalidStatusCode(u16)` and drops
//! the response headers, so `Retry-After` would be lost. We therefore do the
//! TCP/TLS part ourselves (same steps as `WebSocket::connect`) and wrap the
//! stream in [`HeadTap`], which keeps a copy of the HTTP response head. This
//! costs no extra request: one connection, one upgrade attempt.

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use futures::SinkExt;
use hood_core::http::parse_retry_after;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::{rustls, TlsConnector};
use yawc::close::CloseCode;
use yawc::{frame::OpCode, CompressionLevel, Frame, HttpRequest, MaybeTlsStream, Options, WebSocket, WebSocketError};

use crate::backoff::EndKind;

const HEAD_CAP: usize = 16 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// Client-initiated close (shutdown, idle timeout, writer gone): how long to
/// wait for the server's Close after sending ours.
pub const CLOSE_REPLY_WAIT: Duration = Duration::from_secs(2);
/// Client-initiated close: bound on the final TLS close_notify / TCP FIN.
const CLOSE_SHUTDOWN_WAIT: Duration = Duration::from_millis(500);

/// TLS with the OS certificate store (works on normal servers and behind
/// TLS-intercepting proxies) and HTTP/1.1 ALPN.
pub fn tls_connector() -> Result<TlsConnector> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(cert);
    }
    anyhow::ensure!(!roots.is_empty(), "no OS root certificates found (install ca-certificates)");
    let mut cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
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
    /// Wrap `inner`; the returned buffer fills with the response head.
    pub fn new(inner: S) -> (Self, Arc<Mutex<Vec<u8>>>) {
        let head = Arc::new(Mutex::new(Vec::new()));
        (Self { inner, head: head.clone(), capturing: true }, head)
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for HeadTap<S> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
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
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, b: &[u8]) -> Poll<std::io::Result<usize>> {
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

/// Parse the captured response head (anything after `\r\n\r\n` is ignored).
pub fn parse_http_head(bytes: &[u8]) -> HttpHead {
    let end = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(bytes.len());
    let text = String::from_utf8_lossy(&bytes[..end]);
    let mut lines = text.split("\r\n");
    let status_line = lines.next().unwrap_or("").trim().to_string();
    let status = status_line.split_whitespace().nth(1).and_then(|s| s.parse().ok());
    let retry_after = lines.find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case("retry-after").then(|| v.trim().to_string())
    });
    HttpHead { status, retry_after, status_line }
}

// ---------------------------------------------------------------- connect ---

/// A failed connect or upgrade, classified for the pause ladder.
#[derive(Debug, Clone)]
pub struct ConnectError {
    pub kind: EndKind,
    /// HTTP status of the upgrade response, if one was seen.
    pub http_status: Option<u16>,
    /// `Retry-After` as sent.
    pub retry_after_raw: Option<String>,
    /// `Retry-After` as a delay (no cap applied).
    pub retry_after: Option<Duration>,
    pub detail: String,
}

impl ConnectError {
    fn net(detail: String) -> Self {
        Self { kind: EndKind::NetError, http_status: None, retry_after_raw: None, retry_after: None, detail }
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

/// An upgraded feed connection.
pub type FeedWs = WebSocket<HeadTap<MaybeTlsStream<TcpStream>>>;

/// Handshake request: with `requested`, the two headers the Nitro feed client
/// sends (task 005: `broadcastclient.go` v3.11.4 L231-L234); without it, no
/// extra headers (the behaviour of tasks 001-008).
fn handshake_request(requested: Option<u64>) -> yawc::HttpRequestBuilder {
    let b = HttpRequest::builder();
    match requested {
        Some(n) => {
            b.header("Arbitrum-Feed-Client-Version", "2").header("Arbitrum-Requested-Sequence-Number", n.to_string())
        }
        None => b,
    }
}

/// Connect and upgrade (one TCP connection, one upgrade request). `ws://`
/// is only for local mock-feed tests; the real feed is `wss://`.
pub async fn connect(url_str: &str, tls: &TlsConnector, requested: Option<u64>) -> Result<FeedWs, ConnectError> {
    let url: url::Url = url_str.parse().map_err(|e| ConnectError::net(format!("bad url: {e}")))?;
    let host = url.host_str().unwrap_or_default().to_string();
    let port = url.port_or_known_default().unwrap_or(443);
    let res = tokio::time::timeout(CONNECT_TIMEOUT, async {
        let tcp =
            TcpStream::connect((host.as_str(), port)).await.map_err(|e| ConnectError::net(format!("tcp: {e}")))?;
        let _ = tcp.set_nodelay(true);
        let stream = if url.scheme() == "ws" {
            MaybeTlsStream::Plain(tcp)
        } else {
            let name =
                ServerName::try_from(host.clone()).map_err(|e| ConnectError::net(format!("server name: {e}")))?;
            let tls_stream = tls.connect(name, tcp).await.map_err(|e| ConnectError::net(format!("tls: {e}")))?;
            MaybeTlsStream::Tls(tls_stream)
        };
        Ok::<_, ConnectError>(HeadTap::new(stream))
    })
    .await;
    let (tap, head_buf) = match res {
        Err(_) => return Err(ConnectError::net("tcp/tls timeout".into())),
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
            let status = match &e {
                WebSocketError::InvalidStatusCode(c) => Some(*c),
                _ => head.status,
            };
            Err(ConnectError {
                kind: classify_upgrade_error(&e, &head),
                http_status: status,
                retry_after: head.retry_after.as_deref().and_then(|v| parse_retry_after(v, SystemTime::now())),
                retry_after_raw: head.retry_after.clone(),
                detail: format!("upgrade: {e}; {}", head.status_line),
            })
        }
        Err(_) => Err(ConnectError { http_status: head.status, ..ConnectError::net("upgrade timeout".into()) }),
    }
}

// ------------------------------------------------------------ close frame ---

/// Name of a frame opcode as stored in `recorderFrame.opcode`.
pub fn opcode_name(op: OpCode) -> &'static str {
    match op {
        OpCode::Text => "text",
        OpCode::Binary => "binary",
        OpCode::Ping => "ping",
        OpCode::Pong => "pong",
        OpCode::Close => "close",
        OpCode::Continuation => "continuation",
    }
}

/// `close frame code=.. reason=..` for a Close frame, None for other frames.
pub fn close_summary(frame: &Frame) -> Option<String> {
    (frame.opcode() == OpCode::Close).then(|| {
        let code = frame.close_code().map(u16::from);
        let reason = frame.close_reason().ok().flatten().unwrap_or("").to_string();
        format!("close frame code={code:?} reason={reason:?}")
    })
}

/// How a client-initiated close handshake ended (`client_close` reason in
/// connections.tsv).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReply {
    /// The server answered with its Close.
    ServerReplied,
    /// No Close from the server within [`CLOSE_REPLY_WAIT`].
    NoReply,
    /// The stream ended before the server's Close.
    StreamEnded,
    /// Our Close could not be sent.
    SendFailed,
}

impl CloseReply {
    /// The string written to connections.tsv (unchanged since task 008).
    pub fn as_str(self) -> &'static str {
        match self {
            CloseReply::ServerReplied => "server_replied",
            CloseReply::NoReply => "no_reply",
            CloseReply::StreamEnded => "stream_ended",
            CloseReply::SendFailed => "send_failed",
        }
    }
}

/// Result of a client-initiated close handshake.
#[derive(Debug, Clone)]
pub struct CloseOutcome {
    pub reply: CloseReply,
    /// From sending our Close to the end of the wait.
    pub took: Duration,
    pub detail: String,
}

/// Client-initiated close (task 008 item 3 for shutdown; task 009 item 6 for
/// idle timeout and a lost writer): send Close 1000 with `reason`, hand
/// whatever still arrives to `on_frame` until the server's Close or
/// [`CLOSE_REPLY_WAIT`], then shut the stream down (TLS close_notify, FIN).
/// Before 008 the socket was dropped on SIGTERM, before 009 on idle timeout,
/// both without a Close.
pub async fn close_handshake(ws: &mut FeedWs, reason: &str, mut on_frame: impl FnMut(&Frame)) -> CloseOutcome {
    let t0 = Instant::now();
    let deadline = tokio::time::Instant::now() + CLOSE_REPLY_WAIT;
    let out = |reply, detail| CloseOutcome { reply, took: t0.elapsed(), detail };
    let sent = tokio::time::timeout_at(deadline, ws.send(Frame::close(CloseCode::Normal, reason.as_bytes()))).await;
    let mut outcome = match sent {
        Err(_) => out(CloseReply::SendFailed, "timeout sending close frame".into()),
        Ok(Err(e)) => out(CloseReply::SendFailed, format!("sending close frame: {e}")),
        Ok(Ok(())) => loop {
            match tokio::time::timeout_at(deadline, ws.next_frame()).await {
                Err(_) => {
                    break out(CloseReply::NoReply, format!("no close frame from server within {CLOSE_REPLY_WAIT:?}"))
                }
                Ok(Err(e)) => {
                    break out(CloseReply::StreamEnded, format!("stream ended before the server's close: {e}"))
                }
                Ok(Ok(frame)) => {
                    // Nothing is dropped, also during the close handshake.
                    on_frame(&frame);
                    if let Some(c) = close_summary(&frame) {
                        break out(CloseReply::ServerReplied, c);
                    }
                }
            }
        },
    };
    outcome.detail = format!("close 1000 {reason:?}: {}", outcome.detail);
    match tokio::time::timeout(CLOSE_SHUTDOWN_WAIT, ws.close()).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => outcome.detail = format!("{}; stream shutdown: {e}", outcome.detail),
        Err(_) => outcome.detail = format!("{}; stream shutdown timed out", outcome.detail),
    }
    outcome
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
        assert_eq!(classify_upgrade_error(&WebSocketError::InvalidUpgradeHeader, &none), EndKind::Forbidden);
        assert_eq!(classify_upgrade_error(&WebSocketError::ConnectionClosed, &none), EndKind::NetError);
    }

    #[test]
    fn close_reply_strings_are_unchanged() {
        let all = [CloseReply::ServerReplied, CloseReply::NoReply, CloseReply::StreamEnded, CloseReply::SendFailed];
        let s: Vec<&str> = all.iter().map(|r| r.as_str()).collect();
        assert_eq!(s, ["server_replied", "no_reply", "stream_ended", "send_failed"]);
    }

    #[tokio::test]
    async fn head_tap_captures_response_head() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut server, client) = tokio::io::duplex(64);
        let (mut tap, head) = HeadTap::new(client);
        tokio::spawn(async move {
            server.write_all(b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 7\r\n\r\nbody").await.unwrap();
        });
        let mut out = Vec::new();
        tap.read_to_end(&mut out).await.unwrap();
        let h = parse_http_head(&head.lock().unwrap());
        assert_eq!((h.status, h.retry_after.as_deref()), (Some(429), Some("7")));
        assert!(out.ends_with(b"body"));
    }
}
