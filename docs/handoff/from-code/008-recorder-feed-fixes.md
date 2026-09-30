# 008 — recorder и feed-audit: исправления по аудитам 002. Отчёт
status: выполнено, все 8 пунктов; ждёт data-auditor
date: 2026-09-30
executor: indexer-engineer
reviewers: data-auditor (не запускался, его запускает основная сессия)

Статус в задаче не менял (по указанию основной сессии он остаётся `in-progress`). Коммитов нет, изменения лежат в рабочем дереве.

## Итог

- Все 8 пунктов сделаны. `cargo build --workspace`, `cargo test --workspace` (69 тестов, 0 упавших) и `cargo clippy --workspace --all-targets -- -D warnings` проходят чисто.
- **Close-фрейм. Было:** на SIGTERM recorder просто ронял сокет, мок видел `Pong` и сразу EOF, без Close. **Стало:** Close 1000, ожидание ответа до 2 с и только потом TLS close_notify и FIN. На моке ответ пришёл через ~0 мс; без ответа recorder сдаётся через 2.002 с. Выход в обоих случаях 0.
- `feed-audit --rpc-sample 0` на копии `data/feed-test-002`: PASS без WARN. ping = 285, `confirmedSequenceNumberMessage` = 26, 9.958 блока/с и 97.0 МБ/ч по сессиям. На копии `data/feed` PASS с цифрами 001: 9.969 блока/с, 90.6 МБ/ч.
- Регрессий `recover` на старом формате (копия `data/feed`) нет: целый файл не тронут; обрезанный файл целиком ушёл в `_torn/`, старт не падает.
- Ни одного подключения к фиду и ни одного вызова RPC. Все прогоны бинарника шли только на `ws://127.0.0.1:<порт>`, с явными `--url` и `--out-dir` и со снятыми `FEED_URL`, `RPC_URL`, `RECORDER_OUT_DIR`. Оригиналы `data/feed` и `data/feed-test-002` не менялись: sha256 всех файлов до и после совпали.

## Сделано

Файлы:

| Файл | Что |
|---|---|
| `crates/recorder/src/writer.rs` | п. 1 (сверка дыр в `recover`), п. 5 (срок фрейма), комментарий к п. 6 |
| `crates/recorder/src/net.rs` | п. 3 (Close на остановке), чтение последнего `connected` из `connections.tsv` |
| `crates/recorder/src/backoff.rs` | п. 2: чистая функция `startup_wait` |
| `crates/recorder/src/main.rs` | п. 2 (флаг, ожидание), п. 3 (остановка через `watch`-канал), события `gap_reconciled` и `client_close` |
| `crates/recorder/src/route.rs` | п. 6 |
| `crates/recorder/tests/mock_feed.rs` | новый: бинарник против локального мока на 127.0.0.1 (п. 2 и 3) |
| `deploy/recorder.service`, `deploy/README.md` | п. 4 |
| `.claude/skills/hoodchain-mev/references/data-model.md` | п. 7 и описание п. 6 и новых событий |
| `.claude/skills/feed-audit/scripts/feed_audit.py`, `SKILL.md` | п. 8 |

`hood-core` и enricher не трогал. Зависимостей не добавлял: в тесте используются `yawc`, `tokio`, `base64`, `futures`, `zstd`, которые уже есть у recorder. Формат строки сырья и имена файлов не менялись.

