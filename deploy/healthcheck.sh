#!/usr/bin/env bash
# hoodchain-mev: host and recorder healthcheck (healthcheck.timer, every 5 min).
#
# Conditions (one notification when a condition starts, one "восстановлено"
# when it ends; state files in HC_STATE_DIR):
#   unit       recorder unit is not `active`
#   feed       last_seq.txt older than HC_FEED_MAX_AGE_S (no new blocks on
#              disk); the current/previous hour files are used only while
#              last_seq.txt does not exist. Not raised while a ban is active:
#              the ban alert already explains the silence
#   disk       data filesystem usage >= HC_DISK_MAX_PCT
#   ban        last connect-related row of connections.tsv is a 4xx refusal
#              (403 ban, 429) or a startup_wait pending_pause >= HC_BAN_MIN_PAUSE_S
#   reconnects more than HC_MAX_CONNECTS_PER_HOUR `connected` rows in the last hour
#   writer     `shutdown writer_error` / `writer_error` row in connections.tsv
#              younger than HC_WRITER_ERROR_WINDOW_S (disk/fsync failure, exit 2)
#   backfill   gaps older than HC_BACKFILL_MAX_LAG_H not covered by filled.tsv
#   clock      chrony not synchronised or |offset| > HC_CLOCK_MAX_OFFSET_S
#   backup     last successful backup older than HC_BACKUP_MAX_AGE_H (0 = off)
#   raid       an md array in HC_MDSTAT (/proc/mdstat) is degraded ([U_], [_U])
#              or inactive. Skipped when the file is missing or has no arrays
# Events (one notification per new batch, no "recovered"):
#   gaps       new rows in gaps.tsv since the last run (count, blocks, minutes)
#   raid_sync  INFO once when a resync/recovery/reshape starts on an array
#              (percent, ETA); nothing while it runs or when it ends (mdadm
#              RebuildFinished and the `raid` recovery cover the end)
#
# Read-only on the data. No network except the optional HC_HEARTBEAT_URL ping
# and whatever the notifier does. Never touches the feed.
set -uo pipefail

HC_CONFIG=${HC_CONFIG:-/etc/hoodchain/healthcheck.env}
if [[ -r $HC_CONFIG ]]; then
    # shellcheck source=/dev/null
    . "$HC_CONFIG"
fi

: "${HC_FEED_DIR:=/srv/hood/data/feed}"
: "${HC_BLOCKS_DIR:=/srv/hood/data/blocks}"
: "${HC_DATA_DIR:=/srv/hood/data}"
: "${HC_STATE_DIR:=/var/lib/hoodchain/health}"
: "${HC_BACKUP_MARKER:=/var/lib/hoodchain/backup/last_ok}"
: "${HC_NOTIFY:=/opt/hoodchain-mev/deploy/notify.sh}"
: "${HC_RECORDER_UNIT:=recorder.service}"
: "${HC_FEED_MAX_AGE_S:=300}"
: "${HC_DISK_MAX_PCT:=80}"
: "${HC_BAN_MIN_PAUSE_S:=600}"
: "${HC_MAX_CONNECTS_PER_HOUR:=6}"
: "${HC_WRITER_ERROR_WINDOW_S:=3600}"
: "${HC_GAP_ALERT_BLOCKS:=2979}"   # ~5 min at ~9.93 blocks/s (chain-facts)
: "${HC_BLOCKS_PER_S:=9.93}"
: "${HC_BACKFILL_MAX_LAG_H:=24}"   # 0 = off
: "${HC_CHECK_CLOCK:=1}"
: "${HC_CLOCK_MAX_OFFSET_S:=0.5}"
: "${HC_BACKUP_MAX_AGE_H:=0}"      # 0 = off (backup not enabled yet)
: "${HC_CHECK_RAID:=1}"
: "${HC_MDSTAT:=/proc/mdstat}"     # tests: a fake mdstat file
: "${HC_HEARTBEAT_URL:=}"
: "${HC_NOW:=}"                    # tests only: fixed "now" (unix seconds)

now_s=${HC_NOW:-$(date +%s)}
rc=0
summary=()

