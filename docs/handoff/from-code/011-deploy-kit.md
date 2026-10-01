# 011 — Набор для развёртывания recorder на сервере. Отчёт
status: выполнено, все 7 пунктов; ждёт проверки indexer-engineer (пути, флаги, рестарт recorder)
date: 2026-10-01
executor: infra-ops
reviewers: indexer-engineer (не запускался, его запускает основная сессия); траты и доступ — Михаил

Статус в задаче не менял: по указанию основной сессии он остаётся `in-progress`. Коммитов нет, изменения лежат в рабочем дереве, в `deploy/` и в этом отчёте.

## Итог

- Готов набор «от пустой Ubuntu 24.04 до записи»: `bootstrap.sh`, `build-on-server.sh`, healthcheck раз в 5 мин с уведомлениями (journald и опционально Telegram), ежедневный `feed-audit`, бэкап через rclone, шаблон дозаливки дыр. Runbook — в `deploy/README.md`.
- Все критерии приёмки, кроме подтверждения indexer-engineer, проверены в Docker на Mac:
  - shellcheck по 10 скриптам: 0 замечаний;
  - `bash -n` ок;
  - `bootstrap.sh` под настоящим systemd в ubuntu:24.04: второй прогон — `0 change(s)` и ни одного изменённого файла;
  - `systemd-analyze verify` по всем юнитам: 0 ошибок;
  - healthcheck на подставных данных: 42 из 42 проверок, по каждому случаю ровно одно уведомление и одно «восстановлено».
- Ни одного подключения к фиду, ни одного вызова RPC, ни одного подключения к реальному серверу. Ничего не куплено и не зарегистрировано. Recorder ни разу не запускался, ни на Mac, ни в контейнере. Код recorder и enricher не менял.
- Трат нет, `docs/costs.md` не менял. Предложение по хранилищу для бэкапа с расчётом — в разделе «Вопросы», решение за Михаилом.

## Сделано

