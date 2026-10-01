# 017 — Декодер входов с L1 (0x64, 0x68). Ревью data-auditor

date: 2026-10-01
reviewer: data-auditor
объект: `crates/decoders/src/l1_inflows.rs`, `crates/decoders/src/lib.rs` (модуль `token_bridge`), `crates/decoders/tests/l1_inflows.rs` + фикстура `tests/fixtures/l1-inflows-blocks.jsonl`, `crates/decoders/examples/l1_inflows_scan.rs`, `sql/002_funding_edges_l1.sql`, правки `references/{data-model,chain-facts,contracts}.md`; отчёт `docs/handoff/from-code/017-decoder-l1-inflows.md`. Рабочее дерево на коммите `5ad4d74` плюс незакоммиченные изменения 017.

**вердикт: PASS.** Все 6 проверок PASS. Критерий приёмки выполнен: на фикстурах входы совпадают с RPC и фидом (`to`, `value`, источник), двойного счёта `0x69`/`0x68` нет, `cargo test`/clippy чистые. Замечания З1–З5 не блокируют декодер. З1 нужно закрыть до задачи загрузчика.

## Как проверял

- **Объём:**
  - 12 блоков RPC: 8 из 014 (`scratchpad/014/rpc_raw`) и 4 из 017 (`scratchpad/017/rpc_raw`). L1-сообщения есть в 11 из них: 5 kind 12, 6 kind 9. Блок 77285519 (kind 13) взят как отрицательный контроль;
  - payload фида 014: `014/kind12.json` (22 kind 12, разбор indexer-engineer), `014-audit-scripts/audit_feed_kinds.json` (те же 22, мой разбор в ревью 014) и `014/kind9_13.json` (49 kind 9);
  - исходники Nitro v3.11.4: `017/tx_processor.go`, `retryable.go`, `util.go`. sha256 `tx_processor.go` и `util.go` совпадают с копиями 014 в `014/nitro/`;
  - выборки `data/samples/hourly-*.jsonl.zst` и `data/blocks/*.jsonl.zst`: 8 файлов, 2 712 блоков.
- **Скрипты** (stdlib + `pycryptodome` для keccak) лежат в `scratchpad/017-audit/`:
  - `audit017.py` — независимый разбор сырых ответов RPC. Без кода декодера строит ожидаемые строки и «не учтено», сверяет поля tx с payload фида и пересчитывает формулы Nitro;
  - `diff017.py` — сравнивает вывод Rust-сканера с ожиданием;
  - `rpc_fetch.py` — мои вызовы RPC, журнал в `rpc_own/calls.log`.

  Скрипты indexer-engineer (`rpc017.py`, crosscheck) не запускал. Сканер `l1_inflows_scan` запускал сам на собственной сборке блоков (`own-blocks.jsonl`), а не на `rpc-blocks.jsonl` исполнителя.
- **Подключений к фиду — 0.** На сервер не заходил. Код и данные только читал. Записан только этот файл.
- **RPC: 4 вызова.** Публичный `RPC_URL` из `.env`, с Mac, жёсткий лимит 4 в скрипте, пауза 1.2 с, User-Agent `hoodchain-auditor-017/1 (read-only)`, при 429/403 скрипт останавливается. Все 4 вернули HTTP 200, 429 не было. Вызовы:
  1. `eth_getBlockByNumber(77312169, true)`;
  2. `eth_getBlockReceipts(77312169)`;
  3. `eth_getBlockByNumber(77354060, true)`;
  4. `eth_getBlockReceipts(77354060)`.

  Поле `result` у всех четырёх **совпало** с сохранёнными ответами 014/017. Итог по задаче: 8 вызовов indexer-engineer и 4 моих, всего 12 из 20.

## 1. Строки на 12 блоках против RPC и фида — PASS

Проверено 2026-10-01 скриптом `audit017.py`: **248 условий, 0 расхождений.**

