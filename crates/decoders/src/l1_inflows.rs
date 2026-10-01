//! Money coming in from L1 (Ethereum) — the first edge of the funding graph.
//!
//! Input: one line of `data/blocks/blocks-*.jsonl.zst` (`{"number", "block"(full txs), "receipts"}`).
//! Output: [`L1Inflow`] rows (map to `hood.funding_edges` via [`L1Inflow::funding_edge`]),
//! [`UnaccountedFlow`] records for L1 money that reaches L2 addresses but is NOT an inflow row,
//! and [`Counters`].
//!
//! What counts as an inflow (references/data-model.md, "L1-сообщения фида"; Nitro v3.11.4
//! `arbos/tx_processor.go`):
//! - `0x64` ArbitrumDepositTx (feed kind 12, also kind 7): ETH `value` minted to the alias
//!   `from` and transferred to `to` without EVM; receipt has no logs. Row only when `status == 1`.
//! - `0x68` ArbitrumRetryTx (auto-redeem after `0x69` in kind 9 blocks, or a manual redeem in any
//!   block) with `status == 1` and `value > 0`: callvalue leaves the ticket escrow and is sent
//!   to `to` (= `retryTo`).
//! - `0x68` that calls an Arbitrum token gateway: the gateway emits `DepositFinalized` (we only
//!   trust it when the emitter is the tx's own `to`), the recipient and amount come from that log,
//!   the L2 token from the matching `Transfer`. Row kind [`InflowKind::BridgedToken`]; no ETH row
//!   for the gateway contract.
//! - `0x69` SubmitRetryable is NEVER an inflow row: its `depositValue` is minted to the alias and
//!   split into fees, refunds and the escrowed callvalue that the `0x68` later moves. Counting it
//!   too would double count.
//!
//! Source of every row: `l1_sender` = unalias(`tx.from`). For bridged tokens the economically
//! relevant L1 depositor is `DepositFinalized.from` (unalias(`tx.from`) is the L1 gateway).
//!
//! Gateway trust: the gateway set is an input ([`GatewayRegistry`]). [`GatewayRegistry::builtin`]
//! holds only `verified` entries of `references/contracts.md` (today: the L2 WETH gateway and
//! L2 WETH); callers may add `observed` entries for research. Rows from gateways that are not in
//! the registry are still emitted but carry `registry: None` and must be filtered downstream.

use std::collections::{BTreeMap, HashMap};

use alloy_primitives::{address, hex, Address, B256, U256};
use alloy_sol_types::SolEvent;
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

use crate::{token_bridge, TOPIC_DEPOSIT_FINALIZED, TOPIC_TRANSFER};

pub const TX_TYPE_DEPOSIT: u8 = 0x64;
pub const TX_TYPE_RETRY: u8 = 0x68;
pub const TX_TYPE_SUBMIT_RETRYABLE: u8 = 0x69;

/// L2 WETH gateway (proxy). `verified` in references/contracts.md (Mihail, 2026-10-01: docs.robinhood.com
/// /chain/protocol-contracts "L2 Weth Gateway" + Blockscout proxy verified; implementation not checked).
pub const L2_WETH_GATEWAY: Address = address!("1d187c3e2da52d72bc9c41e3aba0fdfa6a7bf055");
/// L2 WETH (aeWETH, proxy). `verified` in references/contracts.md (Mihail, 2026-10-01: docs.robinhood.com
/// /chain/contracts "WETH" + Blockscout proxy verified; implementation not checked).
pub const L2_WETH: Address = address!("0bd7d308f8e1639fab988df18a8011f41eacad73");

/// Nitro `AddressAliasOffset` (protocol constant, not a contract): L2 alias = L1 address + offset
/// mod 2^160. Nitro v3.11.4 `arbos/util/util.go` `init()` / `RemapL1Address`.
pub const ALIAS_OFFSET: Address = address!("1111000000000000000000000000000000001111");

/// L1 address behind an L2 alias: `(alias - 0x1111…1111) mod 2^160`
/// (Nitro `InverseRemapL1Address`).
pub fn unalias(alias: Address) -> Address {
    let modulus = U256::from(1u8) << 160;
    let a = U256::from_be_slice(alias.as_slice());
    let off = U256::from_be_slice(ALIAS_OFFSET.as_slice());
    let r: U256 = (a + modulus - off) % modulus;
    Address::from_slice(&r.to_be_bytes::<32>()[12..])
}

