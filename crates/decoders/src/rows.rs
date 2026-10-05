//! Output row types and their TSV form: the single place that maps decoder output to
//! ClickHouse columns ([`FundingEdge`] -> `hood.funding_edges`, [`SwapRow`] -> `hood.swaps`). Decoder-specific scanner outputs (e.g. `l1_inflows::UnaccountedFlow`)
//! keep their TSV next to their type and reuse the helpers below.
//!
//! TSV conventions (ClickHouse `TabSeparated`): addresses and hashes are lowercase `0x…`,
//! `U256` is decimal, an absent value of a `String DEFAULT ''` column is the empty string, an
//! absent value of a column that is (or would be) `Nullable` is `\N`, a `Float64` is Rust's
//! shortest round-trip decimal (never an exponent) and an unknown `Float64` is `nan`. No value
//! written here can contain a tab, newline or backslash, so no escaping is needed.

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
    /// Types in ClickHouse: `block_number UInt64`, `tx_index UInt32`, `from_addr`/`to_addr String`,
    /// `value_wei String` (decimal), `kind Enum8`, `tx_hash String`, `log_index UInt32` (no NULL),
    /// `token`/`l1_token`/`gateway String DEFAULT ''`, `gateway_status Enum8`, `l2_alias String`,
    /// `tx_type UInt8`, `l1_request_id String DEFAULT ''` (decimal), `ticket_id String DEFAULT ''`.
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

/// `hood.swaps.venue` / `hood.tokens.venue` (Enum8, sql/004). Discriminants are the Enum8 values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(i8)]
pub enum Venue {
    /// Pons v1 (per-token Uniswap v3 pools). Assigned only from a pool registry entry.
    PonsV1 = 1,
    /// Pons v2 bonding curve (its own events, not decoded yet).
    PonsV2Curve = 2,
    UniV3 = 3,
    UniV4 = 4,
    /// pools.trade. Assigned only from a pool registry entry.
    PoolsTrade = 5,
    /// Known pool, venue not established (e.g. a v4 pool with a hook that is not in the registry).
    Other = 9,
}

impl Venue {
    /// Every variant, in Enum8 value order.
    pub const ALL: [Self; 6] =
        [Self::PonsV1, Self::PonsV2Curve, Self::UniV3, Self::UniV4, Self::PoolsTrade, Self::Other];

    /// Enum8 name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PonsV1 => "pons_v1",
            Self::PonsV2Curve => "pons_v2_curve",
            Self::UniV3 => "uni_v3",
            Self::UniV4 => "uni_v4",
            Self::PoolsTrade => "pools_trade",
            Self::Other => "other",
        }
    }

    /// Inverse of [`Self::as_str`].
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// Enum8 value.
    #[must_use]
    pub const fn enum8(self) -> i8 {
        self as i8
    }
}

/// `hood.swaps.side` (Enum8, sql/001): the trader's side relative to `token` (`buy` = the trader
/// received the token, references/data-model.md "Соглашения").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(i8)]
pub enum Side {
    Buy = 1,
    Sell = 2,
}

impl Side {
    /// Every variant, in Enum8 value order.
    pub const ALL: [Self; 2] = [Self::Buy, Self::Sell];

    /// Enum8 name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Buy => "buy",
            Self::Sell => "sell",
        }
    }

    /// Enum8 value.
    #[must_use]
    pub const fn enum8(self) -> i8 {
        self as i8
    }
}

/// `hood.swaps.pool`: the v3 pool address or the v4 pool id (both lowercase `0x…`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SwapPool {
    V3(Address),
    V4(B256),
}

impl fmt::Display for SwapPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::V3(a) => write!(f, "{a:#x}"),
            Self::V4(id) => write!(f, "{id:#x}"),
        }
    }
}

