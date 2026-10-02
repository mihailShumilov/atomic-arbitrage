# 021 — Структура recorder: архитектурное ревью (architect-reviewer)

- Дата: 2026-10-02.
- Объём: незакоммиченные изменения `crates/recorder` относительно HEAD (на начало ревью `7c0f95f`, к концу `87bf774`; коммиты 020/024 между ними recorder не трогают, `git log 9e1b5ab..HEAD -- crates/recorder` пуст). Файлы: `src/{lib,main,app,session,transport,connlog,layout,rawline,seqtrack,recovery,writer,backoff}.rs`, удалён `src/net.rs`, `examples/feed_probe.rs`, `tests/mock_feed.rs`. Для контекста прочитаны `route.rs`, `hood-core/src/{ranges,fsutil,http}.rs`, `deploy/healthcheck.sh`, `feed_audit.py` (чтение `connections.tsv`), задача, отчёт исполнителя и моё ревью `full-2026-10-02-recorder-architect-reviewer.md`.
- **Вердикт: PASS с замечаниями.** Блокирующих нет. Все пункты задачи выполнены. Замечания ниже не меняют поведение и могут уйти в тот же деплой или в следующий.

## Линт: вывод команд (проверено 2026-10-02 локально на Mac; сеть, ssh, docker не использовались)

| Команда | Итог |
|---|---|
| `cargo fmt --all -- --check` | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0, предупреждений нет |
| `cargo test -p recorder -p hood-core` (FEED_URL/RPC_URL/RECORDER_OUT_DIR сняты, TMPDIR = scratchpad) | exit 0: recorder юнит 65/65, `mock_feed` 15/15 (14 тестов 012 + новый), hood-core 21/21 |
| `cargo test --workspace` | exit 0, FAILED нет |
| pedantic+nursery (`-A module_name_repetitions`), `-p recorder --all-targets`, рекомендательно | HEAD (git worktree в scratchpad, свой target dir): 170 уникальных (bin+tests 129, mock_feed 28, feed_probe 13). Рабочее дерево: 254 (lib+tests 216, bin 1, mock_feed 27, feed_probe 10) |

Почему pedantic вырос: появился `lib.rs`, и у `pub`-элементов добавились `must_use` (8 → 34), `missing # Errors` (7 → 10); `Self` вместо имени типа (22 → 52, в основном `ConnEventKind::X` в `match` и `ALL`); длинные литералы и бэктики в golden-тесте (18 → 28, 38 → 48). Смысловые предупреждения ушли: `too_many_lines` у `main` (263 строки) и `run_connection` закрыты; остались только у golden-теста `connlog.rs:412` (122 строки, это таблица) и `feed_probe.rs:172` (`main` пробы, 117). Касты `u128→u64` 10 → 6. Чинить механический шум стоит отдельным коммитом через `cargo clippy --fix`, не смешивая с 021.

`cargo clean` не запускал (по просьбе координатора, target общий). Worktree HEAD и его target в scratchpad удалены.

## Проверка контракта `connections.tsv` на реальном выводе старого бинарника (2026-10-02)

Golden-тест сверяет строки с образцами, но формат нужно было сверить и с живым выводом HEAD. Собрал два debug-бинарника: HEAD (worktree) и рабочее дерево. Запустил оба на одинаково подготовленном `--out-dir`: три часовых файла с дырами на стыке 10→11 (93..94), на стыке 11→12 (97..99) и внутри 12 (102); рваный хвост у файла 12; `last_seq.txt` = 90. Адрес `--url ws://127.0.0.1:9` (порт закрыт, сети нет). Два запуска, каждый останавливался по SIGTERM. Потом сравнил `connections.tsv`, замаскировав время, `pause_s` (джиттер) и путь каталога.

- Сценарий без битой строки `gaps.tsv`: **diff пуст.** Строки `gap_reconciled` ×3, `torn_repair`, `disconnected net_error … rule=transient tcp: Connection refused`, `shutdown SIGTERM`, `startup_wait pending_pause`, `shutdown SIGTERM` совпадают побайтово, у всех 11 столбцов. `gaps.tsv` и `last_seq.txt` (103) тоже совпадают, коды выхода 0/0.
- Сценарий с битой строкой `oops` в `gaps.tsv`: отличие только в двух новых строках `gaps_line_skipped broken … gaps.tsv line 2: column from is not a number: "oops"` (по одной на каждый старт), как описано в отчёте. Остальные строки совпадают, `gaps.tsv` у обоих бинарников одинаковый.
- События `connected`/`backlog`/`client_close` в этом сценарии не возникают (нужен WS-сервер). Их покрывают golden-тест на строках из `data/feed-test-009` и E2E-тесты `mock_feed`.

