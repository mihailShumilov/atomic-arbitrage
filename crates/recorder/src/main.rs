//! Phase-1 feed recorder: command line, logging, exit code. Everything else
//! is in the library (`src/lib.rs`, module docs there).

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use hood_core::FEED_URL;
use recorder::app::{self, Config, Outcome, EXIT_WRITER_ERROR};

#[derive(Parser, Debug)]
#[command(about = "Record the Robinhood Chain sequencer feed to hourly zstd files")]
struct Args {
    #[arg(long, env = "FEED_URL", default_value = FEED_URL)]
    url: String,
    #[arg(long, env = "RECORDER_OUT_DIR", default_value = "data/feed")]
    out_dir: PathBuf,
    /// Reconnect if no frame arrives for this many seconds
    /// (blocks are ~100 ms, silence means a broken stream).
    #[arg(long, default_value_t = 10)]
    idle_timeout_secs: u64,
    #[arg(long, default_value_t = 3)]
    zstd_level: i32,
    /// Close the zstd frame and fsync at least this often. Bounds the data
    /// lost on kill -9 / power loss.
    #[arg(long, default_value_t = 60)]
    frame_secs: u64,
    /// Do not wait out a reconnect pause (e.g. Retry-After of a ban) that a
    /// previous run recorded in connections.tsv, and skip the
    /// --min-connect-interval-secs wait. Use only if you know the ban is
    /// over; the default is to honour both.
    #[arg(long)]
    ignore_pending_pause: bool,
    /// Minimum time between the end of the previous session and the first
    /// connect after a (re)start. End = last row of connections.tsv after the
    /// last `connected`, or the mtime of the newest hourly data file if that
    /// is later (kill -9 leaves no row). The 403 + Retry-After: 3600 ban of
    /// 2026-09-30 followed a reconnect 11 s after kill -9; the cause is
    /// unknown, so restarts are spaced out.
    #[arg(long, default_value_t = 120)]
    min_connect_interval_secs: u64,
    /// Reconnect (Close 1000, then resume at last_seq + 1) if no frame with a
    /// block (seq > 0) arrives for this many seconds, even if pings and
    /// confirmations keep the stream alive. Blocks are ~100 ms apart. 0
    /// disables.
    #[arg(long, default_value_t = 30)]
    block_idle_timeout_secs: u64,
    /// Do not send `Arbitrum-Feed-Client-Version` /
    /// `Arbitrum-Requested-Sequence-Number` on connect: the stream then
    /// starts at the tip, as before task 009.
    #[arg(long)]
    no_requested_seq: bool,
}

impl From<Args> for Config {
    fn from(a: Args) -> Self {
        Config {
            url: a.url,
            out_dir: a.out_dir,
            idle: Duration::from_secs(a.idle_timeout_secs),
            zstd_level: a.zstd_level,
            frame: Duration::from_secs(a.frame_secs),
            ignore_pending_pause: a.ignore_pending_pause,
            min_interval: Duration::from_secs(a.min_connect_interval_secs),
            block_idle: Duration::from_secs(a.block_idle_timeout_secs),
            requested_seq: !a.no_requested_seq,
        }
    }
}

/// Exit 0 after a signal, 2 after a writer error, 1 if `run` fails before
/// recording (anyhow prints the error chain).
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    match app::run(Args::parse().into()).await? {
        Outcome::Stopped => Ok(()),
        Outcome::WriterFailed => std::process::exit(EXIT_WRITER_ERROR),
    }
}
