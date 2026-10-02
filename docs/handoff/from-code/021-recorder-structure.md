# 021 — Структура recorder: отчёт

Исполнитель: indexer-engineer. Дата: 2026-10-02. База: работа начата на HEAD `9e1b5ab` (019); к моменту ревью HEAD `87bf774` (между ними коммиты 020/022/023/024, `git log 9e1b5ab..87bf774 -- crates/recorder` пуст). При коммите 021 база — `87bf774`. Затронуты только `crates/recorder` (+ `examples/feed_probe.rs`) и одна запись в `.claude/skills/hoodchain-mev/references/chain-facts.md`. `Cargo.toml`/`Cargo.lock` не менялись, новых зависимостей нет. Не закоммичено, статус задачи не менялся, ревьюеры не запускались, деплоя не было.

## Сделано

Все 7 пунктов задачи плюс мелочи из приоритетного списка ревью (Р2, Р4, Р5, часть Р3).

| П. | Что | Где |
|---|---|---|
| 7 | Проверка серверного сырья до правки восстановления (результат ниже) | scratchpad `021/feed/`, `chain-facts.md` |
| 1 | `rawline.rs`: один разборщик строки сырья (`parse_raw_line`, `RawLine::seqs`, `for_each_raw_line`, `file_seq_max`). `max_seq_in_file` удалён; стык с файлом n-3 считается по `seq_max` сообщений, как writer. Столбцы seq разбираются сразу как `u64`, без каста из `u128` | `src/rawline.rs`, `src/recovery.rs` |
| 2 | `SeqTracker` (`check` / `advance` / `observe`, `Seen::{Stale, Fresh{seam, intra}}`) — одно правило для `FeedWriter::accept`, `scan_seq_holes` и `Sink::push` | `src/seqtrack.rs` |
| 3 | `lib.rs` + тонкий `main.rs` (83 строки: `Args` побайтово как в HEAD, `tracing`, код выхода) + `app.rs` (`startup_wait_phase`, `net_loop`, `run` → `Outcome`, `EXIT_WRITER_ERROR = 2`). В `run_connection` четыре копии ветки закрытия заменены одним путём через `enum ClientEnd`; неизменяемые параметры — в `ConnParams`, `#[allow(too_many_arguments)]` больше нет | `src/lib.rs`, `src/main.rs`, `src/app.rs`, `src/session.rs` |
| 4 | `connlog.rs`: `ConnEventKind` с текущими строками, `mod col` (константы столбцов), конструктор на каждое событие (`ConnEvent::connected/backlog/…`), чистая `format_row(ns, &event)`. Golden-тест сверяет строки всех событий; 5 строк из них взяты побайтово из журналов старого бинарника (`data/feed-test-009`, `data/feed-test-002`), ещё у одной (`torn_repair`) оттуда взят `detail` | `src/connlog.rs` |
| 5 | Контекст ошибок: путь и шаг (`create`, `open`, `start zstd frame in`, `write line to`, `finish zstd frame of`, `flush`, `fsync`, `truncate … to N bytes`, `read`, `stat`, `zstd reader for`). `HourFile` хранит `path` | `src/writer.rs`, `src/recovery.rs`, `src/rawline.rs`, `src/app.rs` |
| 6 | `feed_probe` собран на `recorder::transport` (`connect`, `close_handshake`, `close_summary`, `opcode_name`, `tls_connector`). Копия `HeadTap`/TLS/разбора заголовков удалена. URL — флаг `--url` со значением по умолчанию `hood_core::FEED_URL` | `examples/feed_probe.rs` |
| опц. | Пропущенная строка `gaps.tsv` теперь видна в `connections.tsv`: новое событие `gaps_line_skipped` (см. «Изменения поведения») | `src/connlog.rs`, `src/recovery.rs`, `src/app.rs` |

## Было → стало по модулям

