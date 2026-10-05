//! `hood.swaps` rows and the audit TSV of decoded swaps on a real block.
//!
//! Fixture `fixtures/swaps-blocks.jsonl`: block 74744924 (see tests/swaps.rs for its source):
//! - tx 3 0xa28443b7016531fd863ae2cc3abc8f9f75abd1ccc54f58216d4de3bc63961277, log 3: v4 `Swap`,
//!   pool id 0x50f0…956c, WETH out of the `PoolManager` (log 4), token 0x61fc…2f45 in (log 5);
//! - tx 4 0x7db0d5bfb2b1c308b53a6bab03643ae0d318aa5ef72a4a7cc2746d7720d8b34d, log 8: v3 `Swap`,
//!   pool 0x52e6…71ca, WETH in (log 7), USDG out (log 6).
//!
//! The registries below are TEST INPUTS, not registry facts: currencies of both pools are read
//! from the `Transfer` logs of the same receipts (and agree with `currency0 < currency1`);
//! USDG (`todo` in references/contracts.md) with 6 decimals and the token with 18 decimals are
//! assumptions of this test; venues `uni_v3`/`uni_v4` are placeholders; the v4 `PoolManager` is
//! `observed`. Expected floats were checked independently with Python `Decimal` (task 033 report).

use alloy_primitives::{address, b256, Address};
use decoders::pools::{PoolEntry, PoolRef, PoolRegistry, PoolSource, TokenEntry, TokenRegistry};
use decoders::registry::RegistryStatus;
use decoders::rows::{Side, SwapRow, Venue};
use decoders::swap_rows::{swap_row, RowInputs, SkipReason, SwapRowCounters};
use decoders::swaps::{decode_block, PoolSwap, SwapEvent};
use decoders::{addresses::L2_WETH, parse_block_line};

const FIXTURE: &str = include_str!("fixtures/swaps-blocks.jsonl");

const POOL_MANAGER_OBSERVED: Address = address!("8366a39cc670b4001a1121b8f6a443a643e40951");
const USDG_TODO: Address = address!("5fc5360d0400a0fd4f2af552add042d716f1d168");
const MEME: Address = address!("61fcfb63902e5976908e1d7137530ffbf5df2f45");
const V3_POOL: Address = address!("52e65b17fb6e5ba00ed806f37afcd2daa50271ca");

fn registries() -> (PoolRegistry, TokenRegistry) {
    let mut pools = PoolRegistry::default();
    pools
        .insert(PoolEntry {
            pool: PoolRef::V3(V3_POOL),
            currency0: L2_WETH,
            currency1: USDG_TODO,
            venue: Venue::UniV3,
            fee_pips: None,
            hooks: None,
            status: RegistryStatus::Observed,
            source: PoolSource::Registry,
        })
        .unwrap();
    pools
        .insert(PoolEntry {
            pool: PoolRef::V4 {
                manager: POOL_MANAGER_OBSERVED,
                id: b256!("50f084055b462da4b7639a15b67e3ee53753563a4429806878379237a115956c"),
            },
            currency0: L2_WETH,
            currency1: MEME,
            venue: Venue::UniV4,
            fee_pips: None,
            hooks: None,
            status: RegistryStatus::Observed,
            source: PoolSource::Registry,
        })
        .unwrap();
    let mut tokens = TokenRegistry::builtin();
    tokens
        .insert(TokenEntry {
            token: USDG_TODO,
            decimals: Some(6),
            quote_rank: Some(1),
            status: RegistryStatus::Observed,
        })
        .unwrap();
    tokens
        .insert(TokenEntry { token: MEME, decimals: Some(18), quote_rank: None, status: RegistryStatus::Observed })
        .unwrap();
    (pools, tokens)
}

fn swaps() -> Vec<PoolSwap> {
    decode_block(&parse_block_line(FIXTURE.lines().next().unwrap()).unwrap()).swaps
}

/// tx 3: the trader sold 229 716 241.108 tokens (into the pool) for 0.2283 WETH, fee 0.3% of the
/// event; tx 4: sold 0.661 WETH for 1 756.302474 USDG (USDG ranks before WETH as quote), v3 fee
/// unknown -> `nan`.
const ROWS: &str = "\
block_number\ttx_index\tlog_index\tvenue\tpool\ttoken\tquote\ttrader\trouter\tside\ttoken_amount_raw\tquote_amount_raw\tquote_amount\tprice\tfee_quote
74744924\t3\t3\tuni_v4\t0x50f084055b462da4b7639a15b67e3ee53753563a4429806878379237a115956c\t0x61fcfb63902e5976908e1d7137530ffbf5df2f45\t0x0bd7d308f8e1639fab988df18a8011f41eacad73\t0x596c58cce408c62b6fa5bcc03291a85c7af5e519\t0xe03952268a04afe16cc1b02c612478bd54d6b7a6\tsell\t229716241107999980000000000\t228335017402612712\t0.22833501740261272\t0.000000000993987261419893\t0.0006850050522078381
74744924\t4\t8\tuni_v3\t0x52e65b17fb6e5ba00ed806f37afcd2daa50271ca\t0x0bd7d308f8e1639fab988df18a8011f41eacad73\t0x5fc5360d0400a0fd4f2af552add042d716f1d168\t0x9e6441c6d930f2e98bfb6cae6dc46729c862055f\t0xc33acc93942105c57602e59e8e448c20ba718a85\tsell\t660973329707868054\t1756302474\t1756.302474\t2657.1457501564205\tnan
";

