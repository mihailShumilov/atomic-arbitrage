# 019 — Общий код в hood-core: отчёт

Исполнитель: indexer-engineer. База: HEAD `449576f` (после форматирования 018). Не закоммичено, статус задачи не менялся, ревьюеры не запускались. Затронуты только `crates/hood-core`, `crates/enricher`, `crates/recorder` и `Cargo.lock` (2 строки: `chrono` у hood-core и dev-зависимость enricher). Деплоя не было.

## Сделано

Новые модули `hood-core`. Зависимости только `std` и `chrono` из `[workspace.dependencies]` (новых крейтов в `Cargo.lock` нет). Политики IO внутри нет, кроме самих fs-операций в `fsutil`:

| Модуль | Что | Где |
|---|---|---|
| `http` | `parse_retry_after(value, now: SystemTime) -> Option<Duration>`: целые и дробные секунды (разбор точный, без f64; знак, экспонента, `inf`/`nan` отвергаются), IMF-fixdate, устаревшие RFC 850 и asctime. Дата в прошлом даёт `ZERO`. Потолок задаёт вызывающий: recorder `RETRY_AFTER_MAX` 6 ч, enricher `max_retry_after` 600 с | `crates/hood-core/src/http.rs:23` |
| `ranges` | `Range` (+ `type Gap = Range`), `Range::new -> Result<_, RangeError>`, `detect_gap` (без переполнения у `u64::MAX`), `merge`, `subtract`, `chunk`, `parse_ranges_file` и `parse_ranges_file_lenient` (после F1; единая политика: последняя строка без `\n` возвращается вызывающему для WARN, битая строка с `\n` — ошибка `LineError` с номером строки), `GapRow` / `FilledRow` с `Display` (единственные писатели строк `gaps.tsv` / `filled.tsv`), `to_lines` | `crates/hood-core/src/ranges.rs:22` и далее |
| `fsutil` | `fsync_dir`, `rename_durable`, `write_atomic` (tmp `.<name>.tmp` → fsync → rename → fsync каталога), `append_synced` / `append_line_synced` (один `write_all`, `sync_all`, fsync каталога). Ошибки возвращаются всегда, `ErrorKind` сохраняется, в тексте — шаг и путь | `crates/hood-core/src/fsutil.rs:28-73` |
| `hex` | `quantity(u64)`, `parse_quantity(&str)` (строго `0x` + 1–16 hex-цифр) | `crates/hood-core/src/hex.rs:10,17` |

`lib.rs`: модули, реэкспорт `detect_gap`, `Gap`, `Range` в корне, doc-комментарии у всех `pub`. **`CHAIN_ID` оставлен**, с doc: его использует проверка `eth_chainId` из задачи 020 («ожидаемый chain id — из hood-core»). Если удалить его сейчас, 020 пришлось бы добавлять константу заново. Пока он по-прежнему нигде не используется: можно читать это как «отложено до 020».

`hex` для адресов не делал: в recorder и enricher адреса не разбираются, а decoders в этой задаче трогать нельзя. 022 может перейти на `hood_core::hex`, когда 019 будет влита.

## Удалённые дубли (было → стало)

«Было» — номера строк на HEAD `449576f`, «стало» — в рабочем дереве.

