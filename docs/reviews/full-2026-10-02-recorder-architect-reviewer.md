# full-2026-10-02 (часть 1 из 4): recorder, архитектурное ревью (architect-reviewer)

- Дата: 2026-10-02.
- Объём: весь `crates/recorder` на HEAD `d241243`: `src/{main,net,writer,backoff,resume,route}.rs`, `tests/mock_feed.rs`, `tests/fixtures/009-session-b-head.tsv`, `examples/feed_probe.rs`. Для поиска дублей прочитаны `crates/hood-core/src/lib.rs`, `crates/enricher/src/{atomic,rpc,ranges}.rs`, `.claude/skills/feed-audit/scripts/feed_audit.py`, `deploy/healthcheck.sh` и `deploy/README.md`. В рабочем дереве `crates/recorder` изменений нет.
- Контекст: recorder **работает на сервере**. Любая правка кода вступает в силу только после деплоя и `systemctl restart recorder`, а рестарт — это ~2 мин простоя (пауза 120 с), из которых бэклог закрывает ~60–70 с, остальное станет дырой. Поэтому все рефакторинги ниже лучше собрать в **один** деплой. Ни одна рекомендация не меняет формат сырья, имена файлов, CLI-флаги или раскладку `connections.tsv`, если рядом нет пометки «меняет поведение» / «ломает совместимость».

**Вердикт: PASS с замечаниями.** Блокирующих нет. Два дубля формально подходят под критерий скилла «дубль, уже разошедшийся по смыслу» (В2, В3). Почему я не ставлю за них FAIL, написано в «Блокирующее». Если координатор трактует критерий буквально, вердикт меняется на FAIL с этими двумя пунктами.

## Линт: вывод команд (проверено 2026-10-02 локально, на HEAD d241243, общий target dir)

| Команда | Итог |
|---|---|
| `cargo fmt --all -- --check` | exit 1, но **в `crates/recorder` расхождений нет**. Все диффы в чужих крейтах: decoders (`examples/l1_inflows_scan.rs`, `src/l1_inflows.rs`, `src/lib.rs`, `tests/l1_inflows.rs`), enricher (`src/{atomic,blocks,lib,logs,main,ranges,rpc,stats}.rs`, `tests/{common/mod,exit_codes,gaps,interrupt,logs,retry}.rs`), `hood-core/src/lib.rs`. Это для ревьюеров тех крейтов: на уровне workspace это блокирующее |
| `cargo clippy -p recorder --all-targets -- -D warnings` | exit 0, предупреждений нет |
| `cargo test -p recorder` | exit 0: юнит 46/46 (1,34 с), `tests/mock_feed.rs` 14/14 (9,15 с) |
| `cargo clippy -p recorder --all-targets -- -W clippy::pedantic -W clippy::nursery -A clippy::module_name_repetitions` (рекомендательно) | 131 предупреждение по bin+tests (+28 mock_feed, +13 feed_probe). Топ: doc без бэктиков 37, `Self` вместо имени типа 22, длинные литералы 18, `const fn` 13, касты `u128→u64` 10 и `u128→i64` 6, `match ()` 8. Что стоит посмотреть: `too_many_lines` у `main.rs:137` (271 строка), `feed_probe.rs:281` (225), `net.rs:536` `run_connection` (118); касты `f64→u128` в `net.rs:839`, `f64→u64` в `backoff.rs:154`, `i64→u64` в `backoff.rs:286`, `u64→i64` в `resume.rs:93`; непроверенное вычитание `Duration` в тестах `writer.rs:1021,1023,1063` |

`cargo clean` не запускал, как просил координатор: target dir общий. Docker и сеть не использовались.

## Блокирующее

Нет.

Почему В2 и В3 не блокирующие, хотя это разошедшиеся дубли:
- в обоих случаях расхождение ведёт к **более осторожному** поведению: лишняя строка в `gaps.tsv`, лишние дубли seq в сырье (их и так снимает загрузчик, см. data-model.md) или более длинная пауза. Потери или скрытия данных нет;
- случай В2 ни разу не встречался на данных: все конверты до сих пор из одного сообщения (комментарий `writer.rs:286`, «Single-message envelopes (all of them so far)»; на сырье я это не пересчитывал);
- исправление требует рестарта живого процесса, а рестарт сам даёт дыру.

Исправить нужно в ближайший плановый деплой.

