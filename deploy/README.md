# deploy — recorder на сервере: от пустой машины до записи

Набор для сервера по решению `docs/decisions/0003-recorder-server.md` (вариант B, Ubuntu 24.04, Hetzner или похожий провайдер). Всё ставится скриптом `deploy/bootstrap.sh`, мониторинг и аудит работают по таймерам systemd, уведомления идут в journald и, если настроить, в Telegram.

Главные правила:
- **С IP сервера никаких тестовых подключений к фиду и ручных проверок** (`websocat`, `curl` на `feed.*`, второй recorder). Наблюдался бан около часа: 2026-09-30 ответ 403 с `Retry-After: 3600` (`docs/handoff/from-code/002-recorder-hardening.md`). Лимит «2 соединения с IP, третье получает 429» **не проверен**: это факт из потерянной сессии. Все эксперименты с фидом — только с Mac.
- Секреты (токен бота, ключ хранилища, URL провайдера RPC) живут только в `/etc/hoodchain/*.env` и `rclone.conf` на сервере. В репозитории — только `*.example`.
- Любая трата (сервер, хранилище для бэкапа, провайдер RPC) — решение Михаила. Сумма, период и ссылка на счёт вносятся в `docs/costs.md`.

## Что в папке

| Файл | Куда ставится | Что делает |
|---|---|---|
| `bootstrap.sh` | запускается из `/opt/hoodchain-mev/src` | идемпотентная подготовка чистой Ubuntu 24.04: пакеты, пользователь `hood`, каталоги, chrony, ufw, лимиты journald, скрипты, юниты. Повторный прогон печатает `done: 0 change(s)` |
| `build-on-server.sh` | там же | сборка `recorder` и `enricher` на сервере от пользователя `hoodbuild` (`cargo build --release --locked`), установка в `/opt/hoodchain-mev/bin`, прошлые бинарники остаются как `*.prev`. Recorder не перезапускает |
| `recorder.service` | `/etc/systemd/system/` | сам recorder (задача 008: `RestartSec=120`, Close при остановке; задача 009: досылка по `Arbitrum-Requested-Sequence-Number`, Close и при idle-таймауте — флагов в юните не требует) |
| `healthcheck.sh`, `healthcheck.service`, `healthcheck.timer` | `/opt/hoodchain-mev/deploy/`, юниты | проверки раз в 5 мин, см. «Мониторинг» |
| `notify.sh`, `notify-failure@.service` | то же | отправка уведомлений: journald всегда, Telegram — если задан в `/etc/hoodchain/notify.env`. `notify-failure@` вызывается через `OnFailure=` у служебных юнитов |
| `feed-audit-daily.sh`, `feed-audit.service`, `feed-audit.timer` | то же; скрипт аудита копируется в `/opt/hoodchain-mev/deploy/feed_audit.py` | в 00:10 UTC проверка прошлых суток через `feed-audit`, без RPC (`--rpc-sample 0`), с `--frame-secs` = `AUDIT_FRAME_SECS` (60, как у recorder). Отчёт кладётся в `/srv/hood/reports/feed-audit-YYYYMMDD.txt` |
| `backup.sh`, `backup.service`, `backup.timer` | то же | бэкап сырья через rclone. **Выключен**, пока хранилище не выбрано |
| `enricher-gaps.service`, `enricher-gaps.timer` | юниты | дозаливка дыр через `enricher --gaps` с обязательным `--max-calls`. **Выключен**, пока не выбран провайдер RPC (0002) |
| `journald-hood.conf` | `/etc/systemd/journald.conf.d/hood.conf` | журнал хранится на диске, до 2 ГБ и до 90 дней |
| `needrestart-hood.conf` | `/etc/needrestart/conf.d/hood.conf` | apt и unattended-upgrades не перезапускают recorder сами: каждый рестарт — дыра от 2 мин |
| `etc/*.example` | `/etc/hoodchain/*.example` | шаблоны конфигов; настоящие файлы создаются руками |
| `test/` | — | офлайн-тесты набора и образ для локальной проверки (см. последний раздел) |

Раскладка на сервере:

