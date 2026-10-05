# 032 — архитектурное ревью (architect-reviewer)

- Дата: 2026-10-05.
- Объём: рабочее дерево против HEAD `8bf5a65`, без коммита.
  - Новый крейт `crates/loader`: `config`, `ch`, `zst`, `tsv`, `rows`, `blocks_file`, `feed`, `load`, `lib`, `main`, `tests/clickhouse.rs`.
  - `crates/decoders/src/model.rs`: новые поля `Block` и `Tx`.
  - `crates/hood-core`: новый `feedline.rs`, `ranges::parse_gaps_file`.
  - `crates/recorder`: `rawline`, `recovery`, `writer` (только тест), `route` (только doc) переведены на `hood_core::feedline`.
- Контекст: `crates/decoders/src/rows.rs` (033), `crates/enricher/src/blocks.rs`, `sql/001`–`004`, `data-model.md` (раздел «Загрузчик»), `clickhouse-best-practices/rules/insert-*`, `docs/reviews/full-2026-10-02-decoders-architect-reviewer.md`.
- **Вердикт: PASS с замечаниями.** Блокирующих нет: fmt, clippy `-D warnings` и офлайн-тесты чистые, паник на внешних данных нет, правила проекта соблюдены. Замечания В1–В3 стоит закрыть до (или в рамках) задачи подключения `hood.swaps` к загрузчику.

## Линт: вывод команд (проверено 2026-10-05, Mac, `--offline`, сеть не использовалась)

| команда | итог |
|---|---|
| `cargo fmt --all -- --check` | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0, предупреждений нет |
| `cargo test --workspace` | exit 0. loader: 17 юнит-тестов прошли. hood-core 29, recorder 71 + 16 (`mock_feed`), decoders 35 + 23 + 1 + 4 + 8, enricher 21 + 25 — все ok |
| не запускалось | `crates/loader/tests/clickhouse.rs::load_counts_roundtrip_idempotency_rollback` — `#[ignore]`, нужен временный ClickHouse (`HOOD_LOADER_TEST_ENV_FILE`) и `data/`. Docker и ClickHouse я не поднимал. Результат этого теста известен только из отчёта исполнителя |
| clippy pedantic + nursery (рекомендательно; по loader и decoders, шумные группы отключены) | по loader: 2 × `redundant_clone` (`config.rs:112`, `zst.rs:117`), 2 × `map_unwrap_or` (`zst.rs:120,123`), `redundant_closure` (`blocks_file.rs:165`), `single_match_else` (`blocks_file.rs:186`), `option_if_let_else` (`config.rs:70`), `too_many_lines` (`tests/clickhouse.rs:115`, 152 строки). По существу ничего |

## Блокирующее

Нет.

## Важное

**В1. [`crates/loader/src/load.rs:165-166`, `175-207`] Откат после неоднозначной ошибки может удалить строки блока, маркер которого уже закоммичен.**
- Что происходит. `rollback` вызывается на любую ошибку `insert_tsv`, в том числе на транспортную: таймаут 600 с (`ch.rs:41`) или обрыв соединения после отправки тела. При таком исходе сервер мог INSERT выполнить. Если это был последний шаг (`hood.blocks`), загрузчик удаляет строки `txs`/`logs`/`funding_edges` для `new_blocks`. Маркер «файл загружен целиком» при этом остаётся, а в сообщении написано «rolled back».
- Почему это важно здесь. Инвариант модуля «строка в `hood.blocks` ⇒ все таблицы файла на месте» (`load.rs:8-9`) тихо нарушается. Повторный запуск всё починит, но после текста «rolled back» оператор считает, что файла в базе нет. Data-auditor увидит такой блок как «полный, без tx». Вероятность на loopback мала, но откат — ровно та ветка, на которую полагаются при сбоях.
- Исправление (около 10 строк, поведение успешного пути не меняется). Перед удалением перечитать маркер и не трогать блоки, которые в нём уже есть:
  ```rust
  // in rollback(), before the DELETE loop
  let present = existing_blocks(ch, new_blocks).await; // already returns HashMap<u64, Existing>
  let to_delete: Vec<u64> = match &present {
      Ok(p) => new_blocks.iter().copied().filter(|n| !p.contains_key(n)).collect(),
      Err(_) => return cause.context("INSERT outcome unknown and hood.blocks unreadable: \
                                      nothing deleted; re-run the loader on this file"),
  };
  if to_delete.len() != new_blocks.len() {
      // marker committed despite the error: the file is in, do not delete its rows
  }
  ```
  В тексте ошибки различать три исхода: «откачено», «маркер уже записан, ничего не удалено», «исход неизвестен».
  Альтернатива: откатывать только при HTTP-ответе сервера с не-2xx (ошибка точно атомарна), а при транспортной ошибке ничего не удалять и сообщать «перезапустите». Для этого `ch::post` должен возвращать различимую ошибку (enum `ChError::{Server, Transport}` или `anyhow` с `downcast`).

