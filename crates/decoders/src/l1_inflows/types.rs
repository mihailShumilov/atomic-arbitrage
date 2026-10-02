//! Output types of the L1 inflow decoder: rows, "unaccounted" records, counters.

use std::collections::BTreeMap;
use std::io::{self, Write};

use alloy_primitives::{Address, B256, U256};

use super::registry::RegistryStatus;
use crate::rows::{EdgeKind, FundingEdge, GatewayStatus, HexOr, NULL};

/// What made an inflow row.
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
    /// Name used in reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EthDeposit => "eth_deposit",
            Self::RetryEth => "retry_eth",
            Self::BridgedToken => "bridged_token",
        }
    }
}

/// Token part of a [`InflowKind::BridgedToken`] row.
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

/// One inflow from L1 (maps to one `hood.funding_edges` row).
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

impl L1Inflow {
    /// The `hood.funding_edges` row of this inflow.
    #[must_use]
    pub fn funding_edge(&self) -> FundingEdge {
        let (from_addr, kind, token, l1_token, gateway, gateway_status) = match &self.token {
            None => (self.l1_sender, EdgeKind::L1Eth, None, None, None, GatewayStatus::None),
            Some(t) => (
                t.l1_from,
                EdgeKind::L1Token,
                t.l2_token,
                Some(t.l1_token),
                Some(t.gateway),
                GatewayStatus::from(t.registry),
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
    /// the token amounts (or that sum overflows U256): the ETH stays with the gateway contract.
    GatewayEthUnexplained,
    /// Successful 0x68 with value > 0 but `to` missing (contract creation): not attributed.
    RetryNoTo,
}

impl UnaccountedKind {
    /// Name used in the TSV and reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SubmitFeeRefund => "submit_fee_refund",
            Self::SubmitNoRedeemUpperBound => "submit_no_redeem_upper_bound",
            Self::RedeemRefundUpperBound => "redeem_refund_upper_bound",
            Self::DepositNotSucceeded => "deposit_not_succeeded",
            Self::SubmitNotSucceeded => "submit_not_succeeded",
            Self::RetryFailed => "retry_failed",
            Self::GatewayEthUnexplained => "gateway_eth_unexplained",
            Self::RetryNoTo => "retry_no_to",
        }
    }
}

/// One "unaccounted" record.
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

impl UnaccountedFlow {
    /// Column order of the scanner's `--unaccounted-out` TSV (no ClickHouse table yet).
    pub const COLUMNS: [&'static str; 7] =
        ["block_number", "tx_index", "tx_hash", "kind", "addr", "amount_wei", "l1_sender"];

    /// Header line: [`Self::COLUMNS`] joined by tabs.
    ///
    /// # Errors
    /// I/O errors of `w`.
    pub fn write_tsv_header(w: &mut impl Write) -> io::Result<()> {
        writeln!(w, "{}", Self::COLUMNS.join("\t"))
    }

    /// One TSV line in [`Self::COLUMNS`] order; unknown `addr` is `\N`.
    ///
    /// # Errors
    /// I/O errors of `w`.
    pub fn write_tsv(&self, w: &mut impl Write) -> io::Result<()> {
        writeln!(
            w,
            "{}\t{}\t{:#x}\t{}\t{}\t{}\t{:#x}",
            self.block_number,
            self.tx_index,
            self.tx_hash,
            self.kind.as_str(),
            HexOr(self.addr, NULL),
            self.amount_wei,
            self.l1_sender,
        )
    }
}

/// Count + sum. The sum saturates at `U256::MAX` and sets `overflowed` instead of wrapping
/// (ruint `+` wraps; amounts of `DepositFinalized` come from any emitter).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Agg {
    pub n: u64,
    pub sum: U256,
    /// `sum` hit `U256::MAX`: it is a lower bound.
    pub overflowed: bool,
}

impl Agg {
    pub(crate) fn add(&mut self, v: U256) {
        self.n += 1;
        self.add_sum(v);
    }

    fn add_sum(&mut self, v: U256) {
        match self.sum.checked_add(v) {
            Some(s) => self.sum = s,
            None => {
                self.sum = U256::MAX;
                self.overflowed = true;
            }
        }
    }

    fn merge(&mut self, o: &Agg) {
        self.n += o.n;
        self.add_sum(o.sum);
        self.overflowed |= o.overflowed;
    }
}

/// Per-block (and, merged, per-run) counters.
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
    /// Successful 0x68 whose token amounts overflow U256 when summed (recorded as
    /// `gateway_eth_unexplained` if `tx.value > 0`).
    pub token_sum_overflow: u64,
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
    /// Adds `o` to `self`. Destructures `o` without `..`, so a new field cannot be forgotten here.
    pub fn merge(&mut self, o: &Counters) {
        let Counters {
            blocks,
            txs,
            tx_types,
            deposit_rows,
            retry_eth_rows,
            token_rows_registered,
            token_rows_unregistered,
            token_l2_missing,
            token_registry_mismatch,
            token_sum_overflow,
            deposit_finalized_foreign,
            submit_ok,
            retry_zero_value,
            refund_identity_anomaly,
            unaccounted,
        } = o;
        self.blocks += blocks;
        self.txs += txs;
        for (k, v) in tx_types {
            *self.tx_types.entry(*k).or_default() += v;
        }
        self.deposit_rows.merge(deposit_rows);
        self.retry_eth_rows.merge(retry_eth_rows);
        self.token_rows_registered.merge(token_rows_registered);
        self.token_rows_unregistered.merge(token_rows_unregistered);
        self.token_l2_missing += token_l2_missing;
        self.token_registry_mismatch += token_registry_mismatch;
        self.token_sum_overflow += token_sum_overflow;
        self.deposit_finalized_foreign += deposit_finalized_foreign;
        self.submit_ok.merge(submit_ok);
        self.retry_zero_value += retry_zero_value;
        self.refund_identity_anomaly += refund_identity_anomaly;
        for (k, v) in unaccounted {
            self.unaccounted.entry(*k).or_default().merge(v);
        }
    }
}

/// Decoder output for one block.
#[derive(Debug, Clone, Default)]
pub struct BlockL1 {
    pub inflows: Vec<L1Inflow>,
    pub unaccounted: Vec<UnaccountedFlow>,
    pub counters: Counters,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agg_saturates_instead_of_wrapping() {
        let mut a = Agg::default();
        a.add(U256::MAX);
        a.add(U256::from(1u8));
        assert_eq!((a.n, a.sum, a.overflowed), (2, U256::MAX, true));
        let mut b = Agg::default();
        b.add(U256::from(2u8));
        b.merge(&a);
        assert_eq!((b.n, b.sum, b.overflowed), (3, U256::MAX, true));
    }

    #[test]
    fn unaccounted_unknown_addr_is_null() {
        let u = UnaccountedFlow {
            block_number: 1,
            tx_index: 0,
            tx_hash: B256::ZERO,
            kind: UnaccountedKind::RetryNoTo,
            addr: None,
            amount_wei: U256::from(5u8),
            l1_sender: Address::ZERO,
        };
        let mut out = Vec::new();
        u.write_tsv(&mut out).unwrap();
        let line = String::from_utf8(out).unwrap();
        let cols: Vec<_> = line.trim_end().split('\t').collect();
        assert_eq!(cols.len(), UnaccountedFlow::COLUMNS.len());
        assert_eq!(cols[3], "retry_no_to");
        assert_eq!(cols[4], "\\N");
    }
}
