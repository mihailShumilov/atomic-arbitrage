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
# Source: committed tree (git archive HEAD) + the working copy of deploy/.
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
ok "chrony active after bootstrap" "chrony not active" x 'systemctl is-active --quiet chrony'
# shellcheck disable=SC2016  # $(...) must expand inside the container
ok "ufw active, only ssh allowed in" "ufw rules" \
    x 'ufw status | grep -q "^Status: active" && [ "$(ufw show added | grep -c "^ufw ")" = 1 ] && ufw show added | grep -q "^ufw limit 22/tcp"'
rstate=$(x 'systemctl is-active recorder.service' || true)
ok "recorder never started ($rstate)" "recorder is $rstate" test "$rstate" = inactive

if (( fail )); then echo "SOME CHECKS FAILED"; exit 1; fi
echo "ALL PASS"
