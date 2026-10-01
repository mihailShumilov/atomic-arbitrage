# Реестр контрактов

Адрес используется в коде **только** со статусом `verified`. Статусы:

- `verified` — адрес подтверждён официальной документацией **и** проверен на Blockscout (контракт верифицирован, испускает ожидаемые события). Указаны URL источника и дата.
- `observed` — найден в данных, но не подтверждён официально. Можно использовать для исследования, нельзя для финальных выводов.
- `todo` — нужно найти.
- `rejected` — адрес оказался неверным или фишинговым; запись не удаляется, указана причина.

Поиск и проверку адресов ведёт агент `contract-registrar`.

## Процедура верификации

1. Найти адрес в официальной документации (docs.robinhood.com/chain, документация Pons, pools.trade / Uniswap). Не с агрегаторов, не из чатов, не из поиска по картинкам.
2. Открыть на Blockscout Robinhood Chain: контракт верифицирован, имя совпадает, есть свежие события нужного topic0.
3. Сверить topic0 событий с `crates/decoders` (тесты `topics_are_canonical`).
4. Записать: адрес, статус, источник (URL), дата, кто проверил. Михаил подтверждает переход в `verified`.

## Topic0 (проверены тестами в `crates/decoders`)

| Событие | topic0 |
|---|---|
| Uniswap v3 `Swap(address,address,int256,int256,uint160,uint128,int24)` | `0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67` |
| Uniswap v4 `Swap(bytes32,address,int128,int128,uint160,uint128,int24,uint24)` | `0x40e9cecb9f5f1f1c5b9c97dec2917b7ee92e57ba5563708daca94dd84ad7112f` |
| Uniswap v4 `Initialize(bytes32,address,address,uint24,int24,address,uint160,int24)` | `0xdd466e674ea557f56295e2d0218a125ea4b4f0f6f3307b95f85e6110838d6438` |
| ERC-20 `Transfer(address,address,uint256)` | `0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef` |

## Адреса

