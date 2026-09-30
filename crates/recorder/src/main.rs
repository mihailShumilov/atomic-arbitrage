//! Phase-1 feed recorder.
//!
//! Stores every feed envelope verbatim, one per line, in hourly zstd files:
//!   <out>/YYYY/MM/DD/feed-YYYYMMDD-HH.tsv.zst
//! Line format (tab-separated):
//!   recv_unix_ns \t seq_first \t seq_last \t <raw envelope JSON>
//! Gaps in sequence numbers are appended to <out>/gaps.tsv for RPC backfill.
//!
//! Deliberately NOT done here: decoding l2Msg, signature checks, anything
//! latency-critical. Raw first, decode later.

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::Parser;
use futures::StreamExt;
use hood_core::{detect_gap, FeedEnvelope, FEED_URL};
use tokio_rustls::{rustls, TlsConnector};
use tracing::{error, info, warn};
use yawc::{frame::OpCode, CompressionLevel, Options, WebSocket};

#[derive(Parser, Debug)]
#[command(about = "Record the Robinhood Chain sequencer feed to hourly zstd files")]
struct Args {
    #[arg(long, env = "FEED_URL", default_value = FEED_URL)]
    url: String,
    #[arg(long, env = "RECORDER_OUT_DIR", default_value = "data/feed")]
    out_dir: PathBuf,
    /// Reconnect if no message arrives for this many seconds
    /// (blocks are ~100 ms, silence means a broken stream).
    #[arg(long, default_value_t = 10)]
    idle_timeout_secs: u64,
    #[arg(long, default_value_t = 3)]
    zstd_level: i32,
}

/// One recorded envelope, handed from the network task to the writer thread.
struct Line {
    recv_ns: u128,
    seq_first: u64,
    seq_last: u64,
    raw: String,
}

fn now_ns() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
}

/// TLS with the OS certificate store (works on normal servers and behind
/// TLS-intercepting proxies) and HTTP/1.1 ALPN.
fn tls_connector() -> Result<TlsConnector> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(cert);
    }
    anyhow::ensure!(!roots.is_empty(), "no OS root certificates found (install ca-certificates)");
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

// ---------------------------------------------------------------- writer ---

struct HourlyWriter {
    root: PathBuf,
    level: i32,
    hour_key: String,
    enc: Option<zstd::Encoder<'static, BufWriter<File>>>,
}

impl HourlyWriter {
    fn new(root: PathBuf, level: i32) -> Self {
        Self { root, level, hour_key: String::new(), enc: None }
    }

    fn path_for(&self, t: DateTime<Utc>) -> PathBuf {
        self.root
            .join(t.format("%Y/%m/%d").to_string())
            .join(format!("feed-{}.tsv.zst", t.format("%Y%m%d-%H")))
    }

    fn rotate_if_needed(&mut self, t: DateTime<Utc>) -> Result<()> {
        let key = t.format("%Y%m%d%H").to_string();
        if key == self.hour_key && self.enc.is_some() {
            return Ok(());
        }
        self.finish()?;
        let path = self.path_for(t);
        fs::create_dir_all(path.parent().unwrap())?;
        // Append mode: after a restart within the same hour we add a new zstd
        // frame to the same file. Concatenated zstd frames decode fine.
        let file = OpenOptions::new().create(true).append(true).open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        self.enc = Some(zstd::Encoder::new(BufWriter::new(file), self.level)?);
        self.hour_key = key;
        info!(file = %path.display(), "writing");
        Ok(())
    }

    fn write(&mut self, l: &Line) -> Result<()> {
        let t: DateTime<Utc> = DateTime::from_timestamp_nanos(l.recv_ns as i64);
        self.rotate_if_needed(t)?;
        let enc = self.enc.as_mut().unwrap();
        // Feed JSON is compact; guard against stray newlines/tabs anyway.
        let raw = l.raw.replace(['\n', '\r'], "");
        writeln!(enc, "{}\t{}\t{}\t{}", l.recv_ns, l.seq_first, l.seq_last, raw)?;
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        if let Some(enc) = self.enc.take() {
            let mut w = enc.finish()?;
            w.flush()?;
        }
        Ok(())
    }
}

