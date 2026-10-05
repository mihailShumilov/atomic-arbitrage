//! One raw blocks file (`blocks-<from>-<to>.jsonl.zst`, or any `.jsonl[.zst]` of the same line
//! format such as `data/samples/hourly-*.jsonl.zst`) -> validated rows of every table it feeds.
//!
//! Nothing here talks to ClickHouse: the whole file is decoded ([`crate::zst`]), parsed
//! (`decoders::parse_block_line`, with its receipt/tx consistency checks) and turned into
//! validated TSV batches before the loader sends the first row. Checks on top of the parser:
//! a block number appears once per file; a file named `blocks-<from>-<to>` holds exactly the
//! blocks `from..=to` (the enricher writes every block of its range).
//!
//! `hood.funding_edges` comes from `decoders::l1_inflows`. Rule against double counting (review
//! 027, З1; data-model.md: `gateway_status = none` rows are dropped by default): `l1_token` rows
//! from a gateway that is not in the registry are NOT loaded (only counted), and the
//! "unaccounted" records (`unregistered_gateway_eth` and the rest) have no table and are never
//! loaded as edges. So a `0x68` into an unregistered `DepositFinalized` emitter contributes no
//! edge at all: neither the token to its recipient nor the ETH to the contract.
//! [`check_no_double_count`] re-checks this per block.
//!
//! Hook for `hood.swaps` (task 033, developed in parallel; `decoders::rows::SwapRow` with
//! `COLUMNS`/`write_tsv`): implement [`crate::tsv::TsvTable`] for it (like `FundingEdge` in
//! [`crate::rows`]; `MAY_BE_EMPTY` = `router`), add a `swaps: Batch<SwapRow>` here filled from
//! `decoders::swaps::decode_block` + the pool/token registries the swap rows need, and insert it
//! in `load::load_file` next to `funding_edges` (before `blocks`, the completion marker). The
//! rollback already deletes by `block_number`, which `hood.swaps` has. Mind 033's re-load rule:
//! a registry change can move a swap to another `token` key, so a re-load of a block range must
//! delete that range's swap rows first (not needed for the tables loaded today: their keys come
//! from the raw data only).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};
use decoders::l1_inflows::{decode_block, BlockL1, GatewayRegistry, UnaccountedKind};
use decoders::rows::{EdgeKind, FundingEdge, GatewayStatus};
use hood_core::Range;

use crate::rows::{BlockRow, LogRow, TxRow};
use crate::tsv::Batch;

/// Counters of one file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileStats {
    pub blocks: usize,
    pub txs: usize,
    pub logs: usize,
    pub edges_l1_eth: usize,
    pub edges_l1_token: usize,
    /// `l1_token` rows with `gateway_status = none`, not loaded.
    pub dropped_unregistered_token_rows: usize,
    /// "Unaccounted" records by kind (not loaded, reported only).
    pub unaccounted: BTreeMap<&'static str, usize>,
    pub frames: usize,
    pub frames_with_checksum: usize,
}

/// Validated rows of one file, ready to insert.
#[derive(Debug)]
pub struct FileRows {
    pub path: PathBuf,
    /// Block rows (`feed_recv_ns` not set yet), in file order.
    pub blocks: Vec<BlockRow>,
    pub txs: Batch<TxRow>,
    pub logs: Batch<LogRow>,
    pub edges: Batch<FundingEdge>,
    pub stats: FileStats,
}

impl FileRows {
    /// Block numbers of the file, sorted.
    pub fn block_numbers(&self) -> Vec<u64> {
        let mut v: Vec<u64> = self.blocks.iter().map(|b| b.block_number).collect();
        v.sort_unstable();
        v
    }
}

/// `blocks-<from>-<to>.jsonl.zst` -> the range; other names -> `None`.
pub fn range_from_name(path: &Path) -> Option<Range> {
    let name = path.file_name()?.to_str()?;
    let mid = name.strip_prefix("blocks-")?.strip_suffix(".jsonl.zst")?;
    let (a, b) = mid.split_once('-')?;
    Range::new(a.parse().ok()?, b.parse().ok()?).ok()
}

