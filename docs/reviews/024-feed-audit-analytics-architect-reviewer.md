# 024 — архитектурное ревью (architect-reviewer)

Дата: 2026-10-02. Объём: незакоммиченное рабочее дерево против HEAD `449576f`, только файлы задачи 024:
- `.claude/skills/feed-audit/scripts/feed_audit.py`, `test_feed_audit.py` (новый), `.claude/skills/feed-audit/SKILL.md`;
- `analytics/hoodlib.py` (новый), `hourly_metrics.py`, `hourly_summary.py`, `hourly_sample.py`, `README.md`;
- `deploy/feed-audit-daily.sh`, `deploy/test/test-feed-audit-daily.sh` (новый);
- `pyproject.toml`.

Контекст: задача `docs/handoff/to-code/024-feed-audit-analytics.md`, отчёт `docs/handoff/from-code/024-feed-audit-analytics.md`, исходные замечания `docs/reviews/full-2026-10-02-python-bash-sql-architect-reviewer.md` (В3–В8, Р12–Р15).

**Вердикт: PASS.** Блокирующих и важных замечаний нет. Рекомендаций — 8, ни одна не мешает закрытию задачи и выкладке.

## Линт: вывод команд (проверено 2026-10-02 на Mac, без сети, кроме загрузки ruff через uvx и образа shellcheck)

| Команда | Результат |
|---|---|
| `uvx ruff check .` (ruff 0.16.10, конфиг `pyproject.toml`) | `All checks passed!` |
| `uvx ruff format --check .` | `33 files already formatted` |
| `uvx ruff check --select E,F,W,B,UP,SIM,PL,RUF --line-length 120 analytics .claude/skills/feed-audit/scripts` (команда скилла, без конфига игноров) | 22 × PLR2004. Правило осознанно выключено в `pyproject.toml` с пояснением (пороги и индексы колонок в отчётных скриптах). Других замечаний нет |
| `python3 -m py_compile analytics/*.py .claude/skills/feed-audit/scripts/*.py` (3.9.6) | OK |
| `python3 .claude/skills/feed-audit/scripts/test_feed_audit.py` (3.9.6) | `Ran 22 tests … OK` |
| То же под `/opt/homebrew/bin/python3.14 -W error::DeprecationWarning` | `Ran 22 tests … OK` |
| `docker run --rm --network none … koalaman/shellcheck:stable -x deploy/feed-audit-daily.sh deploy/test/test-feed-audit-daily.sh` (ShellCheck 0.11.0) | 0 замечаний, rc=0. Образ удалён (`docker rmi`) |
| `bash -n` обоих скриптов | OK |
| `bash deploy/test/test-feed-audit-daily.sh` (Mac, bash 3.2, python 3.9.6, zstd) | `23 passed, 0 failed`; «вчера по умолчанию» — SKIP (BSD date) |

Дополнительно, дифференциальные прогоны (проверено 2026-10-02, вывод в scratchpad, в `data/` ничего не писалось):
- `hourly_summary.py` HEAD против рабочей копии на `data/samples` + `data/blocks`: stdout (283 строки) побайтно совпадает, оба rc=0;
- `feed_audit.py` HEAD против рабочей копии на `data/feed-test-009` (`--rpc-sample 0 --now 2026-10-02T12:00:00Z`): оба rc=0; единственное отличие — новая строка `bad_envelopes      0`.

Созданный прогоном тестов `__pycache__` удалён. cargo не запускался (по заданию).

## Блокирующее

Нет.

## Важное

Нет. Все восемь важных замечаний исходного ревью, относящихся к объёму 024 (В3–В8), закрыты; проверка по пунктам — ниже, в «Что проверено по замечаниям».

## Рекомендации

**Р1. `feed_audit.py:384-385`: в `Tally` лежит курсор сканера (`cur`, `prev_env_ns`).**
Что не так: `Tally` задуман как накопитель итогов («Everything the scan accumulates»), а `cur` и `prev_env_ns` — рабочее состояние `Scanner.track_session`/`scan_envelope`, в сводку они не попадают. Почему это стоит поправить: при следующей правке сессий легко начать читать `t.cur` в `build_summary` как итог. Исправление: перенести их в `Scanner.__init__` (`self.cur = None; self.prev_env_ns = None`), в `track_session`/`scan_envelope` писать `self.cur`. Поведение не меняется, тесты те же. `fails`/`warns` в `Tally` оставить: их пополняют и сканер, и `check_*`.

