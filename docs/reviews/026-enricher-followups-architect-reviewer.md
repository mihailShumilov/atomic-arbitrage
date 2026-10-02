# 026 — архитектурное ревью (architect-reviewer)

- Дата: 2026-10-03.
- Объём: незакоммиченные изменения рабочего дерева против HEAD `83cadb4`: `crates/enricher/src/{atomic,lib,ranges,rpc}.rs`, новый `crates/enricher/src/testdir.rs`, `crates/enricher/tests/{common/mod.rs,exit_codes.rs}`, новый `crates/enricher/tests/range.rs`; документация: `crates/hood-core/src/jitter.rs` (одна строка doc), `.claude/skills/hoodchain-mev/references/data-model.md`, `deploy/README.md`. `docs/STATE.md` и статус задачи — не код, не ревьюировались.
- Вердикт: **PASS с замечаниями**. Блокирующих замечаний нет.

## Линт — вывод команд (проверено 2026-10-03, локально на Mac, без сети)

- `cargo fmt --all -- --check` → rc 0, вывода нет.
- `cargo clippy --workspace --all-targets -- -D warnings` → rc 0 (`Finished dev profile`).
- `cargo test -p enricher -p hood-core` (`TMPDIR` = мой подкаталог scratchpad, `RPC_URL`/`FEED_URL`/`RECORDER_OUT_DIR` сняты) → rc 0: enricher lib 21, exit_codes 6, gaps 6, interrupt 2, logs 2, range 3, retry 6; hood-core 27. 0 failed. После прогона `TMPDIR` пуст: временные каталоги убираются.
- Рекомендательный `clippy -W pedantic -W nursery` по изменённым строкам: `testdir.rs:10` redundant_pub_crate (см. Р1, уходит при объединении guard'ов), `lib.rs:316` semicolon_if_nothing_returned, `atomic.rs:159` cast `u64 as u8` (в тестовом генераторе, намеренно), `tests/range.rs:94` много однобуквенных имён. Ни одно не влияет на поведение.
- `cargo clean` не запускал (по указанию координатора).

## Блокирующее

Нет.

## Важное

Нет. Р1 ниже — дубль формулы, расхождение пока только в граничном случае. Поэтому он в рекомендациях, но его стоит закрыть в этом же коммите: правка в 10 строк.

## Рекомендации

**Р1. Расчёт `planned_calls` и печать строк `to fill` написаны дважды, формулы уже разные.** `crates/enricher/src/lib.rs:312` (диапазон): `chunks.iter().map(calls_needed).sum().saturating_add(CHAIN_ID_CALLS)`. `lib.rs:357` (`--gaps`): `todo_blocks * CALLS_PER_BLOCK + if chunks.is_empty() { 0 } else { CHAIN_ID_CALLS }`. Цикл `to fill` — `lib.rs:319-321` и `lib.rs:361-363`.
- Почему важно здесь: на «сколько вызовов будет» смотрят перед ручным прогоном и при выборе `--max-calls` (4000/4001/4201). Если правило поменяется (например, второй служебный вызов), одну из формул легко забыть. Формулы уже расходятся. В `--gaps` умножение без saturating: на абсурдном `gaps.tsv` в debug это паника, в release — переполнение в строке журнала. В диапазоне пустой план добавил бы `+1`, хотя сейчас пустым он быть не может.
- Исправление: одна функция рядом с `min_calls_per_run`, вызывать в обоих местах. Поведение не меняется.
```rust
/// Calls the whole blocks plan makes: every file plus the chain id check
/// (0 for an empty plan, which makes no call).
fn planned_calls(chunks: &[Range]) -> u64 {
    if chunks.is_empty() {
        return 0;
    }
    chunks.iter().map(|c| blocks::calls_needed(*c)).fold(CHAIN_ID_CALLS, u64::saturating_add)
}

fn log_files_to_fill(chunks: &[Range]) {
    for c in chunks {
        info!(from = c.from, to = c.to, blocks = c.blocks(), "to fill");
    }
}
```

**Р2. Два одинаковых guard'а временного каталога. Обоснование «интеграционные тесты не видят `#[cfg(test)]`» неполное.** `crates/enricher/src/testdir.rs:1-37` и `crates/enricher/tests/common/mod.rs:210-239` (`Scratch`) — одинаковый код на ~20 строк. Разница только в имени типа и префиксе каталога.
- Почему важно: guard решает, удалить каталог или оставить его для разбора (`thread::panicking`). Если поправить это правило в одной копии, во второй оно останется старым. Новый крейт или feature ради 20 строк не нужен. Хватает `#[path]`.
- Исправление (проверено 2026-10-03 на копии рабочего дерева в scratchpad: clippy `-D warnings` и fmt чистые, все 46 тестов enricher проходят, `TMPDIR` пуст):
  - `src/testdir.rs`: `pub(crate) struct` / `pub(crate) fn new` → `pub`. Модуль подключён как `#[cfg(test)] mod testdir;` (приватный), так что публичный API крейта не расширяется. Заодно уходит pedantic `redundant_pub_crate`. В doc модуля заменить фразу про «cannot see» на «shared with `tests/common` via `#[path]`».
  - `tests/common/mod.rs`: вместо `struct Scratch` и двух impl:
```rust
#[path = "../../src/testdir.rs"]
mod testdir;
pub use testdir::TestDir as Scratch;

/// Fresh empty `$TMPDIR/enricher-it-<name>-<pid>`, removed when dropped.
pub fn scratch(name: &str) -> Scratch {
    Scratch::new(&format!("it-{name}"))
}
```
  - и удалить ставший лишним `use std::path::PathBuf;` (строка 6). Имена каталогов (`enricher-it-…`) и места вызова не меняются.
  - Вне объёма 026: в `crates/recorder/tests/mock_feed.rs` (строки 260, 305, … 714) уборка ручная, `remove_dir_all(&out).ok()` в конце теста. При падении теста это то же самое, но при панике раньше этой строки каталог тоже остаётся. Общий guard на два крейта — только если recorder получит задачу на тесты. Сейчас не трогать.

**Р3. Тест `rand01` в enricher дублирует тест hood-core.** `crates/enricher/src/rpc.rs:571-574`: цикл `for _ in 0..1000 { assert!((0.0..1.0).contains(&rand01())) }` проверяет функцию другого крейта. То же уже проверяет `hood-core/src/jitter.rs:118` (`unit_values_stay_in_range_and_spread`). Исправление: удалить цикл из `backoff_delay`-теста. Граница `u = 0.999` уже проверена строкой выше.

**Р4. Отчёт исполнителя расходится с рабочим деревом.** `docs/handoff/from-code/026-enricher-followups.md`:
- вопрос 2 («строку "Коды выхода" в data-model.md не дополнял»): в дереве она дополнена, `data-model.md:65`;
- вопрос 3 («в `hood-core/src/jitter.rs` осталось "switches over in task 026"»): исправлено, `jitter.rs:2`;
- раздел «Объём» не упоминает `jitter.rs`.

Правки верные и по объёму минимальные (одна фраза в каждом файле), но отчёт надо привести в соответствие до коммита. Иначе Cowork задаст закрытые вопросы.

**Р5. Мелочи.** `lib.rs:316`: в ветке `Mode::Logs` — `info!(…);` с точкой с запятой или без фигурных скобок, как в ветке `Mode::Blocks` (pedantic). `tests/range.rs:12-17` и `atomic.rs:144-147`: `has_checksum_flag` повторяется. Это тестовый код в разных крейтах сборки, по 4 строки. Если делать Р2, функцию можно положить в тот же `testdir.rs` (переименовать модуль в `testutil.rs`). Это не обязательно.

## Дубли

- Генератор джиттера: в enricher своего генератора больше нет (grep `jitter|rand01|xorshift` по `crates/enricher/src`). Пауза повтора берёт `hood_core::jitter::rand01()` (`rpc.rs:23, 436`). `next_step`/`backoff_delay` по-прежнему получают `u` параметром, поэтому юнит-тесты пауз детерминированы. xorshift в `atomic.rs:150-160` — это генератор тестовых данных с фиксированным seed (нужны несжимаемые байты), а не джиттер. Дублем не считаю.
- Проверка бюджета плана: в диапазоне переиспользуется та же `check_plan_fits_budget` (`lib.rs:217`, вызовы `lib.rs:271` и `lib.rs:365`), своей логики планирования нет. Текст ошибки и подсказка `lower --chunk … / raise --max-calls …` одинаковые. Дубль только в журнальной оценке `planned_calls` и цикле `to fill` (Р1).
- Guard временного каталога — две копии (Р2).

## Структура и разбиение

Переразбиение не нужно. `lib.rs` — 416 строк, `run` — ~50 строк, вложенность ≤ 3. Подготовка плана в диапазоне и в `--gaps` устроена по-разному: в `--gaps` сначала lock, потому что план вычитает `filled.tsv`; диапазон от диска не зависит. Общий «планировщик» здесь был бы абстракцией на вырост. Хватает двух helper'ов из Р1.

## Что хорошо (не сломать при правках)

1. Контрольная сумма включена в единственном месте записи — `AtomicZstdFile::create` (`atomic.rs:45`), с контекстом ошибки. `blocks.rs`/`logs.rs` не тронуты, других `zstd::Encoder` в enricher нет (grep). Тест `corrupted_byte_is_detected_by_the_checksum` с контролем: та же порча без checksum распаковывается молча. Так доказано, что тест проверяет именно сумму, а не структуру фрейма.
2. `--dry-run` в диапазоне — только план: проверки плана (`--max-blocks`, бюджет, `--topic0`) выполняются, но до lock, создания каталога и `connect`. Тест подтверждает 0 запросов и отсутствие каталога для обоих режимов. Семантика описана в doc модуля `lib.rs:19-31` и в `--help` (`--dry-run`: «Print the plan, check it and exit without RPC calls (range and --gaps)»; `--max-calls`: «Blocks mode (range or `--gaps`): less than the largest file + 1 is an error (exit 1)»; проверено по выводу `target/debug/enricher --help`). Отличие `--gaps` (берёт lock) объяснено.
3. Ошибка бюджета в диапазоне проверяется и как `!is_budget_exhausted` в lib-тесте, и как код 1 с 0 запросов на бинарнике (`exit_codes.rs`). Граница `--max-calls 21 --chunk 5` → ровно 21 вызов.
4. `check_max_blocks` стоит до `hr::chunk` (`lib.rs:264`, с комментарием): огромный диапазон с маленьким `--chunk` не строит и не печатает список файлов.
5. CLI-флаги не менялись, поменялись только тексты справки. Формат строк и сетка файлов не тронуты.

## deploy/README.md — объём

Задача (п. 5) разрешала только текст README. Изменена одна строка `README.md:305`: «1 000 блоков за прогон при 4000», объяснение через `eth_chainId` и пример WARN. Пример WARN сверен с форматом `rpc.rs:183` (`{what}: call budget exhausted: {sent} calls sent, {n} more would exceed --max-calls {max}`): 2000 + 1 вызов отправлено, следующий файл 2000 → 4001 > 4000. Пример верный. Правка примера выходит за букву п. 5, но исправляет неверный текст в том же абзаце. Принимаю. `deploy/enricher-gaps.service` не тронут, как велит задача. Комментарий в его шапке («4000 calls = 2000 blocks per run», строка 7) остаётся неверным до решения Михаила (4001/4201). Это нужно закрыть той же правкой, которой будет выбрано значение.

## Предполагается / не проверено

- Python `zstandard` (`analytics/hoodlib.py`) читает файлы с checksum и проверяет её. Я этого не проверял. Это зона data-auditor.
- Побайтовое совпадение распакованного содержимого с HEAD и `zstd -lv`/`zstd -t` на CLI я не повторял: в отчёте исполнителя это проверено, а для приёмки это критерий data-auditor.
- Вариант Р2 проверен только на копии в scratchpad. В рабочее дерево проекта я ничего не вносил. Артефакты сборки копии легли в общий `target/` (ключи — другой путь крейта), каталог копии удалён.
