#!/usr/bin/env bash
# Offline test of deploy/feed-audit-daily.sh: python3 is a shim that prints a
# canned audit output and exits with a given code (the decision must follow
# the exit code), the notifier is a fake that appends "LEVEL|TITLE|BODY". No
# systemd, no network.
#
# If a real python3 and zstd are installed and the repository's
# .claude/skills/feed-audit/scripts/feed_audit.py is next to deploy/, the
# wrapper is also run against the real audit on a small synthetic feed (PASS
# day, day with a malformed envelope, day without files). Otherwise those
# cases print SKIP.
#
#   bash deploy/test/test-feed-audit-daily.sh     (bash 3.2+; default-day case needs GNU date)
# check() (lib.sh) evals its single-quoted expression later (SC2016); rc is
# read there too (SC2034).
# shellcheck disable=SC2016,SC2034
set -uo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source-path=SCRIPTDIR source=lib.sh
. "$HERE/lib.sh"
W=$HERE/../feed-audit-daily.sh
REAL_AUDIT=$HERE/../../.claude/skills/feed-audit/scripts/feed_audit.py
REAL_PY=$(command -v python3 || true)   # before t_init puts the python3 shim first in PATH
t_init
mkdir -p "$T/feed" "$T/reports"

# python3 shim: FAKE_AUDIT=real runs the real interpreter; otherwise it logs
# its argv, prints $FAKE_AUDIT_OUT and exits with $FAKE_AUDIT_RC.
cat > "$T/bin/python3" <<'EOF'
#!/usr/bin/env bash
if [[ ${FAKE_AUDIT:-} == real ]]; then exec "$REAL_PY" "$@"; fi
printf '%s\n' "$@" > "$T_ARGV"
[[ -n ${FAKE_AUDIT_OUT:-} ]] && cat "$FAKE_AUDIT_OUT"
exit "${FAKE_AUDIT_RC:-0}"
EOF
chmod +x "$T/bin/python3"
t_fake_notify
export T_ARGV=$T/argv REAL_PY \
    AUDIT_FEED_DIR=$T/feed AUDIT_REPORT_DIR=$T/reports AUDIT_SCRIPT=/fake/feed_audit.py \
    HC_NOTIFY=$T/bin/fake-notify

t_diag() { sed 's/^/      n: /' "$T_N" 2> /dev/null; sed 's/^/      out: /' "$T/out" 2> /dev/null; }
# run [ARGS...]: the wrapper with a fresh notification log; rc, $T/out.
run() { : > "$T_N"; rm -f "$T_ARGV"; bash "$W" "$@" > "$T/out" 2>&1; rc=$?; }

cat > "$T/pass.txt" <<'EOF'
files              24
lines              900000
envelopes          860000
bad_envelopes      0
blocks             860000
missing_blocks     0
gaps_tsv_entries   3
sessions           2
blocks_per_s       9.954
mb_per_hour        93.1
verdict            PASS
EOF
cat > "$T/fail.txt" <<'EOF'
blocks             859000
missing_blocks     1000
WARN  last_seq.txt=1, last recorded seq=2 (ok only if other files follow)
FAIL  gap 100..1099 is not listed in gaps.tsv
verdict            FAIL
EOF
cat > "$T/crash.txt" <<'EOF'
Traceback (most recent call last):
  File "feed_audit.py", line 1, in <module>
KeyError: 'message'
EOF
R=$T/reports/feed-audit-20261001.txt