**Р2. `hourly_summary.py:112-115`: колонки `*_wei` по-прежнему переводятся во `float` (остаток Р12).**
Сейчас из них используется только `base_fee_wei / 1e9` для отображения, поэтому результат не меняется. Но правило python.md («wei — int») нарушено, и следующий расчёт по `fee_sum_wei` молча потеряет точность. Исправление:
```python
for k in r:
    if k in ("hour_start_utc", "date", "weekday"):
        continue
    if k.endswith("_wei"):
        r[k] = int(r[k]) if r[k] != "" else None
    else:
        r[k] = float(r[k]) if r[k] != "" else NAN
```
Вывод останется прежним (`bf_gwei` всё равно делится на `1e9`). Это стоит подтвердить тем же побайтным сравнением stdout.

**Р3. `hourly_metrics.py:155-156`: каждая строка разбирается JSON-ом дважды** (`json.loads(line)["number"]` в `main`, затем снова в `metrics`). На строку блока это лишние ~десятки КБ разбора; для сэмпла и окон 006 неважно, для будущих прогонов по `data/blocks` за сутки — заметно. Исправление: `def metrics(line, hour_start=None, obj=None)`, где `o = obj if obj is not None else json.loads(line)`; в `main` передавать уже разобранный объект. `calibrate` в `hourly_summary` вызывает `metrics(line)` как раньше.

**Р4. `hourly_sample.py:276-281`: проверка receipts частично повторяет `hoodlib.tx_receipt_pairs`.**
Дубль оправдан: здесь проверяется ещё и `blockHash`, а при несовпадении сначала пишется журнал вызовов, потом `Stop`. Скрипт RPC-шный и намеренно не тянет `hoodlib`. Исправление — одна строка комментария над проверкой: `# same rule as hoodlib.tx_receipt_pairs (count, txHash) plus blockHash; logged to the ledger before Stop`. Тогда при изменении правила в `hoodlib` будет видно, что нужно поправить и здесь.

**Р5. `deploy/test/test-feed-audit-daily.sh` не подключён к штатным прогонам, а в шапке неверно указана версия bash.**
- Стр. 13: написано «bash 4+», а тест проходит на bash 3.2 (Mac; проверено выше). Исправить на «bash 3.2+».
- Тест не вызывают ни `deploy/README.md` (раздел «Проверка набора локально», стр. ~361-364), ни `deploy/test/run-systemd-container.sh`. Без этого он не будет запускаться при правках `deploy/` и устареет. Исправление: в README добавить строку `docker run --rm --network none -v "$PWD/deploy":/deploy:ro ubuntu:24.04 bash /deploy/test/test-feed-audit-daily.sh` (там сработают 20 случаев на подставном python3). В `run-systemd-container.sh` добавить шаг по образцу стр. 147-148: `ok "test-feed-audit-daily.sh" "test-feed-audit-daily.sh" x 'bash /opt/hoodchain-mev/src/deploy/test/test-feed-audit-daily.sh | tail -n 1; exit "${PIPESTATUS[0]}"'`. Там есть python3 и zstd, так что заработают и 4 случая на реальном `feed_audit.py`, если в `src` скопирован `.claude/`. Если не скопирован — они честно напечатают SKIP. Это вопрос 1 отчёта исполнителя; правка мелкая, её можно сделать в коммите 024 или в ближайшей задаче `deploy/`.

**Р6. `deploy/test/test-feed-audit-daily.sh:36-49`: шестая копия обвязки (`fake-notify`, `pass/fail`, `check()`, `result:`).**
Копия дословно повторяет `test-mdadm-event.sh:22-44`, только диагностика другая (`out:` вместо `log:`). Новой проблемы здесь нет: это стиль соседних тестов, а вынос в `deploy/test/lib.sh` уже записан как Р5 полного ревью и в объём 024 не входил. Отмечаю, что с этим файлом выигрыш от `lib.sh` вырос: `check DESC EXPR [DIAG_FILE...]` покрывает все пять вариантов.

**Р7. `test_feed_audit.py:174-187`: класс `FrameLen` целиком пропускается без `zstd`, хотя половине проверок он не нужен.**
Skippable-фрейм, мусор и обрезанный magic строятся из байтов. Исправление: вынести эти три утверждения в `PureFunctions.test_frame_len_without_zstd`, а в `FrameLen` оставить случаи с настоящим фреймом. Тогда разбор заголовка проверяется на любой машине.

