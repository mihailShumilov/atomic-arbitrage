# 022 — Модель и строки декодеров. Отчёт

Исполнитель: indexer-engineer. Дата: 2026-10-02. База: HEAD `449576f`. Объём: только `crates/decoders`, плюс одна запись в `references/chain-facts.md`. Сеть и RPC не использовались (0 вызовов). `cargo clean` не запускал. Коммита нет, статус задачи не менял, ревьюеров не запускал: это делает основная сессия.

## Сделано

1. **`model.rs`** — один типизированный слой для всех декодеров.
   - Типы `Block`, `Tx` (tx объединён со своим чеком), `ArbFields`, `Log`, `TxCtx`; функция `parse_block_line`.
   - Serde-структуры со строковыми полями приватные. Они переводятся в типы один раз, это вариант (а) из ревью. Feature `serde` у alloy не включал.
   - `ArbFields` — enum `None | Deposit | Retry | SubmitRetryable`. Обязательные поля проверяются при разборе: у `0x68` нет `ticketId` или `maxRefund` → ошибка; у `0x69` нет `depositValue` или `retryValue` → ошибка.
   - Проверки согласованности:
     - `block.number` равен `number`;
     - число чеков равно числу tx;
     - `transactionHash` чека равен хэшу tx;
     - `transactionIndex` tx равен позиции в блоке;
     - `transactionIndex` и `blockHash` чека совпадают с tx и блоком (если поле есть);
     - `transactionHash` лога равен хэшу tx (если поле есть);
     - `logIndex` строго возрастает по блоку;
     - `status` обязателен и равен `0x0` или `0x1`, иначе ошибка (п. 4 задачи).
   - Все ошибки содержат контекст `block N tx i 0x<hash>` (Р6).
   - Здесь же собраны все hex-хелперы. `quantity_*` требует `0x` и хотя бы одну цифру: `""`, `"0x"` и строка без префикса — ошибка (п. 5). `parse_bytes` требует `0x`. Хелперы помечены `TODO: move to hood-core::hex after task 019`. 019 ещё в работе, поэтому `hood-core::hex` не использую.
   - `RawLog`, `DecodedLog`, `BlockLine`/`Rpc*` и `parse_line` удалены. Тип лога теперь один — `model::Log`.
2. **`rows.rs`**:
   - `EdgeKind` и `GatewayStatus` — `#[repr(i8)]`, дискриминанты равны значениям Enum8; есть `ALL`, `as_str()`, `enum8()`;
   - у `FundingEdge` поля `kind: EdgeKind` и `gateway_status: GatewayStatus` вместо `&'static str`;
   - `FundingEdge::COLUMNS`, `TX_LEVEL_LOG_INDEX = u32::MAX`, `log_index_column()`, `write_tsv_header`, `write_tsv`. `Some(u32::MAX)` в `log_index` — ошибка `InvalidData`, потому что не отличим от маркера;
   - у `UnaccountedFlow` тоже есть `COLUMNS`, `write_tsv_header`, `write_tsv`; неизвестный `addr` пишется как `\N`;
   - тесты:
     - golden-тест на всех 4 блоках фикстуры 017, включая 77312169 (`tests/rows.rs`);
     - «Enum8 в SQL = enum в Rust»: читает `sql/*.sql` по порядку имён, берёт **последнее** определение `kind Enum8(...)` и `gateway_status Enum8(...)` и сравнивает пары имя=значение точно;
     - слабая проверка, что каждая колонка `COLUMNS` определена в `sql/`;
     - проверки маркера и `\N`.
3. **`swaps.rs`**:
   - `decode_swap(ctx: TxCtx, log: &Log) -> SwapDecode { NotSwap | Swap(PoolSwap) | Malformed(SwapError) }`, где `SwapError` = `TopicCount(n) | Abi(alloy_sol_types::Error)`;
   - `decode_block(&Block) -> BlockSwaps { swaps, malformed, counters: SwapCounters { logs, v3, v4, malformed } }`;
   - в `PoolSwap` добавлены позиция (`block_number`, `tx_index`, `log_index`, `tx_hash`), `trader = tx.from`, `router = tx.to` и `event: V3 | V4`;
   - `print_topics` удалён.
   - **Изменение поведения, см. «Важно» ниже:** суммы v4 приводятся к знаку v3, то есть к стороне пула.
4. Отсутствующий `status` — ошибка (см. п. 1).
5. Суммы:
   - `Agg` складывает через `checked_add`. При переполнении сумма насыщается на `U256::MAX` и ставится флаг `overflowed`;
   - сумма токен-строк `0x68` считается через `try_fold(checked_add)`. Переполнение увеличивает счётчик `Counters::token_sum_overflow` и при `tx.value > 0` записывается как `gateway_eth_unexplained`;
   - в stdout сканера `token_sum_overflow` печатается, только если он не 0, поэтому отчёт обычного прогона не изменился.
