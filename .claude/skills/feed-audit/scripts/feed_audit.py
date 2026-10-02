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
  - every message of an envelope has an integer sequenceNumber and
    message.message.header.{kind, blockNumber}; a malformed envelope is
    counted (bad_envelopes) and is a FAIL, the audit goes on;
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

from __future__ import annotations

import argparse
import base64
import binascii
import bisect
import collections
import contextlib
import dataclasses
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
# datetime.UTC exists only since 3.11; timezone.utc works on 3.9 too.
UTC = datetime.timezone.utc


def pct(sorted_vals, q):
    """Nearest-rank percentile of an already sorted list (nan if empty)."""
    if not sorted_vals:
        return float("nan")
    return sorted_vals[min(len(sorted_vals) - 1, round(q * (len(sorted_vals) - 1)))]


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

    Remark R1 of the 008 audit: the recorder's start-up reconciliation may
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
    """Timezone-aware UTC datetime of 'YYYYMMDD-HH'."""
    return datetime.datetime.strptime(hour, "%Y%m%d-%H").replace(tzinfo=UTC)


def parse_now(text):
    """Timezone-aware UTC datetime of 'YYYY-MM-DDTHH:MM:SSZ' (ValueError if malformed)."""
    return datetime.datetime.strptime(text, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=UTC)


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


# A byte-level walk over the zstd frame layout (RFC 8878, 3.1.1): one return per
# way the data can end or be malformed reads better than nested helpers.
def frame_len(buf, pos):  # noqa: PLR0911, PLR0912
    """Length of the zstd (or skippable) frame starting at `pos`.

    Returns (n, None) for a structurally complete frame, (None, "incomplete")
    if the data ends inside the frame, (None, "invalid") for bad magic or a
    reserved field. Content and checksum are verified later by `zstd -dc`.
    """
    n = len(buf)
    if pos + 4 > n:
        return None, "incomplete" if buf[pos:] == ZSTD_MAGIC.to_bytes(4, "little")[: n - pos] else "invalid"
    magic = int.from_bytes(buf[pos : pos + 4], "little")
    if 0x184D2A50 <= magic <= 0x184D2A5F:  # skippable frame
        if pos + 8 > n:
            return None, "incomplete"
        end = pos + 8 + int.from_bytes(buf[pos + 4 : pos + 8], "little")
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
        return name[len("feed-") : -len(".tsv.zst")]
    return None


def is_feed_file(path):
    parts = os.path.normpath(path).split(os.sep)
    name = parts[-1]
    if "_torn" in parts[:-1] or name.endswith(".tmp") or name == "connections.tsv":
        return False
    return fnmatch.fnmatch(name, "feed-*.tsv.zst")


class ZstdError(RuntimeError):
    """`zstd -dc` exited non-zero (corrupt frame); the message is zstd's stderr."""


# Deeply nested JSON makes json.loads raise RecursionError (a RuntimeError):
# it is a broken line, not a zstd failure.
JSON_ERRORS = (ValueError, RecursionError)


