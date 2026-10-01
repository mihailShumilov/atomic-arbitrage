#!/usr/bin/env bash
# hoodchain-mev: mdadm --monitor PROGRAM hook -> notify.sh.
#
#   mdadm-event.sh EVENT MD_DEVICE [COMPONENT_DEVICE]
#
# Installed by bootstrap.sh as /opt/hoodchain-mev/deploy/mdadm-event.sh and
# referenced from /etc/mdadm/mdadm.conf.d/hood.conf (PROGRAM line). mdmonitor
# (mdadm --monitor --scan, root) runs it once per event; so do the daily
# mdmonitor-oneshot reminder and `mdadm --monitor --oneshot --test`.
#
#   ALERT  Fail, FailSpare, DegradedArray, DeviceDisappeared, SpareActive
#   INFO   RebuildFinished, TestMessage
#   other  (RebuildStarted, RebuildNN, NewArray, MoveSpare, SparesMissing, ...)
#          journald only (tag hood-mdadm), no notification: progress every
#          20 % would be spam; healthcheck.sh reports a started resync once.
#
# Always exits 0: mdadm ignores the status, and a failed notification of a
# degraded array is repeated by healthcheck.sh (`raid`, retried every run).
set -uo pipefail

NOTIFY=${MDADM_EVENT_NOTIFY:-/opt/hoodchain-mev/deploy/notify.sh}
MDSTAT=${MDADM_EVENT_MDSTAT:-/proc/mdstat}

event=${1:-}
md=${2:-?}
comp=${3:-}

log() { logger -t hood-mdadm -p user.info -- "$*" 2>/dev/null || echo "hood-mdadm: $*" >&2; }

if [[ -z $event ]]; then
    log "called without arguments"
    exit 0
fi

# Kernel name (md2) for the /proc/mdstat excerpt; mdadm passes /dev/md/2 or /dev/md2.
kname=$(basename "$(readlink -f "$md" 2>/dev/null || echo "$md")")
if [[ $kname != md* && $md =~ ^/dev/md/([0-9]+)$ ]]; then
    kname=md${BASH_REMATCH[1]}   # /dev/md/N without the udev symlink
fi
excerpt=""
if [[ -r $MDSTAT ]]; then
    excerpt=$(awk -v d="$kname" '
        $1 == d && $2 == ":" { on = 1; print; next }
        on && /^[[:space:]]*$/ { exit }
        on && /^[a-z]/ { exit }
        on { gsub(/^[[:space:]]+/, ""); print }' "$MDSTAT" | head -n 4)
fi
dev_txt=${comp:+, диск $comp}
hint="cat /proc/mdstat; mdadm --detail $md. Сервер не перезагружать и recorder не трогать; данные массива сейчас на одном диске, сырьё фида без бэкапа невосполнимо."

level="" title=""
case $event in
    Fail)              level=alert; title="RAID: диск выпал из массива $md$dev_txt" ;;
    FailSpare)         level=alert; title="RAID: запасной диск отказал при сборке $md$dev_txt" ;;
    DegradedArray)     level=alert; title="RAID деградирован: $md" ;;
    DeviceDisappeared) level=alert; title="RAID: массив $md пропал" ;;
    SpareActive)       level=alert; title="RAID: в $md активирован запасной/новый диск$dev_txt (после отказа или замены)"
                       hint="cat /proc/mdstat; mdadm --detail $md. Если диск не меняли, выяснить, какой отказал (journalctl -u mdmonitor; journalctl -k)." ;;
    RebuildFinished)   level=info;  title="RAID: синхронизация $md завершена"
                       hint="cat /proc/mdstat" ;;
    TestMessage)       level=info;  title="RAID: тестовое сообщение mdadm для $md"
                       hint="Проверка цепочки mdadm --monitor -> notify.sh, действий не нужно." ;;
esac

if [[ -z $level ]]; then
    log "event $event on $md${comp:+ ($comp)}: journal only"
    exit 0
fi

body=$hint
[[ -n $excerpt ]] && body="$excerpt"$'\n'"$hint"
if ! "$NOTIFY" "$level" "$title" "$body"; then
    log "notify failed for $event on $md (healthcheck raid check repeats degradation alerts)"
fi
exit 0