1. **З1 — сверка дыр при старте.**
   - После ремонта хвостов `recover` проходит по seq в двух последних часовых файлах, в тех же, что проверяются на обрыв. Стартовая точка — максимальный seq файла перед ними, так проверяется стык.
   - Логика повторяет `FeedWriter::accept`:
     - строки, где все seq ≤ уже виденного, пропускаются;
     - разрыв между конвертами — `detect_gap`;
     - разрыв внутри многосообщенческого конверта — `intra_envelope_gaps`. JSON разбирается только у таких конвертов, у одиночных хватает столбцов.
   - Найденные дыры вычитаются из того, что уже есть в `gaps.tsv`. Если дыра покрыта частично, дописывается только непокрытая часть. Остаток дописывается в `gaps.tsv` (`from \t to \t recv_ns`, где `recv_ns` — первой строки после дыры), затем `sync_data` и fsync каталога.
   - На каждую дописанную строку: событие `gap_reconciled` в `connections.tsv` и WARN `hole in data was missing from gaps.tsv, appended`.
   - Повторный запуск ничего не дописывает. Стык «конец данных → первый seq новой сессии» по-прежнему пишет `accept`: recover видит только то, что уже на диске, так что двойной записи нет (тест `live_gap_is_written_once_by_accept_not_by_recover`).
   - Файл перед двумя последними не ремонтируется. Если его не удалось прочитать, будет WARN, и стык не проверяется: старт не должен падать из-за старого файла.
2. **Пауза между подключениями переживает рестарт.**
   - Новый флаг `--min-connect-interval-secs`, по умолчанию 120.
   - При старте recorder берёт из `connections.tsv` время последнего `connected` и остаток паузы из последнего `disconnected` и ждёт дольшее из двух. Событие `startup_wait` с причиной `min_connect_interval` или `pending_pause`.
   - `--ignore-pending-pause` отключает оба ожидания.
   - Разбор `connected` работает с обоими видами строк в `data/feed-test-002/connections.tsv`: 10 столбцов у первого запуска 002 и 11 у последующих.
   - SIGTERM прерывает ожидание, выход 0. Паузы между переподключениями внутри процесса тоже прерываются сигналом (раньше это делал `select`, который ронял весь future).
3. **Close-фрейм при остановке.**
   - По коду до 008: в `main` future сети был в `select!` с сигналом и при сигнале просто ронялся. У нативного `yawc::WebSocket` нет `Drop`, который шлёт Close, так что сокет закрывался без Close.
   - Теперь при сигнале main взводит `watch`-флаг и ждёт сетевой цикл не больше 5 с. Цикл отправляет Close 1000 с причиной `recorder shutdown`. Дальше до 2 с (`CLOSE_REPLY_WAIT`) читает кадры, все они пишутся в сырьё, пока не придёт Close сервера. Затем `ws.close()` за ≤ 0.5 с (TLS close_notify, FIN). Только после этого закрывается канал к писателю, тот дописывает очередь и делает commit.
   - Итог пишется событием `client_close` с причиной `server_replied`, `no_reply`, `stream_ended` или `send_failed`; в detail время ожидания.
   - Было/стало подтверждено тестом на моке, см. ниже.
4. **systemd.**
   - `RestartSec=120`.
   - Комментарий «limit is 2, the 3rd gets 429» помечен как UNVERIFIED (из потерянной сессии).
   - В `deploy/README.md` новый раздел «Почему `RestartSec=120`»: бан 403/3600, п. 2, Close при остановке, цена рестарта. Обновлены описания SIGTERM, kill -9 и ручного `restart`.
5. **З4.**
   - Срок фрейма — `frame_max − COMMIT_GUARD` (200 мс на finish + fsync) от первой строки фрейма.
   - Писатель никогда не спит на канале дольше остатка до срока: `recv_timeout(min(500 мс, time_left))`. Значит, фрейм фиксируется в интервале [`frame_max − 200 мс`, `frame_max`] и в тишине тоже.
   - Потери при kill -9 при `--frame-secs 60` не больше 60 с.
   - Тесты: чистая граница (`frame_deadline_boundary`) и замер на живом потоке писателя (`frame_committed_within_frame_max_in_silence`, `frame_max` = 1.5 с).
6. **З3.**
   - Текст, который не парсится как JSON, пишется как `{"recorderFrame":{"opcode":"text","payloadBase64":"…"}}`: байты точные, CR/LF/TAB сохраняются.
   - Валидный JSON — как раньше: конверт с `messages` получает seq, прочий валидный JSON пишется как есть с seq 0.
   - Удаление `\n`/`\r` в `write_line` осталось. Теперь туда попадает только валидный JSON, а в нём это незначащие пробелы между токенами; это описано в комментарии и в `data-model.md`.
