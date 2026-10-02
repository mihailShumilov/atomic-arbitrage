# 021 — Структура recorder (задача C ревью 2026-10-02)
status: done
phase: 1a
depends-on: 019
executor: indexer-engineer
reviewers: architect-reviewer, data-auditor

## Зачем
`docs/reviews/full-2026-10-02-recorder-architect-reviewer.md`: нет `lib.rs`, `main` 271 строка, `writer.rs`/`net.rs` смешивают ответственности, разошедшийся дубль `max_seq_in_file` ↔ `scan_seq_holes`, тройное правило seq, контракт `connections.tsv` на литералах.

## Что сделать (по приоритетному списку отчёта)
1. `rawline.rs` — один разборщик строки сырья; удалить `max_seq_in_file`, восстановление по `seq_max` сообщений; тест на конверт не по порядку на стыке файлов.
2. `SeqTracker` — одно правило last_seq / устаревший конверт / дыры для writer, scan и `Sink::push`.
3. `lib.rs` + тонкий `main.rs` + `app.rs` (цикл переподключений); `run_connection` без четырёх копий ветки закрытия.
4. Enum событий `connections.tsv` с текущими строками, константы столбцов, golden-тест формата (строки и столбцы не меняются — их читают healthcheck и feed_audit.py).
5. Контекст в ошибках commit/recover (имя файла).
6. `examples/feed_probe.rs` — на общем коде из lib, без хардкода URL.
7. Перед изменением п. 1: проверить на копии серверного сырья (rsync только закрытых часов с `hood-rec`, только чтение), есть ли конверты из нескольких сообщений / не по порядку.

## Что НЕ делать
- Не менять формат сырья, имена файлов, CLI-флаги, раскладку `connections.tsv`. Счётчик банов (strikes) не менять — решение Михаила.
- Не деплоить: деплой recorder — отдельным шагом после ревью, один плановый рестарт по runbook, команды для Михаила.

## Критерии приёмки
- fmt/clippy/test чистые; golden-тест `connections.tsv`; все mock-тесты 012 зелёные.
- data-auditor: PASS (свой прогон на моке: досылка, дыры, kill -9, writer error, block_idle — поведение как в 012); architect-reviewer: PASS.

## Формат отчёта
Сделано; было/стало по модулям; проверено и как; команды деплоя для Михаила. `status: done` после деплоя, отдельный коммит.
