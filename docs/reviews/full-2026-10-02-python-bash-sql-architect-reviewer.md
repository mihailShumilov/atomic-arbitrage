# Полное ревью, часть 4 из 4: Python, Bash и systemd, SQL. Архитектурное ревью (architect-reviewer)

Дата: 2026-10-02. HEAD `d241243`, ревьюилось всё дерево, а не дифф.
Объём:
- `analytics/hourly_{metrics,sample,summary}.py`;
- `.claude/skills/feed-audit/scripts/feed_audit.py`;
- `deploy/*.sh`, `deploy/*.service`, `deploy/*.timer`;
- `deploy/test/*`;
- `sql/001_schema.sql`, `sql/002_funding_edges_l1.sql`;
- `docker-compose.yml`.

Набор `deploy/` работает на сервере: у каждой правки, которая требует обновить сервер, стоит пометка **[сервер]**. Процедура обновления — README, раздел «Обновление только скриптов `deploy/`»: повторный запуск `bootstrap.sh`, recorder при этом не перезапускается.

**Вердикт: PASS с замечаниями.** Блокирующих замечаний нет. Важных — 8, рекомендаций — 15.

## Линт: вывод команд (проверено 2026-10-02 на Mac, команды из скилла)

| Команда | Результат |
|---|---|
| `uvx ruff check --select E,F,W,B,UP,SIM,PL,RUF --line-length 120 analytics .claude/skills/feed-audit/scripts` | **131 замечание**: feed_audit.py 57, hourly_summary.py 51, hourly_sample.py 21, hourly_metrics.py 2 |
| `uvx ruff format --check --line-length 120 …` | **4 файла из 5 не отформатированы** (только hourly_metrics.py в порядке) |
| `python3 -m py_compile` (3.9.6) | OK |
| `shellcheck -x deploy/*.sh deploy/test/*.sh` (docker `--network none`, образ удалён) | **0 замечаний** |
| `bash -n deploy/*.sh deploy/test/*.sh` | OK |
| `npx jscpd --min-lines 8 --format bash,python` | 1 клон: `test-mdadm-event.sh:13-24` ≈ `test-smartd-event.sh:19-30`, 12 строк. Python — 0 клонов; дубли там смысловые, см. «Дубли» |

Сводка ruff по правилам:

| Правило | Число |
|---|---|
| E501 | 33 |
| UP031 (`%`-формат, почти всё в feed_audit) | 32 |
| PLR2004 | 23 |
| SIM115 (open без `with`) | 8 |
| B905 (`zip` без `strict`) | 7 |
| PLR0912 | 4 |
| PLR0915 | 3 |
| B904 | 3 |
| E741, PLR0911, RUF100, F841 | по 2 |
| B023, B007, PLW2901, SIM105, PLR0913, PLR0917, RUF046, RUF002, RUF003, UP028 | по 1 |

Значимые находки ruff:
- F841 `hourly_summary.py:247-248`: мёртвые `t_first`/`t_last`;
- PLR0915/PLR0912 у трёх функций `main()`: feed_audit — 223 оператора и 64 ветвления, hourly_summary — 182/28, hourly_sample — 132/26;
- RUF002/RUF003: кириллическая «Р» в `feed_audit.py:81,318` («Remark Р1»).

B023 в `hourly_summary.py:146` — ложное срабатывание: лямбда вызывается сразу. B905 в feed_audit нельзя исправить через `strict=`: скрипт заявлен для Python 3.9+, а `strict` появился в 3.10.

Дополнительно, проверено 2026-10-02 на синтетическом фиде в scratchpad, Python 3.14.7, как на сервере (README: Python 3.14):
- `feed_audit.py` печатает две `DeprecationWarning`, на `utcnow` (стр. 315) и `utcfromtimestamp` (стр. 278);
- конверт без `message.message.header` роняет скрипт с `KeyError` (см. В3, В5).

## Блокирующее

Нет.

## Важное

**В1. `sql/001_schema.sql:91` + `sql/002_funding_edges_l1.sql`: ключ `funding_edges` не однозначен, ReplacingMergeTree тихо схлопнет разные рёбра.**

Что не так. `ORDER BY (to_addr, block_number, tx_index)` не содержит ни `kind`, ни `log_index`. Схлопнутся:
- (а) два `DepositFinalized` одному получателю в одной tx. Это ограничение уже записано в data-model.md;
- (б) **рёбра разных видов в одной tx.** У `0x64`/`0x68` есть `tx.value > 0` и `to` = получатель. Будущий общий экстрактор `eth` (по `tx.value`) даст строку с тем же ключом, что и `l1_eth`, и одна из них молча исчезнет;
- (в) несколько WETH-`Transfer` одному адресу в одной tx (`kind='weth'`). Для роутеров это обычное дело.