fn append_line(path: &Path, line: &str) -> Result<()> {
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{line}")?;
    Ok(())
}

/// Writer thread: owns the files so the network task never blocks on disk
/// or compression. Tracks the last sequence number and records gaps.
fn writer_thread(rx: Receiver<Line>, out: PathBuf, level: i32) -> Result<()> {
    let state_path = out.join("last_seq.txt");
    let gaps_path = out.join("gaps.tsv");
    let mut last_seq: Option<u64> = fs::read_to_string(&state_path)
        .ok()
        .and_then(|s| s.trim().parse().ok());
    if let Some(s) = last_seq {
        info!(last_seq = s, "resuming");
    }
    let mut w = HourlyWriter::new(out.clone(), level);
    let mut since_state = 0u32;

    for l in rx {
        // Duplicate or stale envelope (e.g. replay after reconnect): skip.
        if let Some(last) = last_seq {
            if l.seq_last <= last {
                continue;
            }
        }
        if let Some(g) = detect_gap(last_seq, l.seq_first) {
            warn!(from = g.from, to = g.to, "gap in feed");
            append_line(&gaps_path, &format!("{}\t{}\t{}", g.from, g.to, l.recv_ns))?;
        }
        w.write(&l)?;
        last_seq = Some(l.seq_last);
        since_state += 1;
        if since_state >= 500 {
            fs::write(&state_path, l.seq_last.to_string())?;
            since_state = 0;
        }
    }
    w.finish()?;
    if let Some(s) = last_seq {
        fs::write(&state_path, s.to_string())?;
    }
    info!("writer stopped cleanly");
    Ok(())
}

// --------------------------------------------------------------- network ---

async fn run_connection(args: &Args, tx: &SyncSender<Line>) -> Result<()> {
    let url = args.url.parse().context("bad feed url")?;
    let ws = WebSocket::connect(url)
        // Server negotiates no_context_takeover both ways; deflate is mandatory.
        .with_options(Options::default().with_compression_level(CompressionLevel::fast()))
        .with_connector(tls_connector()?)
        .await
        .context("connect")?;
    info!(url = %args.url, "connected");
    let mut ws = ws;
    let idle = Duration::from_secs(args.idle_timeout_secs);
    let mut count: u64 = 0;
    let mut last_log = std::time::Instant::now();

    loop {
        let frame = match tokio::time::timeout(idle, ws.next()).await {
            Err(_) => anyhow::bail!("idle for {:?}, reconnecting", idle),
            Ok(None) => anyhow::bail!("stream closed by server"),
            Ok(Some(f)) => f,
        };
        let recv_ns = now_ns();
        if frame.opcode() != OpCode::Text {
            continue;
        }
        let raw = frame.as_str().to_owned();
        let env: FeedEnvelope = match serde_json::from_str(&raw) {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, "unparseable envelope, stored with seq 0");
                tx.send(Line { recv_ns, seq_first: 0, seq_last: 0, raw })?;
                continue;
            }
        };
        let Some((seq_first, seq_last)) = env.seq_range() else {
            continue; // e.g. confirmedSequenceNumber-only messages
        };
        tx.send(Line { recv_ns, seq_first, seq_last, raw })?;
        count += 1;
        if last_log.elapsed() >= Duration::from_secs(60) {
            info!(envelopes = count, last_seq = seq_last, "alive");
            last_log = std::time::Instant::now();
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    fs::create_dir_all(&args.out_dir)?;

    let (tx, rx) = sync_channel::<Line>(200_000);
    let out = args.out_dir.clone();
    let level = args.zstd_level;
    let writer = std::thread::spawn(move || {
        if let Err(e) = writer_thread(rx, out, level) {
            error!(error = %e, "writer failed");
            std::process::exit(2);
        }
    });

    let net = async {
        let mut backoff = Duration::from_millis(250);
        loop {
            match run_connection(&args, &tx).await {
                Ok(()) => backoff = Duration::from_millis(250),
                Err(e) => warn!(error = %e, ?backoff, "connection ended"),
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    };

    tokio::select! {
        _ = net => {}
        _ = tokio::signal::ctrl_c() => info!("ctrl-c, shutting down"),
    }
    drop(tx);
    writer.join().ok();
    Ok(())
}
