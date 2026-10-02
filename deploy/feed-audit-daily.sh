#!/usr/bin/env bash
# hoodchain-mev: daily feed audit (feed-audit.timer, 00:10 UTC).
#
#   feed-audit-daily.sh [YYYYMMDD]     default: yesterday (UTC)
#
# Runs feed_audit.py over one UTC day of raw feed files, offline
# (--rpc-sample 0 until an RPC provider is chosen; AUDIT_RPC_SAMPLE > 0 would
# call RPC_URL or the public RPC). Writes the report to
# AUDIT_REPORT_DIR/feed-audit-YYYYMMDD.txt and prints it to the journal.
#
# The decision is taken from feed_audit.py's exit code (0 PASS, 1 FAIL,
# 2 usage error, e.g. no files for the day), not from its text output; the
# `verdict` line only tells a FAIL from a crash (a Python traceback also
# exits 1). PASS -> short info message (daily "alive" signal) unless
# AUDIT_NOTIFY_PASS=0, exit 0. Anything else -> alert, exit 3.
#
# Exit codes: 0 PASS; 3 not PASS and the alert was delivered (the unit treats
# 3 as success, so OnFailure does not send a second message); 1 the notifier
# failed; 2 bad argument.
#
# No `set -e` on purpose: a non-zero audit is an expected outcome that must
# still be reported; every step checks its own result.
set -uo pipefail

: "${AUDIT_FEED_DIR:=/srv/hood/data/feed}"
: "${AUDIT_REPORT_DIR:=/srv/hood/reports}"
: "${AUDIT_SCRIPT:=/opt/hoodchain-mev/deploy/feed_audit.py}"
: "${AUDIT_RPC_SAMPLE:=0}"
# Must equal the recorder's --frame-secs (recorder.service: default 60). The
# audit's grace window for an open previous-hour frame is HH:00 + frame-secs
# + 60 s; at 00:10 it is long over, so --now is not passed (default: now UTC).
: "${AUDIT_FRAME_SECS:=60}"
: "${AUDIT_NOTIFY_PASS:=1}"
: "${HC_NOTIFY:=/opt/hoodchain-mev/deploy/notify.sh}"

day=${1:-$(date -u -d yesterday +%Y%m%d)}
if [[ ! $day =~ ^[0-9]{8}$ ]]; then
    echo "usage: feed-audit-daily.sh [YYYYMMDD]" >&2
    exit 2
fi
dir="$AUDIT_FEED_DIR/${day:0:4}/${day:4:2}/${day:6:2}"
report="$AUDIT_REPORT_DIR/feed-audit-$day.txt"
mkdir -p "$AUDIT_REPORT_DIR"
tmp=$(mktemp "$AUDIT_REPORT_DIR/.feed-audit-$day.XXXXXX")
trap 'rm -f "$tmp"' EXIT

audit_rc=0
{
    echo "# feed-audit $day, run $(date -u +%Y-%m-%dT%H:%M:%SZ), rpc-sample $AUDIT_RPC_SAMPLE"
    python3 "$AUDIT_SCRIPT" --feed-root "$AUDIT_FEED_DIR" --rpc-sample "$AUDIT_RPC_SAMPLE" \
        --frame-secs "$AUDIT_FRAME_SECS" "$dir/feed-*.tsv.zst" 2>&1
    audit_rc=$?
    echo "# exit $audit_rc"
} > "$tmp"
chmod 0640 "$tmp"
mv -f "$tmp" "$report"
trap - EXIT
cat "$report"

if (( audit_rc == 0 )); then
    if [[ $AUDIT_NOTIFY_PASS == 1 ]]; then
        nums=$(awk '$1 ~ /^(blocks|missing_blocks|gaps_tsv_entries|sessions|blocks_per_s|mb_per_hour)$/ { printf "%s=%s ", $1, $2 }' "$report")
        "$HC_NOTIFY" info "feed-audit $day: PASS" "$nums"$'\n'"$report" || exit 1
    fi
    exit 0
fi

if (( audit_rc == 1 )) && grep -qx 'verdict  *FAIL' "$report"; then
    title=FAIL
elif (( audit_rc == 1 )); then
    title="FAIL (нет вердикта, rc=1)"
else
    title="аудит не отработал (rc=$audit_rc)"
fi
fails=$(grep -E '^(FAIL|WARN) ' "$report" | head -n 10)
[[ -n $fails ]] || fails=$(tail -n 5 "$report")
"$HC_NOTIFY" alert "feed-audit $day: $title" "$fails"$'\n'"Отчёт: $report" || exit 1
exit 3