| Файл в `deploy/` | Пункт задачи | Что |
|---|---|---|
| `bootstrap.sh` | 1 | Идемпотентный, под root. Делает: пакеты (chrony ufw zstd python3 curl ca-certificates rclone; docker — только с `--with-docker`), пользователь `hood`, каталоги `/opt/hoodchain-mev/{bin,deploy}`, `/srv/hood/data/{feed,blocks,logs}`, `/srv/hood/reports`, `/etc/hoodchain`, `/var/lib/hoodchain`; chrony с проверкой синхронизации; ufw — только ssh (`limit`, порт из `sshd -T` или `--ssh-port`), без ssh-правила ufw не включается; лимиты journald; юниты и скрипты. Каждый шаг сначала проверяет, потом меняет; в конце — счётчик реальных изменений. Recorder не стартует; `backup.timer` и `enricher-gaps.timer` не включает никогда; `healthcheck.timer` и `feed-audit.timer` включает, только когда есть `/opt/hoodchain-mev/bin/recorder` |
| `build-on-server.sh` | 2 | Сборка на сервере: rustup из архива Ubuntu, toolchain stable minimal, `cargo build --release --locked -p recorder -p enricher`. Сборка идёт от отдельного пользователя `hoodbuild` без доступа к сырью. Установка через rename (работающий recorder остаётся на старом inode), прошлые бинарники — в `*.prev`, `BUILD_INFO` с sha256. Ничего не перезапускает. Новых зависимостей в workspace нет |
| `healthcheck.sh`, `healthcheck.{service,timer}` | 3 | Условия: `unit`, `feed` (mtime `last_seq.txt` и файлов текущего и прошлого часа, порог 300 с), `disk` (80%), `ban` (последняя строка подключения в `connections.tsv` — 4xx, или `startup_wait pending_pause` ≥ 600 с), `reconnects` (> 6 `connected` за час), `backfill` (дыры старше 24 ч не покрыты `filled.tsv`), `clock` (chrony), `backup` (выключено до включения бэкапа). Событие `gaps`: новые строки `gaps.tsv` — число, блоки, минуты, самая длинная; от 2979 блоков (~5 мин) ALERT, короче INFO. Не спамит: одно уведомление на начало состояния и одно «восстановлено: …» на конец. Если уведомление не ушло, состояние не сохраняется, и попытка повторится через 5 мин. Пока активен `ban`, `feed` отдельно не шлётся. Опционально `HC_HEARTBEAT_URL` — внешний «пульс», по умолчанию пусто |
| `notify.sh`, `notify-failure@.service` | 3 | Подключаемый notifier. journald (`hood-notify`) работает всегда, Telegram — если в `/etc/hoodchain/notify.env` есть `TELEGRAM_BOT_TOKEN` и `TELEGRAM_CHAT_ID`. Файл секретов разбирается, но не исполняется. Токен не попадает в argv (URL передаётся curl через stdin `-K -`) и маскируется в ошибках. `notify-failure@` висит на `OnFailure=` у служебных юнитов |
| `feed-audit-daily.sh`, `feed-audit.{service,timer}` | 4 | В 00:10 UTC (`Persistent=true`) по прошлым суткам UTC, `--rpc-sample 0`. Отчёт пишется в `/srv/hood/reports/feed-audit-YYYYMMDD.txt` и в журнал. FAIL или нет файлов — ALERT. Код выхода 3 означает «FAIL, уведомление отправлено» (`SuccessExitStatus=3`), поэтому второго уведомления через OnFailure нет. PASS — короткая сводка INFO как ежедневный сигнал «жив» (`AUDIT_NOTIFY_PASS=0` отключает) |
| `backup.sh`, `backup.{service,timer}` | 5 | rclone, только `copy`, на удалённой стороне ничего не удаляется. Копирует закрытые часовые файлы (mtime > 65 мин) и `_torn/`, `blocks`/`logs` `*.jsonl.zst`, а в `meta/` — `gaps.tsv`, `connections.tsv`, `last_seq.txt`, `filled.tsv`. Копирование инкрементальное. Если закрытый файл изменился, старая копия уходит в `replaced/<время>/`. `--verify` — сверка. С плейсхолдером `CHANGE-ME` не запускается. Таймер выключен |
| `README.md` | 6 | Runbook: что дать, шаги 1–7 с командами, таблица «первые 15 минут», чек-лист перед уходом, мониторинг, обновление бинарника без лишней дыры (build → `systemctl restart` → ожидаемая пауза 120 с), откат через `*.prev`, бэкап, дозаливка, локальная проверка набора. Разделы 008 о поведении recorder сохранены и сокращены |
| `enricher-gaps.{service,timer}` | 7 | Шаблон, выключен. `--max-calls 4000 --rps 2 --batch 10 --concurrency 1`, таймер раз в час, то есть ≤ 96 000 вызовов в сутки. `EnvironmentFile=/etc/hoodchain/enricher.env` обязателен (без `-`), плюс `ExecStartPre` отвергает пустой `RPC_URL` и плейсхолдер. Так юнит не может молча уйти на публичный RPC |
| `journald-hood.conf`, `needrestart-hood.conf` | 1 | Журнал хранится на диске, до 2 ГБ и до 90 дней. needrestart (apt, unattended-upgrades) не перезапускает recorder сам: в Ubuntu 24.04 он по умолчанию перезапускает службы после обновления библиотек, а каждый рестарт — дыра от 2 мин |
| `etc/*.example` | 3, 5, 7 | Шаблоны `notify.env`, `healthcheck.env`, `backup.env`, `rclone.conf`, `enricher.env` без значений. Ставятся как `/etc/hoodchain/*.example` (`root:hood 0640`) |
| `test/` | приёмка | `test-healthcheck.sh`, `test-notify.sh`, `test-backup.sh` (офлайн), `Dockerfile.systemd` и `run-systemd-container.sh` (полный сценарий под systemd) |

`recorder.service` не менял: пути и флаги совпадают с текущим CLI (`--out-dir /srv/hood/data/feed`, остальное по умолчанию, `--min-connect-interval-secs` 120).

## Проверено (2026-10-01, как)

