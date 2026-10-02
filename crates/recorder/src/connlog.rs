//! The connection log `<out>/connections.tsv` (task 002 items 4, 5; task 021
//! item 4: one place for its contract).
//!
//! 11 tab-separated columns ([`CONNECTIONS_HEADER`], [`col`]); the first run
//! of task 002 wrote 10 (no `strikes`). Readers besides the recorder itself
//! (pending pause, end of the previous session): `deploy/healthcheck.sh`
//! (by position: `$2` ts_unix_ns, `$3` event, `$4` reason, `$5` http_status,
//! `$7` pause_s, `$11` detail) and `feed_audit.py` (`connected` rows). Event
//! names ([`ConnEventKind::as_str`]), reasons and the column layout must not
//! change: renaming breaks those readers and old logs. New data goes into
//! `detail` (the last column) only. The golden test below pins the exact
//! rows.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hood_core::ranges::GapRow;
use tracing::warn;

use crate::backoff::{Rule, SessionEndSource, StartupWaitReason};
use crate::now_ns;
use crate::recovery::{GapsSkip, SkippedGapLine, TornRepair};
use crate::resume::{Backlog, ResumeMode};
use crate::session::ConnEnd;
use crate::transport::CloseOutcome;

/// File name inside `--out-dir`.
pub const CONNECTIONS_FILE: &str = "connections.tsv";

/// First line of the file.
pub const CONNECTIONS_HEADER: &str =
    "# ts_utc\tts_unix_ns\tevent\treason\thttp_status\tretry_after\tpause_s\tsession_s\tenvelopes\tstrikes\tdetail";

/// 0-based column indices (healthcheck's awk `$N` is `N - 1` here).
pub mod col {
    pub const TS_UTC: usize = 0;
    pub const TS_UNIX_NS: usize = 1;
    pub const EVENT: usize = 2;
    pub const REASON: usize = 3;
    pub const HTTP_STATUS: usize = 4;
    pub const RETRY_AFTER: usize = 5;
    pub const PAUSE_S: usize = 6;
    pub const SESSION_S: usize = 7;
    pub const ENVELOPES: usize = 8;
    pub const STRIKES: usize = 9;
    pub const DETAIL: usize = 10;
    /// Columns of a row written since task 002's second run.
    pub const COUNT: usize = 11;
}

/// Value of the `event` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnEventKind {
    Connected,
    Backlog,
    ClientClose,
    Disconnected,
    StartupWait,
    TornRepair,
    GapReconciled,
    /// Task 021: a line of gaps.tsv was skipped at start-up.
    GapsLineSkipped,
    Shutdown,
    WriterError,
}

impl ConnEventKind {
    /// Every event, in the order of the golden test.
    pub const ALL: [ConnEventKind; 10] = [
        ConnEventKind::Connected,
        ConnEventKind::Backlog,
        ConnEventKind::ClientClose,
        ConnEventKind::Disconnected,
        ConnEventKind::StartupWait,
        ConnEventKind::TornRepair,
        ConnEventKind::GapReconciled,
        ConnEventKind::GapsLineSkipped,
        ConnEventKind::Shutdown,
        ConnEventKind::WriterError,
    ];

    /// The exact string in the `event` column.
    pub const fn as_str(self) -> &'static str {
        match self {
            ConnEventKind::Connected => "connected",
            ConnEventKind::Backlog => "backlog",
            ConnEventKind::ClientClose => "client_close",
            ConnEventKind::Disconnected => "disconnected",
            ConnEventKind::StartupWait => "startup_wait",
            ConnEventKind::TornRepair => "torn_repair",
            ConnEventKind::GapReconciled => "gap_reconciled",
            ConnEventKind::GapsLineSkipped => "gaps_line_skipped",
            ConnEventKind::Shutdown => "shutdown",
            ConnEventKind::WriterError => "writer_error",
        }
    }
}

