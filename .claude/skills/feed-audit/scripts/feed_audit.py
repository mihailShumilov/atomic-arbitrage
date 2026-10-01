#!/usr/bin/env python3
"""Audit recorded sequencer-feed files (data/feed/**/feed-*.tsv.zst).

Stdlib only (Python 3.9+); decompression via the `zstd` CLI. Streams line by
line, so a full day of feed is fine.

Checks:
  - zstd integrity per frame: every complete frame decompresses (checksum);
    bytes after the last complete frame are allowed only as the open frame of
    the current UTC hour (<= 1 frame, recorder still writing), or of the
    previous hour while the recorder may not have closed it yet (audit runs
    before HH:00 + --frame-secs + 60 s and the current hour's file has no
    complete frame). A torn tail in a closed hour is a FAIL;
  - 4 TSV columns; seq_first/seq_last columns agree with the JSON;
  - seq-0 lines are classified: recorderFrame:<opcode>,
    confirmedSequenceNumberMessage, other (only "other" is a WARN);
  - sequence numbers strictly +1 in file order, no duplicates;
  - every missing range is covered by <feed-root>/gaps.tsv (adjacent and
    overlapping rows are merged first);
  - recv_unix_ns non-decreasing; last_seq.txt == last recorded seq;
  - optional: --rpc-sample N blocks compared with eth_getBlockByNumber
    (blockHash; l1BlockNumber vs feed header.blockNumber, see chain-facts.md:
    equal for kind 3, running max of header.blockNumber for delayed kinds).

Rates (blocks/s, MB/h) are computed over session time: the recording is cut
into sessions at `connected` events of <feed-root>/connections.tsv (if it
exists) and wherever two neighbouring lines are more than --session-gap-s
apart; a session lasts from its first to its last line. The plain first-to-
last span is printed separately (recv_span_s, *_span).

Ignored inputs: anything under `_torn/`, `connections.tsv`, `*.tmp`, and
names that are not feed-*.tsv.zst.

Exit code: 0 PASS, 1 FAIL, 2 usage error.

Usage:
  feed_audit.py data/feed/2026/09/30/feed-20260930-11.tsv.zst
  feed_audit.py --feed-root data/feed --rpc-sample 20 'data/feed/2026/09/30/*.tsv.zst'
"""

import argparse
import base64
import binascii
import bisect
import collections
import datetime
import fnmatch
import glob
import json
import os
import random
import subprocess
import sys
import threading
import urllib.request

PUBLIC_RPC_URL = "https://rpc.mainnet.chain.robinhood.com"
ZSTD_MAGIC = 0xFD2FB528


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


def merge_ranges(ranges):
    """Merge adjacent and overlapping (from, to) ranges (inclusive ends).

    Remark Р1 of the 008 audit: the recorder's start-up reconciliation may
    list one hole as several adjacent rows; a hole is covered if the merged
    ranges cover it."""
    out = []
    for f, t in sorted(r for r in ranges if r[0] <= r[1]):
        if out and f <= out[-1][1] + 1:
            out[-1] = (out[-1][0], max(out[-1][1], t))
        else:
            out.append((f, t))
    return out


def hour_start(hour):
    """datetime (UTC, naive) of 'YYYYMMDD-HH'."""
    return datetime.datetime.strptime(hour, "%Y%m%d-%H")


def hour_file(path, hour):
    """Path of the hourly file for `hour` next to `path` (same feed root)."""
    root = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(path)))))
    return os.path.join(root, hour[0:4], hour[4:6], hour[6:8], "feed-%s.tsv.zst" % hour)


def has_complete_frame(path):
    try:
        with open(path, "rb") as fh:
            data = fh.read()
    except OSError:
        return False
    return split_frames(data)[0] > 0


def read_connected(path):
    """Unix-ns timestamps of `connected` events in connections.tsv (sorted)."""
    out = []
    if not os.path.exists(path):
        return None
    with open(path, errors="replace") as f:
        for ln in f:
            c = ln.rstrip("\n").split("\t")
            if len(c) >= 3 and c[2] == "connected" and c[1].isdigit():
                out.append(int(c[1]))
    return sorted(out)


