-- 005: venue 'pons_v2_hook' = 6 in hood.swaps and hood.tokens (task 036; Mihail's decision 2026-10-05).
--
-- Uniswap v4 pools whose PoolKey.hooks is the Pons v2 meme hook (`verified` in
-- references/contracts.md) get their own venue instead of 'other'. Their rows carry only the AMM
-- leg of the trade: the v4 Swap event is emitted before afterSwap (data-model.md, "hood.swaps").
--
-- Only a new Enum8 value is added; existing names and values are unchanged (1..5 and 9), so the
-- stored data is untouched: extending an Enum8 is a metadata-only ALTER (no mutation, no rewrite).
-- Neither column is in a sorting key. The new type is written in value order (= Venue::ALL in
-- crates/decoders/src/rows.rs; the test swaps_enum8_in_sql_equals_rust_enums reads it from here).
-- No EXCHANGE TABLES / CREATE OR REPLACE / REPLACE TABLE (data-model.md, "Схема и миграции").
--
-- Idempotency: MODIFY COLUMN to the type the column already has is a no-op, so the whole file can
-- be re-run; apply.sh also skips it (outcome 'skipped') when both columns already have the target
-- type. The first check refuses to run if either column has a type that is neither the 004 type
-- nor the target (someone changed it by hand: look before overwriting).
-- apply-unless: SELECT count() = 2 FROM system.columns WHERE database = 'hood' AND table IN ('swaps', 'tokens') AND name = 'venue' AND type = 'Enum8(''pons_v1'' = 1, ''pons_v2_curve'' = 2, ''uni_v3'' = 3, ''uni_v4'' = 4, ''pools_trade'' = 5, ''pons_v2_hook'' = 6, ''other'' = 9)'

SELECT throwIf(
    (SELECT count() FROM system.columns
     WHERE database = 'hood' AND table IN ('swaps', 'tokens') AND name = 'venue'
       AND type IN (
           'Enum8(''pons_v1'' = 1, ''pons_v2_curve'' = 2, ''uni_v3'' = 3, ''uni_v4'' = 4, ''pools_trade'' = 5, ''other'' = 9)',
           'Enum8(''pons_v1'' = 1, ''pons_v2_curve'' = 2, ''uni_v3'' = 3, ''uni_v4'' = 4, ''pools_trade'' = 5, ''pons_v2_hook'' = 6, ''other'' = 9)'
       )) != 2,
    '005: swaps.venue / tokens.venue missing or of an unexpected type (neither 004 nor 005): refusing to modify'
);

ALTER TABLE hood.swaps MODIFY COLUMN venue Enum8('pons_v1' = 1, 'pons_v2_curve' = 2, 'uni_v3' = 3, 'uni_v4' = 4, 'pools_trade' = 5, 'pons_v2_hook' = 6, 'other' = 9);

ALTER TABLE hood.tokens MODIFY COLUMN venue Enum8('pons_v1' = 1, 'pons_v2_curve' = 2, 'uni_v3' = 3, 'uni_v4' = 4, 'pools_trade' = 5, 'pons_v2_hook' = 6, 'other' = 9);

SELECT throwIf(
    (SELECT count() FROM system.columns
     WHERE database = 'hood' AND table IN ('swaps', 'tokens') AND name = 'venue'
       AND type = 'Enum8(''pons_v1'' = 1, ''pons_v2_curve'' = 2, ''uni_v3'' = 3, ''uni_v4'' = 4, ''pools_trade'' = 5, ''pons_v2_hook'' = 6, ''other'' = 9)') != 2,
    '005: after MODIFY COLUMN swaps.venue / tokens.venue do not have the 005 type'
);