/// `reason` of `torn_repair` / `gap_reconciled`.
const STARTUP: &str = "startup";
/// `reason` of the `writer_error` event (failure in the final commit).
const FINAL_COMMIT: &str = "final_commit";
/// Longest gaps.tsv line quoted in a `gaps_line_skipped` row.
const SKIPPED_LINE_MAX: usize = 200;
/// At most this many `gaps_line_skipped` rows per start-up name a line; the
/// rest is one summary row (finding Б1 of the 021 data audit: hundreds of
/// rows per start bloated the journal).
pub const SKIPPED_ROWS_MAX: usize = 20;

/// One row of connections.tsv. `None` fields are written as `-`, an empty
/// `detail` too. Built by the constructors below, one per event.
#[derive(Debug, Clone, PartialEq)]
pub struct ConnEvent<'a> {
    pub(crate) event: ConnEventKind,
    pub(crate) reason: &'a str,
    pub(crate) http_status: Option<u16>,
    pub(crate) retry_after: Option<&'a str>,
    pub(crate) pause: Option<Duration>,
    pub(crate) session: Option<Duration>,
    pub(crate) envelopes: Option<u64>,
    pub(crate) strikes: Option<u32>,
    pub(crate) detail: String,
}

impl<'a> ConnEvent<'a> {
    fn new(event: ConnEventKind, reason: &'a str, detail: String) -> Self {
        Self {
            event,
            reason,
            http_status: None,
            retry_after: None,
            pause: None,
            session: None,
            envelopes: None,
            strikes: None,
            detail,
        }
    }

    /// Upgrade succeeded. `detail`: `<url> requested=<N|-> mode=<mode>` (task 009).
    pub fn connected(url: &str, requested: Option<u64>, mode: ResumeMode, strikes: u32) -> Self {
        let req = requested.map_or_else(|| "-".to_string(), |n| n.to_string());
        let detail = format!("{url} requested={req} mode={}", mode.as_str());
        Self { http_status: Some(101), strikes: Some(strikes), ..Self::new(ConnEventKind::Connected, "-", detail) }
    }

    /// Backlog statistics; `reason` is `done` or `session_ended` (task 009).
    pub fn backlog(b: &Backlog, reason: &'a str) -> Self {
        Self {
            http_status: Some(101),
            session: Some(b.live_after),
            envelopes: Some(b.blocks),
            ..Self::new(ConnEventKind::Backlog, reason, b.detail())
        }
    }

    /// Outcome of the recorder's own Close 1000.
    pub fn client_close(c: &CloseOutcome, session: Duration, envelopes: u64) -> Self {
        let detail = format!("sent close 1000, waited {} ms; {}", c.took.as_millis(), c.detail);
        Self {
            http_status: Some(101),
            session: Some(session),
            envelopes: Some(envelopes),
            ..Self::new(ConnEventKind::ClientClose, c.reply.as_str(), detail)
        }
    }

    /// Connection ended or failed, with the chosen pause. Read back at the
    /// next start ([`parse_pause_row`]).
    pub fn disconnected(end: &'a ConnEnd, pause: Duration, rule: Rule, strikes: u32) -> Self {
        Self {
            http_status: end.http_status,
            retry_after: end.retry_after_raw.as_deref(),
            pause: Some(pause),
            session: Some(end.session),
            envelopes: Some(end.envelopes),
            strikes: Some(strikes),
            ..Self::new(
                ConnEventKind::Disconnected,
                end.kind.as_str(),
                format!("rule={} {}", rule.as_str(), end.detail),
            )
        }
    }

    /// Wait before the first connect (task 008 item 2, task 012 item 1).
    /// `session_end` is needed for [`StartupWaitReason::MinConnectInterval`].
    pub fn startup_wait(
        why: StartupWaitReason,
        wait: Duration,
        strikes: u32,
        now_ns: u128,
        session_end: Option<(u128, SessionEndSource)>,
        min_interval: Duration,
    ) -> Self {
        let detail = match (why, session_end) {
            (StartupWaitReason::MinConnectInterval, Some((end, src))) => format!(
                "previous session ended {:.3}s ago (end={}), min interval {}s",
                now_ns.saturating_sub(end) as f64 / 1e9,
                src.as_str(),
                min_interval.as_secs()
            ),
            // `startup_wait` only picks the interval when there is an end.
            (StartupWaitReason::MinConnectInterval, None) => {
                format!("min interval {}s", min_interval.as_secs())
            }
            (StartupWaitReason::PendingPause, _) => format!("remaining pause from previous run, strikes={strikes}"),
        };
        Self {
            pause: Some(wait),
            strikes: Some(strikes),
            ..Self::new(ConnEventKind::StartupWait, why.as_str(), detail)
        }
    }

