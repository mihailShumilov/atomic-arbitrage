//! Shared types for the Robinhood Chain research pipeline.
//!
//! Verified facts (2026-09-28, see .claude/skills/hoodchain-mev/references/chain-facts.md):
//! - `sequenceNumber` in the feed == L2 block number (blockHash matched via RPC).
//! - `header.blockNumber` is the **L1** block number, not L2.
//! - `header.timestamp` has 1-second resolution (~10 L2 blocks per second),
//!   so ordering must always use (block_number, tx_index, log_index).
//!
//! Modules (pure logic and thin std helpers shared by recorder, enricher and
//! loader): [`ranges`] (block ranges, `gaps.tsv` / `filled.tsv`), [`feedline`]
//! (one line of the raw feed on disk), [`http`]
//! (`Retry-After`), [`fsutil`] (atomic write, fsync, append), [`hex`]
//! (JSON-RPC quantities), [`jitter`] (random spread of retry pauses), [`redact`]
//! (endpoint URLs without provider keys in logs and errors).

pub mod feedline;
pub mod fsutil;
pub mod hex;
pub mod http;
pub mod jitter;
pub mod ranges;
pub mod redact;

use serde::Deserialize;

pub use ranges::{detect_gap, Gap, Range};

/// Robinhood Chain mainnet chain id (`references/chain-facts.md`). The
/// enricher's `eth_chainId` check at start-up (task 020) compares against it.
pub const CHAIN_ID: u64 = 4663;
/// Public sequencer broadcast feed (WebSocket).
pub const FEED_URL: &str = "wss://feed.mainnet.chain.robinhood.com";
/// Public JSON-RPC endpoint; rate limited, not for production runs.
pub const PUBLIC_RPC_URL: &str = "https://rpc.mainnet.chain.robinhood.com";

/// Minimal view of a broadcast-feed envelope. Unknown fields are ignored on
/// purpose: the raw text is always stored verbatim, this is only for routing.
#[derive(Debug, Deserialize)]
pub struct FeedEnvelope {
    /// Feed protocol version (1 so far).
    #[serde(default)]
    pub version: u32,
    /// One message per L2 block; empty for e.g. `confirmedSequenceNumberMessage`.
    #[serde(default)]
    pub messages: Vec<FeedMessageHead>,
}

/// The part of one feed message the recorder routes on.
#[derive(Debug, Deserialize)]
pub struct FeedMessageHead {
    /// Equal to the L2 block number on Robinhood Chain.
    #[serde(rename = "sequenceNumber")]
    pub sequence_number: u64,
    /// L2 block hash, when the feed sends it.
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
}
