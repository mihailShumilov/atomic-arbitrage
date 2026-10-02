# 027 — decoders: хвосты ревью. Ревью data-auditor

date: 2026-10-03
reviewer: data-auditor
объект: рабочее дерево поверх HEAD `3b8e6a4` (не закоммичено): `crates/decoders/src/{model,swaps,rows,events,arbitrum}.rs`, `src/l1_inflows/{mod,registry,types}.rs`, `tests/l1_inflows.rs`, `examples/l1_inflows_scan.rs`, новый `crates/decoders/clippy.toml`, одна строка в `.claude/skills/hoodchain-mev/references/data-model.md`. Отчёт: `docs/handoff/from-code/027-decoders-followups.md`.

**Вердикт: PASS.** Критерий приёмки выполнен. Вывод `l1_inflows_scan` побайтно прежний в 14 из 16 файлов. Отличаются 2 файла, оба в прогоне «фикстура 017 + `--no-builtin`»: это одна документированная запись `unregistered_gateway_eth`. Её семантика сверена с сырой транзакцией. Строгий разбор `0x` не отверг ни одного из 2 712 локальных блоков. Вывод `swaps::decode_block` идентичен HEAD. Замечания З1–З3 не блокируют.

## Как проверял

- Сеть: RPC 0, фид 0, ssh 0. Код репозитория не менял, `cargo clean` в репозитории не запускал. Записан только этот файл.
- Сборки (release, `--offline`, `-p decoders --examples`) — в отдельных `CARGO_TARGET_DIR` в `scratchpad/027-audit/`. После проверки удалены.
  - HEAD: `git archive HEAD` → `scratchpad/027-audit/head/`.
  - Рабочее дерево: снимок `rsync` (без `target`, `data`, `.git`) → `scratchpad/027-audit/wt/`. В конце проверки снимок `crates/decoders` сверен с репозиторием (`diff -r`): идентичен, кроме моего временного примера. Крейт `decoders` не зависит от `hood-core`, поэтому параллельные правки recorder/hood-core/sql на результат не влияют.
- Входы:
  - фикстура 017 `tests/fixtures/l1-inflows-blocks.jsonl` (4 блока);
  - фикстура свопов `tests/fixtures/swaps-blocks.jsonl` (1 блок);
  - `data/blocks/*.jsonl.zst` (7 файлов) + `data/samples/hourly-20260804-20260930.jsonl.zst`: 2 712 блоков, 34 344 tx.
- Свопы: собственный одноразовый пример `swapdump` (только в scratchpad). Он печатает `Debug` каждого `PoolSwap`, каждого `MalformedSwap` и счётчики `decode_block` по каждому блоку, а также каждую ошибку `parse_block_line` (без остановки).

## Результаты

| # | проверка | объём | итог |
|---|---|---|---|
| 1 | `l1_inflows_scan`: HEAD против рабочего дерева, 4 режима (фикстура / фикстура `--no-builtin` / data / data `--no-builtin`); `cmp` stdout, stderr, `--edges-out`, `--unaccounted-out` | 16 пар файлов | **PASS**: 14 совпали; 2 отличаются (stdout и unaccounted TSV в режиме «фикстура + `--no-builtin`») ровно на одну строку `unregistered_gateway_eth` |
| 2 | Семантика записи З2 против сырья | блок 77312169, tx_index 2 | **PASS** (подробно ниже) |
| 3 | Нет двойного учёта с токенной строкой | фикстура + синтетика | **PASS** |
| 4 | Строгий `0x` не отвергает реальные данные | 2 712 блоков + 5 блоков фикстур | **PASS**: `parse_errors=0`, сканер exit 0 во всех 8 прогонах |
| 5 | Строгий `0x` реально строже HEAD | 3 синтетические строки | **PASS** |
| 6 | `swaps::decode_block` HEAD против рабочего дерева | 2 717 блоков, 16 250 строк вывода | **PASS**: `cmp` идентичен; 13 532 свопа (13 530 на data + 2 в фикстурах), malformed 0 |
| 7 | `cargo test -p decoders` | — | **PASS**: lib 19 + `l1_inflows` 23 + `rows` 1 + `swaps` 8 = 51, 0 failed |
| 8 | `cargo clippy -p decoders --all-targets -- -D warnings`, `cargo fmt -p decoders -- --check` | — | **PASS**: exit 0 |

### 2. Запись `unregistered_gateway_eth`: сверка с сырьём

Строка в unaccounted TSV (рабочее дерево, фикстура, `--no-builtin`):
`77312169  2  0x8a448fd9…5d4a  unregistered_gateway_eth  0x1d187c3e2da52d72bc9c41e3aba0fdfa6a7bf055  104126314999636103765  0xf7e12b96…cf1b`.

