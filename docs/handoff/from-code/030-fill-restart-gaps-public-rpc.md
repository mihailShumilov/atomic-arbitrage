# 030 — Залить две дыры от плановых рестартов через публичный RPC (отчёт)

- Исполнитель: indexer-engineer. Дата: 2026-10-05.
- База: HEAD `3ffb8dc` (в ней 026: файлы с контрольной суммой). Код не менял. Бинарник enricher собран из рабочей копии (`crates/` без незакоммиченных правок на момент сборки), `--release`, в отдельном `CARGO_TARGET_DIR` в scratch. После работы каталог удалён, `cargo clean` не запускал: параллельно работают 031–033.
- Не коммитил, статус задачи не менял (`in-progress`), ревьюеров не запускал. На сервере ничего не писал. К фиду не подключался: 0 соединений.

## Сделано

1. **Копия с `hood-rec`, только чтение.** Сначала `ssh -O check hood-rec` (ControlMaster жив), потом один вызов `rsync -a` на три файла: `/srv/hood/data/feed/gaps.tsv`, `2026/10/02/feed-20261002-20.tsv.zst` (59.95 МБ), `2026/10/03/feed-20261003-03.tsv.zst` (38.28 МБ). Оба часовых файла закрыты (mtime 21:00 и 04:00 UTC), `zstd -t` OK.
   - В `gaps.tsv` две строки, как в задаче: `78504700 78505336` (637 блоков, 2026-10-02 ~20:07Z) и `78780059 78780154` (96 блоков, 2026-10-03 ~03:50Z). mtime файла — 2026-10-03 03:53 UTC, то есть новых дыр с тех пор не было (на момент копирования, 2026-10-05 ~07:15Z).
   - Соседи обеих дыр есть в скопированных часах: 78504699 и 78505337 в часе 20, 78780058 и 78780155 в часе 03. Сосед справа пришёл с тем же `recv_ns`, что записан в `gaps.tsv`.
2. **Дозаливка на Mac.**
   `enricher --gaps <scratch>/030/feed/gaps.tsv --out-dir <scratch>/030/blocks --rps 2 --batch 10 --concurrency 1 --max-calls 1600 --stats-json …`, `RPC_URL` из `.env` (публичный `rpc.mainnet.chain.robinhood.com`). Флага User-Agent у enricher нет, поэтому шёл UA reqwest по умолчанию: публичный RPC его пропускает (chain-facts, 2026-09-30). Перед прогоном сделал `--dry-run`: план 1 467 вызовов, 2 файла, 0 вызовов RPC.
   - Итог: выход 0, 727.8 с, 75 HTTP-запросов, 0 ответов 429, 0 повторов, 0 ошибок транспорта и таймаутов.
   - Файлы (с контрольной суммой XXH64, как задумано в 026):
     | файл | блоков | байт | sha256 |
     |---|---|---|---|
     | `blocks-78504700-78505336.jsonl.zst` | 637 | 2 516 550 | `b5a061bb23441ce08d0981e84299c6f4c4224e69e48cb42b81d75df1d6744b61` |
     | `blocks-78780059-78780154.jsonl.zst` | 96 | 193 768 | `b1aaa540ed555cce4f02880b3984e96e4d103f6897d32c77f1908f0eb23f3549` |
     | `filled.tsv` (2 строки) | — | 128 | `d40f4f0ccb7429898018ebfc5f5f029a969871fbebae794deea72d742f3c72b4` |
   - Лежат в `/private/tmp/claude-501/-Users-mihailshumilov-sites-my-crypto-atomic-arbitrage/a477d6fc-6af5-4e28-af3e-63445fb94d7a/scratchpad/030/blocks/`. Копии фида для сверки стыков — в `…/scratchpad/030/feed/`, скрипт проверки — `…/scratchpad/030/verify.py`.

## Вызовы RPC

| что | вызовов |
|---|---|
| enricher: `eth_chainId` | 1 |
| enricher: `eth_getBlockByNumber(full)` | 733 |
| enricher: `eth_getBlockReceipts` | 733 |
| проверка правых стыков: `eth_getBlockByNumber(to+1, false)` | 2 |
| **всего** | **1 469** из 2 000 |

Для data-auditor остаётся **531 вызов**. Сверка `hash` 20 случайных блоков — это 20 вызовов.

## Проверено (2026-10-05, как)

Проверял скриптом `verify.py`: Python, разбор каждой строки обоих файлов и сырья фида; результат — PASS.
- **`zstd -t`**: код 0 у обоих файлов. `zstd -lv` показывает `Check: XXH64` (`027b9826`, `c4233689`).
- **Непрерывность**: строки идут подряд, `from..to` без пропусков и повторов, в каждой `number` = `block.number`. Всего 637 + 96 = 733 блока.
- **`parentHash`**: у каждого блока внутри файла `parentHash` = `hash` предыдущего, разрывов 0.
- **tx = чеки**: 4 549 = 4 549 и 400 = 400. Каждый чек сверен со своей tx по `transactionHash`, `transactionIndex`, `blockHash` и `blockNumber`, расхождений 0. Чеков со `status=0x0` — 492 и 24.
- **Левые стыки, без RPC**: `parentHash` первого блока дыры = `blockHash` из сырья фида для блока `from−1`. 78504700 → `0xe31b8588…fdb30107` (фид 78504699); 78780059 → `0x45bf1e47…57a49136` (фид 78780058).
- **Правые стыки, 2 вызова RPC**: `hash` блока `to+1` из RPC = `blockHash` фида, а его `parentHash` = `hash` последнего блока файла. 78505337 → `0xb6b1c9b6…a080197a`, 78780155 → `0x3bee9f0e…57d6e1084e`; оба условия выполнены.
- **`filled.tsv`**: две строки, `from`/`to`/имя файла совпадают с дырами из `gaps.tsv`.
- В сырье фида у четырёх соседних блоков в конверте ровно одно сообщение, `sequenceNumber` = seq строки, повторов с другим хэшем нет.