# 1. PASS: info with the numbers, exit 0, report written atomically, argv.
FAKE_AUDIT_OUT=$T/pass.txt FAKE_AUDIT_RC=0 run 20261001
check "PASS: exit 0" '[[ $rc -eq 0 ]]'
check "PASS: one INFO with the numbers and the report path" '[[ $(wc -l < "$T_N") -eq 1 ]] && grep -qx "info|feed-audit 20261001: PASS|blocks=860000 missing_blocks=0 gaps_tsv_entries=3 sessions=2 blocks_per_s=9.954 mb_per_hour=93.1  / $R" "$T_N"'
check "PASS: report = header + audit output + exit line" '[[ $(head -n 1 "$R") == "# feed-audit 20261001, run "*", rpc-sample 0" ]] && grep -qx "verdict            PASS" "$R" && [[ $(tail -n 1 "$R") == "# exit 0" ]]'
check "PASS: report mode 0640, no temp file left" '[[ $(stat -c %a "$R" 2> /dev/null || stat -f %Lp "$R") == 640 ]] && [[ -z $(find "$T/reports" -name ".feed-audit-*") ]]'
check "PASS: report printed to stdout (journal)" 'grep -qx "verdict            PASS" "$T/out"'
check "argv: feed root, rpc-sample 0, frame-secs 60, glob of the day" '[[ $(paste -sd " " "$T_ARGV") == "/fake/feed_audit.py --feed-root $T/feed --rpc-sample 0 --frame-secs 60 $T/feed/2026/10/01/feed-*.tsv.zst" ]]'

AUDIT_FRAME_SECS=120 AUDIT_RPC_SAMPLE=5 FAKE_AUDIT_OUT=$T/pass.txt run 20261001
check "argv: AUDIT_FRAME_SECS and AUDIT_RPC_SAMPLE passed through" 'grep -qx 120 "$T_ARGV" && grep -qx 5 "$T_ARGV" && head -n 1 "$R" | grep -q "rpc-sample 5$"'

# 2. PASS with AUDIT_NOTIFY_PASS=0: silent.
AUDIT_NOTIFY_PASS=0 FAKE_AUDIT_OUT=$T/pass.txt FAKE_AUDIT_RC=0 run 20261001
check "PASS, AUDIT_NOTIFY_PASS=0: exit 0, no notification" '[[ $rc -eq 0 && ! -s $T_N ]]'

# 3. FAIL with a verdict: alert with FAIL/WARN lines, exit 3.
FAKE_AUDIT_OUT=$T/fail.txt FAKE_AUDIT_RC=1 run 20261001
check "FAIL: exit 3" '[[ $rc -eq 3 ]]'
check "FAIL: one ALERT titled FAIL with WARN/FAIL lines and the report path" '[[ $(wc -l < "$T_N") -eq 1 ]] && grep -qx "alert|feed-audit 20261001: FAIL|WARN  last_seq.txt=1, last recorded seq=2 (ok only if other files follow) / FAIL  gap 100..1099 is not listed in gaps.tsv / Отчёт: $R" "$T_N"'
check "FAIL: report ends with # exit 1" '[[ $(tail -n 1 "$R") == "# exit 1" ]]'

# 4. Crash (traceback, rc 1, no verdict): alert without verdict, tail as body.
FAKE_AUDIT_OUT=$T/crash.txt FAKE_AUDIT_RC=1 run 20261001
check "crash rc=1: exit 3, ALERT 'нет вердикта' with the traceback tail" '[[ $rc -eq 3 ]] && grep -q "^alert|feed-audit 20261001: FAIL (нет вердикта, rc=1)|.*KeyError: .message. / # exit 1 / Отчёт: $R$" "$T_N"'

# 5. The decision follows the exit code, not the text.
FAKE_AUDIT_OUT=$T/pass.txt FAKE_AUDIT_RC=1 run 20261001
check "text says PASS but rc=1: ALERT, exit 3" '[[ $rc -eq 3 ]] && grep -q "^alert|feed-audit 20261001: FAIL (нет вердикта, rc=1)|" "$T_N"'
FAKE_AUDIT_OUT=$T/fail.txt FAKE_AUDIT_RC=0 run 20261001
check "text says FAIL but rc=0: PASS by exit code" '[[ $rc -eq 0 ]] && grep -q "^info|feed-audit 20261001: PASS|" "$T_N"'

