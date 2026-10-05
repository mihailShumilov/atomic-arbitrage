#!/usr/bin/env bash
# Apply sql/NNN_*.sql migrations, in order, to a running ClickHouse over its HTTP port.
#
#   sql/apply.sh              apply every pending migration
#   sql/apply.sh --to N       apply pending migrations with version <= N only
#   sql/apply.sh --dry-run    list what would be applied, change nothing
#
# Connection (environment wins over .env; .env is parsed, never executed):
#   CLICKHOUSE_URL        default http://127.0.0.1:${CLICKHOUSE_HTTP_PORT:-18123}/ (also read from .env)
#   CLICKHOUSE_USER       default hood
#   CLICKHOUSE_PASSWORD   required; sent in a curl config on a pipe, never in argv
#   ENV_FILE              default <repo>/.env
# .env format: plain KEY=value lines (last one wins; CRLF and one pair of surrounding quotes are
# stripped). No `export KEY=...`, no inline comments (`KEY=v # c` makes "v # c" the value), no
# newlines in values (the password must fit on one curl config line).
#
# Rules (see .claude/skills/hoodchain-mev/references/data-model.md, "Схема и миграции"):
# - Applied migrations are recorded in hood.schema_migrations (version, name, sha256, outcome).
#   A recorded migration is never re-run; if its file changed afterwards, the script stops:
#   applied migrations are immutable, add a new NNN file instead.
# - Every migration is idempotent on its own (IF NOT EXISTS, superset enums, guards), so a run
#   interrupted half-way can simply be repeated.
# - A file may carry one line "-- apply-unless: <SQL returning one number>". If that query
#   returns non-zero, the schema is already in the target state: the statements are skipped and
#   the migration is recorded with outcome 'skipped'.
# - Statements are split on ';' outside quotes and comments; comments are not sent.
# - A failing or non-numeric apply-unless query aborts the run (nothing is journaled for that file).
# - A statement starting with EXCHANGE, CREATE OR REPLACE (any object: TABLE, VIEW, DICTIONARY...)
#   or REPLACE TABLE aborts the run before any statement of that file is sent, also under --dry-run
#   and for a file apply-unless would skip (banned: they break metadata on the Docker Desktop bind mount).
# - Do not run two apply.sh in parallel: there is no lock (single local operator for now).
# Test: sql/test_apply.sh (fake curl, no ClickHouse needed).
#
# Exit codes: 0 ok (prints "changes: N"), 1 error (nothing after the failing statement is run).
set -euo pipefail

SQL_DIR=$(cd "$(dirname "$0")" && pwd)
ROOT=$(dirname "$SQL_DIR")
ENV_FILE=${ENV_FILE:-$ROOT/.env}

die() { echo "apply.sh: $*" >&2; exit 1; }

TO=""
DRY_RUN=0
while [ $# -gt 0 ]; do
    case "$1" in
        --to) [ $# -ge 2 ] || die "--to needs a version"; TO=$2; shift 2 ;;
        --dry-run) DRY_RUN=1; shift ;;
        -h|--help) awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"; exit 0 ;;
        *) die "unknown argument: $1" ;;
    esac
