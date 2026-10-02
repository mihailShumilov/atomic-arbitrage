-- 003: funding_edges key that does not collapse distinct edges (task 023; review 2026-10-02, В1).
--
-- The old key ORDER BY (to_addr, block_number, tx_index) is also the ReplacingMergeTree dedup key,
-- so edges of different kinds in one tx (e.g. l1_token + weth Transfer) or two token logs to one
-- recipient in one tx collapsed into one row. New key adds kind and log_index.
--
-- log_index is no longer Nullable (a Nullable column cannot be in the sorting key without
-- allow_nullable_key): tx-level edges (0x64 / 0x68 ETH, plain tx.value) use the marker
-- 4294967295 = u32::MAX. Not 0: 0 is a valid index of the first log of a block.
-- BREAKS COMPATIBILITY with the 002 column type (Nullable(UInt32) -> UInt32).
-- Columns, types, defaults and column order are otherwise those of 001 + 002 (= FundingEdge::COLUMNS
-- in crates/decoders/src/rows.rs, checked by a unit test there); Enum8 values are unchanged.
-- Internal (trace) edges will need their own discriminator (e.g. trace_index) when traces arrive.
--
-- The sorting key cannot be changed by ALTER, so the table is recreated: new table, two plain
-- RENAMEs, DROP of the old one. The migration REFUSES to run if the old table holds any row or
-- detached part (throwIf below): data in funding_edges must be migrated explicitly by a dedicated
-- migration, never dropped. Checked 2026-10-02: the local table is empty.
--
-- Not EXCHANGE TABLES: on a Docker Desktop (macOS) bind mount (fs "fakeowner") it lost the
-- metadata file funding_edges.sql (the table vanished on restart). Reproduced 2026-10-02 on
-- 26.9.6.6; plain RENAME works there and survives a restart.
-- If a run is interrupted between the two RENAMEs (funding_edges missing, funding_edges_003 and
-- funding_edges_pre003 present), finish by hand: RENAME TABLE hood.funding_edges_003 TO hood.funding_edges
--
-- Idempotency: sql/apply.sh skips this file when the table already has the new key and no
-- leftover of a half-done run exists (apply-unless guard below), so a re-run never touches a
-- migrated (possibly populated) table. A leftover funding_edges_003 / funding_edges_pre003 (e.g.
-- the old table kept with rows by the last throwIf) makes the guard 0: the file runs again and
-- fails loudly (throwIf or RENAME onto an existing name) instead of being skipped silently.
-- apply-unless: SELECT count() = 1 AND (SELECT count() FROM system.tables WHERE database = 'hood' AND name IN ('funding_edges_pre003', 'funding_edges_003')) = 0 FROM system.tables WHERE database = 'hood' AND name = 'funding_edges' AND sorting_key = 'to_addr, block_number, tx_index, kind, log_index'

SELECT throwIf(
    (SELECT count() FROM hood.funding_edges) > 0
    OR (SELECT count() FROM system.detached_parts WHERE database = 'hood' AND table = 'funding_edges') > 0,
    '003: hood.funding_edges is not empty (rows or detached parts): refusing to recreate it'
);

CREATE TABLE IF NOT EXISTS hood.funding_edges_003 (
    block_number   UInt64,
    tx_index       UInt32,
    from_addr      String,
    to_addr        String,
    value_wei      String,                      -- uint256 as decimal string (raw units)
    kind           Enum8('eth' = 1, 'weth' = 2, 'internal' = 3, 'l1_eth' = 4, 'l1_token' = 5),
    tx_hash        String DEFAULT '',
    log_index      UInt32 DEFAULT 4294967295,   -- block-level log index; 4294967295 = tx-level edge
    token          String DEFAULT '',           -- l1_token: L2 token ('' if no matching Transfer)
    l1_token       String DEFAULT '',           -- l1_token: DepositFinalized.l1Token (Ethereum address)
    gateway        String DEFAULT '',           -- l1_token: L2 gateway (= 0x68 to)
    gateway_status Enum8('none' = 0, 'observed' = 1, 'verified' = 2) DEFAULT 'none',
    l2_alias       String DEFAULT '',           -- tx.from of the 0x64/0x68 (aliased L1 sender)
    tx_type        UInt8 DEFAULT 0,             -- 100 = 0x64, 104 = 0x68; 0 for older kinds
    l1_request_id  String DEFAULT '',           -- 0x64 requestId (decimal)
    ticket_id      String DEFAULT ''            -- 0x68 ticketId (= hash of the 0x69)
) ENGINE = ReplacingMergeTree
ORDER BY (to_addr, block_number, tx_index, kind, log_index);

-- Leftover of an interrupted earlier run must be empty as well.
SELECT throwIf(
    (SELECT count() FROM hood.funding_edges_003) > 0,
    '003: hood.funding_edges_003 is not empty: refusing to continue'
);

RENAME TABLE hood.funding_edges TO hood.funding_edges_pre003;

RENAME TABLE hood.funding_edges_003 TO hood.funding_edges;

-- Re-check the old table before DROP: rows inserted between the first check and the RENAME must
-- not be lost (the DROP is not reached, the error is loud).
SELECT throwIf(
    (SELECT count() FROM hood.funding_edges_pre003) > 0
    OR (SELECT count() FROM system.detached_parts WHERE database = 'hood' AND table = 'funding_edges_pre003') > 0,
    '003: old funding_edges received rows during the migration: kept as hood.funding_edges_pre003, not dropped'
);

DROP TABLE hood.funding_edges_pre003 SYNC;