Всё в Docker на Mac (Docker 29.4.2, arm64). Порты не публиковались: `docker port` пуст, это проверяется и в скрипте. Контейнеры назывались `hood-deploy-test-*` и удалены (`--rm` или удаление в конце сценария). В systemd-контейнере `feed.mainnet.chain.robinhood.com`, `delayed-feed.…`, `rpc.mainnet.chain.robinhood.com` и `api.telegram.org` направлены на 127.0.0.1 (`--add-host`). Офлайн-тесты шли с `--network none`. Сеть systemd-контейнера использовалась только для apt, rustup и crates.io.

1. **shellcheck** 0.11.0 (образ `koalaman/shellcheck:stable`, `--network none`, `-x`) по 10 скриптам (6 рабочих + 4 тестовых): 0 замечаний, rc=0. С `-o all -S style` остаются только стилистические SC2250 (фигурные скобки), SC2312 и SC2249; к критерию они не относятся.
2. **`bash -n`** по всем 10 скриптам внутри ubuntu:24.04 (bash 5.2): ок.
3. **bootstrap.sh под systemd.**
   - Образ — ubuntu:24.04 + `systemd systemd-sysv dbus` (`deploy/test/Dockerfile.systemd`), systemd работает как PID 1.
   - Запуск без `--privileged`: `--cap-add SYS_ADMIN --cap-add NET_ADMIN --cgroupns=host -v /sys/fs/cgroup:/sys/fs/cgroup:rw`, tmpfs для `/run`. Без rw-cgroup systemd не стартует («Failed to create /init.scope control group: Read-only file system»).
   - CAP_SYS_TIME не выдавался: chronyd в контейнере запускается Ubuntu с `-x` и часы ВМ не трогает (проверено по `ps`).

   | Прогон | Результат |
   |---|---|
   | 1 (чистая система) | 41 изменение, 1 WARN (бинарника нет, таймеры не включены). chrony `synchronised (0.0034 s slow)`, ufw `active`: `22/tcp LIMIT IN` (v4+v6), deny incoming / allow outgoing |
   | 2 | `done: 0 change(s)`; `find / -xdev -newer <метка>` вне /proc, /sys, /run, /tmp, /var/log, /var/lib/systemd, /var/cache — пусто |
   | 3 (после сборки) | 2 изменения: `healthcheck.timer`, `feed-audit.timer` enabled |
   | 4 | `0 change(s)`, файлы не менялись |

   Права проверены через `stat`: данные `hood:hood 750`, `/etc/hoodchain` и примеры `root:hood 750/640`. journald пишет в `/var/log/journal`. `recorder.service`, `backup.timer`, `enricher-gaps.timer` остались `disabled / inactive`. `journalctl -u recorder` — 0 строк, `ExecMainStartTimestamp` пуст.

   Финальный сценарий с нуля (`bash deploy/test/run-systemd-container.sh --build`) повторяет прогоны 1–4, verify, healthcheck, guard enricher и тест бэкапа. Его итог — в разделе «Цифры».
4. **build-on-server.sh в том же контейнере.**
   - Компиляция заняла 36.2 с.
   - Окружение: rustup из Ubuntu (rustc 1.98.1), 14 ядер, сборка из `git archive HEAD` (81b245f) плюс рабочая копия `deploy/`. Рабочее дерево `crates/` не брал: в нём параллельно идёт 009.
   - Оба бинарника прошли `--help` (clap, без сети).
   - Повторный запуск: `recorder: unchanged`, `enricher: unchanged`, `nothing to install`.
