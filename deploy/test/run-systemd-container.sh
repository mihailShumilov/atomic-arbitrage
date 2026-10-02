#!/usr/bin/env bash
# Local check of the deploy kit under a real systemd in ubuntu:24.04 or 26.04
# (Docker on the Mac). No published ports; feed/RPC/Telegram hosts point to 127.0.0.1 in
# the container; the recorder is never started. The container is removed at
# the end.
#
#   bash deploy/test/run-systemd-container.sh [--ubuntu 24.04|26.04] [--build] [--keep]
#     --ubuntu  image version (default 24.04; the server runs 26.04)
#     --build   also run build-on-server.sh (downloads rustup + crates; ~minutes)
#     --keep    do not remove the container (name: hood-deploy-test-boot-<ver>)
#
# Source: committed tree (git archive HEAD) + the working copy of deploy/ and of
# .claude/skills/feed-audit/scripts (bootstrap installs feed_audit.py from there).
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
ubuntu=24.04 build=0 keep=0
while [[ $# -gt 0 ]]; do
    case $1 in
        --ubuntu) ubuntu=${2:?--ubuntu needs a version}; shift ;;
        --build) build=1 ;;
        --keep) keep=1 ;;
        *) echo "unknown: $1" >&2; exit 2 ;;
    esac
    shift
done
[[ $ubuntu =~ ^[0-9]{2}\.[0-9]{2}$ ]] || { echo "bad --ubuntu: $ubuntu" >&2; exit 2; }
NAME=hood-deploy-test-boot-${ubuntu/./}
IMAGE=hood-deploy-test-systemd:$ubuntu

WORK=$(mktemp -d)
chmod 755 "$WORK"   # copied with cp -a; user hood must be able to read the tests
cleanup() {
    rm -rf "$WORK"
    (( keep )) || docker rm -f "$NAME" > /dev/null 2>&1 || true
}
trap cleanup EXIT

git -C "$ROOT" archive HEAD | tar -x -C "$WORK"
rm -rf "$WORK/deploy" && cp -R "$ROOT/deploy" "$WORK/deploy"
rm -rf "$WORK/.claude/skills/feed-audit/scripts" && mkdir -p "$WORK/.claude/skills/feed-audit" &&
    cp -R "$ROOT/.claude/skills/feed-audit/scripts" "$WORK/.claude/skills/feed-audit/scripts"

docker build -q --build-arg UBUNTU="$ubuntu" -t "$IMAGE" -f "$HERE/Dockerfile.systemd" "$HERE" > /dev/null
docker rm -f "$NAME" > /dev/null 2>&1 || true
docker run -d -t --name "$NAME" --hostname hood-test \
    --tmpfs /run --tmpfs /run/lock --cap-add SYS_ADMIN --cap-add NET_ADMIN \
    --cgroupns=host -v /sys/fs/cgroup:/sys/fs/cgroup:rw \
    --security-opt seccomp=unconfined --security-opt apparmor=unconfined \
    --add-host feed.mainnet.chain.robinhood.com:127.0.0.1 \
    --add-host delayed-feed.mainnet.chain.robinhood.com:127.0.0.1 \
    --add-host rpc.mainnet.chain.robinhood.com:127.0.0.1 \
    --add-host api.telegram.org:127.0.0.1 \
    -v "$WORK":/srcro:ro "$IMAGE" > /dev/null
for _ in $(seq 1 30); do
    s=$(docker exec "$NAME" systemctl is-system-running 2>/dev/null || true)
    [[ $s == running || $s == degraded ]] && break
    sleep 1
done
[[ -z $(docker port "$NAME") ]] || { echo "container publishes ports, abort" >&2; exit 1; }
docker exec "$NAME" sh -c 'mkdir -p /opt/hoodchain-mev && cp -a /srcro /opt/hoodchain-mev/src'
# Test-only: chrony.service on 26.04 has ConditionVirtualization=!container and
# is skipped in Docker (24.04 starts it). Clear the condition so bootstrap's
# chrony step runs on both; chronyd-starter.sh adds -x in a container, so the
# clock of the Mac is never touched. Not part of the kit, not on the server.
docker exec "$NAME" sh -c 'mkdir -p /etc/systemd/system/chrony.service.d &&
    printf "[Unit]\nConditionVirtualization=\n" > /etc/systemd/system/chrony.service.d/test-container.conf &&
    systemctl daemon-reload'

