# 014 — Сервер: RAID-алерт, UTC, текст healthcheck; разбор kind 12

## Пункты 1–3 (infra-ops)

date: 2026-10-01
executor: infra-ops
reviewer: indexer-engineer (ещё не запускался)

Статус задачи не менял, коммитов не делал. На сервере я **только читал** (`ssh hood-rec` через ControlMaster). Привилегированные шаги (rsync, bootstrap, перезапуск mdmonitor, смена пояса) у субагента заблокированы, их выполняет Михаил по списку ниже. Подключений к фиду — 0, вызовов RPC — 0, recorder я не трогал. IP сервера нигде не записан.

### Сделано (в репозитории)

1. **RAID: события mdadm.**
   - `deploy/mdadm-event.sh` (новый) — это `PROGRAM` для `mdadm --monitor`. ALERT получают `Fail`, `FailSpare`, `DegradedArray`, `DeviceDisappeared`, `SpareActive`. INFO получают `RebuildFinished` и `TestMessage`. Остальные события (`RebuildStarted`, `Rebuild20/40/60/80`, `NewArray`, …) пишутся только в журнал (`hood-mdadm`): иначе прогресс каждые 20 % был бы спамом.
   - В текст уведомления попадают строки массива из `/proc/mdstat`. Скрипт всегда завершается с кодом 0.
   - `deploy/mdadm-hood.conf` (новый) содержит одну строку `PROGRAM /opt/hoodchain-mev/deploy/mdadm-event.sh` и ставится как `/etc/mdadm/mdadm.conf.d/hood.conf`. Основной `mdadm.conf` не правится.
2. **RAID: проверка `raid` в `healthcheck.sh`.** Источник — `/proc/mdstat`, путь переопределяется через `HC_MDSTAT`.
   - `_` в карте дисков (`[U_]`, `[_U]`) или `inactive` → один ALERT со строками этих массивов. Когда карта снова `[UU]`, приходит «восстановлено».
   - Начало resync/recovery/reshape → одно INFO с процентом и оставшимся временем. Пока синхронизация идёт и когда она кончается, уведомлений нет.
   - `check` (ежемесячный mdcheck) не шлётся, он виден только в итоговой строке.
   - Если `/proc/mdstat` нет или в нём нет массивов, проверка пропускается (`raid=none`).
3. **`bootstrap.sh`, идемпотентно.**
   - Добавлен пакет `tzdata`, ставится `mdadm-event.sh`.
   - Если mdadm установлен, ставится drop-in, и `mdmonitor.service` перезапускается, **только если drop-in изменился и mdmonitor запущен**. Если массивы есть, а mdmonitor не работает, он запускается.
   - Часовой пояс приводится к `Etc/UTC` через `timedatectl set-timezone`, только если он другой.
4. **Текст healthcheck (п. 3).** Подсказка «только ping?» теперь добавляется только в ALERT `feed`: `last_seq.txt` старше порога, а файл часа свежий. Там она по делу: соединение живо, блоков нет. В «восстановлено» текст теперь такой: «last_seq.txt обновлялся N с назад, last_seq=…». Раньше там всегда было «блоков нет (только ping?)»: пока блоки идут, файл часа свежий всегда.
5. **Тесты.**
   - `deploy/test/test-mdadm-event.sh` (новый, 20 проверок).
   - `test-healthcheck.sh`: добавлен раздел 16 «raid» на подставном `/proc/mdstat`. Снимки взяты по образцу `hood-rec`: начальный resync md3 81.7 % + md2 DELAYED, деградация с `(F)`, recovery, `[_U]` + inactive, check, отказ notifier. Добавлена проверка текста «восстановлено» без «только ping».
   - Стенд `run-systemd-container.sh`: проверки часового пояса (Europe/Berlin → 1 изменение → Etc/UTC → 0 изменений), drop-in mdadm, чтения `PROGRAM` mdadm'ом и прохода `mdadm-event.sh` → `notify.sh` → journald.
   - В тестовый образ добавлен mdadm.
6. `deploy/README.md`: строки таблицы файлов, `raid`/`raid_sync` в «Мониторинг», подразделы «RAID» и «Часовой пояс», «Обновление только скриптов `deploy/`», заметка о mdadm в 26.04. `deploy/etc/healthcheck.env.example`: `HC_CHECK_RAID`, `HC_MDSTAT`.

`recorder.service`, остальные юниты, `notify.sh` и код recorder не менялись.

