#!/usr/bin/env python3
"""Summary tables for the hourly block sample (task 010). Offline, no RPC.

Inputs: hourly-metrics.tsv (analytics/hourly_metrics.py), the raw sample
.jsonl.zst (per-tx fees by day) and the 006 window files in data/blocks.
Prints markdown tables to stdout.

Weights: each sampled block stands for the blocks of its hour; the weight is
n(next hour sample) - n(this sample) (last hour: the median weight). Means are
block-weighted; percentiles are over sampled blocks (unweighted).
File size per block = line_zstd3_b (line compressed alone) x calibration factor
measured on 006 windows (blocks-*.jsonl.zst size / sum of per-line zstd-3).
Intervals: day-block bootstrap (resample days with replacement), 2000 draws, seed 10.
"""

import argparse
import csv
import datetime as dt
import json
import os
import random
import statistics as st
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import hourly_metrics as hm

WINDOWS_006 = [
    ("W1 29.09 12:01 Tue", "75640227-75640426"),
    ("W2 23.09 12:00 Wed", "70496152-70496351"),
    ("W3 09.09 12:00 Wed", "58529862-58530061"),
    ("W4 04.08 12:00 Tue", "27607293-27607492"),
    ("003 30.09 12:56 Wed", "76532007-76532306"),
]


def pct(xs, p):
    xs = sorted(xs)
    if not xs:
        return float("nan")
    k = (len(xs) - 1) * p / 100
    f = int(k)
    c = min(f + 1, len(xs) - 1)
    return xs[f] + (xs[c] - xs[f]) * (k - f)


def rank(xs, v):
    """percent of xs strictly below v, plus half of ties"""
    lo = sum(1 for x in xs if x < v)
    eq = sum(1 for x in xs if x == v)
    return 100.0 * (lo + 0.5 * eq) / len(xs)


def wmean(rows, f):
    num = sum(r["w"] * f(r) for r in rows)
    den = sum(r["w"] for r in rows)
    return num / den


def ratio_w(rows, a, b):
    return sum(r["w"] * r[a] for r in rows) / max(1e-12, sum(r["w"] * r[b] for r in rows))


