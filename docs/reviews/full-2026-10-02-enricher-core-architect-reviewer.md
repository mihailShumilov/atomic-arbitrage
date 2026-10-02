# Полное ревью, часть 2 из 4: enricher + hood-core — архитектурное ревью (architect-reviewer)

Дата: 2026-10-02. Объём: HEAD `d241243`, весь код `crates/enricher/src/*.rs` (1709 строк), `crates/enricher/tests/**` (557 строк), `crates/hood-core/src/lib.rs` (79 строк). Для поиска дублей прочитан контекст: `crates/recorder/src/{writer,backoff,main,route}.rs`, `crates/decoders/src/{lib,l1_inflows}.rs`, `deploy/enricher-gaps.service`, `deploy/README.md`, скилл `hoodchain-mev` (SKILL.md, data-model.md, chain-facts.md).

**Вердикт: FAIL.** Блокирующих замечания два, оба чинятся быстро: `cargo fmt --check` не проходит (131 расхождение в 14 файлах), а парсер `Retry-After` продублирован в recorder и enricher и уже разошёлся по смыслу. Остальное — «важное» и «рекомендации». Архитектура enricher в целом здоровая: атомарность, блокировка каталога, бюджет вызовов и коды выхода продуманы и покрыты тестами.

## Линт — вывод команд (проверено 2026-10-02 на локальном Mac, без сети и без RPC)

```
$ cargo fmt --all -- --check                     -> exit 1
  hunks per file (enricher + hood-core only):
    enricher/src: atomic 7, blocks 9, lib 6, logs 5, main 1, ranges 7, rpc 18, stats 2
    enricher/tests: common/mod 40, exit_codes 4, gaps 8, interrupt 7, logs 4, retry 11
    hood-core/src/lib.rs 2
  (+ decoders: 42 hunks, out of scope of this part; recorder is clean)
  No rustfmt.toml in the repo: the code was written for a wider max_width.
  Known debt: docs/handoff/from-code/008-recorder-feed-fixes.md:96-97.

$ cargo clippy -p enricher -p hood-core --all-targets -- -D warnings
    Finished `dev` profile ... -> clean, 0 warnings

$ cargo test -p enricher -p hood-core
  enricher unit 17 passed; tests/exit_codes 2, gaps 4, interrupt 2, logs 2, retry 4 passed;
  hood-core unit 2 passed. Total 33 passed, 0 failed.

$ cargo clippy ... -W clippy::pedantic -W clippy::nursery -A clippy::module_name_repetitions
  unique locations: enricher 102 (rpc 29, ranges 16, stats 14, atomic 11, blocks 8, logs 6,
  lib 5, main 1, tests 12), hood-core 8; (decoders 52, out of scope)
  top: missing `# Errors` 24, doc backticks 21, use_self 19, must_use 32, const fn 7,
  `# Panics` 7, u64->f64 precision 4, future_not_send 2 (rpc.rs:253, rpc.rs:304),
  cast truncation u128->u64 / u64->u32 3 (rpc.rs:195, 273, 299),
  significant_drop 2 (stats.rs:92, 108).
