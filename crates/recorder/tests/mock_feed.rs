//! End-to-end tests of the recorder binary against a local mock feed on
//! 127.0.0.1 (task 008, items 2 and 3; task 009, items 1, 2 and 6; task 012,
//! items 1-3). No connection to the real feed and no
//! RPC: the binary always gets an explicit `--url ws://127.0.0.1:<port>` and
//! `--out-dir <tmp>`, and FEED_URL / RPC_URL / RECORDER_OUT_DIR are removed
//! from its environment.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::SinkExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};
use yawc::{Frame, OpCode, Options, Role, WebSocket};

fn now_ns() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "recorder-it-{tag}-{}-{}",
        std::process::id(),
        now_ns()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

// ------------------------------------------------------------- mock feed ---

/// Minimal SHA-1 (RFC 3174), only for the Sec-WebSocket-Accept header of the
/// mock server. yawc 0.4.2 does not check that header, but the mock should
/// still answer like a real server.
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap());
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 20];
    for (i, x) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&x.to_be_bytes());
    }
    out
}

fn accept_key(key: &str) -> String {
    use base64::Engine;
    let mut s = key.as_bytes().to_vec();
    s.extend_from_slice(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    base64::engine::general_purpose::STANDARD.encode(sha1(&s))
}

#[test]
fn sha1_known_vector() {
    // RFC 6455 section 1.3 example.
    assert_eq!(
        accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
        "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
    );
}

/// Accept one TCP connection, answer the HTTP upgrade (no permessage-deflate:
/// the server may decline the extension) and hand back a server-side socket.
async fn accept_ws(listener: &TcpListener) -> WebSocket<TcpStream> {
    accept_ws_req(listener).await.0
}

/// Value of request header `name` (case-insensitive) in an HTTP request head.
fn header(req: &str, name: &str) -> Option<String> {
    req.lines().skip(1).find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case(name)
            .then(|| v.trim().to_string())
    })
}

/// Like [`accept_ws`], also returns the client's HTTP request head.
async fn accept_ws_req(listener: &TcpListener) -> (WebSocket<TcpStream>, String) {
    let (mut tcp, _) = listener.accept().await.unwrap();
    let mut req = Vec::new();
    let mut buf = [0u8; 1024];
    while !req.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = tcp.read(&mut buf).await.unwrap();
        assert!(n > 0, "client closed during upgrade");
        req.extend_from_slice(&buf[..n]);
    }
    let text = String::from_utf8_lossy(&req).to_string();
    let key = text
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case("sec-websocket-key")
                .then(|| v.trim().to_string())
        })
        .expect("Sec-WebSocket-Key");
    let resp = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        accept_key(&key)
    );
    tcp.write_all(resp.as_bytes()).await.unwrap();
    (
        WebSocket::from_stream(tcp, Role::Server, Options::default()).unwrap(),
        text,
    )
}

// Shape of a real envelope captured on 2026-09-28 (block 74755960,
// l2Msg/signature shortened), see hood-core and route.rs tests.
fn envelope(seq: u64) -> String {
    format!(
        r#"{{"version":1,"messages":[{{"sequenceNumber":{seq},"message":{{"message":{{"header":{{"kind":3,"sender":"0xa4b000000000000000000073657175656e636572","blockNumber":26075606,"timestamp":1790594344,"requestId":null,"baseFeeL1":0}},"l2Msg":"AAAA"}},"delayedMessagesRead":328658}},"blockHash":"0x529d8dcb881a6f5ed00232db376cf449ad881aa2a7ac4f8eb0078ea40a17f0f4","signatureV2":"AA==","blockMetadata":null}}]}}"#
    )
}

/// Same envelope with `header.timestamp` = `ts` (unix s).
fn envelope_ts(seq: u64, ts: u64) -> String {
    envelope(seq).replace("\"timestamp\":1790594344", &format!("\"timestamp\":{ts}"))
}

fn unix_s() -> u64 {
    (now_ns() / 1_000_000_000) as u64
}

// ------------------------------------------------------------ the binary ---

fn spawn_recorder(url: &str, out: &Path, extra: &[&str]) -> Child {
    let log = std::fs::File::create(out.with_extension("log")).unwrap();
    Command::new(env!("CARGO_BIN_EXE_recorder"))
        .arg("--url")
        .arg(url)
        .arg("--out-dir")
        .arg(out)
        .args(extra)
        .env_remove("FEED_URL")
        .env_remove("RPC_URL")
        .env_remove("RECORDER_OUT_DIR")
        .env("RUST_LOG", "info")
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .kill_on_drop(true)
        .spawn()
        .unwrap()
}

fn sigterm(child: &Child) {
    let pid = child.id().expect("child still running");
    let st = std::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .unwrap();
    assert!(st.success());
}

fn read_log(out: &Path) -> String {
    std::fs::read_to_string(out.with_extension("log")).unwrap_or_default()
}

