//! Rows of the tables loaded straight from raw files: `hood.blocks`, `hood.txs`, `hood.logs`
//! (from `blocks-*.jsonl.zst`), `hood.feed_gaps` (from `gaps.tsv`), plus the [`TsvTable`]
//! description of the decoder row `decoders::rows::FundingEdge` (its TSV writer stays in the
//! decoders crate).
//!
//! Column names and order are those of the last `CREATE TABLE` of each table in `sql/` (001 for
//! `blocks`, 004 for `txs`/`logs`/`feed_gaps`, 003 for `funding_edges`); the tests at the bottom
//! read `sql/` and compare. Conventions (`references/data-model.md`): addresses and hashes are
//! lowercase `0x…`, uint256 is a decimal string, `''` = absent in `String DEFAULT ''` columns,
//! `blocks.feed_recv_ns` is the only Nullable column (`\N` = block not seen by the recorder).
//!
//! Range checks happen in the constructors, so a value that does not fit its column is an error
//! with the block/tx position, never a silent truncation.

use std::io::{self, Write};

use alloy_primitives::{hex, Address, B256, U256};
use anyhow::{anyhow, ensure, Context, Result};
use decoders::rows::FundingEdge;
use decoders::{Block, Log, Tx};

use crate::tsv::TsvTable;

/// One row of `hood.blocks`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockRow {
    pub block_number: u64,
    pub block_hash: B256,
    /// Unix seconds (`DateTime`, written as 10-digit unix time).
    pub ts: u32,
    pub l1_block: u64,
    pub base_fee_wei: u64,
    /// All transactions of the block, the StartBlock `0x6a` at index 0 included.
    pub tx_count: u16,
    /// Receive time of the block in the recorded feed, unix ns; `None` = not seen by the recorder.
    pub feed_recv_ns: Option<u64>,
}

impl BlockRow {
    /// Row of a parsed block; `feed_recv_ns` is set later by the loader.
    ///
    /// # Errors
    /// Block 0, a timestamp outside `DateTime`, base fee over `u64`, more than 65 535 txs.
    pub fn new(b: &Block) -> Result<Self> {
        let n = b.number;
        ensure!(n > 0, "block 0 is not loaded");
        Ok(Self {
            block_number: n,
            block_hash: b.hash,
            ts: u32::try_from(b.timestamp)
                .map_err(|_| anyhow!("block {n}: timestamp {} over DateTime", b.timestamp))?,
            l1_block: b.l1_block_number,
            base_fee_wei: u64_of(b.base_fee_per_gas).with_context(|| format!("block {n}: baseFeePerGas"))?,
            tx_count: u16::try_from(b.txs.len()).map_err(|_| anyhow!("block {n}: {} txs over UInt16", b.txs.len()))?,
            feed_recv_ns: None,
        })
    }
}

impl TsvTable for BlockRow {
    const TABLE: &'static str = "hood.blocks";
    const COLUMNS: &'static [&'static str] =
        &["block_number", "block_hash", "ts", "l1_block", "base_fee_wei", "tx_count", "feed_recv_ns"];
    const MAY_BE_EMPTY: &'static [&'static str] = &[];
    const NULLABLE: &'static [&'static str] = &["feed_recv_ns"];

    fn write_tsv(&self, w: &mut Vec<u8>) -> io::Result<()> {
        write!(
            w,
            "{}\t{:#x}\t{}\t{}\t{}\t{}\t",
            self.block_number, self.block_hash, self.ts, self.l1_block, self.base_fee_wei, self.tx_count
        )?;
        match self.feed_recv_ns {
            Some(ns) => writeln!(w, "{ns}"),
            None => writeln!(w, "\\N"),
        }
    }
}

/// One row of `hood.txs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxRow {
    pub block_number: u64,
    pub tx_index: u32,
    pub tx_hash: B256,
    pub from_addr: Address,
    /// `None` = contract creation (written as `''`).
    pub to_addr: Option<Address>,
    pub value_wei: U256,
    /// `None` = input shorter than 4 bytes (written as `''`).
    pub selector: Option<[u8; 4]>,
    pub input_len: u32,
    pub status: u8,
    pub gas_used: u64,
    pub gas_used_l1: u64,
    pub eff_gas_price: u64,
    /// `gas_used * eff_gas_price` (on Arbitrum `gasUsed` includes the L1 part).
    pub fee_wei: u128,
}

