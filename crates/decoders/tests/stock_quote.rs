//! Robinhood Stock Tokens as quotes (task 037) on a real block: a stock/stock pool is priced in
//! the lower address.
//!
//! Fixture `fixtures/stock-pair-block.jsonl`: block 39645654, one unchanged line of
//! `data/samples/hourly-20260804-20260930.jsonl.zst` (task 010: public RPC
//! `eth_getBlockByNumber(full)` + `eth_getBlockReceipts`; block hash
//! 0x11172397689d4e31bd2166d2a5e79170fae4f6d97b1817dbcb1c28c146911868).
//! - tx 2 0x84e981c3d2cd7c1db7338588490a35800ea195dc321253f6e0b7af7bb725e39d, log 0: v4 `Swap` of
//!   the `PoolManager`, pool id 0x4174…9a7f (RDDT 0x05b3…6f4c / SPY 0x117c…4c0c, no hooks); raw
//!   event amounts (swapper side) +43 051 670 910 510 244 RDDT, -9 151 337 342 420 906 SPY, fee 625;
//! - logs 1-4: SPY `Transfer` trader -> `PoolManager` 9 151 337 342 420 906 and RDDT `Transfer`
//!   `PoolManager` -> trader 43 051 670 910 510 244, each followed by the ERC-8056
//!   `TransferWithScaledUI` of the token (not decoded here).
//!
//! The pool line is the one of `data/registry/pools-035.tsv` (`PoolKey` from the v4 `PositionManager`,
//! proven by `keccak256(abi.encode(key))` = pool id, task 035). The token lines are those of
//! `data/registry/tokens-037-verified.tsv`: both stocks have rank `STOCK_QUOTE_RANK`, so RDDT (the
//! lower address, currency0) is the quote and SPY is the token.

use alloy_primitives::{address, b256, Address, U256};
use decoders::parse_block_line;
use decoders::pools::{PoolRegistry, TokenRegistry, STOCK_QUOTE_RANK};
use decoders::registry::RegistryStatus;
use decoders::rows::{Side, SwapPool, Venue};
use decoders::swap_rows::{swap_row, RowInputs, SkipReason};
use decoders::swaps::decode_block;

const FIXTURE: &str = include_str!("fixtures/stock-pair-block.jsonl");

/// Line of data/registry/pools-035.tsv (task 035) for the pool of the fixture swap.
const POOL_TSV: &str = "uni_v4\t0x8366a39cc670b4001a1121b8f6a443a643e40951\t\
0x41743585e01364ab3898ff28b3f09fe5aa9a5b17e722f5bea1adb65a07769a7f\t\
0x05b37fb53a299a1b874a619e1c4c404d52c36f4c\t0x117cc2133c37b721f49de2a7a74833232b3b4c0c\t-\t\
0x0000000000000000000000000000000000000000\tverified\tsrc=035:posm_poolkeys;key_fee=500;tick_spacing=5\n";

/// Lines of data/registry/tokens-037-verified.tsv (task 037) for the two stocks.
const TOKENS_TSV: &str = "0x05b37fb53a299a1b874a619e1c4c404d52c36f4c\t18\t3\tverified\tsymbol=RDDT\n\
0x117cc2133c37b721f49de2a7a74833232b3b4c0c\t18\t3\tverified\tsymbol=SPY\n";

const RDDT: Address = address!("05b37fb53a299a1b874a619e1c4c404d52c36f4c");
const SPY: Address = address!("117cc2133c37b721f49de2a7a74833232b3b4c0c");
const POOL_ID: alloy_primitives::B256 = b256!("41743585e01364ab3898ff28b3f09fe5aa9a5b17e722f5bea1adb65a07769a7f");

fn fixture_swap() -> decoders::swaps::PoolSwap {
    let block = parse_block_line(FIXTURE.lines().next().unwrap()).unwrap();
    assert_eq!(block.number, 39_645_654);
    let swaps = decode_block(&block).swaps;
    assert_eq!(swaps.len(), 1);
    swaps.into_iter().next().unwrap()
}

#[test]
fn stock_pair_is_priced_in_the_lower_address() {
    assert_eq!(STOCK_QUOTE_RANK, 3);
    assert!(RDDT < SPY);
    let s = fixture_swap();
    let mut pools = PoolRegistry::builtin();
    pools.extend(PoolRegistry::parse_tsv(POOL_TSV).unwrap()).unwrap();
    let mut tokens = TokenRegistry::builtin();
    tokens.extend(TokenRegistry::parse_tsv(TOKENS_TSV).unwrap()).unwrap();
    let inputs = RowInputs { pools: &pools, tokens: &tokens, min_status: RegistryStatus::Verified };
    assert_eq!(s.tx_hash, b256!("84e981c3d2cd7c1db7338588490a35800ea195dc321253f6e0b7af7bb725e39d"));
    let row = swap_row(&s, &inputs).unwrap().row;
    assert_eq!((row.block_number, row.tx_index, row.log_index), (39_645_654, 2, 0));
    assert_eq!((row.venue, row.pool), (Venue::UniV4, SwapPool::V4(POOL_ID)));
    // The trader paid SPY (into the pool) and got RDDT: a sell of SPY, priced in RDDT.
    assert_eq!((row.token, row.quote, row.side), (SPY, RDDT, Side::Sell));
    assert_eq!(row.trader, address!("c617da044303fb14ac7f42e87fc1289020e82c9e"));
    assert_eq!(row.router, Some(address!("8876789976decbfcbbbe364623c63652db8c0904")));
    assert_eq!(row.token_amount_raw, U256::from(9_151_337_342_420_906_u64));
    assert_eq!(row.quote_amount_raw, U256::from(43_051_670_910_510_244_u64));
    // Raw token units (18 decimals both); the ERC-8056 uiMultiplier is not applied.
    let price = 43_051_670_910_510_244_f64 / 9_151_337_342_420_906_f64;
    assert!((row.price - price).abs() < 1e-12 * price, "price {}", row.price);
    assert!((row.fee_quote - row.quote_amount * 625.0 / 1e6).abs() < 1e-18);
}

#[test]
fn stock_pair_without_stock_quotes_has_no_row() {
    let s = fixture_swap();
    let mut pools = PoolRegistry::builtin();
    pools.extend(PoolRegistry::parse_tsv(POOL_TSV).unwrap()).unwrap();
    // As in task 035/036: stocks are not quotes -> no_quote.
    let tokens = TokenRegistry::builtin();
    let inputs = RowInputs { pools: &pools, tokens: &tokens, min_status: RegistryStatus::Verified };
    assert_eq!(swap_row(&s, &inputs).map(|_| ()), Err(SkipReason::NoQuote));
}