| Было (HEAD 9e1b5ab) | Стало |
|---|---|
| `main.rs` 399 строк: CLI, `main` на 263 строки, все литералы `connections.tsv` | `main.rs` 83 (CLI + запуск), `app.rs` 298 (запуск, цикл, остановка, коды выхода), `lib.rs` 68 (карта модулей, `now_ns`) |
| `net.rs` 848: транспорт, сессия, журнал | `transport.rs` 397 (TCP/TLS/апгрейд, `HeadTap`, `ConnectError`, `close_handshake`, `CloseReply`), `session.rs` 339 (`Sink`, `run_connection`, `ClientEnd`, `ConnEnd`), `connlog.rs` 605 (контракт журнала, ~210 из них тесты) |
| `writer.rs` 953: раскладка, восстановление, `gaps.tsv`, writer | `layout.rs` 99 (единственное место схемы `YYYY/MM/DD/feed-YYYYMMDD-HH.tsv.zst`: `hour_path`, `hour_key`, `list_feed_files`), `recovery.rs` 562 (~275 тесты), `writer.rs` 461 (~190 тесты), `rawline.rs` 155, `seqtrack.rs` 134 |
| `backoff.rs`, `resume.rs`, `route.rs` | без изменений; в `backoff.rs` только пути в тестах (`crate::net` → `crate::connlog`) |
| `examples/feed_probe.rs` 418, копия сетевого кода | 302, на общем коде |
| `tests/mock_feed.rs`: свои `CONN_HEADER`, `now_ns`, обход файлов | берёт их из lib (`recorder::connlog::CONNECTIONS_HEADER`, `recorder::now_ns`, `recorder::layout::list_feed_files`) + новый E2E-тест |

Видимость (после ревью): наружу только `app` (бинарник), `transport` (проба), `connlog` (контракт журнала: тесты и проба), `list_feed_files` (реэкспорт из `layout`, тесты) и `now_ns`. Остальные модули приватные.

## П. 7: серверное сырьё (проверено 2026-10-02)

Порядок: `ssh -O check hood-rec`. Мастер-соединения не было, поэтому один вызов `rsync -a --files-from` открыл одно соединение (ControlMaster auto). Скопированы 4 закрытых часа: `2026/10/01/feed-20261001-23`, `2026/10/02/feed-20261002-{00,09,16}.tsv.zst`, 236 МБ. На сервер ничего не писалось, текущий час (18 UTC) и recorder не трогал. Разбор офлайн (`scratchpad/021/count.py`, полный `json.loads` каждой строки):

| файл | строк | с seq | без seq | `seq_first≠seq_last` | >1 сообщения | не по порядку | дыры/повторы |
|---|---|---|---|---|---|---|---|
| 20261001-23 | 37 543 | 35 646 | 1 897 | 0 | 0 | 0 | 0 |
| 20261002-00 | 37 586 | 35 682 | 1 904 | 0 | 0 | 0 | 0 |
| 20261002-09 | 37 556 | 35 673 | 1 883 | 0 | 0 | 0 | 0 |
| 20261002-16 | 37 713 | 35 787 | 1 926 | 0 | 0 | 0 | 0 |

Стык 23→00 непрерывен (77787154 → 77787155). Во всех файлах 4 столбца, `seq_first`/`seq_last` совпадают с `sequenceNumber` в JSON. Вывод: на данных старое и новое правило стыка дают одно и то же. Новое поведение срабатывает только в ненаблюдавшемся случае (конверт из нескольких сообщений не по порядку в конце файла n-3). Факт внесён в `chain-facts.md`.

## Изменения поведения

Формат сырья, имена файлов, CLI-флаги (`Args` совпадает с HEAD по `diff`), раскладка `connections.tsv` (11 столбцов, строки событий и причин) и счётчик страйков не менялись.