def main():
    root = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--metrics", default=os.path.join(root, "data/samples/hourly-metrics.tsv"))
    ap.add_argument("--raw", default=os.path.join(root, "data/samples/hourly-20260804-20260930.jsonl.zst"))
    ap.add_argument("--index", default=os.path.join(root, "data/samples/hourly-20260804-20260930.index.tsv"))
    ap.add_argument("--blocks-dir", default=os.path.join(root, "data/blocks"))
    a = ap.parse_args()

    rows = list(csv.DictReader(open(a.metrics), delimiter="\t"))
    for r in rows:
        for k in r:
            if k not in ("hour_start_utc", "date", "weekday"):
                r[k] = float(r[k]) if r[k] != "" else float("nan")
    rows.sort(key=lambda r: r["block"])
    idx = list(csv.DictReader(open(a.index), delimiter="\t"))
    gaps = [i["hour_start_utc"] for i in idx if not i["block"]]
    n_hours = len(idx)

    # weights: blocks per hour from consecutive samples (only if hours are adjacent)
    for i, r in enumerate(rows):
        nxt = rows[i + 1] if i + 1 < len(rows) else None
        if nxt is not None:
            hours = round((nxt["ts"] - nxt["offset_s"] - (r["ts"] - r["offset_s"])) / 3600)
            r["w"] = (nxt["block"] - r["block"]) / max(1, hours)
        else:
            r["w"] = None
    med_w = st.median([r["w"] for r in rows if r["w"] is not None])
    for r in rows:
        if r["w"] is None:
            r["w"] = med_w

    # calibration on 006 windows
    cal = []
    win = {}
    for name, rng in WINDOWS_006:
        f = os.path.join(a.blocks_dir, f"blocks-{rng}.jsonl.zst")
        raw = subprocess.run(["zstd", "-dc", f], capture_output=True, check=True).stdout
        ms = [hm.metrics(l) for l in raw.splitlines()]
        nb = len(ms)
        line_z = sum(m["line_zstd3_b"] for m in ms) / nb
        fpb = os.path.getsize(f) / nb
        cal.append((name, fpb, line_z, fpb / line_z))
        nu = sum(m["n_user"] for m in ms)
        win[name] = dict(
            file_kb=fpb / 1000,
            line_z_kb=line_z / 1000,
            user=nu / nb,
            swaps=sum(m["swap_logs"] for m in ms) / nb,
            txsw=sum(m["tx_with_swap"] for m in ms) / nb,
            rev=sum(m["reverts"] for m in ms) / nu,
            raw_kb=sum(m["block_raw_b"] + m["receipts_raw_b"] for m in ms) / nb / 1000,
        )
    cal_lo = min(c[3] for c in cal)
    cal_hi = max(c[3] for c in cal)
    cal_mid = st.mean(c[3] for c in cal)

    for r in rows:
        r["raw_kb"] = (r["block_raw_b"] + r["receipts_raw_b"]) / 1000
        r["line_z_kb"] = r["line_zstd3_b"] / 1000
        r["file_kb"] = r["line_z_kb"] * cal_mid
        r["rev_share"] = r["reverts"] / r["n_user"] if r["n_user"] else float("nan")

    P = print
    P(
        f"## Coverage\n\nhours in period: {n_hours}; sampled: {len(rows)} ({100 * len(rows) / n_hours:.2f}%); gaps: {len(gaps)} {gaps}"
    )
    offs = [r["offset_s"] for r in rows]
    P(
        f"offset ts-T, s: min {min(offs):.0f} p10 {pct(offs, 10):.0f} p50 {pct(offs, 50):.0f} p90 {pct(offs, 90):.0f} max {max(offs):.0f}"
    )
    P(
        f"blocks per hour (weight): min {min(r['w'] for r in rows):.0f} p50 {med_w:.0f} max {max(r['w'] for r in rows):.0f}"
    )
    P(
        "tx types: "
        + ", ".join(
            f"{k}={int(sum(r[k] for r in rows))}"
            for k in ("n_tx", "n_sys", "n_user", "n_l2", "n_l1", "n_other", "reverts", "reverts_l2", "reverts_l1")
        )
    )
    P(
        f"blocks with n_sys != 1: {sum(1 for r in rows if r['n_sys'] != 1)}; blocks without user tx: {sum(1 for r in rows if r['n_user'] == 0)}"
    )

    P(
        "\n## Calibration file/line (006 windows)\n\n| window | file B/blk | line zstd-3 B/blk | ratio |\n|---|---|---|---|"
    )
    for c in cal:
        P(f"| {c[0]} | {c[1]:.0f} | {c[2]:.0f} | {c[3]:.3f} |")
    P(f"ratio min/mean/max = {cal_lo:.3f} / {cal_mid:.3f} / {cal_hi:.3f}")

    # 1. period stats
    P("\n## Period stats (sampled blocks; mean = block-weighted, p = unweighted)\n")
    P("| metric | mean | p10 | p50 | p90 | max |\n|---|---|---|---|---|---|")
    mets = [
        ("file KB/blk (line zstd-3 x %.3f)" % cal_mid, "file_kb"),
        ("line zstd-3 KB/blk", "line_z_kb"),
        ("raw JSON block+receipts KB/blk", "raw_kb"),
        ("user tx/blk (no 0x6a)", "n_user"),
        ("swap logs v3+v4/blk", "swap_logs"),
        ("  v3", "swap_v3"),
        ("  v4", "swap_v4"),
        ("tx with swap/blk", "tx_with_swap"),
        ("reverts/blk", "reverts"),
        ("gasUsed/blk, M", "gas_m"),
        ("base fee, gwei", "bf_gwei"),
    ]
    for r in rows:
        r["gas_m"] = r["gas_used"] / 1e6
        r["bf_gwei"] = r["base_fee_wei"] / 1e9
    for lab, k in mets:
        xs = [r[k] for r in rows]
        P(
            f"| {lab} | {wmean(rows, lambda r: r[k]):.3f} | {pct(xs, 10):.3f} | {pct(xs, 50):.3f} | {pct(xs, 90):.3f} | {max(xs):.3f} |"
        )
    rs = [r["rev_share"] for r in rows if r["n_user"]]
    P(
        f"| revert share per block | pooled {ratio_w(rows, 'reverts', 'n_user'):.4f} | {pct(rs, 10):.4f} | {pct(rs, 50):.4f} | {pct(rs, 90):.4f} | {max(rs):.4f} |"
    )
    P(f"swap logs per tx-with-swap (pooled): {ratio_w(rows, 'swap_logs', 'tx_with_swap'):.3f}")

    # bootstrap by day
    days = sorted({r["date"] for r in rows})
    by_day = {d: [r for r in rows if r["date"] == d] for d in days}
    rnd = random.Random(10)

    def boot(fn, n=2000):
        vals = []
        for _ in range(n):
            pick = [r for d in (rnd.choice(days) for _ in days) for r in by_day[d]]
            vals.append(fn(pick))
        return pct(vals, 2.5), pct(vals, 97.5)

    bl = {}
    for lab, k in [
        ("file_kb", "file_kb"),
        ("line_z_kb", "line_z_kb"),
        ("raw_kb", "raw_kb"),
        ("n_user", "n_user"),
        ("swap_logs", "swap_logs"),
        ("tx_with_swap", "tx_with_swap"),
    ]:
        bl[k] = boot(lambda p, k=k: wmean(p, lambda r: r[k]))
    bl["rev"] = boot(lambda p: ratio_w(p, "reverts", "n_user"))
    P(
        "\nday-bootstrap 95% interval of the period mean: "
        + "; ".join(f"{k} {v[0]:.3f}..{v[1]:.3f}" for k, v in bl.items())
    )

    # 2. hour profile
    def prof(key_fn, keys, title):
        P(
            f"\n## {title}\n\n| {title.split()[-1]} | n | user tx/blk mean (p50) | swap logs/blk | tx w/ swap/blk | revert share (pooled) | blocks with revert | file KB/blk mean (p50) | raw KB/blk | base fee gwei (p50) |\n|---|---|---|---|---|---|---|---|---|---|"
        )
        for kk in keys:
            g = [r for r in rows if key_fn(r) == kk]
            if not g:
                P(f"| {kk} | 0 | | | | | | | | |")
                continue
            P(
                f"| {kk} | {len(g)} | {wmean(g, lambda r: r['n_user']):.2f} ({pct([r['n_user'] for r in g], 50):.0f}) | "
                f"{wmean(g, lambda r: r['swap_logs']):.2f} | {wmean(g, lambda r: r['tx_with_swap']):.2f} | "
                f"{100 * ratio_w(g, 'reverts', 'n_user'):.1f}% | {100 * sum(1 for r in g if r['reverts'] > 0) / len(g):.0f}% | "
                f"{wmean(g, lambda r: r['file_kb']):.2f} ({pct([r['file_kb'] for r in g], 50):.2f}) | "
                f"{wmean(g, lambda r: r['raw_kb']):.1f} | {pct([r['bf_gwei'] for r in g], 50):.4f} |"
            )

    prof(lambda r: int(r["hour"]), range(24), "Profile by hour UTC")
    prof(lambda r: r["weekday"], hm.WD, "Profile by weekday")

    # per-tx fees and l1gas by day from raw
    raw = subprocess.run(["zstd", "-dc", a.raw], capture_output=True, check=True).stdout
    day_fees, day_l1 = {}, {}
    for line in raw.splitlines():
        o = json.loads(line)
        b, rc = o["block"], o["receipts"]
        ts = int(b["timestamp"], 16)
        d = dt.datetime.fromtimestamp(ts - ts % 3600, dt.timezone.utc).strftime("%Y-%m-%d")
        for t, r in zip(b["transactions"], rc):
            if t["type"] == hm.SYS:
                continue
            fee = int(r["gasUsed"], 16) * int(r["effectiveGasPrice"], 16)
            l1 = int(r.get("gasUsedForL1", "0x0"), 16)
            day_fees.setdefault(d, []).append(fee)
            day_l1.setdefault(d, []).append(l1)

    P("\n## Daily (all days): base fee, gasUsedForL1>0 share, fee\n")
    P(
        "| date | wd | n blk | user tx | base fee gwei min/p50/max | tx with gasUsedForL1>0 | median fee, µETH | p90 fee, µETH | median fee µETH: l1>0 / l1=0 | blocks/day | file KB/blk | GB/day est |\n|---|---|---|---|---|---|---|---|---|---|---|---|"
    )
    daily = []
    for i, d in enumerate(days):
        g = by_day[d]
        bfs = [r["bf_gwei"] for r in g]
        fees = day_fees.get(d, [])
        l1 = day_l1.get(d, [])
        pos = [f for f, x in zip(fees, l1) if x > 0]
        zer = [f for f, x in zip(fees, l1) if x == 0]
        g_sorted = sorted(g, key=lambda r: r["hour"])
        # blocks/day from first sample of this day to first sample of the next day
        nxt = by_day.get(days[i + 1]) if i + 1 < len(days) else None
        if nxt and len(g_sorted) == 24 and min(r["hour"] for r in nxt) == 0 and g_sorted[0]["hour"] == 0:
            bpd = min(nxt, key=lambda r: r["hour"])["block"] - g_sorted[0]["block"]
            bpd *= 86400 / (
                min(nxt, key=lambda r: r["hour"])["ts"]
                - min(nxt, key=lambda r: r["hour"])["offset_s"]
                - (g_sorted[0]["ts"] - g_sorted[0]["offset_s"])
            )
        else:
            bpd = sum(r["w"] for r in g) * 24 / len(g)
        fkb = wmean(g, lambda r: r["file_kb"])
        rkb = wmean(g, lambda r: r["raw_kb"])
        lz = wmean(g, lambda r: r["line_z_kb"])
        daily.append(dict(date=d, bpd=bpd, file_kb=fkb, raw_kb=rkb, line_z_kb=lz, n=len(g)))
        wd = g[0]["weekday"]
        P(
            f"| {d} | {wd} | {len(g)} | {len(fees)} | {min(bfs):.4f}/{pct(bfs, 50):.4f}/{max(bfs):.4f} | "
            f"{100 * len(pos) / max(1, len(fees)):.1f}% | {st.median(fees) / 1e12 if fees else float('nan'):.3f} | "
            f"{pct(fees, 90) / 1e12 if fees else float('nan'):.3f} | "
            f"{(st.median(pos) / 1e12) if pos else float('nan'):.3f} / {(st.median(zer) / 1e12) if zer else float('nan'):.3f} | "
            f"{bpd:.0f} | {fkb:.2f} | {bpd * fkb / 1e6:.2f} |"
        )

    P(
        "\n## Hourly 28.09-30.09: base fee and gasUsedForL1>0\n\n| hour | block | base fee gwei | user tx | l1>0 | median fee µETH |\n|---|---|---|---|---|---|"
    )
    for line in raw.splitlines():
        o = json.loads(line)
        b, rc = o["block"], o["receipts"]
        ts = int(b["timestamp"], 16)
        if ts < 1790553600:  # 2026-09-28 00:00 UTC
            continue
        fees = [
            int(r["gasUsed"], 16) * int(r["effectiveGasPrice"], 16)
            for t, r in zip(b["transactions"], rc)
            if t["type"] != hm.SYS
        ]
        l1 = [int(r.get("gasUsedForL1", "0x0"), 16) for t, r in zip(b["transactions"], rc) if t["type"] != hm.SYS]
        P(
            f"| {dt.datetime.fromtimestamp(ts, dt.timezone.utc):%m-%d %H:%M:%S} | {o['number']} | {int(b['baseFeePerGas'], 16) / 1e9:.4f} | "
            f"{len(fees)} | {sum(1 for x in l1 if x > 0)} | {st.median(fees) / 1e12 if fees else float('nan'):.3f} |"
        )

    # 5. disk and new-block stream
    first, last = rows[0], rows[-1]
    t_first = first["ts"] - first["offset_s"]
    t_last = last["ts"] - last["offset_s"]
    span_rate = (last["block"] - first["block"]) / (last["ts"] - first["ts"])
    last7 = [r for r in rows if r["ts"] >= last["ts"] - 7 * 86400]
    r7 = (last["block"] - last7[0]["block"]) / (last["ts"] - last7[0]["ts"])
    bpds = [x["bpd"] for x in daily if x["n"] == 24]
    rate_lo, rate_hi = min(bpds) / 86400, max(bpds) / 86400
    P(
        f"\n## Block counts\n\nfirst sample {int(first['block'])} @ {first['hour_start_utc']} (+{first['offset_s']:.0f}s); last {int(last['block'])} @ {last['hour_start_utc']} (+{last['offset_s']:.0f}s)"
    )
    P(f"mean rate over sample {span_rate:.4f} blk/s; last 7 days {r7:.4f}; daily min/max {rate_lo:.4f}/{rate_hi:.4f}")
    P(
        f"blocks/day over 58 days: min {min(bpds):.0f} p10 {pct(bpds, 10):.0f} p50 {pct(bpds, 50):.0f} p90 {pct(bpds, 90):.0f} max {max(bpds):.0f}"
    )

    hist_kb = wmean(rows, lambda r: r["line_z_kb"])
    b_lo, b_hi = bl["line_z_kb"]
    l7_kb = wmean(last7, lambda r: r["line_z_kb"])
    l7_days = [x["line_z_kb"] for x in daily if x["date"] >= last7[0]["date"]]
    P(
        f"\nline zstd-3 KB/blk: period mean {hist_kb:.3f} (boot {b_lo:.3f}..{b_hi:.3f}); last 7 days {l7_kb:.3f} (daily {min(l7_days):.3f}..{max(l7_days):.3f})"
    )
    P(
        f"raw JSON KB/blk: period mean {wmean(rows, lambda r: r['raw_kb']):.1f} (boot {bl['raw_kb'][0]:.1f}..{bl['raw_kb'][1]:.1f})"
    )
    P(
        "\n| as of (00:00 UTC) | blocks since 04.08 00:00, M (lo..hi) | file GB: point (lo..hi) | raw JSON TB: point (lo..hi) | blocks since 04.08 12:00, M |\n|---|---|---|---|---|"
    )
    n_0408_12 = 27607293
    raw_kb = wmean(rows, lambda r: r["raw_kb"])
    raw_lo, raw_hi = bl["raw_kb"]
    l7_raw = wmean(last7, lambda r: r["raw_kb"])
    l7_raw_days = [x["raw_kb"] for x in daily if x["date"] >= last7[0]["date"]]
    for label, iso_d in [("07.10", "2026-10-07"), ("14.10", "2026-10-14")]:
        T = int(dt.datetime.fromisoformat(iso_d).replace(tzinfo=dt.timezone.utc).timestamp())
        dt_s = T - last["ts"]
        past = last["block"] - first["block"]
        fut_pt, fut_lo, fut_hi = r7 * dt_s, rate_lo * dt_s, rate_hi * dt_s
        nb_pt, nb_lo, nb_hi = past + fut_pt, past + fut_lo, past + fut_hi
        gb_pt = (past * hist_kb + fut_pt * l7_kb) * cal_mid / 1e6
        gb_lo = (past * b_lo + fut_lo * min(l7_days)) * cal_lo / 1e6
        gb_hi = (past * b_hi + fut_hi * max(l7_days)) * cal_hi / 1e6
        tb_pt = (past * raw_kb + fut_pt * l7_raw) / 1e9
        tb_lo = (past * raw_lo + fut_lo * min(l7_raw_days)) / 1e9
        tb_hi = (past * raw_hi + fut_hi * max(l7_raw_days)) / 1e9
        P(
            f"| {label} | {nb_pt / 1e6:.2f} ({nb_lo / 1e6:.2f}..{nb_hi / 1e6:.2f}) | {gb_pt:.0f} ({gb_lo:.0f}..{gb_hi:.0f}) | "
            f"{tb_pt:.2f} ({tb_lo:.2f}..{tb_hi:.2f}) | {(last['block'] - n_0408_12 + fut_pt) / 1e6:.2f} |"
        )

    gbd = [x["bpd"] * x["line_z_kb"] for x in daily if x["n"] == 24]
    P("\n## New-block stream, GB/day (daily blocks x daily mean line zstd-3 KB x calibration)\n")
    P("| set | min | p10 | p50 | p90 | max | mean |\n|---|---|---|---|---|---|---|")
    for lab, c in [(f"x{cal_lo:.3f}", cal_lo), (f"x{cal_mid:.3f}", cal_mid), (f"x{cal_hi:.3f}", cal_hi)]:
        v = [g * c / 1e6 for g in gbd]
        P(
            f"| all 58 days {lab} | {min(v):.2f} | {pct(v, 10):.2f} | {pct(v, 50):.2f} | {pct(v, 90):.2f} | {max(v):.2f} | {st.mean(v):.2f} |"
        )
    v7 = [x["bpd"] * x["line_z_kb"] * cal_mid / 1e6 for x in daily if x["n"] == 24 and x["date"] >= last7[0]["date"]]
    P(f"| last 7 days x{cal_mid:.3f} | {min(v7):.2f} | | {pct(v7, 50):.2f} | | {max(v7):.2f} | {st.mean(v7):.2f} |")
    rawd = [x["bpd"] * x["raw_kb"] / 1e6 for x in daily if x["n"] == 24]
    P(
        f"| raw JSON GB/day, all days | {min(rawd):.1f} | {pct(rawd, 10):.1f} | {pct(rawd, 50):.1f} | {pct(rawd, 90):.1f} | {max(rawd):.1f} | {st.mean(rawd):.1f} |"
    )

    # 6. 006 windows in the distribution
    P(
        "\n## 006 windows vs sample distribution (percentile rank: share of sampled blocks / days / 12:00 blocks below the window mean)\n"
    )
    P(
        "| window | metric | window value | rank among blocks | rank among daily means | rank among 12:00 UTC blocks |\n|---|---|---|---|---|---|"
    )
    noon = [r for r in rows if int(r["hour"]) == 12]
    dm = {
        k: [wmean(by_day[d], lambda r, k=k: r[k]) for d in days]
        for k in ("n_user", "swap_logs", "tx_with_swap", "line_z_kb", "raw_kb")
    }
    dm["rev"] = [ratio_w(by_day[d], "reverts", "n_user") for d in days]
    for name, _ in WINDOWS_006:
        w = win[name]
        for lab, wk, rk in [
            ("user tx/blk", "user", "n_user"),
            ("swap logs/blk", "swaps", "swap_logs"),
            ("tx w/ swap/blk", "txsw", "tx_with_swap"),
            ("line zstd-3 KB/blk", "line_z_kb", "line_z_kb"),
            ("raw KB/blk", "raw_kb", "raw_kb"),
            ("revert share", "rev", "rev_share"),
        ]:
            blocks = [r[rk] for r in rows if r[rk] == r[rk]]
            nn = [r[rk] for r in noon if r[rk] == r[rk]]
            dkey = "rev" if rk == "rev_share" else rk
            if rk == "rev_share":  # per-block shares are mostly 0; only daily pooled shares are comparable
                P(f"| {name} | {lab} | {w[wk]:.3f} | — | {rank(dm[dkey], w[wk]):.0f} | — |")
            else:
                P(
                    f"| {name} | {lab} | {w[wk]:.3f} | {rank(blocks, w[wk]):.0f} | {rank(dm[dkey], w[wk]):.0f} | {rank(nn, w[wk]):.0f} |"
                )
        P(f"| {name} | file KB/blk (real file) | {w['file_kb']:.3f} | | | |")


if __name__ == "__main__":
    main()
