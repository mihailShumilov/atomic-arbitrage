# 024 — feed_audit.py, обёртка аудита, общий модуль analytics (отчёт)

Исполнитель: indexer-engineer (Python и обёртка `deploy/`; по указанию координатора обёртку тоже делал я, в стиле `deploy/*.sh` и их тестов). Дата: 2026-10-02. База: HEAD `449576f`.

Коммита нет, статус задачи не менял, ревьюеров не запускал (по указанию координатора). Сеть: RPC и фид не использовались, ssh тоже. `uvx` запускал ruff 0.16.10 из кэша, образ `koalaman/shellcheck:stable` скачан, использован с `--network none` и удалён. Сервер не трогал. cargo не запускал. Файлы параллельных задач 019, 022 и 023 (`crates/`, `sql/`, `docker-compose.yml`) не трогал.

## Сделано

### 1. `.claude/skills/feed-audit/scripts/feed_audit.py` (замечания В3, В4, В5 ревью)

Скрипт по-прежнему один файл, только stdlib, совместим с Python 3.9 (`from __future__ import annotations`, без `slots=`, `match` и `datetime.UTC`).

- **Время с часовым поясом.** Добавлена константа `UTC = datetime.timezone.utc`. Теперь aware: `hour_start()`, разбор `--now` (`parse_now`) и умолчание `datetime.now(UTC)`, то есть все три места сразу. `utc(ns)` делит ns целочисленно (`divmod`) и печатает миллисекунды. Раньше было `utcfromtimestamp(ns / 1e9)`: float, округление до микросекунд, затем отбрасывание. В набросках ревью было `%02d`, но тогда исчезла бы третья цифра миллисекунд, поэтому оставлен формат `.mmmZ`, как в выводе до правок.
- **Битый конверт.** Вместо traceback увеличивается счётчик. Функция `parse_messages(env)` возвращает `None`, если:
  - конверт не объект;
  - `messages` не список;
  - у сообщения нет целого `sequenceNumber` или `message.message.header.kind`/`blockNumber`. `bool` целым не считается.

  Такой конверт пропускается и считается в `bad_envelopes`, в `envelopes` не входит, его seq становится дырой. Это FAIL с текстом `N envelopes with a malformed message (no integer sequenceNumber / header.kind / header.blockNumber)`. Аудит дня идёт дальше.
- **В сводке новая строка `bad_envelopes`** (стоит после `envelopes`). Других изменений формата нет.
- **Разбиение `main()`** (было 310 строк, 64 ветвления; стало 35 строк):
  - `Clock` (frozen dataclass: текущий и прошлый час, `grace_until`);
  - `Session`, `Tally` (dataclass вместо ~25 локальных переменных);
  - `Scanner` (`scan_file`, `check_tail`, `scan_line`, `track_session`, `scan_envelope`);
  - функции `check_counts`, `check_feed_root`, `check_rpc`, `build_summary`, `print_text`, `print_no_blocks`, `parse_args`, `resolve_inputs`;
  - `main(argv=None)` вызывается из тестов.

  Порядок проверок и порядок строк FAIL/WARN прежний.
- **Мелкие правки:**
  - процесс `zstd` больше не оставляет незакрытый stderr: раньше был скрытый `ResourceWarning`, его было видно при `-W default` и в unittest;
  - `last_seq.txt`, в котором не число, даёт WARN, а не traceback;
  - `contextlib.suppress`, `yield from`, `round()` без `int()`;
  - кириллическая «Р» в «Remark Р1/Р3» заменена на латинскую R.