| Что | Было | Стало |
|---|---|---|
| `parse_retry_after` (recorder: целые + дата) | `crates/recorder/src/backoff.rs:266` (+ тест `:526`) | `hood_core::http::parse_retry_after`, вызов `crates/recorder/src/net.rs:273` |
| `parse_retry_after` (enricher: целые + дробные) | `crates/enricher/src/rpc.rs:51` (+ тест `:451`) | то же, вызов `crates/enricher/src/rpc.rs:349` |
| Тип диапазона | `crates/hood-core/src/lib.rs:45` `Gap`, `crates/enricher/src/ranges.rs:16` `Range` | `crates/hood-core/src/ranges.rs:22` `Range`, `:30` `type Gap` |
| `detect_gap` + формула `a+1..=b-1` | `crates/hood-core/src/lib.rs:52`, `crates/recorder/src/route.rs:83` | `crates/hood-core/src/ranges.rs:74`; `route.rs:82` вызывает `detect_gap` |
| Вычитание диапазонов | `crates/recorder/src/writer.rs:218` `uncovered`, `crates/enricher/src/ranges.rs:101` `subtract` (+ `merge :88`, `chunk :120`) | `crates/hood-core/src/ranges.rs:80,93,123`; recorder `writer.rs:273` |
| Разбор `gaps.tsv` | `crates/recorder/src/writer.rs:203` `read_gap_ranges` (молча пропускал битые строки), `crates/enricher/src/ranges.rs:37,66,80` `parse_ranges_tsv` / `split_unterminated` / `read_gaps_file` | разбор `crates/hood-core/src/ranges.rs:143-219`; IO-обёртки `crates/enricher/src/ranges.rs:25,32`, `crates/recorder/src/writer.rs:187` |
| Разбор `filled.tsv` | `crates/enricher/src/ranges.rs:55` `read_ranges_file` (без обработки недописанной строки) | `crates/enricher/src/ranges.rs:32` `read_filled_file` на той же политике, WARN в `crates/enricher/src/lib.rs:197-202` |
| Строка `gaps.tsv` | `crates/recorder/src/writer.rs:196` `GapRow`, `format!` в `:526,:532`, `writeln!` в `:312` | `hood_core::ranges::GapRow` + `Display` (`ranges.rs:223`); `pending_gaps: Vec<GapRow>` (`writer.rs:494,500`) |
| Строка `filled.tsv` | `format!` в `crates/enricher/src/blocks.rs:146` | `hood_core::ranges::FilledRow` (`ranges.rs:239`), `blocks.rs:149` |
| fsync каталога | `crates/recorder/src/writer.rs:44` `sync_dir` (глушил ошибку), `crates/enricher/src/atomic.rs:81` `fsync_dir` (ошибка) | `crates/hood-core/src/fsutil.rs:28` (ошибка); recorder `writer.rs:97,445` |
| Атомарная запись | `crates/recorder/src/writer.rs:51` `write_atomic`, `crates/enricher/src/atomic.rs:54-58` rename + fsync каталога | `fsutil.rs:50` `write_atomic`, `fsutil.rs:35` `rename_durable`; recorder `writer.rs:358,553`, enricher `atomic.rs:57` |
| Дописать + fsync | `crates/recorder/src/writer.rs:310-315` (`sync_data` + `sync_dir`), `:580-584` (`sync_data`, без fsync каталога), `crates/enricher/src/atomic.rs:131` (`sync_all`, два `write_all`) | `fsutil.rs:63,73`; recorder `writer.rs:278,548`, enricher `blocks.rs:150` |
| hex-quantity | `crates/enricher/src/logs.rs:77` `hex_u64`, `format!("0x{..:x}")` в `logs.rs:90`, `blocks.rs:32,56`, `tests/common/mod.rs:51,89` | `hood_core::hex`; `blocks.rs:35,59`, `logs.rs:88,150,157`, `tests/common/mod.rs` |

Тесты перенесены в hood-core: `retry_after_parsing` (оба крейта), `uncovered_ranges` (recorder), тесты `enricher/src/ranges.rs`. Оставшийся в recorder `file.sync_data()` (`writer.rs:540`) — это fsync потокового часового файла после zstd-фрейма, а не дубль `fsutil`.

## Изменения поведения

