#!/usr/bin/env bash
# Offline test of deploy/smartd-event.sh (smartd -M exec hook): the notifier is
# a fake that appends "LEVEL|TITLE|BODY", logger is a shim, the SMARTD_*
# environment is what smartd 7.4/7.5 sets (smartd.cpp, MailWarning). No
# smartd, no disks, no systemd, no network.
#
# If smartmontools is installed (/usr/share/smartmontools/smartd_warning.sh,
# or SMARTD_WARNING_SH), the hook is also run through the real warning
# script exactly as smartd does it (-m <nomailer>: SMARTD_ADDRESS empty), and
# the output smartd would log as "unexpected output" must be empty.
#
#   bash deploy/test/test-smartd-event.sh     (Linux, bash 4+)
# check() (lib.sh) evals its single-quoted expression later (SC2016); rc and
# out are read there too (SC2034).
# shellcheck disable=SC2016,SC2034
set -uo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source-path=SCRIPTDIR source=lib.sh
. "$HERE/lib.sh"
H=$HERE/../smartd-event.sh
t_init
t_shim_logger
t_fake_notify --noisy
export SMARTD_EVENT_NOTIFY=$T/bin/fake-notify

out=""
t_diag() { sed 's/^/      n: /' "$T_N" 2> /dev/null; sed 's/^/      log: /' "$T_LOG" 2> /dev/null; printf '      out: %s\n' "$out"; }

# ev FAILTYPE MESSAGE [PREVCNT] [NEXTDAYS]: run the hook with smartd's environment.
ev() {
    : > "$T_N"; : > "$T_LOG"
    out=$(env -i PATH="$PATH" T_LOG="$T_LOG" T_N="$T_N" SMARTD_EVENT_NOTIFY="$SMARTD_EVENT_NOTIFY" \
        FAKE_NOTIFY_FAIL="${FAKE_NOTIFY_FAIL:-0}" \
        SMARTD_MAILER="$H" SMARTD_FAILTYPE="$1" SMARTD_MESSAGE="$2" \
        SMARTD_PREVCNT="${3:-0}" SMARTD_NEXTDAYS="${4-1}" \
        SMARTD_TFIRST="Thu Oct  1 17:00:00 2026 UTC" SMARTD_TFIRSTEPOCH=1790874000 \
        SMARTD_DEVICE=/dev/sda SMARTD_DEVICESTRING="/dev/sda [SAT]" SMARTD_DEVICETYPE=auto \
        SMARTD_DEVICEINFO="ST4000NM0245-1Z2107, S/N:TESTSERIAL, WWN:5-000c50-000000000, FW:SN05, 4.00 TB" \
        SMARTD_SUBJECT="SMART error ($1) detected on host: hood-test" \
        bash "$H" < /dev/null 2>&1)
    rc=$?
}

ev EmailTest "TEST EMAIL from smartd for device: /dev/sda [SAT]" 0 ""
check "EmailTest: exit 0, silent" '[[ $rc -eq 0 && -z $out ]]'
check "EmailTest: one INFO with the disk" '[[ $(wc -l < "$T_N") -eq 1 ]] && grep -q "^info|SMART: тестовое сообщение smartd для /dev/sda \[SAT\]|TEST EMAIL from smartd for device: /dev/sda \[SAT\] / Диск: ST4000NM0245-1Z2107" "$T_N"'
check "EmailTest: no reboot warning, no repeat line" '! grep -qE "перезагружать|Повтор|напоминание" "$T_N"'

ev Usage "Device: /dev/sda [SAT], SMART Prefailure Attribute: 5 Reallocated_Sector_Ct changed from 100 [Raw 0] to 100 [Raw 8]"
check "Usage (-R 5!: reallocated sectors grew): exit 0, silent" '[[ $rc -eq 0 && -z $out ]]'
check "Usage: one ALERT, smartd message in the body" '[[ $(wc -l < "$T_N") -eq 1 ]] && grep -q "^alert|SMART: атрибут диска /dev/sda \[SAT\] изменился (переназначенные сектора или порог)|Device: /dev/sda \[SAT\], SMART Prefailure Attribute: 5 Reallocated_Sector_Ct changed from 100 \[Raw 0\] to 100 \[Raw 8\]" "$T_N"'
check "Usage: hint with smartctl -x /dev/sda, do not reboot, Hetzner Robot" 'grep -q "smartctl -x /dev/sda; cat /proc/mdstat. Сервер не перезагружать и recorder не трогать" "$T_N" && grep -q "Hetzner Robot" "$T_N"'
check "Usage: next reminder line (-M diminishing, NEXTDAYS=1)" 'grep -q "Следующее напоминание через 1 сут." "$T_N" && ! grep -q "Повтор" "$T_N"'

ev Usage "Device: /dev/sda [SAT], SMART Prefailure Attribute: 5 Reallocated_Sector_Ct changed from 100 [Raw 8] to 99 [Raw 24]" 2 4
check "Usage repeated (PREVCNT=2): repeat number and first time" 'grep -q "Повтор №3, первое сообщение Thu Oct  1 17:00:00 2026 UTC." "$T_N" && grep -q "через 4 сут." "$T_N"'

