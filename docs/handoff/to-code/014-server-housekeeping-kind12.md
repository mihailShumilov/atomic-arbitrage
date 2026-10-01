# 014 — Сервер: RAID-алерт, UTC, текст healthcheck; разбор kind 12
status: done
phase: 1a (п. 1–3), 1b-подготовка (п. 4)
depends-on: 013
executor: infra-ops (п. 1–3), indexer-engineer (п. 4)
reviewers: indexer-engineer (п. 1–3), data-auditor (п. 4)

## Зачем
Закрыть мелочи после деплоя, пока идёт 7-суточная запись, и выяснить смысл новых сообщений фида kind 12 до написания декодеров 1b.

## Контекст
- Отчёт и аудиты 013 (`docs/handoff/from-code/013-server-deploy.md`, `docs/reviews/013-*`).
- Запись на сервере идёт с 2026-10-01 ~09:57Z; resync RAID ещё идёт — **сервер не перезагружать, recorder не перезапускать**.
- kind 12: ~4 в час; data-auditor 013: payload похож на 20 байт адреса + 32 байта суммы (~0.1 ETH), отправитель — alias. Предположение: `L1MessageType_EthDeposit` по нумерации Nitro (3 L2Message, 9 SubmitRetryable, 12 EthDeposit, 13 BatchPostingReport) — **не сверено**.

## Что сделать
1. **RAID-алерт.** Деградация md-массива должна доходить через наш notifier:
   - `mdadm --monitor` (`mdmonitor`/`mdadm.service` в 26.04 — проверить имя) с `PROGRAM` → обёртка над `deploy/notify.sh` (события Fail, DegradedArray, SpareActive, RebuildFinished — последнее как INFO);
   - плюс проверка `raid` в `healthcheck.sh` по `/proc/mdstat` (`[U_]`/`[_U]` → ALERT, resync/recovery → INFO с процентом, без спама);
   - всё в `deploy/` + `bootstrap.sh` (идемпотентно) + тест на подставном `/proc/mdstat`; применить на сервере без перезагрузки и без рестарта recorder.
2. **UTC.** `timedatectl set-timezone Etc/UTC` на сервере и тот же шаг в `bootstrap.sh`. Убедиться, что recorder (метки в UTC/ns) и таймеры (`feed-audit` в 00:10 UTC) не затронуты; рестарт recorder не нужен — проверить, что его не происходит.
3. **Текст healthcheck.** Убрать ложную подсказку «только ping?» из сообщения «восстановлено» (оставить её только в ALERT, где она по делу, если по делу). Тест на тексты уведомлений. Развернуть на сервере обновлением скрипта, без рестарта recorder.
4. **kind 12 — выяснить, что это** (офлайн + ≤ 20 вызовов RPC, с Mac, не с сервера):
   - по исходникам Nitro (версия, совместимая с ArbOS 61): константы `L1MessageType_*`, формат payload для 12, какую L2-транзакцию он порождает (ожидается `ArbitrumDepositTx`, type `0x64`), ссылки на файл/строку;
   - на копиях закрытых часовых файлов с сервера (rsync на Mac, только чтение): все kind 12 за доступные часы — разбор payload, `header.poster/sender`, `requestId`, L1-блок;
   - для 5 из них — блок L2 через RPC: есть ли tx `0x64`, совпадают ли получатель и сумма, как выглядит в чеке (логи? gasUsed?);
   - частота kind 9/12/13 за сутки записи, распределение сумм kind 12;
   - итог — в `chain-facts.md` (проверенное с датой; остальное «предполагается») и в `references/data-model.md` раздел «L1-сообщения фида» с таблицей kind → смысл → какая tx в блоке → что брать декодерам (в т.ч. для графа финансирования: депозиты с L1 = вход денег на кошельки).

## Что НЕ делать
- Не перезагружать сервер; не перезапускать recorder (если без рестарта никак — остановиться и спросить Михаила).
- С сервера — никаких подключений к фиду кроме recorder и никаких вызовов RPC.
- Не менять код recorder и формат сырья.

## Критерии приёмки
- shellcheck и `deploy/test/` чистые; на сервере: `systemctl status` mdmonitor/healthcheck ок, `timedatectl` = UTC, `systemctl show recorder -p NRestarts,ActiveEnterTimestamp` — без изменений с 013.
- Тестовое уведомление RAID через `mdadm --monitor --test` (или эквивалент) дошло в journald (и Telegram, если уже настроен).
- data-auditor: PASS по п. 4 — ссылки на Nitro проверены, 5/5 kind 12 сопоставлены с L2-tx, таблица в `data-model.md` согласована с данными.

## Стоимость и риски
$0; ≤ 20 вызовов RPC с Mac. Риск — случайный рестарт recorder при правках на сервере (контролировать по `NRestarts`).

## Формат отчёта
Сделано; проверено и как; таблица kind; что не получилось; вопросы. `status: done`, отдельный коммит.