1. **recorder понимает дробный `Retry-After`** (`"90.5"` → 90,5 с, правило `retry_after`). До 019 такое значение давало `None` и включало лестницу 429 (5 мин). Значения меньше 1 с лестница по-прежнему поднимает до 1 с. Ещё recorder теперь понимает устаревшие форматы дат RFC 850 и asctime, а задержку до даты считает от точного `now`, без округления до секунды (раньше пауза могла быть до 1 с длиннее).
2. **enricher понимает HTTP-date** в `Retry-After`. До 019 дата давала `None` и экспоненциальную паузу ≤ 60 с. Потолок 600 с не изменился. Строки, которые раньше проходили через `f64` (`"1e3"`, `".5"`, `"5."`), теперь отвергаются и дают экспоненциальную паузу. По RFC это не delta-seconds.
3. Побочные изменения: в задаче сказано, что поведение меняется только в п. 1–2, поэтому перечисляю их отдельно. Формат файлов они не меняют.
   - **fsync каталога в recorder стал ошибкой**, раньше глушился. Это требование п. 3 задачи. Места: создание часового файла, `last_seq.txt`, `_torn/`, `gaps.tsv`. В writer'е такая ошибка даёт `shutdown writer_error` и выход 2, при старте — выход 1. На исправном диске поведение то же. Обоснование — в комментарии `crates/recorder/src/writer.rs:42-46` и doc `fsutil.rs`.
   - Дописывание `gaps.tsv` в `commit` теперь делает fsync каталога (раньше не делало), `sync_data` заменён на `sync_all`. Строки уходят одним `write_all`, а не `writeln!` по частям. Так строка без `\n` после kill -9 практически невозможна. Для `filled.tsv` то же: один `write_all` вместо двух (замечание R2 ревью enricher) плюс fsync каталога.
   - **Recorder при чтении `gaps.tsv` на старте:** недописанная последняя строка пропускается с WARN, раньше она учитывалась, если разбиралась. Битая строка с `\n` пропускается, но теперь с WARN, а не молча.
   - **enricher при чтении `filled.tsv`:** недописанная последняя строка пропускается с WARN, и её диапазон скачивается заново. Раньше валидная недописанная строка учитывалась, а битая была ошибкой.
   - `parse_quantity` строже прежнего `hex_u64` (отвергает `0x+1`) — на ответах RPC такого не бывает.

**Отступление от буквы п. 2 (прошу решения):** «одна политика строгости» выполнена в hood-core и в enricher. Но recorder при старте **пропускает** битую строку `gaps.tsv` с `\n` с WARN, а не падает. Причина: `recover` выполняется при каждом старте живого процесса. Ошибка там означает падение каждые 120 с под systemd из-за собственного файла состояния, то есть потерю фида. Пропуск в худшем случае даёт повторную строку дыры, а enricher сливает пересекающиеся диапазоны. Сам `enricher --gaps` такой файл отвергает, так что строку заметят. Ревьюер recorder (В9) предлагал то же самое. Обоснование — в doc `read_gap_ranges` (`crates/recorder/src/writer.rs:177-186`). Если Михаил решит, что recorder тоже должен падать, это одна строка.

## Проверено (2026-10-02, локально на Mac, без сети, RPC и фида; только 127.0.0.1)

- `cargo fmt -p hood-core -p enricher -p recorder -- --check` — чисто.
- `cargo clippy -p hood-core -p enricher -p recorder --all-targets -- -D warnings` — 0 предупреждений.
- `cargo test -p hood-core -p enricher -p recorder`: **107 passed, 0 failed** (до изменений было 93). hood-core 19 (было 2); enricher 28 (unit 13, exit_codes 2, gaps 4, interrupt 2, logs 2, retry 5); recorder 60 (unit 46, mock_feed 14).
- Новые тесты:
  - hood-core: HTTP-date всех трёх форм, дробные секунды, отказ на `-1`/`1e3`/`inf`/…, точный `now`; вычитание и склейка (включая прежние случаи `uncovered` и края `u64::MAX`); строгость разбора (недописанная строка ↔ битая с `\n`, номер строки в ошибке); побайтовый формат `GapRow` / `FilledRow`; `fsutil` (атомарная замена без остатков tmp, байты дописывания, ошибки с шагом и путём); `hex`.
  - enricher: `retry_after_http_date_is_honoured` — мок на 127.0.0.1 отдаёт 429 с датой +3 с, ожидание ≥ 1,9 с при `--backoff-ms 10`; `ranges::files_follow_one_policy`.
  - recorder: `fractional_and_date_retry_after_reach_the_ladder`, `gap_ranges_reading_policy`.
