# 012 — Исправления перед запуском на сервере. Отчёт

## Пункты 1, 2, 3, 5 и кодовая часть 6 (indexer-engineer)

- Дата: 2026-10-01.
- Исполнитель: indexer-engineer. Проверяющий по задаче: data-auditor (п. 1–3). Его я не запускал, запускает основная сессия.
- Статус задачи не менял, по указанию основной сессии он остаётся `in-progress`. Коммитов нет.
- Трогал только `crates/` и `references/data-model.md`. `deploy/` не трогал, это зона infra-ops.
- Подключений к фиду 0, вызовов RPC 0. Бинарники recorder и enricher запускались только тестами против своих моков на `127.0.0.1`. `FEED_URL`, `RPC_URL`, `RECORDER_OUT_DIR` сняты, `TMPDIR` = `scratchpad/012/tmp/`, туда же ложатся out-dir тестов. `data/feed*` не трогал.
- Формат строки сырья не менялся. Столбцы `connections.tsv` те же 11, новое — только в `event`/`reason`/`detail`.

### Итог

- **П. 1.** `--min-connect-interval-secs` теперь считается от конца прошлой сессии. Ждём `max(остаток паузы, интервал − (now − конец))`.
- **П. 2 (Н1).** При фатальной ошибке writer'а recorder шлёт Close 1000 `"recorder writer error"`, ждёт ответ ≤ 2 с и выходит с кодом 2. Раньше выходил сразу, без Close.
- **П. 3.** Новый флаг `--block-idle-timeout-secs` (по умолчанию 30 с, 0 — выключено). Если блоков нет дольше порога, recorder делает Close, переподключается с досылкой и пишет причину `block_idle`.
- **П. 5.** enricher: код выхода **75**, если исчерпан `--max-calls`. `--gaps` пропускает недописанную последнюю строку с WARN.
- **П. 6.** Н3 и Н4 уже были внесены координатором, проверил. В Н5 не хватало одного: что устаревшая строка `backlog` в 009 есть и у сессии A. Дописал. Новые флаги и события описаны в `data-model.md`.
- `cargo test --workspace`: **95 passed** (было 82, +13). `cargo clippy --workspace --all-targets -- -D warnings` и `cargo build --workspace` чистые, `cargo fmt -p recorder --check` чистый.

### Сделано по пунктам

**1. Интервал от конца сессии** (`backoff.rs`, `net.rs`, `writer.rs`, `main.rs`).

- `ConnLog::last_session` / `parse_last_session` находят последний `connected` и время последней строки любого типа после него.
- `writer::newest_data_mtime_ns` берёт mtime двух самых новых часовых файлов.
- `backoff::session_end_ns` выбирает конец сессии, `startup_wait` считает от него.
- Оба значения читаются **до** `recover` и до строк нового запуска (`torn_repair`, `gap_reconciled`). Причины:
  - ремонт торн-хвоста меняет mtime файла;
  - строки нового запуска выглядели бы как «более поздний конец».
- Два отступления от буквы задачи, оба делают ожидание только длиннее или оставляют как было:
  - **mtime данных учитывается и тогда, когда строки после `connected` есть; берётся более позднее.** Причина: с 009 через ~1 с после `connected` всегда пишется `backlog`. После kill -9 посреди 10-минутной сессии «последней строкой после `connected`» была бы она, и интервал отсчитался бы почти от начала сессии. Без kill -9 последняя строка (`client_close`, `shutdown`) всегда не раньше финального commit, так что результат совпадает с буквой задачи.
  - **Если в журнале нет ни одного `connected`, интервал не ждём, как до 012** (новая папка, скопированные данные). Без этого свежезаписанная папка с данными и без журнала ждала бы 120 с. На этом упали три теста 009, пока я этого не учёл.
- Строка `startup_wait min_connect_interval` теперь с `detail` вида `previous session ended 40.490s ago (end=data_mtime), min interval 120s`. Значения `end=`: `log_row`, `data_mtime` или `connected`.
- Имена причин `startup_wait` прежние: `pending_pause` и `min_connect_interval`.

**2. Н1 — Close при ошибке writer'а** (`main.rs`, `net.rs`).

