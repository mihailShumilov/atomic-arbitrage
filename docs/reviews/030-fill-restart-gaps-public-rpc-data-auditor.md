# 030 — Дозаливка двух дыр от рестартов через публичный RPC. Ревью data-auditor

date: 2026-10-05
reviewer: data-auditor
объект: файлы в `scratchpad/030/blocks/` (до выкладки на сервер), копии часов фида и `gaps.tsv` в `scratchpad/030/feed/`, отчёт `docs/handoff/from-code/030-fill-restart-gaps-public-rpc.md` (раздел «Команды для Михаила»).

**Вердикт по данным: PASS.** Два файла и `filled.tsv` можно выкладывать.
**Вердикт по командам выкладки: FAIL (Б1, Б2).** Как написано, первая команда на этом Mac не выполнится, а проверка «файл уже есть» ни на что не влияет. Ниже исправленный вариант. С ним выкладка разрешена.
**Отдельная находка вне 030 (Б3):** в `deploy/healthcheck.sh` проверка `backfill` ничего не проверяет, пока `filled.tsv` нет или он пуст. Поэтому `backfill=ok` после выкладки сам по себе не доказывает, что дыры закрыты.

## Как проверял

- Своими скриптами, `verify.py` не использовал и не читал: `scratchpad/030-audit/audit_offline.py` (офлайн) и `scratchpad/030-audit/audit_rpc.py` (RPC). Выводы лежат там же: `offline.txt`, `rpc.txt`, `fa-*.txt`.
- **Сеть:** фид 0 соединений, ssh 0. RPC: 22 вызова `eth_getBlockByNumber(n,false)` к публичному `rpc.mainnet.chain.robinhood.com` из `.env`. Один вызов на HTTP-запрос, без батчей, интервал ≥ 1.1 с, User-Agent `hoodchain-mev-data-auditor/030`. В коде жёсткий предел 25 вызовов. Ответов с ошибкой 0. Бюджет задачи: было 531, осталось 509.
- Файлы в `scratchpad/030/` не менял. Записал только `scratchpad/030-audit/` и этот файл.

## 1. Целостность файлов — PASS (проверено 2026-10-05)

| файл | `zstd -t` | фреймов | Check | sha256 (совпадает с отчётом) |
|---|---|---|---|---|
| `blocks-78504700-78505336.jsonl.zst` | 0 | 1 | XXH64 | `b5a061bb…6744b61` |
| `blocks-78780059-78780154.jsonl.zst` | 0 | 1 | XXH64 | `b1aaa540…f3549` |
| `filled.tsv` | — | — | — | `d40f4f0c…3c72b4` |

Флаг content checksum стоит и в заголовке фрейма (бит 2 байта FHD). В каталоге больше ничего нет, кроме пустого `.enricher.lock`: в команды выкладки он не попадает, потому что файлы перечислены явно.

## 2. Полнота и порядок — PASS

- 78504700..78505336: 637 строк, 78504700..78505336 = 637 = to − from + 1. 78780059..78780154: 96 строк = 96. Всего 733.
- Строки идут строго +1, повторов `number` и `hash` нет. `number` — int, равен `block.number`. Ключи строки ровно `{number, block, receipts}`, как в старых файлах `data/blocks/`.
- Цепочка `parentHash` внутри каждого файла без разрывов: 636 + 95 связей.

## 3. Транзакции и чеки — PASS

- tx = чеков в каждом блоке, всего 4 949 = 4 949 (4 549 + 400, как в отчёте). Все tx полные (объекты, не хэши).
- Для каждой пары tx↔чек совпадают `hash`/`transactionHash`, `transactionIndex` (равен позиции), `blockHash`, `blockNumber`, `from`, `to`, `type`.
- `cumulativeGasUsed` не убывает. `block.gasUsed` = последний `cumulativeGasUsed`. `logIndex` сквозной по блоку с 0. Логи ссылаются на свою tx и блок.
- `status` только `0x0`/`0x1`. Откатившихся 516 (492 + 24), ни у одной нет логов: свопов они не дадут.

## 4. Стыки с фидом — PASS

В обоих часах фида нет ни одного блока внутри дыр. У каждого соседа ровно одно сообщение и один `blockHash`.

