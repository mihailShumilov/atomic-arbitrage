---
name: feed-audit
description: Repeatable audit of recorded Robinhood Chain sequencer-feed files (data/feed/**/feed-*.tsv.zst) — completeness, gaps vs gaps.tsv, duplicates, zstd frame integrity, blocks/s, MB/h, inter-arrival latency, and blockHash / L1 block cross-check against RPC. Use whenever checking a feed recording (smoke test, daily check, 7-day phase-1a criterion, after recorder changes or restarts), before loading feed data into ClickHouse, or when someone asks "is the recording complete / how much did we record".
---

# feed-audit — проверка записи фида

Один скрипт на все проверки записи, чтобы цифры в отчётах считались одинаково. Факты о фиде — в скилле `hoodchain-mev` (`references/chain-facts.md`); при расхождении прав он.

## Запуск

```bash
# одна запись или сутки; glob можно передать строкой
python3 .claude/skills/feed-audit/scripts/feed_audit.py \
  --feed-root data/feed --rpc-sample 20 'data/feed/2026/09/30/feed-*.tsv.zst'
# --json — машиночитаемая сводка для отчёта
```

Нужны Python 3.9+ (только stdlib) и `zstd` CLI. `RPC_URL` берётся из окружения, иначе публичный RPC. `--rpc-sample N` делает один пакетный запрос на N блоков. Вызовов немного, но на публичном RPC не ставь больше ~50.

## Что проверяет

| Проверка | Итог при нарушении |
|---|---|
| `zstd -dc` читает файл целиком (незакрытый фрейм = recorder упал или ещё пишет) | FAIL |
| 4 TSV-столбца, валидный JSON, seq_first/seq_last = JSON | FAIL |
| sequenceNumber строго +1, без дублей и откатов | FAIL |
| каждая дыра есть в `gaps.tsv` (нужен `--feed-root`) | FAIL |
| `recv_unix_ns` не убывает | FAIL |
| blockHash = RPC на выборке (всегда первый, последний и часть отложенных сообщений kind ≠ 3) | FAIL |
| l1BlockNumber из RPC = `header.blockNumber` для kind 3 и нарастающему максимуму для kind 9/13 | WARN (модель проверена 2026-09-30) |
| `last_seq.txt` ≠ последнему seq (нормально, если передан не последний файл) | WARN |
| строки с seq 0 (recorder сохранил непарсящийся конверт) | WARN |

Цифры: блоков, ожидалось, пропущено, виды `header.kind`, длительность, блоков/с, МБ сжато и в распакованном виде, МБ/ч, интервал между конвертами (p50, p99, max).

## Как читать результат

- **Дыра в `gaps.tsv`** — ожидаемое поведение после переподключения. Её закрывают дозаливкой через enricher, и это отдельная проверка. Дыра **не** в `gaps.tsv` — ошибка recorder.
- **Файл текущего часа** во время записи всегда даёт FAIL по zstd: фрейм закрывается только при ротации или остановке (известный недостаток, задача 002). Проверяй закрытые часы.
- **Блоков/с и МБ/ч** считаются по `recv_unix_ns`, то есть по времени прихода, а не по меткам блоков. Метки блоков имеют разрешение в 1 с и немонотонны у отложенных сообщений.
- Скрипт проверяет запись, а не декодирование. Проверку декодеров и `hood.*` по-прежнему делает data-auditor по своему списку.

Проверено 2026-09-30:
- на 10-минутной записи из задачи 001 цифры совпали с независимым аудитом;
- на синтетических файлах: удалены 5 строк — FAIL «gap not listed», с записью в `gaps.tsv` — PASS; обрезанный файл — FAIL по zstd и битому JSON.