**В2. [`crates/loader/src/rows.rs:245-288`, `crates/decoders/src/rows.rs:440-499`] Разбор `sql/` в тестах продублирован, и копии уже разошлись.**
- Что разошлось:
  - имя пересоздаваемой таблицы: loader принимает только суффикс `_NNN` ровно из трёх цифр (`rows.rs:269`), decoders — любое число цифр (`rows.rs:491`);
  - поиск поздних `ALTER`: loader ищет `ALTER TABLE hood.t` и исключает `hood.t_` (`rows.rs:279`), decoders ищет `ALTER TABLE hood.t ` с пробелом (`rows.rs:496`). То есть `ALTER TABLE hood.t\n` с переводом строки или `hood.t(` одна копия поймает, а другая нет;
  - `sql_lines` в decoders проверяет, что файлы есть, в loader — нет.
- Почему это важно. Миграция 005, которая тронет `swaps` или `blocks`, может пройти один тест и не пройти другой. Знание «какие колонки могут быть пустыми» (`MAY_BE_EMPTY`) для `FundingEdge` лежит в loader (`rows.rs:228`), далеко от `FundingEdge::COLUMNS` и writer'а в decoders. Для `SwapRow` (`router`) будет так же: третья точка правды о таблице.
- Исправление (рекомендую; делать в задаче подключения `hood.swaps`). Описание таблиц собрать в одном месте — `decoders::rows`. Модуль чистый (без IO), так и задумано раскладкой ревью 2026-10-02 («rows.rs — ClickHouse row types and their TSV»), и это уже декларирует doc `decoders/src/rows.rs:1`.
  - Перенести трейт `TsvTable` (`TABLE`, `COLUMNS`, `MAY_BE_EMPTY`, `NULLABLE`, `write_tsv(&self, &mut Vec<u8>)`) и `BlockRow`/`TxRow`/`LogRow`/`FeedGapRow` из `loader/src/{tsv,rows}.rs` в `decoders/src/rows/`. Все четыре строятся из `decoders::model` и простых чисел, HTTP не нужен.
  - Реализовать `TsvTable` для `FundingEdge` и `SwapRow` рядом с их `COLUMNS`.
  - Один набор SQL-тестов (`last_create_table`, `column_order == COLUMNS` для всех шести таблиц, Enum8) — в `decoders/src/rows/tests.rs`. Копию в loader удалить.
  - В loader остаются `Batch` и `check_line` (валидация перед вставкой) и всё сетевое.
  - Раскладка, чтобы `rows.rs` (сейчас 670 строк) не разросся: `rows/mod.rs` (трейт, `HexOr`/`DecOr`/`F64`, реэкспорт), `rows/raw.rs` (blocks/txs/logs/feed_gaps), `rows/funding.rs`, `rows/swaps.rs`, `rows/tests.rs`.

  Минимальный вариант, если переносить не хочется: вынести одну чистую функцию `fn last_create_columns(sql: &str, table: &str) -> Result<Vec<String>, String>` (например, в `hood_core::sqlschema`) и вызвать её из обоих тестов. Дубль уйдёт, но `MAY_BE_EMPTY` останется вдали от writer'а.

