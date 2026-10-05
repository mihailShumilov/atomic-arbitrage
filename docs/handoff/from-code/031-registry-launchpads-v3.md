# 031 — Реестр: USDG, Uniswap v3/v4, Pons v1/v2, pools.trade, прочие launchpad'ы (отчёт)

Исполнитель: contract-registrar. Дата: 2026-10-05. Коммитов нет. `status` в `to-code/031` не менял (оставлен координатору; там `in-progress`). `verified` нигде не ставил.

## Сделано

- Разобрал все логи локальных данных (`data/blocks/*.jsonl.zst` и `data/samples/hourly-20260804-20260930.jsonl.zst`, только чтение): 2 712 блоков, 34 344 tx, 127 824 лога. Нашёл эмиттеры v3/v4 `Swap`, `Initialize`, `PoolCreated`, неизвестных topic0 из аудита 006 и самые частые контракты. Промежуточные файлы лежат в scratchpad `031/`.
- Нашёл адреса в первоисточниках: docs.robinhood.com, документация Uniswap (developers.uniswap.org, куда docs.uniswap.org отдаёт 301) и её `deployments.json`, блог Uniswap про pools.trade, docs.ponsfamily.com, docs.doppler.lol. Сверил их с данными и проверил 26 вызовами публичного RPC.
- `.claude/skills/hoodchain-mev/references/contracts.md`:
  - обновил строку v4 PoolManager (теперь есть официальный источник);
  - заполнил все строки `todo` из задачи;
  - добавил 22 строки: USDG, v3 Factory, класс v3-пулов, периферию Uniswap, Pons v1/v2, pools.trade (Liquidity Launchpad), Doppler, PAIR;
  - добавил 8 наблюдений (двойники и предупреждения, разбор `0xdc093dab…` и `0x364784d5…`);
  - пометил старое наблюдение от 09-28 как частично закрытое;
  - чужие записи не удалял.
- `abi/`: создал пять папок, в каждой есть `SOURCE.md` (репозиторий, коммит, дата, topic0, сколько раз встречается в данных):
  - `pons/`: ABI фабрики V1 из репозитория как есть, события V1/V2;
  - `pools-trade/`: LiquidityLauncher, InstantLaunchStrategy, FeeSplitter, CCA;
  - `doppler/`;
  - `uniswap-v3/`;
  - `uniswap-v4/`.

  JSON содержит только события и собран из исходников. Это не ABI с Blockscout.

## Таблица: роль → адрес → источник → проверено → статус

