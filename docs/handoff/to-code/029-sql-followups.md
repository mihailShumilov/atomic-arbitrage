# 029 — Схема ClickHouse: хвосты ревью
status: done
phase: 1b-подготовка
depends-on: 023
executor: indexer-engineer
reviewers: architect-reviewer, data-auditor

## Зачем
`docs/reviews/023-*`, `full-2026-10-02-python-bash-sql-architect-reviewer.md` (Р7–Р9 и SQL-рекомендации). Таблицы пусты — менять схему сейчас дёшево.

## Что сделать
1. Миграция 004: типы по best practices и `data-model.md` — убрать лишние Nullable, `selector` как `FixedString(10)` или единообразно String, `venue`/перечисления как `LowCardinality(String)`/Enum; ключ `labels` с `source`; у `feed_gaps` колонка версии для ReplacingMergeTree. Через новую таблицу + `RENAME` (не EXCHANGE), guard от непустых таблиц. Тип сумм не менять (UInt256 — решение Михаила).
2. `sql/test_apply.sh`: строгая проверка числа стейтментов; тест awk-разбора SQL.
3. В `data-model.md` запрет `CREATE OR REPLACE`/`REPLACE TABLE` рядом с запретом EXCHANGE (проверить на bind-монтировании, как в 023); комментарий в 003 об остающейся пустой `funding_edges_003` после неудачного прогона.
4. Локально: применить, второй прогон 0 изменений, рестарт контейнера; убрать пустой каталог старой таблицы в `data/clickhouse/store`, если ClickHouse его не убирает (только локально).
5. Обновить `crates/decoders` тест порядка колонок, если затронуты таблицы с Rust-отображением.

## Что НЕ делать
- Не ставить ClickHouse на сервер; порты прежние (127.0.0.1:18123/19100).

## Критерии приёмки
- apply.sh на пустом и существующем томе, повтор — 0 изменений; test_apply.sh зелёный; DESCRIBE в отчёте.
- data-auditor и architect-reviewer: PASS.

## Формат отчёта
Сделано; проверено и как. `status: done`, отдельный коммит.
