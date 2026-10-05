# 036 — Отдельная площадка для пулов Pons v2 (хук мем-пулов). Отчёт

- Дата: 2026-10-05.
- Исполнитель: indexer-engineer.
- Сеть: 0 вызовов RPC, 0 подключений к фиду. Сервер не трогал. Docker использовал только локально: временный контейнер ClickHouse из уже скачанного образа и локальный ClickHouse проекта.
- Не коммитил. Статус задачи — `in-progress`, `done` ставит координатор после ревью.
- `contracts.md` не правил. Адрес хука взят из него: строка «Pons v2 meme hook», `verified` (Михаил, 2026-10-05).
- Scratch (не в git): `/private/tmp/claude-501/-Users-mihailshumilov-sites-my-crypto-atomic-arbitrage/a477d6fc-6af5-4e28-af3e-63445fb94d7a/scratchpad/036/`:
  - `scan-before.txt`, `scan-after.txt` — вывод `swaps_scan` до и после;
  - `found.txt`, `find.py` — независимый поиск свопов в пулах хука Pons v2.

## Сделано

1. **Миграция `sql/005_venue_pons_v2_hook.sql`.** Добавляет `'pons_v2_hook' = 6` в `venue` таблиц `hood.swaps` и `hood.tokens` через `ALTER TABLE … MODIFY COLUMN`. Остальные имена и значения не тронуты (1–5 и 9).
   - Перед ALTER: `throwIf`, если тип колонки не равен ни типу 004, ни целевому. Так ручная правка схемы не будет молча перезаписана.
   - После ALTER: `throwIf`, если тип не стал целевым.
   - `apply-unless`: обе колонки уже имеют точный целевой тип.
   - Без EXCHANGE / CREATE OR REPLACE / REPLACE TABLE, таблицы не пересоздаются.
2. **`rows.rs`.**
   - Добавлен `Venue::PonsV2Hook = 6`: в `ALL` (стоит в порядке значений, между `PoolsTrade` и `Other`), в `as_str` и `enum8`. `parse` работает через `ALL`.
   - Тест сверки с SQL раньше брал тело последнего `CREATE TABLE` и падал на любом последующем `ALTER`. Теперь хелпер `current_table` накладывает на это тело однострочные `ALTER TABLE hood.<t> MODIFY COLUMN …` из последующих миграций. Любой другой `ALTER` по-прежнему роняет тест. Значит, `swaps_enum8_in_sql_equals_rust_enums` сверяет Rust с типом из 005: 7 значений, тест зелёный.
3. **Классификация по хуку (`pools.rs`).**
   - `addresses.rs`: `PONS_V2_MEME_HOOK` с источником и статусом из `contracts.md`. Адрес включён в список `VERIFIED` и в тест модуля.
   - `pools::BUILTIN_V4_HOOKS = [(PONS_V2_MEME_HOOK, PonsV2Hook)]` и `PoolRegistry::builtin()` (хуки разрешены как `verified`), по образцу `TokenRegistry::builtin()`.
   - `PoolRegistry::allow_v4_hook(hook, venue, status)` — общий механизм. Для хука `0x0` и для площадок `uni_v3` / `uni_v4` / `other` возвращает ошибку. Повтор с другой площадкой или статусом — тоже ошибка.
   - Правило: v4-пул с разрешённым хуком получает площадку хука. Действует и при `insert` (TSV, `Initialize`), и задним числом при `allow_v4_hook`, поэтому порядок регистрации не важен.
     - `other` повышается до площадки хука. Площадка хука сохраняется. Любая другая площадка у пула с этим хуком — ошибка, при этом ничего не меняется.
     - Статус пула становится минимумом из статусов пула и хука.
     - Пул без хуков остаётся `uni_v4`, пул с неразрешённым хуком — `other`, как раньше.
   - Инвариант `insert`: площадка `pons_v2_hook` возможна только у v4-пула с ненулевыми хуками.
   - `extend` переносит разрешённые хуки.
