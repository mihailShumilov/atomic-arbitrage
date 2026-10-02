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

One-off report of task 010 (accepted): the period (58 days), the forecast dates
and the 04.08 12:00 anchor block are fixed below on purpose.
"""

from __future__ import annotations

import argparse
import csv
import dataclasses
import datetime as dt
import os
import random
import statistics as st
from operator import itemgetter

import hoodlib as hl
import hourly_metrics as hm

WINDOWS_006 = [
    ("W1 29.09 12:01 Tue", "75640227-75640426"),
    ("W2 23.09 12:00 Wed", "70496152-70496351"),
    ("W3 09.09 12:00 Wed", "58529862-58530061"),
    ("W4 04.08 12:00 Tue", "27607293-27607492"),
    ("003 30.09 12:56 Wed", "76532007-76532306"),
]
HOURLY_FROM_TS = 1790553600  # 2026-09-28 00:00 UTC: start of the "Hourly 28.09-30.09" table
BLOCK_0408_12 = 27607293  # first block of 2026-08-04 12:00 UTC (task 006 anchor)
FORECAST_DATES = [("07.10", "2026-10-07"), ("14.10", "2026-10-14")]
NAN = float("nan")


def pct(xs, p):
    """Percentile p (0..100) with linear interpolation between ranks."""
    xs = sorted(xs)
    if not xs:
        return NAN
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


def median_or_nan(xs):
    return st.median(xs) if xs else NAN


@dataclasses.dataclass
class Sample:
    """Sampled blocks (rows of hourly-metrics.tsv) and their grouping by day."""

    rows: list
    idx: list
    med_w: float
    days: list
    by_day: dict


@dataclasses.dataclass
class Calibration:
    """file size / per-line zstd-3 ratio on the 006 windows, plus window means."""

    cal: list
    win: dict
    lo: float
    mid: float
    hi: float


@dataclasses.dataclass
class RawSample:
    """Per-tx data of the raw sample, read in one streaming pass."""

    day_fees: dict
    day_l1: dict
    late_blocks: list  # (ts, number, base fee wei, fees, gasUsedForL1) of blocks from HOURLY_FROM_TS


def load_sample(metrics_path, index_path):
    with open(metrics_path) as f:
        rows = list(csv.DictReader(f, delimiter="\t"))
    for r in rows:
        for k in r:
            if k in ("hour_start_utc", "date", "weekday"):
                continue
            if k.endswith("_wei"):
                # wei stay exact integers (empty -> None: arithmetic fails loudly, unlike NaN);
                # divide only for display (bf_gwei)
                r[k] = int(r[k]) if r[k] != "" else None
            else:
                r[k] = float(r[k]) if r[k] != "" else NAN
    rows.sort(key=itemgetter("block"))
    with open(index_path) as f:
        idx = list(csv.DictReader(f, delimiter="\t"))

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
    days = sorted({r["date"] for r in rows})
    by_day = {d: [r for r in rows if r["date"] == d] for d in days}
    return Sample(rows, idx, med_w, days, by_day)


def calibrate(blocks_dir):
    cal = []
    win = {}
    for name, rng in WINDOWS_006:
        f = os.path.join(blocks_dir, f"blocks-{rng}.jsonl.zst")
        ms = [hm.metrics(line) for line in hl.iter_block_lines(f)]
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
    return Calibration(cal, win, min(c[3] for c in cal), st.mean(c[3] for c in cal), max(c[3] for c in cal))


def add_derived(rows, cal_mid):
    for r in rows:
        r["raw_kb"] = (r["block_raw_b"] + r["receipts_raw_b"]) / 1000
        r["line_z_kb"] = r["line_zstd3_b"] / 1000
        r["file_kb"] = r["line_z_kb"] * cal_mid
        r["rev_share"] = r["reverts"] / r["n_user"] if r["n_user"] else NAN
        r["gas_m"] = r["gas_used"] / 1e6
        r["bf_gwei"] = r["base_fee_wei"] / 1e9


def read_raw(path):
    """Per-day user-tx fees and gasUsedForL1, plus the blocks of the last days, in one pass."""
    raw = RawSample({}, {}, [])
    for o in hl.iter_blocks(path):
        b = o["block"]
        pairs = hl.user_tx_pairs(b, o["receipts"])
        ts = int(b["timestamp"], 16)
        d = dt.datetime.fromtimestamp(ts - ts % 3600, dt.timezone.utc).strftime("%Y-%m-%d")
        fees = [hl.fee_wei(r) for _, r in pairs]
        l1 = [hl.gas_used_for_l1(r) for _, r in pairs]
        raw.day_fees.setdefault(d, []).extend(fees)
        raw.day_l1.setdefault(d, []).extend(l1)
        if ts >= HOURLY_FROM_TS:
            raw.late_blocks.append((ts, o["number"], int(b["baseFeePerGas"], 16), fees, l1))
    return raw


def print_coverage(s):
    rows, med_w = s.rows, s.med_w
    gaps = [i["hour_start_utc"] for i in s.idx if not i["block"]]
    n_hours = len(s.idx)
    print(
        f"## Coverage\n\nhours in period: {n_hours}; sampled: {len(rows)} ({100 * len(rows) / n_hours:.2f}%); "
        f"gaps: {len(gaps)} {gaps}"
    )
    offs = [r["offset_s"] for r in rows]
    print(
        f"offset ts-T, s: min {min(offs):.0f} p10 {pct(offs, 10):.0f} p50 {pct(offs, 50):.0f} "
        f"p90 {pct(offs, 90):.0f} max {max(offs):.0f}"
    )
    print(
        f"blocks per hour (weight): min {min(r['w'] for r in rows):.0f} p50 {med_w:.0f} "
        f"max {max(r['w'] for r in rows):.0f}"
    )
    print(
        "tx types: "
        + ", ".join(
            f"{k}={int(sum(r[k] for r in rows))}"
            for k in ("n_tx", "n_sys", "n_user", "n_l2", "n_l1", "n_other", "reverts", "reverts_l2", "reverts_l1")
        )
    )
    print(
        f"blocks with n_sys != 1: {sum(1 for r in rows if r['n_sys'] != 1)}; "
        f"blocks without user tx: {sum(1 for r in rows if r['n_user'] == 0)}"
    )


def print_calibration(c):
    print(
        "\n## Calibration file/line (006 windows)\n\n"
        "| window | file B/blk | line zstd-3 B/blk | ratio |\n|---|---|---|---|"
    )
    for x in c.cal:
        print(f"| {x[0]} | {x[1]:.0f} | {x[2]:.0f} | {x[3]:.3f} |")
    print(f"ratio min/mean/max = {c.lo:.3f} / {c.mid:.3f} / {c.hi:.3f}")


def print_period_stats(rows, cal_mid):
    print("\n## Period stats (sampled blocks; mean = block-weighted, p = unweighted)\n")
    print("| metric | mean | p10 | p50 | p90 | max |\n|---|---|---|---|---|---|")
    mets = [
        (f"file KB/blk (line zstd-3 x {cal_mid:.3f})", "file_kb"),
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
    for lab, k in mets:
        xs = [r[k] for r in rows]
        print(
            f"| {lab} | {wmean(rows, itemgetter(k)):.3f} | {pct(xs, 10):.3f} | {pct(xs, 50):.3f} | "
            f"{pct(xs, 90):.3f} | {max(xs):.3f} |"
        )
    rs = [r["rev_share"] for r in rows if r["n_user"]]
    print(
        f"| revert share per block | pooled {ratio_w(rows, 'reverts', 'n_user'):.4f} | {pct(rs, 10):.4f} | "
        f"{pct(rs, 50):.4f} | {pct(rs, 90):.4f} | {max(rs):.4f} |"
    )
    print(f"swap logs per tx-with-swap (pooled): {ratio_w(rows, 'swap_logs', 'tx_with_swap'):.3f}")


def bootstrap(s):
    """Day-block bootstrap 95% intervals of period means (seed 10, draw order fixed)."""
    rnd = random.Random(10)

    def boot(fn, n=2000):
        vals = []
        for _ in range(n):
            pick = [r for d in (rnd.choice(s.days) for _ in s.days) for r in s.by_day[d]]
            vals.append(fn(pick))
        return pct(vals, 2.5), pct(vals, 97.5)

    bl = {}
    for k in ("file_kb", "line_z_kb", "raw_kb", "n_user", "swap_logs", "tx_with_swap"):
        bl[k] = boot(lambda p, k=k: wmean(p, itemgetter(k)))
    bl["rev"] = boot(lambda p: ratio_w(p, "reverts", "n_user"))
    print(
        "\nday-bootstrap 95% interval of the period mean: "
        + "; ".join(f"{k} {v[0]:.3f}..{v[1]:.3f}" for k, v in bl.items())
    )
    return bl


def print_profile(rows, key_fn, keys, title):
    print(
        f"\n## {title}\n\n| {title.split()[-1]} | n | user tx/blk mean (p50) | swap logs/blk | tx w/ swap/blk | "
        "revert share (pooled) | blocks with revert | file KB/blk mean (p50) | raw KB/blk | base fee gwei (p50) |\n"
        "|---|---|---|---|---|---|---|---|---|---|"
    )
    for kk in keys:
        g = [r for r in rows if key_fn(r) == kk]
        if not g:
            print(f"| {kk} | 0 | | | | | | | | |")
            continue
        print(
            f"| {kk} | {len(g)} | {wmean(g, itemgetter('n_user')):.2f} ({pct([r['n_user'] for r in g], 50):.0f}) | "
            f"{wmean(g, itemgetter('swap_logs')):.2f} | {wmean(g, itemgetter('tx_with_swap')):.2f} | "
            f"{100 * ratio_w(g, 'reverts', 'n_user'):.1f}% | "
            f"{100 * sum(1 for r in g if r['reverts'] > 0) / len(g):.0f}% | "
            f"{wmean(g, itemgetter('file_kb')):.2f} ({pct([r['file_kb'] for r in g], 50):.2f}) | "
            f"{wmean(g, itemgetter('raw_kb')):.1f} | {pct([r['bf_gwei'] for r in g], 50):.4f} |"
        )


def blocks_per_day(s, i, d):
    """Blocks/day from the first sample of day d to the first sample of the next day
    (needs 24 samples and both midnights), else from the hour weights."""
    g = s.by_day[d]
    g_sorted = sorted(g, key=itemgetter("hour"))
    nxt = s.by_day.get(s.days[i + 1]) if i + 1 < len(s.days) else None
    if nxt and len(g_sorted) == 24 and min(r["hour"] for r in nxt) == 0 and g_sorted[0]["hour"] == 0:
        bpd = min(nxt, key=itemgetter("hour"))["block"] - g_sorted[0]["block"]
        bpd *= 86400 / (
            min(nxt, key=itemgetter("hour"))["ts"]
            - min(nxt, key=itemgetter("hour"))["offset_s"]
            - (g_sorted[0]["ts"] - g_sorted[0]["offset_s"])
        )
        return bpd
    return sum(r["w"] for r in g) * 24 / len(g)


def print_daily(s, raw):
    print("\n## Daily (all days): base fee, gasUsedForL1>0 share, fee\n")
    print(
        "| date | wd | n blk | user tx | base fee gwei min/p50/max | tx with gasUsedForL1>0 | median fee, µETH | "
        "p90 fee, µETH | median fee µETH: l1>0 / l1=0 | blocks/day | file KB/blk | GB/day est |\n"
        "|---|---|---|---|---|---|---|---|---|---|---|---|"
    )
    daily = []
    for i, d in enumerate(s.days):
        g = s.by_day[d]
        bfs = [r["bf_gwei"] for r in g]
        fees = raw.day_fees.get(d, [])
        l1 = raw.day_l1.get(d, [])
        pos = [f for f, x in zip(fees, l1) if x > 0]
        zer = [f for f, x in zip(fees, l1) if x == 0]
        bpd = blocks_per_day(s, i, d)
        fkb = wmean(g, itemgetter("file_kb"))
        rkb = wmean(g, itemgetter("raw_kb"))
        lz = wmean(g, itemgetter("line_z_kb"))
        daily.append(dict(date=d, bpd=bpd, file_kb=fkb, raw_kb=rkb, line_z_kb=lz, n=len(g)))
        wd = g[0]["weekday"]
        print(
            f"| {d} | {wd} | {len(g)} | {len(fees)} | {min(bfs):.4f}/{pct(bfs, 50):.4f}/{max(bfs):.4f} | "
            f"{100 * len(pos) / max(1, len(fees)):.1f}% | {median_or_nan(fees) / 1e12:.3f} | "
            f"{pct(fees, 90) / 1e12 if fees else NAN:.3f} | "
            f"{median_or_nan(pos) / 1e12:.3f} / {median_or_nan(zer) / 1e12:.3f} | "
            f"{bpd:.0f} | {fkb:.2f} | {bpd * fkb / 1e6:.2f} |"
        )
    return daily


def print_hourly_tail(raw):
    print(
        "\n## Hourly 28.09-30.09: base fee and gasUsedForL1>0\n\n"
        "| hour | block | base fee gwei | user tx | l1>0 | median fee µETH |\n|---|---|---|---|---|---|"
    )
    for ts, number, base_fee, fees, l1 in raw.late_blocks:
        print(
            f"| {dt.datetime.fromtimestamp(ts, dt.timezone.utc):%m-%d %H:%M:%S} | {number} | {base_fee / 1e9:.4f} | "
            f"{len(fees)} | {sum(1 for x in l1 if x > 0)} | {median_or_nan(fees) / 1e12:.3f} |"
        )


@dataclasses.dataclass
class Rates:
    first: dict
    last: dict
    last7: list
    r7: float
    rate_lo: float
    rate_hi: float


def print_block_counts(rows, daily):
    first, last = rows[0], rows[-1]
    span_rate = (last["block"] - first["block"]) / (last["ts"] - first["ts"])
    last7 = [r for r in rows if r["ts"] >= last["ts"] - 7 * 86400]
    r7 = (last["block"] - last7[0]["block"]) / (last["ts"] - last7[0]["ts"])
    bpds = [x["bpd"] for x in daily if x["n"] == 24]
    rate_lo, rate_hi = min(bpds) / 86400, max(bpds) / 86400
    print(
        f"\n## Block counts\n\nfirst sample {int(first['block'])} @ {first['hour_start_utc']} "
        f"(+{first['offset_s']:.0f}s); last {int(last['block'])} @ {last['hour_start_utc']} (+{last['offset_s']:.0f}s)"
    )
    print(
        f"mean rate over sample {span_rate:.4f} blk/s; last 7 days {r7:.4f}; daily min/max {rate_lo:.4f}/{rate_hi:.4f}"
    )
    print(
        f"blocks/day over 58 days: min {min(bpds):.0f} p10 {pct(bpds, 10):.0f} p50 {pct(bpds, 50):.0f} "
        f"p90 {pct(bpds, 90):.0f} max {max(bpds):.0f}"
    )
    return Rates(first, last, last7, r7, rate_lo, rate_hi)


def print_forecast(rows, daily, c, bl, rt):
    first, last, last7, r7 = rt.first, rt.last, rt.last7, rt.r7
    hist_kb = wmean(rows, itemgetter("line_z_kb"))
    b_lo, b_hi = bl["line_z_kb"]
    l7_kb = wmean(last7, itemgetter("line_z_kb"))
    l7_days = [x["line_z_kb"] for x in daily if x["date"] >= last7[0]["date"]]
    print(
        f"\nline zstd-3 KB/blk: period mean {hist_kb:.3f} (boot {b_lo:.3f}..{b_hi:.3f}); "
        f"last 7 days {l7_kb:.3f} (daily {min(l7_days):.3f}..{max(l7_days):.3f})"
    )
    print(
        f"raw JSON KB/blk: period mean {wmean(rows, itemgetter('raw_kb')):.1f} "
        f"(boot {bl['raw_kb'][0]:.1f}..{bl['raw_kb'][1]:.1f})"
    )
    print(
        "\n| as of (00:00 UTC) | blocks since 04.08 00:00, M (lo..hi) | file GB: point (lo..hi) | "
        "raw JSON TB: point (lo..hi) | blocks since 04.08 12:00, M |\n|---|---|---|---|---|"
    )
    raw_kb = wmean(rows, itemgetter("raw_kb"))
    raw_lo, raw_hi = bl["raw_kb"]
    l7_raw = wmean(last7, itemgetter("raw_kb"))
    l7_raw_days = [x["raw_kb"] for x in daily if x["date"] >= last7[0]["date"]]
    for label, iso_d in FORECAST_DATES:
        t_end = int(dt.datetime.fromisoformat(iso_d).replace(tzinfo=dt.timezone.utc).timestamp())
        dt_s = t_end - last["ts"]
        past = last["block"] - first["block"]
        fut_pt, fut_lo, fut_hi = r7 * dt_s, rt.rate_lo * dt_s, rt.rate_hi * dt_s
        nb_pt, nb_lo, nb_hi = past + fut_pt, past + fut_lo, past + fut_hi
        gb_pt = (past * hist_kb + fut_pt * l7_kb) * c.mid / 1e6
        gb_lo = (past * b_lo + fut_lo * min(l7_days)) * c.lo / 1e6
        gb_hi = (past * b_hi + fut_hi * max(l7_days)) * c.hi / 1e6
        tb_pt = (past * raw_kb + fut_pt * l7_raw) / 1e9
        tb_lo = (past * raw_lo + fut_lo * min(l7_raw_days)) / 1e9
        tb_hi = (past * raw_hi + fut_hi * max(l7_raw_days)) / 1e9
        print(
            f"| {label} | {nb_pt / 1e6:.2f} ({nb_lo / 1e6:.2f}..{nb_hi / 1e6:.2f}) | "
            f"{gb_pt:.0f} ({gb_lo:.0f}..{gb_hi:.0f}) | "
            f"{tb_pt:.2f} ({tb_lo:.2f}..{tb_hi:.2f}) | {(last['block'] - BLOCK_0408_12 + fut_pt) / 1e6:.2f} |"
        )


def print_stream(daily, c, last7):
    gbd = [x["bpd"] * x["line_z_kb"] for x in daily if x["n"] == 24]
    print("\n## New-block stream, GB/day (daily blocks x daily mean line zstd-3 KB x calibration)\n")
    print("| set | min | p10 | p50 | p90 | max | mean |\n|---|---|---|---|---|---|---|")
    for lab, k in [(f"x{c.lo:.3f}", c.lo), (f"x{c.mid:.3f}", c.mid), (f"x{c.hi:.3f}", c.hi)]:
        v = [g * k / 1e6 for g in gbd]
        print(
            f"| all 58 days {lab} | {min(v):.2f} | {pct(v, 10):.2f} | {pct(v, 50):.2f} | {pct(v, 90):.2f} | "
            f"{max(v):.2f} | {st.mean(v):.2f} |"
        )
    v7 = [x["bpd"] * x["line_z_kb"] * c.mid / 1e6 for x in daily if x["n"] == 24 and x["date"] >= last7[0]["date"]]
    print(f"| last 7 days x{c.mid:.3f} | {min(v7):.2f} | | {pct(v7, 50):.2f} | | {max(v7):.2f} | {st.mean(v7):.2f} |")
    rawd = [x["bpd"] * x["raw_kb"] / 1e6 for x in daily if x["n"] == 24]
    print(
        f"| raw JSON GB/day, all days | {min(rawd):.1f} | {pct(rawd, 10):.1f} | {pct(rawd, 50):.1f} | "
        f"{pct(rawd, 90):.1f} | {max(rawd):.1f} | {st.mean(rawd):.1f} |"
    )


def print_windows(s, c):
    rows = s.rows
    print(
        "\n## 006 windows vs sample distribution "
        "(percentile rank: share of sampled blocks / days / 12:00 blocks below the window mean)\n"
    )
    print(
        "| window | metric | window value | rank among blocks | rank among daily means | "
        "rank among 12:00 UTC blocks |\n|---|---|---|---|---|---|"
    )
    noon = [r for r in rows if int(r["hour"]) == 12]
    dm = {
        k: [wmean(s.by_day[d], itemgetter(k)) for d in s.days]
        for k in ("n_user", "swap_logs", "tx_with_swap", "line_z_kb", "raw_kb")
    }
    dm["rev"] = [ratio_w(s.by_day[d], "reverts", "n_user") for d in s.days]
    for name, _ in WINDOWS_006:
        w = c.win[name]
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
                print(f"| {name} | {lab} | {w[wk]:.3f} | — | {rank(dm[dkey], w[wk]):.0f} | — |")
            else:
                print(
                    f"| {name} | {lab} | {w[wk]:.3f} | {rank(blocks, w[wk]):.0f} | {rank(dm[dkey], w[wk]):.0f} | "
                    f"{rank(nn, w[wk]):.0f} |"
                )
        print(f"| {name} | file KB/blk (real file) | {w['file_kb']:.3f} | | | |")


def main():
    root = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--metrics", default=os.path.join(root, "data/samples/hourly-metrics.tsv"))
    ap.add_argument("--raw", default=os.path.join(root, "data/samples/hourly-20260804-20260930.jsonl.zst"))
    ap.add_argument("--index", default=os.path.join(root, "data/samples/hourly-20260804-20260930.index.tsv"))
    ap.add_argument("--blocks-dir", default=os.path.join(root, "data/blocks"))
    a = ap.parse_args()

    s = load_sample(a.metrics, a.index)
    c = calibrate(a.blocks_dir)
    add_derived(s.rows, c.mid)

    print_coverage(s)
    print_calibration(c)
    print_period_stats(s.rows, c.mid)
    bl = bootstrap(s)
    print_profile(s.rows, lambda r: int(r["hour"]), range(24), "Profile by hour UTC")
    print_profile(s.rows, itemgetter("weekday"), hm.WD, "Profile by weekday")

    raw = read_raw(a.raw)
    daily = print_daily(s, raw)
    print_hourly_tail(raw)
    rt = print_block_counts(s.rows, daily)
    print_forecast(s.rows, daily, c, bl, rt)
    print_stream(daily, c, rt.last7)
    print_windows(s, c)


if __name__ == "__main__":
    main()
