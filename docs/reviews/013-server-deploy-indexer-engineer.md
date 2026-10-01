# 013 — Развёртывание recorder на сервере. Ревью indexer-engineer: первые 15 мин записи, пути и флаги

date: 2026-10-01
reviewer: indexer-engineer
объект: recorder на `hood-rec`, коммит `1b13aa4`, старт 2026-10-01T09:57:12Z
**вердикт: PASS** (пункты 1–5 — PASS; замечания не блокируют)

## Как проверял

- Сервер: только `ssh hood-rec` через ControlMaster, 5 сессий команд, всё только на чтение. Команды: `ps -o … -C recorder`, `systemctl show/cat`, `sha256sum` (бинарник, `/proc/<pid>/exe`, unit), `ls -la`, `stat`, `find ! -user hood`, `cat` служебных файлов, `zstd -t`, `zstd -lv`, `zstd -dc | awk` (потоком, на диск ничего не пишется), `journalctl -u recorder|healthcheck`, `journalctl -t hood-notify`, `chronyc -n tracking`. awk-скрипт разбора передавался через stdin ssh (`bash -s`). Подключений к фиду и вызовов RPC нет. Рестартов нет. IP в выводе скрыты.
- Одна оплошность: из-за опечатки в команде прошёл один вызов `scp` в `hood-rec:/dev/stdout` (вывод подавлен). Файл на сервере не создан: это запись в поток `scp` на удалённой стороне. Больше таких вызовов не было.
- Код: `git show 1b13aa4:crates/recorder/src/{main,net,resume,backoff,writer}.rs`. Локально `git diff 1b13aa4 -- crates/` пуст.
- Снимки: 10:15:15Z (процесс, unit, файлы), 10:15:30Z (фреймы), 10:16:04Z (разбор seq), 10:16:24Z (журналы).

## 1. Бинарник и unit — PASS

Проверено (2026-10-01 10:15Z, `ps`, `systemctl show`, `sha256sum`):
- Процесс PID 7347, пользователь `hood`, аргументы `/opt/hoodchain-mev/bin/recorder --out-dir /srv/hood/data/feed`. Больше флагов нет.
- `systemctl show`:
  - `User=hood`, `Group=hood`, `Restart=always`, `RestartUSec=2min`, `KillSignal=15` (SIGTERM), `KillMode=mixed`, `TimeoutStopUSec=30s`;
  - `WorkingDirectory=/opt/hoodchain-mev`, `ReadWritePaths=/srv/hood/data/feed`;
  - `Environment=RUST_LOG=info`, и только это: ни `FEED_URL`, ни `RECORDER_OUT_DIR`, ни `EnvironmentFile`;
  - `NRestarts=0`, `ExecMainStartTimestamp` 09:57:12Z.
- `/etc/systemd/system/recorder.service`: sha256 `641172f3…f68a2`. Совпадает с `deploy/recorder.service` из `1b13aa4` в репозитории и с `/opt/hoodchain-mev/src/deploy/recorder.service`. Drop-in каталога `recorder.service.d` нет.
- sha256 бинарника `/opt/hoodchain-mev/bin/recorder` и работающего `/proc/7347/exe` — `f65c4fab68480234304b0e2014d15465f67ed34f0a653d0f28f3c02508f59761`, совпадает с `BUILD_INFO`. В `BUILD_INFO`: `git=1b13aa4` без `-dirty`, `rustc=1.98.1`.
- Флаги не заданы, поэтому действуют умолчания `Args` в `main.rs@1b13aa4`:
  - `--min-connect-interval-secs 120`. Отсчёт от конца прошлой сессии (`session_end_ns`): берётся последняя строка `connections.tsv` после последнего `connected` или mtime самого нового часового файла, если он позже;
  - `--block-idle-timeout-secs 30`;
  - `--idle-timeout-secs 10`;
  - `--frame-secs 60`;
  - `--zstd-level 3`;
  - заголовок досылки включён: `--no-requested-seq` не задан;
  - `--ignore-pending-pause` не задан.

  URL по умолчанию подтверждает `detail` строки `connected`: `wss://feed.mainnet.chain.robinhood.com`.
- Владельцы:
  - `/srv/hood/data/feed` — `hood:hood 750`;
  - всё внутри, включая `2026/10/01/*`, `connections.tsv`, `last_seq.txt`, — `hood`. `find ! -user hood` пуст;
  - `/srv/hood` — `root:root 755`, `/srv/hood/data` — `hood:hood 750`.

## 2. connections.tsv — PASS