impl TxRow {
    /// # Errors
    /// `effectiveGasPrice` over `u64` or a fee over `u128`.
    pub fn new(block: u64, tx: &Tx) -> Result<Self> {
        let ctx = || format!("block {block} tx {}", tx.index);
        let fee = U256::from(tx.gas_used)
            .checked_mul(tx.effective_gas_price)
            .ok_or_else(|| anyhow!("{}: fee overflows U256", ctx()))?;
        Ok(Self {
            block_number: block,
            tx_index: tx.index,
            tx_hash: tx.hash,
            from_addr: tx.from,
            to_addr: tx.to,
            value_wei: tx.value,
            selector: tx.selector,
            input_len: tx.input_len,
            status: u8::from(tx.status),
            gas_used: tx.gas_used,
            gas_used_l1: tx.gas_used_for_l1,
            eff_gas_price: u64_of(tx.effective_gas_price).with_context(|| format!("{}: effectiveGasPrice", ctx()))?,
            fee_wei: u128::try_from(fee).map_err(|_| anyhow!("{}: fee {fee} over UInt128", ctx()))?,
        })
    }
}

impl TsvTable for TxRow {
    const TABLE: &'static str = "hood.txs";
    const COLUMNS: &'static [&'static str] = &[
        "block_number",
        "tx_index",
        "tx_hash",
        "from_addr",
        "to_addr",
        "value_wei",
        "selector",
        "input_len",
        "status",
        "gas_used",
        "gas_used_l1",
        "eff_gas_price",
        "fee_wei",
    ];
    const MAY_BE_EMPTY: &'static [&'static str] = &["to_addr", "selector"];
    const NULLABLE: &'static [&'static str] = &[];

    fn write_tsv(&self, w: &mut Vec<u8>) -> io::Result<()> {
        write!(w, "{}\t{}\t{:#x}\t{:#x}\t", self.block_number, self.tx_index, self.tx_hash, self.from_addr)?;
        if let Some(to) = self.to_addr {
            write!(w, "{to:#x}")?;
        }
        write!(w, "\t{}\t", self.value_wei)?;
        if let Some(sel) = self.selector {
            write!(w, "{}", hex::encode_prefixed(sel))?;
        }
        writeln!(
            w,
            "\t{}\t{}\t{}\t{}\t{}\t{}",
            self.input_len, self.status, self.gas_used, self.gas_used_l1, self.eff_gas_price, self.fee_wei
        )
    }
}

/// One row of `hood.logs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRow {
    pub block_number: u64,
    pub tx_index: u32,
    /// The parsed log (`Bytes` clones are reference-counted).
    pub log: Log,
}

impl LogRow {
    /// # Errors
    /// More than 4 topics (impossible in the EVM: the row would lose data).
    pub fn new(block: u64, tx_index: u32, log: &Log) -> Result<Self> {
        ensure!(log.topics.len() <= 4, "block {block} log {}: {} topics", log.index, log.topics.len());
        Ok(Self { block_number: block, tx_index, log: log.clone() })
    }
}

impl TsvTable for LogRow {
    const TABLE: &'static str = "hood.logs";
    const COLUMNS: &'static [&'static str] =
        &["block_number", "tx_index", "log_index", "address", "topic0", "topic1", "topic2", "topic3", "data"];
    /// `topic0` is empty for anonymous logs (110 of 127 824 logs in data/blocks + data/samples,
    /// counted 2026-10-05).
    const MAY_BE_EMPTY: &'static [&'static str] = &["topic0", "topic1", "topic2", "topic3"];
    const NULLABLE: &'static [&'static str] = &[];

    fn write_tsv(&self, w: &mut Vec<u8>) -> io::Result<()> {
        write!(w, "{}\t{}\t{}\t{:#x}", self.block_number, self.tx_index, self.log.index, self.log.address)?;
        for i in 0..4 {
            match self.log.topics.get(i) {
                Some(t) => write!(w, "\t{t:#x}")?,
                None => w.write_all(b"\t")?,
            }
        }
        writeln!(w, "\t{}", hex::encode_prefixed(&self.log.data))
    }
}

/// One row of `hood.feed_gaps`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeedGapRow {
    pub from_seq: u64,
    pub to_seq: u64,
    /// `recv_ns` of the first feed line after the hole (`gaps.tsv` column 3).
    pub detected_ns: u64,
    /// 1 = every block of the range is in `hood.blocks` (version column: 1 wins on merge).
    pub filled: u8,
}

impl TsvTable for FeedGapRow {
    const TABLE: &'static str = "hood.feed_gaps";
    const COLUMNS: &'static [&'static str] = &["from_seq", "to_seq", "detected_ns", "filled"];
    const MAY_BE_EMPTY: &'static [&'static str] = &[];
    const NULLABLE: &'static [&'static str] = &[];

    fn write_tsv(&self, w: &mut Vec<u8>) -> io::Result<()> {
        writeln!(w, "{}\t{}\t{}\t{}", self.from_seq, self.to_seq, self.detected_ns, self.filled)
    }
}