## Предполагается (не проверено)

- После выкладки healthcheck покажет `backfill` = ok. Это следует из кода `healthcheck.sh` (awk сравнивает покрытие `gaps.tsv` строками `filled.tsv`), на сервере не проверял.
- На сервере `filled.tsv` пока нет: `enricher-gaps.timer` выключен (0002), каталог `blocks` создаёт bootstrap (`hood:hood 0750`). Каталог я не смотрел, поэтому команды ниже не перезаписывают `filled.tsv`, а дописывают в него и сначала проверяют, нет ли уже этих файлов.
- `feed_audit.py` `filled.tsv` не читает: он проверяет только, что каждая дыра есть в `gaps.tsv`. «Дыры закрыты» показывают healthcheck `backfill` и отдельная awk-проверка ниже.

## Команды для Михаила

На Mac:
```bash
S=/private/tmp/claude-501/-Users-mihailshumilov-sites-my-crypto-atomic-arbitrage/a477d6fc-6af5-4e28-af3e-63445fb94d7a/scratchpad/030/blocks
rsync -a --mkpath "$S/blocks-78504700-78505336.jsonl.zst" "$S/blocks-78780059-78780154.jsonl.zst" "$S/filled.tsv" hood-rec:/root/hood-030/
```

На сервере (root):
```bash
cd /root/hood-030 && sha256sum * && zstd -t blocks-*.jsonl.zst
#   b5a061bb…6744b61  blocks-78504700-78505336.jsonl.zst
#   b1aaa540…f3549    blocks-78780059-78780154.jsonl.zst
#   d40f4f0c…3c72b4   filled.tsv
B=/srv/hood/data/blocks
ls -la $B; cat $B/filled.tsv 2>/dev/null          # ожидается: блоков 78504700.. / 78780059.. там нет
for f in blocks-*.jsonl.zst; do test -e "$B/$f" && echo "EXISTS $f — стоп"; done
# сначала данные, потом состояние (filled.tsv не должен опережать файлы)
install -o hood -g hood -m 0644 blocks-78504700-78505336.jsonl.zst blocks-78780059-78780154.jsonl.zst $B/
cat filled.tsv >> $B/filled.tsv && chown hood:hood $B/filled.tsv && chmod 0644 $B/filled.tsv
ls -la $B && cat $B/filled.tsv
```

Проверка, только чтение:
```bash
runuser -u hood -- zstd -t /srv/hood/data/blocks/blocks-*.jsonl.zst
systemctl start healthcheck.service && journalctl -u healthcheck -n 3 -o cat   # backfill не в списке алертов; если алерт был — придёт «восстановлено»
ls /var/lib/hoodchain/health/                                                 # нет backfill.alert
# покрытие дыр строками filled.tsv (ожидается: uncovered=0)
awk -F'\t' 'FNR==NR{f[++n]=$1;t[n]=$2;next} $1~/^[0-9]+$/{c=0;for(i=1;i<=n;i++){lo=($1>f[i])?$1:f[i];hi=($2<t[i])?$2:t[i];if(hi>=lo)c+=hi-lo+1} u+=($2-$1+1)-c} END{print "uncovered="u+0}' /srv/hood/data/blocks/filled.tsv /srv/hood/data/feed/gaps.tsv
# аудит фида за сутки обеих дыр: verdict PASS, дыры = строки gaps.tsv
runuser -u hood -- python3 /opt/hoodchain-mev/deploy/feed_audit.py --feed-root /srv/hood/data/feed --rpc-sample 0 --frame-secs 60 "/srv/hood/data/feed/2026/10/02/feed-*.tsv.zst"
runuser -u hood -- python3 /opt/hoodchain-mev/deploy/feed_audit.py --feed-root /srv/hood/data/feed --rpc-sample 0 --frame-secs 60 "/srv/hood/data/feed/2026/10/03/feed-*.tsv.zst"
rm -r /root/hood-030
```

Когда `enricher-gaps.timer` включат, он прочитает `filled.tsv` и эти диапазоны пропустит.

## Прочее

- В `references/chain-facts.md` (раздел RPC) добавлен пункт про этот прогон: 1 467 вызовов при 2 вызовах/с, 0 ответов 429, блоки двухсуточной давности пришли целиком, размер на блок.
- Что не получилось: всё получилось.
- Вопросы: нет. Дальше — выкладка на сервер (команды выше), затем data-auditor, после выкладки `status: done` и отдельный коммит. Это по задаче, я этого не делал.

## Выкладка на сервер (2026-10-05)

Выполнено Михаилом скриптом `install.sh`, собранным координатором по исправленному варианту data-auditor (sha256-проверка, отрицательный контроль, остановка при существующих файлах/диапазонах, атомарная установка от `hood`): до выкладки `uncovered=733`, после — `uncovered=0`; `/srv/hood/data/blocks` — 2 файла + `filled.tsv`, владелец `hood`. После выкладки healthcheck с исправлением 034: `gaps_rows=2 backfill_calc=ok backfill=ok` — дыры закрыты (теперь это честная проверка). Recorder не тронут (`NRestarts=0`, PID 89622).
