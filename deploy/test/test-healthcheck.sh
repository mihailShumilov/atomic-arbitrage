#!/usr/bin/env bash
# Offline test of deploy/healthcheck.sh on fake data (no systemd, no network).
# systemctl, chronyc and df are replaced by shims; the notifier is a fake that
# appends "LEVEL|TITLE" to a file, so every notification can be counted.
#
#   bash deploy/test/test-healthcheck.sh     (Linux with GNU coreutils, bash 4+)
set -uo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
HC=$HERE/../healthcheck.sh
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT

mkdir -p "$T/feed" "$T/blocks" "$T/state" "$T/bin" "$T/backup"
NLOG=$T/notify.log
: > "$NLOG"

# ------------------------------------------------------------------ shims ---
cat > "$T/bin/systemctl" <<'EOF'
#!/usr/bin/env bash
s=${FAKE_UNIT_STATE:-active}; echo "$s"; [[ $s == active ]]
EOF
cat > "$T/bin/chronyc" <<'EOF'
#!/usr/bin/env bash
echo "Reference ID    : 7F7F0101 ()"
echo "Leap status     : ${FAKE_CHRONY_LEAP:-Normal}"
echo "System time     : ${FAKE_CHRONY_OFFSET:-0.000123} seconds fast of NTP time"
EOF
cat > "$T/bin/df" <<'EOF'
#!/usr/bin/env bash
echo "Filesystem 1024-blocks Used Available Capacity Mounted on"
echo "/dev/fake 1000000 $(( ${FAKE_DF_PCT:-40} * 10000 )) 1 ${FAKE_DF_PCT:-40}% /srv"
EOF
cat > "$T/bin/fake-notify" <<'EOF'
#!/usr/bin/env bash
[[ ${FAKE_NOTIFY_FAIL:-0} == 1 ]] && exit 1
printf '%s|%s\n' "$1" "$2" >> "$NLOG"
printf '%s|%s|%s\n' "$1" "$2" "${3:-}" >> "$NLOG.body"
EOF
chmod +x "$T/bin/"*
export PATH="$T/bin:$PATH" NLOG

export HC_CONFIG=/nonexistent HC_FEED_DIR=$T/feed HC_BLOCKS_DIR=$T/blocks HC_DATA_DIR=$T \
    HC_STATE_DIR=$T/state HC_NOTIFY=$T/bin/fake-notify HC_BACKUP_MARKER=$T/backup/last_ok \
    HC_BACKUP_MAX_AGE_H=0

now=$(date +%s)
ns() { echo "$(( $1 ))000000000"; }
conn_row() { # EVENT REASON STATUS RETRY PAUSE AGE_S
    local ts=$((now - $6))
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t-\t-\t0\t-\n' "$(date -u -d "@$ts" +%Y-%m-%dT%H:%M:%S.000Z)" \
        "$(ns "$ts")" "$1" "$2" "$3" "$4" "$5" >> "$T/feed/connections.tsv"
}

printf '# ts_utc\tts_unix_ns\tevent\treason\thttp_status\tretry_after\tpause_s\tsession_s\tenvelopes\tstrikes\tdetail\n' > "$T/feed/connections.tsv"
conn_row connected - 101 - - 7200
echo 76600000 > "$T/feed/last_seq.txt"
: > "$T/feed/gaps.tsv"
printf '# from\tto\tfile\tfilled_unix_s\n' > "$T/blocks/filled.tsv"

pass=0
fail=0
seen=0
run() { bash "$HC" > "$T/last.out" 2>&1 || true; }
# expect N DESC [REGEX]: exactly N new notifications since the last expect,
# and each of them matches REGEX.
expect() {
    local n=$1 desc=$2 re=${3:-.} total new bad
    total=$(wc -l < "$NLOG")
    new=$((total - seen))
    bad=$(tail -n +"$((seen + 1))" "$NLOG" | grep -Evc -- "$re")
    if (( new == n && bad == 0 )); then
        pass=$((pass + 1)); echo "PASS  $desc ($new)"
    else
        fail=$((fail + 1)); echo "FAIL  $desc: expected $n matching /$re/, got $new ($bad not matching)"
        tail -n +"$((seen + 1))" "$NLOG" | sed 's/^/      /'
        sed 's/^/      out: /' "$T/last.out"
    fi
    tail -n +"$((seen + 1))" "$NLOG" | sed 's/^/      -> /'
    seen=$total
}
fresh() { touch -d "@$now" "$T/feed/last_seq.txt"; }