**В3. [`crates/enricher/src/blocks.rs:30`, `crates/loader/src/blocks_file.rs:77-82`] Имя файла блоков: формат пишет один крейт, разбирает другой.**
- `blocks-{from}-{to}.jsonl.zst` создаётся в enricher, а разбирается в loader. На этом имени держатся две проверки загрузчика: «в файле ровно `from..=to`» (`blocks_file.rs:102-113`) и ограничение индекса фида (`lib.rs:115-118`). Если в enricher поменяют формат (например, добавят префикс), loader молча перестанет видеть файлы в каталоге: `expand_blocks_paths` их отфильтрует и закончит работу с «no blocks files found» или, хуже, с частичным списком.
- Исправление: в `hood_core::ranges` (там уже `Range` и `filled.tsv`) положить пару `pub fn blocks_file_name(r: Range) -> String` и `pub fn parse_blocks_file_name(name: &str) -> Option<Range>` с тестом «туда-обратно». Enricher вызывает первую, loader — вторую. Поведение не меняется. Enricher на сервере — правку выкатывать с его ближайшей задачей, отдельный деплой ради этого не нужен.

## Рекомендации

- **Р1. [`feed.rs:26`, `load.rs:84`, `load.rs:118-126`] Хэши блоков хранятся строками.** `FeedSeen.block_hash: Option<String>` и `Existing.hash: String` сравниваются с `format!("{:#x}", B256)`. Разбирать в `B256` при индексации (`hood_core::hex` или `B256::from_str`) — тогда битый `blockHash` фида станет ошибкой с позицией, а сравнение — сравнением типов. Это 3 места, меньше аллокаций на каждый блок.
- **Р2. [`load.rs:154-166`, `load.rs:292`, `ch.rs:78`] Тело каждой вставки копируется (`body.to_vec()`).** На файле с 79 446 логами копия — несколько МБ, на большом архиве удвоит пик памяти. Дать `Batch::into_body(self) -> Vec<u8>` и разбирать `FileRows` по полям (`let FileRows { txs, logs, edges, .. } = rows;`), чтобы `steps` владели телами.
- **Р3. [`zst.rs:79`] `rest = &rest[len..]`.** Сейчас паники нет: `ZSTD_findFrameCompressedSize` возвращает ошибку на усечённом кадре, и тест `corruption_and_truncation_fail_the_whole_file` это покрывает. Но recorder в том же обходе защищается явно (`recovery.rs:37`, `n > 0 && n <= rest.len()`). Сделать так же: `rest = rest.get(len..).filter(|_| len > 0).ok_or_else(|| anyhow!("frame {}: bad size {len}", frames + 1))?;`. Это дёшево и снимает зависимость от контракта C-библиотеки.
- **Р4. [`feed.rs:58-71`] Открытый час recorder валит весь запуск.** Если `--feed-dir` указывает на живой каталог (а не на копию), последний `feed-*.tsv.zst` заканчивается незакрытым кадром, и `read_zst` даёт ошибку на весь прогон. Это задокументировано в отчёте, но не в `--help`. Минимум: добавить в doc флага `--feed-dir` фразу «copy of closed hours only; the open hour fails the run». Вариант с изменением поведения (флаг `--feed-skip-open-hour`) — только по отдельному решению, сейчас не нужен.
- **Р5. [`lib.rs:139-231`] `run` — 92 строки, счётчики дублируются.** Суммирование `FileStats` в `Summary` (`lib.rs:186-193`) и лог «valid»/«loaded» повторяют одни и те же поля. Дать `FileStats::merge(&mut self, &FileStats)` и хранить в `Summary` одно поле `totals: FileStats`, а лог файла вынести в `fn log_file(path, &FileStats, Option<&FileOutcome>)`. Тогда `run` — это оркестрация в ~50 строк.
- **Р6. Нет офлайн-теста `run`.** `--dry-run` обходит ClickHouse целиком, поэтому `run(&Args { dry_run: true, blocks: vec![<фикстура 017>], gaps: vec![<временный gaps.tsv>], .. })` проверяется без сети. Это покроет `expand_blocks_paths`, `read_gaps`, `wanted_ranges` и ветку `dedupe_gaps` в `lib.rs:226-229`: сейчас они проверяются только `#[ignore]`-тестом.
- **Р7. Публичный API шире нужного.** Интеграционный тест использует `blocks_file::read_blocks_file`, `ch::Client`, `config::ChConfig`, `expand_blocks_paths`, `tsv::Batch`, `rows::BlockRow`, `feed::FeedIndex`. Остальное можно сделать `pub(crate)`: `config::env_file_value`, `ch::tsv_rows`, `ch::INSERT_SETTINGS`, `tsv::check_line`, `feed::feed_files`, `FeedIndex::add_text`, `load::dedupe_gaps`, `blocks_file::parse_lines`, `Summary`. Крейт листовой, поэтому это рекомендация, а не «важное».
- **Р8. [`load.rs:66-79`] `check_schema` сверяет только имена колонок.** Порядок плюс имена ловят непримененную миграцию, этого достаточно. Типы покрыты круговым тестом. Если захочется строже — сверять `system.columns.type` для `block_number`/`feed_recv_ns`, но не для всех колонок: иначе придётся дублировать DDL в Rust.
- **Р9. [`recorder/src/writer.rs:160`] Строку сырья пишет recorder, а разбирает теперь hood-core.** Пара «писатель/читатель» в разных крейтах. Когда recorder будут трогать по другой задаче, можно добавить `hood_core::feedline::write_raw_line(w, recv_ns, seq_first, seq_last, json)` и тест «туда-обратно». Сейчас не трогать: recorder работает на сервере, а выигрыш небольшой.
- **Р10. Текст отчёта и `data-model.md` про `insert-mutation-avoid-delete`.** Правило запрещает `ALTER TABLE … DELETE` (мутацию) и само рекомендует lightweight `DELETE FROM`. Загрузчик использует как раз `DELETE FROM … SETTINGS lightweight_deletes_sync = 2` (`load.rs:183`), то есть правило не нарушено. Формулировку «нарушается сознательно» лучше поправить, чтобы следующий читатель не искал проблему, которой нет.