- **Побайтовая сверка с бинарниками до изменений.** Сборка HEAD сохранена до правок. Сверял на копиях в scratchpad, оригиналы в `data/` не трогал.
  - recorder на копиях `data/feed-test-009`, `--url ws://127.0.0.1:9`, затем SIGTERM. Три варианта: как есть; пустой `gaps.tsv` (строка дописывается через reconcile); `gaps.tsv` удалён и устаревший `last_seq.txt` = 77169000. В каждом варианте `gaps.tsv`, `last_seq.txt` и часовой файл у старого и нового бинарника совпадают по SHA-1, а `gaps.tsv` совпадает с оригиналом (`77169135\t77169712\t1790837152631000000\n`). Строки `gap_reconciled` в `connections.tsv` совпадают побайтно (кроме столбцов времени).
  - enricher `--gaps data/feed-test-009/gaps.tsv --dry-run` на копиях `data/blocks`: как есть и с двумя добавленными строками `filled.tsv`, частично перекрывающими дыру (`--chunk 100`). План (диапазоны, `already_filled_blocks`, куски) у старого и нового бинарника совпадает строка в строку. `filled.tsv` не изменился. Формат записи `filled.tsv` закреплён юнит-тестом и тестом `retry.rs`, где он пишется через мок.
- Workspace: `cargo build --workspace` проходит. `cargo clippy/test --workspace` и `cargo fmt --all --check` сейчас **не проходят из-за `crates/decoders`**: незавершённые правки параллельной задачи 022 (`tests/l1_inflows.rs`: нет `L2_WETH_GATEWAY`, `L2_WETH`; fmt в `src/events.rs`, `src/l1_inflows/mod.rs`). К 019 это не относится. Поэтому проверял через `-p hood-core -p enricher -p recorder`.

## Предполагается / не проверено

- Что fsync каталога на сервере (Linux, ext4/xfs) не возвращает ошибок на исправном диске, то есть новая политика не даст ложных `writer_error`. На macOS это проверено тестами и сверкой выше, на сервере — нет (деплоя не было).
- Что в серверном `gaps.tsv` нет недописанной последней строки и битых строк. Если есть, новый recorder выдаст WARN и, возможно, допишет повторную строку дыры. Проверка без рестарта: `tail -c1 /srv/hood/data/feed/gaps.tsv | od -c` и `awk -F'\t' 'NF<3 || $1!~/^[0-9]+$/ || $2!~/^[0-9]+$/' gaps.tsv`.
- Что провайдеры действительно присылают `Retry-After` датой или дробным числом: не наблюдалось, это защита на будущее.
- ~~Склейка дописывания с недописанной строкой~~ — закрыто, см. «Исправление F1».

## Исправление F1 (после ревью data-auditor и architect-reviewer, 2026-10-02)

**Находка F1** (`docs/reviews/019-hood-core-shared-data-auditor.md`, она же В2 у architect-reviewer). Сценарий: `gaps.tsv` кончается собственной строкой recorder без `\n`. Новый код по политике 019 пропускал её с WARN, reconcile дописывал строку дыры без ведущего `\n`, и строки склеивались в одну строку из 5 столбцов с испорченным `recv_ns`. Для `filled.tsv` в enricher то же самое.

Что исправлено:
- **`hood_core::fsutil::append_synced`** (`crates/hood-core/src/fsutil.rs`) открывает файл с `read + append`. Если файл не пуст и его последний байт не `\n`, сначала пишется `\n` — в том же `write_all`, до `sync_all`. `append_line_synced` идёт через неё. Обрывок прошлого падения становится отдельной завершённой строкой: валидный будет прочитан как строка, битый в recorder даст WARN и пропуск, в enricher — ошибку. Новая строка всегда начинается с новой строки. Пустой `data` файл не трогает. Обрывок считается следом прошлого падения, а не записью в процессе: у обоих файлов писатель один (recorder сам себе; enricher под блокировкой каталога). Это записано в doc функции.
- **Форматы в штатном пути не изменились:** если файл кончается `\n` или пуст, байты те же, что раньше.