6. `GatewayRegistry::new` теперь возвращает `Result<Self>`. Через одну функцию `push_unique` идут `new`, `extend` и `parse_tsv`. Тест: `observed` перед `verified` для того же адреса — ошибка.
7. Раскладка — ниже. Сделаны также Р1 (`retry` разбит на `gateway_token_rows` и чистую `matching_l2_transfer` с синтетическими тестами, включая mint `0x0 → получатель`), Р2 (`L1Tx::inflow`), Р3 (`Counters::merge` через деструктуризацию без `..`), Р8 (`HashSet` вместо `dedup`, негативные тесты) и Р9 (case-insensitive `.zst`, doc-комментарии у `pub`).

## Новая раскладка

```
crates/decoders/src/
  lib.rs            документация крейта, pub mod, реэкспорты (ALL_TOPIC0, TOPIC_*, v3/v4/erc20/token_bridge,
                    parse_block_line, Block, Log, Tx, TxCtx) — путь decoders::ALL_TOPIC0 для enricher сохранён
  model.rs          Block/Tx/ArbFields/Log/TxCtx, parse_block_line, все hex-хелперы        338 строк (с тестами)
  arbitrum.rs       TX_TYPE_*, ALIAS_OFFSET, alias/unalias + тест                            57
  addresses.rs      только verified: L2_WETH_GATEWAY, L2_WETH, список VERIFIED + тест          36
  events.rs         sol! v3/v4/erc20/token_bridge, TOPIC_*, ALL_TOPIC0, topics_are_canonical 102
  rows.rs           EdgeKind, GatewayStatus, FundingEdge (+TSV), TSV UnaccountedFlow + тесты 389 (код ~230)
  swaps.rs          PoolSwap, SwapDecode, decode_swap, decode_block, SwapCounters            229
  l1_inflows/mod.rs decode_block, deposit/submit/retry, gateway_token_rows, matching_l2_transfer 336
  l1_inflows/registry.rs  RegistryStatus, GatewayEntry, GatewayRegistry (+From для GatewayStatus) 211
  l1_inflows/types.rs     InflowKind, TokenInflow, L1Inflow(+funding_edge), UnaccountedKind/Flow, Agg, Counters, BlockL1 302
tests/  l1_inflows.rs (23), rows.rs (1, golden), swaps.rs (8); fixtures/swaps-blocks.jsonl (новая)
examples/l1_inflows_scan.rs  разбор аргументов + цикл; TSV — через rows
```

Trait `Decoder` и диспетчер не вводил, как и требует задача.

## Порядок колонок `funding_edges` для 023

Оставлен прежний порядок из `sql/001` + `sql/002`. Заголовок TSV сканера побайтно не изменился. Это `FundingEdge::COLUMNS`:

| # | колонка | тип ClickHouse | что пишет `write_tsv` |
|---|---|---|---|
| 1 | `block_number` | UInt64 | десятичное |
| 2 | `tx_index` | UInt32 | десятичное |
| 3 | `from_addr` | String | `0x…` в нижнем регистре |
| 4 | `to_addr` | String | `0x…` в нижнем регистре |
| 5 | `value_wei` | String | U256, десятичное |
| 6 | `kind` | Enum8('eth'=1,'weth'=2,'internal'=3,'l1_eth'=4,'l1_token'=5) | имя |
| 7 | `tx_hash` | String | `0x…` |
| 8 | `log_index` | **UInt32 DEFAULT 4294967295**, без NULL | индекс лога; `4294967295` для ребра уровня tx |
| 9 | `token` | String DEFAULT '' | `0x…` или пусто |
| 10 | `l1_token` | String DEFAULT '' | `0x…` или пусто |
| 11 | `gateway` | String DEFAULT '' | `0x…` или пусто |
| 12 | `gateway_status` | Enum8('none'=0,'observed'=1,'verified'=2) DEFAULT 'none' | имя |
| 13 | `l2_alias` | String DEFAULT '' | `0x…` |
| 14 | `tx_type` | UInt8 DEFAULT 0 | десятичное (100, 104) |
| 15 | `l1_request_id` | String DEFAULT '' | десятичное или пусто |
| 16 | `ticket_id` | String DEFAULT '' | `0x…` или пусто |

- В строках `funding_edges` `\N` не бывает: Nullable-колонок нет, тест это проверяет.
- Ключ из задачи 023 `ORDER BY (to_addr, block_number, tx_index, kind, log_index)` от порядка колонок не зависит.
- Предложение к 023:
  - создать таблицу в этом же порядке. Ревью В1 предлагало ставить `log_index`/`kind` сразу после `tx_index`, это допустимо, но тогда расходится с TSV;
  - загрузчик пусть всегда вставляет с явным списком: `INSERT INTO hood.funding_edges (<COLUMNS через запятую>) FORMAT TabSeparatedWithNames`. Так он не зависит от порядка колонок в таблице.
