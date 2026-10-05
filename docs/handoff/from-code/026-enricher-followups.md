# 026 — enricher: хвосты ревью (отчёт)

- Исполнитель: indexer-engineer. Дата: 2026-10-03.
- База: HEAD `83cadb4`. Задача 025 закоммичена, в ней появился общий джиттер `hood_core::jitter::rand01`.
- Объём: `crates/enricher`, `.claude/skills/hoodchain-mev/references/data-model.md` (запись о контрольной сумме; фразу о кодах выхода добавил координатор), `deploy/README.md` (только текст про блоки за прогон при `--max-calls 4000`), `crates/hood-core/src/jitter.rs` (одна строка doc, правил координатор).
- Не коммитил, статус задачи не менял, ревьюеров не запускал. Реальный RPC, фид и ssh не трогал: 0 обращений. Моки работали только на 127.0.0.1.

## Сделано

### 1. Контрольная сумма zstd в `blocks-*` и `logs-*`
- `atomic.rs`: `AtomicZstdFile::create` включает `enc.include_checksum(true)`. Через этот тип пишутся оба вида файлов, так что `blocks.rs` и `logs.rs` менять не пришлось. Содержимое строк и сетка файлов не менялись.
- Чтение старых файлов (без checksum) не затронуто. Флаг стоит в заголовке каждого фрейма, и декодер проверяет сумму только там, где она есть. Сам enricher свои файлы не читает. Их читают `zstd -dc` и `analytics/hoodlib.py`.
- Тесты:
  - `atomic::corrupted_byte_is_detected_by_the_checksum`. Пишутся 64 КиБ несжимаемых данных: zstd кладёт их raw-блоком, поэтому порча байта меняет именно содержимое, а не ломает структуру. В файле проверяется флаг checksum. После инверсии одного байта в середине чтение падает с ошибкой `checksum`. Контроль: та же порча в файле без checksum (так писали до 026) распаковывается без ошибки в неверные байты;
  - `atomic::old_files_without_checksum_still_decode`: файл без checksum читается, как раньше;
  - `tests/range.rs::output_files_carry_the_zstd_checksum`: на моке у `blocks-*` и `logs-*` есть флаг checksum, содержимое читается (50 строк, у логов 51), а испорченный байт сжатых данных даёт ошибку чтения.

### 2. Режим диапазона: бюджет и `--dry-run`
- **Кусок больше бюджета теперь ошибка конфигурации (выход 1).** Это та же проверка `check_plan_fits_budget`, что и в `--gaps`. В `--mode blocks` она стоит до взятия lock и до любого вызова. До 026 каждый прогон тратил вызов на `eth_chainId` и выходил с 75, без прогресса. Текст ошибки тот же, что в `--gaps`, с подсказкой `lower --chunk to <= N or raise --max-calls to >= M`. В `--mode logs` число вызовов зависит от данных (окно адаптивное), поэтому план не проверяется. Бюджет там, как и раньше, ограничивается только во время работы.
- **`--dry-run` в режиме диапазона: выбран вариант «только план».** Раньше флаг в диапазоне молча игнорировался, и данные скачивались (замечание З5 data-auditor к 020). Теперь:
  - пишется план (`range plan`: `from`, `to`, `blocks`, `files`, в blocks ещё `planned_calls` = 2 × блоки + 1; затем строка `to fill` на каждый файл);
  - выполняются проверки плана: `--max-blocks`, бюджет (в blocks), разбор `--topic0` (в logs);
  - выход 0 без единого вызова RPC, даже без `eth_chainId`;
  - на диске ничего не трогается: каталог не создаётся, lock не берётся, `*.partial` не чистятся. План диапазона от содержимого диска не зависит. В `--gaps` dry-run, как и раньше, берёт lock, потому что план вычитает `filled.tsv`.
  - Почему не «отвергать»: так поведение одно и то же для `--gaps` и диапазона, а «сколько вызовов и файлов будет» полезно проверить перед ручным прогоном.
