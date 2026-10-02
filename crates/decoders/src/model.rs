//! Typed model of one line of `data/blocks/blocks-*.jsonl.zst`: `{"number", "block"(full txs), "receipts"}`.
//!
//! Every decoder of this crate reads blocks through [`parse_block_line`]: the raw RPC strings are
//! parsed into typed values exactly once, here, and every hex/quantity parser of the crate lives in
//! this module only. The block/receipt consistency checks below duplicate the ones the enricher
//! runs before writing the file (`crates/enricher/src/blocks.rs`, the source of truth); the decoder
//! re-checks defensively because it reads files from disk, possibly from older runs.
//!
//! Broken input (bad hex, missing required field, receipts that do not match the transactions) is
//! an error with the block/tx position in its context, never a silent default.

use alloy_primitives::{hex, Address, Bytes, B256, U256};
use anyhow::{anyhow, bail, ensure, Context, Result};
use serde::Deserialize;

use crate::arbitrum::{TX_TYPE_DEPOSIT, TX_TYPE_RETRY, TX_TYPE_SUBMIT_RETRYABLE};

/// One block with each transaction merged with its receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// L2 block number.
    pub number: u64,
    /// `block.hash`.
    pub hash: B256,
    /// Transactions in block order (`txs[i].index == i`, checked).
    pub txs: Vec<Tx>,
}

impl Block {
    /// Position + context of every transaction, in block order.
    pub fn tx_ctxs(&self) -> impl Iterator<Item = TxCtx<'_>> {
        self.txs.iter().map(move |tx| TxCtx { block: self.number, tx })
    }
}

/// A transaction and the fields of its receipt the decoders need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tx {
    /// `transactionIndex` (= position in the block).
    pub index: u32,
    pub hash: B256,
    /// EIP-2718 type (`0x64`/`0x68`/`0x69` are Arbitrum L1 messages, see [`crate::arbitrum`]).
    pub ty: u8,
    pub from: Address,
    /// `None` for contract creation.
    pub to: Option<Address>,
    pub value: U256,
    /// Receipt `status == 0x1`. A receipt without `status` is an error, not a failure.
    pub status: bool,
    /// Arbitrum-specific fields of L1-message transactions.
    pub arb: ArbFields,
    /// Receipt logs in log-index order.
    pub logs: Vec<Log>,
}

/// Fields that only Arbitrum L1-message transactions carry. Required fields of each type are
/// checked at parse time, so a `Retry` always has its `ticket_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArbFields {
    /// Any other transaction type.
    None,
    /// `0x64` `ArbitrumDepositTx`.
    Deposit {
        /// `requestId` (delayed message number), if the node returned it.
        request_id: Option<U256>,
    },
    /// `0x68` `ArbitrumRetryTx`.
    Retry {
        /// `ticketId` (= hash of the `0x69`).
        ticket_id: B256,
        /// `maxRefund`.
        max_refund: U256,
        /// `refundTo` (= the `0x69` `FeeRefundAddr`).
        refund_to: Option<Address>,
    },
    /// `0x69` `ArbitrumSubmitRetryableTx`.
    SubmitRetryable {
        /// `depositValue` (L1 ETH minted to the alias).
        deposit_value: U256,
        /// `retryValue` (callvalue escrowed for the `0x68`).
        retry_value: U256,
        /// `refundTo` (`FeeRefundAddr`).
        refund_to: Option<Address>,
    },
}

/// One receipt log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Log {
    /// Emitter.
    pub address: Address,
    pub topics: Vec<B256>,
    pub data: Bytes,
    /// Block-level `logIndex`.
    pub index: u32,
}

/// Position + transaction context every output row needs: the `hood.*` ordering key
/// `(block_number, tx_index, log_index)` and `trader = tx.from` / `router = tx.to`.
#[derive(Debug, Clone, Copy)]
pub struct TxCtx<'a> {
    pub block: u64,
    pub tx: &'a Tx,
}