### Проверено (2026-10-01, как)

**Сервер, только чтение (~12:53Z и ~13:09Z):**
- mdadm 4.5-5ubuntu1. Юнит называется **`mdmonitor.service`**: `static`, `active (running)` с 09:19Z, `ExecStart=/usr/sbin/mdadm --monitor --scan`, без `--program`. Его поднимает udev-правило (`SYSTEMD_WANTS+="mdmonitor.service"`).
- Есть `mdmonitor-oneshot.timer` (enabled, ежедневное напоминание о деградации). Юнитов `mdadm.service` и `mdmonitor-oneshot.service` с install-секцией нет.
- В `/etc/mdadm/mdadm.conf` стоит `MAILADDR root`, `PROGRAM` нет, каталога `mdadm.conf.d` нет. Почтового агента нет, то есть о деградации сейчас никто не узнает.
- `/proc/mdstat`: 4 × RAID1, все `[UU]`. md3 resync 81.7 % (12:53Z), затем 86.5 %, finish ≈ 42 мин (13:09Z). md2 `resync=DELAYED`: после md3 начнётся resync 2 ТБ, это ещё несколько часов.
- `timedatectl`: `Europe/Berlin (CEST)`, NTP synchronized. При этом `/etc/timezone` уже `Etc/UTC`, а `/etc/localtime` → Berlin.
- Таймеры набора: `feed-audit.timer` — `OnCalendar=… 00:10:00 UTC`, следующий запуск 02:10 CEST = 00:10 UTC. `healthcheck.timer` — каждые 5 мин.
- `awk` на сервере — **gawk**, `comm`/`paste` — uutils. Тесты я гонял и с mawk (образы ubuntu), и с gawk.
- sha256 установленных файлов против рабочей копии: все 10 юнитов, `notify.sh`, `backup.sh`, `build-on-server.sh`, `feed-audit-daily.sh`, `feed_audit.py`, все `*.example` (сравнивал до своей правки `healthcheck.env.example`), journald- и needrestart-конфиги **совпадают**. Отличаются только `healthcheck.sh` и `README.md`: README разошёлся ещё в коммите 40edcf6.
- Пакеты `tzdata`, `mdadm`, `needrestart` установлены.
- recorder (13:09Z): `NRestarts=0`, `MainPID=7347`, `ActiveEnterTimestamp=11:57:12 CEST` (= 09:57:12Z). Это совпадает с базой координатора.

**mdadm читает drop-in** (контейнер ubuntu:26.04, mdadm 4.5-5ubuntu1.1, `--network none`). Убрал `MAILADDR`, и без `hood.conf` mdadm пишет «No mail address or alert command - not monitoring». С `/etc/mdadm/mdadm.conf.d/hood.conf` сообщения нет. Файл без расширения `.conf` не читается. Та же проверка теперь стоит в стенде на 24.04 и 26.04.

**Тесты:**

| Набор | ubuntu:26.04 | ubuntu:24.04 | 24.04 + gawk |
|---|---|---|---|
| shellcheck 0.11.0 (`koalaman/shellcheck:stable`, `--network none`), все `deploy/*.sh`, `deploy/test/*.sh` | 0 замечаний | — | — |
| `test-healthcheck.sh`, `--network none` | 100/100 | 100/100 | 100/100 |
| `test-mdadm-event.sh`, `--network none` | 20/20 | 20/20 | 20/20 |
| `test-notify.sh`, `--network none` | 14/14 | 14/14 | 14/14 |
| `run-systemd-container.sh` (systemd; сеть только для apt; портов нет; хосты фида, RPC и Telegram → 127.0.0.1) | ALL PASS: bootstrap 43 → 0, TZ 1 → 0 | ALL PASS, то же | — |

`bash -n` по всем скриптам чистый. Прогон с gawk шёл с сетью (только чтобы поставить gawk через apt), сами тесты сеть не используют.

**Симуляция обновления сервера** (контейнер ubuntu:26.04 под systemd). Сначала bootstrap из закоммиченного HEAD (`92ee9b9`, старый набор), затем подставной `recorder` (`sleep infinity`), включённые таймеры, запущенный `recorder.service` и пояс Europe/Berlin. Потом новое дерево и bootstrap. Результат:
- **6 изменений**: `healthcheck.sh`, `mdadm-event.sh`, `README.md`, `healthcheck.env.example`, `/etc/mdadm/mdadm.conf.d/hood.conf`, `timezone Europe/Berlin -> Etc/UTC`;
- **`daemon-reload` не было**;
- у recorder `MainPID`, `NRestarts=0` и `ActiveEnterTimestamp` те же, время показано в UTC;
- следующий прогон — 0 изменений;
- `feed-audit.timer` — следующий запуск 00:10 UTC.

