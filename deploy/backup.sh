#!/usr/bin/env bash
# hoodchain-mev: off-host backup of raw data with rclone (backup.timer, hourly).
# NOT enabled by default: needs a remote chosen and paid for by Mihail.
#
#   backup.sh            copy new/changed files to the remote
#   backup.sh --verify   compare local files with the remote (one-way, sizes+hashes)
#
# Config: /etc/hoodchain/backup.env (BACKUP_ENV), see backup.env.example.
# rclone remote credentials: /etc/hoodchain/rclone.conf (RCLONE_CONFIG).
#
# Layout on the remote (BACKUP_REMOTE, e.g. "hoodbox:hood/<host>"):
#   feed/YYYY/MM/DD/feed-*.tsv.zst, feed/_torn/**   raw feed, closed hours only
#   blocks/*.jsonl.zst, logs/*.jsonl.zst             RPC raw files (atomic, final)
#   meta/feed/{gaps.tsv,connections.tsv,last_seq.txt}, meta/blocks/filled.tsv
#   replaced/<UTC timestamp>/...                     old copies of any file that
#                                                     changed locally (should not
#                                                     happen for closed hours)
# Never deletes anything on the remote (rclone copy, not sync).
set -euo pipefail

BACKUP_ENV=${BACKUP_ENV:-/etc/hoodchain/backup.env}
if [[ -r $BACKUP_ENV ]]; then
    # shellcheck source=/dev/null
    . "$BACKUP_ENV"
fi
: "${BACKUP_REMOTE:=}"
: "${RCLONE_CONFIG:=/etc/hoodchain/rclone.conf}"
: "${BACKUP_FEED_DIR:=/srv/hood/data/feed}"
: "${BACKUP_BLOCKS_DIR:=/srv/hood/data/blocks}"
: "${BACKUP_LOGS_DIR:=/srv/hood/data/logs}"
: "${BACKUP_MIN_AGE:=65m}"     # an hourly feed file is final once its hour is over
: "${BACKUP_BWLIMIT:=0}"       # e.g. 20M; 0 = unlimited
: "${BACKUP_MARKER:=/var/lib/hoodchain/backup/last_ok}"
export RCLONE_CONFIG

if [[ -z $BACKUP_REMOTE || $BACKUP_REMOTE == *CHANGE-ME* ]]; then
    echo "BACKUP_REMOTE is not set in $BACKUP_ENV; refusing to run" >&2
    exit 2
fi
command -v rclone > /dev/null || { echo "rclone is not installed" >&2; exit 2; }

common=(--config "$RCLONE_CONFIG" --bwlimit "$BACKUP_BWLIMIT" -v --stats-one-line --stats 0)
# Filter rules are evaluated in order; the first match wins.
skip=(--filter '- *.partial' --filter '- *.tmp' --filter '- .*')
feed_files=("${skip[@]}" --filter '+ /[0-9][0-9][0-9][0-9]/**' --filter '+ /_torn/**' --filter '- **')
rpc_files=("${skip[@]}" --filter '+ *.jsonl.zst' --filter '- **')

if [[ ${1:-} == --verify ]]; then
    rclone check "${common[@]}" "${feed_files[@]}" --one-way --min-age "$BACKUP_MIN_AGE" \
        "$BACKUP_FEED_DIR" "$BACKUP_REMOTE/feed"
    for d in blocks logs; do
        var="BACKUP_${d^^}_DIR"
        [[ -d ${!var} ]] || continue
        rclone check "${common[@]}" "${rpc_files[@]}" --one-way "${!var}" "$BACKUP_REMOTE/$d"
    done
    echo "verify: OK"
    exit 0
fi

stamp=$(date -u +%Y%m%dT%H%M%SZ)
replaced="$BACKUP_REMOTE/replaced/$stamp"

# 1. Raw feed: closed hourly files and torn tails. Content never changes after
#    the hour is over; --backup-dir keeps the old copy if it ever does
#    (e.g. a torn-tail repair of a closed hour after a long outage).
rclone copy "${common[@]}" "${feed_files[@]}" --min-age "$BACKUP_MIN_AGE" \
    --backup-dir "$replaced/feed" \
    "$BACKUP_FEED_DIR" "$BACKUP_REMOTE/feed"

# 2. RPC raw files are written atomically (*.partial -> rename) and are final.
for d in blocks logs; do
    var="BACKUP_${d^^}_DIR"
    src=${!var}
    [[ -d $src ]] || continue
    rclone copy "${common[@]}" "${rpc_files[@]}" \
        --backup-dir "$replaced/$d" "$src" "$BACKUP_REMOTE/$d"
done

# 3. Small append-only state files: overwrite the remote copy.
rclone copy "${common[@]}" --filter '+ /gaps.tsv' --filter '+ /connections.tsv' \
    --filter '+ /last_seq.txt' --filter '- **' "$BACKUP_FEED_DIR" "$BACKUP_REMOTE/meta/feed"
if [[ -e $BACKUP_BLOCKS_DIR/filled.tsv ]]; then
    rclone copy "${common[@]}" --filter '+ /filled.tsv' --filter '- **' \
        "$BACKUP_BLOCKS_DIR" "$BACKUP_REMOTE/meta/blocks"
fi

mkdir -p "$(dirname "$BACKUP_MARKER")"
touch "$BACKUP_MARKER"
echo "backup: OK ($stamp)"
