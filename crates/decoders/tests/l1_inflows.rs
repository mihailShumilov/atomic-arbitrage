//! L1 inflow decoder on real blocks.
//!
//! Fixture `fixtures/l1-inflows-blocks.jsonl`: four blocks in the `data/blocks/blocks-*.jsonl.zst`
//! line format, assembled from raw public-RPC responses (`eth_getBlockByNumber(full)` +
//! `eth_getBlockReceipts`, unchanged, wrapped as `{"number", "block", "receipts"}`); the first three
//! were saved by task 014, the fourth fetched by task 017, both on 2026-10-01:
//! - 77285531 — feed kind 12, tx 1 = 0x64 0x3f8e47b000a564834eb5c32a200b620ea02daa8e32ad48b1e29e1277dcb522c1
//! - 77300695 — feed kind 9 without a gateway: 0x69 0x275246a060d7058277dfc2567459ac1f31be069bfc650ba7b11ca09e855769a2,
//!   0x68 0x41955de7873739ffa416d4f7c2ea6524bb1fa199ae069999e5b13404363a2011
//! - 77312169 — feed kind 9 through the WETH gateway: 0x69 0x933699df7f07b8ee5e51038b2918bbf0dd5d3d10f840a4d6e4383d1af8604207,
//!   0x68 0x8a448fd9b19c63c41c5f3045b08b38745ed7bf80dd5deb3a34a9218424c05d4a
//! - 77285521 — feed kind 9 with callvalue 0 (selector 0x97a97cd4, 44 of 49 kind 9 in 014):
//!   0x69 0xa47ab092a015af8ea8ff3dbe55d16326450c5f19105ee3dc2f3372e15300aa0b,
//!   0x68 0x69d849ea582671b00a08efb572693f4a555c5755588dc560344f39b6b485e7cd
//!
//! Tests marked "synthetic" mutate a real fixture (status, registry) to reach code paths that
//! were not observed on chain.

use alloy_primitives::{address, b256, Address, U256};
use decoders::l1_inflows::*;
use serde_json::Value;

const FIXTURE: &str = include_str!("fixtures/l1-inflows-blocks.jsonl");

// L2 WETH gateway and L2 WETH are `verified` (references/contracts.md, Mihail, 2026-10-01) and
// come from the library. The two below are Ethereum (L1) addresses used only as expected values:
// L1 WETH = DepositFinalized.l1Token / l2Weth.l1Address() (task 016); the L1 WETH gateway is
// `observed` (research only, not used by the decoder).
const L1_WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
const L1_L2_WETH_GATEWAY: Address = address!("f7e12b9614b509c747ab4423bc4acf923759cf1b");

fn raw(n: u64) -> Value {
    FIXTURE
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|v| v["number"].as_u64() == Some(n))
        .unwrap()
}

fn decode_value(v: &Value, reg: &GatewayRegistry) -> anyhow::Result<BlockL1> {
    decode_block(&parse_line(&v.to_string())?, reg)
}

fn decode(n: u64, reg: &GatewayRegistry) -> BlockL1 {
    decode_value(&raw(n), reg).unwrap()
}

fn weth_registry(l2_token: Address, status: RegistryStatus) -> GatewayRegistry {
    GatewayRegistry::new(vec![GatewayEntry { gateway: L2_WETH_GATEWAY, l2_token: Some(l2_token), status }])
}

fn unaccounted(r: &BlockL1, k: UnaccountedKind) -> Vec<&UnaccountedFlow> {
    r.unaccounted.iter().filter(|u| u.kind == k).collect()
}

#[test]
fn kind12_eth_deposit() {
    let r = decode(77285531, &GatewayRegistry::default());
    assert_eq!(r.inflows.len(), 1);
    let row = &r.inflows[0];
    assert_eq!(row.kind, InflowKind::EthDeposit);
    assert_eq!(row.tx_type, 0x64);
    assert_eq!(row.tx_index, 1);
    assert_eq!(row.tx_hash, b256!("3f8e47b000a564834eb5c32a200b620ea02daa8e32ad48b1e29e1277dcb522c1"));
    assert_eq!(row.to, address!("7b5ba3c268bb600e48bb5aaed5f9f701adc2c066"));
    assert_eq!(row.amount, U256::from(99_911_045_644_425_076u64)); // 0.099911 ETH, 014 table
    assert_eq!(row.l2_alias, address!("8c6ca3c268bb600e48bb5aaed5f9f701adc2d177"));
    // EOA deposit: the L1 sender is the recipient itself.
    assert_eq!(row.l1_sender, row.to);
    assert_eq!(row.l1_request_id, Some(U256::from(340_191u64))); // feed header.requestId, task 014
    assert!(r.unaccounted.is_empty());

    let e = row.funding_edge();
    assert_eq!((e.kind, e.from_addr, e.to_addr, e.value_wei), ("l1_eth", row.to, row.to, row.amount));
    assert_eq!(e.gateway_status, "none");
    assert_eq!(r.counters.deposit_rows.n, 1);
    assert_eq!(r.counters.tx_types.get(&0x6a), Some(&1));
}