/// Reads, decodes and validates a whole file. `.zst` files are decoded whole first; other files
/// are read as plain JSON lines (the decoder test fixtures).
///
/// # Errors
/// Any IO, zstd, parse, range or validation error; the file is then not loaded at all.
pub fn read_blocks_file(path: &Path, registry: &GatewayRegistry) -> Result<FileRows> {
    let (text, frames, frames_with_checksum) = if path.extension().is_some_and(|e| e == "zst") {
        let z = crate::zst::read_zst(path)?;
        ensure!(!z.data.is_empty(), "{}: empty file", path.display());
        (z.data, z.frames, z.frames_with_checksum)
    } else {
        (std::fs::read(path).with_context(|| format!("read {}", path.display()))?, 0, 0)
    };
    let text = String::from_utf8(text).with_context(|| format!("{}: not UTF-8", path.display()))?;
    let mut rows = parse_lines(&text, registry).with_context(|| format!("{}", path.display()))?;
    rows.path = path.to_path_buf();
    rows.stats.frames = frames;
    rows.stats.frames_with_checksum = frames_with_checksum;
    if let Some(r) = range_from_name(path) {
        let nums = rows.block_numbers();
        let want = r.blocks();
        ensure!(
            nums.first() == Some(&r.from) && nums.last() == Some(&r.to) && nums.len() as u64 == want,
            "{}: name says {r} ({want} blocks), file has {} blocks {:?}..{:?}",
            path.display(),
            nums.len(),
            nums.first(),
            nums.last()
        );
    }
    Ok(rows)
}

/// The pure part of [`read_blocks_file`]: decoded text -> rows.
///
/// # Errors
/// A line that does not parse, a repeated block number, a value that does not fit its column.
pub(crate) fn parse_lines(text: &str, registry: &GatewayRegistry) -> Result<FileRows> {
    let mut out = FileRows {
        path: PathBuf::new(),
        blocks: Vec::new(),
        txs: Batch::default(),
        logs: Batch::default(),
        edges: Batch::default(),
        stats: FileStats::default(),
    };
    let mut seen = BTreeSet::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let at = || format!("line {}", i + 1);
        let b = decoders::parse_block_line(line).with_context(at)?;
        ensure!(seen.insert(b.number), "{}: block {} repeated in the file", at(), b.number);
        out.blocks.push(BlockRow::new(&b).with_context(at)?);
        for tx in &b.txs {
            out.txs.push(&TxRow::new(b.number, tx)?)?;
            for l in &tx.logs {
                out.logs.push(&LogRow::new(b.number, tx.index, l)?)?;
            }
            out.stats.logs += tx.logs.len();
        }
        out.stats.txs += b.txs.len();

        let l1 = decode_block(&b, registry).with_context(at)?;
        for e in edges_to_load(&l1, &mut out.stats) {
            out.edges.push(&e)?;
        }
        check_no_double_count(&l1, out.edges.rows(), &out.stats).with_context(at)?;
    }
    ensure!(!out.blocks.is_empty(), "no blocks");
    out.stats.blocks = out.blocks.len();
    Ok(out)
}

/// Edges of one block that go to `hood.funding_edges` (see the module docs); updates counters.
fn edges_to_load(l1: &BlockL1, stats: &mut FileStats) -> Vec<FundingEdge> {
    for u in &l1.unaccounted {
        *stats.unaccounted.entry(u.kind.as_str()).or_default() += 1;
    }
    let mut out = Vec::new();
    for e in l1.inflows.iter().map(|r| r.funding_edge()) {
        match (e.kind, e.gateway_status) {
            (EdgeKind::L1Token, GatewayStatus::None) => stats.dropped_unregistered_token_rows += 1,
            (EdgeKind::L1Token, _) => {
                stats.edges_l1_token += 1;
                out.push(e);
            }
            _ => {
                stats.edges_l1_eth += 1;
                out.push(e);
            }
        }
    }
    out
}

