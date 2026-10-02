# 029 — Схема ClickHouse: хвосты ревью. Ревью data-auditor

date: 2026-10-03
reviewer: data-auditor
объект: рабочее дерево поверх HEAD `179e353` (не закоммичено): новый `sql/004_types_keys.sql` (sha256 `e51ef320…`), правки `sql/apply.sh` (запрет DDL) и `sql/test_apply.sh`, раздел «Схема и миграции» и «Соглашения» в `references/data-model.md`. Отчёт: `docs/handoff/from-code/029-sql-followups.md`.

**Вердикт: PASS.** Все пункты проверены моими прогонами, блокирующих замечаний нет. Замечания З1–З5 ниже не блокируют.

## Как проверял

- **Сеть:** RPC 0, фид 0, ssh 0. Код не менял, `cargo clean` не запускал. Записан только этот файл.
- **Временный ClickHouse:**
  - контейнер `hood-audit029-a477d6fc`, образ `clickhouse/clickhouse-server:26.9.6.6` (id `eb4870e7ca7e`);
  - bind-монтирование `scratchpad/029-audit/chdata` → `/var/lib/clickhouse`, ФС в контейнере `fakeowner`, как у рабочего тома;
  - опубликован только `127.0.0.1:28391` → 8123, одноразовый пароль в файле 0600, `ENV_FILE=/dev/null` (основной `.env` скриптом не читался).
- **Основной локальный ClickHouse** (`127.0.0.1:18123`): только `SELECT`/`DESCRIBE`. Пароль читался из `.env` в переменную и не печатался.
- **Уборка:** `docker rm -f`, каталог `029-audit` с данными и паролем удалён. `docker ps -a | grep audit029` → 0. Анонимных томов нет: `VOLUME` перекрыт bind-монтированием.

## 1. Офлайн-тест и Rust — PASS

- `bash sql/test_apply.sh` → `ok: 23 checks`, exit 0.
- Прочитал новые проверки:
  - число стейтментов строго `-eq 2`, служебные запросы в отдельном файле;
  - лексер сравнивается с ожидаемым текстом целиком;
  - незакрытая кавычка → отказ до отправки, журнал пуст;
  - 3 варианта запрещённого DDL, в том числе с переносом строки;
  - нет ложного срабатывания на колонки `exchange`/`replace`.
- `cargo test -p decoders` → 19 + 23 + 1 + 8 passed, 0 failed. В том числе `funding_edges_column_order_in_sql_equals_columns`, `enum8_in_sql_equals_rust_enums`, `every_column_is_defined_in_sql`: они читают все `sql/*.sql`, включая 004.

## 2. apply.sh: свежий том, повтор, рестарт — PASS

| шаг | результат |
|---|---|
| прогон 1 | `applied` 001–004, `changes: 4`, exit 0 |
| прогон 2 | `already applied: 4`, `changes: 0` |
| `docker restart` + прогон 3 | `changes: 0` |
| `metadata/hood/` после рестарта | 10 `.sql`: 9 таблиц + `schema_migrations` |
| каталоги `store` без таблицы/базы | 0 |

sha256 в журнале равны файлам: `c8624f53…`, `c969fff5…`, `7ef02d0f…`, `e51ef320…`.

## 3. Отказ 004 на непустой таблице, частичный прогон, остатки — PASS

Подготовка: `DROP DATABASE hood SYNC`, `apply.sh --to 3`, одна строка в `labels` (старый ключ).

| сценарий | результат |
|---|---|
| `apply.sh` при строке в `labels` | exit 1, `Code: 395 … 004: hood.labels is not empty`. Строка цела, ключ `labels` старый (`address, label`), записи 4 в журнале нет. `txs`…`wallets` уже пересозданы, `feed_gaps` старая |
| повтор, строка ещё на месте | снова exit 1 на `labels`, строка цела |
| `TRUNCATE labels`, повтор | `applied: 004`, `changes: 1`; `labels`/`feed_gaps` с новыми ключом/версией; остатков `*_004`/`*_pre004` 0; DESCRIBE 9 таблиц совпал со свежим томом (`diff` пуст) |
| остаток `txs_pre004` с 1 строкой, `txs` пуста, записи 4 нет | guard 0, exit 1: `Code: 57 … txs_pre004 already exists`. Строка в `txs_pre004` цела, записи 4 нет |
| то же + строка в `txs` | exit 1 на первом `throwIf` (`Code: 395 … txs is not empty`), обе строки целы |
| остатки удалены, `txs` непуста, записи 4 нет | guard 1 → `skipped: 004`, строка цела |
| обрыв между двумя `RENAME` у `labels` (`labels` нет; есть `labels_004` и `labels_pre004`) | exit 1, `Code: 60 Unknown table … hood.labels`. После ручного доведения по шапке 004 (`RENAME`, `DROP` пустой `pre004`) — прогон проходит |
| пустой остаток `wallets_004`, записи 4 нет | guard 0, файл выполнен, остаток переиспользован, `changes: 1`, остатков 0 |