После тестов удалены контейнеры и образы `hood-deploy-test-systemd:24.04/26.04`, `koalaman/shellcheck:stable`, `ubuntu:26.04`. `ubuntu:24.04` был на Mac до начала, его оставил.

### Предполагается, не проверено

- **Поведение живого `mdmonitor` на настоящем событии.** В Docker на Mac нет драйвера md, поэтому ни `Fail`, ни `RebuildFinished` вживую не проверялись. Проверено, что mdadm читает `PROGRAM` из drop-in и что `mdadm-event.sh` доводит сообщение до journald. Полную цепочку на сервере покажет `mdadm --monitor --scan --oneshot --test` (шаг 5 ниже): тот же бинарник и тот же конфиг, что у демона после перезапуска.
- Что перезапуск `mdmonitor` во время resync ничего не затрагивает. По устройству mdadm это только процесс, который опрашивает `/proc/mdstat`. Синхронизацией управляет ядро, recorder от mdmonitor не зависит. Вживую на сервере не проверялось.
- Что dracut-образ (26.04) от drop-in не пострадает. Монитор в initramfs не запускается, initramfs при этом не пересобирается.

### Что увидит Михаил после применения

- ~4 INFO «RAID: тестовое сообщение mdadm для /dev/md/N» (шаг 5).
- Одно INFO от healthcheck «RAID: идёт синхронизация: …»: md3, если он ещё идёт, и/или md2. Это если resync к моменту применения не закончился.
- Позже — INFO «RAID: синхронизация /dev/md/… завершена» от mdadm.
- **Telegram не настроен** (`/etc/hoodchain/notify.env` нет), поэтому всё это, как и прочие алерты, пока остаётся только в journald. До Михаила RAID-алерт дойдёт только после настройки бота (решение 013: «в план»).

### Команды для Михаила (по порядку)

Координатор сначала коммитит. rsync копирует закоммиченное дерево целиком, как в runbook; на сервере значимы только эти файлы:
- `deploy/healthcheck.sh`, `deploy/mdadm-event.sh` (новый), `deploy/mdadm-hood.conf` (новый) и `deploy/bootstrap.sh`;
- `deploy/etc/healthcheck.env.example` и `deploy/README.md`;
- `deploy/test/*` только хранится в `src`, не ставится.

Все `ssh` идут через ControlMaster, новых TCP-соединений нет (лимит ufw не задевается). Если master закрыт, команды стоит вводить не чаще 6 за 30 с.

