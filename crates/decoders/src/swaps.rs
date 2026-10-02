//! Uniswap v3 / v4 `Swap` logs -> venue-agnostic [`PoolSwap`] with its position and tx context.
//! Base of `hood.swaps`; the mapping to token/quote and buy/sell needs pool metadata (which
//! currency is the meme token) and happens downstream.
//!
//! Every log is classified ([`SwapDecode`]): not a swap, a swap, or malformed (topic0 of a
//! `Swap` but the topics/data do not decode). Malformed logs are counted, never dropped silently,
//! so the data-auditor can prove `hood.swaps` complete.
//!
//! The emitter is not checked: filtering by the `verified` `PoolManager` / pool registry happens
//! downstream (loader), like `registry: None` rows of `l1_inflows`. Any contract can emit a log
//! with these topic0.

use alloy_primitives::{Address, B256, I256, U256};
use alloy_sol_types::SolEvent;

use crate::events::{v3, v4, TOPIC_SWAP_V3, TOPIC_SWAP_V4};
use crate::model::{Block, Log, TxCtx};

/// Which `Swap` event a [`PoolSwap`] comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapEvent {
    /// Uniswap v3 pool `Swap` (emitter = the pool).
    V3,
    /// Uniswap v4 `PoolManager` `Swap` (emitter = the `PoolManager`, pool = `pool_id`).
    V4,
}

/// Venue-agnostic swap.
///
/// Sign convention (both events normalized to it): positive amount = token flowed INTO the pool,
/// negative = out of the pool to the swapper. v3 emits it like that; v4 emits the swapper's
/// `BalanceDelta` (positive = the swapper receives), so v4 amounts are negated here.
/// Checked on data 2026-10-02 against ERC-20 `Transfer` logs of the same receipts in
/// data/blocks + data/samples (2 712 blocks): v3 positive amounts match a Transfer INTO the pool in
/// 6 859 of 6 862 swaps; raw v4 positive amounts match a Transfer FROM the `PoolManager` 3 338
/// times vs 229 the other way (native-ETH legs have no Transfer). Fixture: block 74744924.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolSwap {
    pub block_number: u64,
    pub tx_index: u32,
    /// Block-level log index.
    pub log_index: u32,
    pub tx_hash: B256,
    /// `tx.from` (the EOA that signed).
    pub trader: Address,
    /// `tx.to` (router or bot contract).
    pub router: Option<Address>,
    pub event: SwapEvent,
    /// Emitter of the log (v3: the pool; v4: expected to be the `PoolManager`). Not verified here.
    pub pool: Address,
    /// v4 only: pool id.
    pub pool_id: Option<B256>,
    /// `msg.sender` of the pool call (router / bot contract).
    pub sender: Address,
    /// Pool-side delta of currency0 (see the sign convention above).
    pub amount0: I256,
    /// Pool-side delta of currency1.
    pub amount1: I256,
    pub sqrt_price_x96: U256,
    pub liquidity: u128,
    pub tick: i32,
    /// v4 only: fee of this swap in pips (hundredths of a bip; dynamic-fee aware).
    pub fee_pips: Option<u32>,
}

/// Why a log with a `Swap` topic0 did not decode.
#[derive(Debug)]
pub enum SwapError {
    /// Number of topics differs from the event's (3 for both v3 and v4).
    TopicCount(usize),
    /// ABI decoding of topics/data failed.
    Abi(alloy_sol_types::Error),
}

impl std::fmt::Display for SwapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TopicCount(n) => write!(f, "{n} topics, expected 3"),
            Self::Abi(e) => write!(f, "abi: {e}"),
        }
    }
}

impl std::error::Error for SwapError {}

/// Classification of one log.
#[derive(Debug)]
pub enum SwapDecode {
    /// topic0 is not a v3/v4 `Swap` (or the log has no topics).
    NotSwap,
    Swap(PoolSwap),
    Malformed(SwapError),
}

/// Number of topics of both `Swap` events (topic0 + 2 indexed).
const SWAP_TOPICS: usize = 3;

