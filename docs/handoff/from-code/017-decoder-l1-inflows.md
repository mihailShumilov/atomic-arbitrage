# 017 — Декодер входов с L1 (0x64, 0x68) для графа финансирования (отчёт)

Исполнитель: indexer-engineer. Дата: 2026-10-01. Коммитов нет. Статус задачи в `to-code/017` не менял (`in-progress`), его выставит координатор после data-auditor. Подключений к фиду — 0. Вызовов RPC — 8 из 20. recorder, `deploy/` и формат сырья не трогал. В ClickHouse ничего не загружал.

## Сделано

1. **Декодер `crates/decoders/src/l1_inflows.rs`.** Вход — строка `blocks-*.jsonl.zst` (`{"number","block","receipts"}`). Выход — строки входов `L1Inflow` (→ `funding_edges` через `funding_edge()`), записи `UnaccountedFlow` и счётчики `Counters`. Ключ — тип tx, а не `header.kind`.
   - `0x64` со `status=1` → `eth_deposit`: `to`, `value`, `l1_request_id`; источник = unalias(`from`).
   - `0x68` со `status=1` и `value` > 0 → `retry_eth`: `to` (= `retryTo`), `value`, `ticket_id`; источник = unalias(`from`).
   - `0x68` со `status=1`, если в его логах есть `DepositFinalized`, испущенный самим `tx.to`, → `bridged_token`. Получатель, сумма, `l1Token` и L1-депозитор берутся из события, L2-токен — эмиттер совпадающего `Transfer(шлюз|0x0 → to, amount)`. ETH-строки на шлюз нет. Ключ здесь — событие и ABI, адрес шлюза для этого не нужен.
   - `0x69` строк не даёт никогда, поэтому двойного счёта с `0x68` нет.
   - Реестр шлюзов `GatewayRegistry` передаётся на вход. `GatewayRegistry::builtin()` содержит только `verified`: L2 WETH gateway `0x1d18…f055` → L2 WETH `0x0bd7…ad73` (обе записи Михаил перевёл в `verified` 2026-10-01, коммит `c9b6870`, по сообщению координатора; в `contracts.md` сверил). Router и L1 WETH gateway остались `observed` и в код не попали. Дополнительные записи грузятся TSV (`gateway \t l2_token|- \t verified|observed`). На `todo`/`rejected` и на дубль шлюза — ошибка. Строка от шлюза вне реестра всё равно выдаётся, но с `registry = None` (в `funding_edges` это `gateway_status='none'`), её нужно отбрасывать.
   - «Не учтено» — записи и счётчики, в `funding_edges` не попадают: `submit_fee_refund`, `submit_no_redeem_upper_bound`, `redeem_refund_upper_bound`, `deposit_not_succeeded` (FilteredFundsRecipient / нет `to`), `submit_not_succeeded`, `retry_failed`, `gateway_eth_unexplained`, `retry_no_to`. Смысл каждого — в `data-model.md`.
   - Битый вход (receipts не совпадают с tx по числу или хэшу, плохой hex) даёт ошибку, а не тихий пропуск.
2. **topic0 token bridge** в `crates/decoders/src/lib.rs`: модуль `token_bridge` (`DepositFinalized`, `WithdrawalInitiated`), константы `TOPIC_DEPOSIT_FINALIZED` и `TOPIC_WITHDRAWAL_INITIATED`. Обе проверяются в `topics_are_canonical` (b256 + keccak сигнатуры), в комментарии указан источник: token-bridge-contracts `0746a71321cdb2d6df6b15158c7ecbb9ece84b12`. В `ALL_TOPIC0` их **нет** намеренно, комментарий в коде объясняет почему: фильтр режима `logs` у enricher не расширяется, а декодеру нужны полные блоки.
3. **Схема:** `sql/002_funding_edges_l1.sql`. В `kind` добавлены `'l1_eth'=4` и `'l1_token'=5`. Новые колонки: `tx_hash`, `log_index`, `token`, `l1_token`, `gateway`, `gateway_status`, `l2_alias`, `tx_type`, `l1_request_id`, `ticket_id`. Ключ сортировки не менял.
4. **Сканер** `crates/decoders/examples/l1_inflows_scan.rs`. Работает офлайн: считает счётчики, по флагам пишет TSV рёбер (`--edges-out`) и неучтённого (`--unaccounted-out`). Флаги `--gateways` и `--no-builtin` управляют реестром.
5. **Документация:** `data-model.md` (абзац о декодере и о неучтённом); `chain-facts.md` (3 новых факта); `contracts.md` (два topic0 в таблице, наблюдение 016 «вне тестов» помечено закрытым).

