# 012 — проверка п. 4 и 6 (indexer-engineer)

- Дата: 2026-10-01.
- Проверяющий: indexer-engineer. Исполнитель п. 4 и 6: infra-ops. Код п. 1–3 и 5 писал indexer-engineer, его проверяет data-auditor, здесь он не оценивается.
- Что сверялось: задача `docs/handoff/to-code/012-prelaunch-fixes.md`, отчёт `docs/handoff/from-code/012-prelaunch-fixes.md` (оба раздела), З1 из `docs/reviews/011-deploy-kit-indexer-engineer.md`, `git diff HEAD -- deploy/`, финальный код `crates/recorder/src` и `crates/enricher/src`, `references/data-model.md` и `chain-facts.md`.
- Условия: подключений к фиду 0, вызовов RPC 0, реальных серверов 0. Docker только с `--network none`, порты не публиковались, все контейнеры запускались с `--rm` (`docker ps -a` после прогона пуст). `crates/` не менялся. Коммитов нет. Статус задачи не менял.

## Итог

| # | Что | Вердикт |
|---|---|---|
| 4 | healthcheck: возраст `feed` по `last_seq.txt`, тест З1, регрессии, строки 012 | **PASS** |
| 6 | `deploy/README.md` и юниты по финальному коду, Н3–Н5 | **PASS после правок** (2 расхождения, найденные исполнителем кода, и 4 мелких исправлены мной в `deploy/`) |

Дополнительно, по предложению из отчёта: в healthcheck добавлено условие `writer` (алерт по `shutdown writer_error` / `writer_error`). Это несколько строк и тест. Ниже — что именно.

## П. 4 — healthcheck: PASS

### Код (проверено 2026-10-01, чтением)

- `deploy/healthcheck.sh`, раздел `feed`: если `last_seq.txt` существует, возраст — только его mtime (`feed_src=last_seq.txt`). Файлы текущего и прошлого часа берутся, только если `last_seq.txt` нет (`feed_src=hour_file`). Если нет ничего — `feed_age=-1`, алерт. Подсказка «только ping?» добавляется, когда файл часа моложе порога, а `last_seq.txt` старше. Так и требует З1-а.
- Предпосылка сверена с кодом recorder: `writer.rs`, `FeedWriter::commit` переписывает `last_seq.txt` только при `self.last_seq != self.durable_seq`. `last_seq` растёт только в `accept` для строк с seq > 0, дубликаты отбрасываются раньше. Значит, ping и `confirmedSequenceNumberMessage` mtime `last_seq.txt` не двигают, а файл часа двигают (фрейм закрывается по таймеру в `run`, не реже `--frame-secs`). Сигнал выбран правильно.
- Ложных алертов на плановом рестарте нет (расчёт, не прогон): финальный commit при SIGTERM пишет `last_seq.txt`, затем ~120 с `startup_wait`, затем первый commit не позже ~60 с. Итого ~180 с при пороге 300 с. После kill -9: последний commit не раньше ~60 с до падения + `RestartSec=120` + ≤ 60 с ≈ 240 с, тоже ниже порога, но запас меньше. Предполагается, на сервере не проверялось.

### Строки 012 и бан (проверено 2026-10-01, тестом)

`ban` смотрит только последнюю строку `connected` / `disconnected` / `startup_wait`, бан — это HTTP 4xx или `startup_wait pending_pause` от 600 с. По финальному коду:
- `disconnected block_idle` — код 101, не 4xx;
- `client_close` (в том числе `"block idle timeout"`, `"recorder writer error"`), `shutdown writer_error`, `writer_error final_commit` в выборку не попадают;
- `startup_wait min_connect_interval` — не `pending_pause`. Лестница `block_idle` ограничена 300 с (`TRANSIENT_CAP`), так что и `pending_pause` после неё до 600 с не дойдёт.

Разбор по позиции (столбцы 1, 3–7) совпадает с финальным выводом: заголовок `CONNECTIONS_HEADER` в `net.rs` прежний, 11 столбцов. Все 15 строк данных в `scratchpad/012/sample-connections.txt` (прогон мока финального бинарника) имеют ровно 11 полей (`awk -F'\t' '{print NF}'`).