log() { printf '%s\n' "$*"; }

if ! mkdir -p "$HC_STATE_DIR" 2>/dev/null || [[ ! -w $HC_STATE_DIR ]]; then
    log "cannot write state dir $HC_STATE_DIR"
    exit 1
fi

notify() { "$HC_NOTIFY" "$@"; }

# raise KEY TITLE BODY — notify once while the condition holds.
raise() {
    local key=$1 title=$2 body=$3 f="$HC_STATE_DIR/$1.alert"
    summary+=("$key=BAD")
    [[ -e $f ]] && return 0
    if notify alert "$title" "$body"; then
        printf '%s\n' "$title" > "$f"
        log "alert sent: $key: $title"
    else
        log "notify failed for $key, will retry next run"
        rc=1
    fi
}

# resolved KEY — notify "восстановлено" once if the condition was raised.
resolved() {
    local key=$1 f="$HC_STATE_DIR/$1.alert" title
    summary+=("$key=ok")
    [[ -e $f ]] || return 0
    title=$(head -n 1 "$f")
    if notify ok "восстановлено: $title" "${2:-}"; then
        rm -f "$f"
        log "recovered: $key"
    else
        log "notify failed for $key recovery, will retry next run"
        rc=1
    fi
}

mtime() { stat -c %Y "$1" 2>/dev/null || echo 0; }

# ------------------------------------------------------------------- unit ---
unit_state=$(systemctl is-active "$HC_RECORDER_UNIT" 2>/dev/null)
unit_state=${unit_state:-unknown}
if [[ $unit_state == active ]]; then
    resolved unit
else
    raise unit "recorder не работает (systemd: $unit_state)" \
        "systemctl status $HC_RECORDER_UNIT; journalctl -u $HC_RECORDER_UNIT -n 100. Рестарт вручную не чаще раза в 2 мин; с этого IP к фиду руками не подключаться."
fi