## Тесты

`cargo test --workspace` — 110 тестов, всё ok. `cargo clippy --workspace --all-targets -- -D warnings` — чисто.

Фикстура `crates/decoders/tests/fixtures/l1-inflows-blocks.jsonl` (4 строки, 38 КБ) собрана из сырых ответов RPC без изменений:

| блок | что | tx |
|---|---|---|
| 77285531 | kind 12, `0x64` | `0x3f8e47b0…b522c1` |
| 77300695 | kind 9 без шлюза | `0x69` `0x275246a0…69a2`, `0x68` `0x41955de7…2011` |
| 77312169 | kind 9 через шлюз WETH | `0x69` `0x933699df…4207`, `0x68` `0x8a448fd9…5d4a` |
| 77285521 | kind 9 с `callvalue` 0 (как 44 из 49 kind 9) | `0x69` `0xa47ab092…aa0b`, `0x68` `0x69d849ea…e7cd` |

Тесты в `crates/decoders/tests/l1_inflows.rs` (12):
- на реальных данных: kind 12; kind 9 без шлюза (значения возвратов); kind 9 через шлюз, встроенный реестр → `verified`; тот же блок с пустым реестром → флаг `none`; `callvalue` 0 → строки нет, но есть `submit_fee_refund`; нет двойного счёта по 4 блокам;
- синтетические, на мутациях реальной фикстуры: запись `observed` в реестре; несовпадение токена с реестром; `0x64` со `status=0`; `0x68` со `status=0`; `0x69` без `0x68` в блоке; несовпадающие receipts.

Unit-тесты (5): alias/unalias с переходом через 2^160, разбор TSV реестра, встроенный реестр.

## Проверено (2026-10-01, как)

- **Декодер против фида, 11 из 11.** Прогнал сканер по 12 блокам RPC (8 из 014 + 4 новых) и сравнил с payload фида из разбора 014 (`kind12.json`, `kind9_13.json`). Скрипт и вывод — `scratchpad/017/crosscheck.out`.
  - kind 12, 5 из 5 (77285531, 77323647, 77346452, 77346458, 77354057): `to`, `value` до wei, `requestId`, источник = unalias(`from`) = `to`.
  - kind 9, 6 из 6 (77285521, 77300695, 77312161, 77312169, 77338854, 77354060): `retryTo`, `callvalue` до wei, `feeRefundAddr` = адрес в `submit_fee_refund`. У 77312169 строка `l1_token`, получатель `0x07ae…0e67`, 104.126314999636103765 WETH. Строк у `0x69` — 0.
- **Возврат этапа `0x69` считается точно.** По коду Nitro v3.11.4 (`tx_processor.go` L279-536, `retryable.go` L394-398; скачал с raw.githubusercontent.com) он равен `depositValue − retryValue − 0x68.maxRefund` = `(maxSubmissionFee − submissionFee) + (maxFeePerGas − baseFee) × gas`. Сверил независимым пересчётом на 6 из 6 блоков kind 9. Там же `submissionFee` = `l1BaseFee × (1400 + 6 × len(retryData))` = `0x68.submissionFeeRefund`, 6 из 6.
- **RPC: 8 вызовов** (`eth_getBlockByNumber(full)` + `eth_getBlockReceipts` для 77285521, 77312161, 77338854, 77354060). Публичный `RPC_URL` из `.env`, с Mac, последовательно, пауза 1.2 с, UA `hoodchain-indexer-017/1 (read-only)`, жёсткий лимит 20 со счётчиком. Все ответы HTTP 200, 429 не было. Сырьё — `scratchpad/017/rpc_raw/`. Зачем понадобились вызовы: в фикстурах 014 не было kind 9 с `callvalue` 0, а это 44 из 49 kind 9; плюс хотелось сверить все 5 kind 9 с `callvalue` > 0.