- Тест Enum8 берёт последнее определение `kind Enum8(` и `gateway_status Enum8(` по `sql/*.sql`. Если в 003 оба определения останутся однострочными, как в 001 и 002, тест автоматически сверится с 003. Если 003 изменит значения Enum8, тест упадёт: это и нужно.
- Проверки «порядок колонок в SQL = `COLUMNS`» нет: я проверяю только, что каждое имя где-то определено. Её стоит добавить в 023 вместе с `DESCRIBE`.

## Проверено и как (2026-10-02, Mac, офлайн)

- **Вывод сканера `l1_inflows_scan`.** Собрал пример на HEAD `449576f` до правок, сохранил бинарник и вывод в scratchpad. Затем прогнал новую сборку на тех же входах:
  - фикстура 017 с builtin-реестром и с `--no-builtin`: stdout, `--unaccounted-out` — **побайтно одинаковые**. `--edges-out` отличается только в 2 строках (`l1_eth` 77285531 и 77300695): колонка 8 `log_index` была пустой, стала `4294967295`. После замены пустого поля 8 на маркер файлы совпадают побайтно (`cmp`);
  - `data/blocks/*.zst` + `data/samples/hourly-20260804-20260930.jsonl.zst` (8 файлов, 2 712 блоков, 34 344 tx): stdout, edges, unaccounted — **побайтно одинаковые** (строк L1 там 0);
  - замена NULL → `\N` на этих данных вывод не меняет: ни в одной записи «не учтено» нет пустого `addr`.
- **Новые строгие проверки модели** прошли на всех 2 712 блоках и на 4 блоках фикстуры без единой ошибки: позиция tx, `blockHash` и `transactionIndex` чека, `transactionHash` лога, рост `logIndex`, обязательный `status`, префикс `0x`.
- **Знак сумм `Swap`.** Python-скрипт сверил знаки с `Transfer` того же чека, без кода декодера. Цифры записаны в `chain-facts.md` и в doc `PoolSwap`:
  - v3: положительная сумма = в пул (6 859 из 6 862);
  - v4 в сыром событии — наоборот: 3 338 против 229 для положительной суммы, 4 480 против 359 для отрицательной.

  На фикстуре 74744924 тест `signs_match_transfer_logs` проверяет нормализованные суммы по сальдо `Transfer` пула, независимо от литералов.
- **Декодер свопов на всех 2 712 блоках** (одноразовый проект в scratchpad, `parse_block_line` + `swaps::decode_block`): 127 824 лога, v3 = 6 862, v4 = 6 668, **malformed = 0**. Числа v3/v4 совпали с независимым подсчётом на Python.
- **alloy-sol-types 1.7.3 `decode_raw_log`** отвергает и 2, и 4 topics. Это было открытым предположением ревью (В5), теперь закреплено тестом `alloy_decode_raw_log_rejects_wrong_topic_count`. Явная проверка числа topics в `decode_swap` оставлена ради типизированной причины.
- **Команды:**
  - `cargo fmt -p decoders` и `cargo fmt --all -- --check` — чисто;
  - `cargo clippy --workspace --all-targets -- -D warnings` — чисто;
  - `cargo build --workspace` — ок;
  - `cargo test --workspace` — все прошли, 0 упавших. decoders: 17 unit + 23 + 1 + 8 интеграционных = 49.

  Workspace собран вместе с незакоммиченными правками 019 в том же дереве: на момент прогона они не мешали.
- **Адреса в `src/`** — только `L2_WETH_GATEWAY` и `L2_WETH` (`verified`, `addresses.rs`) и протокольная константа `ALIAS_OFFSET`. PoolManager `0x8366…0951` (`observed`) встречается только в `tests/swaps.rs` как ожидаемое значение фикстуры и помечен там.
- **Фикстура `tests/fixtures/swaps-blocks.jsonl`** — неизменённая строка блока 74744924 из `data/samples/hourly-20260804-20260930.jsonl.zst`: задача 010, публичный RPC, 2026-10-01, 18 167 Б, sha256 `70ac370e…a512`. `hash` совпадает с `index.tsv` выборки. tx 3 log 3 — v4, tx 4 log 8 — v3.

## Важно (изменения поведения и решения, которые стоит подтвердить)

1. **Знак v4 в `PoolSwap` инвертирован.** Раньше doc обещал «positive = into the pool» для обоих venue, а v4 отдавал знак свопера, то есть обратный. Теперь v4 приводится к стороне пула, как и v3. Потребителей у `decode_swap` не было, поэтому ломать нечего.
   - Предполагается, не проверено: что хуки Pons v2 / pools.trade с собственным учётом (custom accounting) не меняют связь между `Swap` и движением токенов. Это нужно проверить при декодере `hood.swaps` после верификации хуков.
