//! Minimal ClickHouse HTTP client: one query or one INSERT per request.
//!
//! Credentials go in `X-ClickHouse-User` / `X-ClickHouse-Key` headers (not in the URL, so they are
//! not in error texts). `wait_end_of_query=1` makes the HTTP status final: an error raised after
//! the first bytes of the result is not reported as 200.
//!
//! INSERT is all-or-nothing per request with [`INSERT_SETTINGS`]: synchronous (`async_insert=0`,
//! on by default in 26.9), one block (`max_insert_block_size` above [`MAX_ATOMIC_ROWS`], no
//! parallel parsing, no squashing), hence one part. Checked 2026-10-05 on 26.9.6.6 (temporary
//! container): 2 500 001 TSV rows with a broken last row inserted 0 rows with these settings, but
//! 1 076 355 rows (one committed part) with the defaults, also with only `max_insert_block_size`
//! raised. Strict input: `\N` into a non-Nullable column is an error
//! (`input_format_null_as_default=0`), unknown header columns are an error.

use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::config::ChConfig;

/// Upper bound of rows in one atomic INSERT ([`INSERT_SETTINGS`] sets `max_insert_block_size`
/// above it). A batch over it is refused before sending: it could be split into parts.
pub const MAX_ATOMIC_ROWS: usize = 50_000_000;

/// Settings of every INSERT, sent as URL parameters.
pub const INSERT_SETTINGS: [(&str, &str); 10] = [
    ("async_insert", "0"),
    ("input_format_parallel_parsing", "0"),
    ("max_insert_block_size", "100000000"),
    ("min_insert_block_size_rows", "0"),
    ("min_insert_block_size_bytes", "0"),
    ("input_format_null_as_default", "0"),
    ("input_format_skip_unknown_fields", "0"),
    ("input_format_with_names_use_header", "1"),
    // 10-digit unix seconds or `YYYY-MM-DD hh:mm:ss` only (the loader writes unix seconds).
    ("date_time_input_format", "basic"),
    ("wait_end_of_query", "1"),
];

/// Timeout of one request (an INSERT of a whole file included).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

/// Connection to one ClickHouse server.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    cfg: ChConfig,
}

impl Client {
    /// # Errors
    /// The HTTP client cannot be built.
    pub fn new(cfg: ChConfig) -> Result<Self> {
        let http = reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build().context("build HTTP client")?;
        Ok(Self { http, cfg })
    }

    /// Base URL (no credentials in it).
    pub fn url(&self) -> &str {
        &self.cfg.url
    }

    /// Runs one statement; returns the body (use an explicit `FORMAT` in `sql` when parsing it).
    ///
    /// # Errors
    /// Network error or a non-2xx status (the server's message is in the error).
    pub async fn query(&self, sql: &str) -> Result<String> {
        self.post(&[("wait_end_of_query", "1")], sql.as_bytes().to_vec())
            .await
            .with_context(|| format!("query: {}", first_line(sql)))
    }

    /// `INSERT INTO table (columns) FORMAT TabSeparatedWithNames` with `body` (header line
    /// included) and [`INSERT_SETTINGS`].
    ///
    /// # Errors
    /// `rows` over [`MAX_ATOMIC_ROWS`], network error, non-2xx status.
    pub async fn insert_tsv(&self, table: &str, columns: &[&str], rows: usize, body: Vec<u8>) -> Result<()> {
        if rows > MAX_ATOMIC_ROWS {
            bail!("{table}: {rows} rows in one INSERT is over {MAX_ATOMIC_ROWS}: it may not be atomic");
        }
        let sql = format!("INSERT INTO {table} ({}) FORMAT TabSeparatedWithNames", columns.join(", "));
        let mut params: Vec<(&str, &str)> = INSERT_SETTINGS.to_vec();
        params.push(("query", &sql));
        self.post(&params, body).await.with_context(|| format!("insert {rows} rows into {table}"))?;
        Ok(())
    }

    async fn post(&self, params: &[(&str, &str)], body: Vec<u8>) -> Result<String> {
        let resp = self
            .http
            .post(&self.cfg.url)
            .query(params)
            .header("X-ClickHouse-User", &self.cfg.user)
            .header("X-ClickHouse-Key", self.cfg.password.expose())
            .body(body)
            .send()
            .await
            .with_context(|| format!("POST {}", self.cfg.url))?;
        let status = resp.status();
        let text = resp.text().await.context("read ClickHouse response")?;
        if !status.is_success() {
            return Err(ServerError { status: status.as_u16(), message: text.trim().to_owned() }.into());
        }
        Ok(text)
    }
}

/// The server answered the request with a non-2xx status: the statement definitely failed (with
/// `wait_end_of_query=1` the status is final, and an INSERT with [`INSERT_SETTINGS`] is atomic, so
/// it inserted nothing). Any other error of [`Client::query`] / [`Client::insert_tsv`] (timeout,
/// connection reset after the body was sent, unreadable response) leaves the outcome UNKNOWN:
/// the server may have committed it. Find it with `err.downcast_ref::<ServerError>()` (works
/// through the added contexts).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerError {
    pub status: u16,
    pub message: String,
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ClickHouse HTTP {}: {}", self.status, self.message)
    }
}

impl std::error::Error for ServerError {}

fn first_line(sql: &str) -> &str {
    let l = sql.trim_start().lines().next().unwrap_or_default();
    l.get(..120).unwrap_or(l)
}

/// Rows of a `TabSeparated` result (no header); an empty body is no rows.
pub(crate) fn tsv_rows(body: &str) -> impl Iterator<Item = Vec<&str>> {
    body.lines().filter(|l| !l.is_empty()).map(|l| l.split('\t').collect())
}