4. **`swaps_scan`.**
   - По умолчанию стартует с `PoolRegistry::builtin()`.
   - Новые флаги: `--no-builtin-hooks` (поведение до 036) и `--v4-hook ADDR=VENUE:STATUS`.
   - Хуки печатаются в сводке.
   - Аргументы `--pools` / `--v4-manager` собираются и применяются после хуков. Результат от порядка флагов не зависит.
5. **Документация.**
   - `data-model.md`: значения `venue` с 6; оговорка «только AMM-часть»; правило «площадка v4-пула — по хуку»; миграция 005 в списке миграций.
   - Docstring'и: `Venue`, модуль `pools` (раздел про площадку по хуку), `observe_log`, `parse_tsv`, модуль `swap_rows`, `SwapRow::COLUMNS`.
6. **Тест на реальных логах:** `crates/decoders/tests/pons_v2_hook.rs`, фикстура `crates/decoders/tests/fixtures/pons-v2-hook-block.jsonl`.
   - Фикстура — блок 61620482, одна неизменённая строка `data/samples/hourly-20260804-20260930.jsonl.zst`; tx `0x9c5612d2cc50ceba13b51b1222dd2703a100ed3ef4ee4f610cedb77b734f7af8`, log 1, пул `0xfaf0…53aa`.
   - Реестр — строка этого пула из `pools-035.tsv` с `other`, как в файле.
   - Проверяется:
     - с хуком строка получает `pons_v2_hook` при любом порядке регистрации, без хука — `other`;
     - `observed`-хук понижает статус пула до `observed`;
     - поля строки сверены с данными, разобранными отдельно на Python: sell, token 525 398 551 135 979 485 204 092, ETH 5 762 611 981 068 415 wei, trader, router;
     - `fee_quote` = 0, потому что комиссия в событии 0.
   - Путь `Initialize` с хуком Pons покрыт синтетическим юнит-тестом: `Initialize` с этим хуком в `data/` нет (см. ниже).

## Исправления по ревью architect-reviewer (2026-10-05)

Ревью: `docs/reviews/036-venue-pons-v2-hook-architect-reviewer.md`, вердикт PASS с замечаниями. Координатор решил: исправить В1 и Р1, Р4–Р6 если дёшево; Р2 не трогать (005 уже применена, sha256 в журнале); Р3 пропустить. Встроенный хук и флаги `--no-builtin-hooks` / `--v4-hook` остаются, `pools-035.tsv` не переписывать.

- **В1 (исправлено).** Добавил статичное правило `venue_fits_hooks(venue, hooks)` в `pools.rs`: `pons_v2_hook` допустим только при `hooks == PONS_V2_MEME_HOOK`. Правило не зависит от того, что зарегистрировано, поэтому работает и в `parse_tsv` из пустого реестра.
  - `insert` отвергает `pons_v2_hook` при неизвестных хуках (`-`), при отсутствии хуков, с чужим хуком и у v3-пула.
  - `allow_v4_hook` принимает только площадки из белого списка {`pons_v2_hook`, `pools_trade`}. `pons_v2_hook` разрешён только для хука Pons.
  - Тесты: `pons_v2_hook_venue_needs_the_pons_hook` проверяет `None`, `0x0`, Doppler и v3, в пустом реестре и в `builtin()`. `allow_v4_hook_rejects_bad_arguments` проверяет `pons_v1`, `pons_v2_curve`, `uni_*`, `other` и Doppler с `pons_v2_hook`.
- **Р1 (исправлено).** Поиск ALTER в тесте сверки SQL теперь идёт по потоку токенов всего текста после `CREATE`: без учёта регистра, с любыми переносами строк, с `IF EXISTS` у таблицы и у колонки. Логика вынесена в `apply_alters`.
  - Любой ALTER таблицы, кроме `MODIFY COLUMN`, роняет тест. Несколько команд в одном ALTER тоже роняют тест.
  - Синтетические тесты: `apply_alters_finds_every_form` (многострочная форма, строчные буквы, `IF EXISTS`, ALTER другой таблицы игнорируется), `apply_alters_rejects_other_commands` (многострочный `ADD COLUMN`), `apply_alters_rejects_several_commands`.