```

`cargo clean` не запускал: по просьбе координатора target общий с параллельными ревьюерами. Docker не использовался.

## Блокирующее

**B1. `cargo fmt --check` не проходит** — 131 расхождение в 14 файлах enricher и hood-core (список выше).
Почему важно: в скилле `fmt --check` стоит в блокирующем линте. Пока в дереве лежит неотформатированный код, каждая правка enricher тащит в дифф чужие строки, и ревью диффа превращается в шум. Долг тянется с задачи 008.
Исправление: отдельный коммит, в котором только `cargo fmt -p enricher -p hood-core`, без других изменений (decoders — в части 3 ревью). Поведение не меняется. Если хочется сохранить широкие строки, нужно один раз завести `rustfmt.toml` с `max_width = 120` на весь workspace. Но тогда переформатируется recorder, а он сейчас чистый, поэтому лучше без него.

**B2. Парсер `Retry-After` продублирован и разошёлся по смыслу** — `crates/enricher/src/rpc.rs:51-54` против `crates/recorder/src/backoff.rs:279-288`.
- enricher принимает целые и дробные секунды («0.5»), HTTP-date не понимает (дата → `None` → экспоненциальная пауза ≤ 60 с).
- recorder принимает целые секунды и HTTP-date (через chrono, `now_unix` передаётся параметром). Дробные секунды не понимает.

Почему важно здесь: у платного провайдера бан с `Retry-After: <HTTP-date>` на 10 минут enricher проигнорирует. Он будет бить в провайдера каждые ≤ 60 с, пока не кончатся `--max-attempts`, а это лишние оплачиваемые вызовы (бюджет `--max-calls` их тоже считает) и риск продлить бан. Это именно «дубль, уже разошедшийся по смыслу»: один заголовок HTTP, две грамматики.
Исправление: одна чистая функция в `hood-core` (chrono уже есть в `[workspace.dependencies]`, новой зависимости нет):
```rust
// crates/hood-core/src/http.rs
/// `Retry-After`: delta-seconds (integer or decimal) or an HTTP-date.
pub fn parse_retry_after(v: &str, now_unix: i64) -> Option<Duration> {
    let v = v.trim();
    if let Ok(x) = v.parse::<f64>() {
        return (x.is_finite() && x >= 0.0).then(|| Duration::from_secs_f64(x));
    }
    let when = chrono::DateTime::parse_from_rfc2822(v).ok()?.timestamp();
    Some(Duration::from_secs(when.saturating_sub(now_unix).max(0) as u64))
}
```
Тесты обоих крейтов (`rpc.rs:420-425`, `backoff.rs:620+`) перенести в hood-core и объединить. Верхнюю границу (`max_retry_after` 600 с в enricher, `RETRY_AFTER_MAX` 6 ч в recorder) оставить у вызывающих: это политика, а не разбор. Совместимость: CLI и форматы не меняются. Меняется поведение в крайних случаях (enricher начнёт соблюдать HTTP-date, recorder — дробные секунды), это стоит отметить в отчёте задачи.

## Важное

**I1. Классификация ошибок держится на строках, и пауза для всех задач может сработать от номера блока** — `rpc.rs:296`, `rpc.rs:333`, `rpc.rs:326/344/379`.
`if reason.contains("429") || reason.starts_with("rpc rate limit") { self.limiter.pause_all(delay) }`. Причину собирают из текстов вида `"block 77429001: eth_getBlockByNumber returned null…"` (`blocks.rs:71`) и `"block N: receipt i blockHash 0x…429… != …"` (`blocks.rs:44`). Подстрока «429» в 8-значном номере блока встречается в ~0,6 % случаев, в 64-символьном хэше — в ~1,5 %. Тогда обычный повтор останавливает все параллельные батчи. Счётчик `rpc_rate_limited` (`rpc.rs:333`) тоже держится только на совпадении префикса со строкой из `parse_batch` (`rpc.rs:379`). На сервере `--concurrency 1`, так что сейчас это почти не стоит пропускной способности. Но управление потоком на подстроках ломается при первой правке текста сообщения.
Исправление: вид ошибки хранить в типе, текст оставить только для лога.
```rust
enum FailKind { RateLimited, Transient }
enum Failure { Timeout, Retry { kind: FailKind, reason: String, retry_after: Option<Duration> } }
// parse_batch -> Result<Vec<Item>, (FailKind, String)>
// call(): if kind == FailKind::RateLimited { limiter.pause_all(delay).await }
```
Совместимость: не затрагивает.

**I2. Диапазоны и `gaps.tsv` описаны трижды** (подробнее — в разделе «Дубли»):
- `hood_core::Gap {from,to}` (`hood-core/src/lib.rs:44-48`) и `enricher::ranges::Range {from,to}` (`ranges.rs:15-19`) — один и тот же тип;
- вычитание диапазонов: `recorder/src/writer.rs:227-243 uncovered()` против `enricher/src/ranges.rs:101-117 subtract()`;
- разбор `gaps.tsv`: `recorder/src/writer.rs:213-225 read_gap_ranges()` (битые строки молча пропускает) против `enricher/src/ranges.rs:37-53 parse_ranges_tsv()` (битая строка — ошибка) + `split_unterminated()`.

Почему важно: формат `gaps.tsv` — контракт между двумя бинарниками, а источника истины у него нет. Строгость уже разная. Для recorder мягкость оправдана: в худшем случае он допишет строку повторно. Но это решение должно быть видно в коде, а не получаться случайно.
Исправление: `hood_core::ranges` — `Range` (с `pub type Gap = Range`, чтобы не трогать recorder сразу), `merge`, `subtract`, `chunk`, `detect_gap`, `parse_ranges_tsv(text, what) -> Result<Vec<Range>>`, `split_unterminated`, `GapRow { range, recv_ns }` с `Display`/`FromStr` для строки `from\tto\trecv_ns`. Recorder вызывает строгий разбор и сам решает, что делать со строками, которые не разобрались (WARN + пропуск). hood-core остаётся без IO: только `&str -> Result`. Риск средний: затрагивает восстановление recorder (`reconcile_gaps`), нужны тесты recorder и data-auditor. Форматы файлов не меняются.

**I3. Хелперы надёжной записи продублированы, и поведение уже разное**:
- fsync каталога: `enricher/src/atomic.rs:81-84 fsync_dir` возвращает ошибку; `recorder/src/writer.rs:44-48 sync_dir` — best effort, ошибку глотает;
- атомарная запись tmp → fsync → rename → fsync каталога: `recorder/src/writer.rs:51-64 write_atomic` и `enricher/src/atomic.rs:48-60 AtomicZstdFile::commit`;
- дописать строку и сделать fsync: `enricher/src/atomic.rs:131-141 append_line_synced` (`sync_all`) против `recorder/src/writer.rs:643-649` (`sync_data`).

Почему важно: политика надёжности должна быть одна. Сейчас тот же сбой fsync каталога останавливает enricher, а recorder продолжает работу. Сообщения тоже разные.
Исправление: `hood_core::fsutil` (только std, зависимостей не добавляет) — `fsync_dir(dir) -> io::Result<()>`, `write_atomic(path, bytes)`, `append_line_synced(path, line)`. Политику «падать или WARN» каждый бинарник выбирает в одном месте. `AtomicZstdFile` и `OutDirLock` остаются в enricher: у recorder нет такого сценария, и переносить их «на вырост» не нужно. Совместимость: не затрагивает.

**I4. API `Rpc::call` заставляет вызывающих разворачивать результат через `unwrap` и разбирать JSON дважды** — `rpc.rs:253`, `blocks.rs:74`, `blocks.rs:95`, `blocks.rs:98-99`, `logs.rs:108`.
Замыкание `check` возвращает только `Accept/Retry`. Поэтому `fetch_batch` повторно достаёт `result.as_ref().unwrap()` и ещё раз разбирает каждый блок и receipts в `Value` (первый разбор — в `check_items`, `blocks.rs:74-75`). `unwrap` безопасны лишь потому, что их гарантирует замыкание в другой функции: связь неявная. Параметр `return_timeout: bool` (`rpc.rs:253`) — булев флаг вместо enum.
Исправление:
```rust
pub enum OnTimeout { Retry, Return }
pub async fn call<T>(&self, calls: &[Call], what: &str, on_timeout: OnTimeout,
                     accept: impl Fn(&[Item]) -> Result<T, String>) -> Result<T, CallError>
