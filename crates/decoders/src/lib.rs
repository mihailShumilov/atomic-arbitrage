//! Log decoders. Generic Uniswap v3/v4 + ERC-20 events live here and are
//! verified by topic0 tests. Launchpad-specific events (Pons v1, Pons v2
//! curve, pools.trade) are TODO: their ABIs must come from official sources
//! and be recorded in .claude/skills/hoodchain-mev/references/contracts.md
//! BEFORE any decoder is written.
//!
//! Every decoder must be checked by the data-auditor agent against real
//! logs (Blockscout / RPC) before its output is used in analytics.

use alloy_primitives::{Address, B256, I256};
use alloy_sol_types::SolEvent;

pub mod v3 {
    alloy_sol_types::sol! {
        /// Uniswap v3 pool (Pons v1 trades in per-token v3 pools).
        event Swap(address indexed sender, address indexed recipient, int256 amount0, int256 amount1, uint160 sqrtPriceX96, uint128 liquidity, int24 tick);
    }
}

pub mod v4 {
    alloy_sol_types::sol! {
        /// Uniswap v4 PoolManager singleton: Pons v2 after graduation, pools.trade.
        event Swap(bytes32 indexed id, address indexed sender, int128 amount0, int128 amount1, uint160 sqrtPriceX96, uint128 liquidity, int24 tick, uint24 fee);
        event Initialize(bytes32 indexed id, address indexed currency0, address indexed currency1, uint24 fee, int24 tickSpacing, address hooks, uint160 sqrtPriceX96, int24 tick);
    }
}

pub mod erc20 {
    alloy_sol_types::sol! {
        event Transfer(address indexed from, address indexed to, uint256 value);
    }
}

pub const TOPIC_SWAP_V3: B256 = v3::Swap::SIGNATURE_HASH;
pub const TOPIC_SWAP_V4: B256 = v4::Swap::SIGNATURE_HASH;
pub const TOPIC_INITIALIZE_V4: B256 = v4::Initialize::SIGNATURE_HASH;
pub const TOPIC_TRANSFER: B256 = erc20::Transfer::SIGNATURE_HASH;

/// Every topic0 the decoders in this crate understand. The enricher's `logs`
/// mode uses this as its default `eth_getLogs` filter, so adding a decoder
/// here automatically widens what gets fetched.
pub const ALL_TOPIC0: [B256; 4] = [TOPIC_SWAP_V3, TOPIC_SWAP_V4, TOPIC_INITIALIZE_V4, TOPIC_TRANSFER];

/// Raw log as it comes from receipts.
pub struct RawLog<'a> {
    pub address: Address,
    pub topics: &'a [B256],
    pub data: &'a [u8],
}

/// Venue-agnostic swap. Signs follow the pool's convention: positive amount
/// = token flowed INTO the pool. Mapping to token/quote and buy/sell needs
/// pool metadata (which currency is the meme token) and happens downstream.
#[derive(Debug, Clone, PartialEq)]
pub struct PoolSwap {
    pub pool: Address,          // v3: pool address; v4: PoolManager address
    pub pool_id: Option<B256>,  // v4 only
    pub sender: Address,
    pub amount0: I256,
    pub amount1: I256,
    pub sqrt_price_x96: alloy_primitives::U256,
    pub liquidity: u128,
    pub tick: i32,
    pub fee_pips: Option<u32>,  // v4 only (dynamic fee aware)
}

pub fn decode_swap(log: &RawLog) -> Option<PoolSwap> {
    let t0 = *log.topics.first()?;
    if t0 == TOPIC_SWAP_V3 {
        let e = v3::Swap::decode_raw_log(log.topics.iter().copied(), log.data).ok()?;
        return Some(PoolSwap {
            pool: log.address,
            pool_id: None,
            sender: e.sender,
            amount0: e.amount0,
            amount1: e.amount1,
            sqrt_price_x96: alloy_primitives::U256::from(e.sqrtPriceX96),
            liquidity: e.liquidity,
            tick: e.tick.as_i32(),
            fee_pips: None,
        });
    }
    if t0 == TOPIC_SWAP_V4 {
        let e = v4::Swap::decode_raw_log(log.topics.iter().copied(), log.data).ok()?;
        return Some(PoolSwap {
            pool: log.address,
            pool_id: Some(e.id),
            sender: e.sender,
            amount0: I256::try_from(e.amount0).ok()?,
            amount1: I256::try_from(e.amount1).ok()?,
            sqrt_price_x96: alloy_primitives::U256::from(e.sqrtPriceX96),
            liquidity: e.liquidity,
            tick: e.tick.as_i32(),
            fee_pips: Some(e.fee.to::<u32>()),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{b256, keccak256};

    #[test]
    fn topics_are_canonical() {
        assert_eq!(TOPIC_SWAP_V3, b256!("c42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67"));
        assert_eq!(TOPIC_TRANSFER, b256!("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"));
        assert_eq!(TOPIC_SWAP_V4, keccak256("Swap(bytes32,address,int128,int128,uint160,uint128,int24,uint24)"));
        assert_eq!(TOPIC_INITIALIZE_V4, keccak256("Initialize(bytes32,address,address,uint24,int24,address,uint160,int24)"));
    }

    #[test]
    fn print_topics() {
        println!("v3 Swap       {TOPIC_SWAP_V3}");
        println!("v4 Swap       {TOPIC_SWAP_V4}");
        println!("v4 Initialize {TOPIC_INITIALIZE_V4}");
    }
}
