#!/usr/bin/env python3
"""Hourly block sample from the public RPC (task 010).

For every hour start T in [--start, --end] (UTC) find a block whose timestamp is
in [T, T + --window) and save eth_getBlockByNumber(n, true) + eth_getBlockReceipts(n)
as one line in the same format as enricher's blocks-*.jsonl.zst:
    {"block":{...},"number":N,"receipts":[...]}   (compact JSON, sorted keys)

Block search: interpolation from the previous accepted hour (local block rate),
then secant refinement around the miss. A fetched full block that lands inside
the window is the sample itself (phase "block"); a miss is phase "search".

Hard limits (refuse to send a request that could exceed them):
  --max-calls   all JSON-RPC calls of all runs together (ledger is persistent)
  --max-search  calls spent on misses
  --min-interval seconds between request starts (0.55 s -> < 2 calls/s)
429: first one -> sleep Retry-After (60 s if absent) and retry; any further 429
in the ledger -> stop. 403 -> stop. Three transport errors in a row -> stop.

Phase 1 rules: read-only RPC, no keys, no signing, no feed connections.
Stdlib only; compression via the `zstd` CLI.
"""

import argparse
import datetime as dt
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

PUBLIC_HOST = "rpc.mainnet.chain.robinhood.com"
UA = "hoodchain-mev-hourly-sample/0.1"
# Anchor from task 006 (chain-facts.md): block 27607293 at 2026-08-04 12:00:01 UTC,
# mean rate 04.08 -> 30.09 = 9.928 blocks/s.
ANCHOR_N, ANCHOR_TS, ANCHOR_RATE = 27607293, 1785844801, 9.928

LEDGER_HDR = "unix_s\tphase\tmethod\tparam\thttp_status\tok\tdetail\n"
INDEX_HDR = "hour_start_utc\thour_unix\tblock\tts\toffset_s\ttries\thash\tn_tx\n"


def read_env_rpc(root):
    url = os.environ.get("RPC_URL")
    path = os.path.join(root, ".env")
    if os.path.exists(path):
        for line in open(path):
            line = line.strip()
            if line.startswith("RPC_URL="):
                url = line.split("=", 1)[1].strip().strip('"').strip("'")
    if not url:
        sys.exit("RPC_URL not set in .env")
    host = urllib.parse.urlparse(url).hostname
    if host != PUBLIC_HOST:
        sys.exit(f"refusing: RPC_URL host {host!r} is not the public RPC {PUBLIC_HOST}")
    return url


class Stop(Exception):
    pass


class Rpc:
    def __init__(self, url, ledger_path, max_calls, max_search, min_interval):
        self.url, self.ledger_path = url, ledger_path
        self.max_calls, self.max_search, self.min_interval = max_calls, max_search, min_interval
        self.total = self.search = self.n429 = 0
        self.by_phase = {}
        new = not os.path.exists(ledger_path)
        if not new:
            with open(ledger_path) as f:
                next(f)
                for line in f:
                    p = line.rstrip("\n").split("\t")
                    self.total += 1
                    self.by_phase[p[1]] = self.by_phase.get(p[1], 0) + 1
                    if p[1] == "search":
                        self.search += 1
                    if p[4] == "429":
                        self.n429 += 1
        self.ledger = open(ledger_path, "a")
        if new:
            self.ledger.write(LEDGER_HDR)
            self.ledger.flush()
        self.last = 0.0
        self.consec_err = 0

    def log(self, phase, method, param, status, ok, detail=""):
        self.total += 1
        self.by_phase[phase] = self.by_phase.get(phase, 0) + 1
        if phase == "search":
            self.search += 1
        self.ledger.write(f"{time.time():.3f}\t{phase}\t{method}\t{param}\t{status}\t{int(ok)}\t{detail}\n")
        self.ledger.flush()
        os.fsync(self.ledger.fileno())

    def call(self, method, params, phase, may_be_search):
        """Send one JSON-RPC call. `phase` is the ledger phase for a successful call.
        `may_be_search`: the caller may later decide this call was a miss; we then need
        search budget for it, so it is checked up front."""
        while True:
            if self.total + 1 > self.max_calls:
                raise Stop(f"total budget {self.max_calls} reached")
            if may_be_search and self.search + 1 > self.max_search:
                raise Stop(f"search budget {self.max_search} reached")
            wait = self.last + self.min_interval - time.time()
            if wait > 0:
                time.sleep(wait)
            self.last = time.time()
            body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
            req = urllib.request.Request(
                self.url,
                data=body,
                headers={"Content-Type": "application/json", "User-Agent": UA, "Accept-Encoding": "identity"},
            )
            param = params[0]
            try:
                with urllib.request.urlopen(req, timeout=60) as r:
                    status = r.status
                    raw = r.read()
            except urllib.error.HTTPError as e:
                status = e.code
                if status == 429:
                    self.n429 += 1
                    ra = e.headers.get("Retry-After")
                    self.log("error", method, param, 429, False, f"retry_after={ra}")
                    if self.n429 >= 2:
                        raise Stop(f"repeated HTTP 429 (count {self.n429}), Retry-After={ra}")
                    pause = float(ra) if ra and ra.replace(".", "", 1).isdigit() else 60.0
                    print(f"HTTP 429, sleeping {pause}s (Retry-After={ra})", flush=True)
                    time.sleep(pause)
                    continue
                self.log("error", method, param, status, False, e.reason)
                if status == 403:
                    raise Stop(f"HTTP 403 {e.reason}")
                self._transport_err(f"HTTP {status}")
                continue
            except Exception as e:  # timeout, connection reset
                self.log("error", method, param, 0, False, repr(e)[:120])
                self._transport_err(repr(e))
                continue
            self.consec_err = 0
            try:
                j = json.loads(raw)
            except ValueError:
                self.log("error", method, param, status, False, "bad json")
                raise Stop("non-JSON response")
            if "error" in j or j.get("result") is None:
                self.log("error", method, param, status, False, json.dumps(j.get("error"))[:120])
                raise Stop(f"rpc error / null result for {method} {param}: {j.get('error')}")
            return j["result"], param, status

    def _transport_err(self, what):
        self.consec_err += 1
        print(f"transport error {self.consec_err}: {what}", flush=True)
        if self.consec_err >= 3:
            raise Stop(f"3 transport errors in a row: {what}")
        time.sleep(5)


