# 029 — Схема ClickHouse: хвосты ревью. Отчёт

- Исполнитель: indexer-engineer. Дата: 2026-10-02/03. Все проверки шли 2026-10-02 около 21:00 UTC, по местному времени это уже 2026-10-03. Поэтому в `data-model.md` и в шапке 004 стоит дата 2026-10-03.
- Сеть: RPC 0, фид 0, сервер/ssh 0. Из сети только `docker pull koalaman/shellcheck:stable`; контейнер шёл с `--network none`, образ потом удалён.
- Не коммитил. Статус задачи не трогал. Ревьюеров не запускал. `cargo clean` не запускал.
- **Нужен прогон data-auditor и architect-reviewer** (по задаче): изменены схема (миграция 004) и загрузчик схемы `sql/apply.sh`.

## Сделано

1. **`sql/004_types_keys.sql`** (новый файл). Пересоздаёт `txs`, `logs`, `swaps`, `tokens`, `wallets`, `labels`, `feed_gaps` так же, как 003. По каждой таблице по порядку:
   - `throwIf`, если в таблице есть строки или detached-части;
   - `CREATE <t>_004`;
   - `throwIf`, если в оставшейся `<t>_004` есть строки;
   - `RENAME <t> → <t>_pre004`, затем `RENAME <t>_004 → <t>`;
   - повторный `throwIf` на `<t>_pre004`;
   - `DROP … SYNC`.

   Без `EXCHANGE`, `CREATE OR REPLACE` и `REPLACE TABLE`. Есть guard `apply-unless`: по маркеру на каждую из 7 таблиц и проверка, что не осталось `*_004`/`*_pre004`. Что изменено:
   - `Nullable` убран, вместо него `DEFAULT ''`/`0`: `txs.to_addr`, `logs.topic1..3`, `swaps.router`, `wallets.first_funder`/`first_funding_block`/`first_funding_wei`;
   - `txs.selector`: `FixedString(10)` → `LowCardinality(String) DEFAULT ''`;
   - `tokens.venue`: `LowCardinality(String)` → тот же `Enum8`, что у `swaps.venue` (одно понятие — один тип);
   - `labels`: `ORDER BY (address, label, source)`;
   - `feed_gaps`: `ReplacingMergeTree(filled)`.

   Оставлено намеренно:
   - `blocks.feed_recv_ns Nullable`: NULL здесь смысловой, это прописано в data-model;
   - суммы — String: UInt256 — решение Михаила;
   - `funding_edges` не тронута, поэтому `crates/decoders/src/rows.rs` менять не понадобилось.
2. **`sql/test_apply.sh`.**
   - Поддельный `curl` складывает служебные запросы apply.sh (`CREATE DATABASE`, `schema_migrations`) в отдельный файл. Число стейтментов теперь проверяется строго (`-eq 2`, раньше было `-ge 4`).
   - Новый тест лексера `split_sql` сравнивает отправленные стейтменты с ожидаемым текстом целиком. В нём: `;`/`--`/`/*` внутри `'…'`, `"…"` и `` `…` ``, `\'` и `''`, многострочный блочный комментарий, пустые стейтменты, кусок из одного комментария, последний стейтмент без `;`.
   - Незакрытая кавычка: прогон останавливается до отправки чего-либо, в журнал ничего не пишется.
   - Запрещённые DDL (3 варианта, включая разбитый на строки `create or\n replace`) отклоняются. Ложного срабатывания на колонки `exchange`/`replace` нет.
   - Итог: 23 проверки (было 13).
3. **`sql/apply.sh`.**
   - До отправки первого стейтмента файла проверяет, не начинается ли какой-либо стейтмент с `EXCHANGE`, `CREATE OR REPLACE` или `REPLACE TABLE`. Если да — `die`. Без учёта регистра, пробелы схлопываются; проверяется только начало стейтмента.
   - В шапке добавлена одна строка правила. Остальное не менял.
4. **`data-model.md`, раздел «Схема и миграции».**
   - Запрет `CREATE OR REPLACE`/`REPLACE TABLE` рядом с запретом EXCHANGE, с результатом проверки.
   - Абзац об остатке пустой `<t>_NNN` после неудачного прогона: `funding_edges_003` у 003 и `<t>_004` у 004.
   - Абзац о сиротах в `store` и автоочистке ClickHouse.
   - 004 добавлена в список миграций, плюс блок «Типы и ключи (004)».
   - В «Соглашениях» про `labels` дописано, что `source` входит в ключ.