- **В пути «no blocks found»** (stderr, код 1, вывод только в stderr) теперь печатаются и FAIL-счётчики (битый JSON, битые конверты и т. п.), раньше только ошибки zstd. Это единственное изменение вывода на путях, где нет блоков.
- **Новый `test_feed_audit.py`**, stdlib `unittest`, 22 теста, на сервер не ставится:
  - чистые функции: `frame_len`/`split_frames` (целый, обрезанный, skippable-фрейм, мусор), `merge_ranges`, `classify_seq0`, `parse_messages`, `utc`, aware-время, `pct`, `is_feed_file`;
  - сквозные прогоны на синтетических `.tsv.zst` во временном каталоге: нормальный день, битый конверт, только битые конверты, дыра вне `gaps.tsv` и покрытая двумя смежными строками, дубль и откат `recv_unix_ns`, открытый хвост текущего часа и закрытого, окно прошлого часа, мусор после фреймов, seq 0 (`with_messages`, `other`), сессии по `connections.tsv` и паузам, `last_seq.txt`, ошибки запуска, ключи текстового вывода, которые читает обёртка;
  - `DeprecationWarning` в прогонах считается ошибкой. Отдельный тест запускает скрипт без `--now`, то есть с реальными часами.

  Запуск: `python3 -m unittest discover -s .claude/skills/feed-audit/scripts -v`.
- **`SKILL.md` feed-audit:**
  - строка о независимости от `analytics/` (п. В7 ревью);
  - новая проверка в таблице, `bad_envelopes` в «Цифрах»;
  - коды выхода и то, как их читает обёртка;
  - раздел «Тесты»;
  - блок «Проверено 2026-10-02, задача 024».

### 2. `deploy/feed-audit-daily.sh` (замечание В8) и `deploy/test/test-feed-audit-daily.sh`

- Решение принимается по коду выхода `feed_audit.py`:
  - 0 → PASS: INFO, если `AUDIT_NOTIFY_PASS=1`, и выход 0;
  - 1 и строка `verdict … FAIL` → ALERT «FAIL»;
  - 1 без вердикта (traceback, «no blocks found») → ALERT «FAIL (нет вердикта, rc=1)»;
  - любой другой код (2 — например, за день нет файлов; 137 — убит) → ALERT «аудит не отработал (rc=N)».

  Все не-PASS дают выход 3. Если notify упал, выход 1, неверная дата — 2. Строка `# exit N` в отчёте осталась. Числа для INFO по-прежнему берутся из текста по ключам: тест на реальном `feed_audit.py` проверяет, что ключи на месте.
- В шапку добавлен комментарий, почему нет `set -e` (Р15), и описание кодов выхода.
- `feed-audit.service` (`SuccessExitStatus=3`) и `bootstrap.sh` не менялись: новых устанавливаемых файлов нет.
- Новый тест в стиле `test-mdadm-event.sh`:
  - `python3` — подставной: печатает заданный вывод и выходит с заданным кодом, параллельно пишет свой argv;
  - `fake-notify` пишет `LEVEL|TITLE|BODY`.

  Случаи:
  - PASS: текст INFO, отчёт, права 0640, нет временного файла, argv, проброс `AUDIT_FRAME_SECS`/`AUDIT_RPC_SAMPLE`;
  - PASS при `AUDIT_NOTIFY_PASS=0`;
  - FAIL;
  - traceback;
  - «в тексте PASS, rc=1» → ALERT и «в тексте FAIL, rc=0» → PASS: доказывает, что решение по коду;
  - rc=2, rc=137;
  - notify упал (FAIL и PASS);
  - плохая дата;
  - день по умолчанию = вчера (только с GNU date);
  - если есть настоящие `python3` и `zstd`, ещё 4 случая на реальном `feed_audit.py` и синтетическом фиде: хороший день, битый конверт, день без файлов.

### 3. `analytics/hoodlib.py` и `hourly_*.py` (замечание В7, Р13, Р14)

- `hoodlib.py` (stdlib, без классов):
  - константы `SYS`, `L2_TYPES`, `L1_TYPES` и topic0 `V3_SWAP`/`V4_SWAP` со ссылкой на `contracts.md`;
  - `iter_block_lines` — поток через `zstd -dc`, без чтения файла целиком;
  - `iter_blocks`;
  - `tx_receipt_pairs` — число receipts = числу tx и совпадение `transactionHash`, иначе `ValueError`;
  - `user_tx_pairs` — всё, кроме `0x6a`;
  - `tx_class`, `fee_wei`, `gas_used_for_l1`;
  - `median_int` — целочисленная медиана для wei (Р13).