// blocks: accept = |items| parse_and_validate(from, items) -> Result<Vec<BlockLine>, String>
```
Блоки разбираются один раз, `unwrap` исчезают. Совместимость: не затрагивает.

**I5. Решение о повторе не тестируется отдельно, тесты зависят от реальных часов** — `rpc.rs:262-301`, `rpc.rs:161-189`, `tests/retry.rs:30,70`.
Политику повтора (сдаться, сколько ждать, останавливать ли всех) можно проверить только через HTTP-мок и реальный сон: `retry.rs` идёт ~2,2 с, проверки вида `elapsed >= 1900ms`. `RateLimiter` использует `std::time::Instant`, поэтому `#[tokio::test(start_paused = true)]` не поможет. В recorder тот же класс логики сделан правильно: `backoff::Ladder` — чистая функция.
Попутно: резерв бюджета `fetch_add` → проверка → `fetch_sub` (`rpc.rs:266-271`) не атомарен как операция. Батч, который не влезает в бюджет, на мгновение раздувает счётчик, и параллельный батч, который влез бы, получает ложный «budget exhausted» (выход 75 раньше времени, недетерминированно).
Исправление:
```rust
enum Step { GiveUp, Wait { delay: Duration, pause_all: bool } }
fn next_step(attempt: u32, f: &Failure, p: &RetryPolicy, u: f64) -> Step  // pure, unit-tested
struct CallBudget { max: Option<u64>, sent: AtomicU64 }
impl CallBudget { fn try_reserve(&self, n: u64) -> Result<(), BudgetExhausted> {
    // self.sent.fetch_update(SeqCst, SeqCst, |s| (s + n <= max).then_some(s + n))
}}
// RateLimiter: tokio::time::Instant instead of std::time::Instant
```
Совместимость: не затрагивает.

