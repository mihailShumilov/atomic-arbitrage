# 016 — Реестр: WETH на L2 и L2-шлюз WETH → observed (отчёт)

Исполнитель: contract-registrar. Дата: 2026-10-01. Коммитов нет. Статус задачи в `to-code/016` не менял (оставлен координатору).

## Сделано

- Полные адреса и роли взяты из сырых ответов RPC задачи 014 (блок 77312169, сохранены в scratchpad 014, новых вызовов на это не тратил) и сверены с официальной страницей docs.robinhood.com.
- В `.claude/skills/hoodchain-mev/references/contracts.md` внесены 4 записи со статусом `observed`: WETH на L2 и L2-шлюз WETH (обе с пометкой «кандидат в verified»), L2 Gateway Router, L1 WETH Gateway (это контракт на Ethereum, на L2 виден как alias). Строка WETH `todo` заменена: прежнего адреса WETH в реестре не было, конфликтов нет. В USDG (`todo`) добавлена подсказка из docs, статус не менял.
- Минимальные ABI с источником: `abi/arbitrum-token-bridge/` (`L2WethGateway.min.json`, `aeWETH.min.json`, `SOURCE.md`).
- Два наблюдения в `contracts.md`: topic0 token bridge пока не покрыты тестом декодеров; у WETH на L2 нет событий `Deposit`/`Withdrawal`.

## Что найдено