- `hourly_metrics.py` переведён на `hoodlib`: `assert` заменены проверкой с `ValueError` (работает и при `python -O`), чтение потоковое, `fee_median_wei` — `median_int`.
- `hourly_summary.py`:
  - `sys.path.insert` убран;
  - обе копии «user tx + комиссия» заменены на `hoodlib`, так что теперь они тоже проверяют receipts и хэши;
  - сэмпл читается один раз и потоково (раньше дважды и целиком в память);
  - `main()` разбит на `load_sample`, `calibrate`, `add_derived`, `read_raw`, `print_*`, `bootstrap`, `blocks_per_day` (было 255 строк, стало 28);
  - захардкоженные значения отчёта 010 вынесены в именованные константы. Они намеренно остаются фиксированными: отчёт разовый и принят (Р12).
- `hourly_sample.py` (RPC; запускать его нельзя, поэтому проверял на подставном RPC, см. ниже):
  - `read_env_rpc` через `with`;
  - `Rpc` стал контекстным менеджером, журнал закрывается (Р14);
  - `raise … from`;
  - `main()` разбит на `parse_args`, `hour_starts`, `load_progress`, `predictor_start`, класс `Sampler` (`find`, `receipts`, `sample_hour`, `run`), `compress_atomically`.

  Поведение и порядок побочных эффектов прежние: журнал создаётся до сверки индекса, как раньше. `Rpc` в `hoodlib` не переносил: второго RPC-скрипта нет (как и советовало ревью).
- `analytics/README.md` переписан: состав, порядок запуска, независимость `feed_audit.py`. Это Р15.

### 4. ruff: остаток 92 замечаний из 018

Стало 0: `uvx ruff check analytics .claude/skills/feed-audit/scripts` → `All checks passed!`; `uvx ruff check .` из корня тоже чист.

Исключения:

| Где | Что | Почему |
|---|---|---|
| `pyproject.toml`, per-file-ignore | `UP031` для `feed_audit.py` (31 место) | рекомендация ревью 018: строки вывода аудита сверяются побайтно, а замена `%` на f-строки — небезопасная правка без пользы. Файла `pyproject.toml` нет в списке объёма задачи, но без него п. 4 не закрыть |
| `feed_audit.py:143` `frame_len` | `noqa: PLR0911, PLR0912` | побайтный разбор заголовка zstd-фрейма (RFC 8878): по одному `return` на каждый вариант обрыва или порчи читается проще вложенных помощников. Пояснение в комментарии над функцией |
| `hourly_sample.py:106` `Rpc.log` | `noqa: PLR0913, PLR0917` | один аргумент на колонку журнала (`LEDGER_HDR`) |
| `hourly_sample.py:89` | `noqa: SIM115` | журнал открыт всё время прогона, закрывается в `Rpc.close()` / `__exit__` |

`hourly_summary.py:161` (UP031) переписан на f-строку, как просило ревью 018. Остальное исправлено в коде, без игнорирования:
- E501 — разбивкой литералов;
- F841, B007, E741, B023 — заменой на `itemgetter`;
- SIM115, B904, PLW2901;
- PLR0912/0915 — разбиением функций.

## Проверено (2026-10-02, Mac, без сети)

