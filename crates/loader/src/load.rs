//! Sending validated rows to ClickHouse: one file at a time, all of its tables or none.
//!
//! Per file ([`load_file`]):
//! 1. read the blocks of the file already in `hood.blocks` (`FINAL`): a different `block_hash`
//!    is a data error (nothing is sent); their `feed_recv_ns` is kept (see below);
//! 2. set `feed_recv_ns` from the feed index (a feed `blockHash` that differs from the RPC hash is
//!    a data error);
//! 3. INSERT `txs`, `logs`, `funding_edges`, then `blocks` LAST: a row in `hood.blocks` means
//!    every table of its file is in. Each INSERT is atomic ([`crate::ch`]);
//! 4. if an INSERT fails with a server answer (non-2xx: it inserted nothing), the rows already
//!    sent for blocks that are still not in `hood.blocks` (re-read after the failure) are deleted
//!    again (lightweight `DELETE`, synchronous) from the tables already written: the file is then
//!    absent, as before. Blocks that were already loaded need no rollback: their rows are
//!    identical, and the tables are `ReplacingMergeTree` on the row key, so after `FINAL` nothing
//!    changed. If the INSERT failed without a server answer (timeout, connection lost), its
//!    outcome is unknown: nothing is deleted and the error asks for a re-run (see `rollback`).
//!
//! Idempotency: loading the same file again inserts identical rows, which `ReplacingMergeTree`
//! collapses (`FINAL` or after merges). `feed_recv_ns` is merged as the earliest known value
//! (existing row vs feed index), so a re-load without the feed copy does not erase it.
//!
//! `hood.feed_gaps` ([`load_gaps`]): key `from_seq` only (review 029, З2). Repeats of a range
//! in `gaps.tsv` collapse into one row with the earliest `detected_ns`; two different `to_seq`
//! for one `from_seq` (in the files or against the table) stop the load: they would silently
//! collapse. `filled` = 1 when every block of the range is in `hood.blocks`; it is the version
//! column, so a later 0 never re-opens a filled gap.
//!
//! Do not run two loaders against one server at the same time: there is no lock.

use std::collections::{BTreeMap, HashMap};

use anyhow::{anyhow, bail, ensure, Context, Result};
use decoders::rows::FundingEdge;
use hood_core::ranges::GapRow;

use crate::blocks_file::FileRows;
use crate::ch::{tsv_rows, Client, ServerError};
use crate::feed::FeedIndex;
use crate::rows::{BlockRow, FeedGapRow, LogRow, TxRow};
use crate::tsv::{Batch, TsvTable};

/// Block numbers per `IN (...)` list in one query.
const IN_CHUNK: usize = 5_000;

/// What happened to one file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileOutcome {
    /// Blocks of the file that were already in `hood.blocks`.
    pub already_loaded: usize,
    /// Blocks with `feed_recv_ns` after the merge.
    pub with_feed_time: usize,
    /// Rows sent per table.
    pub inserted: BTreeMap<&'static str, usize>,
}

/// Checks that each table's columns in the server equal the loader's (migrations applied).
///
/// # Errors
/// Query error or a column list that differs.
pub async fn check_schema(ch: &Client) -> Result<()> {
    check_table::<BlockRow>(ch).await?;
    check_table::<TxRow>(ch).await?;
    check_table::<LogRow>(ch).await?;
    check_table::<FundingEdge>(ch).await?;
    check_table::<FeedGapRow>(ch).await
}

async fn check_table<T: TsvTable>(ch: &Client) -> Result<()> {
    let (db, table) = T::TABLE.split_once('.').ok_or_else(|| anyhow!("table name {}", T::TABLE))?;
    let sql = format!(
        "SELECT name FROM system.columns WHERE database = '{db}' AND table = '{table}' ORDER BY position FORMAT TabSeparated"
    );
    let got: Vec<String> = ch.query(&sql).await?.lines().map(str::to_owned).collect();
    ensure!(
        got == T::COLUMNS,
        "{}: server columns {got:?} differ from the loader's {:?}; apply sql/apply.sh",
        T::TABLE,
        T::COLUMNS
    );
    Ok(())
}

/// A block already in `hood.blocks`.
#[derive(Debug, Clone)]
struct Existing {
    hash: String,
    feed_recv_ns: Option<u64>,
}