| стык | проверка | результат |
|---|---|---|
| левый 78504700 | `parentHash` = `blockHash` фида у 78504699 | `0xe31b8588…fdb30107` = |
| левый 78780059 | `parentHash` = `blockHash` фида у 78780058 | `0x45bf1e47…57a49136` = |
| правый 78505337 | RPC `hash` = `blockHash` фида; RPC `parentHash` = `hash` блока 78505336 из файла | `0xb6b1c9b6…a080197a` =; `0x0476bfe3…cd849601` = |
| правый 78780155 | то же, для 78780154 | `0x3bee9f0e…d6e1084e` =; `0x684bf146…302c8ad7` = |

У фида нет `parentHash`, поэтому для правых стыков нужно 2 вызова RPC.

`feed_audit.py` (скилл `feed-audit`, `--rpc-sample 0`) по двум часам: PASS. Пропущено 637 и 96 блоков, обе дыры есть в `gaps.tsv`, других дыр в этих часах нет.

## 5. Сверка с RPC, 20 случайных блоков — PASS

Seed 20261005. Выборка пропорциональна размерам дыр: 18 блоков из первой, 2 из второй. У всех 20 `hash`, `number` и число tx совпали с файлами, расхождений 0. Список блоков — в `rpc.txt`.

## 6. `filled.tsv` — PASS

Ровно 2 строки, `\n` в конце, без `\r`, по 4 колонки: `from \t to \t file_name \t filled_unix_s`. Это формат `FilledRow` (`crates/hood-core/src/ranges.rs:322`), он совпадает с локальным `data/blocks/filled.tsv`. Диапазоны и имена файлов совпадают с файлами и с `gaps.tsv`. Время 1791185192 / 1791185289 — это 2026-10-05 ~07:26Z. Покрытие `gaps.tsv` строками `filled.tsv` (awk из отчёта): `uncovered=0`.

## 7. Команды выкладки — FAIL

Что в порядке: на сервере пишется только `/root/hood-030` и `/srv/hood/data/blocks`. Recorder, `feed/`, `gaps.tsv` и юниты не трогаются. Сначала данные, потом `filled.tsv`. `filled.tsv` дописывается (`>>`), а не перезаписывается. Владелец `hood:hood`, режим 0644 (как у enricher под systemd с umask 022). `feed_audit.py` идёт с `--rpc-sample 0`. Несовпадение `last_seq.txt` при аудите прошлых суток даёт только WARN.