def decompress_lines(data):
    """Yield decompressed lines of `data` (complete frames only); raise
    ZstdError with zstd's stderr if it exits non-zero.

    If the consumer stops early (exception, generator closed), zstd is killed
    and reaped; the helper threads are daemons, so an unexpected exception can
    never leave the interpreter waiting on a blocked pipe at exit."""
    proc = subprocess.Popen(["zstd", "-dc"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    err = []

    def feed():
        try:
            with contextlib.suppress(BrokenPipeError):
                proc.stdin.write(data)
        finally:
            with contextlib.suppress(BrokenPipeError):
                proc.stdin.close()

    def drain_err():
        with proc.stderr:
            err.append(proc.stderr.read())

    threads = [threading.Thread(target=feed, daemon=True), threading.Thread(target=drain_err, daemon=True)]
    for th in threads:
        th.start()
    finished = False
    try:
        yield from proc.stdout
        finished = True
    finally:
        if not finished:
            proc.kill()
        proc.stdout.close()
        for th in threads:
            th.join()
        rc = proc.wait()
    if rc != 0:
        raise ZstdError((err[0] if err else b"").decode(errors="replace").strip() or "rc=%d" % rc)


def _is_base64(s):
    try:
        base64.b64decode(s, validate=True)
    except (binascii.Error, TypeError, ValueError):
        return False
    return True


def classify_seq0(raw):
    """Category of a seq-0 line, or 'with_messages' if it wrongly has messages."""
    try:
        obj = json.loads(raw)
    except JSON_ERRORS:
        obj = None
    if not isinstance(obj, dict):
        return "other"
    if obj.get("messages"):
        return "with_messages"
    rf = obj.get("recorderFrame")
    if isinstance(rf, dict):
        return "recorderFrame:%s" % rf.get("opcode") if _is_base64(rf.get("payloadBase64", "")) else "other"
    return "confirmedSequenceNumberMessage" if "confirmedSequenceNumberMessage" in obj else "other"


def _is_int(v):
    return isinstance(v, int) and not isinstance(v, bool)


def parse_messages(env):
    """[(seq, kind, l1_block, blockHash)] of a sequenced envelope, or None if
    the envelope is malformed (not an object, `messages` not a list, a
    message without an integer sequenceNumber / header.kind / header.blockNumber).
    An envelope without messages gives [] (counted as a seq-column mismatch)."""
    if not isinstance(env, dict):
        return None
    msgs = env.get("messages") or []
    if not isinstance(msgs, list):
        return None
    out = []
    try:
        for m in msgs:
            s = m["sequenceNumber"]
            hdr = m["message"]["message"]["header"]
            kind, l1 = hdr["kind"], hdr["blockNumber"]
            if not (_is_int(s) and _is_int(kind) and _is_int(l1)):
                return None
            out.append((s, kind, l1, m.get("blockHash")))
    except (KeyError, TypeError, AttributeError):
        return None
    return out


def rpc_blocks(url, numbers):
    calls = [{"jsonrpc": "2.0", "id": n, "method": "eth_getBlockByNumber", "params": [hex(n), False]} for n in numbers]
    req = urllib.request.Request(
        url,
        data=json.dumps(calls).encode(),
        headers={"content-type": "application/json", "user-agent": "hoodchain-feed-audit/1"},
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
    """'YYYY-MM-DDTHH:MM:SS.mmmZ' of a unix-ns time (milliseconds truncated)."""
    secs, rem = divmod(ns, 1_000_000_000)
    return datetime.datetime.fromtimestamp(secs, UTC).strftime("%Y-%m-%dT%H:%M:%S") + ".%03dZ" % (rem // 1_000_000)


@dataclasses.dataclass(frozen=True)
class Clock:
    """Which hours count as open, derived from --now / --current-hour / --frame-secs."""

    current_hour: str
    prev_hour: str
    # Remark R3 of the 008 audit: the recorder closes the previous hour's last
    # frame at the first line of the new hour or, in silence, at its frame
    # deadline (<= frame_secs). Until grace_until that tail is an open frame.
    grace_until: datetime.datetime
    prev_grace: bool

    @classmethod
    def make(cls, now, current_hour, frame_secs):
        current_hour = current_hour or now.strftime("%Y%m%d-%H")
        start = hour_start(current_hour)
        grace_until = start + datetime.timedelta(seconds=frame_secs + 60)
        prev_hour = (start - datetime.timedelta(hours=1)).strftime("%Y%m%d-%H")
        return cls(current_hour, prev_hour, grace_until, now < grace_until)


@dataclasses.dataclass
class Session:
    conn: int
    first: int
    last: int
    blocks: int = 0


@dataclasses.dataclass
class Tally:
    """Everything the scan accumulates over all input files."""

    lines: int = 0
    envelopes: int = 0
    col_errors: int = 0
    bad_json: int = 0
    bad_envelopes: int = 0
    col_mismatch: int = 0
    dups: int = 0
    ns_backwards: int = 0
    raw_bytes: int = 0
    zst_bytes: int = 0
    frames: int = 0
    seq0: collections.Counter = dataclasses.field(default_factory=collections.Counter)
    kinds: collections.Counter = dataclasses.field(default_factory=collections.Counter)
    msgs_per_env: collections.Counter = dataclasses.field(default_factory=collections.Counter)
    gaps: list = dataclasses.field(default_factory=list)
    # seq -> (blockHash, header.blockNumber, kind, running max header.blockNumber)
    blocks: dict = dataclasses.field(default_factory=dict)
    first_seq: int | None = None
    last_seq: int | None = None
    first_ns: int | None = None
    last_ns: int | None = None
    l1_max: int | None = None
    open_tails: list = dataclasses.field(default_factory=list)
    prev_open_tails: list = dataclasses.field(default_factory=list)
    sessions: list = dataclasses.field(default_factory=list)
    cur: Session | None = None
    prev_env_ns: int | None = None  # last sequenced line in the current session
    interarrival: list = dataclasses.field(default_factory=list)
    fails: list = dataclasses.field(default_factory=list)
    warns: list = dataclasses.field(default_factory=list)


class Scanner:
    """Feeds lines of the input files into a Tally (sessions, seq order, blocks)."""

    def __init__(self, clock, connected, session_gap_s):
        self.clock = clock
        self.connected = connected
        self.session_gap_s = session_gap_s
        self.gap_ns = int(session_gap_s * 1e9)
        self.t = Tally()

    def scan_file(self, path):
        t = self.t
        t.zst_bytes += os.path.getsize(path)
        with open(path, "rb") as fh:
            data = fh.read()
        frames, end, tail = split_frames(data)
        t.frames += frames
        self.check_tail(path, len(data) - end, frames, tail)
        if end == 0:
            return
        try:
            with contextlib.closing(decompress_lines(data[:end])) as lines:
                for bline in lines:
                    self.scan_line(bline)
        except ZstdError as e:
            t.fails.append("zstd -dc failed on complete frames of %s (corrupt frame?): %s" % (path, e))

    def check_tail(self, path, tail_bytes, frames, tail):
        if not tail_bytes:
            return
        t, c = self.t, self.clock
        if tail == "incomplete" and hour_of(path) == c.current_hour:
            t.open_tails.append((path, tail_bytes))
        elif (
            tail == "incomplete"
            and hour_of(path) == c.prev_hour
            and c.prev_grace
            and not has_complete_frame(hour_file(path, c.current_hour))
        ):
            t.prev_open_tails.append((path, tail_bytes))
        elif tail == "incomplete":
            t.fails.append(
                "torn zstd tail in closed hour: %s (%d bytes after %d complete frames)" % (path, tail_bytes, frames)
            )
        else:
            t.fails.append("garbage after the last complete zstd frame: %s (%d bytes)" % (path, tail_bytes))

    def scan_line(self, bline):
        t = self.t
        t.raw_bytes += len(bline)
        line = bline.decode("utf-8", "replace").rstrip("\n")
        parts = line.split("\t", 3)
        try:
            if len(parts) != 4:
                raise ValueError
            ns, sf, sl = int(parts[0]), int(parts[1]), int(parts[2])
        except ValueError:
            t.col_errors += 1
            return
        t.lines += 1
        if t.last_ns is not None and ns < t.last_ns:
            t.ns_backwards += 1
        self.track_session(ns)
        if t.first_ns is None:
            t.first_ns = ns
        t.last_ns = ns

        if sf == 0 and sl == 0:
            t.seq0[classify_seq0(parts[3])] += 1
            return
        if sf == 0 or sl == 0:
            t.col_mismatch += 1
        try:
            env = json.loads(parts[3])
        except JSON_ERRORS:
            t.bad_json += 1
            return
        msgs = parse_messages(env)
        if msgs is None:
            t.bad_envelopes += 1
            return
        self.scan_envelope(ns, sf, sl, msgs)

    def track_session(self, ns):
        """Session bookkeeping over all lines, pings included."""
        t = self.t
        conn_idx = bisect.bisect_right(self.connected, ns) - 1 if self.connected else -1
        if t.cur is None or conn_idx != t.cur.conn or (t.last_ns is not None and ns - t.last_ns > self.gap_ns):
            t.cur = Session(conn_idx, ns, ns)
            t.sessions.append(t.cur)
            t.prev_env_ns = None
        t.cur.last = max(t.cur.last, ns)

    def scan_envelope(self, ns, sf, sl, msgs):
        t = self.t
        t.envelopes += 1
        if t.prev_env_ns is not None:
            t.interarrival.append((ns - t.prev_env_ns) / 1e6)
        t.prev_env_ns = ns
        t.msgs_per_env[len(msgs)] += 1
        if not msgs or msgs[0][0] != sf or msgs[-1][0] != sl:
            t.col_mismatch += 1
        for s, kind, l1, block_hash in msgs:
            t.kinds[kind] += 1
            if s in t.blocks:
                t.dups += 1
                continue
            if t.last_seq is not None and s != t.last_seq + 1:
                if s > t.last_seq + 1:
                    t.gaps.append((t.last_seq + 1, s - 1))
                else:
                    t.fails.append("seq went backwards: %d after %d" % (s, t.last_seq))
            t.l1_max = l1 if t.l1_max is None else max(t.l1_max, l1)
            t.blocks[s] = (block_hash, l1, kind, t.l1_max)
            t.cur.blocks += 1
            if t.first_seq is None:
                t.first_seq = s
            t.last_seq = s


def check_counts(t):
    """FAIL/WARN from the counters of the scan."""
    if t.col_errors:
        t.fails.append("%d lines without 4 TSV columns / integer seq columns" % t.col_errors)
    if t.bad_json:
        t.fails.append("%d lines with broken JSON" % t.bad_json)
    if t.bad_envelopes:
        t.fails.append(
            "%d envelopes with a malformed message (no integer sequenceNumber / header.kind / header.blockNumber)"
            % t.bad_envelopes
        )
    if t.col_mismatch:
        t.fails.append("%d envelopes where seq columns disagree with JSON" % t.col_mismatch)
    if t.dups:
        t.fails.append("%d duplicate sequence numbers" % t.dups)
    if t.ns_backwards:
        t.fails.append("recv_unix_ns went backwards %d times" % t.ns_backwards)
    if t.seq0.get("with_messages"):
        t.fails.append("%d seq-0 lines carry messages" % t.seq0["with_messages"])
    if t.seq0.get("other"):
        t.warns.append("%d seq-0 lines are neither recorderFrame nor confirmedSequenceNumberMessage" % t.seq0["other"])


def check_feed_root(t, feed_root):
    """Gaps vs gaps.tsv and last_seq.txt; returns the gaps.tsv rows."""
    if not feed_root:
        if t.gaps:
            t.warns.append("gaps found; pass --feed-root to check them against gaps.tsv")
        return []
    gaps_file = read_gaps(os.path.join(feed_root, "gaps.tsv"))
    merged = merge_ranges(gaps_file)
    for g in t.gaps:
        if not any(f <= g[0] and g[1] <= e for f, e in merged):
            t.fails.append("gap %d..%d is not listed in gaps.tsv" % g)
    ls_path = os.path.join(feed_root, "last_seq.txt")
    if os.path.exists(ls_path):
        with open(ls_path) as f:
            text = f.read().strip()
        try:
            ls = int(text)
        except ValueError:
            t.warns.append("last_seq.txt is not an integer: %r" % text[:40])
        else:
            if ls != t.last_seq:
                t.warns.append(
                    "last_seq.txt=%d, last recorded seq=%d (ok only if other files follow)" % (ls, t.last_seq)
                )
    return gaps_file


def check_rpc(t, url, sample):
    """Compare a sample of blocks with eth_getBlockByNumber (one batch call)."""
    blocks = t.blocks
    seqs = sorted(blocks)
    pick = {seqs[0], seqs[-1]}
    delayed = [s for s in seqs if blocks[s][2] != 3]
    pick.update(random.sample(delayed, min(len(delayed), max(1, sample // 4))))
    rest = [s for s in seqs if s not in pick]
    pick.update(random.sample(rest, min(len(rest), max(0, sample - len(pick)))))
    pick = sorted(pick)
    try:
        res = rpc_blocks(url, pick)
    except Exception as e:  # report, don't crash the audit
        t.fails.append("rpc check failed: %s" % e)
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
    if hash_bad:
        t.fails.append("blockHash differs from RPC for %s" % hash_bad)
    if l1_bad:
        t.warns.append("l1BlockNumber model mismatch (seq, kind, header, running max, rpc): %s" % l1_bad)
    return {"sampled": len(pick) if res else 0, "hash_mismatch": hash_bad, "l1_mismatch": l1_bad}


def build_summary(scanner, n_files, n_ignored, gaps_file, rpc):
    t, clock, connected = scanner.t, scanner.clock, scanner.connected
    t.interarrival.sort()
    span_s = (t.last_ns - t.first_ns) / 1e9 if t.last_ns and t.first_ns else 0.0
    session_s = sum((s.last - s.first) / 1e9 for s in t.sessions)
    n_blocks = len(t.blocks)
    zst_bytes = t.zst_bytes
    source = "gap>%gs" % scanner.session_gap_s
    if connected is not None:
        source = "connections.tsv (%d connected) + %s" % (len(connected), source)
    grace = clock.grace_until.strftime("%Y-%m-%dT%H:%M:%S")
    return {
        "files": n_files,
        "ignored_inputs": n_ignored,
        "zstd_frames": t.frames,
        "open_tail": [
            "%s: %d bytes (open frame of current hour %s, not audited)" % (p, b, clock.current_hour)
            for p, b in t.open_tails
        ]
        + [
            "%s: %d bytes (previous hour %s, frame may still be open: audit before %sZ" % (p, b, clock.prev_hour, grace)
            + " and no complete frame in hour %s yet; not audited)" % clock.current_hour
            for p, b in t.prev_open_tails
        ],
        "lines": t.lines,
        "envelopes": t.envelopes,
        "bad_envelopes": t.bad_envelopes,
        "seq0_lines": dict(sorted(t.seq0.items())),
        "msgs_per_envelope": dict(t.msgs_per_env),
        "first_seq": t.first_seq,
        "last_seq": t.last_seq,
        "blocks": n_blocks,
        "expected_blocks": t.last_seq - t.first_seq + 1,
        "missing_blocks": sum(e - f + 1 for f, e in t.gaps),
        "gaps": t.gaps,
        "gaps_tsv_entries": len(gaps_file),
        "kinds": dict(t.kinds),
        "sessions": len(t.sessions),
        "session_source": source,
        "session_list": [
            "%s +%.1fs blocks=%d" % (utc(s.first), (s.last - s.first) / 1e9, s.blocks) for s in t.sessions
        ],
        "session_s": round(session_s, 1),
        "blocks_per_s": round(n_blocks / session_s, 3) if session_s else None,
        "zst_mb": round(zst_bytes / 1e6, 2),
        "raw_mb": round(t.raw_bytes / 1e6, 1),
        "mb_per_hour": round(zst_bytes / 1e6 / session_s * 3600, 1) if session_s else None,
        "recv_span_s": round(span_s, 1),
        "blocks_per_s_span": round(n_blocks / span_s, 3) if span_s else None,
        "mb_per_hour_span": round(zst_bytes / 1e6 / span_s * 3600, 1) if span_s else None,
        "interarrival_ms": {
            "p50": round(pct(t.interarrival, 0.5), 1),
            "p99": round(pct(t.interarrival, 0.99), 1),
            "max": round(t.interarrival[-1], 1) if t.interarrival else None,
        },
        "rpc": rpc,
        "warnings": t.warns,
        "failures": t.fails,
        "verdict": "FAIL" if t.fails else "PASS",
    }


def print_text(summary):
    for k, v in summary.items():
        if k in ("warnings", "failures", "verdict"):
            continue
        if isinstance(v, list) and k in ("session_list", "open_tail"):
            print("%-18s %s" % (k, "" if v else "[]"))
            for item in v:
                print("  " + item)
            continue
        print("%-18s %s" % (k, v))
    for w in summary["warnings"]:
        print("WARN  " + w)
    for f in summary["failures"]:
        print("FAIL  " + f)
    print("verdict            " + summary["verdict"])


def print_no_blocks(t, clock):
    for p, b in t.open_tails:
        print("open frame of current hour %s: %s, %d bytes (not audited)" % (clock.current_hour, p, b), file=sys.stderr)
    for p, b in t.prev_open_tails:
        print(
            "possibly open frame of previous hour %s: %s, %d bytes (not audited)" % (clock.prev_hour, p, b),
            file=sys.stderr,
        )
    for f in t.fails:
        print("FAIL  " + f, file=sys.stderr)
    print("no blocks found", file=sys.stderr)


def parse_args(argv):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("files", nargs="+", help="feed-*.tsv.zst files or globs")
    ap.add_argument("--feed-root", help="recorder --out-dir (for gaps.tsv, last_seq.txt, connections.tsv)")
    ap.add_argument("--rpc-sample", type=int, default=0, help="blocks to compare with RPC (0 = none)")
    ap.add_argument("--rpc-url", default=os.environ.get("RPC_URL", PUBLIC_RPC_URL))
    ap.add_argument(
        "--session-gap-s",
        type=float,
        default=30.0,
        help="a pause between neighbouring lines longer than this starts a new session (default 30)",
    )
    ap.add_argument(
        "--current-hour", default=None, help="YYYYMMDD-HH treated as the open hour (default: hour of --now)"
    )
    ap.add_argument(
        "--now",
        default=None,
        help="audit time, YYYY-MM-DDTHH:MM:SSZ (default: real UTC time); for tests and old records",
    )
    ap.add_argument(
        "--frame-secs",
        type=float,
        default=60.0,
        help="recorder --frame-secs (default 60): bounds how long the previous hour's frame may stay open",
    )
    ap.add_argument("--json", action="store_true", help="print the summary as JSON")
    return ap.parse_args(argv)


def resolve_inputs(patterns):
    """(files, ignored) after glob expansion, or None after printing a usage error."""
    expanded = sorted({f for pat in patterns for f in (glob.glob(pat) or [pat])})
    missing_files = [f for f in expanded if not os.path.exists(f)]
    if missing_files:
        print("no such file: %s" % ", ".join(missing_files), file=sys.stderr)
        return None
    files = [f for f in expanded if os.path.isfile(f) and is_feed_file(f)]
    if not files:
        print("no feed-*.tsv.zst files among the inputs", file=sys.stderr)
        return None
    return files, [f for f in expanded if f not in files]


def main(argv=None):
    a = parse_args(argv)
    inputs = resolve_inputs(a.files)
    if inputs is None:
        return 2
    files, ignored = inputs
    try:
        now = parse_now(a.now) if a.now else datetime.datetime.now(UTC)
    except ValueError:
        print("--now must look like 2026-10-01T12:00:30Z", file=sys.stderr)
        return 2
    clock = Clock.make(now, a.current_hour, a.frame_secs)
    connected = read_connected(os.path.join(a.feed_root, "connections.tsv")) if a.feed_root else None

    scanner = Scanner(clock, connected, a.session_gap_s)
    for path in files:
        scanner.scan_file(path)
    t = scanner.t
    check_counts(t)
    if not t.blocks:
        print_no_blocks(t, clock)
        return 1
    no_hash = sum(1 for v in t.blocks.values() if not v[0])
    if no_hash:
        t.warns.append("%d blocks without blockHash" % no_hash)
    gaps_file = check_feed_root(t, a.feed_root)
    rpc = check_rpc(t, a.rpc_url, a.rpc_sample) if a.rpc_sample > 0 else {}

    summary = build_summary(scanner, len(files), len(ignored), gaps_file, rpc)
    if a.json:
        print(json.dumps(summary, indent=2))
    else:
        print_text(summary)
    return 1 if t.fails else 0


if __name__ == "__main__":
    sys.exit(main())
