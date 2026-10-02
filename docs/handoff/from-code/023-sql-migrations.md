# 023 — Миграции ClickHouse: ключ funding_edges, применение миграций. Отчёт

Дата: 2026-10-02. Исполнитель: indexer-engineer. HEAD `449576f`, не закоммичено. Статус задачи не менял, ревьюеров не запускал (по указанию координатора).

Отчёт обновлён по запросу координатора после задачи 022. Изменения:
- порядок колонок приведён к `FundingEdge::COLUMNS`;
- добавлен тест на этот порядок;
- **`EXCHANGE TABLES` заменён на два `RENAME`**: на bind-монтировании macOS он терял метаданные таблицы, см. «Найдено»;
- локальный том пересобран.

## Сделано

1. **`sql/003_funding_edges_key.sql`** — новая `hood.funding_edges`:
   - `log_index UInt32 DEFAULT 4294967295` (маркер «ребро уровня tx»), `ORDER BY (to_addr, block_number, tx_index, kind, log_index)`;
   - порядок колонок как в 001 + 002, он равен `FundingEdge::COLUMNS` (022): `block_number, tx_index, from_addr, to_addr, value_wei, kind, tx_hash, log_index, token, l1_token, gateway, gateway_status, l2_alias, tx_type, l1_request_id, ticket_id`;
   - типы, DEFAULT и значения Enum8 — как в 001 + 002 (`kind`: eth=1, weth=2, internal=3, l1_eth=4, l1_token=5; `gateway_status`: none=0, observed=1, verified=2). Каждое определение Enum8 записано одной строкой, их разбирает тест 022;
   - порядок действий: `throwIf` (в старой таблице есть строки или detached-части) → `CREATE … funding_edges_003` → `throwIf` (остаток прерванного прогона не пуст) → `RENAME funding_edges TO funding_edges_pre003` → `RENAME funding_edges_003 TO funding_edges` → повторный `throwIf` по старой таблице → `DROP … SYNC`;
   - строка `-- apply-unless:` пропускает файл, если ключ уже новый, так что повторный запуск не трогает мигрированную таблицу, даже заполненную;
   - если прогон оборвётся между двумя `RENAME`, доделать вручную. Способ описан в комментарии файла.
2. **`sql/apply.sh`** — применяет `sql/NNN_*.sql` по порядку к работающему ClickHouse по HTTP:
   - адрес `127.0.0.1:${CLICKHOUSE_HTTP_PORT:-18123}`. `.env` разбирается, но не исполняется; окружение важнее `.env`. Пароль передаётся в curl-конфиге через pipe, в argv его нет;
   - журнал — `hood.schema_migrations (version, name, sha256, outcome applied|skipped, applied_at)`;
   - уже записанная миграция повторно не запускается. Если sha256 файла не совпадает с записанным, скрипт останавливается;
   - опции `--dry-run` и `--to N`; в конце печатается `changes: N`;
   - стейтменты разбирает awk-лексер (`;` вне кавычек и комментариев);
   - shellcheck (docker, `--network none`): 0 замечаний, образ удалён.

   В этом раунде скрипт не менялся.
3. **`sql/002_funding_edges_l1.sql`**: исправлен только комментарий о способе применения.
4. **`docker-compose.yml`**:
   - образ `clickhouse/clickhouse-server:26.9.6.6` вместо `latest`;
   - порты прежние, `127.0.0.1:18123/19100`;
   - монтирование `./sql:/docker-entrypoint-initdb.d` убрано: оно срабатывает только на пустом томе, ничего не записывает и запускало бы `apply.sh` внутри контейнера.
5. **Тест** `rows::tests::funding_edges_column_order_in_sql_equals_columns` в `crates/decoders/src/rows.rs`:
   - берёт последний `CREATE TABLE hood.funding_edges*` из `sql/*.sql` и сравнивает имена колонок с `FundingEdge::COLUMNS`;
   - падает, если после этого `CREATE` есть `ALTER TABLE hood.funding_edges`;
   - лежит рядом с тестом Enum8 (`enum8_in_sql_equals_rust_enums`), а не в `tests/rows.rs`, как просил координатор: тест Enum8 и разборщик `sql_lines()` находятся в модуле `src/rows.rs`, а не в `tests/rows.rs`, и новый тест их переиспользует (~15 строк).
