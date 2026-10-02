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
//! - the probe's own log is `<out-dir>/probe-connections.tsv` (task 021; before:
//!   `connections.tsv`, so attempts logged by older runs are not counted);
//! - refuses if the recorder's `connections.tsv` (`--recorder-log`) shows a
//!   connection less than 3600 s ago;
//! - the attempt is logged before the TCP connect, so a crash still counts.
//!
//! Connect, response head and close handshake come from the recorder's
//! library (`recorder::transport`, task 021 item 6; before: a copy of
//! `src/net.rs`). The feed URL defaults to `hood_core::FEED_URL`.
//! Research tool only (task 005, closed); not used by the recorder.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{bail, Context as _, Result};
use base64::Engine as _;
use clap::Parser;
use hood_core::FEED_URL;
use recorder::connlog::{col, ConnEventKind};
use recorder::now_ns;
use recorder::transport::{close_handshake, close_summary, connect, opcode_name, tls_connector};
use yawc::frame::OpCode;
use yawc::Frame;

const RECORDER_MIN_GAP_NS: u128 = 3600 * 1_000_000_000;

#[derive(Parser, Debug)]
struct Args {
    /// Feed URL.
    #[arg(long, default_value = FEED_URL)]
    url: String,
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

fn utc(ns: u128) -> String {
    chrono::DateTime::from_timestamp_nanos(ns as i64).format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

fn clean(s: &str) -> String {
    s.replace('\t', " ").replace("\r\n", " | ").replace(['\n', '\r'], " ")
}

// ------------------------------------------------------------ safety rails ---

/// The probe's own journal in `--out-dir` (its schema is [`LOG_HEADER`]).
const PROBE_LOG: &str = "probe-connections.tsv";
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
                let event = *f.get(col::EVENT)?;
                (event == ConnEventKind::Connected.as_str() || event == ConnEventKind::Disconnected.as_str())
                    .then(|| f.get(col::TS_UNIX_NS)?.parse::<u128>().ok())
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

/// A non-text frame as a probe line (`recorderFrame` with base64 payload).
fn opaque_line(recv: u128, f: &Frame) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(f.payload());
    format!(
        "{recv}\t0\t0\t0\t{{\"recorderFrame\":{{\"opcode\":\"{}\",\"payloadBase64\":\"{b64}\"}}}}",
        opcode_name(f.opcode())
    )
}

/// Any frame as a probe line: text as received, other opcodes wrapped.
fn opaque_or_text_line(recv: u128, f: &Frame) -> String {
    match f.opcode() {
        OpCode::Text => format!("{recv}\t0\t0\t0\t{}", String::from_utf8_lossy(f.payload())),
        _ => opaque_line(recv, f),
    }
}

// -------------------------------------------------------------------- main ---

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    std::fs::create_dir_all(&args.out_dir)?;
    // Not `connections.tsv`: that name and its 11-column schema belong to the
    // recorder (healthcheck reads it); before task 021 the probe used it too.
    let log = args.out_dir.join(PROBE_LOG);
    check_rails(&args, &log)?;

    let requested = args.tip.checked_sub(args.depth).context("depth > tip")?;
    let sent_headers = format!("Arbitrum-Feed-Client-Version: 2; Arbitrum-Requested-Sequence-Number: {requested}");
    log_row(&log, "attempt", &args, requested, "-", "-", &sent_headers);

    let tls = tls_connector()?;
    let t_conn = Instant::now();
    let mut ws = match connect(&args.url, &tls, Some(requested)).await {
        Ok(ws) => ws,
        Err(e) => {
            let status = e.http_status.map_or_else(|| "-".to_string(), |s| s.to_string());
            let retry = e.retry_after_raw.unwrap_or_else(|| "-".into());
            log_row(&log, "failed", &args, requested, &status, &retry, &e.detail);
            bail!("connect failed: {}; status {status}, Retry-After {retry}", e.detail);
        }
    };
    let connected_ns = now_ns();
    log_row(
        &log,
        "connected",
        &args,
        requested,
        "101",
        "-",
        &format!("upgrade_ms={} url={}", t_conn.elapsed().as_millis(), args.url),
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
            _ => {
                writeln!(out, "{}", opaque_line(recv, &frame))?;
                if let Some(c) = close_summary(&frame) {
                    end_reason = format!("server {c}");
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
    let close_outcome = if !end_reason.starts_with("server close") && !end_reason.starts_with("stream ended") {
        let mut late = Vec::new();
        let c = close_handshake(&mut ws, "probe done", |f| late.push(opaque_or_text_line(now_ns(), f))).await;
        for l in late {
            writeln!(out, "{l}")?;
        }
        format!("{} ({})", c.reply.as_str(), c.detail)
    } else {
        "server ended first".to_string()
    };
    out.flush()?;

    let summary = format!(
        "end={end_reason} close={close_outcome} close_ms={} session_s={:.1} envelopes={envelopes} msgs={msgs} max_msgs_per_envelope={max_per_env} first_seq={first_seq} first_minus_requested={} last_seq={last_seq} frames={}",
        t_close.elapsed().as_millis(),
        started.elapsed().as_secs_f64(),
        first_seq as i128 - requested as i128,
        frames_path.display()
    );
    log_row(&log, "closed", &args, requested, "101", "-", &summary);
    println!("{summary}");
    Ok(())
}
