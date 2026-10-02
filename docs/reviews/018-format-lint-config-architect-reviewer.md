# 018 — архитектурное ревью (architect-reviewer)

Дата: 2026-10-02. Объём: рабочее дерево против HEAD (`842bcf5`), без коммита: 23 файла `.rs` (recorder 8, enricher 11, decoders 4), 4 файла `.py`, новые `rustfmt.toml`, `pyproject.toml`; плюс `docs/handoff/...018...`. `docs/STATE.md` тоже изменён, но к задаче не относится, в коммит 018 его включать не нужно.

**Вердикт: PASS с замечаниями.** Блокирующих замечаний нет. Изменение только форматирующее, это доказано строго (см. ниже). Остальные замечания — рекомендации, их можно сделать в 024 или отдельно.

## Линт и проверки (проверено 2026-10-02 на Mac, локально, без сети, кроме `uvx`)

| Проверка | Результат |
|---|---|
| Rust: только форматирование | Взял HEAD через `git archive HEAD crates Cargo.toml Cargo.lock` во временный каталог, положил туда новый `rustfmt.toml`, выполнил `cargo fmt --all`. Все `crates/**/*.rs` побайтно совпали с рабочим деревом (`cmp`, 0 отличий). Значит, дифф Rust — ровно вывод rustfmt, а rustfmt сохраняет семантику. Это сильнее проверки исполнителя через «удалить пробелы и `,{}`» |
| `cargo fmt --all -- --check` | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0, предупреждений нет |
| `cargo test --workspace` | все `test result: ok`, 0 упавших (5+12+17+2+4+2+2+4+2+46+14 = 110) |
| Python: AST | `ast.dump(ast.parse())` для `git show HEAD:<f>` и рабочей копии совпадает для всех 4 файлов (Python 3.9.6) |
| Python: комментарии | Сравнил через `tokenize`. Изменились только 2: в `hourly_summary.py` удалён `# noqa: E402`, в `feed_audit.py:518` `# noqa: BLE001 - report, don't crash the audit` заменён на `# report, don't crash the audit` |
| `uvx ruff format --check analytics .claude/skills/feed-audit/scripts` (ruff 0.16.10) | `5 files already formatted` |
| `uvx ruff check` (с конфигурацией из pyproject) | 92 замечания, распределение по правилам совпадает с таблицей в отчёте исполнителя (UP031 32, E501 26, SIM115 8, …). Автоисправимых безопасных нет |
| `python3 -m py_compile` | ок для всех 4 файлов |
| B905 | при `py39` не выдаётся (`All checks passed`); при `--target-version py310` — 7 мест, все в `analytics/` (hourly_summary 5, hourly_metrics 1, hourly_sample 1) |
| `cargo clean` | выполнен (1.7 GiB) |

## Блокирующее

Нет.

## Важное

Нет.

## Рекомендации

1. **[`.git-blame-ignore-revs` — нет файла] Коммит 018 зашумит `git blame`.** Recorder: −1059/+277 строк из-за `use_small_heuristics = "Max"`, это почти весь `writer.rs`, `net.rs` и `mock_feed.rs`. Исправление: после коммита добавить в корень `.git-blame-ignore-revs` с хешем коммита 018 (одна строка и комментарий) и `git config blame.ignoreRevsFile .git-blame-ignore-revs`. Это можно сделать отдельным мелким коммитом сразу после 018: хеш до коммита неизвестен.
2. **[`pyproject.toml`: `extend-exclude`] `ruff format .` из корня затронет чужие файлы.** `uvx ruff format --check .` хочет переформатировать блоки кода в 4 markdown-файлах: в 3 файлах стороннего скилла `.claude/skills/clickhouse-best-practices/` (`AGENTS.md`, `rules/agent-connect-mcp.md`, `rules/insert-format-native.md`; это vendored, см. `skills-lock.json`) и в `docs/reviews/full-2026-10-02-python-bash-sql-architect-reviewer.md`. Если кто-то запустит `ruff format .` без путей, получится дифф в vendored-скилле. Исправление: `extend-exclude = ["data", "target", "deploy", "docs", ".claude/skills/clickhouse-*"]`. `ruff check .` уже чистый по scope (те же 92 замечания).
3. **[`pyproject.toml`, комментарий о B905] Комментарий неточен.** Там написано, что B905 «must stay off for feed_audit.py», но в feed_audit.py `zip()` без `strict` не срабатывает, все 7 мест — в `analytics/`. Смысл верный: `target-version = py39` действует на весь репозиторий, а `strict=` появился в 3.10. Исправление: `# B905 needs Python 3.10 (zip(strict=)); stays silent while target-version is py39.`
4. **[`rustfmt.toml`] Нет `edition`.** `cargo fmt` берёт edition 2021 из workspace, а голый `rustfmt file.rs` (некоторые редакторы, pre-commit) по умолчанию берёт 2015. Исправление: `edition = "2021"` в `rustfmt.toml`. Не обязательно, на текущий результат не влияет.
5. **Статус задачи.** В рабочем дереве `docs/handoff/to-code/018-format-lint-config.md` стоит `status: in-progress`, а отчёт исполнителя пишет `status: done`. Перед коммитом выставить `done`. Это делает координатор, по протоколу cowork-handoff.