**Р8. Выборка RPC в `feed_audit.check_rpc` (стр. 566, 568) всё ещё без seed (Р11 полного ревью).**
В объём 024 не входило, исполнитель это явно отметил. Оставить в очереди: при `--rpc-sample > 0` повторить выборку нельзя. Исправление прежнее: `--seed` (по умолчанию `time.time_ns()`), `random.Random(seed)`, `rpc.seed` в сводке. Ключ сводки новый — это изменение формата вывода, поэтому стоит делать отдельной задачей.

## Дубли

| Где | Состояние после 024 | Решение |
|---|---|---|
| user-tx и комиссия: были `hourly_metrics` + 2 места в `hourly_summary` | одно определение: `hoodlib.tx_receipt_pairs`/`user_tx_pairs`/`fee_wei`/`gas_used_for_l1`. Копии в summary теперь тоже проверяют число receipts и хэши (grep 2026-10-02: `effectiveGasPrice`, `transactionHash`, `0x6a` в коде есть только в `hoodlib.py` и в RPC-проверке `hourly_sample`) | закрыто |
| `zstd -dc` целиком в память, 3 места | одно потоковое `hoodlib.iter_block_lines`; оставшиеся вызовы `zstd` — сжатие (`hourly_metrics.zlen`, `hourly_sample.compress_atomically`), это другие операции | закрыто |
| `sys.path.insert` в `hourly_summary` | убран: импорт `hoodlib`/`hourly_metrics` из каталога скрипта | закрыто |
| `pct`: интерполяция (`hourly_summary.pct`) и ближайший ранг (`feed_audit.pct`) | разные скрипты, docstring у обоих называет метод | оставить |
| проверка receipts: `hoodlib` и `hourly_sample.Sampler.receipts` | правило одно, у sample добавлены `blockHash` и запись в журнал | оставить, комментарий (Р4) |
| разбор фида и RPC: recorder (Rust) и `feed_audit.py` | осознанный дубль, источник истины записан в SKILL.md feed-audit (стр. 10), в `analytics/README.md` и в docstring `hoodlib` | закрыто как требовало В7 |
| обвязка `deploy/test/test-*.sh` | +1 копия, не разошлась | Р5 полного ревью (`lib.sh`), см. Р6 |
| форматирование UTC-времени (`hourly_sample.iso`, `hourly_metrics`, `hourly_summary`) | три коротких `fromtimestamp(…, timezone.utc)` с разными форматами | оставить: выигрыша от выноса нет |

## Структура и разбиение

Изменения не нужны.
- `feed_audit.py` — 767 строк, один файл stdlib (требование задачи: ставится одним файлом).
  - Деление по смыслу: `Clock` (frozen, только время), `Tally` (итоги), `Scanner` (проход по файлам и строкам), чистые функции разбора (`frame_len`, `split_frames`, `parse_messages`, `classify_seq0`, `merge_ranges`), `check_*` (проверки по итогам), `build_summary`/`print_*` (вывод), `main` на 35 строк.
  - Самая длинная функция — `build_summary` (~60 строк, словарь сводки); дробить её незачем.
  - `Scanner` — уместный класс (состояние + поведение). `Clock` как frozen dataclass с фабрикой `make` легко подменять в тестах.
  - Совместимость с py39 соблюдена: `from __future__ import annotations` (поэтому `int | None` в аннотациях полей безопасен), без `slots=`, `match`, `datetime.UTC`, `zip(strict=)`.
- `hoodlib.py` — 86 строк, функции и константы, без классов, как и предлагало ревью. `Rpc` справедливо не вынесен: второго RPC-скрипта нет.
- `hourly_summary.py` — 520 строк. `main` стал списком шагов. Строки — словари из TSV: для разового отчёта это нормально, переводить их в dataclass ради ~30 колонок не стоит. Агрегаты (`Sample`, `Calibration`, `RawSample`, `Rates`) — dataclass'ы. `print_block_counts` и печатает, и возвращает `Rates`; для разового отчёта это приемлемо.
- `hourly_sample.py` — `Rpc` (контекстный менеджер) и `Sampler` (состояние предсказателя) — классы по делу, остальное — функции.

## Что проверено по замечаниям