**Важное 1 architect-reviewer** — сделано, изменение небольшое:
- Добавлена `hood_core::ranges::parse_ranges_file_lenient(text) -> ParsedRanges`. В `ParsedRanges` новое поле `broken: Vec<LineError>` (завершённые `\n` строки, которые не разобрались). Строгая `parse_ranges_file` теперь — та же функция плюс «первая битая строка → `Err`». Цикл разбора теперь в одном месте.
- Recorder `read_gap_ranges` (`crates/recorder/src/writer.rs`) использует lenient-вариант и пишет WARN по `unterminated` и по каждой строке из `broken`. Копия цикла убрана.
- Стали приватными `parse_ranges_tsv` (удалена: её заменил lenient-цикл), `split_unterminated`, `parse_range_line` (`ranges.rs`) и `atomic_tmp_path` (`fsutil.rs`). Grep по recorder, enricher и decoders: внешних использований нет.

Новые тесты:
- hood-core: `fsutil::append_after_unterminated_line_starts_a_new_line` (байты точно; завершённый файл не получает лишний `\n`; пустой append ничего не меняет; `filled.tsv`-обрывок через `append_line_synced`), `ranges::lenient_collects_broken_lines` (номера битых строк; строгий вариант возвращает первую из них; без битых строк оба варианта совпадают).
- recorder: `writer::tests::recover_after_own_unterminated_gap_row_does_not_glue_rows`. В `gaps.tsv` строка `101\t106\t6` без `\n`, в данных дыра 101..106. После старта файл равен `101\t106\t6\n101\t106\t6\n`: старый обрывок завершён, строка reconcile стоит отдельно. Повторный старт ничего не дописывает.
- enricher: `tests/gaps.rs::torn_last_filled_row_is_not_glued_to_the_next_one`. В `filled.tsv` строка без `\n`, мок на 127.0.0.1. Диапазон скачивается заново, в `filled.tsv` 2 строки по 4 столбца, файл кончается `\n`, повторный прогон даёт 0 запросов.

Проверено (2026-10-02, локально, без сети):
- `cargo fmt --check`, `cargo clippy --all-targets -D warnings` и `cargo test` для `-p hood-core -p enricher -p recorder`: 111 passed / 0 failed (hood-core 21, enricher 29, recorder 61).
- Workspace (decoders из 022 к этому моменту собирается): `cargo build --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check` проходят чисто; `cargo test --workspace` — 161 passed / 0 failed.
- Сценарий data-auditor на копии `data/feed-test-009` (в scratchpad, оригинал не тронут): `gaps.tsv` = `77169135\t77169712\t1790837152631000000` без `\n`, два старта нового бинарника с `--url ws://127.0.0.1:9`. После первого старта файл равен `77169135\t77169712\t1790837152631000000\n77169135\t77169712\t1790837152631000000\n` (`od -c`), строк по 3 столбца. Второй старт файл не меняет, строка `gap_reconciled` в `connections.tsv` одна.

Остаётся (осознанно, вне F1): в этом сценарии в `gaps.tsv` две одинаковые строки одной дыры. Диапазон верный, enricher их сливает. Загрузчик `hood.feed_gaps` должен убирать повторы диапазонов — это стоит отметить в его задаче.

## Вопросы к Cowork / Михаилу

1. Принять исключение для recorder (битая строка `gaps.tsv` с `\n` при старте → WARN и пропуск, а не падение) или требовать падения, как в enricher?
2. `CHAIN_ID` оставлен для 020. Подходит или удалить сейчас?
3. Деплой recorder — вместе с 021, как в задаче. Новый бинарник меняет поведение только в п. 1 и 3 выше.

## Нужна проверка

Изменены загрузчики состояния (`gaps.tsv`, `filled.tsv`) и запись recorder. **Прошу прогнать data-auditor** (побайтовая сверка `gaps.tsv`/`filled.tsv` на копиях `data/feed-test-009` и `data/blocks`) **и architect-reviewer** по `crates/hood-core`, `crates/enricher`, `crates/recorder`.