```
/opt/hoodchain-mev/src        исходники (rsync с Mac), владелец hoodbuild
/opt/hoodchain-mev/bin        recorder, enricher, *.prev, BUILD_INFO (root)
/opt/hoodchain-mev/deploy     скрипты, feed_audit.py, этот README (root)
/srv/hood/data/feed           сырьё фида (hood)
/srv/hood/data/blocks, logs   сырьё RPC (hood)
/srv/hood/reports             ежедневные отчёты feed-audit (hood)
/etc/hoodchain                notify.env, healthcheck.env, backup.env, rclone.conf, enricher.env (root:hood 0640)
/var/lib/hoodchain/health     состояние healthcheck: какие алерты уже отправлены
/var/lib/hoodchain/backup     last_ok — время последнего удачного бэкапа
```

## Что нужно от Михаила

1. Сервер: Ubuntu 24.04, **выделенный публичный IPv4**, диск ≥ 0.5 ТБ (только фид) или 1.5–2 ТБ (фид, история, блоки), трафик ≥ 3 ТБ/мес или без лимита (0003). Нужны IP и доступ root по ssh-ключу.
2. Telegram (по желанию, бесплатно): бот от @BotFather (токен) и chat id. Без них уведомления остаются только в journald, а до Михаила они не дойдут: **без Telegram мониторинг формально есть, но никого не будит.**
3. Позже, отдельными решениями: хранилище для бэкапа (п. «Бэкап») и провайдер RPC для дозаливки (0002).

## Runbook: от пустого сервера до записи

Команды с Mac помечены `mac$`, на сервере — `srv#` (под root). Вместо `<IP>` подставьте адрес сервера. В репозиторий его не записывать.

### 1. Первый вход и исходники

```bash
mac$ ssh root@<IP> 'cat /etc/os-release | head -2; nproc; free -g; df -h /; ip -4 addr show scope global'
#     Ubuntu 24.04? Есть ли публичный IPv4 на интерфейсе (не 10.x/172.16-31.x/192.168.x — иначе NAT)?
mac$ cd ~/sites/my/crypto/atomic-arbitrage
mac$ rsync -a --delete --exclude target --exclude data --exclude .env --exclude '.env*' \
       --exclude .idea --exclude '*.zip' ./ root@<IP>:/opt/hoodchain-mev/src/
```

`git clone` на сервере тоже подходит, но тогда серверу нужен deploy-ключ к приватному репозиторию. rsync проще и не оставляет на сервере ключей к GitHub. `.env` и `data/` не копируются.

### 2. Bootstrap

```bash
srv# bash /opt/hoodchain-mev/src/deploy/bootstrap.sh
#     если sshd слушает не 22: добавить --ssh-port <порт> (иначе он берётся из sshd -T)
#     Docker/ClickHouse не нужны для записи; при надобности: --with-docker
```

Ожидаемо: пакеты, пользователь `hood`, каталоги, юниты, `ufw: Status: active`, `chrony: synchronised`. В конце будет WARN «recorder not found: timers not enabled yet»: бинарника ещё нет. **Не закрывайте текущую ssh-сессию**, пока не проверите вход из второго окна (`mac$ ssh root@<IP> true`): включён ufw.

### 3. Сборка

```bash
srv# bash /opt/hoodchain-mev/src/deploy/build-on-server.sh
srv# cat /opt/hoodchain-mev/bin/BUILD_INFO
```

Нужно ~4 ГБ RAM. При меньшем объёме сборка идёт в 1 поток; если её убьёт OOM, добавьте swap (`fallocate -l 4G /swapfile && chmod 600 /swapfile && mkswap /swapfile && swapon /swapfile`). Проверено в контейнере ubuntu:24.04 (arm64): rustup из архива Ubuntu, rustc 1.98.1, компиляция ~36 с на 14 ядрах. На x86_64-сервере ожидается так же, но это не проверялось.

Почему не кросс-компиляция с Mac: под Linux нужен свой линкер или `cross`/`zig`, это новые инструменты. Сборка на сервере использует ровно `Cargo.lock` (`--locked`), а Rust ставится одним пакетом.

### 4. Уведомления (до старта recorder)

