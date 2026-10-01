# 011 — набор для развёртывания: проверка indexer-engineer (юниты, пути, рестарт recorder)
date: 2026-10-01
reviewer: indexer-engineer
scope: `deploy/` (скрипты, юниты, `etc/*.example`, `test/`) против текущего кода `crates/recorder` (после 009) и `crates/enricher`, `feed_audit.py`, `data-model.md`
verdict: **PASS с замечаниями**. Блокирующих расхождений с CLI и путями нет. В `deploy/` внесены правки для совместимости с 009 (список ниже). Два замечания не блокируют, но их стоит закрыть до или сразу после запуска: залипание «только ping» не видно мониторингу (З1), и неверная команда отката в README (исправлена).

Условия проверки: подключений к фиду 0, вызовов RPC 0, реальных серверов 0. Docker только с `--network none`, порты не публиковались. Коммитов нет. `crates/` не менялся.

## Итог по пунктам

| # | Что | Вердикт |
|---|---|---|
| 1 | Пути и флаги recorder / enricher / feed-audit | PASS |
| 2 | Рестарт: `RestartSec`, `KillSignal`, `TimeoutStopSec`, процедура обновления | PASS по юниту; FAIL по тексту runbook (исправлен) |
| 3 | healthcheck: разбор `connections.tsv`, `gaps.tsv`, `last_seq.txt` | PASS по совместимости с 009; замечание З1 (не 009) |
| 4 | feed-audit-daily.sh | PASS; добавлен `--frame-secs` |
| 5 | Правки README по 009 | сделано |
| 6 | Ответы infra-ops | ниже |

## 1. Пути и флаги — PASS

Проверено по коду 2026-10-01: `crates/recorder/src/main.rs` (struct `Args`), `crates/enricher/src/lib.rs` (struct `Args`), `feed_audit.py` (`argparse`).

- **recorder.service**: `ExecStart=/opt/hoodchain-mev/bin/recorder --out-dir /srv/hood/data/feed`. Флаг есть (`--out-dir`, env `RECORDER_OUT_DIR`, по умолчанию `data/feed`). `FEED_URL` и `RECORDER_OUT_DIR` в юните не заданы, `EnvironmentFile` нет, dotenv в коде нет: URL берётся из `hood_core::FEED_URL`. Остальное по умолчанию: `--idle-timeout-secs 10`, `--frame-secs 60`, `--min-connect-interval-secs 120`; заголовок досылки включён (флаг `--no-requested-seq` — только для отката). Ответ на вопрос infra-ops 1(б): да, в юните флаги не нужны.
- Recorder пишет только в `out_dir`: часовые файлы `YYYY/MM/DD/feed-*.tsv.zst`, `gaps.tsv`, `last_seq.txt` (tmp + rename в той же папке), `connections.tsv`, `_torn/`. Всё это внутри `/srv/hood/data/feed`, значит `ReadWritePaths=/srv/hood/data/feed` достаточно, `WorkingDirectory=/opt/hoodchain-mev` только читается. Ответ на 1(г): подтверждаю.
- **healthcheck.sh**: `HC_FEED_DIR=/srv/hood/data/feed` → `last_seq.txt`, `connections.tsv`, `gaps.tsv` там же. `HC_BLOCKS_DIR=/srv/hood/data/blocks` → `filled.tsv` (его пишет enricher в `--out-dir`, см. `ranges.rs`, `data-model.md`). Совпадает.
- **enricher-gaps.service**: `--gaps /srv/hood/data/feed/gaps.tsv --out-dir /srv/hood/data/blocks --max-calls 4000 --rps 2 --batch 10 --concurrency 1`. Все флаги существуют. `--gaps` несовместим с `--from/--to` и с `--mode logs`, здесь не используются. `RPC_URL` из `EnvironmentFile` (у `--rpc-url` есть `env = "RPC_URL"`), без файла юнит не стартует. `ReadOnlyPaths` на feed и `ReadWritePaths` на blocks верны: `OutDirLock`, `*.partial`, `filled.tsv` лежат в `--out-dir`. Бюджет: 4000 вызовов при `--rps 2` — около 33 мин, `TimeoutStartSec=55min` хватает. Таймер раз в час, а `OutDirLock` защищает от наложения прогонов.
  - Замечание (не блок): в режиме `--gaps` по умолчанию `--chunk 1000`, то есть 2000 вызовов на файл. Бюджет 4000 — ровно 2 файла. Если на 2-м файле был хоть один повтор, бюджет кончится внутри него: недописанный `*.partial` удалится, до 2000 вызовов уйдут впустую, `filled.tsv` за этот файл не продвинется. Совет infra-ops: `--chunk 250` (500 вызовов на файл, потеря при исчерпании ≤ 500) или запас в бюджете. Сам не менял: бюджет — решение Михаила и infra-ops.
