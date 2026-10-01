# 014 — Пункт 4 (что такое kind 12). Ревью data-auditor

date: 2026-10-01
reviewer: data-auditor
объект: отчёт `docs/handoff/from-code/014-server-housekeeping-kind12.md` (раздел «Пункт 4»), раздел «L1-сообщения фида» в `.claude/skills/hoodchain-mev/references/data-model.md`, подраздел «L1-сообщения фида (`header.kind`), задача 014» и пометки «сверено в 014» в `references/chain-facts.md`.
**вердикт: PASS** по п. 4. Все 4 проверки PASS. Замечания З1–З4 не блокируют.

## Как проверял

- Объём:
  - 3 закрытых часовых файла `feed-20261001-{09,10,11}.tsv.zst`: 73 112 блоков, seq 77284408..77357519;
  - 13 файлов исходников Nitro, которые скачал indexer-engineer;
  - 16 сохранённых ответов RPC (8 блоков);
  - 4 собственных вызова RPC.
- Работал с собственной копией `…/scratchpad/014-audit/`. sha256 трёх файлов фида совпали с `server.sha256` (`6b2ad824…69d45`, `8fc85e65…1c332`, `bf14d32b…8ae83`). Хэши исходной копии после проверок не изменились. Копию по окончании удалил.
- Скрипты (stdlib + `zstd` CLI) и их вывод лежат в `…/scratchpad/014-audit-scripts/`:
  - `audit_feed_kinds.py` → `audit_feed_kinds.json` — виды, `requestId`, `delayedMessagesRead`, все kind 12;
  - `audit_k9_k13_payload.py` — разбор payload kind 9 и kind 13;
  - `audit_rpc_k12.py` — сверка kind 12 с блоками и чеками;
  - `audit_rpc_k9_k13.py` — блоки kind 9 и kind 13;
  - `audit_rpc_fetch.py` и `rpc_own/` — мои вызовы RPC, журнал `rpc_own/calls.log`.
  - Скрипты indexer-engineer (`analyze_kinds.py`, `rpc_check.py`) не запускал, всё пересчитал своими.
- Подключений к фиду — 0. На сервер не заходил. Код и данные только читал.
- RPC: **4 вызова**, публичный `RPC_URL` из `.env`, с Mac. Жёсткий лимит 4 стоит в скрипте. Пауза ≥ 1.1 с, User-Agent `hoodchain-auditor-014/1`, при 429/403 скрипт останавливается. Все 4 вернули HTTP 200, 429 не было. Вызовы:
  1. `eth_getBlockByNumber(0x49b489b, true)` — блок 77285531;
  2. `eth_getBlockReceipts(0x49b489b)`;
  3. `eth_getBlockByNumber(0x49c5449, true)` — блок 77354057;
  4. `eth_getBlockReceipts(0x49c5449)`.

  Итог по задаче: 16 вызовов indexer-engineer и 4 моих, всего 20 из 20. Бюджет исчерпан.

## 1. Ссылки на Nitro — PASS

Проверено 2026-10-01: открыл каждую ссылку в `nitro/` и сверил по номерам строк.

| Ссылка в отчёте | Что в файле | Итог |
|---|---|---|
| `arbostypes/incomingmessage.go` L24-35 | константы 3, 6, 7, 8, 9, 10, 11, **12 `L1MessageType_EthDeposit`**, 13, 0xFF ровно на L24-35 | совпадает |
| там же L51-58 | `Poster common.Address \`json:"sender"\``, `RequestId`, `BlockNumber`, `Timestamp`, `L1BaseFee \`json:"baseFeeL1"\`` | совпадает |
| `parse_l2.go` L25-90, kind 12 L67-72 | `ParseL2Transactions` занимает L25-89; `case L1MessageType_EthDeposit` — L67 | совпадает |
| `parse_l2.go` L277-297 | `parseEthDepositMessage` (L277-297): `AddressFromReader` (20 Б) + `HashFromReader` (32 Б), без `RequestId` — ошибка; результат `ArbitrumDepositTx{L1RequestId: *header.RequestId, From: header.Poster, To: to, Value}` | совпадает |
| geth `core/types/transaction.go` L48-54 | `ArbitrumDepositTxType = 0x64`, 0x65, 0x66, 0x68, 0x69, 0x6A | совпадает |
| `arb_types.go` L498-504, `transaction_marshalling.go` L179-184 | структура `ArbitrumDepositTx`; JSON-поля `requestId`, `from`, `chainId`, `value`, `to` | совпадает |
| `tx_processor.go` L239-270 (фильтр L249-261) | `MintBalance(from, value)`, затем `core.Transfer(from, to, value)`, EVM не вызывается, `ZeroGas`. Если tx в фильтре — `to = FilteredFundsRecipientOrDefault()` и `ErrFilteredTx` | совпадает |
| `util/util.go` L36, L203-209 | смещение `0x1111000000000000000000000000000000001111` (L36); `RemapL1Address` = сложение, при переполнении — младшие 20 байт (L203-209) | совпадает |
| `block_processor.go` L189-227, L368, L711 | `Coinbase = l1info.poster`, `Time` = max(L1 ts, prev.Time) (L189-227); `startTx := InternalTxStartBlock` (L368); `PutUint64(header.Nonce, delayedMessagesRead)` (L711) | совпадает |
| `parse_l2.go` L381-425; `incomingmessage.go` L383-418 | `createBatchPostingReportTransaction`: при `lastArbosVersion < 50` — V1, иначе V2; поля отчёта | совпадает |
| `params/config_arbitrum.go` L51; `arbosstate.go` L504 | `MaxArbosVersionSupported = ArbosVersion_61`; `case params.ArbosVersion_61` | совпадает |
| `Inbox.sol` L202-214, L317-326; `MessageTypes.sol` | `depositEth`: `dest = msg.sender`, а для контракта или при `tx.origin != msg.sender` — alias. Payload `abi.encodePacked(dest, msg.value)` = 20 + 32 Б. `_deliverToBridge` ставит отправителем `applyL1ToL2Alias(sender)`. `L1MessageType_ethDeposit = 12` | совпадает |