- **Р4 (сделано).** Поля `V4Hook` приватные, доступ через геттеры `hook()` / `venue()` / `status()`; `swaps_scan` обновлён.
- **Р5 (сделано).** Добавлены хелперы тестов `v4_ref(id)` и `venue(r, id)`. `allow_v4_hook_checks` разбит на 4 теста: аргументы, правило `pons_v2_hook`, классификация при `insert`, конфликт и `extend`.
- **Р6 (сделано).** В `allow_v4_hook` затронутые пулы сортируются по `pool` до проверки, поэтому в сообщении об ошибке всегда первый пул в этом порядке.
- **Не трогал:** `sql/005` (Р2), разбиение `pools.rs` (Р3), `pools-035.tsv`, crates задачи 038. `cargo fmt` запускал только как `-p decoders`.
- **Проверено после исправлений (2026-10-05):**
  - `cargo fmt -p decoders -- --check` — чисто;
  - `cargo clippy -p decoders --all-targets -- -D warnings` — код 0;
  - `cargo test --workspace` — 0 failed; decoders lib 43 (было 37), `pons_v2_hook` 2;
  - `swaps_scan` на тех же входах: вывод побайтно совпал с `scan-after.txt`. Прогон с `--no-builtin-hooks` побайтно совпал с `scan-before.txt`. Числа по площадкам не изменились.

## Что проверено и как

**Проверено (2026-10-05, как):**
- `cargo fmt --all --check` — чисто.
- `cargo clippy --workspace --all-targets -- -D warnings` — чисто.
- `cargo test --workspace` — всё зелёное, 0 failed. В `decoders`: lib 37 (было 35: +2 теста в `pools`; тест сверки SQL переписан), `tests/pons_v2_hook.rs` 2.
- `bash sql/test_apply.sh` — `ok: 31 checks`.
- **Миграции на пустом томе:** временный контейнер `clickhouse-server:26.9.6.6` на `127.0.0.1:28125` с bind-монтированием каталога scratch (как у рабочего тома).
  - Первый `apply.sh` — `applied` 001–005, `changes: 5`. Второй — `changes: 0`.
- **005 на непустых таблицах:** `--to 4`, затем вставлены строки `other` / `uni_v4` в `swaps` и `pools_trade` в `tokens`.
  - Вставка `pons_v2_hook` до 005 даёт `Code: 691 Unknown element`.
  - После 005 вставка проходит. Старые строки целы, значения 9 и 4 не изменились, мутаций 0 (ALTER только метаданные).
- **Идемпотентность:**
  - оба ALTER повторно вручную — без ошибок;
  - запись 005 удалена из журнала, `apply.sh` — `skipped: 005`;
  - после `docker restart` типы и строки на месте.
- **Отказ на чужом типе:** у `tokens.venue` вручную добавлено `'x' = 7`, затем `apply.sh` падает с `005: … unexpected type … refusing to modify`, в журнал ничего не записано.
- Временный контейнер и его каталог удалены.
- **Локальный ClickHouse (`127.0.0.1:18123`, том `data/clickhouse`).** До работы контейнер был остановлен (`Exited` 2 дня), после работы снова остановлен.
  - `apply.sh --dry-run`: pending только 005.
  - `apply.sh`: `applied: 005`. Повтор: `changes: 0`.
  - `system.columns`: у обеих таблиц целевой тип.
  - `hood.swaps` и `hood.tokens` — 0 строк, незавершённых мутаций 0.
  - После `docker compose restart` то же самое, `apply.sh` — `changes: 0`.
  - Журнал: 1 applied, 2 applied, 3 skipped, 4 applied, 5 applied.
