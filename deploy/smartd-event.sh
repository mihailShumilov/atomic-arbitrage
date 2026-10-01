#!/usr/bin/env bash
# hoodchain-mev: smartd warning hook -> notify.sh (task 015).
#
# smartd.conf: ... -m <nomailer> -M exec /opt/hoodchain-mev/deploy/smartd-event.sh
# smartd runs its smartd_warning.sh, which execs this script (stdin
# /dev/null) with the SMARTD_* environment: SMARTD_FAILTYPE, SMARTD_DEVICE,
# SMARTD_DEVICESTRING, SMARTD_MESSAGE, SMARTD_DEVICEINFO, SMARTD_PREVCNT,
# SMARTD_TFIRST, SMARTD_NEXTDAYS (smartmontools 7.4/7.5, smartd.cpp MailWarning).
#
#   INFO   EmailTest (-M test, smartd-test.conf)
#   ALERT  everything else: Health, Usage (e.g. -R 5! reallocated sectors grew),
#          SelfTest, ErrorCount, CurrentPendingSector, OfflineUncorrectableSector,
#          Temperature, FailedHealthCheck, FailedReadSmart*, FailedOpenDevice
#
# Must stay silent on stdout/stderr: smartd logs any output as "unexpected
# output" at LOG_CRIT. Always exits 0; a failed notification is logged under
# the tag hood-smartd (smartd itself repeats per -M diminishing).
set -uo pipefail

NOTIFY=${SMARTD_EVENT_NOTIFY:-/opt/hoodchain-mev/deploy/notify.sh}

log() { logger -t hood-smartd -p user.info -- "$*" > /dev/null 2>&1 || true; }

type=${SMARTD_FAILTYPE:-}
dev=${SMARTD_DEVICE:-}
disk=${SMARTD_DEVICESTRING:-${dev:-?}}
msg=${SMARTD_MESSAGE:-}
info=${SMARTD_DEVICEINFO:-}
prev=${SMARTD_PREVCNT:-0}
next=${SMARTD_NEXTDAYS:-}

if [[ -z $type ]]; then
    log "called without SMARTD_FAILTYPE (not from smartd?)"
    exit 0
fi

check="smartctl -x ${dev:-<диск>}; cat /proc/mdstat"
hint="$check. Сервер не перезагружать и recorder не трогать. Пока RAID1 полный, данные есть на втором диске; при выпадении диска придёт RAID-алерт. Замена диска — через Hetzner Robot (решение Михаила), к заявке приложить smartctl -x."
level=alert
case $type in
    EmailTest)
        level=info
        title="SMART: тестовое сообщение smartd для $disk"
        hint="Проверка цепочки smartd -> notify.sh, действий не нужно." ;;
    Health)            title="SMART: диск $disk сообщает об отказе (health FAILED)" ;;
    FailedHealthCheck) title="SMART: smartd не может проверить здоровье диска $disk" ;;
    Usage)             title="SMART: атрибут диска $disk изменился (переназначенные сектора или порог)" ;;
    CurrentPendingSector)       title="SMART: на диске $disk есть сектора, ожидающие переназначения" ;;
    OfflineUncorrectableSector) title="SMART: на диске $disk есть неисправимые сектора" ;;
    SelfTest)          title="SMART: самотест диска $disk завершился с ошибкой" ;;
    ErrorCount)        title="SMART: растёт журнал ошибок диска $disk" ;;
    Temperature)       title="SMART: диск $disk перегрет"
                       hint="$check. Перегрев — вопрос к Hetzner (охлаждение); сервер не перезагружать и recorder не трогать." ;;
    FailedOpenDevice)  title="SMART: smartd не может открыть диск $disk (диск пропал?)" ;;
    FailedReadSmart*)  title="SMART: не читаются данные SMART диска $disk ($type)" ;;
    *)                 title="SMART: $type на диске $disk" ;;
esac

body=""
[[ -n $msg ]] && body=$msg
[[ -n $info ]] && body+=${body:+$'\n'}"Диск: $info"
if [[ $level == alert ]]; then
    if [[ $prev =~ ^[0-9]+$ ]] && (( prev > 0 )); then
        body+=${body:+$'\n'}"Повтор №$((prev + 1)), первое сообщение ${SMARTD_TFIRST:-?}."
    fi
    if [[ $next =~ ^[0-9]+$ ]] && (( next > 0 )); then
        body+=${body:+$'\n'}"Следующее напоминание через $next сут., если проблема останется."
    fi
fi
body+=${body:+$'\n'}$hint

if ! "$NOTIFY" "$level" "$title" "$body" > /dev/null 2>&1; then
    log "notify failed for $type on $disk"
fi
exit 0