7. **З2 — документация.** В `data-model.md`:
   - частично перекрывающий конверт пишется целиком;
   - дедупликация по `sequenceNumber` — обязанность разбора и загрузчика; какое вхождение брать, решает загрузчик;
   - расхождение `blockHash` у повторов — ошибка данных.
   Там же: непарсящийся текст теперь в обёртке `text`, столбец JSON всегда валиден, список событий `connections.tsv`.
8. **feed-audit.**
   - Строки с seq 0 разбираются по типам: `recorderFrame:<opcode>`, `confirmedSequenceNumberMessage`, `other`, `with_messages`. WARN только для `other` (сюда же битый base64), FAIL для seq 0 с `messages`. В `envelopes` строки с seq 0 не считаются, для всех строк есть отдельный счётчик `lines`.
   - Сессии режутся по `connected` из `connections.tsv` (если файл есть) и по паузам между соседними строками больше `--session-gap-s` (по умолчанию 30). Сессия длится от первой до последней своей строки. `blocks_per_s` и `mb_per_hour` теперь по сессиям, по общему интервалу — отдельно: `recv_span_s`, `blocks_per_s_span`, `mb_per_hour_span`. Интервал между конвертами считается только внутри сессии и только между строками с блоками.
   - Целостность проверяется по фреймам:
     - скрипт сам разбирает заголовки zstd и находит границы фреймов;
     - через `zstd -dc` проходят только целые фреймы, так проверяются содержимое и контрольная сумма;
     - один недописанный фрейм в файле текущего часа UTC — это `open_tail`, не FAIL;
     - оборванный хвост в закрытом часе — FAIL, мусор после фреймов — FAIL.
     Флаг `--current-hour` задаёт открытый час явно.
   - Игнорируются `_torn/`, `connections.tsv`, `*.tmp` и любые имена не вида `feed-*.tsv.zst` (счётчик `ignored_inputs`).
   - `SKILL.md` переписан: убрано «файл текущего часа всегда FAIL», описаны сессии, типы seq 0 и новые флаги; добавлен блок «Проверено 2026-09-30, задача 008».
   - Скрипт ничего не пишет на диск.

## Проверено и как (2026-09-30)

### Сборка и тесты

```
cargo build --workspace                                  → ok
cargo test --workspace                                   → 69 passed, 0 failed
   recorder unit 34 (было 24), recorder tests/mock_feed.rs 4, enricher 16+3+2+2+4, hood-core 2, decoders 2
cargo clippy --workspace --all-targets -- -D warnings    → чисто
cargo fmt -p recorder -- --check                         → чисто (в enricher/decoders/hood-core
                                                           расхождения rustfmt были и до 008, не трогал)
```

`tests/mock_feed.rs` прогонял 5 раз подряд: стабильно, ~4.7 с на прогон.

Новые тесты по критериям приёмки:

| Критерий | Тест | Результат |
|---|---|---|
| З1: разрыв без строки в `gaps.tsv` → ровно одна строка, повторный старт ничего не дописывает | `writer::tests::recover_appends_missing_gap_row_once` | ok: после 1-го `recover` `gaps.tsv` = `104\t106\t6`, после 2-го то же самое |
| З1: стык, дыра внутри конверта, частично перечисленная дыра, дубль | `recover_reconciles_seam_intra_and_partial_holes`, `uncovered_ranges` | ok |
| З1: живой стык пишет `accept`, recover его не дублирует | `live_gap_is_written_once_by_accept_not_by_recover` | ok |
| п. 2: `connected` 30 с назад → ожидание ≈ 90 с | `backoff::tests::startup_wait_after_recent_connect` (ровно 90 с) и бинарник в `mock_feed::min_connect_interval_survives_restart_and_sigterm_interrupts` | ok: `startup_wait min_connect_interval pause_s=89.592` (0.4 с ушли на запуск процесса); мок за 1.5 с не увидел ни одной попытки подключения |
| п. 2: SIGTERM прерывает ожидание, выход 0 | тот же тест | ok: выход 0 меньше чем через 2 с после сигнала, в `connections.tsv` последний — `shutdown SIGTERM`, новых `connected` нет |
| п. 3: мок видит Close 1000 от клиента при SIGTERM | `mock_feed::sigterm_sends_close_1000_and_waits_for_reply` | ok (см. «было/стало») |
| п. 3: сервер не отвечает на Close | `mock_feed::sigterm_without_close_reply_gives_up_after_2s` | ok: выход через 2.0–2.3 с, код 0 |
| п. 5: граница срока фрейма | `frame_deadline_boundary`, `frame_committed_within_frame_max_in_silence` | ok |
| п. 6 | `route::tests::unparseable_text_is_base64_wrapped`, `valid_json_of_unknown_shape_is_kept_verbatim` | ok |

