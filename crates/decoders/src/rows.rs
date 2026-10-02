//! Output row types and their TSV form: the single place that maps decoder output to
//! ClickHouse columns. Decoder-specific scanner outputs (e.g. `l1_inflows::UnaccountedFlow`)
//! keep their TSV next to their type and reuse the helpers below.
//!
//! TSV conventions (ClickHouse `TabSeparated`): addresses and hashes are lowercase `0x…`,
//! `U256` is decimal, an absent value of a `String DEFAULT ''` column is the empty string, an
//! absent value of a column that is (or would be) `Nullable` is `\N`. No value written here can
//! contain a tab, newline or backslash, so no escaping is needed.

use std::fmt;
use std::io::{self, Write};

use alloy_primitives::{Address, B256, U256};

/// `hood.funding_edges.kind` (Enum8). Discriminants are the Enum8 values (sql/001 + sql/002).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(i8)]
pub enum EdgeKind {
    /// Plain ETH transfer (tx.value), not decoded yet.
    Eth = 1,
    /// WETH `Transfer`, not decoded yet.
    Weth = 2,
    /// Internal ETH transfer (needs traces), not decoded yet.
    Internal = 3,
    /// ETH from L1: `0x64` deposit or `0x68` retry with value.
    L1Eth = 4,
    /// Token from L1 through a token gateway (`DepositFinalized`).
    L1Token = 5,
}

impl EdgeKind {
    /// Every variant, in Enum8 value order.
    pub const ALL: [Self; 5] = [Self::Eth, Self::Weth, Self::Internal, Self::L1Eth, Self::L1Token];

    /// Enum8 name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Eth => "eth",
            Self::Weth => "weth",
            Self::Internal => "internal",
            Self::L1Eth => "l1_eth",
            Self::L1Token => "l1_token",
        }
    }

    /// Enum8 value.
    #[must_use]
    pub const fn enum8(self) -> i8 {
        self as i8
    }
}

/// `hood.funding_edges.gateway_status` (Enum8, sql/002). Discriminants are the Enum8 values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(i8)]
pub enum GatewayStatus {
    /// ETH rows, and token rows from a gateway that is not in the caller's registry.
    None = 0,
    /// Gateway is `observed` in the registry (research only).
    Observed = 1,
    /// Gateway is `verified` in the registry.
    Verified = 2,
}

impl GatewayStatus {
    /// Every variant, in Enum8 value order.
    pub const ALL: [Self; 3] = [Self::None, Self::Observed, Self::Verified];

    /// Enum8 name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Observed => "observed",
            Self::Verified => "verified",
        }
    }

    /// Enum8 value.
    #[must_use]
    pub const fn enum8(self) -> i8 {
        self as i8
    }
}

/// One row of `hood.funding_edges`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FundingEdge {
    pub block_number: u64,
    pub tx_index: u32,
    /// L1 (Ethereum) address of the funder for `l1_*` rows: unalias(tx.from) for ETH rows,
    /// `DepositFinalized.from` for token rows.
    pub from_addr: Address,
    pub to_addr: Address,
    /// Wei for ETH rows, raw token units for token rows.
    pub value_wei: U256,
    pub kind: EdgeKind,
    pub tx_hash: B256,
    /// Block-level log index of the log the edge comes from; `None` = tx-level edge
    /// (written as [`FundingEdge::TX_LEVEL_LOG_INDEX`]).
    pub log_index: Option<u32>,
    /// L2 token (`l1_token` rows; `None` if no matching `Transfer`).
    pub token: Option<Address>,
    /// `DepositFinalized.l1Token` (Ethereum address).
    pub l1_token: Option<Address>,
    /// L2 gateway (= the `0x68` `to`).
    pub gateway: Option<Address>,
    pub gateway_status: GatewayStatus,
    /// `tx.from` of the `0x64`/`0x68` (aliased L1 sender).
    pub l2_alias: Address,
    pub tx_type: u8,
    /// `0x64` `requestId`.
    pub l1_request_id: Option<U256>,
    /// `0x68` `ticketId`.
    pub ticket_id: Option<B256>,
}

impl FundingEdge {
    /// `log_index` value of a tx-level edge (`0x64`, `0x68` ETH, plain `tx.value`): `u32::MAX`,
    /// because 0 is a valid index of the first log of a block. Matches
    /// `log_index UInt32 DEFAULT 4294967295` of `hood.funding_edges` (migration 003, task 023).
    pub const TX_LEVEL_LOG_INDEX: u32 = u32::MAX;