x() { docker exec "$NAME" bash -c "$1"; }
# boot: runs bootstrap, shows its WARN/ERROR lines on stderr, prints the change count.
boot() {
    x 'bash /opt/hoodchain-mev/src/deploy/bootstrap.sh' > "$WORK/boot.log" 2>&1 || true
    grep -E 'WARN|ERROR' "$WORK/boot.log" | sed 's/^/      /' >&2 || true
    awk '/done:/ { print $3 }' "$WORK/boot.log"
}
fail=0
# ok DESC_PASS DESC_FAIL CMD... — runs CMD (errexit-safe) and reports.
ok() { local p=$1 f=$2; shift 2; if "$@"; then echo "PASS  $p"; else echo "FAIL  $f"; fail=1; fi; }
not() { ! "$@"; }
expect_changes() { # WANT GOT DESC
    if [[ $2 == "$1" ]]; then echo "PASS  $3: $2 change(s)"; else echo "FAIL  $3: want $1, got $2"; fail=1; fi
}

c1=$(boot); ok "bootstrap run 1: $c1 change(s)" "bootstrap run 1: $c1" test "$c1" -gt 0
x 'touch /tmp/m; sleep 1'
c2=$(boot); expect_changes 0 "$c2" "bootstrap run 2"
mod=$(x 'find / -xdev \( -path /proc -o -path /sys -o -path /run -o -path /tmp -o -path /var/log -o -path /var/lib/systemd -o -path /var/cache \) -prune -o -newer /tmp/m -print 2>/dev/null')
ok "run 2 modified no files" "run 2 modified: $mod" test -z "$mod"

# Task 014: timezone and mdadm steps of bootstrap.
tz=$(x 'timedatectl show -p Timezone --value' || true)
ok "timezone after bootstrap: $tz" "timezone after bootstrap: $tz" test "$tz" = Etc/UTC
x 'timedatectl set-timezone Europe/Berlin'
c=$(boot); expect_changes 1 "$c" "bootstrap with timezone Europe/Berlin"
tz=$(x 'timedatectl show -p Timezone --value' || true)
ok "timezone back to Etc/UTC: $tz" "timezone not reset: $tz" test "$tz" = Etc/UTC
c=$(boot); expect_changes 0 "$c" "bootstrap after the timezone fix"
ok "mdadm drop-in /etc/mdadm/mdadm.conf.d/hood.conf installed" "mdadm drop-in missing" \
    x 'grep -qx "PROGRAM /opt/hoodchain-mev/deploy/mdadm-event.sh" /etc/mdadm/mdadm.conf.d/hood.conf'
# mdadm says "No mail address or alert command - not monitoring" when it has
# neither MAILADDR nor PROGRAM. Without MAILADDR (temporarily), the message
# must appear without the drop-in and disappear with it: mdadm reads PROGRAM
# from mdadm.conf.d. No arrays in the container, so nothing else happens.
# shellcheck disable=SC2016  # expands inside the container
ok "mdadm reads PROGRAM from mdadm.conf.d (control: message without the drop-in)" "mdadm ignores the drop-in" \
    x 'cp -p /etc/mdadm/mdadm.conf /tmp/mdadm.conf.saved && sed -i "/^MAILADDR/d" /etc/mdadm/mdadm.conf
       with=$(mdadm --monitor --scan --oneshot 2>&1)
       mv /etc/mdadm/mdadm.conf.d/hood.conf /tmp/hood.conf.saved
       without=$(mdadm --monitor --scan --oneshot 2>&1)
       mv /tmp/hood.conf.saved /etc/mdadm/mdadm.conf.d/hood.conf && mv /tmp/mdadm.conf.saved /etc/mdadm/mdadm.conf
       ! grep -q "not monitoring" <<< "$with" && grep -q "not monitoring" <<< "$without"'
