#!/usr/bin/env bash
# Offline test of deploy/backup.sh with rclone and a LOCAL directory as the
# "remote" (no network, no credentials). Needs rclone and GNU coreutils.
#
#   bash deploy/test/test-backup.sh
# check() evals its single-quoted expression later, so variables in single
# quotes are intended (SC2016); the variables it uses look unused (SC2034).
# shellcheck disable=SC2016,SC2034
set -uo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
B=$HERE/../backup.sh
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
command -v rclone > /dev/null || { echo "SKIP: rclone not installed"; exit 0; }

F=$T/feed; mkdir -p "$F/2026/09/30" "$F/_torn" "$T/blocks" "$T/logs" "$T/remote"
old=$(( $(date +%s) - 3 * 3600 ))
for h in 12 13; do head -c 100000 /dev/urandom > "$F/2026/09/30/feed-20260930-$h.tsv.zst"; done
cur=$F/$(date -u +%Y/%m/%d)/feed-$(date -u +%Y%m%d-%H).tsv.zst
mkdir -p "$(dirname "$cur")"; head -c 5000 /dev/urandom > "$cur"
echo torn > "$F/_torn/feed-20260930-12.tsv.zst.at1.torn"
printf '1\t2\t3\n' > "$F/gaps.tsv"; echo 77 > "$F/last_seq.txt"; echo '# h' > "$F/connections.tsv"
echo x > "$F/2026/09/30/feed-20260930-14.tsv.zst.tmp"
head -c 2000 /dev/urandom > "$T/blocks/blocks-1-10.jsonl.zst"; echo p > "$T/blocks/blocks-11-20.jsonl.zst.partial"
printf '1\t10\tblocks-1-10.jsonl.zst\t0\n' > "$T/blocks/filled.tsv"
touch -d "@$old" "$F"/2026/09/30/* "$F/_torn/"*
: > "$T/rclone.conf"
cat > "$T/backup.env" <<EOT
BACKUP_REMOTE=$T/remote
RCLONE_CONFIG=$T/rclone.conf
BACKUP_FEED_DIR=$F
BACKUP_BLOCKS_DIR=$T/blocks
BACKUP_LOGS_DIR=$T/logs
BACKUP_MARKER=$T/state/last_ok
EOT
export BACKUP_ENV=$T/backup.env
pass=0 fail=0
check() { if eval "$2"; then pass=$((pass + 1)); echo "PASS  $1"; else fail=$((fail + 1)); echo "FAIL  $1"; fi; }
R=$T/remote

bash "$B" > "$T/out1" 2>&1; rc=$?
check "run 1: exit 0" '[[ $rc -eq 0 ]]'
check "closed hours copied byte-identical" 'cmp -s "$F/2026/09/30/feed-20260930-12.tsv.zst" "$R/feed/2026/09/30/feed-20260930-12.tsv.zst" && cmp -s "$F/2026/09/30/feed-20260930-13.tsv.zst" "$R/feed/2026/09/30/feed-20260930-13.tsv.zst"'
check "current (open) hour NOT copied" '[[ ! -e $R/feed/${cur#"$F"/} ]]'
check "_torn copied" '[[ -e $R/feed/_torn/feed-20260930-12.tsv.zst.at1.torn ]]'
check "*.tmp and *.partial not copied" '! find "$R" -name "*.tmp" -o -name "*.partial" | grep -q .'
check "meta copied, not mixed into feed/" '[[ -e $R/meta/feed/gaps.tsv && -e $R/meta/feed/last_seq.txt && -e $R/meta/feed/connections.tsv && ! -e $R/feed/gaps.tsv ]]'
check "blocks + filled.tsv copied" '[[ -e $R/blocks/blocks-1-10.jsonl.zst && -e $R/meta/blocks/filled.tsv ]]'
check "rclone logs copies (so the run-2 check is meaningful)" 'grep -q "Copied (new)" "$T/out1"'
check "marker written" '[[ -e $T/state/last_ok ]]'

bash "$B" > "$T/out2" 2>&1; rc=$?
check "run 2: exit 0, nothing re-copied (incremental)" '[[ $rc -eq 0 ]] && ! grep -q "Copied" "$T/out2"'

# A closed hour changes locally (e.g. torn-tail repair): new copy uploaded,
# old copy kept under replaced/.
printf 'more' >> "$F/2026/09/30/feed-20260930-13.tsv.zst"; touch -d "@$old" "$F/2026/09/30/feed-20260930-13.tsv.zst"
bash "$B" > "$T/out3" 2>&1; rc=$?
check "changed closed hour: re-uploaded" '[[ $rc -eq 0 ]] && cmp -s "$F/2026/09/30/feed-20260930-13.tsv.zst" "$R/feed/2026/09/30/feed-20260930-13.tsv.zst"'
check "changed closed hour: old copy kept in replaced/" '[[ -n $(find "$R/replaced" -name feed-20260930-13.tsv.zst) ]]'

# Local deletion never propagates.
rm "$F/2026/09/30/feed-20260930-12.tsv.zst"
bash "$B" > "$T/out4" 2>&1
check "local delete: remote copy stays" '[[ -e $R/feed/2026/09/30/feed-20260930-12.tsv.zst ]]'

bash "$B" --verify > "$T/out5" 2>&1; rc=$?
check "--verify: exit 0" '[[ $rc -eq 0 ]]'

printf 'BACKUP_REMOTE=CHANGE-ME:path\n' > "$T/bad.env"
BACKUP_ENV=$T/bad.env bash "$B" > /dev/null 2>&1; rc=$?
check "placeholder remote: refuses (exit 2)" '[[ $rc -eq 2 ]]'

echo "result: $pass passed, $fail failed"
(( fail == 0 )) || { tail -n 20 "$T"/out*; exit 1; }
