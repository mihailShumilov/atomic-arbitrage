#!/usr/bin/env bash
# hoodchain-mev: idempotent bootstrap of a clean Ubuntu 24.04 host (run as root)
# for the feed recorder. Safe to re-run: every step checks before changing,
# and the summary line counts real changes (a second run must print 0).
#
#   bash deploy/bootstrap.sh [--with-docker] [--ssh-port N]... [--no-firewall]
#
# Run from the source checkout (default /opt/hoodchain-mev/src). It installs:
#   packages   chrony ufw zstd python3 curl ca-certificates rclone
#              (+ docker.io docker-compose-v2 with --with-docker)
#   user       hood (system, no login, home /opt/hoodchain-mev)
#   dirs       /opt/hoodchain-mev/{bin,deploy}, /srv/hood/data/{feed,blocks,logs},
#              /srv/hood/reports, /etc/hoodchain, /var/lib/hoodchain/{health,backup}
#   scripts    deploy/*.sh, feed_audit.py, README -> /opt/hoodchain-mev/deploy
#   units      recorder, healthcheck(+timer), feed-audit(+timer),
#              backup(+timer), enricher-gaps(+timer), notify-failure@
#   config     journald limits, needrestart exclusion for recorder,
#              /etc/hoodchain/*.example (real secrets are created by hand)
#   chrony     enabled, sync checked (warning only)
#   ufw        deny incoming except ssh (rate-limited), allow outgoing
#
# It never starts or restarts the recorder, never enables backup.timer or
# enricher-gaps.timer, and makes no connection to the feed or any RPC.
# healthcheck.timer and feed-audit.timer are enabled only once
# /opt/hoodchain-mev/bin/recorder exists (run build-on-server.sh, then re-run
# this script).
set -euo pipefail

PREFIX=/opt/hoodchain-mev
DATA=/srv/hood
ETC=/etc/hoodchain
STATE=/var/lib/hoodchain
UNIT_DIR=/etc/systemd/system

SRC=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
DEPLOY_SRC=$SRC/deploy
AUDIT_SRC=$SRC/.claude/skills/feed-audit/scripts/feed_audit.py

with_docker=0
firewall=1
ssh_ports=()
while [[ $# -gt 0 ]]; do
    case $1 in
        --with-docker) with_docker=1 ;;
        --no-firewall) firewall=0 ;;
        --ssh-port)
            [[ ${2:-} =~ ^[0-9]+$ ]] || { echo "--ssh-port needs a number" >&2; exit 2; }
            ssh_ports+=("$2"); shift ;;
        -h|--help) sed -n '2,30p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done

changes=0
warnings=0
say() { printf '[bootstrap] %s\n' "$*"; }
changed() { changes=$((changes + 1)); say "CHANGED: $*"; }
warn() { warnings=$((warnings + 1)); say "WARN: $*"; }
die() { say "ERROR: $*"; exit 1; }

[[ $EUID -eq 0 ]] || die "run as root"
[[ -d /run/systemd/system ]] || die "systemd is not running (PID 1)"
[[ -f $DEPLOY_SRC/recorder.service ]] || die "run from the source checkout: $DEPLOY_SRC/recorder.service not found"
[[ -f $AUDIT_SRC ]] || die "$AUDIT_SRC not found"
# shellcheck source=/dev/null
. /etc/os-release
[[ ${ID:-} == ubuntu && ${VERSION_ID:-} == 24.04 ]] ||
    warn "tested on Ubuntu 24.04 only, this is ${PRETTY_NAME:-unknown}"

# ------------------------------------------------------------- packages ---
pkgs=(chrony ufw zstd python3 curl ca-certificates rclone)
(( firewall )) || pkgs=(chrony zstd python3 curl ca-certificates rclone)
(( with_docker )) && pkgs+=(docker.io docker-compose-v2)
missing=()
for p in "${pkgs[@]}"; do
    if [[ $(dpkg-query -W -f='${db:Status-Status}' "$p" 2>/dev/null) != installed ]]; then
        missing+=("$p")
    fi