```bash
srv# install -o root -g hood -m 0640 /etc/hoodchain/notify.env.example /etc/hoodchain/notify.env
srv# nano /etc/hoodchain/notify.env        # TELEGRAM_BOT_TOKEN, TELEGRAM_CHAT_ID
srv# runuser -u hood -- /opt/hoodchain-mev/deploy/notify.sh info "тест уведомления"
#     сообщение пришло в Telegram? В журнале: journalctl -t hood-notify -n 5
```

Chat id узнавайте на Mac (написать боту, затем открыть `https://api.telegram.org/bot<TOKEN>/getUpdates`), а не скриптом на сервере. Токен не вставляйте в команды, которые попадут в history: только редактором в файл.

### 5. Таймеры и старт recorder

```bash
srv# bash /opt/hoodchain-mev/src/deploy/bootstrap.sh     # теперь включит healthcheck.timer и feed-audit.timer
srv# systemctl enable --now recorder
srv# journalctl -u recorder -f
```

### 6. Первые 15 минут записи

| Когда | Что смотреть | Норма |
|---|---|---|
| 0–1 мин | `journalctl -u recorder -n 50` | `connected`, затем раз в минуту `frame committed`. На самом первом старте `startup_wait` нет: `connections.tsv` ещё пуст |
| 1–2 мин | `cat /srv/hood/data/feed/last_seq.txt` дважды с паузой 60 с | число растёт примерно на 600 в минуту (~9.93 блока/с) |
| 2 мин | `tail -n 3 /srv/hood/data/feed/connections.tsv` | строка `connected` с кодом 101 и `detail` вида `wss://… requested=- mode=no_data` (папка была пустая, заголовок досылки не отправлялся), за ней одна строка `backlog done`; ни одной `disconnected`. После любого следующего старта — `requested=<last_seq+1> mode=header` |
| 2 мин | `ls -la /srv/hood/data/feed/$(date -u +%Y/%m/%d)/` | файл `feed-YYYYMMDD-HH.tsv.zst` растёт примерно на 1.5 МБ/мин (90–97 МБ/ч по 001/002) |
| 5 мин | `systemctl start healthcheck; journalctl -u healthcheck -n 3 -o cat` | строка `healthcheck: unit=active feed_age_s=<60 ... unit=ok ban=ok feed=ok disk=ok` |
| 5 мин | Telegram | при включении таймеров (до старта recorder) healthcheck прислал «recorder не работает» и «фид молчит»; через ≤ 5 мин после старта — «восстановлено» по обоим. Это и есть проверка канала уведомлений |
| 10 мин | `zstd -t` по файлу прошлого часа (если час сменился) | OK |
| 15 мин | `cat /srv/hood/data/feed/gaps.tsv` | пусто (дыр нет) |
| 15 мин | `python3 /opt/hoodchain-mev/deploy/feed_audit.py --feed-root /srv/hood/data/feed --rpc-sample 0 --frame-secs 60 "/srv/hood/data/feed/$(date -u +%Y/%m/%d)/feed-*.tsv.zst"` | `verdict PASS`, `blocks_per_s` ~9.9, открытый фрейм текущего часа в `open_tail` — это нормально. В первые ~2 мин нового часа недописанный фрейм прошлого часа тоже попадает в `open_tail` (`previous hour … frame may still be open`), это не FAIL. `--now` нужен только для разбора старых записей |

Если в `connections.tsv` появился `disconnected` с 403 или 429: **ничего не делать**. Recorder сам выждет `Retry-After`, а healthcheck пришлёт одно уведомление и потом «восстановлено». Не перезапускайте recorder и не проверяйте фид руками с этого IP.

### 7. Чек-лист перед уходом