# ---------------------------------------------------------------- baseline ---
fresh
run; expect 0 "baseline: healthy, first run (gaps offset initialised)"
run; expect 0 "baseline: healthy, second run"

# ------------------------------------------------------------ 1. quiet feed ---
touch -d "@$((now - 600))" "$T/feed/last_seq.txt"
run; expect 1 "quiet feed 600 s: one alert" '^alert\|фид молчит'
run; expect 0 "quiet feed: no repeat"
run; expect 0 "quiet feed: no repeat (3rd run)"
fresh
run; expect 1 "quiet feed: recovered" '^ok\|восстановлено: фид молчит'
run; expect 0 "quiet feed: nothing after recovery"

# --------------------------------------------------------------- 2. 403 ban ---
conn_row disconnected forbidden 403 3600 3600.000 60
run; expect 1 "403 in connections.tsv: one alert" '^alert\|фид отказал'
run; expect 0 "403: no repeat"
touch -d "@$((now - 900))" "$T/feed/last_seq.txt"
run; expect 0 "403 + feed silent: no extra 'feed' alert while banned"
conn_row shutdown SIGTERM - - - 50
conn_row startup_wait pending_pause - - 3174.118 40
run; expect 0 "restart during ban (startup_wait pending_pause): still the same alert"
conn_row connected - 101 - - 10
fresh
run; expect 1 "ban over (connected): recovered" '^ok\|восстановлено: фид отказал'
run; expect 0 "ban: nothing after recovery"

# ------------------------------------------------------------ 2b. 429 ------
conn_row disconnected too_many_requests 429 - 300.000 5
run; expect 1 "429 in connections.tsv: one alert" '^alert\|фид отказал'
conn_row connected - 101 - - 1
run; expect 1 "429: recovered" '^ok\|восстановлено'

# ------------------------------------------------------------- 3. new gap ---
printf '76600001\t76601200\t%s\n' "$(ns "$now")" >> "$T/feed/gaps.tsv"
run; expect 1 "new gap 1200 blocks (~2 min): one INFO" '^info\|новые дыры в фиде: 1 шт., 1200 блоков'
run; expect 0 "new gap: no repeat"
printf '76700000\t76703999\t%s\n76710000\t76710009\t%s\n' "$(ns "$now")" "$(ns "$now")" >> "$T/feed/gaps.tsv"
run; expect 1 "two new gaps, one >= 5 min: one ALERT for the batch" '^alert\|новые дыры в фиде: 2 шт., 4010 блоков.*самая длинная 4000'
run; expect 0 "two new gaps: no repeat"

# ------------------------------------------------ 3b. backfill lag + filled ---
printf '76500000\t76500999\t%s\n' "$(ns $((now - 25 * 3600)))" >> "$T/feed/gaps.tsv"
run; expect 2 "old unfilled gap: one gap event + one backfill alert" '^(info|alert)\|(новые дыры|дозаливка отстаёт: 1 дыр, 1000 блоков)'
run; expect 0 "backfill: no repeat"
printf '76500000\t76500499\tblocks-76500000-76500499.jsonl.zst\t%s\n' "$now" >> "$T/blocks/filled.tsv"
run; expect 0 "backfill half filled: still lagging, no repeat"
printf '76500500\t76500999\tblocks-76500500-76500999.jsonl.zst\t%s\n' "$now" >> "$T/blocks/filled.tsv"
run; expect 1 "backfill filled: recovered" '^ok\|восстановлено: дозаливка'

# ---------------------------------------------------------------- 4. disk ---
export FAKE_DF_PCT=85
run; expect 1 "disk 85% >= 80%: one alert" '^alert\|диск заполнен на 85%'
export FAKE_DF_PCT=92
run; expect 0 "disk 92%: no repeat"
export FAKE_DF_PCT=60
run; expect 1 "disk 60%: recovered" '^ok\|восстановлено: диск'

# ---------------------------------------------------------------- 5. unit ---
export FAKE_UNIT_STATE=activating
run; expect 1 "recorder not active: one alert" '^alert\|recorder не работает \(systemd: activating\)'
run; expect 0 "recorder not active: no repeat"
export FAKE_UNIT_STATE=active
run; expect 1 "recorder active again: recovered" '^ok\|восстановлено: recorder'

