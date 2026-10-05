# 033 — архитектурное ревью (architect-reviewer)

- Дата: 2026-10-05.
- Объём: рабочее дерево против HEAD `8bf5a65`, без коммита, крейт `crates/decoders`:
  - `src/rows.rs`: `SwapRow`, `Venue`, `Side`, `SwapPool`, `F64`, SQL-тесты;
  - новые `src/registry.rs`, `src/pools.rs`, `src/swap_rows.rs`;
  - `src/swaps.rs`: аудит-TSV `PoolSwap`, `SwapEvent::as_str`;
  - `src/l1_inflows/registry.rs`: реэкспорт `RegistryStatus`;
  - `src/lib.rs`, `examples/swaps_scan.rs`, `tests/swap_rows.rs`.
- Контекст: `sql/004_types_keys.sql` (`hood.swaps_004`), `crates/loader` (032, точка подключения в `blocks_file.rs:18-26`), `data-model.md` (раздел «Строки `hood.swaps`»), `docs/reviews/full-2026-10-02-decoders-architect-reviewer.md` (предложенная раскладка).
- **Вердикт: PASS с замечаниями.** Блокирующих нет. Границы модулей совпадают с раскладкой ревью 2026-10-02. `swap_row` — чистая функция, правила знака, нулей, `nan` и «цена никогда не делится на 0» выполнены и покрыты тестами. Невыверенных адресов в коде нет. Два «важных» замечания касаются дублей и места оркестрации на один блок — их стоит закрыть при подключении `hood.swaps` к загрузчику.

## Линт: вывод команд (проверено 2026-10-05, Mac, `--offline`, сеть не использовалась)

| команда | итог |
|---|---|
| `cargo fmt --all -- --check` | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| `cargo test --workspace` | exit 0. decoders: lib 35, `l1_inflows` 23, `rows` 1, `swap_rows` 4, `swaps` 8 — все прошли. Остальной workspace тоже зелёный. Тестов, которым нужен ClickHouse, в decoders нет. Единственный такой тест в workspace — `loader/tests/clickhouse.rs` (`#[ignore]`), я его не запускал |
| clippy pedantic + nursery (рекомендательно) | по файлам 033: `too_many_lines` в `examples/swaps_scan.rs:92` (`main`, 101 строка); `float_cmp` в тестах `swap_rows.rs:350`, `rows.rs:668` (точные значения намеренны). По существу ничего |

## Блокирующее

Нет.

Проверка правила «адрес не `verified` в коде»:
- в `src/` новых адресов нет: `pools.rs` берёт только `addresses::L2_WETH` (`verified`) и `Address::ZERO` (нативный ETH по протоколу v4);
- PoolManager не зашит, его разрешает вызывающий через `allow_v4_manager` (`pools.rs:158`);
- адреса `observed`/`todo` встречаются только во входах теста `tests/swap_rows.rs:24-27`. Там они названы `POOL_MANAGER_OBSERVED`/`USDG_TODO`, а шапка теста оговаривает, что это не факты реестра. Это допустимо.

## Важное

**В1. [`crates/decoders/src/pools.rs:385-403`, `crates/decoders/src/l1_inflows/registry.rs:118-139`] Разбор TSV реестров продублирован.**
- Обе функции `parse_tsv` повторяют одно и то же:
  - пропуск пустых строк и `#`-комментариев, `trim`, `split('\t')` и `trim` колонок, номер строки с 1;
  - правило «`-` или пусто = `None`»;
  - разбор статуса с текстом «not allowed (verified|observed)».