# 6. Usage error / audit could not run.
printf 'no such file: /srv/hood/data/feed/2026/10/01/feed-*.tsv.zst\n' > "$T/nofiles.txt"
FAKE_AUDIT_OUT=$T/nofiles.txt FAKE_AUDIT_RC=2 run 20261001
check "rc=2 (no files for the day): ALERT 'аудит не отработал (rc=2)', exit 3" '[[ $rc -eq 3 ]] && grep -q "^alert|feed-audit 20261001: аудит не отработал (rc=2)|.*no such file" "$T_N"'
FAKE_AUDIT_RC=137 run 20261001
check "rc=137 (killed): ALERT, exit 3" '[[ $rc -eq 3 ]] && grep -q "^alert|feed-audit 20261001: аудит не отработал (rc=137)|" "$T_N"'

# 7. Notifier down.
FAKE_NOTIFY_FAIL=1 FAKE_AUDIT_OUT=$T/fail.txt FAKE_AUDIT_RC=1 run 20261001
check "FAIL, notifier down: exit 1" '[[ $rc -eq 1 ]]'
FAKE_NOTIFY_FAIL=1 FAKE_AUDIT_OUT=$T/pass.txt FAKE_AUDIT_RC=0 run 20261001
check "PASS, notifier down: exit 1" '[[ $rc -eq 1 ]]'

# 8. Arguments.
rm -f "$T/reports/"*
run 2026-10-01
check "bad date: exit 2, no report, no audit, no notification" '[[ $rc -eq 2 && ! -s $T_N && ! -e $T_ARGV && -z $(ls "$T/reports") ]]'
if yesterday=$(date -u -d yesterday +%Y%m%d 2> /dev/null); then
    FAKE_AUDIT_OUT=$T/pass.txt run
    check "no argument: yesterday (UTC)" '[[ $rc -eq 0 && -e $T/reports/feed-audit-$yesterday.txt ]]'
else
    echo "SKIP  no argument: yesterday (needs GNU date)"
fi

# 9. Against the real feed_audit.py (needs python3 and zstd).
if [[ -n $REAL_PY && -f $REAL_AUDIT ]] && command -v zstd > /dev/null; then
    export FAKE_AUDIT=real AUDIT_SCRIPT=$REAL_AUDIT
    # 3 blocks (seq 100..102) and a ping, 2026-10-01 06:00 UTC.
    env_json() { printf '{"version":1,"messages":[{"sequenceNumber":%d,"message":{"message":{"header":{"kind":3,"blockNumber":26095700}}},"blockHash":"0x%064x"}]}' "$1" "$1"; }
    day=$T/feed/2026/10/01
    mkdir -p "$day"
    {
        printf '1790834400000000000\t0\t0\t{"recorderFrame":{"opcode":"ping","payloadBase64":""}}\n'
        for s in 100 101 102; do printf '17908344%02d000000000\t%d\t%d\t%s\n' $((s - 99)) "$s" "$s" "$(env_json "$s")"; done
    } | zstd -q -c > "$day/feed-20261001-06.tsv.zst"
    run 20261001
    check "real audit, good day: exit 0, INFO blocks=3" '[[ $rc -eq 0 ]] && grep -q "^info|feed-audit 20261001: PASS|blocks=3 missing_blocks=0 gaps_tsv_entries=0 sessions=1 " "$T_N"'
    check "real audit, good day: report has bad_envelopes 0 and # exit 0" 'grep -qx "bad_envelopes      0" "$R" && [[ $(tail -n 1 "$R") == "# exit 0" ]]'

    printf '1790834404000000000\t103\t103\t{"messages":[{"sequenceNumber":103,"message":{}}]}\n' | zstd -q -c >> "$day/feed-20261001-06.tsv.zst"
    run 20261001
    check "real audit, malformed envelope: ALERT FAIL with the counter, exit 3" '[[ $rc -eq 3 ]] && grep -q "^alert|feed-audit 20261001: FAIL|FAIL  1 envelopes with a malformed message" "$T_N"'

    run 20261002
    check "real audit, day without files: ALERT rc=2, exit 3" '[[ $rc -eq 3 ]] && grep -q "^alert|feed-audit 20261002: аудит не отработал (rc=2)|.*no such file" "$T_N"'
    unset FAKE_AUDIT
else
    echo "SKIP  real feed_audit.py cases (need python3, zstd and $REAL_AUDIT)"
fi

t_result