5. **`systemd-analyze verify`** по recorder, healthcheck (+timer), feed-audit (+timer), backup (+timer), enricher-gaps (+timer), `notify-failure@x`. До сборки сообщались только 2 строки «binary is not executable» (recorder, enricher), после сборки — 0 ошибок. Расписание `*-*-* 00:10:00 UTC` проверено через `systemd-analyze calendar`. `systemd-analyze security` для recorder, healthcheck и enricher-gaps: 7.4 MEDIUM.
6. **Сервисы под systemd (как `hood`, с песочницей юнитов).**
   - `systemctl start healthcheck.service` → в журнале `healthcheck: unit=inactive feed_age_s=-1 … clock=ok` и ровно 2 уведомления в `journalctl -t hood-notify`: «recorder не работает», «фид молчит». Повторный запуск — 0 новых. `systemctl is-active` и `chronyc` работают от `hood`.
   - `feed-audit.service` на копии `data/feed-test-002` (оригинал не тронут, скопирован `docker cp`) → PASS, `blocks=5657 gaps_tsv_entries=1 sessions=2 blocks_per_s=9.958 mb_per_hour=97.0`. Цифры совпадают с отчётом 008. Отчёт записан в `/srv/hood/reports/feed-audit-20260930.txt`, пришло одно INFO.
   - Тот же день с пустым `gaps.tsv` → FAIL «gap 76532007..76568582 is not listed», ровно 1 уведомление, Result юнита `success` (код 3). День без файлов → ALERT «FAIL (нет вердикта) … no such file».
   - `enricher-gaps.service` без `enricher.env` → `Result=resources`, с плейсхолдером из примера → `ExecStartPre` «RPC_URL not set», `Result=exit-code`. В обоих случаях enricher не исполнялся (0 строк `enricher[` в журнале), через `notify-failure@` пришло по одному уведомлению.
7. **healthcheck на подставных данных** (`deploy/test/test-healthcheck.sh`, ubuntu:24.04, `--network none`; `systemctl`, `chronyc`, `df` — шимы; notifier — подставной, считает каждое уведомление): **42 из 42**. По каждому случаю ровно одно уведомление, повторные прогоны молчат, затем одно «восстановлено»:

   | Случай | Начало | Повтор | Конец |
   |---|---|---|---|
   | тихий фид (mtime −600 с) | 1 ALERT | 0, 0 | 1 «восстановлено» |
   | 403 в `connections.tsv` | 1 ALERT | 0; при одновременно молчащем фиде — 0 (feed подавлен); после рестарта во время бана (`startup_wait pending_pause 3174 с`) — 0 | после `connected` — 1 |
   | 429 | 1 | — | 1 |
   | новая дыра 1200 блоков | 1 INFO | 0 | — (событие) |
   | две дыры, одна 4000 блоков | 1 ALERT на пачку | 0 | — |
   | старая незалитая дыра (−25 ч) | 1 событие дыры + 1 ALERT `backfill` | 0; залита наполовину — 0 | залита целиком — 1 |
   | диск 85% (порог 80) | 1 | 92% — 0 | 60% — 1 |
   | recorder `activating` | 1 | 0 | 1 |
   | chrony не синхронизирован | 1 | смещение 0.9 с — 0 | 1 |
   | 8 подключений за час | 1 | 0 | выпали из окна — 1 |
   | бэкап включён, `last_ok` нет | 1 | — | 1 |
   | notifier недоступен (диск 81%) | 0, в журнале «notify failed» | после восстановления notifier — 1 | 1 |

8. **notify.sh** (`test-notify.sh`, curl и logger — шимы, без сети): 14 из 14.
   - Без `notify.env` — одна строка в журнале (`user.crit`, многострочное тело склеено), curl не вызывается.
   - С Telegram токена нет в argv curl, URL с токеном уходит через stdin `-K -`, поля `chat_id` и `text` верные, в логе токена нет.
   - Ответ API `ok:false` → код 1 и запись в журнале.
   - Сетевая ошибка с URL в тексте → в журнале `bot***/sendMessage`, токена нет.
   - Ошибки использования → код 2.
9. **backup.sh** (`test-backup.sh`, rclone 1.60.1 из Ubuntu, «удалённое хранилище» — локальный каталог, от `hood`): 15 из 15.
   - Закрытые часы скопированы побайтно, открытый час не скопирован.
   - `_torn/` скопирован, а `*.tmp` и `*.partial` — нет.
   - `meta/` лежит отдельно от `feed/`.
   - Второй прогон ничего не копирует.
   - Изменённый закрытый час перезалит, старая копия лежит в `replaced/`.
   - Локальное удаление на удалённую сторону не переносится.
   - `--verify` ок, плейсхолдер `CHANGE-ME` → код 2.