Дополнительно проверил keccak по сигнатурам (pycryptodome):
- `batchPostingReportV2(uint256,address,uint64,uint64,uint64,uint64,uint256)` → `0x9998269e`;
- `startBlock(uint256,uint64,uint64,uint64)` → `0x6bf6a42d`;
- `DepositFinalized(address,address,address,uint256)` → `0xc7f2e9c5`.

Вывод из `Inbox.sol`, который проверяет данные: при депозите из EOA `to` = L1-адрес, поэтому alias(`to`) = `header.sender`. При депозите из контракта `to` = alias(`msg.sender`) = `header.sender`, и тогда alias(`to`) ≠ `header.sender`. Значит, проверка «alias(`to`) = `sender`» действительно отличает депозит из EOA.

**Предполагается (не проверено):**
- что файлы взяты именно из тега `v3.11.4` (коммит `7d5ac271…`), go-ethereum `dff2aadc…` и nitro-contracts `4341b132…`. В самих файлах версии нет, а сеть для этой проверки я не использовал. С заявленной версией согласуются косвенные признаки: копирайт 2021-2026, `MaxArbosVersionSupported = 61`, код фильтрации транзакций (`FilteredFundsRecipient`);
- какая версия контрактов развёрнута на L1 у Robinhood. Это в отчёте честно помечено как непроверенное.

Мелочь: `parseSubmitRetryableMessage` заканчивается на L377, а не на L379, как сказано в отчёте и в chain-facts (З2).

## 2. Данные — PASS

Проверено 2026-10-01 скриптом `audit_feed_kinds.py` по всем 73 112 блокам:
- уникальных блоков 73 112, ожидалось 73 112 (= 77357519 − 77284408 + 1);
- нарушений `delayedMessagesRead` 0: на kind 3 значение не меняется, на остальных видах растёт ровно на 1;
- кроме блоков, в файлах 3 684 ping и 198 `confirmedSequenceNumberMessage`.

| файл (час UTC) | kind 3 | kind 9 | kind 12 | kind 13 |
|---|---|---|---|---|
| 09 | 1 652 | 4 | 1 | 10 |
| 10 | 35 571 | 24 | 3 | 92 |
| 11 | 35 623 | 21 | 18 | 93 |
| всего | 72 846 | 49 | 22 | 195 |

Все цифры совпадают с отчётом и chain-facts. Других видов нет.

- **requestId:** 266 отложенных сообщений, 340181..340446, номера идут подряд без пропусков и в порядке seq.
- **Отправители:**
  - kind 3 — один, `0xa4b000000000000000000073657175656e636572`;
  - kind 13 — один, `0xdaa526086787d9debe1d7f3ffdb1fe50cf8687f4`. Он же `batchPoster` в payload у 195 из 195;
  - kind 9 — четыре отправителя, 44 сообщения от `0xc4f4…d2f4`.
