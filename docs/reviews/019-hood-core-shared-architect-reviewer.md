# 019 — архитектурное ревью (architect-reviewer)

Дата: 2026-10-02. Объём: незакоммиченное рабочее дерево относительно HEAD `449576f`, только `crates/hood-core`, `crates/enricher`, `crates/recorder` и `Cargo.lock` (17 файлов, +261/−389 по `git diff --stat`, плюс новые `hood-core/src/{http,ranges,fsutil,hex}.rs`). Прочитаны задача `docs/handoff/to-code/019-hood-core-shared.md`, отчёт `docs/handoff/from-code/019-hood-core-shared.md`, исходные замечания `full-2026-10-02-recorder-architect-reviewer.md` (В3, В9) и `full-2026-10-02-enricher-core-architect-reviewer.md` (B2, I2, I3, R2, R4, I6/R10).

**Вердикт: PASS с замечаниями.** Блокирующих замечаний нет. Все дубли, перечисленные в обоих ревью для 019, удалены, остатков grep не нашёл. API hood-core маленький, ошибки типизированы, политика «падать или WARN» оставлена вызывающим. Изменения recorder ограничены переключением на hood-core. Форматы, флаги и раскладка `connections.tsv` не менялись. Есть два важных замечания: (1) публичный API разбора шире нужного, а мягкий разбор `gaps.tsv` в recorder написан копией цикла; (2) риск склейки строк после недописанной строки. Он существовал и до 019, но теперь его дёшево закрыть. Оба не блокируют.

## Линт: вывод команд (проверено 2026-10-02 локально на Mac, общий target; `cargo clean` не запускал по указанию координатора; сеть и docker не использовались)

| Команда | Итог |
|---|---|
| `cargo fmt -p hood-core -p enricher -p recorder -- --check` | exit 0 |
| `cargo fmt --all -- --check` | exit 0 (на момент проверки чисто и в decoders) |
| `cargo clippy -p hood-core -p enricher -p recorder --all-targets -- -D warnings` | exit 0, предупреждений нет |
| `cargo test -p hood-core -p enricher -p recorder` | exit 0: hood-core 19; enricher unit 13, exit_codes 2, gaps 4, interrupt 2, logs 2, retry 5; recorder unit 46, mock_feed 14. Всего 107 passed, 0 failed, совпадает с отчётом |
| `cargo clippy -p hood-core --all-targets -- -W clippy::pedantic -W clippy::nursery -A clippy::module_name_repetitions` (рекомендательно) | 30 предупреждений в lib: `missing_errors_doc` 9 (fsutil ×5, ranges ×4), `must_use_candidate` 11, бэктики в doc 5, длинные литералы 4 (тесты), `const fn` 2, `needless_pass_by_value` 1 (`fsutil.rs:14`), `format_collect` 1 (`ranges.rs:256`), `single_match_else` 1 (`ranges.rs:103`) |

Workspace-целиком (`clippy/test --workspace`) не запускал: decoders параллельно правит другой агент.

## Блокирующее

Нет.

## Важное

**В1. Публичный API разбора шире нужного, и мягкий разбор `gaps.tsv` в recorder сделан копией строгого цикла.** Места: `crates/hood-core/src/ranges.rs:143` `split_unterminated`, `:174` `parse_range_line`, `:190` `parse_ranges_tsv`; `crates/recorder/src/writer.rs:189-208`.
- `parse_ranges_tsv` снаружи hood-core не используется (grep по recorder/enricher/decoders ничего не нашёл). При этом это способ обойти единую политику: если вызвать его на содержимом файла, недописанная последняя строка станет ошибкой, а не WARN. `split_unterminated` и `parse_range_line` публичны только ради recorder.
- `read_gap_ranges` в recorder (`writer.rs:199-207`) повторяет цикл `parse_ranges_tsv` (`enumerate` → `parse_range_line` → пропуск `Ok(None)`). Отличается только веткой `Err`. Дубль маленький, но политика строгости распалась на два места, хотя задача хотела видеть её в одном.

Почему важно здесь: формат `gaps.tsv` — контракт двух бинарников. Задача 019 как раз собирала его в одно место.