- Поток writer'а больше не вызывает `exit(2)`. Ошибку он отдаёт через `oneshot` в главную задачу. К этому моменту `rx` уже освобождён, поэтому сетевая сторона не может зависнуть на полном канале.
- Главная задача:
  1. пишет `shutdown` с `reason=writer_error`, текст ошибки — в `detail`;
  2. посылает сигнал stop в сетевой цикл;
  3. сетевой цикл делает `close_gracefully`: Close 1000 `"recorder writer error"`, ожидание ответа ≤ 2 с, затем `ws.close()` ≤ 0.5 с, затем строка `client_close`;
  4. после `join` writer'а — `exit(2)`.
- Сигнал stop теперь несёт причину Close: `watch<Option<&'static str>>` вместо `watch<bool>`. Для SIGINT/SIGTERM причина прежняя, `"recorder shutdown"`.
- Writer может упасть и на финальном commit, уже после сигнала. Тогда пишется отдельное событие `writer_error` с `reason=final_commit`, код выхода 2 (как раньше).

**3. Залипание «только ping»** (`net.rs`, `backoff.rs`, `main.rs`).

- В цикле кадров `run_connection` появилась третья ветка `select!`: таймер `last_block + block_idle`. Отсчёт идёт от начала сессии и сбрасывается каждым кадром с seq > 0. Повторы из бэклога тоже сбрасывают: они тоже блоки от сервера.
- Когда таймер срабатывает, recorder пишет:
  - `client_close` с `"block idle timeout"` в `detail`;
  - `disconnected` с `reason=block_idle`, `detail` = `rule=transient no block (seq > 0) for 30s`.
- Затем обычное переподключение: в заголовке `last_seq + 1` из памяти.
- **Пауза после `block_idle` — всегда по лестнице `transient`** (5 → 10 → … → 300 с, ±20 %). В задаче этого нет, решение моё. По обычным правилам сессия ≥ 30 с даёт паузу 1–5 с. При устойчивом залипании это ~100 подключений в час, то есть риск бана. С лестницей — не больше ~11 в час, и healthcheck поймает это по `reconnects`. Сессия ≥ 10 мин (до залипания шли блоки) сбрасывает лестницу, так что первая пауза — 5 с.

**5. enricher** (`rpc.rs`, `main.rs`, `ranges.rs`, `lib.rs`).

- Новый вариант `CallError::Budget(BudgetExhausted)` вместо `Failed(anyhow!(…))`. Текст ошибки не изменился (`… call budget exhausted: N calls sent, M more would exceed --max-calls X`).
- `rpc::is_budget_exhausted` ищет этот вариант по цепочке ошибки. `main` выходит с `rpc::EXIT_BUDGET_EXHAUSTED = 75` и пишет WARN `…; stopping with exit code 75, the next run continues`. До выхода, как и раньше, вызывается `report` (и `--stats-json`).
- Прочие коды: любая другая ошибка — 1, сигнал — 130 (без изменений).
- `ranges::read_gaps_file` + `split_unterminated`: последняя строка без `\n` отрезается, `run_gaps` пишет WARN `ignoring unterminated last line of gaps file (being written?); the next run picks it up` с текстом строки.
  - Отрезается даже строка, которая сама по себе разбирается (`from\tto` без `recv_ns`): она недописана и будет взята следующим прогоном.
  - Строка, которая заканчивается `\n`, но не разбирается, — по-прежнему ошибка, код 1.
  - `filled.tsv` (его пишет сам enricher) читается строго, как раньше.

**6. Справочники.**

- `chain-facts.md`: Н3 («~1200 ± 5», экономия ~620) и Н4 («связь с активностью сети — предположение») уже на месте, проверено чтением. Новых фактов о сети в 012 нет: всё проверено на моке. Поэтому `chain-facts.md` не менял.
- `data-model.md`:
  - Н5: при остановке `disconnected` нет — уже было. Добавил, что обе строки `backlog` в `data/feed-test-009` устаревшие: A — 4 вместо 0, B — 6 вместо 621.
  - Поправил фразу «по `connected` считается `--min-connect-interval-secs`».
  - Описал: причины `disconnected`; `block_idle`; `startup_wait` с новым правилом и форматом `detail`; `shutdown writer_error` и `writer_error final_commit`; новые причины Close в `client_close`; коды выхода recorder (0 / 1 / 2) и enricher (0 / 1 / 75 / 130); WARN `--gaps` про недописанную строку.

### Проверено (2026-10-01, как)

Тесты, `cargo test --workspace`: 95 passed, 0 failed. `mock_feed` прогнан ещё 2 раза, 14/14 каждый раз. Новые тесты:

