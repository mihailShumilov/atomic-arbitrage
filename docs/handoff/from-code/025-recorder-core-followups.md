# 025 — recorder и hood-core: хвосты ревью (отчёт)

- Исполнитель: indexer-engineer. Дата: 2026-10-02, правки после ревью — 2026-10-03.
- База: HEAD `3b8e6a4`. На сервере работает recorder `703676b`; в `crates/recorder` и `crates/hood-core` между `703676b` и `3b8e6a4` изменений нет.
- Объём: `crates/hood-core`, `crates/recorder`, одна строка в `crates/enricher/src/lib.rs` (см. п. 4), одна правка в `.claude/skills/hoodchain-mev/references/data-model.md`.
- Не коммитил, статус задачи не менял, ревьюеров не запускал, деплоя не было.

## Сделано

### 1. `envelope_seqs`: одна функция для seq_max и дыр
- `route.rs`: `pub struct EnvSeqs { seq_first, seq_last, seq_max, intra_gaps, intra_disorder }` и `pub fn envelope_seqs(&FeedEnvelope) -> Option<EnvSeqs>`.
- Её вызывают `route_text` (живые кадры) и `RawLine::seqs` в `rawline.rs` (строки с диска). Копии `seqs → intra_envelope_gaps → max` больше нет, `intra_envelope_gaps` стала приватной.
- Запасной путь `rawline` не менялся: если JSON битый или в нём нет сообщений, берётся больший из столбцов `seq_first`/`seq_last`.
- Тесты: `route::envelope_seqs_derivation`, `rawline::disk_reader_agrees_with_live_routing`. Второй проверяет 6 конвертов (одно сообщение, дыры, беспорядок, повтор, `[3,9,4]`): разбор с диска и живой разбор дают одинаковые `seq_max` и дыры.

### 2. Порядок записи при смене часа
- `FeedWriter::accept` (`writer.rs`) вызывает `ensure_hour` для строки до того, как ставит её строки дыр в `pending_gaps` и сдвигает `last_seq`. Из `write_line` вызов `ensure_hour` убран.
- Результат: коммит, который закрывает прошлый час, больше не публикует строку дыры для строки, которая ещё лежит в открытом фрейме. Строка дыры уходит в `gaps.tsv` вместе с коммитом, после которого эта строка данных на диске.
- Устаревшая строка (`Stale`) по-прежнему не открывает файл часа и не вызывает смену часа: она возвращается до `ensure_hour`, как и раньше.
- Формат `gaps.tsv` не менялся. Изменился только момент записи строки, и только в одном случае: первая строка нового часа с дырой перед ней.
- Тесты:
  - `writer::hour_rotation_publishes_gap_row_after_its_line`. После смены часа в `gaps.tsv` только строка прошлого часа, `last_seq` = 113, файл нового часа пуст. После commit появляется строка `114 119`, `last_seq` = 120. Устаревшая строка в следующем часе файла не создаёт;
  - `writer::gaps_and_last_seq_never_ahead_of_data_across_rotations`. Инвариант после **каждого** `accept` (то, что осталось бы после kill -9): каждая строка `gaps.tsv` относится к строке в полном фрейме на диске, `last_seq.txt` не больше максимума данных. План из 9 строк: 5 часов, дыры внутри часов и на каждом стыке, устаревшая строка сразу после смены часа;
  - старый `hour_rotation_commits_only_written_lines` (021) не менялся и проходит.
- Мутационная проверка: временно вернул старый порядок (`ensure_hour` после постановки строк дыр). Оба новых теста упали, остальные тесты writer прошли. После этого файл восстановлен из копии.

### 3. Общий генератор джиттера в hood-core
- Новый модуль `crates/hood-core/src/jitter.rs`, только std:
  - `SplitMix64` — детерминированный поток (после ревью перенесён в `#[cfg(test)]`, см. «Правки после ревью»);
  - `rand01()` — один поток на процесс в `[0, 1)`. Seed = часы XOR pid, берётся один раз (`OnceLock`). Позиция в потоке — атомарный счётчик, поэтому два одновременных вызова не получают одно значение.