Исправление (набросок, поведение не меняется):
```rust
// hood-core/src/ranges.rs
pub struct ParsedRanges<'a> {
    pub ranges: Vec<Range>,
    pub unterminated: Option<&'a str>,
    pub broken: Vec<LineError>,          // terminated lines that did not parse
}
/// Lenient read: collects broken lines instead of failing (recorder start-up).
pub fn parse_ranges_file_lenient(text: &str) -> ParsedRanges<'_> { ... }
/// Strict read (enricher): first broken line is an error.
pub fn parse_ranges_file(text: &str) -> Result<ParsedRanges<'_>, LineError> {
    let p = parse_ranges_file_lenient(text);
    match p.broken.into_iter().next() { Some(e) => Err(e), None => Ok(ParsedRanges { broken: vec![], ..p }) }
}
```
Recorder: `let p = parse_ranges_file_lenient(&text); for e in &p.broken { warn!(...) }`. После этого `parse_ranges_tsv`, `split_unterminated`, `parse_range_line` и `atomic_tmp_path` (`fsutil.rs:42`, используется только в тесте того же модуля) станут приватными. Если ради одной функции не хочется трогать `ParsedRanges`, минимальный вариант такой: сделать приватными `parse_ranges_tsv` и `atomic_tmp_path`, а дубль цикла в recorder оставить с комментарием «lenient twin of `parse_ranges_tsv`».

**В2. Дописывание после недописанной строки склеивает строки.** Риск был и до 019, но 019 сделала исправление дешёвым. Места: `crates/hood-core/src/fsutil.rs:63` `append_synced`; вызовы `crates/recorder/src/writer.rs:278` (reconcile на старте), `:548` (commit), `crates/enricher/src/blocks.rs:150` (`filled.tsv`).
Сценарий: после падения в `gaps.tsv` осталась строка без `\n`. При старте recorder её видит (`writer.rs:194`, WARN) и тут же дописывает строки reconcile в конец, то есть приклеивает к обрывку. Получается завершённая `\n` строка, которую строгий разбор принимает, но неверно. Пример: обрывок `77200000\t772` + `77300000\t77300005\t…\n` → диапазон `77200000..=77277300000`. Ещё пример: обрывок `…\t17` поглощает следующую строку целиком в «лишние» столбцы, и строка дыры теряется. В первом случае enricher `--gaps` пытается скачать диапазон дальше головы цепи и падает (громко), во втором дыра тихо пропадает из плана. Recorder — единственный писатель `gaps.tsv`, а enricher под блокировкой — единственный писатель `filled.tsv`. Значит, в момент дописывания обрывок — это всегда след прошлого падения, а не запись в процессе. Вероятность мала (одна `write_all` на десятки байт плюс fsync), но последствие — тихая потеря строки дыры.
Исправление: в `append_synced` перед записью проверить последний байт файла и, если он не `\n`, дописать `\n` в той же `write_all`:
```rust
let mut f = OpenOptions::new().create(true).read(true).append(true).open(path)?;
let len = f.metadata()?.len();
let mut buf = Vec::with_capacity(data.len() + 1);
if len > 0 {
    let mut last = [0u8; 1];
    std::os::unix::fs::FileExt::read_at(&f, &mut last, len - 1)?;
    if last[0] != b'\n' { buf.push(b'\n'); }
}
buf.extend_from_slice(data);
f.write_all(&buf)?;
```
Обрывок станет отдельной битой строкой с `\n`: recorder пропустит её с WARN, enricher `--gaps` откажется работать, пока строку не уберут вручную (громко, а не тихо). **Ломает побайтовую совместимость только в аварийном случае** (в файле появляется `\n` после обрывка). Нужны согласие Михаила, строка в `data-model.md` и проверка data-auditor. В объём 019 не входит: исполнитель сам вынес это в «Предполагается» и правильно не стал делать без согласования. Предлагаю отдельную мини-задачу или включить в 021.

## Рекомендации