def frame_len(buf, pos):
    """Length of the zstd (or skippable) frame starting at `pos`.

    Returns (n, None) for a structurally complete frame, (None, "incomplete")
    if the data ends inside the frame, (None, "invalid") for bad magic or a
    reserved field. Content and checksum are verified later by `zstd -dc`.
    """
    n = len(buf)
    if pos + 4 > n:
        return None, "incomplete" if buf[pos:] == ZSTD_MAGIC.to_bytes(4, "little")[: n - pos] else "invalid"
    magic = int.from_bytes(buf[pos:pos + 4], "little")
    if 0x184D2A50 <= magic <= 0x184D2A5F:  # skippable frame
        if pos + 8 > n:
            return None, "incomplete"
        end = pos + 8 + int.from_bytes(buf[pos + 4:pos + 8], "little")
        return (end - pos, None) if end <= n else (None, "incomplete")
    if magic != ZSTD_MAGIC:
        return None, "invalid"
    p = pos + 4
    if p >= n:
        return None, "incomplete"
    fhd = buf[p]
    p += 1
    if (fhd >> 3) & 1:
        return None, "invalid"
    fcs_flag, single, checksum, did = fhd >> 6, (fhd >> 5) & 1, (fhd >> 2) & 1, fhd & 3
    p += 0 if single else 1
    p += (0, 1, 2, 4)[did]
    p += (1 if single else 0, 2, 4, 8)[fcs_flag]
    while True:
        if p + 3 > n:
            return None, "incomplete"
        h = buf[p] | (buf[p + 1] << 8) | (buf[p + 2] << 16)
        p += 3
        last, btype, bsize = h & 1, (h >> 1) & 3, h >> 3
        if btype == 3:
            return None, "invalid"
        p += 1 if btype == 1 else bsize
        if p > n:
            return None, "incomplete"
        if last:
            break
    if checksum:
        p += 4
    if p > n:
        return None, "incomplete"
    return p - pos, None


def split_frames(buf):
    """(frames, complete_end, tail_state): tail_state is None (no tail),
    "incomplete" (one unfinished frame) or "invalid" (garbage)."""
    pos, frames = 0, 0
    while pos < len(buf):
        n, state = frame_len(buf, pos)
        if n is None:
            return frames, pos, state
        pos += n
        frames += 1
    return frames, pos, None


def hour_of(path):
    """'YYYYMMDD-HH' from feed-YYYYMMDD-HH.tsv.zst, else None."""
    name = os.path.basename(path)
    if name.startswith("feed-") and name.endswith(".tsv.zst"):
        return name[len("feed-"):-len(".tsv.zst")]
    return None


def is_feed_file(path):
    parts = os.path.normpath(path).split(os.sep)
    name = parts[-1]
    if "_torn" in parts[:-1] or name.endswith(".tmp") or name == "connections.tsv":
        return False
    return fnmatch.fnmatch(name, "feed-*.tsv.zst")