def iso(ts):
    return dt.datetime.fromtimestamp(ts, dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def main():
    root = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--start", default="2026-08-04T00:00:00")
    ap.add_argument("--end", default="2026-09-30T23:00:00", help="last hour start, inclusive")
    ap.add_argument("--out", default=os.path.join(root, "data/samples/hourly-20260804-20260930.jsonl.zst"))
    ap.add_argument("--max-calls", type=int, default=3100)
    ap.add_argument("--max-search", type=int, default=300)
    ap.add_argument("--min-interval", type=float, default=0.55)
    ap.add_argument("--window", type=int, default=60, help="accept ts in [T, T+window)")
    ap.add_argument("--aim", type=int, default=15, help="aim at T+aim seconds")
    ap.add_argument("--max-tries", type=int, default=4, help="fetches per hour before giving up")
    ap.add_argument("--stop-after-hours", type=int, default=0, help="smoke test: stop after N new hours")
    a = ap.parse_args()

    out = a.out
    base = out[: -len(".jsonl.zst")] if out.endswith(".jsonl.zst") else out
    partial = base + ".jsonl.partial"
    index_path = base + ".index.tsv"
    ledger_path = base + ".calls.tsv"
    os.makedirs(os.path.dirname(out), exist_ok=True)
    if os.path.exists(out):
        sys.exit(f"{out} already exists; nothing to do")

    rpc = Rpc(read_env_rpc(root), ledger_path, a.max_calls, a.max_search, a.min_interval)

    t_start = int(dt.datetime.fromisoformat(a.start).replace(tzinfo=dt.timezone.utc).timestamp())
    t_end = int(dt.datetime.fromisoformat(a.end).replace(tzinfo=dt.timezone.utc).timestamp())
    hours = list(range(t_start, t_end + 1, 3600))

    done = {}
    if os.path.exists(index_path):
        with open(index_path) as f:
            next(f)
            for line in f:
                p = line.rstrip("\n").split("\t")
                # gap rows have an empty block column; they are not retried
                done[int(p[1])] = (int(p[2]), int(p[3])) if p[2] else (-1, -1)
    else:
        with open(index_path, "w") as f:
            f.write(INDEX_HDR)
    n_lines = 0
    if os.path.exists(partial):
        with open(partial, "rb") as f:
            n_lines = sum(1 for _ in f)
    n_ok = sum(1 for v in done.values() if v[0] >= 0)
    if n_lines != n_ok:
        sys.exit(f"partial has {n_lines} lines but index has {n_ok} accepted hours; fix by hand")

    # predictor state: last accepted point and local rate
    prev = (ANCHOR_N, ANCHOR_TS)
    rate = ANCHOR_RATE
    accepted = sorted((h, v) for h, v in done.items() if v[0] >= 0)
    if len(accepted) >= 2:
        (_, (n1, t1)), (_, (n2, t2)) = accepted[-2], accepted[-1]
        prev, rate = (n2, t2), (n2 - n1) / max(1, t2 - t1)
    elif accepted:
        prev = accepted[-1][1]

    pf = open(partial, "ab")
    idx = open(index_path, "a")
    stop_reason = None
    new_hours = 0
    try:
        for T in hours:
            if T in done:
                continue
            if a.stop_after_hours and new_hours >= a.stop_after_hours:
                raise Stop(f"--stop-after-hours {a.stop_after_hours}")
            new_hours += 1
            aim = T + a.aim
            n = prev[0] + round(rate * (aim - prev[1]))
            hit = None
            tries = 0
            seen = set()
            while tries < a.max_tries:
                tries += 1
                if n in seen:
                    n += 5
                seen.add(n)
                blk, param, status = rpc.call("eth_getBlockByNumber", [hex(n), True], "block", True)
                ts = int(blk["timestamp"], 16)
                if T <= ts < T + a.window:
                    rpc.log("block", "eth_getBlockByNumber", param, status, True, f"hit ts={ts}")
                    hit = (n, ts, blk)
                    break
                rpc.log("search", "eth_getBlockByNumber", param, status, True, f"miss ts={ts} off={ts - T}")
                # secant step with the local rate; blocks share 1-s timestamps
                step = round(rate * (aim - ts))
                if step == 0:
                    step = 1 if ts < T else -1
                n += step
            if hit is None:
                idx.write(f"{iso(T)}\t{T}\t\t\t\t{tries}\t\t\n")
                idx.flush()
                print(f"{iso(T)} GAP after {tries} tries", flush=True)
                continue
            n, ts, blk = hit
            rc, param, status = rpc.call("eth_getBlockReceipts", [hex(n)], "receipts", False)
            txs = blk["transactions"]
            bad = None
            if len(rc) != len(txs):
                bad = f"{len(rc)} receipts for {len(txs)} txs"
            elif any(
                r.get("blockHash") != blk["hash"] or r.get("transactionHash") != t["hash"] for r, t in zip(rc, txs)
            ):
                bad = "receipt blockHash/txHash mismatch"
            rpc.log("receipts", "eth_getBlockReceipts", param, status, bad is None, bad or f"n={len(rc)}")
            if bad:
                raise Stop(f"block {n}: {bad}")
            line = json.dumps(
                {"number": n, "block": blk, "receipts": rc}, sort_keys=True, separators=(",", ":"), ensure_ascii=False
            ).encode()
            pf.write(line + b"\n")
            pf.flush()
            os.fsync(pf.fileno())
            idx.write(f"{iso(T)}\t{T}\t{n}\t{ts}\t{ts - T}\t{tries}\t{blk['hash']}\t{len(txs)}\n")
            idx.flush()
            if ts - prev[1] > 0:
                r = (n - prev[0]) / (ts - prev[1])
                if 3.0 < r < 15.0:
                    rate = r
            prev = (n, ts)
            print(
                f"{iso(T)} block {n} ts+{ts - T}s tries={tries} txs={len(txs)} calls={rpc.total} search={rpc.search}",
                flush=True,
            )
    except Stop as e:
        stop_reason = str(e)
    except KeyboardInterrupt:
        stop_reason = "interrupted"
    finally:
        pf.close()
        idx.close()

    print(f"calls total={rpc.total} by phase={rpc.by_phase} http429={rpc.n429}", flush=True)
    if stop_reason:
        print(f"STOPPED: {stop_reason}", flush=True)
        sys.exit(2)
    # all hours attempted: compress atomically
    tmp = out + ".partial"
    subprocess.run(["zstd", "-3", "-q", "-f", partial, "-o", tmp], check=True)
    chk = subprocess.run(["zstd", "-dc", tmp], capture_output=True, check=True).stdout
    if chk != open(partial, "rb").read():
        sys.exit("compressed file does not round-trip")
    fd = os.open(tmp, os.O_RDONLY)
    os.fsync(fd)
    os.close(fd)
    os.rename(tmp, out)
    os.remove(partial)
    print(f"wrote {out}", flush=True)


if __name__ == "__main__":
    main()
