# 037 — Токенизированные акции как валюта котировки. Отчёт (черновик, часть contract-registrar)

- Дата: 2026-10-05.
- Исполнитель этой части: contract-registrar (п. 1 и п. 4 задачи). Пункты 2–3 (ранг котировки, `pools.rs`, прогон `swaps_scan`) — indexer-engineer, их здесь нет.
- RPC: `PROVIDER_RPC_URL` (ниже `<alchemy>`), **100 вызовов из 100**, только чтение (`eth_chainId`, `eth_getCode`, `eth_getStorageAt`, `eth_call`), пакетами. URL и ключ не печатались: клиент читает `.env` сам и в ошибках выводит только тип исключения.
- `crates/`, `sql/`, `data/` не менял. Не коммитил, статус задачи не менял.
- Изменено: `.claude/skills/hoodchain-mev/references/contracts.md` (5 строк в «Адреса», 2 пункта в «Наблюдения»), новая папка `abi/robinhood-stock-tokens/` (`SOURCE.md`, `StockToken.erc8056.json`).
- Scratch (не в git): `/private/tmp/claude-501/-Users-mihailshumilov-sites-my-crypto-atomic-arbitrage/a477d6fc-6af5-4e28-af3e-63445fb94d7a/scratchpad/037/`:
  - `stocks.tsv` — 194 адреса, колонки `address, symbol, decimals, source` (вход для реестра токенов);
  - `rpc.py` — клиент, счётчик в `calls.txt`;
  - `rpc_batch2.json` — сырые ответы основного пакета;
  - `seq_erc20.json`, `imdev16.md` — копии реестра;
  - `contracts.html`, `index.js`, `chunks/` — страницы и бандл docs.robinhood.com от 2026-10-05.

## Источники

1. **Официальный.** На https://docs.robinhood.com/chain/contracts/ таблица «Stock Tokens & Tokenized ETFs» не статична. JS страницы (`index-DhgXFtCP.js`) загружает её из `GET https://api.robinhood.com/rhj/assets` (поле `deployments[0].contractAddress`). Этот же endpoint описан на https://docs.robinhood.com/chain/stock-token-apis/ как реестр Stock Tokens. На странице есть предупреждение: «a token with a matching name/ticker but a different contract address is not a Robinhood Stock Token».
   - **С этой машины endpoint отдаёт 403**: «Your request has been blocked due to originating from a jurisdiction restricted by US sanctions». Через WebFetch тоже 403. Обходить блокировку не пытался.
   - Прямо в тексте docs адреса есть только у трёх токенов (примеры ответов API на /chain/stock-token-apis): AAPL `0xaf3d76f1834a1d425780943c99ea8a608f8a93f9`, P (Everpure) `0x1cdad396db64bda184d5182a97dd9b3c62100b7d`, APLD `0xb8dbf92f9741c9ac1c32115e78581f23509916fd`.