done
if [ -n "$TO" ]; then
    [[ "$TO" =~ ^[0-9]+$ ]] || die "--to must be a number"
    TO=$((10#$TO))
fi

# Read KEY from .env without executing it (only if not already set in the environment).
env_get() {
    local key=$1 line val=""
    [ -f "$ENV_FILE" ] || return 0
    while IFS= read -r line || [ -n "$line" ]; do
        case "$line" in
            "$key="*) val=${line#"$key="} ;;
        esac
    done < "$ENV_FILE"
    val=${val%$'\r'}
    case "$val" in
        \"*\") val=${val#\"}; val=${val%\"} ;;
        \'*\') val=${val#\'}; val=${val%\'} ;;
    esac
    printf '%s' "$val"
}

CH_USER=${CLICKHOUSE_USER:-$(env_get CLICKHOUSE_USER)}
CH_USER=${CH_USER:-hood}
CH_PASSWORD=${CLICKHOUSE_PASSWORD:-$(env_get CLICKHOUSE_PASSWORD)}
[ -n "$CH_PASSWORD" ] || die "CLICKHOUSE_PASSWORD is not set (environment or $ENV_FILE)"
CH_PORT=${CLICKHOUSE_HTTP_PORT:-$(env_get CLICKHOUSE_HTTP_PORT)}
# CLICKHOUSE_URL from the environment, then from .env (like the loader, crates/loader/src/config.rs;
# review 032 data-auditor, Н1: an env file pointing at a temporary server must not fall through to 18123).
CH_URL=${CLICKHOUSE_URL:-$(env_get CLICKHOUSE_URL)}
CH_URL=${CH_URL:-http://127.0.0.1:${CH_PORT:-18123}/}

# curl config with credentials, produced by builtins only (not visible in ps).
curl_auth() {
    local u=${CH_USER//\\/\\\\} p=${CH_PASSWORD//\\/\\\\}
    u=${u//\"/\\\"}; p=${p//\"/\\\"}
    printf 'header = "X-ClickHouse-User: %s"\nheader = "X-ClickHouse-Key: %s"\n' "$u" "$p"
}

# Run one SQL statement read from stdin; print the result (TSV).
ch() {
    curl -sS --fail-with-body -K <(curl_auth) --data-binary @- "$CH_URL"
}

TMP=$(mktemp -d "${TMPDIR:-/tmp}/hood-apply.XXXXXX")
trap 'rm -rf "$TMP"' EXIT

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
    else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

# Split a SQL file into $2/stmt-NNNN.sql: ';' outside '...', "...", `...`, -- and /* */.
split_sql() {
    awk -v out="$2" '
    function flush(   t) {
        t = buf; gsub(/^[ \t\r\n]+|[ \t\r\n]+$/, "", t)
        if (t != "") { n++; f = sprintf("%s/stmt-%04d.sql", out, n); printf "%s\n", t > f; close(f) }
        buf = ""
    }
    BEGIN { n = 0; buf = ""; q = ""; blk = 0 }
    {
        line = $0; len = length(line); i = 1
        while (i <= len) {
            c = substr(line, i, 1); c2 = substr(line, i, 2)
            if (blk) { if (c2 == "*/") { blk = 0; i += 2 } else i++; continue }
            if (q != "") {
                buf = buf c
                if (c == "\\") { buf = buf substr(line, i + 1, 1); i += 2; continue }
                if (c == q) q = ""
                i++; continue
            }
            if (c2 == "--") break
            if (c2 == "/*") { blk = 1; i += 2; continue }
            if (c == "\047" || c == "\"" || c == "`") { q = c; buf = buf c; i++; continue }
            if (c == ";") { flush(); i++; continue }
            buf = buf c; i++
        }
        buf = buf "\n"
    }
    END {
        if (q != "" || blk) { print "unterminated quote or comment" > "/dev/stderr"; exit 2 }
        flush()
    }' "$1"
}

# Refuse a file whose statement starts with EXCHANGE / CREATE OR REPLACE / REPLACE TABLE: on the
# Docker Desktop bind mount they lose or orphan metadata. Statement start only (split_sql trims it;
# whitespace runs folded to one space).
check_banned() {
    local s
    for s in "$2"/stmt-*.sql; do
        if tr -s '[:space:]' ' ' < "$s" | grep -Eiq '^(EXCHANGE |CREATE OR REPLACE |REPLACE TABLE )'; then
            die "$1: EXCHANGE / CREATE OR REPLACE / REPLACE TABLE are banned (data-model.md, \"Схема и миграции\"): use CREATE <t>_NNN + two RENAMEs"
        fi
    done
}

# Wait for the server (fresh container: up to ~60 s).
for _ in $(seq 1 60); do
    if curl -sS --max-time 2 -o /dev/null "${CH_URL%/}/ping" 2>/dev/null; then break; fi
    sleep 1
done
echo 'SELECT 1' | ch >/dev/null || die "ClickHouse at $CH_URL is not reachable or rejects the credentials"
echo "server: $CH_URL version $(echo 'SELECT version()' | ch)"

if [ "$DRY_RUN" -eq 0 ]; then
    echo 'CREATE DATABASE IF NOT EXISTS hood' | ch >/dev/null
    ch >/dev/null <<'SQL'
CREATE TABLE IF NOT EXISTS hood.schema_migrations (
    version     UInt32,
    name        String,
    sha256      String,
    outcome     Enum8('applied' = 1, 'skipped' = 2),
    applied_at  DateTime64(3, 'UTC') DEFAULT now64(3)
) ENGINE = MergeTree ORDER BY version
SQL
fi

have_log=$(echo "SELECT count() FROM system.tables WHERE database = 'hood' AND name = 'schema_migrations'" | ch)

shopt -s nullglob
files=("$SQL_DIR"/[0-9][0-9][0-9]_*.sql)
[ ${#files[@]} -gt 0 ] || die "no migrations in $SQL_DIR"

changes=0; already=0; seen=" "
for f in "${files[@]}"; do
    name=$(basename "$f")
    [[ "$name" =~ ^([0-9]{3})_[a-z0-9_]+\.sql$ ]] || die "bad migration file name: $name"
    ver=$((10#${BASH_REMATCH[1]}))
    case "$seen" in *" $ver "*) die "duplicate migration version $ver ($name)" ;; esac
    seen="$seen$ver "
    if [ -n "$TO" ] && [ "$ver" -gt "$TO" ]; then continue; fi
    sum=$(sha256_of "$f")

    recorded=""
    if [ "$have_log" != "0" ]; then
        recorded=$(echo "SELECT sha256 FROM hood.schema_migrations WHERE version = $ver ORDER BY applied_at LIMIT 1" | ch)
    fi
    if [ -n "$recorded" ]; then
        [ "$recorded" = "$sum" ] || die "$name changed after it was applied (recorded sha256 $recorded, file $sum); applied migrations are immutable, add a new NNN file"
        already=$((already + 1))
        continue
    fi

    # Split and check every pending file, also under --dry-run and when apply-unless would skip it
    # (on a fresh volume the same file runs): review 029, Р2.
    dir="$TMP/$ver"; mkdir -p "$dir"
    split_sql "$f" "$dir" || die "$name: cannot split statements"
    check_banned "$name" "$dir"

    guard=$(sed -n 's/^-- apply-unless: //p' "$f")
    [ "$(printf '%s\n' "$guard" | grep -c .)" -le 1 ] || die "$name: more than one apply-unless line"
    if [ "$DRY_RUN" -eq 1 ]; then
        echo "pending: $name${guard:+ (has apply-unless guard)}"
        changes=$((changes + 1))
        continue
    fi

    outcome=applied
    if [ -n "$guard" ]; then
        # Not inside [ ]: a failing query must abort, never turn into 'skipped' (review 023, Б1).
        g=$(printf '%s' "$guard" | ch 2>&1) || die "$name: apply-unless query failed: $g"
        [[ "$g" =~ ^[0-9]+$ ]] || die "$name: apply-unless must return one number, got: $g"
        [ "$g" = "0" ] || outcome=skipped
    fi
    if [ "$outcome" = applied ]; then
        for s in "$dir"/stmt-*.sql; do
            if ! out=$(ch < "$s" 2>&1); then
                echo "--- failed statement ($name, $(basename "$s")):" >&2
                cat "$s" >&2
                die "$name: $out"
            fi
        done
    fi
    printf "INSERT INTO hood.schema_migrations (version, name, sha256, outcome) VALUES (%d, '%s', '%s', '%s')" \
        "$ver" "$name" "$sum" "$outcome" | ch >/dev/null
    echo "$outcome: $name"
    changes=$((changes + 1))
done

echo "already applied: $already"
if [ "$DRY_RUN" -eq 1 ]; then echo "pending: $changes"; else echo "changes: $changes"; fi