- Recorder перешёл на `hood_core::jitter::rand01()` (`app.rs`), его собственная `rand01` удалена. Enricher не трогал: его `jitter()` в `rpc.rs` переедет в задаче 026.
- Как меняется поведение: раньше каждый вызов брал splitmix64 от текущего `now_ns`, теперь это последовательный поток splitmix64 со случайным seed. Функция вывода та же (это проверяет тест `same_finaliser_as_the_old_recorder_jitter`). Распределение то же, равномерное в `[0, 1)`. Лестница пауз и её ±20 % не менялись.
- Тесты: эталонные значения `splitmix64.c` для seed 1234567 (`6457827717110365317, 3203168211198807973, 9817491932198370423`); совпадение `nth` с проходом по потоку; диапазон и разброс на 10 000 значений; 1 000 вызовов `rand01` из 4 потоков дают 1 000 разных значений.

### 4. hood-core
- **Типизированная причина в `LineError`**: `reason: LineFault`, где `enum LineFault { MissingColumn(&'static str), NotANumber(&'static str), BadRange(RangeError) }`. `Display` даёт прежние тексты побайтно (тест `line_fault_texts_are_unchanged`), поэтому WARN-строки и `detail` у `gaps_line_skipped` не изменились. Это подтверждает golden-тест `connlog`, его строки не менялись. В recorder `GapsSkip::Broken(String)` заменён на `GapsSkip::Broken(LineFault)`.
- **`subtract(want: &[Range], have: &[Range])`**: в `recovery::reconcile_gaps` пропал клон `listed.ranges` на каждую дыру. **Enricher:** чтобы он собирался, исправлена одна строка `crates/enricher/src/lib.rs:295`: `hr::subtract(gaps.clone(), filled.ranges)` → `hr::subtract(&gaps, &filled.ranges)`. Логика та же, только без клона.
- **Doc `parse_quantity`** исправлен: «`0x` и hex-цифры, значение которых помещается в u64; ведущие нули не считаются». Добавлен тест: `0x` + 20 цифр с ведущими нулями даёт `u64::MAX`.
- **`# Errors`** добавлен в doc `Range::new`, `parse_ranges_file`, `fsync_dir`, `rename_durable`, `write_atomic`, `append_synced`, `append_line_synced`. Проверка: `cargo clippy -p hood-core --lib -- -W clippy::missing_errors_doc -W clippy::missing_panics_doc` предупреждений не даёт.
- **`gaps.tsv` не в UTF-8 — решение:** recorder читает файл с заменой (`String::from_utf8_lossy`) и пишет один WARN `gaps.tsv is not valid UTF-8 …`. Битая последовательность становится U+FFFD:
  - если она в `from`/`to`, строка считается `broken`: WARN и `gaps_line_skipped broken` в `connections.tsv`, формат строки прежний;
  - если в `recv_ns`, диапазон учитывается.

  Остальные ошибки чтения (права, IO) по-прежнему дают выход 1. Обоснование то же, что у отступления 019: падение на собственном файле состояния даёт crash-loop под systemd и потерю фида, а лишняя строка дыры безвредна. Enricher `--gaps` такой файл по-прежнему не примет (его чтение не менялось), так что повреждение будет замечено. Задокументировано в doc `read_gap_ranges` и в `data-model.md` (абзац `gaps_line_skipped`). Тест `recovery::non_utf8_gaps_file_is_read_lossily` проверяет чтение и весь `recover`: ошибки нет, дыра 104..106 считается записанной, файл не переписывается.