- **Сырьё.** Сборка из `rpc_raw/` совпадает с `017/rpc-blocks.jsonl` (12 из 12 блоков) и с фикстурой теста (4 из 4). Фикстура действительно содержит неизменённые ответы RPC.
- **kind 12, 5 из 5** (77285531, 77323647, 77346452, 77346458, 77354057). У `0x64` сверены `to`, `value` (до wei), `from` = `header.sender`, `requestId` = `header.requestId`, `blockHash`. Сверка шла с **обоими** разборами фида: indexer-engineer и моим из 014. Кроме того: `status` 1, логов нет, unalias(`from`) = `to` (депозит из EOA).
- **kind 9, 6 из 6** (77285521, 77300695, 77312161, 77312169, 77338854, 77354060). Поля `0x69` совпадают с payload фида: `from`, unalias, `requestId`, `retryTo`, `retryValue` = `callvalue`, `depositValue`, `maxSubmissionFee`, `refundTo` = `feeRefundAddr`, `beneficiary`, `gas` = `gasLimit`, `maxFeePerGas`, `l1BaseFee` = `baseFeeL1`, `value` = 0. У `0x68`: `from` = `0x69.from`, `to` = `retryTo`, `value` = `callvalue`, `refundTo` = `feeRefundAddr`, `ticketId` = hash `0x69`, `status` 1.
- **Вывод декодера против моего ожидания** (`diff017.py`):
  - строк 10 из 10, ключ `(block, tx_index, log_index)` уникален;
  - по каждой строке совпали `from_addr`, `to_addr`, `value_wei`, `kind`, `tx_hash`, `token`, `l1_token`, `gateway`, `gateway_status`, `tx_type`, `l1_request_id`, `ticket_id`;
  - записей «не учтено» 12 из 12, адрес и сумма совпали до wei;
  - лишних строк нет. Мой вывод сканера побайтно (после сортировки) совпал с `017/edges-rpc.tsv` и `017/unacc-rpc.tsv` исполнителя.

| что | n | сумма |
|---|---|---|
| `l1_eth` от `0x64` | 5 | 410 296 683 333 992 970 wei (0.410297 ETH) |
| `l1_eth` от `0x68` | 4 | 10 880 290 840 551 855 489 wei (10.880291 ETH): 0.006661, 0.355840, 0.009990, 10.507800 |
| `l1_token` (WETH, `verified`) | 1 | 104 126 314 999 636 103 765 (18 decimals = 104.126315 WETH) |
| `0x68` с `value` 0 (77285521) | 1 | строки нет, `retry_zero_value` |

- **`0x69` не даёт строк** ни в одном из 6 блоков. В сканере нет строк с `tx_type` 105. Блок kind 13 (77285519) строк тоже не дал.
- **Токен-строка, блок 77312169, tx `0x8a448fd9…5d4a`:**
  - `DepositFinalized` (log 4) испущен `tx.to` = L2 WETH gateway `0x1d18…f055`;
  - `l1Token` = `0xc02a…6cc2` (WETH на L1), `from` = `to` = `0x07ae…0e67`, `amount` = 104 126 314 999 636 103 765;
  - логи `Transfer` L2 WETH `0x0bd7…ad73`: log 2 — `0x0 → шлюз`, log 3 — `шлюз → 0x07ae…0e67`, обе на ту же сумму. Декодер взял log 3, `token` = L2 WETH;
  - `tx.value` = `amount`, поэтому `gateway_eth_unexplained` = 0;
  - **ETH-строки на шлюз нет**, ни одна строка не идёт на `0x1d18…f055`;
  - `from_addr` = L1-депозитор `0x07ae…0e67`, а не L1-шлюз (unalias(`0x08f2…e02c`) = `0xf7e1…cf1b`);
  - с `--no-builtin` строка та же, но `gateway_status` = `none`.

## 2. Формула `submit_fee_refund` и счётчики «не учтено» — PASS

Проверено 2026-10-01 по `tx_processor.go` (ветка `ArbitrumSubmitRetryableTx`):
- `availableRefund` = `depositValue`, затем `takeFunds(retryValue)`;
- из пула навсегда уходят только две суммы, обе переводятся на `FeeRefundAddr`: `submissionFeeRefund = takeFunds(maxSubmissionFee − submissionFee)` и `gasPriceRefund = takeFunds((GasFeeCap − baseFee) × Gas)`;
- `withheldSubmissionFee` и `withheldGasFunds` возвращаются в пул;
- остаток пула передаётся в `retryable.MakeTx(…, maxRefund = availableRefund, submissionFeeRefund = submissionFee)`.