    /// Column order of the TSV line (and of the loader's `INSERT … (columns) FORMAT TSV`).
    /// Types in ClickHouse: block_number UInt64, tx_index UInt32, from_addr/to_addr String,
    /// value_wei String (decimal), kind Enum8, tx_hash String, log_index UInt32 (no NULL),
    /// token/l1_token/gateway String DEFAULT '', gateway_status Enum8, l2_alias String,
    /// tx_type UInt8, l1_request_id String DEFAULT '' (decimal), ticket_id String DEFAULT ''.
    pub const COLUMNS: [&'static str; 16] = [
        "block_number",
        "tx_index",
        "from_addr",
        "to_addr",
        "value_wei",
        "kind",
        "tx_hash",
        "log_index",
        "token",
        "l1_token",
        "gateway",
        "gateway_status",
        "l2_alias",
        "tx_type",
        "l1_request_id",
        "ticket_id",
    ];

    /// `log_index` as written to the table.
    ///
    /// # Errors
    /// `Some(u32::MAX)` would be indistinguishable from a tx-level edge.
    pub fn log_index_column(&self) -> io::Result<u32> {
        match self.log_index {
            None => Ok(Self::TX_LEVEL_LOG_INDEX),
            Some(Self::TX_LEVEL_LOG_INDEX) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("block {} tx {}: log_index u32::MAX is reserved", self.block_number, self.tx_index),
            )),
            Some(i) => Ok(i),
        }
    }

    /// Header line: [`Self::COLUMNS`] joined by tabs.
    ///
    /// # Errors
    /// I/O errors of `w`.
    pub fn write_tsv_header(w: &mut impl Write) -> io::Result<()> {
        writeln!(w, "{}", Self::COLUMNS.join("\t"))
    }

    /// One TSV line in [`Self::COLUMNS`] order.
    ///
    /// # Errors
    /// I/O errors of `w`; a reserved `log_index` (see [`Self::log_index_column`]).
    pub fn write_tsv(&self, w: &mut impl Write) -> io::Result<()> {
        writeln!(
            w,
            "{}\t{}\t{:#x}\t{:#x}\t{}\t{}\t{:#x}\t{}\t{}\t{}\t{}\t{}\t{:#x}\t{}\t{}\t{}",
            self.block_number,
            self.tx_index,
            self.from_addr,
            self.to_addr,
            self.value_wei,
            self.kind.as_str(),
            self.tx_hash,
            self.log_index_column()?,
            HexOr(self.token, ""),
            HexOr(self.l1_token, ""),
            HexOr(self.gateway, ""),
            self.gateway_status.as_str(),
            self.l2_alias,
            self.tx_type,
            DecOr(self.l1_request_id, ""),
            HexOr(self.ticket_id, ""),
        )
    }
}

/// ClickHouse TSV NULL.
pub(crate) const NULL: &str = "\\N";

/// `Some(v)` as `{v:#x}`, `None` as the given placeholder.
pub(crate) struct HexOr<T>(pub(crate) Option<T>, pub(crate) &'static str);

impl<T: fmt::LowerHex> fmt::Display for HexOr<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Some(v) => write!(f, "{v:#x}"),
            None => f.write_str(self.1),
        }
    }
}

