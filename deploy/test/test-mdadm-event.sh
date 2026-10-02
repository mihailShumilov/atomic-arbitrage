#!/usr/bin/env bash
# Offline test of deploy/mdadm-event.sh (mdadm --monitor PROGRAM hook): the
# notifier is a fake that appends "LEVEL|TITLE|BODY", logger is a shim, and
# /proc/mdstat is a fake copy. No mdadm, no systemd, no network.
#
#   bash deploy/test/test-mdadm-event.sh     (Linux, bash 4+)
# check() (lib.sh) evals its single-quoted expression later (SC2016); rc is
# read there too (SC2034).
# shellcheck disable=SC2016,SC2034
set -uo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source-path=SCRIPTDIR source=lib.sh
. "$HERE/lib.sh"
H=$HERE/../mdadm-event.sh
t_init
t_shim_logger
t_fake_notify
# Degraded md2 (sdb3 failed), healthy md1: the excerpt must contain md2 only.
cat > "$T/mdstat" <<'EOF'
Personalities : [raid1]
md2 : active raid1 sda3[0] sdb3[1](F)
      2111699968 blocks super 1.2 [2/1] [U_]
      bitmap: 16/16 pages [64KB], 65536KB chunk

md1 : active raid1 sda2[0] sdb2[1]
      1046528 blocks super 1.2 [2/2] [UU]

unused devices: <none>
EOF
export MDADM_EVENT_NOTIFY=$T/bin/fake-notify MDADM_EVENT_MDSTAT=$T/mdstat

t_diag() { sed 's/^/      n: /' "$T_N" 2> /dev/null; sed 's/^/      log: /' "$T_LOG" 2> /dev/null; }
ev() { : > "$T_N"; : > "$T_LOG"; bash "$H" "$@"; rc=$?; }

ev Fail /dev/md/2 /dev/sdb3
check "Fail: exit 0" '[[ $rc -eq 0 ]]'
check "Fail: one ALERT with array and disk" '[[ $(wc -l < "$T_N") -eq 1 ]] && grep -q "^alert|RAID: диск выпал из массива /dev/md/2, диск /dev/sdb3|" "$T_N"'
check "Fail: body has the md2 excerpt from mdstat, not md1" 'grep -q "md2 : active raid1 sda3\[0\] sdb3\[1\](F) / 2111699968 blocks super 1.2 \[2/1\] \[U_\]" "$T_N" && ! grep -q "md1 :" "$T_N"'
check "Fail: body says do not reboot / do not touch recorder" 'grep -q "не перезагружать и recorder не трогать" "$T_N"'

ev DegradedArray /dev/md2
check "DegradedArray (/dev/md2 form): ALERT with excerpt" 'grep -q "^alert|RAID деградирован: /dev/md2|md2 : active" "$T_N"'

ev FailSpare /dev/md/2 /dev/sdc3
check "FailSpare: ALERT" 'grep -q "^alert|RAID: запасной диск отказал" "$T_N"'
ev DeviceDisappeared /dev/md/9
check "DeviceDisappeared (array not in mdstat): ALERT, no excerpt" 'grep -q "^alert|RAID: массив /dev/md/9 пропал|cat /proc/mdstat" "$T_N"'
ev SpareActive /dev/md/2 /dev/sdb3
check "SpareActive: ALERT" 'grep -q "^alert|RAID: в /dev/md/2 активирован запасной/новый диск, диск /dev/sdb3" "$T_N"'

ev RebuildFinished /dev/md/1
check "RebuildFinished: one INFO" '[[ $(wc -l < "$T_N") -eq 1 ]] && grep -q "^info|RAID: синхронизация /dev/md/1 завершена|md1 : active" "$T_N"'
check "RebuildFinished: no reboot warning in an INFO" '! grep -q "перезагружать" "$T_N"'
ev TestMessage /dev/md/1
check "TestMessage: INFO" 'grep -q "^info|RAID: тестовое сообщение mdadm для /dev/md/1|" "$T_N"'

for e in RebuildStarted Rebuild20 Rebuild80 NewArray MoveSpare SparesMissing; do
    ev "$e" /dev/md/3
    check "$e: journal only, no notification" '[[ ! -s $T_N ]] && grep -q "hood-mdadm .*event '"$e"' on /dev/md/3: journal only" "$T_LOG"'
done

ev
check "no arguments: exit 0, logged" '[[ $rc -eq 0 && ! -s $T_N ]] && grep -q "called without arguments" "$T_LOG"'

FAKE_NOTIFY_FAIL=1 ev Fail /dev/md/2 /dev/sdb3
check "notifier failing: exit 0, failure logged" '[[ $rc -eq 0 ]] && grep -q "notify failed for Fail on /dev/md/2" "$T_LOG"'

MDADM_EVENT_MDSTAT=$T/missing ev DegradedArray /dev/md/2
check "no mdstat: ALERT still sent" 'grep -q "^alert|RAID деградирован: /dev/md/2|cat /proc/mdstat" "$T_N"'

t_result
