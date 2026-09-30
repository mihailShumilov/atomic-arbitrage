#!/usr/bin/env python3
"""Audit recorded sequencer-feed files (data/feed/**/feed-*.tsv.zst).

Stdlib only (Python 3.9+); decompression via the `zstd` CLI, which handles
multi-frame files. Streams line by line, so a full day of feed is fine.

Checks:
  - every file decompresses completely (`zstd -dc` rc == 0);
  - 4 TSV columns; seq_first/seq_last columns agree with the JSON;
  - sequence numbers strictly +1 in file order, no duplicates;
  - every missing range is listed in <feed-root>/gaps.tsv;
  - recv_unix_ns non-decreasing; last_seq.txt == last recorded seq;
  - optional: --rpc-sample N blocks compared with eth_getBlockByNumber
    (blockHash; l1BlockNumber vs feed header.blockNumber, see chain-facts.md:
    equal for kind 3, running max of header.blockNumber for delayed kinds).

Prints numbers (blocks, blocks/s, MB/h, inter-arrival percentiles, kinds)
and a PASS/FAIL verdict. Exit code: 0 PASS, 1 FAIL, 2 usage error.

Usage:
  feed_audit.py data/feed/2026/09/30/feed-20260930-11.tsv.zst
  feed_audit.py --feed-root data/feed --rpc-sample 20 data/feed/2026/09/30/*.tsv.zst
"""

import argparse
import collections
import glob
import json
import os
import random
import subprocess
import sys
import urllib.request

PUBLIC_RPC_URL = "https://rpc.mainnet.chain.robinhood.com"


def pct(sorted_vals, q):
    if not sorted_vals:
        return float("nan")
    return sorted_vals[min(len(sorted_vals) - 1, int(round(q * (len(sorted_vals) - 1))))]


def read_gaps(path):
    gaps = []
    if os.path.exists(path):
        with open(path) as f:
            for ln in f:
                p = ln.split("\t")
                if len(p) >= 2 and p[0].strip().isdigit():
                    gaps.append((int(p[0]), int(p[1])))
    return gaps