Отсюда возврат этапа `0x69` = `depositValue − retryValue − 0x68.maxRefund`, точно. `takeFunds` ограничивает изъятие размером пула, и тождество верно даже при этом ограничении. Единственное исключение — неудачный `transfer` с `log.Error` («should never happen»).

Пересчёт на 6 из 6 блоков kind 9:
- (`maxSubmissionFee` − `l1BaseFee` × (1400 + 6 × `dataLen`)) + (`maxFeePerGas` − `baseFeePerGas` блока) × `gas` = `depositValue − retryValue − maxRefund` — совпало до wei. `dataLen` взят из фида;
- `l1BaseFee × (1400 + 6·dataLen)` = `0x68.submissionFeeRefund` — 6 из 6 (`retryable.go` L394-398);
- `maxRefund` и `submissionFeeRefund` из `0x68` присутствуют в данных лога `RedeemScheduled` у `0x69` — 6 из 6;
- `0x68.gasPrice` = `baseFee` блока, `0x68.gas` = `0x69.gas` — 6 из 6.

Суммы: `submit_fee_refund` 6 шт., 13 525 634 452 768 040 wei (0.013526 ETH). Это совпадает с отчётом и тестами: 77285521 — 492 192 227 999 664, 77300695 — 5 174 074 212 200, 77312169 — 12 996 694 115 787 776.

Верхние границы описаны честно:
- `redeem_refund_upper_bound` = `maxRefund`. В ветке `ArbitrumRetryTx`/EndTxHook каждый `refund()` идёт через `takeFunds(maxRefund, …)`, а излишек сверх `maxRefund` уходит на `inner.From` (alias), а не на `refundTo`. Значит, `maxRefund` — верхняя граница прихода на `refundTo`;
- `submit_no_redeem_upper_bound` = `depositValue − retryValue`. В ветке без авто-redeem на `FeeRefundAddr` уходят `submissionFeeRefund` + `takeFunds(maxGasCost)` ≤ пул.

В `data-model.md` каждый вид «не учтено» описан с пометкой «по коду, не наблюдали» там, где наблюдений не было. Цифры отчёта по фиду пересчитал из `kind9_13.json`, все совпали: 44 сообщения с `callvalue` 0 от одного L1-отправителя `0xb3e3…c1e3`, 41 разный `feeRefundAddr`, `depositValue − callvalue` = 0.0221 ETH (по всем 49 — 0.0362 ETH). У 77312161 и 77354060 `feeRefundAddr` = alias(`retryTo`) — пересчитал оба.

## 3. Адреса в коде — PASS

- В `src/` и `examples/` три адресных литерала:
  - `L2_WETH_GATEWAY` `0x1d187c3e…f055` и `L2_WETH` `0x0bd7d308…ad73` — в `contracts.md` на коммите `c9b6870` оба `verified` (Михаил, 2026-10-01), адреса совпадают побайтно;
  - `ALIAS_OFFSET` `0x1111…1111` — константа протокола, совпадает с `util.go` L36 (`AddressAliasOffset`). `unalias`/`alias` соответствуют `InverseRemapL1Address`/`RemapL1Address` (L203-217), переход через 2^160 покрыт тестом.
- Остальные литералы в `src/` — синтетика unit-тестов (`0x…aa`, `0x…01`, `0xff…ff`) и адреса tx 77285531 в тесте alias.
- `GatewayRegistry::builtin()` содержит одну запись: шлюз → L2 WETH, `Verified`.
- Router `0x1e324b93…1b89` в коде нет. L1 WETH gateway `0xf7e1…cf1b` и L1 WETH `0xc02a…6cc2` есть только в `tests/l1_inflows.rs` как ожидаемые значения, с комментарием `observed`.
- Реестр из TSV отклоняет `todo`/`rejected` и дубли. `extend` не позволяет перезаписать встроенную запись.