1. **Стык при восстановлении** (п. 1): если файл n-3 кончается конвертом `[110, 108]`, seam = 110, а не 108. Раньше дописалась бы фальшивая дыра `109..110`, а без seq в двух новейших файлах `resume_seq` был бы 108 (повторная запись 109–110). На сервере такого не было (п. 7).
2. **Столбец seq больше `u64`** при восстановлении: строка пропускается. Раньше разбиралась как `u128` и молча обрезалась. Recorder таких строк не пишет.
3. **`parse_pause_row`** (Р2): пауза `inf`/`NaN`/отрицательная считается за 0, сложение насыщающее. Раньше в release было переполнение, в debug — паника. Recorder такие паузы не пишет (`Duration` → `{:.3}`).
4. **Новое событие `gaps_line_skipped`** — строка того же формата: 11 столбцов, `reason` = `broken` или `unterminated`, остальные числовые столбцы `-`, `detail` = `gaps.tsv line <N>: <причина>: "<строка, escape_debug, до 200 символов>"`. Пишется при старте, рядом с `gap_reconciled`/`torn_repair`, по строке на каждую пропущенную строку `gaps.tsv`. Пока строка в файле, событие повторяется при каждом старте. Почему healthcheck его не перепутает (проверено чтением `deploy/healthcheck.sh` и E2E-тестом): `ban` смотрит только строки с `$3` = `connected|disconnected|startup_wait`; `reconnects` — `$3 == "connected"`; `writer` — `shutdown writer_error` и `writer_error`; `feed_audit.py` — только `connected`. Как и у других стартовых строк, время этой строки считается «концом прошлой сессии» для `--min-connect-interval-secs`. Это ошибка в осторожную сторону, как у `torn_repair`/`gap_reconciled`/`startup_wait`.
5. **Тексты ошибок** (п. 5): в `detail` строки `shutdown writer_error` / `writer_error final_commit` и в stderr при выходе с кодом 1 теперь есть путь и шаг. Формат строки тот же, healthcheck берёт `detail` целиком (`$11`).
6. **Журнал**: строка `client close handshake done` пишется теперь и для «writer gone» (раньше там терялся `info!`, В6). Только journald.
7. **`feed_probe`** (не деплоится): новый флаг `--url`; в строке `connected` лога пробы теперь `upgrade_ms=… url=…` вместо сырого заголовка ответа (общий `connect` возвращает разобранный статус, а не сырой заголовок).
8. `expect("interval wait needs an end")` убран (Р4): для невозможного сочетания `detail` = `min interval Ns`.

**Регрессия, пойманная тестами по ходу работы (исправлено):** в первой версии writer сдвигал `last_seq` через `SeqTracker::observe` до записи строки. Тогда смена часа (`ensure_hour → commit`) публиковала в `last_seq.txt` seq строки, которой ещё нет на диске. Упали `writer_commits_frames_gaps_and_state_in_order` и `frame_committed_within_frame_max_in_silence`. Исправлено разделением на `check` (чистое решение) и `advance` (после `write_line`), как было до 021. Добавлен явный тест `hour_rotation_commits_only_written_lines`.

## Проверено (2026-10-02, локально на Mac; сеть — только один rsync из п. 7; фид и RPC не вызывались)

| Команда | Итог |
|---|---|
| `cargo fmt --all -- --check` | exit 0 (весь workspace) |
| `cargo clippy -p recorder --all-targets -- -D warnings` | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| `cargo build --workspace`; `cargo build --release --locked -p recorder` | exit 0; `recorder --help` работает |
| `cargo test -p recorder -p hood-core` (FEED_URL/RPC_URL/RECORDER_OUT_DIR сняты, `TMPDIR` = scratchpad), 3 прогона подряд | юнит recorder 65/65 (было 47), `mock_feed` 15/15 (было 14), hood-core 21/21 |
| `cargo test --workspace` | все зелёные, FAILED нет |