2. **`UnaccountedFlow.addr = None` пишется как `\N`**, раньше было пусто. Таблицы для «не учтено» нет, и семантически это «неизвестно», то есть Nullable. На данных разницы нет.
3. **Модель строже прежнего кода.** Ошибками стали:
   - строка без `0x` в quantity;
   - `"0x"` как число;
   - чек без `logs`;
   - несовпадение позиции tx или `blockHash`;
   - убывающий `logIndex`.

   На всех локальных данных таких случаев 0. Если старые файлы другого происхождения дадут ошибку, это сигнал о битом сырье, а не о декодере.
4. В `l1_inflows::Counters` добавлено поле `token_sum_overflow`, в `Agg` — `overflowed`. Это публичные структуры, в крейте ими пользуется только пример.

## Что не сделано

- `LogsLine` для `logs-*.jsonl.zst` не делал: потребителя пока нет. Как строить `TxCtx` из таких файлов, написано в ревью В2. Это решение задачи загрузчика 1b.
- З2 аудитора 017 (контракт вне реестра испускает `DepositFinalized` и «забирает» ETH-строку) не трогал. Это вопрос семантики для data-auditor. Правка ляжет в `gateway_token_rows`, ветка `entry == None`.
- `Counters.token_rows_*.sum` по-прежнему складывает сырые единицы разных токенов (ревью Р4, вторая часть). Это задокументировано в поле, менять вывод не стал.
- Свой hex-разбор у enricher не трогал (вне объёма, 019).

## Вопросы

1. К 023: создавать таблицу в порядке `COLUMNS` выше? Я рекомендую так. И грузить с явным списком колонок.
2. К Михаилу и data-auditor: принять нормализацию знака v4 (п. «Важно» 1)?

После принятия нужен прогон **data-auditor** (декодеры изменены: модель, L1, свопы, строки TSV) и **architect-reviewer**.

## Правки после ревью

2026-10-02, по ревью architect-reviewer (`docs/reviews/022-decoders-model-rows-architect-reviewer.md`, PASS с замечаниями). Правки минимальные, потому что параллельно идёт аудит data-auditor.

1. **Р1. `model::quantity_u256` отвергает не-hex символы явно.** `U256::from_str_radix` в ruint пропускает `_`: `"0x_"` давало 0, `"0x1_0"` давало 16. В тест добавлены `"0x_"`, `"0x1_0"`, `"0x 1"`. Грамматика теперь совпадает с `hood_core::hex::parse_quantity`.
2. **Р3. Убрана взаимная ссылка `rows` ↔ `l1_inflows`.**
   - `impl UnaccountedFlow { COLUMNS, write_tsv_header, write_tsv }` и тест `unaccounted_unknown_addr_is_null` перенесены в `l1_inflows/types.rs`;
   - в `rows.rs` `NULL` и `HexOr` стали `pub(crate)`;
   - `rows` больше не импортирует `l1_inflows`, зависимость теперь в одну сторону: `l1_inflows` → `rows`.
3. **Р4. Устаревшие комментарии:**
   - в `rows.rs` «then EXCHANGE» заменено на «then RENAME»;
   - в `rows.rs` ссылка «golden test» заменена на тест `funding_edges_column_order_in_sql_equals_columns`;
   - в `references/data-model.md` путь исправлен на `crates/decoders/src/l1_inflows/`.
4. **Р5. В doc `swaps` записано, что эмиттер не проверяется.** Фильтр по `verified` PoolManager или реестру пулов делается ниже по конвейеру, в загрузчике. Doc поля `pool` уточнён: «expected to be the PoolManager, not verified here».
5. **Р6. Пример печатает `Agg::overflowed`.** Строка `overflow <name>: sum=… is a lower bound` выводится только при установленном флаге.
6. **Р2. TODO о переносе hex-хелперов заменён комментарием.** Новый текст: грамматика quantity должна совпадать с `hood_core::hex::parse_quantity`, держать их синхронно. Переноса в hood-core не будет: там нет alloy.

Проверено:
- `cargo fmt -p decoders`, `cargo fmt --all -- --check` — чисто;
- `cargo clippy -p decoders --all-targets -- -D warnings` — чисто;
- `cargo test --workspace` — все прошли, 0 упавших;
- сканер на фикстуре 017 (с builtin-реестром и с `--no-builtin`) и на 2 712 блоках `data/blocks` + `data/samples`: stdout, edges и unaccounted побайтно совпали с выводом до этих правок.

Не делал (вне просьбы координатора):
- Р1, вторая часть: требовать `0x` у адресов и хэшей. Это меняет строгость разбора во время аудита;
- Р7: позиционные аргументы замыкания `base` в `swaps.rs`.