## Счётчики по выборкам

**`data/samples/hourly-20260804-20260930.jsonl.zst` + `data/blocks/*.jsonl.zst`** — 8 файлов, 2 712 блоков (713002..76893221), 34 344 tx:
- типы tx: `0x00`=4070, `0x01`=34, `0x02`=27446, `0x04`=66, `0x6a`=2728;
- `0x64`/`0x68`/`0x69` — **0**: строк 0, неучтённого 0.

Это не ошибка декодера. В выборках нет ни одной L1-tx. В фиде 01.10 kind 9+12 — около 71 на 73 112 блоков, то есть ~0.1 %. На 2 712 блоках при таком темпе ожидалось бы ~2.6, вероятность увидеть 0 — около 7 %. Темп в августе–сентябре мог быть ниже (предполагается).

**12 блоков RPC (014 + 017):**

| что | n | сумма |
|---|---|---|
| `eth_deposit` (`0x64`) | 5 | 0.410297 ETH |
| `retry_eth` (`0x68`) | 4 | 10.880291 ETH |
| `bridged_token`, реестр `verified` | 1 | 104.126315 WETH |
| `retry_zero_value` | 1 | — |
| `token_l2_missing`, `token_registry_mismatch`, `deposit_finalized_foreign`, `refund_identity_anomaly` | 0 | — |
| не учтено: `submit_fee_refund` | 6 | 0.013526 ETH |
| не учтено: `redeem_refund_upper_bound` | 6 | ≤ 0.001120 ETH |

С `--no-builtin` та же токен-строка уходит в «unregistered».

**Фид 014 (2.05 ч, для масштаба, по payload):**
- kind 12 — 22 шт., 2.108565 ETH;
- kind 9 — 49 шт.: `callvalue` > 0 у 5 (115.006606 ETH), `callvalue` 0 у 44;
- `depositValue − callvalue` по всем 49 — 0.0362 ETH; это верхняя граница неучтённых возвратов, в неё входят и сетевые комиссии.

## Важное наблюдение для графа

У 44 kind 9 с `callvalue` 0 (один L1-отправитель `0xb3e3c228…c1e3`, селектор 0x97a97cd4) — **41 разный `feeRefundAddr`**. Каждый получает ~0.00049 ETH возврата (в 77285521 вернулось 0.000492 из 0.000502), всего ~0.022 ETH за 2 ч. Это реальный приход ETH с L1 на десятки L2-адресов, и виден он только как `submit_fee_refund`. По условию задачи такие суммы в `funding_edges` не попадают, а идут в «не учтено». Сумма этапа `0x69` точная (по коду и 6 блокам), поэтому её можно сделать отдельным видом ребра — см. вопрос 1.

У retryable от L1-контракта `0x0038dfb2…59cd` `feeRefundAddr` = alias(`retryTo`): возврат идёт на алиас-адрес.

## Предполагается / не проверено