- Все 14 mock-тестов 012 проходят без правок логики: Close 1000 при SIGTERM, ожидание ответа ≤ 2 с, `min_connect_interval` от конца сессии и после kill -9 (mtime), `pending_pause`, заголовок досылки, одна дыра при позднем бэклоге, `no_data`, `--no-requested-seq`, idle → Close → досылка из памяти, `writer_error` → Close → код 2, `block_idle`. Mock-фид только на 127.0.0.1, у бинарника явные `--url ws://127.0.0.1:<port>` и `--out-dir`.
- Новые тесты: `connlog::golden_rows_of_every_event` (все 10 событий, 11 столбцов, позиции, которые читает healthcheck, заголовок); `last_session_and_pause_from_both_log_layouts` (10- и 11-столбцовые строки из `data/feed-test-002`, замена удалённого `parse_connected_row`, Р5); `bad_pause_values_do_not_overflow`; `recovery::out_of_order_envelope_at_the_seam` (п. 1); `errors_carry_the_file_name`, `writer_error_names_the_path` (п. 5); `gap_ranges_reading_policy` (+ пропущенные строки); `seqtrack::*` (5); `rawline::*` (2); `session::client_end_parts_keep_the_logged_strings`, `sink_marks_stale_frames_by_the_shared_rule`; `transport::close_reply_strings_are_unchanged`; `layout::hour_path_is_listed_and_others_are_not`; `writer::hour_rotation_commits_only_written_lines`; E2E `broken_gaps_line_is_logged_and_recording_goes_on` (битая строка `gaps.tsv` → строка `gaps_line_skipped`, 11 столбцов; последняя строка из тех, что смотрит `ban`, — `connected`; запись идёт, код 0; `gaps.tsv` не переписан).
- `cargo clean` не запускал. `crates/enricher`, decoders, sql, analytics, feed_audit, deploy не трогал.

## Предполагается / не проверено

- Поведение на сервере после деплоя: проверяется только плановым рестартом (команды ниже).
- Что фид никогда не шлёт конверты из нескольких сообщений: не установлено. Не встретилось ни одного в 142 788 конвертах (4 часа).
- Вывод pedantic-clippy не пересчитывал: из него закрыты только `too_many_lines` у `main`/`run_connection` и касты из Р2.
- Не сделано (вне задачи или требует решения): Р6 — страйки после долгой сессии (решение Михаила); Р3 — `EndKind::ClientStop/WriterGone` (меняет `reason` в редкой гонке) и newtype времени; Р10 — `Slot::Empty`.

## Документация (после сообщения координатора, 2026-10-02)

Внесено, код не менялся:
- `.claude/skills/hoodchain-mev/references/data-model.md`, раздел `connections.tsv`: событие `gaps_line_skipped` (`reason`, `detail`, повтор при каждом старте, enricher `--gaps` на битой строке падает, healthcheck и `feed_audit.py` его не читают). Где формат задан в коде (`connlog.rs`, golden-тест; `rawline.rs`, `seqtrack.rs`). У ссылки на `main.rs` в описании `client_close` добавлено, что с 021 этот код в `app.rs`.
- `deploy/README.md`: `gaps_line_skipped` добавлен в таблицу healthcheck (`ban`: не учитывается), в таблицу файлов (`connections.tsv`) и в «Поведение при сбоях» (пункт «Сверка дыр при старте»).

## Правки после ревью (architect-reviewer PASS с замечаниями, 2026-10-02)

Только изменения, которые не меняют поведение recorder (идёт аудит data-auditor):

1. В1: `session` и `layout` стали приватными; добавлен `pub use layout::list_feed_files`, `tests/mock_feed.rs` использует `recorder::list_feed_files`. `connlog` остался публичным.
2. Тест `connlog::all_lists_every_event_kind`: исчерпывающий `match` по `ConnEventKind`. Новый вариант не скомпилируется без ветки в `match`, а без добавления в `ALL` тест упадёт.
3. `examples/feed_probe.rs`: журнал recorder (`--recorder-log`) читается через `connlog::col::{EVENT, TS_UNIX_NS}` и `ConnEventKind::{Connected, Disconnected}.as_str()`, без литералов. Собственный журнал пробы переименован в `<out-dir>/probe-connections.tsv`: имя `connections.tsv` и его 11 столбцов принадлежат recorder. Меняется только имя файла пробы; recorder и healthcheck это не затрагивает. Следствие: попытки, записанные старыми прогонами пробы в `connections.tsv`, ограничители пробы больше не видят (задача 005 закрыта, пробу не запускали).
4. Поля `ConnEvent` — `pub(crate)`; снаружи событие строится только конструкторами.
5. База коммита в шапке отчёта исправлена (`87bf774`).

Не делал (это меняет поведение, решено вынести в отдельную задачу): дедупликацию `envelope_seqs` и порядок записи «дыры до данных».

Проверено после правок: `cargo fmt --all` (вне `crates/recorder` ничего не изменилось), `cargo clippy --workspace --all-targets -- -D warnings` → exit 0, `cargo test -p recorder -p hood-core` → юнит recorder 66/66, `mock_feed` 15/15, hood-core 21/21. `cargo clean` не запускал.

