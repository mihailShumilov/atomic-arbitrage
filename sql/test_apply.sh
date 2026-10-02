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

# Fake curl. /ping -> ok. Otherwise the query is stdin; journal INSERTs go to $T/journal, every
# other query to $T/queries. A query containing "broken_guard" fails like ClickHouse (HTTP 500 ->
# curl exit 22, body on stdout); "text_guard" returns a non-number; "one_guard" returns 1.
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
run() {
    rm -f "$T"/sql/[0-9]*.sql "$T/journal" "$T/queries"
    { [ -z "$1" ] || echo "-- apply-unless: SELECT $1"; printf '%s\n' "$2"; } > "$T/sql/001_t.sql"
    set +e; out=$(bash "$T/sql/apply.sh" 2>&1); rc=$?; set -e
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

# Guard 0 -> applied; lexer: ';' inside a string and comments do not split, comments not sent.
run zero_guard "SELECT 'a;b' -- c;d
; /* e;f */ SELECT 2;"
[ "$rc" -eq 0 ] || fail "guard 0: rc=$rc $out"; check
case "$journal" in *"'applied')"*) check ;; *) fail "guard 0: journal: $journal" ;; esac
[ "$(printf '%s' "$queries" | grep -c '^---$')" -ge 4 ] || fail "guard 0: statements: $queries"; check
case "$queries" in *"SELECT 'a;b'"*"SELECT 2"*) check ;; *) fail "lexer: $queries" ;; esac
case "$queries" in *"c;d"*|*"e;f"*) fail "lexer: comment sent: $queries" ;; *) check ;; esac

echo "ok: $n checks"