| Тест | Что проверяет |
|---|---|
| `backoff::tests::interval_counts_from_last_row_after_connected` | п. 1, случай 1: 10-минутная сессия, `client_close` 30 с назад → ждать 90 с, `end=log_row`; конец > 120 с назад → не ждать |
| `backoff::tests::interval_after_kill9_counts_from_data_mtime` | п. 1, случай 2: после `connected` строк нет, mtime 40 с назад → 80 с; только ранний `backlog` + свежие данные → `data_mtime`; без данных → `connected`; без `connected` → не ждать |
| `backoff::tests::pending_pause_and_end_interval_take_the_longer` | п. 1, случай 3: остаток паузы 403 (800 с) против интервала от конца (21 с) → пауза; короткая пауза против интервала 119 с → интервал |
| `backoff::tests::block_idle_uses_transient_ladder` | п. 3: 5, 10, …, 300 с после сессий по 31 с; сброс сессией 10 мин и здоровым закрытием |
| `mock_feed::min_connect_interval_counts_from_end_of_session` | бинарник, случай 1: `startup_wait min_connect_interval` 88–90.5 с, `end=log_row`, мок не видит ни одного подключения, SIGTERM → код 0. До 012 подключился бы сразу |
| `mock_feed::min_connect_interval_after_kill9_uses_data_mtime` | бинарник, случай 2: mtime файла выставлен на now−40 с → 78–80.5 с, `end=data_mtime` |
| `mock_feed::pending_pause_beats_interval_from_end` | бинарник, случай 3: `startup_wait pending_pause` 798–800.5 с |
| `mock_feed::min_connect_interval_survives_restart_and_sigterm_interrupts` (008, обновлён) | `connected` 30 с назад, `shutdown` 20 с назад: теперь ~100 с от `shutdown` (было ~90 от `connected`), `end=log_row` |
| `mock_feed::writer_error_sends_close_then_exits_2` | п. 2: ошибка записи имитируется так: на месте каталога года лежит обычный файл, и `create_dir_all` падает с «Not a directory», как при отказе диска. Мок получает Close 1000 `"recorder writer error"`, отвечает; код выхода **2** < 4 с; `shutdown writer_error`, `client_close server_replied`, `disconnected` нет, подключение одно |
| `mock_feed::block_idle_closes_and_resumes` | п. 3: после 2 блоков мок шлёт только ping каждые 300 мс (`--idle-timeout-secs 1` при этом не срабатывает). Close 1000 `"block idle timeout"` через 1.9–3 с после последнего блока; повторное подключение через 3.5–7 с с `Arbitrum-Requested-Sequence-Number: 302`; `disconnected block_idle 101 … rule=transient no block (seq > 0) for 2s`; `idle_timeout` нет; все строки по 11 столбцов; `gaps.tsv` пуст; записано 300..304 |
| `ranges::tests::unterminated_last_gaps_line_is_cut_off` | п. 5: четыре варианта недописанной строки из отзыва 011 (`77200000`, `77200000\t`, `77200000\t77200099`, `…\t17908`) отрезаются; строка с `\n` берётся; битая строка с `\n` — ошибка |
| `gaps::unterminated_last_gaps_line_is_ignored_until_complete` | п. 5, через `run`: 4 прогона с недописанной строкой → мок видит только блоки 100..102; когда строка дописана → 300..302; битая строка с `\n` → ошибка |
| `exit_codes::max_calls_exhausted_exits_75_and_keeps_finished_files` | п. 5, бинарник: `--max-calls 12`, два файла по 8 вызовов → код **75**, первый файл готов и записан в `filled.tsv`, второго нет, ровно 12 вызовов, `--stats-json` записан |
| `exit_codes::other_errors_still_exit_1` | 429 навсегда + `--max-attempts 1` → код 1 (не 75); нет файла `--gaps` → 1 |
| `retry::call_budget_is_never_exceeded` (дополнен) | ошибка бюджета распознаётся `is_budget_exhausted` |

Строки `connections.tsv` из прогона мока, для сверки с healthcheck. Полный вывод — `scratchpad/012/sample-connections.txt`.