ev Health "Device: /dev/sda [SAT], FAILED SMART self-check. BACK UP DATA NOW!"
check "Health: ALERT" 'grep -q "^alert|SMART: диск /dev/sda \[SAT\] сообщает об отказе (health FAILED)|Device: /dev/sda \[SAT\], FAILED SMART self-check" "$T_N"'
ev CurrentPendingSector "Device: /dev/sda [SAT], 8 Currently unreadable (pending) sectors"
check "CurrentPendingSector: ALERT" 'grep -q "^alert|SMART: на диске /dev/sda \[SAT\] есть сектора, ожидающие переназначения|" "$T_N"'
ev OfflineUncorrectableSector "Device: /dev/sda [SAT], 8 Offline uncorrectable sectors"
check "OfflineUncorrectableSector: ALERT" 'grep -q "^alert|SMART: на диске /dev/sda \[SAT\] есть неисправимые сектора|" "$T_N"'
ev SelfTest "Device: /dev/sda [SAT], new Self-Test Log error at hour timestamp 41234"
check "SelfTest: ALERT" 'grep -q "^alert|SMART: самотест диска /dev/sda \[SAT\] завершился с ошибкой|" "$T_N"'
ev ErrorCount "Device: /dev/sda [SAT], ATA error count increased from 0 to 3"
check "ErrorCount: ALERT" 'grep -q "^alert|SMART: растёт журнал ошибок диска /dev/sda \[SAT\]|" "$T_N"'
ev Temperature "Device: /dev/sda [SAT], Temperature 56 Celsius reached critical limit of 55 Celsius (Min/Max 31/56)"
check "Temperature: ALERT with the cooling hint" 'grep -q "^alert|SMART: диск /dev/sda \[SAT\] перегрет|.*Перегрев — вопрос к Hetzner" "$T_N"'
ev FailedOpenDevice "Device: /dev/sda [SAT], unable to open ATA device"
check "FailedOpenDevice: ALERT" 'grep -q "^alert|SMART: smartd не может открыть диск /dev/sda \[SAT\] (диск пропал?)|" "$T_N"'
ev FailedReadSmartData "Device: /dev/sda [SAT], failed to read SMART Attribute Data"
check "FailedReadSmartData: ALERT with the type" 'grep -q "^alert|SMART: не читаются данные SMART диска /dev/sda \[SAT\] (FailedReadSmartData)|" "$T_N"'
ev SomethingNew "Device: /dev/sda [SAT], future failure type"
check "unknown type: ALERT, not dropped" 'grep -q "^alert|SMART: SomethingNew на диске /dev/sda \[SAT\]|" "$T_N"'

: > "$T_N"; : > "$T_LOG"
out=$(env -i PATH="$PATH" T_LOG="$T_LOG" T_N="$T_N" SMARTD_EVENT_NOTIFY="$SMARTD_EVENT_NOTIFY" bash "$H" < /dev/null 2>&1); rc=$?
check "no SMARTD_FAILTYPE: exit 0, silent, logged, no notification" '[[ $rc -eq 0 && -z $out && ! -s $T_N ]] && grep -q "hood-smartd .*called without SMARTD_FAILTYPE" "$T_LOG"'

FAKE_NOTIFY_FAIL=1 ev Health "Device: /dev/sda [SAT], FAILED SMART self-check. BACK UP DATA NOW!"
check "notifier failing: exit 0, silent (stderr not leaked to smartd), failure logged" '[[ $rc -eq 0 && -z $out ]] && grep -q "notify failed for Health on /dev/sda \[SAT\]" "$T_LOG"'

# ---- through the real smartd_warning.sh (only where smartmontools is installed) ---
W=${SMARTD_WARNING_SH:-/usr/share/smartmontools/smartd_warning.sh}
if [[ -r $W ]]; then
    : > "$T_N"; : > "$T_LOG"
    # smartd sets these, then popen()s "smartd_warning.sh 2>&1" and logs any output.
    out=$(env -i PATH=/usr/bin:/bin T_LOG="$T_LOG" T_N="$T_N" SMARTD_EVENT_NOTIFY="$SMARTD_EVENT_NOTIFY" \
        SMARTD_MAILER="$H" SMARTD_ADDRESS="" SMARTD_FAILTYPE=EmailTest \
        SMARTD_MESSAGE="TEST EMAIL from smartd for device: /dev/sdb [SAT]" SMARTD_PREVCNT=0 \
        SMARTD_TFIRST="" SMARTD_TFIRSTEPOCH=0 SMARTD_NEXTDAYS="" SMARTD_DEVICE=/dev/sdb \
        SMARTD_DEVICESTRING="/dev/sdb [SAT]" SMARTD_DEVICETYPE=auto SMARTD_DEVICEINFO="ST4000NM0245-1Z2107" \
        SMARTD_SUBJECT="" sh "$W" 2>&1); rc=$?
    # The real warning script resets PATH to /usr/local/bin:/usr/bin:/bin, so the
    # logger shim is not used here: only the notifier result is checked.
    check "smartd_warning.sh -> hook: rc 0, no output for smartd's log" '[[ $rc -eq 0 && -z $out ]]'
    check "smartd_warning.sh -> hook: one INFO for /dev/sdb" '[[ $(wc -l < "$T_N") -eq 1 ]] && grep -q "^info|SMART: тестовое сообщение smartd для /dev/sdb \[SAT\]|" "$T_N"'
else
    echo "SKIP  smartd_warning.sh not installed ($W)"
fi

t_result