- [ ] `systemctl is-enabled recorder healthcheck.timer feed-audit.timer` → `enabled` ×3
- [ ] `systemctl list-timers | grep -E 'healthcheck|feed-audit'`: ближайшие запуски есть
- [ ] `systemctl is-enabled backup.timer enricher-gaps.timer` → `disabled` (пока нет решений по хранилищу и провайдеру)
- [ ] тестовое уведомление дошло до Telegram (п. 4)
- [ ] `ufw status`: открыт только ssh; вход по ssh из нового окна работает
- [ ] `chronyc tracking`: `Leap status : Normal`, смещение меньше 0.5 с. Это нужно не только для `recv_unix_ns`: граница бэклога в строке `backlog` (задача 009) считается по правилу «отставание `header.timestamp` kind 3 от времени приёма < 2 с», и при ошибке часов больше 1–2 с статистика бэклога съезжает (данные и `gaps.tsv` от этого не зависят)
- [ ] `df -h /srv/hood`: свободно больше 80%
- [ ] `/etc/hoodchain/*.env` — `root:hood 0640`, в `/opt/hoodchain-mev/src` нет `.env`
- [ ] IP сервера записан **только** у Михаила, не в репозитории; на Mac, где идут тесты фида, этот IP не используется как прокси
- [ ] `docs/costs.md`: строка про сервер (сумма, период, ссылка на счёт)

## Мониторинг

`healthcheck.timer` запускает `healthcheck.sh` раз в 5 мин от пользователя `hood`, данные только читаются. На каждое состояние уходит **одно** уведомление при начале и одно «восстановлено: …» при окончании. Что уже отправлено, хранится в `/var/lib/hoodchain/health/*.alert`. Если уведомление не ушло (Telegram недоступен), состояние не записывается, и попытка повторится через 5 мин.

| Ключ | Условие | Порог (`/etc/hoodchain/healthcheck.env`) |
|---|---|---|
| `unit` | `systemctl is-active recorder` ≠ `active` (в том числе пауза `RestartSec` после падения) | — |
| `feed` | самый свежий mtime из `last_seq.txt` и файлов текущего и прошлого часа старше порога. Пока активен `ban`, отдельно не шлётся | `HC_FEED_MAX_AGE_S=300` |
| `ban` | последняя строка `connected`/`disconnected`/`startup_wait` в `connections.tsv` — отказ 4xx (403, 429) или `startup_wait pending_pause` от 600 с. Строки `backlog`, `client_close`, `shutdown`, `torn_repair`, `gap_reconciled` не учитываются; `startup_wait min_connect_interval` и `disconnected idle_timeout` баном не считаются | `HC_BAN_MIN_PAUSE_S=600` |
| `reconnects` | больше N строк `connected` за последний час (риск бана) | `HC_MAX_CONNECTS_PER_HOUR=6` |
| `disk` | заполнение файловой системы `/srv/hood/data` | `HC_DISK_MAX_PCT=80` |
| `backfill` | в `gaps.tsv` есть дыры старше N часов, не покрытые `blocks/filled.tsv` (отставание дозаливки) | `HC_BACKFILL_MAX_LAG_H=24` |
| `clock` | chrony не синхронизирован или смещение больше порога | `HC_CLOCK_MAX_OFFSET_S=0.5` |
| `backup` | `last_ok` бэкапа старше N часов; выключено, пока бэкап не включён | `HC_BACKUP_MAX_AGE_H=0` → поставить 3 |
| `gaps` (событие) | новые строки в `gaps.tsv`: число, сумма блоков и минут, самая длинная. От ~5 мин (2979 блоков) — ALERT, короче — INFO. «Восстановлено» для дыры — это уход условия `backfill` | `HC_GAP_ALERT_BLOCKS=2979` |

Кроме того:
- `feed-audit` в 00:10 UTC: при FAIL — ALERT, при PASS — короткая сводка INFO. Это ежедневный сигнал «жив», отключается `AUDIT_NOTIFY_PASS=0` в drop-in юнита;
- любой служебный юнит (healthcheck, feed-audit, backup, enricher-gaps), завершившийся с ошибкой, присылает «юнит … завершился с ошибкой» через `notify-failure@`.

Чего этот мониторинг **не** ловит:
- соединение живо, но идут только ping без блоков. `feed` берёт самый свежий mtime из `last_seq.txt` и часовых файлов, а ping тоже пишутся в часовой файл (фрейм закрывается раз в 60 с), и idle-таймаут recorder ping тоже сбрасывают. Такое залипание видно только по `last_seq.txt` (`cat` дважды с паузой) и по дыре после переподключения. Открытый вопрос к infra-ops/indexer-engineer, см. отзыв `docs/reviews/011-deploy-kit-indexer-engineer.md`;
- сервер целиком упал или пропала сеть. Тогда некому слать уведомления. Для этого нужен внешний «пульс» (dead-man switch), например check на healthchecks.io: `HC_HEARTBEAT_URL` в `healthcheck.env`, пинг на каждом прогоне. Это сторонний аккаунт, решение за Михаилом. Без него узнать о падении сервера можно только по отсутствию ежедневной сводки feed-audit.