```
…	client_close	server_replied	101	-	-	2.002	2	-	sent close 1000, waited 0 ms; close 1000 "block idle timeout": close frame code=Some(1000) reason=""
…	disconnected	block_idle	101	-	5.454	2.002	2	0	rule=transient no block (seq > 0) for 2s
…	connected	-	101	-	-	-	-	0	ws://127.0.0.1:60020/ requested=302 mode=header
…	shutdown	writer_error	-	-	-	-	-	-	Not a directory (os error 20)
…	client_close	server_replied	101	-	-	0.000	1	-	sent close 1000, waited 0 ms; close 1000 "recorder writer error": close frame code=Some(1000) reason=""
…	startup_wait	min_connect_interval	-	-	79.510	-	-	0	previous session ended 40.490s ago (end=data_mtime), min interval 120s
```

### Предполагается, не проверено

- Что залипание «только ping» вообще бывает у этого фида. В записях 001/002/009 его не искали. Порог 30 с выбран по задаче. Распределение естественных пауз между блоками мерили только до 2.9 с (max в 009), более длинные паузы не исключены.
- Что `block_idle` с паузой 5 с почти всегда укладывается в бэклог (~60–70 с) и не даёт дыры: 30 + 5 с < 60 с. Это расчёт, не прогон на реальном фиде.
- Что mtime часового файла после kill -9 отстаёт от момента kill не больше чем на `--frame-secs`. Пока идут кадры, фрейм закрывается не реже раза в 60 с, но на реальном сервере после kill -9 это не проверялось.
- Поведение при настоящем отказе диска (ENOSPC, EIO на fsync) не воспроизводилось. Имитировалась ошибка создания каталога часа: путь в коде тот же (`writer::run` → `Err`), но место падения другое.

### Что не получилось / ограничения

- **Повторяющийся отказ диска.** Если диск полон, каждый рестарт делает так: подключение → первый кадр → ошибка writer'а → Close → выход 2. Затем systemd ждёт `RestartSec=120`, к этому моменту интервал от конца сессии (120 с) уже выдержан. Итог — ~1 подключение в 2 мин, ~30 в час, пока не освободят место. healthcheck ловит это по `disk`, `reconnects` и `feed`. Защиты в самом recorder нет. Можно сделать отдельной задачей, например проверять запись в out-dir перед подключением.
- Каждый плановый `systemctl restart` теперь ждёт ~120 с от `shutdown`, это ~50–60 с дыры за пределами бэклога. Прямое следствие решения по 009 в.1. infra-ops вынес это в свой вопрос 1.
- В `--stats-json` нет поля `budget_exhausted` (предлагалось в отзыве 011). Не добавлял, чтобы не менять формат файла статистики. Различать по коду выхода 75 и WARN.
- Прогон по бюджету «на границе файла» (отзыв 011, п. 6а) не делал: в задаче его нет. При исчерпании внутри файла недописанный `*.partial` по-прежнему теряется, до 2 × `--chunk` вызовов.

### Для infra-ops (`deploy/` не трогал)

Проверил `deploy/README.md` и `deploy/enricher-gaps.service` по финальному коду. Всё сходится: код 75, `SuccessExitStatus=75`, WARN про недописанную строку, `block_idle`, Close `"block idle timeout"`, лестница 5 → 300 с, конец сессии как «позднее из двух». Две мелочи:

1. README, стр. 228: «сообщение об исчерпании бюджета **и остатке**». В сообщении остатка нет. Текст: `<что>: call budget exhausted: N calls sent, M more would exceed --max-calls X; stopping with exit code 75, the next run continues`. Остаток виден в строке `gaps plan` (`todo_blocks`) следующего прогона или в `enricher --gaps … --dry-run`.
2. README, стр. 240: «`client_close` … перед `disconnected`». При ошибке writer'а `disconnected` нет: порядок `shutdown writer_error` → `client_close` → выход 2, как при SIGTERM. Ещё нюанс к стр. 176/239: если в `connections.tsv` нет ни одного `connected`, интервал не ждётся.

Для healthcheck: при желании можно отдельно алертить по `shutdown` с `reason=writer_error` и по событию `writer_error` (3-й столбец), это явная ошибка диска. Сейчас её ловят только косвенно (`unit` / `disk` / `feed`).

### Вопросы к Cowork / Михаилу

1. Пауза после `block_idle` по лестнице `transient` (5 → 300 с) вместо обычных 1–5 с. Это моё решение против частых переподключений, в задаче его нет. Устраивает?
2. Конец сессии берётся как «позднее из последней строки после `connected` и mtime данных», а не строго «строка, иначе mtime» (причина — строка `backlog`, п. 1). Устраивает?
3. Нужна ли в recorder защита от цикла рестартов при полном диске (см. «Ограничения»), или достаточно `RestartSec=120` + healthcheck `disk`?

