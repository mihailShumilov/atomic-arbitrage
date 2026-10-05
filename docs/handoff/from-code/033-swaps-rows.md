# 033 — Строки `hood.swaps` из декодера свопов v3/v4. Отчёт

- Дата: 2026-10-05.
- Исполнитель: indexer-engineer.
- Объём: `crates/decoders` и раздел про `hood.swaps` в `.claude/skills/hoodchain-mev/references/data-model.md`.
- Сеть: RPC 0, фид 0. `cargo clean` не запускал.
- Не коммитил, статус задачи не менял, ревьюеров не запускал (так велел координатор).
- Параллельно идёт 032 (крейт `loader`, правки `decoders/src/model.rs`). Их не трогал.

## Сделано

1. **`rows.rs`: тип строки `hood.swaps` и сопутствующие типы.**
   - `SwapRow`: 15 полей = колонки `hood.swaps` после 004.
   - `SwapRow::COLUMNS`, `write_tsv_header`, `write_tsv`.
   - Enum8 `Venue` (`pons_v1`=1 … `other`=9) и `Side` (`buy`=1, `sell`=2) с `ALL`/`as_str`/`enum8`; у `Venue` есть ещё `parse`.
   - `SwapPool`: адрес v3-пула или id v4-пула.
   - `F64`: запись Float64 в TSV. Обычное число — кратчайшая точная десятичная запись без экспоненты; особые значения — `nan`, `inf`, `-inf`.
   - `router = None` пишется как `''`.
   - Тесты:
     - порядок колонок последнего `CREATE TABLE hood.swaps*` равен `COLUMNS`, и после него нет `ALTER`;
     - Enum8 `venue` в `swaps` и в `tokens`, а также `side` совпадают с Rust;
     - TSV одной строки;
     - `F64`.
   - Помощники SQL-тестов обобщены (`last_create_table`). Тест `funding_edges` теперь использует их, его проверка не изменилась.
2. **`registry.rs` (новый): `RegistryStatus` вынесен из `l1_inflows`**, чтобы его использовали и реестр шлюзов, и реестр пулов.
   - Добавлен `parse`.
   - Путь `decoders::l1_inflows::RegistryStatus` сохранён реэкспортом.
3. **`pools.rs` (новый): метаданные пулов и токенов — вход вызывающего.**
   - `PoolRegistry` хранит `PoolRef`: `V3(pool)` или `V4{manager, id}`. В записи: `currency0 < currency1`, `venue`, `fee_pips` (v3), `hooks` (v4), статус, источник.
   - Пулы приходят из TSV (`parse_tsv`) и из v4 `Initialize`, но только от менеджеров, явно разрешённых через `allow_v4_manager(addr, status)`.
   - Итог каждого `Initialize` считается: `added` / `known` / `mismatch` / `foreign_emitter` / `malformed`.
   - Реестр пулов одновременно служит фильтром эмиттера.
   - **PoolManager не зашит** (он `observed`).
   - `TokenRegistry` хранит decimals и `quote_rank`. Встроены только нативный ETH (`address(0)`, 18 decimals) и L2 WETH (`verified`). Остальное — из TSV.
   - Проверки: дубликаты, decimals ≤ 77, у котируемой валюты обязательны decimals, статусы `todo`/`rejected` — ошибка.
4. **`swap_rows.rs` (новый): чистая функция `swap_row(PoolSwap, RowInputs) -> Result<MappedSwap, SkipReason>` и `SwapRowCounters`.**
   - Счётчики: по площадкам; по событию и причине пропуска; `price_unknown`, `fee_unknown`, `v4_fee_zero`, `v4_rows_with_hooks`, `v4_rows_hooks_unknown`.
   - Инвариант `is_balanced`: Σ свопов = Σ строк + Σ пропусков.
   - Правила по пунктам 2 задачи и аудиту 022:
     - **Знак.** Суммы — сторона пула (v4 уже нормализован). Токен ушёл из пула → `buy`. Суммы пишутся без знака.
     - **Нули (З3).** Нулевая сумма токена или котируемой валюты → строки нет (`zero_amount`). Суммы одного знака → `same_sign`. Цена никогда не делится на 0 и не бывает ±inf (decimals ≤ 77).
     - **`price`.** Котируемая валюта за целый токен. Если decimals токена неизвестны → `nan`.
     - **`fee_quote`** = `quote_amount × fee_pips / 1e6`. Для v4 `fee_pips` берётся из события, `fee = 0` пишется как 0; комиссия хука не учитывается. Для v3 — из реестра, иначе `nan`.
     - **Хуки v4 (З2).** Колонки нет. Документировано, что `Swap` идёт до `afterSwap`, поэтому суммы — нога AMM. Есть счётчики строк из пулов с хуками.
     - **quote.** Котируемая валюта — та, у которой есть ранг. Если котируемых две, берётся меньший ранг. Если их нет или ранги равны — строки нет.
     - **Статус.** Пул и котируемая валюта должны быть не ниже `min_status`. Decimals токена ниже `min_status` считаются неизвестными.