## Статус приоритетного списка из `full-2026-10-02-recorder-architect-reviewer.md`

| № | Пункт | Статус | Где |
|---|---|---|---|
| 1 | `rawline.rs`, seam по `seq_max`, удалить `max_seq_in_file` (В2) | **сделано** | `rawline.rs` (`parse_raw_line`, `RawLine::seqs`, `for_each_raw_line`, `file_seq_max`); `recovery.rs:243`; тесты `recovery::out_of_order_envelope_at_the_seam`, `rawline::*` |
| 2 | `SeqTracker` для writer / scan / Sink (В1) | **сделано** | `seqtrack.rs`; `writer.rs:167` (`check`, затем `advance` после `write_line`), `recovery.rs:172` и `session.rs:114` (`observe`) |
| 3 | Контекст ошибок (В8) | **сделано** | `writer.rs`, `recovery.rs`, `rawline.rs`, `app.rs:195`; fs-шаги в `hood_core::fsutil::with_path` (019) |
| 4 | `lib.rs` + тонкий `main.rs` + `app.rs` (В4) | **сделано** | `main.rs` 83 строки (Args побайтово как в HEAD + `From<Args> for Config`), `app.rs` перенесён без изменения логики (сверено построчно с HEAD `main.rs:135-399`) |
| 5 | Разбить `writer.rs`/`net.rs`, `end_by_client`, `ConnParams` (В5, В6) | **сделано** | `transport`/`session`/`connlog`, `layout`/`recovery`/`writer`; одна ветка закрытия через `ClientEnd` (`session.rs:293-303`), `ConnParams`; `#[allow(too_many_arguments)]` ушёл. `fsutil` и `gaps` в hood-core уже с 019 |
| 6 | `ConnEventKind` + `col` + golden-тест (В7) | **сделано** | `connlog.rs:37-99`, `:412`; формат подтверждён живым выводом HEAD (см. выше) |
| 7 | hood-core: `parse_retry_after`, `GapRow`, `ranges` (В3, В9) | **сделано в 019** | `transport.rs` → `hood_core::http`, `recovery.rs`/`writer.rs` → `hood_core::ranges`, `fsutil` |
| 8 | Р2 переполнения, Р5 мёртвый код, Р3 `CloseOutcome` enum, Р4 `expect` | **сделано** | `connlog.rs:383` (`parse_pause_row`: `is_finite`, `saturating_add`), `parse_connected_row`/`last_connected_ns` удалены, `CloseReply`, `ConnEvent::startup_wait` без `expect` |
| 9 | `feed_probe` на lib (Р7) | **сделано** | `examples/feed_probe.rs` на `recorder::transport`, `--url` по умолчанию `hood_core::FEED_URL`; копии `HeadTap`/TLS нет |
| 10 | Р6 страйки после долгой сессии | **не делалось** (решение Михаила) | `app.rs:118` с комментарием |
| — | Р1 комментарий к ёмкости канала | сделано | `app.rs:37` (`CHANNEL_CAP`) |
| — | Р3 newtype времени, `EndKind::ClientStop/WriterGone` | не делалось (второе меняет `reason` в редкой гонке) | — |
| — | Р10 `Slot::Empty` | не делалось | `writer.rs:41-49,130,222` |
| — | Р8 тесты, зависящие от времени | без изменений (тесты 012 не трогали, это правильно) | — |

## Блокирующее

Нет.

## Важное

**В1. Публичная поверхность шире нужного, и отчёт описывает её неточно.** `lib.rs:51-62`: `pub mod session` и `pub mod layout`. Снаружи (бинарник, пример, тесты) используются только `app::{run, Config, Outcome, EXIT_WRITER_ERROR}`, `now_ns`, `transport::{connect, close_handshake, close_summary, opcode_name, tls_connector}`, `connlog::CONNECTIONS_HEADER` и `layout::list_feed_files` (`grep recorder::`). `session` снаружи не используется вовсе, хотя в отчёте написано «их используют бинарник, пример и тесты». Почему это важно: каждый `pub` в этом крейте маскирует мёртвый код от `dead_code` и выглядит как API, которое нельзя менять. Исправление (проверено: копия рабочего дерева в scratchpad, `cargo clippy -p recorder --all-targets -D warnings` → exit 0):
```rust
// lib.rs
mod layout;
mod session;
pub use layout::list_feed_files;
// tests/mock_feed.rs: recorder::list_feed_files(out)
```
`connlog` лучше оставить `pub`. Если сделать его приватным, `dead_code` срабатывает на `col::{TS_UTC, REASON, HTTP_STATUS, RETRY_AFTER, SESSION_S, ENVELOPES, DETAIL}` и `ConnEventKind::ALL`: их читает только golden-тест. Это документация контракта для healthcheck, и `pub` здесь оправдан, но об этом стоит одна строка в doc модуля: «pub: the column constants document the contract for external readers». Поведение не меняется.