| Роль | Адрес | Статус | Источник / как найден | Дата |
|---|---|---|---|---|
| Uniswap v4 PoolManager | `0x8366a39cc670b4001a1121b8f6a443a643e40951` | `observed` | Единственный эмиттер v4 `Swap` за 300 блоков до 74759304 (588 логов) | 2026-09-28 |
| WETH (L2 WETH, aeWETH, прокси) | `0x0bd7d308f8e1639fab988df18a8011f41eacad73` | `observed` — **кандидат в verified**: docs совпадает побайтно; в цепи `name`/`symbol` = `WETH`, `decimals` = 18, `l2Gateway()` = L2-шлюз WETH (строка ниже), `l1Address()` = `0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2` (WETH на L1); EIP-1967 impl `0xc6b81b429797e0f555440b70cd99e032d7ae947e`, admin `0xa3acd31afb851b4eb9dad00f5204c01d924267df` (= «L2 Proxy Admin» в docs). Не сделано: Blockscout (API за Cloudflare, 403) | [docs.robinhood.com/chain/contracts](https://docs.robinhood.com/chain/contracts/) (WETH) и [/chain/protocol-contracts](https://docs.robinhood.com/chain/protocol-contracts/) («L2 Weth»); в данных — mint/transfer в блоке 77312169 (задача 014). Проверка — задача 016: 10 вызовов RPC (`eth_call`, `eth_getStorageAt`), contract-registrar | 2026-10-01 |
| L2 WETH Gateway (шлюз WETH, прокси) | `0x1d187c3e2da52d72bc9c41e3aba0fdfa6a7bf055` | `observed` — **кандидат в verified**: docs совпадает побайтно; в цепи `counterpartGateway()` = `0xf7e12b9614b509c747ab4423bc4acf923759cf1b` (= «L1 Weth Gateway» в docs), `router()` = L2 Gateway Router (строка ниже); EIP-1967 impl `0x0354a93fe0db94bb72ec053f43301746fc806edf`; в блоке 77312169 — `retryTo` авто-redeem, `DepositFinalized` (topic0 `0xc7f2e9c5…`, topic1 = WETH на L1). Admin не читал (лимит вызовов). Не сделано: Blockscout (403) | [docs.robinhood.com/chain/protocol-contracts](https://docs.robinhood.com/chain/protocol-contracts/) («L2 Weth Gateway»); блок 77312169 (задача 014); RPC — задача 016 | 2026-10-01 |
| L2 Gateway Router (token bridge) | `0x1e324b9316138ca9a73f960213621ad1aaf01b89` | `observed` | docs («L2 Gateway Router») и `router()` L2-шлюза WETH в цепи (задача 016). `getCode`/события отдельно не проверялись | 2026-10-01 |
| L1 WETH Gateway (Ethereum L1, не L2!) | `0xf7e12b9614b509c747ab4423bc4acf923759cf1b` | `observed` | docs («L1 Weth Gateway»); = `counterpartGateway()` L2-шлюза; unalias(`0x08f22b9614b509c747ab4423bc4acf923759e02c`) — `from` tx `0x69`/`0x68` в блоке 77312169. На L2 в данных он виден только как alias `0x08f22b9614b509c747ab4423bc4acf923759e02c` (`header.sender` kind 9). Контракт на L1 через Ethereum RPC не проверялся | 2026-10-01 |
| USDG | — | `todo` | Подсказка для следующей задачи: docs.robinhood.com/chain/contracts указывает `0x5fc5360d0400a0fd4f2af552add042d716f1d168`; в цепи не проверено (задача 016, вне объёма) | |
| Uniswap v3 Factory | — | `todo` | | |
| Pons v1 factory / launcher | — | `todo` | | |
| Pons v2 launcher / curve factory | — | `todo` | | |
| Pons v2 hook(s) | — | `todo` | | |
| pools.trade factory / hook | — | `todo` | | |
| Crowd Launch / другие launchpad'ы | — | `todo` | | |

## ABI launchpad'ов

Для Pons v1, Pons v2 и pools.trade: сохранить официальные ABI в `abi/<venue>/` с источником. Декодеры launchpad-специфичных событий (создание токена, покупка/продажа на кривой, выпуск) пишутся только после этого.

## Наблюдения, требующие объяснения

- 2026-10-01 (задача 016): topic0 token bridge **вне тестов `crates/decoders`** (keccak сигнатуры, ABI — `abi/arbitrum-token-bridge/`): `DepositFinalized(address,address,address,uint256)` = `0xc7f2e9c55c40a50fbc217dfc70cd39a222940dfa62145aa0ca49eb9535d4fcb2` (совпал с логом в 77312169), `WithdrawalInitiated(address,address,address,uint256,uint256,uint256)` = `0x3073a74ecb728d10be779fe19a74a1428e20468f5b4d167bf9c73d9067847d73` (в цепи не наблюдали). Перед использованием в декодере — добавить в тест.
- 2026-10-01 (задача 016): у WETH на L2 (aeWETH по исходникам Arbitrum; что развёрнутая реализация совпадает с исходником — предполагается, Blockscout не открылся) **нет событий WETH9 `Deposit`/`Withdrawal`**: wrap/unwrap = `Transfer` с/на нулевой адрес. Mint `Transfer(0 → L2-шлюз WETH)` + `Transfer(шлюз → получатель)` в одной tx — это депозит WETH с L1, а не эмиссия. `name()` = `WETH` (не «Wrapped Ether», как у WETH на Arbitrum One) — по docs и цепи это канонический адрес; двойников с тем же `name`/`symbol` не искали (Blockscout недоступен).
- 2026-09-28: за 300 блоков v3 `Swap` испустили 82 разных контракта. Это ожидаемо для Pons v1 (отдельный v3-пул на каждый токен), но какие из них Pons, а какие нет — определить через фабрику после её верификации.
