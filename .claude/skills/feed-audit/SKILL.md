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
# без сети: --rpc-sample 0 (RPC не вызывается вообще)
```

Нужны Python 3.9+ (только stdlib) и `zstd` CLI. `RPC_URL` берётся из окружения, иначе публичный RPC. `--rpc-sample N` делает один пакетный запрос на N блоков. Вызовов немного, но на публичном RPC не ставь больше ~50. Скрипт ничего не пишет на диск.

Прочие флаги:
- `--session-gap-s 30` — пауза между соседними строками, после которой начинается новая сессия (см. ниже);
- `--current-hour YYYYMMDD-HH` — какой час считать открытым (по умолчанию текущий час UTC). Нужен для проверки старых записей и тестов.

Игнорируются, даже если попали в glob: всё внутри `_torn/`, `connections.tsv`, `*.tmp`, любые имена не вида `feed-*.tsv.zst` (счётчик `ignored_inputs`).

## Что проверяет

| Проверка | Итог при нарушении |
|---|---|
| Целостность по zstd-фреймам: каждый целый фрейм распаковывается и сходится с контрольной суммой | FAIL |
| Байты после последнего целого фрейма: один недописанный фрейм в файле **текущего** часа — это открытый фрейм, recorder ещё пишет (`open_tail`, не проверяется) | не FAIL |
| То же в **закрытом** часе (оборванный хвост) или мусор вместо фрейма | FAIL |
| 4 TSV-столбца, целые `recv_unix_ns`/`seq_first`/`seq_last`, валидный JSON, seq_first/seq_last = JSON | FAIL |
| sequenceNumber строго +1, без дублей и откатов | FAIL |
| каждая дыра есть в `gaps.tsv` (нужен `--feed-root`) | FAIL |
| `recv_unix_ns` не убывает (по всем строкам, включая seq 0) | FAIL |
| строка с seq 0, в которой есть `messages` | FAIL |
| строки с seq 0 «прочие» (не `recorderFrame` и не `confirmedSequenceNumberMessage`, либо битый base64) | WARN |
| blockHash = RPC на выборке (всегда первый, последний и часть отложенных сообщений kind ≠ 3) | FAIL |
| l1BlockNumber из RPC = `header.blockNumber` для kind 3 и нарастающему максимуму для kind 9/13 | WARN (модель проверена 2026-09-30) |
| `last_seq.txt` ≠ последнему seq (нормально, если передан не последний файл или есть открытый фрейм) | WARN |

Цифры:
- `lines` — все строки; `envelopes` — только строки с блоками (seq ≠ 0);
- `seq0_lines` — строки с seq 0 по типам: `recorderFrame:<opcode>` (ping, pong, close, binary, text — текст, который не JSON), `confirmedSequenceNumberMessage`, `other`, `with_messages`;
- блоков, ожидалось, пропущено, виды `header.kind`, число zstd-фреймов;
- сессии (`session_list`), `session_s`, **блоков/с и МБ/ч по времени сессий**;
- `recv_span_s`, `blocks_per_s_span`, `mb_per_hour_span` — по всему интервалу от первой до последней строки, вместе с простоями;
- интервал между конвертами (p50, p99, max) — только между соседними строками с блоками внутри одной сессии.

## Как читать результат

- **Дыра в `gaps.tsv`** — ожидаемое поведение после переподключения. Её закрывают дозаливкой через enricher, и это отдельная проверка. Дыра **не** в `gaps.tsv` — ошибка recorder. С задачи 008 recorder при старте сам дописывает в `gaps.tsv` разрывы в двух последних файлах, которых там нет (событие `gap_reconciled` в `connections.tsv`).
- **Файл текущего часа** во время записи: recorder закрывает zstd-фрейм не реже раза в 60 с, поэтому в открытом часе недописан не больше одного фрейма (последней минуты). Скрипт его пропускает и показывает в `open_tail`, это не FAIL. Оборванный хвост в закрытом часе — FAIL: recorder упал и ещё не перезапускался (при старте он переносит хвост в `_torn/`). Старые файлы задачи 001 (один фрейм на час) в открытом часе целиком попадают в `open_tail`.
- **Сессии.** Запись режется на сессии по событиям `connected` из `<feed-root>/connections.tsv` (если файл есть) и везде, где между соседними строками больше `--session-gap-s` (30 с). Сессия длится от первой до последней своей строки. Поэтому простой и бан не занижают блоков/с и МБ/ч. Во время соединения строки идут чаще раза в 2 с (ping), так что порог 30 с внутри сессии не срабатывает.
- **Блоков/с и МБ/ч** считаются по `recv_unix_ns`, то есть по времени прихода, а не по меткам блоков. Метки блоков имеют разрешение в 1 с и немонотонны у отложенных сообщений.
- Скрипт проверяет запись, а не декодирование. Проверку декодеров и `hood.*` по-прежнему делает data-auditor по своему списку.

Проверено 2026-09-30:
- на 10-минутной записи из задачи 001 цифры совпали с независимым аудитом;
- на синтетических файлах: удалены 5 строк — FAIL «gap not listed», с записью в `gaps.tsv` — PASS; обрезанный файл — FAIL по zstd и битому JSON.

Проверено 2026-09-30, задача 008 (офлайн, `--rpc-sample 0`, на копиях):
- запись 002 (`data/feed-test-002`): PASS без WARN; `seq0_lines` ping = 285, `confirmedSequenceNumberMessage` = 26; 2 сессии, 9.958 блока/с и 97.0 МБ/ч по сессиям (по всему интервалу 1.335 и 13.0); 10 фреймов;
- запись 001 (`data/feed`): PASS, цифры как в 001 (9.969 блока/с, 90.6 МБ/ч);
- синтетика: файл обрезан внутри последнего фрейма — в открытом часе PASS с `open_tail`, в закрытом FAIL; мусор после фреймов — FAIL; испорченный байт внутри целого фрейма — FAIL по контрольной сумме; строки seq 0 «прочие» — WARN, seq 0 с `messages` — FAIL; `_torn/`, `connections.tsv`, `gaps.tsv`, `*.tmp` в glob игнорируются.
