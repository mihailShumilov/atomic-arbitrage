# 024 — feed_audit.py, обёртка аудита, общий модуль analytics (задача G ревью 2026-10-02)
status: done
phase: 1a
depends-on: 018
executor: indexer-engineer (Python), infra-ops (deploy/feed-audit-daily.sh и выкладка)
reviewers: architect-reviewer, data-auditor

## Зачем
`docs/reviews/full-2026-10-02-python-bash-sql-architect-reviewer.md` п. 3–5: устаревшие `utcnow`/`utcfromtimestamp` (Python 3.14 на сервере), битый конверт обрывает аудит дня, `main()` 310 строк без тестов, обёртка берёт вердикт из текста, тройные дубли в `analytics/`.

## Что сделать
1. feed_audit.py: время с часовым поясом (совместимо с 3.9); счётчик битых конвертов → FAIL с вердиктом, а не traceback; разбиение `main` на функции, dataclass `Tally`; остаётся одним файлом stdlib-only (ставится bootstrap'ом); `test_feed_audit.py` на unittest (синтетика: нормальный день, битый конверт, дыра, open tail, seq 0); вывод на `data/feed-test-009` и `data/feed` — как до правок (кроме исчезнувших предупреждений).
2. `deploy/feed-audit-daily.sh`: вердикт по коду выхода, `test-feed-audit-daily.sh`; shellcheck.
3. `analytics/hoodlib.py`: чтение `*.jsonl.zst` потоком, отбор пользовательских tx (`0x6a` — системная), комиссия, проверка receipts = tx и хэшей; `hourly_*.py` переходят на него; результаты `hourly_summary` на `data/samples` — прежние.
4. Остаток замечаний ruff из 018.
5. Выкладка на сервер: rsync закоммиченного состояния + `bootstrap.sh` — командами для Михаила; recorder не перезапускается.

## Что НЕ делать
- feed_audit.py в общий модуль не переводить (он независим и ставится одним файлом). RPC — 0. Recorder не трогать.

## Критерии приёмки
- ruff/format/py_compile/shellcheck чистые; тесты зелёные; вывод аудита и сводок прежний.
- data-auditor: PASS; architect-reviewer: PASS; на сервере `feed-audit` по прошлым суткам без предупреждений, recorder NRestarts без изменений.

## Формат отчёта
Сделано; проверено и как; команды для Михаила. `status: done` после выкладки, отдельный коммит.