Добавил в `deploy/test/test-healthcheck.sh` раздел 15: строки из `sample-connections.txt` без изменения столбцов, время сдвинуто к «сейчас»:
- `connected … mode=no_data`, `backlog session_ended`, `client_close … "block idle timeout"`, `disconnected block_idle 101 … 5.454`, `connected … requested=302 mode=header`, `backlog done`, `shutdown SIGTERM`, `client_close … "recorder shutdown"`, `startup_wait min_connect_interval 79.510 … previous session ended 40.490s ago (end=data_mtime)` → 0 уведомлений, `ban=ok reconnects=ok writer=ok`;
- последовательность ошибки writer'а `shutdown writer_error` → `backlog session_ended` → `client_close … "recorder writer error"` (без `disconnected`) → `startup_wait min_connect_interval` → `ban=ok`, только алерт `writer` (см. ниже).

Перед разделом 15 из журнала убираются `connected` последнего часа, оставленные разделами 11 и 14: их уже 6, ещё 4 подняли бы `reconnects`, и тест проверял бы не то.

### Прогоны (2026-10-01)

- `docker run --rm --network none -v deploy:/deploy:ro ubuntu:24.04 bash /deploy/test/test-healthcheck.sh`: до моих правок **63/63**, после — **71/71**.
- Регрессия: текущие тесты против `healthcheck.sh` из HEAD — 62 passed, **9 failed**. В том числе «hour file fresh, last_seq.txt 600 s old: ALERT» (0 уведомлений вместо 1), fallback, `feed_src`, все случаи `writer`. Тест ловит З1.
- `test-notify.sh`: 14/14.
- `bash -n` по всем `deploy/*.sh`, `deploy/test/*.sh`: ок. shellcheck (`koalaman/shellcheck:stable`, `--network none`, `-x`, все скрипты): rc=0.
- `run-systemd-container.sh` / `systemd-analyze verify` не перезапускал: юниты я не менял. Прогон infra-ops по `recorder.service` и `enricher-gaps.service` (включая проверку `SuccessExitStatus=75` под systemd 255) принимаю по отчёту, не повторял.

## П. 6 — README и справочники: PASS после правок

### Сверка с финальным кодом (проверено 2026-10-01, чтением кода)

| Что | Код | README |
|---|---|---|
| Заголовок досылки | `Arbitrum-Requested-Sequence-Number: last_seq+1`, `requested=… mode=header\|no_data\|disabled` | «Досылка после переподключения» — совпадает |
| Close 1000 на idle / block_idle / ошибке writer'а | `net.rs` `close_gracefully`; причины `"idle timeout"`, `"block idle timeout"`, `"recorder writer error"`, `"recorder writer gone"`, `"recorder shutdown"` | совпадает; порядок строк исправлен (ниже) |
| `--block-idle-timeout-secs` | `main.rs`: по умолчанию 30, 0 выключает; `backoff.rs`: `BlockIdle` → лестница `transient` 5 с × 2ⁿ, ±20 %, потолок 300 с, сброс после сессии ≥ 10 мин | совпадает; добавлен джиттер |
| `--min-connect-interval-secs` от конца сессии | `session_end_ns`: позднее из последней строки после `connected` и mtime часового файла; без `connected` не ждём | совпадает; добавлен случай «нет `connected`» |
| `systemctl restart` ждёт ~120 с | следствие предыдущего | «Обновление бинарника» — совпадает |
| `chronyc tracking` в чек-листе | — | п. 7 чек-листа, есть |
| enricher 75 | `rpc::EXIT_BUDGET_EXHAUSTED = 75`, `main.rs` | «Дозаливка», `enricher-gaps.service` `SuccessExitStatus=75` — совпадает; текст сообщения исправлен |
| WARN на недописанной строке `gaps.tsv` | `lib.rs`: `ignoring unterminated last line of gaps file …` | совпадает |

### Исправлено мной в `deploy/`