- **`swaps_scan` до/после** на тех же входах, что в 035. Файлы: 7 файлов `data/blocks` по возрастанию номера, затем `data/samples/hourly-…`, всего 2 712 блоков. Аргументы: `--pools data/registry/pools-035.tsv --tokens data/registry/tokens-035-verified.tsv --v4-manager 0x8366…0951=verified`, режим `verified`.
  - Построчное сравнение `rows-before.tsv` и `rows-after.tsv`: изменилась только колонка `venue`, ровно в 1 242 строках, все `other → pons_v2_hook`. Все 1 242 строки — из пулов, у которых в реестре `hooks = 0xe5e7…e044`. Ни одна строка этого хука не осталась `other`.
  - Независимый поиск по сырым логам (Python, `find.py`): в данных 1 910 v4-`Swap` в пулах хука Pons v2. 1 242 из них в пулах с котировкой ETH/WETH/USDG, это и есть 1 242 строки. 668 без такой котировки, пропущены как `no_quote` (пары против других токенов; вероятно, акций — это задача 037).
  - Пулов хука Pons v2 в реестре 035: 947.
  - `Initialize` с хуком Pons v2 в данных нет: 0 из 30.

## Числа: площадки до и после

Режим `verified`, входы 035 (2 712 блоков, 13 530 свопов):

| venue | до (как в 035) | после | `--no-builtin-hooks` |
|---|---|---|---|
| uni_v3 | 5 854 | 5 854 | 5 854 |
| uni_v4 | 2 834 | 2 834 | 2 834 |
| **pons_v2_hook** | — | **1 242** | — |
| other | 1 922 | **680** | 1 922 |
| всего строк | 10 610 | 10 610 | 10 610 |

- Пропуски не изменились: всего 2 920 (v3 `no_pool_meta` 956, `no_quote` 51, `zero_amount` 1; v4 `no_pool_meta` 382, `no_quote` 1 514, `zero_amount` 16).
- `v4_rows_with_hooks` = 1 922, как было: счётчик считает все хуки.
- Остаток `other` (680 строк) — другие хуки. Больше всего у Doppler `0x4e34…a544` (87, `observed`), дальше `0xce10…4a80` 27, `0xfa57…8a80` 26, `0x594e…a080` 26 и т. д.

## Предполагается (не проверено)

- В `HookFeeCollected` хука (log 2 фикстуры) третье слово равно 57 626 119 810 684 — это ровно 1% ETH-ноги свопа. Предполагаю, что это комиссия хука: проверено на одной tx, событие не декодируется. Это иллюстрация оговорки «только AMM-часть», а не факт для `chain-facts.md`.
- Расширение Enum8 через `MODIFY COLUMN` не переписывает данные. Проверено косвенно: 0 мутаций и сохранность строк на маленькой таблице. На больших таблицах не проверял.

## Что не получилось или что стоит знать

- **`cargo fmt --all`** я запустил один раз, когда другой агент (судя по файлам, задача 038) параллельно правил `crates/enricher`, `crates/loader`, `crates/recorder`, `crates/hood-core`. Если его правки были не отформатированы, `cargo fmt` мог поменять в них форматирование (только форматирование). Сейчас `cargo fmt --check` по всему воркспейсу чист. В его файлах я ничего содержательно не менял.
- Тест `Initialize` → `pons_v2_hook` синтетический: реального `Initialize` с этим хуком в локальных данных нет.
- В `chain-facts.md` ничего не добавил: новых проверенных фактов о сети нет, только статистика по локальным данным (она в `data-model.md` и здесь).

## Вопросы

Оба вопроса закрыты решением координатора по ревью: встроенный хук остаётся, `pools-035.tsv` не переписывается. Новых вопросов нет.

## Изменённые файлы

- `sql/005_venue_pons_v2_hook.sql` (новый)
- `crates/decoders/src/rows.rs`
- `crates/decoders/src/addresses.rs`
- `crates/decoders/src/pools.rs`
- `crates/decoders/src/swap_rows.rs` (только doc)
- `crates/decoders/examples/swaps_scan.rs`
- `crates/decoders/tests/pons_v2_hook.rs` (новый)
- `crates/decoders/tests/fixtures/pons-v2-hook-block.jsonl` (новый, 10 032 байта)
- `.claude/skills/hoodchain-mev/references/data-model.md`
- `docs/handoff/to-code/036-venue-pons-v2-hook.md` (status: in-progress)

Нужны ревью: **data-auditor** (изменились декодер и схема) и **architect-reviewer** (код).
