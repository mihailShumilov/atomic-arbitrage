# 017 — Декодер входов с L1 (0x64, 0x68) для графа финансирования
status: done
phase: 1b
depends-on: 014
executor: indexer-engineer
reviewers: data-auditor

## Зачем
По 014 депозиты с L1 видны только как tx `0x64` (kind 12, без логов), а retryable — как `0x69` + `0x68` (kind 9). Это вход денег на кошельки, первое ребро графа финансирования. Решение Михаила 2026-10-01: делать.

## Контекст
- `references/data-model.md`, раздел «L1-сообщения фида» (таблица kind → tx → что брать), оговорка про `feeRefundAddr` и onchain-фильтр (data-auditor 014, З3).
- `sql/001_schema.sql` — таблица `funding_edges`; `crates/decoders`.
- Фикстуры: блок 77285531, tx `0x3f8e47b0…b522c1` (kind 12); блок 77312169 (kind 9 с WETH-шлюзом); сырые ответы RPC 014 и блоки в `data/blocks`, `data/samples`.

## Что сделать
1. В `crates/decoders`: разбор блока (формат `blocks-*.jsonl.zst`) → строки входов с L1: `0x64` (from = alias, to, value, l1RequestId), `0x68` с value > 0 (retryTo, value, ticket id), источник = unalias(from). Если `0x68` вызывает шлюз — получатель и токен из логов (WETH через шлюз — пометить как токен-вход, адреса из `contracts.md`, статус не ниже `observed`).
2. Схема: соответствие полям `funding_edges` (при необходимости — миграция `002_…`), без загрузки в ClickHouse (загрузчик — отдельная задача).
3. Тесты на фикстурах (kind 12, kind 9 без шлюза, kind 9 со шлюзом), прогон по `data/samples/hourly-*.jsonl.zst` и `data/blocks/*` — счётчики.
4. Неучтённые потоки (`feeRefundAddr`, FilteredFundsRecipient) — явно в документации и счётчике «не учтено».

## Что НЕ делать
- Не трогать recorder и формат сырья; не подключаться к фиду; RPC — не больше 20 вызовов (с Mac), только если фикстур не хватает.

## Критерии приёмки
- `cargo test --workspace`, clippy чистые.
- data-auditor: PASS — входы на фикстурах совпадают с RPC/фидом (to, value, источник), нет двойного счёта `0x69`/`0x68`.

## Формат отчёта
Сделано; проверено и как; счётчики по выборкам; что не получилось. `status: done`, отдельный коммит.
