# 020 — Надёжность enricher: отчёт

Исполнитель: indexer-engineer. База: HEAD `9e1b5ab` (019 закоммичена). Изменения не закоммичены, статус задачи не менял (`in-progress`), ревьюеров не запускал. Затронут только `crates/enricher`. В `hood-core` правок нет. `deploy/` и `references/` не трогал. `Cargo.lock` не изменился. Деплоя не было, RPC не вызывался: всё проверено на моке 127.0.0.1.

## Сделано (по пунктам задачи)

| # | Что | Где |
|---|---|---|
| 1 | `FailKind { RateLimited, Transient }` и `Failure::Retry { kind, reason, retry_after }`. `parse_batch` возвращает `(FailKind, String)`. Глобальная пауза и счётчик `rpc_rate_limited` зависят только от вида ошибки, текст идёт только в лог. Поиска «429» в тексте больше нет | `crates/enricher/src/rpc.rs:255`, `:541` |
| 1 (I5) | Решение о повторе — чистая `next_step(attempt, kind, retry_after, policy, u) -> Step { GiveUp \| Wait { delay, pause_all } }`, проверяется юнит-тестами без сервера и часов. `RateLimiter` переведён на `tokio::time::Instant`: его тест идёт на остановленных часах (`start_paused`), точно и без реального сна | `rpc.rs:286` |
| 2 | Перед первым скачиванием каждого прогона делается `eth_chainId` (1 вызов, входит в `--max-calls`) и сравнивается с `hood_core::CHAIN_ID` (4663). Если не совпало — ошибка `wrong network: the RPC endpoint <host> reports chain id N (0x..), expected Robinhood Chain 4663 (0x1237); check RPC_URL / --rpc-url. Nothing was downloaded` и выход 1. При `--dry-run` и пустом плане (всё уже в `filled.tsv`) вызова нет | `lib.rs:160` (`connect`), `rpc.rs:401` (`Rpc::chain_id`) |
| 3 | `--gaps`: при планировании проверяется, что `--max-calls` ≥ 2 × (блоков в самом большом куске) + 1 (`eth_chainId`). Если нет — ошибка конфигурации, выход 1 (а не 75), с подсказкой `lower --chunk to <= N or raise --max-calls to >= M`. Проверка идёт до `--dry-run`, так что пробный прогон тоже её показывает | `lib.rs:199` |
| 4 | `CallBudget::try_reserve` — один `fetch_update` (CAS). Запрос, который не влезает, счётчик не трогает, поэтому ложного раннего 75 у параллельного запроса нет | `rpc.rs:208`, `:219` |
| 5 | Коды выхода собраны в `crates/enricher/src/exit.rs`: `OK=0`, `FAILED=1`, `BUDGET_EXHAUSTED=75`, `INTERRUPTED=130`. Там же чистая `code(&Outcome)`. В `main.rs` ошибка `report()` (например, `--stats-json` не записался) пишется в WARN `…; the exit code is not affected` и код не меняет. Алиас `rpc::EXIT_BUDGET_EXHAUSTED` удалён после ревью (см. ниже) | `exit.rs:32`, `main.rs` |
| 6 | `filled.tsv` читается мягко: `parse_ranges_file_lenient`, битые строки с `\n` пропускаются с WARN `ignoring broken line of filled.tsv; …` (номер строки, строка, причина). `gaps.tsv` читается строго, как раньше | `ranges.rs:47`, `lib.rs:293` |
| 7 | `Rpc::call<T>(calls, what, OnTimeout, accept: Fn(&[Item]) -> Result<T, String> + Sync)`. Булева параметра `return_timeout` больше нет, есть `enum OnTimeout { Retry, Return }`. Блоки разбираются один раз (`accept_batch`), в `blocks.rs`/`logs.rs` нет ни одного `unwrap` на ответах RPC. Срез ответов разбирается через `let [b, r] = pair else …` — без индексов, которые могут вызвать панику. Размеры ответов для статистики записываются в `Rpc::send` после `accept`, как и раньше: только для принятых ответов | `rpc.rs:423`, `blocks.rs:79`, `logs.rs:92` |