Ручной просмотр:

```bash
journalctl -u healthcheck -n 20 -o cat      # итоговая строка каждого прогона
journalctl -t hood-notify --since today     # все уведомления
ls /var/lib/hoodchain/health/               # активные алерты (*.alert)
```

## Обновление бинарника без лишней дыры

```bash
mac$ rsync -a --delete --exclude target --exclude data --exclude .env --exclude '.env*' --exclude .idea --exclude '*.zip' ./ root@<IP>:/opt/hoodchain-mev/src/
srv# bash /opt/hoodchain-mev/src/deploy/build-on-server.sh   # запись идёт, бинарник подменяется rename
srv# bash /opt/hoodchain-mev/src/deploy/bootstrap.sh         # если менялись deploy/ или юниты (идемпотентно)
srv# systemctl restart recorder                              # SIGTERM → Close 1000 → fsync → старт нового
srv# journalctl -u recorder -f
```

Что ожидаемо после `restart` (проверено по коду recorder 2026-10-01, вживую на сервере не проверялось):
- остановка занимает 0–2.5 с: Close 1000, ожидание ответного Close до 2 с, закрытие TLS до 0.5 с, затем дописать очередь, закрыть фрейм, fsync, `last_seq.txt`. В `connections.tsv` — `shutdown`, затем `client_close server_replied`. `TimeoutStopSec=30` с большим запасом;
- `systemctl restart` не ждёт `RestartSec` (он только для падений). Пауза при старте — это `--min-connect-interval-secs 120`, и она **считается от последнего `connected`, а не от конца сессии**. Поэтому:
  - если recorder проработал больше 120 с (обычный случай), новый процесс подключается **сразу**, простой ~1–3 с;
  - если последний `connected` был меньше 120 с назад, новый процесс ждёт остаток (`startup_wait min_connect_interval` в журнале и в `connections.tsv`). Это защита от бана, не ошибка. Не запускайте `restart` повторно;
- новый процесс просит у фида `last_seq + 1` (заголовок `Arbitrum-Requested-Sequence-Number`, задача 009; в `connections.tsv` — `connected … requested=<N> mode=header`, затем строка `backlog`). Фид досылает бэклог примерно за последние 60–70 с (два замера 005 и один 009, глубина может меняться). Значит:
  - простой короче бэклога (немедленный рестарт) — дыры обычно нет, строки в `gaps.tsv` не появится, `backlog … first_minus_requested=0`;
  - простой длиннее бэклога — одна строка в `gaps.tsv`, короче простоя на размер бэклога (в 009: простой 121 с → дыра 578 блоков вместо ~1200). healthcheck пришлёт INFO «новые дыры в фиде».

Открытый вопрос (009, вопрос 1, решает Михаил/Cowork): безопасен ли немедленный реконнект после Close 1000. Бан 2026-09-30 был после реконнекта через 11 с после `kill -9` **без** Close; реконнект через 120 с после Close проверен один раз (009). Пока ответа нет, осторожный вариант обновления — выдержать паузу руками:

```bash
srv# systemctl stop recorder && sleep 120 && systemctl start recorder   # дыра ~50–60 с вместо ~0, зато интервал между подключениями ≥ 120 с
```

Во время `sleep` healthcheck может прислать «recorder не работает» и потом «восстановлено» — это ожидаемо.

Откат бинарника. `cp` поверх работающего бинарника падает с `Text file busy`, поэтому только через временный файл и `mv`:

```bash
srv# install -m 0755 /opt/hoodchain-mev/bin/recorder.prev /opt/hoodchain-mev/bin/recorder.new \
       && mv -f /opt/hoodchain-mev/bin/recorder.new /opt/hoodchain-mev/bin/recorder && systemctl restart recorder
```