**I6. Сеть не проверяется: `hood_core::CHAIN_ID` нигде не используется** — `hood-core/src/lib.rs:11`, `enricher/src/lib.rs:115-124`.
`validate_block` (`blocks.rs:31-51`) проверяет только внутреннюю согласованность блока и receipts. Если в `RPC_URL` по ошибке окажется эндпоинт другой сети (у провайдеров в URL меняется одно слово), enricher молча запишет «верные» блоки чужой сети в `data/blocks`, а `filled.tsv` отметит дыры закрытыми. Это тихая порча сырья, хотя и при ошибке конфигурации.
Исправление: в `run`/`run_gaps` перед первым скачиванием сделать один вызов `eth_chainId` и `ensure!(id == hood_core::CHAIN_ID)`. **Меняет поведение:** +1 вызов на прогон, он входит в `--max-calls`. `--dry-run` вызов не делает. CLI-флаги не меняются.

**I7. `--gaps` при `--max-calls` меньше одного куска не продвигается, а systemd считает это успехом** — `lib.rs:198`, `lib.rs:219-226`, `deploy/enricher-gaps.service` (`SuccessExitStatus=75`).
Кусок по умолчанию — 1000 блоков = 2000 вызовов. Если на сервере через drop-in выставить `--max-calls` < 2000 или `--chunk` > `max_calls/2`, каждый прогон истратит бюджет на незакоммиченный кусок, удалит `*.partial` и выйдет с 75, то есть «успехом». Уведомлений не будет, healthcheck заметит только через 24 ч (`backfill`). Сейчас значения в юните безопасны (4000 ≥ 2000), но README прямо предлагает менять их drop-in'ом.
Исправление: в `run_gaps` после построения плана:
```rust
if let Some(m) = a.max_calls {
    let need = chunks.iter().map(Range::blocks).max().unwrap_or(0) * 2;
    ensure!(need <= m, "--max-calls {m} < {need} calls for one file; lower --chunk to <= {}", m / 2);
}
```
**Меняет поведение** только для неверных конфигураций (выход 1 и уведомление вместо тихого 75). Флаги не меняются.