## 4. Миграция 002 против `FundingEdge` — PASS (с замечанием З1)

| колонка | тип в CH | поле `FundingEdge` / вывод сканера | итог |
|---|---|---|---|
| `block_number`, `tx_index` | UInt64, UInt32 (001) | u64, u32 | ок |
| `from_addr`, `to_addr` | String (001) | Address, `{:#x}` (нижний регистр, 42 символа) | ок |
| `value_wei` | String (001) | U256 в десятичной записи | ок |
| `kind` | Enum8(…, `'l1_eth'=4`, `'l1_token'=5`) | `"l1_eth"` / `"l1_token"` | ок; значения 1–3 из 001 сохранены |
| `tx_hash` | String | B256 `{:#x}` | ок |
| `log_index` | Nullable(UInt32) | Option<u32> | ок (см. З3) |
| `token`, `l1_token`, `gateway` | String DEFAULT '' | Option<Address> → '' | ок |
| `gateway_status` | Enum8(`none`=0, `observed`=1, `verified`=2) | `"none"`/`"observed"`/`"verified"` | ок |
| `l2_alias` | String | Address `{:#x}` | ок |
| `tx_type` | UInt8 | 100 / 104 | ок |
| `l1_request_id` | String (десятичная запись) | Option<U256>, Display = десятичная (340191 = `0x530df`) | ок |
| `ticket_id` | String | Option<B256> | ок |

Порядок колонок в TSV сканера совпадает с порядком таблицы после `ADD COLUMN`. Семантика `from_addr` = L1-адрес (Ethereum) одинаково описана в `FundingEdge`, в комментарии миграции и в `data-model.md`.

Схлопывание ключа: см. **З1**. Синтаксис миграции в ClickHouse не проверял. ClickHouse на Mac не запускал, это запрещено задачей. Оценка опирается на документацию: `MODIFY COLUMN` с расширением Enum8 и `ADD COLUMN IF NOT EXISTS` — штатные операции.

## 5. topic0 token bridge — PASS

- Сигнатуры и `indexed` в `token_bridge` совпадают с `abi/arbitrum-token-bridge/L2WethGateway.min.json`:
  - `DepositFinalized`: 3 indexed, `amount` не indexed;
  - `WithdrawalInitiated`: indexed `_from`, `_to`, `_l2ToL1Id`.
- keccak пересчитал сам: `0xc7f2e9c5…fcb2` и `0x3073a74e…7d73`. Оба значения закреплены в `topics_are_canonical` (b256 + keccak), источник указан (token-bridge-contracts `0746a713…`). `DepositFinalized` совпал с log 4 в 77312169.
- **Что их нет в `ALL_TOPIC0` — правильно.** `ALL_TOPIC0` — фильтр `eth_getLogs` режима `logs` у enricher (`crates/enricher/src/logs.rs:35`, тесты `enricher/tests/logs.rs:30`, `logs.rs:202`). Расширение фильтра изменило бы объём и стоимость выгрузки, а декодеру это не помогает: у `0x64` логов нет, а для `0x68` нужны тип tx, `status`, `value` и `ticketId`, то есть полные блоки. `crates/enricher` не менялся. Комментарий у `ALL_TOPIC0` объясняет исключение. Без него первая строка «Every topic0 the decoders understand» была бы неточной.

## 6. Сборка — PASS

2026-10-01, на Mac:
- `cargo test --workspace` — 110 тестов, 0 упавших. Из них 17 по 017: 12 интеграционных и 5 unit;
- `cargo clippy --workspace --all-targets -- -D warnings` — чисто;
- повторный офлайн-прогон сканера по `data/samples` и `data/blocks`: 2 712 блоков, 34 344 tx, `0x64`/`0x68`/`0x69` — 0. Вывод побайтно совпал с `017/scan-data.out`;
- в конце выполнен `cargo clean`.

## Замечания