| Проверка | Результат |
|---|---|
| `uvx ruff check` / `ruff format --check` (analytics, feed-audit/scripts; и `ruff check .`) | чисто; `7 files already formatted` |
| `python3 -m py_compile` под 3.9.6 | ок для всех 6 файлов `.py` |
| `test_feed_audit.py` | 22 теста ok под 3.9.6 и под 3.14 (`/opt/homebrew/bin/python3.14`) |
| `feed_audit.py` на реальных данных: `data/feed`, `data/feed-test-009` и дополнительно `data/feed-test-002`; `--feed-root data/<x> --rpc-sample 0 --now 2026-10-02T12:00:00Z 'data/<x>/2026/*/*/feed-*.tsv.zst'`, текст и `--json`, под 3.9.6 и 3.14; HEAD и рабочая копия, `diff -r` по stdout, stderr и коду выхода | 12 пар прогонов, у всех код 0 и PASS. Отличия ровно такие: (1) в 6 текстовых выводах новая строка `bad_envelopes      0` после `envelopes`; (2) в 6 JSON новый ключ `"bad_envelopes": 0,` после `"envelopes"`; (3) в 6 stderr под 3.14 исчезли 2 строки `DeprecationWarning: datetime.datetime.utcfromtimestamp() …` и строка кода под ней. Больше отличий нет |
| То же без `--now` под `python3.14 -W error` | код 0, предупреждений нет. HEAD в этом режиме печатает 2 `DeprecationWarning` (`utcnow` и `utcfromtimestamp`) |
| Дифференциальный прогон HEAD и новой версии на 13 синтетических сценариях × (текст, `--json`) | 26 из 26 совпали, если не считать строки `bad_envelopes`. Сценарии: нормальный день; дыра вне `gaps.tsv`; дыра в двух смежных строках `gaps.tsv`; дубль, откат времени и откат seq; хвост при 4 значениях `--now` (текущий час, окно прошлого часа, после окна, закрытый час); мусор после фреймов; seq 0 с битыми колонками, битым JSON и несовпадением колонок; сессии по `connections.tsv` с проигнорированным входом; блоки без хэша и kind 13; файл только с недописанным фреймом |
| Битый конверт `{"messages":[{"sequenceNumber":103,"message":{}}]}` | новая версия: FAIL с вердиктом, `bad_envelopes 1`, остальной день посчитан. HEAD: `KeyError: 'message'`, traceback (воспроизведено ревью) |
| `hourly_metrics.py`: сэмпл с `--index` и 7 файлов `data/blocks/blocks-*.jsonl.zst`, HEAD и новая версия | 8 TSV побайтно равны; `hourly-metrics.tsv` побайтно равен `data/samples/hourly-metrics.tsv`. Целочисленная медиана: в 684 блоках из 1 392 чётное число пользовательских tx, результат совпал с прежним `int(st.median())` |
| `hourly_summary.py` на `data/samples` + `data/blocks`, 3.9.6 и 3.14 | stdout (283 строки), stderr и код выхода побайтно совпадают с HEAD |
| `hourly_sample.py`: дифференциальный прогон HEAD и новой версии на подставном RPC (`urllib.request.urlopen`, `time.time`/`time.sleep` подменены в процессе, синтетическая цепь; 48 часов) | 14 сценариев, все совпали по коду выхода, stdout, всем файлам (журнал вызовов, индекс, `.partial`, распакованный `.jsonl.zst`) и последовательности `sleep`. Сценарии: нормальный прогон; один 429; два 429 → стоп; 403 → стоп и затем продолжение; `--stop-after-hours 5` и продолжение; бюджет вызовов; бюджет поиска; несовпадение receipts; не-JSON; 2 сетевые ошибки и успех; 3 ошибки → стоп; дыры (окно 1 с); выход уже существует. Повторено под 3.14 |
| `--help` всех четырёх скриптов (3.9.6 и 3.14) | работает. Текст изменился только там, где дописан docstring: в `feed_audit.py` строка о проверке конвертов, в `hourly_metrics.py` строки о проверке receipts и целочисленной медиане, в `hourly_summary.py` абзац о фиксированных значениях отчёта 010 |
| Ничего не пишется в `data/` | `shasum` всех файлов `data/`, кроме `data/clickhouse` (28 файлов), до и после всех прогонов совпадает. Выходные файлы лежали в scratchpad и временных каталогах |
| `test-feed-audit-daily.sh` в `ubuntu:24.04 --network none` (`docker run --rm`) | 20 passed, 0 failed. Случаи на реальном `feed_audit.py` пропущены: в образе нет python3 и zstd. Контейнер удалён (`--rm`), образ `ubuntu:24.04` оставлен |
| То же на Mac (bash 3.2, python 3.9.6, zstd) | 23 passed, 0 failed: вместе с 4 случаями на реальном `feed_audit.py`. «Вчера по умолчанию» пропущен (BSD date). С `python3` → 3.14 тоже 23 passed |
| Тот же тест против HEAD-обёртки | 5 FAIL: traceback, «PASS в тексте при rc=1», «FAIL в тексте при rc=0», rc=2, rc=137. Тест действительно ловит разбор вердикта по тексту |
| shellcheck: `docker run --rm --network none koalaman/shellcheck:stable -x` по всем `deploy/*.sh deploy/test/*.sh` | 0 замечаний; `bash -n` ок; образ удалён (`docker rmi`) |
| `__pycache__` | созданные прогонами каталоги удалены |