### Close-фрейм: было / стало

Мок — `tests/mock_feed.rs`, сервер на `yawc` (`Role::Server`) на 127.0.0.1. Он шлёт 5 конвертов реальной формы (seq 100..104) и ping, затем тест посылает бинарнику SIGTERM.

**Было** (тот же тест на коде до правок, 2026-09-30):
```
mock saw no Close frame from the client before EOF, frames: ["Pong", "stream end: Connection is closed"]
```
Клиент ответил на ping и закрыл TCP без Close.

**Стало:**
```
14:44:59.946Z connected       ws://127.0.0.1:51137/
14:45:00.271Z shutdown        SIGTERM
14:45:00.272Z client_close    server_replied  session_s=0.325 envelopes=5  close frame code=Some(1000) reason=""
```
- Мок получил Close с кодом 1000 меньше чем через 2 с после сигнала. Выход 0.
- В сырье seq 100..104, ping и ответный close-кадр сервера (`recorderFrame:close`); `last_seq.txt` = 104.
- Без ответа сервера: `client_close no_reply … no close frame from server within 2s`, выход через 2.002 с.

### feed-audit: до / после

Все прогоны на копиях в scratch (права только на чтение) с `--rpc-sample 0 --rpc-url http://127.0.0.1:9` и без `RPC_URL` в окружении.

Копия `data/feed-test-002`:

| Поле | До (скрипт 001) | После |
|---|---|---|
| verdict | PASS | PASS |
| WARN | `311 unparseable envelopes stored with seq 0` | нет |
| envelopes | 5968 (вместе с seq 0) | 5657; `lines` 5968 |
| seq 0 | — | `recorderFrame:ping` 285, `confirmedSequenceNumberMessage` 26 |
| zstd_frames | — | 10 |
| сессии | — | 2: 12:52:21.511Z +240.4 с, 2380 блоков; 13:57:32.601Z +327.7 с, 3277 блоков |
| blocks_per_s | 1.335 (по интервалу) | **9.958** по сессиям; `blocks_per_s_span` 1.335 |
| mb_per_hour | 13.0 | **97.0** по сессиям; `mb_per_hour_span` 13.0 |
| interarrival_ms | p50 113.3, p99 179.8, max 3 670 695.6 | p50 113.6, p99 180.3, max 1177.0 |
| blocks / gaps | 5657; (76532007, 76568582), есть в `gaps.tsv` | то же |

Копия `data/feed` (001):

| Поле | До | После |
|---|---|---|
| verdict | PASS | PASS |
| blocks / expected / missing | 6015 / 6015 / 0 | то же |
| kinds | 3: 5990, 13: 24, 9: 1 | то же |
| blocks_per_s / mb_per_hour | 9.969 / 90.6 | 9.969 / 90.6 (1 сессия, 603.4 с) |
| interarrival_ms | p50 113.7, p99 171.2, max 976.5 | то же |
| zstd_frames | — | 1 |

Синтетика (копии в scratch):

