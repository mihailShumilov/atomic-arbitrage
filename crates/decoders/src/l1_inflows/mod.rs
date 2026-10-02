//! Money coming in from L1 (Ethereum) — the first edge of the funding graph.
//!
//! Input: a [`Block`] parsed by [`crate::model::parse_block_line`] from one line of
//! `data/blocks/blocks-*.jsonl.zst`. Output: [`L1Inflow`] rows (map to `hood.funding_edges` via
//! [`L1Inflow::funding_edge`]), [`UnaccountedFlow`] records for L1 money that reaches L2 addresses
//! but is NOT an inflow row, and [`Counters`].
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

mod registry;
mod types;

use std::collections::HashMap;

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolEvent;
use anyhow::{anyhow, Context, Result};

pub use registry::{GatewayEntry, GatewayRegistry, RegistryStatus};
pub use types::{Agg, BlockL1, Counters, InflowKind, L1Inflow, TokenInflow, UnaccountedFlow, UnaccountedKind};

use crate::arbitrum::unalias;
use crate::events::{token_bridge, TOPIC_DEPOSIT_FINALIZED, TOPIC_TRANSFER};
use crate::model::{ArbFields, Block, Log, TxCtx};

/// Decode one block. Malformed input is rejected by [`crate::model::parse_block_line`]; the only
/// error left here is an undecodable `DepositFinalized` from the tx's own `to`. Semantic oddities
/// go to counters / unaccounted records.
///
/// # Errors
/// A `DepositFinalized` log emitted by the `0x68` `to` that does not ABI-decode.
pub fn decode_block(block: &Block, registry: &GatewayRegistry) -> Result<BlockL1> {
    let mut out = BlockL1::default();
    out.counters.blocks = 1;
    out.counters.txs = block.txs.len() as u64;

    // ticketId -> maxRefund of 0x68s in this block, to settle the 0x69 refund identity.
    let redeem_max_refund: HashMap<B256, U256> = block
        .txs
        .iter()
        .filter_map(|tx| match tx.arb {
            ArbFields::Retry { ticket_id, max_refund, .. } => Some((ticket_id, max_refund)),
            _ => None,
        })
        .collect();

    for ctx in block.tx_ctxs() {
        let tx = ctx.tx;
        *out.counters.tx_types.entry(tx.ty).or_default() += 1;
        let b = L1Tx { ctx, l1_sender: unalias(tx.from) };
        match tx.arb {
            ArbFields::None => {}
            ArbFields::Deposit { request_id } => deposit(&mut out, &b, request_id),
            ArbFields::SubmitRetryable { deposit_value, retry_value, refund_to } => {
                submit(&mut out, &b, deposit_value, retry_value, refund_to, &redeem_max_refund);
            }
            ArbFields::Retry { ticket_id, max_refund, refund_to } => {
                retry(&mut out, &b, ticket_id, max_refund, refund_to, registry)
                    .with_context(|| format!("block {} tx {} {}", block.number, tx.index, tx.hash))?;
            }
        }
    }
    Ok(out)
}

/// An L1-message transaction with its unaliased sender.
struct L1Tx<'a> {
    ctx: TxCtx<'a>,
    l1_sender: Address,
}

impl L1Tx<'_> {
    /// Row with the fields every inflow kind shares; kind-specific fields are set by the caller.
    fn inflow(&self, kind: InflowKind, to: Address, amount: U256) -> L1Inflow {
        let tx = self.ctx.tx;
        L1Inflow {
            block_number: self.ctx.block,
            tx_index: tx.index,
            log_index: None,
            tx_hash: tx.hash,
            tx_type: tx.ty,
            kind,
            to,
            amount,
            tx_value: tx.value,
            l2_alias: tx.from,
            l1_sender: self.l1_sender,
            l1_request_id: None,
            ticket_id: None,
            token: None,
        }
    }
}

impl BlockL1 {
    fn unaccounted(&mut self, b: &L1Tx, kind: UnaccountedKind, addr: Option<Address>, amount_wei: U256) {
        self.counters.unaccounted.entry(kind).or_default().add(amount_wei);
        self.unaccounted.push(UnaccountedFlow {
            block_number: b.ctx.block,
            tx_index: b.ctx.tx.index,
            tx_hash: b.ctx.tx.hash,
            kind,
            addr,
            amount_wei,
            l1_sender: b.l1_sender,
        });
    }
}