/// Inverse of [`unalias`] (Nitro `RemapL1Address`).
pub fn alias(l1: Address) -> Address {
    let modulus = U256::from(1u8) << 160;
    let a = U256::from_be_slice(l1.as_slice());
    let off = U256::from_be_slice(ALIAS_OFFSET.as_slice());
    let r: U256 = (a + off) % modulus;
    Address::from_slice(&r.to_be_bytes::<32>()[12..])
}

// ---------------------------------------------------------------------------
// Raw block line (only the fields this decoder reads; everything else is ignored).
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct BlockLine {
    pub number: u64,
    pub block: RpcBlock,
    pub receipts: Vec<RpcReceipt>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcBlock {
    pub number: String,
    pub transactions: Vec<RpcTx>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcTx {
    pub hash: String,
    #[serde(rename = "type")]
    pub tx_type: String,
    pub from: String,
    #[serde(default)]
    pub to: Option<String>,
    pub value: String,
    pub transaction_index: String,
    // 0x64
    #[serde(default)]
    pub request_id: Option<String>,
    // 0x68
    #[serde(default)]
    pub ticket_id: Option<String>,
    #[serde(default)]
    pub max_refund: Option<String>,
    // 0x68 and 0x69 (0x69: FeeRefundAddr; 0x68: RefundTo = the same FeeRefundAddr)
    #[serde(default)]
    pub refund_to: Option<String>,
    // 0x69
    #[serde(default)]
    pub deposit_value: Option<String>,
    #[serde(default)]
    pub retry_value: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcReceipt {
    pub transaction_hash: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub logs: Vec<RpcLog>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcLog {
    pub address: String,
    pub topics: Vec<String>,
    pub data: String,
    pub log_index: String,
}

fn hex_u256(s: &str) -> Result<U256> {
    let h = s.strip_prefix("0x").unwrap_or(s);
    if h.is_empty() {
        return Ok(U256::ZERO);
    }
    U256::from_str_radix(h, 16).map_err(|e| anyhow!("bad quantity {s:?}: {e}"))
}

fn hex_u64(s: &str) -> Result<u64> {
    let v = hex_u256(s)?;
    u64::try_from(v).map_err(|_| anyhow!("quantity {s:?} does not fit u64"))
}

fn parse_addr(s: &str) -> Result<Address> {
    s.parse::<Address>().map_err(|e| anyhow!("bad address {s:?}: {e}"))
}

fn parse_b256(s: &str) -> Result<B256> {
    s.parse::<B256>().map_err(|e| anyhow!("bad hash {s:?}: {e}"))
}

fn opt_u256(v: &Option<String>, field: &str) -> Result<U256> {
    hex_u256(v.as_deref().ok_or_else(|| anyhow!("missing field {field}"))?)
}

// ---------------------------------------------------------------------------
// Gateway registry (caller-supplied; nothing hardcoded).
// ---------------------------------------------------------------------------

/// Status of a registry entry, mirrors `references/contracts.md`. Only `verified` may be used for
/// final conclusions; `observed` is research-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RegistryStatus {
    Observed,
    Verified,
}

impl RegistryStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RegistryStatus::Observed => "observed",
            RegistryStatus::Verified => "verified",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayEntry {
    /// L2 gateway contract that emits `DepositFinalized`.
    pub gateway: Address,
    /// Expected L2 token minted/transferred by this gateway, if the gateway is single-token
    /// (e.g. the WETH gateway). `None` = any token (standard ERC-20 gateway).
    pub l2_token: Option<Address>,
    pub status: RegistryStatus,
}

#[derive(Debug, Clone, Default)]
pub struct GatewayRegistry {
    entries: Vec<GatewayEntry>,
}

impl GatewayRegistry {
    pub fn new(entries: Vec<GatewayEntry>) -> Self {
        Self { entries }
    }

    /// Only `verified` entries from references/contracts.md. The L2 Gateway Router and the
    /// L1 WETH gateway are `observed` (2026-10-01) and are deliberately absent; the router does
    /// not emit `DepositFinalized` on L2 anyway, and the L1 gateway is an Ethereum contract.
    pub fn builtin() -> Self {
        Self {
            entries: vec![GatewayEntry {
                gateway: L2_WETH_GATEWAY,
                l2_token: Some(L2_WETH),
                status: RegistryStatus::Verified,
            }],
        }
    }

    /// Adds entries from `other`; an entry for a gateway already present is an error
    /// (no silent override of a verified entry).
    pub fn extend(&mut self, other: GatewayRegistry) -> Result<()> {
        for e in other.entries {
            if self.get(&e.gateway).is_some() {
                bail!("gateway {} is already in the registry", e.gateway);
            }
            self.entries.push(e);
        }
        Ok(())
    }

    pub fn entries(&self) -> &[GatewayEntry] {
        &self.entries
    }

    pub fn get(&self, gateway: &Address) -> Option<&GatewayEntry> {
        self.entries.iter().find(|e| &e.gateway == gateway)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// TSV: `gateway \t l2_token|- \t verified|observed [\t note…]`. `#` comments and blank lines
    /// are skipped. Statuses `todo`/`rejected` (or anything else) are an error: such addresses
    /// must not be fed to the decoder at all.
    pub fn parse_tsv(text: &str) -> Result<Self> {
        let mut entries = Vec::new();
        for (i, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let cols: Vec<&str> = line.split('\t').map(str::trim).collect();
            if cols.len() < 3 {
                bail!("gateway registry line {}: expected >= 3 tab-separated columns", i + 1);
            }
            let gateway = parse_addr(cols[0]).with_context(|| format!("line {}", i + 1))?;
            let l2_token = match cols[1] {
                "-" | "" => None,
                s => Some(parse_addr(s).with_context(|| format!("line {}", i + 1))?),
            };
            let status = match cols[2] {
                "verified" => RegistryStatus::Verified,
                "observed" => RegistryStatus::Observed,
                other => bail!("gateway registry line {}: status {other:?} not allowed (verified|observed)", i + 1),
            };
            if entries.iter().any(|e: &GatewayEntry| e.gateway == gateway) {
                bail!("gateway registry line {}: duplicate gateway {gateway}", i + 1);
            }
            entries.push(GatewayEntry { gateway, l2_token, status });
        }
        Ok(Self { entries })
    }
}

// ---------------------------------------------------------------------------
// Output types.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InflowKind {
    /// `0x64` ArbitrumDepositTx.
    EthDeposit,
    /// `0x68` RetryTx with value > 0 to a non-gateway `to`.
    RetryEth,
    /// `0x68` into a token gateway, from `DepositFinalized`.
    BridgedToken,
}

impl InflowKind {
    pub fn as_str(self) -> &'static str {
        match self {
            InflowKind::EthDeposit => "eth_deposit",
            InflowKind::RetryEth => "retry_eth",
            InflowKind::BridgedToken => "bridged_token",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenInflow {
    /// Emitter of `DepositFinalized` (= the 0x68 `to`).
    pub gateway: Address,
    /// `DepositFinalized.l1Token` (token address on Ethereum).
    pub l1_token: Address,
    /// Emitter of the matching `Transfer(gateway|0x0 -> to, amount)` in the same receipt.
    /// `None` if no such Transfer was found.
    pub l2_token: Option<Address>,
    /// `DepositFinalized.from`: the depositor on L1.
    pub l1_from: Address,
    /// Registry status of `gateway`; `None` = gateway not in the caller's registry.
    pub registry: Option<RegistryStatus>,
    /// `Some(false)` if the registry pins an L2 token for this gateway and `l2_token` differs.
    pub l2_token_matches_registry: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct L1Inflow {
    pub block_number: u64,
    pub tx_index: u32,
    /// Block-level log index of `DepositFinalized` (token rows only).
    pub log_index: Option<u32>,
    pub tx_hash: B256,
    pub tx_type: u8,
    pub kind: InflowKind,
    /// L2 recipient.
    pub to: Address,
    /// Wei for ETH rows, raw token units for token rows.
    pub amount: U256,
    /// `tx.value` (for token rows via the WETH gateway equals `amount`).
    pub tx_value: U256,
    /// `tx.from` as on L2 (the alias).
    pub l2_alias: Address,
    /// unalias(`tx.from`).
    pub l1_sender: Address,
    /// `0x64.requestId` (delayed message number).
    pub l1_request_id: Option<U256>,
    /// `0x68.ticketId` (= hash of the 0x69).
    pub ticket_id: Option<B256>,
    pub token: Option<TokenInflow>,
}

/// One row of `hood.funding_edges` (columns from sql/001 + sql/002).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FundingEdge {
    pub block_number: u64,
    pub tx_index: u32,
    /// L1 (Ethereum) address of the funder: unalias(tx.from) for ETH rows,
    /// `DepositFinalized.from` for token rows.
    pub from_addr: Address,
    pub to_addr: Address,
    pub value_wei: U256,
    /// `l1_eth` | `l1_token`.
    pub kind: &'static str,
    pub tx_hash: B256,
    pub log_index: Option<u32>,
    pub token: Option<Address>,
    pub l1_token: Option<Address>,
    pub gateway: Option<Address>,
    /// `none` | `observed` | `verified` (`none` also for ETH rows, where `gateway` is empty).
    pub gateway_status: &'static str,
    pub l2_alias: Address,
    pub tx_type: u8,
    pub l1_request_id: Option<U256>,
    pub ticket_id: Option<B256>,
}

impl L1Inflow {
    pub fn funding_edge(&self) -> FundingEdge {
        let (from_addr, kind, token, l1_token, gateway, gateway_status) = match &self.token {
            None => (self.l1_sender, "l1_eth", None, None, None, "none"),
            Some(t) => (
                t.l1_from,
                "l1_token",
                t.l2_token,
                Some(t.l1_token),
                Some(t.gateway),
                t.registry.map(RegistryStatus::as_str).unwrap_or("none"),
            ),
        };
        FundingEdge {
            block_number: self.block_number,
            tx_index: self.tx_index,
            from_addr,
            to_addr: self.to,
            value_wei: self.amount,
            kind,
            tx_hash: self.tx_hash,
            log_index: self.log_index,
            token,
            l1_token,
            gateway,
            gateway_status,
            l2_alias: self.l2_alias,
            tx_type: self.tx_type,
            l1_request_id: self.l1_request_id,
            ticket_id: self.ticket_id,
        }
    }
}

/// L1 money that is not an inflow row. Kept explicit so the graph's blind spots are counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnaccountedKind {
    /// 0x69 stage refund to `FeeRefundAddr` (= 0x69 `refundTo`): excess submission fee +
    /// gas price refund. Exact by Nitro code: `depositValue - retryValue - maxRefund(0x68)`.
    SubmitFeeRefund,
    /// 0x69 whose 0x68 (auto-redeem) is not in the block (gas checks failed, or filtered):
    /// `depositValue - retryValue` is an upper bound of what went to `FeeRefundAddr`.
    SubmitNoRedeemUpperBound,
    /// 0x68 stage refunds to `refundTo` (submission fee on success + unused gas):
    /// `maxRefund` is an upper bound, exact value needs balance traces.
    RedeemRefundUpperBound,
    /// 0x64 with status != 1 (onchain filter -> `FilteredFundsRecipient`, or `to` missing):
    /// value did not reach `to`. By code; not observed.
    DepositNotSucceeded,
    /// 0x69 with status != 1 (onchain filter redirects `FeeRefundAddr`/`Beneficiary` to
    /// `FilteredFundsRecipient`, or submission failed). By code; not observed.
    SubmitNotSucceeded,
    /// 0x68 with status != 1 and value > 0: callvalue went back to the ticket escrow
    /// (later: manual redeem = new 0x68, or `beneficiary` on cancel/expiry).
    RetryFailed,
    /// Successful 0x68 to a contract that emitted `DepositFinalized`, but `tx.value` != sum of
    /// the token amounts: the ETH stays with the gateway contract.
    GatewayEthUnexplained,
    /// Successful 0x68 with value > 0 but `to` missing (contract creation): not attributed.
    RetryNoTo,
}

impl UnaccountedKind {
    pub fn as_str(self) -> &'static str {
        match self {
            UnaccountedKind::SubmitFeeRefund => "submit_fee_refund",
            UnaccountedKind::SubmitNoRedeemUpperBound => "submit_no_redeem_upper_bound",
            UnaccountedKind::RedeemRefundUpperBound => "redeem_refund_upper_bound",
            UnaccountedKind::DepositNotSucceeded => "deposit_not_succeeded",
            UnaccountedKind::SubmitNotSucceeded => "submit_not_succeeded",
            UnaccountedKind::RetryFailed => "retry_failed",
            UnaccountedKind::GatewayEthUnexplained => "gateway_eth_unexplained",
            UnaccountedKind::RetryNoTo => "retry_no_to",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnaccountedFlow {
    pub block_number: u64,
    pub tx_index: u32,
    pub tx_hash: B256,
    pub kind: UnaccountedKind,
    /// Who (may have) received it on L2, when known.
    pub addr: Option<Address>,
    pub amount_wei: U256,
    pub l1_sender: Address,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Agg {
    pub n: u64,
    pub sum: U256,
}

impl Agg {
    fn add(&mut self, v: U256) {
        self.n += 1;
        self.sum += v;
    }
    fn merge(&mut self, o: &Agg) {
        self.n += o.n;
        self.sum += o.sum;
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counters {
    pub blocks: u64,
    pub txs: u64,
    pub tx_types: BTreeMap<u8, u64>,
    /// Rows: 0x64 status 1.
    pub deposit_rows: Agg,
    /// Rows: 0x68 ETH (wei).
    pub retry_eth_rows: Agg,
    /// Rows: token via a gateway in the registry (raw token units, may mix tokens).
    pub token_rows_registered: Agg,
    /// Rows: token via a gateway NOT in the registry.
    pub token_rows_unregistered: Agg,
    /// Token rows with no matching L2 `Transfer`.
    pub token_l2_missing: u64,
    /// Token rows whose L2 token differs from the registry's pinned token.
    pub token_registry_mismatch: u64,
    /// `DepositFinalized` in a successful 0x68 emitted by someone other than `tx.to` (ignored).
    pub deposit_finalized_foreign: u64,
    /// 0x69 status 1 (sum = depositValue; not an inflow, context only).
    pub submit_ok: Agg,
    /// 0x68 status 1 with value 0 and no DepositFinalized: no money moved.
    pub retry_zero_value: u64,
    /// 0x69 with `maxRefund(0x68) > depositValue - retryValue` (should be 0 by Nitro code).
    pub refund_identity_anomaly: u64,
    pub unaccounted: BTreeMap<UnaccountedKind, Agg>,
}

impl Counters {
    pub fn merge(&mut self, o: &Counters) {
        self.blocks += o.blocks;
        self.txs += o.txs;
        for (k, v) in &o.tx_types {
            *self.tx_types.entry(*k).or_default() += v;
        }
        self.deposit_rows.merge(&o.deposit_rows);
        self.retry_eth_rows.merge(&o.retry_eth_rows);
        self.token_rows_registered.merge(&o.token_rows_registered);
        self.token_rows_unregistered.merge(&o.token_rows_unregistered);
        self.token_l2_missing += o.token_l2_missing;
        self.token_registry_mismatch += o.token_registry_mismatch;
        self.deposit_finalized_foreign += o.deposit_finalized_foreign;
        self.submit_ok.merge(&o.submit_ok);
        self.retry_zero_value += o.retry_zero_value;
        self.refund_identity_anomaly += o.refund_identity_anomaly;
        for (k, v) in &o.unaccounted {
            self.unaccounted.entry(*k).or_default().merge(v);
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct BlockL1 {
    pub inflows: Vec<L1Inflow>,
    pub unaccounted: Vec<UnaccountedFlow>,
    pub counters: Counters,
}

impl BlockL1 {
    fn unaccounted(&mut self, base: &TxBase, kind: UnaccountedKind, addr: Option<Address>, amount_wei: U256) {
        self.counters.unaccounted.entry(kind).or_default().add(amount_wei);
        self.unaccounted.push(UnaccountedFlow {
            block_number: base.block_number,
            tx_index: base.tx_index,
            tx_hash: base.tx_hash,
            kind,
            addr,
            amount_wei,
            l1_sender: base.l1_sender,
        });
    }
}

struct TxBase {
    block_number: u64,
    tx_index: u32,
    tx_hash: B256,
    tx_type: u8,
    l2_alias: Address,
    l1_sender: Address,
    to: Option<Address>,
    value: U256,
    ok: bool,
}

struct DecodedLog {
    address: Address,
    topics: Vec<B256>,
    data: Vec<u8>,
    log_index: u32,
}

fn decode_logs(rc: &RpcReceipt) -> Result<Vec<DecodedLog>> {
    rc.logs
        .iter()
        .map(|l| {
            Ok(DecodedLog {
                address: parse_addr(&l.address)?,
                topics: l.topics.iter().map(|t| parse_b256(t)).collect::<Result<_>>()?,
                data: hex::decode(&l.data).map_err(|e| anyhow!("bad log data: {e}"))?,
                log_index: u32::try_from(hex_u64(&l.log_index)?)?,
            })
        })
        .collect()
}

pub fn parse_line(line: &str) -> Result<BlockLine> {
    serde_json::from_str(line).context("block line is not {number, block, receipts}")
}

/// Decode one block. Errors only on malformed input (receipts misaligned, bad hex); semantic
/// oddities go to counters / unaccounted records.
pub fn decode_block(line: &BlockLine, registry: &GatewayRegistry) -> Result<BlockL1> {
    let n = line.number;
    if hex_u64(&line.block.number)? != n {
        bail!("block {n}: block.number is {}", line.block.number);
    }
    let txs = &line.block.transactions;
    if txs.len() != line.receipts.len() {
        bail!("block {n}: {} receipts for {} transactions", line.receipts.len(), txs.len());
    }
    let mut out = BlockL1::default();
    out.counters.blocks = 1;
    out.counters.txs = txs.len() as u64;

    // ticketId -> maxRefund of 0x68s in this block, to settle the 0x69 refund identity.
    let mut redeem_max_refund: HashMap<B256, U256> = HashMap::new();
    for tx in txs {
        if hex_u64(&tx.tx_type)? == TX_TYPE_RETRY as u64 {
            let ticket = parse_b256(tx.ticket_id.as_deref().ok_or_else(|| anyhow!("block {n}: 0x68 without ticketId"))?)?;
            redeem_max_refund.insert(ticket, opt_u256(&tx.max_refund, "maxRefund")?);
        }
    }

    for (tx, rc) in txs.iter().zip(&line.receipts) {
        if !tx.hash.eq_ignore_ascii_case(&rc.transaction_hash) {
            bail!("block {n}: receipt {} does not match tx {}", rc.transaction_hash, tx.hash);
        }
        let ty = u8::try_from(hex_u64(&tx.tx_type)?).map_err(|_| anyhow!("block {n}: tx type {}", tx.tx_type))?;
        *out.counters.tx_types.entry(ty).or_default() += 1;
        if !matches!(ty, TX_TYPE_DEPOSIT | TX_TYPE_RETRY | TX_TYPE_SUBMIT_RETRYABLE) {
            continue;
        }
        let l2_alias = parse_addr(&tx.from)?;
        let base = TxBase {
            block_number: n,
            tx_index: u32::try_from(hex_u64(&tx.transaction_index)?)?,
            tx_hash: parse_b256(&tx.hash)?,
            tx_type: ty,
            l2_alias,
            l1_sender: unalias(l2_alias),
            to: tx.to.as_deref().map(parse_addr).transpose()?,
            value: hex_u256(&tx.value)?,
            ok: rc.status.as_deref().map(hex_u64).transpose()? == Some(1),
        };
        match ty {
            TX_TYPE_DEPOSIT => deposit(&mut out, &base, tx)?,
            TX_TYPE_SUBMIT_RETRYABLE => submit(&mut out, &base, tx, &redeem_max_refund)?,
            _ => retry(&mut out, &base, tx, rc, registry)?,
        }
    }
    Ok(out)
}

fn deposit(out: &mut BlockL1, b: &TxBase, tx: &RpcTx) -> Result<()> {
    match (b.ok, b.to) {
        (true, Some(to)) => {
            out.counters.deposit_rows.add(b.value);
            out.inflows.push(L1Inflow {
                block_number: b.block_number,
                tx_index: b.tx_index,
                log_index: None,
                tx_hash: b.tx_hash,
                tx_type: b.tx_type,
                kind: InflowKind::EthDeposit,
                to,
                amount: b.value,
                tx_value: b.value,
                l2_alias: b.l2_alias,
                l1_sender: b.l1_sender,
                l1_request_id: tx.request_id.as_deref().map(hex_u256).transpose()?,
                ticket_id: None,
                token: None,
            });
        }
        // Filtered (value went to FilteredFundsRecipient) or no `to`: not credited to `to`.
        _ => out.unaccounted(b, UnaccountedKind::DepositNotSucceeded, b.to, b.value),
    }
    Ok(())
}

fn submit(out: &mut BlockL1, b: &TxBase, tx: &RpcTx, redeem_max_refund: &HashMap<B256, U256>) -> Result<()> {
    let deposit_value = opt_u256(&tx.deposit_value, "depositValue")?;
    let retry_value = opt_u256(&tx.retry_value, "retryValue")?;
    let fee_refund_addr = tx.refund_to.as_deref().map(parse_addr).transpose()?;
    if !b.ok {
        out.unaccounted(b, UnaccountedKind::SubmitNotSucceeded, fee_refund_addr, deposit_value);
        return Ok(());
    }
    out.counters.submit_ok.add(deposit_value);
    let pool = deposit_value.saturating_sub(retry_value);
    match redeem_max_refund.get(&b.tx_hash) {
        Some(max_refund) => {
            if *max_refund > pool {
                out.counters.refund_identity_anomaly += 1;
            }
            let refund = pool.saturating_sub(*max_refund);
            out.unaccounted(b, UnaccountedKind::SubmitFeeRefund, fee_refund_addr, refund);
        }
        None => out.unaccounted(b, UnaccountedKind::SubmitNoRedeemUpperBound, fee_refund_addr, pool),
    }
    Ok(())
}

fn retry(out: &mut BlockL1, b: &TxBase, tx: &RpcTx, rc: &RpcReceipt, registry: &GatewayRegistry) -> Result<()> {
    let ticket_id = Some(parse_b256(tx.ticket_id.as_deref().unwrap_or_default())?);
    let refund_to = tx.refund_to.as_deref().map(parse_addr).transpose()?;
    let max_refund = opt_u256(&tx.max_refund, "maxRefund")?;
    if !max_refund.is_zero() {
        out.unaccounted(b, UnaccountedKind::RedeemRefundUpperBound, refund_to, max_refund);
    }
    if !b.ok {
        if !b.value.is_zero() {
            out.unaccounted(b, UnaccountedKind::RetryFailed, b.to, b.value);
        }
        return Ok(());
    }
    let Some(to) = b.to else {
        if !b.value.is_zero() {
            out.unaccounted(b, UnaccountedKind::RetryNoTo, None, b.value);
        }
        return Ok(());
    };

    let logs = decode_logs(rc)?;
    let mut token_rows = Vec::new();
    for l in &logs {
        if l.topics.first() != Some(&TOPIC_DEPOSIT_FINALIZED) {
            continue;
        }
        if l.address != to {
            out.counters.deposit_finalized_foreign += 1;
            continue;
        }
        let e = token_bridge::DepositFinalized::decode_raw_log(l.topics.iter().copied(), &l.data)
            .map_err(|e| anyhow!("block {}: bad DepositFinalized: {e}", b.block_number))?;
        // Matching L2 token movement: WETH-style gateways transfer from themselves after
        // minting to themselves, standard gateways mint straight to the recipient.
        let l2_token = logs
            .iter()
            .rfind(|t| {
                t.log_index < l.log_index
                    && t.topics.len() == 3
                    && t.topics[0] == TOPIC_TRANSFER
                    && t.topics[2] == e.to.into_word()
                    && (t.topics[1] == to.into_word() || t.topics[1] == B256::ZERO)
                    && t.data.len() == 32
                    && U256::from_be_slice(&t.data) == e.amount
            })
            .map(|t| t.address);
        let entry = registry.get(&to);
        let l2_token_matches_registry = match (entry.and_then(|g| g.l2_token), l2_token) {
            (Some(want), Some(got)) => Some(want == got),
            (Some(_), None) => Some(false),
            _ => None,
        };
        if l2_token.is_none() {
            out.counters.token_l2_missing += 1;
        }
        if l2_token_matches_registry == Some(false) {
            out.counters.token_registry_mismatch += 1;
        }
        if entry.is_some() {
            out.counters.token_rows_registered.add(e.amount);
        } else {
            out.counters.token_rows_unregistered.add(e.amount);
        }
        token_rows.push(L1Inflow {
            block_number: b.block_number,
            tx_index: b.tx_index,
            log_index: Some(l.log_index),
            tx_hash: b.tx_hash,
            tx_type: b.tx_type,
            kind: InflowKind::BridgedToken,
            to: e.to,
            amount: e.amount,
            tx_value: b.value,
            l2_alias: b.l2_alias,
            l1_sender: b.l1_sender,
            l1_request_id: None,
            ticket_id,
            token: Some(TokenInflow {
                gateway: to,
                l1_token: e.l1Token,
                l2_token,
                l1_from: e.from,
                registry: entry.map(|g| g.status),
                l2_token_matches_registry,
            }),
        });
    }

    if !token_rows.is_empty() {
        let token_sum = token_rows.iter().fold(U256::ZERO, |s, r| s + r.amount);
        if !b.value.is_zero() && b.value != token_sum {
            out.unaccounted(b, UnaccountedKind::GatewayEthUnexplained, Some(to), b.value);
        }
        out.inflows.extend(token_rows);
    } else if !b.value.is_zero() {
        out.counters.retry_eth_rows.add(b.value);
        out.inflows.push(L1Inflow {
            block_number: b.block_number,
            tx_index: b.tx_index,
            log_index: None,
            tx_hash: b.tx_hash,
            tx_type: b.tx_type,
            kind: InflowKind::RetryEth,
            to,
            amount: b.value,
            tx_value: b.value,
            l2_alias: b.l2_alias,
            l1_sender: b.l1_sender,
            l1_request_id: None,
            ticket_id,
            token: None,
        });
    } else {
        out.counters.retry_zero_value += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_roundtrip_and_wrap() {
        // Block 77285531, tx 0x3f8e47b0…b522c1 (0x64): from = alias(to) for an EOA deposit.
        let from = address!("8c6ca3c268bb600e48bb5aaed5f9f701adc2d177");
        let to = address!("7b5ba3c268bb600e48bb5aaed5f9f701adc2c066");
        assert_eq!(unalias(from), to);
        assert_eq!(alias(to), from);
        // Wrap-around below the offset: unalias(0x…01) = 2^160 + 1 - offset.
        let low = address!("0000000000000000000000000000000000000001");
        assert_eq!(unalias(low), address!("eeeeffffffffffffffffffffffffffffffffeef0"));
        assert_eq!(alias(unalias(low)), low);
        // Wrap-around above: alias(0xfff…f) = offset - 1.
        let high = address!("ffffffffffffffffffffffffffffffffffffffff");
        assert_eq!(alias(high), address!("1111000000000000000000000000000000001110"));
    }

    #[test]
    fn builtin_registry_is_verified_weth_only() {
        let r = GatewayRegistry::builtin();
        assert_eq!(r.len(), 1);
        let e = r.get(&L2_WETH_GATEWAY).unwrap();
        assert_eq!((e.l2_token, e.status), (Some(L2_WETH), RegistryStatus::Verified));
        let mut r2 = GatewayRegistry::builtin();
        assert!(r2.extend(GatewayRegistry::builtin()).is_err());
    }

    #[test]
    fn registry_tsv() {
        let r = GatewayRegistry::parse_tsv(
            "# comment\n\n0x00000000000000000000000000000000000000aa\t0x00000000000000000000000000000000000000bb\tobserved\tnote\n0x00000000000000000000000000000000000000cc\t-\tverified\n",
        )
        .unwrap();
        assert_eq!(r.len(), 2);
        let a = r.get(&address!("00000000000000000000000000000000000000aa")).unwrap();
        assert_eq!(a.status, RegistryStatus::Observed);
        assert_eq!(a.l2_token, Some(address!("00000000000000000000000000000000000000bb")));
        assert_eq!(r.get(&address!("00000000000000000000000000000000000000cc")).unwrap().l2_token, None);
        assert!(GatewayRegistry::parse_tsv("0x00000000000000000000000000000000000000aa\t-\ttodo\n").is_err());
        assert!(GatewayRegistry::parse_tsv("0x00000000000000000000000000000000000000aa\t-\trejected\n").is_err());
        assert!(GatewayRegistry::parse_tsv(
            "0x00000000000000000000000000000000000000aa\t-\tobserved\n0x00000000000000000000000000000000000000aa\t-\tverified\n"
        )
        .is_err());
    }
}