- **feed-audit-daily.sh**: `--feed-root /srv/hood/data/feed --rpc-sample 0 "<день>/feed-*.tsv.zst"`. Glob в кавычках — так и задумано: `feed_audit.py` раскрывает его сам (`glob.glob`, «glob можно передать строкой» в SKILL.md). При `--rpc-sample 0` `rpc_blocks` не вызывается (условие `if a.rpc_sample > 0`).
- **bootstrap.sh** ставит `feed_audit.py` в `/opt/hoodchain-mev/deploy/feed_audit.py` (`AUDIT_SCRIPT` по умолчанию). Каталоги `/srv/hood/data/{feed,blocks,logs}` создаются.

## 2. Рестарт — юнит PASS, runbook исправлен

Путь остановки по коду (`main.rs`, `net.rs`, проверено 2026-10-01):
SIGTERM → строка `shutdown` → `stop_tx` → сетевой цикл: `close_gracefully` (Close 1000 `"recorder shutdown"`, ожидание ответа ≤ 2 с, кадры за это время пишутся в сырьё, затем `ws.close()` ≤ 0.5 с) → `client_close` → выход из цикла (внешний предел 5 с) → `drop(tx)` → writer дописывает очередь, `commit` (закрыть фрейм, `sync_data`, `gaps.tsv` + fsync, `last_seq.txt` атомарно) → выход 0.
- Если SIGTERM пришёл во время паузы, `startup_wait` или установки соединения, выход сразу: везде стоит `select!` со `stopped(stop)`.
- **`TimeoutStopSec=30`** — запас больше чем в 10 раз: сеть ≤ 2.5 с (предел 5 с), commit обычно меньше 1 с. Ответ на 1(в): хватает, idle-Close на остановку не влияет. Он идёт внутри сессии, а не при SIGTERM.
- `KillSignal=SIGTERM` + `KillMode=mixed` — верно: обработчик SIGTERM есть (`shutdown_signal`), SIGKILL достанется только по таймауту.
- Мелочь (предполагается, не проверено): обработчик сигнала ставится после `recover`. SIGTERM во время восстановления убьёт процесс действием по умолчанию. `recover` повторяем, на данные это не влияет.
- `RestartSec=120` действует только после падения (`Restart=always`): kill -9, `exit(2)` writer'а. После падения пауза 120 с отсчитывается от падения, затем `startup_wait` проверяет интервал от последнего `connected`.
- `After=time-sync.target` реально ждёт синхронизации, только если включён `chrony-wait.service`. Предполагается, что в Ubuntu 24.04 он есть, но выключен (не проверял). Для статистики бэклога после перезагрузки это может иметь значение (п. 5), для данных — нет. Решение за infra-ops.

**Runbook «обновить без дыры» был неверен в двух местах (исправлено в README):**
1. Там было: «новый процесс ждёт ~120 с … дыра ~2 мин (~1200 блоков)». По коду `startup_wait` (`backoff.rs`) считает `--min-connect-interval-secs` **от последнего `connected`**, а не от конца сессии. После сессии дольше 120 с `systemctl restart` подключается **сразу**: простой ~1–3 с, это внутри бэклога (~60–70 с), с заголовком досылки дыры обычно нет. 120 с выжидается, только если последний `connected` был меньше 120 с назад. Это открытый вопрос 009 (вопрос 1): безопасен ли немедленный реконнект после Close 1000. Бан 2026-09-30 был через 11 с после kill -9 без Close; реконнект через 120 с после Close видели один раз. В README оба варианта: `systemctl restart` (фактическое поведение) и осторожный `stop && sleep 120 && start` (дыра ~50–60 с, интервал ≥ 120 с). Выбор — за Михаилом/Cowork.
2. Откат `cp -p recorder.prev recorder` при работающем recorder падает с `ETXTBSY` (Text file busy): `cp` открывает исполняемый файл на запись. Заменено на `install … recorder.new && mv -f` (тот же rename, что в `build-on-server.sh`). Добавлен откат только досылки: drop-in с `--no-requested-seq`.

