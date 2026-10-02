//! Uniswap v3/v4 `Swap` decoder on a real block.
//!
//! Fixture `fixtures/swaps-blocks.jsonl`: block 74744924 (2026-09-28T11:00:35Z), one unchanged
//! line of `data/samples/hourly-20260804-20260930.jsonl.zst` (task 010: public RPC
//! `eth_getBlockByNumber(full)` + `eth_getBlockReceipts`, fetched 2026-10-01; block hash
//! 0xdff18dcfb69192b603464b96892c0f115d32170c977c33b34b75ca8bba35fbb3 = index.tsv of the sample).
//! - tx 3 0xa28443b7016531fd863ae2cc3abc8f9f75abd1ccc54f58216d4de3bc63961277, log 3: v4 `Swap`
//!   (PoolManager), WETH out of the pool to the swapper (log 4), token into the pool (log 5);
//! - tx 4 0x7db0d5bfb2b1c308b53a6bab03643ae0d318aa5ef72a4a7cc2746d7720d8b34d, log 8: v3 `Swap`,
//!   WETH into the pool (log 7), USDG out of the pool (log 6).
//!
//! Tests marked "synthetic" mutate a real log to reach the malformed paths.

use std::collections::BTreeMap;

use alloy_primitives::{address, b256, Address, Bytes, B256, I256, U256};
use alloy_sol_types::SolEvent;
use decoders::swaps::{decode_block, decode_swap, SwapDecode, SwapError, SwapEvent};
use decoders::{parse_block_line, v3, Block, Log, TxCtx, TOPIC_TRANSFER};

const FIXTURE: &str = include_str!("fixtures/swaps-blocks.jsonl");

// Uniswap v4 PoolManager: `observed` in references/contracts.md (2026-09-28). Used here ONLY as
// an expected value of the fixture; the decoder itself takes the emitter from the log.
const POOL_MANAGER_OBSERVED: Address = address!("8366a39cc670b4001a1121b8f6a443a643e40951");

fn block() -> Block {
    parse_block_line(FIXTURE.lines().next().unwrap()).unwrap()
}

fn i256(s: &str) -> I256 {
    I256::from_dec_str(s).unwrap()
}

#[test]
fn block_counters() {
    let b = block();
    assert_eq!(b.number, 74744924);
    let r = decode_block(&b);
    assert_eq!((r.counters.logs, r.counters.v3, r.counters.v4, r.counters.malformed), (9, 1, 1, 0));
    assert!(r.malformed.is_empty());
    // (tx_index, log_index) order.
    let pos: Vec<_> = r.swaps.iter().map(|s| (s.tx_index, s.log_index)).collect();
    assert_eq!(pos, [(3, 3), (4, 8)]);
}

#[test]
fn v4_swap_fields_and_pool_side_signs() {
    let r = decode_block(&block());
    let s = &r.swaps[0];
    assert_eq!(s.event, SwapEvent::V4);
    assert_eq!((s.block_number, s.tx_index, s.log_index), (74744924, 3, 3));
    assert_eq!(s.tx_hash, b256!("a28443b7016531fd863ae2cc3abc8f9f75abd1ccc54f58216d4de3bc63961277"));
    assert_eq!(s.trader, address!("596c58cce408c62b6fa5bcc03291a85c7af5e519"));
    assert_eq!(s.router, Some(address!("e03952268a04afe16cc1b02c612478bd54d6b7a6")));
    assert_eq!(s.pool, POOL_MANAGER_OBSERVED);
    assert_eq!(s.pool_id, Some(b256!("50f084055b462da4b7639a15b67e3ee53753563a4429806878379237a115956c")));
    assert_eq!(s.sender, address!("e03952268a04afe16cc1b02c612478bd54d6b7a6"));
    // Raw event: amount0 = +228335017402612712 (swapper receives WETH), amount1 < 0 (swapper pays).
    // Pool side: WETH left the pool (log 4: PoolManager -> swapper), the token came in (log 5).
    assert_eq!(s.amount0, i256("-228335017402612712"));
    assert_eq!(s.amount1, i256("229716241107999980000000000"));
    assert_eq!(s.sqrt_price_x96, U256::from_str_radix("2517823344577893815381440112942427", 10).unwrap());
    assert_eq!(s.liquidity, 1_054_954_277_126_839_729_686_337);
    assert_eq!(s.tick, 207341);
    assert_eq!(s.fee_pips, Some(3000)); // 0.3%
}

