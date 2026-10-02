#!/usr/bin/env bash
# Offline test of sql/apply.sh: a fake `curl` on PATH plays ClickHouse, no server needed.
# Copies apply.sh next to synthetic migrations in a temp dir, so the real sql/NNN files are unused.
#   bash sql/test_apply.sh        -> "ok: N checks", exit 0; first failure -> exit 1
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
T=$(mktemp -d "${TMPDIR:-/tmp}/hood-apply-test.XXXXXX")
trap 'rm -rf "$T"' EXIT
mkdir -p "$T/bin" "$T/sql"
cp "$HERE/apply.sh" "$T/sql/apply.sh"

# Fake curl. /ping -> ok. Otherwise the query is stdin; journal INSERTs go to $T/journal, the two
# bootstrap statements of apply.sh (CREATE DATABASE / schema_migrations) to $T/service, every other
# query (= the migration statements) to $T/queries, each followed by a "---" line. A query
# containing "broken_guard" fails like ClickHouse (HTTP 500 -> curl exit 22, body on stdout);
# "text_guard" returns a non-number; "one_guard" returns 1, "zero_guard" 0.
cat > "$T/bin/curl" <<'EOF'
#!/usr/bin/env bash
for a in "$@"; do case "$a" in */ping) exit 0 ;; esac; done
q=$(cat)
case "$q" in
    *broken_guard*) echo "Code: 47. DB::Exception: Unknown identifier broken_guard"; exit 22 ;;
    *text_guard*) echo "abc" ;;
    *one_guard*) echo "1" ;;
    *zero_guard*) echo "0" ;;
    "SELECT version()") echo "26.9.6.6" ;;
    "SELECT 1") echo "1" ;;
    INSERT*schema_migrations*) printf '%s\n' "$q" >> "$FAKE_DIR/journal" ;;
    "CREATE DATABASE IF NOT EXISTS hood"|"CREATE TABLE IF NOT EXISTS hood.schema_migrations"*)
        printf '%s\n---\n' "$q" >> "$FAKE_DIR/service" ;;
    *"name = 'schema_migrations'"*) echo "1" ;;
    "SELECT sha256 FROM hood.schema_migrations"*) ;;   # nothing recorded yet
    *) printf '%s\n---\n' "$q" >> "$FAKE_DIR/queries" ;;
esac
EOF
chmod +x "$T/bin/curl"

export PATH="$T/bin:$PATH" FAKE_DIR="$T" ENV_FILE=/dev/null CLICKHOUSE_PASSWORD=x CLICKHOUSE_URL=http://fake/
n=0
fail() { echo "FAIL: $*" >&2; exit 1; }
check() { n=$((n + 1)); }

# run <guard-or-empty> <body>: one migration 001_t.sql; sets rc, out, journal, queries.
# RUN_ARGS: extra apply.sh arguments (e.g. --dry-run).
run() {
    rm -f "$T"/sql/[0-9]*.sql "$T/journal" "$T/queries" "$T/service"
    { [ -z "$1" ] || echo "-- apply-unless: SELECT $1"; printf '%s\n' "$2"; } > "$T/sql/001_t.sql"
    set +e; out=$(bash "$T/sql/apply.sh" ${RUN_ARGS:+"$RUN_ARGS"} 2>&1); rc=$?; set -e
    journal=$(cat "$T/journal" 2>/dev/null || true)
    queries=$(cat "$T/queries" 2>/dev/null || true)
}

# Б1: a failing guard query aborts, nothing journaled, migration body not run.
run broken_guard "CREATE TABLE body_marker (a UInt8) ENGINE = Memory;"
[ "$rc" -ne 0 ] || fail "failing guard: exit 0"; check
[ -z "$journal" ] || fail "failing guard: journaled: $journal"; check
case "$out" in *"apply-unless query failed"*) check ;; *) fail "failing guard: message: $out" ;; esac
case "$queries" in *body_marker*) fail "failing guard: body executed" ;; *) check ;; esac

# Non-numeric guard answer aborts as well.
run text_guard "CREATE TABLE body_marker (a UInt8) ENGINE = Memory;"
[ "$rc" -ne 0 ] && [ -z "$journal" ] || fail "text guard: rc=$rc journal=$journal"; check

