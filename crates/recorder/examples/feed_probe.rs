//! Task 005 probe: does the public feed honour `Arbitrum-Requested-Sequence-Number`?
//!
//! One short connection per run. Sends the same handshake headers as the Nitro
//! broadcast client (`broadcastclient/broadcastclient.go`, `connect()`):
//!   Arbitrum-Feed-Client-Version: 2
//!   Arbitrum-Requested-Sequence-Number: <N>
//! and records every frame raw. Closes with WebSocket Close 1000.
//!
//! Safety rails (feed bans are 403 + Retry-After: 3600, see chain-facts.md):
//! - refuses to connect if the probe log already has any 403/429, has 3 or
//!   more attempts, or the last attempt was less than `--min-gap-secs` ago;
//! - refuses if the recorder's `connections.tsv` (`--recorder-log`) shows a
//!   connection less than 3600 s ago;
//! - the attempt is logged before the TCP connect, so a crash still counts.
//!
//! Minimal copy of the connect / HeadTap / close code from `src/net.rs`
//! (the recorder is a binary crate, its modules are not importable).
//! Research tool only; not used by the recorder.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context as _, Result};
use base64::Engine as _;
use clap::Parser;
use futures::SinkExt;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::{rustls, TlsConnector};
use yawc::close::CloseCode;
use yawc::{frame::OpCode, CompressionLevel, Frame, HttpRequest, MaybeTlsStream, Options, WebSocket};

const FEED_URL: &str = "wss://feed.mainnet.chain.robinhood.com";
const HEAD_CAP: usize = 16 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const CLOSE_REPLY_WAIT: Duration = Duration::from_secs(2);
const RECORDER_MIN_GAP_NS: u128 = 3600 * 1_000_000_000;

#[derive(Parser, Debug)]
struct Args {
    /// Depth label (blocks back from `--tip`), used for file names and the log.
    #[arg(long)]
    depth: u64,
    /// Current tip from one `eth_blockNumber` call right before the probe.
    #[arg(long)]
    tip: u64,
    /// Output directory (scratchpad), holds the probe log and frame files.
    #[arg(long)]
    out_dir: PathBuf,
    /// Recorder connection log to check for a recent connection.
    #[arg(long)]
    recorder_log: Vec<PathBuf>,
    /// Hard cap on the session length.
    #[arg(long, default_value_t = 60)]
    max_secs: u64,
    /// Stop once this many blocks past `--tip` were received (and >= 5 s passed).
    #[arg(long, default_value_t = 30)]
    past_tip: u64,
    /// Minimum pause since the previous probe attempt.
    #[arg(long, default_value_t = 180)]
    min_gap_secs: u64,
    /// Maximum probe attempts in total (all runs).
    #[arg(long, default_value_t = 3)]
    max_attempts: usize,
}

fn now_ns() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos()
}

fn utc(ns: u128) -> String {
    chrono::DateTime::from_timestamp_nanos(ns as i64).format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

fn clean(s: &str) -> String {
    s.replace('\t', " ").replace("\r\n", " | ").replace(['\n', '\r'], " ")
}

// ------------------------------------------------------------ safety rails ---

const LOG_HEADER: &str = "# ts_utc\tts_unix_ns\tevent\tdepth\trequested\ttip\thttp_status\tretry_after\tdetail";

fn check_rails(args: &Args, log: &PathBuf) -> Result<()> {
    let now = now_ns();
    if let Ok(text) = std::fs::read_to_string(log) {
        let rows: Vec<Vec<&str>> =
            text.lines().filter(|l| !l.starts_with('#') && !l.is_empty()).map(|l| l.split('\t').collect()).collect();
        for r in &rows {
            if matches!(r.get(6), Some(&"403") | Some(&"429")) {
                bail!("probe log has a {} row; stop, do not retry: {r:?}", r[6]);
            }
        }
        let attempts: Vec<&Vec<&str>> = rows.iter().filter(|r| r.get(2) == Some(&"attempt")).collect();
        if attempts.len() >= args.max_attempts {
            bail!("already {} attempts in the probe log (cap {})", attempts.len(), args.max_attempts);
        }
        if let Some(last) = attempts.last() {
            let t: u128 = last[1].parse().unwrap_or(0);
            let gap = now.saturating_sub(t);
            if gap < args.min_gap_secs as u128 * 1_000_000_000 {
                bail!("last probe attempt {:.0} s ago, need >= {} s", gap as f64 / 1e9, args.min_gap_secs);
            }
        }
    }
    for p in &args.recorder_log {
        let Ok(text) = std::fs::read_to_string(p) else {
            continue;
        };
        let last = text
            .lines()
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| {
                let f: Vec<&str> = l.split('\t').collect();
                (f.get(2) == Some(&"connected") || f.get(2) == Some(&"disconnected"))
                    .then(|| f.get(1)?.parse::<u128>().ok())
                    .flatten()
            })
            .max();
        if let Some(t) = last {
            if now.saturating_sub(t) < RECORDER_MIN_GAP_NS {
                bail!(
                    "recorder log {} has a feed connection {:.0} s ago (< 3600 s)",
                    p.display(),
                    (now - t) as f64 / 1e9
                );
            }
        }
    }
    Ok(())
}