/// One row of `hood.swaps`, built by [`crate::swap_rows::swap_row`] from a decoded swap and the
/// caller's pool/token registries. Every field is in the table; the registry status of the
/// entries the row was built from is not (the caller loads only rows of at least the status it
/// asked for).
///
/// Semantics (references/data-model.md, "hood.swaps"):
/// - amounts are unsigned, the direction is `side`;
/// - v4 amounts are the AMM leg of the `PoolManager` (`Swap` is emitted before `afterSwap`), not
///   necessarily what the trader paid/received in pools with hooks;
/// - `price` is `nan` when the token decimals are unknown, `fee_quote` is `nan` when the fee is
///   unknown (v3 pool without a fee tier in the registry); neither is ever ±inf.
#[derive(Debug, Clone, PartialEq)]
pub struct SwapRow {
    pub block_number: u64,
    pub tx_index: u32,
    pub log_index: u32,
    pub venue: Venue,
    pub pool: SwapPool,
    /// The traded (non-quote) currency; `0x000…0` = native ETH (v4).
    pub token: Address,
    /// The quote currency (WETH, native ETH, USDG, …).
    pub quote: Address,
    /// `tx.from`.
    pub trader: Address,
    /// `tx.to`; `None` = contract creation (written as `''`).
    pub router: Option<Address>,
    pub side: Side,
    /// |pool-side token delta|, raw units.
    pub token_amount_raw: U256,
    /// |pool-side quote delta|, raw units.
    pub quote_amount_raw: U256,
    /// `quote_amount_raw / 10^quote_decimals`.
    pub quote_amount: f64,
    /// Quote per whole token (execution price); `nan` = token decimals unknown.
    pub price: f64,
    /// Pool fee of this swap in quote units; `nan` = fee unknown.
    pub fee_quote: f64,
}

impl SwapRow {
    /// Column order of the TSV line (and of the loader's `INSERT … (columns) FORMAT TSV`); equal
    /// to the last `CREATE TABLE hood.swaps*` (sql/004, checked by a test). Types in ClickHouse:
    /// `block_number UInt64`, `tx_index`/`log_index UInt32`, `venue Enum8`, `pool`/`token`/`quote`/
    /// `trader String`, `router String DEFAULT ''`, `side Enum8`, `token_amount_raw`/
    /// `quote_amount_raw String` (decimal), `quote_amount`/`price`/`fee_quote Float64`.
    pub const COLUMNS: [&'static str; 15] = [
        "block_number",
        "tx_index",
        "log_index",
        "venue",
        "pool",
        "token",
        "quote",
        "trader",
        "router",
        "side",
        "token_amount_raw",
        "quote_amount_raw",
        "quote_amount",
        "price",
        "fee_quote",
    ];

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
    /// I/O errors of `w`.
    pub fn write_tsv(&self, w: &mut impl Write) -> io::Result<()> {
        writeln!(
            w,
            "{}\t{}\t{}\t{}\t{}\t{:#x}\t{:#x}\t{:#x}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            self.block_number,
            self.tx_index,
            self.log_index,
            self.venue.as_str(),
            self.pool,
            self.token,
            self.quote,
            self.trader,
            HexOr(self.router, ""),
            self.side.as_str(),
            self.token_amount_raw,
            self.quote_amount_raw,
            F64(self.quote_amount),
            F64(self.price),
            F64(self.fee_quote),
        )
    }
}

/// `Float64` for ClickHouse TSV: shortest round-trip decimal, `nan` / `inf` / `-inf` for the
/// special values (ClickHouse's text names; Rust would write `NaN`).
pub(crate) struct F64(pub(crate) f64);

impl fmt::Display for F64 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let v = self.0;
        if v.is_nan() {
            f.write_str("nan")
        } else if v.is_infinite() {
            f.write_str(if v > 0.0 { "inf" } else { "-inf" })
        } else {
            write!(f, "{v}")
        }
    }
}