def rpc_blocks(url, numbers):
    calls = [
        {"jsonrpc": "2.0", "id": n, "method": "eth_getBlockByNumber", "params": [hex(n), False]}
        for n in numbers
    ]
    req = urllib.request.Request(
        url, data=json.dumps(calls).encode(), headers={"content-type": "application/json", "user-agent": "hoodchain-feed-audit/1"}
    )
    with urllib.request.urlopen(req, timeout=30) as r:
        resp = json.load(r)
    out = {}
    for item in resp:
        if "error" in item or not item.get("result"):
            raise RuntimeError("rpc error for block %s: %s" % (item.get("id"), item.get("error")))
        out[item["id"]] = item["result"]
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("files", nargs="+", help="feed-*.tsv.zst files or globs")
    ap.add_argument("--feed-root", help="recorder --out-dir (for gaps.tsv and last_seq.txt)")
    ap.add_argument("--rpc-sample", type=int, default=0, help="blocks to compare with RPC (0 = none)")
    ap.add_argument("--rpc-url", default=os.environ.get("RPC_URL", PUBLIC_RPC_URL))
    ap.add_argument("--json", action="store_true", help="print the summary as JSON")
    a = ap.parse_args()

    files = sorted({f for pat in a.files for f in (glob.glob(pat) or [pat])})
    missing_files = [f for f in files if not os.path.exists(f)]
    if missing_files:
        print("no such file: %s" % ", ".join(missing_files), file=sys.stderr)
        return 2

    fails = []
    warns = []
    envelopes = 0
    msgs_per_env = collections.Counter()
    kinds = collections.Counter()
    seq0_lines = 0
    col_errors = 0
    bad_json = 0
    col_mismatch = 0
    dups = 0
    gaps = []
    first_seq = last_seq = None
    first_ns = last_ns = None
    ns_backwards = 0
    interarrival = []
    raw_bytes = 0
    zst_bytes = 0
    # seq -> (blockHash, header.blockNumber, kind, running max header.blockNumber)
    blocks = {}
    l1_max = None

    for path in files:
        zst_bytes += os.path.getsize(path)
        proc = subprocess.Popen(["zstd", "-dc", path], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        for bline in proc.stdout:
            raw_bytes += len(bline)
            line = bline.decode("utf-8", "replace").rstrip("\n")
            parts = line.split("\t", 3)
            if len(parts) != 4:
                col_errors += 1
                continue
            ns, sf, sl = int(parts[0]), int(parts[1]), int(parts[2])
            envelopes += 1
            if last_ns is not None:
                if ns < last_ns:
                    ns_backwards += 1
                interarrival.append((ns - last_ns) / 1e6)
            if first_ns is None:
                first_ns = ns
            last_ns = ns
            if sf == 0:
                seq0_lines += 1  # stored unparseable envelope
                continue
            try:
                env = json.loads(parts[3])
            except ValueError:
                bad_json += 1  # typically a cut-off last line of an unfinished frame
                continue
            msgs = env.get("messages") or []
            msgs_per_env[len(msgs)] += 1
            seqs = [m["sequenceNumber"] for m in msgs]
            if not seqs or seqs[0] != sf or seqs[-1] != sl:
                col_mismatch += 1
            for m in msgs:
                s = m["sequenceNumber"]
                hdr = m["message"]["message"]["header"]
                kinds[hdr["kind"]] += 1
                if s in blocks:
                    dups += 1
                    continue
                if last_seq is not None and s != last_seq + 1:
                    if s > last_seq + 1:
                        gaps.append((last_seq + 1, s - 1))
                    else:
                        fails.append("seq went backwards: %d after %d" % (s, last_seq))
                l1 = hdr["blockNumber"]
                l1_max = l1 if l1_max is None else max(l1_max, l1)
                blocks[s] = (m.get("blockHash"), l1, hdr["kind"], l1_max)
                if first_seq is None:
                    first_seq = s
                last_seq = s
        proc.stdout.close()
        err = proc.stderr.read().decode().strip()
        if proc.wait() != 0:
            fails.append("zstd -dc failed on %s (incomplete frame?): %s" % (path, err))

    if not blocks:
        print("no blocks found", file=sys.stderr)
        return 1

    if col_errors:
        fails.append("%d lines without 4 TSV columns" % col_errors)
    if bad_json:
        fails.append("%d lines with broken JSON" % bad_json)
    if col_mismatch:
        fails.append("%d envelopes where seq columns disagree with JSON" % col_mismatch)
    if dups:
        fails.append("%d duplicate sequence numbers" % dups)
    if ns_backwards:
        fails.append("recv_unix_ns went backwards %d times" % ns_backwards)
    if seq0_lines:
        warns.append("%d unparseable envelopes stored with seq 0" % seq0_lines)
    no_hash = sum(1 for v in blocks.values() if not v[0])
    if no_hash:
        warns.append("%d blocks without blockHash" % no_hash)

    gaps_file = []
    if a.feed_root:
        gaps_file = read_gaps(os.path.join(a.feed_root, "gaps.tsv"))
        for g in gaps:
            if not any(f <= g[0] and g[1] <= t for f, t in gaps_file):
                fails.append("gap %d..%d is not listed in gaps.tsv" % g)
        ls_path = os.path.join(a.feed_root, "last_seq.txt")
        if os.path.exists(ls_path):
            ls = int(open(ls_path).read().strip())
            if ls != last_seq:
                warns.append("last_seq.txt=%d, last recorded seq=%d (ok only if other files follow)" % (ls, last_seq))
    elif gaps:
        warns.append("gaps found; pass --feed-root to check them against gaps.tsv")

    rpc = {}
    if a.rpc_sample > 0:
        seqs = sorted(blocks)
        pick = {seqs[0], seqs[-1]}
        delayed = [s for s in seqs if blocks[s][2] != 3]
        pick.update(random.sample(delayed, min(len(delayed), max(1, a.rpc_sample // 4))))
        rest = [s for s in seqs if s not in pick]
        pick.update(random.sample(rest, min(len(rest), max(0, a.rpc_sample - len(pick)))))
        pick = sorted(pick)
        try:
            res = rpc_blocks(a.rpc_url, pick)
        except Exception as e:  # noqa: BLE001 - report, don't crash the audit
            fails.append("rpc check failed: %s" % e)
            res = None
        hash_bad, l1_bad = [], []
        for s in pick if res else []:
            b = res[s]
            fh, l1, kind, run_max = blocks[s]
            if b["hash"] != fh:
                hash_bad.append(s)
            rl1 = int(b["l1BlockNumber"], 16)
            # kind 3: header.blockNumber == l1BlockNumber. Delayed kinds carry the
            # inbox L1 block; RPC reports the running max (verified 2026-09-30).
            expected = l1 if kind == 3 else run_max
            if rl1 != expected and not (kind != 3 and s == seqs[0]):
                l1_bad.append((s, kind, l1, run_max, rl1))
        rpc = {"sampled": len(pick) if res else 0, "hash_mismatch": hash_bad, "l1_mismatch": l1_bad}
        if hash_bad:
            fails.append("blockHash differs from RPC for %s" % hash_bad)
        if l1_bad:
            warns.append("l1BlockNumber model mismatch (seq, kind, header, running max, rpc): %s" % l1_bad)

    interarrival.sort()
    span_s = (last_ns - first_ns) / 1e9 if last_ns and first_ns else 0.0
    n_blocks = len(blocks)
    summary = {
        "files": len(files),
        "envelopes": envelopes,
        "msgs_per_envelope": dict(msgs_per_env),
        "first_seq": first_seq,
        "last_seq": last_seq,
        "blocks": n_blocks,
        "expected_blocks": last_seq - first_seq + 1,
        "missing_blocks": sum(t - f + 1 for f, t in gaps),
        "gaps": gaps,
        "gaps_tsv_entries": len(gaps_file),
        "kinds": dict(kinds),
        "recv_span_s": round(span_s, 1),
        "blocks_per_s": round(n_blocks / span_s, 3) if span_s else None,
        "zst_mb": round(zst_bytes / 1e6, 2),
        "raw_mb": round(raw_bytes / 1e6, 1),
        "mb_per_hour": round(zst_bytes / 1e6 / span_s * 3600, 1) if span_s else None,
        "interarrival_ms": {
            "p50": round(pct(interarrival, 0.5), 1),
            "p99": round(pct(interarrival, 0.99), 1),
            "max": round(interarrival[-1], 1) if interarrival else None,
        },
        "rpc": rpc,
        "warnings": warns,
        "failures": fails,
        "verdict": "FAIL" if fails else "PASS",
    }

    if a.json:
        print(json.dumps(summary, indent=2))
    else:
        for k, v in summary.items():
            if k in ("warnings", "failures", "verdict"):
                continue
            print("%-18s %s" % (k, v))
        for w in warns:
            print("WARN  " + w)
        for f in fails:
            print("FAIL  " + f)
        print("verdict            " + summary["verdict"])
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
