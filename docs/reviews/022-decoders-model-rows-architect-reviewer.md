# 022 — архитектурное ревью (architect-reviewer)

- Дата: 2026-10-02.
- Объём: рабочее дерево относительно HEAD `449576f`, только `crates/decoders`: `src/{lib,model,arbitrum,addresses,events,rows,swaps}.rs`, `src/l1_inflows/{mod,registry,types}.rs` (старый `src/l1_inflows.rs` удалён), `tests/{l1_inflows,rows,swaps}.rs`, `tests/fixtures/swaps-blocks.jsonl`, `examples/l1_inflows_scan.rs`.
- Контекст: задача и отчёт 022, исходное ревью `full-2026-10-02-decoders-architect-reviewer.md` (В1–В7, Р1–Р10), `sql/001–003`, `crates/hood-core/src/hex.rs` (задача 019, в том же дереве), `crates/enricher/src/logs.rs`, `references/contracts.md`.
- **Вердикт: PASS с замечаниями.** Блокирующих замечаний нет. Все важные пункты исходного ревью закрыты (В1 со стороны Rust, В2–В7). Замечания ниже можно закрыть в этой же задаче или отдельной мелкой задачей; ни одно не меняет вывод сканера.

## Линт — вывод команд (проверено 2026-10-02, Mac, офлайн)

| команда | итог |
|---|---|
| `cargo fmt --all -- --check` | exit 0 |
| `cargo clippy -p decoders --all-targets -- -D warnings` | exit 0, чисто |
| `cargo test -p decoders` | lib 18 + `l1_inflows` 23 + `rows` 1 + `swaps` 8 = 50 тестов, все прошли (doc-тестов 0) |
| `cargo check -p enricher --all-targets` | ok: путь `decoders::ALL_TOPIC0` (`enricher/src/logs.rs:37`) сохранён |
| clippy pedantic + nursery (рекомендательно) | 49 — бэктики в доках; 35 — разделители в литералах; 6 — `Self`; 5 — длинный первый абзац doc; 2 — `const fn`; по 1 — `&Option<T>` (`model.rs:205`), `too_many_lines` (`examples/l1_inflows_scan.rs:28` `main`, 133/100), `map_or`, `if let` |

`cargo clean` не запускал по указанию координатора (target общий).

## Блокирующее

Нет.

## Важное

Нет. Пункты исходного ревью закрыты так:

| пункт | статус | где |
|---|---|---|
| В1 (Rust-сторона ключа `funding_edges`) | закрыт: `log_index: Option<u32>` → `u32::MAX` в одном месте (`FundingEdge::log_index_column`), `Some(u32::MAX)` — ошибка; NULL в `funding_edges` не пишется | `rows.rs` |
| В2 (общая модель) | закрыт: `model::{Block, Tx, ArbFields, Log, TxCtx, parse_block_line}`, serde-слой приватный (вариант «а»), `RawLog`/`DecodedLog`/`parse_line` удалены, тип лога один | `model.rs` |
| В3 (строки → ClickHouse в библиотеке) | закрыт: `COLUMNS`, `write_tsv_header`, `write_tsv`, golden-тест на 4 блоках фикстуры 017, тест порядка колонок по последнему `CREATE` в `sql/` | `rows.rs`, `tests/rows.rs` |
| В4 (enum вместо `&'static str`) | закрыт: `EdgeKind`/`GatewayStatus` `#[repr(i8)]`, дискриминанты = Enum8, тест «Enum8 в SQL = enum в Rust» сравнивает пары имя=значение | `rows.rs` |
| В5 (`decode_swap`) | закрыт: `NotSwap / Swap / Malformed(SwapError)`, `SwapCounters`, позиция и `TxCtx`, фикстура реального блока, проверка знаков независимо от литералов, поведение alloy на число topics закреплено тестом | `swaps.rs`, `tests/swaps.rs` |
| В6 (`status`) | закрыт: `status: String` без `default`, значения кроме `0x0`/`0x1` — ошибка; тесты есть | `model.rs:300`, `:152-156` |
| В7 (дубли в реестре) | закрыт: `push_unique` для `new`/`extend`/`parse_tsv`, тест `observed` перед `verified` | `l1_inflows/registry.rs` |
| Р1–Р3, Р4 (часть), Р5, Р6, Р7, Р8, Р9, Р10 | закрыты; остаток Р4 (сумма сырых единиц разных токенов) задокументирован в поле, вывод не меняли — согласен | |