Попутно (из рекомендаций ревью, по мелочи):
- R1: проверки аргументов собраны в `Args::validate()` (`lib.rs:131`). Общий цикл «скачать → commit → `filled.tsv`» вынесен в `fill_blocks` (`lib.rs:222`), используется в обоих режимах. `check_budget` переименован в `check_max_blocks`. Лишняя проверка `g.exists()` убрана: `read_gaps_file` и так даёт ошибку с путём.
- R3 частично: `Item`, `Call`, `call`, `fetch_batch`, `make_rpc` стали `pub(crate)`, поля `Rpc.policy`/`stats` больше не `pub`.
- R6: WARN, если не удалось поставить обработчик SIGTERM.
- R12: `n.min(u32::MAX as u64) as u32` заменено на `u32::try_from(n).unwrap_or(u32::MAX)`.
- Новый счётчик `global_pauses` в `Counters`/`--stats-json` и в логе `stats`: сколько раз ограничение частоты останавливало все задачи. Без него пункт 1 нельзя было проверить интеграционно.

Не делал: I8 (сырые байты RPC в `blocks-*`/`logs-*`) — запрещено задачей. R8 (`spawn_blocking` для zstd/fsync) и R5 (общий `rand01`) — вне объёма.

## Изменения поведения

1. **+1 вызов на прогон** (`eth_chainId`) в обоих режимах, если есть что качать. Он виден в `--stats-json`: `http_requests` +1, `calls.eth_chainId = 1`, строка `sizes.eth_chainId`.
2. **Не та сеть** — выход 1 до любой записи (новый случай для кода 1, новых кодов выхода нет).
3. **`--gaps` с `--max-calls` меньше одного файла + 1** — выход 1 при планировании (раньше тихий 75 на каждом прогоне).
4. **Файл, за который не заплатить остатком бюджета, не начинается** (`fill_blocks` → `Rpc::ensure_budget`): прогон сразу выходит с 75, вызовы на файл, который всё равно был бы выброшен, не тратятся. Касается режима `blocks` (и диапазона, и `--gaps`). Если в режиме диапазона весь кусок больше `--max-calls`, теперь будет выход 75 после одного `eth_chainId`. Раньше бюджет сначала тратился, а потом выход был тот же 75. Повторы по-прежнему могут исчерпать бюджет посреди файла — это обычный 75.
5. **Сбой записи `--stats-json`** — WARN, код выхода сохраняется (раньше 75/130 превращались в 1).
6. **Битая строка `filled.tsv`** — WARN и пропуск (раньше выход 1 на каждом прогоне, пока строку не удалят руками). Строка остаётся в файле, поэтому WARN повторяется на каждом прогоне, пока её не удалят. Данные это не портит.
7. **logs**: если `eth_getLogs` вернул не массив, запрос теперь повторяется и только потом даёт ошибку (раньше ошибка была сразу). Формат файла тот же.
8. Нет файла gaps: текст ошибки теперь `read gaps file <path>: No such file…` вместо `gaps file … does not exist`. Код тот же — 1.

Коды выхода (значения) не менялись, новых нет. **`deploy/README.md` не правил**: его раздел про коды (0/75/130/1) остаётся верным.

**Важно для `deploy/enricher-gaps.service` (не правил, вне объёма):** в юните `--max-calls 4000` при куске по умолчанию 1000 блоков. До 020 прогон закрывал ровно 2 файла (2 × 2000 = 4000). Теперь 1 + 2000 = 2001, и на второй файл остаётся 1999 < 2000. По п. 4 второй файл не начинается, так что за прогон закрывается **1 файл (1000 блоков) вместо 2**, и выход 75. Вызовы не теряются, но пропускная способность юнита падает вдвое. Чтобы вернуть 2 файла за прогон: `--max-calls 4001` (drop-in или правка юнита) либо `--chunk 999`. Комментарий в юните «4000 calls = 2000 blocks per run» тоже стоит поправить. Решать Михаилу/infra-ops. Юнит выключен, так что сейчас это ни на что не влияет.