| Случай | Результат |
|---|---|
| glob со всей папкой: `_torn/*`, `connections.tsv`, `gaps.tsv`, `feed-…-15.tsv.zst.tmp` | `ignored_inputs 4`, PASS, цифры как выше |
| файл 14 обрезан на 100 000 Б (внутри последнего фрейма), `--current-hour 20260930-14` | PASS, `open_tail` 2 094 044 Б |
| то же, `--current-hour 20260930-15` (час закрыт) | FAIL `torn zstd tail in closed hour … after 2 complete frames` |
| 13 байт мусора после фреймов | FAIL `garbage after the last complete zstd frame` |
| испорчен байт внутри целого фрейма файла 13 | FAIL `Restored data doesn't match checksum` |
| добавлен фрейм: `recorderFrame:text`, «not json» с seq 0, seq 0 с `messages` | `recorderFrame:text` 1 без WARN; `other` 1 → WARN; `with_messages` 1 → FAIL |
| `gaps.tsv` пуст | FAIL `gap 76532007..76568582 is not listed` |
| файл 001 обрезан на 5000 Б (один фрейм на час) | открытый час: «open frame … not audited», `no blocks found`, rc 1; закрытый: FAIL `torn zstd tail in closed hour … after 0 complete frames` |

Первый прогон «час закрыт» без `--current-hour` дал PASS: сейчас по UTC и есть час 14 (2026-09-30 14:xx), так что файл 14 законно считался открытым. Проверку закрытого часа повторил с явным `--current-hour 20260930-15`.

### recover на копиях (бинарник, `--url ws://127.0.0.1:9/`: порт закрыт, подключение сразу отклоняется)

| Случай | Результат |
|---|---|
| R1: копия `data/feed` (старый формат, целый) | `torn_repairs=0`, sha256 `.zst` не изменился, `last_seq` 76493010, `gaps.tsv` не создан, `_torn/` нет, выход 0 |
| R2: копия `data/feed`, файл обрезан на 5000 Б | весь файл (15 175 313 Б) ушёл в `_torn/…at0…torn`, файл 0 Б, `data=None`, продолжение по `last_seq.txt` = 76493010, старт не упал, выход 0 |
| R3: копия `data/feed-test-002` (целая) | `.zst` и `_torn` не изменились, `gaps.tsv` — та же одна строка, `gap_reconciled` нет |
| R4: копия `data/feed-test-002`, `gaps.tsv` опустошён, два старта | после 1-го: ровно `76532007\t76568582\t1790776652601532000`, **побайтно как оригинальный `gaps.tsv`**, одно событие `gap_reconciled`; после 2-го без изменений. На 2-м старте видно `startup_wait pending_pause 1.77 s`: остаток паузы 4.26 с из 1-го прогона, как и должно быть |
| R5: копия `data/feed-test-002`, файл 14 обрезан на 100 000 Б | хвост 2 094 044 Б в `_torn/`, `last_seq.txt` 76571859 → 76571245 по данным, `gaps.tsv` не изменился (новых дыр внутри данных нет; потерянный хвост закроет `accept` при следующем подключении) |

Сверка дыр на двух файлах 002 (~70 МБ в распакованном виде) заняла ~0.1–0.25 с.

## Цифры

| Показатель | Значение |
|---|---|
| Тесты | 69 в workspace; recorder 34 unit (+10) + 4 интеграционных (новые) |
| Close при SIGTERM | было: нет (Pong → EOF); стало: Close 1000, ответ мока ~0 мс, без ответа 2.002 с |
| Ожидание после рестарта (`connected` 30 с назад) | 89.592 с по `connections.tsv` (чистая функция: ровно 90 с) |
| Срок фрейма | [frame_max − 200 мс, frame_max]; при 60 с потери при kill -9 ≤ 60 с |
| feed-test-002 по сессиям | 568.1 с, 9.958 блока/с, 97.0 МБ/ч |
| feed (001) | 603.4 с, 9.969 блока/с, 90.6 МБ/ч |
| Подключения к фиду / вызовы RPC | 0 / 0 |

## Что не получилось и ограничения