async fn existing_blocks(ch: &Client, nums: &[u64]) -> Result<HashMap<u64, Existing>> {
    let mut out = HashMap::new();
    for chunk in nums.chunks(IN_CHUNK) {
        let sql = format!(
            "SELECT block_number, block_hash, feed_recv_ns FROM hood.blocks FINAL WHERE block_number IN ({}) FORMAT TabSeparated",
            join(chunk)
        );
        for r in tsv_rows(&ch.query(&sql).await?) {
            let [n, hash, recv] = r[..] else { bail!("unexpected row {r:?} from hood.blocks") };
            let feed_recv_ns = match recv {
                "\\N" => None,
                v => Some(v.parse().with_context(|| format!("feed_recv_ns {v:?}"))?),
            };
            out.insert(
                n.parse().with_context(|| format!("block_number {n:?}"))?,
                Existing { hash: hash.to_owned(), feed_recv_ns },
            );
        }
    }
    Ok(out)
}

/// Sets `feed_recv_ns` of every block: the earliest of the existing row and the feed index.
/// Returns the number of blocks already in the table.
///
/// # Errors
/// An existing row or the feed with another block hash.
fn merge_block_state(blocks: &mut [BlockRow], existing: &HashMap<u64, Existing>, feed: &FeedIndex) -> Result<usize> {
    let mut already = 0;
    for b in blocks.iter_mut() {
        let hash = format!("{:#x}", b.block_hash);
        let old = existing.get(&b.block_number);
        if let Some(e) = old {
            already += 1;
            ensure!(e.hash == hash, "block {}: hood.blocks has hash {}, file has {hash}", b.block_number, e.hash);
        }
        let seen = feed.get(b.block_number);
        if let Some(fh) = seen.and_then(|s| s.block_hash.as_deref()) {
            ensure!(fh == hash, "block {}: feed blockHash {fh}, RPC hash {hash}", b.block_number);
        }
        b.feed_recv_ns = [old.and_then(|e| e.feed_recv_ns), seen.map(|s| s.recv_ns)].into_iter().flatten().min();
    }
    Ok(already)
}