/// Parses and checks one line of `blocks-*.jsonl.zst`.
///
/// # Errors
/// Bad JSON or hex, a missing required field, `block.number` != `number`, receipts not aligned
/// with transactions (count, `transactionHash`, `transactionIndex`, `blockHash`), a log that
/// names another transaction, or log indices that are not strictly increasing over the block.
pub fn parse_block_line(line: &str) -> Result<Block> {
    let raw: RawLine = serde_json::from_str(line).context("block line is not {number, block, receipts}")?;
    let n = raw.number;
    ensure!(quantity_u64(&raw.block.number)? == n, "block {n}: block.number is {}", raw.block.number);
    let hash = parse_b256(&raw.block.hash).with_context(|| format!("block {n}: block.hash"))?;
    ensure!(
        raw.block.transactions.len() == raw.receipts.len(),
        "block {n}: {} receipts for {} transactions",
        raw.receipts.len(),
        raw.block.transactions.len()
    );
    let mut txs = Vec::with_capacity(raw.receipts.len());
    let mut last_log_index: Option<u32> = None;
    for (i, (tx, rc)) in raw.block.transactions.iter().zip(&raw.receipts).enumerate() {
        let tx = parse_tx(i, hash, tx, rc).with_context(|| format!("block {n} tx {i} {}", tx.hash))?;
        for l in &tx.logs {
            ensure!(
                last_log_index.is_none_or(|p| l.index > p),
                "block {n} tx {i}: logIndex {} after {last_log_index:?}",
                l.index
            );
            last_log_index = Some(l.index);
        }
        txs.push(tx);
    }
    Ok(Block { number: n, hash, txs })
}

fn parse_tx(position: usize, block_hash: B256, tx: &RawTx, rc: &RawReceipt) -> Result<Tx> {
    let hash = parse_b256(&tx.hash)?;
    ensure!(parse_b256(&rc.transaction_hash)? == hash, "receipt {} does not match the tx", rc.transaction_hash);
    let index = quantity_u32(&tx.transaction_index)?;
    ensure!(usize::try_from(index).ok() == Some(position), "transactionIndex {index} at position {position}");
    if let Some(ri) = &rc.transaction_index {
        ensure!(quantity_u32(ri)? == index, "receipt transactionIndex {ri}");
    }
    if let Some(bh) = &rc.block_hash {
        ensure!(parse_b256(bh)? == block_hash, "receipt blockHash {bh}");
    }
    let ty = u8::try_from(quantity_u64(&tx.tx_type)?).map_err(|_| anyhow!("tx type {}", tx.tx_type))?;
    let status = match quantity_u64(&rc.status)? {
        0 => false,
        1 => true,
        s => bail!("receipt status {s}"),
    };
    let logs = rc
        .logs
        .iter()
        .map(|l| parse_log(l, hash).with_context(|| format!("log {}", l.log_index)))
        .collect::<Result<_>>()?;
    Ok(Tx {
        index,
        hash,
        ty,
        from: parse_addr(&tx.from)?,
        to: tx.to.as_deref().map(parse_addr).transpose()?,
        value: quantity_u256(&tx.value)?,
        status,
        arb: parse_arb(ty, tx)?,
        logs,
    })
}

fn parse_arb(ty: u8, tx: &RawTx) -> Result<ArbFields> {
    let refund_to = || tx.refund_to.as_deref().map(parse_addr).transpose();
    Ok(match ty {
        TX_TYPE_DEPOSIT => ArbFields::Deposit { request_id: tx.request_id.as_deref().map(quantity_u256).transpose()? },
        TX_TYPE_RETRY => ArbFields::Retry {
            ticket_id: parse_b256(required(tx.ticket_id.as_deref(), "ticketId")?)?,
            max_refund: quantity_u256(required(tx.max_refund.as_deref(), "maxRefund")?)?,
            refund_to: refund_to()?,
        },
        TX_TYPE_SUBMIT_RETRYABLE => ArbFields::SubmitRetryable {
            deposit_value: quantity_u256(required(tx.deposit_value.as_deref(), "depositValue")?)?,
            retry_value: quantity_u256(required(tx.retry_value.as_deref(), "retryValue")?)?,
            refund_to: refund_to()?,
        },
        _ => ArbFields::None,
    })
}