## Рекомендации

**Р1. `feed_probe` читает журнал recorder по литералам.** `examples/feed_probe.rs:105-113`: `f.get(2) == Some(&"connected") || … "disconnected"`, `f.get(1)`. Это ровно то, от чего п. 4 задачи уходил в самом recorder, а `connlog` уже `pub`:
```rust
use recorder::connlog::{col, ConnEventKind as K};
let ev = f.get(col::EVENT).copied();
(ev == Some(K::Connected.as_str()) || ev == Some(K::Disconnected.as_str()))
    .then(|| f.get(col::TS_UNIX_NS)?.parse::<u128>().ok()).flatten()
```
Пример не деплоится, рестарта не требует.

**Р2. `ConnEventKind::ALL` поддерживается вручную** (`connlog.rs:71`). Golden-тест проверяет, что всё из `ALL` покрыто строками, но не то, что в `ALL` есть все варианты. Новое событие без строки в `ALL` пройдёт тест незамеченным. Исправление в тесте: исчерпывающий `match` без `_`, который компилятор заставит дополнить:
```rust
fn listed(k: ConnEventKind) -> bool { use ConnEventKind::*;
    match k { Connected | Backlog | ClientClose | Disconnected | StartupWait | TornRepair
            | GapReconciled | GapsLineSkipped | Shutdown | WriterError => ConnEventKind::ALL.contains(&k) } }
```

**Р3. Остаток дубля «seq_max + дыры по сообщениям конверта».** `route.rs:105-108` (`route_text`) и `rawline.rs:72-77` (`RawLine::seqs`) одинаково собирают `seqs: Vec<u64>` → `intra_envelope_gaps` → `max`. Пока это 3 строки, и копии совпадают. Но это та же граница смысла, на которой разошлись `max_seq_in_file`/`scan_seq_holes`. Предлагаю одну функцию в `route.rs`, её вызывают оба места:
```rust
pub(crate) struct EnvSeqs { pub seq_max: u64, pub gaps: Vec<Gap>, pub disorder: u32 }
pub(crate) fn envelope_seqs(env: &FeedEnvelope) -> Option<EnvSeqs>
```
Поведение не меняется.

**Р4. Строка `gaps.tsv` для первой строки нового часа публикуется раньше самой строки** (было до 021, не регрессия). `writer.rs:165-180`: `accept` кладёт дыру шва в `pending_gaps`, затем `write_line` → `ensure_hour` → `commit()` и дописывает её в `gaps.tsv` до записи строки. Doc модуля (`writer.rs:9-12`) обещает, что `gaps.tsv` не опережает данные на диске. При `kill -9` в этом окне (миллисекунды раз в час) досылка закроет дыру, а строка в `gaps.tsv` останется фальшивой: enricher лишний раз сходит в RPC. Последствие безвредное, ошибка в осторожную сторону. Исправление в 2 строки: в начале `accept` вызвать `self.ensure_hour(DateTime::from_timestamp_nanos(l.recv_ns as i64))?`, а повторный вызов в `write_line` станет пустым. Формально **меняет поведение** (порядок записи `gaps.tsv` при смене часа), поэтому только с отдельным тестом в духе `hour_rotation_commits_only_written_lines`, или оставить как есть с поправкой doc.