Проверено (10:15Z, `cat`, `awk -F'\t' '{print NF}'`): 3 строки, у всех ровно 11 полей.
- Заголовок: `# ts_utc ts_unix_ns event reason http_status retry_after pause_s session_s envelopes strikes detail` — 11 столбцов, как в `data-model.md`.
- `2026-10-01T09:57:12.952Z connected - 101 … strikes=0 wss://feed.mainnet.chain.robinhood.com requested=- mode=no_data`.
- `2026-10-01T09:57:13.282Z backlog done 101 … session_s=0.000 envelopes=0`, detail:
  - `requested=-`, `last_seq_before=-`, `first_seq=77284408`, `first_minus_requested=-`;
  - `first_lag_ms=1282`, `backlog_blocks=0`, `backlog_end_seq=-`;
  - `live_seq=77284408`, `live_lag_ms=1282`, `live_after_ms=0`;
  - `stale_frames=0`, `complete=true`.

  Для первого старта без заголовка это норма: первый кадр уже живой. Лаг 1.28 с меньше порога 2 с. Он объясняется секундным разрешением `header.timestamp` и не говорит об ошибке часов: chrony показывает смещение 0.05 мс.
- Строк `disconnected`, `startup_wait`, `client_close`, `shutdown`, `torn_repair`, `gap_reconciled`, `writer_error` нет. 403/429 нет.

## 3. Данные — PASS

Проверено (10:15–10:16Z, `ls`, `zstd -t`, `zstd -lv`, потоковый `zstd -dc | awk`):
- Файлы в `/srv/hood/data/feed/2026/10/01/`:
  - `feed-20261001-09.tsv.zst`: 1 975 796 Б, mtime 10:00Z, закрыт;
  - `feed-20261001-10.tsv.zst`: пишется.
- `gaps.tsv` нет. `_torn/` нет. В журнале `recovery done state=None data=None resume=None torn_repairs=0`, то есть каталог перед стартом был пуст.
- Фреймы:
  - Час 09: `zstd -lv` — 3 фрейма, XXH64. `zstd -t` OK, 12 022 636 Б в распакованном виде. 3 фрейма — то, что ожидается: 09:57:12→09:58:12, 09:58:12→09:59:12 и последний фрейм, закрытый на границе часа.
  - Час 10 в 10:15:30Z: `zstd -lv` насчитал 15 фреймов, затем `Error while reading block header` — это открытый последний фрейм. 15 закрытых фреймов за 15.5 мин часа — по одному в минуту, как и должно быть.
  - Журнал: в 10:16:02Z `frame committed … frames=19`, это 3 + 16.
- Открытый фрейм. Сначала фиксировал длину файла (`stat -c %s`), читал ровно столько байт (`head -c $S | zstd -dc`), а `premature end` в конце игнорировал. `zstd -t` на таком снимке даёт `premature end`, это ожидаемо. Целостность закрытых фреймов часа 10 подтверждает то, что поток распаковался до последнего закрытого фрейма без ошибок. Снимок 10:16:04Z пришёлся через 2 с после коммита фрейма (10:16:02Z). Последняя строка снимка — seq 77295583, ровно `last_seq.txt`.
- `last_seq.txt` = 77295583 (mtime 10:16:02Z). Это максимальный seq в данных: найден в последней строке снимка, строк с seq больше `last_seq.txt` — 0.
- Непрерывность. Часы 09 и 10 разбирались одним потоком, так что стык часов тоже проверен.

  | | Значение |
  |---|---|
  | строк всего | 11 772, у всех ровно 4 TSV-поля |
  | строк с блоком | 11 176, seq 77284408 → 77295583 |
  | шагов `seq = prev+1` | 11 175 из 11 175 |
  | дыр | 0 |
  | повторов | 0 |
  | конвертов с > 1 сообщением | 0 |
  | строк с seq 0 | 596: ping (`recorderFrame` opcode ping) — 565, `confirmedSequenceNumberMessage` — 31, прочих — 0 |
  | kind | 3 — 11 135, 13 — 31, 9 — 9, 12 — 1 |
  | только час 09 | 1 754 строки, 1 667 блоков (77284408…77286074), 84 ping, 3 confirmed, 0 дыр |

- Темп:
  - **9.900 блока/с** по `recv_ns`: 11 176 блоков за 1 128.9 с, с 09:57:13.28 до 10:16:02.17Z;
  - час 09 — 10.01 блока/с, 1 667 блоков за 166.6 с;
  - строки `alive` в журнале — +586…+606 seq в минуту.