    /// A torn zstd tail was cut off at start-up.
    pub fn torn_repair(r: &TornRepair) -> Self {
        let detail =
            format!("{} kept={} torn={} saved={}", r.file.display(), r.kept_bytes, r.torn_bytes, r.saved_to.display());
        Self::new(ConnEventKind::TornRepair, STARTUP, detail)
    }

    /// A hole in the data was missing from gaps.tsv and got appended.
    pub fn gap_reconciled(g: &GapRow) -> Self {
        let detail = format!("{}..{} recv_ns={} missing from gaps.tsv, appended", g.range.from, g.range.to, g.recv_ns);
        Self::new(ConnEventKind::GapReconciled, STARTUP, detail)
    }

    /// A gaps.tsv line was skipped at start-up (task 021). `reason`:
    /// `broken` or `unterminated`.
    pub fn gaps_line_skipped(s: &SkippedGapLine) -> Self {
        let quoted: String = s.line.chars().take(SKIPPED_LINE_MAX).collect::<String>().escape_debug().to_string();
        let (reason, why) = match &s.why {
            GapsSkip::Broken(r) => ("broken", r.as_str()),
            GapsSkip::Unterminated => ("unterminated", "no trailing newline"),
        };
        let detail = format!("gaps.tsv line {}: {why}: \"{quoted}\"", s.line_no);
        Self::new(ConnEventKind::GapsLineSkipped, reason, detail)
    }

    /// Summary after [`SKIPPED_ROWS_MAX`] `gaps_line_skipped` rows:
    /// `reason` = `more`, `detail` = how many further lines were skipped.
    pub fn gaps_lines_skipped_more(more: usize, total: usize) -> Self {
        let detail = format!("gaps.tsv: {more} more skipped lines not listed ({total} skipped in total)");
        Self::new(ConnEventKind::GapsLineSkipped, "more", detail)
    }

    /// Process stops: `reason` is `SIGINT`/`SIGTERM` or `writer_error`
    /// (then `detail` is the error).
    pub fn shutdown(reason: &'a str, detail: &str) -> Self {
        Self::new(ConnEventKind::Shutdown, reason, detail.to_string())
    }

    /// The writer failed in its final commit after a signal (task 012).
    pub fn writer_error_final(detail: &str) -> Self {
        Self::new(ConnEventKind::WriterError, FINAL_COMMIT, detail.to_string())
    }
}

/// Rows for the gaps.tsv lines skipped at start-up: one per line for the
/// first [`SKIPPED_ROWS_MAX`], then one `more` row with the rest.
pub(crate) fn gaps_skipped_events(skipped: &[SkippedGapLine]) -> Vec<ConnEvent<'static>> {
    let mut out: Vec<ConnEvent<'static>> =
        skipped.iter().take(SKIPPED_ROWS_MAX).map(ConnEvent::gaps_line_skipped).collect();
    if skipped.len() > SKIPPED_ROWS_MAX {
        out.push(ConnEvent::gaps_lines_skipped_more(skipped.len() - SKIPPED_ROWS_MAX, skipped.len()));
    }
    out
}

fn dash<T: ToString>(v: Option<T>) -> String {
    v.map(|x| x.to_string()).unwrap_or_else(|| "-".into())
}

/// Tabs and newlines become spaces; empty becomes `-`.
fn clean(s: &str) -> String {
    let c = s.replace(['\t', '\n', '\r'], " ");
    if c.is_empty() {
        "-".to_string()
    } else {
        c
    }
}