# Guard 1 -> skipped, journaled, body not run.
run one_guard "CREATE TABLE body_marker (a UInt8) ENGINE = Memory;"
[ "$rc" -eq 0 ] || fail "guard 1: rc=$rc $out"; check
case "$journal" in *"'skipped')"*) check ;; *) fail "guard 1: journal: $journal" ;; esac
case "$queries" in *body_marker*) fail "guard 1: body executed" ;; *) check ;; esac

# Guard 0 -> applied; exactly the two statements are sent (no service query among them).
run zero_guard "SELECT 'a;b' -- c;d
; /* e;f */ SELECT 2;"
[ "$rc" -eq 0 ] || fail "guard 0: rc=$rc $out"; check
case "$journal" in *"'applied')"*) check ;; *) fail "guard 0: journal: $journal" ;; esac
[ "$(printf '%s\n' "$queries" | grep -c '^---$')" -eq 2 ] || fail "guard 0: statements: $queries"; check

# Lexer (split_sql): the exact statements sent for a file with ';' / '--' / '/*' inside '...',
# "..." and `...`, a backslash-escaped and a doubled quote, a multi-line block comment, empty
# statements, a comment-only chunk and a last statement without ';'. Comments are not sent,
# whitespace at both ends of a statement is trimmed, inner newlines are kept.
run "" "-- header; not a statement
SELECT 'x;y', \"q;\", \`b;\` ; ;
SELECT 'it\\'s;' , 'a''b;c', '--no', '/*no*/'; /* multi;
line; */
-- only a comment ;
INSERT INTO t VALUES (1),-- tail; comment
  (2)
;SELECT 3"
[ "$rc" -eq 0 ] || fail "lexer: rc=$rc $out"; check
expected="SELECT 'x;y', \"q;\", \`b;\`
---
SELECT 'it\\'s;' , 'a''b;c', '--no', '/*no*/'
---
INSERT INTO t VALUES (1),
  (2)
---
SELECT 3
---"
[ "$queries" = "$expected" ] || fail "lexer: got
$queries
--- expected
$expected"; check
[ "$(printf '%s\n' "$(cat "$T/service")" | grep -c '^---$')" -eq 2 ] || fail "service statements"; check

# Unterminated quote: the run aborts before sending anything of the file, nothing journaled.
run "" "SELECT 1; SELECT 'open"
[ "$rc" -ne 0 ] && [ -z "$journal" ] && [ -z "$queries" ] || fail "unterminated: rc=$rc journal=$journal queries=$queries"; check
case "$out" in *"cannot split statements"*) check ;; *) fail "unterminated: message: $out" ;; esac

# Banned DDL (EXCHANGE / CREATE OR REPLACE / REPLACE TABLE, any case, split over lines): the run
# aborts before the first statement of the file is sent, nothing journaled.
for stmt in "EXCHANGE TABLES a AND b" "create or
  replace table a (x UInt8) ENGINE = Memory" "REPLACE TABLE a (x UInt8) ENGINE = Memory"; do
    run "" "SELECT 1; $stmt;"
    [ "$rc" -ne 0 ] && [ -z "$journal" ] && [ -z "$queries" ] || fail "banned '$stmt': rc=$rc journal=$journal queries=$queries"; check
    case "$out" in *"are banned"*) check ;; *) fail "banned '$stmt': message: $out" ;; esac
done
# The ban is checked for every pending file: under --dry-run and when apply-unless would skip it.
RUN_ARGS=--dry-run run "" "SELECT 1; EXCHANGE TABLES a AND b;"
[ "$rc" -ne 0 ] && [ -z "$journal" ] || fail "banned + dry-run: rc=$rc journal=$journal"; check
case "$out" in *"are banned"*) check ;; *) fail "banned + dry-run: message: $out" ;; esac
run one_guard "REPLACE TABLE a (x UInt8) ENGINE = Memory;"
[ "$rc" -ne 0 ] && [ -z "$journal" ] || fail "banned + guard 1: rc=$rc journal=$journal"; check
case "$out" in *"are banned"*) check ;; *) fail "banned + guard 1: message: $out" ;; esac

# ...but a column or a value named like them is fine.
run "" "CREATE TABLE t (exchange String, replace UInt8) ENGINE = Memory; SELECT 'EXCHANGE x';"
[ "$rc" -eq 0 ] || fail "banned false positive: rc=$rc $out"; check

echo "ok: $n checks"