fn parse_log(l: &RawLog, tx_hash: B256) -> Result<Log> {
    if let Some(h) = &l.transaction_hash {
        ensure!(parse_b256(h)? == tx_hash, "log transactionHash {h}");
    }
    Ok(Log {
        address: parse_addr(&l.address)?,
        topics: l.topics.iter().map(|t| parse_b256(t)).collect::<Result<_>>()?,
        data: parse_bytes(&l.data)?,
        index: quantity_u32(&l.log_index)?,
    })
}

fn required<'a>(v: Option<&'a str>, field: &str) -> Result<&'a str> {
    v.ok_or_else(|| anyhow!("missing field {field}"))
}

// ---------------------------------------------------------------------------
// Hex helpers on alloy types. The quantity grammar (0x + >=1 hex digit, leading zeros ok) must
// match hood_core::hex::parse_quantity — keep them in sync. Not moved to hood-core: it has no
// alloy dependency and the decoders are the only alloy-based consumer.
// ---------------------------------------------------------------------------

/// Hex digits after a mandatory lowercase `0x` prefix. Every character is checked here:
/// `const-hex` decoding would accept a second `0x` (`"0x0x…"`) and ruint's `from_str_radix`
/// skips `_` (`"0x_"` would parse as 0).
fn hex_digits<'a>(s: &'a str, what: &str) -> Result<&'a str> {
    let h = s.strip_prefix("0x").ok_or_else(|| anyhow!("{what} {s:?} without 0x"))?;
    ensure!(h.bytes().all(|b| b.is_ascii_hexdigit()), "bad {what} {s:?}");
    Ok(h)
}

/// RPC quantity: `0x` followed by at least one hex digit (leading zeros allowed). `""`, `"0x"`
/// and a missing prefix are errors (the enricher requires the prefix too,
/// `crates/enricher/src/logs.rs`).
pub(crate) fn quantity_u256(s: &str) -> Result<U256> {
    let h = hex_digits(s, "quantity")?;
    ensure!(!h.is_empty(), "empty quantity {s:?}");
    U256::from_str_radix(h, 16).map_err(|e| anyhow!("bad quantity {s:?}: {e}"))
}

pub(crate) fn quantity_u64(s: &str) -> Result<u64> {
    u64::try_from(quantity_u256(s)?).map_err(|_| anyhow!("quantity {s:?} does not fit u64"))
}

pub(crate) fn quantity_u32(s: &str) -> Result<u32> {
    u32::try_from(quantity_u256(s)?).map_err(|_| anyhow!("quantity {s:?} does not fit u32"))
}

/// Address: `0x` + exactly 40 hex digits (any case; the EIP-55 checksum is not checked).
pub(crate) fn parse_addr(s: &str) -> Result<Address> {
    let h = hex_digits(s, "address")?;
    ensure!(h.len() == 2 * Address::len_bytes(), "bad address {s:?}: expected 40 hex digits");
    Ok(Address::from_slice(&hex::decode(h).map_err(|e| anyhow!("bad address {s:?}: {e}"))?))
}

/// 32-byte hash: `0x` + exactly 64 hex digits.
pub(crate) fn parse_b256(s: &str) -> Result<B256> {
    let h = hex_digits(s, "hash")?;
    ensure!(h.len() == 2 * B256::len_bytes(), "bad hash {s:?}: expected 64 hex digits");
    Ok(B256::from_slice(&hex::decode(h).map_err(|e| anyhow!("bad hash {s:?}: {e}"))?))
}

/// RPC data: `0x` + an even number of hex digits (`"0x"` = empty).
pub(crate) fn parse_bytes(s: &str) -> Result<Bytes> {
    let h = hex_digits(s, "data")?;
    Ok(hex::decode(h).map_err(|e| anyhow!("bad data {s:?}: {e}"))?.into())
}