Откат только досылки (бинарник 009 оставить): drop-in `systemctl edit recorder` с

```ini
[Service]
ExecStart=
ExecStart=/opt/hoodchain-mev/bin/recorder --out-dir /srv/hood/data/feed --no-requested-seq
```

затем `systemctl restart recorder`. В `connections.tsv` будет `mode=disabled`, поток начнётся с вершины, как до 009, и каждый рестарт снова даст дыру на весь простой.

Обновление ОС: unattended-upgrades ставит обновления безопасности, но recorder не перезапускается (`needrestart-hood.conf`). Перезагрузку сервера (`reboot`) делайте вручную, когда удобно. После неё recorder стартует сам; пауза `min_connect_interval` будет, только если последний `connected` был меньше 120 с назад (после долгой сессии её нет). Простой перезагрузки длиннее бэклога (~60–70 с) даст строку в `gaps.tsv`.

## Бэкап сырья (выключен до решения Михаила)

Сырьё фида невосполнимо: фид досылает только последние ~60–70 с (бэклог, задача 009), а RPC даёт блоки, но не время прихода (`recv_unix_ns`). Бэкап нужен вне сервера.

`backup.sh` использует rclone: обычные файлы на удалённой стороне можно читать без нашего софта, данные публичные и шифрования не требуют. Каждый час он копирует:
- закрытые часовые файлы фида (mtime старше 65 мин) и `_torn/`;
- `blocks/*.jsonl.zst`, `logs/*.jsonl.zst`;
- в `meta/` — `gaps.tsv`, `connections.tsv`, `last_seq.txt`, `filled.tsv`.

Копирование инкрементальное: уже скопированные файлы пропускаются. Файл текущего часа не копируется. На удалённой стороне ничего не удаляется (`rclone copy`, не `sync`). Если закрытый файл всё же изменился (ремонт хвоста после долгого простоя), старая копия сохраняется в `replaced/<время>/`. Проверка: `backup.sh --verify` (сверка размеров и хэшей).

Включение после выбора хранилища:

```bash
srv# install -o root -g hood -m 0640 /etc/hoodchain/backup.env.example /etc/hoodchain/backup.env   # BACKUP_REMOTE
srv# rclone config --config /etc/hoodchain/rclone.conf && chown root:hood /etc/hoodchain/rclone.conf && chmod 0640 /etc/hoodchain/rclone.conf
#     ключ Storage Box (если sftp) — /etc/hoodchain/storagebox_ed25519, root:hood 0640
srv# runuser -u hood -- /opt/hoodchain-mev/deploy/backup.sh && runuser -u hood -- /opt/hoodchain-mev/deploy/backup.sh --verify
srv# systemctl enable --now backup.timer
srv# echo 'HC_BACKUP_MAX_AGE_H=3' >> /etc/hoodchain/healthcheck.env
```

Оценка объёма: фид ~2.2 ГБ/сутки, ~65 ГБ/мес (chain-facts, 001/006). История с 04.08 по 0001 — 120–240 ГБ. 1 ТБ хватит примерно на год фида плюс историю. Рекомендация: учётные данные хранилища — с правом только на запись, без удаления (append-only), если провайдер так умеет. Тогда взлом сервера не уничтожит бэкап.

## Дозаливка дыр (выключена до выбора провайдера, 0002)