/// `Some(v)` as `{v}`, `None` as the given placeholder.
struct DecOr<T>(Option<T>, &'static str);

impl<T: fmt::Display> fmt::Display for DecOr<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Some(v) => write!(f, "{v}"),
            None => f.write_str(self.1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, b256};

    /// All `sql/*.sql` files in name order (= migration order), comments stripped.
    fn sql_lines() -> Vec<String> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sql");
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "sql"))
            .collect();
        files.sort();
        assert!(!files.is_empty(), "no sql files in {}", dir.display());
        files
            .iter()
            .flat_map(|p| std::fs::read_to_string(p).unwrap().lines().map(str::to_owned).collect::<Vec<_>>())
            .map(|l| l.split("--").next().unwrap_or_default().to_owned())
            .collect()
    }

    /// `(name, value)` pairs of the LAST `<column> Enum8(...)` definition across the migrations.
    fn last_enum8(column: &str) -> Vec<(String, i8)> {
        let mut last = None;
        for line in sql_lines() {
            let toks: Vec<&str> = line.split_whitespace().collect();
            if toks.windows(2).any(|w| w[0] == column && w[1].starts_with("Enum8(")) {
                let rest = &line[line.find("Enum8(").unwrap() + "Enum8(".len()..];
                last = Some(rest[..rest.find(')').unwrap()].to_owned());
            }
        }
        let body = last.unwrap_or_else(|| panic!("no `{column} Enum8(` in sql/"));
        body.split(',')
            .map(|p| {
                let (name, value) = p.split_once('=').unwrap();
                (name.trim().trim_matches('\'').to_owned(), value.trim().parse().unwrap())
            })
            .collect()
    }

    #[test]
    fn enum8_in_sql_equals_rust_enums() {
        let kinds: Vec<_> = EdgeKind::ALL.iter().map(|k| (k.as_str().to_owned(), k.enum8())).collect();
        assert_eq!(last_enum8("kind"), kinds);
        let statuses: Vec<_> = GatewayStatus::ALL.iter().map(|k| (k.as_str().to_owned(), k.enum8())).collect();
        assert_eq!(last_enum8("gateway_status"), statuses);
    }

    /// Column order of the LAST `CREATE TABLE hood.funding_edges*` (sql/003: built as
    /// funding_edges_003, then RENAME) equals COLUMNS, and no later ALTER changes the table.
    #[test]
    fn funding_edges_column_order_in_sql_equals_columns() {
        let lines = sql_lines();
        let is_create = |l: &str| l.contains("CREATE TABLE") && l.contains("hood.funding_edges");
        let start = lines.iter().rposition(|l| is_create(l)).expect("no CREATE TABLE hood.funding_edges*");
        let cols: Vec<&str> = lines[start + 1..]
            .iter()
            .map(|l| l.trim())
            .take_while(|l| !l.starts_with(')'))
            .filter_map(|l| l.split_whitespace().next())
            .collect();
        assert_eq!(cols, FundingEdge::COLUMNS);
        let later_alter = lines[start..].iter().any(|l| l.contains("ALTER TABLE hood.funding_edges"));
        assert!(!later_alter, "ALTER of funding_edges after its last CREATE: update this test");
    }

    /// Weak check (a column name defined in any table passes): catches typos in COLUMNS. The
    /// exact order is pinned by `funding_edges_column_order_in_sql_equals_columns` above.
    #[test]
    fn every_column_is_defined_in_sql() {
        let lines = sql_lines();
        for c in FundingEdge::COLUMNS {
            let defined = lines.iter().any(|l| {
                let t = l.trim_start();
                let t = t.strip_prefix("ADD COLUMN IF NOT EXISTS").map_or(t, str::trim_start);
                t.split_whitespace().next() == Some(c) && t.split_whitespace().nth(1).is_some()
            });
            assert!(defined, "column {c} is not defined in sql/");
        }
    }

    fn edge() -> FundingEdge {
        FundingEdge {
            block_number: 1,
            tx_index: 2,
            from_addr: address!("00000000000000000000000000000000000000aa"),
            to_addr: address!("00000000000000000000000000000000000000bb"),
            value_wei: U256::from(10u8),
            kind: EdgeKind::L1Eth,
            tx_hash: b256!("00000000000000000000000000000000000000000000000000000000000000cc"),
            log_index: None,
            token: None,
            l1_token: None,
            gateway: None,
            gateway_status: GatewayStatus::None,
            l2_alias: address!("00000000000000000000000000000000000000dd"),
            tx_type: 0x64,
            l1_request_id: Some(U256::from(7u8)),
            ticket_id: None,
        }
    }

    #[test]
    fn tx_level_edge_uses_the_marker_and_fills_every_column() {
        let mut out = Vec::new();
        FundingEdge::write_tsv_header(&mut out).unwrap();
        edge().write_tsv(&mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines[0].split('\t').count(), FundingEdge::COLUMNS.len());
        let cols: Vec<_> = lines[1].split('\t').collect();
        assert_eq!(cols.len(), FundingEdge::COLUMNS.len());
        assert_eq!(cols[7], "4294967295");
        assert_eq!(cols[5], "l1_eth");
        assert_eq!(cols[14], "7");
        assert!(!text.contains("\\N"), "funding_edges has no Nullable column");
    }

    #[test]
    fn reserved_log_index_is_an_error() {
        let e = FundingEdge { log_index: Some(u32::MAX), ..edge() };
        assert!(e.write_tsv(&mut Vec::new()).is_err());
        let e = FundingEdge { log_index: Some(0), ..edge() };
        assert_eq!(e.log_index_column().unwrap(), 0);
    }
}