**I8. Ответы RPC пишутся не «как есть», а перекодированными через `Value`** — `blocks.rs:98-100`, `logs.rs:150-167`.
В `serde_json` нет `preserve_order` и `arbitrary_precision` (проверено `cargo tree -e features`: только `default`, `std`, `raw_value`). Значит, ключи объектов в `blocks-*.jsonl.zst` переупорядочены по алфавиту, а JSON-числа больше u64 или дробные прошли бы через f64. Это расходится с правилом 1 скилла («ответы RPC сохраняются как есть») и плюс к I4 добавляет лишний разбор. Сейчас поля RPC — hex-строки, так что числовых потерь, скорее всего, нет (см. «Предполагается»).
Исправление: писать строку из сырых байтов, они уже есть как `RawValue`: `{"number":N,"block":<raw>,"receipts":<raw>}` через `write!` и `rb.get()`. **Ломает побайтовую совместимость** файлов: семантика JSON та же, порядок ключей другой. Делать только отдельной задачей, с проверкой data-auditor и пометкой в data-model.md; старые файлы не переписывать.

**I9. Коды выхода разбросаны, и `--stats-json` может их замаскировать** — `rpc.rs:124-127` (75 живёт в сетевом модуле), `main.rs:54` (литерал 130), `main.rs:49`.
`report(...)?` выполняется до `match outcome`. Если запись `--stats-json` упадёт, бинарник выйдет с 1 вместо 75 или 130. Для systemd-юнита (`SuccessExitStatus=75`) это ложное уведомление. Сейчас юнит `--stats-json` не передаёт, поэтому замечание латентное.
Исправление: `crates/enricher/src/exit.rs` с `pub const OK: i32 = 0; FAILED = 1; BUDGET_EXHAUSTED = 75; INTERRUPTED = 130;` и ссылкой на data-model.md, строка 39. В `main.rs` ошибку `report` логировать WARN и не пробрасывать через `?`. `rpc::EXIT_BUDGET_EXHAUSTED` оставить реэкспортом, чтобы не ломать тест. Значения не меняются.

## Рекомендации

