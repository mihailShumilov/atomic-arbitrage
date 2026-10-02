#!/usr/bin/env bash
# Shared harness of the offline tests deploy/test/test-*.sh. Sourced, not run:
#
#   HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
#   # shellcheck source-path=SCRIPTDIR source=lib.sh
#   . "$HERE/lib.sh"
#   t_init                       # T (temp dir, removed on exit), $T/bin first in PATH,
#                                # T_LOG=$T/log, T_N=$T/n exported, pass=0 fail=0
#   t_shim_logger                # $T/bin/logger appends its argv to $T_LOG
#   t_fake_notify [--noisy]      # $T/bin/fake-notify appends "LEVEL|TITLE|BODY" to $T_N
#   t_diag() { ...; }            # optional: printed under every FAIL of check
#   check DESC EXPR              # EXPR is eval'ed: callers pass it in single quotes
#   t_result                     # "result: N passed, M failed"; status 0 only if M == 0
#
# Not installed on the server (bootstrap.sh copies deploy/*.sh only, not test/).
# Works with bash 3.2 (Mac) and later.

# t_init: temp dir and counters. The EXIT trap removes $T.
t_init() {
    T=$(mktemp -d)
    trap 'rm -rf "$T"' EXIT
    mkdir -p "$T/bin"
    export PATH="$T/bin:$PATH" T_LOG=$T/log T_N=$T/n
    pass=0
    fail=0
}

# t_shim_logger: logger(1) replacement, one line per call with the full argv.
t_shim_logger() {
    cat > "$T/bin/logger" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$T_LOG"
EOF
    chmod +x "$T/bin/logger"
}

# t_fake_notify [--noisy]: notify.sh replacement, "LEVEL|TITLE|BODY" per call,
# newlines of BODY joined with " / ". FAKE_NOTIFY_FAIL=1 makes it exit 1.
# --noisy: also prints a line to stdout and, when failing, to stderr (for hooks
# that must not leak the notifier's output, e.g. smartd-event.sh).
t_fake_notify() {
    if [[ ${1:-} == --noisy ]]; then
        cat > "$T/bin/fake-notify" <<'EOF'
#!/usr/bin/env bash
[[ ${FAKE_NOTIFY_FAIL:-0} == 1 ]] && { echo "fake notify failed" >&2; exit 1; }
echo "notify.sh would print nothing, this goes to stdout"
EOF
    else
        cat > "$T/bin/fake-notify" <<'EOF'
#!/usr/bin/env bash
[[ ${FAKE_NOTIFY_FAIL:-0} == 1 ]] && exit 1
EOF
    fi
    cat >> "$T/bin/fake-notify" <<'EOF'
printf '%s|%s|%s\n' "$1" "$2" "${3//$'\n'/ / }" >> "$T_N"
EOF
    chmod +x "$T/bin/fake-notify"
}

# check DESC EXPR: eval EXPR, count and print PASS/FAIL; on FAIL call t_diag if
# the test defines it.
check() {
    if eval "$2"; then
        pass=$((pass + 1))
        echo "PASS  $1"
    else
        fail=$((fail + 1))
        echo "FAIL  $1"
        if declare -F t_diag > /dev/null; then t_diag; fi
    fi
}

# t_result: summary line; the status is the test's result (use it last).
t_result() {
    echo "result: $pass passed, $fail failed"
    (( fail == 0 ))
}