- **Б1 (блокирует): `rsync --mkpath` не работает на этом Mac.** Здесь `/usr/bin/rsync` — openrsync (протокол 29), другого rsync в PATH нет. Проверено: `rsync --mkpath` → `unrecognized option`. Исправление — убрать `--mkpath`. Каталог-приёмник с `/` на конце rsync создаёт сам, вложенность здесь одна.
- **Б2 (блокирует): защита от перезаписи не работает.** Строка `for f …; do test -e "$B/$f" && echo "EXISTS $f — стоп"; done` только печатает, и следующий `install` перезапишет существующий файл. Кроме того, `cat >> filled.tsv` (в отличие от `append_synced` enricher'а) склеит строку, если в существующем `filled.tsv` нет `\n` в конце. И нет проверки, что этих диапазонов в `filled.tsv` ещё нет.
- **З1 (не блокирует):** `install` пишет сразу под итоговым именем, то есть файл какое-то время виден недописанным. Сейчас его никто не читает (таймер выключен), но лучше временное имя + `mv` в том же каталоге.
- **З2 (не блокирует):** вместо визуальной сверки sha256 лучше `sha256sum -c` с полными хэшами.

Исправленный вариант. На Mac:
```bash
S=/private/tmp/claude-501/-Users-mihailshumilov-sites-my-crypto-atomic-arbitrage/a477d6fc-6af5-4e28-af3e-63445fb94d7a/scratchpad/030/blocks
rsync -a "$S/blocks-78504700-78505336.jsonl.zst" "$S/blocks-78780059-78780154.jsonl.zst" "$S/filled.tsv" hood-rec:/root/hood-030/
```
На сервере (root, bash):
```bash
cd /root/hood-030 && sha256sum -c - <<'EOF2' && zstd -t blocks-*.jsonl.zst
b5a061bb23441ce08d0981e84299c6f4c4224e69e48cb42b81d75df1d6744b61  blocks-78504700-78505336.jsonl.zst
b1aaa540ed555cce4f02880b3984e96e4d103f6897d32c77f1908f0eb23f3549  blocks-78780059-78780154.jsonl.zst
d40f4f0ccb7429898018ebfc5f5f029a969871fbebae794deea72d742f3c72b4  filled.tsv
EOF2
( set -e
  B=/srv/hood/data/blocks
  F1=blocks-78504700-78505336.jsonl.zst; F2=blocks-78780059-78780154.jsonl.zst
  for f in $F1 $F2; do [ ! -e "$B/$f" ] || { echo "EXISTS $B/$f, stop"; exit 1; }; done
  if [ -s "$B/filled.tsv" ]; then
    ! grep -q -e $'^78504700\t' -e $'^78780059\t' "$B/filled.tsv" || { echo "ranges already in filled.tsv, stop"; exit 1; }
    [ -z "$(tail -c1 "$B/filled.tsv")" ] || { echo "filled.tsv: no trailing newline, stop"; exit 1; }
  fi
  for f in $F1 $F2; do install -o hood -g hood -m 0644 "$f" "$B/.$f.tmp"; mv "$B/.$f.tmp" "$B/$f"; done
  sync
  cat filled.tsv >> "$B/filled.tsv"; chown hood:hood "$B/filled.tsv"; chmod 0644 "$B/filled.tsv"
  ls -la "$B"; cat "$B/filled.tsv" )
```
Дальше — проверки из отчёта без изменений (`zstd -t`, awk `uncovered`, два прогона `feed_audit.py`, `rm -r /root/hood-030`). Но с поправкой Б3 ниже.

## 8. Б3 (вне 030, блокирует доверие к `backfill=ok`): healthcheck при отсутствующем или пустом `filled.tsv`

`deploy/healthcheck.sh:299`: `[[ -r $filled ]] || filled=/dev/null`, затем awk с `FNR == NR`. Если первый файл пустой, `FNR == NR` выполняется и на строках `gaps.tsv`. Тогда сами дыры читаются как «залитые», и получается 0 незалитых. Проверено 2026-10-05 локально: тот же awk на `/dev/null` + серверная копия `gaps.tsv` (две дыры старше 24 ч) печатает `0 0`. awk `uncovered` из отчёта на пустом `filled.tsv` печатает `uncovered=0`.

Следствия:
- (предполагается: на сервере `filled.tsv` нет, отчёт это тоже допускает) алерт `backfill` по этим двум дырам ни разу не срабатывал, хотя должен был;
- после выкладки «нет `backfill.alert`» и «восстановлено не пришло» ничего не доказывают. Проверка с awk имеет силу только потому, что в `filled.tsv` будут строки.

В тестах (`deploy/test/test-healthcheck.sh:74`) `filled.tsv` всегда создаётся со строкой-комментарием, поэтому случай без файла не покрыт.

Для выкладки 030 достаточно отрицательного контроля перед `cat >>`:
```bash
printf '0\t0\tx\t0\n' > /root/hood-030/ctl.tsv
awk -F'\t' 'FNR==NR{f[++n]=$1;t[n]=$2;next} $1~/^[0-9]+$/{c=0;for(i=1;i<=n;i++){lo=($1>f[i])?$1:f[i];hi=($2<t[i])?$2:t[i];if(hi>=lo)c+=hi-lo+1} u+=($2-$1+1)-c} END{print "uncovered="u+0}' /root/hood-030/ctl.tsv /srv/hood/data/feed/gaps.tsv
# ожидается uncovered=733 (больше, если с 2026-10-03 появились новые дыры); после выкладки тот же awk с filled.tsv → 0
```
Исправить healthcheck (например, `FILENAME == ARGV[1]` вместо `FNR == NR`, или служебная первая строка), добавить тест «`filled.tsv` нет» — задача для indexer-engineer/infra-ops, отдельно от 030.

## Предполагается (не проверено)

- Состояние сервера: `filled.tsv` и файлов с этими именами в `/srv/hood/data/blocks` нет, в `gaps.tsv` с момента копирования (mtime 2026-10-03 03:53Z) не появилось новых строк. Исправленные команды это проверяют перед записью.
- Healthcheck после выкладки покажет `backfill` ok. Из-за Б3 это ничего не доказывает.