Также поправлены фразы «после reboot выждет те же 120 с» и «живой фид не досылается».

## 3. healthcheck и формат 009 — PASS (+ замечание З1)

Формат `connections.tsv` после 009 не изменился: 11 столбцов, заголовок тот же (`CONNECTIONS_HEADER` в `net.rs`). `requested=`/`mode=` и поля `backlog` лежат в `detail` — последнем столбце. Пожелание infra-ops 1(а) выполнено самим кодом 009.

Разбор в `healthcheck.sh`:
- `ban`: из строк берутся только события `connected` / `disconnected` / `startup_wait`, из них последняя. Значит:
  - `backlog` и `client_close` (код 101), а также `shutdown`, `torn_repair`, `gap_reconciled` не учитываются;
  - `disconnected idle_timeout` (код 101) — не 4xx, не бан;
  - `startup_wait min_connect_interval` — не бан. Баном считается только `pending_pause` от 600 с.

  Столбцы 1, 3–7 читаются по позиции: `ts_utc, event, reason, http_status, retry_after, pause_s`, совпадает с кодом. Имена причин в коде: `http_429`, `forbidden`, `idle_timeout`… Healthcheck смотрит на код ответа, а не на имя, поэтому имена не важны.
- `reconnects`: число `connected` за час по столбцу 2. Переподключения после idle-таймаута считаются, так и должно быть.
- `gaps`: считаются строки по `wc -l`, то есть по переводам строки. Строка без `\n` не учитывается до следующего прогона, двойного уведомления нет.
- `feed`: самый свежий mtime из `last_seq.txt` и файлов текущего и прошлого часа.

**Прогон тестов (2026-10-01, `docker run --rm --network none … ubuntu:24.04 bash /deploy/test/test-healthcheck.sh`):** до правок 42/42, после добавления случаев 009 — **50/50**. shellcheck по всем 10 скриптам (`koalaman/shellcheck:stable`, `--network none`, `-x`): rc=0; `bash -n` ок.

Добавлено в `deploy/test/test-healthcheck.sh` (разделы 11 и 12):
- реальные строки `connected … requested=77169135 mode=header` и `backlog done …` из `data/feed-test-009/connections.tsv` (время сдвинуто к «сейчас»; у этой `backlog` ещё поля старого правила);
- строки в формате финального кода (`resume::Backlog::detail`): `client_close server_replied` с `"idle timeout"`, `disconnected idle_timeout 101`, `backlog session_ended`, `client_close send_failed` (`"recorder writer gone"`) → 0 уведомлений, `ban=ok reconnects=ok`;
- плановый рестарт `shutdown` → `client_close` → `startup_wait min_connect_interval 97.5` → 0 уведомлений; затем `connected` + `backlog done` (621 блок) → 0;
- 403 после строк 009 → 1 ALERT, `connected` → 1 «восстановлено»;
- строка `gaps.tsv` без перевода строки → 0; дописана → ровно 1 INFO «1 шт., 100 блоков».

**З1 (не связано с 009, не исправлял):** если соединение живо, но идут только ping без блоков, это не видит ни recorder, ни healthcheck.
- Recorder: idle-таймаут сбрасывается любым кадром, в том числе ping (`run_connection`: `timeout(idle, ws.next_frame())`).
- Healthcheck: ping пишутся в часовой файл, фрейм закрывается раз в ≤ 60 с, поэтому mtime файла часа свежий. `last_seq.txt` при этом не обновляется: `commit` переписывает его, только если seq вырос.
- Итог: молчит до переподключения, после чего появится дыра, а в отчёте аудита — на следующий день.

Предложения:
- (а) infra-ops: в `healthcheck.sh` считать возраст `feed` по mtime `last_seq.txt`, а часовые файлы брать только если `last_seq.txt` нет. Сейчас заголовок алерта «нет новых блоков» не соответствует проверке. Добавить случай в тест: файл часа свежий, `last_seq.txt` старый → ALERT.
- (б) indexer-engineer, отдельной задачей: idle-таймаут recorder по кадрам с seq > 0 (или отдельный `--block-idle-timeout-secs`, например 30 с), чтобы залипание лечилось переподключением с досылкой.