### Изменённые файлы

- `crates/recorder/src/backoff.rs`, `main.rs`, `net.rs`, `writer.rs`;
- `crates/recorder/tests/mock_feed.rs` (+5 тестов, 1 обновлён);
- `crates/enricher/src/rpc.rs`, `main.rs`, `ranges.rs`, `lib.rs`;
- `crates/enricher/tests/exit_codes.rs` (новый), `gaps.rs`, `retry.rs`;
- `.claude/skills/hoodchain-mev/references/data-model.md`.

### Что проверить data-auditor (п. 1–3)

- Код: `crates/recorder/src/{backoff,net,main,writer}.rs`, особенно:
  - порядок чтения конца сессии до `recover`;
  - ветку `writer_err_rx` в `main`;
  - третью ветку `select!` в `run_connection`.
- Тесты: `mock_feed` (5 новых + обновлённый 008), `backoff::tests` (4 новых).
- Свой прогон на моке: kill -9 посреди сессии → `startup_wait … end=data_mtime`; `block_idle` при потоке ping; ошибка записи → Close 1000 и код 2.
- `data-model.md`: разделы `connections.tsv` и enricher.

## Пункты 4 и 6 (infra-ops)

Исполнитель: infra-ops. Проверяющий по задаче: indexer-engineer (не запускался). Статус задачи не менял, коммитов нет. `crates/` и `references/` не трогал. Подключений к фиду 0, вызовов RPC 0, реальных серверов 0. Docker только с `--network none`, порты не публиковались, тестовые контейнеры (`hood-deploy-test-*`) удалены.

### Сделано

**П. 4 — healthcheck (З1-а из отзыва 011)**, `deploy/healthcheck.sh`:
- возраст `feed` теперь — mtime `last_seq.txt` (recorder переписывает его только при росте seq, то есть по новым блокам);
- файлы текущего и прошлого часа учитываются, только если `last_seq.txt` нет (первые минуты на пустой папке);
- если файл часа свежий, а `last_seq.txt` старый, в тексте алерта подсказка «соединение, похоже, живо, но блоков нет (только ping?)»;
- в итоговой строке журнала добавлено `feed_src=last_seq.txt|hour_file`.

`deploy/test/test-healthcheck.sh`:
- раздел 13: «файл часа свежий, `last_seq.txt` 600 с → ALERT», без повтора, «восстановлено»; подсказка про ping в теле; fallback без `last_seq.txt` (свежий файл часа → тихо, старый → ALERT, ничего нет → `feed_age_s=-1`), возврат `last_seq.txt` → «восстановлено»;
- раздел 14: строки `client_close … "block idle timeout"` + `disconnected block_idle` + `connected` + `backlog` → 0 уведомлений, `ban=ok reconnects=ok`;
- подставной notifier теперь пишет и тело уведомления (`$NLOG.body`) для проверки подсказки.

**П. 6 — `deploy/README.md`:**
- «Обновление бинарника» переписан: `--min-connect-interval-secs` считается от конца прошлой сессии, поэтому `systemctl restart` сам ждёт ~120 с после остановки; повторно `restart` не запускать; ручной `stop && sleep 120 && start` убран как лишний; каждый рестарт — одна строка `gaps.tsv` на ~50–60 с (простой ~120 с минус бэклог ~60–70 с); обновлять пачкой. То же — в абзацах про reboot, «Почему `RestartSec=120`», «Пауза переживает перезапуск», в таблице файлов (`needrestart-hood.conf`).
- Close 1000: добавлены случаи `block_idle` и ошибка writer'а (Close ≤ 2 с перед выходом с кодом 2; что делает systemd и healthcheck, при полном диске сначала освободить место).
- Новый пункт «Нет блоков при живом соединении»: `--block-idle-timeout-secs` (по умолчанию 30, 0 выключает), причина `block_idle`, Close `"block idle timeout"`, пауза по лестнице 5 → 10 → … → 300 с, устойчивое залипание даёт до ~11 подключений в час и ловится `reconnects`.
- Заголовок досылки уже был описан (раздел «Досылка после переподключения», 011); добавлена связь с паузой рестарта.
- Мониторинг: новое правило `feed`, `block_idle` не бан, `reconnects` ловит циклы `block_idle`; раздел «чего не ловит» — З1 закрыт, остаётся окно < 5 мин; коды 3 (feed-audit) и 75 (enricher-gaps) — не ошибка.
- Чек-лист: `chronyc tracking` уже был (из 009/011), уточнён (`Leap status : Normal`, `System time` < 0.5 с).
- Дозаливка: код 75 и `SuccessExitStatus=75`; недописанная последняя строка `gaps.tsv` пропускается с WARN; как отличить «бюджет исчерпан» (только по журналу enricher).