- **kind 12 (22 из 22):**
  - payload ровно 52 байта;
  - (`to` + `0x1111000000000000000000000000000000001111`) mod 2^160 = `header.sender` у **22 из 22**;
  - `requestId` ≠ 0 и `baseFeeL1` ≠ 0 у 22 из 22;
  - получателей 21: `0x321bb3c1…0792` получил два депозита, 0.007404 и 0.103176 ETH.
  - Мой разбор совпал с `kind12.json` исполнителя по всем 22 сообщениям: seq, `to`, `value` до wei, `requestId`, L1-блок, `sender`, `blockHash`, `delayedMessagesRead` — 0 расхождений. Таблица из 9 строк в отчёте тоже совпала.
- **Суммы kind 12:**
  - всего 2.108565 ETH, p50 0.0998937 ETH;
  - 20 из 22 лежат в диапазоне 0.0998937–0.0999121 ETH, ещё одна 0.007404 и одна 0.103176.
- **Задержка** от `header.timestamp` до приёма блока: kind 12 — 421.0–744.7 с, p50 618.0; kind 9 — 400.8–770.4 с; kind 13 — 398.6–775.9 с. Совпадает с отчётом.
- **Всплеск 11:54Z:**
  - 13 депозитов ровно по 99 893 702 843 840 096 wei, все на 13 разных адресов;
  - seq 77354057..77354081, `requestId` 340424..340446, между ними 10 других отложенных сообщений;
  - L1-блоки 26097236..26097263, разброс L1-времени 324 с;
  - приняты в 11:54:11.739–11:54:12.026Z, за 0.287 с.
- **kind 9 и kind 13** (`audit_k9_k13_payload.py`):
  - kind 9: длина payload = 288 + `dataLen` у 49 из 49. 44 сообщения от одного отправителя на `retryTo` `0x1a4f…5666` с селектором `0x97a97cd4` и `callvalue` 0. У 5 сообщений `callvalue` > 0: 0.0067, 0.01, 0.3558, 10.5078 и 104.1263 ETH, всего 115.0066 ETH. Одно из них — `finalizeInboundTransfer` (`0x2e567b36`);
  - kind 13: `batchNum` 296100..296294 подряд.

## 3. Сверка 5 из 5 kind 12 с L2-tx — PASS

Проверено 2026-10-01, `audit_rpc_k12.py`. Сохранённые ответы `rpc_raw/` по блокам 77285531, 77323647, 77346452, 77346458 и 77354057 сверены с моим разбором фида. По каждому блоку прошли все 19 условий:
- `number` = seq, `hash` = `blockHash` фида;
- в блоке ровно 2 tx: индекс 0 — `0x6a` StartBlock (`0x6bf6a42d`, `from` = `to` = `0x…0a4b05`), индекс 1 — `0x64`;
- у `0x64`: `to` = `to` из payload, `value` = сумма из payload до wei, `from` = `header.sender`, `requestId` = `header.requestId`. `gas`, `gasPrice` и `nonce` равны 0, `input` = `0x`;
- шапка блока: `miner` = `header.sender`, `nonce` = `delayedMessagesRead`, `gasUsed` = 0;
- 2 чека. У чека `0x64`: `status` 0x1, `gasUsed`, `gasUsedForL1` и `cumulativeGasUsed` равны 0, `logs` = [], `type` 0x64, `blockHash` = фиду. В обоих чеках блока логов нет.

Разница `l1BlockNumber` (RPC) − `header.blockNumber` по блокам: 41, 53, 57, 42 и 61. Отчёт пишет 41–61, совпадает.

Независимая перепроверка через RPC (4 вызова, см. выше): блоки 77285531 и 77354057. Мои ответы `result` **побайтно совпали** с `rpc_raw/` исполнителя. Тот же скрипт по моим ответам дал OK. Фикстура для декодера: tx `0x3f8e47b0…b522c1` в блоке 77285531, подтверждаю.

Блоки kind 9 и kind 13 проверены по сохранённым ответам (`audit_rpc_k9_k13.py`, своих вызовов не делал):
- 77300695 и 77312169 (kind 9): по 3 tx — `0x6a`, `0x69` (`to` = `0x…6e`, `value` 0, логи `0x7c793cce` + `0x5ccd0095`) и `0x68` (`status` 1). `value` у `0x68` = `retryValue` у `0x69` до wei в 2 из 2 (0.006661257477643089 и 104.1263149996361 ETH). В 77312169 у `0x68` два `Transfer` и `0xc7f2e9c5`. В 77300695 `retryTo` = unalias(`from`), то есть деньги пришли на тот же L1-адрес;
- 77285519 (kind 13): две tx `0x6a` — `0x6bf6a42d` и `0x9998269e`, `gasUsed` 0, логов нет, `miner` = batch poster.

## 4. Таблица в data-model.md и записи в chain-facts — PASS

Сверил каждую строку таблицы с данными (п. 2–3) и кодом (п. 1):

