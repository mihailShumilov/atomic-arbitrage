# 028 — Python и deploy: хвосты ревью (отчёт)

Дата: 2026-10-02. Исполнитель: indexer-engineer (Python) и infra-ops (deploy), одна сессия. База: HEAD `3b8e6a4`, изменения не закоммичены (коммит делает координатор). Статус задачи не менял, ревьюеров не запускал.

RPC, фид и ssh не использовались. На сервер ничего не выкладывалось.

## Сделано

### deploy

1. **`deploy/test/lib.sh`** — общая обвязка тестов. Функции:
   - `t_init` — временный каталог `T` с trap на удаление, `$T/bin` первым в `PATH`, `T_LOG`/`T_N`, счётчики;
   - `t_shim_logger`;
   - `t_fake_notify [--noisy]` — формат `LEVEL|TITLE|BODY`; `--noisy` — вариант для smartd-хука, который печатает в stdout и stderr;
   - `check DESC EXPR` — при FAIL вызывает `t_diag`, если тест его определил;
   - `t_result`.

   На неё переведены все шесть `test-*.sh`. В `test-healthcheck.sh` свой формат уведомлений (`LEVEL|TITLE` плюс отдельный файл тел) и свой `expect`: они нужны для подсчёта уведомлений и остались. Его `chk` и десять ручных блоков `if …; then pass…; else fail…; fi` заменены на `check`. Диагностика при FAIL сохранена через `t_diag`.
2. **`healthcheck.sh`: `write_state FILE CONTENT`** — запись во временный файл `FILE.tmp.$$` в том же каталоге, затем `mv -f`. Применено во всех шести местах: `.alert`, `gaps.offset` ×3, `raid_sync.keys` ×2. Содержимое файлов прежнее (`printf '%s\n'` = `echo`). Новое поведение при ошибке записи: временный файл удаляется, старый остаётся, в журнал пишется `cannot write state file …`, и `rc=1`. Раньше ошибка молча игнорировалась. Это нарочно: юнит упадёт, и `notify-failure@` сообщит об этом. В тест добавлены 2 проверки: `gaps.offset` заменён через rename (новый inode) и временных файлов не осталось.
3. **Список юнитов в одном месте.** Список теперь задают сами файлы `deploy/*.service` и `deploy/*.timer`:
   - `bootstrap.sh` ставит их через glob;
   - `run-systemd-container.sh` строит из того же glob список для `systemd-analyze verify`; `notify-failure@.service` превращается в `@x.service`;
   - стенд получил новую проверку «каждый юнит установлен как есть» (`cmp`).

   Оба жёстко прописанных списка в стенде удалены. Строка таблицы в README осталась описательной, это разрешено задачей («минимум скрипты»).
4. **README** (`deploy/README.md`): в разделе «Проверка набора локально» описаны `lib.sh` и правило «юниты = файлы», строка `test/` в таблице обновлена.

### Python

5. `analytics/hourly_summary.py`: колонки `*_wei` загружаются как `int` (пустая ячейка → `None`), остальные числовые — как `float`. Из wei-колонок используется только `base_fee_wei / 1e9`.
6. `analytics/hourly_metrics.py`: `metrics(line, hour_start=None, obj=None)`. `main` разбирает JSON один раз и передаёт объект. `hourly_summary.calibrate` вызывает `metrics(line)`, как раньше.
7. `analytics/hourly_sample.py`: над проверкой receipts добавлен комментарий: правило то же, что в `hoodlib.tx_receipt_pairs`, плюс `blockHash`, и запись в журнал до `Stop`. Код не менялся.
8. `test_feed_audit.py`: из `FrameLen` вынесены случаи без zstd в `PureFunctions.test_frame_len_without_zstd`: skippable-фрейм, обрезанный skippable, мусор, обрезанный magic (2 и 4 байта), смещение, `split_frames` на skippable и на мусоре. В `FrameLen` (skip без zstd) остались только случаи с настоящим фреймом. Добавлены тесты:
   - `test_rpc_pick_is_deterministic_for_a_seed` — без zstd;
   - `EndToEnd.test_rpc_seed` — `rpc_blocks` подменён, сети нет.

   Было 22 теста, стало 27.
9. `feed_audit.py`: `--seed N`. Выборка вынесена в чистую функцию `rpc_pick(blocks, sample, seed)` на `random.Random(seed)` вместо глобального генератора. Seed по умолчанию — `time.time_ns()`. Когда что печатается:
   - при `--rpc-sample > 0` всегда печатается `rpc.seed`;
   - при `--rpc-sample 0` seed печатается, только если задан (`rpc {'seed': N}`);
   - без `--seed` и без выборки остаётся `rpc {}`, как раньше.

   Попутно `check_rpc` больше не сортирует все блоки второй раз (`min(blocks)` вместо `sorted(blocks)[0]`). Скрипт по-прежнему один файл, только stdlib, py39 (`time` из stdlib). SKILL.md feed-audit обновлён: флаг и тесты.