## Рекомендации

**Р1. `quantity_u256` пропускает `_`** (`model.rs:215-218`).
- Что не так: `U256::from_str_radix` в ruint 1.20.1 игнорирует `_` (`string.rs:74`, `decode_digit`: `b'_' => Ok(None)`). Поэтому `"0x_"` разбирается как 0, `"0x1_0"` — как 16. Это тот же класс дыры, что Р5 исходного ревью («пустое значение = 0»), и он расходится с `hood_core::hex::parse_quantity`, которая проверяет каждую цифру.
- Почему важно здесь: модуль обещает «broken input is an error, never a silent default», а тест `quantity_requires_prefix_and_digits` создаёт впечатление полной строгости. В реальном RPC `_` не бывает, так что риск низкий, но правка — одна строка.
- Исправление:
  ```rust
  let h = s.strip_prefix("0x").ok_or_else(|| anyhow!("quantity {s:?} without 0x"))?;
  ensure!(!h.is_empty() && h.bytes().all(|b| b.is_ascii_hexdigit()), "bad quantity {s:?}");
  ```
  и строку `assert!(quantity_u256("0x_").is_err());` в тест.
- Заодно: `parse_addr`/`parse_b256` (`model.rs:229-235`) через `FromStr` alloy принимают строку и без `0x`, а `quantity_*`/`parse_bytes` префикс требуют. Для единообразия — `s.strip_prefix("0x")` + разбор, или явно написать в doc, что для адресов и хэшей префикс не проверяется.

**Р2. TODO про `hood-core::hex` — менять формулировку, не переносить сейчас** (`model.rs:210`).
- Факт (проверено по `crates/hood-core/src/hex.rs` в дереве 019): там только `quantity(u64)` и `parse_quantity(&str) -> Option<u64>`; alloy в hood-core нет. Декодерам нужны `U256`, `u32`, `Address`, `B256`, `Bytes` — на alloy-типах. Перенос в hood-core потянул бы туда `alloy-primitives` ради одного потребителя; ревью части 3 это уже не советовало.
- Решение: хелперы остаются в `decoders::model` (это и есть граница «сырьё → типы» для alloy). Переключать на hood-core ни сейчас, ни после 019 не нужно. Общее у двух модулей — только грамматика quantity (`0x` + 1..n hex-цифр, ведущие нули допустимы). После Р1 она совпадёт.
- Исправление: заменить TODO на
  ```rust
  // Hex helpers on alloy types. The quantity grammar (0x + >=1 hex digit, leading zeros ok)
  // is the same as hood_core::hex::parse_quantity; keep them in sync. Not moved to hood-core:
  // it has no alloy dependency and the decoders are the only alloy-based consumer.
  ```
  Если позже enricher начнёт разбирать адреса или U256 — тогда вынести общий модуль, не раньше.

**Р3. Циклическая зависимость модулей `rows` ↔ `l1_inflows`** (`rows.rs:14`, `l1_inflows/types.rs:8`, `l1_inflows/registry.rs:9`).
- Что не так: `rows` импортирует `l1_inflows::UnaccountedFlow` (ради `impl UnaccountedFlow { COLUMNS, write_tsv }`), а `l1_inflows` импортирует `rows::{EdgeKind, FundingEdge, GatewayStatus}`. Компилятор это допускает, но слой «строки таблиц» начинает зависеть от конкретного декодера. С приходом `swaps`/`transfers` в `rows` потянутся типы всех декодеров.
- Почему важно: `UnaccountedFlow` — не строка ClickHouse (таблицы нет, это сказано в doc), это выход сканера.
- Исправление: перенести `impl UnaccountedFlow { COLUMNS, write_tsv_header, write_tsv }` в `l1_inflows/types.rs`, а в `rows.rs` сделать `pub(crate) const NULL` и `pub(crate) struct HexOr` (+ `DecOr`). Тест `unaccounted_unknown_addr_is_null` — туда же. Тогда `rows` зависит только от alloy, `l1_inflows` → `rows` в одну сторону. Вывод не меняется.