Связано ли это с реальным поведением фида, не проверено: случаев «ping без блоков» в записях 001/002/009 не искал.

## 4. feed-audit-daily.sh — PASS (одна правка)

- Флаги корректны. `--now` передавать не нужно: по умолчанию это текущее UTC. Окно Р3 (`HH:00 + frame-secs + 60 с`) для 00:10 давно закрыто, поэтому оборванный хвост часа 23 законно даёт FAIL. Recorder к этому времени закрыл фрейм сам: по таймеру фрейма writer'а, даже без строк, или после рестарта перенёс хвост в `_torn/`. Догоняющий запуск (`Persistent=true`) идёт ещё позже, вывод тот же.
- Добавлено: `AUDIT_FRAME_SECS` (по умолчанию 60) → `--frame-secs`. Значение должно совпадать с `--frame-secs` recorder, как просил 009.
- Проверено 2026-10-01 на Mac, офлайн. Копия `data/feed-test-009` лежала в scratchpad, notifier подставной, `RPC_URL=http://127.0.0.1:9`. Команда `feed-audit-daily.sh 20261001` → `verdict PASS`, `blocks 9525`, `sessions 2`, `# exit 0`, ровно одно INFO. Оригинал данных не тронут.
- Известное ограничение из отчёта infra-ops подтверждаю: стык суток (последний seq дня N−1 → первый seq дня N) аудит не проверяет. Дыра ровно на полуночи видна только в `gaps.tsv` и healthcheck.

## 5. Изменения, внесённые в `deploy/`

1. `deploy/feed-audit-daily.sh`: переменная `AUDIT_FRAME_SECS` (60) и флаг `--frame-secs "$AUDIT_FRAME_SECS"`, комментарий, почему не нужен `--now`.
2. `deploy/test/test-healthcheck.sh`: разделы 11 (строки 009: `requested=`/`mode=`, `backlog`, `client_close`, `idle_timeout`, `startup_wait min_connect_interval`, 403 после них) и 12 (недописанная строка `gaps.tsv`); функция `conn_full`. 42 → 50 проверок.
3. `deploy/README.md`:
   - таблица файлов: `recorder.service` (009), feed-audit с `--frame-secs`;
   - «Первые 15 минут»: `connected … requested=- mode=no_data` + строка `backlog`; команда feed-audit с `--frame-secs 60`, `open_tail` прошлого часа в первые ~2 мин, `--now` для старых записей;
   - чек-лист: chrony нужен и для границы бэклога (отставание < 2 с);
   - мониторинг: какие события учитывает `ban`; новый пункт «чего не ловит» — залипание с одними ping (З1);
   - «Обновление бинарника»: переписан раздел «что ожидаемо». Интервал считается от `connected`, рестарт после долгой сессии подключается сразу; досылка и бэклог; открытый вопрос и осторожный вариант `stop && sleep 120 && start`; исправлен откат (`install` + `mv` вместо `cp`); откат досылки drop-in'ом с `--no-requested-seq`; фраза про reboot;
   - бэкап: фид досылает только ~60–70 с;
   - дозаливка: код выхода при исчерпании `--max-calls` и поведение при недописанной строке `gaps.tsv`;
   - «Поведение recorder (задачи 008, 009)»: Close 1000 и при idle-таймауте и пропаже writer, все исходы `client_close`; новый подраздел «Досылка после переподключения»; описание `connections.tsv` (события, 11 столбцов, новые поля только в `detail`); SIGTERM во время паузы; idle-таймаут; интервал от `connected`.

Юниты, `healthcheck.sh`, `bootstrap.sh`, `build-on-server.sh`, `backup.sh`, `notify.sh` не менял.

## 6. Ответы на вопросы infra-ops к indexer-engineer

**(а) Отдельный код выхода для «исчерпан `--max-calls`».** Сейчас его нет. Проверено по коду 2026-10-01: `rpc.rs` возвращает `CallError::Failed(anyhow!("… call budget exhausted …"))`, ошибка поднимается из `run`, `main` возвращает `Err` → **код 1**, как у любой ошибки. Отдельный код только у сигнала — 130 (`main.rs`, тест `tests/interrupt.rs`). Юнит видит `Result=exit-code`, срабатывает `OnFailure` → «юнит enricher-gaps завершился с ошибкой» каждый час, пока очередь дыр больше бюджета. Прогресс при этом сохраняется: готовые файлы уже в `filled.tsv`, недописанный `*.partial` удаляется. Бюджет проверяется до отправки пакета, так что лишнего вызова не бывает.

