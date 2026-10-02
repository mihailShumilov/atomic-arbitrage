# 025 — recorder и hood-core: хвосты ревью
status: ready
phase: 1a
depends-on: 021
executor: indexer-engineer
reviewers: architect-reviewer, data-auditor

## Зачем
Остатки ревью 019/021 (`docs/reviews/019-*`, `021-*`, `full-2026-10-02-recorder-architect-reviewer.md`), отложенные как меняющие поведение.

## Что сделать
1. `envelope_seqs` — одна функция подсчёта seq_max/дыр по сообщениям конверта вместо копий в `route.rs` и `rawline.rs`.
2. Порядок записи при смене часа: строка дыры в `gaps.tsv` пишется после того, как строка данных легла на диск (сейчас — раньше). Тест на смену часа.
3. Общий генератор джиттера в `hood-core` (сейчас свой в recorder и в enricher); recorder переходит на него (enricher — задача 026).
4. hood-core: типизированная причина в `LineError`; `subtract` на срезах; уточнить doc `parse_quantity`; `# Errors` в doc публичных функций; решить и задокументировать поведение при не-UTF-8 `gaps.tsv` (сейчас выход 1).
5. По желанию (если мелко): newtype для времени ns в recorder; `Slot::Empty` из ревью.

## Что НЕ делать
- Не менять формат сырья, имена файлов, CLI-флаги, строки и столбцы `connections.tsv`, счётчик страйков (решение Михаила).
- Деплой — отдельным шагом после ревью (один плановый рестарт, команды Михаилу), можно вместе с 028.

## Критерии приёмки
- fmt/clippy/test workspace чистые; все mock-тесты зелёные.
- data-auditor: PASS (сравнение HEAD и нового на моке: досылка, дыры, kill -9, writer error, смена часа — `last_seq` не опережает данные, строка дыры не раньше данных); architect-reviewer: PASS.

## Формат отчёта
Сделано; проверено и как. `status: done`, отдельный коммит.
