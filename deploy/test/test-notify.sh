#!/usr/bin/env bash
# Offline test of deploy/notify.sh: curl and logger are shims, no network.
# Checks: journald-only mode, Telegram request shape, the bot token never in
# curl argv or in the log, masking of the token in curl errors, exit codes.
#
#   bash deploy/test/test-notify.sh
# check() evals its single-quoted expression later, so variables in single
# quotes are intended (SC2016); the variables it uses look unused (SC2034).
# shellcheck disable=SC2016,SC2034
set -uo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
N=$HERE/../notify.sh
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
mkdir -p "$T/bin"
TOKEN="123456:FAKE-token-for-tests-only"

cat > "$T/bin/logger" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$T_LOG"
EOF
cat > "$T/bin/curl" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$@" > "$T_ARGV"
cat > "$T_STDIN"
case ${FAKE_CURL:-ok} in
    ok) echo '{"ok":true,"result":{}}' ;;
    api_error) echo '{"ok":false,"error_code":401,"description":"Unauthorized"}' ;;
    net_error) url=$(sed -n 's/^url = "\(.*\)"$/\1/p' "$T_STDIN"); echo "curl: (7) Failed to connect to $url"; exit 7 ;;
esac
EOF
chmod +x "$T/bin/"*
export PATH="$T/bin:$PATH" T_LOG=$T/log T_ARGV=$T/argv T_STDIN=$T/stdin

pass=0 fail=0
check() { if eval "$2"; then pass=$((pass + 1)); echo "PASS  $1"; else fail=$((fail + 1)); echo "FAIL  $1"; fi; }

# 1. No notify.env: journald only, curl not called.
: > "$T_LOG"; rm -f "$T_ARGV"
NOTIFY_ENV=$T/missing.env bash "$N" alert "диск заполнен" "строка 1
строка 2"
rc=$?
check "journald-only: exit 0" '[[ $rc -eq 0 ]]'
check "journald-only: one log line, crit, multi-line joined" 'grep -q -- "-p user.crit -- \[ALERT\] .*: диск заполнен | строка 1 | строка 2" "$T_LOG" && [[ $(wc -l < "$T_LOG") -eq 1 ]]'
check "journald-only: curl not called" '[[ ! -e $T_ARGV ]]'

# 2. Telegram configured (quoted values, comment lines).
cat > "$T/notify.env" <<EOF
# comment
TELEGRAM_BOT_TOKEN="$TOKEN"
TELEGRAM_CHAT_ID='-100123'
NOTIFY_HOST_LABEL=hood-recorder
EOF
: > "$T_LOG"
NOTIFY_ENV=$T/notify.env FAKE_CURL=ok bash "$N" ok "восстановлено: фид молчит"
rc=$?
check "telegram ok: exit 0" '[[ $rc -eq 0 ]]'
check "telegram: token not in curl argv" '! grep -qF "$TOKEN" "$T_ARGV"'
check "telegram: URL with token passed on stdin (-K -)" 'grep -qx -- "-K" "$T_ARGV" && grep -qF "url = \"https://api.telegram.org/bot$TOKEN/sendMessage\"" "$T_STDIN"'
check "telegram: chat_id and text form fields" 'grep -qx "chat_id=-100123" "$T_ARGV" && grep -qx "text=\[OK\] hood-recorder: восстановлено: фид молчит" "$T_ARGV"'
check "telegram: token not in log" '! grep -qF "$TOKEN" "$T_LOG"'

# 3. API error -> exit 1, logged without the token.
: > "$T_LOG"
NOTIFY_ENV=$T/notify.env FAKE_CURL=api_error bash "$N" alert "x"
rc=$?
check "telegram api error: exit 1" '[[ $rc -eq 1 ]]'
check "telegram api error: logged" 'grep -q "telegram send failed" "$T_LOG"'

# 4. Network error whose message contains the URL -> token masked.
: > "$T_LOG"
NOTIFY_ENV=$T/notify.env FAKE_CURL=net_error bash "$N" info "y"
rc=$?
check "telegram net error: exit 1" '[[ $rc -eq 1 ]]'
check "telegram net error: token masked in log" 'grep -q "bot\*\*\*/sendMessage" "$T_LOG" && ! grep -qF "$TOKEN" "$T_LOG"'

# 5. Usage errors.
bash "$N" alert > /dev/null 2>&1; rc=$?
check "usage: missing title -> exit 2" '[[ $rc -eq 2 ]]'
bash "$N" panic "t" > /dev/null 2>&1; rc=$?
check "usage: bad level -> exit 2" '[[ $rc -eq 2 ]]'

echo "result: $pass passed, $fail failed"
(( fail == 0 ))