5. **`swaps.rs`.**
   - `PoolSwap::COLUMNS` + `write_tsv`: аудит-TSV всех полей свопа со знаками со стороны пула. Это не таблица ClickHouse, а вход для data-auditor.
   - `SwapEvent`: добавлены derive `Ord`/`Hash` и `as_str`.
   - Обновлён doc про фильтр эмиттера.
   - **`decode_swap`/`decode_block` не менялись.**
6. **Пример `examples/swaps_scan.rs`** — `data/blocks` + `data/samples` → TSV и счётчики по площадкам.
   - Флаги: `--pools`, `--tokens`, `--no-builtin-tokens`, `--v4-manager ADDR=observed|verified`, `--min-status` (по умолчанию `verified`), `--rows-out`, `--pool-swaps-out`.
   - `Initialize` блока применяется до свопов этого блока. Файлы нужно передавать по возрастанию блоков: пул из `Initialize` действует только на последующие свопы.
   - Если инвариант счётчиков нарушен, пример завершается ошибкой.
7. **Тесты на реальном блоке** — `tests/swap_rows.rs`, фикстура 74744924: tx 3 `0xa28443b7…1277` log 3 (v4) и tx 4 `0x7db0d5bf…b34d` log 8 (v3).
   - Golden TSV строк `hood.swaps`.
   - Golden аудит-TSV.
   - Стороны.
   - `min_status = verified` и пустой реестр: всё уходит в пропуски, счётчики сходятся.
   - Реестры в тесте — тестовые входы, а не факты реестра; в шапке теста это сказано.
8. **`data-model.md`**: раздел «Строки `hood.swaps` (задача 033)». В нём все правила выше, `nan`, хуки, перезагрузка при смене реестра, аудит-TSV.

## Проверено (2026-10-05, как)

- **Декодирование не изменилось.** `swaps_scan` на `data/blocks/*.jsonl.zst` (7 файлов) и `data/samples/hourly-20260804-20260930.jsonl.zst`:
  - 2 712 блоков, 127 824 лога;
  - v3 6 862, v4 6 668, malformed 0, **всего 13 530** — как в аудитах 022 и 027.
- **Аудит-TSV против сырья.** Независимый Python-скрипт читал сырые логи напрямую, без кода декодера:
  - знаковое декодирование слов data, отрицание сумм v4, `tx.from`/`tx.to`;
  - сравнение со всеми 13 530 строками `--pool-swaps-out` поле в поле;
  - **расхождений 0**.
  - Скрипт: `scratchpad/033/check.py`.
- **Golden-значения.** Float-ы фикстуры пересчитаны в Python: то же округление до f64 и `Decimal` с точностью 60 знаков. Совпали побайтно:
  - v4: `quote_amount` 0.22833501740261272, `price` 9.93987261419893e-10, `fee_quote` 0.0006850050522078381;
  - v3: 1756.302474 USDG, `price` 2657.1457501564205.
- **Знаки на фикстуре.**
  - v4: токен вошёл в пул (log 5 → PoolManager), WETH вышел (log 4) → `sell`.
  - v3: WETH вошёл в пул (log 7), USDG вышел (log 6) → `sell` WETH за USDG.
- **Строки на `data/`.**

  | Режим | Строк | Пропуски |
  |---|---|---|
  | по умолчанию (`verified`, без реестра пулов) | 0 | v3 `no_pool_meta` 6 862, v4 `no_pool_meta` 6 668; `Initialize` 30 = `foreign_emitter` |
  | `--v4-manager 0x8366…0951=observed`, `min_status` по умолчанию | 0 | то же, только 3 свопа v4 уходят в `below_min_status` |
  | то же + `--min-status observed` | 3 (`uni_v4`) | 13 527; все 30 `Initialize` добавлены |

  - В режиме `observed` все 3 строки — покупки за нативный ETH, `price = nan` (decimals мем-токенов неизвестны), хуки нулевые.
  - Знаки этих 3 строк сверены с аудит-TSV.
- **Сборка.** Workspace сейчас не собирается из-за 032: `crates/loader` есть в `members`, но в нём пока нет `src/lib.rs`/`main.rs`. Поэтому проверял в отдельном scratch-workspace: симлинк на `crates/decoders` + `sql`, тот же `Cargo.lock`, `CARGO_TARGET_DIR` в scratchpad. Общий target не трогал.
  - `cargo fmt --check` чисто.
  - `cargo clippy --all-targets -- -D warnings` чисто.
  - `cargo test`: 71 тест = lib 35 + `l1_inflows` 23 + `rows` 1 + `swap_rows` 4 + `swaps` 8. Все прошли.
  - `cargo doc -D warnings` чисто.
  - Тесты прошли и с текущими правками 032 в `model.rs`.
  - К концу работы 032 добавил исходники `loader`, и общий workspace снова собирается. В нём (2026-10-05, с текущими правками 032):
    - `cargo fmt -p decoders -- --check` чисто;
    - `cargo clippy --workspace --all-targets -- -D warnings` чисто;
    - `cargo build --workspace` чисто;
    - `cargo test --workspace` — все наборы ok, 0 failed.