#[test]
fn kind9_retry_without_gateway() {
    let r = decode(77300695, &GatewayRegistry::default());
    // Only the 0x68 makes a row; the 0x69 (depositValue) must not.
    assert_eq!(r.inflows.len(), 1);
    let row = &r.inflows[0];
    assert_eq!(row.kind, InflowKind::RetryEth);
    assert_eq!(row.tx_type, 0x68);
    assert_eq!(row.tx_index, 2);
    assert_eq!(row.to, address!("514aa066504a926f754e5a2f2c012e8efc5234fa")); // retryTo
    assert_eq!(row.amount, U256::from(6_661_257_477_643_089u64)); // = 0x69.retryValue
    assert_eq!(row.l1_sender, address!("514aa066504a926f754e5a2f2c012e8efc5234fa"));
    assert_eq!(row.ticket_id, Some(b256!("275246a060d7058277dfc2567459ac1f31be069bfc650ba7b11ca09e855769a2")));
    assert!(row.token.is_none());

    // Refunds to FeeRefundAddr 0xf310…c00c. 0x69 stage = depositValue - retryValue - maxRefund,
    // equal to (maxSubmissionFee - submissionFee) + (gasFeeCap - baseFee) * gas recomputed by
    // hand from the same block (task 017, 2026-10-01).
    let refund_to = address!("f310d8ee808d2f3c2c0949ce5e5473b34ebcc00c");
    let s = unaccounted(&r, UnaccountedKind::SubmitFeeRefund);
    assert_eq!(s.len(), 1);
    assert_eq!((s[0].addr, s[0].amount_wei), (Some(refund_to), U256::from(5_174_074_212_200u64)));
    let rr = unaccounted(&r, UnaccountedKind::RedeemRefundUpperBound);
    assert_eq!((rr[0].addr, rr[0].amount_wei), (Some(refund_to), U256::from(5_163_395_442_600u64)));
    assert_eq!(r.counters.refund_identity_anomaly, 0);
    assert_eq!(r.counters.submit_ok.n, 1);
}

#[test]
fn kind9_zero_callvalue_has_no_row_but_counts_fee_refund() {
    let r = decode(77285521, &GatewayRegistry::builtin());
    assert!(r.inflows.is_empty());
    assert_eq!(r.counters.retry_zero_value, 1);
    // The ETH that does reach an L2 address here is the 0x69 fee refund to FeeRefundAddr:
    // depositValue 0x1c8ea63dc6000 - retryValue 0 - maxRefund 0x944f4f32050.
    let refund_to = address!("1ad5cff2132065ae3f9bc2dc3af3d71926e68f63");
    let s = unaccounted(&r, UnaccountedKind::SubmitFeeRefund);
    assert_eq!((s[0].addr, s[0].amount_wei), (Some(refund_to), U256::from(492_192_227_999_664u64)));
    assert_eq!(s[0].l1_sender, address!("b3e3c2281c1b6ba4ded3437264aa7366d007c1e3"));
}

#[test]
fn kind9_weth_gateway_builtin_verified() {
    let r = decode(77312169, &GatewayRegistry::builtin());
    assert_eq!(r.inflows.len(), 1, "token row only, no ETH row for the gateway contract");
    let row = &r.inflows[0];
    assert_eq!(row.kind, InflowKind::BridgedToken);
    assert_eq!(row.tx_index, 2);
    assert_eq!(row.log_index, Some(4));
    let recipient = address!("07ae8551be970cb1cca11dd7a11f47ae82e70e67");
    assert_eq!(row.to, recipient);
    assert_eq!(row.amount, U256::from(104_126_314_999_636_103_765u128)); // 104.126 ETH
    assert_eq!(row.tx_value, row.amount);
    assert_eq!(row.l1_sender, L1_L2_WETH_GATEWAY); // unalias(0x08f22b96…e02c)
    let t = row.token.as_ref().unwrap();
    assert_eq!(t.gateway, L2_WETH_GATEWAY);
    assert_eq!(t.l1_token, L1_WETH);
    assert_eq!(t.l2_token, Some(L2_WETH));
    assert_eq!(t.l1_from, recipient);
    assert_eq!(t.registry, Some(RegistryStatus::Verified));
    assert_eq!(t.l2_token_matches_registry, Some(true));

    let e = row.funding_edge();
    assert_eq!(e.kind, "l1_token");
    assert_eq!(e.from_addr, recipient); // L1 depositor, not the L1 gateway
    assert_eq!(e.token, Some(L2_WETH));
    assert_eq!(e.gateway_status, "verified");

    assert!(unaccounted(&r, UnaccountedKind::GatewayEthUnexplained).is_empty());
    let s = unaccounted(&r, UnaccountedKind::SubmitFeeRefund);
    assert_eq!((s[0].addr, s[0].amount_wei), (Some(recipient), U256::from(12_996_694_115_787_776u64)));
    assert_eq!(r.counters.token_rows_registered.n, 1);
    assert_eq!(r.counters.token_l2_missing + r.counters.token_registry_mismatch, 0);
}