## Предполагается / не проверено

- Под Python 3.14 на сервере новый `feed_audit.py` не запускался, только локально под 3.14 (Homebrew). Команда для проверки на сервере — ниже.
- Целочисленное деление в `utc()` может дать другую миллисекунду, чем прежний float-путь, если время прихода лежит в пределах ~0.5 мкс от границы миллисекунды. Новая версия точнее. На трёх реальных записях `session_list` совпал.
- Память `feed_audit.py` на сутки (~290 МБ по оценке ревью) не изменилась: структура `blocks` та же. Замеров нет.
- `deploy/test/run-systemd-container.sh` новый тест не вызывает. Реальный `feed_audit.py` через обёртку под настоящими python3 и zstd в Ubuntu не прогонялся, только на Mac.

## Что не сделано / вне объёма

- `--seed` для RPC-выборки (Р11) не добавлял: его нет в списке задачи, и он меняет вывод только при `--rpc-sample > 0`.
- ~~Строка теста в `deploy/README.md` и вызов из `run-systemd-container.sh`~~ — сделано в правках после ревью.
- `references/chain-facts.md` не менял: новых фактов о сети нет.

## Изменённые файлы

- `.claude/skills/feed-audit/scripts/feed_audit.py`, `.claude/skills/feed-audit/scripts/test_feed_audit.py` (новый), `.claude/skills/feed-audit/SKILL.md`
- `analytics/hoodlib.py` (новый), `analytics/hourly_metrics.py`, `analytics/hourly_summary.py`, `analytics/hourly_sample.py`, `analytics/README.md`
- `deploy/feed-audit-daily.sh`, `deploy/test/test-feed-audit-daily.sh` (новый, `chmod +x`)
- после ревью: `deploy/README.md`, `deploy/test/run-systemd-container.sh`
- `pyproject.toml` (per-file-ignore UP031)

## Выкладка на сервер

_Заполняется после выкладки: вывод bootstrap, результат пробного аудита, состояние recorder._

Команды для Михаила. Предполагается, что координатор уже закоммитил изменения и сделал rsync чистого клона в `/opt/hoodchain-mev/src` по README, раздел «Обновление только скриптов `deploy/`». recorder не перезапускается, bootstrap его не трогает.