`log_index` в 002 объявлен `Nullable(UInt32)`, поэтому в ключ его не добавить без `allow_nullable_key`. Ключ сортировки ClickHouse не меняет через ALTER: дописать в конец можно только колонку, добавленную в том же ALTER.

Почему исправлять сейчас. Проверено 2026-10-02: в локальном ClickHouse у таблицы `funding_edges` нет ни одного part. В `data/clickhouse/store/f8c/f8c2d276-…/` есть только `detached/` и `format_version.txt`, то есть таблица пуста. Пересоздание сейчас ничего не стоит. Когда появится загрузчик, это будет тихая потеря данных, то есть блокирующее замечание.

Исправление — миграция `sql/003_funding_edges_key.sql`, только вперёд. Набросок:

```sql
CREATE TABLE IF NOT EXISTS hood.funding_edges_v2 (
    block_number UInt64, tx_index UInt32,
    log_index    UInt32 DEFAULT 4294967295,   -- tx-level rows (eth, l1_eth): sentinel, not NULL
    kind         Enum8('eth'=1,'weth'=2,'internal'=3,'l1_eth'=4,'l1_token'=5),
    from_addr String, to_addr String, value_wei String,
    tx_hash String DEFAULT '', token String DEFAULT '', l1_token String DEFAULT '',
    gateway String DEFAULT '', gateway_status Enum8('none'=0,'observed'=1,'verified'=2) DEFAULT 'none',
    l2_alias String DEFAULT '', tx_type UInt8 DEFAULT 0,
    l1_request_id String DEFAULT '', ticket_id String DEFAULT ''
) ENGINE = ReplacingMergeTree
ORDER BY (to_addr, block_number, tx_index, kind, log_index);
-- table is empty (checked 2026-10-02): swap names, no data copy needed
EXCHANGE TABLES hood.funding_edges AND hood.funding_edges_v2;
DROP TABLE IF EXISTS hood.funding_edges_v2;
```

`internal` (трассировки) тоже нужен свой дискриминатор: `trace_address` или порядковый номер вызова. Его стоит добавить заранее или договориться о правиле в задаче загрузчика. В Rust: `FundingEdge.log_index: Option<u32>` → `u32` с тем же значением-заглушкой при маппинге в строку (`crates/decoders/src/l1_inflows.rs:359`). В data-model.md заменить фразу «Ограничение ключа…» новым ключом.

Пометки: меняет схему (задача для indexer-engineer, затем data-auditor). Колонка `log_index` из `Nullable` становится не-Nullable — **ломает совместимость** со схемой 002, но данных нет. Сервер не затрагивает: ClickHouse только локальный (STATE.md: docker на 127.0.0.1:18123).

**В2. `docker-compose.yml:14` + `sql/002_funding_edges_l1.sql:2`: миграции применяются только на пустом томе, 002 локально не применена.**

Что не так. `./sql` смонтирован в `/docker-entrypoint-initdb.d`, а этот каталог исполняется только при первом старте с пустым `data/clickhouse`.

Проверено 2026-10-02 по `data/clickhouse/store/.../funding_edges.sql` (через `find`): у локальной таблицы 6 колонок из 001, enum без `l1_eth`/`l1_token`, колонок 002 нет. Комментарий 002 («Applied after 001 by docker-entrypoint-initdb») для существующего тома неверен. Журнала применённых миграций нет, поэтому загрузчик упадёт на `INSERT` с колонками 002 или, хуже, пойдёт по неверной схеме.

Почему это важно. Это нарушает правило data-model «изменения схемы — новым файлом миграции»: файл есть, но механизма применения нет.

Исправление — минимальное, без новых зависимостей. Файл `sql/apply.sh`:

```bash
#!/usr/bin/env bash
# Apply every sql/NNN_*.sql in order. Each file must be idempotent (IF NOT EXISTS, superset enums).
set -euo pipefail
cd "$(dirname "$0")"
for f in [0-9][0-9][0-9]_*.sql; do
    echo "applying $f"
    docker compose exec -T clickhouse clickhouse-client --user hood --password "$CLICKHOUSE_PASSWORD" --multiquery < "$f"
done
```

Плюс правило в data-model.md: «каждая миграция идемпотентна». 001 и 002 уже идемпотентны: `CREATE … IF NOT EXISTS`; `MODIFY COLUMN` на надмножество enum; `ADD COLUMN IF NOT EXISTS`. Таблица `hood.schema_migrations` — только когда появится неидемпотентная миграция, раньше не нужна. Комментарий в 002, стр. 2, поправить.

**В3. `feed_audit.py:278, 315`: `utcfromtimestamp`/`utcnow` устарели, предупреждения попадают в ежедневные отчёты на сервере.**

