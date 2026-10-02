//! Event ABIs (`sol!`) and their topic0. Generic Uniswap v3/v4, ERC-20 and the Arbitrum token
//! bridge. Launchpad events (Pons v1/v2, pools.trade) come here only after their official ABIs
//! are saved in `abi/<venue>/` and recorded in references/contracts.md.

use alloy_primitives::B256;
use alloy_sol_types::SolEvent;

/// Uniswap v3 pool events.
pub mod v3 {
    alloy_sol_types::sol! {
        /// Uniswap v3 pool (Pons v1 trades in per-token v3 pools).
        event Swap(address indexed sender, address indexed recipient, int256 amount0, int256 amount1, uint160 sqrtPriceX96, uint128 liquidity, int24 tick);
    }
}

/// Uniswap v4 `PoolManager` events.
pub mod v4 {
    alloy_sol_types::sol! {
        /// Uniswap v4 `PoolManager` singleton: Pons v2 after graduation, pools.trade.
        event Swap(bytes32 indexed id, address indexed sender, int128 amount0, int128 amount1, uint160 sqrtPriceX96, uint128 liquidity, int24 tick, uint24 fee);
        /// Pool creation in the `PoolManager`.
        event Initialize(bytes32 indexed id, address indexed currency0, address indexed currency1, uint24 fee, int24 tickSpacing, address hooks, uint160 sqrtPriceX96, int24 tick);
    }
}

/// ERC-20 events.
pub mod erc20 {
    alloy_sol_types::sol! {
        /// ERC-20 transfer; mint/burn = from/to the zero address (also WETH wrap/unwrap on L2).
        event Transfer(address indexed from, address indexed to, uint256 value);
    }
}

/// Arbitrum token bridge events (L2 side).
pub mod token_bridge {
    alloy_sol_types::sol! {
        /// Arbitrum token bridge, L2 side (`L2ArbitrumGateway.sol`). Emitted by the
        /// L2 gateway when a deposit from L1 is finalized (inside a `0x68` `RetryTx`).
        /// Source: OffchainLabs/token-bridge-contracts @ 0746a71321cdb2d6df6b15158c7ecbb9ece84b12,
        /// see abi/arbitrum-token-bridge/SOURCE.md.
        event DepositFinalized(address indexed l1Token, address indexed from, address indexed to, uint256 amount);
        /// Same contract: withdrawal L2 -> L1 started. Not decoded yet, only pinned by the topic test.
        event WithdrawalInitiated(address l1Token, address indexed from, address indexed to, uint256 indexed l2ToL1Id, uint256 exitNum, uint256 amount);
    }
}

/// Uniswap v3 `Swap`.
pub const TOPIC_SWAP_V3: B256 = v3::Swap::SIGNATURE_HASH;
/// Uniswap v4 `Swap`.
pub const TOPIC_SWAP_V4: B256 = v4::Swap::SIGNATURE_HASH;
/// Uniswap v4 `Initialize`.
pub const TOPIC_INITIALIZE_V4: B256 = v4::Initialize::SIGNATURE_HASH;
/// ERC-20 `Transfer`.
pub const TOPIC_TRANSFER: B256 = erc20::Transfer::SIGNATURE_HASH;
/// Arbitrum token bridge `DepositFinalized`.
pub const TOPIC_DEPOSIT_FINALIZED: B256 = token_bridge::DepositFinalized::SIGNATURE_HASH;
/// Arbitrum token bridge `WithdrawalInitiated`.
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{b256, keccak256};

    /// Literals = the topic0 table of references/contracts.md.
    #[test]
    fn topics_are_canonical() {
        assert_eq!(TOPIC_SWAP_V3, b256!("c42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67"));
        assert_eq!(TOPIC_TRANSFER, b256!("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"));
        assert_eq!(TOPIC_SWAP_V4, keccak256("Swap(bytes32,address,int128,int128,uint160,uint128,int24,uint24)"));
        assert_eq!(TOPIC_SWAP_V4, b256!("40e9cecb9f5f1f1c5b9c97dec2917b7ee92e57ba5563708daca94dd84ad7112f"));
        assert_eq!(
            TOPIC_INITIALIZE_V4,
            keccak256("Initialize(bytes32,address,address,uint24,int24,address,uint160,int24)")
        );
        assert_eq!(TOPIC_INITIALIZE_V4, b256!("dd466e674ea557f56295e2d0218a125ea4b4f0f6f3307b95f85e6110838d6438"));
        // Arbitrum token bridge, OffchainLabs/token-bridge-contracts @ 0746a71321cdb2d6df6b15158c7ecbb9ece84b12
        // (2026-03-13), contracts/tokenbridge/arbitrum/gateway/L2ArbitrumGateway.sol; abi/arbitrum-token-bridge/SOURCE.md.
        // DepositFinalized: seen on chain in block 77312169, tx 0x8a448fd9b19c63c41c5f3045b08b38745ed7bf80dd5deb3a34a9218424c05d4a, log 4.
        assert_eq!(TOPIC_DEPOSIT_FINALIZED, b256!("c7f2e9c55c40a50fbc217dfc70cd39a222940dfa62145aa0ca49eb9535d4fcb2"));
        assert_eq!(TOPIC_DEPOSIT_FINALIZED, keccak256("DepositFinalized(address,address,address,uint256)"));
        // WithdrawalInitiated: not observed on chain yet (2026-10-01).
        assert_eq!(
            TOPIC_WITHDRAWAL_INITIATED,
            b256!("3073a74ecb728d10be779fe19a74a1428e20468f5b4d167bf9c73d9067847d73")
        );
        assert_eq!(
            TOPIC_WITHDRAWAL_INITIATED,
            keccak256("WithdrawalInitiated(address,address,address,uint256,uint256,uint256)")
        );
    }
}