```bash
# 0. До выкладки: зафиксировать состояние recorder (база: NRestarts=0, MainPID=7347,
#    ActiveEnterTimestamp=Thu 2026-10-01 09:57:12 UTC)
srv# systemctl show recorder -p NRestarts -p MainPID -p ActiveEnterTimestamp

# 1. Владелец исходников (rsync с Mac ставит uid Mac)
srv# chown -R hoodbuild:hoodbuild /opt/hoodchain-mev/src

# 2. Установка изменённых файлов. Ожидается:
#    CHANGED: file /opt/hoodchain-mev/deploy/feed-audit-daily.sh
#    CHANGED: file /opt/hoodchain-mev/deploy/feed_audit.py
#    CHANGED: file /opt/hoodchain-mev/deploy/README.md   (строка про новый тест, правки после ревью)
#    (плюс файлы параллельных задач, если они меняли deploy/), без "systemd: daemon-reload"
#    из-за 024 (юниты не менялись); в конце "done: N change(s), 0 warning(s)"
srv# bash /opt/hoodchain-mev/src/deploy/bootstrap.sh

# 3. Установлено то, что в исходниках
srv# cmp /opt/hoodchain-mev/src/.claude/skills/feed-audit/scripts/feed_audit.py /opt/hoodchain-mev/deploy/feed_audit.py \
       && cmp /opt/hoodchain-mev/src/deploy/feed-audit-daily.sh /opt/hoodchain-mev/deploy/feed-audit-daily.sh && echo same

# 4. Пробный аудит прошлых суток от hood, только в stdout: без отчёта в /srv/hood/reports
#    и без уведомления (обёртка не вызывается). Идёт долго: сутки ~2.2 ГБ zstd.
#    Норма: в конце "verdict            PASS", строка "bad_envelopes      0",
#    ни одной строки с "Warning", последняя строка "exit 0".
srv# runuser -u hood -- nice -n 10 python3 /opt/hoodchain-mev/deploy/feed_audit.py \
       --feed-root /srv/hood/data/feed --rpc-sample 0 --frame-secs 60 \
       "/srv/hood/data/feed/$(date -u -d yesterday +%Y/%m/%d)/feed-*.tsv.zst" 2>&1; echo "exit $?"

# 5. recorder не тронут: должно совпасть с шагом 0 (NRestarts=0, MainPID=7347,
#    ActiveEnterTimestamp=Thu 2026-10-01 09:57:12 UTC)
srv# systemctl show recorder -p NRestarts -p MainPID -p ActiveEnterTimestamp
```

Следующий плановый запуск `feed-audit.timer` в 00:10 UTC уже пойдёт через новую обёртку. Если что-то не так, её решение видно в отчёте `/srv/hood/reports/feed-audit-YYYYMMDD.txt` по последней строке `# exit N`.

## Правки после ревью (2026-10-02)

Оба ревью дали PASS (`docs/reviews/024-feed-audit-analytics-architect-reviewer.md`, `…-data-auditor.md`). По просьбе координатора исправлено:

1. **`RecursionError` при разборе JSON (data-auditor З1).** `json.loads` на строке с глубокой вложенностью бросает `RecursionError`, это подкласс `RuntimeError`. Раньше он попадал в `except RuntimeError` вокруг цикла файла: FAIL «zstd -dc failed … (corrupt frame?)», и остаток файла не считался. Исправление:
   - в `scan_line` и `classify_seq0` ловится `JSON_ERRORS = (ValueError, RecursionError)`; строка с seq ≠ 0 идёт в `bad_json`, строка с seq 0 — в `other`;
   - `decompress_lines` бросает свой `ZstdError(RuntimeError)`, и `scan_file` ловит только его.
2. **Зависание при неожиданном исключении (З2).**
   - Потоки подачи stdin и чтения stderr стали `daemon=True`.
   - В `decompress_lines` добавлен `try/finally`: если чтение прервано, `zstd` получает `kill()`, после этого закрывается stdout, потоки join'ятся и процесс reap'ится через `wait()`.
   - `scan_file` оборачивает генератор в `contextlib.closing`, так что `finally` выполняется сразу при исключении, а не при сборке мусора.