| Роль | Адрес | Источник | Что проверено в цепи | Статус |
|---|---|---|---|---|
| WETH на L2 (aeWETH, прокси) | `0x0bd7d308f8e1639fab988df18a8011f41eacad73` | [docs: contracts](https://docs.robinhood.com/chain/contracts/) (WETH), [docs: protocol-contracts](https://docs.robinhood.com/chain/protocol-contracts/) («L2 Weth») | `name`=`symbol`=`WETH`, `decimals`=18, `l2Gateway()`=`0x1d18…f055`, `l1Address()`=`0xc02a…6cc2`; impl `0xc6b81b429797e0f555440b70cd99e032d7ae947e`, admin `0xa3acd31afb851b4eb9dad00f5204c01d924267df` (= «L2 Proxy Admin» в docs) | `observed`, кандидат в verified |
| L2-шлюз WETH (прокси) | `0x1d187c3e2da52d72bc9c41e3aba0fdfa6a7bf055` | docs: protocol-contracts («L2 Weth Gateway») | `counterpartGateway()`=`0xf7e1…cf1b`, `router()`=`0x1e32…1b89`; impl `0x0354a93fe0db94bb72ec053f43301746fc806edf`; `retryTo` и эмиттер `DepositFinalized` в 77312169 | `observed`, кандидат в verified |
| L2 Gateway Router | `0x1e324b9316138ca9a73f960213621ad1aaf01b89` | docs: protocol-contracts | только как `router()` шлюза | `observed` |
| L1 WETH Gateway (Ethereum) | `0xf7e12b9614b509c747ab4423bc4acf923759cf1b` | docs: protocol-contracts | = `counterpartGateway()` шлюза = unalias(`0x08f22b9614b509c747ab4423bc4acf923759e02c`), а это `from` у `0x69`/`0x68` в 77312169 | `observed` |
| WETH на L1 (Ethereum, для справки) | `0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2` | docs: protocol-contracts («L1 Weth») | topic1 в `DepositFinalized`, `l1Address()` WETH на L2 | в реестр L2 отдельной строкой не вносил |

## Проверено (2026-10-01, как)

- **Адреса в docs совпадают побайтно.** Скачал HTML `docs.robinhood.com/chain/protocol-contracts/` и `/chain/contracts/` через curl (HTTP 200) и сравнил строки в нижнем регистре. На странице 4 адреса стоят в своих строках таблицы: L1/L2 Weth Gateway, L1/L2 Weth. Адрес explorer'а взял из ссылок той же страницы: `https://robinhoodchain.blockscout.com/`.
- **Связь в блоке 77312169 (сырьё 014, без новых вызовов).** В tx `0x68` (`0x8a448fd9…5d4a`, `to` = шлюз, `value` = 104.12631 ETH) идёт `Transfer` WETH 0 → шлюз, затем `Transfer` шлюз → `0x07ae8551…0e67` на ту же сумму, затем `DepositFinalized(l1Token=0xc02a…6cc2, from=to=0x07ae…0e67)`. Это ровно `inboundEscrowTransfer` из `L2WethGateway.sol` (`deposit{value}` + `safeTransfer`). `from` = `0x08f22b96…e02c`, после unalias получается `0xf7e12b96…cf1b`, то есть L1 WETH Gateway из docs.
- **topic0 и селектор.** keccak `DepositFinalized(address,address,address,uint256)` = `0xc7f2e9c5…fcb2`, совпал с логом. `finalizeInboundTransfer(...)` = `0x2e567b36`, совпал с `retryData`. Сигнатуры взяты из OffchainLabs/token-bridge-contracts `main` @ `0746a713` (2026-03-13). В тесте `topics_are_canonical` (`crates/decoders`) этих topic0 нет, это задача indexer-engineer.
- **RPC: 10 вызовов** на публичный `RPC_URL`, с Mac, последовательно, пауза 1.2 с, UA `hoodchain-mev-registrar/0.1 (read-only)`. Все ответили HTTP 200, 429 не было. 7 × `eth_call` (WETH `name`, `symbol`, `decimals`, `l2Gateway`, `l1Address`; шлюз `counterpartGateway`, `router`) и 3 × `eth_getStorageAt` (EIP-1967 impl у WETH и шлюза, admin у WETH). `eth_getCode` отдельно не вызывал: непустые ответы `eth_call` уже показывают, что код есть. Сырьё лежит в `scratchpad/016/rpc_raw/`. К фиду не подключался.

## Предполагается / не проверено

- **Blockscout не проверен.** `/api/v2/addresses/…` и `/api/v2/smart-contracts/…` вернули Cloudflare-челлендж (403) и через curl, и через WebFetch. Обходить его не стал. Поэтому не проверено, что реализации `0xc6b8…947e` и `0x0354…6edf` верифицированы и совпадают с исходниками Arbitrum. ABI в `abi/` переписан из исходников, это не ABI с Blockscout.
- Admin прокси шлюза не читал из-за лимита в 10 вызовов. Скорее всего это тот же L2 Proxy Admin.
- Что L2 Gateway Router и L1 WETH Gateway действительно контракты (код на L1 / L2, события), отдельно не проверял.
- **Двойники не искались.** Поиск токенов по `name`/`symbol` = `WETH` делается через Blockscout, а он недоступен. На стороне потребителя риск снят: в docs явно сказано, что токен с тем же тикером, но другим адресом — не канонический.
- По исходнику, у aeWETH нет `Deposit`/`Withdrawal`: wrap и unwrap видны как `Transfer` с нулевого адреса и на него. На данных это видно только для пути через шлюз (77312169).

## Вопросы к Михаилу / Cowork

1. Открыть в браузере Blockscout для `0x0bd7…ad73` и `0x1d18…f055`: верифицирован ли контракт, какие имя и реализация. Это последний недостающий пункт до `verified`. После этого переводить обе записи в `verified`?
2. Ставить ли задачу на USDG (`0x5fc5360d…d168` из docs) и поиск двойников WETH/USDG? Обоим нужен рабочий доступ к Blockscout API: ключ или другой способ.
3. Задача 017 (декодер L1-входов): indexer-engineer должен добавить `DepositFinalized` / `WithdrawalInitiated` в `topics_are_canonical` и учесть, что mint WETH через шлюз — это вход с L1, а не эмиссия. Адреса пока `observed`. По правилам скилла в код идут только `verified`, поэтому порядок за Михаилом.