#[test]
fn v3_swap_fields_and_signs() {
    let r = decode_block(&block());
    let s = &r.swaps[1];
    assert_eq!(s.event, SwapEvent::V3);
    assert_eq!((s.block_number, s.tx_index, s.log_index), (74744924, 4, 8));
    assert_eq!(s.tx_hash, b256!("7db0d5bfb2b1c308b53a6bab03643ae0d318aa5ef72a4a7cc2746d7720d8b34d"));
    assert_eq!(s.trader, address!("9e6441c6d930f2e98bfb6cae6dc46729c862055f"));
    assert_eq!(s.router, Some(address!("c33acc93942105c57602e59e8e448c20ba718a85")));
    assert_eq!(s.pool, address!("52e65b17fb6e5ba00ed806f37afcd2daa50271ca"));
    assert_eq!(s.pool_id, None);
    assert_eq!(s.sender, address!("c33acc93942105c57602e59e8e448c20ba718a85"));
    // WETH into the pool (log 7), 1756.302474 USDG-units out (log 6).
    assert_eq!(s.amount0, i256("660973329707868054"));
    assert_eq!(s.amount1, i256("-1756302474"));
    assert_eq!(s.sqrt_price_x96, U256::from_str_radix("4084204449514342393577472", 10).unwrap());
    assert_eq!(s.liquidity, 4_886_374_150_637_239_697);
    assert_eq!(s.tick, -197470);
    assert_eq!(s.fee_pips, None);
}

/// Independent of the literals above: the pool's net ERC-20 flow per token in the same receipt
/// equals {amount0, amount1} with the documented sign (positive = into the pool).
#[test]
fn signs_match_transfer_logs() {
    let b = block();
    let r = decode_block(&b);
    for s in &r.swaps {
        let tx = &b.txs[s.tx_index as usize];
        let mut net: BTreeMap<Address, I256> = BTreeMap::new();
        for l in tx.logs.iter().filter(|l| l.topics.len() == 3 && l.topics[0] == TOPIC_TRANSFER) {
            let v = I256::try_from(U256::from_be_slice(&l.data)).unwrap();
            if l.topics[2] == s.pool.into_word() {
                *net.entry(l.address).or_default() += v;
            }
            if l.topics[1] == s.pool.into_word() {
                *net.entry(l.address).or_default() -= v;
            }
        }
        let mut flows: Vec<I256> = net.into_values().collect();
        let mut amounts = vec![s.amount0, s.amount1];
        flows.sort();
        amounts.sort();
        assert_eq!(flows, amounts, "swap at tx {} log {}", s.tx_index, s.log_index);
    }
}

fn v3_log() -> (Block, Log) {
    let b = block();
    let log = b.txs[4].logs.iter().find(|l| l.index == 8).unwrap().clone();
    (b, log)
}

fn classify(b: &Block, log: &Log) -> SwapDecode {
    decode_swap(TxCtx { block: b.number, tx: &b.txs[4] }, log)
}

#[test]
fn synthetic_wrong_topic_count_is_malformed() {
    let (b, mut log) = v3_log();
    log.topics.pop();
    assert!(matches!(classify(&b, &log), SwapDecode::Malformed(SwapError::TopicCount(2))));
    let (b, mut log) = v3_log();
    log.topics.push(B256::ZERO);
    assert!(matches!(classify(&b, &log), SwapDecode::Malformed(SwapError::TopicCount(4))));
}

#[test]
fn synthetic_short_data_is_malformed() {
    let (b, mut log) = v3_log();
    log.data = Bytes::from(log.data[..64].to_vec());
    assert!(matches!(classify(&b, &log), SwapDecode::Malformed(SwapError::Abi(_))));
}

#[test]
fn not_swap() {
    let (b, mut log) = v3_log();
    log.topics[0] = TOPIC_TRANSFER;
    assert!(matches!(classify(&b, &log), SwapDecode::NotSwap));
    log.topics.clear();
    assert!(matches!(classify(&b, &log), SwapDecode::NotSwap));
}

/// Pins what alloy-sol-types 1.7.3 itself does with a wrong topic count (review 2026-10-02, B5,
/// was an open assumption): both fewer and extra topics are an error. The explicit check in
/// `decode_swap` does not rely on it, it only gives the error a typed reason.
#[test]
fn alloy_decode_raw_log_rejects_wrong_topic_count() {
    let (_, log) = v3_log();
    let two = &log.topics[..2];
    let mut four = log.topics.clone();
    four.push(B256::ZERO);
    assert!(v3::Swap::decode_raw_log(log.topics.iter().copied(), &log.data).is_ok());
    assert!(v3::Swap::decode_raw_log(two.iter().copied(), &log.data).is_err());
    assert!(v3::Swap::decode_raw_log(four.iter().copied(), &log.data).is_err());
}