## Цифры

| Показатель | Значение |
|---|---|
| shellcheck / bash -n | 0 замечаний по 10 скриптам / ок |
| bootstrap: изменений в прогонах 1 / 2 / 3 / 4 | 41 / 0 / 2 / 0 |
| systemd-analyze verify (после сборки) | 0 ошибок, 10 юнитов |
| healthcheck / notify / backup, офлайн-тесты | 42/42, 14/14, 15/15 |
| Компиляция на сервере (контейнер arm64, 14 ядер) | 36.2 с, rustc 1.98.1 |
| Бюджет enricher-gaps по умолчанию | 4000 вызовов/прогон = 2000 блоков, ≤ 96 000 вызовов/сутки |
| Подключения к фиду / вызовы RPC / реальные серверы | 0 / 0 / 0 |

Финальный прогон `bash deploy/test/run-systemd-container.sh --build` с нуля, на новом контейнере: **ALL PASS**, rc=0.
- bootstrap 41 → 0 изменений, второй прогон не изменил ни одного файла;
- сборка 34.4 с, затем bootstrap 2 → 0;
- verify всех юнитов чистый;
- healthcheck.service отработал от `hood`;
- enricher-gaps без `enricher.env` отказался стартовать;
- тест бэкапа 15/15;
- recorder `inactive`.

Предыдущий прогон этого сценария упал на последнем шаге (rc=126). Причина в самом тестовом скрипте: каталог из `mktemp -d` имеет права 0700, после `cp -a` пользователь `hood` не мог прочитать тесты. Исправлено (`chmod 755`), набор это не затрагивает. Контейнеры удалены; остались только образы `hood-deploy-test-systemd:24.04`, `ubuntu:24.04`, `koalaman/shellcheck:stable`.

## Что Михаил должен дать и сделать при получении сервера

1. **IP и доступ root по ssh-ключу.** Ubuntu 24.04, выделенный публичный IPv4, диск ≥ 0.5 ТБ (или 1.5–2 ТБ под историю), трафик ≥ 3 ТБ/мес (0003). IP сообщить лично, в репозиторий его не пишем.
2. **Telegram-бот** (бесплатно): токен от @BotFather и chat id. Вписать самому в `/etc/hoodchain/notify.env` на сервере (runbook, шаг 4). **Без этого алерты остаются в журнале и до Михаила не доходят.**
3. Пройти runbook `deploy/README.md`, шаги 1–7, или разрешить сессии infra-ops сделать это по ssh. Это отдельное разрешение на подключение к серверу.
4. Прислать сумму, период и ссылку на счёт за сервер — внесу в `docs/costs.md`.
5. Решить: хранилище для бэкапа (предложение ниже), внешний «пульс» (healthchecks.io или аналог), провайдер RPC (0002). После решений включить `backup.timer` и `enricher-gaps.timer` по README.

## Что не получилось и ограничения

- **Проверено на arm64, не на x86_64.** Контейнер на Mac — arm64. Скрипты от архитектуры не зависят. Сборка и rustup на x86_64-сервере предполагаются такими же, но не проверены.
- **systemd в контейнере ≠ сервер.** ufw включался в сетевом пространстве контейнера. chrony был в режиме `-x`, и «synchronised» — это измерение без подстройки часов. Реальный образ провайдера (cloud-init, свой sshd-порт, предустановленный firewall провайдера) не проверен. Runbook просит проверить вход по ssh из второго окна до выхода.
- **Конец к концу с recorder не проверялось.** Recorder нигде не запускался по условиям задачи, поэтому реакция healthcheck на настоящую запись проверена на подставных данных и на копии записи 002, а не вживую.
- **Сервер целиком упал — уведомить некому.** Это закрывает только внешний «пульс» (решение Михаила). Без него косвенный сигнал — отсутствие ежедневной сводки feed-audit.
- **Ежедневный feed-audit не проверяет стык суток** (последний seq прошлых суток → первый seq текущих). Дыра ровно на полуночи попадёт только в `gaps.tsv` и в healthcheck, не в отчёт аудита.
- **healthcheck читает `connections.tsv` по позициям столбцов 2–7** (`ts_unix_ns`, `event`, `reason`, `http_status`, `retry_after`, `pause_s`). Если 009 вставит новые столбцы раньше 7-го, разбор сломается (см. вопрос 1).