**Р4. Устаревшие комментарии в тестах `rows.rs`** (`rows.rs:311`, `:329`).
- `:311`: «built as funding_edges_003, then EXCHANGE» — после 023 там два `RENAME`. Заменить на «then RENAME».
- `:329`: «The exact order is pinned by the golden test in tests/rows.rs» — теперь порядок закрепляет `funding_edges_column_order_in_sql_equals_columns` (тремя строками выше). Сослаться на него.
- Устаревший путь `crates/decoders/src/l1_inflows.rs` (теперь каталог): `data-model.md:86`, `chain-facts.md:138` (историческая запись, можно оставить), `sql/002_funding_edges_l1.sql:1` (не трогать: файл в журнале миграций, sha256 зафиксирован). Поправить `data-model.md:86` на `crates/decoders/src/l1_inflows/`.

**Р5. `decode_swap` не фильтрует эмиттер — сказать это в doc** (`swaps.rs:96`, поле `pool` `:46`).
- Что не так: v4 `Swap` с тем же topic0 от любого контракта даёт `SwapEvent::V4` с `pool` = этот контракт. Это правильно по правилам проекта (PoolManager `observed`, хардкодить нельзя), но doc поля `pool` говорит «v4: PoolManager address», как будто это гарантия.
- Исправление: в doc модуля одна фраза: «The emitter is not checked: filtering by the `verified` PoolManager / pool registry happens downstream (loader), like `registry: None` rows of `l1_inflows`.» Счётчик по эмиттерам — в задаче декодера `hood.swaps`, не сейчас.

**Р6. Пример: `Agg::overflowed` не печатается** (`examples/l1_inflows_scan.rs`, строки вывода `rows …`/`unaccounted`).
- Сумма при переполнении — `U256::MAX`, и в отчёте она выглядит как обычное число. По образцу `token_sum_overflow` печатать `(lower bound: overflowed)` только при `a.overflowed`, тогда обычный вывод не меняется.

**Р7. Позиционные аргументы замыкания `base`** (`swaps.rs:107-124`, 8 параметров, из них `amount0`/`amount1` одного типа `I256`).
- Перепутать порядок легко, тесты на знаки это поймают, но читать трудно. Альтернатива без новых абстракций: в каждой ветке собрать `PoolSwap { … }` литералом с общими полями из `let pos = (ctx.block, ctx.tx.index, log.index, ctx.tx.hash, ctx.tx.from, ctx.tx.to)` — или оставить как есть. Мелочь.

**Р8. Pedantic.** `required(v: &Option<String>)` → `Option<&str>` (`model.rs:205`, вызывать `required(tx.ticket_id.as_deref(), "ticketId")`). `main` примера 133 строки — после того как TSV ушёл в `rows`, остаток — печать отчёта; вынести `fn print_report(c: &Counters, registry: &GatewayRegistry, files: usize, range: (u64, u64))`. Остальное механика.

## Дубли

| что | где | расхождение | решение |
|---|---|---|---|
| грамматика hex-quantity | `decoders/src/model.rs:215` (U256, принимает `_`), `hood-core/src/hex.rs` (u64, строгая; 019), enricher — через hood-core после 019 | расходятся только на `_` | Р1 сводит к одной грамматике; переносить не нужно (Р2) |
| проверка «блок ↔ чеки» | `enricher/src/blocks.rs` (источник истины) и `model::parse_block_line` | намеренная защитная перепроверка, в doc модуля это сказано | оставить |
| цикл «прочитать `jsonl.zst` построчно → `parse_block_line`» | только `examples/l1_inflows_scan.rs:76-81` | дубля пока нет | когда появится загрузчик 1b — вынести итератор в библиотеку (`model::read_block_lines`), не раньше |
| topic0 v3/v4 | `events.rs` и `analytics/hourly_metrics.py` | совпадают | как в исходном ревью: допустимо как независимая проверка |

## Структура и разбиение

Раскладка совпадает с предложенной в исходном ревью; ни один файл не больше ~410 строк (`rows.rs` 407 с тестами, код ~230). Перекладывать нечего, кроме Р3.

`model.rs` как основа декодеров 1b: годится. `Block`/`Tx`/`Log`/`TxCtx` дают всё, что нужно `hood.swaps` (позиция, `trader`, `router`). Для `hood.blocks`/`hood.txs` понадобятся поля, которых сейчас нет (`timestamp`, `l1BlockNumber`, `baseFeePerGas`, `gasUsed`, `gasUsedForL1`, `effectiveGasPrice`, селектор и длина `input`). Они добавляются аддитивно в приватные `Raw*` и публичные `Block`/`Tx`, без изменения интерфейса — это нормальный путь, заранее не нужно.

## Что хорошо (не сломать при правках)