fn deposit(out: &mut BlockL1, b: &L1Tx, request_id: Option<U256>) {
    let tx = b.ctx.tx;
    match (tx.status, tx.to) {
        (true, Some(to)) => {
            out.counters.deposit_rows.add(tx.value);
            out.inflows.push(L1Inflow { l1_request_id: request_id, ..b.inflow(InflowKind::EthDeposit, to, tx.value) });
        }
        // Filtered (value went to FilteredFundsRecipient) or no `to`: not credited to `to`.
        _ => out.unaccounted(b, UnaccountedKind::DepositNotSucceeded, tx.to, tx.value),
    }
}

fn submit(
    out: &mut BlockL1,
    b: &L1Tx,
    deposit_value: U256,
    retry_value: U256,
    fee_refund_addr: Option<Address>,
    redeem_max_refund: &HashMap<B256, U256>,
) {
    if !b.ctx.tx.status {
        out.unaccounted(b, UnaccountedKind::SubmitNotSucceeded, fee_refund_addr, deposit_value);
        return;
    }
    out.counters.submit_ok.add(deposit_value);
    let pool = deposit_value.saturating_sub(retry_value);
    match redeem_max_refund.get(&b.ctx.tx.hash) {
        Some(max_refund) => {
            if *max_refund > pool {
                out.counters.refund_identity_anomaly += 1;
            }
            let refund = pool.saturating_sub(*max_refund);
            out.unaccounted(b, UnaccountedKind::SubmitFeeRefund, fee_refund_addr, refund);
        }
        None => out.unaccounted(b, UnaccountedKind::SubmitNoRedeemUpperBound, fee_refund_addr, pool),
    }
}

fn retry(
    out: &mut BlockL1,
    b: &L1Tx,
    ticket_id: B256,
    max_refund: U256,
    refund_to: Option<Address>,
    registry: &GatewayRegistry,
) -> Result<()> {
    let tx = b.ctx.tx;
    if !max_refund.is_zero() {
        out.unaccounted(b, UnaccountedKind::RedeemRefundUpperBound, refund_to, max_refund);
    }
    if !tx.status {
        if !tx.value.is_zero() {
            out.unaccounted(b, UnaccountedKind::RetryFailed, tx.to, tx.value);
        }
        return Ok(());
    }
    let Some(to) = tx.to else {
        if !tx.value.is_zero() {
            out.unaccounted(b, UnaccountedKind::RetryNoTo, None, tx.value);
        }
        return Ok(());
    };

    let token_rows = gateway_token_rows(b, to, ticket_id, registry, &mut out.counters)?;
    if !token_rows.is_empty() {
        let token_sum = token_rows.iter().try_fold(U256::ZERO, |s, r| s.checked_add(r.amount));
        if token_sum.is_none() {
            out.counters.token_sum_overflow += 1;
        }
        if !tx.value.is_zero() && token_sum != Some(tx.value) {
            out.unaccounted(b, UnaccountedKind::GatewayEthUnexplained, Some(to), tx.value);
        }
        out.inflows.extend(token_rows);
    } else if !tx.value.is_zero() {
        out.counters.retry_eth_rows.add(tx.value);
        out.inflows.push(L1Inflow { ticket_id: Some(ticket_id), ..b.inflow(InflowKind::RetryEth, to, tx.value) });
    } else {
        out.counters.retry_zero_value += 1;
    }
    Ok(())
}