// ---------------------------------------------------------------------------
// Raw serde layer (private): only the fields the decoders read; everything else is ignored.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct RawLine {
    number: u64,
    block: RawBlock,
    receipts: Vec<RawReceipt>,
}

#[derive(Deserialize)]
struct RawBlock {
    number: String,
    hash: String,
    transactions: Vec<RawTx>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawTx {
    hash: String,
    #[serde(rename = "type")]
    tx_type: String,
    from: String,
    #[serde(default)]
    to: Option<String>,
    value: String,
    transaction_index: String,
    // 0x64
    #[serde(default)]
    request_id: Option<String>,
    // 0x68
    #[serde(default)]
    ticket_id: Option<String>,
    #[serde(default)]
    max_refund: Option<String>,
    // 0x68 and 0x69 (0x69: FeeRefundAddr; 0x68: RefundTo = the same FeeRefundAddr)
    #[serde(default)]
    refund_to: Option<String>,
    // 0x69
    #[serde(default)]
    deposit_value: Option<String>,
    #[serde(default)]
    retry_value: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawReceipt {
    transaction_hash: String,
    #[serde(default)]
    transaction_index: Option<String>,
    #[serde(default)]
    block_hash: Option<String>,
    /// Required: on Robinhood Chain every receipt has it (2026-10-02: 2 712 blocks of
    /// data/blocks + data/samples, and the 017 fixtures).
    status: String,
    logs: Vec<RawLog>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawLog {
    address: String,
    topics: Vec<String>,
    data: String,
    log_index: String,
    #[serde(default)]
    transaction_hash: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantity_requires_prefix_and_digits() {
        assert_eq!(quantity_u256("0x0").unwrap(), U256::ZERO);
        assert_eq!(quantity_u256("0x1f").unwrap(), U256::from(31u8));
        assert!(quantity_u256("").is_err());
        assert!(quantity_u256("0x").is_err());
        assert!(quantity_u256("1f").is_err());
        assert!(quantity_u256("0xzz").is_err());
        assert!(quantity_u256("0x_").is_err());
        assert!(quantity_u256("0x1_0").is_err());
        assert!(quantity_u256("0x 1").is_err());
        assert!(quantity_u32("0x100000000").is_err());
        assert_eq!(quantity_u64("0xffffffffffffffff").unwrap(), u64::MAX);
    }

    #[test]
    fn bytes_require_prefix() {
        assert_eq!(parse_bytes("0x").unwrap().len(), 0);
        assert_eq!(parse_bytes("0x00ff").unwrap().as_ref(), &[0u8, 0xff]);
        assert!(parse_bytes("00ff").is_err());
        assert!(parse_bytes("0x0").is_err());
        assert!(parse_bytes("0x0x00").is_err());
    }

    #[test]
    fn addresses_and_hashes_require_prefix() {
        let a = "0x00000000000000000000000000000000000000aB";
        assert_eq!(parse_addr(a).unwrap(), Address::with_last_byte(0xab));
        assert!(parse_addr(&a[2..]).is_err());
        assert!(parse_addr("0X00000000000000000000000000000000000000ab").is_err());
        assert!(parse_addr("0x0x000000000000000000000000000000000000ab").is_err());
        assert!(parse_addr("0x000000000000000000000000000000000000ab").is_err());
        assert!(parse_addr("0x0000000000000000000000000000000000000000ab").is_err());
        assert!(parse_addr("0x00000000000000000000000000000000000000_b").is_err());
        let h = format!("0x{}", "11".repeat(32));
        assert_eq!(parse_b256(&h).unwrap(), B256::repeat_byte(0x11));
        assert!(parse_b256(&h[2..]).is_err());
        assert!(parse_b256(&h[..65]).is_err());
        assert!(parse_b256(&format!("0x0x{}", "11".repeat(31))).is_err());
        assert!(parse_b256("").is_err());
    }
}
