# 033 — Строки `hood.swaps` из декодера свопов v3/v4
status: done
phase: 1b
depends-on: 022, 029
executor: indexer-engineer
reviewers: data-auditor, architect-reviewer

## Зачем
`swaps::decode_block` (022) даёт `PoolSwap`, но строк для `hood.swaps` и их загрузки нет.

## Что сделать
1. В `rows.rs` (или `swaps`): `SwapRow` с `COLUMNS` по `hood.swaps` (после 004), `write_tsv`, golden-тест, тест «порядок колонок SQL = COLUMNS», Enum8 `venue`.
2. Семантика из аудитов 022: знак сумм — сторона пула (v4 нормализован); в v4 `Swap` испускается до хуков — суммы = нога AMM, не платёж трейдера (поле/флаг или документировать); нулевые суммы (20 случаев) и `fee = 0` — правило для цены (не делить на 0); эмиттер не проверяется в декодере — фильтр по `verified` PoolManager/фабрикам — параметр (адреса из реестра; пока v4 PoolManager `observed` — не хардкодить).
3. Пример/подкоманда: `data/blocks` + `data/samples` → TSV; счётчики по venue.
4. Если загрузчик 032 уже влит — подключить загрузку `hood.swaps`, иначе оставить точку подключения и описать.

## Что НЕ делать
- RPC — 0; не хардкодить невыверенные адреса.

## Критерии приёмки
- fmt/clippy/test чистые; data-auditor: PASS (13 530 свопов на `data/`, сверка полей с сырыми логами, знаки); architect-reviewer: PASS.

## Формат отчёта
Сделано; проверено и как. `status: done`, отдельный коммит.