/// Classifies `log` of the transaction `ctx`.
#[must_use]
pub fn decode_swap(ctx: TxCtx<'_>, log: &Log) -> SwapDecode {
    let event = match log.topics.first() {
        Some(t) if *t == TOPIC_SWAP_V3 => SwapEvent::V3,
        Some(t) if *t == TOPIC_SWAP_V4 => SwapEvent::V4,
        _ => return SwapDecode::NotSwap,
    };
    // Checked explicitly: the position of indexed fields is part of the ABI, and an event with the
    // same signature but different `indexed` must not decode as a Uniswap swap.
    if log.topics.len() != SWAP_TOPICS {
        return SwapDecode::Malformed(SwapError::TopicCount(log.topics.len()));
    }
    let topics = log.topics.iter().copied();
    let fields = match event {
        SwapEvent::V3 => v3::Swap::decode_raw_log(topics, &log.data).map(|e| SwapFields {
            pool_id: None,
            sender: e.sender,
            amount0: e.amount0,
            amount1: e.amount1,
            sqrt_price_x96: U256::from(e.sqrtPriceX96),
            liquidity: e.liquidity,
            tick: e.tick.as_i32(),
            fee_pips: None,
        }),
        SwapEvent::V4 => v4::Swap::decode_raw_log(topics, &log.data).map(|e| SwapFields {
            pool_id: Some(e.id),
            sender: e.sender,
            amount0: pool_side(e.amount0),
            amount1: pool_side(e.amount1),
            sqrt_price_x96: U256::from(e.sqrtPriceX96),
            liquidity: e.liquidity,
            tick: e.tick.as_i32(),
            fee_pips: Some(e.fee.to::<u32>()),
        }),
    };
    match fields {
        Ok(f) => SwapDecode::Swap(f.into_swap(ctx, log, event)),
        Err(e) => SwapDecode::Malformed(SwapError::Abi(e)),
    }
}

/// Event-specific part of a [`PoolSwap`], already in the pool-side sign convention. Named fields
/// instead of positional arguments: `amount0`/`amount1` share a type and are easy to swap.
struct SwapFields {
    pool_id: Option<B256>,
    sender: Address,
    amount0: I256,
    amount1: I256,
    sqrt_price_x96: U256,
    liquidity: u128,
    tick: i32,
    fee_pips: Option<u32>,
}

impl SwapFields {
    /// Adds the position and tx context shared by both events.
    fn into_swap(self, ctx: TxCtx<'_>, log: &Log, event: SwapEvent) -> PoolSwap {
        let Self { pool_id, sender, amount0, amount1, sqrt_price_x96, liquidity, tick, fee_pips } = self;
        PoolSwap {
            block_number: ctx.block,
            tx_index: ctx.tx.index,
            log_index: log.index,
            tx_hash: ctx.tx.hash,
            trader: ctx.tx.from,
            router: ctx.tx.to,
            event,
            pool: log.address,
            pool_id,
            sender,
            amount0,
            amount1,
            sqrt_price_x96,
            liquidity,
            tick,
            fee_pips,
        }
    }
}

/// v4 swapper delta (int128) -> pool-side delta. Negating in I256 cannot overflow for an int128.
fn pool_side(swapper_delta: i128) -> I256 {
    -I256::try_from(swapper_delta).expect("i128 always fits I256")
}

/// Counters of [`decode_block`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SwapCounters {
    /// Logs looked at.
    pub logs: u64,
    pub v3: u64,
    pub v4: u64,
    /// Logs with a `Swap` topic0 that did not decode (see [`BlockSwaps::malformed`]).
    pub malformed: u64,
}

impl SwapCounters {
    /// Adds `o` to `self`.
    pub fn merge(&mut self, o: &SwapCounters) {
        let SwapCounters { logs, v3, v4, malformed } = o;
        self.logs += logs;
        self.v3 += v3;
        self.v4 += v4;
        self.malformed += malformed;
    }
}

/// A malformed `Swap` log and its position.
#[derive(Debug)]
pub struct MalformedSwap {
    pub block_number: u64,
    pub tx_index: u32,
    pub log_index: u32,
    pub error: SwapError,
}

/// Decoder output for one block.
#[derive(Debug, Default)]
pub struct BlockSwaps {
    pub swaps: Vec<PoolSwap>,
    pub malformed: Vec<MalformedSwap>,
    pub counters: SwapCounters,
}

/// Every swap of `block`, in `(tx_index, log_index)` order.
#[must_use]
pub fn decode_block(block: &Block) -> BlockSwaps {
    let mut out = BlockSwaps::default();
    for ctx in block.tx_ctxs() {
        for log in &ctx.tx.logs {
            out.counters.logs += 1;
            match decode_swap(ctx, log) {
                SwapDecode::NotSwap => {}
                SwapDecode::Swap(s) => {
                    match s.event {
                        SwapEvent::V3 => out.counters.v3 += 1,
                        SwapEvent::V4 => out.counters.v4 += 1,
                    }
                    out.swaps.push(s);
                }
                SwapDecode::Malformed(error) => {
                    out.counters.malformed += 1;
                    out.malformed.push(MalformedSwap {
                        block_number: ctx.block,
                        tx_index: ctx.tx.index,
                        log_index: log.index,
                        error,
                    });
                }
            }
        }
    }
    out
}