## Важное

**В1. Правило «last_seq / устаревший конверт / дыры» существует в трёх копиях.**
- `writer.rs:571-609`, `FeedWriter::accept`: пропуск `seq_max <= last`, `detect_gap`, фильтр внутренних дыр `g.to > last`, затем `last = max(last, seq_max)`.
- `writer.rs:300-320`, `scan_seq_holes`: та же машина состояний, повторённая для восстановления (в doc-комментарии прямо написано «Replays the writer's gap bookkeeping»).
- `net.rs:430-444`, `Sink::push`: снова `stale = seq_max <= last` и `last = max(...)` (комментарий «Same rule as FeedWriter::accept»).

Почему это важно: `recover` сверяет `gaps.tsv` с тем, что *записал бы* writer. Если поменять правило в одной копии, при старте появятся фальшивые или пропущенные дыры, а `stale_frames` в `backlog` разойдётся с `dup_skipped`. Тесты (`live_gap_is_written_once_by_accept_not_by_recover`) ловят только часть таких расхождений.

Исправление: чистый тип в новом `src/seqtrack.rs`, используется во всех трёх местах:
```rust
pub(crate) struct SeqTracker { last: Option<u64> }
pub(crate) enum Seen { Stale, Fresh { gaps: Vec<Gap> } } // gaps: seam + intra, already filtered
impl SeqTracker {
    pub fn observe(&mut self, seq_first: u64, seq_max: u64, intra: &[Gap]) -> Seen { ... }
    pub fn last(&self) -> Option<u64> { self.last }
}
```
`accept` превращает `Fresh.gaps` в строки `gaps.tsv` и обновляет счётчики. `scan_seq_holes` превращает их в `GapRow`. `Sink::push` берёт только `Stale` и `last()`. Юнит-тесты пишутся один раз, на `SeqTracker`. Риск низкий: поведение не меняется. Нужен рестарт (новый бинарник).

**В2. `max_seq_in_file` и `scan_seq_holes` разошлись по смыслу (дубль разбора строки сырья).**
- `writer.rs:175-202`: `max_seq_in_file` берёт максимум столбца `seq_last`.
- `writer.rs:262-323`: `scan_seq_holes` берёт `seq_max` из JSON (максимум по сообщениям). Writer (`accept`) тоже ведёт `last_seq` по `seq_max`.
- В `recover` (`writer.rs:393-410`) первая функция даёт `seam`, то есть последний seq файла перед двумя новейшими, и запасной `data_seq`. Вторая даёт всё остальное.

Если конверт в конце файла `n-3` внутри не по порядку (например, `[110, 108]`: `seq_last = 108`, `seq_max = 110`), seam окажется 108, и при старте будет дописана фальшивая дыра `109..110`. Если в двух новейших файлах нет seq, `resume_seq` станет 108, и 109–110 после досылки запишутся повторно. Сами циклы «открыть zstd → пустой файл → split по `\n` → splitn(4) → разобрать числа» тоже скопированы (`writer.rs:176-200` и `267-282`).

Исправление: модуль `src/rawline.rs` с одним разборщиком и одним итератором по файлу:
```rust
pub(crate) struct RawLine<'a> { pub recv_ns: u128, pub seq_first: u64, pub seq_last: u64, pub json: &'a [u8] }
pub(crate) fn parse_raw_line(line: &[u8]) -> Option<RawLine<'_>>;        // u64 for seq columns, not u128 + cast
pub(crate) fn seq_max(l: &RawLine) -> (u64, Vec<Gap>);                  // JSON only when first != last
pub(crate) fn for_each_raw_line(path: &Path, f: impl FnMut(RawLine<'_>)) -> Result<()>; // empty file = Ok
```
`max_seq_in_file` удалить, а seam считать через `for_each_raw_line` + `seq_max`, как в `scan_seq_holes`. Заодно уйдёт каст `first as u64` после разбора как `u128` (`writer.rs:276-282`), который молча обрезал бы испорченное число. Добавить юнит-тест «конверт не по порядку на стыке» (сейчас такого нет). Риск низкий. Меняет поведение только в этом ненаблюдавшемся случае, и в лучшую сторону. Нужен рестарт.

**В3. `parse_retry_after` в recorder и в enricher разошлись.**
- `backoff.rs:279-288` (recorder): целые секунды и HTTP-date. `"1.5"` даёт `None`, и вместо `Retry-After` включается лестница 429 (5 мин).
- `crates/enricher/src/rpc.rs:51`: целые и дробные секунды. HTTP-date даёт `None`, и включается экспоненциальная пауза; тест `rpc.rs:423` прямо фиксирует `None` для даты.

Это разбор одного и того же HTTP-заголовка, и у каждой копии своя дыра. Исправление: одна чистая функция в `hood-core`, без IO, туда это и относится по `rust.md`:
```rust
/// delta-seconds (integer or decimal) or an HTTP-date; `now_unix` turns a date into a delay.
pub fn parse_retry_after(v: &str, now_unix: i64) -> Option<Duration>;
```
Каждый клиент оставляет свой потолок (`RETRY_AFTER_MAX` = 6 ч в recorder, `max_retry_after` = 600 с в enricher). Тесты обеих копий переносятся в hood-core. Меняет поведение в редком случае: recorder начнёт уважать дробный `Retry-After`. Нужен рестарт recorder; enricher подхватит при следующем запуске таймера. Задача общая с ревьюером enricher.

**В4. Нет `lib.rs`: логика не импортируется, поэтому код копируется и тестируется только через бинарник.**
- `main.rs` — 436 строк, функция `main` — 271 (pedantic `too_many_lines`). В ней стартовое ожидание, цикл переподключений, сборка всех строк `connections.tsv` (`connected`, `backlog`, `client_close`, `disconnected`, `startup_wait`, `torn_repair`, `gap_reconciled`, `shutdown`, `writer_error`) и выбор кода выхода. Ни одну из этих веток нельзя проверить без запуска процесса и мок-фида. Отсюда 14 E2E-тестов по 0,5–12 с с временными окнами.
- `examples/feed_probe.rs:16-17` прямо пишет «Minimal copy of the connect / HeadTap / close code from src/net.rs (the recorder is a binary crate, its modules are not importable)». В примере ~120 строк копии (`HeadTap`, `tls_connector`, разбор заголовков ответа, Close-рукопожатие) и захардкоженный `FEED_URL` (`feed_probe.rs:41`) вместо `hood_core::FEED_URL`. Копия уже отстала: нет `poll_write_vectored`, нет проверки пустого хранилища корневых сертификатов.
- `tests/mock_feed.rs` дублирует `CONNECTIONS_HEADER` (`:783`, `CONN_HEADER`), обход часовых файлов (`:216-240`, аналог `list_feed_files`) и `now_ns` (`:19`).

Исправление: `src/lib.rs` с модулями (видимость `pub(crate)`, наружу только то, что нужно тестам и примеру) и тонкий `main.rs`: разбор CLI, `tracing`, вызов `recorder::run(args)`, код выхода. Цикл переподключений вынести в `src/app.rs`:
```rust
pub struct NetLoopCfg<'a> { url: &'a str, idle: Duration, block_idle: Duration, min_interval: Duration,
                            ignore_pending_pause: bool, requested_enabled: bool }
pub async fn startup_wait_phase(cfg, &ConnLog, session_end, now_ns, stop) -> ControlFlow<()>;
pub async fn net_loop(cfg, &ConnLog, &TlsConnector, &SyncSender<Line>, last_seq: Option<u64>, stop) ;
```
Код и имена бинарника, флаги и файлы не меняются. Риск средний: перенос большого блока; E2E-тесты остаются страховкой. Нужен рестарт. `feed_probe` потом либо собрать на lib, либо удалить (Р7).

**В5. `writer.rs` (1156 строк, ~690 без тестов) смешивает пять ответственностей.**
Там fs-утилиты (`sync_dir`, `write_atomic`), раскладка каталога (`list_feed_files`, `newest_data_mtime_ns`, а путь часового файла живёт отдельно, в `FeedWriter::path_for`, `:501`, и одна и та же схема `YYYY/MM/DD/feed-YYYYMMDD-HH.tsv.zst` закодирована в двух местах: `:146` depth==3/префикс/суффикс и `:504`), восстановление после аварии (`valid_prefix_len`, `repair_torn`, `recover`), учёт `gaps.tsv` (`GapRow`, `read_gap_ranges`, `uncovered`, `reconcile_gaps`) и сам writer (`Slot`, `FeedWriter`, `run`). Раскладка по смысловым границам — в разделе «Структура». Риск низкий: чистый перенос. Делать вместе с В1/В2, иначе придётся трогать файлы дважды.

**В6. `net.rs` (978 строк) смешивает транспорт, сессию и журнал `connections.tsv`; в `run_connection` четыре копии одной ветки.**
- `net.rs:593-661`: ветки `Stop`, `BlockIdle`, idle-таймаут и «writer gone» повторяют одно и то же: `close_gracefully` → `flush_backlog` → `info!` → `end(kind, detail)` → `client_close = Some(Box::new(outcome))`. Это 4×10 строк, и в ветке writer gone `info!` уже потерялся, то есть копии начали расходиться. Исправление:
  ```rust
  async fn end_by_client(ws: &mut FeedWs, sink: &mut Sink<'_>, on_backlog: &mut impl FnMut(&Backlog),
                         close_reason: &str, kind: EndKind, detail: String, started: Instant) -> ConnEnd
  ```
- `net.rs:535-546`: 9 аргументов и `#[allow(clippy::too_many_arguments)]`. Неизменяемые параметры (`url`, `tls`, `idle`, `block_idle`) собрать в `struct ConnParams<'a>` (тот же `NetLoopCfg` из В4). Колбэки оставить замыканиями, отдельный trait здесь не нужен.
- Разбиение файла — в «Структуре». Риск низкий. Нужен рестарт.

**В7. Контракт `connections.tsv` держится на строковых литералах и номерах столбцов, а читают его три программы.**
- Пишут: `main.rs:169,184,263,288,312,336,358,395,423` (имена событий литералами), `net.rs:714-751` (порядок столбцов в `format!`).
- Читают: сам recorder (`net.rs:794-842`: `c[2] == "disconnected"`, `c[6]` — пауза, `c[9]` — strikes, `== "connected"`), `deploy/healthcheck.sh:128,218,235` (`$3 == "connected"|"disconnected"|"startup_wait"|"shutdown"|"writer_error"`, `$4 == "writer_error"`, `$5`, `$7`) и `feed_audit.py:121` (`c[2] == "connected"`).

Защита от бана после рестарта (`last_pause`) и интервал от конца сессии (`last_session`) зависят от того, что формат записи и разбор совпадают. Опечатка в литерале не ловится компилятором. Исправление без изменения формата:
```rust
#[derive(Clone, Copy)] pub enum ConnEventKind { Connected, Backlog, Disconnected, StartupWait, TornRepair,
    GapReconciled, Shutdown, ClientClose, WriterError }
impl ConnEventKind { pub const fn as_str(self) -> &'static str { /* exact current strings */ } }
mod col { pub const TS_NS: usize = 1; pub const EVENT: usize = 2; pub const PAUSE_S: usize = 6; pub const STRIKES: usize = 9; }
```
`ConnEvent.event: ConnEventKind`. Плюс golden-тест, который фиксирует `CONNECTIONS_HEADER` и строки всех вариантов: их читают `healthcheck.sh` и `feed_audit.py`, менять их нельзя (переименование **сломало бы совместимость** с healthcheck и старыми журналами). Риск низкий. Нужен рестарт.

**В8. Ошибки без контекста на путях, которые уходят в `shutdown writer_error` и в алерт healthcheck.**
Места: `writer.rs:517` (`create_dir_all(dir)?`), `:539-540` (encoder), `:635-637` (`enc.finish()?`, `into_inner`, `sync_data()?`), `:645-649` (открыть/дописать/fsync `gaps.tsv`), `:122-124` (усечение рваного хвоста), `:176,:267` (`File::open(path)?` в `max_seq_in_file`/`scan_seq_holes`), `:343-347` (`reconcile_gaps`), `main.rs:144` (`create_dir_all(&args.out_dir)?`). Сейчас при полном диске в `detail` попадёт `No space left on device (os error 28)` без имени файла и без шага, на котором упало. При ошибке в `recover` процесс выходит с кодом 1 и сообщением без пути, systemd перезапускает его каждые 120 с. Исправление: `.with_context(|| format!("fsync {}", path.display()))` и подобное на каждом шаге. Для этого `HourFile` должен хранить `path: PathBuf` рядом с `key`. Риск нулевой. Меняется только текст `detail`, формат строки тот же. Нужен рестарт.

**В9. Формат и алгоритмы `gaps.tsv` размазаны по крейтам, и разбор в них разной строгости.**
- Запись: `writer.rs:583,590` (`pending_gaps: Vec<String>`, `format!("{}\t{}\t{}")`) и `writer.rs:345` (`writeln!` с тем же шаблоном). Дозапись в файл тоже дважды: `:643-650` (`commit`, без `sync_dir`) и `:341-349` (`reconcile_gaps`, с `sync_dir`).
- Чтение: `writer.rs:214-226` (`read_gap_ranges`) молча пропускает битые строки. `enricher/src/ranges.rs:80` (`read_gaps_file`) на битой строке падает, а недописанную последнюю строку пропускает. `feed_audit.py:67` своя третья версия.
- Вычитание интервалов: `writer.rs:229-256` (`uncovered`) — тот же алгоритм, что `enricher/src/ranges.rs` (`subtract` + `merge`).

Исправление: в `hood-core` (чисто, без IO) `GapRow` с `Display`/`FromStr` и модуль `ranges` (`Range`, `merge`, `subtract`). Recorder и enricher берут их оттуда. В recorder `pending_gaps: Vec<GapRow>` и одна `append_gap_rows(root, &[GapRow]) -> Result<()>` (open append → writeln → `sync_data` → `sync_dir`) для `commit` и `reconcile_gaps`. Разная строгость чтения задумана (recorder при старте не должен падать из-за чужой строки), поэтому её оставить, но явно: `parse_gap_line(&str) -> Result<GapRow>`, а политику (пропустить или упасть) выбирает вызывающий. `feed_audit.py` остаётся независимой проверкой (см. «Дубли»). Риск низкий. Нужен рестарт recorder, задача общая с ревьюером enricher/hood-core.

## Рекомендации

**Р1. Блокирующие вызовы в async.** `Sink::push` (`net.rs:446`) вызывает блокирующий `std::sync::mpsc::SyncSender::send` прямо в задаче main. Сетевой цикл и `shutdown_signal()` опрашиваются в одном `select!` (`main.rs:376`), поэтому, если канал на 200 000 строк заполнится (при ~9 строках/с это ~6 ч зависшего fsync), процесс перестанет реагировать на SIGTERM, и systemd через `TimeoutStopSec=30` убьёт его без Close. Это маловероятно, менять не обязательно. Достаточно комментария у `sync_channel` (`main.rs:191`), почему ёмкость такая и что будет при переполнении. `ConnLog::event` (`net.rs:714`) делает синхронную дозапись в файл из той же задачи; это несколько строк в час, приемлемо.

**Р2. Переполнение на внешних данных** (в release `overflow-checks` выключен: будет перенос, в debug и тестах — паника). Места: `route.rs:82-85` (`a + 1`, `b - 1` при seq = `u64::MAX`) — нужен `checked_add`. `net.rs:839` (`ts + (pause * 1e9) as u128`): строка `inf` в столбце паузы даёт `u128::MAX`, и сложение переносится в маленькое число, то есть ожидания не будет. Нужен `saturating_add`, а также `pause.is_finite() && pause >= 0.0`. `writer.rs:282` — см. В2.

**Р3. Типы.**
- Время представлено тремя способами: `u128` нс (`now_ns`, `not_before_ns`, `session_end_ns`, `recv_ns`), `i64` секунды (`net.rs:314` `now_unix`, `parse_retry_after`) и смесь нс с секундами (`resume.rs:92` `lag_ms(recv_ns, ts)`). Хватит одного `struct UnixNanos(u128)` с методами `as_unix_secs()` и `saturating_sub() -> Duration`. Newtype `Seq(u64)` в recorder почти ничего не даст (`header.blockNumber` здесь не читается, путать не с чем); его место в hood-core и decoders.
- `CloseOutcome.reason: &'static str` (`net.rs:208`) лучше сделать enum с `as_str()`, как `EndKind`.
- `EndKind::ServerClosed` используется для остановки клиентом (`net.rs:601`), а `EndKind::NetError` — для «writer gone» и «stopped before upgrade» (`net.rs:550,658`). Сейчас эти значения не попадают в журнал (после stop цикл выходит раньше), но вводят в заблуждение. Нужны варианты `ClientStop`/`WriterGone`. **Меняет поведение:** в редкой гонке, когда writer упал и строка `disconnected` успела записаться, `reason` станет другим. healthcheck смотрит только `connected`/`disconnected`/`startup_wait` и 4xx, так что его это не затрагивает.

**Р4.** `main.rs:253` `session_end.expect("interval wait needs an end")`: инвариант верный, но держится на связи между `startup_wait` и вызывающим. Лучше пусть вариант сам несёт данные: `StartupWaitReason::MinConnectInterval { end_ns, src }`. Тогда `expect` не нужен.

**Р5. Мёртвый код:** `net.rs:771-774` `last_connected_ns` под `#[allow(dead_code)]` и `net.rs:822-827` `parse_connected_row` нужны только тестам, и логика у них своя, не `row_ns_event`. Удалить, а тест `last_connected_from_both_log_layouts` (оба формата строк, 10 и 11 столбцов) переписать на `parse_last_session`.

**Р6. Поведение, вне фокуса архитектуры (меняет поведение, нужен рестарт):** `main.rs:229-232` восстанавливает `ladder.strikes` из последней строки `disconnected`, даже если после неё была долгая здоровая сессия, закончившаяся `shutdown` (на остановке строки `disconnected` нет, и сброс `RESET_AFTER` не срабатывает). Следующий 403 тогда начнётся с 30 мин вместо 15. Ошибка в осторожную сторону. Решать Михаилу; исправление — не брать strikes, если после этой строки есть `connected` с сессией ≥ 10 мин.

**Р7.** `examples/feed_probe.rs`: задача 005 закрыта, инструмент ходит в живой фид. Варианты: удалить (останется в истории git) или пересобрать на `lib.rs` (В4) с `hood_core::FEED_URL`. Пока он лежит, копия `HeadTap`/TLS будет расходиться с `net.rs`.

**Р8. Тесты.**
- Чувствительны ко времени: `writer.rs:1047-1069` (нижняя граница с запасом 50 мс), окна в `mock_feed.rs:368` (1,9–4 с), `:735` (0,9–3 с), `:1009` (1,9–3 с), `:1022` (3,5–7 с). На загруженной машине возможны редкие падения. Не переделывать, но при первом флейке расширить окна.
- Не хватает юнит-теста на конверт не по порядку на стыке файлов (В2) и на разбор `inf`/отрицательной паузы (Р2).
- Самописный SHA-1 в `mock_feed.rs:41` обоснован комментарием (новых зависимостей нет), можно оставить.

**Р9.** Pedantic: стоит исправить касты из Р2/Р3 и `too_many_lines` (их закроют В4 и В6). Остальное (бэктики в doc, `Self`, `const fn`, разделители литералов) — по желанию, одним механическим коммитом через `cargo clippy --fix`, не смешивая с рефакторингом.

**Р10.** `writer.rs:443-450`: промежуточное состояние `Slot::Empty` и два `unreachable!()` (`:537`, `:633`). Если `enc.finish()` упадёт, слот останется `Empty`. Ошибка фатальная, так что последствий нет, но проще держать `file: Option<File>` + `enc: Option<Encoder>` или `std::mem::take` по enum с `Default`. Делать только вместе с В5.

## Дубли

| Что | Где | Насколько разошлось | Куда вынести |
|---|---|---|---|
| Правило last_seq/stale/дыры | `writer.rs:571-609`, `:300-320`, `net.rs:430-444` | пока совпадает | `recorder/src/seqtrack.rs` (В1) |
| Разбор строки сырья + чтение zstd | `writer.rs:176-200`, `:267-299` | **разошлось** (`seq_last` против `seq_max`) | `recorder/src/rawline.rs` (В2) |
| `Retry-After` | `backoff.rs:279`, `enricher/src/rpc.rs:51` | **разошлось** (дата против дробных секунд) | `hood-core` (В3) |
| HeadTap/TLS/разбор заголовков ответа/Close | `net.rs:55-181,456-514`, `feed_probe.rs:199-276,466-501` | слегка (нет `poll_write_vectored` и проверки пустого хранилища сертификатов) | `lib.rs` или удалить пример (В4, Р7) |
| Формат строки и дозапись `gaps.tsv` | `writer.rs:345,583,590,643-650,341-349` | в `commit` нет `sync_dir` (безвредно: следом идёт `write_atomic` в тот же каталог) | `append_gap_rows` + `GapRow: Display` (В9) |
| Чтение `gaps.tsv` | `writer.rs:214`, `enricher/src/ranges.rs:80`, `feed_audit.py:67` | разная строгость, частично задумано | `hood-core::GapRow: FromStr`, политику выбирает вызывающий (В9) |
| Вычитание интервалов | `writer.rs:229` `uncovered`, `enricher/src/ranges.rs` `subtract`/`merge` | совпадает по смыслу | `hood-core::ranges` (В9) |
| Атомарная запись / fsync каталога | `writer.rs:44-65` (`sync_dir` best effort, `write_atomic`), `enricher/src/atomic.rs:47-84` (`AtomicZstdFile`, `fsync_dir` с ошибкой) | разная семантика: маленький файл состояния против потокового zstd; best effort против ошибки | **Не объединять** между крейтами: третьего пользователя нет, а hood-core по правилам без IO. Внутри recorder — в один `fsutil.rs` (В5) |
| `now_ns` / `SystemTime::now` | `net.rs:46`, `feed_probe.rs:75`, `mock_feed.rs:19`, enricher `blocks.rs:147`, `rpc.rs:195`, `logs.rs:120` | по одной строке | Не выносить: однострочник, разные единицы |
| Разбор сырья в Python | `feed_audit.py:126-237,379-430` (обход zstd-фреймов, 4 столбца, классификация seq-0, `recorderFrame`) | — | **Задуманная независимая проверка**, общий код не нужен. Но источник истины нигде не назван явно: добавить в docstring `feed_audit.py` и в doc `writer.rs` фразу «формат сырья определяет `references/data-model.md`; feed_audit намеренно не переиспользует код recorder и сверяет с этим описанием» |

## Структура и разбиение

Предлагаемая раскладка `crates/recorder/src` (имена бинарника, флаги, файлы данных не меняются; размеры примерные, без тестов):

```
lib.rs          pub(crate) mod ...; pub fn run(args) -> exit code       (~30)
main.rs         clap Args, tracing init, recorder::run                   (~110: Args + 20 строк)
app.rs          startup_wait_phase, net_loop, shutdown/exit-code logic   (~220)  <- из main.rs
seqtrack.rs     SeqTracker (В1)                                          (~60)
route.rs        без изменений                                            (~135)
rawline.rs      parse_raw_line, seq_max, for_each_raw_line (В2)          (~80)
layout.rs       STATE_FILE, GAPS_FILE, TORN_DIR, hour_path(t), list_feed_files,
                newest_data_mtime_ns                                     (~70)
fsutil.rs       sync_dir, write_atomic, append_synced                    (~50)
gaps.rs         GapRow (или реэкспорт из hood-core), read_gap_ranges, reconcile_gaps,
                append_gap_rows (В9)                                     (~90)
recovery.rs     valid_prefix_len, repair_torn, recover, Recovery, TornRepair (~170)
writer.rs       Slot, HourFile, FeedWriter, WriterStats, run              (~230)
transport.rs    tls_connector, HeadTap, HttpHead, parse_http_head, connect,
                classify_upgrade_error, handshake_request                (~270)
session.rs      Stop, Sink, run_connection, end_by_client (В6), close_gracefully,
                route_frame, ConnEnd, CloseOutcome                       (~300)
connlog.rs      CONNECTIONS_FILE/HEADER, ConnEventKind + col (В7), ConnEvent, ConnLog,
                PendingPause, LogSession, parse_last_session, parse_pause_row (~180)
backoff.rs      без изменений (чистая логика, хорошо покрыта); rand01 из main.rs можно
                перенести сюда как `jitter()`                            (~290)
resume.rs       без изменений                                            (~170)
```

Делить `backoff.rs` (636 строк) и `resume.rs` не нужно: это одна ответственность каждый, чистые функции, больше половины объёма — тесты.

### Приоритетный список рефакторингов

Всё ниже требует нового бинарника, то есть рестарта на сервере (~2 мин простоя). Рекомендация: один деплой на всё, после него `feed_audit` и проверка `connections.tsv`.

| № | Что | Зачем | Риск | Рестарт / совместимость |
|---|---|---|---|---|
| 1 | `rawline.rs` + seam через `seq_max`, удалить `max_seq_in_file` (В2) | убрать разошедшийся дубль в восстановлении | низкий, + новый юнит-тест | рестарт; формат не меняется; поведение меняется только в ненаблюдавшемся случае конверта не по порядку |
| 2 | `SeqTracker` для `accept` / `scan_seq_holes` / `Sink::push` (В1) | одно правило вместо трёх | низкий | рестарт; поведение то же |
| 3 | Контекст ошибок в writer/recover (В8) | диагностика `writer_error` и падений при старте | нулевой | рестарт; меняется только текст `detail` |
| 4 | `lib.rs` + тонкий `main.rs` + `app.rs` (В4) | тестируемость, конец копирования в пример и тесты | средний (крупный перенос), страховка — 14 E2E | рестарт; CLI и файлы те же |
| 5 | Разбить `writer.rs` и `net.rs` по раскладке выше, `end_by_client`, `ConnParams` (В5, В6) | читаемость, одна ответственность на модуль | низкий (перенос), делать после п. 4 | рестарт |
| 6 | `ConnEventKind` + константы столбцов + golden-тест (В7) | контракт с healthcheck и feed_audit проверяется компилятором и тестом | низкий | рестарт; строки и столбцы **не менять**: переименование сломает совместимость с healthcheck и старыми журналами |
| 7 | `hood-core`: `parse_retry_after`, `GapRow`, `ranges` (В3, В9) | убрать межкрейтовые дубли | низкий; согласовать с ревьюером enricher/hood-core | рестарт recorder; enricher — со следующим таймером; recorder начнёт уважать дробный `Retry-After` |
| 8 | Переполнения (Р2), мёртвый код (Р5), enum `CloseOutcome` (Р3), `expect` в `main.rs:253` (Р4) | мелкая гигиена | нулевой | рестарт; вместе с п. 5 |
| 9 | `feed_probe`: удалить или собрать на lib (Р7) | убрать копию сетевого кода | нулевой | рестарта не требует (пример не деплоится) |
| 10 | Strikes после долгой сессии (Р6) | — | — | **меняет поведение**, решение Михаила |

## Что хорошо (не сломать при правках)

- `backoff.rs` и `resume.rs` — чистая логика без IO. Время и случайность приходят параметрами (`now_ns`, `rand01`), поэтому лестница пауз, стартовое ожидание и статистика бэклога проверены юнит-тестами без часов. Тест `real_session_b_backlog` идёт на маленькой фикстуре с указанным источником (сессия B, 2026-10-01, `data/feed-test-009`).
- Порядок commit в writer (фрейм → fsync данных → `gaps.tsv` + fsync → атомарный `last_seq.txt`) и вывод точки продолжения из самих данных, а не из файла состояния. Рваный хвост не удаляется, а уходит в `_torn/`. Тест `valid_prefix_of_clean_and_torn_data` перебирает все точки обрыва.
- Writer — отдельный поток, владеющий файлами и zstd; сеть не блокируется на диске. Ожидание обрезается до дедлайна фрейма (`frame_time_left(now)` принимает время параметром).
- `route.rs` ничего не теряет: всё, что не JSON, оборачивается в `recorderFrame`/base64, а столбец JSON всегда валиден. Тесты прямо проверяют побайтовое совпадение.
- E2E-тесты на мок-фиде на 127.0.0.1 без сети, переменные окружения `FEED_URL`/`RPC_URL`/`RECORDER_OUT_DIR` убраны из окружения дочернего процесса. Покрыты Close 1000, интервал после рестарта, заголовок досылки, `block_idle`, выход с кодом 2 при ошибке writer.

## Предполагается / не проверено

- Не проверено на сырье, что на сервере нет конвертов из нескольких сообщений или не по порядку (вывод В2 «не наблюдалось» взят из комментария `writer.rs:286` и data-model.md). Проверка без рестарта: `feed_audit.py` или `zstd -dc ... | awk -F'\t' '$2!=$3'` по `/srv/hood/data/feed`. Сеть и ssh в этом ревью не использовались.
- Оценка «канал на 200 000 строк — это ~6 ч» (Р1) предполагает ~9 строк/с (блок ~113 мс плюс строки без seq), а не замер.
- Гонка в Р3 (строка `disconnected net_error` при падении writer) выведена из чтения кода; в тестах не воспроизводилась (`writer_error_sends_close_then_exits_2` проходит, `disconnected` нет).
- Как предложенные выносы в hood-core согласуются с замечаниями ревьюеров enricher/hood-core, которые работают параллельно, я не видел.