### 5. По желанию
- **`Slot::Empty` убран** (`writer.rs`). Теперь `HourFile.slot: Option<Slot>`, переходы через `take()`. `None` бывает только после неудачного перехода, когда writer уже останавливается с ошибкой. Оба `unreachable!()` ушли. Поведение не менялось: при «невозможном» состоянии та же ошибка `writer in inconsistent state`.
- **Newtype времени (ns) не делал.** Это не мелкая правка: `u128` нс проходит через `Line.recv_ns`, `connlog` (`format_row`, `PendingPause`, `LogSession`), `backoff` (`startup_wait`, `session_end_ns`), `resume`, `layout::newest_data_mtime_ns` и `mock_feed`. Получился бы широкий дифф без изменения поведения, в одном деплое с поведенческими правками. Предлагаю отдельной задачей, если она нужна.

### Документация
- `data-model.md`: в описании `gaps.tsv` добавлено правило «строка дыры не раньше данных, с 025 и при смене часа», в абзаце `gaps_line_skipped` — политика для не-UTF-8. **Внимание при коммите:** в этом же файле лежит незакоммиченная правка задачи 028 (не моя), хунки нужно разделить.
- `chain-facts.md` не трогал: новых фактов о сети нет.

## Проверено (2026-10-02, локально на Mac; фид, RPC, ssh не использовались; `cargo clean` не запускал)

| Команда | Итог |
|---|---|
| `cargo fmt --all -- --check` | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 (вместе с параллельными правками 027 в decoders на момент прогона) |
| `cargo test --workspace` (TMPDIR = scratchpad, `FEED_URL`/`RPC_URL`/`RECORDER_OUT_DIR` сняты) | exit 0, 206 passed / 0 failed: hood-core 27 (было 21), recorder юнит 72 (было 67), `mock_feed` 16/16; остальное — enricher и decoders |
| Мутация п. 2 (старый порядок) | 2 новых теста падают, остальные зелёные; файл восстановлен |

Дымовой прогон нового debug-бинарника. Каталог в scratchpad; `gaps.tsv` с байтами `\xff` в `from` и `\xfe` в `recv_ns`; `--url ws://127.0.0.1:9` (закрытый порт); SIGTERM через 3 с. Результат:
- код выхода 0;
- в `connections.tsv` ровно одна строка `gaps_line_skipped broken … gaps.tsv line 2: column from is not a number: "1�2\t130\t5"`, затем `disconnected net_error`, `shutdown SIGTERM`; у всех строк 11 столбцов;
- `gaps.tsv` побайтно не изменён;
- в journal WARN про не-UTF-8.

Каталог потом удалён. Временные каталоги `recorder-test-*`, оставшиеся от намеренно падавших прогонов, тоже удалены.

## Предполагается / не проверено

- Сравнение HEAD и нового бинарника на моке (досылка, дыры, kill -9, writer error, смена часа) я не делал, это критерий приёмки для data-auditor. По коду ожидается побайтовое совпадение сырья, `last_seq.txt` и `connections.tsv` во всех сценариях, кроме двух:
  - (а) при смене часа с дырой перед первой строкой нового часа строка дыры появляется позже: со следующим коммитом, а не с коммитом ротации;
  - (б) не-UTF-8 `gaps.tsv`: HEAD выходит с кодом 1 (так в 019 Р6 и по коду `fs::read_to_string`; сам HEAD я на этом не запускал), новый бинарник работает дальше.
- `pause_s` в `connections.tsv` и раньше случаен (±20 %). Новый генератор даёт тот же диапазон. Статистически на живом процессе это не сравнивалось.
- На сервере (Linux) не проверялось ничего: деплоя не было.

## Прошу