Почему это важно. Проверено 2026-10-02 на Python 3.14.7: обе `DeprecationWarning` печатаются в stderr, потому что код выполняется в модуле `__main__`. `feed-audit-daily.sh:38` пишет `2>&1` в `feed-audit-YYYYMMDD.txt`. В 3.14 это шум в каждом отчёте. В будущей версии эти функции удалят, и аудит упадёт без вердикта.

Исправление. Стоит сохранить совместимость с 3.9: `datetime.UTC` есть только с 3.11, `timezone.utc` есть и в 3.9.

```python
UTC = datetime.timezone.utc
def utc(ns):
    return datetime.datetime.fromtimestamp(ns // 1_000_000_000, UTC).strftime("%Y-%m-%dT%H:%M:%S") \
        + ".%02dZ" % (ns % 1_000_000_000 // 10_000_000)
def hour_start(hour):
    return datetime.datetime.strptime(hour, "%Y%m%d-%H").replace(tzinfo=UTC)
# --now:   datetime.datetime.strptime(a.now, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=UTC)
# default: datetime.datetime.now(UTC)
```

Все три места (`hour_start`, `--now`, значение по умолчанию) должны стать aware одновременно, иначе `now < grace_until` бросит `TypeError` на сравнении naive и aware. Целочисленное деление ns заодно убирает потерю точности `ns / 1e9`.

**[сервер]** скопировать новый `feed_audit.py`: повторно запустить `bootstrap.sh`, рестартов не нужно.

**В4. `feed_audit.py:281-590`: `main()` на 310 строк (64 ветвления), а тестов у скрипта нет совсем.**

Что не так. Скрипт решает критерий фазы 1a (7 суток без дыр) и каждый день проверяет сервер. Проверено `grep` по репозиторию 2026-10-02: ни одного теста. В одной функции смешаны: разбор аргументов, сканирование файлов, учёт сессий, разбор конвертов, сверка с gaps.tsv, RPC, сборка сводки и печать. Чистые функции (`frame_len`, `split_frames`, `merge_ranges`, `classify_seq0`, `hour_file`) тестируются в одну строку, но не тестируются.

Исправление. Файл не делить: bootstrap ставит его на сервер одним файлом (`bootstrap.sh:154`), модульность через пакет добавила бы зависимость установки. Разбить внутри файла:

```python
@dataclasses.dataclass
class Tally:                 # replaces ~20 loose locals of main()
    lines: int = 0; envelopes: int = 0; col_errors: int = 0; bad_json: int = 0
    bad_envelope: int = 0; col_mismatch: int = 0; dups: int = 0; ns_backwards: int = 0
    raw_bytes: int = 0; zst_bytes: int = 0; frames: int = 0
    seq0: collections.Counter = dataclasses.field(default_factory=collections.Counter)
    kinds: collections.Counter = dataclasses.field(default_factory=collections.Counter)
    msgs_per_env: collections.Counter = dataclasses.field(default_factory=collections.Counter)
    gaps: list = dataclasses.field(default_factory=list)
    blocks: dict = dataclasses.field(default_factory=dict)
    first_seq: int | None = None   # Python 3.9: Optional[int]
    ...

def classify_tail(path, data, ctx) -> tuple[str, int] | None   # open / prev_open / torn / garbage
def scan_line(line: str, t: Tally, sess: Sessions) -> None      # one TSV line, no I/O
def scan_file(path, t, sess, ctx, fails) -> None                # zstd + scan_line
def check_gaps_file(t, feed_root, fails, warns) -> list
def rpc_check(t, url, n, seed, fails, warns) -> dict
def build_summary(t, sess, ctx, ...) -> dict
def print_text(summary) -> None
def main() -> int                                               # argparse + glue, < 50 lines
```

Тесты положить рядом: `.claude/skills/feed-audit/scripts/test_feed_audit.py`, stdlib `unittest`, на сервер не ставится. В них:
- `frame_len` на целом, обрезанном и skippable-фрейме, на мусоре;
- `merge_ranges` со смежными и перекрывающимися диапазонами;
- `classify_seq0`;
- сквозной прогон на синтетическом `.tsv.zst` через `zstd`, как сделано в этом ревью: PASS, дыра вне gaps.tsv → FAIL, дубль → FAIL, битый конверт → FAIL (В5), открытый хвост текущего часа → не FAIL.

Запуск: `python3 -m unittest discover .claude/skills/feed-audit/scripts`. Число в `summary` и вывод должны совпасть с текущими: формат читает `feed-audit-daily.sh` (В8).

**[сервер]** после рефакторинга повторно запустить `bootstrap.sh`.

**В5. `feed_audit.py:423-428`: `KeyError`/`TypeError` на конверте неожиданной формы обрывает аудит всего дня.**

