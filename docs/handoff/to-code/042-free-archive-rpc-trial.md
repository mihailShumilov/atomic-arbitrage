# 042 — Бесплатный архивный RPC: Dwellir и OrbitFlare
status: blocked
phase: 1b-подготовка
depends-on: 040
executor: indexer-engineer
reviewers: skeptic-analyst

## Зачем
Предложение Михаила 2026-10-05: форк (повтор блоков на revm) держать на бесплатном архивном RPC. Dwellir (dwellir.com/networks/robinhood, страница из поиска 2026-10-05) заявляет архивный узел Robinhood и debug-методы Nitro. В выдаче поиска было, что `eth_getLogs` доступен с тарифа Developer за $49; на бесплатном тарифе это не проверено. У OrbitFlare есть endpoint `robinhood.rpc.orbitflare.com` и ключ лицензии; лимиты и архив не описаны. Alchemy Free уже проверен в 040 (архив до 04.08, v4 `extsload`, `stateOverride`), это эталон для сравнения.

## Блокер
Нужны `DWELLIR_RPC_URL` и/или `ORBITFLARE_RPC_URL` в `.env` (регистрирует Михаил). Без них `status: blocked`.

## Что сделать (≤ 100 вызовов на каждого провайдера, жёсткий счётчик)
1. `eth_chainId` = 0x1237; голова; список доступных методов по факту (`eth_getLogs`, `eth_getBlockReceipts`, `debug_traceTransaction`, `debug_traceBlockByNumber`, `trace_*`, `eth_getProof`).
2. Архив: те же проверки, что в 040 (`eth_call` `slot0` v3, `extsload` v4, `eth_getStorageAt`, `eth_getBalance`, `stateOverride`) на 04.08 (блок 27176616 и 27607348) и на 1 месяц / 1 неделю назад; значения сверить с ответами 040 (`data/probes/040/`), то есть совпадение между провайдерами.
3. **Трассировки:** работает ли `debug_traceTransaction` (callTracer) на бесплатном тарифе; на 2–3 tx с внутренними переводами ETH — это закрывает пробел графа финансирования.
4. Лимиты: устойчивый rps на `eth_getStorageAt` (форк читает слоты по одному), короткие ступени до первого 429; суточная/месячная квота по документации.
5. Условия бесплатного тарифа (запрет коммерческого использования, хранения и т. п.).

## Что НЕ делать
- Ключи нигде не печатать, URL маскировать (`hood_core::redact` или своя маскировка). Ничего не покупать. Alchemy, публичный RPC, фид — 0 вызовов. Код в `crates/` не менять.

## Критерии приёмки
- Таблица по провайдерам × проверкам; совпадение архивных значений с 040; skeptic-analyst — вердикт.

## Формат отчёта
Как в 040. `status: done` после ревью, отдельный коммит.
