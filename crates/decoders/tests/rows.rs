//! Golden TSV of `hood.funding_edges` rows and of "unaccounted" records on the 017 fixture
//! (`fixtures/l1-inflows-blocks.jsonl`, see tests/l1_inflows.rs for its source).
//!
//! The expected text is the scanner output of HEAD 449576f (before task 022) on the same fixture,
//! with one documented change: the empty `log_index` of tx-level edges is now 4294967295
//! (`FundingEdge::TX_LEVEL_LOG_INDEX`, migration 003 of task 023). Block 77312169 is the row the
//! data-auditor checked by hand in review 017 (`edges-rpc.tsv`).

use decoders::l1_inflows::{decode_block, GatewayRegistry, UnaccountedFlow};
use decoders::parse_block_line;
use decoders::rows::FundingEdge;

const FIXTURE: &str = include_str!("fixtures/l1-inflows-blocks.jsonl");

const EDGES: &str = "\
block_number\ttx_index\tfrom_addr\tto_addr\tvalue_wei\tkind\ttx_hash\tlog_index\ttoken\tl1_token\tgateway\tgateway_status\tl2_alias\ttx_type\tl1_request_id\tticket_id
77285531\t1\t0x7b5ba3c268bb600e48bb5aaed5f9f701adc2c066\t0x7b5ba3c268bb600e48bb5aaed5f9f701adc2c066\t99911045644425076\tl1_eth\t0x3f8e47b000a564834eb5c32a200b620ea02daa8e32ad48b1e29e1277dcb522c1\t4294967295\t\t\t\tnone\t0x8c6ca3c268bb600e48bb5aaed5f9f701adc2d177\t100\t340191\t
77300695\t2\t0x514aa066504a926f754e5a2f2c012e8efc5234fa\t0x514aa066504a926f754e5a2f2c012e8efc5234fa\t6661257477643089\tl1_eth\t0x41955de7873739ffa416d4f7c2ea6524bb1fa199ae069999e5b13404363a2011\t4294967295\t\t\t\tnone\t0x625ba066504a926f754e5a2f2c012e8efc52460b\t104\t\t0x275246a060d7058277dfc2567459ac1f31be069bfc650ba7b11ca09e855769a2
77312169\t2\t0x07ae8551be970cb1cca11dd7a11f47ae82e70e67\t0x07ae8551be970cb1cca11dd7a11f47ae82e70e67\t104126314999636103765\tl1_token\t0x8a448fd9b19c63c41c5f3045b08b38745ed7bf80dd5deb3a34a9218424c05d4a\t4\t0x0bd7d308f8e1639fab988df18a8011f41eacad73\t0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2\t0x1d187c3e2da52d72bc9c41e3aba0fdfa6a7bf055\tverified\t0x08f22b9614b509c747ab4423bc4acf923759e02c\t104\t\t0x933699df7f07b8ee5e51038b2918bbf0dd5d3d10f840a4d6e4383d1af8604207
";

const UNACCOUNTED: &str = "\
block_number\ttx_index\ttx_hash\tkind\taddr\tamount_wei\tl1_sender
77300695\t1\t0x275246a060d7058277dfc2567459ac1f31be069bfc650ba7b11ca09e855769a2\tsubmit_fee_refund\t0xf310d8ee808d2f3c2c0949ce5e5473b34ebcc00c\t5174074212200\t0x514aa066504a926f754e5a2f2c012e8efc5234fa
77300695\t2\t0x41955de7873739ffa416d4f7c2ea6524bb1fa199ae069999e5b13404363a2011\tredeem_refund_upper_bound\t0xf310d8ee808d2f3c2c0949ce5e5473b34ebcc00c\t5163395442600\t0x514aa066504a926f754e5a2f2c012e8efc5234fa
77312169\t1\t0x933699df7f07b8ee5e51038b2918bbf0dd5d3d10f840a4d6e4383d1af8604207\tsubmit_fee_refund\t0x07ae8551be970cb1cca11dd7a11f47ae82e70e67\t12996694115787776\t0xf7e12b9614b509c747ab4423bc4acf923759cf1b
77312169\t2\t0x8a448fd9b19c63c41c5f3045b08b38745ed7bf80dd5deb3a34a9218424c05d4a\tredeem_refund_upper_bound\t0x07ae8551be970cb1cca11dd7a11f47ae82e70e67\t3305884212224\t0xf7e12b9614b509c747ab4423bc4acf923759cf1b
77285521\t1\t0xa47ab092a015af8ea8ff3dbe55d16326450c5f19105ee3dc2f3372e15300aa0b\tsubmit_fee_refund\t0x1ad5cff2132065ae3f9bc2dc3af3d71926e68f63\t492192227999664\t0xb3e3c2281c1b6ba4ded3437264aa7366d007c1e3
77285521\t2\t0x69d849ea582671b00a08efb572693f4a555c5755588dc560344f39b6b485e7cd\tredeem_refund_upper_bound\t0x1ad5cff2132065ae3f9bc2dc3af3d71926e68f63\t10191772000336\t0xb3e3c2281c1b6ba4ded3437264aa7366d007c1e3
";

#[test]
fn golden_tsv_on_017_fixture() {
    let reg = GatewayRegistry::builtin();
    let (mut edges, mut unacc) = (Vec::new(), Vec::new());
    FundingEdge::write_tsv_header(&mut edges).unwrap();
    UnaccountedFlow::write_tsv_header(&mut unacc).unwrap();
    for line in FIXTURE.lines() {
        let r = decode_block(&parse_block_line(line).unwrap(), &reg).unwrap();
        for i in &r.inflows {
            i.funding_edge().write_tsv(&mut edges).unwrap();
        }
        for u in &r.unaccounted {
            u.write_tsv(&mut unacc).unwrap();
        }
    }
    assert_eq!(String::from_utf8(edges).unwrap(), EDGES);
    assert_eq!(String::from_utf8(unacc).unwrap(), UNACCOUNTED);
}