# --------------------------------------------------------------- 6. clock ---
export FAKE_CHRONY_LEAP="Not synchronised"
run; expect 1 "clock not synced: one alert" '^alert\|часы не синхронизированы'
export FAKE_CHRONY_LEAP=Normal FAKE_CHRONY_OFFSET=0.9
run; expect 0 "clock offset 0.9 s: still bad, no repeat"
export FAKE_CHRONY_OFFSET=0.0001
run; expect 1 "clock synced: recovered" '^ok\|восстановлено: часы'

# ----------------------------------------------------------- 7. reconnects ---
for i in 1 2 3 4 5 6 7; do conn_row connected - 101 - - $((i * 300)); done
run; expect 1 "8 connects in the last hour: one alert" '^alert\|частые переподключения'
run; expect 0 "reconnects: no repeat"
# An hour later the connects age out of the window: keep only old rows.
awk -F'\t' -v cut="$(ns $((now - 3600)))" '/^#/ || $2 + 0 < cut + 0' "$T/feed/connections.tsv" > "$T/c" &&
    mv "$T/c" "$T/feed/connections.tsv"
conn_row connected - 101 - - 4000
run; expect 1 "connects aged out of the hour: recovered" '^ok\|восстановлено: частые'

# ---------------------------------------------------------------- 8. backup ---
export HC_BACKUP_MAX_AGE_H=3
run; expect 1 "backup enabled, never ran: one alert" '^alert\|бэкап сырья'
touch "$T/backup/last_ok"
run; expect 1 "backup ran: recovered" '^ok\|восстановлено: бэкап'
export HC_BACKUP_MAX_AGE_H=0

# --------------------------------------------- 9. notifier down -> retried ---
export FAKE_NOTIFY_FAIL=1 FAKE_DF_PCT=81
run; expect 0 "notifier failing: nothing delivered"
if grep -q 'notify failed for disk' "$T/last.out"; then
    echo "PASS  notifier failure logged"; pass=$((pass + 1))
else
    echo "FAIL  notifier failure not logged"; fail=$((fail + 1))
fi
export FAKE_NOTIFY_FAIL=0
run; expect 1 "notifier back: the pending alert is delivered once" '^alert\|диск заполнен на 81%'
export FAKE_DF_PCT=40
run; expect 1 "disk back: recovered" '^ok'

# ------------------------------- 11. task 009 rows: backlog, client_close ---
# Recorder after task 009: `connected` carries `requested=`/`mode=` in detail,
# one `backlog` row per connection (http_status 101), `client_close` before
# `disconnected` on idle timeout / shutdown. None of this is a ban.
conn_full() { # AGE_S then the 9 columns after ts_utc/ts_unix_ns, tab-joined
    local ts=$((now - $1)); shift
    local IFS=$'\t'
    printf '%s\t%s\t%s\n' "$(date -u -d "@$ts" +%Y-%m-%dT%H:%M:%S.000Z)" "$(ns "$ts")" "$*" >> "$T/feed/connections.tsv"
}
# Real rows from data/feed-test-009/connections.tsv (2026-10-01, session B,
# recorder 009 binary; this backlog row still has the old-rule fields),
# timestamps shifted to "now".
conn_full 300 connected - 101 - - - - 0 'wss://feed.mainnet.chain.robinhood.com requested=77169135 mode=header'
conn_full 299 backlog "done" 101 - - 0.014 6 - 'requested=77169135 last_seq_before=77169134 first_seq=77169713 first_minus_requested=578 backlog_blocks=6 backlog_end_seq=77169718 burst_ms=13 pause_ms=98 stale_frames=0 complete=true'
# Final-code rows (field set of resume::Backlog::detail): idle timeout with
# Close, reconnect from memory, backlog that ends with the session.
conn_full 200 client_close server_replied 101 - - 100.000 1000 - 'sent close 1000, waited 120 ms; close 1000 "idle timeout": close frame code=Some(1000) reason="idle timeout"'
conn_full 200 disconnected idle_timeout 101 - 2.315 100.000 1000 0 'rule=jitter_1_5s no frame for 10s'
conn_full 197 connected - 101 - - - - 0 'wss://feed.mainnet.chain.robinhood.com requested=77170000 mode=header'
conn_full 196 backlog session_ended 101 - - 0.300 25 - 'requested=77170000 last_seq_before=77169999 first_seq=77170000 first_minus_requested=0 first_lag_ms=2900 backlog_blocks=25 backlog_end_seq=77170024 live_seq=- live_lag_ms=- live_after_ms=300 stale_frames=0 complete=false'
conn_full 196 client_close send_failed 101 - - 1.000 25 - 'sent close 1000, waited 0 ms; close 1000 "recorder writer gone": sending close frame: broken pipe'
fresh
run; expect 0 "009 rows (connected requested=/mode=, backlog, client_close, idle_timeout): silent"
if grep -q 'ban=ok' "$T/last.out" && grep -q 'reconnects=ok' "$T/last.out"; then
    echo "PASS  009 rows: ban=ok reconnects=ok"; pass=$((pass + 1))