Что не так. Обращения `m["sequenceNumber"]` и `m["message"]["message"]["header"]` ничем не защищены. Проверено 2026-10-02: конверт `{"messages":[{"sequenceNumber":103,"message":{}}]}` даёт traceback `KeyError: 'message'` и код 1, вердикта нет. `feed-audit-daily.sh:59` пришлёт «FAIL (нет вердикта)», так что сбой не тихий. Но отчёт за сутки (дыры, блоки/с, сессии) теряется целиком из-за одной строки. Цель аудита — сосчитать плохие строки, а не упасть на первой.

Исправление:

```python
try:
    seqs = [int(m["sequenceNumber"]) for m in msgs]
    hdrs = [m["message"]["message"]["header"] for m in msgs]
    kinds_l1 = [(h["kind"], int(h["blockNumber"])) for h in hdrs]
except (KeyError, TypeError, ValueError):
    t.bad_envelope += 1
    continue
...
if t.bad_envelope:
    fails.append("%d envelopes without sequenceNumber/header.kind/blockNumber" % t.bad_envelope)
```

Аналогично защитить `env.get("messages")` на случай, когда `env` не `dict`. **[сервер]**

**В6. Python-линт не настроен: 131 замечание ruff, 4 файла из 5 не отформатированы, конфигурации ruff нет.**

Что не так. Нет `ruff.toml`/`pyproject.toml`. Поэтому ruff по умолчанию предлагает несовместимое с заявленным Python 3.9 (B905 `strict=`). Каждый ревьюер получает разный вывод, и формат кода не закреплён.

Почему сейчас. Перед фазой 1c в `analytics/` появятся новые скрипты, и проще задать правила до них.

Исправление — `ruff.toml` в корне:

```toml
line-length = 120
target-version = "py39"          # feed_audit.py runs on stdlib 3.9+ (server: 3.14)
[lint]
select = ["E","F","W","B","UP","SIM","PL","RUF"]
ignore = ["PLR2004"]             # report scripts: thresholds read better inline
[lint.per-file-ignores]
".claude/skills/feed-audit/scripts/feed_audit.py" = ["B905", "UP031"]   # 3.9; %-format is fine
```

Затем `uvx ruff format analytics .claude/skills/feed-audit/scripts` и `uvx ruff check --fix`. Руками: F841 (`hourly_summary.py:247-248`), SIM115 (`open` без `with`: `hourly_sample.py:48,82,302`, `hourly_summary.py:69,75`, `feed_audit.py:485`), «Р» → «R» в `feed_audit.py:81,318`. Поведение не меняется.

**[сервер]** только если правится `feed_audit.py`; удобно сделать вместе с В3–В5.

**В7. `analytics/`: смысловые дубли разбора блоков и чтения zstd в трёх скриптах.**

Где дубли:
- «Пользовательская tx и её комиссия» определена трижды:
  - `hourly_metrics.py:66-92` (`zip(txs, rc)`, пропуск `SYS`, `gasUsed*effectiveGasPrice`, `gasUsedForL1`);
  - `hourly_summary.py:195-201`;
  - `hourly_summary.py:240-241`.

  Определения пока совпадают. Но `hourly_metrics` проверяет `len(txs)==len(rc)` и совпадение хэшей, а `hourly_summary` не проверяет: копия уже слабее оригинала.
- Чтение `*.jsonl.zst` целиком в память через `subprocess.run(["zstd","-dc"]).stdout` — в трёх местах (`hourly_metrics.py:116`, `hourly_summary.py:97,188`). Файл с семплом читается дважды за прогон (`:188` и цикл `:234`).
- `pct` в двух вариантах с разной семантикой: интерполяция в `hourly_summary.py:33` и ближайший ранг в `feed_audit.py:61`. Это допустимо, это разные скрипты, но их стоит назвать по-разному.
- `hourly_summary.py:25-26` подключает `hourly_metrics` через `sys.path.insert`.

Исправление — один небольшой модуль `analytics/hoodlib.py`, только stdlib, без классов на вырост:

```python
from __future__ import annotations
import json, subprocess
from collections.abc import Iterator

V3_SWAP = "0xc42079f9…"; V4_SWAP = "0x40e9cecb…"   # from contracts.md, as in hourly_metrics today
SYS = "0x6a"; L2_TYPES = {...}; L1_TYPES = {...}

def iter_block_lines(path: str) -> Iterator[bytes]:
    """Stream lines of a blocks-format .jsonl.zst (multi-frame safe) without loading it whole."""
    with subprocess.Popen(["zstd", "-dc", path], stdout=subprocess.PIPE) as p:
        yield from p.stdout
    if p.returncode:
        raise RuntimeError(f"zstd -dc {path}: rc={p.returncode}")

def user_tx_pairs(block: dict, receipts: list) -> Iterator[tuple[dict, dict]]:
    txs = block["transactions"]
    if len(txs) != len(receipts):
        raise ValueError(f"block {block['number']}: {len(txs)} txs, {len(receipts)} receipts")
    for t, r in zip(txs, receipts):
        if t["hash"] != r["transactionHash"]:
            raise ValueError(f"tx/receipt order mismatch in block {block['number']}")
        if t["type"] != SYS:
            yield t, r

def fee_wei(r: dict) -> int:
    return int(r["gasUsed"], 16) * int(r["effectiveGasPrice"], 16)
```