/// Token rows of a successful `0x68` into `to`: one per `DepositFinalized` emitted by `to`.
fn gateway_token_rows(
    b: &L1Tx,
    to: Address,
    ticket_id: B256,
    registry: &GatewayRegistry,
    c: &mut Counters,
) -> Result<Vec<L1Inflow>> {
    let logs = &b.ctx.tx.logs;
    let mut rows = Vec::new();
    for l in logs {
        if l.topics.first() != Some(&TOPIC_DEPOSIT_FINALIZED) {
            continue;
        }
        if l.address != to {
            c.deposit_finalized_foreign += 1;
            continue;
        }
        let e = token_bridge::DepositFinalized::decode_raw_log(l.topics.iter().copied(), &l.data)
            .map_err(|e| anyhow!("bad DepositFinalized at log {}: {e}", l.index))?;
        let l2_token = matching_l2_transfer(logs, l.index, to, e.to, e.amount);
        let entry = registry.get(&to);
        let l2_token_matches_registry = match (entry.and_then(|g| g.l2_token), l2_token) {
            (Some(want), Some(got)) => Some(want == got),
            (Some(_), None) => Some(false),
            _ => None,
        };
        if l2_token.is_none() {
            c.token_l2_missing += 1;
        }
        if l2_token_matches_registry == Some(false) {
            c.token_registry_mismatch += 1;
        }
        if entry.is_some() {
            c.token_rows_registered.add(e.amount);
        } else {
            c.token_rows_unregistered.add(e.amount);
        }
        rows.push(L1Inflow {
            log_index: Some(l.index),
            ticket_id: Some(ticket_id),
            token: Some(TokenInflow {
                gateway: to,
                l1_token: e.l1Token,
                l2_token,
                l1_from: e.from,
                registry: entry.map(|g| g.status),
                l2_token_matches_registry,
            }),
            ..b.inflow(InflowKind::BridgedToken, e.to, e.amount)
        });
    }
    Ok(rows)
}

/// L2 token moved for a `DepositFinalized` at log `before`: the emitter of the LAST
/// `Transfer(gateway | 0x0 -> recipient, amount)` (exact amount) before it in the same receipt.
/// WETH-style gateways mint to themselves and then transfer from themselves; standard gateways
/// mint straight to the recipient (`from` = 0x0).
fn matching_l2_transfer(
    logs: &[Log],
    before: u32,
    gateway: Address,
    recipient: Address,
    amount: U256,
) -> Option<Address> {
    logs.iter()
        .rfind(|t| {
            t.index < before
                && t.topics.len() == 3
                && t.topics[0] == TOPIC_TRANSFER
                && t.topics[2] == recipient.into_word()
                && (t.topics[1] == gateway.into_word() || t.topics[1] == B256::ZERO)
                && t.data.len() == 32
                && U256::from_be_slice(&t.data) == amount
        })
        .map(|t| t.address)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, Bytes};

    const GW: Address = address!("00000000000000000000000000000000000000a1");
    const TO: Address = address!("00000000000000000000000000000000000000b2");
    const TOKEN: Address = address!("00000000000000000000000000000000000000c3");

    fn transfer(index: u32, token: Address, from: Address, to: Address, amount: u64) -> Log {
        Log {
            address: token,
            topics: vec![TOPIC_TRANSFER, from.into_word(), to.into_word()],
            data: Bytes::from(U256::from(amount).to_be_bytes::<32>().to_vec()),
            index,
        }
    }

    // Synthetic: the standard ERC-20 gateway path (mint 0x0 -> recipient) was not observed on chain.
    #[test]
    fn synthetic_matching_transfer_mint_from_zero() {
        let logs = [transfer(5, TOKEN, Address::ZERO, TO, 100)];
        assert_eq!(matching_l2_transfer(&logs, 6, GW, TO, U256::from(100u8)), Some(TOKEN));
    }

    #[test]
    fn synthetic_matching_transfer_rules() {
        let other = address!("00000000000000000000000000000000000000d4");
        let logs = [
            transfer(1, other, GW, TO, 100),            // earlier match
            transfer(2, TOKEN, GW, TO, 100),            // last match before -> wins
            transfer(3, TOKEN, GW, TO, 99),             // wrong amount
            transfer(4, TOKEN, other, TO, 100),         // wrong sender
            transfer(5, TOKEN, GW, other, 100),         // wrong recipient
            transfer(7, other, Address::ZERO, TO, 100), // after the DepositFinalized
        ];
        assert_eq!(matching_l2_transfer(&logs, 6, GW, TO, U256::from(100u8)), Some(TOKEN));
        assert_eq!(matching_l2_transfer(&logs, 2, GW, TO, U256::from(100u8)), Some(other));
        assert_eq!(matching_l2_transfer(&logs, 1, GW, TO, U256::from(100u8)), None);
        // A Transfer with 4 topics (ERC-721 style) never matches.
        let mut nft = transfer(1, TOKEN, GW, TO, 100);
        nft.topics.push(B256::ZERO);
        assert_eq!(matching_l2_transfer(&[nft], 6, GW, TO, U256::from(100u8)), None);
    }
}