ok "mdadm-event.sh TestMessage reaches journald through notify.sh" "mdadm-event.sh -> journald" \
    x '/opt/hoodchain-mev/deploy/mdadm-event.sh TestMessage /dev/md/0 && sleep 1 &&
       journalctl -t hood-notify -o cat --no-pager | grep -q "^\[INFO\] hood-test: RAID: тестовое сообщение mdadm для /dev/md/0"'

# Task 015: smartd. Installed by bootstrap from apt; our config via the drop-in.
# shellcheck disable=SC2016  # expands inside the container
ok "smartmontools installed, smartd-event.sh and smartd-test.conf in place" "smartmontools / hook missing" \
    x 'dpkg-query -W -f="\${db:Status-Status}" smartmontools | grep -qx installed &&
       test -x /opt/hoodchain-mev/deploy/smartd-event.sh && test -f /opt/hoodchain-mev/deploy/smartd-test.conf'
ok "smartd ExecStart reads /etc/hoodchain/smartd.conf (drop-in)" "smartd drop-in not applied" \
    x 'systemctl show -p ExecStart --value smartmontools.service | grep -q -- "-c /etc/hoodchain/smartd.conf" &&
       cmp -s /etc/hoodchain/smartd.conf /opt/hoodchain-mev/src/deploy/smartd-hood.conf'
# shellcheck disable=SC2016  # expands inside the container
ok "packaged /etc/smartd.conf left as shipped (dpkg conffile)" "/etc/smartd.conf modified" \
    x 'dpkg-query -W -f="\${Conffiles}\n" smartmontools | awk "\$1 == \"/etc/smartd.conf\" { print \$2 \"  \" \$1 }" | md5sum -c --quiet'
# shellcheck disable=SC2016  # expands inside the container
ok "smartd.conf and smartd-test.conf parse (smartd stops only for lack of disks)" "smartd config error" \
    x 'for c in /etc/hoodchain/smartd.conf /opt/hoodchain-mev/deploy/smartd-test.conf; do
           out=$(smartd -q onecheck -s - -c "$c" 2>&1)
           grep -q "was parsed, found DEVICESCAN" <<< "$out" && ! grep -qi "syntax" <<< "$out" || exit 1
       done'
# In Docker the unit is skipped (ConditionVirtualization=no), as on any VM:
# healthcheck must say smartd is not running.
sstate=$(x 'systemctl is-active smartmontools.service' || true)
ok "smartd not active in a container ($sstate): healthcheck ALERT smartd" "no smartd ALERT" \
    x 'systemctl start healthcheck.service && sleep 1 &&
       journalctl -t hood-notify -o cat --no-pager | grep -q "^\[ALERT\] hood-test: smartd не работает (systemd: inactive)"'
# Test-only: clear the condition so the daemon really starts with our
# ExecStart; with no disks smartd exits 17 after reading our config.
x 'mkdir -p /etc/systemd/system/smartmontools.service.d &&
   printf "[Unit]\nConditionVirtualization=\n" > /etc/systemd/system/smartmontools.service.d/test-container.conf &&
   systemctl daemon-reload; systemctl start smartmontools.service 2>/dev/null; sleep 1' || true
ok "smartd under systemd opens /etc/hoodchain/smartd.conf" "smartd did not read our config" \
    x 'journalctl -u smartmontools.service -o cat --no-pager | grep -q "Opened configuration file /etc/hoodchain/smartd.conf"'
x 'rm -f /etc/systemd/system/smartmontools.service.d/test-container.conf && systemctl daemon-reload && systemctl reset-failed smartmontools.service' || true
# shellcheck disable=SC2016  # PIPESTATUS must expand inside the container
ok "test-smartd-event.sh incl. the real smartd_warning.sh" "test-smartd-event.sh" \
    x 'bash /opt/hoodchain-mev/src/deploy/test/test-smartd-event.sh | tail -n 1; exit "${PIPESTATUS[0]}"'