fn log_row(log: &PathBuf, event: &str, args: &Args, requested: u64, status: &str, retry: &str, detail: &str) {
    let fresh = !log.exists();
    let mut f = OpenOptions::new().create(true).append(true).open(log).expect("open probe log");
    if fresh {
        let _ = writeln!(f, "{LOG_HEADER}");
    }
    let ns = now_ns();
    let _ = writeln!(
        f,
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        utc(ns),
        ns,
        event,
        args.depth,
        requested,
        args.tip,
        status,
        retry,
        clean(detail)
    );
    let _ = f.sync_all();
}

// ----------------------------------------------------------------- HeadTap ---

struct HeadTap<S> {
    inner: S,
    head: Arc<Mutex<Vec<u8>>>,
    capturing: bool,
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
}

fn head_text(buf: &[u8]) -> String {
    let end = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

fn head_field(head: &str, name: &str) -> Option<String> {
    head.split("\r\n").skip(1).find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim().to_string())
    })
}

fn tls_connector() -> Result<TlsConnector> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(cert);
    }
    let mut cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsConnector::from(Arc::new(cfg)))
}

// -------------------------------------------------------------------- main ---

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    std::fs::create_dir_all(&args.out_dir)?;
    let log = args.out_dir.join("connections.tsv");
    check_rails(&args, &log)?;

    let requested = args.tip.checked_sub(args.depth).context("depth > tip")?;
    let sent_headers = format!("Arbitrum-Feed-Client-Version: 2; Arbitrum-Requested-Sequence-Number: {requested}");
    log_row(&log, "attempt", &args, requested, "-", "-", &sent_headers);

    let url: url::Url = FEED_URL.parse()?;
    let host = url.host_str().unwrap_or_default().to_string();
    let tls = tls_connector()?;
    let t_conn = Instant::now();
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, async {
        let tcp = TcpStream::connect((host.as_str(), 443)).await?;
        let _ = tcp.set_nodelay(true);
        let name = ServerName::try_from(host.clone())?;
        let tls_stream = tls.connect(name, tcp).await?;
        anyhow::Ok(MaybeTlsStream::Tls(tls_stream))
    })
    .await;
    let stream = match stream {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            log_row(&log, "failed", &args, requested, "-", "-", &format!("tcp/tls: {e}"));
            bail!("tcp/tls: {e}");
        }
        Err(_) => {
            log_row(&log, "failed", &args, requested, "-", "-", "tcp/tls timeout");
            bail!("tcp/tls timeout");
        }
    };
    let head_buf = Arc::new(Mutex::new(Vec::new()));
    let tap = HeadTap { inner: stream, head: head_buf.clone(), capturing: true };
    let req = HttpRequest::builder()
        .header("Arbitrum-Feed-Client-Version", "2")
        .header("Arbitrum-Requested-Sequence-Number", requested.to_string());
    let opts = Options::default().with_compression_level(CompressionLevel::fast());
    let hs = tokio::time::timeout(CONNECT_TIMEOUT, WebSocket::handshake_with_request(url, tap, opts, req)).await;
    let head = head_text(&head_buf.lock().map(|h| h.clone()).unwrap_or_default());
    let status = head.split_whitespace().nth(1).unwrap_or("-").to_string();
    let retry = head_field(&head, "retry-after").unwrap_or_else(|| "-".into());
    let mut ws = match hs {
        Ok(Ok(ws)) => ws,
        Ok(Err(e)) => {
            log_row(&log, "failed", &args, requested, &status, &retry, &format!("upgrade: {e}; head: {head}"));
            bail!("upgrade failed: {e}; status {status}, Retry-After {retry}");
        }
        Err(_) => {
            log_row(&log, "failed", &args, requested, &status, &retry, &format!("upgrade timeout; head: {head}"));
            bail!("upgrade timeout");
        }
    };
    let connected_ns = now_ns();
    log_row(
        &log,
        "connected",
        &args,
        requested,
        &status,
        &retry,
        &format!("upgrade_ms={} head: {head}", t_conn.elapsed().as_millis()),
    );
    eprintln!("connected, requested={requested} tip={}", args.tip);

    let frames_path = args.out_dir.join(format!("frames-depth{}.tsv", args.depth));
    let mut out = std::io::BufWriter::new(std::fs::File::create(&frames_path)?);
    writeln!(
        out,
        "# probe 005 depth={} requested={requested} tip={} connected_ns={connected_ns}",
        args.depth, args.tip
    )?;
    writeln!(out, "# recv_unix_ns\tfirst_seq\tlast_seq\tn_msgs\tpayload (text frames as received; other opcodes as recorderFrame base64)")?;

    let started = Instant::now();
    let (mut envelopes, mut msgs, mut first_seq, mut last_seq, mut max_per_env) = (0u64, 0u64, 0u64, 0u64, 0usize);
    let mut end_reason = String::from("max_secs");
    loop {
        let left = Duration::from_secs(args.max_secs).saturating_sub(started.elapsed());
        if left.is_zero() {
            break;
        }
        let frame = match tokio::time::timeout(left.min(Duration::from_secs(15)), ws.next_frame()).await {
            Err(_) if left > Duration::from_secs(15) => {
                end_reason = "idle 15s".into();
                break;
            }
            Err(_) => break,
            Ok(Err(e)) => {
                end_reason = format!("stream ended: {e}");
                break;
            }
            Ok(Ok(f)) => f,
        };
        let recv = now_ns();
        match frame.opcode() {
            OpCode::Text => {
                let text = String::from_utf8_lossy(frame.payload()).into_owned();
                let (mut lo, mut hi, mut n) = (0u64, 0u64, 0usize);
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(arr) = v.get("messages").and_then(|m| m.as_array()) {
                        for m in arr {
                            if let Some(s) = m.get("sequenceNumber").and_then(|s| s.as_u64()) {
                                if lo == 0 || s < lo {
                                    lo = s;
                                }
                                hi = hi.max(s);
                                n += 1;
                            }
                        }
                    }
                }
                if n > 0 {
                    envelopes += 1;
                    msgs += n as u64;
                    max_per_env = max_per_env.max(n);
                    if first_seq == 0 {
                        first_seq = lo;
                        eprintln!("first seq {lo} (requested {requested}, tip {})", args.tip);
                    }
                    last_seq = last_seq.max(hi);
                }
                writeln!(out, "{recv}\t{lo}\t{hi}\t{n}\t{text}")?;
            }
            op => {
                let name = format!("{op:?}").to_lowercase();
                let b64 = base64::engine::general_purpose::STANDARD.encode(frame.payload());
                writeln!(
                    out,
                    "{recv}\t0\t0\t0\t{{\"recorderFrame\":{{\"opcode\":\"{name}\",\"payloadBase64\":\"{b64}\"}}}}"
                )?;
                if op == OpCode::Close {
                    end_reason = format!("server close code={:?}", frame.close_code().map(u16::from));
                    break;
                }
            }
        }
        if last_seq >= args.tip + args.past_tip && started.elapsed() >= Duration::from_secs(5) {
            end_reason = format!("reached tip+{}", args.past_tip);
            break;
        }
    }

    // Client close: Close 1000, wait for the server's Close, then shut down.
    let t_close = Instant::now();
    let deadline = tokio::time::Instant::now() + CLOSE_REPLY_WAIT;
    let mut close_outcome = "send_failed".to_string();
    if !end_reason.starts_with("server close") && !end_reason.starts_with("stream ended") {
        if let Ok(Ok(())) =
            tokio::time::timeout_at(deadline, ws.send(Frame::close(CloseCode::Normal, b"probe done"))).await
        {
            close_outcome = "no_reply".into();
            while let Ok(r) = tokio::time::timeout_at(deadline, ws.next_frame()).await {
                match r {
                    Ok(f) => {
                        let recv = now_ns();
                        let name = format!("{:?}", f.opcode()).to_lowercase();
                        let b64 = base64::engine::general_purpose::STANDARD.encode(f.payload());
                        writeln!(out, "{recv}\t0\t0\t0\t{{\"recorderFrame\":{{\"opcode\":\"{name}\",\"payloadBase64\":\"{b64}\"}}}}")?;
                        if f.opcode() == OpCode::Close {
                            close_outcome = format!("server_replied code={:?}", f.close_code().map(u16::from));
                            break;
                        }
                    }
                    Err(e) => {
                        close_outcome = format!("stream_ended: {e}");
                        break;
                    }
                }
            }
        }
        let _ = tokio::time::timeout(Duration::from_millis(500), ws.close()).await;
    } else {
        close_outcome = "server ended first".into();
    }
    out.flush()?;

    let summary = format!(
        "end={end_reason} close={close_outcome} close_ms={} session_s={:.1} envelopes={envelopes} msgs={msgs} max_msgs_per_envelope={max_per_env} first_seq={first_seq} first_minus_requested={} last_seq={last_seq} frames={}",
        t_close.elapsed().as_millis(),
        started.elapsed().as_secs_f64(),
        first_seq as i128 - requested as i128,
        frames_path.display()
    );
    log_row(&log, "closed", &args, requested, &status, &retry, &summary);
    println!("{summary}");
    Ok(())
}