`enricher-gaps.service` — oneshot `enricher --gaps /srv/hood/data/feed/gaps.tsv --out-dir /srv/hood/data/blocks --max-calls 4000 --rps 2 --batch 10 --concurrency 1`, таймер раз в час.
- `--max-calls 4000` — жёсткий бюджет вызовов на прогон, повторы включены, 2 вызова на блок. Это 2000 блоков за прогон и не больше 96 000 вызовов в сутки. При исчерпании прогон завершается с ошибкой (придёт «юнит … завершился с ошибкой»). Прогресс сохраняется в `filled.tsv`, следующий прогон продолжит. Отдельного кода выхода у «бюджет исчерпан» пока нет (код 1, как у любой ошибки), поэтому при большой дозаливке «завершился с ошибкой» будет приходить каждый час; предложение — в отзыве `docs/reviews/011-deploy-kit-indexer-engineer.md`.
- Если recorder дописывает строку `gaps.tsv` в момент чтения, enricher может увидеть строку без второго столбца и завершить прогон с ошибкой (`gaps line N: missing column` / `not a number`), ничего не скачав. Неверный диапазон из недописанной строки получиться практически не может (см. отзыв). Следующий прогон пройдёт; если ошибка повторяется — строка испорчена, смотреть `gaps.tsv` руками.
- Без `/etc/hoodchain/enricher.env` юнит падает намеренно. Иначе enricher молча взял бы публичный RPC. Плейсхолдер `YOUR-PROVIDER` тоже отвергается (проверено в контейнере).
- Включение: создать `enricher.env` (`RPC_URL=…`, `root:hood 0640`), при необходимости поправить бюджет через `systemctl edit enricher-gaps.service`, затем `systemctl enable --now enricher-gaps.timer`. Цену провайдера при выбранном бюджете — в `docs/costs.md`.

## Поведение recorder (задачи 008, 009)

### Почему `RestartSec=120`

2026-09-30 переподключение через 11 с после `kill -9` получило бан IP: `403 Forbidden` с `Retry-After: 3600`, то есть час дыры. Перед этим 429 не было. Причина не установлена; гипотезы — частота подключений или обрыв без close-фрейма. Поэтому (задача 008):
- systemd перезапускает упавший recorder не раньше чем через 120 с (`RestartSec=120`);
- recorder и сам не подключается раньше чем через 120 с после последнего успешного подключения предыдущего запуска (`--min-connect-interval-secs 120`, время берётся из события `connected` в `connections.tsv`). Это действует и при ручном `systemctl restart`, который `RestartSec` не ждёт;
- когда соединение заканчивает сам recorder, он отправляет WebSocket Close 1000 и ждёт ответный Close до 2 с (кадры, пришедшие за это время, пишутся в сырьё), а уже потом закрывает TLS/TCP. С задачи 009 это не только остановка (`"recorder shutdown"`), но и idle-таймаут (`"idle timeout"`, нет кадров 10 с) и пропажа writer (`"recorder writer gone"`). Итог пишется в `connections.tsv` событием `client_close` (`server_replied`, `no_reply`, `stream_ended`, `send_failed`), перед `disconnected`.

### Досылка после переподключения (задача 009)

- При каждом подключении, если данные уже есть, recorder отправляет `Arbitrum-Feed-Client-Version: 2` и `Arbitrum-Requested-Sequence-Number: <last_seq+1>`. last_seq — из памяти процесса, при старте — из самих данных (не из `last_seq.txt`). Пустая папка — заголовков нет. По умолчанию включено, в юните флаг не нужен.
- В `connections.tsv`: `connected` с `detail` = `<url> requested=<N|-> mode=<header|no_data|disabled>`; одна строка `backlog` на соединение (`done` — пришёл первый живой кадр, `session_ended` — соединение кончилось раньше) с полями `requested`, `first_seq`, `first_minus_requested`, `backlog_blocks`, `live_seq`, `live_lag_ms` и др. Формат файла не менялся: те же 11 столбцов, новые данные — в `detail`.
- `first_minus_requested=0` — сервер начал ровно с запрошенного, дыры нет; больше 0 — запрошенное старше бэклога, остаток дыры в `gaps.tsv`.
- Граница бэклога считается по часам сервера (отставание kind 3 < 2 с), поэтому нужен chrony (п. 7 чек-листа).
- Отключение — `--no-requested-seq` (только для отката, см. «Обновление бинарника»).

### Что пишет recorder (`/srv/hood/data/feed`)

