# analytics/

Research scripts. Today: stdlib-only Python (3.9+) of task 010 plus a shared
helper module. polars + clickhouse-connect come with phase 1c; backtests are
written only after the data-auditor has signed off on decoders (see CLAUDE.md).

| File | What | Network |
|---|---|---|
| `hoodlib.py` | shared helpers: stream `*.jsonl.zst` (`iter_block_lines`, `iter_blocks`), tx/receipt pairing with count and hash checks (`tx_receipt_pairs`, `user_tx_pairs`: everything but `0x6a`), `fee_wei`, `gas_used_for_l1`, `median_int`, Swap topic0 from `contracts.md` | none |
| `hourly_sample.py` | one block per hour from the public RPC with a hard call budget and a call ledger | public RPC (budgeted) |
| `hourly_metrics.py` | per-block metrics TSV from a blocks-format `.jsonl.zst` | none |
| `hourly_summary.py` | markdown tables of the task 010 report | none |

Order: `hourly_sample.py` → `hourly_metrics.py --index … --out data/samples/hourly-metrics.tsv` →
`hourly_summary.py`. The scripts import `hoodlib` from their own directory, so run them
as `python3 analytics/<script>.py`.

`.claude/skills/feed-audit/scripts/feed_audit.py` deliberately does not use
`hoodlib`: it is installed on the server as a single file.

Every result file must follow .claude/skills/hoodchain-mev/references/backtest-rules.md
and be reviewed by the skeptic-analyst agent before it is shown to Michael.
