//! Phase-1 feed recorder (library part of the `recorder` binary; task 021).
//!
//! Stores every feed frame verbatim, one per line, in hourly zstd files:
//!   <out>/YYYY/MM/DD/feed-YYYYMMDD-HH.tsv.zst
//! Line format (tab-separated, unchanged):
//!   recv_unix_ns \t seq_first \t seq_last \t <raw envelope JSON>
//! Lines without sequence numbers (JSON envelopes without `messages`;
//! non-text frames and text that is not JSON, wrapped as
//! `{"recorderFrame":...}`) have `seq_first = seq_last = 0`. The format is
//! defined by `.claude/skills/hoodchain-mev/references/data-model.md`;
//! `feed_audit.py` deliberately does not reuse this code and checks the data
//! against that description.
//!
//! Start-up: repair torn zstd tails, add holes missing from gaps.tsv, wait
//! out a pending pause / the minimum connect interval, counted from the end
//! of the previous session (task 012 item 1). Every connection asks
//! the feed to resume at `last_seq + 1` (`Arbitrum-Requested-Sequence-Number`,
//! task 009; `last_seq` from memory, at start-up from the data), unless there
//! is no data yet or `--no-requested-seq` is given. Whenever the recorder ends
//! a connection itself (SIGINT/SIGTERM, idle timeout, no block for
//! `--block-idle-timeout-secs`, fatal writer error): WebSocket Close 1000,
//! wait <= 2 s for the reply, then drop TCP; on shutdown drain and commit.
//! A fatal writer error (disk full, fsync) exits with code 2 after the Close
//! (task 012 item 2).
//!
//! Side files in <out>:
//!   gaps.tsv         `from \t to \t recv_ns` of missing L2 blocks (for RPC backfill)
//!   last_seq.txt     highest seq that is fsynced to disk (atomic replace)
//!   connections.tsv  connect/backlog/disconnect/startup/shutdown events and chosen pauses
//!   _torn/           torn zstd tails cut off after a crash (kept for analysis)
//!
//! Deliberately NOT done here: decoding l2Msg, signature checks, anything
//! latency-critical. Raw first, decode later. Exactly one feed connection.
//!
//! Modules:
//! - [`app`]: start-up, reconnect loop, shutdown and exit codes;
//! - [`session`]: one feed connection (frame loop, client close);
//! - [`transport`]: TCP/TLS/WebSocket upgrade with the response head kept;
//! - [`connlog`]: the `connections.tsv` contract (events, columns, readers);
//! - [`layout`]: file names and the hourly directory layout;
//! - `writer` / `recovery`: durable hourly writer and crash recovery;
//! - `rawline` / `seqtrack`: the one raw-line parser and the one
//!   last_seq / stale / gap rule (writer, recovery and network side);
//! - `route`, `resume`, `backoff`: frame -> line, resume statistics,
//!   reconnect pauses (pure logic).
//!
//! The library exists so that the binary, the tests and `examples/feed_probe.rs`
//! share one implementation; it is not a stable API. Public: [`app`] (the
//! binary), [`transport`] (the probe), [`connlog`] (the journal contract, read
//! by the tests and the probe), [`list_feed_files`] and [`now_ns`].

pub mod app;
mod backoff;
pub mod connlog;
mod layout;
mod rawline;
mod recovery;
mod resume;
mod route;
mod seqtrack;
mod session;
pub mod transport;
mod writer;

pub use layout::list_feed_files;

use std::time::{SystemTime, UNIX_EPOCH};

/// Wall-clock time as unix nanoseconds (0 if the clock is before 1970).
pub fn now_ns() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos()
}