# What smartd does on "-M test": env + smartd_warning.sh -> hook -> notify.sh -> journald.
# shellcheck disable=SC2016  # expands inside the container
ok "smartd_warning.sh -> smartd-event.sh -> notify.sh -> journald (EmailTest)" "smartd chain -> journald" \
    x 'out=$(env -i PATH=/usr/bin:/bin SMARTD_MAILER=/opt/hoodchain-mev/deploy/smartd-event.sh SMARTD_ADDRESS= \
           SMARTD_FAILTYPE=EmailTest SMARTD_MESSAGE="TEST EMAIL from smartd for device: /dev/sdb [SAT]" SMARTD_PREVCNT=0 \
           SMARTD_NEXTDAYS= SMARTD_DEVICE=/dev/sdb SMARTD_DEVICESTRING="/dev/sdb [SAT]" SMARTD_DEVICETYPE=auto \
           SMARTD_DEVICEINFO=ST4000NM0245-1Z2107 SMARTD_SUBJECT= sh /usr/share/smartmontools/smartd_warning.sh 2>&1) &&
       [ -z "$out" ] && sleep 1 &&
       journalctl -t hood-notify -o cat --no-pager | grep -q "^\[INFO\] hood-test: SMART: тестовое сообщение smartd для /dev/sdb \[SAT\]"'
c=$(boot); expect_changes 0 "$c" "bootstrap after the smartd checks"

if (( build )); then
    x 'bash /opt/hoodchain-mev/src/deploy/build-on-server.sh' | grep '^\[build\]'
    c3=$(boot); expect_changes 2 "$c3" "bootstrap run 3 (enables 2 timers)"
    c4=$(boot); expect_changes 0 "$c4" "bootstrap run 4"
    units='recorder.service healthcheck.service healthcheck.timer feed-audit.service feed-audit.timer backup.service backup.timer enricher-gaps.service enricher-gaps.timer notify-failure@x.service'
    ok "systemd-analyze verify (all units)" "verify" x "cd /etc/systemd/system && systemd-analyze verify $units"
else
    # Without binaries verify reports only the two missing ExecStart binaries.
    out=$(x 'cd /etc/systemd/system && systemd-analyze verify recorder.service healthcheck.service healthcheck.timer feed-audit.service feed-audit.timer backup.service backup.timer enricher-gaps.service enricher-gaps.timer notify-failure@x.service 2>&1' || true)
    other=$(grep -v 'is not executable: No such file or directory' <<< "$out" || true)
    ok "systemd-analyze verify (only missing binaries reported)" "verify: $other" test -z "$other"
fi

ok "healthcheck.service runs under systemd as hood" "healthcheck.service" x 'systemctl start healthcheck.service'
x 'journalctl -u healthcheck.service -o cat --no-pager | grep "^healthcheck:" | tail -n 1'
ok "enricher-gaps refuses to start without /etc/hoodchain/enricher.env" "enricher-gaps ran without enricher.env" \
    not x 'systemctl start enricher-gaps.service 2>/dev/null'
# shellcheck disable=SC2016  # PIPESTATUS must expand inside the container
ok "backup test (rclone, local remote) as hood" "backup test" \
    x 'runuser -u hood -- bash /opt/hoodchain-mev/src/deploy/test/test-backup.sh | tail -n 1; exit "${PIPESTATUS[0]}"'
# python3 and zstd come from bootstrap, so the cases against the real feed_audit.py run too.
# shellcheck disable=SC2016  # PIPESTATUS must expand inside the container
ok "test-feed-audit-daily.sh incl. the real feed_audit.py, as hood" "test-feed-audit-daily.sh" \
    x 'runuser -u hood -- bash /opt/hoodchain-mev/src/deploy/test/test-feed-audit-daily.sh | tail -n 1; exit "${PIPESTATUS[0]}"'
ok "chrony active after bootstrap" "chrony not active" x 'systemctl is-active --quiet chrony'
# shellcheck disable=SC2016  # $(...) must expand inside the container
ok "ufw active, only ssh allowed in" "ufw rules" \
    x 'ufw status | grep -q "^Status: active" && [ "$(ufw show added | grep -c "^ufw ")" = 1 ] && ufw show added | grep -q "^ufw limit 22/tcp"'
rstate=$(x 'systemctl is-active recorder.service' || true)
ok "recorder never started ($rstate)" "recorder is $rstate" test "$rstate" = inactive

if (( fail )); then echo "SOME CHECKS FAILED"; exit 1; fi
echo "ALL PASS"