## Дубли

| где | насколько разошлись | куда |
|---|---|---|
| разбор `sql/` в тестах: `loader/src/rows.rs:245-288` и `decoders/src/rows.rs:440-499` | уже разошлись (суффикс `_NNN`, поиск `ALTER`, проверка пустого каталога) | В2: одно место в `decoders::rows` |
| `MAY_BE_EMPTY` таблицы `funding_edges` в loader (`rows.rs:228`), а `COLUMNS`/writer — в decoders | пока совпадают; сломается громко (валидация), не тихо | В2 |
| формат имени `blocks-<from>-<to>.jsonl.zst`: `enricher/src/blocks.rs:30` и `loader/src/blocks_file.rs:77` | совпадают | В3: `hood_core::ranges` |
| обход кадров zstd: `loader/src/zst.rs:63-82` и `recorder/src/recovery.rs:32-46` | разные задачи: подсчёт флагов checksum против «самого длинного валидного префикса» | допустимо, объединять не надо (recorder живой) |
| разбор строки сырья: recorder → `hood_core::feedline` | **устранён в этой задаче**, один парсер на recorder и loader | — |
| разбор `.env`: `loader/src/config.rs:91` и `sql/apply.sh` | намеренно одинаковые правила, есть тест `env_file_rules_match_apply_sh` | допустимо: Rust и bash, источник истины назван в doc |

## Перенос `feedline` в hood-core и поведение recorder (проверено чтением кода 2026-10-05)

