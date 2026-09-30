-- ClickHouse schema, phase 1. Ordering key everywhere: (block_number, tx_index, log_index).
-- Timestamps have 1 s resolution (~10 blocks/s): NEVER order by time.
-- Raw values are kept as strings (uint256/int256), normalized floats alongside.

CREATE DATABASE IF NOT EXISTS hood;

CREATE TABLE IF NOT EXISTS hood.blocks (
    block_number   UInt64,
    block_hash     String,
    ts             DateTime,
    l1_block       UInt64,
    base_fee_wei   UInt64,
    tx_count       UInt16,
    feed_recv_ns   Nullable(UInt64)   -- from recorder, NULL for backfilled blocks
) ENGINE = ReplacingMergeTree ORDER BY block_number;

CREATE TABLE IF NOT EXISTS hood.txs (
    block_number   UInt64,
    tx_index       UInt32,
    tx_hash        String,
    from_addr      String,
    to_addr        Nullable(String),
    value_wei      String,
    selector       FixedString(10),   -- 0x + 4 bytes, '' for plain transfers
    input_len      UInt32,
    status         UInt8,             -- 1 ok, 0 reverted
    gas_used       UInt64,
    gas_used_l1    UInt64,            -- receipt.gasUsedForL1
    eff_gas_price  UInt64,
    fee_wei        UInt128            -- gas_used * eff_gas_price (includes L1 part on Arbitrum)
) ENGINE = ReplacingMergeTree ORDER BY (block_number, tx_index);

CREATE TABLE IF NOT EXISTS hood.logs (
    block_number   UInt64,
    tx_index       UInt32,
    log_index      UInt32,
    address        String,
    topic0         String,
    topic1         Nullable(String),
    topic2         Nullable(String),
    topic3         Nullable(String),
    data           String
) ENGINE = ReplacingMergeTree ORDER BY (block_number, log_index);

-- Normalized swaps: one row per swap, from the token's point of view.
CREATE TABLE IF NOT EXISTS hood.swaps (
    block_number      UInt64,
    tx_index          UInt32,
    log_index         UInt32,
    venue             Enum8('pons_v1' = 1, 'pons_v2_curve' = 2, 'uni_v3' = 3, 'uni_v4' = 4, 'pools_trade' = 5, 'other' = 9),
    pool              String,          -- v3 pool address or v4 pool id
    token             String,          -- the meme / traded token
    quote             String,          -- WETH, USDG, stock token...
    trader            String,          -- tx.from (EOA that signed)
    router            Nullable(String),-- tx.to
    side              Enum8('buy' = 1, 'sell' = 2),
    token_amount_raw  String,
    quote_amount_raw  String,
    quote_amount      Float64,         -- in quote units (e.g. ETH)
    price             Float64,         -- quote per token, execution price
    fee_quote         Float64          -- pool/launchpad fee paid, in quote units
) ENGINE = ReplacingMergeTree ORDER BY (token, block_number, tx_index, log_index);

CREATE TABLE IF NOT EXISTS hood.tokens (
    token          String,
    venue          LowCardinality(String),
    creator        String,
    created_block  UInt64,
    created_tx     String,
    pool           String,
    quote          String,
    fee_params     String              -- JSON: per-launch fee params read on-chain
) ENGINE = ReplacingMergeTree ORDER BY token;

CREATE TABLE IF NOT EXISTS hood.wallets (
    address              String,
    first_seen_block     UInt64,
    first_funder         Nullable(String),
    first_funding_block  Nullable(UInt64),
    first_funding_wei    Nullable(String)
) ENGINE = ReplacingMergeTree ORDER BY address;

-- Direct ETH/WETH funding edges (internal transfers need traces: see enricher docs).
CREATE TABLE IF NOT EXISTS hood.funding_edges (
    block_number  UInt64,
    tx_index      UInt32,
    from_addr     String,
    to_addr       String,
    value_wei     String,
    kind          Enum8('eth' = 1, 'weth' = 2, 'internal' = 3)
) ENGINE = ReplacingMergeTree ORDER BY (to_addr, block_number, tx_index);

CREATE TABLE IF NOT EXISTS hood.labels (
    address     String,
    label       LowCardinality(String),  -- retail, bot, creator, router, fleet:<id>, bait_suspect...
    source      LowCardinality(String),  -- heuristic name or 'manual'
    confidence  Float32,
    note        String,
    updated_at  DateTime DEFAULT now()
) ENGINE = ReplacingMergeTree(updated_at) ORDER BY (address, label);

CREATE TABLE IF NOT EXISTS hood.feed_gaps (
    from_seq     UInt64,
    to_seq       UInt64,
    detected_ns  UInt64,
    filled       UInt8 DEFAULT 0
) ENGINE = ReplacingMergeTree ORDER BY from_seq;