- План `range plan` теперь пишется в журнал и при обычном прогоне в режиме диапазона. Раньше в этом режиме плана в журнале не было. Проверка `--max-blocks` по-прежнему идёт до разбиения на куски: огромный диапазон с маленьким `--chunk` отвергается до того, как строится и пишется список файлов.
- Где задокументировано: doc модуля `lib.rs` (разделы «Plan checks» и `--dry-run`), `--help` у `--dry-run` («Print the plan, check it and exit without RPC calls (range and --gaps)») и у `--max-calls` («Blocks mode (range or `--gaps`): less than the largest file + 1 is an error (exit 1)»). Флаги CLI не менялись, изменились только тексты справки (diff `--help` ниже).
- Тесты: `tests/range.rs::range_dry_run_makes_no_call_and_touches_nothing` (blocks и logs; ошибки плана видны и в dry-run), `tests/range.rs::range_budget_below_one_file_is_a_config_error` (Err, но не «budget exhausted»; 0 вызовов, каталог не создан; с `--chunk 5` и `--max-calls 21` ровно 21 вызов и 2 файла), `tests/exit_codes.rs::range_budget_below_one_file_is_a_config_error` (бинарник: код 1, 0 запросов).

### 3. Джиттер из hood-core
- В `rpc.rs` удалён свой генератор `jitter()` (xorshift64 со статическим состоянием). Пауза повтора берёт `hood_core::jitter::rand01()`. Неиспользуемый импорт `UNIX_EPOCH` убран. `next_step` и `backoff_delay` не менялись: джиттер по-прежнему передаётся параметром `u`, поэтому юнит-тесты пауз остаются детерминированными.

### 4. Тесты убирают временные каталоги
- `tests/common/mod.rs`: `scratch()` возвращает guard `Scratch` (`Deref<Target = Path>`), который удаляет каталог в `Drop`. Если тест упал, каталог остаётся для разбора, а путь печатается. Места вызова менять не пришлось: везде используется `d.join(…)` / `d.to_str()`.
- Guard один: `src/testdir.rs` (`TestDir`). Юнит-тесты (`atomic.rs`, `ranges.rs`) подключают его как приватный `#[cfg(test)] mod testdir`, а `tests/common/mod.rs` — через `#[path = "../../src/testdir.rs"]` (`pub use testdir::TestDir as Scratch`). Дубля нет (см. «Правки после ревью», Р2).

### 5. `deploy/README.md`
- Строка про бюджет: «**Это 1 000 блоков за прогон при 4000**». Почему: 1 вызов уходит на `eth_chainId`, а второй файл (2 000 вызовов) на оставшиеся 1 999 не начинается. Фактически прогон тратит ~2 001 вызов (~48 000 в сутки), потолок — 96 000. Значение 4001 / 4201 выбирает Михаил, до решения остаётся 4000.
- В том же абзаце поправлен пример WARN. Теперь там `blocks <from>..=<to>: … 2001 calls sent, 2000 more …` и пояснение, что с 020 «M more» — это вызовы на весь следующий файл (замечание З1 data-auditor к 020). Если это выходит за рамки «только текст про блоки за прогон», правку можно откатить отдельно.
- `deploy/enricher-gaps.service` не трогал: ни `--max-calls 4000`, ни комментарий в шапке. Комментарий «4000 calls = 2000 blocks per run» в юните остаётся неверным до решения Михаила.

### data-model.md
- В п. 2 («Сырые блоки») добавлена запись о контрольной сумме (с 026, 2026-10-03). Касается `blocks-*` и `logs-*`: в `zstd -lv` видно `Check: XXH64`, старые файлы — `Check: None`, они читаются как прежде, но порча в них может пройти незамеченной. Проверка целостности — `zstd -t`.

## Проверено (2026-10-03, как)

Всё локально на Mac, без сети.

- **Линт и тесты:**
  - `cargo fmt --all -- --check` → rc 0;
  - `cargo clippy --workspace --all-targets -- -D warnings` → rc 0;
  - `cargo build --workspace` → rc 0;
  - `cargo test --workspace` (с `TMPDIR` = мой подкаталог scratchpad, переменные `RPC_URL`/`FEED_URL`/`RECORDER_OUT_DIR`/`ENRICHER_*` сняты) → 0 failed. Enricher: lib 21 (было 19, +2), exit_codes 6 (+1), gaps 6, interrupt 2, logs 2, range 3 (новый), retry 6. Остальные: decoders 19+23+1+8, hood-core 27, recorder 72 + mock_feed 16.
  - `cargo test -p enricher` прогнал ещё 2 раза: 46/46 оба раза.