# -------------------------------------------------------------------- ban ---
conn="$HC_FEED_DIR/connections.tsv"
ban_row=""
if [[ -r $conn ]]; then
    last_row=$(tail -c 262144 "$conn" | awk -F'\t' '
        $0 !~ /^#/ && ($3 == "connected" || $3 == "disconnected" || $3 == "startup_wait") { last = $0 }
        END { print last }')
    if [[ -n $last_row ]]; then
        IFS=$'\t' read -r b_ts _ b_event b_reason b_status b_retry b_pause _ <<< "$last_row"
        if [[ $b_event == disconnected && $b_status =~ ^4[0-9][0-9]$ ]]; then
            ban_row="$b_ts: disconnected $b_reason, HTTP $b_status, Retry-After $b_retry, пауза $b_pause с"
        elif [[ $b_event == startup_wait && $b_reason == pending_pause && $b_pause =~ ^[0-9.]+$ ]] &&
            awk -v p="$b_pause" -v m="$HC_BAN_MIN_PAUSE_S" 'BEGIN { exit !(p + 0 >= m + 0) }'; then
            ban_row="$b_ts: startup_wait pending_pause $b_pause с (recorder выжидает паузу прошлого запуска)"
        fi
    fi
fi
if [[ -n $ban_row ]]; then
    raise ban "фид отказал в подключении (бан/лимит)" \
        "$ban_row. Recorder сам выжидает паузу и Retry-After; не перезапускайте его и не подключайтесь к фиду с этого IP. Дыра попадёт в gaps.tsv."
else
    resolved ban
fi

# ------------------------------------------------------------------- feed ---
# Feed age = mtime of last_seq.txt. The recorder rewrites it only when the
# last seq on disk grew (at each frame commit, <= --frame-secs), so it tracks
# new blocks. Hourly files are NOT a block signal: ping-only frames still go
# into them and the frame closes every <= 60 s even without blocks (review
# 011, Z1). They are used only when last_seq.txt does not exist yet.
last_seq_file="$HC_FEED_DIR/last_seq.txt"
cur_hour_file="$HC_FEED_DIR/$(date -u -d "@$now_s" +%Y/%m/%d)/feed-$(date -u -d "@$now_s" +%Y%m%d-%H).tsv.zst"
prev_s=$((now_s - 3600))
prev_hour_file="$HC_FEED_DIR/$(date -u -d "@$prev_s" +%Y/%m/%d)/feed-$(date -u -d "@$prev_s" +%Y%m%d-%H).tsv.zst"
hour_newest=0
for f in "$cur_hour_file" "$prev_hour_file"; do
    m=$(mtime "$f")
    (( m > hour_newest )) && hour_newest=$m
done
last_seq=""
if [[ -e $last_seq_file ]]; then
    feed_src=last_seq.txt
    newest=$(mtime "$last_seq_file")
    last_seq=$(head -c 32 "$last_seq_file" 2>/dev/null | tr -dc '0-9')
else
    feed_src=hour_file
    newest=$hour_newest
fi
if (( newest == 0 )); then
    feed_age=-1
    feed_msg="нет ни last_seq.txt, ни файла текущего или прошлого часа в $HC_FEED_DIR"
else
    feed_age=$((now_s - newest))
    if [[ $feed_src == last_seq.txt ]]; then
        if (( feed_age < HC_FEED_MAX_AGE_S )); then
            feed_msg="last_seq.txt обновлялся ${feed_age} с назад, last_seq=${last_seq:-?}"
        else
            feed_msg="last_seq.txt не менялся ${feed_age} с, last_seq=${last_seq:-?}"
        fi
        # The ping-only hint belongs to the alert only: while blocks arrive the
        # hour file is always fresh too, so in the "recovered" text it was a
        # false "no blocks" (task 014, item 3).
        if (( feed_age >= HC_FEED_MAX_AGE_S && hour_newest > 0 && now_s - hour_newest < HC_FEED_MAX_AGE_S )); then
            feed_msg+=". Файл часа при этом обновлялся $((now_s - hour_newest)) с назад: соединение, похоже, живо, но блоков нет (только ping?)"
        fi
    else
        feed_msg="last_seq.txt нет; файл часа обновлялся ${feed_age} с назад"
    fi
fi
if (( feed_age >= 0 && feed_age < HC_FEED_MAX_AGE_S )); then
    resolved feed "$feed_msg"
elif [[ -n $ban_row ]]; then
    summary+=("feed=quiet(ban)")
else
    raise feed "фид молчит: нет новых блоков дольше $HC_FEED_MAX_AGE_S с" \
        "$feed_msg. Дыра появится в gaps.tsv после переподключения. journalctl -u $HC_RECORDER_UNIT -n 100; tail $conn"
fi

# ------------------------------------------------------------------- disk ---
disk_line=$(df -P "$HC_DATA_DIR" 2>/dev/null | awk 'NR == 2')
disk_pct=$(awk '{ gsub("%", "", $5); print $5 }' <<< "$disk_line")
if [[ $disk_pct =~ ^[0-9]+$ ]]; then
    if (( disk_pct >= HC_DISK_MAX_PCT )); then
        raise disk "диск заполнен на ${disk_pct}% (порог ${HC_DISK_MAX_PCT}%)" \
            "$(df -h "$HC_DATA_DIR" | awk 'NR == 2'). Сырьё не удалять: сначала убедиться, что оно в бэкапе."
    else
        resolved disk
    fi
else
    raise disk "не удалось узнать заполнение диска $HC_DATA_DIR" "df -P $HC_DATA_DIR"
fi

# ------------------------------------------------------------- reconnects ---
if [[ -r $conn ]]; then
    n_conn=$(tail -c 262144 "$conn" | awk -F'\t' -v since="$(( now_s - 3600 ))" '
        $0 !~ /^#/ && $3 == "connected" && ($2 / 1e9) >= since { n++ } END { print n + 0 }')
    if (( n_conn > HC_MAX_CONNECTS_PER_HOUR )); then
        raise reconnects "частые переподключения к фиду: $n_conn за час" \
            "Риск бана IP. tail -n 30 $conn; journalctl -u $HC_RECORDER_UNIT -n 200"
    else
        resolved reconnects
    fi
fi

# ----------------------------------------------------------------- writer ---
# Task 012: a fatal writer error (disk full, fsync, EIO) makes the recorder
# send Close 1000 and exit 2; connections.tsv gets `shutdown writer_error`
# (or `writer_error final_commit` if it failed in the last commit after a
# signal). systemd restarts it after RestartSec=120, so `unit` may never be
# seen. Raised while such a row is younger than HC_WRITER_ERROR_WINDOW_S.
if [[ -r $conn ]]; then
    w_row=$(tail -c 262144 "$conn" | awk -F'\t' -v since="$(( now_s - HC_WRITER_ERROR_WINDOW_S ))" '
        $0 !~ /^#/ && (($3 == "shutdown" && $4 == "writer_error") || $3 == "writer_error") &&
        ($2 / 1e9) >= since { n++; last = $1 " " $3 " " $4 ": " $11 }
        END { if (n) print n "\t" last }')
    if [[ -n $w_row ]]; then
        raise writer "recorder: ошибка записи на диск (${w_row%%$'\t'*} за $((HC_WRITER_ERROR_WINDOW_S / 60)) мин)" \
            "${w_row#*$'\t'}. Recorder закрыл соединение и вышел с кодом 2, systemd перезапустит через 120 с. df -h $HC_DATA_DIR; journalctl -u $HC_RECORDER_UNIT -n 100. Сырьё не удалять, пока оно не в бэкапе."
    else
        resolved writer
    fi
fi

# ------------------------------------------------------------------- gaps ---
gaps="$HC_FEED_DIR/gaps.tsv"
off_file="$HC_STATE_DIR/gaps.offset"
n_lines=0
[[ -r $gaps ]] && n_lines=$(wc -l < "$gaps")
if [[ ! -e $off_file ]]; then
    # First run (or wiped state): do not replay history.
    echo "$n_lines" > "$off_file"
    log "gaps: initialised offset at $n_lines rows"
else
    off=$(tr -dc '0-9' < "$off_file")
    off=${off:-0}
    if (( n_lines < off )); then
        log "gaps.tsv shrank ($off -> $n_lines rows), resetting offset"
        echo "$n_lines" > "$off_file"
    elif (( n_lines > off )); then
        new=$(tail -n +"$((off + 1))" "$gaps" | head -n "$((n_lines - off))")
        read -r g_n g_blocks g_max < <(awk -F'\t' '$1 ~ /^[0-9]+$/ && $2 ~ /^[0-9]+$/ {
            l = $2 - $1 + 1; n++; s += l; if (l > m) m = l } END { print n + 0, s + 0, m + 0 }' <<< "$new")
        mins=$(awk -v b="$g_blocks" -v r="$HC_BLOCKS_PER_S" 'BEGIN { printf "%.1f", b / r / 60 }')
        max_mins=$(awk -v b="$g_max" -v r="$HC_BLOCKS_PER_S" 'BEGIN { printf "%.1f", b / r / 60 }')
        level=info
        (( g_max >= HC_GAP_ALERT_BLOCKS )) && level=alert
        listing=$(awk -F'\t' '$1 ~ /^[0-9]+$/ { printf "%s..%s (%d)\n", $1, $2, $2 - $1 + 1 }' <<< "$new" | head -n 10)
        if notify "$level" "новые дыры в фиде: $g_n шт., $g_blocks блоков (~$mins мин), самая длинная $g_max (~$max_mins мин)" \
            "$listing"$'\n'"Дозаливка: enricher --gaps (enricher-gaps.timer)."; then
            echo "$n_lines" > "$off_file"
            log "gaps: notified $g_n new rows"
        else
            log "notify failed for new gaps, will retry next run"
            rc=1
        fi
    fi
fi
summary+=("gaps_rows=$n_lines")

# --------------------------------------------------------------- backfill ---
if (( HC_BACKFILL_MAX_LAG_H > 0 )) && [[ -r $gaps ]]; then
    filled="$HC_BLOCKS_DIR/filled.tsv"
    [[ -r $filled ]] || filled=/dev/null
    read -r u_n u_blocks < <(awk -F'\t' -v cutoff="$(( now_s - HC_BACKFILL_MAX_LAG_H * 3600 ))" '
        FNR == NR { if ($1 ~ /^[0-9]+$/ && $2 ~ /^[0-9]+$/) { nf++; ff[nf] = $1; ft[nf] = $2 }; next }
        $1 ~ /^[0-9]+$/ && $2 ~ /^[0-9]+$/ && $3 ~ /^[0-9]+$/ && ($3 / 1e9) < cutoff {
            len = $2 - $1 + 1; cov = 0
            for (i = 1; i <= nf; i++) {
                lo = ($1 > ff[i]) ? $1 : ff[i]; hi = ($2 < ft[i]) ? $2 : ft[i]
                if (hi >= lo) cov += hi - lo + 1
            }
            if (cov < len) { n++; b += len - cov }
        }
        END { print n + 0, b + 0 }' "$filled" "$gaps")
    if (( u_n > 0 )); then
        raise backfill "дозаливка отстаёт: $u_n дыр, $u_blocks блоков старше ${HC_BACKFILL_MAX_LAG_H} ч не залиты" \
            "Проверить enricher-gaps.timer (systemctl list-timers; journalctl -u enricher-gaps). Пока провайдер RPC не выбран, таймер выключен."
    else
        resolved backfill
    fi
fi

# ------------------------------------------------------------------ clock ---
if [[ $HC_CHECK_CLOCK == 1 ]]; then
    tracking=$(chronyc -n tracking 2>/dev/null)
    leap=$(awk -F': *' '/^Leap status/ { print $2 }' <<< "$tracking")
    offset=$(awk '/^System time/ { print $4 }' <<< "$tracking")
    if [[ $leap == Normal && $offset =~ ^[0-9.]+$ ]] &&
        awk -v o="$offset" -v m="$HC_CLOCK_MAX_OFFSET_S" 'BEGIN { exit !(o + 0 <= m + 0) }'; then
        resolved clock
    else
        raise clock "часы не синхронизированы (chrony: ${leap:-нет ответа}, смещение ${offset:-?} с)" \
            "chronyc tracking; chronyc sources -v. recv_unix_ns фида используется для оценки задержек."
    fi
fi

# ----------------------------------------------------------------- backup ---
if (( HC_BACKUP_MAX_AGE_H > 0 )); then
    b_m=$(mtime "$HC_BACKUP_MARKER")
    b_age_h=$(( (now_s - b_m) / 3600 ))
    if (( b_m > 0 && b_age_h < HC_BACKUP_MAX_AGE_H )); then
        resolved backup
    else
        raise backup "бэкап сырья не обновлялся больше ${HC_BACKUP_MAX_AGE_H} ч" \
            "journalctl -u backup -n 100; systemctl list-timers backup.timer"
    fi
fi

# ------------------------------------------------------------------- raid ---
# /proc/mdstat: "mdN : active raid1 ..." then "[2/2] [UU]" and, while it runs,
# "resync = 81.7% (...) finish=56.3min" or "resync=DELAYED". A "_" in the
# member map means a missing/failed member. mdadm --monitor (mdadm-event.sh)
# reports the events; this check repeats a degradation until it is fixed and
# says "восстановлено" when the map is full again.
raid_sync_txt=""
if [[ $HC_CHECK_RAID == 1 && -r $HC_MDSTAT ]]; then
    raid_out=$(awk '
        /^md[^ ]* : / { dev = $1; if ($3 == "inactive") print "BAD", dev, "inactive"; next }
        /^unused devices/ { dev = ""; next }
        dev == "" { next }
        match($0, /\[[0-9]+\/[0-9]+\] \[[U_]+\]/) {
            st = substr($0, RSTART, RLENGTH); n_arr++
            if (st ~ /_/) print "BAD", dev, st
            next
        }
        match($0, /(resync|recovery|reshape|check) *= *[0-9.]+%/) {
            s = substr($0, RSTART, RLENGTH); gsub(/ /, "", s); split(s, a, "=")
            fin = "-"
            if (match($0, /finish=[0-9.]+min/)) fin = substr($0, RSTART + 7, RLENGTH - 10)
            print "SYNC", dev, a[1], a[2], fin
            next
        }
        match($0, /(resync|recovery|reshape|check) *= *[A-Z]+/) {
            s = substr($0, RSTART, RLENGTH); gsub(/ /, "", s); split(s, a, "=")
            print "SYNC", dev, a[1], a[2], "-"
        }
        END { print "ARRAYS", n_arr + 0 }' "$HC_MDSTAT")
    n_arrays=$(awk '$1 == "ARRAYS" { print $2 }' <<< "$raid_out")
    if (( n_arrays > 0 )); then
        raid_bad=$(awk '$1 == "BAD" { $1 = ""; sub(/^ /, ""); printf "%s%s", sep, $0; sep = ", " }' <<< "$raid_out")
        if [[ -n $raid_bad ]]; then
            raise raid "RAID деградирован: $raid_bad" \
                "$(awk -v list=" $(awk '$1 == "BAD" { print $2 }' <<< "$raid_out" | paste -sd ' ') " '
                    /^md[^ ]* : / { on = index(list, " " $1 " ") > 0 }
                    /^unused devices/ { on = 0 }
                    on && NF { sub(/^[[:space:]]+/, ""); print }' "$HC_MDSTAT" | head -n 20)"$'\n'"cat /proc/mdstat; mdadm --detail /dev/<md>. Сервер не перезагружать и recorder не трогать; массив сейчас без избыточности, сырьё фида без бэкапа невосполнимо. Замена диска — через Hetzner Robot (решение Михаила)."
        else
            resolved raid "все массивы в /proc/mdstat полные ($n_arrays шт.)"
        fi
        # Human-readable list of running/pending syncs, "check" included in
        # the summary but never notified (mdcheck runs it monthly).
        raid_sync_txt=$(awk '$1 == "SYNC" {
            what = ($4 ~ /%$/) ? $4 : "ожидает (" $4 ")"
            eta = ($5 != "-") ? sprintf(", осталось ~%d мин", int($5) + ($5 > int($5))) : ""
            printf "%s%s %s %s%s", sep, $2, $3, what, eta; sep = "; " }' <<< "$raid_out")
        sync_keys=$(awk '$1 == "SYNC" && $3 != "check" { print $2 ":" $3 }' <<< "$raid_out" | sort -u)
        keys_file="$HC_STATE_DIR/raid_sync.keys"
        old_keys=""
        [[ -r $keys_file ]] && old_keys=$(sort -u "$keys_file")
        new_keys=$(comm -13 <(printf '%s\n' "$old_keys" | sed '/^$/d') <(printf '%s\n' "$sync_keys" | sed '/^$/d'))
        if [[ -n $new_keys ]]; then
            if notify info "RAID: идёт синхронизация: $raid_sync_txt" \
                "Новая: $(paste -sd ' ' <<< "$new_keys"). Действий не нужно, если это начальная сборка, ежемесячная проверка или замена диска; сервер не перезагружать до конца. cat /proc/mdstat"; then
                printf '%s\n' "$sync_keys" > "$keys_file"
                log "raid: sync notified: $(paste -sd ' ' <<< "$new_keys")"
            else
                log "notify failed for raid sync, will retry next run"
                rc=1
            fi
        elif [[ $sync_keys != "$old_keys" ]]; then
            # Some syncs ended: forget them silently (no "finished" spam).
            printf '%s\n' "$sync_keys" > "$keys_file"
        fi
    else
        summary+=("raid=none")
    fi
fi
[[ -n $raid_sync_txt ]] && summary+=("raid_sync=${raid_sync_txt// /_}")

log "healthcheck: unit=$unit_state feed_age_s=$feed_age feed_src=$feed_src disk_pct=${disk_pct:-?} ${summary[*]}"

# -------------------------------------------------------------- heartbeat ---
# Optional external dead-man switch: alerts when this host stops pinging.
if [[ -n $HC_HEARTBEAT_URL ]]; then
    curl -fsS -m 10 --retry 2 -o /dev/null -K - <<< "url = \"$HC_HEARTBEAT_URL\"" ||
        log "heartbeat ping failed"
fi

exit "$rc"