## Исправление Б1 (аудит data-auditor, 2026-10-02)

Б1: каждая битая строка `gaps.tsv` давала при старте строку `gaps_line_skipped`. Эти строки писались **до** того, как `last_pause()` читал последние 64 КиБ журнала. Сотни таких строк вытесняли `disconnected 403` из окна, и recorder подключался во время бана: аудитор воспроизвёл ожидание 51 с и 0 с вместо 3540 с у HEAD. Исправлено двумя независимыми правками, обе без изменения формата:

1. `app::run` читает `last_pause()` вместе с `last_session()`, до `recover` и до любой стартовой строки, и передаёт результат в `startup_wait_phase`. Сам `startup_wait_phase` журнал больше не читает.
2. Ограничение: за один старт не больше `SKIPPED_ROWS_MAX` = 20 строк `gaps_line_skipped`, остальное сводится в одну итоговую строку того же формата (11 столбцов, событие `gaps_line_skipped`, `reason` = `more`, `detail` = `gaps.tsv: <K> more skipped lines not listed (<N> skipped in total)`). `connlog::gaps_skipped_events`.

Н1 (повтор строк при каждом старте): выбрал ограничение, а не дедупликацию между стартами. Дедупликация потребовала бы хранить состояние «уже сообщено», а это новый файл или чтение журнала. Теперь за старт пишется не больше 21 строки (~5 КБ), паузу они не задевают (п. 1), и пока строка в файле, это видно после каждого рестарта.

Тесты:
- `connlog::skipped_rows_are_capped_with_one_summary`: 20 строк — без итоговой; 700 — 20 + итоговая; точная строка итоговой.
- E2E `many_broken_gaps_lines_do_not_hide_a_ban`: свежий `disconnected 403` (пауза 3600 с) и 700 битых строк по ~200 символов. Два старта подряд оба пишут `startup_wait pending_pause` > 3400 с, `strikes=1`; подключений нет; 2 × 21 строк `gaps_line_skipped`.

Повтор сценариев c16/c17 аудитора. `bin/rec.sh` запустить как есть нельзя: бинарников `target-head`/`target-new` в scratchpad уже нет, setup-фрагменты не сохранены. Поэтому повторил по описанию своим скриптом `scratchpad/021/b1/run.sh` с release-бинарником текущего дерева: копия `data/feed-test-009`, в журнал дописан `disconnected forbidden 403, Retry-After 3600, pause 3600` минутной давности, в `gaps.tsv` — N битых строк, два старта подряд, каждый останавливается SIGTERM, адрес `ws://127.0.0.1:9`.

| N | старт 1 | старт 2 | строк `gaps_line_skipped` |
|---|---|---|---|
| 300 | `pending_pause` 3539.8 с, strikes=1 | `pending_pause` 3539.0 с, strikes=1 | 42 (2 × 21) |
| 450 | 3539.8 с, strikes=1 | 3539.0 с, strikes=1 | 42 |
| 700 | 3539.8 с, strikes=1 | 3539.0 с, strikes=1 | 42 |

В логах ни одной попытки подключения. Ограничение: правку 1 отдельно от правки 2 на E2E не показать. С ограничением строки одного старта никогда не выходят за 64 КиБ, поэтому правка 1 — это защита «на всякий случай» (её видно по коду `app.rs`).

Документация: `data-model.md` и `deploy/README.md` дополнены ограничением, строкой `more` и порядком чтения журнала.

Н4: журнал `examples/feed_probe.rs` переименован в `probe-connections.tsv` (см. «Правки после ревью», п. 3). Recorder и healthcheck это не затрагивает.