2. **Decimals.** Официально: https://docs.robinhood.com/chain/building-with-stock-tokens/ — «Stock tokens are ERC-20 (18 decimals)». Там же интерфейс ERC-8056 (`uiMultiplier()`, `UIMultiplierUpdated`, `TransferWithScaledUI`).
3. **Копии реестра** (не первоисточник, но обе ссылаются на `rhj/assets`):
   - [0xsequence/token-directory PR #158](https://github.com/0xsequence/token-directory/pull/158), merge `c54ee45dd4` от 2026-09-16, файл `index/robinhood/erc20.json`: 194 токена «… • Robinhood Token», у всех decimals 18, логотипы с `cdn.robinhood.com/ncw_assets/logos/<address>.png`;
   - [imDev2023/robinhood-chain-resources](https://github.com/imDev2023/robinhood-chain-resources), `robinhood-chain/16-token-contracts.md` @ `172809c08c`: 194 токена; приписка от 2026-09-23 про 195-й токен QNT `0xb7edfe2f33c1ac06830a971dfb559bde8a2a3d76`.
   - **Копии совпали по всем 194 адресам и символам.** Три адреса из docs есть в обеих копиях с теми же символами. QNT есть только в одной копии, поэтому в список не включён (открытый вопрос).
4. **Blockscout** (https://robinhoodchain.blockscout.com, URL из docs): 403, Cloudflare. Не проверено.

## Проверено (2026-10-05, как)

- **Состав списка** — офлайн-сверка двух копий реестра (скрипт, сравнение множеств адресов и символов): 194 = 194, расхождений 0.
- **Пересечение с данными 035** — офлайн по `data/registry/tokens-035.tsv` и `pools-035.tsv`:
  - в `tokens-035` 68 Stock Tokens;
  - в `pools-035` — **72** (4 токена — ELF, FIX, WDAY, LUNR — есть в пулах, но не в `tokens-035`);
  - пулов хотя бы с одной акцией: **684**. Из них 662 с одной акцией (194 в паре с USDG/WETH/ETH, 1 210 свопов; 468 с прочими токенами) и 22 «акция/акция» (66 свопов). Сумма по токенам в таблице ниже — 706, потому что пары «акция/акция» считаются дважды.
- **Код в цепи** (`eth_getCode`, `<alchemy>`, latest):
  - у всех 72 токенов из пулов и у трёх токенов из docs runtime-код **побайтно одинаковый** (sha256 совпадает): 283 байта, BeaconProxy со встроенным beacon `0xe10b6f6b275de231345c20d14ab812db62151b00`;
  - у AAPL/P/APLD EIP-1967 beacon-слот = этот адрес, impl-слот = 0;
  - `implementation()` beacon = `0xb35490d6f9163de4f80d88dc75c3516eb64c5ae2`, 11 614 байт, в коде есть селектор `uiMultiplier()` `0xa60bf13d` и topic0 `UIMultiplierUpdated`;
  - `owner()` beacon — revert.
- **View-функции** (`eth_call`):
  - `name()`: AAPL = «Apple • Robinhood Token», GME = «GameStop • Robinhood Token», SPY = «SPDR S&P 500 ETF Trust • Robinhood Token»;
  - `symbol()` AAPL = `AAPL`;
  - `uiMultiplier()` AAPL = 1 000 566 080 061 092 436 (≈1.000566, был дивиденд — как описано в docs);
  - `decimals()` = 18 у ELF, FIX, WDAY, LUNR, AAPL (037) и у 67 остальных из `tokens-035` (035) — **72 из 72**.
- **Двойники по адресу** — офлайн: ни у одного адреса из `tokens-035`/`pools-035` не совпадают первые 4 и последние 4 hex с каким-либо из 194 адресов. Таких нет.
- **Двойники по символу/имени**: среди 100 токенов `tokens-035` с известным символом (у остальных 1 898 символ не записан) нашлись три. `name()` прочитан через RPC:

| Адрес | `symbol` / `name()` | Пулов в pools-035 | Свопов | Решение |
|---|---|---|---|---|
| `0xf51fb54de60f6e16252e852a5ed0e60b8307606a` | NVDAx3L / «NVDA 3x Long» | 13 | 45 | `rejected` как Stock Token, котировкой не считать |
| `0x32ac8c1d7672667d5ebdea22935f7b06fc8d496f` | HOOD / «HOOD» | 14 | 27 | `rejected`: HOOD нет в реестре Stock Tokens |
| `0x9c11d47659087f196b7e02ffb26b47d23b631e18` | ROBINHOOD / «Robinhood» | 1 | 18 | `rejected`: использует бренд, в docs нет |

## Предполагается (не проверено)

- Что две копии точно повторяют `rhj/assets` на сегодня. Копии от 2026-09-16 и 2026-09-23, реестр мог измениться: например, QNT добавлен, а удаления и замены адресов возможны.
- Что одинаковый BeaconProxy-код — признак подлинности. Это сильный косвенный признак, но не доказательство: сторонний BeaconProxy может указывать на тот же beacon. Поэтому статус остаётся `observed`.
- Что у 122 токенов вне данных 035 decimals = 18. Это взято из docs и копий, RPC их не читал.
- Природа трёх `rejected`: NVDAx3L может быть левередж-токеном стороннего эмитента, а не подделкой; HOOD, вероятно, мем-токен. Для задачи важно одно: это не Stock Tokens и котировкой они не являются.
- Кто владеет beacon и может обновлять реализацию всех 194 токенов: `owner()` дал revert, Blockscout недоступен.

## Stock Tokens, встречающиеся в pools-035 (72 из 194)

Колонки «пулов» и «свопов» — по `pools-035.tsv` (`swaps_in_data`). `other` = v4 с хуком. Все 194 адреса — в `stocks.tsv` в scratch.

| # | Символ | Адрес | Пулов в pools-035 (v3 / v4 / other) | Свопов в этих пулах | decimals | В tokens-035 |
|---|---|---|---|---|---|---|
| 1 | GME | `0x1b0e319c6a659f002271b69db8a7df2f911c153e` | 25 (4 / 1 / 20) | 403 | 18 (rpc-035) | да |
| 2 | NVDA | `0xd0601ce157db5bdc3162bbac2a2c8af5320d9eec` | 81 (4 / 13 / 64) | 351 | 18 (rpc-035) | да |
| 3 | SPY | `0x117cc2133c37b721f49de2a7a74833232b3b4c0c` | 71 (7 / 28 / 36) | 245 | 18 (rpc-035) | да |
| 4 | SPCX | `0x4a0e65a3eccec6dbe60ae065f2e7bb85fae35eea` | 43 (5 / 5 / 33) | 214 | 18 (rpc-035) | да |
| 5 | SGOV | `0x92fd66527192e3e61d4ddd13322aa222de86f9b5` | 15 (3 / 3 / 9) | 109 | 18 (rpc-035) | да |
| 6 | AAPL | `0xaf3d76f1834a1d425780943c99ea8a608f8a93f9` | 23 (3 / 4 / 16) | 93 | 18 (rpc-037) | да |
| 7 | META | `0xc0d6457c16cc70d6790dd43521c899c87ce02f35` | 31 (2 / 5 / 24) | 86 | 18 (rpc-035) | да |
| 8 | GLD | `0xc9a981fee1f9dec688bb123ccdecc63d0debfc4e` | 21 (6 / 1 / 14) | 80 | 18 (rpc-035) | да |
| 9 | DJT | `0x1d11f0496982706c5e14a514d4e79f2e6bde4516` | 28 (4 / 3 / 21) | 75 | 18 (rpc-035) | да |
| 10 | AMC | `0x05a3d1cd21d0c88145e82600e62e7e496e0f222b` | 19 (1 / 6 / 12) | 62 | 18 (rpc-035) | да |
| 11 | GOOGL | `0x2e0847e8910a9732eb3fb1bb4b70a580adad4fe3` | 19 (3 / 4 / 12) | 61 | 18 (rpc-035) | да |
| 12 | PLTR | `0x894e1ec2d74ffe5aef8dc8a9e84686accb964f2a` | 15 (2 / 4 / 9) | 58 | 18 (rpc-035) | да |
| 13 | QQQ | `0xd5f3879160bc7c32ebb4dc785f8a4f505888de68` | 19 (2 / 1 / 16) | 53 | 18 (rpc-035) | да |
| 14 | RDDT | `0x05b37fb53a299a1b874a619e1c4c404d52c36f4c` | 20 (3 / 3 / 14) | 52 | 18 (rpc-035) | да |
| 15 | TSLA | `0x322f0929c4625ed5bad873c95208d54e1c003b2d` | 16 (3 / 3 / 10) | 46 | 18 (rpc-035) | да |
| 16 | USO | `0xa30fa36db767ad9ed3f7a60fc79526fb4d56d344` | 5 (3 / 1 / 1) | 37 | 18 (rpc-035) | да |
| 17 | MSFT | `0xe93237c50d904957cf27e7b1133b510c669c2e74` | 16 (1 / 5 / 10) | 36 | 18 (rpc-035) | да |
| 18 | COST | `0x4ea005168d7f09a7a0ba9d1def21a479950e44c2` | 13 (2 / 5 / 6) | 35 | 18 (rpc-035) | да |
| 19 | LLY | `0x8005d266423c7ea827372c9c864491e5786600ea` | 13 (4 / 3 / 6) | 35 | 18 (rpc-035) | да |
| 20 | MSTR | `0xec262a75e413fafd0df80480274532c79d42da09` | 15 (4 / 4 / 7) | 34 | 18 (rpc-035) | да |
| 21 | BE | `0x822cc93ffd030293e9842c30bbd678f530701867` | 5 (1 / 1 / 3) | 34 | 18 (rpc-035) | да |
| 22 | AMZN | `0x12f190a9f9d7d37a250758b26824b97ce941bf54` | 11 (1 / 2 / 8) | 30 | 18 (rpc-035) | да |
| 23 | TTWO | `0x5e81213613b6b86eab4c6c50d718d34359459786` | 11 (1 / 3 / 7) | 29 | 18 (rpc-035) | да |
| 24 | SNAP | `0xf6589f11bc40b669e584073f428b05562f568733` | 2 (0 / 1 / 1) | 29 | 18 (rpc-035) | да |
| 25 | LULU | `0x4e62068525ab11fe768e29dfd00ef909b9803016` | 13 (1 / 3 / 9) | 24 | 18 (rpc-035) | да |
| 26 | INTC | `0xc72b96e0e48ecd4dc75e1e45396e26300bc39681` | 7 (1 / 1 / 5) | 24 | 18 (rpc-035) | да |
| 27 | MU | `0xff080c8ce2e5feadaca0da81314ae59d232d4afd` | 7 (1 / 3 / 3) | 18 | 18 (rpc-035) | да |
| 28 | AMD | `0x86923f96303d656e4aa86d9d42d1e57ad2023fdc` | 9 (2 / 2 / 5) | 17 | 18 (rpc-035) | да |
| 29 | HIMS | `0xccee82fe024c36fa15e1005ede3e9e4787e23d09` | 8 (1 / 2 / 5) | 14 | 18 (rpc-035) | да |
| 30 | MRNA | `0x43b07d15ce533bec5476d70c22a78a1b2b662155` | 5 (2 / 0 / 3) | 13 | 18 (rpc-035) | да |
| 31 | PFE | `0x7066a64c24e4206cd62e83bf198c1e7eb361f51e` | 8 (3 / 2 / 3) | 12 | 18 (rpc-035) | да |
| 32 | SLV | `0x411efb0e7f985935daec3d4c3ebaea0d0ad7d89f` | 6 (4 / 0 / 2) | 10 | 18 (rpc-035) | да |
| 33 | RBLX | `0xf0c4bf4c582cb3836e98394b1d4e7b7281101be8` | 5 (2 / 1 / 2) | 10 | 18 (rpc-035) | да |
| 34 | BABA | `0xad25ac6c84d497db898fa1e8387bf6af3532a1c4` | 3 (1 / 1 / 1) | 10 | 18 (rpc-035) | да |
| 35 | QUBT | `0x59818904ab4ce163b3ce4ffb64f2d6ca02c434b4` | 6 (1 / 2 / 3) | 8 | 18 (rpc-035) | да |
| 36 | UPS | `0xf23250dac154d05bb671cb0d0ebef3c635c79ce2` | 5 (1 / 0 / 4) | 8 | 18 (rpc-035) | да |
| 37 | COIN | `0x6330d8c3178a418788df01a47479c0ce7ccf450b` | 4 (1 / 2 / 1) | 8 | 18 (rpc-035) | да |
| 38 | INDA | `0xacef2e09adb47ad6abebad9ff06689e60615c2b6` | 4 (0 / 3 / 1) | 8 | 18 (rpc-035) | да |
| 39 | TSM | `0x58ffe4a942d3885baa22d7520691f611ef09e7aa` | 4 (0 / 2 / 2) | 8 | 18 (rpc-035) | да |
| 40 | CRCL | `0xdf0992e440dd0be65bd8439b609d6d4366bf1cb5` | 5 (2 / 0 / 3) | 7 | 18 (rpc-035) | да |
| 41 | RIVN | `0xb1bf26c1d20ff267a4f93550d1e0d06ac40a114b` | 4 (1 / 1 / 2) | 7 | 18 (rpc-035) | да |
| 42 | SOFI | `0x98e75885157c80992a8d41b696d8c9c6fb30a926` | 3 (0 / 1 / 2) | 7 | 18 (rpc-035) | да |
| 43 | JNJ | `0x03dfbbe0ac4e7bcdafd08ed41a400326b77d8c80` | 6 (2 / 1 / 3) | 6 | 18 (rpc-035) | да |
| 44 | SNDK | `0xb90a19ff0af67f7779aff50a882a9cff42446400` | 5 (1 / 2 / 2) | 6 | 18 (rpc-035) | да |
| 45 | PENG | `0x9b23573b156b52565012f5ce02cdf60afbaa70be` | 3 (1 / 1 / 1) | 5 | 18 (rpc-035) | да |
| 46 | IBM | `0x980dcf6766fa79f5cf0c4aadb3ab477ff15a9619` | 3 (2 / 0 / 1) | 4 | 18 (rpc-035) | да |
| 47 | MRVL | `0x62fd0668e10d8b72339be2dcf7643001688ff13b` | 3 (1 / 1 / 1) | 4 | 18 (rpc-035) | да |
| 48 | SNOW | `0xba0cab75495255d0cb58e22b648bfed4ecd1f47e` | 3 (0 / 1 / 2) | 4 | 18 (rpc-035) | да |
| 49 | SOUN | `0x6e3dfd9f7e1649baa14d25cac18c94d62db10a54` | 3 (1 / 1 / 1) | 4 | 18 (rpc-035) | да |
| 50 | ADBE | `0x232b8ed6377be97813853b0ac104c4cda8378d1b` | 2 (0 / 1 / 1) | 4 | 18 (rpc-035) | да |
| 51 | BA | `0x4d21483a44bf67a86b77e3da301411880797d452` | 2 (1 / 0 / 1) | 4 | 18 (rpc-035) | да |
| 52 | SKHY | `0x84cab63bc87912e71ad199ff14a0ba45de68fef8` | 2 (1 / 0 / 1) | 4 | 18 (rpc-035) | да |
| 53 | ZM | `0x44c4f142009036cf477ed2d09932051843137cf1` | 2 (0 / 1 / 1) | 4 | 18 (rpc-035) | да |
| 54 | BB | `0x48e39e56acdba37b09020c0b734a613c9a2f100a` | 2 (0 / 1 / 1) | 3 | 18 (rpc-035) | да |
| 55 | RCAT | `0xfde6b5d9bb419b10c23268c74e369abff39c0460` | 2 (0 / 1 / 1) | 3 | 18 (rpc-035) | да |
| 56 | EWY | `0x7f0abef0c07280f82c6a08ead09ded6bae2c13fc` | 1 (0 / 0 / 1) | 3 | 18 (rpc-035) | да |
| 57 | CEG | `0xae517a2903e68bd929dfd15be875f8369d53e94a` | 2 (0 / 1 / 1) | 2 | 18 (rpc-035) | да |
| 58 | F | `0x25c288e6d899b9bc30160965ad9644c67e73be0c` | 2 (0 / 1 / 1) | 2 | 18 (rpc-035) | да |
| 59 | FIG | `0x41f4267525a8aff329540ef24fd83d9044758b33` | 2 (0 / 1 / 1) | 2 | 18 (rpc-035) | да |
| 60 | LMT | `0x329fcaceb9ad6f9580dd5f643fed0646900d043c` | 2 (0 / 1 / 1) | 2 | 18 (rpc-035) | да |
| 61 | MOD | `0xc6cbad1016b38b797610c25e1dc7d95988b1f362` | 2 (0 / 1 / 1) | 2 | 18 (rpc-035) | да |
| 62 | NFLX | `0xe0444ef8bf4ed74f74fd73686e2ddf4c1c5591e8` | 2 (1 / 1 / 0) | 2 | 18 (rpc-035) | да |
| 63 | ON | `0xbbd09f72b025360fee5c928053dca6248d35be54` | 2 (1 / 0 / 1) | 2 | 18 (rpc-035) | да |
| 64 | SHOP | `0xf53f66751b1eff985311b693531e3290f600c410` | 2 (0 / 1 / 1) | 2 | 18 (rpc-035) | да |
| 65 | ANET | `0x28babd556b60e53663b8615036479a29c2cdd1bf` | 1 (1 / 0 / 0) | 1 | 18 (rpc-035) | да |
| 66 | ASML | `0x47f93d52cbec7c6d2cfc080e154002370a60daea` | 1 (1 / 0 / 0) | 1 | 18 (rpc-035) | да |
| 67 | ELF | `0x39ec44bee4f6a116c6f9b8de566848a985c53c60` | 1 (0 / 0 / 1) | 1 | 18 (rpc-037) | нет |
| 68 | FIX | `0x93dbb1d2dc5d63f4abacff30485273f538df68ac` | 1 (0 / 1 / 0) | 1 | 18 (rpc-037) | нет |
| 69 | LUNR | `0xa5d4968421ba94814be3b136b15cf422101ac1a3` | 1 (0 / 0 / 1) | 1 | 18 (rpc-037) | нет |
| 70 | NU | `0x408c14038a04f7bd235329e26d2bf569ee20e250` | 1 (1 / 0 / 0) | 1 | 18 (rpc-035) | да |
| 71 | SPMO | `0xad622320e520de39e72d41ef07438c3fd3354875` | 1 (1 / 0 / 0) | 1 | 18 (rpc-035) | да |
| 72 | WDAY | `0x82da4646242e1d962e96e932269dc644c94a9caa` | 1 (0 / 0 / 1) | 1 | 18 (rpc-037) | нет |

Остальные 122 Stock Token в данных 035 не встречаются. Полный список — `stocks.tsv`.

## Кандидаты в `verified` для Михаила

Что проверено в цепи — выше. Для перехода в `verified` нужны скриншоты, сам я их получить не могу (403).

1. **Весь класс (194 адреса) одним скриншотом.** Откройте https://docs.robinhood.com/chain/contracts/ , блок «Stock Tokens & Tokenized ETFs». В этом блоке нужно сверить, что адреса совпадают с `stocks.tsv`, хотя бы у 72 токенов из таблицы выше. Лучше сохранить JSON `https://api.robinhood.com/rhj/assets` (из юрисдикции, где он не заблокирован) — тогда сверку сделаю скриптом.
2. **Топ по свопам — Blockscout** (ожидается: токен «… • Robinhood Token», decimals 18, контракт BeaconProxy, beacon `0xe10b…1b00`):
   - GME: https://robinhoodchain.blockscout.com/token/0x1b0e319c6a659f002271b69db8a7df2f911c153e
   - NVDA: https://robinhoodchain.blockscout.com/token/0xd0601ce157db5bdc3162bbac2a2c8af5320d9eec
   - SPY: https://robinhoodchain.blockscout.com/token/0x117cc2133c37b721f49de2a7a74833232b3b4c0c
   - SPCX: https://robinhoodchain.blockscout.com/token/0x4a0e65a3eccec6dbe60ae065f2e7bb85fae35eea
   - SGOV: https://robinhoodchain.blockscout.com/token/0x92fd66527192e3e61d4ddd13322aa222de86f9b5
   - AAPL (адрес есть и в docs): https://robinhoodchain.blockscout.com/token/0xaf3d76f1834a1d425780943c99ea8a608f8a93f9
   - META: https://robinhoodchain.blockscout.com/token/0xc0d6457c16cc70d6790dd43521c899c87ce02f35
   - GLD: https://robinhoodchain.blockscout.com/token/0xc9a981fee1f9dec688bb123ccdecc63d0debfc4e
3. **Beacon и реализация** — кто владелец (owner/admin) и верифицирован ли код:
   - beacon: https://robinhoodchain.blockscout.com/address/0xe10b6f6b275de231345c20d14ab812db62151b00
   - implementation: https://robinhoodchain.blockscout.com/address/0xb35490d6f9163de4f80d88dc75c3516eb64c5ae2

Предложение: если скриншот (1) совпадёт со `stocks.tsv`, перевести в `verified` весь класс. Обоснование: адреса из официального реестра плюс одинаковый код в цепи. Это по аналогии с правилом производных адресов для пулов, но решение за Михаилом.

## Вопросы

1. QNT (`0xb7ed…3d76`) — добавлять ли по одной копии реестра? В данных 035 его нет, так что на результат `swaps_scan` он не влияет.
2. Можно ли получить JSON `rhj/assets` (сохранённый файл) для сверки? Отсюда он гео-заблокирован.
3. Для indexer-engineer: события ERC-8056 `UIMultiplierUpdated` (`0x2205df45…b055`) и `TransferWithScaledUI` (`0x37e7f0db…3802`) — в `abi/robinhood-stock-tokens/SOURCE.md`. В `topics_are_canonical` их нет, по логам не сверялись. Для котировки они не нужны, но цена акции в «долях» зависит от `uiMultiplier` (сейчас AAPL ≈1.000566). Это стоит учесть, если цену акции переводить в USD.

---

# Часть indexer-engineer (п. 2–3 задачи)

- Дата: 2026-10-05. Исполнитель: indexer-engineer.
- Сеть: **0 вызовов RPC**, фид и сервер не трогал. Не коммитил, статус задачи не менял (`in-progress`).
- Источник адресов акций: `data/registry/rhj-assets-2026-10-05.tsv` (официальный `rhj/assets`, класс `verified` в `contracts.md`, Михаил 2026-10-05). Скрипт сборки дополнительно сверил его со `stocks.tsv` из части contract-registrar: 194 = 194 адреса, символы совпали.
- Scratch (не в git): `/private/tmp/claude-501/-Users-mihailshumilov-sites-my-crypto-atomic-arbitrage/a477d6fc-6af5-4e28-af3e-63445fb94d7a/scratchpad/037b/`:
  - `build.py` — сборка `tokens-037*.tsv`; `scan.sh` — прогон `swaps_scan`; `compare.py` — построчное сравнение;
  - `scan-before.txt` / `scan-mid.txt` / `scan-after.txt` (+ `obs-*` для режима `observed`), `rows-*.tsv`, `pool-swaps-after.tsv`.

## Сделано

1. **Ранг акций** (`crates/decoders/src/pools.rs`):
   - `STOCK_QUOTE_RANK = 3` (рядом с `ETH_QUOTE_RANK = 2`), doc: все Robinhood Stock Tokens, адреса — вход вызывающего (TSV), в код не зашиты; `uiMultiplier` не применяется.
   - `ADDRESS_TIEBREAK_MIN_RANK = STOCK_QUOTE_RANK`: с этого ранга две котировки одного ранга разрешаются по адресу — котировка = меньший адрес = `currency0` (инвариант `PoolRegistry`: `currency0 < currency1`). Ниже (1, 2) поведение прежнее: `ambiguous_quote` (пул ETH/WETH — обёртка, а не цена). Константная проверка `const _: () = assert!(…)` фиксирует, что акции ≥ порога, а ETH/WETH < порога.
2. **Сравнение** (`crates/decoders/src/swap_rows.rs`, `swap_row`): вместо «одинаковый ранг → `AmbiguousQuote`» — `match rank0.cmp(&rank1)`, при равенстве и ранге ≥ порога котировка `currency0`, иначе `AmbiguousQuote`. Doc модуля и варианта `AmbiguousQuote` обновлены.
3. **Тесты:**
   - юнит `same_rank_quotes_tie_break_by_address_from_stock_rank`: ранги 3, 4, 255 → котировка по меньшему адресу (сторона и цена проверены); ранги 1 и 2 → `AmbiguousQuote`; акция против ранга 2 остаётся токеном независимо от адресов;
   - на реальных логах `crates/decoders/tests/stock_quote.rs`, фикстура `crates/decoders/tests/fixtures/stock-pair-block.jsonl` (17 513 байт) — блок **39645654**, неизменённая строка `data/samples/hourly-20260804-20260930.jsonl.zst`, tx `0x84e981c3d2cd7c1db7338588490a35800ea195dc321253f6e0b7af7bb725e39d`, log 0, v4-пул `0x4174…9a7f` RDDT/SPY без хуков (строка из `pools-035.tsv`). Проверяется: котировка RDDT (меньший адрес), токен SPY, `sell`, trader/router, обе сырые суммы, цена в сырых единицах, `fee_quote` по fee 625 из события; без реестра акций тот же своп — `no_quote`. Суммы отдельно сверены Python-разбором лога: `Swap` amount0 = +43 051 670 910 510 244 (RDDT трейдеру), amount1 = −9 151 337 342 420 906 (SPY от трейдера), а также `Transfer` SPY трейдер → PoolManager и RDDT PoolManager → трейдер на те же суммы.
4. **Реестр токенов** (`data/`, не в git):
   - `data/registry/tokens-037.tsv` = `tokens-035.tsv` (1 998 строк) − 68 строк акций + 194 строки акций = **2 124** строки;
   - `data/registry/tokens-037-verified.tsv` = `tokens-035-verified.tsv` (USDG) + 194 акции = **195** строк;
   - строка акции: `адрес  18  3  verified  symbol=…;uiMultiplier=…;src=contracts.md+rhj/assets-2026-10-05`;
   - дубли с 035: у всех 68 акций в `tokens-035` было `decimals 18`, ранга нет, `observed` (скрипт проверил это assert'ом) — их строки **заменены** строкой акции, расхождений по decimals нет. ELF, FIX, WDAY, LUNR, которых в `tokens-035` не было, добавлены как остальные;
   - три `rejected` (NVDAx3L `0xf51f…606a`, HOOD `0x32ac…496f`, ROBINHOOD `0x9c11…1e18`) оставлены строками 035 без ранга; скрипт проверяет, что их нет в `rhj/assets`, и что в итоговых строках они ни разу не котировка.
5. **`data-model.md`** («hood.swaps» → «quote / token», «Соглашения»): ранги 1/2/3, правило «один ранг → меньший адрес с ранга 3», исключения ETH/WETH, `rejected`-двойники; **акции = токены × `uiMultiplier`, `hood.swaps` его не применяет**, `price` — за целый токен, не за акцию; 46 из 194 с `currentMultiplier` ≠ 1, CRWD = 4; перевод — по множителю на момент блока, не по `currentMultiplier`; числа прогона.

## Числа `swaps_scan`

Входы — как в отчёте 036: 8 файлов (7 `data/blocks` по возрастанию + `data/samples/hourly-…`), 2 712 блоков, 13 530 свопов; `--pools data/registry/pools-035.tsv --v4-manager 0x8366a39cc670b4001a1121b8f6a443a643e40951=verified`, встроенный хук Pons v2, режим `verified`. «До» = `tokens-035-verified.tsv` (вывод побайтно совпал с `scan-after-r1.txt` из 036, и после изменения кода — тоже, rows-TSV идентичен). «Промежуточно» = реестр 037 на старом коде (без правила по адресу). «После» = реестр 037 + новое правило.

| venue | до | промежуточно | после | Δ |
|---|---|---|---|---|
| uni_v3 | 5 854 | 5 860 | 5 866 | +12 |
| uni_v4 | 2 834 | 2 889 | 2 949 | +115 |
| pons_v2_hook | 1 242 | 1 895 | 1 895 | +653 |
| other | 680 | 1 206 | 1 206 | +526 |
| **всего строк** | **10 610** | 11 850 | **11 916** | **+1 306** |

| пропуск | до | после |
|---|---|---|
| v3 no_pool_meta | 956 | 956 |
| v3 no_quote | 51 | 39 |
| v3 zero_amount | 1 | 1 |
| v4 no_pool_meta | 382 | 382 |
| v4 no_quote | 1 514 | 220 |
| v4 zero_amount | 16 | 16 |
| ambiguous_quote | 0 | 0 (на старом коде: v3 6 + v4 60) |
| **всего** | **2 920** | **1 614** |

- **Добавлено 1 306 строк**, все с котировкой-акцией: 1 240 «акция / прочий токен» (other 526, pons_v2_hook 653, uni_v4 55, uni_v3 6) и **66 «акция/акция»** (uni_v4 60, uni_v3 6; совпадает с 66 свопами в 22 пулах из части contract-registrar). Во всех 66 котировка — меньший адрес (assert в `compare.py`); чаще всего SPY (`0x117c…`), есть RDDT/SPY, SLV/GLD, CRCL/MSTR, AMZN/AAPL.
- Из 668 `no_quote` пулов хука Pons v2 (отчёт 036) строками стали 653; остальные 15 — в остатке ниже.
- **Прежние строки:** ни одна не пропала, `token`/`quote`/`venue`/суммы не изменились. Изменилась только `price` в **1 210** строках «акция / USDG|ETH|WETH»: была `nan` (акций не было в verified-реестре), стала числом. Перекладки ключа `(token, …)` для старых строк нет.
- Котировки после: ETH/WETH 6 459, USDG 4 151, акция 1 306.
- У 1 240 новых строк «акция / прочий» `price` = `nan`: у прочего токена нет `verified` decimals (в режиме `observed` с `tokens-037.tsv` таких 212 из всех `price_unknown` 1 213). У 580 новых строк котировка — акция с `uiMultiplier` ≠ 1 (цена в токенах, не в акциях).
- **Остаток `no_quote` 259 (v3 39 + v4 220), все — пулы без котировки на обеих сторонах:**
  - 40 свопов с `rejected`-двойником на одной стороне: NVDAx3L 24 (8 пулов), HOOD 16 (11 пулов) — котировкой не стали, как требовалось;
  - 219 — пулы «прочий/прочий» (98 пулов); чаще всего в них AI `0x2e8c…1e18` (77), OPEN `0xf1b2…1e18` (22), ANTHROPICx1L `0x1937…6c33` (21) — все `observed`, не в `contracts.md`.
- `no_pool_meta` (1 338) и `zero_amount` (17) не изменились — к 037 не относятся.
- Режим `observed` (`tokens-035.tsv` → `tokens-037.tsv`): строки 10 610 → 11 916, по площадкам и пропускам — те же числа, что в `verified`.

## Проверено (2026-10-05, как)

- `cargo fmt -p decoders -- --check` — чисто (fmt запускал только `-p decoders`).
- `cargo build --workspace` — ок; `cargo clippy --workspace --all-targets -- -D warnings` — ок.
- `cargo test --workspace` — 272 passed, 0 failed; decoders lib 44 (было 43), `tests/stock_quote.rs` 2 (новый).
- Прогоны `swaps_scan` и построчное сравнение — как описано выше; финальный прогон повторён после последней правки кода — вывод и rows-TSV побайтно те же.

## Предполагается (не проверено)

- Множитель меняется во времени: у SPY `currentMultiplier` ≈ 1.0017, а в фикстуре (блок 39645654) `TransferWithScaledUI` SPY и RDDT несут два равных числа — то есть множитель тогда был 1. Это по одной tx; если считать акции, нужен множитель на блок (`UIMultiplierUpdated`). Заодно: topic0 `TransferWithScaledUI` `0x37e7f0db…3802` реально встречается в логах акций (логи 2 и 4 этой tx) — сверено с `abi/robinhood-stock-tokens/SOURCE.md`; в `contracts.md`/`chain-facts.md` не вносил (это реестр contract-registrar), кандидат на заметку.
- Цены «акция/акция» (например, NVDA ≈ 0.29 SPY) выглядят правдоподобно, но с внешними котировками не сверялись.

## Вопросы

1. Правило по адресу намеренно не распространено на ранги 1–2 (ETH/WETH, будущие пары стейблкоинов остаются `ambiguous_quote`). Если Михаил хочет, чтобы пара двух стейблкоинов тоже получала строку — это смена `ADDRESS_TIEBREAK_MIN_RANK` на 1, одна строка + тесты.
2. Встроить ли 194 акции в код (как ETH/WETH) или оставить входом через TSV? Сейчас — TSV, по образцу USDG.

## Изменённые файлы (часть indexer-engineer)

- `crates/decoders/src/pools.rs` — `STOCK_QUOTE_RANK`, `ADDRESS_TIEBREAK_MIN_RANK`, const-проверка, doc `TokenEntry::quote_rank` и `ETH_QUOTE_RANK`
- `crates/decoders/src/swap_rows.rs` — правило сравнения, doc, юнит-тест
- `crates/decoders/tests/stock_quote.rs` (новый)
- `crates/decoders/tests/fixtures/stock-pair-block.jsonl` (новый, 17 513 байт)
- `.claude/skills/hoodchain-mev/references/data-model.md`
- `data/registry/tokens-037.tsv`, `data/registry/tokens-037-verified.tsv` (новые, не в git)

Нужны ревью: **data-auditor** (изменилось правило выбора котировки и реестр: 10 случайных новых строк сверить с сырыми логами; rows — `scratchpad/037b/rows-after.tsv`, аудит-TSV — `pool-swaps-after.tsv`) и **architect-reviewer** (код).

## Доработка по ревью architect-reviewer (2026-10-05)

Ревью: `docs/reviews/037-stock-tokens-quote-architect-reviewer.md`, PASS, блокирующих и важных замечаний нет. По решению координатора исправлены Р1, Р2, Р4, Р6 и pedantic-замечания к документации. Р3 и Р5 пропущены. Вопрос о стейблкоинах: ничего не меняем, для рангов 1–2 остаётся `ambiguous_quote`.

- **Р1** (`swap_rows.rs`, `swap_row`): ранги теперь сравниваются как `u8` (`rank_of` = `tokens.get(c).and_then(|t| t.quote_rank)`). Сравнения `Option` больше нет, поведение то же.
- **Р2** (`pools.rs`): в const-проверке оставлено одно условие с сообщением: `assert!(ETH_QUOTE_RANK < ADDRESS_TIEBREAK_MIN_RANK, "an ETH/WETH pool is a wrap, not a price")`.
- **Р4** (тесты `swap_rows.rs`): добавлен хелпер `tokens(&[(addr, decimals, rank)])`, через него переписаны `both`/`ranked` в `status_quote_and_pool_rules`. Тест разбит на три: `same_rank_from_threshold_is_priced_in_the_lower_address` (ранги порог, порог+1, 255), `same_rank_below_threshold_is_ambiguous` (`1..ADDRESS_TIEBREAK_MIN_RANK`), `stock_against_lower_rank_is_token`. Дубль значения 3 в массиве убран.
- **Р6 и замечания к документации:**
  - подробное правило описано только в doc `ADDRESS_TIEBREAK_MIN_RANK` и в `data-model.md`; doc модуля `swap_rows` и `SkipReason::AmbiguousQuote` теперь только ссылаются на него;
  - в doc `ETH_QUOTE_RANK`, `STOCK_QUOTE_RANK` и `ADDRESS_TIEBREAK_MIN_RANK` первым абзацем идёт короткое определение;
  - в doc `STOCK_QUOTE_RANK` теперь сказано, что акция — котировка против любого токена без ранга, а для пар «акция/акция» дана ссылка на порог;
  - в `tests/stock_quote.rs` у `PoolKey` и `PositionManager` добавлены обратные кавычки.
- **Проверено (2026-10-05):**
  - `cargo fmt -p decoders -- --check` — чисто;
  - `cargo clippy -p decoders --all-targets -- -D warnings` — 0 предупреждений;
  - `pedantic`/`nursery` по изменённым строкам doc — без `too_long_first_doc_paragraph` и `doc_markdown`;
  - `cargo test -p decoders` — 0 failed, lib 46 (было 44: −1 тест, +3), `stock_quote` 2;
  - `swaps_scan` на тех же входах с `tokens-037-verified.tsv`: `cmp` с `scratchpad/037b/scan-after.txt`, `rows-after.tsv` и `pool-swaps-after.tsv` — **побайтно одинаково**;
  - контрольный прогон с `tokens-035-verified.tsv` побайтно совпал со `scan-before.txt` и `rows-before.tsv`;
  - сеть не использовалась, `target/` очищен.