- **Временные каталоги:** после `cargo test --workspace` отдельный `TMPDIR` пуст (`ls` ничего не выводит). Тесты recorder и hood-core тоже за собой убирают.
- **Распакованное содержимое = HEAD** (две release-сборки в моём подкаталоге scratchpad: HEAD из `git archive 83cadb4` и рабочее дерево; один Python-мок на 127.0.0.1). Мок отдаёт блоки с полями Arbitrum (`l1BlockNumber`, `sendCount`, `sendRoot`), `to: null`, `value` > u64, блоки без tx, `status 0x0`, логи в чеках, юникод и кавычки в строке.

| Режим | Параметры | Файлы | Распакованное | Сжатое |
|---|---|---|---|---|
| диапазон blocks | 1000..1099, `--chunk 30 --batch 7 --concurrency 3` | 4 | sha256 совпадает у всех 4 | отличается |
| `--gaps` | 50..99, 150, 300..499; `--chunk 64 --batch 10 --concurrency 4` | 6 | совпадает у всех 6 | отличается |
| logs | 1000..4000, `--chunk 1500 --logs-window 250` | 3 | совпадает после замены `meta.created_unix` (время создания, менялось и до 026) | отличается |

  - Набор файлов в каталогах одинаковый. Столбцы 1–3 `filled.tsv` совпадают в обоих режимах blocks.
  - Отличие сжатых файлов blocks-формата (10 из 10) — ровно флаг checksum в байте дескриптора и 4 байта суммы в конце. Проверка: если в новом файле сбросить бит 2 байта 4 и отрезать последние 4 байта, получается побайтно файл HEAD.
  - `zstd -lv`: HEAD — `Check: None`, новый — `Check: XXH64 …` у всех 13 файлов. `zstd -t` проходит у всех файлов обеих версий (2 046 071 байт распакованного у каждой).
- **Порча байта на CLI** (`blocks-300-363`, 199 одиночных инверсий бита по всему файлу, `zstd -dc`): файл HEAD — ошибка в 84 случаях, в 115 код 0 (порча не замечена); новый файл — ошибка в 199 из 199.
- **Режим диапазона, бинарники на том же моке:**

| Случай | HEAD | Новый |
|---|---|---|
| `--from 100 --to 109 --max-calls 20` | код 75, 1 HTTP-запрос | код 1, 0 запросов, текст с `lower --chunk to <= 9 or raise --max-calls to >= 21` |
| `--from 100 --to 199 --chunk 30 --dry-run` | код 0, 8 запросов, записаны 4 файла и `filled.tsv` | код 0, 0 запросов, каталог не создан, план: `files=4 planned_calls=201` и 4 строки `to fill` |
| `--mode logs --from 100 --to 199 --dry-run` | код 0, 2 запроса, файл записан | код 0, 0 запросов, каталог не создан |

- **CLI:** список `--флагов` в `--help` HEAD и нового кода совпадает. Отличаются только тексты справки `--max-calls` и `--dry-run` (приведены выше).

## Предполагается / не проверено

- Что `analytics/hoodlib.py` (Python `zstandard`) читает новые файлы и проверяет сумму. Модуля `zstandard` в локальном Python нет, поэтому проверял только `zstd` CLI и Rust-крейтом `zstd`. По документации `zstandard` проверяет content checksum, если флаг стоит. Data-auditor стоит прогнать `hourly_metrics.py` на новом файле.
- Контрольная сумма ловит порчу содержимого после записи (диск, копирование). От ошибки RPC-ответа она не защищает: сумма считается от того, что пришло. От этого защищают проверки `validate_block`.
- Старые файлы на сервере контрольную сумму не получают. Переписывать их задача не просила. Для полной защиты можно перепаковать их (`zstd -dc | zstd --check`) — это решение за Михаилом, формат строк при этом не меняется.

## Вопросы к Cowork / Михаилу

