#!/usr/bin/env python3
"""Tests of feed_audit.py on synthetic feed files (stdlib unittest, Python 3.9+).

    python3 -m unittest discover -s .claude/skills/feed-audit/scripts -v

Needs the `zstd` CLI (the end-to-end cases are skipped without it). Every file
is written to a temporary directory; no network, no RPC. Not installed on the
server (bootstrap.sh copies feed_audit.py only).

Synthetic lines follow the recorder format (see feed_audit.py and
.claude/skills/hoodchain-mev/references/data-model.md):
    recv_unix_ns <TAB> seq_first <TAB> seq_last <TAB> raw JSON
"""

from __future__ import annotations

import contextlib
import datetime
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
import warnings

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import feed_audit as fa

HAVE_ZSTD = shutil.which("zstd") is not None
# 2026-10-01 06:00:00 UTC in ns; all synthetic hours are 2026-10-01 06 and 07.
T0 = 1790834400 * 10**9
MS = 10**6


def zstd_frame(data: bytes) -> bytes:
    return subprocess.run(["zstd", "-q", "-c", "--check"], input=data, capture_output=True, check=True).stdout


def envelope(seq, kind=3, l1=26095700, block_hash=None):
    msg = {
        "sequenceNumber": seq,
        "message": {"message": {"header": {"kind": kind, "blockNumber": l1}, "l2Msg": "AA=="}},
        "blockHash": block_hash if block_hash is not None else f"0x{seq:064x}",
    }
    return json.dumps({"version": 1, "messages": [msg]}, separators=(",", ":"))


def block_line(ns, seq, **kw):
    return f"{ns}\t{seq}\t{seq}\t{envelope(seq, **kw)}"


def seq0_line(ns, raw):
    return f"{ns}\t0\t0\t{raw}"


PING = json.dumps({"recorderFrame": {"opcode": "ping", "payloadBase64": ""}})
CONFIRMED = json.dumps({"version": 1, "confirmedSequenceNumberMessage": {"sequenceNumber": 99}})


def day_lines(first_seq=100, n=50, start_ns=T0 + 10 * 10**9):
    """n consecutive blocks 100 ms apart, a ping every 10 blocks, one confirmed message."""
    out = []
    ns = start_ns
    for i in range(n):
        if i % 10 == 0:
            out.append(seq0_line(ns, PING))
        out.append(block_line(ns, first_seq + i))
        ns += 100 * MS
    out.append(seq0_line(ns, CONFIRMED))
    return out


class FeedDir:
    """A temporary recorder --out-dir with hourly feed files."""

    def __init__(self, root):
        self.root = root

    def write_hour(self, hour, lines, frame_every=10, extra=b""):
        """Write `lines` as several zstd frames (`frame_every` lines each), then `extra` raw bytes."""
        path = fa.hour_file(os.path.join(self.root, "x", "x", "x", "x"), hour)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        data = b""
        for i in range(0, len(lines), frame_every):
            data += zstd_frame("".join(ln + "\n" for ln in lines[i : i + frame_every]).encode())
        with open(path, "wb") as f:
            f.write(data + extra)
        return path

    def write(self, name, text):
        with open(os.path.join(self.root, name), "w") as f:
            f.write(text)


def run_audit(*args):
    """(rc, stdout, stderr) of feed_audit.main(args); DeprecationWarning is an error."""
    out, err = io.StringIO(), io.StringIO()
    with warnings.catch_warnings():
        warnings.simplefilter("error", DeprecationWarning)
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = fa.main(list(args))
    return rc, out.getvalue(), err.getvalue()


