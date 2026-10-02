# 023 — Миграции ClickHouse: ключ funding_edges, применение миграций (задача F ревью 2026-10-02)
status: done
phase: 1b-подготовка
depends-on: 018
executor: indexer-engineer
reviewers: architect-reviewer, data-auditor

## Зачем
Ключ `funding_edges` молча схлопывает разные рёбра; миграции применяются только на пустом томе (002 локально не применена). Сводка п. 4; отчёты decoders (В1) и python-bash-sql (п. 1–2).

## Что сделать
1. `sql/003_*.sql`: новая `funding_edges` — `log_index UInt32 DEFAULT 4294967295` (маркер «ребро уровня tx»), `ORDER BY (to_addr, block_number, tx_index, kind, log_index)`, все колонки 002; перенос через новую таблицу + `EXCHANGE TABLES`; перед DROP — проверка, что таблица пуста (миграция падает, если нет). Согласовать с `rows.rs` задачи 022 (порядок колонок, NULL).
2. `sql/apply.sh`: применяет миграции по порядку к работающему ClickHouse (порт 18123 из `.env`), ведёт таблицу версий, каждая миграция идемпотентна; исправить неверный комментарий в 002.
3. Закрепить версию образа ClickHouse в `docker-compose.yml` (вместо `latest`), порты прежние (127.0.0.1:18123/19100).
4. Прогнать локально: `docker compose up -d clickhouse`, `sql/apply.sh` дважды (второй — без изменений), `DESCRIBE` таблиц.

## Что НЕ делать
- Не менять тип сумм на `UInt256` (решение Михаила). Не ставить ClickHouse на сервер.

## Критерии приёмки
- Миграции применяются на пустом и на существующем томе; второй прогон — 0 изменений; схема совпадает с `FundingEdge`.
- data-auditor: PASS; architect-reviewer: PASS.

## Формат отчёта
Сделано; проверено и как (вывод DESCRIBE). `status: done`, отдельный коммит.
