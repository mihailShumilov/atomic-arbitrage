# 016 — Реестр: WETH на L2 и L2-шлюз WETH → observed
status: done
phase: 1b-подготовка
depends-on: 014
executor: contract-registrar
reviewers: — (`verified` ставит Михаил)

## Зачем
В 014 в блоке 77312169 (авто-redeem kind 9, tx `0x68`) встретились адреса, похожие на L2-шлюз WETH и WETH на L2. Решение Михаила 2026-10-01: внести в реестр со статусом `observed`.

## Контекст
- `docs/handoff/from-code/014-server-housekeeping-kind12.md` (раздел про kind 9, блок 77312169): `0x0bd7d308f8e1639fab988df18a8011f41eacad73` (mint токена, вероятно WETH на L2), `0x1d187c3e…f055` (вероятно L2-шлюз WETH; полный адрес — в сырых ответах RPC 014 или в логах блока 77312169), `DepositFinalized` с topic1 `0xc02aaa39…6cc2` (WETH на L1).
- `.claude/skills/hoodchain-mev/references/contracts.md` — правила статусов и источников.

## Что сделать
1. Установить полные адреса и роли по данным (логи блока 77312169) и официальным источникам (docs.robinhood.com, Arbitrum token bridge, Blockscout), отличить от фишинговых двойников.
2. Записать в `contracts.md` со статусом `observed` (или `verified-candidate`, если источники это позволяют), с источниками и датой. Сверить с уже записанной позицией WETH, если она есть.
3. Вызовов RPC — не больше 10 (с Mac, ≤ 1/с, свой User-Agent); к фиду не подключаться.

## Критерии приёмки
- Записи в `contracts.md` с источниками; Михаил решает про `verified`.

## Формат отчёта
Что найдено; источники; статус; вопросы. `status: done`, отдельный коммит.