- **Р1. Ошибки разбора строки — строки.** `ranges.rs:174` `parse_range_line -> Result<_, String>`, `LineError.reason: String` (`:160`). Сейчас по `reason` никто не ветвится (только `contains` в тестах), поэтому это не срочно. Если появится третий читатель (feed_audit через FFI не появится, а вот decoders — возможно), стоит завести `enum LineFault { MissingColumn(&'static str), NotANumber(&'static str), BadRange(RangeError) }` с `Display`. Формат текста не меняется.
- **Р2. `RangesRead = (Vec<Range>, Option<String>)`** (`crates/enricher/src/ranges.rs:17`): кортеж с безымянными полями поверх уже существующего `ParsedRanges`. Если сделать `ParsedRanges.unterminated: Option<String>` (аллокация на одну строку ничего не стоит), enricher вернёт `ParsedRanges` напрямую и псевдоним исчезнет. Делать вместе с В1.
- **Р3. `subtract(want: Vec<Range>, have: Vec<Range>)`** (`ranges.rs:93`) забирает векторы по значению. Из-за этого recorder клонирует `listed` на каждую дыру (`writer.rs:273`) и каждый раз заново сортирует `have`. Подпись `subtract(want: &[Range], have: &[Range])` с внутренним `merge(have.to_vec())` убирает клон у вызывающих. Сигнатура унаследована от enricher, на объёмах `gaps.tsv` это незаметно.
- **Р4. Два пути к одному элементу.** `lib.rs:21` реэкспортирует `detect_gap, Gap, Range`, и `crates/recorder/src/writer.rs:25-27` импортирует `hood_core::detect_gap` рядом с `hood_core::ranges::{…}`. Реэкспорт оставить ради `route.rs`, а внутри одного файла брать всё из `hood_core::ranges`. Косметика.
- **Р5. Doc `hex::parse_quantity`** (`hex.rs:14`): «1 to 16 hex digits» противоречит «leading zeros are tolerated». `0x` + 20 цифр с ведущими нулями принимается. Переписать на «`0x` and hex digits whose value fits in u64».
- **Р6. `read_gap_ranges` теперь падает на нечитаемом файле** (`writer.rs:189-193`), например на не-UTF-8 после повреждения диска. До 019 файл молча считался пустым. Это задокументировано в doc и согласуется с новой политикой fsync. Но у отступления есть обоснование «не уходить в crash-loop из-за собственного файла состояния», и этот случай в него не вписывается. Если хочется последовательности: `String::from_utf8_lossy(&fs::read(&path)?)`. Тогда битые байты дадут WARN по строке, а не выход 1. Решение за Михаилом, сейчас поведение безопасное (громкое).
- **Р7. Pedantic.** Стоит добавить `# Errors` в doc пяти функций `fsutil` и `Range::new`: это публичный API общего крейта. `must_use` и `const fn` можно не трогать.
- **Р8. `CHAIN_ID`** (`lib.rs:25`) по-прежнему не используется. Doc ссылается на 020, вопрос вынесен Михаилу. Приемлемо, если 020 будет взята следующей. Иначе удалить, чтобы не висела мёртвая константа.

## Дубли (проверено grep 2026-10-02)

| Что (из ревью) | Было | Сейчас |
|---|---|---|
| `parse_retry_after` (B2/В3, разошлись) | recorder `backoff.rs`, enricher `rpc.rs` | одна `hood_core::http::parse_retry_after` (`http.rs:23`); вызовы `recorder/src/net.rs:273`, `enricher/src/rpc.rs:349`; потолки у вызывающих (`backoff.rs:131,142` `RETRY_AFTER_MAX`, `rpc.rs:320` `max_retry_after`) |
| Тип диапазона (I2) | `hood_core::Gap`, `enricher::ranges::Range` | `hood_core::ranges::Range` + `type Gap` |
| `detect_gap` / формула `a+1..=b-1` | hood-core + `route.rs` | `route.rs:82` вызывает `detect_gap`; переполнение у `u64::MAX` закрыто (Р2 ревью recorder) |
| Вычитание диапазонов | recorder `uncovered`, enricher `subtract` | `ranges.rs:93`; прежние случаи `uncovered` перенесены в тест `ranges.rs:297-305` |
| Разбор `gaps.tsv` / `filled.tsv` | 2 реализации разной строгости | одна в hood-core; отличие recorder задокументировано (`writer.rs:177-186`); остаток — цикл `writer.rs:199-207` (см. В1) |
| Запись строк `gaps.tsv` / `filled.tsv` | `format!` в 4 местах | `GapRow` / `FilledRow` + `Display` — единственные писатели; побайтовый тест `ranges.rs:367` |
| fsync каталога, атомарная запись, append + fsync (I3, R2) | recorder `sync_dir` (глушил), `write_atomic`; enricher `fsync_dir`, `append_line_synced` (2 `write_all`) | `hood_core::fsutil`, одна политика (ошибка), одна `write_all` |
| hex-quantity (R4) | `logs.rs hex_u64`, `format!("0x{:x}")` ×4 | `hood_core::hex`; остатки `format!("0x{:064x}")` в `enricher/tests/common/mod.rs:56,60` — это хэши, не quantity, дублем не считаются |