## Проверено (2026-10-02, Mac и Docker, без сети, кроме оговорённого)

| Что | Результат |
|---|---|
| `uvx --offline ruff check .` / `ruff format --check .` (ruff 0.16.10, конфиг `pyproject.toml`) | `All checks passed!` / `33 files already formatted` |
| ruff по команде скилла без конфига (`--select E,F,W,B,UP,SIM,PL,RUF --line-length 120`) | 22 × PLR2004, как до задачи (правило выключено в `pyproject.toml` осознанно) |
| `py_compile` всех `.py` (3.9.6) | OK; `__pycache__` удалён |
| `unittest` feed-audit: 3.9.6; 3.14 с `-W error::DeprecationWarning` | 27 ok / 27 ok |
| То же без `zstd` в `PATH` | 9 ok, 18 skip. Без zstd выполняются разбор заголовков фреймов и детерминизм seed, раньше `FrameLen` пропускался целиком |
| `feed_audit.py` HEAD против рабочей копии: `data/feed`, `data/feed-test-002`, `data/feed-test-009`; текст и `--json`; 3.9.6 и 3.14 (`--rpc-sample 0 --now 2026-10-02T12:00:00Z`) | 12 пар: stdout, stderr и код выхода побайтно совпадают, у всех rc=0 |
| `hourly_summary.py` HEAD против рабочей копии: явные пути и пути по умолчанию; 3.9.6 и 3.14 | stdout (283 строки + rc) и stderr (пусто) побайтно совпадают |
| `hourly_metrics.py` HEAD против рабочей копии: сэмпл с `--index` и 7 файлов `data/blocks/blocks-*.jsonl.zst`; 3.9.6 и 3.14 | побайтно равны; на сэмпле ещё и равны `data/samples/hourly-metrics.tsv` |
| `data/` до и после прогонов (`shasum -a 256` всех файлов, кроме `data/clickhouse`) | без изменений |
| shellcheck 0.11.0 (`koalaman/shellcheck:stable`, `--network none`, `-x`, все `deploy/*.sh` и `deploy/test/*.sh`) | 0 замечаний, rc=0. `lib.sh` подключается через `# shellcheck source-path=SCRIPTDIR source=lib.sh`, SC1091 нет, то есть shellcheck файл проходит |
| `bash -n` всех скриптов | OK |
| Mac (bash 3.2): notify / mdadm-event / smartd-event / feed-audit-daily | 14 / 20 / 19 / 23 passed, 0 failed. Совпадает с HEAD-версиями тестов. Случай «вчера по умолчанию» — SKIP (BSD date), как раньше |
| `ubuntu:24.04 --network none`: healthcheck / notify / mdadm / smartd / feed-audit-daily | 121 / 14 / 20 / 19 / 20 passed, 0 failed. HEAD: 119 / 14 / 20 / 19 / 20; +2 — новые проверки атомарной записи |
| `ubuntu:26.04 --network none`, те же тесты | 121 / 14 / 20 / 19 / 20, 0 failed (uutils: `stat -c %i` работает) |
| Перекрёстно: старый `test-healthcheck.sh` с новым `healthcheck.sh` | 119 passed, поведение не изменилось |
| Перекрёстно: новый тест со старым `healthcheck.sh` | 1 FAIL: «gaps.offset … replaced by rename» — тест действительно ловит неатомарную запись |
| Сам `lib.sh` | `check` при FAIL вызывает `t_diag`; `t_result` возвращает 1 при FAIL; `fake-notify --noisy` при `FAKE_NOTIFY_FAIL=1` пишет в stderr и даёт rc 1; временный каталог удаляется при выходе |
| `run-systemd-container.sh` (24.04) | ALL PASS: bootstrap 47 изменений, затем 0; второй прогон не трогает файлы; все юниты установлены (`cmp`); verify по списку из файлов (10 юнитов); test-feed-audit-daily от hood с настоящим `feed_audit.py` — 24 passed; бэкап — 15 passed; recorder не запускался |
| `run-systemd-container.sh --ubuntu 26.04` | ALL PASS, те же цифры |
| Обновление на стенде 24.04 (`--keep`): `src` заменён на `git archive HEAD` → bootstrap → снова рабочая копия → bootstrap → ещё раз bootstrap | Переход на рабочую копию: `CHANGED` ровно 3 файла (`deploy/healthcheck.sh`, `deploy/feed_audit.py`, `deploy/README.md`), `daemon-reload` нет. Третий прогон — 0 изменений. recorder: `NRestarts=0`, inactive до и после. После `systemctl start healthcheck.service` (песочница юнита, `ReadWritePaths=/var/lib/hoodchain`) в `/var/lib/hoodchain/health` нет `*.tmp.*` |