- Первая версия теста «сервер не отвечает на Close» была ошибочной: мок после Close снова опрашивал сокет, и yawc на стороне сервера сам отправлял эхо-Close. Мок исправлен, теперь он просто держит соединение.
- Файлы старого формата (один фрейм на час) в открытом часе целиком попадают в `open_tail`, по ним feed-audit ничего не проверяет. Новый recorder так не пишет.
- `--current-hour` по умолчанию — текущий час UTC. Аудит свежей записи, у которой последний файл закрыт в этот же час (например, после SIGTERM), не отличит «закрыт» от «пишется». Недописанный фрейм тогда не FAIL, а `open_tail`. После SIGTERM недописанного фрейма нет, так что на практике это касается только аварии в текущем часе.
- В `connections.tsv` у строки `startup_wait` поле `strikes` = 0, если паузы из прошлого запуска нет. Так было и раньше.
- Под Linux и systemd не проверял: сервера нет.

## Предполагается, не проверено

- Что Close 1000 при остановке снижает риск бана. Причина бана 2026-09-30 так и не установлена; kill -9 и падение по-прежнему закрывают сокет без Close.
- Что настоящий сервер фида отвечает на Close своим Close. Проверено только на моке; к фиду не подключался по условиям задачи.
- Что 120 с между подключениями достаточно. Это консервативная оценка, а не измерение.

## Вопросы к Cowork и Михаилу (открытые, не решал)

1. **Сохранять ли дубликаты** (конверт, где все seq ≤ записанного)? Сейчас они по-прежнему пропускаются со счётчиком `dup_skipped`. Вопрос 2 из 002, остаётся за Михаилом.
2. **Спасать ли строки из `_torn/`?** Вопрос 4 из 002, остаётся за Михаилом. Сейчас хвост только сохраняется.
3. **Close при idle-таймауте.** Когда нет кадров 10 с (`--idle-timeout-secs`), recorder рвёт соединение **без Close** и переподключается через 5 с и больше (переходная лестница). Если гипотеза «обрыв без Close → бан» верна, это тот же риск. Предлагаю в следующей задаче тоже слать там Close; это +2 с к переподключению. В задаче этого не было, поэтому не делал.
4. **Минимальный интервал и переподключения внутри процесса.** `--min-connect-interval-secs` действует только при старте. После обычного закрытия сервером recorder переподключается через 1–5 с, как в 002. Нужен ли общий минимум и там? За ~30 мин живой записи (001 + 002) сервер ни разу не закрыл соединение сам, так что на данных это не проверить.
5. **Цена рестарта.** Каждый рестарт — дыра не меньше ~2 мин (~1200 блоков), которую закрывает дозаливка через RPC. Это учтено в `deploy/README.md`. Если дозаливка на сервере будет не сразу, это стоит иметь в виду для 7-суточного критерия.
6. `data/feed-test-002` пока нужна: на ней data-auditor проверяет 008. Удалять ли её потом — вопрос 5 из 002.

## Для data-auditor

- Копии в scratch: `/private/tmp/claude-501/-Users-mihailshumilov-sites-my-crypto-atomic-arbitrage/a477d6fc-6af5-4e28-af3e-63445fb94d7a/scratchpad/008/`:
  - `feed-orig/`, `feed-test-002-orig/` — нетронутые копии, только чтение;
  - `feed-recover*`, `feed-test-002-recover`, `feed-test-002-nogaps`, `feed-test-002-cut` — после прогонов R1–R5 (рядом `*.log`);
  - `synth-*` — синтетика feed-audit;
  - `audit-before-*.txt`, `audit-after-*.txt` — вывод feed-audit до и после;
  - `feed_audit_before.py` — скрипт до правок;
  - `orig-sha256.txt` — sha256 оригиналов;
  - `recover.sh`, `synth.sh` — сценарии.
- Для проверки по пунктам 1, 3, 5, 6, 8: код в `crates/recorder/src/{writer,net,main,route}.rs`, тесты в `crates/recorder/src/writer.rs` (`mod tests`) и `crates/recorder/tests/mock_feed.rs`, скрипт `.claude/skills/feed-audit/scripts/feed_audit.py`.
- Новых фактов о сети нет (к сети не подключался), `chain-facts.md` не менял.