else
    echo "FAIL  009 rows: summary $(tail -n 1 "$T/last.out")"; fail=$((fail + 1))
fi
# Planned restart: shutdown + client_close, then the min-interval wait.
conn_full 100 shutdown SIGTERM - - - - - - -
conn_full 100 client_close server_replied 101 - - 97.000 970 - 'sent close 1000, waited 146 ms; close 1000 "recorder shutdown": close frame code=Some(1000) reason="recorder shutdown"'
conn_full 99 startup_wait min_connect_interval - - 97.512 - - 0 'last connect less than 120s ago'
run; expect 0 "startup_wait min_connect_interval after shutdown: not a ban"
conn_full 2 connected - 101 - - - - 0 'wss://feed.mainnet.chain.robinhood.com requested=77171000 mode=header'
conn_full 1 backlog "done" 101 - - 0.800 621 - 'requested=77171000 last_seq_before=77170999 first_seq=77171000 first_minus_requested=0 first_lag_ms=63600 backlog_blocks=621 backlog_end_seq=77171620 live_seq=77171621 live_lag_ms=1420 live_after_ms=1128 stale_frames=0 complete=true'
run; expect 0 "reconnect after restart with backlog: silent"
# A 403 after 009 rows is still a ban; backlog/client_close after it never
# hide it (they only follow a successful upgrade).
conn_full 0 disconnected forbidden 403 3600 3600.000 - - 1 'rule=retry_after upgrade: HTTP 403'
run; expect 1 "403 after 009 rows: one alert" '^alert\|фид отказал'
conn_full 0 connected - 101 - - - - 0 'wss://feed.mainnet.chain.robinhood.com requested=77172000 mode=header'
run; expect 1 "connected after 403: recovered" '^ok\|восстановлено: фид отказал'

# ------------------------------------- 12. gaps.tsv row written in pieces ---
# The recorder appends a row with more than one write(); a run can see a
# line without its newline. wc -l does not count it, so it is reported once,
# when complete.
printf '76800000\t768' >> "$T/feed/gaps.tsv"
run; expect 0 "partial gaps.tsv row (no newline yet): not reported"
printf '00099\t%s\n' "$(ns "$now")" >> "$T/feed/gaps.tsv"
run; expect 1 "gaps.tsv row completed: one INFO, 100 blocks" '^info\|новые дыры в фиде: 1 шт., 100 блоков'

# ------------------------- 13. feed age by last_seq.txt, not hour files ---
# Review 011 Z1 / task 012 item 4: ping-only frames keep the hour file fresh
# while no block reaches disk; only last_seq.txt tracks new blocks. Hour
# files count only when last_seq.txt does not exist.
hour_dir="$T/feed/$(date -u -d "@$now" +%Y/%m/%d)"
hour_file="$hour_dir/feed-$(date -u -d "@$now" +%Y%m%d-%H).tsv.zst"
mkdir -p "$hour_dir"
: > "$hour_file"
touch -d "@$now" "$hour_file"
touch -d "@$((now - 600))" "$T/feed/last_seq.txt"
run; expect 1 "hour file fresh, last_seq.txt 600 s old: ALERT" '^alert\|фид молчит'
if tail -n 1 "$NLOG.body" | grep -q 'только ping'; then
    echo "PASS  ping-only hint in the alert body"; pass=$((pass + 1))
else
    echo "FAIL  ping-only hint missing: $(tail -n 1 "$NLOG.body")"; fail=$((fail + 1))
