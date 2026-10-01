#!/usr/bin/env bash
# hoodchain-mev: pluggable notifier.
#
#   notify.sh LEVEL TITLE [BODY]      LEVEL = alert | ok | info
#
# Always writes the message to journald (tag hood-notify). If
# /etc/hoodchain/notify.env (NOTIFY_ENV) defines TELEGRAM_BOT_TOKEN and
# TELEGRAM_CHAT_ID, the message is also sent to Telegram.
#
# Exit status: 0 if every configured channel accepted the message, 1 if the
# Telegram send failed (callers retry on the next run), 2 on usage error.
#
# The bot token never reaches argv (curl reads the URL from stdin with -K -)
# and is masked in any curl error output that ends up in the journal.
set -uo pipefail

NOTIFY_ENV=${NOTIFY_ENV:-/etc/hoodchain/notify.env}

if [[ $# -lt 2 ]]; then
    echo "usage: notify.sh alert|ok|info TITLE [BODY]" >&2
    exit 2
fi
level=$1
title=$2
body=${3:-}

case $level in
    alert) prio=user.crit; tag='[ALERT]' ;;
    ok)    prio=user.notice; tag='[OK]' ;;
    info)  prio=user.info; tag='[INFO]' ;;
    *) echo "notify.sh: unknown level '$level'" >&2; exit 2 ;;
esac

# Read KEY=VALUE pairs without executing the file (it holds secrets and is
# not meant to be a shell script). Quotes around the value are stripped.
TELEGRAM_BOT_TOKEN=${TELEGRAM_BOT_TOKEN:-}
TELEGRAM_CHAT_ID=${TELEGRAM_CHAT_ID:-}
NOTIFY_HOST_LABEL=${NOTIFY_HOST_LABEL:-}
if [[ -r $NOTIFY_ENV ]]; then
    while IFS= read -r line || [[ -n $line ]]; do
        [[ $line =~ ^[[:space:]]*# ]] && continue
        [[ $line =~ ^[[:space:]]*([A-Z_][A-Z0-9_]*)=(.*)$ ]] || continue
        key=${BASH_REMATCH[1]}
        val=${BASH_REMATCH[2]}
        val=${val%$'\r'}
        if [[ $val =~ ^\"(.*)\"$ || $val =~ ^\'(.*)\'$ ]]; then
            val=${BASH_REMATCH[1]}
        fi
        case $key in
            TELEGRAM_BOT_TOKEN) TELEGRAM_BOT_TOKEN=$val ;;
            TELEGRAM_CHAT_ID) TELEGRAM_CHAT_ID=$val ;;
            NOTIFY_HOST_LABEL) NOTIFY_HOST_LABEL=$val ;;
        esac
    done < "$NOTIFY_ENV"
fi

host=${NOTIFY_HOST_LABEL:-$(hostname -s 2>/dev/null || echo host)}
text="$tag $host: $title"
if [[ -n $body ]]; then
    text+=$'\n'"$body"
fi

# 1. journald (falls back to stderr when there is no syslog socket).
one_line=${text//$'\n'/ | }
if ! logger -t hood-notify -p "$prio" -- "$one_line" 2>/dev/null; then
    echo "hood-notify $prio: $one_line" >&2
fi

# 2. Telegram, optional.
if [[ -z $TELEGRAM_BOT_TOKEN || -z $TELEGRAM_CHAT_ID ]]; then
    exit 0
fi
# Telegram limit is 4096 characters per message.
if (( ${#text} > 4000 )); then
    text="${text:0:4000}"$'\n'"[обрезано]"
fi

resp=$(printf 'url = "https://api.telegram.org/bot%s/sendMessage"\n' "$TELEGRAM_BOT_TOKEN" |
    curl -sS --max-time 20 --retry 2 --retry-delay 3 -K - \
        --data-urlencode "chat_id=$TELEGRAM_CHAT_ID" \
        --data-urlencode "text=$text" \
        --data-urlencode "disable_web_page_preview=true" 2>&1)
rc=$?
if [[ $rc -eq 0 && $resp == *'"ok":true'* ]]; then
    exit 0
fi
safe=${resp//"$TELEGRAM_BOT_TOKEN"/***}
safe=${safe//$'\n'/ }
logger -t hood-notify -p user.err -- "telegram send failed (curl rc=$rc): ${safe:0:300}" 2>/dev/null ||
    echo "hood-notify: telegram send failed (curl rc=$rc): ${safe:0:300}" >&2
exit 1