| Роль | Адрес | Источник | Что проверено в цепи / на данных | Предлагаемый статус |
|---|---|---|---|---|
| USDG (Global Dollar, прокси) | `0x5fc5360d0400a0fd4f2af552add042d716f1d168` | [docs.robinhood.com/chain/contracts](https://docs.robinhood.com/chain/contracts/) | `name`=`Global Dollar`, `symbol`=`USDG`, `decimals`=6; EIP-1967 impl `0x68184c449e1a8f34fa18d289737129fd27b66f8f`, admin-слот = 0; 10 709 `Transfer` в данных | `observed`, кандидат в verified |
| Uniswap v3 Factory | `0x1f7d7550b1b028f7571e69a784071f0205fd2efa` | [Uniswap v3 Robinhood deployments](https://developers.uniswap.org/docs/protocols/v3/deployments/v3-robinhood-chain-deployments), [deployments.json](https://developers.uniswap.org/deployments.json); тот же адрес в docs Pons | код 24 535 байт; `owner()`=`0x05c420bc…aa25`; 3/3 `PoolCreated` в данных, CREATE2 сходится 3/3 | `observed`, кандидат в verified |
| Uniswap v3 пулы (класс) | CREATE2 от фабрики | то же | 1 275 из 1 464 эмиттеров v3 `Swap` (5 906 из 6 862 логов) выводятся от фабрики | `observed` |
| Uniswap v4 PoolManager | `0x8366a39cc670b4001a1121b8f6a443a643e40951` | [Uniswap v4 deployments](https://developers.uniswap.org/contracts/v4/deployments), deployments.json | код 24 009 байт; единственный эмиттер v4 `Swap` (6 668), `Initialize` (30), `ModifyLiquidity` (681); `poolManager()` хуков Pons v2 и Doppler указывает на него | `observed`, кандидат в verified |
| v3 NPM / SwapRouter02 / v4 PositionManager | `0x73991a25…de0d3` / `0xcaf681a6…e5cb2` / `0x58daec31…04fa7` | Uniswap docs, deployments.json | только данные: `to` у tx создания пулов и свопов | `observed` |
| Universal Router | `0x8876789976decbfcbbbe364623c63652db8c0904`, `0x06afba43fd06227fa663b0daecf536f6eaa6bf99` | docs v3/v4 указывают первый адрес, deployments.json — второй | код обоих 24 546 байт, различаются только immutables | `observed`, расхождение источников |
| Pons v1 factory (активная) | `0xa5aab3f0c6eeadf30ef1d3eb997108e976351feb` | [docs.ponsfamily.com/docs](https://docs.ponsfamily.com/docs), README pons-labs | код 24 353 байт; `TokenLaunched` (`0xdb51ea9a…`, совпал с docs): 0 в данных и 0 за 50 000 блоков по `eth_getLogs` | `observed`, кандидат с оговоркой (свежих событий нет) |
| Pons v1 locker / legacy factory / legacy locker | `0x736d7669…7f35` / `0x0c37a24f…77a4` / `0x31ca5e10…54b5` | docs Pons V1 | locker встречается в данных (29 логов); legacy в данных нет | `observed` |
| PONS / его пул | `0x39dbed3a…4571` / `0x10cc6bd3…26ba` | docs Pons V1 (контрольный токен) | пул выводится CREATE2 от v3 Factory (WETH/PONS, fee 10000) | `observed` |
| Pons v2 factory | `0x7ed598bcef8bd9edd8c97a195c6d13f40801ec7e` | [docs.ponsfamily.com/docs/v2](https://docs.ponsfamily.com/docs/v2), README | код 24 177 байт; `TokenLaunched` v2 (`0x8d4aad49…`): 34 в данных, 19 за последние 5 000 блоков | `observed`, кандидат в verified |
| Pons v2 meme hook | `0xe5e702641ea86f4ae6cc3cdaed2b886f976be044` | docs Pons v2 | код 15 167 байт; `poolManager()` = PoolManager; `HookFeeCollected` 1 840, `PoolFeesSwept` 87 | `observed`, кандидат в verified |
| Pons v2 кривые (класс) | CREATE2 на каждый запуск | docs Pons v2 | `CurveBuy` 604 от 377 кривых, `CurveSell` 475 от 323 | `observed` |
| Pons v2: fee escrow, buyback vault, locker, launch-and-buy, deployer, graduation executor/guard | 7 адресов, см. contracts.md | docs Pons v2 | только данные (escrow, vault, launch-and-buy видны) | `observed` |
| pools.trade: LiquidityLauncher | `0x0000ffffbe8efe702c8703ae3477ff5de3d319c0` | [Uniswap Liquidity Launchpad deployments](https://developers.uniswap.org/docs/liquidity/liquidity-launchpad/deployments), deployments.json; связь с pools.trade — [блог Uniswap](https://blog.uniswap.org/pools-trade-a-new-way-to-launch-on-robinhood-chain) | код 4 127 байт; `TokenCreated`/`TokenDistributed`; 2 `Initialize` с hooks=0, fee 2500, tickSpacing 25 | `observed`, кандидат в verified |
| pools.trade: InstantLaunchStrategy v3.2.0 / v3.3.0 (creator fees) | `0x23f8209572b4a1c2ad88a42749e830791fb027f1` / `0x7c48dde3b447381f4d986334679b3afc7f2d35c2` | Launchpad deployments (v3.3.0 есть только на странице docs) | код 10 822 / 11 287 байт; каждый испустил `TokenLaunched` (`0x3b3d2baf…`) | `observed`, кандидат в verified |
| pools.trade: ILS без creator fee, FeeSplitter, vault, claim recipients | 10 адресов, см. contracts.md | Launchpad deployments | FeeSplitter `0xeff1…acdf` испускает события в данных | `observed` |
| pools.trade Crowd Launch: CCA factory v2.1.0 / v2.0.0, LBPStrategy, InitializerHook | `0x000000001f26…63f8` и др. | Launchpad deployments | код v2.1.0 24 214 байт; один аукцион `0x9a36f2f3…caee` в данных | `observed` |
| Doppler: DopplerHookInitializer | `0x4e3468951d49f2eea976ed0d6e75ffcb44a9a544` | [docs.doppler.lol](https://docs.doppler.lol/reference/contract-addresses) | `poolManager()` = PoolManager; hooks в 9 из 30 `Initialize`; собственный `Swap` 687 (687/687 дублируют `Swap` PoolManager) | `observed`, кандидат в verified |
| Doppler: Airlock | `0xeb7c034704ef8dcd2d32324c1545f62fb4ad0862` | docs.doppler.lol | код 5 695 байт; `Create` 9 из 9 | `observed`, кандидат в verified |
| PAIR: PairPriceOracle (предполагается) | `0xf15f6ff9a1f0ed55b8223a4f0bd6f9c8c0ab877b` | только статья в X (подсказка) | `0x364784d5…` — публикация цен (8 знаков, timestamp), не сделки | `observed`, источник неофициальный |

## Ссылки для Михаила (скриншоты Blockscout → `verified`)

Нужно посмотреть: контракт верифицирован, имя, у прокси — реализация и admin.

1. USDG: https://robinhoodchain.blockscout.com/address/0x5fc5360d0400a0fd4f2af552add042d716f1d168?tab=contract, реализация: https://robinhoodchain.blockscout.com/address/0x68184c449e1a8f34fa18d289737129fd27b66f8f?tab=contract
2. Uniswap v3 Factory: https://robinhoodchain.blockscout.com/address/0x1f7d7550b1b028f7571e69a784071f0205fd2efa?tab=contract
3. Uniswap v4 PoolManager: https://robinhoodchain.blockscout.com/address/0x8366a39cc670b4001a1121b8f6a443a643e40951?tab=contract
4. Pons v1 factory: https://robinhoodchain.blockscout.com/address/0xa5aab3f0c6eeadf30ef1d3eb997108e976351feb?tab=contract
5. Pons v2 factory: https://robinhoodchain.blockscout.com/address/0x7ed598bcef8bd9edd8c97a195c6d13f40801ec7e?tab=contract
6. Pons v2 meme hook: https://robinhoodchain.blockscout.com/address/0xe5e702641ea86f4ae6cc3cdaed2b886f976be044?tab=contract
7. pools.trade LiquidityLauncher: https://robinhoodchain.blockscout.com/address/0x0000ffffbe8efe702c8703ae3477ff5de3d319c0?tab=contract
8. pools.trade InstantLaunchStrategy v3.2.0: https://robinhoodchain.blockscout.com/address/0x23f8209572b4a1c2ad88a42749e830791fb027f1?tab=contract
9. pools.trade InstantLaunchStrategy v3.3.0: https://robinhoodchain.blockscout.com/address/0x7c48dde3b447381f4d986334679b3afc7f2d35c2?tab=contract
10. Doppler DopplerHookInitializer: https://robinhoodchain.blockscout.com/address/0x4e3468951d49f2eea976ed0d6e75ffcb44a9a544?tab=contract
11. Doppler Airlock: https://robinhoodchain.blockscout.com/address/0xeb7c034704ef8dcd2d32324c1545f62fb4ad0862?tab=contract

Желательно для разбора расхождений и двойников:

12. Universal Router (docs): https://robinhoodchain.blockscout.com/address/0x8876789976decbfcbbbe364623c63652db8c0904?tab=contract
13. Universal Router (deployments.json): https://robinhoodchain.blockscout.com/address/0x06afba43fd06227fa663b0daecf536f6eaa6bf99?tab=contract
14. Хук с событиями Pons, которого нет в docs: https://robinhoodchain.blockscout.com/address/0x57387759ea3a3116330f4bd2cae48b03091a2044?tab=contract
15. Фабрика v3-форка № 1: https://robinhoodchain.blockscout.com/address/0x1ac9db4a2608ba45d6127b1737949b51bb54b7f3?tab=contract
16. Фабрика v3-форка № 2: https://robinhoodchain.blockscout.com/address/0xe0c4ceb92d08ca985bb70fe0a22feb121a9854a8?tab=contract
17. Эмиттер `0xdc093dab…`: https://robinhoodchain.blockscout.com/address/0x2d0da469115232ad00159757f9d25f81d498206d?tab=contract

Ещё полезно открыть вкладку токенов поиска Blockscout по `USDG`, `WETH` и `PONS`: есть ли токены с тем же именем или тикером на других адресах (двойники по имени).

## Проверено (2026-10-05, как)

- **Адреса в первоисточниках.** Скачал страницы через curl (HTTP 200, UA `hoodchain-mev-registrar/0.1 (read-only)`) и сравнил адреса в нижнем регистре:
  - docs.robinhood.com `/chain/contracts/`: USDG;
  - developers.uniswap.org: страницы v3 и v4 deployments для Robinhood, Liquidity Launchpad deployments, Instant Launch, `deployments.json` (generatedAt 2026-09-22, источник Uniswap/contracts @ `a677c0d4`);
  - blog.uniswap.org: пост о pools.trade от 05.08.2026, адресов в нём нет;
  - docs.ponsfamily.com `/docs` и `/docs/v2`: адреса и сигнатуры событий;
  - docs.doppler.lol `/reference/contract-addresses`: полные адреса взял из ссылок на robinhoodchain.blockscout.com.

  Адреса из ответов агрегаторов (Bitquery, Mobula) и поисковой выдачи использовал только как подсказки. Ни один адрес не внесён только по ним, кроме PAIR, а у него помечено, что источник неофициальный.
- **topic0.** Посчитал keccak канонических сигнатур, собранных из исходников (коммиты указаны в `abi/*/SOURCE.md`), и сверил с данными. Все topic0, которые встречаются в данных, совпали; список в SOURCE.md. Topic0 V1 `TokenLaunched` из docs Pons совпал с keccak сигнатуры из репозитория. В `crates/decoders` `topics_are_canonical` есть только v3/v4 `Swap`, v4 `Initialize`, `Transfer` и token bridge. Событий launchpad'ов, v3 `PoolCreated`/`Initialize` и Doppler там нет. Это работа для indexer-engineer.
- **CREATE2 v3.** Init code hash `0xe34f199b…8b54` дал точные адреса для 3 из 3 `PoolCreated` в данных. Затем для каждого эмиттера v3 `Swap` взял пары токенов из `Transfer` в/из пула в той же tx и перебрал fee 100/500/2500/3000/10000. 1 275 эмиттеров вывелись, 189 нет.
- **pools.trade = Liquidity Launchpad.** Оба `Initialize` в tx с `TokenLaunched` (InstantLaunchStrategy) имеют hooks = 0, fee 2500, tickSpacing 25, currency0 = ETH. Это ровно параметры Instant Launch из docs Uniswap.
- **Doppler.** Все 687 событий `Swap` хука имеют `Swap` PoolManager с тем же PoolId в той же tx.
- **Двойники по адресу.** Среди 136 167 адресов из данных нет ни одного с совпадающими первыми 4 и последними 4 hex-символами.
- **RPC: 26 вызовов** на публичный `RPC_URL` (`https://rpc.mainnet.chain.robinhood.com`), с Mac, последовательно, пауза 1.2 с, UA `hoodchain-mev-registrar/0.1 (read-only; task 031)`. Все ответили HTTP 200, 429 не было. Состав:
  - 1 × `eth_blockNumber` (80 608 414);
  - 3 × `eth_call` USDG и 2 × `eth_getStorageAt` (EIP-1967);
  - 12 × `eth_getCode`;
  - 6 × прочих `eth_call`: `owner()` фабрики v3, `poolManager()` у трёх хуков, `factory()` у двух пулов. Итого вместе с USDG 9 × `eth_call`;
  - 2 × `eth_getLogs`: Pons v1 за 50 000 блоков, Pons v2 за 5 000, оба с фильтром по адресу и topic0.

  Сырые ответы лежат в `scratchpad/031/rpc_raw/calls.jsonl`. К фиду не подключался, кошельков нет.
- GitHub читал через `gh api` (только чтение): ponsdotdev/pons-labs @ `44a3db91`, Uniswap/v3-core @ `d0831dc6`, v4-core @ `46c68346`, liquidity-launcher @ `4007258d`, continuous-clearing-auction @ `6c9e559e`, whetstoneresearch/doppler @ `5754c7ee`. Код не запускал.

## Предполагается / не проверено

- **Blockscout не проверен ни для одного адреса** (API за Cloudflare, как в 016). Не проверено, что контракты верифицированы и имена совпадают. У USDG не известны верификация реализации и owner/admin. Admin-слот EIP-1967 пуст; что это UUPS — предположение.
- Что все 377 кривых с `CurveBuy` созданы фабрикой Pons v2: в выборке только 34 запуска.
- Что Pons v1 всё ещё используется: код есть, но событий нет ни в выборке, ни за последние 50 000 блоков (~1.4 ч).
- Связь pools.trade с конкретными адресами Liquidity Launchpad: блог называет механики (Instant Launch, Crowd Launch/CCA), а адреса дают docs Uniswap без слова «pools.trade». Связь подтверждена совпадением параметров на данных.
- `0xdc093dab…`: «игра или лотерея» — только интерпретация данных.
- PAIR (`0xf15f6ff9…`) и Relay (`0xb92fe925…`) определены по подсказкам из поиска.
- Двойники по `name`/`symbol` не искались.

## Найденные двойники и предупреждения

- v3-форки с тем же topic0 `Swap`: 189 эмиттеров и 956 логов. Две фабрики: `0x1ac9db4a2608ba45d6127b1737949b51bb54b7f3` и `0xe0c4ceb92d08ca985bb70fe0a22feb121a9854a8`.
- Хуки с событиями Pons, которых нет в docs Pons: `0x57387759ea3a3116330f4bd2cae48b03091a2044` (тот же PoolManager) и `0x332f85e7e323214b2d55286414b059ebb1f2a044`.
- Источники Uniswap расходятся по адресу Universal Router.
- Репозиторий Pons ссылается на разные GitHub-аккаунты, в `lib/` лежат посторонние файлы.
- pools.trade переехал на pools.xyz, и домен помечен в scam-базах.
- Doppler сам предупреждает: адреса не из его docs не канонические.

## Вопросы к Михаилу / Cowork

1. Посмотреть Blockscout по ссылкам 1–11. Переводить в `verified` USDG, v3 Factory, v4 PoolManager, Pons v2 factory и hook, LiquidityLauncher и ILS? Как быть с Pons v1 factory: код и docs есть, событий нет.
2. Universal Router: какой адрес считать каноническим, `0x8876…` (docs, 1 365 tx в данных) или `0x06af…` (deployments.json)? Или оба?
3. Включать ли Doppler (и фронтенды на нём) и PAIR в объём фазы 1b как «прочие launchpad'ы»? Doppler в данных заметнее pools.trade: 9 запусков против 2.
4. Нужна задача indexer-engineer: добавить topic0 из `abi/pons`, `abi/pools-trade`, `abi/doppler`, v3 `PoolCreated`/`Initialize` в `topics_are_canonical`. В декодерах фильтровать v3-пулы по фабрике (форки), а Doppler-`Swap` не считать вторым свопом.
5. Предлагаю внести в `chain-facts.md` проверенные на данных факты:
   - Instant Launch pools.trade = v4 hookless, fee 2500, tickSpacing 25, через LiquidityLauncher;
   - у Doppler-пулов dynamic fee и дублирующий `Swap` хука;
   - `0x364784d5…` — цены, а не сделки.

   Сам не вносил: по моим правам правлю только `contracts.md` и `abi/`.
