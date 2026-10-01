# 012 — Исправления перед запуском на сервере
status: done
phase: 1a
depends-on: 009, 011
executor: indexer-engineer (п. 1–3, 5), infra-ops (п. 4, 6)
reviewers: data-auditor (п. 1–3), indexer-engineer (п. 4, 6)

## Зачем
Закрыть замечания аудитов 009 и 011, из-за которых на сервере возможны быстрые переподключения (риск бана) или незамеченное «залипание» фида. Без сети, до получения IP сервера.

## Контекст
- `docs/handoff/from-code/009-recorder-requested-seq.md` (вопросы 1–3), `docs/reviews/009-recorder-requested-seq-data-auditor.md` (Н1–Н5).
- `docs/handoff/from-code/011-deploy-kit.md` (вопросы 1–2), `docs/reviews/011-deploy-kit-indexer-engineer.md` (З1).

## Решения Cowork по вопросам
- 009 в.1 — да: интервал считать от **конца** прошлой сессии.
- 009 в.2 — строка `backlog` отдельно — принято.
- 009 в.3 — да: проверка `chronyc tracking` в чек-листе деплоя (если ещё нет).
- 011 в.2 — да, оба пункта.

## Что сделать
1. **`--min-connect-interval-secs` от конца сессии.** Момент конца = время последней строки `connections.tsv` любого типа (`disconnected`, `client_close`, `shutdown`, …); если после последнего `connected` строк нет (kill -9) — mtime последнего часового файла данных. Ждать `max(остаток паузы, интервал − (now − конец))`. Тесты на все три случая.
2. **Н1.** Ошибка writer'а (диск, fsync): перед выходом с кодом 2 — сигнал stop в сетевой цикл, Close 1000, ожидание ≤ 2 с. Тест на моке (имитация ошибки записи).
3. **Залипание «только ping» (З1-б).** Флаг `--block-idle-timeout-secs` (по умолчанию 30): нет сообщений с seq > 0 дольше порога — Close и переподключение с досылкой; причина `block_idle` в `connections.tsv`. Тест на моке.
4. **Healthcheck (З1-а).** Возраст `feed` — по mtime `last_seq.txt`; часовые файлы — только если `last_seq.txt` нет. Тест «файл часа свежий, `last_seq.txt` старый → ALERT».
5. **enricher (011 в.2).** Отдельный код выхода «исчерпан `--max-calls`» (например, 75) и `SuccessExitStatus` в `enricher-gaps.service`; `--gaps` игнорирует недописанную последнюю строку без `\n` (с WARN), а не падает. Тесты.
6. **Мелочи:** Н3–Н5 в `chain-facts.md` / `data-model.md`; `deploy/README.md` — заголовок досылки, Close на idle/ошибке, `--block-idle-timeout-secs`, `chronyc tracking` в чек-листе.

## Что НЕ делать
- 0 подключений к фиду, 0 вызовов RPC; бинарник — только против `ws://127.0.0.1` мока.
- Не трогать `data/feed*`.

## Критерии приёмки
- `cargo test --workspace`, `clippy -D warnings` чистые; shellcheck и тесты `deploy/test/` проходят.
- data-auditor: PASS по п. 1–3 (код + тесты + свой прогон на моке).
- indexer-engineer: PASS по п. 4 и 6.

## Стоимость и риски
$0, без сети.

## Формат отчёта
Сделано; проверено и как; что не получилось; вопросы. `status: done`, отдельный коммит.