5. **Комментарий в 003 не правил (отступление от п. 3 задачи).** 003 уже в журнале локального тома (sha256 `7ef02d0f…`), а применённые миграции неизменяемы: правка комментария меняет sha256, и `apply.sh` остановится («changed after it was applied»). Поэтому описание остатка `funding_edges_003` есть в `data-model.md` и в шапке 004. Если нужен именно комментарий в 003, придётся вручную поправить sha в журнале локального тома (так делали в 023) — решать Михаилу/Cowork.
6. **Локально:** 004 применена к тому `data/clickhouse`. Удалён осиротевший каталог `store/fb1/fb1c8399-…` (см. ниже).

## Проверено (2026-10-02 ~21:00 UTC, как)

**Временный контейнер №1** `hood029-bind-a477d6fc`:
- образ `26.9.6.6`; bind-монтирование каталога scratchpad, в контейнере тип ФС `fakeowner`, как у рабочего тома;
- опубликован только `127.0.0.1:28291`; одноразовый пароль в файле 0600; `ENV_FILE=/dev/null`.

| проверка | результат |
|---|---|
| `CREATE OR REPLACE TABLE xt.a` (в ней 1 строка) | `Code: 1001 filesystem error: in rename … _tmp_replace_….sql → metadata_dropped/…`, хотя замена уже сделана: колонки `x, y`, 0 строк |
| `REPLACE TABLE xt.b` | та же ошибка, замена тоже сделана |
| `system.tables` до рестарта | «призраки» `_tmp_replace_*` с путями к несуществующим `.sql` |
| после `docker restart` | `a`, `b` с новой схемой; 2 каталога в `store` без таблицы, в одном часть `all_1_1_0` |
| `DROP TABLE xt.c SYNC` (1 строка) | каталог удалён полностью, сирот нет |
| `apply.sh` на пустом томе | `applied` 001–004, `changes: 4` |
| повтор | `changes: 0` |
| `docker restart` + повтор | `changes: 0`; все 10 `.sql` в `metadata/hood`; новых сирот в `store` нет |
| `--to 3`, 1 строка в `labels`, затем `apply.sh` | exit 1, `Code: 395 … 004: hood.labels is not empty`. `txs`…`wallets` уже пересозданы, `labels` со старым ключом и строкой цела, записи 4 в журнале нет |
| `TRUNCATE labels`, повтор (частичный прогон) | `applied: 004`, `changes: 1`; ключ `labels` новый, остатков `*004` нет |
| `feed_gaps`: вставка `filled 1`, затем `filled 0`, `OPTIMIZE FINAL` | осталась одна строка, `filled = 1` |
| `labels`: одна метка `bot` от `h1` и от `h2`, `OPTIMIZE FINAL` | 2 строки |
| `txs` без `to_addr`/`selector` | `''`, `''`, тип `LowCardinality(String)` |
| `tokens.venue = 'bogus'` | `Code: 691 Unknown element 'bogus' for enum` |
| guard при остатке `wallets_pre004` + непустой `txs` + без записи 4 в журнале | guard 0, файл запущен, exit 1 на `txs is not empty`; строка цела, записи 4 нет |
| то же без остатка | guard 1, `skipped`, строка цела |

**Временный контейнер №2** `hood029-bind2-a477d6fc`, `127.0.0.1:28292`, с итоговой версией `apply.sh`, где уже есть запрет DDL:
- `changes: 4`, потом `changes: 0`;
- рестарт, `changes: 0`;
- DESCRIBE 7 таблиц совпал с локальным томом (`diff` пуст).

Уборка: оба контейнера (`docker rm -f`), каталоги данных и пароли удалены; `docker ps -a | grep hood029` → 0. Анонимных томов нет: `VOLUME` перекрыт bind-монтированием.

**Локальный том** (`atomic-arbitrage-clickhouse-1`, `127.0.0.1:18123/19100`; пароль читался из `.env` и не печатался):
- контейнер был остановлен (Exited 137 примерно с 20:00 UTC — не мной); запустил `docker start`;
- до применения: `version()` = 26.9.6.6, все `hood.*` по 0 строк, detached-частей 0, журнал 001/002 `applied`, 003 `skipped`;
- `apply.sh`: `applied: 004_types_keys.sql`, `changes: 1`; второй прогон `changes: 0`;
- остановка контейнера → удаление сироты → `docker start` → `apply.sh` `changes: 0`, `--dry-run` `pending: 0`;
- журнал: 4 строки, sha256 равны файлам (`c8624f53`, `c969fff5`, `7ef02d0f`, `e51ef320`);
- 10 `.sql` в `metadata/hood`, сирот в `store` нет, ошибок `hood` в `text_log` за час нет.

