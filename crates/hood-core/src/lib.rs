//! Shared types for the Robinhood Chain research pipeline.
//!
//! Verified facts (2026-09-28, see .claude/skills/hoodchain-mev/references/chain-facts.md):
//! - `sequenceNumber` in the feed == L2 block number (blockHash matched via RPC).
//! - `header.blockNumber` is the **L1** block number, not L2.
//! - `header.timestamp` has 1-second resolution (~10 L2 blocks per second),
//!   so ordering must always use (block_number, tx_index, log_index).

use serde::Deserialize;

pub const CHAIN_ID: u64 = 4663;
pub const FEED_URL: &str = "wss://feed.mainnet.chain.robinhood.com";
pub const PUBLIC_RPC_URL: &str = "https://rpc.mainnet.chain.robinhood.com";

/// Minimal view of a broadcast-feed envelope. Unknown fields are ignored on
/// purpose: the raw text is always stored verbatim, this is only for routing.
#[derive(Debug, Deserialize)]
pub struct FeedEnvelope {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub messages: Vec<FeedMessageHead>,
}

#[derive(Debug, Deserialize)]
pub struct FeedMessageHead {
    /// Equal to the L2 block number on Robinhood Chain.
    #[serde(rename = "sequenceNumber")]
    pub sequence_number: u64,
    #[serde(rename = "blockHash", default)]
    pub block_hash: Option<String>,
}

impl FeedEnvelope {
    /// First and last sequence numbers carried by this envelope, if any.
    pub fn seq_range(&self) -> Option<(u64, u64)> {
        let first = self.messages.first()?.sequence_number;
        let last = self.messages.last()?.sequence_number;
        Some((first, last))
    }
}

/// A contiguous range of L2 blocks missing from the recorded feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap {
    pub from: u64,
    pub to: u64,
}

/// Given the last recorded sequence number and the first one of a new
/// envelope, returns the gap between them (if any).
pub fn detect_gap(last_seen: Option<u64>, first_new: u64) -> Option<Gap> {
    match last_seen {
        Some(last) if first_new > last + 1 => Some(Gap { from: last + 1, to: first_new - 1 }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_envelope_shape() {
        // Shape captured from the live feed on 2026-09-28 (l2Msg shortened).
        let raw = r#"{"version":1,"messages":[{"sequenceNumber":74755960,"message":{"message":{"header":{"kind":3,"sender":"0xa4b000000000000000000073657175656e636572","blockNumber":26075606,"timestamp":1790594344,"requestId":null,"baseFeeL1":0},"l2Msg":"AAAA"},"delayedMessagesRead":328658},"blockHash":"0x529d8dcb881a6f5ed00232db376cf449ad881aa2a7ac4f8eb0078ea40a17f0f4","signatureV2":"AA==","blockMetadata":null}]}"#;
        let env: FeedEnvelope = serde_json::from_str(raw).unwrap();
        assert_eq!(env.seq_range(), Some((74755960, 74755960)));
        assert!(env.messages[0].block_hash.as_deref().unwrap().starts_with("0x529d"));
    }

    #[test]
    fn gap_detection() {
        assert_eq!(detect_gap(None, 10), None);
        assert_eq!(detect_gap(Some(9), 10), None);
        assert_eq!(detect_gap(Some(10), 10), None); // duplicate, not a gap
        assert_eq!(detect_gap(Some(5), 10), Some(Gap { from: 6, to: 9 }));
    }
}
