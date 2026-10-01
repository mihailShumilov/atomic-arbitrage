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

pub mod l1_inflows;

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

pub mod token_bridge {
    alloy_sol_types::sol! {
        /// Arbitrum token bridge, L2 side (`L2ArbitrumGateway.sol`). Emitted by the
        /// L2 gateway when a deposit from L1 is finalized (inside a 0x68 RetryTx).
        /// Source: OffchainLabs/token-bridge-contracts @ 0746a71321cdb2d6df6b15158c7ecbb9ece84b12,
        /// see abi/arbitrum-token-bridge/SOURCE.md.
        event DepositFinalized(address indexed l1Token, address indexed from, address indexed to, uint256 amount);
        /// Same contract: withdrawal L2 -> L1 started. Not decoded yet, only pinned by the topic test.
        event WithdrawalInitiated(address l1Token, address indexed from, address indexed to, uint256 indexed l2ToL1Id, uint256 exitNum, uint256 amount);
    }
}

pub const TOPIC_SWAP_V3: B256 = v3::Swap::SIGNATURE_HASH;
pub const TOPIC_SWAP_V4: B256 = v4::Swap::SIGNATURE_HASH;
pub const TOPIC_INITIALIZE_V4: B256 = v4::Initialize::SIGNATURE_HASH;
pub const TOPIC_TRANSFER: B256 = erc20::Transfer::SIGNATURE_HASH;
pub const TOPIC_DEPOSIT_FINALIZED: B256 = token_bridge::DepositFinalized::SIGNATURE_HASH;
pub const TOPIC_WITHDRAWAL_INITIATED: B256 = token_bridge::WithdrawalInitiated::SIGNATURE_HASH;

/// Every topic0 the decoders in this crate understand. The enricher's `logs`
/// mode uses this as its default `eth_getLogs` filter, so adding a decoder
/// here automatically widens what gets fetched.
///
/// The token bridge topics (`TOPIC_DEPOSIT_FINALIZED`, `TOPIC_WITHDRAWAL_INITIATED`)
/// are deliberately NOT here: the L1 inflow decoder (`l1_inflows`) needs full
/// blocks (0x64 deposits have no logs), so `logs` mode cannot serve it, and
/// widening the enricher filter is a separate decision.
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
        // Arbitrum token bridge, OffchainLabs/token-bridge-contracts @ 0746a71321cdb2d6df6b15158c7ecbb9ece84b12
        // (2026-03-13), contracts/tokenbridge/arbitrum/gateway/L2ArbitrumGateway.sol; abi/arbitrum-token-bridge/SOURCE.md.
        // DepositFinalized: seen on chain in block 77312169, tx 0x8a448fd9b19c63c41c5f3045b08b38745ed7bf80dd5deb3a34a9218424c05d4a, log 4.
        assert_eq!(TOPIC_DEPOSIT_FINALIZED, b256!("c7f2e9c55c40a50fbc217dfc70cd39a222940dfa62145aa0ca49eb9535d4fcb2"));
        assert_eq!(TOPIC_DEPOSIT_FINALIZED, keccak256("DepositFinalized(address,address,address,uint256)"));
        // WithdrawalInitiated: not observed on chain yet (2026-10-01).
        assert_eq!(TOPIC_WITHDRAWAL_INITIATED, b256!("3073a74ecb728d10be779fe19a74a1428e20468f5b4d167bf9c73d9067847d73"));
        assert_eq!(
            TOPIC_WITHDRAWAL_INITIATED,
            keccak256("WithdrawalInitiated(address,address,address,uint256,uint256,uint256)")
        );
    }

    #[test]
    fn print_topics() {
        println!("v3 Swap       {TOPIC_SWAP_V3}");
        println!("v4 Swap       {TOPIC_SWAP_V4}");
        println!("v4 Initialize {TOPIC_INITIALIZE_V4}");
    }
}
