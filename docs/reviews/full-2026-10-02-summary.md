# Полное архитектурное ревью кода — сводка (2026-10-02, HEAD d241243)

Четыре ревью architect-reviewer по скиллу `architect-review`, только чтение, без сети и без доступа к серверу:

| Часть | Файл | Вердикт | Блок. | Важн. | Рек. |
|---|---|---|---|---|---|
| recorder | `full-2026-10-02-recorder-architect-reviewer.md` | PASS с замечаниями (по букве скилла — FAIL из-за двух разошедшихся дублей, см. п. 2–3) | 0 | 9 | 10 |
| enricher + hood-core | `full-2026-10-02-enricher-core-architect-reviewer.md` | FAIL | 2 | 9 | 12 |
| decoders | `full-2026-10-02-decoders-architect-reviewer.md` | FAIL (только `cargo fmt`) | 1 | 7 | 10 |
| Python, Bash, SQL | `full-2026-10-02-python-bash-sql-architect-reviewer.md` | PASS с замечаниями | 0 | 8 | 15 |

Проверено ревьюерами (2026-10-02): `cargo clippy -D warnings` чистый во всех крейтах; тесты проходят (recorder 46 + 14, enricher + hood-core 33, decoders 17); shellcheck, `bash -n`, `py_compile` чистые. Не проходят: `cargo fmt --check` (enricher, hood-core, decoders; recorder чистый), `ruff` (131 замечание, конфигурации нет).

## Общие темы (повторяются в нескольких отчётах)

1. **Форматирование.** `cargo fmt` не проходит в трёх крейтах, нет `rustfmt.toml` и конфигурации ruff. Предложение: `rustfmt.toml` (`max_width = 120`), `ruff` в `pyproject.toml`, один коммит «только форматирование».
2. **Разошедшийся дубль `Retry-After`** (recorder ↔ enricher): recorder понимает HTTP-date, enricher — дробные секунды. Для enricher это риск игнорировать бан провайдера до времени и тратить оплачиваемые вызовы. Одна функция в `hood-core`.
3. **`hood-core` почти не используется**, общая логика скопирована между бинарниками и местами разошлась: диапазоны и вычитание диапазонов, разбор `gaps.tsv` (ошибка ↔ пропуск), fsync каталога (ошибка ↔ глушение), атомарная запись, hex. Внутри recorder — тройное правило «last_seq / устаревший конверт / дыры» и разошедшийся `max_seq_in_file` в восстановлении (ложная строка `gaps.tsv` в редком случае, на данных не встречалось).
4. **Ключ `funding_edges`** (ORDER BY без `kind` и `log_index`, `log_index` Nullable) — тихое схлопывание разных рёбер в ReplacingMergeTree. Таблица пуста; миграция 003 до загрузчика. Плюс: миграции применяются только на пустом томе ClickHouse, 002 локально не применена — нужен `sql/apply.sh` и идемпотентные миграции.
5. **Тихие ошибки:** enricher не проверяет `eth_chainId`; `--gaps` при малом `--max-calls` не продвигается, но выходит 75 (systemd считает успехом); пауза при 429 решается по подстроке в тексте ошибки; в decoders отсутствующий `status` в чеке = неуспех, битый Swap = «не своп»; в feed_audit.py конверт без `header` обрывает аудит дня.
6. **Структура:** recorder без `lib.rs`, `main` 271 строка, `writer.rs`/`net.rs` по 3–5 ответственностей, контракт `connections.tsv` на строковых литералах (читают ещё healthcheck и feed_audit.py); в decoders нет общей модели блока/tx/лога для декодеров 1b; `feed_audit.py` `main()` 310 строк без тестов, устаревшие `utcnow`/`utcfromtimestamp`; тройные дубли в `analytics/`.

## Предлагаемая разбивка на задачи (для Cowork)

| # | Что | Где | Сервер |
|---|---|---|---|
| A | `rustfmt.toml` + `cargo fmt`, ruff-конфиг + форматирование Python — один коммит без изменений логики | все | нет (бинарник не меняется по смыслу; деплой не нужен) |
| B | `hood-core`: `retry_after`, `ranges` (+ разбор `gaps.tsv`), `fsutil` (атомарная запись, fsync каталога, append+fsync), `hex`; перевод enricher и recorder на них | crates | recorder — новый бинарник (рестарт, собрать с C) |
| C | recorder: `lib.rs` + тонкий `main`, `rawline.rs`, `SeqTracker`, enum событий `connections.tsv` + golden-тест формата, разбиение `writer.rs`/`net.rs`, контекст в ошибках; `feed_probe` на общем коде | recorder | один плановый рестарт по runbook (~2 мин) |
| D | enricher: `FailKind` вместо подстроки, проверка `eth_chainId`, проверка «кусок влезает в бюджет», коды выхода в одном месте | enricher | нет (enricher-gaps выключен) |
| E | decoders: `model.rs`, `rows.rs` (enum'ы, `COLUMNS`, `write_tsv`, golden-тест), `decode_swap` → `NotSwap/Swap/Malformed`, ошибка при отсутствии `status`, `checked_add`, реестр шлюзов без дублей | decoders | нет |
| F | SQL: миграция 003 `funding_edges` с ключом `(to_addr, block_number, tx_index, kind, log_index)`, `log_index UInt32` с маркером, `sql/apply.sh`, идемпотентность, закрепить версию образа ClickHouse | sql, docker-compose | нет |
| G | feed_audit.py: время с часовым поясом, счётчик битых конвертов → FAIL, разбиение `main`, `test_feed_audit.py`; `feed-audit-daily.sh` по коду выхода + тест; `analytics/hoodlib.py` | Python, deploy | обновление скриптов `bootstrap.sh`, без рестарта recorder |

Порядок: A → (B, D, E, F, G параллельно) → C (после B, вместе с B один деплой recorder). F — обязательно до задачи загрузчика ClickHouse.

Решения за Михаилом: перенос счётчика банов (strikes) после долгой здоровой сессии (recorder); переход сумм на `UInt256` в SQL (меняет соглашение data-model); побайтовое «сырьё как есть» для ответов RPC в enricher (ломает совместимость файлов `blocks-*`).
