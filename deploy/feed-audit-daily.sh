#!/usr/bin/env bash
# hoodchain-mev: daily feed audit (feed-audit.timer, 00:10 UTC).
#
#   feed-audit-daily.sh [YYYYMMDD]     default: yesterday (UTC)
#
# Runs feed_audit.py over one UTC day of raw feed files, offline
# (--rpc-sample 0 until an RPC provider is chosen; AUDIT_RPC_SAMPLE > 0 would
# call RPC_URL or the public RPC). Writes the report to
# AUDIT_REPORT_DIR/feed-audit-YYYYMMDD.txt and prints it to the journal.
# FAIL -> alert, exit 3. PASS -> short info message (daily "alive" signal) unless
# AUDIT_NOTIFY_PASS=0.
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

{
    echo "# feed-audit $day, run $(date -u +%Y-%m-%dT%H:%M:%SZ), rpc-sample $AUDIT_RPC_SAMPLE"
    python3 "$AUDIT_SCRIPT" --feed-root "$AUDIT_FEED_DIR" --rpc-sample "$AUDIT_RPC_SAMPLE" \
        --frame-secs "$AUDIT_FRAME_SECS" "$dir/feed-*.tsv.zst" 2>&1
    echo "# exit $?"
} > "$tmp"
chmod 0640 "$tmp"
mv -f "$tmp" "$report"
trap - EXIT
cat "$report"

verdict=$(awk '$1 == "verdict" { print $2 }' "$report")
if [[ $verdict == PASS ]]; then
    if [[ $AUDIT_NOTIFY_PASS == 1 ]]; then
        nums=$(awk '$1 ~ /^(blocks|missing_blocks|gaps_tsv_entries|sessions|blocks_per_s|mb_per_hour)$/ { printf "%s=%s ", $1, $2 }' "$report")
        "$HC_NOTIFY" info "feed-audit $day: PASS" "$nums"$'\n'"$report" || exit 1
    fi
    exit 0
fi

fails=$(grep -E '^(FAIL|WARN) ' "$report" | head -n 10)
[[ -n $fails ]] || fails=$(tail -n 5 "$report")
# Exit 3 = audit FAIL, alert delivered (the unit treats it as success, so
# OnFailure does not send a second message). Exit 1 = alert not delivered.
"$HC_NOTIFY" alert "feed-audit $day: ${verdict:-FAIL (нет вердикта)}" "$fails"$'\n'"Отчёт: $report" || exit 1
exit 3