1. `--max-calls` в `deploy/enricher-gaps.service`: 4001 или 4201 (см. З1 data-auditor к 020). До решения — 4000, и комментарий в шапке юнита («4000 calls = 2000 blocks per run») неверен.
2. ~~Строка «Коды выхода enricher» в `data-model.md`~~ — закрыт: фразу о коде 1 в режиме blocks (диапазон с 026, `--gaps` с 020) добавил координатор.
3. ~~Doc в `crates/hood-core/src/jitter.rs` («switches over in task 026»)~~ — закрыт: исправил координатор.

## Следующий шаг

Нужна проверка data-auditor (изменён формат сжатия файлов enricher: побайтно распакованное = HEAD, checksum в `zstd -lv`) и architect-reviewer. Прошу запустить обоих.

## Правки после ревью (2026-10-03)

Источник — `docs/reviews/026-enricher-followups-architect-reviewer.md` (PASS с замечаниями). Поведение и выходные файлы не менялись.

- **Р1.** В `lib.rs` добавлены два helper'а, общие для диапазона и `--gaps`:
  - `planned_calls(chunks)`: 0 для пустого плана, иначе `CHAIN_ID_CALLS` + вызовы всех файлов через `saturating_add`. В `--gaps` раньше было `todo_blocks * CALLS_PER_BLOCK + …` без saturating; для любого реального плана значение то же;
  - `log_files_to_fill(chunks)`: строки `to fill`.
  - Заодно в ветке `Mode::Logs` у `info!` поставлена `;` (pedantic, Р5).
- **Р2.** Guard временного каталога теперь один: `src/testdir.rs` (`pub struct TestDir` / `pub fn new`; модуль приватный и только `#[cfg(test)]`, публичный API крейта не расширяется). `tests/common/mod.rs` подключает его через `#[path]` и реэкспортирует как `Scratch`; копия `Scratch` и импорт `PathBuf` удалены. Имена каталогов (`enricher-it-…`) и места вызова не менялись.
- **Р3.** Из `rpc.rs::backoff_grows_and_is_capped` удалён цикл проверки `rand01()`: это покрывает `hood-core` (`unit_values_stay_in_range_and_spread`).
- **Р4.** Отчёт приведён в соответствие с деревом: `jitter.rs` в объёме, вопросы 2 и 3 закрыты.
- Р5 в части `has_checksum_flag` (по 4 строки в двух крейтах сборки) не трогал — необязательно, а правки сейчас нужно держать минимальными.

Проверено (2026-10-03, локально): `cargo fmt --all -- --check` → rc 0; `cargo clippy --workspace --all-targets -- -D warnings` → rc 0; `cargo test -p enricher -p hood-core` (отдельный `TMPDIR`) → enricher lib 21, exit_codes 6, gaps 6, interrupt 2, logs 2, range 3, retry 6; hood-core 27; 0 failed; `TMPDIR` после прогона пуст. `cargo clean` не запускал (по указанию координатора).

## Файлы

- `crates/enricher/src/atomic.rs`, `crates/enricher/src/lib.rs`, `crates/enricher/src/rpc.rs`, `crates/enricher/src/ranges.rs`, `crates/enricher/src/testdir.rs` (новый)
- `crates/enricher/tests/common/mod.rs`, `crates/enricher/tests/exit_codes.rs`, `crates/enricher/tests/range.rs` (новый)
- `.claude/skills/hoodchain-mev/references/data-model.md`, `deploy/README.md`

## Решения Михаила (2026-10-05)

- `--max-calls` в `deploy/enricher-gaps.service` — **4201** (1 вызов `eth_chainId` + 2 файла по 1 000 блоков + 200 вызовов запаса на повторы); юнит, комментарий и `deploy/README.md` обновлены. При `--rps 2` прогон ~35 мин, `TimeoutStartSec=55min` хватает.
- Старые файлы enricher перепаковать с контрольной суммой — **да**. Выполнено координатором 2026-10-05 на Mac: 9 файлов (`data/blocks` 7, `data/logs` 1, `data/samples` 1; у последнего сумма уже была), содержимое после `zstd -dc` побайтно то же (sha256 до/после), `zstd -t` проходит, mtime сохранён. На сервере файлов enricher нет (`/srv/hood/data/blocks`, `/srv/hood/data/logs` пусты).