impl TsvTable for FundingEdge {
    const TABLE: &'static str = "hood.funding_edges";
    const COLUMNS: &'static [&'static str] = &FundingEdge::COLUMNS;
    const MAY_BE_EMPTY: &'static [&'static str] = &["token", "l1_token", "gateway", "l1_request_id", "ticket_id"];
    const NULLABLE: &'static [&'static str] = &[];

    fn write_tsv(&self, w: &mut Vec<u8>) -> io::Result<()> {
        FundingEdge::write_tsv(self, w)
    }
}

fn u64_of(v: U256) -> Result<u64> {
    u64::try_from(v).map_err(|_| anyhow!("{v} does not fit UInt64"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tsv::Batch;

    /// All `sql/NNN_*.sql` files in migration order, `--` comments stripped.
    fn sql_lines() -> Vec<String> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sql");
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "sql"))
            .collect();
        files.sort();
        files
            .iter()
            .flat_map(|p| std::fs::read_to_string(p).unwrap().lines().map(str::to_owned).collect::<Vec<_>>())
            .map(|l| l.split("--").next().unwrap_or_default().to_owned())
            .collect()
    }

    /// `hood.<t>` or a rebuild name `hood.<t>_NNN` (migrations 003/004 build `<t>_NNN`, then RENAME).
    fn creates(line: &str, table: &str) -> bool {
        let Some(rest) = line.split("CREATE TABLE IF NOT EXISTS hood.").nth(1) else { return false };
        let name = rest.split([' ', '(']).next().unwrap_or_default();
        name == table
            || name
                .strip_prefix(table)
                .and_then(|s| s.strip_prefix('_'))
                .is_some_and(|s| s.len() == 3 && s.bytes().all(|b| b.is_ascii_digit()))
    }

    /// Column names of the LAST `CREATE TABLE` of `table` in `sql/`; panics if a later ALTER
    /// touches the table (then this test must learn about it).
    fn sql_columns(table: &str) -> Vec<String> {
        let lines = sql_lines();
        let start = lines.iter().rposition(|l| creates(l, table)).unwrap_or_else(|| panic!("no CREATE {table}"));
        let alter = format!("ALTER TABLE hood.{table}");
        assert!(
            !lines[start..].iter().any(|l| l.contains(&alter) && !l.contains(&format!("{alter}_"))),
            "ALTER of {table} after its last CREATE"
        );
        lines[start + 1..]
            .iter()
            .map(|l| l.trim())
            .take_while(|l| !l.starts_with(')'))
            .filter_map(|l| l.split_whitespace().next().map(str::to_owned))
            .collect()
    }

    #[test]
    fn column_order_equals_sql() {
        assert_eq!(sql_columns("blocks"), BlockRow::COLUMNS);
        assert_eq!(sql_columns("txs"), TxRow::COLUMNS);
        assert_eq!(sql_columns("logs"), LogRow::COLUMNS);
        assert_eq!(sql_columns("feed_gaps"), FeedGapRow::COLUMNS);
        assert_eq!(sql_columns("funding_edges"), <FundingEdge as TsvTable>::COLUMNS);
    }

    /// Block 76491176 tx 1 (data/blocks/blocks-76491176-76491195.jsonl.zst, 2026-09-30), trimmed
    /// to the fields the loader reads: a type-2 tx with 4 logs, the first a Transfer mint.
    const BLOCK_LINE: &str = r#"{"number":76491176,"block":{"number":"0x48f29a8","hash":"0x8c2fc16f5b33c0f8c227dbd045647fb625671f96e25b934e05e45ed3f327858b","timestamp":"0x6abcf6e4","l1BlockNumber":"0x18e1a6b","baseFeePerGas":"0x1526100","transactions":[{"hash":"0xc49c922d61fb9c0c784bb24d5f7bcb6f9b36746b6d136beeb3397d6eb6cb31ab","type":"0x6a","from":"0x00000000000000000000000000000000000a4b05","to":"0x00000000000000000000000000000000000a4b05","value":"0x0","input":"0x6bf6a42d00","transactionIndex":"0x0"},{"hash":"0x5c3324ca098c476574a18e19f2ddabaa623b4f4c99bb7401663364c81cfe77d9","type":"0x2","from":"0xa1e2cf8273a84c1186a2dd1af31734afbedc9cbb","to":null,"value":"0x10","input":"0x3593","transactionIndex":"0x1"}]},"receipts":[{"transactionHash":"0xc49c922d61fb9c0c784bb24d5f7bcb6f9b36746b6d136beeb3397d6eb6cb31ab","status":"0x1","gasUsed":"0x0","gasUsedForL1":"0x0","effectiveGasPrice":"0x1526100","logs":[]},{"transactionHash":"0x5c3324ca098c476574a18e19f2ddabaa623b4f4c99bb7401663364c81cfe77d9","status":"0x0","gasUsed":"0x30644","gasUsedForL1":"0x5","effectiveGasPrice":"0x1526100","logs":[{"address":"0x0bd7d308f8e1639fab988df18a8011f41eacad73","topics":["0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef","0x0000000000000000000000000000000000000000000000000000000000000000"],"data":"0x","logIndex":"0x0"},{"address":"0x0bd7d308f8e1639fab988df18a8011f41eacad73","topics":[],"data":"0x00ff","logIndex":"0x1"}]}]}"#;

    #[test]
    fn golden_rows() {
        let b = decoders::parse_block_line(BLOCK_LINE).unwrap();
        let mut blocks = Batch::<BlockRow>::default();
        let mut row = BlockRow::new(&b).unwrap();
        blocks.push(&row).unwrap();
        row.feed_recv_ns = Some(1_790_768_448_986_314_000);
        blocks.push(&row).unwrap();
        assert_eq!(
            String::from_utf8(blocks.body().to_vec()).unwrap(),
            "block_number\tblock_hash\tts\tl1_block\tbase_fee_wei\ttx_count\tfeed_recv_ns\n\
             76491176\t0x8c2fc16f5b33c0f8c227dbd045647fb625671f96e25b934e05e45ed3f327858b\t1790768868\t26090091\t22176000\t2\t\\N\n\
             76491176\t0x8c2fc16f5b33c0f8c227dbd045647fb625671f96e25b934e05e45ed3f327858b\t1790768868\t26090091\t22176000\t2\t1790768448986314000\n"
        );
        let mut txs = Batch::<TxRow>::default();
        for tx in &b.txs {
            txs.push(&TxRow::new(b.number, tx).unwrap()).unwrap();
        }
        let text = String::from_utf8(txs.body().to_vec()).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(
            lines[1],
            "76491176\t0\t0xc49c922d61fb9c0c784bb24d5f7bcb6f9b36746b6d136beeb3397d6eb6cb31ab\t\
             0x00000000000000000000000000000000000a4b05\t0x00000000000000000000000000000000000a4b05\t0\t0x6bf6a42d\t5\t1\t0\t0\t22176000\t0"
        );
        // Contract creation (to = null), short input, reverted, fee = 198212 * 22176000.
        assert_eq!(
            lines[2],
            "76491176\t1\t0x5c3324ca098c476574a18e19f2ddabaa623b4f4c99bb7401663364c81cfe77d9\t\
             0xa1e2cf8273a84c1186a2dd1af31734afbedc9cbb\t\t16\t\t2\t0\t198212\t5\t22176000\t4395549312000"
        );
        let mut logs = Batch::<LogRow>::default();
        for l in &b.txs[1].logs {
            logs.push(&LogRow::new(b.number, 1, l).unwrap()).unwrap();
        }
        let text = String::from_utf8(logs.body().to_vec()).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(
            lines[1],
            "76491176\t1\t0\t0x0bd7d308f8e1639fab988df18a8011f41eacad73\t\
             0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef\t\
             0x0000000000000000000000000000000000000000000000000000000000000000\t\t\t0x"
        );
        assert_eq!(lines[2], "76491176\t1\t1\t0x0bd7d308f8e1639fab988df18a8011f41eacad73\t\t\t\t\t0x00ff");
    }

    #[test]
    fn values_that_do_not_fit_are_errors() {
        let mut b = decoders::parse_block_line(BLOCK_LINE).unwrap();
        b.base_fee_per_gas = U256::from(u64::MAX) + U256::from(1u8);
        assert!(BlockRow::new(&b).is_err());
        let mut b = decoders::parse_block_line(BLOCK_LINE).unwrap();
        b.timestamp = u64::from(u32::MAX) + 1;
        assert!(BlockRow::new(&b).is_err());
        let mut b = decoders::parse_block_line(BLOCK_LINE).unwrap();
        b.number = 0;
        assert!(BlockRow::new(&b).is_err());
        let mut tx = decoders::parse_block_line(BLOCK_LINE).unwrap().txs[1].clone();
        tx.effective_gas_price = U256::from(u64::MAX) + U256::from(1u8);
        assert!(TxRow::new(1, &tx).is_err());
        let mut log = decoders::parse_block_line(BLOCK_LINE).unwrap().txs[1].logs[0].clone();
        log.topics = vec![B256::ZERO; 5];
        assert!(LogRow::new(1, 0, &log).is_err());
    }
}
