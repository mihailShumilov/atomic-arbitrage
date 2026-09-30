# Реестр контрактов

Адрес используется в коде **только** со статусом `verified`. Статусы:

- `verified` — адрес подтверждён официальной документацией **и** проверен на Blockscout (контракт верифицирован, испускает ожидаемые события). Указаны URL источника и дата.
- `observed` — найден в данных, но не подтверждён официально. Можно использовать для исследования, нельзя для финальных выводов.
- `todo` — нужно найти.

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
| WETH | — | `todo` | | |
| USDG | — | `todo` | | |
| Uniswap v3 Factory | — | `todo` | | |
| Pons v1 factory / launcher | — | `todo` | | |
| Pons v2 launcher / curve factory | — | `todo` | | |
| Pons v2 hook(s) | — | `todo` | | |
| pools.trade factory / hook | — | `todo` | | |
| Crowd Launch / другие launchpad'ы | — | `todo` | | |

## ABI launchpad'ов

Для Pons v1, Pons v2 и pools.trade: сохранить официальные ABI в `abi/<venue>/` с источником. Декодеры launchpad-специфичных событий (создание токена, покупка/продажа на кривой, выпуск) пишутся только после этого.

## Наблюдения, требующие объяснения

- 2026-09-28: за 300 блоков v3 `Swap` испустили 82 разных контракта. Это ожидаемо для Pons v1 (отдельный v3-пул на каждый токен), но какие из них Pons, а какие нет — определить через фабрику после её верификации.