- В `pools.rs` это уже вынесено в `tsv_rows`/`dash_or`/`parse_status`, а реестр шлюзов делает то же самое вручную.
- Почему это важно. Реестров будет больше (токены уже есть, дальше launchpad'ы после 031), а правило «`todo`/`rejected` — ошибка» — правило проекта. Оно должно жить в одной функции, а не в трёх копиях текста.
- Исправление: перенести три помощника в `src/registry.rs` (это общий модуль реестров, ради него `RegistryStatus` и выносили) как `pub(crate)`. В `GatewayRegistry::parse_tsv` заменить ручной цикл на `for (ln, cols) in tsv_rows(text)`, а `match cols[1]` — на `dash_or(cols[1], parse_addr)`. Поведение и тексты ошибок сохраняются (тест `l1_inflows` их проверит).

**В2. [`crates/decoders/examples/swaps_scan.rs:116-141`] Порядок обработки блока живёт в примере, а загрузчику он понадобится в том же виде.**
- Порядок такой: сначала `observe_log` для всех `Initialize` блока, потом `decode_block`, потом `swap_row` для каждого свопа, потом `SwapRowCounters::record`. Вне примера нигде не закреплено, что пулы из `Initialize` применяются до свопов того же блока и что файлы идут по возрастанию блоков. Точка подключения 032 (`loader/src/blocks_file.rs:18-26`) и отчёт 033 («Что не сделано», п. 2–4) описывают ровно эту последовательность для загрузчика. Если её скопировать, получатся два места, где порядок можно нарушить по-разному. Потребитель реальный и уже запланирован, так что это не абстракция «на вырост».
- Исправление (в задаче подключения `hood.swaps`, не раньше): чистая функция в `swap_rows.rs`.
  ```rust
  pub struct BlockSwapRows {
      pub rows: Vec<SwapRow>,
      pub pool_swaps: Vec<PoolSwap>,        // audit TSV input
      pub malformed: Vec<MalformedSwap>,
      pub decoded: SwapCounters,
      pub inits: BTreeMap<InitOutcome, u64>,
  }
  /// Applies the block's v4 `Initialize` logs to `pools` first, then maps its swaps.
  pub fn rows_of_block(b: &Block, pools: &mut PoolRegistry, tokens: &TokenRegistry,
                       min_status: RegistryStatus, counters: &mut SwapRowCounters) -> BlockSwapRows
  ```
  Пример и загрузчик вызывают её. `main` примера сократится примерно до 60 строк (заодно уйдёт pedantic `too_many_lines`), а тест «Initialize в том же блоке до свопа» можно будет написать один раз на эту функцию.

## Рекомендации

- **Р1. [`src/rows.rs:1-3`] Doc модуля противоречит 032.** Там написано «the single place that maps decoder output to ClickHouse columns», а строки `blocks`/`txs`/`logs`/`feed_gaps` и `MAY_BE_EMPTY` для `FundingEdge` лежат в `loader/src/rows.rs`. Либо выполнить В2 ревью 032 (перенести описание таблиц сюда — рекомендую), либо поправить doc. Заодно разбить первую строку doc: сейчас она ~170 символов, `rustfmt` комментарии не переносит.
- **Р2. `MAY_BE_EMPTY` для `SwapRow` (`router`)** по плану 032 окажется в loader. Лучше объявить его рядом с `SwapRow::COLUMNS` (константа `SwapRow::MAY_BE_EMPTY` или реализация `TsvTable` здесь, см. 032 В2). Тогда новая колонка `String DEFAULT ''` в миграции 005 потребует правки в одном файле.
- **Р3. [`src/swap_rows.rs:159-161`] `scaled` через `format!` и `parse`.** Это правильно: одно корректное округление, и тест на крайних значениях есть. Аллокация на каждый вызов для 13 530 свопов незаметна. Менять не нужно, но если загрузчик пойдёт по всей истории (миллионы свопов) и профиль покажет это место — заменить на `U256 → f64` с последующим делением на `10f64.powi(d)` там, где `raw < 2^53`, а для больших значений оставить текущий путь.
- **Р4. [`src/pools.rs:232-235`] `insert` с ошибкой превращается в `InitOutcome::Malformed`.** Сейчас это верно: дубликат исключён выше, `fee_pips = None`, `hooks = Some`, остаётся только порядок валют. Если в `insert` добавят новый инвариант, его нарушение молча попадёт в `malformed`. Комментарий из одной строки «only `currency0 >= currency1` can fail here» в этом месте сделает предположение явным.
- **Р5. [`examples/swaps_scan.rs:104`] Пример читает zstd потоково.** Если в файле без checksum (до задачи 026) испорчен байт, строки до места порчи уже будут учтены. Для офлайн-примера это допустимо: процесс завершится ошибкой, а data-auditor смотрит на код выхода. Если появится третий читатель `*.jsonl.zst` (сейчас это `loader::zst` и этот пример), вынести `zst::decode` из loader в `hood-core` (там нужна зависимость `zstd`, она уже есть в workspace).
- **Р6. Публичный API.** Почти всё оправдано: пример и интеграционный тест используют `pools::*`, `swap_rows::*`, `registry::RegistryStatus`. `MAX_FEE_PIPS` и `ETH_QUOTE_RANK` можно оставить `pub`: это документация правил. `F64`/`HexOr`/`DecOr` уже `pub(crate)`. Замечаний нет.

## Дубли

| где | состояние | куда |
|---|---|---|
| разбор TSV реестров: `pools.rs:385-403` и `l1_inflows/registry.rs:118-139` | логика одинаковая, текст продублирован | В1: `registry.rs` |
| `RegistryStatus`: был в `l1_inflows/registry.rs` | **устранён в этой задаче**: вынесен в `registry.rs`, старый путь сохранён реэкспортом, добавлен `parse` | — |
| разбор `sql/` в тестах: `decoders/src/rows.rs:440-499` и `loader/src/rows.rs:245-288` | уже разошлись (подробности — 032 В2) | 032 В2 |
| оркестрация блока «Initialize → decode → swap_row → counters» в `examples/swaps_scan.rs`, второй экземпляр запланирован в loader | пока один экземпляр | В2: `swap_rows::rows_of_block` |
| `HexOr`/`DecOr` для аудит-TSV (`swaps.rs:120-137`) | переиспользованы из `rows.rs`, не скопированы | — |

## Проверка правил `swap_row` (чтением кода и тестов, 2026-10-05)

- **Чистота.** Нет IO, времени, глобального состояния. На вход — `&PoolSwap` и `&RowInputs`, на выход — `Result<MappedSwap, SkipReason>`. Два `expect` (`swap_rows.rs:112`, `125`) охраняют инварианты: котируемая валюта найдена строкой выше, а `TokenRegistry::insert` требует decimals у котируемой валюты (`pools.rs:327`). Оба описаны в `# Panics`.
- **Знак.** Суммы со стороны пула. Токен ушёл из пула (`token_delta < 0`) → `Buy`. Суммы записываются через `unsigned_abs`. Тест `buy_and_sell_from_pool_side_signs` и золотые строки на блоке 74744924 (v3 и v4) это проверяют.
- **Нули и одинаковый знак.** Проверка `is_zero` для любой из сумм и `is_negative ==` идёт до любого деления (`swap_rows.rs:116-121`). Тест `zero_and_same_sign_amounts_have_no_row` включает случай `[0, 3709]` из ревью 022.
- **Цена никогда не делится на 0 и не бывает ±inf.** Знаменатель — `scaled(token_amount_raw ≠ 0, d ≤ 77)` ≥ 1e-77 > 0. Числитель ≤ `U256::MAX` ≈ 1.2e77, поэтому частное конечно. Граница `MAX_DECIMALS = 77` обоснована в doc (`pools.rs:28-30`) и проверена тестом `extreme_amounts_stay_finite`.
- **`nan`.** `price = nan` только если decimals токена неизвестны или ниже `min_status`. `fee_quote = nan` только если неизвестна комиссия. Это проверяет `unknown_fee_and_decimals_are_nan_not_zero`. В TSV пишется `nan`, а не `NaN` (`F64`, тест `f64_column_text`).
- **Статус.** Строка появляется только при `min(pool.status, quote.status) ≥ min_status`. У примера по умолчанию `verified`.
- **Учёт.** `SwapRowCounters::is_balanced` (Σ свопов = Σ строк + Σ пропусков). Пример падает, если баланс нарушен.
- **`SwapRow` ↔ SQL.** Порядок колонок равен последнему `CREATE TABLE hood.swaps*` и после него нет `ALTER` (`swaps_column_order_in_sql_equals_columns`). Enum8 `venue` в `swaps` и в `tokens` и `side` совпадают с Rust (`swaps_enum8_in_sql_equals_rust_enums`). Типы колонок тестом не сверяются: `Float64` и `String` проверит вставка в загрузчике. Приём `nan` подтверждён в 032 на временном контейнере.

## Структура и разбиение

Раскладка соответствует предложенной в ревью 2026-10-02 и разделяет слои:
- `swaps` — декодирование лога без метаданных;
- `pools` и `registry` — вход вызывающего, он же фильтр эмиттера;
- `swap_rows` — чистое отображение в строку;
- `rows` — строка таблицы и её TSV.

Размеры в норме: `pools.rs` 524 строки (около 120 из них тесты), `swap_rows.rs` 434, `rows.rs` 670 (около 235 — тесты). Разбивать не нужно. Если `rows.rs` получит строки сырых таблиц из loader (032 В2), его стоит превратить в каталог `rows/`, раскладка описана там.

## Что хорошо (не сломать при правках)

- Реестр пулов одновременно служит фильтром эмиттера, а PoolManager разрешается явно и со статусом (`allow_v4_manager`). Каждый `Initialize` получает исход (`added`/`known`/`mismatch`/`foreign_emitter`/`malformed`), поэтому ни один лог не пропадает молча.
- `swap_row` возвращает типизированную причину пропуска, а счётчики проверяют баланс. Это даёт data-auditor доказуемую полноту.
- `MAX_DECIMALS` выбран из арифметики `f64`, а не произвольно, и обоснование записано рядом с константой.
- Аудит-TSV `PoolSwap` хранит знаки со стороны пула отдельно от строк таблицы. Сверка с сырьём (13 530 строк, 0 расхождений по отчёту) не зависит от реестров.
- `decode_swap`/`decode_block` не менялись. Тесты `swaps` (8) прошли без правок.

## Предполагается / не проверено

- Сверку 13 530 строк аудит-TSV с сырыми логами и золотые значения float (Python `Decimal`) я не повторял. Это зона data-auditor, цифры взяты из отчёта 033.
- 18 decimals у WETH и трактовка `Swap.fee` в v4 как полной комиссии — предположения исполнителя, они помечены в коде и в `data-model.md`. Я их не проверял.
- Оценка производительности `scaled` (Р3) — по коду, без замеров.