class PureFunctions(unittest.TestCase):
    def test_merge_ranges(self):
        self.assertEqual(fa.merge_ranges([(5, 7), (1, 3), (4, 4), (10, 12), (11, 20), (9, 8)]), [(1, 7), (10, 20)])
        self.assertEqual(fa.merge_ranges([(1, 2), (4, 5)]), [(1, 2), (4, 5)])
        self.assertEqual(fa.merge_ranges([]), [])

    def test_classify_seq0(self):
        self.assertEqual(fa.classify_seq0(PING), "recorderFrame:ping")
        self.assertEqual(fa.classify_seq0(CONFIRMED), "confirmedSequenceNumberMessage")
        self.assertEqual(fa.classify_seq0('{"recorderFrame":{"opcode":"text","payloadBase64":"!!"}}'), "other")
        self.assertEqual(fa.classify_seq0('{"recorderFrame":{"opcode":"text","payloadBase64":5}}'), "other")
        self.assertEqual(fa.classify_seq0('{"messages":[{"sequenceNumber":1}]}'), "with_messages")
        self.assertEqual(fa.classify_seq0("not json"), "other")
        self.assertEqual(fa.classify_seq0("[1, 2]"), "other")
        self.assertEqual(fa.classify_seq0('{"something":"else"}'), "other")

    def test_parse_messages(self):
        good = json.loads(envelope(7, kind=13, l1=5))
        self.assertEqual(fa.parse_messages(good), [(7, 13, 5, f"0x{7:064x}")])
        self.assertEqual(fa.parse_messages({"version": 1}), [])
        for bad in (
            [1, 2],
            "str",
            {"messages": {"a": 1}},
            {"messages": [{"sequenceNumber": 103, "message": {}}]},
            {"messages": [{"message": {"message": {"header": {"kind": 3, "blockNumber": 1}}}}]},
            {
                "messages": [
                    {"sequenceNumber": "103", "message": {"message": {"header": {"kind": 3, "blockNumber": 1}}}}
                ]
            },
            {"messages": [{"sequenceNumber": True, "message": {"message": {"header": {"kind": 3, "blockNumber": 1}}}}]},
            {"messages": [{"sequenceNumber": 1, "message": {"message": {"header": {"kind": 3}}}}]},
            {"messages": [{"sequenceNumber": 1, "message": {"message": {"header": None}}}]},
            {"messages": ["x"]},
        ):
            self.assertIsNone(fa.parse_messages(bad), bad)

    def test_utc_truncates_to_milliseconds(self):
        self.assertEqual(fa.utc(1790836432708872000), "2026-10-01T06:33:52.708Z")
        # Exact integer split: .999999999 s stays in the same millisecond and second.
        self.assertEqual(fa.utc(T0 + 999_999_999), "2026-10-01T06:00:00.999Z")
        self.assertEqual(fa.utc(T0), "2026-10-01T06:00:00.000Z")

    def test_times_are_aware(self):
        self.assertEqual(fa.hour_start("20261001-06").tzinfo, datetime.timezone.utc)
        self.assertEqual(fa.parse_now("2026-10-01T06:01:30Z").tzinfo, datetime.timezone.utc)
        c = fa.Clock.make(fa.parse_now("2026-10-01T07:01:30Z"), None, 60.0)
        self.assertEqual((c.current_hour, c.prev_hour), ("20261001-07", "20261001-06"))
        self.assertTrue(c.prev_grace)
        self.assertFalse(fa.Clock.make(fa.parse_now("2026-10-01T07:02:01Z"), None, 60.0).prev_grace)
        # The default (real) clock is aware too: comparing it with grace_until must not raise.
        fa.Clock.make(datetime.datetime.now(fa.UTC), None, 60.0)

    def test_pct_nearest_rank(self):
        self.assertEqual(fa.pct([1, 2, 3, 4], 0.5), 3)
        self.assertEqual(fa.pct([1, 2, 3, 4], 0.99), 4)
        self.assertTrue(fa.pct([], 0.5) != fa.pct([], 0.5))  # nan

    def test_is_feed_file(self):
        self.assertTrue(fa.is_feed_file("data/feed/2026/10/01/feed-20261001-06.tsv.zst"))
        self.assertFalse(fa.is_feed_file("data/feed/_torn/feed-20261001-06.tsv.zst"))
        self.assertFalse(fa.is_feed_file("data/feed/2026/10/01/feed-20261001-06.tsv.zst.tmp"))
        self.assertFalse(fa.is_feed_file("data/feed/connections.tsv"))

    def test_frame_len_without_zstd(self):
        """Header parsing that needs no real frame: runs on any machine (zstd CLI not required)."""
        zstd_magic = (0xFD2FB528).to_bytes(4, "little")
        self.assertEqual(fa.frame_len(zstd_magic[:2], 0), (None, "incomplete"))
        self.assertEqual(fa.frame_len(zstd_magic, 0), (None, "incomplete"))
        skippable = (0x184D2A50).to_bytes(4, "little") + (3).to_bytes(4, "little") + b"abc"
        self.assertEqual(fa.frame_len(skippable, 0), (11, None))
        self.assertEqual(fa.frame_len(skippable[:9], 0), (None, "incomplete"))
        self.assertEqual(fa.frame_len(b"xx" + skippable, 2), (11, None))
        self.assertEqual(fa.frame_len(b"garbage!", 0), (None, "invalid"))
        self.assertEqual(fa.split_frames(skippable + skippable), (2, 22, None))
        self.assertEqual(fa.split_frames(skippable + b"garbage!"), (1, 11, "invalid"))

    def test_rpc_pick_is_deterministic_for_a_seed(self):
        # blocks: seq -> (blockHash, header.blockNumber, kind, running max); every 7th is delayed (kind 9)
        blocks = {s: ("0x", 1, 9 if s % 7 == 0 else 3, 1) for s in range(100, 300)}
        a = fa.rpc_pick(blocks, 20, 42)
        self.assertEqual(a, fa.rpc_pick(blocks, 20, 42))
        self.assertEqual(a, sorted(a))
        self.assertEqual(len(a), 20)
        self.assertTrue({100, 299} <= set(a))
        self.assertGreaterEqual(sum(1 for s in a if s % 7 == 0), 5)  # sample // 4 delayed
        self.assertNotEqual(a, fa.rpc_pick(blocks, 20, 43))
        self.assertEqual(fa.rpc_pick(blocks, 500, 1), sorted(blocks))  # sample larger than the record