```
# 0. база до изменений (ожидается NRestarts=0, MainPID=7347, 11:57:12 CEST)
! ssh hood-rec 'systemctl show recorder -p NRestarts -p ActiveEnterTimestamp -p MainPID'

# 1. исходники на сервер (с Mac, из закоммиченного состояния)
! cd /Users/mihailshumilov/sites/my/crypto/atomic-arbitrage && git status --short
! cd /Users/mihailshumilov/sites/my/crypto/atomic-arbitrage && rsync -a --delete --include .env.example --exclude target --exclude data --exclude .env --exclude '.env*' --exclude .idea --exclude '*.zip' ./ hood-rec:/opt/hoodchain-mev/src/

# 2. владелец src как раньше (openrsync с Mac ставит uid Mac)
! ssh hood-rec 'chown -R hoodbuild:hoodbuild /opt/hoodchain-mev/src'

# 3. bootstrap: скрипты, drop-in mdadm + перезапуск mdmonitor, Etc/UTC
! ssh hood-rec 'bash /opt/hoodchain-mev/src/deploy/bootstrap.sh'
#    ожидается: 6 CHANGED (healthcheck.sh, mdadm-event.sh, README.md,
#    healthcheck.env.example, mdadm.conf.d/hood.conf, timezone Europe/Berlin -> Etc/UTC),
#    строка "mdmonitor.service restarted", НЕТ "systemd: daemon-reload", "done: 6 change(s)"

# 4. повтор: идемпотентность
! ssh hood-rec 'bash /opt/hoodchain-mev/src/deploy/bootstrap.sh | tail -n 3'
#    ожидается "done: 0 change(s), 0 warning(s)"

# 5. тестовое уведомление RAID (TestMessage для каждого из 4 массивов)
! ssh hood-rec 'mdadm --monitor --scan --oneshot --test'
! ssh hood-rec 'journalctl -t hood-notify -n 6 -o short-iso --no-pager'
#    ожидается 4 строки "[INFO] ...: RAID: тестовое сообщение mdadm для /dev/md/N"
#    (stderr mdadm про отправку почты, если будет, не важен: MTA нет)

# 6. проверки
! ssh hood-rec 'cat /etc/mdadm/mdadm.conf.d/hood.conf; systemctl status mdmonitor --no-pager -n 5'
! ssh hood-rec 'timedatectl'
! ssh hood-rec 'systemctl list-timers healthcheck.timer feed-audit.timer --no-pager'
! ssh hood-rec 'systemctl start healthcheck.service; journalctl -u healthcheck -n 2 -o cat --no-pager'
#    ожидается "... raid=ok" и, пока идёт resync, "raid_sync=md3_resync_..." или "md2_..."
! ssh hood-rec 'journalctl -t hood-notify -n 2 -o cat --no-pager'
#    ожидается одно INFO "RAID: идёт синхронизация: ...", если resync ещё идёт
! ssh hood-rec 'systemctl show recorder -p NRestarts -p ActiveEnterTimestamp -p MainPID'
#    ожидается NRestarts=0, MainPID=7347, ActiveEnterTimestamp=Thu 2026-10-01 09:57:12 UTC
```

Ожидаемый вывод в шаге 3 — по симуляции, описанной выше.

**Запасной путь без bootstrap** (тот же результат, кроме README и `.example`; их потом доставит любой следующий прогон bootstrap):

```
! ssh hood-rec 'install -m 755 /opt/hoodchain-mev/src/deploy/healthcheck.sh /opt/hoodchain-mev/src/deploy/mdadm-event.sh /opt/hoodchain-mev/deploy/'
! ssh hood-rec 'install -D -m 644 /opt/hoodchain-mev/src/deploy/mdadm-hood.conf /etc/mdadm/mdadm.conf.d/hood.conf'
! ssh hood-rec 'systemctl restart mdmonitor.service'
! ssh hood-rec 'timedatectl set-timezone Etc/UTC'
```

**Что может задеть recorder — разбор по `bootstrap.sh`:**
- `recorder.service` ставится, только если отличается от установленного (`cmp`). sha256 совпадают, значит записи не будет. `daemon-reload` делается, только если изменился какой-то юнит, а изменённых юнитов нет. В симуляции его не было.
- `systemctl restart` в bootstrap есть только для `systemd-journald` (если изменился `journald-hood.conf`; он совпадает) и для `mdmonitor` (если изменился `hood.conf`).
- apt не запустится: все пакеты, включая `tzdata`, уже стоят. Если бы и запустился, `needrestart-hood.conf` исключает recorder.
- Для recorder в bootstrap только чтение: `systemctl is-enabled/is-active`. Его он не стартует и не перезапускает.
- `timedatectl set-timezone` меняет `/etc/localtime` и никакие службы не перезапускает. Таймеры набора заданы в UTC. Системные таймеры (logrotate, apt-daily, mdcheck) сдвинутся на 2 ч в местном исчислении, это безвредно.
- **Ни один шаг не перезапускает recorder.** Контроль — шаг 6, последняя команда.

После rsync `src` будет на новом коммите. Бинарники остаются прежними (`BUILD_INFO` `1b13aa4`): код recorder в 014 не менялся, пересборка не нужна.

### Вопросы к Cowork / Михаилу

1. **Telegram.** Без `notify.env` и RAID-алерт, и все остальные остаются в journald сервера. Пока это так, требование «алерт доходит до Михаила» не выполнено. Настройка бесплатная, нужны токен и chat id (runbook, шаг 4).
2. **SMART.** `smartmontools` не установлен, поэтому деградирующий, но ещё не выпавший диск не виден. Предлагаю отдельной мелкой задачей: пакет (бесплатно), `smartd` → тот же `notify.sh`. Ставить пакет на сервере во время 7-суточной записи — на решение Михаила.

## Пункт 4 (indexer-engineer)

date: 2026-10-01
executor: indexer-engineer
reviewer: data-auditor (ещё не запускался)