3. **`hourly_metrics.py --out` (З3).** Вывод пишется в `<out>.tmp.<pid>` рядом и переносится через `os.replace` только после успешного чтения всего входа. При любой ошибке временный файл удаляется, прежний `--out` не трогается.
4. **Р5 архитектора.**
   - `deploy/README.md`, раздел «Проверка набора локально»: добавлена строка `docker run … test-feed-audit-daily.sh` и упоминание в описании стенда.
   - `deploy/test/run-systemd-container.sh`: новый шаг запускает `test-feed-audit-daily.sh` от hood, после bootstrap, где есть python3 и zstd. Чтобы шаг проверял текущую версию, а не HEAD, в стенд копируется рабочая копия `.claude/skills/feed-audit/scripts`. Шапка скрипта обновлена.
   - Шапка `test-feed-audit-daily.sh`: «bash 3.2+».

Проверено (2026-10-02, Mac, без сети):

- Два новых теста в `test_feed_audit.py`, всего 24, ok под 3.9.6 и 3.14:
  - JSON `[`×200000 `]`×200000 в строке с блоком и в строке seq 0 → `failures == ["1 lines with broken JSON"]`, WARN `other`, остальные 29 блоков посчитаны;
  - `MemoryError` на 100-й строке файла из 20 000 строк в 10 фреймах, в подпроцессе с таймаутом → процесс завершается с traceback, без зависания.
- Мутационная проверка: на копии без исправлений оба теста падают. Тест на зависание падает по таймауту, если убрать и `closing`, и `kill`, и `daemon`.
- `feed_audit.py` на `data/feed`, `data/feed-test-009`, `data/feed-test-002` (текст и `--json`, 3.9 и 3.14, фиксированный `--now`): `diff -r` с выводом рабочего дерева до этих правок пустой. 13 синтетических сценариев против HEAD: отличается только строка `bad_envelopes`, как и раньше.
- `hourly_metrics.py`: 8 TSV побайтно равны выводу до правок, `hourly-metrics.tsv` равен `data/samples/hourly-metrics.tsv`, stderr тот же. `hourly_summary.py` на `data/samples`: побайтно тот же.
- Два сбойных входа: обрезанный `.jsonl.zst` и несовпадение receipts. Оба раза код 1, прежний `out.tsv` («old») не изменился, временного файла не осталось.
- ruff check и format (в том числе `ruff check .`): чисто. py_compile под 3.9.6: ок.
- `test-feed-audit-daily.sh`: 23/23 на Mac, 20/20 в `ubuntu:24.04 --network none --rm`.
- shellcheck в docker `--network none` по всем `deploy/*.sh deploy/test/*.sh`: 0 замечаний. Образ удалён. `bash -n` ок.
- Файлы `data/` (без `data/clickhouse`) не изменились: `shasum` совпадает.

Не проверено: `run-systemd-container.sh` целиком не запускал. Стенду нужна сеть (apt в bootstrap) и сборка образа. Новый шаг проверен только shellcheck и `bash -n`.

Попутное наблюдение (`crates/` вне объёма, не трогал): файлы `data/blocks/*.jsonl.zst` записаны без контрольной суммы zstd (`zstd -lv`: `Check: None`). Изменённый байт внутри фрейма `blocks-75640227-75640426.jsonl.zst` распаковался без ошибки, и все 200 строк разобрались. Порчу таких файлов `zstd -dc` не обнаружит. Стоит рассмотреть `--check`/checksum в enricher (задача для indexer-engineer, проверка — data-auditor).

## Вопросы к Cowork / Михаилу

1. ~~Добавить ли `test-feed-audit-daily.sh` в `deploy/README.md` и в `run-systemd-container.sh`?~~ Сделано в правках после ревью.
2. Обёртка присылает ALERT и при rc=2: например, за сутки нет ни одного файла, то есть recorder молчал весь день. Раньше было то же самое, только под заголовком «FAIL (нет вердикта)». Новый заголовок — «аудит не отработал (rc=2)». Подходит ли такая формулировка?

После этого изменения декодеров и загрузчиков нужен прогон data-auditor, по задаче — вместе с architect-reviewer: `hoodlib` и `hourly_*` читают сырьё. Ревьюеров я не запускал, по указанию координатора.