done
if (( ${#missing[@]} )); then
    say "installing: ${missing[*]}"
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -q
    apt-get install -y -q --no-install-recommends "${missing[@]}"
    changed "packages installed: ${missing[*]}"
else
    say "packages: ok"
fi

# ----------------------------------------------------------------- user ---
if ! id -u hood > /dev/null 2>&1; then
    useradd --system --user-group --home-dir "$PREFIX" --no-create-home \
        --shell /usr/sbin/nologin hood
    changed "user hood created"
else
    say "user hood: ok"
fi

# ------------------------------------------------------------ directories ---
# ensure_dir PATH OWNER GROUP MODE(octal, no leading 0)
ensure_dir() {
    local p=$1 o=$2 g=$3 m=$4
    if [[ ! -d $p ]]; then
        install -d -o "$o" -g "$g" -m "$m" "$p"
        changed "dir $p"
    elif [[ $(stat -c '%U:%G:%a' "$p") != "$o:$g:$m" ]]; then
        chown "$o:$g" "$p"
        chmod "$m" "$p"
        changed "dir $p owner/mode -> $o:$g $m"
    fi
}
ensure_dir "$PREFIX" root root 755
ensure_dir "$PREFIX/bin" root root 755
ensure_dir "$PREFIX/deploy" root root 755
ensure_dir "$DATA" root root 755
ensure_dir "$DATA/data" hood hood 750
ensure_dir "$DATA/data/feed" hood hood 750
ensure_dir "$DATA/data/blocks" hood hood 750
ensure_dir "$DATA/data/logs" hood hood 750
ensure_dir "$DATA/reports" hood hood 750
ensure_dir "$ETC" root hood 750
ensure_dir "$STATE" hood hood 750
ensure_dir "$STATE/health" hood hood 750
ensure_dir "$STATE/backup" hood hood 750
say "directories: checked"

# ----------------------------------------------------------------- files ---
# install_file SRC DST MODE(octal, no leading 0) [OWNER GROUP]; sets FILE_CHANGED.
FILE_CHANGED=0
install_file() {
    local src=$1 dst=$2 m=$3 o=${4:-root} g=${5:-root}
    FILE_CHANGED=0
    if [[ -f $dst ]] && cmp -s "$src" "$dst" && [[ $(stat -c '%U:%G:%a' "$dst") == "$o:$g:$m" ]]; then
        return 0
    fi
    install -D -o "$o" -g "$g" -m "$m" "$src" "$dst"
    FILE_CHANGED=1
    changed "file $dst"
}

for s in healthcheck.sh notify.sh feed-audit-daily.sh backup.sh build-on-server.sh; do
    install_file "$DEPLOY_SRC/$s" "$PREFIX/deploy/$s" 755
done
install_file "$AUDIT_SRC" "$PREFIX/deploy/feed_audit.py" 755
install_file "$DEPLOY_SRC/README.md" "$PREFIX/deploy/README.md" 644
for e in "$DEPLOY_SRC"/etc/*.example; do
    install_file "$e" "$ETC/$(basename "$e")" 640 root hood
done

units_changed=0
units=(recorder.service healthcheck.service healthcheck.timer feed-audit.service feed-audit.timer
    backup.service backup.timer enricher-gaps.service enricher-gaps.timer notify-failure@.service)
for u in "${units[@]}"; do
    install_file "$DEPLOY_SRC/$u" "$UNIT_DIR/$u" 644
    (( FILE_CHANGED )) && units_changed=1
done
if (( units_changed )); then
    systemctl daemon-reload
    say "systemd: daemon-reload"
fi
say "scripts and units: checked"

install_file "$DEPLOY_SRC/journald-hood.conf" /etc/systemd/journald.conf.d/hood.conf 644
if (( FILE_CHANGED )); then
    systemctl restart systemd-journald || warn "could not restart systemd-journald"
fi
install_file "$DEPLOY_SRC/needrestart-hood.conf" /etc/needrestart/conf.d/hood.conf 644

# ---------------------------------------------------------------- chrony ---
if [[ $(systemctl is-enabled chrony 2>/dev/null) != enabled ]]; then
    systemctl enable chrony > /dev/null 2>&1 || warn "could not enable chrony"
    [[ $(systemctl is-enabled chrony 2>/dev/null) == enabled ]] && changed "chrony enabled"
fi
if ! systemctl is-active --quiet chrony; then
    systemctl start chrony 2> /dev/null || true
    if systemctl is-active --quiet chrony; then
        changed "chrony started"
    else
        warn "chrony is not running (systemctl status chrony); clock sync not verified"
    fi
fi
if systemctl is-active --quiet chrony; then
    synced=0
    for _ in $(seq 1 12); do
        if chronyc -n tracking 2>/dev/null | grep -Eq '^Leap status +: Normal'; then
            synced=1
            break
        fi
        sleep 5
    done
    if (( synced )); then
        say "chrony: synchronised ($(chronyc -n tracking | awk '/^System time/ { print $4, $5, $6, $7, $8 }'))"
    else
        warn "chrony not synchronised after 60 s (chronyc tracking; chronyc sources -v)"
    fi
fi

# ------------------------------------------------------------------- ufw ---
if (( firewall )); then
    if (( ${#ssh_ports[@]} == 0 )); then
        mapfile -t ssh_ports < <(sshd -T 2>/dev/null | awk '$1 == "port" { print $2 }' | sort -u)
        (( ${#ssh_ports[@]} )) || ssh_ports=(22)
    fi
    added=$(ufw show added 2>/dev/null || true)
    for port in "${ssh_ports[@]}"; do
        if ! grep -Eq "^ufw (limit|allow) ${port}/tcp( |$)" <<< "$added"; then
            ufw limit "${port}/tcp" comment ssh > /dev/null
            changed "ufw: limit ${port}/tcp (ssh)"
        fi
    done
    # shellcheck source=/dev/null
    . /etc/default/ufw
    if [[ ${DEFAULT_INPUT_POLICY:-} != DROP ]]; then
        ufw default deny incoming > /dev/null
        changed "ufw: default deny incoming"
    fi
    if [[ ${DEFAULT_OUTPUT_POLICY:-} != ACCEPT ]]; then
        ufw default allow outgoing > /dev/null
        changed "ufw: default allow outgoing"
    fi
    if ! ufw status | grep -q '^Status: active'; then
        # Never enable without an ssh rule: that would lock us out.
        added=$(ufw show added 2>/dev/null || true)
        grep -Eq "^ufw (limit|allow) ${ssh_ports[0]}/tcp( |$)" <<< "$added" ||
            die "no ufw rule for ssh port ${ssh_ports[0]}, refusing to enable ufw"
        ufw --force enable > /dev/null
        changed "ufw enabled (ssh ports: ${ssh_ports[*]})"
    fi
    say "ufw: $(ufw status | head -n 1)"
else
    say "ufw: skipped (--no-firewall)"
fi

# ---------------------------------------------------------------- docker ---
if (( with_docker )); then
    if [[ $(systemctl is-enabled docker 2>/dev/null) != enabled ]] || ! systemctl is-active --quiet docker; then
        systemctl enable --now docker
        changed "docker enabled"
    fi
    say "docker: ok. Publish ClickHouse ports on 127.0.0.1 only (docker bypasses ufw)."
fi

# ---------------------------------------------------------------- timers ---
if [[ -x $PREFIX/bin/recorder ]]; then
    for t in healthcheck.timer feed-audit.timer; do
        if [[ $(systemctl is-enabled "$t" 2>/dev/null) != enabled ]] || ! systemctl is-active --quiet "$t"; then
            systemctl enable --now "$t" > /dev/null 2>&1
            changed "$t enabled"
        fi
    done
else
    warn "$PREFIX/bin/recorder not found: healthcheck/feed-audit timers not enabled yet; run build-on-server.sh, then this script again"
fi

for u in recorder.service backup.timer enricher-gaps.timer; do
    say "$u: $(systemctl is-enabled "$u" 2>/dev/null || true) / $(systemctl is-active "$u" 2>/dev/null || true)"
done
say "done: $changes change(s), $warnings warning(s)"