fn connections(out: &Path) -> String {
    std::fs::read_to_string(out.join("connections.tsv")).unwrap_or_default()
}

/// Wait until `cond` holds, polling every 20 ms.
async fn wait_until(what: &str, limit: Duration, mut cond: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while !cond() {
        assert!(t0.elapsed() < limit, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn all_lines(out: &Path) -> Vec<String> {
    use std::io::Read;
    let mut files = Vec::new();
    fn walk(d: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            let n = e.file_name().to_string_lossy().into_owned();
            if p.is_dir() && !n.starts_with('_') {
                walk(&p, out);
            } else if n.starts_with("feed-") && n.ends_with(".tsv.zst") {
                out.push(p);
            }
        }
    }
    walk(out, &mut files);
    files.sort();
    let mut s = String::new();
    for f in files {
        zstd::stream::read::Decoder::new(std::fs::File::open(f).unwrap())
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
    }
    s.lines().map(str::to_owned).collect()
}

/// Item 3: on SIGTERM the recorder sends WebSocket Close 1000, waits for the
/// server's Close (<= 2 s) and only then drops TCP; exit code 0; frames that
/// arrived before the signal are on disk.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sigterm_sends_close_1000_and_waits_for_reply() {
    let out = tmpdir("close");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    let mut child = spawn_recorder(&url, &out, &[]);

    let mut ws = tokio::time::timeout(Duration::from_secs(10), accept_ws(&listener))
        .await
        .expect("recorder did not connect to the mock");
    for seq in 100..=104u64 {
        ws.send(Frame::text(envelope(seq))).await.unwrap();
    }
    ws.send(Frame::ping(Vec::<u8>::new())).await.unwrap();
    wait_until("connected event", Duration::from_secs(5), || {
        connections(&out).contains("\tconnected\t")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    sigterm(&child);
    let t_sig = Instant::now();

    // Read what the client sends until its Close frame or EOF.
    let mut client_close: Option<(Option<u16>, Duration)> = None;
    let mut seen = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(5), ws.next_frame()).await {
            Err(_) => panic!("client neither closed nor sent Close within 5 s: {seen:?}"),
            Ok(Err(e)) => {
                seen.push(format!("stream end: {e}"));
                break;
            }
            Ok(Ok(f)) => {
                seen.push(format!("{:?}", f.opcode()));
                if f.opcode() == OpCode::Close {
                    client_close = Some((f.close_code().map(u16::from), t_sig.elapsed()));
                    break;
                }
            }
        }
    }
    let (code, after) = client_close.unwrap_or_else(|| {
        panic!("mock saw no Close frame from the client before EOF, frames: {seen:?}")
    });
    assert_eq!(code, Some(1000), "close code, frames: {seen:?}");
    assert!(after < Duration::from_secs(2), "close sent after {after:?}");
    // Reply like a well-behaved server; the client should then exit.
    ws.close().await.ok();

    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("recorder did not exit")
        .unwrap();
    assert_eq!(status.code(), Some(0), "log:\n{}", read_log(&out));

    let conns = connections(&out);
    eprintln!("connections.tsv:\n{conns}");
    assert!(
        conns.contains("\tclient_close\tserver_replied\t"),
        "connections.tsv:\n{conns}"
    );
    let lines = all_lines(&out);
    let seqs: Vec<&str> = lines
        .iter()
        .map(|l| l.split('\t').nth(1).unwrap())
        .filter(|s| *s != "0")
        .collect();
    assert_eq!(seqs, ["100", "101", "102", "103", "104"], "{lines:?}");
    assert!(lines.iter().any(|l| l.contains(r#""opcode":"ping""#)));
    // The server's Close reply is recorded too (nothing is dropped).
    assert!(
        lines.iter().any(|l| l.contains(r#""opcode":"close""#)),
        "{lines:?}"
    );
    assert_eq!(
        std::fs::read_to_string(out.join("last_seq.txt")).unwrap(),
        "104"
    );
    std::fs::remove_dir_all(&out).ok();
    std::fs::remove_file(out.with_extension("log")).ok();
}

/// Item 3, server that never answers the Close: the recorder gives up after
/// ~2 s and still exits 0.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sigterm_without_close_reply_gives_up_after_2s() {
    let out = tmpdir("noreply");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    let mut child = spawn_recorder(&url, &out, &[]);
    let mut ws = tokio::time::timeout(Duration::from_secs(10), accept_ws(&listener))
        .await
        .expect("recorder did not connect to the mock");
    ws.send(Frame::text(envelope(7))).await.unwrap();
    wait_until("connected event", Duration::from_secs(5), || {
        connections(&out).contains("\tconnected\t")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    sigterm(&child);
    let t_sig = Instant::now();
    // Keep the socket open but never reply. Drain in the background so the
    // client's writes do not block.
    let drain = tokio::spawn(async move {
        let mut got_close = None;
        while let Ok(Ok(f)) = tokio::time::timeout(Duration::from_secs(10), ws.next_frame()).await {
            if f.opcode() == OpCode::Close {
                got_close = f.close_code().map(u16::from);
                // Do not reply. Polling the socket again would make yawc
                // flush its automatic Close echo, so just hold it open.
                tokio::time::sleep(Duration::from_secs(4)).await;
                break;
            }
        }
        (got_close, ws)
    });
    let status = tokio::time::timeout(Duration::from_secs(6), child.wait())
        .await
        .expect("recorder did not exit")
        .unwrap();
    let took = t_sig.elapsed();
    assert_eq!(status.code(), Some(0), "log:\n{}", read_log(&out));
    assert!(
        took >= Duration::from_millis(1900) && took < Duration::from_secs(4),
        "exit after {took:?}"
    );
    let (code, _ws) = drain.await.unwrap();
    assert_eq!(code, Some(1000));
    let conns = connections(&out);
    eprintln!("connections.tsv:\n{conns}");
    assert!(
        conns.contains("\tclient_close\tno_reply\t"),
        "connections.tsv:\n{conns}"
    );
    std::fs::remove_dir_all(&out).ok();
    std::fs::remove_file(out.with_extension("log")).ok();
}

/// Item 2 (008): `connected` 30 s ago and `shutdown` 20 s ago in
/// connections.tsv -> the new process waits the rest of the 120 s minimum
/// interval before connecting. Since task 012 the interval counts from the
/// end of the session (the `shutdown` row): ~100 s (008: ~90 s from
/// `connected`). SIGTERM interrupts the wait and the exit code is 0. The
/// mock must see no attempt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn min_connect_interval_survives_restart_and_sigterm_interrupts() {
    let out = tmpdir("minint");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    let t_conn = now_ns() - 30 * 1_000_000_000;
    std::fs::write(
        out.join("connections.tsv"),
        format!(
            "# ts_utc\tts_unix_ns\tevent\treason\thttp_status\tretry_after\tpause_s\tsession_s\tenvelopes\tstrikes\tdetail\n\
             2026-09-30T00:00:00.000Z\t{t_conn}\tconnected\t-\t101\t-\t-\t-\t-\t0\t{url}\n\
             2026-09-30T00:00:10.000Z\t{}\tshutdown\tSIGTERM\t-\t-\t-\t-\t-\t-\t-\n",
            t_conn + 10_000_000_000
        ),
    )
    .unwrap();
    let mut child = spawn_recorder(&url, &out, &[]);
    wait_until("startup_wait event", Duration::from_secs(10), || {
        connections(&out).contains("\tstartup_wait\tmin_connect_interval\t")
    })
    .await;
    let conns = connections(&out);
    eprintln!("connections.tsv:\n{conns}");
    let row = conns
        .lines()
        .find(|l| l.contains("\tstartup_wait\t"))
        .unwrap();
    let pause: f64 = row.split('\t').nth(6).unwrap().parse().unwrap();
    assert!((98.0..=100.5).contains(&pause), "pause {pause}, row {row}");
    assert!(row.contains("(end=log_row)"), "{row}");

    // Nobody may connect while the wait is running.
    let accepted = tokio::time::timeout(Duration::from_millis(1500), listener.accept()).await;
    assert!(accepted.is_err(), "recorder connected during the wait");

    sigterm(&child);
    let t_sig = Instant::now();
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("SIGTERM did not interrupt the wait")
        .unwrap();
    assert_eq!(status.code(), Some(0), "log:\n{}", read_log(&out));
    assert!(t_sig.elapsed() < Duration::from_secs(2));
    let conns = connections(&out);
    eprintln!("connections.tsv:\n{conns}");
    assert_eq!(conns.matches("\tconnected\t").count(), 1, "{conns}");
    assert!(conns
        .lines()
        .last()
        .unwrap()
        .contains("\tshutdown\tSIGTERM"));
    std::fs::remove_dir_all(&out).ok();
    std::fs::remove_file(out.with_extension("log")).ok();
}

// ------------------------------------------------------------- task 009 ---

/// One recorder-format zstd frame with envelopes `seqs` (one per line).
fn data_frame(seqs: std::ops::RangeInclusive<u64>) -> Vec<u8> {
    use std::io::Write;
    let mut enc = zstd::Encoder::new(Vec::new(), 3).unwrap();
    enc.include_checksum(true).unwrap();
    for (i, seq) in seqs.enumerate() {
        writeln!(
            enc,
            "{}\t{seq}\t{seq}\t{}",
            1_790_769_600_000_000_000u128 + i as u128 * 100_000_000,
            envelope(seq)
        )
        .unwrap();
    }
    enc.finish().unwrap()
}

/// Out-dir with an earlier recording of seqs 100..=104 (hour 2026-09-30 12)
/// and a stale last_seq.txt (90): the header must come from the data.
fn seeded_out(tag: &str) -> PathBuf {
    let out = tmpdir(tag);
    let day = out.join("2026/09/30");
    std::fs::create_dir_all(&day).unwrap();
    std::fs::write(day.join("feed-20260930-12.tsv.zst"), data_frame(100..=104)).unwrap();
    std::fs::write(out.join("last_seq.txt"), "90").unwrap();
    out
}

fn gaps(out: &Path) -> String {
    std::fs::read_to_string(out.join("gaps.tsv")).unwrap_or_default()
}

fn recorded_seqs(out: &Path) -> Vec<u64> {
    all_lines(out)
        .iter()
        .map(|l| l.split('\t').nth(1).unwrap().parse::<u64>().unwrap())
        .filter(|&s| s != 0)
        .collect()
}

/// Mock backlog + live stream: `backlog` back to back with
/// `header.timestamp` 60 s old, then `live` with the current timestamp and
/// 120 ms pauses (live feed ~113 ms per block, see chain-facts.md).
async fn send_burst_then_live(ws: &mut WebSocket<TcpStream>, backlog: &[u64], live: &[u64]) {
    for &seq in backlog {
        ws.send(Frame::text(envelope_ts(seq, unix_s() - 60)))
            .await
            .unwrap();
    }
    for &seq in live {
        tokio::time::sleep(Duration::from_millis(120)).await;
        ws.send(Frame::text(envelope_ts(seq, unix_s())))
            .await
            .unwrap();
    }
}

/// SIGTERM, answer the client's Close, wait for exit code 0.
async fn stop_recorder(child: &mut Child, ws: &mut WebSocket<TcpStream>, out: &Path) {
    sigterm(child);
    loop {
        match tokio::time::timeout(Duration::from_secs(5), ws.next_frame()).await {
            Ok(Ok(f)) if f.opcode() == OpCode::Close => break,
            Ok(Ok(_)) => continue,
            _ => break,
        }
    }
    ws.close().await.ok();
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("recorder did not exit")
        .unwrap();
    assert_eq!(status.code(), Some(0), "log:\n{}", read_log(out));
}

fn row<'a>(conns: &'a str, event: &str) -> &'a str {
    conns
        .lines()
        .find(|l| l.split('\t').nth(2) == Some(event))
        .unwrap_or_else(|| panic!("no {event} row in:\n{conns}"))
}

/// Item 1 + acceptance: data up to 104 -> the handshake carries
/// `Arbitrum-Feed-Client-Version: 2` and `Arbitrum-Requested-Sequence-Number:
/// 105` (from the data, not from the stale last_seq.txt); a mock that honours
/// it sends 105.. -> no gap, gaps.tsv stays empty; connections.tsv has the
/// requested seq and the burst statistics.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn header_requests_last_seq_plus_one_and_backlog_closes_gap() {
    let out = seeded_out("resume-ok");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    let mut child = spawn_recorder(&url, &out, &[]);
    let (mut ws, req) = tokio::time::timeout(Duration::from_secs(10), accept_ws_req(&listener))
        .await
        .expect("recorder did not connect to the mock");
    assert_eq!(
        header(&req, "Arbitrum-Feed-Client-Version").as_deref(),
        Some("2"),
        "{req}"
    );
    assert_eq!(
        header(&req, "Arbitrum-Requested-Sequence-Number").as_deref(),
        Some("105"),
        "{req}"
    );
    // A server that honours the header starts exactly at 105.
    send_burst_then_live(&mut ws, &[105, 106, 107, 108, 109, 110], &[111, 112]).await;
    wait_until("backlog event", Duration::from_secs(5), || {
        connections(&out).contains("\tbacklog\t")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    stop_recorder(&mut child, &mut ws, &out).await;

    let conns = connections(&out);
    eprintln!("connections.tsv:\n{conns}");
    assert!(row(&conns, "connected").ends_with(&format!("{url} requested=105 mode=header")));
    let b = row(&conns, "backlog");
    assert!(b.contains("\tbacklog\tdone\t"), "{b}");
    assert!(
        b.contains("requested=105 last_seq_before=104 first_seq=105 first_minus_requested=0 first_lag_ms=6"),
        "{b}"
    );
    assert!(
        b.contains("backlog_blocks=6 backlog_end_seq=110 live_seq=111"),
        "{b}"
    );
    assert!(b.contains("stale_frames=0 complete=true"), "{b}");
    assert_eq!(gaps(&out), "", "gaps.tsv must stay empty");
    assert_eq!(recorded_seqs(&out), (100..=112).collect::<Vec<_>>());
    assert_eq!(
        std::fs::read_to_string(out.join("last_seq.txt")).unwrap(),
        "112"
    );
    std::fs::remove_dir_all(&out).ok();
    std::fs::remove_file(out.with_extension("log")).ok();
}

/// Acceptance: the requested seq is older than the mock's backlog, so the
/// mock starts later (120) -> exactly one gaps.tsv row 105..119; frames older
/// than last_seq that a server might replay are skipped, not recorded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backlog_after_requested_leaves_exactly_one_gap() {
    let out = seeded_out("resume-gap");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    let mut child = spawn_recorder(&url, &out, &[]);
    let (mut ws, req) = tokio::time::timeout(Duration::from_secs(10), accept_ws_req(&listener))
        .await
        .expect("recorder did not connect to the mock");
    assert_eq!(
        header(&req, "Arbitrum-Requested-Sequence-Number").as_deref(),
        Some("105"),
        "{req}"
    );
    send_burst_then_live(&mut ws, &[120, 121, 122, 123], &[124, 125]).await;
    wait_until("backlog event", Duration::from_secs(5), || {
        connections(&out).contains("\tbacklog\t")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    stop_recorder(&mut child, &mut ws, &out).await;

    let conns = connections(&out);
    eprintln!("connections.tsv:\n{conns}");
    let b = row(&conns, "backlog");
    assert!(
        b.contains("requested=105 last_seq_before=104 first_seq=120 first_minus_requested=15")
            && b.contains("backlog_blocks=4 backlog_end_seq=123 live_seq=124"),
        "{b}"
    );
    let g = gaps(&out);
    let rows: Vec<&str> = g.lines().collect();
    assert_eq!(rows.len(), 1, "gaps.tsv:\n{g}");
    let c: Vec<&str> = rows[0].split('\t').collect();
    assert_eq!((c[0], c[1]), ("105", "119"), "gaps.tsv:\n{g}");
    let mut want: Vec<u64> = (100..=104).collect();
    want.extend(120..=125);
    assert_eq!(recorded_seqs(&out), want);
    std::fs::remove_dir_all(&out).ok();
    std::fs::remove_file(out.with_extension("log")).ok();
}

/// Item 1: no data -> no resume headers at all (same request as before 009).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_header_without_data() {
    let out = tmpdir("resume-nodata");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    let mut child = spawn_recorder(&url, &out, &[]);
    let (mut ws, req) = tokio::time::timeout(Duration::from_secs(10), accept_ws_req(&listener))
        .await
        .expect("recorder did not connect to the mock");
    assert_eq!(
        header(&req, "Arbitrum-Requested-Sequence-Number"),
        None,
        "{req}"
    );
    assert_eq!(header(&req, "Arbitrum-Feed-Client-Version"), None, "{req}");
    send_burst_then_live(&mut ws, &[500, 501], &[502]).await;
    wait_until("backlog event", Duration::from_secs(5), || {
        connections(&out).contains("\tbacklog\t")
    })
    .await;
    stop_recorder(&mut child, &mut ws, &out).await;
    let conns = connections(&out);
    eprintln!("connections.tsv:\n{conns}");
    assert!(row(&conns, "connected").ends_with("requested=- mode=no_data"));
    assert!(row(&conns, "backlog").contains("requested=- last_seq_before=- first_seq=500"));
    assert_eq!(gaps(&out), "");
    std::fs::remove_dir_all(&out).ok();
    std::fs::remove_file(out.with_extension("log")).ok();
}

/// Item 1: `--no-requested-seq` -> no resume headers even with data; the
/// stream from the tip leaves the usual gap row.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_requested_seq_flag_disables_header() {
    let out = seeded_out("resume-off");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    let mut child = spawn_recorder(&url, &out, &["--no-requested-seq"]);
    let (mut ws, req) = tokio::time::timeout(Duration::from_secs(10), accept_ws_req(&listener))
        .await
        .expect("recorder did not connect to the mock");
    assert_eq!(
        header(&req, "Arbitrum-Requested-Sequence-Number"),
        None,
        "{req}"
    );
    assert_eq!(header(&req, "Arbitrum-Feed-Client-Version"), None, "{req}");
    send_burst_then_live(&mut ws, &[200], &[201]).await;
    wait_until("backlog event", Duration::from_secs(5), || {
        connections(&out).contains("\tbacklog\t")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    stop_recorder(&mut child, &mut ws, &out).await;
    let conns = connections(&out);
    eprintln!("connections.tsv:\n{conns}");
    assert!(row(&conns, "connected").ends_with("requested=- mode=disabled"));
    let g = gaps(&out);
    assert_eq!(g.lines().count(), 1, "{g}");
    assert!(g.starts_with("105\t199\t"), "{g}");
    std::fs::remove_dir_all(&out).ok();
    std::fs::remove_file(out.with_extension("log")).ok();
}

/// Item 6: the mock goes silent for longer than --idle-timeout-secs -> the
/// recorder sends Close 1000 ("idle timeout") instead of dropping TCP, logs
/// `client_close` and `disconnected idle_timeout`, then reconnects after the
/// pause and asks for last_seq + 1 from memory (the first connection had no
/// data, so no header there).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_timeout_sends_close_then_resumes_from_memory() {
    let out = tmpdir("idle");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    let mut child = spawn_recorder(&url, &out, &["--idle-timeout-secs", "1"]);
    let (mut ws, req) = tokio::time::timeout(Duration::from_secs(10), accept_ws_req(&listener))
        .await
        .expect("recorder did not connect to the mock");
    assert_eq!(
        header(&req, "Arbitrum-Requested-Sequence-Number"),
        None,
        "{req}"
    );
    ws.send(Frame::text(envelope(300))).await.unwrap();
    ws.send(Frame::text(envelope(301))).await.unwrap();
    let t_last = Instant::now();
    // Silence. The client must send Close 1000 after ~1 s.
    let (code, reason, after) = loop {
        match tokio::time::timeout(Duration::from_secs(5), ws.next_frame()).await {
            Err(_) => panic!("no Close from the client within 5 s of silence"),
            Ok(Err(e)) => panic!("stream ended without a Close frame: {e}"),
            Ok(Ok(f)) if f.opcode() == OpCode::Close => {
                break (
                    f.close_code().map(u16::from),
                    f.close_reason().ok().flatten().unwrap_or("").to_string(),
                    t_last.elapsed(),
                )
            }
            Ok(Ok(_)) => continue,
        }
    };
    assert_eq!(code, Some(1000));
    assert_eq!(reason, "idle timeout");
    assert!(
        after >= Duration::from_millis(900) && after < Duration::from_secs(3),
        "Close after {after:?}"
    );
    ws.close().await.ok();
    drop(ws);

    // Short session -> transient pause ~4-6 s, then a second connection that
    // asks for 302 (last_seq of this process + 1).
    let (mut ws2, req2) = tokio::time::timeout(Duration::from_secs(12), accept_ws_req(&listener))
        .await
        .expect("recorder did not reconnect");
    assert_eq!(
        header(&req2, "Arbitrum-Feed-Client-Version").as_deref(),
        Some("2"),
        "{req2}"
    );
    assert_eq!(
        header(&req2, "Arbitrum-Requested-Sequence-Number").as_deref(),
        Some("302"),
        "{req2}"
    );
    send_burst_then_live(&mut ws2, &[302, 303], &[304]).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    stop_recorder(&mut child, &mut ws2, &out).await;

    let conns = connections(&out);
    eprintln!("connections.tsv:\n{conns}");
    let cc = row(&conns, "client_close");
    assert!(cc.contains("\tclient_close\tserver_replied\t"), "{cc}");
    assert!(cc.contains("\"idle timeout\""), "{cc}");
    assert!(conns.contains("\tdisconnected\tidle_timeout\t"), "{conns}");
    // First burst never ended in a pause: logged at the end of the session.
    assert!(conns.contains("\tbacklog\tsession_ended\t"), "{conns}");
    let connected: Vec<&str> = conns
        .lines()
        .filter(|l| l.split('\t').nth(2) == Some("connected"))
        .collect();
    assert_eq!(connected.len(), 2, "{conns}");
    assert!(connected[0].ends_with("requested=- mode=no_data"));
    assert!(connected[1].ends_with("requested=302 mode=header"));
    assert_eq!(gaps(&out), "");
    assert_eq!(recorded_seqs(&out), (300..=304).collect::<Vec<_>>());
    std::fs::remove_dir_all(&out).ok();
    std::fs::remove_file(out.with_extension("log")).ok();
}

// ------------------------------------------------------------- task 012 ---

const CONN_HEADER: &str = "# ts_utc\tts_unix_ns\tevent\treason\thttp_status\tretry_after\tpause_s\tsession_s\tenvelopes\tstrikes\tdetail";

/// Start the recorder on a prepared out-dir, wait for its `startup_wait`
/// row, check that nobody connects, SIGTERM it (exit 0) and return the row.
async fn startup_wait_row(out: &Path) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    let mut child = spawn_recorder(&url, out, &[]);
    wait_until("startup_wait event", Duration::from_secs(10), || {
        connections(out).contains("\tstartup_wait\t")
    })
    .await;
    let accepted = tokio::time::timeout(Duration::from_millis(500), listener.accept()).await;
    assert!(accepted.is_err(), "recorder connected during the wait");
    sigterm(&child);
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("SIGTERM did not interrupt the wait")
        .unwrap();
    assert_eq!(status.code(), Some(0), "log:\n{}", read_log(out));
    let conns = connections(out);
    eprintln!("connections.tsv:\n{conns}");
    row(&conns, "startup_wait").to_string()
}

fn pause_of(row: &str) -> f64 {
    row.split('\t').nth(6).unwrap().parse().unwrap()
}

/// Item 1, case "clean end": a 10-minute session (connected 630 s ago)
/// that ended with SIGTERM 30 s ago. Before 012 the recorder would connect
/// at once (last `connected` > 120 s ago); now it waits ~90 s.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn min_connect_interval_counts_from_end_of_session() {
    let out = tmpdir("end-log");
    let now = now_ns();
    let s = 1_000_000_000u128;
    std::fs::write(
        out.join("connections.tsv"),
        format!(
            "{CONN_HEADER}\n\
             x\t{}\tconnected\t-\t101\t-\t-\t-\t-\t0\tws://127.0.0.1:9/ requested=- mode=no_data\n\
             x\t{}\tbacklog\tdone\t101\t-\t-\t0.300\t4\t-\trequested=-\n\
             x\t{}\tshutdown\tSIGTERM\t-\t-\t-\t-\t-\t-\t-\n\
             x\t{}\tclient_close\tserver_replied\t101\t-\t-\t600.000\t6000\t-\tsent close 1000\n",
            now - 630 * s,
            now - 629 * s,
            now - 31 * s,
            now - 30 * s
        ),
    )
    .unwrap();
    let r = startup_wait_row(&out).await;
    assert!(r.contains("\tstartup_wait\tmin_connect_interval\t"), "{r}");
    assert!((88.0..=90.5).contains(&pause_of(&r)), "{r}");
    assert!(r.contains("(end=log_row)"), "{r}");
    std::fs::remove_dir_all(&out).ok();
    std::fs::remove_file(out.with_extension("log")).ok();
}

/// Item 1, case "kill -9": nothing after the last `connected` (600 s ago);
/// the newest hourly file was last written 40 s ago -> wait ~80 s from its
/// mtime.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn min_connect_interval_after_kill9_uses_data_mtime() {
    let out = seeded_out("end-mtime");
    let now = now_ns();
    let s = 1_000_000_000u128;
    std::fs::write(
        out.join("connections.tsv"),
        format!(
            "{CONN_HEADER}\n\
             x\t{}\tconnected\t-\t101\t-\t-\t-\t-\t0\tws://127.0.0.1:9/ requested=- mode=no_data\n",
            now - 600 * s
        ),
    )
    .unwrap();
    let f = out.join("2026/09/30/feed-20260930-12.tsv.zst");
    std::fs::File::options()
        .write(true)
        .open(&f)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(40))
        .unwrap();
    let r = startup_wait_row(&out).await;
    assert!(r.contains("\tstartup_wait\tmin_connect_interval\t"), "{r}");
    assert!((78.0..=80.5).contains(&pause_of(&r)), "{r}");
    assert!(r.contains("(end=data_mtime)"), "{r}");
    std::fs::remove_dir_all(&out).ok();
    std::fs::remove_file(out.with_extension("log")).ok();
}

/// Item 1, case "pending pause": a 403 with a 900 s pause 100 s ago, then
/// SIGTERM 99 s ago -> the pause remainder (~800 s) beats the interval
/// from the end (~21 s).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_pause_beats_interval_from_end() {
    let out = tmpdir("end-pause");
    let now = now_ns();
    let s = 1_000_000_000u128;
    std::fs::write(
        out.join("connections.tsv"),
        format!(
            "{CONN_HEADER}\n\
             x\t{}\tconnected\t-\t101\t-\t-\t-\t-\t0\tws://127.0.0.1:9/\n\
             x\t{}\tdisconnected\tforbidden\t403\t-\t900.000\t0.000\t0\t1\tupgrade\n\
             x\t{}\tshutdown\tSIGTERM\t-\t-\t-\t-\t-\t-\t-\n",
            now - 400 * s,
            now - 100 * s,
            now - 99 * s
        ),
    )
    .unwrap();
    let r = startup_wait_row(&out).await;
    assert!(r.contains("\tstartup_wait\tpending_pause\t"), "{r}");
    assert!((798.0..=800.5).contains(&pause_of(&r)), "{r}");
    std::fs::remove_dir_all(&out).ok();
    std::fs::remove_file(out.with_extension("log")).ok();
}