fi
if grep -q 'feed_src=last_seq.txt' "$T/last.out"; then
    echo "PASS  feed_src=last_seq.txt in summary"; pass=$((pass + 1))
else
    echo "FAIL  feed_src: $(tail -n 1 "$T/last.out")"; fail=$((fail + 1))
fi
run; expect 0 "hour file fresh, last_seq.txt old: no repeat"
fresh
run; expect 1 "last_seq.txt fresh again: recovered" '^ok\|восстановлено: фид молчит'
# No last_seq.txt (first minutes after the very first start): hour file used.
mv "$T/feed/last_seq.txt" "$T/last_seq.saved"
run; expect 0 "no last_seq.txt, hour file fresh: healthy (fallback)"
if grep -q 'feed_src=hour_file' "$T/last.out" && grep -q 'feed=ok' "$T/last.out"; then
    echo "PASS  fallback: feed_src=hour_file feed=ok"; pass=$((pass + 1))
else
    echo "FAIL  fallback: $(tail -n 1 "$T/last.out")"; fail=$((fail + 1))
fi
touch -d "@$((now - 600))" "$hour_file"
run; expect 1 "no last_seq.txt, hour file 600 s old: ALERT" '^alert\|фид молчит'
rm -f "$hour_file"
run; expect 0 "no last_seq.txt, no hour files: still the same alert"
if grep -q 'feed_age_s=-1' "$T/last.out"; then
    echo "PASS  nothing at all: feed_age_s=-1"; pass=$((pass + 1))
else
    echo "FAIL  nothing at all: $(tail -n 1 "$T/last.out")"; fail=$((fail + 1))
fi
mv "$T/last_seq.saved" "$T/feed/last_seq.txt"
fresh
run; expect 1 "last_seq.txt back and fresh: recovered" '^ok\|восстановлено: фид молчит'

# ------------------------------- 14. task 012 rows: block_idle reconnect ---
# Recorder after task 012: no message with seq > 0 for
# --block-idle-timeout-secs (30) -> Close 1000, `disconnected block_idle`,
# reconnect with the requested-seq header. Not a ban; frequent ones are
# caught by `reconnects`. Reason name and Close text as in the task 012 code
# (backoff.rs EndKind::BlockIdle, net.rs "block idle timeout"); pause from
# the transient ladder (first step 5 s).
conn_full 40 client_close server_replied 101 - - 300.000 2900 - 'sent close 1000, waited 90 ms; close 1000 "block idle timeout": close frame code=Some(1000) reason="block idle timeout"'
conn_full 40 disconnected block_idle 101 - 5.000 300.000 2900 1 'rule=transient no block (seq > 0) for 30s'
conn_full 38 connected - 101 - - - - 0 'wss://feed.mainnet.chain.robinhood.com requested=77173000 mode=header'
conn_full 37 backlog "done" 101 - - 0.500 300 - 'requested=77173000 last_seq_before=77172999 first_seq=77173000 first_minus_requested=0 backlog_blocks=300 complete=true'
fresh
run; expect 0 "012 block_idle reconnect rows: silent"
if grep -q 'ban=ok' "$T/last.out" && grep -q 'reconnects=ok' "$T/last.out"; then
    echo "PASS  block_idle rows: ban=ok reconnects=ok"; pass=$((pass + 1))
else
    echo "FAIL  block_idle rows: $(tail -n 1 "$T/last.out")"; fail=$((fail + 1))
fi

# ------------------- 15. final task 012 recorder rows (mock run, 2026-10-01) ---
# Rows copied from the task 012 mock runs of the final recorder binary
# (scratchpad 012/sample-connections.txt; ws://127.0.0.1 mock, no feed),
# columns unchanged, timestamps shifted to "now". Checks the 11-column
# positional parsing against the real output, and that none of these is a ban.
# Earlier sections left 6 `connected` rows in the last hour; drop them so
# that `reconnects` stays below the threshold here.
awk -F'\t' -v cut="$(ns $((now - 3600)))" '/^#/ || $2 + 0 < cut + 0' "$T/feed/connections.tsv" > "$T/c" &&
    mv "$T/c" "$T/feed/connections.tsv"