#[test]
fn kind9_weth_gateway_unregistered_is_flagged() {
    let r = decode(77312169, &GatewayRegistry::default());
    assert_eq!(r.inflows.len(), 1);
    let t = r.inflows[0].token.as_ref().unwrap();
    assert_eq!(t.registry, None);
    assert_eq!(t.l2_token_matches_registry, None);
    assert_eq!(r.inflows[0].funding_edge().gateway_status, "none");
    assert_eq!(r.counters.token_rows_unregistered.n, 1);
    assert_eq!(r.counters.token_rows_registered.n, 0);
}

#[test]
fn synthetic_registry_observed_entry() {
    let r = decode(77312169, &weth_registry(L2_WETH, RegistryStatus::Observed));
    assert_eq!(r.inflows[0].token.as_ref().unwrap().registry, Some(RegistryStatus::Observed));
    assert_eq!(r.inflows[0].funding_edge().gateway_status, "observed");
}

#[test]
fn synthetic_registry_token_mismatch() {
    let r = decode(
        77312169,
        &weth_registry(address!("00000000000000000000000000000000000000ee"), RegistryStatus::Verified),
    );
    assert_eq!(r.inflows[0].token.as_ref().unwrap().l2_token_matches_registry, Some(false));
    assert_eq!(r.counters.token_registry_mismatch, 1);
}

#[test]
fn no_double_count_over_fixtures() {
    let mut c = Counters::default();
    let mut rows = Vec::new();
    for n in [77285531, 77300695, 77312169, 77285521] {
        let r = decode(n, &GatewayRegistry::builtin());
        c.merge(&r.counters);
        rows.extend(r.inflows);
    }
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|r| r.tx_type != 0x69));
    assert_eq!(c.tx_types.get(&0x69), Some(&3));
    assert_eq!(c.retry_zero_value, 1);
    assert_eq!(c.deposit_rows.n + c.retry_eth_rows.n + c.token_rows_registered.n, 3);
    // Each (tx_hash) yields at most one row here.
    let mut hashes: Vec<_> = rows.iter().map(|r| r.tx_hash).collect();
    hashes.dedup();
    assert_eq!(hashes.len(), 3);
}

#[test]
fn synthetic_deposit_not_succeeded() {
    let mut v = raw(77285531);
    v["receipts"][1]["status"] = Value::from("0x0");
    let r = decode_value(&v, &GatewayRegistry::default()).unwrap();
    assert!(r.inflows.is_empty());
    let u = unaccounted(&r, UnaccountedKind::DepositNotSucceeded);
    assert_eq!(u.len(), 1);
    assert_eq!(u[0].amount_wei, U256::from(99_911_045_644_425_076u64));
}

#[test]
fn synthetic_retry_failed() {
    let mut v = raw(77300695);
    v["receipts"][2]["status"] = Value::from("0x0");
    let r = decode_value(&v, &GatewayRegistry::default()).unwrap();
    assert!(r.inflows.is_empty());
    let u = unaccounted(&r, UnaccountedKind::RetryFailed);
    assert_eq!(u[0].amount_wei, U256::from(6_661_257_477_643_089u64));
    // The 0x69 refund identity does not depend on the redeem outcome.
    assert_eq!(unaccounted(&r, UnaccountedKind::SubmitFeeRefund).len(), 1);
}

#[test]
fn synthetic_submit_without_redeem_in_block() {
    let mut v = raw(77300695);
    v["block"]["transactions"].as_array_mut().unwrap().pop();
    v["receipts"].as_array_mut().unwrap().pop();
    let r = decode_value(&v, &GatewayRegistry::default()).unwrap();
    assert!(r.inflows.is_empty());
    let u = unaccounted(&r, UnaccountedKind::SubmitNoRedeemUpperBound);
    // depositValue - retryValue = 0x17b3c7beeb4261 - 0x17aa60ddb65b51
    assert_eq!(u[0].amount_wei, U256::from(0x17b3c7beeb4261u64 - 0x17aa60ddb65b51u64));
}

#[test]
fn misaligned_receipts_are_an_error() {
    let mut v = raw(77300695);
    v["receipts"].as_array_mut().unwrap().swap(1, 2);
    assert!(decode_value(&v, &GatewayRegistry::default()).is_err());
    let mut v = raw(77300695);
    v["receipts"].as_array_mut().unwrap().pop();
    assert!(decode_value(&v, &GatewayRegistry::default()).is_err());
}