6. **`data-model.md`**:
   - раздел «Схема и миграции»: apply.sh, журнал, идемпотентность, `apply-unless`, запрет молча удалять данные, **запрет `EXCHANGE TABLES`**, закреплённая версия, ключ 003, порядок колонок и тест на него;
   - фраза «Ограничение ключа…» заменена описанием нового ключа.

## Исправление Б1 (ревью architect-reviewer 023, 2026-10-02)

**Б1. Ошибка запроса `apply-unless` превращалась в `skipped`. Исправлено.** Раньше запрос guard стоял внутри `[ "$(…)" != "0" ]`, где `set -e` не срабатывает. Если запрос падал, миграция записывалась в журнал как `skipped`, а скрипт выходил с кодом 0. Теперь:
- запрос guard выполняется отдельным присваиванием: `g=$(… | ch 2>&1) || die "… apply-unless query failed: $g"`;
- ответ проверяется регуляркой `^[0-9]+$`, иначе `die`;
- только после этого `0` → `applied`, всё остальное → `skipped`;
- при `die` в журнал ничего не пишется, тело миграции не выполняется.

**Тест `sql/test_apply.sh`** (новый, офлайн):
- поддельный `curl` в `PATH` изображает ClickHouse; `apply.sh` копируется во временный каталог вместе с синтетическими миграциями, настоящие `sql/NNN` не используются;
- 13 проверок:
  - guard с ошибкой (HTTP 500, curl 22): код ≠ 0, журнал пуст, сообщение `apply-unless query failed`, тело не выполнено;
  - нечисловой ответ guard: код ≠ 0, журнал пуст;
  - guard = 1: `skipped`, тело не выполнено;
  - guard = 0: `applied`;
  - лексер: `;` в строке и в комментариях `--` и `/* */` не делит стейтмент, комментарии на сервер не уходят.
- Результат: `bash sql/test_apply.sh` → `ok: 13 checks`.
- Отрицательная проверка: тест на копии `apply.sh` со старым условием падает с `FAIL: failing guard: exit 0`.

**Рекомендации:**
- **Р1.** Guard в 003 расширен: файл пропускается, только если у `funding_edges` новый ключ **и** нет остатков `funding_edges_003` / `funding_edges_pre003`. При остатке guard возвращает 0, файл выполняется и громко падает.
  - Проверено на временном bind-контейнере: создана `funding_edges_pre003` с 1 строкой, запись 3 удалена из журнала. `apply.sh` упал (`Code: 57 … Table hood… already exists` на `RENAME`), записи 3 в журнале нет, строка в `pre003` цела.
  - sha256 003 изменился: `5ae7af72…` → `7ef02d0f…`.
- **Р2.** Добавлен `--max-time 2` к ожиданию `/ping`. У `ch()` таймаута нет: DDL с `SYNC` может идти долго.
- **Р3.** В шапке `apply.sh` написано: два прогона параллельно не запускать, блокировки нет.
- **Р4.** `--help` печатает начальный блок комментариев (awk до первой строки без `#`) вместо жёсткого диапазона строк.
- **Р5.** В шапке описаны ограничения разбора `.env`:
  - только `KEY=value`, побеждает последнее вхождение;
  - снимаются CRLF и одна пара кавычек;
  - нет `export`, нет комментариев в строке;
  - нет переводов строки в значении.
- **Р6.** `rows.rs`: «then EXCHANGE» → «then RENAME». `data-model.md`: путь `l1_inflows.rs` → `l1_inflows/`. `sql/002` не трогал: файл в журнале, неизменяем.
- **Р7.** Пин тегом оставлен, как и советует ревью.
- В `data-model.md` добавлено: ошибка guard останавливает прогон; офлайн-тест `sql/test_apply.sh`; параллельно не запускать.

**Проверено после исправления (2026-10-02):**
- **Guard с опечаткой на настоящем сервере.** Временный bind-контейнер `hood023-b1-bind` (`127.0.0.1:28223`), копия `sql/` с `sorting_keyy` в guard 003. `apply.sh` применил 001 и 002, затем остановился: `apply-unless query failed … Code: 47 … Unknown expression or function identifier sorting_keyy`. В журнале только 1 и 2, ключ старый.
- **Тот же контейнер, настоящий 003.**
  - Прогон 1: `already applied: 2`, `changes: 1`. Прогон 2: `changes: 0`.
  - После `docker restart`: `changes: 0`, ключ новый.
  - Потом проверка остатка, описанная в Р1.
  - Контейнер и каталог удалены. Чужие контейнеры (data-auditor) не трогал.