Новых дублей не появилось. Хелпер `scratch()` в тестах `fsutil.rs:81` и `enricher/src/ranges.rs` — тестовый код, ревью enricher (R11, таблица дублей) рекомендовало его не выносить. Вне объёма 019 остались: дозапись `connections.tsv` без fsync в `recorder/src/net.rs:650` (журнал, а не состояние; трогать вместе с 021, В6/В7 ревью recorder) и `recorder/examples/feed_probe.rs:140` (Р7 ревью recorder).

## Recorder: объём изменений (проверено по диффу)

- Изменения: `backoff.rs` (функция удалена, тест перенесён, добавлен тест лестницы), `net.rs` (импорт и `SystemTime::now()` вместо `now_unix`), `route.rs` (вызов `detect_gap`), `main.rs` (`g.range.from` в тексте `detail`, строка `gap_reconciled` та же), `writer.rs` (переключение на `fsutil`/`ranges`). Логика writer/net не перестроена, это оставлено для 021.
- CLI-флаги не менялись (в `main.rs` в диффе одна строка). Заголовок и столбцы `connections.tsv` (`net.rs:585`, `:641`) не менялись; `retry_after` пишется сырой строкой, как раньше.
- Поведенческие изменения перечислены в отчёте исполнителя честно и полно: дробный `Retry-After`, ошибка fsync каталога вместо глушения, WARN на битых и недописанных строках `gaps.tsv`, ошибка на нечитаемом `gaps.tsv`. Последнее в отчёте не названо отдельно, см. Р6.
- Отступление (битая строка `gaps.tsv` с `\n` → WARN и пропуск) задокументировано в коде, в doc `read_gap_ranges` (`writer.rs:177-186`), с обоснованием. Его закрепляет тест `gap_ranges_reading_policy`.

## Тесты (проверено чтением и прогоном)

- HTTP-date: IMF-fixdate, RFC 850, asctime, дата в прошлом → ZERO, неверная дата → None, точный `now` без округления (`http.rs:105-125`). Дробные секунды, точный разбор без f64, насыщение, отказ на `-1`/`1e3`/`inf`/`.5`/`5.` (`http.rs:82-103`). Интеграция: `enricher/tests/retry.rs` `retry_after_http_date_is_honoured` (мок на 127.0.0.1, нижняя граница по времени, не флакает) и `recorder backoff.rs` `fractional_and_date_retry_after_reach_the_ladder`.
- Ranges: вычитание и склейка, включая края `u64::MAX` и неотсортированный вход; строгость (недописанная строка ↔ битая с `\n`, номер строки в ошибке); побайтовый формат и круговая проверка `GapRow`/`FilledRow`.
- fsutil: атомарная замена без остатка tmp, точные байты дописывания, ошибки с шагом, путём и `ErrorKind`. Путь «fsync каталога вернул ошибку» юнит-тестом не воспроизвести, это нормально.
- Не покрыто (не критично): дата до 1970 → ZERO (`http.rs:68`); `chunk` с `chunk == 0` (паника задокументирована, вызывающие проверяют флаг).

## Что хорошо (не сломать при правках)

- `parse_retry_after(value, now: SystemTime)`: время передаётся параметром, разбор точный, потолок у вызывающего. Пример того, как отделять разбор от политики.
- `GapRow` / `FilledRow` с `Display` — единственные писатели строк состояния, и побайтовый тест закрепляет формат.
- `fsutil` возвращает `io::Error` с сохранённым `ErrorKind` и текстом «шаг + путь». Модуль ничего не решает за вызывающего.
- `detect_gap` и `subtract` без переполнения на `u64::MAX`, с тестами на края.
- Отчёт исполнителя разделяет изменения поведения и рефакторинг и прикладывает побайтовую сверку со старым бинарником.

## Предполагается / не проверено

- Побайтовую сверку `gaps.tsv` / `filled.tsv` на копиях `data/feed-test-009` и `data/blocks` я не повторял: это часть data-auditor. Опираюсь на отчёт исполнителя.
- Что fsync каталога на сервере (Linux, ext4/xfs) не возвращает ложных ошибок. На Mac тесты проходят, на сервере не проверялось (деплоя не было).
- Что в серверном `gaps.tsv` нет обрывков и битых строк (команды проверки есть в отчёте исполнителя). Если обрывок есть, срабатывает сценарий В2.
- clippy и тесты на уровне workspace не запускались: decoders правится параллельно (задача 022).

## Повторная проверка (2026-10-02, после «Исправление F1»)

Объём: правки по моему важному В1 и находке F1 data-auditor (она же моя В2) — `crates/hood-core/src/{ranges,fsutil}.rs`, `crates/recorder/src/writer.rs` (`read_gap_ranges`), новые тесты в hood-core, recorder и `enricher/tests/gaps.rs`.