**Из п. 5 (юнит):** `deploy/enricher-gaps.service` — `SuccessExitStatus=75` и комментарий. При коде 75 `OnFailure` (`notify-failure@`) не срабатывает; отставание дозаливки по-прежнему ловит `backfill` в healthcheck (дыры старше 24 ч). healthcheck сам код выхода enricher не читает, правок в нём для этого не нужно.

**Комментарии без изменения поведения:** `deploy/recorder.service` (интервал от конца сессии, значения по умолчанию `--block-idle-timeout-secs 30`, флагов в юните не нужно), `deploy/needrestart-hood.conf` (цена рестарта).

### Проверено (2026-10-01, как)

- `test-healthcheck.sh` в `ubuntu:24.04`, `--network none`: **63/63** (было 50; +11 в разделе 13, +2 в разделе 14). Все прежние случаи зелёные.
- Регрессия: те же тесты против `healthcheck.sh` из HEAD (до правки) — 5 FAIL, в том числе «файл часа свежий, `last_seq.txt` старый → ALERT» (0 уведомлений вместо 1). То есть тест ловит З1.
- `test-notify.sh`: 14/14.
- shellcheck (`koalaman/shellcheck:stable`, `--network none`, `-x`) по 10 скриптам: rc=0. `bash -n` по всем: ок.
- `systemd-analyze verify` (systemd 255, образ `hood-deploy-test-systemd:24.04`, `--network none`, бинарники — заглушки) по всем 10 юнитам, включая изменённые `recorder.service` и `enricher-gaps.service`: rc=0, замечаний нет.
- `enricher-gaps.service` под настоящим systemd (PID 1, контейнер `--network none`, enricher — заглушка с заданным кодом, `RPC_URL` фиктивный):

  | код заглушки | `Result` | уведомлений `notify-failure@` |
  |---|---|---|
  | 75 | success | 0 |
  | 1 | exit-code | 1 («юнит enricher-gaps.service завершился с ошибкой») |
  | 0 | success | 0 |

- Попутно: systemd 255 после успешного oneshot показывает `ExecMainStatus=0` и для кода 75. Поэтому в README «бюджет исчерпан» предлагается отличать по журналу enricher, а не по `systemctl show`.
- Имена сверены с рабочим деревом indexer-engineer (`crates/recorder/src`, состояние на момент сверки, код ещё в работе): флаг `--block-idle-timeout-secs` (по умолчанию 30, 0 выключает), причина `block_idle`, Close `"block idle timeout"`, лестница пауз `block_idle` 5 → 300 с, конец сессии — позднее из «последней строки после `connected`» и «mtime последнего часового файла» (`session_end_ns`).

### Предполагается, не проверено

- Код выхода 75 и пропуск недописанной строки `gaps.tsv` с WARN в enricher — **по задаче, сверить**: в `crates/enricher` на момент сверки этих изменений ещё не было. Текст сообщения enricher об исчерпании бюджета в README не приводится.
- Строка `client_close`/`disconnected` при ошибке writer'а: точный формат не сверял (healthcheck на неё не опирается).
- Что за ~120 с паузы `restart` плюс ≤ 60 с до первого фрейма `feed` (порог 300 с) не сработает — расчёт, не прогон.
- Вживую на сервере ничего не проверялось.

### Вопросы

1. Михаилу/Cowork: каждый плановый рестарт теперь стоит ~50–60 с дыры (раньше ~0 при долгой сессии). Это прямое следствие решения по 009 в.1, в README так и написано. Подтвердить, что это ожидаемая цена.
2. indexer-engineer: после окончания п. 1, 3, 5 сверить README (разделы «Обновление бинарника», «Поведение при сбоях», «Дозаливка») с финальным кодом, особенно код 75 и WARN.
