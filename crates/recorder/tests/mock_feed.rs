//! End-to-end tests of the recorder binary against a local mock feed on
//! 127.0.0.1 (task 008, items 2 and 3). No connection to the real feed and no
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
    WebSocket::from_stream(tcp, Role::Server, Options::default()).unwrap()
}

// Shape of a real envelope captured on 2026-09-28 (block 74755960,
// l2Msg/signature shortened), see hood-core and route.rs tests.
fn envelope(seq: u64) -> String {
    format!(
        r#"{{"version":1,"messages":[{{"sequenceNumber":{seq},"message":{{"message":{{"header":{{"kind":3,"sender":"0xa4b000000000000000000073657175656e636572","blockNumber":26075606,"timestamp":1790594344,"requestId":null,"baseFeeL1":0}},"l2Msg":"AAAA"}},"delayedMessagesRead":328658}},"blockHash":"0x529d8dcb881a6f5ed00232db376cf449ad881aa2a7ac4f8eb0078ea40a17f0f4","signatureV2":"AA==","blockMetadata":null}}]}}"#
    )
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

/// Item 2: `connected` 30 s ago in connections.tsv -> the new process waits
/// the rest of the 120 s minimum interval (~90 s) before connecting; SIGTERM
/// interrupts the wait and the exit code is 0. The mock must see no attempt.
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
    assert!((88.0..=90.5).contains(&pause), "pause {pause}, row {row}");

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
