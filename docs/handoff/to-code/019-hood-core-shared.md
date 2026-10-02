# 019 — Общий код в hood-core (задача B ревью 2026-10-02)
status: ready
phase: 1a
depends-on: 018
executor: indexer-engineer
reviewers: architect-reviewer, data-auditor

## Зачем
Логика скопирована между recorder и enricher и уже разошлась (сводка `docs/reviews/full-2026-10-02-summary.md`, п. 2–3; отчёты recorder и enricher-core).

## Что сделать
Модули `hood-core` (только std/chrono из workspace, без IO-политики внутри), с тестами:
1. `http::parse_retry_after(value, now)` — целые и дробные секунды и HTTP-date; recorder и enricher используют её, каждый со своим потолком.
2. `ranges` — тип диапазона (вместо `Gap` и `ranges.rs` enricher), вычитание, склейка; разбор/запись `gaps.tsv` и `filled.tsv` с одной политикой строгости (недописанная последняя строка без `\n` — пропуск с WARN, битая строка с `\n` — ошибка, как в 012).
3. `fsutil` — атомарная запись (tmp + fsync + rename + fsync каталога), append строки с fsync; одна политика ошибок fsync каталога (ошибка, не глушение — если recorder где-то глушит намеренно, обосновать в коде).
4. `hex` — разбор/формат hex-чисел и адресов, общий для enricher/decoders, если используется в 2+ местах.
5. Перевести enricher и recorder на эти модули; удалить копии; `CHAIN_ID` — использовать или удалить.

## Что НЕ делать
- Не менять форматы файлов, CLI-флаги, раскладку `connections.tsv`; не деплоить recorder (деплой — вместе с 021).
- Поведение меняется только в одном месте: recorder начинает понимать дробный `Retry-After`, enricher — HTTP-date. Описать в отчёте.

## Критерии приёмки
- fmt/clippy/test workspace чистые; тесты на каждый модуль hood-core (включая HTTP-date и дробные секунды, вычитание диапазонов, строгость разбора gaps).
- data-auditor: PASS (форматы `gaps.tsv`/`filled.tsv` побайтно прежние на копиях `data/feed-test-009`, `data/blocks`); architect-reviewer: PASS.

## Формат отчёта
Сделано; удалённые дубли (было/стало, файл:строка); проверено и как. `status: done`, отдельный коммит.
