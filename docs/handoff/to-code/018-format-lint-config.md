# 018 — Форматирование и конфиги линтеров (задача A ревью 2026-10-02)
status: done
phase: 1a
depends-on: —
executor: indexer-engineer
reviewers: architect-reviewer

## Зачем
`cargo fmt --check` не проходит в enricher, hood-core, decoders; конфигурации ruff нет (131 замечание). Без чистого форматирования любой следующий дифф шумный. Сводка: `docs/reviews/full-2026-10-02-summary.md`, п. 1.

## Что сделать
1. `rustfmt.toml` (`max_width = 120`, `use_small_heuristics = "Max"` — по ревью decoders), `cargo fmt --all`.
2. `pyproject.toml` с секцией `[tool.ruff]` (line-length 120, target py39 — feed_audit.py должен работать на 3.9+; select E,F,W,B,UP,SIM,PL,RUF с разумными ignore), `ruff format` для `analytics/` и `.claude/skills/feed-audit/scripts/`; исправить только автоисправимое и безопасное (`ruff check --fix` без unsafe), остальное — список в отчёте для задачи 024.
3. Один коммит «только форматирование» — без изменения логики.

## Что НЕ делать
- Не менять поведение, имена, флаги. Не трогать сервер.

## Критерии приёмки
- `cargo fmt --all -- --check` чистый; `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` чистые; `ruff format --check` чистый; `py_compile` ок; feed_audit.py выдаёт тот же вывод на `data/feed-test-009` (до/после).
- architect-reviewer: PASS.

## Формат отчёта
Сделано; проверено и как; что осталось для 024. `status: done`, отдельный коммит.