Тихих потерь нет: в каждом случае либо громкий отказ с целыми данными, либо корректное доведение.

## 4. DESCRIBE против data-model.md — PASS

- Имена и порядок колонок у 7 пересозданных таблиц совпадают с состоянием после 001–003 (`diff` по именам пуст). Типы изменились ровно так, как перечислено в шапке 004 и в блоке «Типы и ключи (004)» `data-model.md`:
  - `txs.to_addr`, `logs.topic1..3`, `swaps.router`, `wallets.first_funder`, `first_funding_wei` → `String DEFAULT ''`;
  - `wallets.first_funding_block` → `UInt64 DEFAULT 0`;
  - `txs.selector` → `LowCardinality(String) DEFAULT ''`;
  - `tokens.venue` → тот же `Enum8`, что у `swaps.venue`.
- Движки:
  - `labels` — `ReplacingMergeTree(updated_at) ORDER BY (address, label, source)`;
  - `feed_gaps` — `ReplacingMergeTree(filled) ORDER BY from_seq`;
  - остальные — без изменений.
- Единственная `Nullable`-колонка в `hood` — `blocks.feed_recv_ns`, как и задумано.
- **`funding_edges` не тронута:** `SHOW CREATE` после `--to 3` и после 004 побайтно равны. `blocks` тоже без изменений.
- Сверено с best practices (`clickhouse-best-practices`):
  - `schema-types-avoid-nullable` — соблюдено;
  - `schema-types-lowcardinality` — `selector`, `label`, `source`;
  - у ReplacingMergeTree `labels`/`feed_gaps` есть колонка версии;
  - `schema-pk-plan-before-creation` — ключи меняются, пока таблицы пусты.

## 5. Семантика — PASS

- **`feed_gaps`.** Вставки `(10,20,filled 1)` → `(10,20,filled 0)`; `(30,40, filled DEFAULT)` → `(30,40,1)` → `(30,40,0)`.
  - До слияния `count()` = 5, `uniqExact(from_seq)` = 2.
  - `FINAL` и после `OPTIMIZE FINAL` — 2 строки, обе с `filled = 1`, независимо от порядка вставки.
- **`labels`.** `bot` от `h1` (10:00:00), от `h2` (10:00:01), снова от `h1` (10:00:05, conf 0.95). После `OPTIMIZE FINAL` 2 строки: `h1` с последней версией (0.95) и `h2`.
- **`venue`.** `'bogus'` в `tokens` и в `swaps` → `Code: 691 Unknown element 'bogus' for enum`, строка не записана. `'pons_v2_curve'` в `tokens` вставляется.
- **Строки «как от загрузчика»** (TSV/JSONEachRow):
  - `txs` с пустыми `to_addr`/`selector` → `''`, тип `String`/`LowCardinality(String)`. С `selector = 0xa9059cbb` → длина 10;
  - `logs` с 0 и 2 топиками, JSONEachRow без `topic1..3` → `''`;
  - `swaps` с пустым `router` → `''`;
  - `wallets` без финансирования (пустые поля и JSON без полей) → `''`/`0`/`''`. С финансированием — значения на месте.

## 6. Запрет `EXCHANGE` / `CREATE OR REPLACE` / `REPLACE TABLE` — PASS

- **Через apply.sh на живом сервере.** Копия `sql/` с `005_banned.sql`: первый стейтмент безобидный, второй — `/* c */ Create   Or\n\tReplace TABLE …`. Результат: exit 1 с сообщением о запрете. Таблица из первого стейтмента не создана, то есть отказ до отправки чего-либо. Записи 5 нет.
- **Независимое воспроизведение причины запрета** (тот же контейнер, `fakeowner`):
  - Сделал `CREATE OR REPLACE TABLE xt.a`, в таблице 1 строка.
  - Ответ: `Code: 1001 filesystem error: in rename: No such file or directory [... _tmp_replace_...]`, хотя замена уже выполнена (колонки `x, y`, 0 строк). В `system.tables` висит призрак `_tmp_replace_…`.
  - После `docker restart`: `xt.a` с новой схемой, а в `store` сирота с частью `all_1_1_0` (данные старой таблицы).
  - Запись в `data-model.md` подтверждена.