Сеть у стенда systemd была: `apt` внутри контейнера и NTP у chrony, как и раньше. Порты не публиковались. Хосты фида, RPC и Telegram в контейнере направлены на 127.0.0.1. Офлайн-тесты и shellcheck шли с `--network none`.

Очистка (проверено `docker images`, `docker ps -a`):
- контейнеры стенда удалены;
- удалены образы `hood-deploy-test-systemd:24.04`, `:26.04`, скачанные `ubuntu:26.04` и `koalaman/shellcheck:stable`;
- `ubuntu:24.04` оставлен: он был до задачи.

Кэш сборки Docker (`docker builder`) не чистил: общий prune задел бы кэш других проектов на этой машине.

## Предполагается / не проверено

- **Что на сервере сейчас.** Предполагается, что `/opt/hoodchain-mev/deploy` соответствует deploy-части HEAD `3b8e6a4`. Тогда bootstrap даст 3 изменения от 028. Если 025 тоже меняет `deploy/` или сервер отстаёт, изменений будет больше. Это нормально, bootstrap идемпотентен.
- Путь ошибки `write_state` (полный диск) тестом не покрыт: в контейнере root обходит права, а pid временного файла заранее не известен. Проверено чтением кода. Успешный путь покрыт тестом и стендом.
- `hourly_sample.py` (RPC) не запускался: изменён только комментарий, `py_compile` OK.
- На сервере (Python 3.14, реальные сутки) новый `feed_audit.py` не запускался. Дефолтный вывод совпадает с HEAD на трёх записях под 3.14.7 локально.
- `--rpc-sample > 0` с настоящим RPC не запускался (сеть запрещена). Сквозной путь проверен с подменённым `rpc_blocks`.

## Команды для Михаила (после rsync чистого клона координатором; recorder эта часть не перезапускает)

```bash
srv# chown -R hoodbuild:hoodbuild /opt/hoodchain-mev/src
srv# systemctl show recorder -p NRestarts -p ActiveEnterTimestamp -p MainPID > /root/rec-before.txt
srv# bash /opt/hoodchain-mev/src/deploy/bootstrap.sh
#    ожидается от 028: CHANGED healthcheck.sh, feed_audit.py, README.md; без daemon-reload
#    (плюс то, что меняет в deploy/ задача 025, если меняет)
srv# bash /opt/hoodchain-mev/src/deploy/bootstrap.sh | tail -n 1      # done: 0 change(s)
srv# systemctl show recorder -p NRestarts -p ActiveEnterTimestamp -p MainPID | diff /root/rec-before.txt - && echo "recorder не тронут"
srv# systemctl start healthcheck.service && journalctl -u healthcheck.service -n 2 -o cat
srv# ls -a /var/lib/hoodchain/health                                  # нет файлов *.tmp.*
srv# python3 /opt/hoodchain-mev/deploy/feed_audit.py --help | grep -A1 -- --seed
# на следующее утро (после 00:10 UTC): отчёт аудита без изменений формата, rpc {}
srv# tail -n 5 /srv/hood/reports/feed-audit-$(date -u -d yesterday +%Y%m%d).txt
```

Перезапуск recorder, если он нужен, относится к деплою 025 и делается по его командам.

## Вопросы

Нет. Замечание к ревью: `write_state` при ошибке записи теперь ставит `rc=1`, раньше ошибка игнорировалась. Это сознательное изменение поведения, архитектору стоит его подтвердить.

## Проверка

После ревью нужен прогон data-auditor (по задаче: data-auditor и architect-reviewer). Сам я их не запускал.

## Правки после ревью

По `docs/reviews/028-python-deploy-followups-architect-reviewer.md` (вердикт PASS, рекомендации Р1–Р4) правки сделаны 2026-10-03. Все правки минимальные, вывод не меняется.