## Предполагается (не проверено)

- WETH имеет 18 decimals (по исходнику aeWETH; в цепи не читалось). Это помечено в коде и в `data-model.md`.
- ClickHouse принимает `nan` во Float64 при вставке TSV. Проверить тестом загрузчика.
- `Swap.fee` v4 — полная комиссия свопа (LP + протокол). Есть одно наблюдение на данных: у пула `0x31a6…107b` `Initialize.fee` = 2500, а `Swap.fee` = 2899. Это совпадает с протокольной комиссией 400 pips: 400 + 2500 − 400·2500/1e6 = 2899. У пула `0xb53f…3371` 2500 = 2500. В `chain-facts.md` не вносил: один случай, вывод по формуле v4-core из памяти.
- Площадка пулов из `Initialize` (`uni_v4` без хуков, `other` с хуками) — заглушка до верификации хуков Pons v2 / pools.trade.
- Приближение `fee_quote` для продаж (комиссия во входящем токене пересчитана по цене свопа).

## Что не сделано / ограничения

- **Загрузка в ClickHouse не подключена.** 032 ещё в работе и не влит, а подключать загрузку самому мне не поручали. Точка подключения — doc в `crates/loader/src/blocks_file.rs`. Что нужно сделать:
  1. реализовать `TsvTable` для `SwapRow` по `SwapRow::COLUMNS`/`write_tsv`;
  2. в `load_file` построить `swaps::decode_block` → `swap_rows::swap_row` с `RowInputs`;
  3. добавить флаги загрузчика `--pools`/`--tokens`/`--v4-manager`/`--min-status` по образцу `swaps_scan`;
  4. `PoolRegistry` держать между файлами и обходить файлы по возрастанию блоков (пулы из `Initialize`);
  5. вставлять до `blocks` — маркера завершения;
  6. перед перезаливкой диапазона удалять его строки `hood.swaps`: ключ включает `token`, поэтому при смене реестра старые строки останутся;
  7. печатать `SwapRowCounters`.
- **Строк на реальных данных практически нет**, и причина — не код. В данных нет метаданных пулов:
  - 1 464 разных v3-пула и 3 027 v4-пулов;
  - `Initialize` есть только у 30 v4-пулов, на них приходится 3 свопа;
  - decimals мем-токенов неизвестны.
  - Без реестра каждый своп честно уходит в пропуск `no_pool_meta`.
- **Pons v1 / pools.trade** в `venue` попадут только из реестра пулов. Своих событий кривой Pons v2 нет — это отдельные декодеры после ABI (031).

## Вопросы к Cowork / Михаилу

1. **Источник метаданных пулов** — нужна отдельная задача.
   - v4: `Initialize` за всю историю PoolManager. Enricher в режиме `logs` уже фильтрует `TOPIC_INITIALIZE_V4` (`ALL_TOPIC0`), новый код не нужен, только вызовы `eth_getLogs`.
   - v3: `token0()`/`token1()`/`fee()` — ~3 `eth_call` на пул, ~1.5 тыс. пулов в выборке. Или `PoolCreated` фабрики после её верификации (031).
   - decimals токенов: `decimals()` по одному `eth_call` на токен.
   - Это RPC, то есть отдельное решение по бюджету.
2. **Ранг котируемых валют.** USDG (`todo`) предлагается ранжировать выше WETH: пул WETH/USDG котируется в USDG. USDG сейчас можно подать только через `--tokens` со статусом `observed`. Нужен `verified` с decimals (031).
3. **Хуки v4.** Нужна ли колонка или флаг в `hood.swaps` (миграция 005), или достаточно счётчика и документации? Сейчас колонки нет.
4. **Пул с комиссией 90 %.** На данных есть пул `0xadb0…d57c`: `Initialize.fee` = 900170 pips, хуков нет, своп в том же блоке 55389813, что и создание. Похоже на ловушку для снайперов; для исследования импульса это полезный класс. Такие свопы честно дают `fee_quote` ≈ 90 % от `quote_amount`.

## Файлы

- `crates/decoders/src/rows.rs` — `SwapRow`, `Venue`, `Side`, `SwapPool`, `F64`, SQL-тесты.
- `crates/decoders/src/registry.rs` — новый.
- `crates/decoders/src/pools.rs` — новый.
- `crates/decoders/src/swap_rows.rs` — новый.
- `crates/decoders/src/swaps.rs` — аудит-TSV, derive и doc.
- `crates/decoders/src/l1_inflows/registry.rs` — реэкспорт `RegistryStatus`.
- `crates/decoders/src/lib.rs`.
- `crates/decoders/examples/swaps_scan.rs` — новый.
- `crates/decoders/tests/swap_rows.rs` — новый.
- `.claude/skills/hoodchain-mev/references/data-model.md` — раздел «Строки `hood.swaps`».

После изменения декодера нужен прогон **data-auditor**: 13 530 свопов на `data/`, сверка аудит-TSV и строк с сырыми логами, знаки. Также нужен **architect-reviewer** (по задаче).