/// Item 2 (remark Н1 of the 009 audit): the writer fails (here: the year
/// directory of the hourly path is a regular file, so creating it fails like
/// a full disk would) -> the recorder sends Close 1000 "recorder writer
/// error", waits for the reply, logs `shutdown writer_error` and
/// `client_close`, and exits with code 2. Before 012 it exited at once
/// without a Close.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writer_error_sends_close_then_exits_2() {
    let out = tmpdir("writer-err");
    let year: i32 = chrono_year_now();
    for y in [year, year + 1] {
        std::fs::write(out.join(y.to_string()), b"not a directory").unwrap();
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    let mut child = spawn_recorder(&url, &out, &[]);
    let mut ws = tokio::time::timeout(Duration::from_secs(10), accept_ws(&listener))
        .await
        .expect("recorder did not connect to the mock");
    ws.send(Frame::text(envelope(500))).await.unwrap();
    let t0 = Instant::now();
    let (code, reason) = loop {
        match tokio::time::timeout(Duration::from_secs(5), ws.next_frame()).await {
            Err(_) => panic!("no Close within 5 s of the write error"),
            Ok(Err(e)) => panic!("stream ended without a Close frame: {e}"),
            Ok(Ok(f)) if f.opcode() == OpCode::Close => {
                break (
                    f.close_code().map(u16::from),
                    f.close_reason().ok().flatten().unwrap_or("").to_string(),
                )
            }
            Ok(Ok(_)) => continue,
        }
    };
    assert_eq!(code, Some(1000));
    assert_eq!(reason, "recorder writer error");
    ws.close().await.ok();
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("recorder did not exit")
        .unwrap();
    assert_eq!(status.code(), Some(2), "log:\n{}", read_log(&out));
    assert!(t0.elapsed() < Duration::from_secs(4), "{:?}", t0.elapsed());
    let conns = connections(&out);
    eprintln!("connections.tsv:\n{conns}");
    let sd = row(&conns, "shutdown");
    assert!(sd.contains("\tshutdown\twriter_error\t"), "{sd}");
    let cc = row(&conns, "client_close");
    assert!(cc.contains("\tclient_close\tserver_replied\t"), "{cc}");
    assert!(cc.contains("\"recorder writer error\""), "{cc}");
    assert_eq!(conns.matches("\tconnected\t").count(), 1, "{conns}");
    assert!(!conns.contains("\tdisconnected\t"), "{conns}");
    std::fs::remove_dir_all(&out).ok();
    std::fs::remove_file(out.with_extension("log")).ok();
}

