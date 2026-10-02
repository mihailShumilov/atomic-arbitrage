# 028 — Python и deploy: хвосты ревью
status: done
phase: 1a
depends-on: 024
executor: indexer-engineer (Python), infra-ops (deploy)
reviewers: architect-reviewer, data-auditor

## Зачем
`docs/reviews/024-*`, `full-2026-10-02-python-bash-sql-architect-reviewer.md` (рекомендации, не вошедшие в 024).

## Что сделать
1. deploy/test: общая обвязка `deploy/test/lib.sh` (check, fake-notify, временные каталоги) вместо шести копий; все test-*.sh на ней.
2. `healthcheck.sh`: атомарная запись файлов состояния (tmp + mv).
3. Список юнитов набора задан в одном месте (сейчас трижды: bootstrap, тест-стенд, README-таблица — минимум скрипты).
4. analytics: `hourly_summary.py` — колонки `*_wei` как int (вывод не меняется); `hourly_metrics.py` — JSON строки разбирается один раз; комментарий к намеренной повторной проверке в `hourly_sample.py`.
5. `test_feed_audit.py`: `FrameLen` — проверки без zstd не пропускать целиком.
6. `feed_audit.py`: опциональный `--seed` для RPC-выборки (по умолчанию вывод не меняется; seed печатается только если задан или выборка > 0).
7. Выкладка на сервер через `bootstrap.sh` — командами для Михаила, вместе с деплоем 025.

## Что НЕ делать
- Не менять вывод аудита и сводок по умолчанию; recorder не трогать.

## Критерии приёмки
- ruff/shellcheck/тесты чистые; deploy/test полностью зелёный (24.04 и 26.04 по возможности); выводы на `data/` прежние.
- data-auditor и architect-reviewer: PASS.

## Формат отчёта
Сделано; проверено и как; команды. `status: done`, отдельный коммит.