- **Основной том после этого:** `apply.sh` → `changes: 0`, сирот от `hood` нет.

## 7. Основной локальный ClickHouse (только чтение) — PASS

Проверено 2026-10-03:
- `version()` = 26.9.6.6.
- `hood.schema_migrations`, sha256 всех строк равны текущим файлам:

  | версия | исход | время |
  |---|---|---|
  | 001 | `applied` | 17:54 |
  | 002 | `applied` | 17:54 |
  | 003 | `skipped` | 18:04 |
  | 004 | `applied` | 2026-10-02 21:06:28 UTC |

- Все 9 таблиц `hood.*` — 0 строк, detached-частей 0, остатков `*_003`/`*_004`/`*_pre00N` нет.
- DESCRIBE 9 таблиц (`txs`, `logs`, `swaps`, `tokens`, `wallets`, `labels`, `feed_gaps`, `funding_edges`, `blocks`) побайтно равен DESCRIBE свежего временного тома. Движки и ключи совпадают.
- `data/clickhouse/metadata/hood/`: 10 `.sql`.

## Замечания (не блокируют)

- **З1. Пустые поля и `\N` молча становятся значением по умолчанию.** Проверено 2026-10-03 на 26.9.6.6 (`input_format_null_as_default = 1`):
  - `\N` в бывшей Nullable-колонке (`txs.to_addr`) записывается как `''` без ошибки;
  - пустое поле TSV в `UInt64` записывается как `0` без ошибки. Это касается и `block_number`: строка с пустым номером блока легла как блок 0.
  - После 004 «не найдено» и «загрузчик потерял значение» неразличимы (`''`/`0`).
  - Что предлагаю: загрузчики `txs`/`logs`/`swaps`/`wallets` (задачи 1b) должны валидировать поля до вставки. В аудит загрузки — проверку `block_number > 0` и долю `''` в `to_addr`/`router` против числа созданий контрактов в receipts.
  - Само по себе это не дефект 004: пустое поле в `UInt64` давало 0 и раньше.
- **З2. Ключ `feed_gaps` — только `from_seq`** (так с 001).
  - Две строки с одним `from_seq` и разным `to_seq` схлопнутся. С версией `filled` побеждает закрытая, даже если её `to_seq` короче.
  - Сейчас recorder пишет одну дыру на один `from_seq`, так что это теоретический случай. Правило стоит описать в задаче загрузчика `gaps.tsv`.
- **З3. Маркеры guard проверяют по одной колонке на таблицу** (`to_addr`, `topic1`, `router`, `venue`, `first_funder`), а не `selector`/`topic2..3`/`first_funding_block`.
  - При ручной частичной правке схемы guard может дать ложный `skipped`.
  - Для миграции, которую пишет только apply.sh, приемлемо.
- **З4. Комментарий в 003 не добавлен** (отступление от п. 3 задачи). Причина верная: 003 в журнале, sha256 неизменяем. Описание есть в `data-model.md` и в шапке 004. Решение за Михаилом/Cowork, как и З4 ревью 023 (`CLAUDE.md`, «Команды»).
- **З5. `venue` как `Enum8`** — схемный выбор.
  - Плюс: неизвестная площадка отклоняется при вставке (проверено).
  - Минус: новая площадка требует миграции `MODIFY COLUMN` в двух таблицах. Это отмечено в `data-model.md`.

## Проверено и предполагается

- **Проверено (2026-10-03, мои прогоны на временном контейнере и чтение основного тома):** п. 1–7, З1–З2 (поведение вставки).
- **Предполагается, не проверено:**
  - поведение запрещённых DDL на ext4/xfs Linux; запрет общий, проверялся только `fakeowner`;
  - `apply.sh`/`test_apply.sh` под GNU bash 4+/awk/tr: проверено только на macOS;
  - `wallets.first_funding_block = 0` как «не найдено» исходит из того, что финансирования в блоке 0 нет. На цепи не проверялось;
  - нагрузка и стоимость `FINAL`: проверено на единицах строк.
