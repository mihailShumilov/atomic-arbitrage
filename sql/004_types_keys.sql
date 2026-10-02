-- 004: column types, labels key, feed_gaps version (task 029; review full-2026-10-02 Р7-Р9).
--
-- Changes (column names and order are those of 001; every other column is unchanged):
--   txs.to_addr          Nullable(String)  -> String DEFAULT ''      ('' = contract creation)
--   txs.selector         FixedString(10)   -> LowCardinality(String) DEFAULT ''
--                        ('' = input shorter than 4 bytes; FixedString stored '' as 10 zero bytes)
--   logs.topic1..topic3  Nullable(String)  -> String DEFAULT ''      ('' = topic absent)
--   swaps.router         Nullable(String)  -> String DEFAULT ''
--   tokens.venue         LowCardinality(String) -> the Enum8 of swaps.venue (one concept, one type)
--   wallets.first_funder / first_funding_wei  Nullable(String) -> String DEFAULT '' ('' = not found)
--   wallets.first_funding_block  Nullable(UInt64) -> UInt64 DEFAULT 0  (0 = not found)
--   labels     ORDER BY (address, label) -> (address, label, source): two heuristics giving the
--              same label to one address are two rows, not collapsed into the latest one.
--   feed_gaps  ReplacingMergeTree -> ReplacingMergeTree(filled): filled 1 wins over 0 on merge,
--              whatever the insert order (a later re-load of gaps.tsv with filled 0 cannot undo it).
-- Kept on purpose:
--   blocks.feed_recv_ns stays Nullable(UInt64): NULL is semantic (block not seen by the recorder,
--     backfilled from RPC); data-model.md tells every latency query to filter IS NOT NULL.
--   uint256 amounts stay decimal String (data-model convention; switching to UInt256 is Michael's call).
--   funding_edges is not touched (its key is 003; FundingEdge::COLUMNS test in crates/decoders).
--
-- Every table is recreated the way 003 does it: refuse if the table has rows or detached parts,
-- CREATE <t>_004, refuse if a leftover <t>_004 holds rows, RENAME <t> -> <t>_pre004,
-- RENAME <t>_004 -> <t>, re-check <t>_pre004, DROP it SYNC.
-- Never EXCHANGE TABLES, CREATE OR REPLACE TABLE or REPLACE TABLE: on the Docker Desktop bind mount
-- (fs fakeowner) they lose or orphan metadata/data (data-model.md, "Схема и миграции").
-- Checked 2026-10-03: all hood.* tables except schema_migrations hold 0 rows on the local volume.
--
-- Idempotency: sql/apply.sh skips the file (outcome 'skipped') when all seven tables are already
-- in the target state (one marker per table, below) and no *_004 / *_pre004 leftover exists.
-- A partial run (died after some tables) re-runs the whole file: already recreated tables are
-- empty, pass the throwIf and are recreated once more. A populated table makes it fail loudly.
-- After a failed RENAME an empty hood.<t>_004 stays behind (CREATE ran before it); the guard stays 0
-- and the next run reuses it (the same holds for funding_edges_003 of migration 003).
-- If a run dies between the two RENAMEs (<t> missing, <t>_004 and <t>_pre004 present), finish by
-- hand: RENAME TABLE hood.<t>_004 TO hood.<t>; then check that hood.<t>_pre004 is empty and DROP it.
-- apply-unless: SELECT (SELECT count() FROM system.columns WHERE database = 'hood' AND ((table = 'txs' AND name = 'to_addr' AND type = 'String') OR (table = 'logs' AND name = 'topic1' AND type = 'String') OR (table = 'swaps' AND name = 'router' AND type = 'String') OR (table = 'tokens' AND name = 'venue' AND startsWith(type, 'Enum8(')) OR (table = 'wallets' AND name = 'first_funder' AND type = 'String'))) = 5 AND (SELECT count() FROM system.tables WHERE database = 'hood' AND ((name = 'labels' AND sorting_key = 'address, label, source') OR (name = 'feed_gaps' AND startsWith(engine_full, 'ReplacingMergeTree(filled)')))) = 2 AND (SELECT count() FROM system.tables WHERE database = 'hood' AND match(name, '_(pre)?004$')) = 0

-- ---------------------------------------------------------------- txs
SELECT throwIf(
    (SELECT count() FROM hood.txs) > 0
    OR (SELECT count() FROM system.detached_parts WHERE database = 'hood' AND table = 'txs') > 0,
    '004: hood.txs is not empty (rows or detached parts): refusing to recreate it'
);

CREATE TABLE IF NOT EXISTS hood.txs_004 (
    block_number   UInt64,
    tx_index       UInt32,
    tx_hash        String,
    from_addr      String,
    to_addr        String DEFAULT '',                  -- '' = contract creation
    value_wei      String,                             -- uint256 as decimal string
    selector       LowCardinality(String) DEFAULT '',  -- 0x + 4 bytes, '' if input < 4 bytes
    input_len      UInt32,
    status         UInt8,                              -- 1 ok, 0 reverted
    gas_used       UInt64,
    gas_used_l1    UInt64,                             -- receipt.gasUsedForL1
    eff_gas_price  UInt64,
    fee_wei        UInt128                             -- gas_used * eff_gas_price (includes L1 part)
) ENGINE = ReplacingMergeTree ORDER BY (block_number, tx_index);

SELECT throwIf((SELECT count() FROM hood.txs_004) > 0, '004: hood.txs_004 is not empty: refusing to continue');

RENAME TABLE hood.txs TO hood.txs_pre004;

RENAME TABLE hood.txs_004 TO hood.txs;

SELECT throwIf(
    (SELECT count() FROM hood.txs_pre004) > 0
    OR (SELECT count() FROM system.detached_parts WHERE database = 'hood' AND table = 'txs_pre004') > 0,
    '004: old txs received rows during the migration: kept as hood.txs_pre004, not dropped'
);

DROP TABLE hood.txs_pre004 SYNC;

-- ---------------------------------------------------------------- logs
SELECT throwIf(
    (SELECT count() FROM hood.logs) > 0
    OR (SELECT count() FROM system.detached_parts WHERE database = 'hood' AND table = 'logs') > 0,
    '004: hood.logs is not empty (rows or detached parts): refusing to recreate it'
);

CREATE TABLE IF NOT EXISTS hood.logs_004 (
    block_number   UInt64,
    tx_index       UInt32,
    log_index      UInt32,                             -- block-level log index
    address        String,
    topic0         String,
    topic1         String DEFAULT '',                  -- '' = topic absent
    topic2         String DEFAULT '',
    topic3         String DEFAULT '',
    data           String
) ENGINE = ReplacingMergeTree ORDER BY (block_number, log_index);

SELECT throwIf((SELECT count() FROM hood.logs_004) > 0, '004: hood.logs_004 is not empty: refusing to continue');

RENAME TABLE hood.logs TO hood.logs_pre004;

RENAME TABLE hood.logs_004 TO hood.logs;

SELECT throwIf(
    (SELECT count() FROM hood.logs_pre004) > 0
    OR (SELECT count() FROM system.detached_parts WHERE database = 'hood' AND table = 'logs_pre004') > 0,
    '004: old logs received rows during the migration: kept as hood.logs_pre004, not dropped'
);

DROP TABLE hood.logs_pre004 SYNC;

-- ---------------------------------------------------------------- swaps
SELECT throwIf(
    (SELECT count() FROM hood.swaps) > 0
    OR (SELECT count() FROM system.detached_parts WHERE database = 'hood' AND table = 'swaps') > 0,
    '004: hood.swaps is not empty (rows or detached parts): refusing to recreate it'
);

CREATE TABLE IF NOT EXISTS hood.swaps_004 (
    block_number      UInt64,
    tx_index          UInt32,
    log_index         UInt32,
    venue             Enum8('pons_v1' = 1, 'pons_v2_curve' = 2, 'uni_v3' = 3, 'uni_v4' = 4, 'pools_trade' = 5, 'other' = 9),
    pool              String,                          -- v3 pool address or v4 pool id
    token             String,                          -- the meme / traded token
    quote             String,                          -- WETH, USDG, stock token...
    trader            String,                          -- tx.from (EOA that signed)
    router            String DEFAULT '',               -- tx.to ('' = contract creation)
    side              Enum8('buy' = 1, 'sell' = 2),
    token_amount_raw  String,
    quote_amount_raw  String,
    quote_amount      Float64,                         -- in quote units (e.g. ETH)
    price             Float64,                         -- quote per token, execution price
    fee_quote         Float64                          -- pool/launchpad fee paid, in quote units
) ENGINE = ReplacingMergeTree ORDER BY (token, block_number, tx_index, log_index);

SELECT throwIf((SELECT count() FROM hood.swaps_004) > 0, '004: hood.swaps_004 is not empty: refusing to continue');

RENAME TABLE hood.swaps TO hood.swaps_pre004;

RENAME TABLE hood.swaps_004 TO hood.swaps;

SELECT throwIf(
    (SELECT count() FROM hood.swaps_pre004) > 0
    OR (SELECT count() FROM system.detached_parts WHERE database = 'hood' AND table = 'swaps_pre004') > 0,
    '004: old swaps received rows during the migration: kept as hood.swaps_pre004, not dropped'
);

DROP TABLE hood.swaps_pre004 SYNC;

-- ---------------------------------------------------------------- tokens
SELECT throwIf(
    (SELECT count() FROM hood.tokens) > 0
    OR (SELECT count() FROM system.detached_parts WHERE database = 'hood' AND table = 'tokens') > 0,
    '004: hood.tokens is not empty (rows or detached parts): refusing to recreate it'
);

CREATE TABLE IF NOT EXISTS hood.tokens_004 (
    token          String,
    venue          Enum8('pons_v1' = 1, 'pons_v2_curve' = 2, 'uni_v3' = 3, 'uni_v4' = 4, 'pools_trade' = 5, 'other' = 9),
    creator        String,
    created_block  UInt64,
    created_tx     String,
    pool           String,
    quote          String,
    fee_params     String              -- JSON: per-launch fee params read on-chain
) ENGINE = ReplacingMergeTree ORDER BY token;

SELECT throwIf((SELECT count() FROM hood.tokens_004) > 0, '004: hood.tokens_004 is not empty: refusing to continue');

RENAME TABLE hood.tokens TO hood.tokens_pre004;

RENAME TABLE hood.tokens_004 TO hood.tokens;

SELECT throwIf(
    (SELECT count() FROM hood.tokens_pre004) > 0
    OR (SELECT count() FROM system.detached_parts WHERE database = 'hood' AND table = 'tokens_pre004') > 0,
    '004: old tokens received rows during the migration: kept as hood.tokens_pre004, not dropped'
);

DROP TABLE hood.tokens_pre004 SYNC;

-- ---------------------------------------------------------------- wallets
SELECT throwIf(
    (SELECT count() FROM hood.wallets) > 0
    OR (SELECT count() FROM system.detached_parts WHERE database = 'hood' AND table = 'wallets') > 0,
    '004: hood.wallets is not empty (rows or detached parts): refusing to recreate it'
);

CREATE TABLE IF NOT EXISTS hood.wallets_004 (
    address              String,
    first_seen_block     UInt64,
    first_funder         String DEFAULT '',     -- '' = no funding edge found
    first_funding_block  UInt64 DEFAULT 0,      -- 0 = no funding edge found
    first_funding_wei    String DEFAULT ''      -- uint256 as decimal string, '' = not found
) ENGINE = ReplacingMergeTree ORDER BY address;

SELECT throwIf((SELECT count() FROM hood.wallets_004) > 0, '004: hood.wallets_004 is not empty: refusing to continue');

RENAME TABLE hood.wallets TO hood.wallets_pre004;

RENAME TABLE hood.wallets_004 TO hood.wallets;

SELECT throwIf(
    (SELECT count() FROM hood.wallets_pre004) > 0
    OR (SELECT count() FROM system.detached_parts WHERE database = 'hood' AND table = 'wallets_pre004') > 0,
    '004: old wallets received rows during the migration: kept as hood.wallets_pre004, not dropped'
);

DROP TABLE hood.wallets_pre004 SYNC;

-- ---------------------------------------------------------------- labels
SELECT throwIf(
    (SELECT count() FROM hood.labels) > 0
    OR (SELECT count() FROM system.detached_parts WHERE database = 'hood' AND table = 'labels') > 0,
    '004: hood.labels is not empty (rows or detached parts): refusing to recreate it'
);

CREATE TABLE IF NOT EXISTS hood.labels_004 (
    address     String,
    label       LowCardinality(String),  -- retail, bot, creator, router, fleet:<id>, bait_suspect...
    source      LowCardinality(String),  -- heuristic name or 'manual'
    confidence  Float32,
    note        String,
    updated_at  DateTime DEFAULT now()   -- version: the latest row per (address, label, source) wins
) ENGINE = ReplacingMergeTree(updated_at) ORDER BY (address, label, source);

SELECT throwIf((SELECT count() FROM hood.labels_004) > 0, '004: hood.labels_004 is not empty: refusing to continue');

RENAME TABLE hood.labels TO hood.labels_pre004;

RENAME TABLE hood.labels_004 TO hood.labels;

SELECT throwIf(
    (SELECT count() FROM hood.labels_pre004) > 0
    OR (SELECT count() FROM system.detached_parts WHERE database = 'hood' AND table = 'labels_pre004') > 0,
    '004: old labels received rows during the migration: kept as hood.labels_pre004, not dropped'
);

DROP TABLE hood.labels_pre004 SYNC;

-- ---------------------------------------------------------------- feed_gaps
SELECT throwIf(
    (SELECT count() FROM hood.feed_gaps) > 0
    OR (SELECT count() FROM system.detached_parts WHERE database = 'hood' AND table = 'feed_gaps') > 0,
    '004: hood.feed_gaps is not empty (rows or detached parts): refusing to recreate it'
);

CREATE TABLE IF NOT EXISTS hood.feed_gaps_004 (
    from_seq     UInt64,
    to_seq       UInt64,
    detected_ns  UInt64,
    filled       UInt8 DEFAULT 0       -- version: 1 (filled) wins over 0 on merge
) ENGINE = ReplacingMergeTree(filled) ORDER BY from_seq;

SELECT throwIf((SELECT count() FROM hood.feed_gaps_004) > 0, '004: hood.feed_gaps_004 is not empty: refusing to continue');

RENAME TABLE hood.feed_gaps TO hood.feed_gaps_pre004;

RENAME TABLE hood.feed_gaps_004 TO hood.feed_gaps;

SELECT throwIf(
    (SELECT count() FROM hood.feed_gaps_pre004) > 0
    OR (SELECT count() FROM system.detached_parts WHERE database = 'hood' AND table = 'feed_gaps_pre004') > 0,
    '004: old feed_gaps received rows during the migration: kept as hood.feed_gaps_pre004, not dropped'
);

DROP TABLE hood.feed_gaps_pre004 SYNC;