- **Локальный том.** До правок `funding_edges` пуста (count 0), остатков нет. Запись 3 удалена из журнала, затем:
  - прогон 1: `skipped: 003_funding_edges_key.sql`, `changes: 1`. Это правильно: ключ уже новый, остатков нет;
  - прогон 2: `changes: 0`;
  - после `docker restart`: `changes: 0`, `funding_edges.sql` на диске, ключ новый, DESCRIBE тот же, что выше;
  - журнал: 1 `applied`, 2 `applied`, 3 `skipped` (sha `7ef02d0f…` = файл).
- **Линт и тесты:**
  - shellcheck (`docker run --rm --network none koalaman/shellcheck:stable -x apply.sh test_apply.sh`): exit 0, 0 замечаний, образ удалён;
  - `bash -n` обоих скриптов: ok;
  - `cargo fmt -p decoders --check`, `cargo clippy -p decoders --all-targets -- -D warnings`, `cargo test -p decoders`: ok.

## Найдено: `EXCHANGE TABLES` теряет метаданные на bind-монтировании macOS (проверено 2026-10-02)

- Как обнаружено. По запросу координатора нужно было сбросить локальную `funding_edges`. `DROP TABLE hood.funding_edges` упал с `filesystem error: in rename: No such file or directory [.../store/8bb/…/funding_edges.sql]`. В каталоге метаданных базы `hood` не было файла `funding_edges.sql`, хотя таблица была видна в `system.tables`.
- Почему. `data/clickhouse` смонтирован в контейнер как `fakeowner`: это файловый шаринг Docker Desktop, `mount` в контейнере показывает `/run/host_mark/Users on /var/lib/clickhouse type fakeowner`. На такой ФС `EXCHANGE TABLES` (атомарный обмен файлов метаданных) отработал без ошибки, но один файл метаданных пропал. Таблица жила только в памяти сервера. После `docker restart` она исчезла (проверено на локальном томе).
- Воспроизведение. Временный контейнер с bind-монтированием каталога в scratchpad (тоже `fakeowner`) и прежний 003 дали тот же результат: после `apply.sh` нет `funding_edges.sql`. Обычные `RENAME TABLE a TO a_old; RENAME TABLE b TO a; DROP a_old` на том же монтировании сохраняют файл, и таблица переживает рестарт.
- Почему не поймал первый раунд. В первом раунде свежий том проверялся на docker named volume (ext4 внутри VM), там `EXCHANGE` работает. Теперь проверка на bind-монтировании с рестартом контейнера записана в data-model.md как обязательная.
- Последствия. Данных не было: таблица пуста, нагрузки нет. От старой таблицы остался каталог `data/clickhouse/store/fb1/fb1c8399-…/`: только `format_version.txt`, частей нет, метаданных нет. Вручную не удалял. Предполагается, что ClickHouse сам уберёт неиспользуемый каталог `store` (фоновая очистка `DatabaseCatalog`), но это не проверено.

## Проверено (2026-10-02, локально; без RPC, фида и сервера)

- **Версия.** Остановленный контейнер запущен, `SELECT version()` = `26.9.6.6`. `docker pull clickhouse/clickhouse-server:26.9.6.6` дал тот же id образа (`sha256:eb4870e7ca7e…`), что у `latest`. Контейнер пересоздан `docker compose up -d clickhouse`: образ `26.9.6.6`, порты `127.0.0.1:18123/19100`, bind-монтирование `data/clickhouse`.
- **Локальный том, сброс** (только локально, таблица пуста: count = 0, detached-частей 0):
  - `TRUNCATE hood.schema_migrations`;
  - `docker restart`: `funding_edges` исчезла сама (метаданных не было).
- **Локальный том, `sql/apply.sh`:**
  - прогон 1: `applied: 001, 002, 003`, `changes: 3`;
  - прогон 2: `already applied: 3`, `changes: 0`, exit 0;
  - `docker restart`: `funding_edges.sql` на диске, ключ новый; ещё один прогон — `changes: 0`;
  - sha256 файла 003 тогда был `5ae7af72…` и совпадал с журналом. После исправления Б1 файл изменился, см. ниже.
