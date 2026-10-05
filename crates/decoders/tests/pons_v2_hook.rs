//! Venue `pons_v2_hook` (task 036) on a real block.
//!
//! Fixture `fixtures/pons-v2-hook-block.jsonl`: block 61620482, one unchanged line of
//! `data/samples/hourly-20260804-20260930.jsonl.zst` (task 010: public RPC
//! `eth_getBlockByNumber(full)` + `eth_getBlockReceipts`; block hash
//! 0xa93d11ab1de487612faee17d3353dbe7545288453e84f2eabf0bb334e213bb1d).
//! - tx 1 0x9c5612d2cc50ceba13b51b1222dd2703a100ed3ef4ee4f610cedb77b734f7af8, log 1: v4 `Swap` of
//!   the `PoolManager`, pool id 0xfaf0…53aa (native ETH / token 0x35a7…ac13); the token goes into
//!   the `PoolManager` (log 0), the pool pays 5 762 611 981 068 415 wei, event fee 0;
//! - log 2: `HookFeeCollected` of the Pons v2 meme hook for the same pool id. Not decoded here; its
//!   third word, 57 626 119 810 684, is 1% of the ETH leg (assumed to be the hook fee): what the
//!   trader got is not the AMM leg in the row, which is why these pools have a venue of their own.
//!
//! The pool line is the one of `data/registry/pools-035.tsv` (PoolKey proven by
//! `keccak256(abi.encode(key))` = pool id from the `verified` PoolManager, task 035), still with
//! venue `other` as written there. The hook address comes from `decoders::addresses` (`verified`).

use alloy_primitives::{address, b256, Address, U256};
use decoders::addresses::PONS_V2_MEME_HOOK;
use decoders::parse_block_line;
use decoders::pools::{PoolRef, PoolRegistry, TokenRegistry};
use decoders::registry::RegistryStatus;
use decoders::rows::{Side, SwapPool, Venue};
use decoders::swap_rows::{swap_row, RowInputs, SwapRowCounters};
use decoders::swaps::decode_block;

const FIXTURE: &str = include_str!("fixtures/pons-v2-hook-block.jsonl");

/// Line of data/registry/pools-035.tsv (task 035) for the pool of the fixture swap.
const POOL_TSV: &str = "other\t0x8366a39cc670b4001a1121b8f6a443a643e40951\t\
0xfaf0d4093602eb2d7f80ce7ba50cffaeece9c5d546b6df363107779e5ff553aa\t\
0x0000000000000000000000000000000000000000\t0x35a79120e07bae083045d44b8349c5d47f37ac13\t-\t\
0xe5e702641ea86f4ae6cc3cdaed2b886f976be044\tverified\tsrc=035:derived;key_fee=0;tick_spacing=200\n";

const POOL_MANAGER: Address = address!("8366a39cc670b4001a1121b8f6a443a643e40951");
const POOL_ID: alloy_primitives::B256 = b256!("faf0d4093602eb2d7f80ce7ba50cffaeece9c5d546b6df363107779e5ff553aa");
const TOKEN: Address = address!("35a79120e07bae083045d44b8349c5d47f37ac13");

fn venue_of(pools: &PoolRegistry) -> Venue {
    pools.get(&PoolRef::V4 { manager: POOL_MANAGER, id: POOL_ID }).unwrap().venue
}

#[test]
fn registry_line_gets_the_hook_venue_in_any_order() {
    assert_eq!(PONS_V2_MEME_HOOK, address!("e5e702641ea86f4ae6cc3cdaed2b886f976be044"));
    // Without the hook allowed: as before task 036.
    let plain = PoolRegistry::parse_tsv(POOL_TSV).unwrap();
    assert_eq!(venue_of(&plain), Venue::Other);
    // Built-in hooks first, then the TSV (what swaps_scan does).
    let mut builtin = PoolRegistry::builtin();
    builtin.extend(PoolRegistry::parse_tsv(POOL_TSV).unwrap()).unwrap();
    assert_eq!(venue_of(&builtin), Venue::PonsV2Hook);
    // TSV first, hook allowed afterwards.
    let mut late = PoolRegistry::parse_tsv(POOL_TSV).unwrap();
    late.allow_v4_hook(PONS_V2_MEME_HOOK, Venue::PonsV2Hook, RegistryStatus::Verified).unwrap();
    assert_eq!(venue_of(&late), Venue::PonsV2Hook);
    // An `observed` hook makes the pool `observed`.
    let mut obs = PoolRegistry::parse_tsv(POOL_TSV).unwrap();
    obs.allow_v4_hook(PONS_V2_MEME_HOOK, Venue::PonsV2Hook, RegistryStatus::Observed).unwrap();
    let e = obs.get(&PoolRef::V4 { manager: POOL_MANAGER, id: POOL_ID }).unwrap();
    assert_eq!((e.venue, e.status), (Venue::PonsV2Hook, RegistryStatus::Observed));
}

#[test]
fn swap_row_on_fixture_has_venue_pons_v2_hook() {
    let block = parse_block_line(FIXTURE.lines().next().unwrap()).unwrap();
    assert_eq!(block.number, 61_620_482);
    let swaps = decode_block(&block).swaps;
    assert_eq!(swaps.len(), 1);
    let mut pools = PoolRegistry::builtin();
    pools.extend(PoolRegistry::parse_tsv(POOL_TSV).unwrap()).unwrap();
    let tokens = TokenRegistry::builtin();
    let inputs = RowInputs { pools: &pools, tokens: &tokens, min_status: RegistryStatus::Verified };
    let mut counters = SwapRowCounters::default();
    let out = swap_row(&swaps[0], &inputs);
    counters.record(&swaps[0], &out);
    let row = out.unwrap().row;
    assert_eq!((row.block_number, row.tx_index, row.log_index), (61_620_482, 1, 1));
    assert_eq!(row.venue, Venue::PonsV2Hook);
    assert_eq!(row.pool, SwapPool::V4(POOL_ID));
    assert_eq!((row.token, row.quote, row.side), (TOKEN, Address::ZERO, Side::Sell));
    assert_eq!(row.trader, address!("285a86c488a42f1e1eca14798399026aaa9a39a8"));
    assert_eq!(row.router, Some(address!("b1000000096bd2f8ca9b6883182eccaf31e7c3fd")));
    assert_eq!(row.token_amount_raw, U256::from(525_398_551_135_979_485_204_092_u128));
    assert_eq!(row.quote_amount_raw, U256::from(5_762_611_981_068_415_u64));
    assert_eq!(row.fee_quote, 0.0, "event fee 0; the hook fee is not in the row");
    assert!(row.price.is_nan(), "token decimals are not in the built-in registry");
    assert_eq!(counters.rows.get(&Venue::PonsV2Hook), Some(&1));
    assert_eq!((counters.v4_rows_with_hooks, counters.v4_fee_zero), (1, 1));
}