### Сделано

1. Сверил нумерацию `L1MessageType_*` и разбор kind 12 по исходникам Nitro, а также по контрактам L1-inbox.
2. Скопировал с `hood-rec` три закрытых часовых файла (09, 10, 11 UTC). Разобрал все kind 9/12/13 офлайн.
3. Проверил через RPC 5 блоков kind 12, плюс 2 блока kind 9 и 1 блок kind 13.
4. Дописал проверенные факты в `.claude/skills/hoodchain-mev/references/chain-facts.md`: новый подраздел «L1-сообщения фида (`header.kind`), задача 014» и три пометки «сверено в 014» у старых записей 001, 006 и 013.
5. Добавил в `.claude/skills/hoodchain-mev/references/data-model.md` раздел «L1-сообщения фида» с таблицей.

Код recorder, формат сырья и `deploy/` не трогал. На сервере ничего не писал, recorder не трогал. К фиду не подключался. Ни одного вызова RPC с сервера.

### Исходники Nitro — проверено (2026-10-01, raw.githubusercontent.com и GitHub API через curl; Context7 не использовал)

Версия — тег **`v3.11.4`**, коммит `7d5ac271b400f710f6267ad759c6afc3e12d7059`. Это последний релиз на 2026-10-01. Поддержка ArbOS 61 заявлена в release notes v3.11.0. В сабмодуле go-ethereum `dff2aadcd2e75e33923b13acce6468d847477e79` файл `params/config_arbitrum.go` содержит `MaxArbosVersionSupported = ArbosVersion_61` (L51). В `arbos/arbosState/arbosstate.go` L504 есть `case params.ArbosVersion_61`. Robinhood Chain работает на ArbOS 61 (docs.robinhood.com, задача 006).