/// The row for `e` written at unix ns `ns` (without `\n`).
pub fn format_row(ns: u128, e: &ConnEvent<'_>) -> String {
    let ts = chrono::DateTime::from_timestamp_nanos(ns as i64).format("%Y-%m-%dT%H:%M:%S%.3fZ");
    format!(
        "{ts}\t{ns}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        clean(e.event.as_str()),
        clean(e.reason),
        dash(e.http_status),
        e.retry_after.map(clean).unwrap_or_else(|| "-".into()),
        dash(e.pause.map(|p| format!("{:.3}", p.as_secs_f64()))),
        dash(e.session.map(|p| format!("{:.3}", p.as_secs_f64()))),
        dash(e.envelopes),
        dash(e.strikes),
        clean(&e.detail),
    )
}

/// Pause decided before the previous process exited (from connections.tsv).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PendingPause {
    /// Unix ns before which we must not reconnect.
    pub not_before_ns: u128,
    pub strikes: u32,
}

/// Append-only event log of connects/disconnects and chosen pauses. It also
/// carries the pause across restarts, so a restarted process (systemd
/// `Restart=always`) does not reconnect in the middle of a ban.
pub struct ConnLog {
    path: PathBuf,
}

impl ConnLog {
    /// The log in `out`.
    pub fn new(out: &Path) -> Self {
        Self { path: out.join(CONNECTIONS_FILE) }
    }