**Итоговый вердикт: PASS.** В1 и В2 закрыты. Новых блокирующих и важных замечаний нет. Рекомендации Р1–Р8 из первого прохода не обязательны и остаются в силе (кроме Р2 — см. ниже).

### Линт и тесты (проверено 2026-10-02 локально, workspace целиком, `cargo clean` не запускал)

| Команда | Итог |
|---|---|
| `cargo fmt --all -- --check` | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0, предупреждений нет |
| `cargo test --workspace` | exit 0, 161 passed, 0 failed (совпадает с отчётом исполнителя) |

### В1 (публичный API и копия цикла): закрыто

- Цикл разбора теперь один: `parse_ranges_file_lenient` (`ranges.rs:204`). Строгая `parse_ranges_file` (`ranges.rs:222`) построена на нём: первая битая строка возвращается как `Err` (`swap_remove(0)` возвращает именно первый элемент, порядок верный). В `read_gap_ranges` recorder (`writer.rs:187-205`) остались только IO и WARN, копии цикла нет.
- Публичная поверхность hood-core (grep `^pub` 2026-10-02) — ровно то, что используют recorder и enricher. `split_unterminated`, `parse_range_line`, `atomic_tmp_path` стали приватными, `parse_ranges_tsv` удалён. Grep по `crates/` вне hood-core внешних использований не нашёл.
- Тест `lenient_collects_broken_lines` проверяет номера битых строк, то, что строгий вариант отдаёт первую из них, и совпадение двух вариантов, когда битых строк нет.

### В2 / F1 (склейка после обрывка): закрыто

- `append_synced` (`fsutil.rs:70`): если файл не пуст и не кончается `\n`, `\n` уходит в той же `write_all`, до `sync_all`. Чтение последнего байта (`ends_with_newline`) через `seek` на файле в режиме append не влияет на позицию записи. При пустом `data` файл не трогается. Обоснование («писатель у каждого файла один, обрывок — след падения») записано в doc. В штатном пути байты прежние.
- Тесты: в hood-core проверены точные байты для `gaps.tsv` и `filled.tsv`, отсутствие лишнего `\n` у завершённого файла и пустой append. В recorder — повторный старт после собственного обрывка, без склейки и без повторной дозаписи. В enricher — оборванная строка `filled.tsv` (мок на 127.0.0.1): диапазон скачивается заново, строки по 4 столбца, повторный прогон без запросов.
- Новых дублей нет: проверка последнего байта живёт в одном месте, и оба бинарника идут через неё.

### Рекомендации после повторной проверки (не блокируют)

- **Р9. Битый обрывок `filled.tsv` теперь навсегда останавливает `enricher --gaps`.** Обрывок вида `3` или `3\t` после дозаписи становится завершённой битой строкой. Строгий разбор `filled.tsv` (`enricher/src/ranges.rs`) даёт выход 1 на каждом прогоне, пока строку не удалят руками. Это громко (systemd-уведомление, в тексте номер строки), данные не портятся, а вероятность обрыва внутри первых двух столбцов мала. Но потеря строки `filled.tsv` безвредна: диапазон просто скачается заново. Поэтому для `filled.tsv` разумно читать мягко, как recorder читает `gaps.tsv`: `parse_ranges_file_lenient` плюс WARN по `broken`. Для `gaps.tsv` в enricher строгий разбор оставить: там потеря строки — потерянная дыра. Решение за Михаилом. Если оставить как есть, стоит добавить в `deploy/README.md` строку «как починить `filled.tsv`» (удалить битую строку, диапазон скачается заново).
- **Р2 (уточнение).** `RangesRead = (Vec<Range>, Option<String>)` (`enricher/src/ranges.rs:17`) остался. Теперь у `ParsedRanges` есть поле `broken`, так что кортеж удобно оставить как узкий вид для enricher. Рекомендацию снимаю.
- Повторы одинаковых строк дыры в `gaps.tsv` после обрывка (отмечено исполнителем): enricher их сливает. Загрузчику `hood.feed_gaps` (задача 023/дальше) нужно убирать повторы. Согласен, что это вне 019.

### Предполагается / не проверено

- Сценарий F1 на копии `data/feed-test-009` я не воспроизводил: это проверка data-auditor, опираюсь на отчёт исполнителя.
- Поведение `seek`/`read` на файле в режиме append на сервере (Linux, ext4/xfs) не проверял. По POSIX `O_APPEND` пишет в конец независимо от позиции, тесты на macOS проходят.