/// Loads one validated file (see the module docs).
///
/// # Errors
/// A data conflict (nothing sent), or an INSERT error (rows of new blocks rolled back; if the
/// rollback fails too, the error says which tables may hold partial rows).
pub async fn load_file(ch: &Client, feed: &FeedIndex, mut rows: FileRows) -> Result<FileOutcome> {
    let nums = rows.block_numbers();
    let existing = existing_blocks(ch, &nums).await?;
    let already = merge_block_state(&mut rows.blocks, &existing, feed)?;
    let new_blocks: Vec<u64> = nums.iter().copied().filter(|n| !existing.contains_key(n)).collect();

    let mut blocks = Batch::<BlockRow>::default();
    for b in &rows.blocks {
        blocks.push(b)?;
    }
    let mut out = FileOutcome {
        already_loaded: already,
        with_feed_time: rows.blocks.iter().filter(|b| b.feed_recv_ns.is_some()).count(),
        inserted: BTreeMap::new(),
    };
    let mut written: Vec<&'static str> = Vec::new();
    let steps: [(&'static str, &[&str], usize, &[u8]); 4] = [
        (TxRow::TABLE, TxRow::COLUMNS, rows.txs.rows(), rows.txs.body()),
        (LogRow::TABLE, LogRow::COLUMNS, rows.logs.rows(), rows.logs.body()),
        (<FundingEdge as TsvTable>::TABLE, <FundingEdge as TsvTable>::COLUMNS, rows.edges.rows(), rows.edges.body()),
        // Last: the completion marker of the file.
        (BlockRow::TABLE, BlockRow::COLUMNS, blocks.rows(), blocks.body()),
    ];
    for (table, columns, n, body) in steps {
        if n == 0 {
            continue;
        }
        if let Err(e) = ch.insert_tsv(table, columns, n, body.to_vec()).await {
            return Err(rollback(ch, &written, &new_blocks, e).await);
        }
        written.push(table);
        out.inserted.insert(table, n);
    }
    Ok(out)
}

/// Handles a failed INSERT; returns the error to report. Rows are deleted only when the outcome
/// is certain (review 032 architect, В1):
/// - the failed INSERT got a non-2xx answer from the server ([`ServerError`]): it inserted
///   nothing. `hood.blocks` is then read again and only blocks that are still NOT in it are
///   deleted from the `written` tables, so a block whose completion marker exists keeps its rows;
/// - any other error (timeout, connection lost after the body was sent): the server may have
///   committed the INSERT, even the marker. Nothing is deleted; the message says the outcome is
///   unknown and asks for a re-run (idempotent: it completes or confirms the file).
async fn rollback(ch: &Client, written: &[&'static str], new_blocks: &[u64], cause: anyhow::Error) -> anyhow::Error {
    if written.is_empty() || new_blocks.is_empty() {
        return cause;
    }
    if cause.downcast_ref::<ServerError>().is_none() {
        return cause.context(format!(
            "INSERT OUTCOME UNKNOWN (no answer from the server): nothing deleted; tables {} already hold rows \
             of blocks {}..={}; re-run the loader on this file (idempotent)",
            written.join(", "),
            new_blocks[0],
            new_blocks[new_blocks.len() - 1]
        ));
    }
    let to_delete: Vec<u64> = match existing_blocks(ch, new_blocks).await {
        Ok(present) => new_blocks.iter().copied().filter(|n| !present.contains_key(n)).collect(),
        Err(e) => {
            return cause.context(format!(
                "hood.blocks unreadable before rollback ({e:#}): nothing deleted; re-run the loader on this file"
            ))
        }
    };
    if to_delete.is_empty() {
        return cause.context(format!(
            "nothing rolled back: all {} blocks are in hood.blocks (completion marker committed), the file is in",
            new_blocks.len()
        ));
    }
    let mut failed = Vec::new();
    for table in written {
        for chunk in to_delete.chunks(IN_CHUNK) {
            let sql = format!(
                "DELETE FROM {table} WHERE block_number IN ({}) SETTINGS lightweight_deletes_sync = 2",
                join(chunk)
            );
            if let Err(e) = ch.query(&sql).await {
                failed.push(format!("{table}: {e:#}"));
                break;
            }
        }
    }
    let kept = new_blocks.len() - to_delete.len();
    let kept = if kept > 0 { format!("; {kept} blocks already in hood.blocks kept") } else { String::new() };
    if failed.is_empty() {
        cause.context(format!(
            "rolled back: rows of {} new blocks deleted from {}{kept}",
            to_delete.len(),
            written.join(", ")
        ))
    } else {
        cause.context(format!(
            "ROLLBACK FAILED, partial rows of blocks {}..={} may remain ({}){kept}; re-run the loader on this file \
             (idempotent) or delete them",
            to_delete[0],
            to_delete[to_delete.len() - 1],
            failed.join("; ")
        ))
    }
}

/// Counters of [`load_gaps`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GapsOutcome {
    /// Rows read from all `gaps.tsv` files.
    pub rows_read: usize,
    /// Distinct gaps after removing repeats.
    pub gaps: usize,
    /// Gaps whose every block is in `hood.blocks`.
    pub filled: usize,
}

/// Repeats removed: one row per `from_seq`, earliest `detected_ns`.
///
/// # Errors
/// Two different `to_seq` for one `from_seq`; `recv_ns` over `u64`.
pub(crate) fn dedupe_gaps(rows: &[GapRow]) -> Result<Vec<FeedGapRow>> {
    let mut by_from: BTreeMap<u64, FeedGapRow> = BTreeMap::new();
    for g in rows {
        let detected_ns =
            u64::try_from(g.recv_ns).map_err(|_| anyhow!("gap {}: recv_ns {} over u64", g.range, g.recv_ns))?;
        let row = FeedGapRow { from_seq: g.range.from, to_seq: g.range.to, detected_ns, filled: 0 };
        match by_from.get_mut(&row.from_seq) {
            None => {
                by_from.insert(row.from_seq, row);
            }
            Some(prev) => {
                ensure!(
                    prev.to_seq == row.to_seq,
                    "gaps: from_seq {} with to_seq {} and {} (the key is from_seq only: they would collapse)",
                    row.from_seq,
                    prev.to_seq,
                    row.to_seq
                );
                prev.detected_ns = prev.detected_ns.min(row.detected_ns);
            }
        }
    }
    Ok(by_from.into_values().collect())
}

/// Loads `gaps.tsv` rows into `hood.feed_gaps` (see the module docs). Run after the blocks.
///
/// # Errors
/// A conflict (see [`dedupe_gaps`], or against the table), query or INSERT error.
pub async fn load_gaps(ch: &Client, rows: &[GapRow]) -> Result<GapsOutcome> {
    let mut gaps = dedupe_gaps(rows)?;
    let mut out = GapsOutcome { rows_read: rows.len(), gaps: gaps.len(), filled: 0 };
    if gaps.is_empty() {
        return Ok(out);
    }
    let froms: Vec<u64> = gaps.iter().map(|g| g.from_seq).collect();
    let mut existing: HashMap<u64, (u64, u64)> = HashMap::new();
    for chunk in froms.chunks(IN_CHUNK) {
        let sql = format!(
            "SELECT from_seq, to_seq, detected_ns FROM hood.feed_gaps FINAL WHERE from_seq IN ({}) FORMAT TabSeparated",
            join(chunk)
        );
        for r in tsv_rows(&ch.query(&sql).await?) {
            let [f, t, d] = r[..] else { bail!("unexpected row {r:?} from hood.feed_gaps") };
            existing.insert(f.parse()?, (t.parse()?, d.parse()?));
        }
    }
    let mut batch = Batch::<FeedGapRow>::default();
    for g in &mut gaps {
        if let Some(&(to, detected)) = existing.get(&g.from_seq) {
            ensure!(
                to == g.to_seq,
                "gap from_seq {}: hood.feed_gaps has to_seq {to}, gaps.tsv {}",
                g.from_seq,
                g.to_seq
            );
            g.detected_ns = g.detected_ns.min(detected);
        }
        let sql = format!(
            "SELECT uniqExact(block_number) FROM hood.blocks WHERE block_number BETWEEN {} AND {} FORMAT TabSeparated",
            g.from_seq, g.to_seq
        );
        let have: u64 = ch.query(&sql).await?.trim().parse().context("uniqExact")?;
        let want = g.to_seq - g.from_seq + 1;
        g.filled = u8::from(have == want);
        out.filled += usize::from(g.filled);
        batch.push(g)?;
    }
    ch.insert_tsv(FeedGapRow::TABLE, FeedGapRow::COLUMNS, batch.rows(), batch.body().to_vec()).await?;
    Ok(out)
}

fn join(nums: &[u64]) -> String {
    nums.iter().map(u64::to_string).collect::<Vec<_>>().join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;
    use hood_core::Range;

    fn g(from: u64, to: u64, recv: u128) -> GapRow {
        GapRow { range: Range { from, to }, recv_ns: recv }
    }

    #[test]
    fn gap_repeats_collapse_and_conflicts_fail() {
        let rows = [g(10, 20, 7), g(30, 30, 9), g(10, 20, 5)];
        assert_eq!(
            dedupe_gaps(&rows).unwrap(),
            vec![
                FeedGapRow { from_seq: 10, to_seq: 20, detected_ns: 5, filled: 0 },
                FeedGapRow { from_seq: 30, to_seq: 30, detected_ns: 9, filled: 0 },
            ]
        );
        assert!(dedupe_gaps(&[g(10, 20, 1), g(10, 21, 1)]).is_err());
        assert!(dedupe_gaps(&[g(1, 2, u128::from(u64::MAX) + 1)]).is_err());
    }

    fn block(n: u64, hash: u8) -> BlockRow {
        BlockRow {
            block_number: n,
            block_hash: B256::repeat_byte(hash),
            ts: 1,
            l1_block: 1,
            base_fee_wei: 1,
            tx_count: 1,
            feed_recv_ns: None,
        }
    }

    #[test]
    fn feed_time_is_the_earliest_known_and_hashes_must_agree() {
        let h = |b: u8| format!("{:#x}", B256::repeat_byte(b));
        let mut feed = FeedIndex::default();
        let line = |recv: u64, n: u64, hash: &str| {
            format!("{recv}\t{n}\t{n}\t{{\"messages\":[{{\"sequenceNumber\":{n},\"blockHash\":\"{hash}\"}}]}}\n")
        };
        let text = format!("{}{}", line(50, 1, &h(1)), line(50, 2, &h(2)));
        feed.add_text("t", text.as_bytes(), None).unwrap();
        let existing: HashMap<u64, Existing> = [
            (1, Existing { hash: h(1), feed_recv_ns: Some(40) }), // older value wins
            (3, Existing { hash: h(3), feed_recv_ns: Some(70) }), // kept without feed
        ]
        .into();
        let mut blocks = vec![block(1, 1), block(2, 2), block(3, 3), block(4, 4)];
        assert_eq!(merge_block_state(&mut blocks, &existing, &feed).unwrap(), 2);
        let got: Vec<_> = blocks.iter().map(|b| b.feed_recv_ns).collect();
        assert_eq!(got, vec![Some(40), Some(50), Some(70), None]);

        let mut bad = vec![block(3, 9)];
        assert!(merge_block_state(&mut bad, &existing, &feed).is_err(), "hash differs from the table");
        let mut bad = vec![block(2, 9)];
        assert!(merge_block_state(&mut bad, &HashMap::new(), &feed).is_err(), "hash differs from the feed");
    }
}