`hourly_metrics.metrics()` и оба места в `hourly_summary` переходят на `user_tx_pairs`/`fee_wei`. RPC-клиент `Rpc` (`hourly_sample.py:64-156`: бюджет, журнал вызовов, 429/403, User-Agent) перенести в `hoodlib` только тогда, когда появится второй скрипт, которому нужен RPC. Сейчас он один, и выносить его заранее — абстракция на вырост.

**`feed_audit.py` в `hoodlib` не переводить** — это осознанный дубль. feed_audit ставится на сервер одним файлом. Его RPC-сверка — один пакетный запрос без бюджета (`feed_audit.py:259`), это допустимо при `--rpc-sample ≤ 50`. Записать это одной строкой в SKILL.md feed-audit: «независим от analytics/, источник истины по фиду — recorder (Rust), feed_audit — независимая проверка».

**В8. `deploy/feed-audit-daily.sh`: не покрыт тестами и разбирает вердикт из текстового вывода, хотя код выхода уже есть.**

Что не так. Это путь ежедневного алерта: коды выхода 0/1/3, `SuccessExitStatus=3`, сообщение PASS «жив». В `deploy/test/` для него теста нет, хотя все пути уже переопределяются (`AUDIT_SCRIPT`, `HC_NOTIFY`, `AUDIT_FEED_DIR`, `AUDIT_REPORT_DIR`). Вердикт берётся через `awk '$1 == "verdict"'` (стр. 49), числа — по именам ключей из `print("%-18s %s")` в feed_audit (стр. 52). Любое переименование ключа при рефакторинге В4 молча обнулит `nums` или вердикт. Без вердикта сработает «FAIL (нет вердикта)», то есть ложный алерт, а не тихий пропуск.

Исправление:
- (1) Код выхода — главный сигнал:

  ```bash
  python3 "$AUDIT_SCRIPT" … >> "$tmp" 2>&1; audit_rc=$?
  echo "# exit $audit_rc" >> "$tmp"
  ```

  Затем `case $audit_rc in 0) PASS;; 1) FAIL;; *) "аудит не отработал (rc=$audit_rc)";; esac`. Строку `verdict` оставить для человека.
- (2) Добавить `deploy/test/test-feed-audit-daily.sh`. Вместо `AUDIT_SCRIPT` — фейковый python-скрипт, который печатает заданный вывод и выходит с заданным кодом. Плюс `fake-notify`. Случаи: PASS → info и exit 0; PASS при `AUDIT_NOTIFY_PASS=0` → тишина; FAIL → alert и exit 3; crash rc=1 без вердикта → alert и exit 3; notify упал → exit 1; неверная дата → exit 2.

Код выхода 1 сейчас означает и FAIL, и traceback. После В5 traceback станет редкостью, но код 2 (ошибка использования) стоит отличать и дальше.

**[сервер]** повторно запустить `bootstrap.sh` (копирует `feed-audit-daily.sh`).

## Рекомендации