| Файл | Что внутри |
|---|---|
| `YYYY/MM/DD/feed-YYYYMMDD-HH.tsv.zst` | Сырьё: `recv_unix_ns \t seq_first \t seq_last \t <JSON>`. Внутри часового файла много zstd-фреймов: фрейм закрывается не реже раза в 60 с (`--frame-secs`), при ротации часа и при остановке. Читать `zstd -dc` или ридером, который понимает несколько фреймов. |
| `gaps.tsv` | `from \t to \t recv_ns` — пропущенные L2-блоки для дозаливки через RPC (enricher). Пишется только после fsync данных. |
| `last_seq.txt` | Последний seq, который уже на диске (fsync). Заменяется атомарно. |
| `connections.tsv` | События `connected`, `backlog`, `client_close`, `disconnected`, `startup_wait`, `shutdown`, `torn_repair`, `gap_reconciled`: причина, HTTP-код, `Retry-After`, выбранная пауза, число страйков, `detail`. Первая строка — заголовок, 11 столбцов (`.claude/skills/hoodchain-mev/references/data-model.md`). healthcheck читает столбцы 1–7 по позиции, поэтому новые поля добавляются только в `detail` (последний столбец). |
| `_torn/` | Оборванные хвосты zstd, отрезанные при старте после аварийного завершения. Хранятся для разбора, recorder их не удаляет. |

Строки с `seq_first = seq_last = 0` (конверты без `messages`, ping и прочие кадры в обёртке `recorderFrame`) — норма, отбрасывать их — задача разбора (`.claude/skills/hoodchain-mev/references/data-model.md`).

### Поведение при сбоях

- **`systemctl stop` / SIGTERM.** Close 1000 → ожидание ответа до 2 с → закрытие TLS до 0.5 с → дописать очередь, закрыть фрейм, fsync, `last_seq.txt` → выход 0. Если SIGTERM пришёл во время паузы, `startup_wait` или установки соединения — выход сразу. `TimeoutStopSec=30`, обычно хватает 0–2.5 с (внутренний предел сети — 5 с).
- **Idle-таймаут** (нет кадров 10 с): Close 1000 `"idle timeout"` → `client_close` → `disconnected idle_timeout` → пауза 1–5 с → переподключение с `requested=<last_seq+1>` из памяти. Короткий обрыв закрывается бэклогом без дыры.
- **kill -9, падение, пропало питание.** Теряется только открытый фрейм, то есть не больше последних 60 с. При следующем старте оборванный хвост уходит в `_torn/`. Простой попадает в `gaps.tsv` одной строкой, когда придёт первый новый блок.
- **Паузы переподключения.** Обычное закрытие: 1–5 с. 429: `Retry-After`, а без него 5 → 10 → 20 → 40 → 60 мин. 403 или отказ апгрейда: 15 → 30 → 60 мин, но не меньше `Retry-After`. Сетевые ошибки и 5xx: от 5 с до 5 мин. Сессия дольше 10 мин сбрасывает лестницу.
- **Пауза переживает перезапуск.** При старте recorder ждёт дольшее из двух: остаток паузы из последней строки `disconnected` и остаток 120 с с последнего `connected` (не с конца сессии: после долгой сессии рестарт подключается сразу). Обойти можно `--ignore-pending-pause`, но только если точно известно, что бан снят.
- **Сверка дыр при старте.** Разрыв в двух последних часовых файлах, которого нет в `gaps.tsv`, дописывается туда, с событием `gap_reconciled`.

## Проверка набора локально (без сервера)

Всё ниже выполняется на Mac в Docker. Порты не публикуются. Хосты фида, RPC и Telegram в тестовом контейнере направлены на 127.0.0.1, recorder не запускается.

```bash
# shellcheck и синтаксис
docker run --rm --network none -e LANG=C.UTF-8 -v "$PWD/deploy":/mnt:ro koalaman/shellcheck:stable -x \
  $(cd deploy && ls *.sh test/*.sh | sed "s#^#/mnt/#")
for f in deploy/*.sh deploy/test/*.sh; do bash -n "$f"; done

# офлайн-тесты healthcheck и notify (подставные данные, без сети)
docker run --rm --network none -v "$PWD/deploy":/deploy:ro ubuntu:24.04 bash /deploy/test/test-healthcheck.sh
docker run --rm --network none -v "$PWD/deploy":/deploy:ro ubuntu:24.04 bash /deploy/test/test-notify.sh

# bootstrap дважды под настоящим systemd в ubuntu:24.04, verify юнитов, тест бэкапа
bash deploy/test/run-systemd-container.sh            # добавить --build, чтобы проверить и сборку
```