## 4. Заголовок досылки при следующем подключении — PASS (по коду)

Проверено чтением кода (`1b13aa4`), переподключение не вызывалось:
- **Переподключение внутри процесса.** `main.rs`: перед каждым `run_connection` вызывается `let (requested, mode) = requested_seq(last_seq, !args.no_requested_seq)`. `last_seq` — переменная процесса. `Sink::push` (`net.rs` L444) поднимает её до `seq_max` каждого блока, переданного writer'у. `resume.rs::requested_seq` возвращает `(Some(last_seq + 1), Header)`, если данные есть и флаг не задан. `net.rs::handshake_request` в этом случае добавляет `Arbitrum-Feed-Client-Version: 2` и `Arbitrum-Requested-Sequence-Number: N`. В `connections.tsv` будет `requested=<last_seq+1> mode=header`.
- **Рестарт процесса.** `last_seq = rec.data_seq` — это `writer::recover`, максимум seq в данных, а не `last_seq.txt`. Сейчас данные есть, так что после рестарта уйдёт `requested=<max seq в данных + 1>`. Кроме того, в `connections.tsv` теперь есть `connected`. Поэтому `session_end_ns` вернёт конец сессии, и перед первым подключением будет `startup_wait min_connect_interval` на остаток до 120 с.
- Ни флага `--no-requested-seq`, ни `--ignore-pending-pause` в unit нет (п. 1).

## 5. Журналы — PASS

Проверено (10:16Z, `journalctl`):
- `journalctl -u recorder` с 09:50Z: 43 строки, все `INFO`. WARN и ERROR — 0. Последовательность: `Started` → `recovery done` → `connected requested=None` → `backlog` → `writing file=…-09` → далее раз в минуту `alive` и `frame committed`, 19 коммитов к 10:16:02Z. Больше сообщений systemd нет, рестартов нет.
- healthcheck:
  - 09:55:17Z (до старта recorder): `unit=BAD feed=BAD`, ушли 2 ALERT в `hood-notify` — ожидаемо по runbook;
  - 10:00:27Z: «восстановлено» по обоим;
  - прогоны 10:00, 10:05, 10:10, 10:15: `unit=ok ban=ok feed=ok disk=ok reconnects=ok writer=ok gaps_rows=0 clock=ok`, `feed_src=last_seq.txt`, `feed_age_s` 26/35/45/47;
  - в `/var/lib/hoodchain/health/` только `gaps.offset`, файлов `*.alert` нет.
- chrony: `Leap status: Normal`, смещение 0.000051 с.

## Замечания (не блокируют)

1. **Ложная подсказка «только ping?» в сообщении «восстановлено».** Пример — `hood-notify` 10:00:26Z: «last_seq.txt не менялся 26 с … Файл часа при этом обновлялся 0 с назад: соединение, похоже, живо, но блоков нет (только ping?)».

   Причина — `deploy/healthcheck.sh` L163–165. Подсказка приписывается к `feed_msg` всегда, если файл часа свежее порога. Writer пишет сжатый поток в часовой файл постоянно, а `last_seq.txt` обновляет раз в минуту. Поэтому в нормальной работе условие выполняется почти всегда, и в OK-сообщении подсказка вводит в заблуждение. В ALERT, где `last_seq.txt` старше 300 с, подсказка уместна.

   Предлагаю добавлять её только когда `feed` = BAD. Это задача для infra-ops, мелкая.
2. **Hostname сервера по умолчанию.** Сейчас это `Ubuntu-resolute-latest-amd64-base.zst`, он попадает в текст уведомлений. Косметика. Решать вместе с вопросом о часовом поясе из отчёта 013.
3. **kind 12.** 1 сообщение за 19 мин, в 001/002/009 kind 12 не встречался. Для recorder это ничего не меняет: сырьё пишется как есть. Для декодеров 1b нужно разобраться. Предполагается, что это `L1MessageType_EthDeposit` по нумерации Nitro, не проверено.

## Предполагается, не проверено

- Поведение при реальном переподключении (`mode=header`, `first_minus_requested=0`, бэклог) на этом сервере и с этого IP. Подтверждено только чтением кода и тестами 009/012. Живой проверкой будет первый `block_idle`/обрыв или плановый рестарт по runbook.
- Что открытый фрейм часа 10 к моменту снимка 10:16:04Z не содержал блоков. Снимок закончился ровно на `last_seq.txt`. Хвост открытого фрейма мог не распаковаться до конца блока zstd, на выводы о непрерывности это не влияет.
