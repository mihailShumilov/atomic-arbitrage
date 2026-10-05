# 031 — Реестр: USDG, Uniswap v3 Factory, Uniswap v4 PoolManager, Pons v1/v2, pools.trade
status: ready
phase: 1b-подготовка
depends-on: 016
executor: contract-registrar
reviewers: — (`verified` ставит Михаил)

## Зачем
Фаза 1b требует реестр до `verified` для Pons v1/v2, pools.trade, WETH, USDG, Uniswap v3/v4. Сейчас: WETH и шлюз — `verified`, v4 PoolManager — `observed`, остальное `todo`.

## Что сделать
1. По официальным источникам (docs.robinhood.com, документация Uniswap, сайты Pons / pools.trade) и данным (логи в `data/blocks`, `data/samples`: эмиттеры Swap v3/v4, `Initialize`, неизвестные topic0 `0xdc093dab…`, `0x364784d5…` из аудита 006) установить адреса и роли: USDG, Uniswap v3 Factory (и пулы как класс), v4 PoolManager, Pons v1 factory/launcher, Pons v2 launcher/curve factory/hooks, pools.trade factory/hook, прочие launchpad'ы.
2. Для каждого: адрес, источник (URL), что проверено в цепи (`eth_call`/`eth_getCode`, ≤ 30 вызовов публичного RPC с Mac, ≤ 1/с, свой User-Agent), события и их topic0 (ABI из официальных репозиториев, сохранить в `abi/` с SOURCE.md), статус `observed` или кандидат в verified; фишинговые двойники.
3. Blockscout API закрыт Cloudflare для агентов — список ссылок на страницы контрактов для проверки Михаилом (скриншоты → `verified`).

## Что НЕ делать
- Не ставить `verified` (решение Михаила); не подключаться к фиду; никаких кошельков.

## Формат отчёта
Таблица: роль → адрес → источник → проверено → статус; ссылки для Михаила. `status: done`, отдельный коммит.