## Проверено (2026-10-02, локально на Mac, без сети и RPC; только мок 127.0.0.1)

- `cargo fmt -p enricher -p hood-core -- --check` — чисто.
- `cargo clippy -p enricher -p hood-core --all-targets -- -D warnings` — чисто.
- `cargo test -p enricher -p hood-core` — 61 тест, 0 падений, 3 прогона подряд (проверка на нестабильность). `cargo build --release -p enricher` — ок.
- `cargo build/test/clippy --workspace` **не проходит из-за `crates/recorder`** (незавершённая перестройка 021: `file not found for module app/connlog/session/transport`). Мои изменения тут ни при чём. Как договаривались, проверял через `-p`.
- `enricher --help`: набор флагов тот же (`--gaps`, `--out-dir`, `--max-calls`, `--rps`, `--batch`, `--concurrency`, `--dry-run` и остальные), ничего не переименовано.
- Тесты по пунктам:
  - п.1: `rpc::tests::next_step_pauses_all_only_for_rate_limits`, `next_step_honours_and_caps_retry_after`, `batch_is_reordered_and_checked` (ошибка батча с «77429001» в тексте даёт `Transient`). Интеграционный `retry::transient_error_with_429_in_text_does_not_pause_all`: блоки 77429001..=77429002, null-ответ, `retries=1`, `global_pauses=0`. В `retries_after_429_honouring_retry_after` проверено `global_pauses=3`.
  - п.2: `exit_codes::wrong_chain_id_exits_1_and_writes_nothing` (мок отвечает `0xa4b1`: выход 1, в тексте 42161 и 4663, 1 вызов `eth_chainId`, 0 запросов блоков, файлов нет). В `gaps::gaps_fill_once_and_skip_closed_ranges` — ровно 1 `eth_chainId` за прогон и 0 при повторном прогоне без работы. В `gaps_dry_run_and_budget_make_no_calls` — 0 при `--dry-run`.
  - п.3: `exit_codes::gaps_budget_below_one_file_is_a_config_error` (1000 блоков, `--max-calls 1500`: выход 1, `2001`, `lower --chunk to <= 749`; при `--max-calls 2001` выход 0, ровно 2001 вызов). В `gaps_dry_run_and_budget_make_no_calls` — та же ошибка при `--dry-run`, она не считается исчерпанием бюджета (`is_budget_exhausted == false`).
  - п.4: `rpc::tests::refused_reserve_does_not_starve_concurrent_ones` (поток, который без конца просит больше `max`, против 4 × 25 000 резервов по 1) и `budget_reserves_exactly_up_to_max`. В `exit_codes::max_calls_exhausted_exits_75_and_keeps_finished_files` теперь ровно 9 вызовов (1 + 8), второй файл не начат.
  - п.5: `exit::tests::codes_by_outcome`; `exit_codes::stats_json_failure_does_not_mask_exit_75` (`--stats-json` указывает на каталог: выход 75 и WARN); `interrupt::sigint_mid_range_leaves_no_final_file` теперь с недоступным для записи `--stats-json`, выход 130.
  - п.6: `ranges::tests::gaps_strict_filled_lenient` и интеграционный `gaps::broken_filled_line_is_skipped` (строки `20` и `x\ty\tz` пропущены, 100..=103 засчитан, скачаны только 200..=201, повтор без запросов). Строгость `gaps.tsv` проверяет `unterminated_last_gaps_line_is_ignored_until_complete`.
  - п.7: `blocks::tests::accepted_batch_gives_the_same_line_bytes` — побайтовое сравнение строки `{"block":…,"number":10,"receipts":…}` с прежним форматом, плюс случаи null, error и неполной пары.