@unittest.skipUnless(HAVE_ZSTD, "zstd CLI not installed")
class FrameLen(unittest.TestCase):
    """Cases that need a real frame from the zstd CLI (the rest: PureFunctions.test_frame_len_without_zstd)."""

    def test_complete_and_truncated_real_frame(self):
        frame = zstd_frame(b"hello\n" * 100)
        self.assertEqual(fa.frame_len(frame, 0), (len(frame), None))
        self.assertEqual(fa.frame_len(frame[:-1], 0), (None, "incomplete"))
        self.assertEqual(fa.frame_len(frame[:2], 0), (None, "incomplete"))
        self.assertEqual(fa.split_frames(frame + frame), (2, 2 * len(frame), None))
        self.assertEqual(fa.split_frames(frame + frame[:10]), (1, len(frame), "incomplete"))
        self.assertEqual(fa.split_frames(frame + b"xyzw1234"), (1, len(frame), "invalid"))


@unittest.skipUnless(HAVE_ZSTD, "zstd CLI not installed")
class EndToEnd(unittest.TestCase):
    NOW = "2026-10-01T12:00:00Z"  # hours 06 and 07 are closed

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="feed-audit-test-")
        self.feed = FeedDir(self.tmp)
        self.glob = os.path.join(self.tmp, "2026", "10", "01", "feed-*.tsv.zst")

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def audit(self, *extra, now=None):
        rc, out, err = run_audit("--feed-root", self.tmp, "--now", now or self.NOW, "--json", *extra, self.glob)
        summary = json.loads(out) if out.strip() else None
        return rc, summary, err

    def write_normal_day(self):
        lines = day_lines(100, 60)
        self.feed.write_hour("20261001-06", lines[:40])
        self.feed.write_hour("20261001-07", lines[40:])
        self.feed.write("last_seq.txt", "159\n")

    def test_normal_day_pass(self):
        self.write_normal_day()
        rc, s, err = self.audit()
        self.assertEqual((rc, s["verdict"], err), (0, "PASS", ""))
        self.assertEqual((s["files"], s["blocks"], s["expected_blocks"], s["missing_blocks"]), (2, 60, 60, 0))
        self.assertEqual((s["first_seq"], s["last_seq"], s["bad_envelopes"]), (100, 159, 0))
        self.assertEqual(s["seq0_lines"], {"confirmedSequenceNumberMessage": 1, "recorderFrame:ping": 6})
        self.assertEqual(s["kinds"], {"3": 60})
        self.assertEqual((s["sessions"], s["warnings"], s["failures"]), (1, [], []))
        self.assertEqual(s["interarrival_ms"]["p50"], 100.0)
        self.assertEqual(s["session_list"], ["2026-10-01T06:00:10.000Z +6.0s blocks=60"])

    def test_text_output_keys(self):
        """The daily wrapper reads these keys from the text output (deploy/feed-audit-daily.sh)."""
        self.write_normal_day()
        rc, out, _ = run_audit("--feed-root", self.tmp, "--now", self.NOW, self.glob)
        self.assertEqual(rc, 0)
        keys = {ln.split()[0] for ln in out.splitlines() if ln and not ln.startswith(" ")}
        for k in ("blocks", "missing_blocks", "gaps_tsv_entries", "sessions", "blocks_per_s", "mb_per_hour"):
            self.assertIn(k, keys)
        self.assertIn("bad_envelopes      0", out.splitlines())
        self.assertEqual(out.splitlines()[-1], "verdict            PASS")

    def test_default_clock_without_now_has_no_deprecation_warning(self):
        self.write_normal_day()
        rc, out, err = run_audit("--feed-root", self.tmp, self.glob)  # warnings are errors in run_audit
        self.assertEqual((rc, err), (0, ""))
        self.assertTrue(out.endswith("verdict            PASS\n"))

    def test_malformed_envelope_is_counted_not_a_traceback(self):
        lines = day_lines(100, 30)
        # seq 103 replaced by an envelope without message.message.header, plus a JSON array line.
        bad = '{"messages":[{"sequenceNumber":103,"message":{}}]}'
        lines = [ln.split("\t", 3)[0] + "\t103\t103\t" + bad if "\t103\t103\t" in ln else ln for ln in lines]
        lines.append(f"{T0 + 20 * 10**9}\t200\t200\t[1,2]")
        self.feed.write_hour("20261001-06", lines)
        self.feed.write("gaps.tsv", "103\t103\t0\n")
        rc, s, _err = self.audit()
        self.assertEqual((rc, s["verdict"]), (1, "FAIL"))
        self.assertEqual(s["bad_envelopes"], 2)
        self.assertIn(
            "2 envelopes with a malformed message (no integer sequenceNumber / header.kind / header.blockNumber)",
            s["failures"],
        )
        # The rest of the day is still audited: 29 good blocks, the skipped seq is a gap.
        self.assertEqual((s["blocks"], s["gaps"]), (29, [[103, 103]]))
        self.assertEqual(len(s["failures"]), 1)

    def test_only_malformed_envelopes_no_blocks(self):
        self.feed.write_hour("20261001-06", [f"{T0}\t5\t5\t" + '{"messages":[{"sequenceNumber":5}]}'])
        rc, s, err = self.audit()
        self.assertEqual((rc, s), (1, None))
        self.assertIn("FAIL  1 envelopes with a malformed message", err)
        self.assertIn("no blocks found", err)

    def test_gap_not_in_gaps_tsv_fails_merged_rows_pass(self):
        lines = [ln for ln in day_lines(100, 60) if not any(f"\t{s}\t{s}\t" in ln for s in range(120, 125))]
        self.feed.write_hour("20261001-06", lines)
        rc, s, _ = self.audit()
        self.assertEqual((rc, s["gaps"], s["missing_blocks"]), (1, [[120, 124]], 5))
        self.assertIn("gap 120..124 is not listed in gaps.tsv", s["failures"])
        self.feed.write("gaps.tsv", "120\t121\t0\n122\t124\t0\n")  # one hole, two adjacent rows
        rc, s, _ = self.audit()
        self.assertEqual((rc, s["verdict"], s["gaps_tsv_entries"]), (0, "PASS", 2))
        self.feed.write("gaps.tsv", "120\t121\t0\n123\t124\t0\n")  # 122 not covered
        rc, s, _ = self.audit()
        self.assertEqual(rc, 1)

    def test_duplicate_and_backwards(self):
        lines = day_lines(100, 20)
        lines.append(block_line(T0 + 30 * 10**9, 110))  # duplicate
        lines.append(block_line(T0 + 29 * 10**9, 200))  # recv_unix_ns goes back; seq jumps
        self.feed.write_hour("20261001-06", lines)
        self.feed.write("gaps.tsv", "120\t199\t0\n")
        rc, s, _ = self.audit()
        self.assertEqual(rc, 1)
        self.assertIn("1 duplicate sequence numbers", s["failures"])
        self.assertIn("recv_unix_ns went backwards 1 times", s["failures"])

    def test_open_tail_current_hour_is_not_a_fail(self):
        lines = day_lines(100, 40)
        path = self.feed.write_hour("20261001-06", lines)
        with open(path, "ab") as f:
            f.write(zstd_frame(b"partial line\n")[:-3])  # recorder still writing the last frame
        rc, s, _ = self.audit(now="2026-10-01T06:30:00Z")
        self.assertEqual((rc, s["verdict"]), (0, "PASS"))
        self.assertEqual(len(s["open_tail"]), 1)
        self.assertIn("open frame of current hour 20261001-06", s["open_tail"][0])
        # Same file once the hour is closed: torn tail -> FAIL.
        rc, s, _ = self.audit(now="2026-10-01T09:00:00Z")
        self.assertEqual(rc, 1)
        self.assertTrue(any(f.startswith("torn zstd tail in closed hour") for f in s["failures"]))

    def test_previous_hour_tail_grace(self):
        path = self.feed.write_hour("20261001-06", day_lines(100, 40))
        with open(path, "ab") as f:
            f.write(zstd_frame(b"partial line\n")[:-3])
        rc, s, _ = self.audit(now="2026-10-01T07:01:30Z")
        self.assertEqual(rc, 0)
        self.assertIn(
            "previous hour 20261001-06, frame may still be open: audit before 2026-10-01T07:02:00Z", s["open_tail"][0]
        )
        rc, s, _ = self.audit(now="2026-10-01T07:02:01Z")
        self.assertEqual(rc, 1)

    def test_garbage_after_frames_fails(self):
        self.feed.write_hour("20261001-06", day_lines(100, 20), extra=b"not a zstd frame")
        rc, s, _ = self.audit()
        self.assertEqual(rc, 1)
        self.assertIn("garbage after the last complete zstd frame", s["failures"][0])

    def test_seq0_lines(self):
        lines = day_lines(100, 20)
        lines.append(seq0_line(T0 + 20 * 10**9, '{"messages":[{"sequenceNumber":1}]}'))
        lines.append(seq0_line(T0 + 21 * 10**9, '{"unexpected":true}'))
        self.feed.write_hour("20261001-06", lines)
        rc, s, _ = self.audit()
        self.assertEqual(rc, 1)
        self.assertEqual(s["seq0_lines"]["with_messages"], 1)
        self.assertEqual(s["seq0_lines"]["other"], 1)
        self.assertIn("1 seq-0 lines carry messages", s["failures"])
        self.assertIn("1 seq-0 lines are neither recorderFrame nor confirmedSequenceNumberMessage", s["warnings"])

    def test_sessions_from_connections_and_pauses(self):
        lines = day_lines(100, 20) + day_lines(120, 20, start_ns=T0 + 100 * 10**9)
        self.feed.write_hour("20261001-06", lines)
        rc, s, _ = self.audit()
        self.assertEqual((rc, s["sessions"]), (0, 2))  # pause > 30 s
        self.feed.write("connections.tsv", f"# hdr\nx\t{T0}\tconnected\nx\t{T0 + 11 * 10**9}\tconnected\n")
        rc, s, _ = self.audit()
        self.assertEqual((rc, s["sessions"]), (0, 3))
        self.assertTrue(s["session_source"].startswith("connections.tsv (2 connected)"))

    def test_last_seq_txt(self):
        self.write_normal_day()
        self.feed.write("last_seq.txt", "150\n")
        rc, s, _ = self.audit()
        self.assertEqual(rc, 0)
        self.assertIn("last_seq.txt=150, last recorded seq=159 (ok only if other files follow)", s["warnings"])
        self.feed.write("last_seq.txt", "garbage\n")
        rc, s, _ = self.audit()
        self.assertEqual(rc, 0)
        self.assertIn("last_seq.txt is not an integer: 'garbage'", s["warnings"])

    def test_deeply_nested_json_is_bad_json_not_a_zstd_failure(self):
        deep = "[" * 200_000 + "]" * 200_000  # json.loads raises RecursionError
        lines = day_lines(100, 30)
        lines = [ln.split("\t", 3)[0] + "\t105\t105\t" + deep if "\t105\t105\t" in ln else ln for ln in lines]
        lines.append(seq0_line(T0 + 20 * 10**9, deep))
        self.feed.write_hour("20261001-06", lines)
        self.feed.write("gaps.tsv", "105\t105\t0\n")
        rc, s, _ = self.audit()
        self.assertEqual(rc, 1)
        self.assertEqual(s["failures"], ["1 lines with broken JSON"])
        self.assertEqual(s["warnings"], ["1 seq-0 lines are neither recorderFrame nor confirmedSequenceNumberMessage"])
        self.assertEqual((s["blocks"], s["gaps"]), (29, [[105, 105]]))  # the rest of the file is still audited

    def test_unexpected_exception_mid_file_exits_promptly(self):
        """An unexpected exception while lines are being read must not leave the
        process waiting on zstd's pipes (the file is far larger than a pipe buffer)."""
        self.feed.write_hour("20261001-06", day_lines(100, 20_000), frame_every=2_000)
        code = (
            "import sys; sys.path.insert(0, sys.argv[1]); import feed_audit as fa\n"
            "n = [0]\n"
            "orig = fa.Scanner.scan_line\n"
            "def boom(self, bline):\n"
            "    n[0] += 1\n"
            "    if n[0] == 100:\n"
            "        raise MemoryError('synthetic')\n"
            "    orig(self, bline)\n"
            "fa.Scanner.scan_line = boom\n"
            "fa.main(['--now', '2026-10-01T12:00:00Z', sys.argv[2]])\n"
        )
        script_dir = os.path.dirname(os.path.abspath(__file__))
        try:
            p = subprocess.run(
                [sys.executable, "-c", code, script_dir, self.glob],
                capture_output=True,
                text=True,
                timeout=60,
                check=False,
            )
        except subprocess.TimeoutExpired:
            self.fail("feed_audit.py hung after an exception mid-file")
        self.assertNotEqual(p.returncode, 0)
        self.assertIn("MemoryError: synthetic", p.stderr)

    def test_rpc_seed(self):
        """--seed: absent from the default output; printed when given; repeats the same RPC sample."""
        self.write_normal_day()
        _, s, _ = self.audit()
        self.assertEqual(s["rpc"], {})
        rc, out, _ = run_audit("--feed-root", self.tmp, "--now", self.NOW, self.glob)
        self.assertIn("rpc                {}", out.splitlines())
        _, s, _ = self.audit("--seed", "7")
        self.assertEqual(s["rpc"], {"seed": 7})

        calls = []

        def fake_rpc_blocks(url, numbers):
            calls.append(list(numbers))
            return {n: {"hash": f"0x{n:064x}", "l1BlockNumber": hex(26095700)} for n in numbers}

        real = fa.rpc_blocks
        fa.rpc_blocks = fake_rpc_blocks
        try:
            r1 = self.audit("--rpc-sample", "8", "--seed", "42", "--rpc-url", "http://127.0.0.1:9")
            r2 = self.audit("--rpc-sample", "8", "--seed", "42", "--rpc-url", "http://127.0.0.1:9")
            r3 = self.audit("--rpc-sample", "8", "--rpc-url", "http://127.0.0.1:9")
        finally:
            fa.rpc_blocks = real
        self.assertEqual(calls[0], calls[1])
        self.assertEqual(len(calls[0]), 8)
        for rc, s, _ in (r1, r2):
            self.assertEqual((rc, s["verdict"]), (0, "PASS"))
            self.assertEqual(s["rpc"], {"seed": 42, "sampled": 8, "hash_mismatch": [], "l1_mismatch": []})
        self.assertIsInstance(r3[1]["rpc"]["seed"], int)  # time_ns default, printed for a rerun

    def test_usage_errors(self):
        self.write_normal_day()
        rc, _, err = run_audit("--now", "yesterday", self.glob)
        self.assertEqual(rc, 2)
        self.assertIn("--now must look like", err)
        rc, _, err = run_audit(os.path.join(self.tmp, "missing-*.tsv.zst"))
        self.assertEqual(rc, 2)
        rc, _, err = run_audit(os.path.join(self.tmp, "last_seq.txt"))
        self.assertEqual(rc, 2)
        self.assertIn("no feed-*.tsv.zst files", err)


if __name__ == "__main__":
    unittest.main()