- **Прогнать data-auditor** (изменены writer и recovery recorder'а). Сценарии из критериев задачи: досылка, дыры, kill -9, writer error, смена часа (`last_seq` не опережает данные, строка дыры не раньше данных), плюс не-UTF-8 `gaps.tsv`.
- **architect-reviewer** — по задаче.

## Вопросы к Cowork / Михаилу

1. Политика для не-UTF-8 `gaps.tsv` (читать с заменой, а не выходить с кодом 1) — моё решение по п. 4 с обоснованием выше. Нужно подтверждение Михаила; откат — одна функция.
2. Newtype времени (п. 5) — нужна ли отдельная задача.
3. Деплой можно совместить с 028 (как сказано в задаче): один рестарт на обе.

## Правки после ревью (architect-reviewer, `docs/reviews/025-recorder-core-followups-architect-reviewer.md`, 2026-10-03)

- **В1 (важное).** В `RawLine::seqs` (`rawline.rs`) убран быстрый путь `seq_first == seq_last`: JSON строки с seq разбирается всегда, через `route::envelope_seqs`. Разбор с диска и живой разбор теперь совпадают и на конвертах `[7, 9, 7]` и `[7, 3, 7]`, оба добавлены в `disk_reader_agrees_with_live_routing`. Запасной путь (битый JSON или нет сообщений → больший из столбцов) не менялся.
  - **Проверено (2026-10-03), что на реальных данных результат recovery прежний.**
    - Скрипт `scratchpad/fastpath_check.py` (`zstd -dc` + разбор JSON) прошёл по `data/feed-test-009`, `data/feed-test-002` и 4 серверным часам из `scratchpad/021/feed`. Итог: 157 970 строк с seq, у всех `seq_first == seq_last`, расходятся со старым правилом 0, то есть во всех сообщениях `sequenceNumber` = `seq_first`.
    - Debug-бинарник запущен на APFS-клоне 4 серверных часов (`ws://127.0.0.1:9`, SIGTERM). Получились те же 2 строки `gaps.tsv` (`77822837..78107253`, `78142927..78357238`) и тот же `last_seq` 78393025, что в аудите 021. Код выхода 0.
  - Цена: теперь разбирается JSON каждой строки. На 4 серверных часах (225 МБ сжатых, сканируются ≤ 3 файла) от запуска до `recovery done` прошло 8,8 с на debug-сборке. Release будет быстрее (не замерял). Старт всё равно ждёт `min_connect_interval` 120 с. Старую сборку для сравнения не замерял.
- **Р1.** `SplitMix64` больше не публичный: перенесён в `#[cfg(test)]` как эталонный пошаговый поток для `nth`. Публичный API `hood_core::jitter` — одна функция `rand01()`, в 026 enricher перейдёт на неё.
- **Р2.** Исправлен doc `rand01`: одновременные вызовы получают разные позиции в потоке и разные 64-битные значения, но `f64` хранит 53 бита, поэтому равные float возможны, хотя ничтожно редки.
- **Р3.** В тесте `hour_rotation_publishes_gap_row_after_its_line` комментарий стоит отдельной строкой после пустой. После `cargo fmt` он остаётся на месте.
- **Р4.** В плане инварианта `(1, 14)` заменено на `(2, 14)`: устаревшая строка теперь первая строка нового часа и не должна вызвать ротацию. Ожидаемые строки дыр и `last_seq` не изменились.
- **Р5.** `recovery.rs`: `matches!(text, Cow::Owned(_))`.
- **Р6.** `connlog.rs`: `detail` для `gaps_line_skipped` собирается сразу в каждом плече, промежуточного `String` нет. Golden-строки не менялись и проходят.
- **Р7.** Добавлен `# Errors` к `app::run`, `transport::tls_connector`, `transport::connect`. `cargo clippy -p recorder -p hood-core --all-targets -- -W clippy::missing_errors_doc` — 0 предупреждений.

Проверено после правок (2026-10-03, локально, без сети, `cargo clean` не запускал):

| Команда | Итог |
|---|---|
| `cargo fmt --all -- --check` | exit 0 |
| `cargo clippy -p recorder -p hood-core -p enricher --all-targets -- -D warnings` | exit 0 |
| `cargo test -p recorder -p hood-core -p enricher` | exit 0, 155 passed / 0 failed: hood-core 27, recorder юнит 72, `mock_feed` 16, enricher 19 + 5 + 6 + 2 + 2 + 6 |

## Деплой (после ревью; один плановый рестарт, как в 021)

Чистый клон с коммитом 025 (и 028, если вместе) копирует rsync'ом в `/opt/hoodchain-mev/src` координатор. Recorder перезапускается **один раз**: простой ~2 мин, ожидается одна новая строка `gaps.tsv` на ~500–600 блоков.

```bash
# 0. Права и состояние до
srv# chown -R hoodbuild:hoodbuild /opt/hoodchain-mev/src
srv# F=/srv/hood/data/feed
srv# systemctl show recorder -p NRestarts -p ActiveEnterTimestamp -p MainPID      # записать

# 1. Пре-проверки gaps.tsv (до рестарта)
srv# test ! -s $F/gaps.tsv || tail -c1 $F/gaps.tsv | od -An -c                     # ожидается: \n
srv# awk -F'\t' '!/^#/ && NF && (NF != 3 || $1 !~ /^[0-9]+$/ || $2 !~ /^[0-9]+$/ || $3 !~ /^[0-9]+$/) { print NR": "$0 }' $F/gaps.tsv
#    ожидается: пусто (иначе новый recorder запишет gaps_line_skipped; enricher --gaps на такой строке падает — сообщить)
srv# iconv -f UTF-8 -t UTF-8 $F/gaps.tsv > /dev/null && echo utf8-ok                 # ожидается: utf8-ok
srv# wc -l < $F/gaps.tsv; tail -n 2 $F/gaps.tsv; cat $F/last_seq.txt
srv# tail -n 3 $F/connections.tsv

# 2. Сборка на сервере (запись идёт, бинарник подменяется через rename)
srv# bash /opt/hoodchain-mev/src/deploy/build-on-server.sh
srv# cat /opt/hoodchain-mev/bin/BUILD_INFO     # git=<коммит 025> без -dirty; sha256 recorder новый; recorder.prev есть
srv# /opt/hoodchain-mev/bin/recorder --help | head -3

# 3. Рестарт — ОДИН раз
srv# systemctl restart recorder
srv# journalctl -u recorder -n 30 --no-pager   # "recovery done", затем "waiting before the first connect" (~120 с); нет "not valid UTF-8"

# 4. Пост-проверки (через ~3 мин)
srv# tail -n 8 $F/connections.tsv
#    по порядку: shutdown SIGTERM → client_close server_replied → startup_wait min_connect_interval
#    → connected … requested=<старый last_seq+1> mode=header → backlog done; нет 403/429, нет gaps_line_skipped
srv# tail -n 8 $F/connections.tsv | awk -F'\t' '{ print NF }' | sort -u            # 11
srv# tail -n 2 $F/gaps.tsv; tail -c1 $F/gaps.tsv | od -An -c                        # одна новая строка, в конце \n
srv# awk -F'\t' 'NF != 3' $F/gaps.tsv                                               # пусто
srv# cat $F/last_seq.txt; sleep 70; cat $F/last_seq.txt                             # растёт
srv# systemctl start healthcheck.service; journalctl -u healthcheck.service -n 20 --no-pager   # только INFO о новой дыре
srv# systemctl show recorder -p NRestarts -p ActiveEnterTimestamp -p MainPID       # PID новый, NRestarts не вырос

# 5. После ближайшей смены часа (HH:00 + ~2 мин): ротация прошла без ошибок writer'а
srv# journalctl -u recorder --since "-10 min" --no-pager | grep -E ' (WARN|ERROR) '  # только "gap in feed" (если были дыры)
srv# ls -l $F/$(date -u +%Y/%m/%d)/ | tail -n 2; cat $F/last_seq.txt                # файл нового часа растёт, last_seq растёт
```

Откат — по `deploy/README.md`, «Откат бинарника» (`recorder.prev`).

## Файлы

- `crates/hood-core/src/jitter.rs` (новый), `crates/hood-core/src/{lib,ranges,fsutil,hex}.rs`
- `crates/recorder/src/{route,rawline,writer,recovery,connlog,app}.rs`
- `crates/enricher/src/lib.rs` (одна строка, вызов `subtract`)
- `.claude/skills/hoodchain-mev/references/data-model.md`