- **Р1.** `feed_audit.py`: `import time` перенесён после `import threading`. `ruff check --select I` чистый.
- **Р2.**
  - Шапка `bootstrap.sh`: перечень юнитов по именам убран. Осталось «every deploy/*.service and deploy/*.timer (the files are the list; which timers get enabled is the explicit list in the `timers` section below)».
  - `deploy/README.md`, раздел «Проверка набора локально»: новый юнит bootstrap установит, а стенд проверит. Включение таймеров задаётся отдельным явным списком в разделе `timers` bootstrap.sh: сейчас это `healthcheck.timer` и `feed-audit.timer`, а `backup` и `enricher-gaps` выключены намеренно.
- **Р3.** `deploy/README.md`, раздел «Мониторинг», добавлено одно предложение: файлы состояния пишутся атомарно. При ошибке записи в журнале появляется `cannot write state file …`, юнит завершается с ошибкой, приходит уведомление через `notify-failure@`. Уже отправленное уведомление может повториться.
- **Р4.** `hourly_summary.py`: комментарий о том, что пустая wei-ячейка становится `None` и арифметика с ней падает громко, в отличие от `NaN`.

Проверено 2026-10-03:
- `uvx --offline ruff check .` — `All checks passed!`; `ruff format --check .` — 33 файла в порядке;
- `test_feed_audit.py` — 27 ok;
- shellcheck 0.11.0 (`--network none`): 0 замечаний, образ удалён;
- `bash -n` всех скриптов — OK;
- `feed_audit.py` (`data/feed`, `data/feed-test-009`) и `hourly_summary.py` побайтно совпадают с HEAD;
- в `bootstrap.sh` изменён только комментарий, число строк справки (`-h`, строки 2–35) прежнее;
- стенд `run-systemd-container.sh` на 24.04: ALL PASS (47 изменений, затем 0), образ стенда удалён. Остался только `ubuntu:24.04`, он был и до задачи.

Команды для Михаила не меняются: bootstrap.sh на сервер не копируется, README копируется (он уже в числе трёх ожидаемых изменений).

### По замечанию data-auditor З1 (2026-10-03)

Источник — `docs/reviews/028-python-deploy-followups-data-auditor.md`.

`healthcheck.sh` теперь пишет строки `alert sent: …`, `gaps: notified …`, `gaps: initialised offset …` и `raid: sync notified …` только после успешного `write_state` (`write_state … && log …`). При ошибке записи поведение прежнее: `cannot write state file …` и `rc=1`. Уведомление в этом случае повторится на следующем запуске («хотя бы один раз»).

В `test-healthcheck.sh` добавлен раздел 18, путь ошибки — подход аудитора. Подставной notifier при `FAKE_NOTIFY_LOCK_STATE=1` после отправки делает каталог состояния read-only. Раздел проверяет:
- оба уведомления (новая дыра и ALERT по диску) ушли;
- `rc=1`, в журнале `cannot write state file` для `gaps.offset` и `disk.alert`;
- строк `gaps: notified` и `alert sent` нет;
- старый offset сохранён, временных файлов нет;
- после восстановления прав оба уведомления отправлены повторно и записаны, `rc=0`, повтора нет.

`run()` теперь сохраняет код выхода в `hc_rc`. Раздел работает только не под root (root игнорирует права на каталог): `docker run --user 1000:1000`. Под root он печатает SKIP.

Проверено 2026-10-03, `ubuntu:24.04 --network none --rm`:
- под root: 121 passed, раздел 18 — SKIP;
- под `--user 1000:1000`: 129 passed, 0 failed;
- копия с прежним порядком (`write_state; log`) под uid 1000: 1 FAIL «no 'gaps: notified' / 'alert sent' line», то есть тест различает поведение;
- shellcheck 0.11.0 (`--network none`): 0 замечаний, образ удалён; `bash -n` — OK.

В `deploy/README.md`, раздел «Проверка набора локально», команду с `--user 1000:1000` не добавлял. Её стоит добавить при следующей правке README:

```
docker run --rm --network none --user 1000:1000 -v "$PWD/deploy":/deploy:ro ubuntu:24.04 bash /deploy/test/test-healthcheck.sh
```

Команды для Михаила не меняются: `healthcheck.sh` уже в числе трёх ожидаемых изменений bootstrap.

## Файлы

- `deploy/test/lib.sh` (новый); `deploy/test/test-{healthcheck,notify,mdadm-event,smartd-event,backup,feed-audit-daily}.sh`; `deploy/test/run-systemd-container.sh`
- `deploy/healthcheck.sh`, `deploy/bootstrap.sh`, `deploy/README.md`
- `analytics/hourly_summary.py`, `analytics/hourly_metrics.py`, `analytics/hourly_sample.py`
- `.claude/skills/feed-audit/scripts/feed_audit.py`, `.claude/skills/feed-audit/scripts/test_feed_audit.py`, `.claude/skills/feed-audit/SKILL.md`