fn chrono_year_now() -> i32 {
    use chrono::Datelike;
    chrono::Utc::now().year()
}

/// Item 3 (З1-б of the 011 review): after two blocks the mock sends only
/// pings. With --block-idle-timeout-secs 2 the recorder sends Close 1000
/// "block idle timeout" ~2 s after the last block (pings keep the plain idle
/// timeout from firing), logs `disconnected block_idle` with the transient
/// rule, reconnects and asks for last_seq + 1.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn block_idle_closes_and_resumes() {
    let out = tmpdir("block-idle");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    let mut child = spawn_recorder(
        &url,
        &out,
        &["--block-idle-timeout-secs", "2", "--idle-timeout-secs", "1"],
    );
    let mut ws = tokio::time::timeout(Duration::from_secs(10), accept_ws(&listener))
        .await
        .expect("recorder did not connect to the mock");
    ws.send(Frame::text(envelope(300))).await.unwrap();
    ws.send(Frame::text(envelope(301))).await.unwrap();
    let t_last = Instant::now();
    // Pings every 300 ms (well inside the 1 s idle timeout), no blocks.
    let (code, reason, after) = loop {
        match tokio::time::timeout(Duration::from_millis(300), ws.next_frame()).await {
            Err(_) => {
                assert!(
                    t_last.elapsed() < Duration::from_secs(6),
                    "no Close within 6 s without blocks"
                );
                ws.send(Frame::ping(Vec::<u8>::new())).await.unwrap();
            }
            Ok(Err(e)) => panic!("stream ended without a Close frame: {e}"),
            Ok(Ok(f)) if f.opcode() == OpCode::Close => {
                break (
                    f.close_code().map(u16::from),
                    f.close_reason().ok().flatten().unwrap_or("").to_string(),
                    t_last.elapsed(),
                )
            }
            Ok(Ok(_)) => continue,
        }
    };
    assert_eq!(code, Some(1000));
    assert_eq!(reason, "block idle timeout");
    assert!(
        after >= Duration::from_millis(1900) && after < Duration::from_secs(3),
        "Close after {after:?}"
    );
    ws.close().await.ok();
    drop(ws);

    // Transient ladder, first step 5 s +-20 %.
    let t_close = Instant::now();
    let (mut ws2, req2) = tokio::time::timeout(Duration::from_secs(12), accept_ws_req(&listener))
        .await
        .expect("recorder did not reconnect");
    let gap = t_close.elapsed();
    assert!(
        gap >= Duration::from_millis(3500) && gap < Duration::from_secs(7),
        "reconnect after {gap:?}"
    );
    assert_eq!(
        header(&req2, "Arbitrum-Requested-Sequence-Number").as_deref(),
        Some("302"),
        "{req2}"
    );
    send_burst_then_live(&mut ws2, &[302, 303], &[304]).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    stop_recorder(&mut child, &mut ws2, &out).await;

    let conns = connections(&out);
    eprintln!("connections.tsv:\n{conns}");
    let cc = row(&conns, "client_close");
    assert!(cc.contains("\tclient_close\tserver_replied\t"), "{cc}");
    assert!(cc.contains("\"block idle timeout\""), "{cc}");
    let d = row(&conns, "disconnected");
    assert!(d.contains("\tdisconnected\tblock_idle\t101\t"), "{d}");
    assert!(
        d.contains("rule=transient no block (seq > 0) for 2s"),
        "{d}"
    );
    assert!(!conns.contains("\tidle_timeout\t"), "{conns}");
    assert!(
        conns.lines().all(|l| l.split('\t').count() == 11),
        "{conns}"
    );
    assert_eq!(gaps(&out), "");
    assert_eq!(recorded_seqs(&out), (300..=304).collect::<Vec<_>>());
    std::fs::remove_dir_all(&out).ok();
    std::fs::remove_file(out.with_extension("log")).ok();
}