1. `deploy/README.md`, «Дозаливка» (было ~стр. 228): «сообщение об исчерпании бюджета **и остатке**» — остатка в сообщении нет. Теперь приведён текст WARN из `rpc.rs` (`<что>: call budget exhausted: N calls sent, M more would exceed --max-calls 4000; stopping with exit code 75, the next run continues`), а остаток — в строке `gaps plan` (`todo_blocks`) следующего прогона или в `enricher --gaps … --dry-run`. Сверено с `lib.rs`: при `--dry-run` выход до `make_rpc`, вызовов RPC нет.
2. `deploy/README.md`, «Почему `RestartSec=120`» (было ~стр. 240): «`client_close` … перед `disconnected`». Теперь: после idle и `block_idle` идёт `disconnected`; при остановке и ошибке writer'а `disconnected` нет, порядок `shutdown` → `client_close` → выход (0 или 2). Порядок сверен с `main.rs` (строка `shutdown` пишется до `stop_tx.send`) и с `sample-connections.txt`.
3. Там же: если в `connections.tsv` нет ни одного `connected`, интервал не ждётся (как в коде и в отчёте indexer-engineer).
4. «Поведение при сбоях», ошибка записи: строки `shutdown writer_error` → `client_close` без `disconnected`, отдельное `writer_error final_commit`, какие ключи healthcheck сработают (теперь и `writer`), цикл ~1 подключение в 2 мин при полном диске.
5. Лестница `block_idle`: добавлено «±20 % джиттера» (в выборке мока пауза 5.454 с).
6. Таблица файлов (`connections.tsv`) — добавлено событие `writer_error`; таблица мониторинга — `writer_error` в списке строк, которые `ban` не учитывает, и новая строка `writer`.

### Алерт `writer` (реализован)

- `deploy/healthcheck.sh`: условие `writer` — в последних 256 КБ `connections.tsv` есть строка `shutdown writer_error` или `writer_error` моложе `HC_WRITER_ERROR_WINDOW_S` (по умолчанию 3600 с). Заголовок `recorder: ошибка записи на диск (N за 60 мин)`, в теле — время, событие и текст ошибки из `detail` последней такой строки. «Восстановлено» — когда в окне таких строк нет. Одно уведомление на эпизод, повтор не шлётся (обычный `raise`/`resolved`).
- Почему окно, а не «до следующего `connected`»: после рестарта recorder снова подключается, и при полном диске условие мигало бы каждые ~2 мин.
- `deploy/etc/healthcheck.env.example`: добавлен `HC_WRITER_ERROR_WINDOW_S=3600`.
- Тест (раздел 15): алерт ровно 1, в теле текст ошибки, `ban=ok`; `writer_error final_commit` пока алерт активен — 0; новый `connected` внутри окна — алерт держится; окно 30 с — «восстановлено» ровно 1; следующий прогон — 0.

### Справочники (проверено 2026-10-01, чтением)

- Н3 — `chain-facts.md`, стр. ~103: «~1200 ± 5 блоков … сэкономил ~620». Есть.
- Н4 — `chain-facts.md`, стр. ~109: «связь с активностью сети — предположение, не проверено». Есть.
- Н5 — `data-model.md`: «при остановке строки `disconnected` нет» есть; про устаревшие строки `backlog` обеих сессий 009 (A — 4 вместо 0, B — 6 вместо 621) есть.
- **Замечание (не блок, `references/` не правил):** в `data-model.md`, описание `client_close`, написано «при остановке (SIGINT/SIGTERM) строки `disconnected` нет — **за `client_close` следует `shutdown`**». По коду и по выборке мока порядок обратный: сначала `shutdown`, потом `client_close`. Предлагаю indexer-engineer поправить на «`shutdown` идёт перед `client_close`».

## Предполагается, не проверено

- Что на реальном фиде `feed` не даст ложного алерта на плановом рестарте и после kill -9 (расчёт выше, ~180 и ~240 с при пороге 300 с).
- Что `tail -c 262144` хватает для окна `writer` в 1 ч. При ~1 подключении в 2 мин строк ~150 в час, это меньше 256 КБ, но расчёт, не замер на журнале сервера.
- Юниты под systemd после моих правок не перепроверял: я их не менял.

## Изменённые файлы

- `deploy/healthcheck.sh` — условие `writer`, `HC_WRITER_ERROR_WINDOW_S`, строка в шапке.
- `deploy/test/test-healthcheck.sh` — раздел 15 (+8 проверок).
- `deploy/README.md` — правки 1–6 выше.
- `deploy/etc/healthcheck.env.example` — `HC_WRITER_ERROR_WINDOW_S`.
