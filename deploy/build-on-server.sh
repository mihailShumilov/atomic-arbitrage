#!/usr/bin/env bash
# hoodchain-mev: build recorder + enricher on the server and install them into
# /opt/hoodchain-mev/bin (run as root). Does NOT restart anything: a running
# recorder keeps its old binary (the file is replaced by rename) until
# `systemctl restart recorder`.
#
#   bash deploy/build-on-server.sh [--src DIR]     default DIR: /opt/hoodchain-mev/src
#
# The build runs as the unprivileged user `hoodbuild` (no access to the raw
# data), with rustup from the Ubuntu archive (toolchain: stable, minimal) and
# `cargo build --release --locked`: dependencies exactly as in Cargo.lock,
# nothing new in the workspace. The previous binaries are kept as *.prev.
# Network: apt, rustup (static.rust-lang.org) and crates.io only.
set -euo pipefail

PREFIX=/opt/hoodchain-mev
SRC=$PREFIX/src
BUILD_USER=hoodbuild
BUILD_HOME=/var/lib/hoodbuild
BINS=(recorder enricher)
MIN_RUST=1.85

while [[ $# -gt 0 ]]; do
    case $1 in
        --src) SRC=${2:?--src needs a directory}; shift ;;
        -h|--help) sed -n '2,14p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done

say() { printf '[build] %s\n' "$*"; }
die() { say "ERROR: $*"; exit 1; }

[[ $EUID -eq 0 ]] || die "run as root"
[[ -f $SRC/Cargo.lock && -d $SRC/crates/recorder ]] || die "no workspace in $SRC (rsync the repo there first)"
[[ -d $PREFIX/bin ]] || die "$PREFIX/bin missing: run deploy/bootstrap.sh first"

# Build user: owns the checkout and the toolchain, nothing else.
if ! id -u "$BUILD_USER" > /dev/null 2>&1; then
    useradd --system --user-group --home-dir "$BUILD_HOME" --create-home \
        --shell /usr/sbin/nologin "$BUILD_USER"
    say "user $BUILD_USER created"
fi

missing=()
for p in build-essential rustup git; do
    [[ $(dpkg-query -W -f='${db:Status-Status}' "$p" 2>/dev/null) == installed ]] || missing+=("$p")
done
if (( ${#missing[@]} )); then
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -q
    apt-get install -y -q --no-install-recommends "${missing[@]}"
fi

chown -R "$BUILD_USER:$BUILD_USER" "$SRC"

as_build() { (cd "$SRC" && runuser -u "$BUILD_USER" -- env HOME="$BUILD_HOME" "$@"); }

as_build rustup toolchain install stable --profile minimal --no-self-update
ver=$(as_build rustup run stable rustc --version | awk '{ print $2 }')
if [[ $(printf '%s\n%s\n' "$MIN_RUST" "$ver" | sort -V | head -n 1) != "$MIN_RUST" ]]; then
    die "rustc $ver < $MIN_RUST"
fi
say "rustc $ver"

# Rust release builds of this workspace need ~2-4 GB RAM per job.
mem_mb=$(awk '/^MemAvailable/ { print int($2 / 1024) }' /proc/meminfo)
jobs=$(nproc)
if (( mem_mb < 4000 )); then
    jobs=1
    say "low memory (${mem_mb} MB available): building with 1 job; add swap if it gets OOM-killed"
fi

say "cargo build --release --locked -p recorder -p enricher (jobs=$jobs)"
as_build rustup run stable cargo build --release --locked --jobs "$jobs" -p recorder -p enricher

installed=()
for b in "${BINS[@]}"; do
    new=$SRC/target/release/$b
    dst=$PREFIX/bin/$b
    [[ -x $new ]] || die "build output $new missing"
    # Smoke check without any network: clap prints help and exits.
    "$new" --help > /dev/null || die "$new --help failed"
    if [[ -f $dst ]] && cmp -s "$new" "$dst"; then
        say "$b: unchanged"
        continue
    fi
    [[ -f $dst ]] && cp -p "$dst" "$dst.prev"
    install -o root -g root -m 0755 "$new" "$dst.new"
    mv -f "$dst.new" "$dst"
    installed+=("$b")
done

if (( ${#installed[@]} )) || [[ ! -f $PREFIX/bin/BUILD_INFO ]]; then
    rev=unknown
    if [[ -d $SRC/.git ]]; then
        rev=$(as_build git -c safe.directory="$SRC" rev-parse --short HEAD 2>/dev/null || echo unknown)
        # Tracked files differing from HEAD (rsync of a working copy with
        # uncommitted edits): the binary is not exactly that commit.
        if [[ $rev != unknown ]] &&
            [[ -n $(as_build git -c safe.directory="$SRC" status --porcelain --untracked-files=no 2>/dev/null) ]]; then
            rev=$rev-dirty
            say "WARNING: $SRC has uncommitted changes to tracked files; BUILD_INFO says git=$rev"
        fi
    fi
    {
        echo "installed_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        echo "source=$SRC git=$rev"
        echo "rustc=$ver"
        (cd "$PREFIX/bin" && sha256sum "${BINS[@]}")
    } > "$PREFIX/bin/BUILD_INFO"
fi

if (( ${#installed[@]} )); then
    say "installed: ${installed[*]} (previous binaries, if any, kept as *.prev)"
    if [[ " ${installed[*]} " == *" recorder "* ]] && systemctl is-active --quiet recorder; then
        say "recorder is running the OLD binary. Switch with: systemctl restart recorder (expect a ~2 min gap, see README)"
    fi
else
    say "nothing to install"
fi
cat "$PREFIX/bin/BUILD_INFO"