Предложение (не реализовано, отдельная задача для indexer-engineer):
- типизированная ошибка `BudgetExhausted` и выход с **кодом 75** (`EX_TEMPFAIL`), только если ошибка — именно бюджет;
- в лог — `remaining_blocks` / `remaining_ranges`, в `--stats-json` — поле `budget_exhausted: true`;
- в юните `SuccessExitStatus=75`. Отставание дозаливки и так ловит `backfill` в healthcheck (дыры старше 24 ч), так что тишина при коде 75 ничего не прячет;
- лучше вместе с этим планировать прогон по бюджету: брать только файлы, для которых `blocks × 2 ≤` остаток бюджета, и выходить с 75 на границе файла, а не внутри него. Тогда вызовы не теряются (см. замечание про `--chunk` в п. 1).

**(б) Терпит ли `enricher --gaps` недописанную последнюю строку `gaps.tsv`.** Нет, не пропускает её, а **завершает прогон с ошибкой, ничего не скачав**. Неверный диапазон из неё получиться практически не может.
- Код: `ranges::parse_ranges_tsv` — строка, которая не разбирается, это ошибка, а не пропуск (так задумано, тест `parses_recorder_gaps_format` проверяет `"5\tx"` → ошибка). Отдельного теста на недописанную последнюю строку нет.
- Как recorder пишет строку. `writer.rs` `commit`: `writeln!(f, "{g}")` в небуферизованный `File`. Это два `write()`: вся строка `from\tto\trecv_ns` и затем `\n`. `reconcile_gaps` при старте использует `writeln!(f, "{}\t{}\t{}", …)` — до шести `write()`, каждое число одним вызовом. Читатель может увидеть строку без `\n` или (только в `reconcile_gaps`) обрыв по границе поля.
- Проверено 2026-10-01: `enricher --gaps … --dry-run`, `RPC_URL=http://127.0.0.1:9`, вызовов RPC 0 (`--dry-run` выходит до `make_rpc`):

  | последняя строка | результат |
  |---|---|
  | `77200000` | `Error: gaps line 2: "77200000"`, код 1 |
  | `77200000\t` | та же ошибка, код 1 |
  | `77200000\t77200099` (без recv_ns и `\n`) | план верный, 678 блоков |
  | `77200000\t77200099\t17908` (обрыв recv_ns) | план верный (3-й столбец не читается) |

- Тихо испорченный диапазон (обрыв внутри числа `to`) из `commit` невозможен: строка пишется одним `write()`. В `reconcile_gaps` число тоже пишется одним вызовом. Даже обрыв внутри числа дал бы `to` с меньшим числом цифр, то есть `to < from` → ошибка `bad range` (кроме перехода через разряд, 10^8 блоков — неактуально).
- Для юнита: редкий сбой → `OnFailure` → следующий час проходит. Если хочется полной терпимости, можно в enricher игнорировать последнюю строку без `\n`, с предупреждением. Это изменение кода, вопрос в очередь задач, сам не делал.

## Проверено / предполагается

**Проверено (2026-10-01):**
- чтение кода `crates/recorder/src/{main,net,backoff,resume,writer}.rs`, `crates/enricher/src/{main,lib,ranges,rpc}.rs`, `feed_audit.py`;
- `test-healthcheck.sh` в `ubuntu:24.04` с `--network none`: 50/50;
- shellcheck rc=0, `bash -n` ок;
- `feed-audit-daily.sh` офлайн на копии 009: PASS, одно INFO;
- `enricher --gaps --dry-run` на 4 вариантах недописанной строки, RPC 0.

**Предполагается, не проверено:**
- что сервер фида досылает бэклог и при немедленном реконнекте после Close (проверено на моке и одной живой точкой 005);
- поведение `time-sync.target` / `chrony-wait.service` на Ubuntu 24.04;
- что состояние «ping без блоков» вообще встречается у этого фида;
- юниты под systemd я сам не прогонял (`systemd-analyze verify` делал infra-ops), юниты не менял.

`cargo build/test/clippy` не запускал: `crates/` не менялся. Отладочная сборка `enricher` нужна была только для `--dry-run`.