| Что | Файл и строки (Nitro v3.11.4, если не указано иное) |
|---|---|
| Константы: 3 L2Message, 6 EndOfBlock, 7 L2FundedByL1, 8 RollupEvent, 9 SubmitRetryable, 10 BatchForGasEstimation, 11 Initialize, **12 EthDeposit**, 13 BatchPostingReport, 0xFF Invalid | [`arbos/arbostypes/incomingmessage.go` L24-35](https://github.com/OffchainLabs/nitro/blob/v3.11.4/arbos/arbostypes/incomingmessage.go#L24-L35) |
| Шапка: поле `Poster` сериализуется в JSON как `"sender"`; `RequestId`, `BlockNumber` (L1), `Timestamp`, `L1BaseFee` (`baseFeeL1`) | [там же, L51-58](https://github.com/OffchainLabs/nitro/blob/v3.11.4/arbos/arbostypes/incomingmessage.go#L51-L58) |
| Разбор по видам | [`arbos/parse_l2.go` L25-90](https://github.com/OffchainLabs/nitro/blob/v3.11.4/arbos/parse_l2.go#L25-L90); kind 12 — L67-72 |
| Payload kind 12: `AddressFromReader` (20 Б `to`) + `HashFromReader` (32 Б `value`) → `ArbitrumDepositTx{L1RequestId: header.RequestId, From: header.Poster, To: to, Value: value}` | [`arbos/parse_l2.go` L277-297](https://github.com/OffchainLabs/nitro/blob/v3.11.4/arbos/parse_l2.go#L277-L297) |
| Тип tx: `ArbitrumDepositTxType = 0x64` (0x65 Unsigned, 0x66 Contract, 0x68 Retry, 0x69 SubmitRetryable, 0x6A Internal) | go-ethereum `dff2aadc`, [`core/types/transaction.go` L48-54](https://github.com/OffchainLabs/go-ethereum/blob/dff2aadcd2e75e33923b13acce6468d847477e79/core/types/transaction.go#L48-L54); структура — `core/types/arb_types.go` L498-504; JSON (`requestId`, `from`, `to`, `value`) — `core/types/transaction_marshalling.go` L179-184 |
| Исполнение депозита: чеканка `value` на `From`, затем `Transfer` на `To`, без EVM. Если хэш tx в onchain-фильтре — получатель `FilteredFundsRecipient` | [`arbos/tx_processor.go` L239-270](https://github.com/OffchainLabs/nitro/blob/v3.11.4/arbos/tx_processor.go#L239-L270) (фильтр — L249-261) |
| Alias: `+0x1111000000000000000000000000000000001111` mod 2^160 | [`arbos/util/util.go` L36, L203-209](https://github.com/OffchainLabs/nitro/blob/v3.11.4/arbos/util/util.go#L203-L209) |
| Шапка блока: `Coinbase` (`miner`) = `Poster`, `Time` = max(L1 ts, время прошлого блока); `nonce` = `delayedMessagesRead`; первой идёт `0x6a` StartBlock | [`arbos/block_processor.go` L189-227](https://github.com/OffchainLabs/nitro/blob/v3.11.4/arbos/block_processor.go#L189-L227), L711, L368 |
| kind 9: формат payload и `ArbitrumSubmitRetryableTx` | `arbos/parse_l2.go` L299-377 |
| kind 13: поля payload; `0x6a` `batchPostingReportV2` при ArbOS ≥ 50 | `arbos/arbostypes/incomingmessage.go` L383-418; `arbos/parse_l2.go` L381-425 |
| L1: `depositEth` — `to` = `msg.sender` для EOA, иначе alias(`msg.sender`); `header.sender` = alias(`msg.sender`) | nitro-contracts `4341b132cfbdcc980ead03765ca5224ff6cb5d97` (сабмодуль Nitro v3.11.4): `src/bridge/Inbox.sol` L202-214, L317-326; `src/libraries/MessageTypes.sol` (12 = `L1MessageType_ethDeposit`) |

Селекторы и topic0 проверил keccak'ом сигнатур (pycryptodome):
- `0x6bf6a42d` — startBlock;
- `0x9998269e` — `batchPostingReportV2(uint256,address,uint64,uint64,uint64,uint64,uint256)`;
- `0xc9f95d32` — submitRetryable;
- `0x7c793cce` — TicketCreated;
- `0x5ccd0095` — RedeemScheduled;
- `0xc7f2e9c5` — `DepositFinalized(address,address,address,uint256)`;
- `0x2e567b36` — finalizeInboundTransfer.

Скачанные исходники лежат в `…/scratchpad/014/nitro/`.

**Предполагается.** Какая версия nitro-contracts развёрнута на L1 у Robinhood, не проверял. Логику alias подтверждают данные: 22 из 22, см. ниже.

### Данные — проверено (2026-10-01, rsync на Mac только на чтение, разбор офлайн)

- Скопировал **только закрытые** часы `feed-20261001-09/10/11.tsv.zst`, одним вызовом rsync через ControlMaster, в 12:56Z. Текущий час 12 не брал.
- sha256 сверил с `sha256sum` на сервере, совпали 3 из 3. Хэши часов 09 и 10 совпали ещё и с аудитом 013.
  - `09` — `6b2ad824…69d45`
  - `10` — `8fc85e65…1c332`
  - `11` — `bf14d32b28c732534a472e301ef7deedf0be3f99bc2ffc7cb29467ae6e58ae83`
- В копии 73 112 блоков, seq 77284408..77357519 (09:57:13–12:00Z). Уникальных 73 112, без дыр. Нарушений `delayedMessagesRead` 0.
- Скрипт — `…/scratchpad/014/analyze_kinds.py` (stdlib + `zstd`). Вывод — `analyze_kinds.out`, `kind12.json`, `kind9_13.json`.

**Частота видов:**

| час (UTC) | kind 3 | kind 9 | kind 12 | kind 13 |
|---|---|---|---|---|
| 09 (с 09:57:13, 2.8 мин) | 1 652 | 4 | 1 | 10 |
| 10 | 35 571 | 24 | 3 | 92 |
| 11 | 35 623 | 21 | 18 | 93 |
| всего за сутки к 12:00Z (2.05 ч) | 72 846 | 49 | 22 | 195 |

Других видов нет. `requestId` всех 266 отложенных сообщений идёт подряд: 340181..340446.

**kind 12 — все 22 сообщения:**
- payload ровно 52 байта;
- alias(первые 20 байт) = `header.sender` у **22 из 22**. Значит, все депозиты — `depositEth` прямо из EOA, и L1-адрес отправителя совпадает с получателем на L2;
- `requestId` и `baseFeeL1` ≠ 0;
- получателей 21: адрес `0x321bb3c1…0792` получил два депозита (0.0074 и 0.1032 ETH);
- задержка от `header.timestamp` (L1 inbox) до приёма в фиде — 421–745 с, p50 618 с. У kind 9 и kind 13 — 399–776 с.

**Суммы kind 12:**
- 20 из 22 — в диапазоне 0.09989–0.09991 ETH (13 из них ровно 0.0998937; уточнено по data-auditor З1); одна 0.0074, одна 0.1032;
- всего 2.1086 ETH, p50 0.099894;
- 13 депозитов ровно по **0.099893702843840096 ETH** на 13 разных адресов. На L1 они попали в блоки 26097236..26097263, это 324 с. В L2 вошли за 0.29 с: 11:54:11Z, блоки 77354057..77354081, вперемешку с другими отложенными сообщениями. Это всплеск часа 11 (18 штук).
- **Предполагается:** это одна раздача кошелькам одного владельца. Сумма одинакова до wei, но L1-отправителей между собой не сверял. Для графа финансирования это кандидат в «флот».

| seq | L1-блок | requestId | to | сумма, ETH |
|---|---|---|---|---|
| 77285531 | 26096683 | 340191 | 0x7b5ba3c268bb600e48bb5aaed5f9f701adc2c066 | 0.099911 |
| 77296973 | 26096768 | 340226 | 0x4bf18a4e3ab26c0151fc33b4fba1fc2aea6c92cd | 0.099905 |
| 77308278 | 26096854 | 340265 | 0xcf79672b3fe1c10592b0018b85f85f2360ca5eaf | 0.099911 |
| 77316047 | 26096938 | 340297 | 0x1cef191074f4782978025a23df2d2d75157d7643 | 0.099912 |
| 77323647 | 26096991 | 340320 | 0x321bb3c1c228c1fe676d16001056d2fe46110792 | 0.007404 |
| 77327372 | 26097022 | 340336 | 0x349eaf4735405cd7610ddc42babfc37ebb1228d5 | 0.099910 |
| 77338855 | 26097109 | 340376 | 0xa9fe1b64b6117dcbabd3d9c169b9f78dac908cfb | 0.099906 |
| 77346452 | 26097176 | 340400 | 0x321bb3c1c228c1fe676d16001056d2fe46110792 | 0.103176 |
| 77346458 | 26097191 | 340406 | 0x3f0898bcc0e81271207dbbe5fae2a74e73c85e5b | 0.099912 |
| 77354057…77354081 (13 шт.) | 26097236…26097263 | 340424…340446 | 13 разных адресов | по 0.099894 |

Полный список — в `kind12.json`.

**kind 9 (49 штук):**
- 44 — от одного L1-отправителя на один `retryTo`, селектор 0x97a97cd4, `callvalue` 0. Смысл не выяснял;
- 5 — с `callvalue` > 0, всего 115.0066 ETH. Четыре с пустыми данными: 0.0067, 0.01, 0.356 и 10.508 ETH. Одно на 104.126 ETH через `finalizeInboundTransfer`.

**kind 13 (195 штук):**
- все от batch poster `0xdaa526086787d9debe1d7f3ffdb1fe50cf8687f4`;
- `batchNum` идут подряд: 296100..296294.

### RPC — проверено (2026-10-01, публичный RPC из `.env`, с Mac)

- Скрипт `…/scratchpad/014/rpc_check.py`. Жёсткий лимит 20 вызовов: счётчик в `rpc_calls.txt`, увеличивается до отправки, план проверяется до старта.
- Не чаще 1 вызова в 1.1 с, User-Agent `hoodchain-indexer-014/1`, при 429/403 скрипт останавливается.
- **Использовано 16 вызовов из 20.** Все HTTP 200, ни одного 429/403.
- Сырые ответы — `rpc_raw/`, сводка — `rpc_check.json`.

**5 из 5 блоков kind 12 совпали:** 77285531, 77323647, 77346452, 77346458, 77354057. По 2 вызова на блок: `eth_getBlockByNumber(full)` + `eth_getBlockReceipts`.

- В блоке ровно 2 tx: индекс 0 — `0x6a` StartBlock, индекс 1 — **`0x64` ArbitrumDepositTx**.
- `blockHash` совпал с фидом (seq фида = номер L2-блока).
- `tx.to` = `to` из payload, `tx.value` = сумма из payload (до wei), `tx.from` = `header.sender` (alias), `tx.requestId` = `header.requestId`.
- Чек: `status` 0x1; `gasUsed`, `gasUsedForL1` и `cumulativeGasUsed` равны 0; **логов 0**. У tx `gas`, `gasPrice` и `nonce` равны 0, `input` = `0x`.
- Шапка блока: `miner` = `header.sender`, `nonce` = `delayedMessagesRead` фида. `l1BlockNumber` в RPC на 41–61 L1-блок больше `header.blockNumber`.
- Фикстура для будущего декодера: блок 77285531, tx `0x3f8e47b000a564834eb5c32a200b620ea02daa8e32ad48b1e29e1277dcb522c1`.

**kind 9 — 2 блока, 4 вызова: 77300695 и 77312169.**
- 3 tx: `0x6a`; `0x69` SubmitRetryable (`to` = `0x…6e`, `value` = 0, суммы в полях `depositValue`/`retryValue`, логи `TicketCreated` + `RedeemScheduled`); `0x68` RetryTx (`to` = `retryTo`, `value` = `callvalue` из payload до wei в 2 из 2, `status` 0x1).
- В 77312169 у `0x68` есть логи: mint токена `0x0bd7d308f8e1639fab988df18a8011f41eacad73` на шлюз `0x1d187c3e…f055` с передачей получателю, плюс `DepositFinalized` с topic1 `0xc02aaa39…6cc2`.
- **Предполагается:** это шлюз WETH, а `0x0bd7…` — WETH на L2. В `contracts.md` у WETH стоит `todo`. В реестр ничего не вносил, это работа contract-registrar.

**kind 13 — 1 блок, 2 вызова: 77285519.**
- 2 tx `0x6a`: StartBlock и `0x9998269e` (batchPostingReportV2). `gasUsed` 0, логов 0, `miner` = batch poster.
- Это закрывает открытый вопрос 006 про `0x9998269e` в блоке 713006.

### Таблица kind (итог; полная версия — в `data-model.md`, «L1-сообщения фида»)

| kind | Смысл | tx в L2-блоке после `0x6a` StartBlock | Что брать декодерам |
|---|---|---|---|
| 3 | L2Message (секвенсор) | пользовательские tx | как обычно |
| 9 | SubmitRetryable | `0x69` + авто-redeem `0x68` | `0x68` с `value` > 0 → вход ETH на `retryTo` (источник: unalias(`from`)); если вызывается шлюз — получатель токена из логов |
| 12 | EthDeposit | одна `0x64`, без логов, gasUsed 0 | вход ETH на `to`, сумма `value`, источник: unalias(`from`) (для EOA = `to`). Только из tx `0x64` или payload, в `logs` депозита нет |
| 13 | BatchPostingReport | `0x6a` `batchPostingReportV2` | ничего для торговли |
| 7 / 6, 8, 10, 11 | L2FundedByL1 / прочие | по коду: `0x64` + `0x65`/`0x66` / нет tx | не наблюдали за 2 ч |

### Что не получилось или не проверено

- Данные только за 2.05 ч, «сутки записи» ещё не набрались. Частоты в таблице — за 01.10 до 12:00Z. За полные часы kind 12 шёл по 3 и 18 в час, поэтому оценка «~4 в час» из 013 неустойчива.
- Не проверено: kind 7, 6, 8, 10, 11 (не встречались); отфильтрованный депозит (`FilteredFundsRecipient`); неудачный авто-redeem; смысл селектора 0x97a97cd4 у 44 сообщений kind 9.
- Сборка, тесты и clippy (`cargo build/test/clippy --workspace`) прошли. Код не менялся, прогон контрольный.

### Для data-auditor

- Локальные данные: `/private/tmp/claude-501/-Users-mihailshumilov-sites-my-crypto-atomic-arbitrage/a477d6fc-6af5-4e28-af3e-63445fb94d7a/scratchpad/014/`
  - `feed/` — 3 часовых файла;
  - `server.sha256` — хэши с сервера;
  - `nitro/` — исходники;
  - `analyze_kinds.py` / `.out`, `rpc_check.py`, `rpc_raw/`, `rpc_check.json`, `rpc_calls.txt` (= 16).
- Для независимой перепроверки RPC осталось 4 вызова до лимита задачи.

### Вопросы к Cowork и Михаилу

1. Передать contract-registrar адреса `0x1d187c3e…f055` (вероятно, L2-шлюз WETH) и `0x0bd7d308…ad73` (вероятно, WETH на L2) со статусом `observed`?
2. Декодер L1-входов (`0x64` / `0x68` → `funding_edges`) поставить отдельной задачей 1b? Фикстура уже есть (блок 77285531).
