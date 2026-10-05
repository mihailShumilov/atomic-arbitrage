# 036 — Отдельная площадка для пулов Pons v2 (хук мем-пулов)
status: done
phase: 1b
depends-on: 033, 035
executor: indexer-engineer
reviewers: architect-reviewer, data-auditor

## Зачем
Михаил 2026-10-05 решил: у пулов Uniswap v4 с хуком Pons v2 (`0xe5e702641ea86f4ae6cc3cdaed2b886f976be044`, `verified`) своя площадка. Сейчас они попадают в `other` (947 пулов в реестре 035). Событие v4 `Swap` пишется до хука, поэтому в таких строках — только AMM-часть сделки; аналитика должна отличать их по `venue`, не по списку адресов.

## Что сделать
1. Миграция `sql/005_*.sql`: новое значение Enum8 `venue` (имя — `pons_v2_hook`, значение — следующее свободное, например 6) в `hood.swaps` и `hood.tokens`. Порядок значений не менять. Тот же безопасный способ, что в 003/004: без EXCHANGE TABLES / CREATE OR REPLACE / REPLACE TABLE, с проверками, через `sql/apply.sh` и журнал `hood.schema_migrations`. Если хватает `ALTER TABLE … MODIFY COLUMN` с расширением Enum8 (только добавление значения) — так проще; проверить на локальном ClickHouse (127.0.0.1:18123).
2. decoders: `Venue::PonsV2Hook` в `rows.rs` (+`ALL`, `as_str`, `parse`, `enum8`); тест сверки с SQL должен читать уже последнюю миграцию.
3. Классификация: v4-пул, у которого `hooks` в PoolKey = verified хук Pons v2, → `pons_v2_hook`. Адрес хука — из реестра/параметра, по правилам `contracts.md`, без «зашитых» непроверенных адресов. Пулы v4 без хука → `uni_v4`; с другим неизвестным хуком → `other`, как сейчас.
4. Документация: `data-model.md` (значения `venue`, оговорка «только AMM-часть»), docstring'и.
5. Прогон `swaps_scan` с реестром `data/registry/*-035.tsv`: сколько строк перешло из `other` в `pons_v2_hook`.

## Что НЕ делать
- Сеть: 0 вызовов RPC и фида. Сервер не трогать. Не коммитить `data/`.

## Критерии приёмки
- `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace` чистые; миграции 001–005 применяются на пустом томе и повторно (идемпотентно).
- Числа по площадкам до/после.
- architect-reviewer и data-auditor: PASS.

## Формат отчёта
Сделано; что проверено и как (дата); числа; что не получилось; вопросы. `status: done` только после ревью, отдельный коммит.