| kind | Утверждение в таблице | Чем подтверждено |
|---|---|---|
| 3 | `sender` `0xa4b0…6572`, 72 846 из 73 112 | данные, 1 отправитель |
| 9 | 49 (~22/ч); `0x69` + авто-redeem `0x68`; `value` у `0x68` = `retryValue`; источник — unalias(`0x69.from`) | данные + 2 блока RPC + `parse_l2.go`. То, что при неудаче деньги остаются в эскроу, помечено «предполагается» |
| 12 | 22 (3 и 18 за полные часы); payload 20 + 32 Б; одна `0x64`, `status` 1, `gasUsed` 0, **логов нет**; брать только из `0x64` или payload; про `FilteredFundsRecipient` — «по коду, не наблюдали» | данные 22 из 22 + RPC 5 из 5 + `tx_processor.go` |
| 13 | 195 (~93/ч); `0x6a` с `0x9998269e`, `gasUsed` 0, логов нет | данные + 1 блок RPC + keccak |
| 7 | по коду `0x64` на alias + `0x65`/`0x66`; не наблюдали | `parse_l2.go` L38-56: `ArbitrumDepositTx{To: header.Poster}` + `parseUnsignedTx` |
| 6, 8, 10, 11 | нет tx (6, 8), ошибка (10), genesis (11) | `parse_l2.go` L31-36, L62-66, L73-75 |

- **Руководство для графа финансирования верное.** Депозит виден только как tx `0x64` или в payload фида, в `logs` его нет (5 из 5). Для kind 9 источник входа — `0x68` с `value` > 0, а `0x69` несёт суммы только в своих полях (`value` = 0). Формула unalias (вычесть смещение mod 2^160) соответствует `util.go`.
- **chain-facts.md:**
  - у каждой новой записи есть дата (2026-10-01), способ проверки и объём: файлы, число блоков, число вызовов RPC и номера блоков;
  - записи по исходникам помечены как «источник: исходники Nitro», с тегом и коммитом;
  - непроверенное помечено явно: версия контрактов на L1, «одна раздача одного владельца», шлюз WETH, kind 7/6/8/10/11, мосты из своего пула.
  - Пометки «сверено в 014» у записей 001, 006 и 013 стоят по делу и подтверждены п. 1–3.

## Замечания (не блокируют)

- **З1. Диапазон сумм в отчёте и chain-facts.** Там написано «20 из 22 — в диапазоне 0.0999–0.1000 ETH». Буквально в [0.0999, 0.1000] попадают только 7 сумм. Остальные 13 равны 0.0998937 и становятся 0.0999 только при округлении до 4 знаков. Точнее писать «0.09989–0.09991 ETH». На выводы это не влияет.
- **З2. Конец функции.** Ссылка `parse_l2.go` L299-379 для kind 9: функция заканчивается на L377. Косметика.
- **З3. Что не покрывает таблица для kind 9.** Для графа финансирования учтён только `0x68.value`. Но при исполнении `0x69` излишек `maxSubmissionFee` переводится на `feeRefundAddr` (`tx_processor.go` L376-382). Это тоже приход ETH на L2-адрес, и его нет ни в `value` какой-либо tx, ни в логах. В двух проверенных блоках разница `depositValue − retryValue` — тысячные доли ETH и меньше, то есть суммы малые. Для `0x69` действует и свой onchain-фильтр: `feeRefundAddr` и `beneficiary` перенаправляются (L288-306). Рекомендация для задачи декодера: отметить в `data-model.md`, что такие возвраты в граф не попадают, или брать их из трассировки.
- **З4. Всплеск может быть шире.** 13 депозитов 11:54Z — последние kind 12 в доступных данных. `requestId` 340446 — последнее отложенное сообщение в копии, после него до 12:00Z (3 438 блоков) отложенных сообщений нет. Час 12 не разбирали, поэтому 13 — нижняя граница размера всплеска, а не его полный размер. Формулировку «13 депозитов» стоит читать как «в доступных данных».

## Проверено и предполагается

- **Проверено (2026-10-01):** всё в п. 1–4 выше. Метод:
  - чтение исходников по строкам;
  - пересчёт трёх файлов фида своими скриптами;
  - сверка 5 сохранённых блоков kind 12;
  - 2 из них перезапрошены мной через RPC (4 вызова), ответы побайтно совпали.
- **Предполагается:**
  - соответствие скачанных файлов тегу `v3.11.4` и указанным коммитам — по содержимому файлов это не доказуемо;
  - версия контрактов Inbox на L1 у Robinhood;
  - поведение kind 7, 6, 8, 10, 11, отфильтрованного депозита и неудачного авто-redeem — известно только по коду;
  - частоты видов — за 2.05 ч, суточной статистики ещё нет.