## Предполагается, не проверено

- Что `apt install rustup` на x86_64 Ubuntu 24.04 даёт тот же путь (`rustup run stable cargo`), что и на arm64.
- Что `sshd -T` на образе Hetzner вернёт фактический порт ssh. В 24.04 ssh активируется сокетом; генератор systemd берёт порт из `sshd_config`.
- Что needrestart в 24.04 перезапустил бы recorder при обновлении glibc. Исключение поставлено на всякий случай, само поведение не проверялось.
- Цены хранилищ ниже — по памяти, на 2026-10-01 не проверены.

## Вопросы к Cowork, Михаилу и indexer-engineer

1. **indexer-engineer, сверка с 009.**
   - (а) Новые поля досылки в `connected` — добавьте их в конец строки или в `detail`, не перед 7-м столбцом: healthcheck читает 2–7 по позиции.
   - (б) В юните флаги не нужны: `--no-requested-seq` по умолчанию выключен, верно?
   - (в) Close на idle-таймауте не меняет ничего в юните. `TimeoutStopSec=30` хватает с запасом?
   - (г) Пути `--out-dir /srv/hood/data/feed`, `ReadWritePaths` — подтвердить.
2. **indexer-engineer, enricher (код не менял, только вопрос).**
   - Нужен отдельный код выхода «исчерпан `--max-calls`», отличный от настоящей ошибки. Тогда юнит сможет считать его нормой (`SuccessExitStatus`), а при большой дозаливке не будет «юнит завершился с ошибкой» каждый час.
   - Терпит ли `enricher --gaps` недописанную последнюю строку `gaps.tsv`, если recorder дописывает её в момент чтения?
3. **Михаил, хранилище для бэкапа (трата, только предложение).**
   - Объём: фид ~2.2 ГБ/сутки ≈ 65 ГБ/мес (chain-facts, 001/006), история с 04.08 — 120–240 ГБ (0001). 1 ТБ хватит на ~12 мес. фида с историей или ~15 мес. только фида.
   - Варианты (цены по памяти, не проверены 2026-10-01): Hetzner Storage Box BX11, 1 ТБ — порядка €4/мес (~€48/год), sftp/rclone, тот же провайдер; Backblaze B2 — порядка $6/ТБ/мес, S3, другой провайдер (лучше на случай проблем с аккаунтом Hetzner); Cloudflare R2 — порядка $15/ТБ/мес, дороже без явной пользы.
   - Рекомендация: Storage Box BX11, если сервер у Hetzner, и учётные данные с правом без удаления, если провайдер так умеет. Перед заказом цены проверить на сайте.
4. **Михаил: внешний «пульс».** healthchecks.io (бесплатный тариф, нужна регистрация) или аналог — да или нет? Это единственный способ узнать, что сервер упал целиком.
5. **Cowork: пороги.** 0003 требует алерт при дыре > 5 мин. Сейчас каждая новая дыра присылает INFO (рестарт даёт ~2 мин), от 5 мин — ALERT. Каждое падение recorder даёт ALERT `unit` (пауза `RestartSec`) и через 5 мин «восстановлено». Ежедневная сводка PASS тоже приходит. Не много ли для Telegram? Всё отключается в `healthcheck.env` и drop-in юнита.
6. **Cowork: `HC_BACKFILL_MAX_LAG_H=24`.** Пока провайдер не выбран, первая же дыра через сутки поднимет алерт `backfill` и будет висеть до дозаливки. Это и есть видимость «незалитых дыр» по правилу 9 скилла, но алерт будет висеть долго. Оставить так?