- **Время с часовым поясом (В3).** `UTC = datetime.timezone.utc` (стр. 67). Aware все три места: `hour_start` (стр. 105), `parse_now` (стр. 110), `datetime.now(UTC)` (стр. 737). `utc()` делит ns целочисленно (стр. 322-323). `utcnow`/`utcfromtimestamp` в Python-файлах не встречаются (grep). Тесты `test_times_are_aware` и `test_default_clock_without_now_has_no_deprecation_warning` гоняют реальные часы при `DeprecationWarning` = error; под 3.14 зелёные.
- **Битый конверт (В5).** `parse_messages` (стр. 279-300) отклоняет:
  - не-объект и `messages` не список;
  - отсутствующий ключ, `None`/строку/список на месте словаря (`KeyError`/`TypeError`/`AttributeError`);
  - нецелые значения, `bool` в том числе.

  Счётчик `bad_envelopes` даёт FAIL в `check_counts`. Счётчики проверяются и до ветки «no blocks found», поэтому FAIL печатается и там. Тесты покрывают 10 форм битого конверта, сквозной случай «битый конверт посреди дня» (остальной день посчитан, seq стал дырой) и случай «только битые конверты». Уточнение, а не замечание: если битый конверт последний в файле, его seq не превращается в дыру, потому что за ним ничего нет. FAIL всё равно есть по счётчику, так что тихого пропуска не будет.
- **Тесты осмысленны (В4).** Проверяются конкретные строки FAIL/WARN, числа сводки и ключи текстового вывода, которые читает обёртка (`test_text_output_keys`), а не только код выхода. Всё в `tempfile`, без сети и `sleep`. Время задаётся через `--now`, кроме одного намеренного теста с реальными часами, а он от часов не зависит: в нём нет хвостов. Хрупких мест не нашёл.
- **Обёртка (В8).**
  - Решение принимается по `audit_rc`. Его присваивание внутри `{ … } > "$tmp"` корректно: это группа, а не подсубшелл.
  - Таблица 0 / 1+verdict / 1 без verdict / прочее совпадает с шапкой и SKILL.md.
  - Комментарий о том, почему нет `set -e`, добавлен.
  - `SuccessExitStatus=3` в юните не трогали, контракт кодов выхода прежний.
  - Тест доказывает, что решение принимается по коду: случаи «PASS в тексте, rc=1» и «FAIL в тексте, rc=0».
- **ruff (В6, п. 4 задачи).**
  - Per-file-ignore `UP031` для `feed_audit.py` обоснован в комментарии: вывод сверяется побайтно, и копия работает на сервере.
  - `noqa: PLR0911, PLR0912` на `frame_len` обоснован: побайтный разбор заголовка RFC 8878, комментарий над функцией.
  - `noqa: PLR0913, PLR0917` на `Rpc.log` обоснован: один аргумент на колонку журнала.
  - `noqa: SIM115` на журнале обоснован: файл живёт весь прогон и закрывается в `close`/`__exit__`.

  Других подавлений нет (grep `noqa`).
- **int wei.** В `hoodlib` и `hourly_metrics` wei везде `int`. `median_int` целочисленная. Исключение — загрузка TSV в `hourly_summary` (Р2).

## Что хорошо (не сломать при правках)

- `feed_audit.py` остался одним stdlib-файлом под 3.9, а `main(argv)` вызывается из тестов. Именно это позволило покрыть сквозные сценарии без подпроцесса.
- `parse_messages` — чистая функция с явным контрактом «список или None», и отказ от traceback не скрывает проблему: битый конверт — это FAIL, а не WARN.
- В обёртке код выхода — сигнал для машины, а строка `verdict` — для человека. Тест ловит возврат к разбору текста: против HEAD-обёртки он даёт 5 FAIL (по отчёту исполнителя).
- `hoodlib.iter_block_lines` потоковый и закрывает процесс через `with Popen`. Ошибка `zstd` превращается в исключение, а не в обрезанные данные.
- Дифференциальная проверка исполнителя (HEAD против новой версии на реальных и синтетических данных) воспроизводится; я повторил её для `hourly_summary` и `feed-test-009`.

## Предполагается / не проверено

- На сервере (Python 3.14 Ubuntu, реальные сутки ~2.2 ГБ) новый `feed_audit.py` и обёртку я не запускал: ssh не использовался. Это шаг выкладки из отчёта исполнителя.
- Дифференциальные прогоны `feed_audit.py` на `data/feed` и `data/feed-test-002`, `hourly_metrics.py` и `hourly_sample.py` на подставном RPC я не повторял — взято из отчёта исполнителя. Сам повторил только `hourly_summary.py` и `feed_audit.py` на `feed-test-009`.
- Тест обёртки в `ubuntu:24.04` не запускал (только на Mac); по отчёту исполнителя там 20 passed.
- Копирует ли `run-systemd-container.sh` каталог `.claude/` в `src` (от этого зависит, заработают ли в Р5 случаи на реальном `feed_audit.py`), не проверял.
- Память `feed_audit.py` на сутки не замерял; структура `blocks` не менялась.