- **Р1. `healthcheck.sh:88, 253, 260, 272, 386, 394`: файлы состояния пишутся неатомарно (`printf > "$f"`, `echo > "$off_file"`).** При полном диске или обрыве `gaps.offset` может остаться пустым. Тогда `off=${off:-0}` на следующем запуске повторит всю историю `gaps.tsv` одним уведомлением. А пустой `.alert`, записанный после успешного notify, даст `head -n 1` = "" в тексте «восстановлено». Исправление: `write_state() { printf '%s\n' "$2" > "$1.tmp.$$" && mv -f "$1.tmp.$$" "$1"; }`, использовать во всех шести местах. **[сервер]**
- **Р2. `healthcheck.sh` (426 строк): 11 проверок идут подряд на верхнем уровне файла.** Сам файл не делить. Обернуть каждую проверку в функцию (`check_unit`, `check_ban`, `check_feed`, … `check_smartd`) и вызывать их списком внизу. Порядок важен: `ban_row` нужен в `check_feed`, это стоит отметить комментарием. Так проще отключать проверки и читать diff. Существующий `test-healthcheck.sh` прогоняет весь скрипт и останется без изменений. **[сервер]**
- **Р3. `bootstrap.sh:80-81`: список пакетов повторён ради исключения `ufw`.** Заменить на `pkgs=(chrony zstd python3 curl ca-certificates rclone tzdata smartmontools); (( firewall )) && pkgs+=(ufw)`. Поведение то же, но порядок установки другой — это неважно. **[сервер]** — косметика, можно отложить до следующего обновления.
- **Р4. Список юнитов задан трижды: `bootstrap.sh:161-162`, `run-systemd-container.sh:166` и `:169`.** Новый юнит легко забыть в проверке `systemd-analyze verify`. В тесте строить список из исходников: `units=$(cd "$ROOT/deploy" && ls *.service *.timer | sed 's/@\.service$/@x.service/' | paste -sd ' ')`. Сервер не затрагивает.
- **Р5. Дублирование в тестовой обвязке `deploy/test/test-*.sh` (5 файлов).** Повторяются:
  - `HERE/T/mktemp/trap`;
  - заглушка `logger` (3 раза, текст идентичен — это клон jscpd);
  - `fake-notify` (3 варианта формата);
  - `pass/fail` и `check()` (4 варианта: с диагностикой и без);
  - итоговая строка `result:`.

  Вынести в `deploy/test/lib.sh`: `t_init`, `t_shim_logger`, `t_fake_notify [--body]`, `check DESC EXPR [DIAG_FN]`, `t_result`. Это минус ~60 строк. Сервер не затрагивает: тесты не ставятся. `run-systemd-container.sh` копирует весь `deploy/`, так что `lib.sh` будет на месте.
- **Р6. Разная модель доверия к env-файлам.** `notify.sh:39-55` разбирает `notify.env` построчно и не исполняет его (там секреты). `healthcheck.sh:40` и `backup.sh:24` выполняют свои env через `.`. Всё корректно (файлы `root:hood 640`, заданы вручную), но различие стоит описать одной строкой в README, раздел «конфиги», чтобы туда не положили секрет в расчёте на безопасный разбор.
- **Р7. SQL, типы (`001_schema.sql`).** Таблицы локально пусты (проверено для `funding_edges`; для остальных — предполагается), поэтому чинить дёшево сейчас:
  - `Nullable` там, где хватает `DEFAULT ''`: `txs.to_addr`, `logs.topic1..3`, `swaps.router`, `wallets.first_*`. Правило `schema-types-avoid-nullable` скилла clickhouse-best-practices;
  - `selector FixedString(10)` с `''` хранится как 10 нулевых байт: лучше `LowCardinality(String)`;
  - одно понятие разными типами: `swaps.venue Enum8`, но `tokens.venue LowCardinality(String)`;
  - `txs.value_wei String`, а `fee_wei UInt128`. Переход `value_wei` на `UInt256` даёт `sum()` без `toUInt256`, но **ломает соглашение data-model** («uint256 строкой»), так что это решение Михаила.

  Адреса везде `String` — это единообразно и соответствует data-model. `PARTITION BY` нигде нет, и это согласовано с `schema-partition-start-without`: сырьё не удаляется, TTL нет.