- Сырьё типизируется один раз на границе; все ошибки разбора несут `block N tx i 0x…`; строгие проверки согласованности прошли на 2 712 локальных блоках (по отчёту исполнителя) и закреплены 14 негативными тестами.
- `ArbFields` — enum с обязательными полями по типу tx: невалидное состояние «`0x68` без `ticketId`» непредставимо, повторный разбор (Р7 исходного ревью) исчез.
- Тесты SQL ↔ Rust читают `sql/*.sql` и сверяют последнее определение Enum8 и порядок колонок последнего `CREATE`; падают при `ALTER` после него. Это реальная связь, а не копия списка.
- `SwapDecode::Malformed` + счётчики + позиции: аудитор сможет доказать полноту `hood.swaps`. Нормализация знака v4 подтверждена независимо от литералов (сальдо `Transfer` пула).
- В `src/` только `verified` адреса (`addresses.rs`, с тестом-перечнем) и протокольная константа `ALIAS_OFFSET`; PoolManager (`observed`) — только как ожидаемое значение в `tests/swaps.rs`, помечен.

## Предполагается / не проверено

- **Проверено 2026-10-02 (этим ревью):** все команды из раздела «Линт»; поиск 40-символьных hex в `crates/decoders/src` (адреса — только `addresses.rs`, `arbitrum.rs` и тестовые синтетические); поведение `_` в `from_str_radix` — по исходнику ruint 1.20.1 в локальном реестре cargo (`src/string.rs:69-83`), не прогоном; содержимое `hood-core/src/hex.rs` в текущем дереве; статусы адресов в `contracts.md:34-36`; компиляция enricher против нового API.
- **По отчёту исполнителя, не перепроверял:** побайтная идентичность вывода `l1_inflows_scan` до/после (кроме `log_index` и `\N`); прогон строгой модели и `swaps::decode_block` на 2 712 блоках (`malformed = 0`); сверка знаков v3/v4 на данных; происхождение и sha256 фикстуры `swaps-blocks.jsonl`.
- **Предполагается:** что хуки Pons v2 / pools.trade не ломают связь `Swap` ↔ движение токенов (вопрос исполнителя к Михаилу/data-auditor; к архитектуре не относится). Нормализацию знака v4 должен принять data-auditor.

## Повторная проверка (2026-10-02)

Объём: правки из раздела «Правки после ревью» отчёта `docs/handoff/from-code/022-decoders-model-rows.md`, рабочее дерево.

**Вердикт: PASS.**

Линт и тесты (проверено 2026-10-02, Mac, офлайн):
- `cargo fmt --all -- --check`: exit 0;
- `cargo clippy -p decoders --all-targets -- -D warnings`: exit 0;
- `cargo test -p decoders`: 18 + 23 + 1 + 8 = 50, все прошли.

По пунктам (проверено чтением кода):
- Р1: в `model.rs` `quantity_u256` проверяет `is_ascii_hexdigit` до `from_str_radix` и объясняет это комментарием; в тесте есть `"0x_"`, `"0x1_0"`, `"0x 1"`. Теперь грамматика совпадает с `hood_core::hex::parse_quantity`. Вторую часть (обязательный `0x` у адресов и хэшей) отложили, потому что идёт аудит. Согласен: это рекомендация.
- Р2: TODO заменён комментарием «держать грамматику в синхроне, в hood-core не переносить».
- Р3: `rows.rs` больше не импортирует `l1_inflows`, наружу в пределах крейта открыты только `NULL` и `HexOr` (`pub(crate)`). `impl UnaccountedFlow` переехал в `l1_inflows/types.rs:173`, зависимость теперь одна: `l1_inflows` → `rows`.
- Р4: в `rows.rs:277` «then RENAME», в `:295` ссылка на тест порядка колонок, путь в `data-model.md:86` исправлен.
- Р5: в doc модуля `swaps.rs:9` написано, что эмиттер не проверяется.
- Р6: пример печатает переполнение только при выставленном `overflowed`. Мелочь: в `examples/l1_inflows_scan.rs:169` дважды сказано одно и то же («is a lower bound (lower bound: overflowed)»). Можно сократить, на вердикт не влияет.
- Р7 (замыкание `base`) и Р8 (pedantic) не делали. Это рекомендации, вердикт от них не зависит.

Не перепроверял, беру из отчёта исполнителя: побайтное совпадение вывода сканера до и после правок, `cargo test --workspace`.