Проверено после правки: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings` → exit 0; `cargo test -p recorder -p hood-core` → юнит recorder 67/67, `mock_feed` 16/16, hood-core 21/21; `cargo build --release --locked -p recorder` → exit 0. `cargo clean` не запускал.

## Вопросы к Cowork / Михаилу

1. Р6 (страйки после долгой сессии) по-прежнему ждёт решения Михаила; в 021 не менялось.

## Деплой

_(заполняет координатор после ревью и планового рестарта)_

### Команды для Михаила (один плановый рестарт по runbook)

Чистый клон уже скопирован rsync'ом в `/opt/hoodchain-mev/src` (делает координатор). Recorder перезапускается **один раз**: простой ~2 мин, одна новая строка `gaps.tsv` на ~500–600 блоков — это ожидаемо (README, «Обновление бинарника без лишней дыры»).

```bash
# 0. Права и состояние до
srv# chown -R hoodbuild:hoodbuild /opt/hoodchain-mev/src
srv# F=/srv/hood/data/feed
srv# systemctl show recorder -p NRestarts -p ActiveEnterTimestamp -p MainPID      # записать значения

# 1. Пре-проверки gaps.tsv (до рестарта)
srv# test ! -s $F/gaps.tsv || tail -c1 $F/gaps.tsv | od -An -c                     # ожидается: \n
srv# awk -F'\t' '!/^#/ && NF && (NF != 3 || $1 !~ /^[0-9]+$/ || $2 !~ /^[0-9]+$/ || $3 !~ /^[0-9]+$/) { print NR": "$0 }' $F/gaps.tsv
#    ожидается: пусто. Если что-то вывелось — новый recorder запишет это в connections.tsv
#    как gaps_line_skipped (запись не остановится), enricher --gaps на такой строке падает: сообщить
srv# wc -l < $F/gaps.tsv; tail -n 2 $F/gaps.tsv; cat $F/last_seq.txt
srv# tail -n 3 $F/connections.tsv

# 2. Сборка на сервере (запись идёт, бинарник подменяется через rename)
srv# bash /opt/hoodchain-mev/src/deploy/build-on-server.sh
srv# cat /opt/hoodchain-mev/bin/BUILD_INFO     # git=<коммит 021> без -dirty; sha256 recorder новый; recorder.prev есть
srv# /opt/hoodchain-mev/bin/recorder --help | head -3

# 3. Рестарт — ОДИН раз, повторно не запускать
srv# systemctl restart recorder
srv# journalctl -u recorder -n 30 --no-pager   # "recovery done", затем "waiting before the first connect" (~120 с)

# 4. Пост-проверки (через ~3 мин после restart)
srv# tail -n 8 $F/connections.tsv
#    ожидается по порядку: shutdown SIGTERM → client_close server_replied → startup_wait min_connect_interval
#    → connected … requested=<старый last_seq+1> mode=header → backlog done;
#    нет disconnected 403/429, нет gaps_line_skipped (если в п. 1 было пусто), у всех строк 11 столбцов:
srv# tail -n 8 $F/connections.tsv | awk -F'\t' '{ print NF }' | sort -u            # ожидается: 11
srv# awk -F'\t' '$3 == "backlog"' $F/connections.tsv | tail -n 1 | tr ' ' '\n' | grep -E '^(requested|first_minus_requested|backlog_blocks|complete)='
srv# tail -n 2 $F/gaps.tsv; tail -c1 $F/gaps.tsv | od -An -c                        # одна новая строка (~500–600 блоков), в конце \n
srv# awk -F'\t' 'NF != 3' $F/gaps.tsv                                               # ожидается: пусто
srv# cat $F/last_seq.txt; sleep 70; cat $F/last_seq.txt                             # растёт (фрейм раз в ≤ 60 с)
srv# systemctl start healthcheck.service; journalctl -u healthcheck.service -n 20 --no-pager
#    допустимо только INFO «новые дыры в фиде»; unit/feed/ban/writer — нет
srv# systemctl show recorder -p NRestarts -p ActiveEnterTimestamp -p MainPID
#    MainPID и ActiveEnterTimestamp новые; NRestarts не больше значения из п. 0
#    (рост при повторной проверке через 10 мин = новый бинарник падает → откат по README, «Откат бинарника»)
srv# journalctl -u recorder --since "-10 min" --no-pager | grep -E ' (WARN|ERROR) '  # только "waiting before the first connect" и "gap in feed"
#    (tracing пишет уровень в текст строки, journald-приоритет у всех строк один, поэтому grep, а не -p warning)
```