- **Р8. `001_schema.sql:100`: `labels ORDER BY (address, label)` без `source`.** Две эвристики, давшие одну метку, схлопнутся в последнюю. Если это задумано («последняя оценка побеждает») — написать в комментарии. Если нет — добавить `source` в ключ (через новую таблицу).
- **Р9. `001_schema.sql:102-107`: у `feed_gaps` нет колонки версии.** Изменение `filled 0 → 1` при слиянии зависит от порядка вставки. Нужно `ReplacingMergeTree(filled)` или `updated_at DateTime DEFAULT now()` как версия.
- **Р10. `docker-compose.yml:3`: `clickhouse-server:latest` без версии.** Поведение схемы и enum может смениться при `pull`. Закрепить мажорную версию.
- **Р11. `feed_audit.py:496-498`: выборка RPC через `random.sample` без seed.** Повторить её нельзя, а правило python.md требует писать seed в отчёт. Добавить `--seed` (по умолчанию из `time_ns`), печатать в `rpc.seed`. Память: словарь `blocks` занимает ~335 Б на блок. Замерено 2026-10-02 на синтетике в 3.14: ~290 МБ на сутки, ~2 ГБ на 7-дневный аудит. Если 7 суток гоняют одним вызовом, хранить только хэш и данные для выборки (резервуарная выборка); дубли и так ловит проверка `s <= last_seq`. **[сервер]**
- **Р12. `hourly_summary.py:60-314`: `main()` на 255 строк и захардкоженные значения отчёта задачи 010.** Захардкожены «58 days» (`:256, 290`), даты `07.10`/`14.10` (`:270`), `1790553600` (`:238`), `n_0408_12` (`:265`), `== 24` (5 мест). Скрипт разовый, отчёт 010 уже принят, поэтому переписывать сейчас не стоит. Если его будут перезапускать на новых периодах — разбить на `load_rows`, `block_weights`, `calibrate`, `section_*` и вывести `len(days)` и даты из аргументов. Отдельно: стр. 73 превращает в `float` все числовые колонки, включая `fee_sum_wei`/`fee_median_wei`. Сейчас они не используются, но колонки `*_wei` лучше оставлять `int`.
- **Р13. `hourly_metrics.py:93`: `int(st.median(fees))`.** При чётном числе комиссий медиана считается через `float` и усекается. Для wei нужна целочисленная медиана: `s = sorted(fees); m = s[len(s)//2] if len(s) % 2 else (s[len(s)//2 - 1] + s[len(s)//2]) // 2`. На текущих величинах (до ~1e14 wei) расхождения нет — это предположение по порядку величин.
- **Р14. `hourly_sample.py`: файл журнала открывается в `Rpc.__init__` (стр. 82) и не закрывается.** Сделать `Rpc` контекстным менеджером (`__enter__`/`__exit__` → `self.ledger.close()`). `read_env_rpc` (стр. 44-57) — третий в репозитории разборщик env-файлов. Пока он один в Python, это допустимо; при выносе `Rpc` в `hoodlib` (В7) перенести и его.
- **Р15. Документация.**
  - `analytics/README.md` устарел: там написано «Empty on purpose», «polars + clickhouse-connect», а в каталоге лежат три stdlib-скрипта задачи 010. Описать их назначение и порядок запуска: sample → metrics → summary.
  - `feed-audit-daily.sh:12` и `healthcheck.sh:35`: `set -uo pipefail` без `-e` без пояснения. Добавить одну строку комментария «без -e: каждая проверка/шаг обрабатывает ошибки сам, скрипт должен дойти до конца». **[сервер]** — только при следующем обновлении.

## Дубли

| Где | Насколько разошлись | Куда вынести |
|---|---|---|
| user-tx и комиссия: `hourly_metrics.py:66-92`, `hourly_summary.py:195-201, 240-241` | копии в summary не проверяют число receipts и хэши | `analytics/hoodlib.py: user_tx_pairs`, `fee_wei` (В7) |
| `zstd -dc` целиком в память: `hourly_metrics.py:116`, `hourly_summary.py:97, 188` | одинаковые; summary читает файл семпла дважды | `hoodlib.iter_block_lines` (потоково) |
| topic0 Swap v3/v4: `hourly_metrics.py:29-30`, `crates/decoders/src/lib.rs`, `contracts.md:23-24` | совпадают побайтно (grep 2026-10-02) | оставить; в Python — ссылка на contracts.md в комментарии |
| RPC: `hourly_sample.Rpc` и `feed_audit.rpc_blocks` | разная политика: бюджет и 429 против одного пакетного запроса; разный UA; sample отказывается работать не с публичным хостом, audit берёт `RPC_URL` | осознанный дубль: feed_audit независим и ставится одним файлом. Записать в SKILL.md feed-audit |
| разбор фида: recorder (Rust) и `feed_audit.py` | намеренно независимая проверка | источник истины — recorder и data-model.md; явно написать в SKILL.md feed-audit |
| тестовая обвязка `deploy/test/test-*.sh` | `check()` в 4 вариантах, `fake-notify` в 3 форматах | `deploy/test/lib.sh` (Р5) |
| `say/die` и цикл «пакет установлен?»: `bootstrap.sh:65-68, 83-88`, `build-on-server.sh:32-33, 46-49` | 2 короткие копии, не разошлись | оставить: оба скрипта самодостаточны и ставятся по отдельности. Общий `deploy/lib.sh` для серверных скриптов добавил бы зависимость установки, а хукам mdadm/smartd он не нужен |
| `log()` в `healthcheck`, `mdadm-event`, `smartd-event` | у каждого свой тег и своя политика вывода; smartd обязан молчать в stdout/stderr | оставить: копии короткие и осознанные |
| список юнитов: `bootstrap.sh:161`, `run-systemd-container.sh:166, 169` | пока совпадают | в тесте строить из файлов (Р4) |

## Структура и разбиение

- `feed_audit.py` — один файл, но функции и `Tally` (В4). Рядом `test_feed_audit.py`, на сервер не ставится.
- `analytics/`:

  ```
  analytics/hoodlib.py          constants (from contracts.md), iter_block_lines, user_tx_pairs, fee_wei, iso_utc
  analytics/hourly_sample.py    CLI + Rpc (moves to hoodlib only when a 2nd RPC script appears)
  analytics/hourly_metrics.py   metrics(line) -> dict (pure) + thin main
  analytics/hourly_summary.py   unchanged for now; uses hoodlib instead of sys.path + hm internals
  ```

  Когда `analytics/` разрастётся в пакет (фаза 1c, polars и ClickHouse), `hoodlib.py` станет `analytics/hood/common.py`. Делать это сейчас рано.