- **R1. lib.rs: дубль цикла и разбросанная валидация.** Цикл `write_range` + `mark_filled` повторяется в `lib.rs:158-161` и `lib.rs:221-225` → `async fn fill_blocks(rpc, dir, chunks, opts)`. Проверки аргументов раскиданы по `lib.rs:116-117` (в `make_rpc`), `138-141`, `126-133` → одна `Args::validate(&self) -> Result<()>` в начале `run`. Проверка `ensure!(g.exists())` в `lib.rs:185` лишняя: `read_gaps_file` и так возвращает ошибку с путём. `check_budget` (`lib.rs:126`) переименовать в `check_max_blocks`, чтобы не путать с бюджетом `--max-calls`. `report` (`lib.rs:236-268`) перенести в `stats.rs` как `Summary::log()`: тогда в lib.rs останутся только CLI и оркестрация.
- **R2. `append_line_synced` пишет строку двумя `write_all`** (`atomic.rs:137-138`). Если kill -9 придётся между ними, строка останется без `\n`, следующая запись приклеится к ней, и одна запись `filled.tsv` пропадёт (повторное скачивание, данные не портятся). Исправление: один `write_all(format!("{line}\n").as_bytes())`, а `filled.tsv` читать через тот же `split_unterminated` (`lib.rs:195`).
- **R3. Видимость.** `Rpc.policy` и `Rpc.stats` объявлены `pub` (`rpc.rs:221-222`), снаружи не используются → `pub(crate)`. `AtomicZstdFile::final_path()` (`atomic.rs:39`) не используется — удалить. `blocks::fetch_batch`, `make_rpc`, `Item`, `Call`, `Check` → `pub(crate)`. Тестам нужны только `run`, `Args`, `stats::Stats` и `rpc::is_budget_exhausted`.
- **R4. Hex-хелперы раскиданы.** `logs.rs:77 hex_u64(&Value)`, `decoders/src/l1_inflows.rs:148 hex_u64(&str)`, `tests/common/mod.rs:51 hex`, форматирование `0x{n:x}` в `blocks.rs:32,56` и `logs.rs:90` → `hood_core::hex::{quantity(u64) -> String, parse_quantity(&str) -> Option<u64>}`. `validate_topic` (`logs.rs:38-44`) можно заменить на `B256::from_str` (alloy есть в workspace deps, decoders его уже тянет).
- **R5. Два ГПСЧ для джиттера**: `enricher/src/rpc.rs:191-203` (xorshift на глобальном атомике, load/store не атомарны вместе) и `recorder/src/main.rs:102-110` (splitmix от часов) → `hood_core::rand01()`, одна реализация, только std.
- **R6. `shutdown_signal` продублирован** (`enricher/src/main.rs:12-32` и `recorder/src/main.rs:112-134`), версии слегка разошлись: recorder пишет WARN, если не удалось поставить обработчик SIGTERM, enricher молчит. В hood-core не выносить: потянет tokio в крейт без IO, 20 строк того не стоят. Достаточно добавить в enricher такой же WARN.
- **R7. Время внутри логики.** `SystemTime::now()` в `blocks.rs:147` (4-я колонка `filled.tsv`) и `logs.rs:120` (`created_unix`). Передавать `now_unix` параметром стоит только тогда, когда тестам понадобится точное значение. Сейчас они проверяют префикс, так что это не срочно.
- **R8. Блокирующий IO в async.** Сжатие zstd и fsync (`blocks.rs:128-139`, `atomic.rs:48-60`) идут прямо в задаче tokio. Пока скорость 2–4 rps, это неважно; к будущему `--follow` вынести запись в `spawn_blocking` или поток-писатель, как в recorder.
- **R9. `future_not_send`** (`rpc.rs:253`, `rpc.rs:304`): у `F: Fn(&[Item]) -> Check` нет `Sync`. Сейчас ничего не спавнится, но `tokio::spawn` по батчу не скомпилируется. Если появится I4, добавить `+ Sync` к ограничению.
- **R10. hood-core.** У `pub` констант, полей `Gap` и `FeedMessageHead` нет doc-комментариев. `CHAIN_ID` не используется (см. I6). `FEED_URL` продублирован в `recorder/examples/feed_probe.rs:41`. Длинные литералы в тесте (`lib.rs:68`) — pedantic.
- **R11. Тесты.** Каталоги `scratch()` (`tests/common/mod.rs:178`, `atomic.rs:148`) не удаляются после тестов. `interrupt.rs:33` держится на `sleep(1500ms)` и проверке «partial уже есть»: на медленной машине тест может стать хрупким. Лучше ждать появления файла в цикле с таймаутом. Проверки `elapsed >=` — нижние границы, флака от медленной машины не дают.
- **R12. Pedantic (рекомендательно).** Стоит взять `cast_possible_truncation` в `rpc.rs:273` (`u64 -> u32`: заменить на `u32::try_from(n).unwrap_or(u32::MAX)`) и `missing_errors_doc` для `pub fn` в `ranges.rs` и `atomic.rs`. `must_use` и `use_self` можно не трогать.

## Дубли