**Р5. Мелочи.**
- `ConnEvent` (`connlog.rs:111-121`): поля `pub`, поэтому снаружи можно собрать строку в обход конструкторов, то есть в обход контракта. Внешних пользователей нет; достаточно `pub(crate)` у полей (тесты внутри крейта).
- `writer.rs:130,222`: два `unreachable!()` вокруг `Slot::Empty` остались (Р10 прошлого ревью). Допустимо; если трогать writer снова — `Option<File>` / `Option<Encoder>` вместо третьего состояния.
- `feed_probe` пишет свой журнал тоже под именем `connections.tsv` (`feed_probe.rs:174`), но с другой схемой (`LOG_HEADER`, 9 столбцов). Каталоги разные (`--out-dir` пробы в scratchpad), это было и до 021. Если пробу когда-нибудь запустят на каталоге recorder, healthcheck прочитает чужую схему. Переименовать в `probe-log.tsv` (**меняет имя файла пробы**, recorder и healthcheck не затрагивает).
- Отчёт исполнителя называет базой `9e1b5ab`; на момент ревью HEAD `87bf774`. Для recorder разницы нет (проверено `git log`), но при коммите 021 стоит указать фактическую базу.

## Дубли

| Что | Где | Разошлось? | Решение |
|---|---|---|---|
| seq_max/дыры из сообщений конверта | `route.rs:105-108`, `rawline.rs:72-77` | нет | Р3, `envelope_seqs` |
| Формат `ts_utc` (`%Y-%m-%dT%H:%M:%S%.3fZ`) | `connlog.rs:267`, `feed_probe.rs:69` | нет | оставить: у пробы свой журнал |
| Новых дублей с hood-core нет: seam/дыры через `hood_core::detect_gap`, `GapRow`/`subtract`/`to_lines`, `fsutil`, `parse_retry_after` берутся из hood-core | — | — | — |
| Разбор сырья в `feed_audit.py` | — | — | задуманная независимая проверка; источник истины теперь назван в doc `lib.rs:10-12` |

## Структура и разбиение

Раскладка совпадает с предложенной в прошлом ревью. Отличия обоснованы: `fsutil` и `gaps` уехали в hood-core в 019, `read_gap_ranges` живёт в `recovery.rs` (единственный пользователь). Зависимости идут в одну сторону: `app` собирает всё; `session` → `transport`, `seqtrack`, `route`; `connlog` знает доменные типы (`ConnEnd`, `CloseOutcome`, `Backlog`, `TornRepair`), чтобы держать конструктор каждого события рядом с форматом. Сеть и диск в `connlog` не протекают: `format_row` чистая, IO только в `ConnLog::event`/`tail`. Размеры без тестов: `connlog` ~395, `recovery` ~290, `writer` ~270, остальные меньше. Дальше делить не нужно.

## Что хорошо (не сломать при правках)

- Разделение `SeqTracker::check`/`advance` с явным doc у `observe`, почему writer его не использует, плюс тест `hour_rotation_commits_only_written_lines`. Исполнитель сам поймал регрессию «last_seq впереди данных» тестами 012 и закрепил её отдельным тестом.
- Одна ветка закрытия через `ClientEnd::parts` и тест, который фиксирует строки `reason`/`detail` всех четырёх причин. Строка `client close handshake done` теперь пишется и для writer gone.
- `connlog`: `format_row(ns, &event)` — чистая функция с временем параметром. Golden-тест на строках старого бинарника и проверка позиций, которые читает healthcheck. Живой вывод HEAD совпал побайтово (см. выше).
- `main.rs` — только CLI и код выхода; `Args` совпадает с HEAD, `From<Args> for Config` отделяет clap от логики.
- `rawline::parse_raw_line` разбирает seq сразу как `u64`; пустой файл и zstd-ошибки дают контекст с путём; mock-тесты берут заголовок, `now_ns` и обход файлов из lib, своих копий больше нет. Тесты recorder за собой убирают (в TMPDIR после прогона нет каталогов `recorder-*`).

## Предполагается / не проверено

- Поведение на сервере после деплоя не проверял: это задача планового рестарта, команды в отчёте исполнителя.
- Живой вывод `connected`/`backlog`/`client_close` старым и новым бинарником напрямую не сравнивал (нужен WS-мок вне тестов). Опираюсь на golden-тест (строки взяты из `data/feed-test-009`, файл сам не открывал) и на 15 зелёных E2E.
- Что фид не шлёт конвертов из нескольких сообщений, установлено только на 4 часах сырья (замер исполнителя, сам я его не повторял: сеть и ssh в ревью не использовались).
- Документация нового события `gaps_line_skipped` в `deploy/README.md` и `data-model.md` (вопрос 1 отчёта) — вне архитектурного ревью; закрыть её нужно до деплоя.
- Попутно: интеграционные тесты enricher оставляют временные каталоги `enricher-*` в TMPDIR (видно после `cargo test --workspace`). К 021 это не относится, сообщаю ревьюеру enricher.