**Сирота `store/fb1/fb1c8399-3a4d-4778-ac7b-0d0f537aa3d8`:**
- это UUID `funding_edges_003` из прогона 023. Лог 17:51:10 UTC: та же `Code: 1001 … rename … metadata_dropped/hood.funding_edges.fb1c8399….sql`;
- в 19:04:49 UTC ClickHouse сам пометил его как неиспользуемый (`DatabaseCatalog: Removing access rights for unused directory … (will remove it when timeout exceed)`) и снял права: режим `drw-------`, поэтому нет доступа ни с хоста, ни из контейнера;
- сам он удалил бы каталог через `database_catalog_unused_dir_rm_timeout_sec` = 2 592 000 с (30 суток);
- UUID не было ни в `system.tables`, ни в `system.databases`;
- при остановленном сервере вернул права, внутри только `format_version.txt` (`1`) и пустой `detached/`. Удалил каталог и пустой префикс `store/fb1`.

**Скрипты и Rust:**
- `bash sql/test_apply.sh` → `ok: 23 checks`.
- Отрицательные проверки на копиях `apply.sh` в scratchpad. Каждая порча ловится:
  - без обработки `` ` `` → `FAIL: lexer`;
  - без обработки `\` → `FAIL: lexer`;
  - без ошибки «unterminated» → `FAIL: unterminated`;
  - без проверки запрета → `FAIL: banned`.
- `bash -n` обоих скриптов — ok. `shellcheck -x apply.sh test_apply.sh` — 0 замечаний.
- `cargo build --workspace` — ok. `cargo test --workspace` — 204 passed, 0 failed, в том числе `rows::tests::funding_edges_column_order_in_sql_equals_columns`, `enum8_in_sql_equals_rust_enums` и `every_column_is_defined_in_sql`: они читают `sql/` вместе с 004. `cargo clippy --workspace --all-targets -- -D warnings` — чисто. Важно: в это же время задача 027 правит `crates/decoders`, прогон шёл по текущему состоянию дерева.

## DESCRIBE изменённых таблиц (локальный том после рестарта; совпадает со свежим томом)

```
== txs
block_number   UInt64
tx_index       UInt32
tx_hash        String
from_addr      String
to_addr        String                  DEFAULT ''
value_wei      String
selector       LowCardinality(String)  DEFAULT ''
input_len      UInt32
status         UInt8
gas_used       UInt64
gas_used_l1    UInt64
eff_gas_price  UInt64
fee_wei        UInt128
== logs
block_number   UInt64
tx_index       UInt32
log_index      UInt32
address        String
topic0         String
topic1         String  DEFAULT ''
topic2         String  DEFAULT ''
topic3         String  DEFAULT ''
data           String
== swaps
block_number      UInt64
tx_index          UInt32
log_index         UInt32
venue             Enum8('pons_v1' = 1, 'pons_v2_curve' = 2, 'uni_v3' = 3, 'uni_v4' = 4, 'pools_trade' = 5, 'other' = 9)
pool              String
token             String
quote             String
trader            String
router            String  DEFAULT ''
side              Enum8('buy' = 1, 'sell' = 2)
token_amount_raw  String
quote_amount_raw  String
quote_amount      Float64
price             Float64
fee_quote         Float64
== tokens
token          String
venue          Enum8('pons_v1' = 1, 'pons_v2_curve' = 2, 'uni_v3' = 3, 'uni_v4' = 4, 'pools_trade' = 5, 'other' = 9)
creator        String
created_block  UInt64
created_tx     String
pool           String
quote          String
fee_params     String
== wallets
address              String
first_seen_block     UInt64
first_funder         String  DEFAULT ''
first_funding_block  UInt64  DEFAULT 0
first_funding_wei    String  DEFAULT ''
== labels
address     String
label       LowCardinality(String)
source      LowCardinality(String)
confidence  Float32
note        String
updated_at  DateTime  DEFAULT now()
== feed_gaps
from_seq     UInt64
to_seq       UInt64
detected_ns  UInt64
filled       UInt8  DEFAULT 0
```

Движки и ключи (`system.tables`):
- `labels` — `ReplacingMergeTree(updated_at) ORDER BY (address, label, source)`;
- `feed_gaps` — `ReplacingMergeTree(filled) ORDER BY from_seq`;
- у остальных таблиц движки и ключи прежние. `funding_edges` и `blocks` не менялись.

## Предполагается / не проверено

- Ошибки `EXCHANGE`, `CREATE OR REPLACE` и `REPLACE TABLE` вызваны именно ФС `fakeowner` Docker Desktop: rename `.sql` в `metadata_dropped` падает с ENOENT. На ext4/xfs сервера, вероятно, всё работает, но запрет общий: проверок на Linux не было.
- `apply.sh`/`test_apply.sh` проверены только на macOS (bash 3.2, BSD awk/tr). На Linux (GNU) не запускались.
- `venue` как `Enum8` подразумевает, что площадки добавляются редко и через миграцию. Если их станет много, разумнее `LowCardinality(String)` в обеих таблицах. Это выбор схемы, а не проверенный факт.
- `wallets.first_funding_block = 0` как «не найдено» исходит из того, что в блоке 0 финансирования не бывает: генезис — не транзакции. На цепи не проверялось.
- Нагрузочного поведения нет: таблицы пустые, проверки шли на единицах строк.

## Вопросы к Cowork/Михаилу

1. Комментарий в 003: оставить описание в `data-model.md` и 004 (как сейчас) или править 003 с ручной правкой sha256 в журнале локального тома?
2. `CLAUDE.md`, раздел «Команды», по-прежнему говорит «схема из sql/ применяется при первом старте». Нужно `docker compose up -d clickhouse && sql/apply.sh` (замечание З4 аудита 023). Файл вне моей зоны.

## Правки после ревью

По ревью `docs/reviews/029-sql-followups-architect-reviewer.md` (PASS с замечаниями) внесены только Р2, Р3 и Р5. `sql/003` и `sql/004` не трогал: они уже в журнале. Р4 и Р6 — для будущих задач (тест `Venue` по обеим таблицам и `uniqExact(selector)` после первой загрузки), здесь ничего не менял.

- **Р2.** Запрет DDL вынесен в функцию `check_banned`. Разбор (`split_sql`) и проверка теперь идут для каждого ожидающего файла сразу после проверки журнала: до ветки `--dry-run` и до `apply-unless`. Поэтому запрещённый стейтмент ловится и при `--dry-run`, и в файле, который на этом томе был бы `skipped`. В ветке `applied` остался только цикл отправки. Для корректных файлов поведение не изменилось. Добавлены два теста: запрет + `--dry-run` → exit 1, журнал пуст; запрет + guard 1 → exit 1, журнал пуст. Чтобы передать аргументы apply.sh, у `run` появилась переменная `RUN_ARGS`.
- **Р3.** Во входе лексер-теста строка `INSERT INTO t VALUES (1),-- tail; comment` теперь без пробела перед комментарием, ожидаемая строка `INSERT INTO t VALUES (1),` — без хвостового пробела. `git diff --check -- sql/` чистый.
- **Р5.** В шапке `apply.sh` уточнено, что `CREATE OR REPLACE` запрещён для любого объекта (TABLE, VIEW, DICTIONARY…). Ещё добавлено, что проверка действует и при `--dry-run`, и для файлов, которые `apply-unless` пропустил бы. Регэксп не менял.

Проверено 2026-10-02 (UTC), Mac:
- `bash sql/test_apply.sh` → `ok: 27 checks`.
- Отрицательные проверки на копии `apply.sh` в scratchpad, где `check_banned` возвращён внутрь ветки `applied` (старое поведение):
  - `FAIL: banned + dry-run`;
  - если убрать тест с dry-run — `FAIL: banned + guard 1`.

  То есть каждый из двух новых тестов ловит регресс.
- `bash -n` обоих скриптов — ok.
- `shellcheck -x apply.sh test_apply.sh` (docker `--network none`): exit 0, 0 замечаний. Первая версия дала SC2086 на `${RUN_ARGS:-}`, исправлено на `${RUN_ARGS:+"$RUN_ARGS"}`. Образ удалён.
- ClickHouse не запускал. Контейнеры data-auditor не трогал. Журнал локального тома не менялся: правились только скрипты, файлы миграций те же.