- **Свежий том, bind-монтирование.** Временный контейнер `hood023-bind-test`: каталог в scratchpad, порт `127.0.0.1:28123`, одноразовый пароль. Последовательность:
  - `--to 2`, затем вставка строки в старую `funding_edges`, затем `apply.sh`: падение на первом `throwIf` (`Code: 395 … refusing to recreate it`). Строка на месте, ключ старый, записи 003 в журнале нет;
  - `TRUNCATE`, затем `apply.sh`: `applied: 003`. Повторный прогон: `changes: 0`;
  - `docker restart`: `funding_edges.sql` на месте, ключ новый, `apply.sh` даёт `changes: 0`;
  - 5 рёбер одной tx одному получателю (`l1_token` log 4 и 5, `weth` log 3, `eth` и `l1_eth` уровня tx) после `OPTIMIZE FINAL` остаются все 5, у двух `log_index = 4294967295`;
  - запись 3 удалена из журнала, `apply.sh`: `skipped: 003`, 5 строк целы.
- **Свежий том, named volume.** `hood023-fresh-test`: `changes: 3`, затем `changes: 0`, рестарт, ключ новый, `changes: 0`.

  Проверки первого раунда тоже на named volume: `--dry-run`; отказ на непустой таблице; остановка при изменённом файле (sha256).
- **Уборка.** Все временные контейнеры, том и каталог scratchpad удалены. Проверено `docker ps -a` / `docker volume ls`, следов `hood023*` нет.
- **Rust:** `cargo fmt -p decoders --check` — ok; `cargo clippy -p decoders --all-targets -- -D warnings` — ok; `cargo test -p decoders` — все проходят, в lib 18 тестов, включая новый. Отрицательная проверка: при `log_index` перед `tx_hash` в 003 тест падает с понятным diff. После этого файл восстановлен, sha тот же.

### DESCRIBE hood.funding_edges после миграции (локальный том)

```
block_number    UInt64
tx_index        UInt32
from_addr       String
to_addr         String
value_wei       String
kind            Enum8('eth' = 1, 'weth' = 2, 'internal' = 3, 'l1_eth' = 4, 'l1_token' = 5)
tx_hash         String  DEFAULT ''
log_index       UInt32  DEFAULT 4294967295
token           String  DEFAULT ''
l1_token        String  DEFAULT ''
gateway         String  DEFAULT ''
gateway_status  Enum8('none' = 0, 'observed' = 1, 'verified' = 2)  DEFAULT 'none'
l2_alias        String  DEFAULT ''
tx_type         UInt8   DEFAULT 0
l1_request_id   String  DEFAULT ''
ticket_id       String  DEFAULT ''
ENGINE = ReplacingMergeTree ORDER BY (to_addr, block_number, tx_index, kind, log_index)
```

Свежий том (bind) дал тот же DESCRIBE. Остальные таблицы `hood.*` — как в 001, все пустые. Плюс `schema_migrations`: 3 строки, у всех `applied`.

## Не сделано / ограничения

- Р7–Р9 ревью вне задачи. Тип сумм не менял (решение Михаила).
- `cargo build/test --workspace` не запускал: проверен только крейт `decoders`, по указанию координатора.
- В `CLAUDE.md` (раздел «Команды») устарела фраза «схема из sql/ применяется при первом старте». Нужно: `docker compose up -d clickhouse && sql/apply.sh`. Файл вне зоны задачи.
- Образ `clickhouse/clickhouse-server:latest` остался локально: тот же id, его не создавал и не удалял.

## Предполагается / не проверено

- `apply.sh` и `test_apply.sh` проверены только на macOS: bash 3.2, BSD awk, curl 8.7.1. Linux не проверялся.
- Будущий сервер с ClickHouse на обычной ФС (ext4/xfs): `EXCHANGE` там, вероятно, работал бы, но 003 его уже не использует.
- Пароли с `"` или `\` экранируются для curl-конфига, но с такими паролями не тестировалось.
- Пустой каталог от старой таблицы (`store/fb1/…`) ClickHouse уберёт сам. Не проверено.

## Вопросы к Cowork/Михаилу

- Добавить в `CLAUDE.md` команду `sql/apply.sh` после `docker compose up`?
- Записать в `docs/decisions/` правило миграций: apply.sh, журнал, `apply-unless`, без `EXCHANGE TABLES`, проверка на bind-монтировании с рестартом?
