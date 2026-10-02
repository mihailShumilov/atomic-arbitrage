# 022 — Модель и строки декодеров (задача E ревью 2026-10-02)
status: ready
phase: 1b
depends-on: 018
executor: indexer-engineer
reviewers: architect-reviewer, data-auditor

## Зачем
`docs/reviews/full-2026-10-02-decoders-architect-reviewer.md`: нет общей модели блока/tx/лога для декодеров 1b, TSV-строки только в example, ошибки маскируются.

## Что сделать
1. `model.rs`: `Block`, `Tx`, `Receipt`, `Log`, `TxCtx` (позиция в блоке, from/to), `parse_block_line` + проверки согласованности блока и чеков; `l1_inflows` переходит на неё; один тип лога.
2. `rows.rs`: enum `EdgeKind`/`GatewayStatus` вместо `&'static str`, `FundingEdge::COLUMNS`, `write_tsv` (NULL как `\N`), golden-тест на блоке 77312169, тест «значения Enum8 в SQL = enum в Rust».
3. `decode_swap` → `NotSwap | Swap | Malformed` + счётчик; фикстура с реальными логами v3/v4 из `data/blocks` или `data/samples`.
4. Отсутствующий `status` в чеке — ошибка, не «неуспех».
5. `checked_add` для сумм токенов; `hex_u256` не принимает `""`/`"0x"`.
6. `GatewayRegistry::new`: дубль адреса — ошибка (`observed` не перекрывает `verified`).
7. Раскладка модулей по отчёту (model, arbitrum, addresses — только verified, events, rows, l1_inflows/{mod,registry,types}); общий `hex` — из hood-core, если 019 уже влита, иначе оставить и пометить.

## Что НЕ делать
- trait `Decoder` с реестром не вводить (ревью не советует). Не трогать recorder/enricher. RPC — 0.

## Критерии приёмки
- fmt/clippy/test чистые; вывод сканера `l1_inflows_scan` на фикстурах 017 побайтно прежний (кроме NULL → `\N`).
- data-auditor: PASS; architect-reviewer: PASS.

## Формат отчёта
Сделано; новая раскладка; проверено и как. `status: done`, отдельный коммит.
