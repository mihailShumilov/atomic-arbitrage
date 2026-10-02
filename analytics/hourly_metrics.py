#!/usr/bin/env python3
"""Per-block metrics for a blocks-format jsonl.zst file (task 010). Offline, no RPC.

Input lines: {"block":{...full txs...},"number":N,"receipts":[...]}  (enricher format).
Output TSV, one row per block, header in the first line.

Definitions (task 006 + data-auditor 006, M1-M3):
  system tx      type 0x6a (ArbitrumInternalTx) only
  user tx        every tx that is not 0x6a; subclasses
                   l2  = signed on L2, types 0x0-0x4
                   l1  = arrived from L1, types 0x64-0x69 and 0x78
                   other = anything else (should be 0)
  revert         receipt status 0x0 among user txs
  swap log       receipt log with topics[0] = Uniswap v3 Swap or v4 Swap, any emitter
                 (logs, not trades; launchpad curve trades are not counted)
  tx_with_swap   user txs with >= 1 swap log
  fee            gasUsed * effectiveGasPrice of a user tx, wei (includes the L1 part)
  sizes          compact sorted-key JSON of "block" / "receipts" (same bytes as
                 enricher's raw counter); line_zstd3 = the whole line compressed alone
                 with `zstd -3` (no cross-block context, see report for calibration)
"""

import argparse
import datetime as dt
import json
import statistics as st
import subprocess
import sys

V3_SWAP = "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67"
V4_SWAP = "0x40e9cecb9f5f1f1c5b9c97dec2917b7ee92e57ba5563708daca94dd84ad7112f"
SYS = "0x6a"
L2_TYPES = {"0x0", "0x1", "0x2", "0x3", "0x4"}
L1_TYPES = {"0x64", "0x65", "0x66", "0x67", "0x68", "0x69", "0x78"}
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


def dumps(o):
    return json.dumps(o, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()


def zlen(b):
    return len(subprocess.run(["zstd", "-3", "-q", "-c"], input=b, capture_output=True, check=True).stdout)


def metrics(line, hour_start=None):
    o = json.loads(line)
    b, rc = o["block"], o["receipts"]
    txs = b["transactions"]
    assert len(txs) == len(rc), f"block {o['number']}: tx/receipt count"
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
    for t, r in zip(txs, rc):
        assert t["hash"] == r["transactionHash"]
        ty = t["type"]
        if ty == SYS:
            m["n_sys"] += 1
            continue
        m["n_user"] += 1
        cls = "l2" if ty in L2_TYPES else "l1" if ty in L1_TYPES else "other"
        m["n_" + cls] += 1
        if r["status"] == "0x0":
            m["reverts"] += 1
            if cls in ("l2", "l1"):
                m["reverts_" + cls] += 1
        sw = 0
        for lg in r["logs"]:
            t0 = lg["topics"][0] if lg["topics"] else None
            if t0 == V3_SWAP:
                m["swap_v3"] += 1
                sw += 1
            elif t0 == V4_SWAP:
                m["swap_v4"] += 1
                sw += 1
        m["swap_logs"] += sw
        m["tx_with_swap"] += 1 if sw else 0
        if int(r.get("gasUsedForL1", "0x0"), 16) > 0:
            m["n_l1gas_pos"] += 1
        fees.append(int(r["gasUsed"], 16) * int(r["effectiveGasPrice"], 16))
    m["fee_median_wei"] = int(st.median(fees)) if fees else ""
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
            for l in f:
                p = l.rstrip("\n").split("\t")
                if p[2]:
                    hour_of[int(p[2])] = int(p[1])
    raw = subprocess.run(["zstd", "-dc", a.input], capture_output=True, check=True).stdout
    n = 0
    with open(a.out, "w") as w:
        w.write("\t".join(COLS) + "\n")
        for line in raw.splitlines():
            num = json.loads(line)["number"]
            m = metrics(line, hour_of.get(num))
            w.write("\t".join(str(m[c]) for c in COLS) + "\n")
            n += 1
    print(f"{n} rows -> {a.out}", file=sys.stderr)


if __name__ == "__main__":
    main()