- **FilteredFundsRecipient.** По коду (`tx_processor.go` L249-270) отфильтрованный `0x64` возвращает `ErrFilteredTx`, а деньги уходят на FilteredFundsRecipient. Что чек при этом получает `status=0`, я вывел из `endTxNow` + `Err` в результате исполнения — на данных не наблюдали. Декодер не даёт строку при `status≠1` и считает `deposit_not_succeeded`. Если отфильтрованный депозит окажется со `status=1`, по блоку его не отличить: потребуется баланс или трассировка.
- **Возврат этапа `0x68`** на `refundTo` (возврат `submissionFee` при успехе + неиспользованный газ + multi-gas) — только верхняя граница `maxRefund`.
- **Отмена/истечение тикета** (эскроу → `beneficiary`) и ручной redeem через несколько дней — на данных не наблюдали. Ручной `0x68` в обычном блоке будет учтён, потому что ключ — тип tx.
- **Стандартный ERC-20 шлюз** (mint сразу получателю, `Transfer(0x0 → to)`, `tx.value` 0) поддержан по исходникам, на данных не наблюдался. Шлюзов, кроме WETH, в реестре нет.
- **Ключ `funding_edges` `(to_addr, block_number, tx_index)`:** два `DepositFinalized` одному получателю в одной tx схлопнутся (не наблюдали). Ключ не менял, чтобы не пересоздавать таблицу.
- **Миграция 002** синтаксически не проверена: ClickHouse на Mac не установлен, а загрузку/запуск задача запрещает. `MODIFY COLUMN` Enum8 с добавлением значений и `ADD COLUMN IF NOT EXISTS` — штатные операции, но проверит загрузчик (отдельная задача).

## Что проверить data-auditor

1. Пересчитать строки на 12 блоках RPC (`scratchpad/017/rpc-blocks.jsonl`, сырьё — `scratchpad/014/rpc_raw`, `scratchpad/017/rpc_raw`) и сверить с фидом: `to`, `value`, источник (unalias), `requestId`/`ticketId`. Убедиться, что у `0x69` строк нет, а токен-строка не дублируется ETH-строкой на шлюз.
2. Проверить формулу `submit_fee_refund` = `depositValue − retryValue − maxRefund` по коду Nitro (L279-536) и на 6 блоках.
3. Решить, достаточно ли счётчиков «не учтено» и их описания в `data-model.md`. Особо — 41 адрес с возвратами у kind 9 с `callvalue` 0.
4. Проверить, что адреса в коде — только `verified` (`L2_WETH_GATEWAY`, `L2_WETH`; `ALIAS_OFFSET` — константа протокола, не контракт). L1-адреса в тестах — только ожидаемые значения, с пометкой `observed`.
5. Проверить миграцию `sql/002_funding_edges_l1.sql`: соответствие полей строке `FundingEdge`, семантику `from_addr` (L1-адрес).

## Вопросы к Cowork / Михаилу

1. Делать ли `submit_fee_refund` отдельным видом ребра (например, `l1_fee_refund`)? Сумма точная, а по фиду это основной канал прихода ETH на десятки адресов у kind 9 с `callvalue` 0. Сейчас она только в «не учтено».
2. Набрать полные блоки kind 9/12 для счётчиков на большем объёме: выборки `data/` их не содержат. Варианты: enricher по seq отложенных сообщений из фида сервера, или отдельная задача на загрузчик, который сам будет брать блоки kind ≠ 3.

## Файлы

- `crates/decoders/src/l1_inflows.rs` (новый), `crates/decoders/src/lib.rs`, `crates/decoders/Cargo.toml` (+`serde`, `serde_json`; dev: `zstd`; всё из `[workspace.dependencies]`), `Cargo.lock`
- `crates/decoders/tests/l1_inflows.rs`, `crates/decoders/tests/fixtures/l1-inflows-blocks.jsonl` (новые)
- `crates/decoders/examples/l1_inflows_scan.rs` (новый)
- `sql/002_funding_edges_l1.sql` (новый)
- `.claude/skills/hoodchain-mev/references/{data-model,chain-facts,contracts}.md`
- Рабочие файлы вне репозитория: `scratchpad/017/` (`rpc017.py`, `rpc_calls.txt` = 8, `rpc_raw/`, `rpc-blocks.jsonl`, `crosscheck.out`, `edges-rpc.tsv`, `unacc-rpc.tsv`, `scan-data.out`, `tx_processor.go`, `retryable.go`, `util.go`)
