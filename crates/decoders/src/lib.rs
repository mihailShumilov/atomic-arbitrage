//! Decoders: typed blocks from `data/blocks/blocks-*.jsonl.zst` -> rows for `hood.*`.
//!
//! Layout (one file per layer or table):
//! - [`model`] — `Block`/`Tx`/`Log`/`TxCtx`, [`parse_block_line`]; all hex parsing lives there;
//! - [`arbitrum`] — Nitro tx types and L1 address aliasing;
//! - [`addresses`] — ONLY `verified` addresses of references/contracts.md;
//! - [`events`] — event ABIs (`sol!`) and topic0 constants, [`ALL_TOPIC0`];
//! - [`rows`] — ClickHouse row types and their TSV ([`rows::FundingEdge`], [`rows::SwapRow`]);
//! - [`l1_inflows`] — inflows from L1 (`0x64`/`0x68`) -> `funding_edges`;
//! - [`swaps`] — Uniswap v3/v4 `Swap` -> [`swaps::PoolSwap`];
//! - [`registry`] — status of caller-supplied registry entries (`verified`/`observed`);
//! - [`pools`] — pool/token metadata for `hood.swaps` (caller input, also the emitter filter);
//! - [`swap_rows`] — [`swaps::PoolSwap`] + [`pools`] -> [`rows::SwapRow`] (`hood.swaps`).
//!
//! Launchpad-specific events (Pons v1, Pons v2 curve, pools.trade) are TODO: their ABIs must come
//! from official sources and be recorded in .claude/skills/hoodchain-mev/references/contracts.md
//! BEFORE any decoder is written.
//!
//! Every decoder must be checked by the data-auditor agent against real logs (Blockscout / RPC)
//! before its output is used in analytics.

pub mod addresses;
pub mod arbitrum;
pub mod events;
pub mod l1_inflows;
pub mod model;
pub mod pools;
pub mod registry;
pub mod rows;
pub mod swap_rows;
pub mod swaps;

pub use events::{
    erc20, token_bridge, v3, v4, ALL_TOPIC0, TOPIC_DEPOSIT_FINALIZED, TOPIC_INITIALIZE_V4, TOPIC_SWAP_V3,
    TOPIC_SWAP_V4, TOPIC_TRANSFER, TOPIC_WITHDRAWAL_INITIATED,
};
pub use model::{parse_block_line, Block, Log, Tx, TxCtx};