## Конфиги — оценка

- `rustfmt.toml`: `max_width = 120` и `use_small_heuristics = "Max"` разумны и согласованы с `line-length = 120` у ruff. Цена — разовый крупный дифф recorder, его снимает рекомендация 1. Нестабильных опций, которым нужен nightly, нет.
- `[tool.ruff]`: `target-version = "py39"` верно, feed_audit.py работает на Mac под 3.9.6. Набор `select = E,F,W,B,UP,SIM,PL,RUF` совпадает со скиллом architect-review. `quote-style = "double"` — это умолчание, строка лишняя, но безвредна.
- `ignore = ["PLR2004"]` обосновано. Здесь magic values — пороги отчётов и индексы TSV-колонок, именованные константы на каждую не улучшат читаемость. Важные константы сети живут в Rust (`hood-core`), а не в этих скриптах. Глобальный ignore лучше россыпи `noqa`.

## Удалённые `noqa`

- `hourly_summary.py`, `# noqa: E402` у `import hourly_metrics as hm` после `sys.path.insert`. Ruff не считает такой импорт нарушением E402 (`sys.path` — разрешённое исключение), поэтому RUF100 его удалил. Смысла не потеряно: ruff — единственный линтер проекта. flake8 бы ругался, но его в проекте нет.
- `feed_audit.py:518`, `except Exception as e`. Код BLE001 бесполезен, пока BLE не выбран, а пояснение «report, don't crash the audit» сохранено обычным комментарием, и намерение широкого `except` по-прежнему задокументировано. Если BLE когда-нибудь включат, `noqa` придётся вернуть. Это нормально.

## UP031 (%-формат) в feed_audit.py — мнение

**Рекомендую per-file ignore для feed_audit.py, не переписывать.** А одно место в `hourly_summary.py:161` — переписать в 024.

Почему:
- 31 место в feed_audit.py — это сообщения FAIL/WARN и выравнивание текстового вывода (`"%-18s %s"`, `"%d duplicate sequence numbers"`). Дефектов там нет, %-формат в Python 3.9+ полностью поддерживается и читается нормально.
- feed_audit.py — аудиторский скрипт, его копия стоит на сервере, а критерий приёмки любых его правок — побайтно тот же вывод до/после. Автоисправление ruff помечено unsafe (у `%` и f-строк разная семантика с кортежами, `%r`, `%-18s`↔`:<18`). Ручной перевод 31 места — риск без пользы, а ещё повод обновлять серверную копию.
- Если в 024/G всё равно будут разбивать `main()` (PLR0912/0915, 223 оператора), тогда и переводить на f-строки строки, которые и так трогаются. ignore можно снять, когда мест не останется.

Набросок для `pyproject.toml`:

```toml
[tool.ruff.lint.per-file-ignores]
# Audit output strings; byte-identical output matters more than f-string style.
".claude/skills/feed-audit/scripts/feed_audit.py" = ["UP031"]
```

## Что хорошо

- Строгая дисциплина «только форматирование»: AST неизменен, Rust равен `cargo fmt(HEAD)`. Исполнитель дополнительно сверил вывод `feed_audit.py` и `hourly_*` до/после и `shasum` файлов `data/`.
- `ruff check --fix` выполнен без `--unsafe-fixes`. Всё неавтоматическое вынесено списком в 024 с номерами строк.
- Пояснение у бывшего `noqa: BLE001` не потеряно.
- Решение по UP031 оставлено явным, его не спрятали в ignore молча.

## Предполагается / не проверено

- Вывод `feed_audit.py` и `hourly_*.py` до/после сам не прогонял: опираюсь на совпадение AST. Оно гарантирует тот же байткод с точностью до номеров строк, номера строк в выводе не используются. Прогоны исполнителя описаны в его отчёте.
- Под Python 3.14 (сервер) не запускал. При неизменном AST и синтаксисе 3.9 поведение должно совпасть.
- Бинарники Rust побайтно не сравнивал. Достаточно того, что исходники совпадают с выводом rustfmt.