- **З1. Ключ `funding_edges` (средняя важность для загрузчика, декодер не блокирует).** `ORDER BY (to_addr, block_number, tx_index)` в ReplacingMergeTree не содержит ни `kind`, ни `log_index`.
  - Два `DepositFinalized` одному получателю в одной tx на практике почти невозможны. Декодер принимает событие только от `tx.to`, а штатный `finalizeInboundTransfer` испускает одно событие. Внутри l1-строк риск низкий.
  - Существеннее **столкновение между видами**. Будущий декодер рёбер `weth`/`eth` для той же tx `0x68` (например, `Transfer` WETH `шлюз → получатель` в 77312169) даст тот же ключ `(0x07ae…, 77312169, 2)`. Тогда ReplacingMergeTree оставит одну строку из двух — ту, что вставлена последней. Это тихая потеря ребра, и какого именно, зависит от порядка загрузки.
  - Таблица пуста, загрузчика нет, поэтому сейчас ничего не теряется. **До задачи загрузчика** нужно пересоздать таблицу с ключом, который включает `kind` и индекс лога (не Nullable, например `log_index UInt32 DEFAULT 0` или отдельный `edge_seq`). Либо явно решить, что рёбра разных видов живут в разных таблицах.
- **З2. «Шлюз» вне реестра забирает ETH-строку.** Любой контракт, вызванный через retryable, может испустить событие с topic0 `DepositFinalized`. Тогда вместо `l1_eth` на `tx.to` декодер выдаёт `l1_token` с `gateway_status = none`, а такие строки по умолчанию отбрасываются. Реальный приход ETH на контракт `to` пропадает из графа. Запись «не учтено» появляется только при `tx.value ≠ amount` (`gateway_eth_unexplained`), иначе остаётся лишь счётчик `token_rows_unregistered`. В данных не наблюдалось (0 случаев). Рекомендация: при `registry = None` дополнительно выдавать `l1_eth` на `tx.to` или запись «не учтено» нового вида.
- **З3. NULL в TSV.** Сканер пишет пустую строку для `log_index` = None. В TSV ClickHouse NULL записывается как `\N`, а пустое поле в `Nullable(UInt32)` зависит от настроек (`input_format_tsv_empty_as_default`). Это вопрос к загрузчику, к декодеру не относится.
- **З4. Устаревшая пометка.** `abi/arbitrum-token-bridge/SOURCE.md` до сих пор пишет «registry: … status `observed`» для шлюза и L2 WETH. После `c9b6870` оба `verified`. Косметика.
- **З5. Возвраты retryable как канал финансирования (решение за Михаилом).** Поддерживаю вопрос 1 отчёта. `submit_fee_refund` точен по коду и подтверждён на 6 из 6 блоков. По фиду это единственный приход ETH на 41 L2-адрес за 2 ч. Если сделать его видом ребра, учесть: у retryable от контрактов (`0x0038…59cd`) получатель возврата — alias(`retryTo`), то есть алиас-адрес, у которого нет ключа на L2.

## Проверено и предполагается

- **Проверено (2026-10-01):** всё в п. 1–6. Метод:
  - собственный разбор 12 сохранённых блоков, сверенный с двумя независимыми разборами фида 014;
  - 2 блока (4 ответа) перезапрошены мной, `result` совпал;
  - построчное чтение `tx_processor.go`, `retryable.go`, `util.go`;
  - запуск сканера на собственной сборке блоков;
  - `cargo test`/clippy.
- **Предполагается (не проверено на данных):**
  - поведение при `status ≠ 1`: отфильтрованный `0x64`/`0x69`, неудачный авто-redeem, ручной redeem в другом блоке, отмена или истечение тикета. Покрыто только синтетическими мутациями фикстур и чтением кода;
  - kind 7, стандартный ERC-20 шлюз (mint `0x0 → to`), `WithdrawalInitiated` — не наблюдались;
  - соответствие файлов Nitro тегу v3.11.4 (как в ревью 014);
  - синтаксис миграции 002 в живом ClickHouse;
  - репрезентативность: L1-сообщений 11, все из окна 01.10 09:57–12:00Z. В выборках августа–сентября их 0, счётчиков на большом объёме нет (вопрос 2 отчёта).