- `hood_core::feedline::parse_raw_line` и `RawLine` побайтно совпадают с удалёнными из `recorder/src/rawline.rs` (та же `splitn(4, '\t')`, тот же `num<T: FromStr>`, те же типы полей). Добавлен только `RawLine::is_unsequenced()` с тем же условием `seq_first == 0 && seq_last == 0`.
- Метод `RawLine::seqs()` стал свободной функцией `rawline::line_seqs(&RawLine)` с идентичным телом: условие на unsequenced заменено вызовом `is_unsequenced()`, разбор `FeedEnvelope` и `envelope_seqs` прежние.
- Продакшен-код записи (`writer.rs`) не изменён, правка только в тесте (`writer.rs:510`). `recovery.rs:177` вызывает `line_seqs(&l)` вместо `l.seqs()`. `route.rs` — только doc.
- Путь `recorder::rawline::{parse_raw_line, RawLine}` сохранён реэкспортом. Тест `parses_columns_strictly` перенесён в hood-core и дополнен проверками `is_unsequenced`.
- recorder: 71 юнит + 16 `mock_feed` тестов прошли.
- **Вывод: поведение recorder не меняется.** Пересборка и перевыкатка recorder ради этой правки не нужны. При ближайшем деплое бинарь изменится только в части тестов.

`ranges::parse_gaps_file` переиспользует `split_unterminated` и `parse_range_line`, то есть ту же политику, что `parse_ranges_file` (строгая, незавершённая последняя строка возвращается). Recorder читает тот же файл мягко (`parse_ranges_file_lenient`) — это намеренно разные политики для разных потребителей, и в doc это сказано.

## Структура и разбиение

Разбиение крейта хорошее, менять его не нужно. Каждый модуль отвечает за одно: настройки, HTTP, распаковка, валидация TSV, строки, разбор файла, фид, порядок вставки. IO собран в тонких функциях (`read_zst`, `read_blocks_file`, `FeedIndex::from_dirs`, `load_file`), а их чистые части (`decode`, `parse_lines`, `add_text`, `merge_block_state`, `dedupe_gaps`, `ChConfig::resolve`) тестируются без диска и сети.

Единственная перестройка, которую предлагаю, — В2: перенести описание таблиц в `decoders::rows`. Её стоит сделать вместе с подключением `hood.swaps`, чтобы `SwapRow` сразу получил `TsvTable` рядом со своими `COLUMNS`.

## Что хорошо (не сломать при правках)

- Порядок «распаковать весь файл с XXH64 → разобрать → построить и проверить все TSV → только потом сеть» и `blocks` последним как маркер завершения. Настройки атомарной вставки собраны в одной константе `INSERT_SETTINGS`, и в doc записано, на каком замере они держатся (`ch.rs:7-13`).
- Секрет: `Password` с редактирующим `Debug`, ключ в заголовке `X-ClickHouse-Key`, а не в URL и не в argv. URL с `user:pass@` отвергается, разрешён только loopback. В тексте ошибок только первая строка SQL.
- `tsv::check_line` — общий валидатор по описанию таблицы. Он закрывает класс ошибок «пустое → 0» (З1 ревью 029) для всех таблиц сразу, а не по месту.
- Идемпотентность `feed_recv_ns` — чистая функция `merge_block_state` (минимум из таблицы и фида, конфликт хэшей — ошибка) с юнит-тестом. Конфликт `to_seq` в `feed_gaps` ловится и в файлах, и против таблицы.
- Новые поля `decoders::model` разбираются там же, где весь hex (`input_head` не декодирует весь `input`), с юнит-тестом. Обязательность полей подтверждена сканом данных и записана в doc.

## Предполагается / не проверено

- Интеграционный тест `tests/clickhouse.rs` (откат, порча файла, идемпотентность, круговой путь, `feed_gaps`) я не запускал: нужен временный ClickHouse. Его результат известен только из отчёта 032.
- Сценарий В1 (транспортная ошибка после коммита INSERT) не воспроизводился. Вывод сделан чтением кода `ch.rs`/`load.rs` и по общему поведению HTTP-интерфейса ClickHouse: вставка с полностью полученным телом может завершиться после разрыва соединения клиентом. На 26.9 это не проверял.
- Атомарность вставки на 26.9.6.6, приём `nan` и `date_time_input_format=basic` — факты из отчёта исполнителя, я их не перепроверял.
- Цифры загрузки (2 712 / 34 344 / 127 824) — зона data-auditor, мной не сверялись.