/// Defensive re-check of the double counting rule for one block: no loaded edge belongs to a tx
/// that has an `unregistered_gateway_eth` record. `loaded` is the running edge count (only used
/// for the error text).
fn check_no_double_count(l1: &BlockL1, loaded: usize, stats: &FileStats) -> Result<()> {
    for u in l1.unaccounted.iter().filter(|u| u.kind == UnaccountedKind::UnregisteredGatewayEth) {
        let clash = l1.inflows.iter().map(|r| r.funding_edge()).find(|e| {
            e.tx_hash == u.tx_hash && !(e.kind == EdgeKind::L1Token && e.gateway_status == GatewayStatus::None)
        });
        if let Some(e) = clash {
            bail!(
                "block {} tx {:#x}: edge {} would be loaded next to unregistered_gateway_eth \
                 (double count; {loaded} edges so far, {} dropped)",
                u.block_number,
                u.tx_hash,
                e.kind.as_str(),
                stats.dropped_unregistered_token_rows
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixture of task 017 (`crates/decoders/tests/fixtures/l1-inflows-blocks.jsonl`, real blocks
    /// 77285521, 77285531, 77300695, 77312169 fetched 2026-10-01; tx hashes in
    /// `crates/decoders/tests/l1_inflows.rs`). Block 77312169 tx 2
    /// 0x8a448fd9b19c63c41c5f3045b08b38745ed7bf80dd5deb3a34a9218424c05d4a is a `0x68` through the
    /// L2 WETH gateway (`verified`).
    fn fixture() -> String {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../decoders/tests/fixtures/l1-inflows-blocks.jsonl");
        std::fs::read_to_string(p).unwrap()
    }

    #[test]
    fn verified_gateway_gives_one_token_edge() {
        let r = parse_lines(&fixture(), &GatewayRegistry::builtin()).unwrap();
        assert_eq!((r.stats.blocks, r.stats.txs), (4, 11));
        assert_eq!((r.stats.edges_l1_eth, r.stats.edges_l1_token, r.stats.dropped_unregistered_token_rows), (2, 1, 0));
        assert_eq!(r.edges.rows(), 3);
        assert_eq!(r.stats.unaccounted.get("unregistered_gateway_eth"), None);
    }

    /// Review 027, З1: with the gateway outside the registry the decoder makes an `l1_token` row
    /// (`gateway_status = none`) and an `unregistered_gateway_eth` record for the same tx; the
    /// loader loads neither, so the money is not counted twice (nor once: it is reported).
    #[test]
    fn unregistered_gateway_is_not_counted_twice() {
        let r = parse_lines(&fixture(), &GatewayRegistry::default()).unwrap();
        assert_eq!((r.stats.edges_l1_eth, r.stats.edges_l1_token, r.stats.dropped_unregistered_token_rows), (2, 0, 1));
        assert_eq!(r.stats.unaccounted.get("unregistered_gateway_eth"), Some(&1));
        let body = String::from_utf8(r.edges.body().to_vec()).unwrap();
        assert!(!body.contains("0x8a448fd9b19c63c41c5f3045b08b38745ed7bf80dd5deb3a34a9218424c05d4a"), "{body}");
        assert_eq!(body.lines().count(), 1 + 2);
    }

    #[test]
    fn repeated_block_in_a_file_is_an_error() {
        let f = fixture();
        let first = f.lines().next().unwrap();
        assert!(parse_lines(&format!("{f}\n{first}\n"), &GatewayRegistry::builtin()).is_err());
        assert!(parse_lines("\n", &GatewayRegistry::builtin()).is_err(), "no blocks");
    }

    #[test]
    fn range_in_file_name() {
        let p = Path::new("/x/blocks-713002-713201.jsonl.zst");
        assert_eq!(range_from_name(p), Some(Range { from: 713002, to: 713201 }));
        assert_eq!(range_from_name(Path::new("blocks-5-4.jsonl.zst")), None);
        assert_eq!(range_from_name(Path::new("blocks-5-9.jsonl.zst.partial")), None);
        assert_eq!(range_from_name(Path::new("hourly-20260804-20260930.jsonl.zst")), None);
    }
}