    /// Append one row (header first if the file is new). An error is only
    /// logged: the log must never stop the recording.
    pub fn event(&self, e: ConnEvent<'_>) {
        let row = format_row(now_ns(), &e);
        let res = (|| -> std::io::Result<()> {
            let new = !self.path.exists();
            let mut f = OpenOptions::new().create(true).append(true).open(&self.path)?;
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

    /// The last session as seen in the log (task 012, item 1).
    pub fn last_session(&self) -> Option<LogSession> {
        parse_last_session(&self.tail()?)
    }
}

/// The last `connected` row and the newest row of any type after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogSession {
    pub connected_ns: u128,
    /// Unix ns of the last row after `connected` (`backlog`, `client_close`,
    /// `disconnected`, `shutdown`, ...). None if `connected` is the last row,
    /// i.e. the process died without writing anything (kill -9, power loss).
    pub last_row_ns: Option<u128>,
}

/// `ts_unix_ns` and `event` of a data row (both layouts: they are the
/// second and third column in each); None for the header, comments and
/// torn lines.
fn row_ns_event(line: &str) -> Option<(u128, &str)> {
    if line.starts_with('#') {
        return None;
    }
    let mut c = line.split('\t');
    let ns: u128 = c.nth(col::TS_UNIX_NS)?.parse().ok()?;
    let event = c.next()?;
    (!event.is_empty()).then_some((ns, event))
}

/// The last session in a piece of the log.
pub fn parse_last_session(text: &str) -> Option<LogSession> {
    let mut out: Option<LogSession> = None;
    for (ns, event) in text.lines().filter_map(row_ns_event) {
        if event == ConnEventKind::Connected.as_str() {
            out = Some(LogSession { connected_ns: ns, last_row_ns: None });
        } else if let Some(s) = out.as_mut() {
            s.last_row_ns = Some(ns);
        }
    }
    out
}

/// Parse a `disconnected` row into the pending pause it defines. A pause
/// that is not a finite non-negative number (never written by the recorder)
/// counts as zero, the row and its strikes still count; before 021 `inf`
/// overflowed (a panic in debug, a wrapped value in release builds).
pub fn parse_pause_row(line: &str) -> Option<PendingPause> {
    let c: Vec<&str> = line.split('\t').collect();
    if c.len() < col::COUNT - 1 || c[col::EVENT] != ConnEventKind::Disconnected.as_str() {
        return None;
    }
    let ts: u128 = c[col::TS_UNIX_NS].parse().ok()?;
    let pause: f64 = c[col::PAUSE_S].parse().ok()?;
    let pause = if pause.is_finite() && pause >= 0.0 { pause } else { 0.0 };
    // In the 10-column layout of task 002's first run, column 9 is `detail`:
    // not a number, so 0 strikes.
    let strikes: u32 = c[col::STRIKES].parse().unwrap_or(0);
    Some(PendingPause { not_before_ns: ts.saturating_add((pause * 1e9) as u128), strikes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backoff::EndKind;
    use crate::transport::CloseReply;
    use hood_core::Range;

    /// Golden test (task 021 item 4): the exact rows of every event. Rows
    /// marked "real" are copied byte for byte from logs of the old binary
    /// (`data/feed-test-009/connections.tsv`, 2026-10-01, and
    /// `data/feed-test-002/connections.tsv`, 2026-09-30); the others follow
    /// the same format with the event's current detail. Changing any of
    /// these strings breaks `deploy/healthcheck.sh`, `feed_audit.py` or the
    /// recorder's own reading of old logs.
    #[test]
    fn golden_rows_of_every_event() {
        let ms = Duration::from_millis;
        let url = "wss://feed.mainnet.chain.robinhood.com";
        let close = |reason: &str, took: u64, reply: CloseReply| CloseOutcome {
            reply,
            took: ms(took),
            detail: format!("close 1000 \"{reason}\": close frame code=Some(1000) reason=\"{reason}\""),
        };
        let forbidden = ConnEnd {
            kind: EndKind::Forbidden,
            http_status: Some(403),
            retry_after_raw: Some("3600".into()),
            retry_after: Some(Duration::from_secs(3600)),
            session: Duration::ZERO,
            envelopes: 0,
            detail: "upgrade: Invalid status code: 403; HTTP/1.1 403 Forbidden".into(),
            client_close: None,
            backlog: None,
        };
        let backlog = Backlog {
            requested: Some(77_169_135),
            last_seq_before: Some(77_169_134),
            first_seq: Some(77_169_713),
            first_lag_ms: Some(64_210),
            blocks: 621,
            end_seq: Some(77_170_333),
            first_ns: Some(1),
            live_seq: Some(77_170_334),
            live_lag_ms: Some(812),
            live_after: ms(1_431),
            stale_frames: 0,
            done: true,
        };
        let torn = TornRepair {
            file: "data/feed-test-002/2026/09/30/feed-20260930-12.tsv.zst".into(),
            kept_bytes: 4_772_435,
            torn_bytes: 1_187_507,
            saved_to: "data/feed-test-002/_torn/feed-20260930-12.tsv.zst.at4772435.20260930T125731Z.torn".into(),
        };
        let now = 1_790_837_152_000_000_000u128;
        let cases: Vec<(u128, ConnEvent<'_>, &str)> = vec![
            // real (009)
            (1790837152293577000, ConnEvent::connected(url, Some(77169135), ResumeMode::Header, 0),
             "2026-10-01T06:45:52.293Z\t1790837152293577000\tconnected\t-\t101\t-\t-\t-\t-\t0\twss://feed.mainnet.chain.robinhood.com requested=77169135 mode=header"),
            // real (009)
            (1790836432383544000, ConnEvent::connected(url, None, ResumeMode::NoData, 0),
             "2026-10-01T06:33:52.383Z\t1790836432383544000\tconnected\t-\t101\t-\t-\t-\t-\t0\twss://feed.mainnet.chain.robinhood.com requested=- mode=no_data"),
            (1790837153724000000, ConnEvent::backlog(&backlog, "done"),
             "2026-10-01T06:45:53.724Z\t1790837153724000000\tbacklog\tdone\t101\t-\t-\t1.431\t621\t-\trequested=77169135 last_seq_before=77169134 first_seq=77169713 first_minus_requested=578 first_lag_ms=64210 backlog_blocks=621 backlog_end_seq=77170333 live_seq=77170334 live_lag_ms=812 live_after_ms=1431 stale_frames=0 complete=true"),
            // real (009)
            (1790837031336494000, ConnEvent::client_close(&close("recorder shutdown", 146, CloseReply::ServerReplied), Duration::from_millis(598_948), 5937),
             "2026-10-01T06:43:51.336Z\t1790837031336494000\tclient_close\tserver_replied\t101\t-\t-\t598.948\t5937\t-\tsent close 1000, waited 146 ms; close 1000 \"recorder shutdown\": close frame code=Some(1000) reason=\"recorder shutdown\""),
            (1790773051514522000, ConnEvent::disconnected(&forbidden, Duration::from_secs(3600), Rule::RetryAfter, 1),
             "2026-09-30T12:57:31.514Z\t1790773051514522000\tdisconnected\tforbidden\t403\t3600\t3600.000\t0.000\t0\t1\trule=retry_after upgrade: Invalid status code: 403; HTTP/1.1 403 Forbidden"),
            // real (002, 11-column run)
            (1790773477396237000, ConnEvent::startup_wait(StartupWaitReason::PendingPause, ms(3_174_118), 0, 0, None, Duration::from_secs(120)),
             "2026-09-30T13:04:37.396Z\t1790773477396237000\tstartup_wait\tpending_pause\t-\t-\t3174.118\t-\t-\t0\tremaining pause from previous run, strikes=0"),
            (now, ConnEvent::startup_wait(StartupWaitReason::MinConnectInterval, ms(118_500), 0, now, Some((now - 1_500_000_000, SessionEndSource::LogRow)), Duration::from_secs(120)),
             "2026-10-01T06:45:52.000Z\t1790837152000000000\tstartup_wait\tmin_connect_interval\t-\t-\t118.500\t-\t-\t0\tprevious session ended 1.500s ago (end=log_row), min interval 120s"),
            // real (002) detail; that run wrote 10 columns, now `strikes` is `-`
            (1790773051286991000, ConnEvent::torn_repair(&torn),
             "2026-09-30T12:57:31.286Z\t1790773051286991000\ttorn_repair\tstartup\t-\t-\t-\t-\t-\t-\tdata/feed-test-002/2026/09/30/feed-20260930-12.tsv.zst kept=4772435 torn=1187507 saved=data/feed-test-002/_torn/feed-20260930-12.tsv.zst.at4772435.20260930T125731Z.torn"),
            (now, ConnEvent::gap_reconciled(&GapRow { range: Range { from: 104, to: 106 }, recv_ns: 6 }),
             "2026-10-01T06:45:52.000Z\t1790837152000000000\tgap_reconciled\tstartup\t-\t-\t-\t-\t-\t-\t104..106 recv_ns=6 missing from gaps.tsv, appended"),
            (now, ConnEvent::gaps_line_skipped(&SkippedGapLine { line_no: 2, line: "broken\tx".into(), why: GapsSkip::Broken("column to is not a number".into()) }),
             "2026-10-01T06:45:52.000Z\t1790837152000000000\tgaps_line_skipped\tbroken\t-\t-\t-\t-\t-\t-\tgaps.tsv line 2: column to is not a number: \"broken\\tx\""),
            (now, ConnEvent::gaps_line_skipped(&SkippedGapLine { line_no: 6, line: "200\t2".into(), why: GapsSkip::Unterminated }),
             "2026-10-01T06:45:52.000Z\t1790837152000000000\tgaps_line_skipped\tunterminated\t-\t-\t-\t-\t-\t-\tgaps.tsv line 6: no trailing newline: \"200\\t2\""),
            // real (009)
            (1790837031189572000, ConnEvent::shutdown("SIGTERM", ""),
             "2026-10-01T06:43:51.189Z\t1790837031189572000\tshutdown\tSIGTERM\t-\t-\t-\t-\t-\t-\t-"),
            (now, ConnEvent::shutdown("writer_error", "fsync /x/feed.tsv.zst: No space left on device (os error 28)"),
             "2026-10-01T06:45:52.000Z\t1790837152000000000\tshutdown\twriter_error\t-\t-\t-\t-\t-\t-\tfsync /x/feed.tsv.zst: No space left on device (os error 28)"),
            (now, ConnEvent::writer_error_final("fsync /x: Input/output error\n(os error 5)"),
             "2026-10-01T06:45:52.000Z\t1790837152000000000\twriter_error\tfinal_commit\t-\t-\t-\t-\t-\t-\tfsync /x: Input/output error (os error 5)"),
        ];
        let mut seen = Vec::new();
        for (ns, e, want) in &cases {
            let row = format_row(*ns, e);
            assert_eq!(row, *want);
            let c: Vec<&str> = row.split('\t').collect();
            assert_eq!(c.len(), col::COUNT, "{row}");
            assert_eq!(c[col::EVENT], e.event.as_str());
            assert_eq!(c[col::TS_UNIX_NS], ns.to_string());
            seen.push(e.event);
        }
        // Every event kind is covered, and the names are exactly these.
        assert!(ConnEventKind::ALL.iter().all(|k| seen.contains(k)));
        let names: Vec<&str> = ConnEventKind::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(
            names,
            [
                "connected",
                "backlog",
                "client_close",
                "disconnected",
                "startup_wait",
                "torn_repair",
                "gap_reconciled",
                "gaps_line_skipped",
                "shutdown",
                "writer_error"
            ]
        );
        assert_eq!(
            CONNECTIONS_HEADER.trim_start_matches("# ").split('\t').collect::<Vec<_>>(),
            [
                "ts_utc",
                "ts_unix_ns",
                "event",
                "reason",
                "http_status",
                "retry_after",
                "pause_s",
                "session_s",
                "envelopes",
                "strikes",
                "detail"
            ]
        );
        // Positions healthcheck.sh reads ($N = col + 1).
        assert_eq!(
            (col::TS_UNIX_NS, col::EVENT, col::REASON, col::HTTP_STATUS, col::PAUSE_S, col::DETAIL),
            (1, 2, 3, 4, 6, 10)
        );
        assert_eq!((col::TS_UTC, col::RETRY_AFTER, col::SESSION_S, col::ENVELOPES, col::STRIKES), (0, 5, 7, 8, 9));
    }

    #[test]
    fn skipped_rows_are_capped_with_one_summary() {
        let line = |n: usize| SkippedGapLine { line_no: n, line: "x".into(), why: GapsSkip::Broken("bad".into()) };
        let few: Vec<SkippedGapLine> = (1..=SKIPPED_ROWS_MAX).map(line).collect();
        assert_eq!(gaps_skipped_events(&few).len(), SKIPPED_ROWS_MAX);
        let many: Vec<SkippedGapLine> = (1..=700).map(line).collect();
        let ev = gaps_skipped_events(&many);
        assert_eq!(ev.len(), SKIPPED_ROWS_MAX + 1);
        assert!(ev.iter().all(|e| e.event == ConnEventKind::GapsLineSkipped));
        let last = format_row(1_790_837_152_000_000_000, ev.last().unwrap());
        assert_eq!(
            last,
            "2026-10-01T06:45:52.000Z\t1790837152000000000\tgaps_line_skipped\tmore\t-\t-\t-\t-\t-\t-\tgaps.tsv: 680 more skipped lines not listed (700 skipped in total)"
        );
        assert!(gaps_skipped_events(&[]).is_empty());
    }

    /// `ConnEventKind::ALL` lists every variant: adding a variant without
    /// adding it to ALL breaks this exhaustive match's count.
    #[test]
    fn all_lists_every_event_kind() {
        fn index(k: ConnEventKind) -> usize {
            match k {
                ConnEventKind::Connected => 0,
                ConnEventKind::Backlog => 1,
                ConnEventKind::ClientClose => 2,
                ConnEventKind::Disconnected => 3,
                ConnEventKind::StartupWait => 4,
                ConnEventKind::TornRepair => 5,
                ConnEventKind::GapReconciled => 6,
                ConnEventKind::GapsLineSkipped => 7,
                ConnEventKind::Shutdown => 8,
                ConnEventKind::WriterError => 9,
            }
        }
        // Every match arm above has an index below 10 = ALL.len(), and ALL
        // holds each index exactly once (in order), so ALL is complete.
        let got: Vec<usize> = ConnEventKind::ALL.iter().map(|&k| index(k)).collect();
        assert_eq!(got, (0..ConnEventKind::ALL.len()).collect::<Vec<_>>());
    }

    #[test]
    fn pause_survives_restart_via_connections_log() {
        let dir = std::env::temp_dir().join(format!("recorder-connlog-{}-{}", std::process::id(), now_ns()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = ConnLog::new(&dir);
        assert_eq!(log.last_pause(), None);
        log.event(ConnEvent::connected("ws://x", None, ResumeMode::NoData, 0));
        assert_eq!(log.last_pause(), None);
        let before = now_ns();
        let end = ConnEnd {
            kind: EndKind::Forbidden,
            http_status: Some(403),
            retry_after_raw: Some("3600".into()),
            retry_after: None,
            session: Duration::ZERO,
            envelopes: 0,
            detail: "upgrade: Invalid status code: 403;\tHTTP/1.1 403 Forbidden".into(),
            client_close: None,
            backlog: None,
        };
        log.event(ConnEvent::disconnected(&end, Duration::from_secs(3600), Rule::RetryAfter, 1));
        log.event(ConnEvent::shutdown("SIGTERM", ""));
        let p = log.last_pause().unwrap();
        assert_eq!(p.strikes, 1);
        let wait = p.not_before_ns - before;
        assert!((3_599_000_000_000..=3_601_000_000_000).contains(&wait), "{wait}");
        let text = std::fs::read_to_string(dir.join(CONNECTIONS_FILE)).unwrap();
        assert!(text.starts_with(CONNECTIONS_HEADER));
        assert!(text.lines().all(|l| l.split('\t').count() == col::COUNT), "{text}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Both layouts (Р5 of the review: replaces the test of the removed
    /// `parse_connected_row`). Rows copied from
    /// data/feed-test-002/connections.tsv (task 002): the first run wrote 10
    /// columns, later runs 11 (with `strikes`).
    #[test]
    fn last_session_and_pause_from_both_log_layouts() {
        let old = "2026-09-30T12:52:21.202Z\t1790772741202619000\tconnected\t-\t101\t-\t-\t-\t-\twss://feed.mainnet.chain.robinhood.com";
        let old_disc = "2026-09-30T12:57:31.514Z\t1790773051514522000\tdisconnected\tforbidden\t403\t3600\t3600.000\t0.000\t0\trule=retry_after strikes=1 upgrade: Invalid status code: 403; HTTP/1.1 403 Forbidden";
        let new = "2026-09-30T13:57:32.226Z\t1790776652226292000\tconnected\t-\t101\t-\t-\t-\t-\t0\twss://feed.mainnet.chain.robinhood.com";
        let shut = "2026-09-30T14:03:00.329Z\t1790776980329123000\tshutdown\tSIGTERM\t-\t-\t-\t-\t-\t-\t-";
        let s = parse_last_session(&format!("{CONNECTIONS_HEADER}\n{old}\n")).unwrap();
        assert_eq!(s, LogSession { connected_ns: 1790772741202619000, last_row_ns: None });
        let s = parse_last_session(&format!("{CONNECTIONS_HEADER}\n{old}\n{old_disc}\n{new}\n{shut}\n")).unwrap();
        assert_eq!(s, LogSession { connected_ns: 1790776652226292000, last_row_ns: Some(1790776980329123000) });
        assert_eq!(parse_last_session(CONNECTIONS_HEADER), None);
        // 10-column `disconnected`: pause read, strikes default to 0.
        let p = parse_pause_row(old_disc).unwrap();
        assert_eq!(p, PendingPause { not_before_ns: 1790773051514522000 + 3_600_000_000_000, strikes: 0 });
        assert_eq!(parse_pause_row(new), None);
    }

    /// Р2 of the review: a pause of `inf`, `NaN` or below zero counts as
    /// zero (no overflow); a non-number gives no pause.
    #[test]
    fn bad_pause_values_do_not_overflow() {
        let row = |p: &str| format!("x\t100\tdisconnected\tforbidden\t403\t-\t{p}\t0.000\t0\t1\trule=x");
        assert_eq!(parse_pause_row(&row("1.5")).unwrap().not_before_ns, 1_500_000_100);
        for bad in ["inf", "NaN", "-1.0"] {
            assert_eq!(parse_pause_row(&row(bad)), Some(PendingPause { not_before_ns: 100, strikes: 1 }), "{bad}");
        }
        assert_eq!(parse_pause_row(&row("x")), None);
        assert_eq!(parse_pause_row(&row("1e300")).unwrap().not_before_ns, u128::MAX);
    }
}