| Что | Где (пары файл:строка) | Расхождение | Куда вынести |
|---|---|---|---|
| Разбор `Retry-After` | enricher `rpc.rs:51-54` ↔ recorder `backoff.rs:279-288` | **разошлись** (дробные секунды vs HTTP-date) | `hood_core::http` (B2) |
| Тип диапазона | enricher `ranges.rs:15-32` ↔ hood-core `lib.rs:44-48` (`Gap`) | одинаковые поля; у `Range` есть `new()` с проверкой | `hood_core::ranges::Range` (I2) |
| Вычитание диапазонов | enricher `ranges.rs:101-117` ↔ recorder `writer.rs:227-243` | алгоритм тот же, типы разные (`Range` / `(u64,u64)`) | `hood_core::ranges::subtract` |
| Разбор `gaps.tsv` | enricher `ranges.rs:37-53, 66-85` ↔ recorder `writer.rs:213-225` | **разная строгость** (ошибка vs пропуск) | `hood_core::ranges::parse_ranges_tsv` + политика у вызывающего |
| Дыры между seq | hood-core `lib.rs:52-57 detect_gap` ↔ recorder `route.rs:77-92 intra_envelope_gaps` | одна формула `a+1..=b-1` | `intra_envelope_gaps` может звать `detect_gap` |
| fsync каталога | enricher `atomic.rs:81-84` ↔ recorder `writer.rs:44-48` | **разошлись** (ошибка vs глушение) | `hood_core::fsutil` (I3) |
| Атомарная замена файла | enricher `atomic.rs:48-60` ↔ recorder `writer.rs:51-64` | разные формы (поток zstd vs строка) | общий `fsutil::rename_durable(tmp, final)` |
| Дописать строку + fsync | enricher `atomic.rs:131-141` ↔ recorder `writer.rs:643-649` | `sync_all` vs `sync_data` | `fsutil::append_line_synced` |
| Джиттер [0,1) | enricher `rpc.rs:191-203` ↔ recorder `main.rs:102-110` | разные алгоритмы, назначение одно | `hood_core::rand01` (R5) |
| Экспонента с потолком | enricher `rpc.rs:44` ↔ recorder `backoff.rs:105-109` | потолок сдвига 20 vs 16 | не выносить: 2 строки, политики разные |
| Hex-quantity | enricher `logs.rs:77-79`, `tests/common/mod.rs:51-53` ↔ decoders `l1_inflows.rs:148` | одно и то же | `hood_core::hex` (R4) |
| Обработчик сигналов | enricher `main.rs:12-32` ↔ recorder `main.rs:112-134` | WARN только в recorder | оставить дубль, выровнять (R6) |
| Временный каталог в тестах | enricher `tests/common/mod.rs:178`, `atomic.rs:148` ↔ recorder `writer.rs:696`, `tests/mock_feed.rs:26` | одинаково | не выносить (тестовый код) |

## Структура и разбиение

hood-core сейчас — 79 строк: константы, `FeedEnvelope`, `Gap` и `detect_gap`. Используется частично (`CHAIN_ID` нигде), а настоящая общая логика лежит в бинарниках. Предлагаемая раскладка, только под реальные дубли выше:

```
crates/hood-core/src/
  lib.rs        consts (CHAIN_ID, FEED_URL, PUBLIC_RPC_URL) + pub mod/re-exports
  feed.rs       FeedEnvelope, FeedMessageHead (moved from lib.rs)
  ranges.rs     Range (+ type Gap = Range), detect_gap, merge, subtract, chunk,
                parse_ranges_tsv, split_unterminated, GapRow (Display/FromStr)
  http.rs       parse_retry_after(v, now_unix)            [chrono from workspace deps]
  fsutil.rs     fsync_dir, write_atomic, append_line_synced   [std only]
  hex.rs        quantity / parse_quantity
  (rand01 — in hex.rs or a small util.rs; no separate crate)
```
enricher после этого: в `ranges.rs` остаётся только `read_gaps_file` и `read_ranges_file` (IO-обёртки), `atomic.rs` — `AtomicZstdFile`, `OutDirLock`, `remove_partials`. `rpc.rs` (471 строка) делить на файлы пока не нужно. Если I4 и I5 его заметно раздуют, разумная граница такая: `rpc/limiter.rs` (`RateLimiter` + `CallBudget`), `rpc/retry.rs` (`RetryPolicy`, `next_step`, `classify`), `rpc/mod.rs` (клиент).

### Приоритетный список рефакторинга