Сырьё фикстуры (собственный разбор JSON, Python):
- tx `0x8a448fd9…5d4a`: `type 0x68`, `status 0x1`;
- `to` = `0x1d187c3e…f055`, совпадает с `addr` записи;
- `value` = 104126314999636103765, совпадает с суммой записи;
- логи: `Transfer` ×2 от L2 WETH на ту же сумму и `DepositFinalized` (topic0 `0xc7f2e9c5…`) от самого `to` на ту же сумму. Сумма токенов = `tx.value`.
- В режиме `--no-builtin` реестр пуст, значит `to` вне реестра. Все условия З2 выполнены.
- Со встроенным реестром (шлюз `verified`) записи нет. Это проверено и прогоном, и тестами `kind9_weth_gateway_builtin_verified` и `synthetic_registry_observed_entry` (`observed`).

Код (`l1_inflows/mod.rs`, `retry`): новая ветка — `else if` к `GatewayEthUnexplained`. Она срабатывает только при `tx.value ≠ 0`, `token_sum == Some(tx.value)` и `registry.get(&to).is_none()`. Реестр ключуется по адресу шлюза (`GatewayEntry::gateway`), то есть это то же условие, что даёт `gateway_status = none` у токенных строк.

### 3. Двойной учёт

- Запись З2 — это строка «не учтено», а не ребро. В `--edges-out` режима «фикстура + `--no-builtin`» нет строки `l1_eth` на `0x1d18…` (обе сборки одинаковы). Есть только `l1_token` на получателя `0x07ae…` с `gateway_status = none`. Внутри декодера ETH и токен одновременно рёбрами не становятся.
- Синтетика (копия блока 77312169 в scratchpad, `--no-builtin`):
  - `value + 1` → только `gateway_eth_unexplained`, `unregistered_gateway_eth` нет. HEAD и рабочее дерево одинаковы;
  - `value = 0` → ни одной из двух записей.
  Взаимоисключение с `gateway_eth_unexplained` подтверждено.

### 5. Строгость разбора (синтетика, сканер)

| мутация | HEAD | рабочее дерево |
|---|---|---|
| `from` без `0x` | принят (exit 0) | ошибка (exit 1) |
| `block.hash` с префиксом `0X` и hex в верхнем регистре | принят | ошибка |
| topic с `0x0x…` (длина 66) | ошибка | ошибка |

Все строковые поля `RawTx`/`RawReceipt`/`RawLog`/`RawBlock` проходят через `parse_addr`/`parse_b256`/`parse_bytes`/`quantity_*` (проверено чтением `model.rs`). Обходных `parse::<Address>` не осталось.

## Замечания (не блокируют)

- **З1. Двойной учёт возможен ниже по конвейеру.** Сам декодер не считает дважды. Но если загрузчик или аналитика когда-нибудь возьмут строки `l1_token` с `gateway_status = none` и одновременно засчитают `unregistered_gateway_eth` как приход, одна и та же сумма войдёт дважды: токеном на получателя и ETH на `to`. Правило «либо одно, либо другое» стоит записать в задаче загрузчика `funding_edges` / таблицы «не учтено» (это вопрос 1 отчёта).
- **З2. Ложные срабатывания по построению.** Запись означает «возможный приход», а не «приход». Прогон фикстуры с `--no-builtin` это и показывает: настоящий WETH-шлюз выглядит как неизвестный контракт. При расширении реестра (`observed` шлюзы через `--gateways`) счётчик будет падать. Это корректно, но отчёты должны печатать его вместе с размером реестра.
- **З3. Объём проверки.** На `data/` L1-сообщений 0, поэтому ветка З2 на реальных данных проверена только на одной транзакции из фикстуры. На большом архиве её не прогоняли (как и в 017).

## Проверено и предполагается

- **Проверено (2026-10-03, Mac, офлайн, мои прогоны):** п. 1–8 таблицы. Метод: две независимые release-сборки (HEAD из `git archive`, снимок рабочего дерева), `cmp` всех выходов, разбор сырья фикстуры на Python, синтетические мутации в scratchpad, чтение диффа `git diff HEAD -- crates/decoders`.
- **Предполагается:**
  - что RPC Robinhood Chain всегда отдаёт адреса и хэши с `0x` в нижнем регистре. Подтверждено только на 2 712 локальных блоках, не на полном архиве;
  - утверждение отчёта о `const-hex` 1.19.3 (снятие `0x` внутри `decode`) я не перепроверял по исходнику. Новое поведение закреплено тестом `parse_bytes("0x0x00").is_err()`, тест проходит.
