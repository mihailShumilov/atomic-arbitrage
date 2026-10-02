#!/usr/bin/env python3
"""Per-block metrics for a blocks-format jsonl.zst file (task 010). Offline, no RPC.

Input lines: {"block":{...full txs...},"number":N,"receipts":[...]}  (enricher format).
Output TSV, one row per block, header in the first line; written to a temporary
file next to --out and renamed on success (no partial file on failure).

Definitions (task 006 + data-auditor 006, M1-M3; constants in hoodlib.py):
  system tx      type 0x6a (ArbitrumInternalTx) only
  user tx        every tx that is not 0x6a; subclasses
                   l2  = signed on L2, types 0x0-0x4
                   l1  = arrived from L1, types 0x64-0x69 and 0x78
                   other = anything else (should be 0)
  receipts       must pair with txs one to one (count and hash), else the run stops
  revert         receipt status 0x0 among user txs
  swap log       receipt log with topics[0] = Uniswap v3 Swap or v4 Swap, any emitter
                 (logs, not trades; launchpad curve trades are not counted)
  tx_with_swap   user txs with >= 1 swap log
  fee            gasUsed * effectiveGasPrice of a user tx, wei (includes the L1 part);
                 fee_median_wei is an integer median (mean of the two middle values floored)
  sizes          compact sorted-key JSON of "block" / "receipts" (same bytes as
                 enricher's raw counter); line_zstd3 = the whole line compressed alone
                 with `zstd -3` (no cross-block context, see report for calibration)
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import subprocess
import sys

import hoodlib as hl

WD = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]

COLS = [
    "hour_start_utc",
    "date",
    "weekday",
    "hour",
    "block",
    "ts",
    "offset_s",
    "n_tx",
    "n_sys",
    "n_user",
    "n_l2",
    "n_l1",
    "n_other",
    "reverts",
    "reverts_l2",
    "reverts_l1",
    "swap_v3",
    "swap_v4",
    "swap_logs",
    "tx_with_swap",
    "gas_used",
    "base_fee_wei",
    "n_l1gas_pos",
    "fee_median_wei",
    "fee_sum_wei",
    "block_raw_b",
    "receipts_raw_b",
    "line_raw_b",
    "line_zstd3_b",
    "size_field",
]


def dumps(o: object) -> bytes:
    return json.dumps(o, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()


def zlen(b: bytes) -> int:
    return len(subprocess.run(["zstd", "-3", "-q", "-c"], input=b, capture_output=True, check=True).stdout)


def metrics(line: bytes, hour_start: int | None = None, obj: dict | None = None) -> dict:
    """One output row (COLS) for one blocks-format line.

    obj: the already parsed line (json.loads(line)), to avoid parsing it twice.
    """
    o = obj if obj is not None else json.loads(line)
    b, rc = o["block"], o["receipts"]
    txs = b["transactions"]
    pairs = hl.tx_receipt_pairs(b, rc)
    ts = int(b["timestamp"], 16)
    hs = hour_start if hour_start is not None else ts - ts % 3600
    d = dt.datetime.fromtimestamp(hs, dt.timezone.utc)
    m = dict.fromkeys(COLS, 0)
    m.update(
        hour_start_utc=d.strftime("%Y-%m-%dT%H:%M:%SZ"),
        date=d.strftime("%Y-%m-%d"),
        weekday=WD[d.weekday()],
        hour=d.hour,
        block=o["number"],
        ts=ts,
        offset_s=ts - hs,
        n_tx=len(txs),
        gas_used=int(b["gasUsed"], 16),
        base_fee_wei=int(b.get("baseFeePerGas", "0x0"), 16),
        size_field=int(b.get("size", "0x0"), 16),
    )
    fees = []
    for t, r in pairs:
        if t["type"] == hl.SYS:
            m["n_sys"] += 1
            continue
        m["n_user"] += 1
        cls = hl.tx_class(t["type"])
        m["n_" + cls] += 1
        if r["status"] == "0x0":
            m["reverts"] += 1
            if cls in ("l2", "l1"):
                m["reverts_" + cls] += 1
        sw = 0
        for lg in r["logs"]:
            t0 = lg["topics"][0] if lg["topics"] else None
            if t0 == hl.V3_SWAP:
                m["swap_v3"] += 1
                sw += 1
            elif t0 == hl.V4_SWAP:
                m["swap_v4"] += 1
                sw += 1
        m["swap_logs"] += sw
        m["tx_with_swap"] += 1 if sw else 0
        if hl.gas_used_for_l1(r) > 0:
            m["n_l1gas_pos"] += 1
        fees.append(hl.fee_wei(r))
    m["fee_median_wei"] = hl.median_int(fees) if fees else ""
    m["fee_sum_wei"] = sum(fees)
    m["block_raw_b"] = len(dumps(b))
    m["receipts_raw_b"] = len(dumps(rc))
    m["line_raw_b"] = len(line)
    m["line_zstd3_b"] = zlen(line)
    return m


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("input", help="blocks-format .jsonl.zst")
    ap.add_argument("--index", help="sampler index TSV (hour_unix per block) to fill hour_start/offset")
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    hour_of = {}
    if a.index:
        with open(a.index) as f:
            next(f)
            for row in f:
                p = row.rstrip("\n").split("\t")
                if p[2]:
                    hour_of[int(p[2])] = int(p[1])
    n = 0
    # Write next to --out and rename only after the whole input was read: a failed
    # run (receipt mismatch, zstd error) never leaves a partial or clobbered TSV.
    tmp = f"{a.out}.tmp.{os.getpid()}"
    try:
        with open(tmp, "w") as w:
            w.write("\t".join(COLS) + "\n")
            for line in hl.iter_block_lines(a.input):
                o = json.loads(line)
                m = metrics(line, hour_of.get(o["number"]), o)
                w.write("\t".join(str(m[c]) for c in COLS) + "\n")
                n += 1
        os.replace(tmp, a.out)
    except BaseException:
        if os.path.exists(tmp):
            os.remove(tmp)
        raise
    print(f"{n} rows -> {a.out}", file=sys.stderr)


if __name__ == "__main__":
    main()
