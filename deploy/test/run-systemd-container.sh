#!/usr/bin/env bash
# Local check of the deploy kit under a real systemd in ubuntu:24.04 (Docker on
# the Mac). No published ports; feed/RPC/Telegram hosts point to 127.0.0.1 in
# the container; the recorder is never started. The container is removed at
# the end.
#
#   bash deploy/test/run-systemd-container.sh [--build] [--keep]
#     --build   also run build-on-server.sh (downloads rustup + crates; ~minutes)
#     --keep    do not remove the container (name: hood-deploy-test-boot)
#
# Source: committed tree (git archive HEAD) + the working copy of deploy/.
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
NAME=hood-deploy-test-boot
IMAGE=hood-deploy-test-systemd:24.04
build=0 keep=0
for a in "$@"; do
    case $a in --build) build=1 ;; --keep) keep=1 ;; *) echo "unknown: $a" >&2; exit 2 ;; esac
done

WORK=$(mktemp -d)
chmod 755 "$WORK"   # copied with cp -a; user hood must be able to read the tests
cleanup() {
    rm -rf "$WORK"
    (( keep )) || docker rm -f "$NAME" > /dev/null 2>&1 || true
}
trap cleanup EXIT

git -C "$ROOT" archive HEAD | tar -x -C "$WORK"
rm -rf "$WORK/deploy" && cp -R "$ROOT/deploy" "$WORK/deploy"

docker build -q -t "$IMAGE" -f "$HERE/Dockerfile.systemd" "$HERE" > /dev/null
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

x() { docker exec "$NAME" bash -c "$1"; }
boot() { x 'bash /opt/hoodchain-mev/src/deploy/bootstrap.sh' | tee /dev/stderr | awk '/done:/ { print $3 }'; }
fail=0
# ok DESC_PASS DESC_FAIL CMD... — runs CMD (errexit-safe) and reports.
ok() { local p=$1 f=$2; shift 2; if "$@"; then echo "PASS  $p"; else echo "FAIL  $f"; fail=1; fi; }
not() { ! "$@"; }
expect_changes() { # WANT GOT DESC
    if [[ $2 == "$1" ]]; then echo "PASS  $3: $2 change(s)"; else echo "FAIL  $3: want $1, got $2"; fail=1; fi
}

c1=$(boot 2>/dev/null); ok "bootstrap run 1: $c1 change(s)" "bootstrap run 1: $c1" test "$c1" -gt 0
x 'touch /tmp/m; sleep 1'
c2=$(boot 2>/dev/null); expect_changes 0 "$c2" "bootstrap run 2"
mod=$(x 'find / -xdev \( -path /proc -o -path /sys -o -path /run -o -path /tmp -o -path /var/log -o -path /var/lib/systemd -o -path /var/cache \) -prune -o -newer /tmp/m -print 2>/dev/null')
ok "run 2 modified no files" "run 2 modified: $mod" test -z "$mod"

if (( build )); then
    x 'bash /opt/hoodchain-mev/src/deploy/build-on-server.sh' | grep '^\[build\]'
    c3=$(boot 2>/dev/null); expect_changes 2 "$c3" "bootstrap run 3 (enables 2 timers)"
    c4=$(boot 2>/dev/null); expect_changes 0 "$c4" "bootstrap run 4"
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
rstate=$(x 'systemctl is-active recorder.service' || true)
ok "recorder never started ($rstate)" "recorder is $rstate" test "$rstate" = inactive

if (( fail )); then echo "SOME CHECKS FAILED"; exit 1; fi
echo "ALL PASS"
