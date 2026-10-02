"""Shared helpers for the stdlib analytics scripts (task 024). No RPC, no third-party deps.

Blocks-format input (enricher's blocks-*.jsonl.zst and the task 010 sample), one
line per block:
    {"block":{...full txs...},"number":N,"receipts":[...]}

Definitions follow task 006 and data-auditor 006 (M1-M3), see
.claude/skills/hoodchain-mev/references/chain-facts.md:
  system tx   type 0x6a (ArbitrumInternalTx) only
  user tx     every tx that is not 0x6a; l2 = types 0x0-0x4, l1 = 0x64-0x69 and 0x78
  fee         gasUsed * effectiveGasPrice of a receipt, wei (includes the L1 part)

feed_audit.py (.claude/skills/feed-audit/scripts) deliberately does not use this
module: it is installed on the server as a single file.
"""

from __future__ import annotations

import json
import subprocess
from collections.abc import Iterable, Iterator

# topic0 of Swap events, from references/contracts.md (same bytes as crates/decoders).
V3_SWAP = "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67"
V4_SWAP = "0x40e9cecb9f5f1f1c5b9c97dec2917b7ee92e57ba5563708daca94dd84ad7112f"

SYS = "0x6a"
L2_TYPES = frozenset({"0x0", "0x1", "0x2", "0x3", "0x4"})
L1_TYPES = frozenset({"0x64", "0x65", "0x66", "0x67", "0x68", "0x69", "0x78"})


def iter_block_lines(path: str) -> Iterator[bytes]:
    """Stream the lines of a .jsonl.zst file (multi-frame safe) without the
    trailing newline, without loading the whole file into memory."""
    with subprocess.Popen(["zstd", "-dc", "--", path], stdout=subprocess.PIPE) as proc:
        for line in proc.stdout:
            yield line.rstrip(b"\n")
    if proc.returncode:
        raise RuntimeError(f"zstd -dc {path}: rc={proc.returncode}")


def iter_blocks(path: str) -> Iterator[dict]:
    """Parsed blocks-format records of a .jsonl.zst file, in file order."""
    for line in iter_block_lines(path):
        yield json.loads(line)


def tx_receipt_pairs(block: dict, receipts: list) -> list[tuple[dict, dict]]:
    """(tx, receipt) of every tx in block order; ValueError if the receipts do
    not match the transactions one to one (count and transaction hash)."""
    txs = block["transactions"]
    if len(txs) != len(receipts):
        raise ValueError(f"block {block.get('number')}: {len(txs)} txs, {len(receipts)} receipts")
    for t, r in zip(txs, receipts):
        if t["hash"] != r["transactionHash"]:
            raise ValueError(f"block {block.get('number')}: tx {t['hash']} paired with receipt {r['transactionHash']}")
    return list(zip(txs, receipts))


def user_tx_pairs(block: dict, receipts: list) -> list[tuple[dict, dict]]:
    """(tx, receipt) of the user txs (type != 0x6a), checked as in tx_receipt_pairs."""
    return [(t, r) for t, r in tx_receipt_pairs(block, receipts) if t["type"] != SYS]


def tx_class(tx_type: str) -> str:
    """'l2', 'l1' or 'other' for a user tx type."""
    if tx_type in L2_TYPES:
        return "l2"
    return "l1" if tx_type in L1_TYPES else "other"


def fee_wei(receipt: dict) -> int:
    return int(receipt["gasUsed"], 16) * int(receipt["effectiveGasPrice"], 16)


def gas_used_for_l1(receipt: dict) -> int:
    return int(receipt.get("gasUsedForL1", "0x0"), 16)


def median_int(values: Iterable[int]) -> int:
    """Median of integers (wei) without a float: the mean of the two middle values is floored."""
    s = sorted(values)
    if not s:
        raise ValueError("median of an empty list")
    mid = len(s) // 2
    return s[mid] if len(s) % 2 else (s[mid - 1] + s[mid]) // 2