- **Проверка, что тесты ловят старые ошибки:** я временно вернул старый резерв (`fetch_add` → проверка → `fetch_sub`) и условие паузы `reason.contains("429")`. `refused_reserve_does_not_starve_concurrent_ones` упал в 3 прогонах из 3, `transient_error_with_429_in_text_does_not_pause_all` упал с `global_pauses = 1`. После этого код восстановлен (в `rpc.rs` снова `fetch_update`), полный прогон зелёный.
- Формат `blocks-*`/`logs-*` не менялся: `Value` строится так же (`json!({"number","block","receipts"})` из разобранных `Value`). Это закреплено побайтовым тестом выше, `logs.rs` меняет только способ получить ответ, запись та же.
- Зависимости: новых крейтов нет. В `[dev-dependencies]` enricher у tokio включена фича `test-util` (нужна для `start_paused`), это видно в `Cargo.toml` с комментарием.

## Предполагается / не проверено

- Что публичный RPC и будущий провайдер отвечают на `eth_chainId` строкой `"0x1237"`. Сам факт `eth_chainId → 0x1237` проверен 2026-09-28 (`chain-facts.md`, строка 7), но в этой задаче к сети я не обращался. Новых фактов о сети нет, поэтому `chain-facts.md` не менял.
- Что усечённая строка `filled.tsv` никогда не заявляет больше блоков, чем записано. Рассуждение такое: строка дописывается только после commit файла, а усечённый столбец `to` короче и потому меньше настоящего (или меньше `from`, и тогда строка битая). На реальных повреждённых файлах это не проверял.
- Поведение под systemd на сервере не проверял: юнит выключен, ssh не использовался.
- `references/data-model.md` (строки 50–52) не правил: файл сейчас меняют задачи 022/023, а коды выхода в нём остаются верными. Стоит дописать туда фразу «битая строка `filled.tsv` пропускается с WARN (с 020), `gaps.tsv` строг» — по согласованию, после коммита 022/023.

## Вопросы к Cowork / Михаилу

1. `deploy/enricher-gaps.service`: поставить `--max-calls 4001` (или `--chunk 999`), чтобы прогон закрывал 2 файла, как раньше? См. «Изменения поведения». Это зона infra-ops.
2. Нужна ли в `deploy/README.md` строка о том, что делать с WARN `ignoring broken line of filled.tsv` (удалить строку руками — необязательно, диапазон скачается заново)?

## Правки после ревью (architect-reviewer: PASS с замечаниями, 2026-10-02)

Только косметика и тесты. Поведение и формат файлов не менялись. `deploy/` не трогал, ответ на вопрос про `--max-calls 4001` ждёт решения Михаила. Поведение режима диапазона прежнее.

- **В2.** В многострочных фикстурах `tests/exit_codes.rs` (3 места) и `tests/gaps.rs` (3 места) стояли буквальные табы и переводы строк. Это артефакт моего инструмента правки, не намеренно. Теперь там однострочные экранированные `"…\t…\n"`, как в соседних тестах. Проверено `grep -P "\t"` по `crates/enricher/tests` и `src`: буквальных табов не осталось.
- Удалены неиспользуемые `rpc::EXIT_BUDGET_EXHAUSTED` (75 теперь задан только в `exit.rs`) и `Rpc::calls_sent`. `CallBudget::sent` нужен только тестам, поэтому помечен `#[cfg(test)]`.
- `min_calls_per_run` и `CHAIN_ID_CALLS` в `lib.rs` теперь приватные: снаружи они не используются.
- Поле лога `gaps plan` переименовано: `min_calls` → `planned_calls`, чтобы не путать с `min_calls_per_run`. Меняется только имя поля в логе, значение то же. Ни скрипты в `deploy/`, ни тесты на это поле не опираются.
- `blocks.rs` (`calls_for`): ёмкость вектора считается через `CALLS_PER_BLOCK`, а не через голое `* 2`. Схема id `n*2` / `n*2+1` не менялась.

Проверено (2026-10-02, локально): `cargo fmt -p enricher -p hood-core -- --check` чисто; `cargo clippy -p enricher -p hood-core --all-targets -- -D warnings` чисто; `cargo test -p enricher -p hood-core` — 61 тест, 0 падений, 2 прогона. `--workspace` по-прежнему падает на `crates/recorder` (идёт 021).

## Нужна проверка

Изменены загрузчик (enricher) и чтение `filled.tsv`. Перед приёмкой нужно прогнать **data-auditor** и **architect-reviewer** (`reviewers` задачи).
