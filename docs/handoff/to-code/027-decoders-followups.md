# 027 — decoders: хвосты ревью
status: done
phase: 1b
depends-on: 022
executor: indexer-engineer
reviewers: architect-reviewer, data-auditor

## Зачем
`docs/reviews/022-*` (Р1 вторая часть, Р7, мелочи), `017-decoder-l1-inflows-data-auditor.md` (З2).

## Что сделать
1. Адреса и хэши без префикса `0x` — ошибка (как уже для quantity).
2. З2 (017): контракт вне реестра испускает `DepositFinalized` и забирает ETH-строку → не терять приход: счётчик/запись «не учтено», если `tx.value` = `amount` и строка ушла в токен со статусом `none`.
3. Р7: позиционные аргументы замыкания `base` в `swaps.rs` → структура/именованные поля.
4. Дублирующая строка про переполнение в `examples/l1_inflows_scan.rs`.
5. Выборочно pedantic: `#[must_use]`, `# Errors`, бэктики в doc для публичного API.

## Что НЕ делать
- Не менять вывод сканера на текущих данных (кроме счётчика З2, если сработает — на данных не ожидается). RPC — 0.

## Критерии приёмки
- fmt/clippy/test чистые; вывод сканера на фикстурах 017 и `data/` побайтно прежний; data-auditor и architect-reviewer: PASS.

## Формат отчёта
Сделано; проверено и как. `status: done`, отдельный коммит.