def decompress_lines(data):
    """Yield decompressed lines of `data` (complete frames only) and finally
    (rc, stderr) via the returned holder."""
    proc = subprocess.Popen(["zstd", "-dc"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    err = []

    def feed():
        try:
            proc.stdin.write(data)
        except BrokenPipeError:
            pass
        finally:
            try:
                proc.stdin.close()
            except BrokenPipeError:
                pass

    def drain_err():
        err.append(proc.stderr.read())

    t, te = threading.Thread(target=feed), threading.Thread(target=drain_err)
    t.start()
    te.start()
    for bline in proc.stdout:
        yield bline
    proc.stdout.close()
    t.join()
    te.join()
    rc = proc.wait()
    if rc != 0:
        raise RuntimeError((err[0] if err else b"").decode(errors="replace").strip() or "rc=%d" % rc)


def classify_seq0(raw):
    """Category of a seq-0 line, or 'with_messages' if it wrongly has messages."""
    try:
        obj = json.loads(raw)
    except ValueError:
        return "other"
    if not isinstance(obj, dict):
        return "other"
    if obj.get("messages"):
        return "with_messages"
    rf = obj.get("recorderFrame")
    if isinstance(rf, dict):
        try:
            base64.b64decode(rf.get("payloadBase64", ""), validate=True)
        except (binascii.Error, TypeError, ValueError):
            return "other"
        return "recorderFrame:%s" % rf.get("opcode")
    if "confirmedSequenceNumberMessage" in obj:
        return "confirmedSequenceNumberMessage"
    return "other"


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


def utc(ns):
    return datetime.datetime.utcfromtimestamp(ns / 1e9).strftime("%Y-%m-%dT%H:%M:%S.%fZ")[:-4] + "Z"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("files", nargs="+", help="feed-*.tsv.zst files or globs")
    ap.add_argument("--feed-root", help="recorder --out-dir (for gaps.tsv, last_seq.txt, connections.tsv)")
    ap.add_argument("--rpc-sample", type=int, default=0, help="blocks to compare with RPC (0 = none)")
    ap.add_argument("--rpc-url", default=os.environ.get("RPC_URL", PUBLIC_RPC_URL))
    ap.add_argument("--session-gap-s", type=float, default=30.0,
                    help="a pause between neighbouring lines longer than this starts a new session (default 30)")
    ap.add_argument("--current-hour", default=None,
                    help="YYYYMMDD-HH treated as the open hour (default: hour of --now)")
    ap.add_argument("--now", default=None,
                    help="audit time, YYYY-MM-DDTHH:MM:SSZ (default: real UTC time); for tests and old records")
    ap.add_argument("--frame-secs", type=float, default=60.0,
                    help="recorder --frame-secs (default 60): bounds how long the previous hour's frame may stay open")
    ap.add_argument("--json", action="store_true", help="print the summary as JSON")
    a = ap.parse_args()

    expanded = sorted({f for pat in a.files for f in (glob.glob(pat) or [pat])})
    missing_files = [f for f in expanded if not os.path.exists(f)]
    if missing_files:
        print("no such file: %s" % ", ".join(missing_files), file=sys.stderr)
        return 2
    files = [f for f in expanded if os.path.isfile(f) and is_feed_file(f)]
    ignored = [f for f in expanded if f not in files]
    if not files:
        print("no feed-*.tsv.zst files among the inputs", file=sys.stderr)
        return 2
    if a.now:
        try:
            now = datetime.datetime.strptime(a.now, "%Y-%m-%dT%H:%M:%SZ")
        except ValueError:
            print("--now must look like 2026-10-01T12:00:30Z", file=sys.stderr)
            return 2
    else:
        now = datetime.datetime.utcnow()
    current_hour = a.current_hour or now.strftime("%Y%m%d-%H")
    prev_hour = (hour_start(current_hour) - datetime.timedelta(hours=1)).strftime("%Y%m%d-%H")
    # Remark Р3 of the 008 audit: the recorder closes the previous hour's last
    # frame at the first line of the new hour or, in silence, at its frame
    # deadline (<= frame_secs). Until then that tail is an open frame.
    grace_until = hour_start(current_hour) + datetime.timedelta(seconds=a.frame_secs + 60)
    prev_grace = now < grace_until

    fails = []
    warns = []
    lines = 0
    envelopes = 0
    msgs_per_env = collections.Counter()
    kinds = collections.Counter()
    seq0 = collections.Counter()
    col_errors = 0
    bad_json = 0
    col_mismatch = 0
    dups = 0
    gaps = []
    first_seq = last_seq = None
    first_ns = last_ns = None
    ns_backwards = 0
    raw_bytes = 0
    zst_bytes = 0
    frames_total = 0
    open_tails = []
    prev_open_tails = []
    # seq -> (blockHash, header.blockNumber, kind, running max header.blockNumber)
    blocks = {}
    l1_max = None

    connected = read_connected(os.path.join(a.feed_root, "connections.tsv")) if a.feed_root else None
    gap_ns = int(a.session_gap_s * 1e9)
    # sessions: list of dicts; current session key
    sessions = []
    cur = None
    prev_env_ns = None  # last sequenced line in the current session
    interarrival = []

    for path in files:
        zst_bytes += os.path.getsize(path)
        with open(path, "rb") as fh:
            data = fh.read()
        frames, end, tail = split_frames(data)
        frames_total += frames
        tail_bytes = len(data) - end
        if tail_bytes:
            if tail == "incomplete" and hour_of(path) == current_hour:
                open_tails.append((path, tail_bytes))
            elif (tail == "incomplete" and hour_of(path) == prev_hour and prev_grace
                  and not has_complete_frame(hour_file(path, current_hour))):
                prev_open_tails.append((path, tail_bytes))
            elif tail == "incomplete":
                fails.append("torn zstd tail in closed hour: %s (%d bytes after %d complete frames)" % (path, tail_bytes, frames))
            else:
                fails.append("garbage after the last complete zstd frame: %s (%d bytes)" % (path, tail_bytes))
        if end == 0:
            continue
        try:
            for bline in decompress_lines(data[:end]):
                raw_bytes += len(bline)
                line = bline.decode("utf-8", "replace").rstrip("\n")
                parts = line.split("\t", 3)
                try:
                    if len(parts) != 4:
                        raise ValueError
                    ns, sf, sl = int(parts[0]), int(parts[1]), int(parts[2])
                except ValueError:
                    col_errors += 1
                    continue
                lines += 1
                if last_ns is not None and ns < last_ns:
                    ns_backwards += 1
                # session bookkeeping (all lines, pings included)
                conn_idx = bisect.bisect_right(connected, ns) - 1 if connected else -1
                new_session = (
                    cur is None
                    or conn_idx != cur["conn"]
                    or (last_ns is not None and ns - last_ns > gap_ns)
                )
                if new_session:
                    cur = {"conn": conn_idx, "first": ns, "last": ns, "blocks": 0}
                    sessions.append(cur)
                    prev_env_ns = None
                cur["last"] = max(cur["last"], ns)
                if first_ns is None:
                    first_ns = ns
                last_ns = ns

                if sf == 0 and sl == 0:
                    cat = classify_seq0(parts[3])
                    seq0[cat] += 1
                    continue
                if sf == 0 or sl == 0:
                    col_mismatch += 1
                try:
                    env = json.loads(parts[3])
                except ValueError:
                    bad_json += 1
                    continue
                envelopes += 1
                if prev_env_ns is not None:
                    interarrival.append((ns - prev_env_ns) / 1e6)
                prev_env_ns = ns
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
                    cur["blocks"] += 1
                    if first_seq is None:
                        first_seq = s
                    last_seq = s
        except RuntimeError as e:
            fails.append("zstd -dc failed on complete frames of %s (corrupt frame?): %s" % (path, e))

    if not blocks:
        for p, b in open_tails:
            print("open frame of current hour %s: %s, %d bytes (not audited)" % (current_hour, p, b), file=sys.stderr)
        for p, b in prev_open_tails:
            print("possibly open frame of previous hour %s: %s, %d bytes (not audited)" % (prev_hour, p, b), file=sys.stderr)
        for f in fails:
            print("FAIL  " + f, file=sys.stderr)
        print("no blocks found", file=sys.stderr)
        return 1

    if col_errors:
        fails.append("%d lines without 4 TSV columns / integer seq columns" % col_errors)
    if bad_json:
        fails.append("%d lines with broken JSON" % bad_json)
    if col_mismatch:
        fails.append("%d envelopes where seq columns disagree with JSON" % col_mismatch)
    if dups:
        fails.append("%d duplicate sequence numbers" % dups)
    if ns_backwards:
        fails.append("recv_unix_ns went backwards %d times" % ns_backwards)
    if seq0.get("with_messages"):
        fails.append("%d seq-0 lines carry messages" % seq0["with_messages"])
    if seq0.get("other"):
        warns.append("%d seq-0 lines are neither recorderFrame nor confirmedSequenceNumberMessage" % seq0["other"])
    no_hash = sum(1 for v in blocks.values() if not v[0])
    if no_hash:
        warns.append("%d blocks without blockHash" % no_hash)

    gaps_file = []
    if a.feed_root:
        gaps_file = read_gaps(os.path.join(a.feed_root, "gaps.tsv"))
        merged = merge_ranges(gaps_file)
        for g in gaps:
            if not any(f <= g[0] and g[1] <= t for f, t in merged):
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
    session_s = sum((s["last"] - s["first"]) / 1e9 for s in sessions)
    n_blocks = len(blocks)
    source = "gap>%gs" % a.session_gap_s
    if connected is not None:
        source = "connections.tsv (%d connected) + %s" % (len(connected), source)
    summary = {
        "files": len(files),
        "ignored_inputs": len(ignored),
        "zstd_frames": frames_total,
        "open_tail": ["%s: %d bytes (open frame of current hour %s, not audited)" % (p, b, current_hour) for p, b in open_tails]
        + ["%s: %d bytes (previous hour %s, frame may still be open: audit before %sZ and no complete frame in hour %s yet; not audited)"
           % (p, b, prev_hour, grace_until.strftime("%Y-%m-%dT%H:%M:%S"), current_hour) for p, b in prev_open_tails],
        "lines": lines,
        "envelopes": envelopes,
        "seq0_lines": dict(sorted(seq0.items())),
        "msgs_per_envelope": dict(msgs_per_env),
        "first_seq": first_seq,
        "last_seq": last_seq,
        "blocks": n_blocks,
        "expected_blocks": last_seq - first_seq + 1,
        "missing_blocks": sum(t - f + 1 for f, t in gaps),
        "gaps": gaps,
        "gaps_tsv_entries": len(gaps_file),
        "kinds": dict(kinds),
        "sessions": len(sessions),
        "session_source": source,
        "session_list": [
            "%s +%.1fs blocks=%d" % (utc(s["first"]), (s["last"] - s["first"]) / 1e9, s["blocks"]) for s in sessions
        ],
        "session_s": round(session_s, 1),
        "blocks_per_s": round(n_blocks / session_s, 3) if session_s else None,
        "zst_mb": round(zst_bytes / 1e6, 2),
        "raw_mb": round(raw_bytes / 1e6, 1),
        "mb_per_hour": round(zst_bytes / 1e6 / session_s * 3600, 1) if session_s else None,
        "recv_span_s": round(span_s, 1),
        "blocks_per_s_span": round(n_blocks / span_s, 3) if span_s else None,
        "mb_per_hour_span": round(zst_bytes / 1e6 / span_s * 3600, 1) if span_s else None,
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
            if isinstance(v, list) and k in ("session_list", "open_tail"):
                print("%-18s %s" % (k, "" if v else "[]"))
                for item in v:
                    print("  " + item)
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