conn_full 90 connected - 101 - - - - 0 'ws://127.0.0.1:60020/ requested=- mode=no_data'
conn_full 88 backlog session_ended 101 - - 0.000 2 - 'requested=- last_seq_before=- first_seq=300 first_minus_requested=- first_lag_ms=251567611 backlog_blocks=2 backlog_end_seq=301 live_seq=- live_lag_ms=- live_after_ms=0 stale_frames=0 complete=false'
conn_full 88 client_close server_replied 101 - - 2.002 2 - 'sent close 1000, waited 0 ms; close 1000 "block idle timeout": close frame code=Some(1000) reason=""'
conn_full 88 disconnected block_idle 101 - 5.454 2.002 2 0 'rule=transient no block (seq > 0) for 2s'
conn_full 83 connected - 101 - - - - 0 'ws://127.0.0.1:60020/ requested=302 mode=header'
conn_full 83 backlog "done" 101 - - 0.122 2 - 'requested=302 last_seq_before=301 first_seq=302 first_minus_requested=0 first_lag_ms=60071 backlog_blocks=2 backlog_end_seq=303 live_seq=304 live_lag_ms=193 live_after_ms=121 stale_frames=0 complete=true'
conn_full 83 shutdown SIGTERM - - - - - - -
conn_full 83 client_close server_replied 101 - - 0.326 3 - 'sent close 1000, waited 0 ms; close 1000 "recorder shutdown": close frame code=Some(1000) reason=""'
conn_full 82 startup_wait min_connect_interval - - 79.510 - - 0 'previous session ended 40.490s ago (end=data_mtime), min interval 120s'
fresh
run; expect 0 "012 mock rows (block_idle, SIGTERM, startup_wait new detail): silent"
if grep -q 'ban=ok' "$T/last.out" && grep -q 'reconnects=ok' "$T/last.out" && grep -q 'writer=ok' "$T/last.out"; then
    echo "PASS  012 mock rows: ban=ok reconnects=ok writer=ok"; pass=$((pass + 1))
else
    echo "FAIL  012 mock rows: $(tail -n 1 "$T/last.out")"; fail=$((fail + 1))
fi
# Writer error: shutdown writer_error, client_close "recorder writer error",
# no `disconnected`, exit 2; then systemd restarts it and the min-interval
# wait follows. Not a ban, but its own `writer` alert.
conn_full 42 connected - 101 - - - - 0 'ws://127.0.0.1:60021/ requested=- mode=no_data'
conn_full 41 shutdown writer_error - - - - - - 'Not a directory (os error 20)'
conn_full 41 backlog session_ended 101 - - 0.000 1 - 'requested=- last_seq_before=- first_seq=500 first_minus_requested=- first_lag_ms=251567617 backlog_blocks=1 backlog_end_seq=500 live_seq=- live_lag_ms=- live_after_ms=0 stale_frames=0 complete=false'
conn_full 41 client_close server_replied 101 - - 0.000 1 - 'sent close 1000, waited 0 ms; close 1000 "recorder writer error": close frame code=Some(1000) reason=""'
conn_full 40 startup_wait min_connect_interval - - 79.510 - - 0 'previous session ended 40.490s ago (end=data_mtime), min interval 120s'
run; expect 1 "shutdown writer_error: one writer alert, no ban" '^alert\|recorder: ошибка записи на диск \(1 за 60 мин\)'
if tail -n 1 "$NLOG.body" | grep -q 'Not a directory' && grep -q 'ban=ok' "$T/last.out"; then
    echo "PASS  writer alert body has the error text; ban=ok"; pass=$((pass + 1))
else
    echo "FAIL  writer alert: $(tail -n 1 "$NLOG.body") / $(tail -n 1 "$T/last.out")"; fail=$((fail + 1))
fi
conn_full 39 writer_error final_commit - - - - - - 'No space left on device (os error 28)'
run; expect 0 "writer_error final_commit while raised: no repeat"
conn_full 1 connected - 101 - - - - 0 'ws://127.0.0.1:60021/ requested=501 mode=header'
run; expect 0 "reconnected but the error is younger than the window: still raised"
export HC_WRITER_ERROR_WINDOW_S=30
run; expect 1 "writer rows older than the window: recovered" '^ok\|восстановлено: recorder: ошибка записи'
run; expect 0 "writer: nothing after recovery"

# ------------------------------------------------- 10. everything at once ---
run; expect 0 "final: healthy, silent"
tail -n 1 "$T/last.out"

echo "result: $pass passed, $fail failed"
(( fail == 0 ))