/// ClickHouse TSV `NULL`.
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
pub(crate) struct DecOr<T>(pub(crate) Option<T>, pub(crate) &'static str);

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

    /// `(name, value)` pairs of an `Enum8(...)` definition on `line`.
    fn enum8_values(line: &str) -> Vec<(String, i8)> {
        let rest = &line[line.find("Enum8(").unwrap() + "Enum8(".len()..];
        rest[..rest.find(')').unwrap()]
            .split(',')
            .map(|p| {
                let (name, value) = p.split_once('=').unwrap();
                (name.trim().trim_matches('\'').to_owned(), value.trim().parse().unwrap())
            })
            .collect()
    }

    fn is_enum8_of(line: &str, column: &str) -> bool {
        let toks: Vec<&str> = line.split_whitespace().collect();
        toks.windows(2).any(|w| w[0] == column && w[1].starts_with("Enum8("))
    }

    /// `(name, value)` pairs of the LAST `<column> Enum8(...)` definition across the migrations.
    fn last_enum8(column: &str) -> Vec<(String, i8)> {
        let last = sql_lines().into_iter().rfind(|l| is_enum8_of(l, column));
        enum8_values(&last.unwrap_or_else(|| panic!("no `{column} Enum8(` in sql/")))
    }

    /// Column lines of the LAST `CREATE TABLE hood.<table>[_NNN]` (migrations build `<table>_NNN`
    /// and RENAME it), checking that no later `ALTER TABLE hood.<table>` changes it.
    fn last_create_table(table: &str) -> Vec<String> {
        let lines = sql_lines();
        let name = format!("hood.{table}");
        let is_create = |l: &str| {
            l.contains("CREATE TABLE")
                && l.split_whitespace().any(|t| {
                    t.strip_prefix(&name).is_some_and(|r| {
                        r.is_empty()
                            || r == "("
                            || r.strip_prefix('_').is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))
                    })
                })
        };
        let start = lines.iter().rposition(|l| is_create(l)).unwrap_or_else(|| panic!("no CREATE TABLE {name}*"));
        let later_alter = lines[start..].iter().any(|l| l.contains(&format!("ALTER TABLE {name} ")));
        assert!(!later_alter, "ALTER of {table} after its last CREATE: update this test");
        lines[start + 1..].iter().map(|l| l.trim().to_owned()).take_while(|l| !l.starts_with(')')).collect()
    }

    fn column_names(body: &[String]) -> Vec<&str> {
        body.iter().filter_map(|l| l.split_whitespace().next()).collect()
    }

    fn rust_enum8<T: Copy>(all: &[T], name: fn(T) -> &'static str, value: fn(T) -> i8) -> Vec<(String, i8)> {
        all.iter().map(|&v| (name(v).to_owned(), value(v))).collect()
    }

    #[test]
    fn enum8_in_sql_equals_rust_enums() {
        assert_eq!(last_enum8("kind"), rust_enum8(&EdgeKind::ALL, EdgeKind::as_str, EdgeKind::enum8));
        assert_eq!(
            last_enum8("gateway_status"),
            rust_enum8(&GatewayStatus::ALL, GatewayStatus::as_str, GatewayStatus::enum8)
        );
    }

    /// `venue` and `side` of the current `hood.swaps`, and `venue` of `hood.tokens` (one type for
    /// one concept, sql/004), equal the Rust enums.
    #[test]
    fn swaps_enum8_in_sql_equals_rust_enums() {
        let venues = rust_enum8(&Venue::ALL, Venue::as_str, Venue::enum8);
        for table in ["swaps", "tokens"] {
            let body = last_create_table(table);
            let line = body.iter().find(|l| is_enum8_of(l, "venue")).unwrap_or_else(|| panic!("{table}.venue"));
            assert_eq!(enum8_values(line), venues, "{table}.venue");
        }
        let body = last_create_table("swaps");
        let side = body.iter().find(|l| is_enum8_of(l, "side")).expect("swaps.side");
        assert_eq!(enum8_values(side), rust_enum8(&Side::ALL, Side::as_str, Side::enum8));
        for v in Venue::ALL {
            assert_eq!(Venue::parse(v.as_str()), Some(v));
        }
        assert_eq!(Venue::parse("uniswap_v3"), None);
    }

    /// Column order of the LAST `CREATE TABLE hood.funding_edges*` (sql/003: built as
    /// funding_edges_003, then RENAME) equals COLUMNS, and no later ALTER changes the table.
    #[test]
    fn funding_edges_column_order_in_sql_equals_columns() {
        assert_eq!(column_names(&last_create_table("funding_edges")), FundingEdge::COLUMNS);
    }

    /// Column order of the LAST `CREATE TABLE hood.swaps*` (sql/004: swaps_004, then RENAME)
    /// equals COLUMNS, and no later ALTER changes the table.
    #[test]
    fn swaps_column_order_in_sql_equals_columns() {
        assert_eq!(column_names(&last_create_table("swaps")), SwapRow::COLUMNS);
    }

    /// Weak check (a column name defined in any table passes): catches typos in COLUMNS. The
    /// exact order is pinned by the `*_column_order_in_sql_equals_columns` tests above.
    #[test]
    fn every_column_is_defined_in_sql() {
        let lines = sql_lines();
        for c in FundingEdge::COLUMNS.into_iter().chain(SwapRow::COLUMNS) {
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

    fn swap_row() -> SwapRow {
        SwapRow {
            block_number: 1,
            tx_index: 2,
            log_index: 3,
            venue: Venue::UniV4,
            pool: SwapPool::V4(B256::repeat_byte(0x11)),
            token: address!("00000000000000000000000000000000000000aa"),
            quote: Address::ZERO,
            trader: address!("00000000000000000000000000000000000000bb"),
            router: None,
            side: Side::Sell,
            token_amount_raw: U256::from(5u8),
            quote_amount_raw: U256::from(7u8),
            quote_amount: 7e-18,
            price: f64::NAN,
            fee_quote: 0.0,
        }
    }

    #[test]
    fn swap_row_tsv_fills_every_column() {
        let mut out = Vec::new();
        SwapRow::write_tsv_header(&mut out).unwrap();
        swap_row().write_tsv(&mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines[0].split('\t').collect::<Vec<_>>(), SwapRow::COLUMNS);
        let cols: Vec<_> = lines[1].split('\t').collect();
        assert_eq!(cols.len(), SwapRow::COLUMNS.len());
        assert_eq!(cols[3], "uni_v4");
        assert_eq!(cols[4], format!("0x{}", "11".repeat(32)));
        assert_eq!(cols[6], "0x0000000000000000000000000000000000000000");
        assert_eq!(cols[8], "", "router None = ''");
        assert_eq!(cols[9], "sell");
        assert_eq!(&cols[12..], ["0.000000000000000007", "nan", "0"]);
        assert!(!text.contains("\\N"), "swaps has no Nullable column after 004");
        let r = SwapRow {
            pool: SwapPool::V3(address!("00000000000000000000000000000000000000cc")),
            router: Some(Address::with_last_byte(0xdd)),
            ..swap_row()
        };
        let mut out = Vec::new();
        r.write_tsv(&mut out).unwrap();
        let line = String::from_utf8(out).unwrap();
        let cols: Vec<_> = line.trim_end().split('\t').collect();
        assert_eq!(cols[4], "0x00000000000000000000000000000000000000cc");
        assert_eq!(cols[8], "0x00000000000000000000000000000000000000dd");
    }

    #[test]
    fn f64_column_text() {
        assert_eq!(F64(f64::NAN).to_string(), "nan");
        assert_eq!(F64(f64::INFINITY).to_string(), "inf");
        assert_eq!(F64(f64::NEG_INFINITY).to_string(), "-inf");
        assert_eq!(F64(2657.5).to_string(), "2657.5");
        assert_eq!(F64(1e21).to_string(), "1000000000000000000000");
        assert_eq!(F64(0.1).to_string().parse::<f64>().unwrap(), 0.1);
    }
}