/// Audit TSV of the same two swaps: pool-side signs (v4 raw event amount0 = +228335017402612712
/// is written negated), `pool_id`/`fee_pips` empty for v3.
const POOL_SWAPS: &str = "\
block_number\ttx_index\tlog_index\ttx_hash\tevent\temitter\tpool_id\tsender\ttrader\trouter\tamount0\tamount1\tsqrt_price_x96\tliquidity\ttick\tfee_pips
74744924\t3\t3\t0xa28443b7016531fd863ae2cc3abc8f9f75abd1ccc54f58216d4de3bc63961277\tv4\t0x8366a39cc670b4001a1121b8f6a443a643e40951\t0x50f084055b462da4b7639a15b67e3ee53753563a4429806878379237a115956c\t0xe03952268a04afe16cc1b02c612478bd54d6b7a6\t0x596c58cce408c62b6fa5bcc03291a85c7af5e519\t0xe03952268a04afe16cc1b02c612478bd54d6b7a6\t-228335017402612712\t229716241107999980000000000\t2517823344577893815381440112942427\t1054954277126839729686337\t207341\t3000
74744924\t4\t8\t0x7db0d5bfb2b1c308b53a6bab03643ae0d318aa5ef72a4a7cc2746d7720d8b34d\tv3\t0x52e65b17fb6e5ba00ed806f37afcd2daa50271ca\t\t0xc33acc93942105c57602e59e8e448c20ba718a85\t0x9e6441c6d930f2e98bfb6cae6dc46729c862055f\t0xc33acc93942105c57602e59e8e448c20ba718a85\t660973329707868054\t-1756302474\t4084204449514342393577472\t4886374150637239697\t-197470\t
";

#[test]
fn golden_rows_on_fixture() {
    let (pools, tokens) = registries();
    let inputs = RowInputs { pools: &pools, tokens: &tokens, min_status: RegistryStatus::Observed };
    let mut out = Vec::new();
    SwapRow::write_tsv_header(&mut out).unwrap();
    let mut counters = SwapRowCounters::default();
    for s in &swaps() {
        let m = swap_row(s, &inputs);
        counters.record(s, &m);
        m.unwrap().row.write_tsv(&mut out).unwrap();
    }
    assert_eq!(String::from_utf8(out).unwrap(), ROWS);
    assert!(counters.is_balanced());
    assert_eq!(counters.rows.get(&Venue::UniV3), Some(&1));
    assert_eq!(counters.rows.get(&Venue::UniV4), Some(&1));
    assert!(counters.skipped.is_empty());
    assert_eq!((counters.price_unknown, counters.fee_unknown, counters.v4_fee_zero), (0, 1, 0));
    assert_eq!((counters.v4_rows_with_hooks, counters.v4_rows_hooks_unknown), (0, 1));
}

#[test]
fn golden_audit_tsv_on_fixture() {
    let mut out = Vec::new();
    PoolSwap::write_tsv_header(&mut out).unwrap();
    for s in &swaps() {
        s.write_tsv(&mut out).unwrap();
    }
    assert_eq!(String::from_utf8(out).unwrap(), POOL_SWAPS);
}

/// Side is relative to the token, independent of which currency is currency0.
#[test]
fn sides_on_fixture() {
    let (pools, tokens) = registries();
    let inputs = RowInputs { pools: &pools, tokens: &tokens, min_status: RegistryStatus::Observed };
    let rows: Vec<_> = swaps().iter().map(|s| swap_row(s, &inputs).unwrap().row).collect();
    // v4: token currency1 into the pool; v3: token currency0 (WETH) into the pool. Both sells.
    assert_eq!(rows.iter().map(|r| r.side).collect::<Vec<_>>(), [Side::Sell, Side::Sell]);
}

/// With `min_status = verified` nothing of this fixture is a row (the v4 `PoolManager`, USDG and
/// the v3 pool are not `verified`); with no registry every swap is `no_pool_meta`. Every swap is
/// counted either way.
#[test]
fn verified_only_and_empty_registry_skip_everything() {
    let (pools, tokens) = registries();
    let mut c = SwapRowCounters::default();
    let verified = RowInputs { pools: &pools, tokens: &tokens, min_status: RegistryStatus::Verified };
    for s in &swaps() {
        let m = swap_row(s, &verified);
        assert_eq!(m.as_ref().map(|_| ()).unwrap_err(), &SkipReason::BelowMinStatus);
        c.record(s, &m);
    }
    let empty = PoolRegistry::default();
    let none = RowInputs { pools: &empty, tokens: &tokens, min_status: RegistryStatus::Observed };
    for s in &swaps() {
        let m = swap_row(s, &none);
        assert_eq!(m.as_ref().map(|_| ()).unwrap_err(), &SkipReason::NoPoolMeta);
        c.record(s, &m);
    }
    assert!(c.rows.is_empty() && c.is_balanced());
    assert_eq!(c.swaps.get(&SwapEvent::V3), Some(&2));
    assert_eq!(c.skipped.get(&(SwapEvent::V4, SkipReason::BelowMinStatus)), Some(&1));
}
