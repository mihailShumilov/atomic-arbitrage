-- 002: funding_edges for inflows from L1 (task 017, decoder crates/decoders/src/l1_inflows.rs).
-- Applied after 001 by sql/apply.sh (task 023), on new and existing volumes. No data is loaded here.
-- Idempotent: MODIFY COLUMN to a superset enum, ADD COLUMN IF NOT EXISTS.
-- The funding_edges key and log_index type are superseded by 003_funding_edges_key.sql
-- (log_index becomes UInt32 with the tx-level marker 4294967295 instead of Nullable).
--
-- Row semantics for the new kinds (one row per tx; token rows: one per DepositFinalized log):
--   l1_eth   : 0x64 ArbitrumDepositTx (status 1)             -> to_addr = tx.to,  value_wei = tx.value
--              0x68 RetryTx (status 1, value > 0, no gateway) -> to_addr = retryTo, value_wei = tx.value
--              from_addr = unalias(tx.from), an L1 (Ethereum) address, NOT an L2 account.
--   l1_token : 0x68 into a token gateway, from DepositFinalized emitted by tx.to
--              -> to_addr = DepositFinalized.to, value_wei = amount (raw token units),
--                 from_addr = DepositFinalized.from (L1 depositor), token = L2 token from the
--                 matching Transfer, gateway_status = registry status of the gateway
--                 ('none' = gateway not in the registry: research only, filter it out by default).
-- 0x69 SubmitRetryable never produces a row (its depositValue is split into fees, refunds and
-- the escrowed callvalue that the 0x68 moves): counting it too would double count.
-- Not in this table (counted by the decoder as "unaccounted"): fee refunds to FeeRefundAddr /
-- refundTo, failed redeems (escrow), FilteredFundsRecipient redirects, cancel/expiry to
-- beneficiary. See references/data-model.md, "L1-сообщения фида".

ALTER TABLE hood.funding_edges
    MODIFY COLUMN kind Enum8('eth' = 1, 'weth' = 2, 'internal' = 3, 'l1_eth' = 4, 'l1_token' = 5);

ALTER TABLE hood.funding_edges
    ADD COLUMN IF NOT EXISTS tx_hash        String DEFAULT '',
    ADD COLUMN IF NOT EXISTS log_index      Nullable(UInt32),  -- l1_token: DepositFinalized log index (block-level)
    ADD COLUMN IF NOT EXISTS token          String DEFAULT '', -- l1_token: L2 token ('' if no matching Transfer)
    ADD COLUMN IF NOT EXISTS l1_token       String DEFAULT '', -- l1_token: DepositFinalized.l1Token (Ethereum address)
    ADD COLUMN IF NOT EXISTS gateway        String DEFAULT '', -- l1_token: L2 gateway (= 0x68 to)
    ADD COLUMN IF NOT EXISTS gateway_status Enum8('none' = 0, 'observed' = 1, 'verified' = 2) DEFAULT 'none',
    ADD COLUMN IF NOT EXISTS l2_alias       String DEFAULT '', -- tx.from of the 0x64/0x68 (aliased L1 sender)
    ADD COLUMN IF NOT EXISTS tx_type        UInt8 DEFAULT 0,   -- 100 = 0x64, 104 = 0x68; 0 for older kinds
    ADD COLUMN IF NOT EXISTS l1_request_id  String DEFAULT '', -- 0x64 requestId (decimal)
    ADD COLUMN IF NOT EXISTS ticket_id      String DEFAULT ''; -- 0x68 ticketId (= hash of the 0x69)