| # | Что | Зачем | Риск | Совместимость |
|---|---|---|---|---|
| 1 | `cargo fmt -p enricher -p hood-core` отдельным коммитом (B1) | снять блокирующее, чистые диффы | нет | нет |
| 2 | Тип `FailKind` вместо строк в `rpc.rs` (I1) | убрать ложные паузы и связь через текст | низкий | нет |
| 3 | `hood_core::http::parse_retry_after` для обоих бинарников (B2) | один разбор, соблюдать HTTP-date у провайдера | низкий | поведение в крайних случаях `Retry-After` |
| 4 | Проверка «кусок ≤ бюджет» в `--gaps` (I7) | не допустить тихого «успеха» без прогресса | низкий | неверный конфиг: выход 1 вместо 75 |
| 5 | Проверка `eth_chainId` на старте (I6) | защититься от не той сети в `RPC_URL` | низкий | +1 вызов на прогон (в `--max-calls`) |
| 6 | Модуль кодов выхода, `report` без `?` (I9) | один контракт с systemd | низкий | значения кодов те же |
| 7 | `Rpc::call<T>` с `accept -> Result<T,String>`, `OnTimeout` (I4) | без `unwrap` и двойного разбора | средний | нет |
| 8 | Чистая `next_step`, `CallBudget::try_reserve`, tokio `Instant` (I5) | тестируемость, ложный выход 75 | средний | нет |
| 9 | `hood_core::{ranges, fsutil, hex}` (I2, I3, R4) | один источник истины для `gaps.tsv` и надёжности | средний (восстановление recorder) | форматы те же; нужны тесты recorder + data-auditor |
| 10 | R1–R3, R5, R6, R11 | чистота | низкий | нет |
| 11 | Сырые байты RPC в `blocks`/`logs` (I8) | правило 1, минус разбор | средний | **ломает побайтовую совместимость** файлов; только отдельной задачей |

enricher-gaps на сервере выключен, поэтому пункты 3–6 можно выкатывать без окна обслуживания. Флаги из `deploy/README.md` (`--gaps`, `--out-dir`, `--max-calls`, `--rps`, `--batch`, `--concurrency`, `--dry-run`) ни один пункт не переименовывает.

## Что хорошо (не сломать при правках)

- Атомарность: `*.partial` → fsync → rename → fsync каталога, `Drop` удаляет незакоммиченное, `OutDirLock` через `try_lock` снимается ОС даже при kill -9. Всё это проверено интеграционными тестами на реальном бинарнике (`tests/interrupt.rs`).
- `filled.tsv` дописывается только после `commit`, поэтому состояние не обгоняет данные. Повторный `--gaps` не делает ни одного вызова (`tests/gaps.rs:35-38`).
- Бюджет `--max-calls` как типизированная ошибка (`CallError::Budget`) плюс `downcast` в `main`, код 75 проверен через бинарник (`tests/exit_codes.rs`).
- Чистые функции с юнит-тестами: `backoff_delay`, `classify`, `parse_batch` (порядок, дубли, потерянные id), `Window`, `merge/subtract/chunk`, `split_unterminated`. HTTP-мок на 127.0.0.1 без лишних зависимостей.
- `main.rs` тонкий (65 строк), логика в `lib.rs` и доступна тестам. Ограничитель считает вызовы, а не HTTP-запросы, что согласуется с биллингом (наблюдение 2026-09-30 в комментарии).

## Предполагается / не проверено

- **Проверено (2026-10-02, локально):** fmt, clippy `-D warnings`, pedantic, тесты (команды и вывод выше); набор фич serde_json (`cargo tree -e features -i serde_json`); использование API hood-core (`grep hood_core` по crates); все пары файл:строка в таблице дублей — чтением кода.
- **Предполагается:** доля ложных пауз в I1 (~0,6 % для номеров блоков, ~1,5 % для 64-символьных хэшей) — оценка по комбинаторике, на логах не мерил. Что в ответах RPC Robinhood Chain нет JSON-чисел больше u64 (I8): поля — hex-строки, но полный набор полей Arbitrum-блока на данных не перебирал. Что провайдеры (решение 0002 не принято) присылают `Retry-After` в виде HTTP-date: возможно, но не наблюдалось.
- **Не проверялось:** поведение на сервере (enricher-gaps выключен, ssh не использовался); Python/bash-потребители `gaps.tsv`/`filled.tsv` в `deploy/` (healthcheck) — это часть 4 ревью; decoders — часть 3.
- `cargo clean` не запускался по просьбе координатора (общий target). Docker не запускался.