- `deploy/` — раскладку не менять. Отдельные изменения: функции внутри `healthcheck.sh` (Р2), `write_state` (Р1), новые `deploy/test/lib.sh` (Р5) и `test-feed-audit-daily.sh` (В8).
- `sql/` — `003_funding_edges_key.sql` (В1), `apply.sh` и правило идемпотентности (В2).

## Список рефакторинга по приоритету

| № | Что | Замечания | Сервер | Объём |
|---|---|---|---|---|
| 1 | Ключ `funding_edges`: 003 до первого загрузчика | В1 | нет | S |
| 2 | Применение миграций: `sql/apply.sh`, правило идемпотентности; применить 002 (и 003) локально | В2 | нет | S |
| 3 | feed_audit: aware-время, защита конверта, seed | В3, В5, Р11 | да (bootstrap) | S |
| 4 | `ruff.toml` + `ruff format` + ручные F841/SIM115 | В6 | вместе с п. 3 | S |
| 5 | Код выхода в `feed-audit-daily.sh` + `test-feed-audit-daily.sh` | В8 | да | S |
| 6 | Разбиение `feed_audit.main()` + `test_feed_audit.py`; вывод сверить с текущим | В4 | да | M |
| 7 | `analytics/hoodlib.py`; перевести на него metrics и summary | В7, Р13 | нет | S |
| 8 | `write_state` в healthcheck | Р1 | да | S |
| 9 | Обвязка тестов `deploy/test/lib.sh`; список юнитов из файлов | Р5, Р4 | нет | S |
| 10 | Типы SQL, `labels`/`feed_gaps`, версия ClickHouse — решение по `UInt256` за Михаилом | Р7–Р10 | нет | S–M |
| 11 | Функции в healthcheck, пакеты в bootstrap, документация | Р2, Р3, Р6, Р15 | да, при оказии | S |

Пункты 3–6 удобно отдать одной задачей indexer-engineer и выкатить одним повторным запуском `bootstrap.sh`. Пункты 1–2 нужны до задачи загрузчика ClickHouse.

## Что хорошо (не сломать при правках)

- `notify.sh`: токен не попадает в argv (`curl -K -`) и маскируется в ошибках; env разбирается без исполнения. Тест `test-notify.sh` проверяет именно это.
- Хуки `mdadm-event.sh`/`smartd-event.sh` всегда выходят с 0, а smartd-хук молчит в stdout/stderr (требование smartd). Тест включает прогон через настоящий `smartd_warning.sh`.
- Идемпотентность `bootstrap.sh`: `install_file`/`ensure_dir` сравнивают перед изменением, и `run-systemd-container.sh` проверяет, что второй прогон даёт 0 изменений и не трогает файлы (`find -newer`). Рестарты — только при изменении своего конфига.
- Все пороги и пути в `healthcheck.sh` переопределяются (`HC_*`, `HC_NOW`, `HC_MDSTAT`), поэтому тест работает без systemd и сети. Юниты закалены единообразно (`ProtectSystem=strict`, `ReadWritePaths` минимальны, `NoNewPrivileges`).
- `hourly_sample.py`: журнал вызовов с fsync, жёсткий бюджет до отправки, отказ при не-публичном хосте, атомарная упаковка с проверкой обратной распаковки. В Python wei везде считается в `int` до самого вывода.

## Предполагается / не проверено

- Остальные таблицы `hood.*` в локальном ClickHouse пусты так же, как `funding_edges`. Проверен только каталог `funding_edges`; общий размер `store` 343 МБ — вероятно, системные таблицы. ClickHouse не запускался.
- `backup.service` под `ProtectSystem=strict` с `HOME=/opt/hoodchain-mev`, доступным только на чтение: rclone для некоторых бэкендов пишет кэш в `~/.cache/rclone`. `test-backup.sh` в контейнере выполняется от hood, но не внутри песочницы юнита. Проверить при включении бэкапа (`systemd-run -p ProtectSystem=strict … backup.sh --verify`).
- Оценка памяти feed_audit (~290 МБ на сутки) получена на синтетике, а не на реальном дне. Объём RAM сервера не сверялся.
- На сервере Python 3.14 — взято из README, на сервере не проверялось (ssh не использовался). Предупреждения В3 воспроизведены локально на 3.14.7.
- Rust-код (`crates/`) в эту часть ревью не входил; `FundingEdge` смотрел только для В1.
- `cargo clean` не выполнялся (по указанию координатора). Docker-образ shellcheck удалён (`docker rmi koalaman/shellcheck:stable`).
