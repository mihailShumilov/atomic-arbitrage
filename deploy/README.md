# deploy — recorder на сервере: от пустой машины до записи

Набор для сервера по решению `docs/decisions/0003-recorder-server.md` (вариант B, Hetzner или похожий провайдер; проверен на Ubuntu 24.04 и 26.04, сервер `hood-rec` — 26.04). Всё ставится скриптом `deploy/bootstrap.sh`, мониторинг и аудит работают по таймерам systemd, уведомления идут в journald и, если настроить, в Telegram.

Главные правила:
- **С IP сервера никаких тестовых подключений к фиду и ручных проверок** (`websocat`, `curl` на `feed.*`, второй recorder). Наблюдался бан около часа: 2026-09-30 ответ 403 с `Retry-After: 3600` (`docs/handoff/from-code/002-recorder-hardening.md`). Лимит «2 соединения с IP, третье получает 429» **не проверен**: это факт из потерянной сессии. Все эксперименты с фидом — только с Mac.
- Секреты (токен бота, ключ хранилища, URL провайдера RPC) живут только в `/etc/hoodchain/*.env` и `rclone.conf` на сервере. В репозитории — только `*.example`.
- Любая трата (сервер, хранилище для бэкапа, провайдер RPC) — решение Михаила. Сумма, период и ссылка на счёт вносятся в `docs/costs.md`.

## Что в папке

| Файл | Куда ставится | Что делает |
|---|---|---|
| `bootstrap.sh` | запускается из `/opt/hoodchain-mev/src` | идемпотентная подготовка чистой Ubuntu 24.04 или 26.04: пакеты, пользователь `hood`, каталоги, chrony, ufw, лимиты journald, скрипты, юниты, `PROGRAM` для mdadm (если mdadm стоит), smartd с нашим конфигом, часовой пояс `Etc/UTC`. Повторный прогон печатает `done: 0 change(s)` |
| `build-on-server.sh` | там же | сборка `recorder` и `enricher` на сервере от пользователя `hoodbuild` (`cargo build --release --locked`), установка в `/opt/hoodchain-mev/bin`, прошлые бинарники остаются как `*.prev`. Recorder не перезапускает |
| `recorder.service` | `/etc/systemd/system/` | сам recorder (задача 008: `RestartSec=120`, Close при остановке; задача 009: досылка по `Arbitrum-Requested-Sequence-Number`, Close и при idle-таймауте; задача 012: Close при ошибке записи, `--block-idle-timeout-secs 30`, пауза 120 с от конца прошлой сессии — флагов в юните не требует) |
| `healthcheck.sh`, `healthcheck.service`, `healthcheck.timer` | `/opt/hoodchain-mev/deploy/`, юниты | проверки раз в 5 мин, см. «Мониторинг» |
| `notify.sh`, `notify-failure@.service` | то же | отправка уведомлений: journald всегда, Telegram — если задан в `/etc/hoodchain/notify.env`. `notify-failure@` вызывается через `OnFailure=` у служебных юнитов |
| `mdadm-event.sh`, `mdadm-hood.conf` | `/opt/hoodchain-mev/deploy/`, `/etc/mdadm/mdadm.conf.d/hood.conf` | события `mdadm --monitor` (отказ диска RAID, деградация, конец синхронизации) → `notify.sh`, см. «Мониторинг → RAID» |
| `smartd-event.sh`, `smartd-hood.conf`, `smartd-hood.service.conf`, `smartd-test.conf` | `/opt/hoodchain-mev/deploy/`, `/etc/hoodchain/smartd.conf`, `/etc/systemd/system/smartmontools.service.d/hood.conf`, `/opt/hoodchain-mev/deploy/smartd-test.conf` | SMART дисков: smartd (пакет `smartmontools`) по нашему конфигу, предупреждения → `smartd-event.sh` → `notify.sh`, см. «Мониторинг → SMART» |
| `feed-audit-daily.sh`, `feed-audit.service`, `feed-audit.timer` | то же; скрипт аудита копируется в `/opt/hoodchain-mev/deploy/feed_audit.py` | в 00:10 UTC проверка прошлых суток через `feed-audit`, без RPC (`--rpc-sample 0`), с `--frame-secs` = `AUDIT_FRAME_SECS` (60, как у recorder). Отчёт кладётся в `/srv/hood/reports/feed-audit-YYYYMMDD.txt` |
| `backup.sh`, `backup.service`, `backup.timer` | то же | бэкап сырья через rclone. **Выключен**, пока хранилище не выбрано |
| `enricher-gaps.service`, `enricher-gaps.timer` | юниты | дозаливка дыр через `enricher --gaps` с обязательным `--max-calls`. **Выключен**, пока не выбран провайдер RPC (0002) |
| `journald-hood.conf` | `/etc/systemd/journald.conf.d/hood.conf` | журнал хранится на диске, до 2 ГБ и до 90 дней |
| `needrestart-hood.conf` | `/etc/needrestart/conf.d/hood.conf` | apt и unattended-upgrades не перезапускают recorder сами: каждый рестарт — простой ~2 мин (пауза 120 с после остановки), из них бэклог фида закрывает ~60–70 с, остальное — дыра |
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

1. Сервер: Ubuntu 24.04 или 26.04, **выделенный публичный IPv4**, диск ≥ 0.5 ТБ (только фид) или 1.5–2 ТБ (фид, история, блоки), трафик ≥ 3 ТБ/мес или без лимита (0003). Нужны IP и доступ root по ssh-ключу.
2. Telegram (по желанию, бесплатно): бот от @BotFather (токен) и chat id. Без них уведомления остаются только в journald, а до Михаила они не дойдут: **без Telegram мониторинг формально есть, но никого не будит.**
3. Позже, отдельными решениями: хранилище для бэкапа (п. «Бэкап») и провайдер RPC для дозаливки (0002).

## Runbook: от пустого сервера до записи

Команды с Mac помечены `mac$`, на сервере — `srv#` (под root). Вместо `<IP>` подставьте адрес сервера. В репозиторий его не записывать.

### 1. Первый вход и исходники

```bash
mac$ ssh root@<IP> 'cat /etc/os-release | head -2; nproc; free -g; df -h /; ip -4 addr show scope global'
#     Ubuntu 24.04/26.04? Есть ли публичный IPv4 на интерфейсе (не 10.x/172.16-31.x/192.168.x — иначе NAT)?
mac$ cd ~/sites/my/crypto/atomic-arbitrage
mac$ rsync -a --delete --include .env.example --exclude target --exclude data --exclude .env --exclude '.env*' \
       --exclude .idea --exclude '*.zip' ./ root@<IP>:/opt/hoodchain-mev/src/
```

`git clone` на сервере тоже подходит, но тогда серверу нужен deploy-ключ к приватному репозиторию. rsync проще и не оставляет на сервере ключей к GitHub. `.env` и `data/` не копируются. `--include .env.example` стоит перед исключениями намеренно: шаблон `.env.example` отслеживается git, и без него `--delete` убрал бы его на сервере, а `BUILD_INFO` показал бы `-dirty`.

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

Нужно ~4 ГБ RAM. При меньшем объёме сборка идёт в 1 поток; если её убьёт OOM, добавьте swap (`fallocate -l 4G /swapfile && chmod 600 /swapfile && mkswap /swapfile && swapon /swapfile`). Проверено в контейнерах ubuntu:24.04 и ubuntu:26.04 (arm64): rustup из архива Ubuntu (в 26.04 — 1.27.1, есть и для amd64), rustc 1.98.1, компиляция ~40 с на 14 ядрах. На x86_64-сервере ожидается так же, но это не проверялось. Если в дереве на сервере есть незакоммиченные правки отслеживаемых файлов, в `BUILD_INFO` будет `git=<коммит>-dirty`: для сверки «коммит = HEAD» копируйте на сервер только закоммиченное состояние.

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
| 5 мин | `systemctl start healthcheck; journalctl -u healthcheck -n 3 -o cat` | строка `healthcheck: unit=active feed_age_s=<60 feed_src=last_seq.txt ... unit=ok ban=ok feed=ok disk=ok`. В первую минуту, пока `last_seq.txt` ещё нет, — `feed_src=hour_file` |
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
- [ ] `chronyc tracking`: `Leap status : Normal`, `System time` — смещение меньше 0.5 с (задачи 009, 012). Это нужно не только для `recv_unix_ns`: граница бэклога в строке `backlog` (задача 009) считается по правилу «отставание `header.timestamp` kind 3 от времени приёма < 2 с», и при ошибке часов больше 1–2 с статистика бэклога съезжает (данные и `gaps.tsv` от этого не зависят)
- [ ] `df -h /srv/hood`: свободно больше 80%
- [ ] `/etc/hoodchain/*.env` — `root:hood 0640`, в `/opt/hoodchain-mev/src` нет `.env`
- [ ] IP сервера записан **только** у Михаила, не в репозитории; на Mac, где идут тесты фида, этот IP не используется как прокси
- [ ] `docs/costs.md`: строка про сервер (сумма, период, ссылка на счёт)

## Мониторинг

`healthcheck.timer` запускает `healthcheck.sh` раз в 5 мин от пользователя `hood`, данные только читаются. На каждое состояние уходит **одно** уведомление при начале и одно «восстановлено: …» при окончании. Что уже отправлено, хранится в `/var/lib/hoodchain/health/*.alert`. Если уведомление не ушло (Telegram недоступен), состояние не записывается, и попытка повторится через 5 мин.

| Ключ | Условие | Порог (`/etc/hoodchain/healthcheck.env`) |
|---|---|---|
| `unit` | `systemctl is-active recorder` ≠ `active` (в том числе пауза `RestartSec` после падения) | — |
| `feed` | mtime `last_seq.txt` старше порога, то есть новые блоки не доходят до диска (recorder переписывает файл только при росте seq, на каждом закрытии фрейма, не реже раза в 60 с). Файлы текущего и прошлого часа берутся, **только если `last_seq.txt` ещё нет** (первые минуты на пустой папке): ping без блоков тоже пишутся в часовой файл, поэтому его свежесть ничего не говорит о блоках (задача 012, З1 из отзыва 011). Если файл часа свежий, а `last_seq.txt` старый, в тексте алерта будет подсказка «только ping?». В «восстановлено» её нет (задача 014: пока блоки идут, файл часа свежий всегда, и подсказка там была ложной). Источник виден в итоговой строке: `feed_src=last_seq.txt` / `hour_file`. Пока активен `ban`, отдельно не шлётся | `HC_FEED_MAX_AGE_S=300` |
| `ban` | последняя строка `connected`/`disconnected`/`startup_wait` в `connections.tsv` — отказ 4xx (403, 429) или `startup_wait pending_pause` от 600 с. Строки `backlog`, `client_close`, `shutdown`, `writer_error`, `torn_repair`, `gap_reconciled` не учитываются; `startup_wait min_connect_interval`, `disconnected idle_timeout` и `disconnected block_idle` баном не считаются | `HC_BAN_MIN_PAUSE_S=600` |
| `reconnects` | больше N строк `connected` за последний час (риск бана). Ловит и цикл переподключений по `block_idle`/`idle_timeout` | `HC_MAX_CONNECTS_PER_HOUR=6` |
| `disk` | заполнение файловой системы `/srv/hood/data` | `HC_DISK_MAX_PCT=80` |
| `writer` | в `connections.tsv` есть строка `shutdown writer_error` или `writer_error` (ошибка записи: диск, fsync; recorder вышел с кодом 2) моложе окна. «Восстановлено» — когда таких строк в окне не осталось, то есть через час без новых ошибок | `HC_WRITER_ERROR_WINDOW_S=3600` |
| `backfill` | в `gaps.tsv` есть дыры старше N часов, не покрытые `blocks/filled.tsv` (отставание дозаливки) | `HC_BACKFILL_MAX_LAG_H=24` |
| `clock` | chrony не синхронизирован или смещение больше порога | `HC_CLOCK_MAX_OFFSET_S=0.5` |
| `backup` | `last_ok` бэкапа старше N часов; выключено, пока бэкап не включён | `HC_BACKUP_MAX_AGE_H=0` → поставить 3 |
| `raid` | в `/proc/mdstat` массив с `_` в карте дисков (`[U_]`, `[_U]`) или `inactive` (в том числе когда активных массивов нет вовсе, задача 015). Текст — строки этих массивов из `/proc/mdstat`. «Восстановлено» — когда карта снова полная (`[UU]`). Нет `/proc/mdstat` или массивов — проверка пропускается (`raid=none`) | `HC_CHECK_RAID=1`, `HC_MDSTAT=/proc/mdstat` |
| `raid_sync` (событие) | на массиве начался resync/recovery/reshape: одно INFO с процентом и оставшимся временем (`finish=`), пока идёт — тишина, конец не шлётся (его сообщает mdadm `RebuildFinished`, а для деградации — «восстановлено» по `raid`). Ежемесячный `check` (mdcheck) не шлётся, виден в итоговой строке `raid_sync=…` | — |
| `smartd` | установлен `smartmontools`, а `systemctl is-active smartmontools.service` ≠ `active`. Сами SMART-предупреждения шлёт smartd (см. «SMART»), healthcheck только следит, что он работает | `HC_CHECK_SMARTD=auto` (`0` — выкл.), `HC_SMARTD_UNIT` |
| `gaps` (событие) | новые строки в `gaps.tsv`: число, сумма блоков и минут, самая длинная. От ~5 мин (2979 блоков) — ALERT, короче — INFO. «Восстановлено» для дыры — это уход условия `backfill` | `HC_GAP_ALERT_BLOCKS=2979` |

Кроме того:
- `feed-audit` в 00:10 UTC: при FAIL — ALERT, при PASS — короткая сводка INFO. Это ежедневный сигнал «жив», отключается `AUDIT_NOTIFY_PASS=0` в drop-in юнита;
- любой служебный юнит (healthcheck, feed-audit, backup, enricher-gaps), завершившийся с ошибкой, присылает «юнит … завершился с ошибкой» через `notify-failure@`. Не ошибка: код 3 у feed-audit (FAIL уже отправлен) и код 75 у enricher-gaps (исчерпан `--max-calls`, `SuccessExitStatus=75`); отставание дозаливки ловит `backfill`.

### RAID (задача 014)

На `hood-rec` 4 массива RAID1 (md0 swap, md1 `/boot`, md2 `/`, md3 `/home`). В `mdadm.conf` стоит `MAILADDR root`, но почтового агента нет, так что письма mdadm никуда не уходят. Поэтому события идут двумя путями:

- **`mdadm --monitor`** (`mdmonitor.service`; в Ubuntu 26.04 он `static`, его поднимает udev-правило массива, «включать» нечего) при каждом событии вызывает `PROGRAM` из `/etc/mdadm/mdadm.conf.d/hood.conf`, то есть `mdadm-event.sh СОБЫТИЕ /dev/md/N [диск]`:

  | Событие mdadm | Уведомление |
  |---|---|
  | `Fail`, `FailSpare`, `DegradedArray`, `DeviceDisappeared`, `SpareActive` | ALERT, со строками массива из `/proc/mdstat` |
  | `RebuildFinished`, `TestMessage` | INFO |
  | `RebuildStarted`, `Rebuild20/40/60/80`, `NewArray`, `MoveSpare`, `SparesMissing` | только журнал (`journalctl -t hood-mdadm`), без уведомления |

  `DegradedArray` повторяет и ежедневный `mdmonitor-oneshot.timer`. mdadm читает `PROGRAM` только при старте, поэтому bootstrap перезапускает `mdmonitor.service`, когда меняется `hood.conf`. Это перезапуск только опрашивающего процесса `mdadm --monitor`: массивы, идущий resync и recorder он не затрагивает.
- **healthcheck `raid`** раз в 5 мин читает `/proc/mdstat`. Он повторяет алерт, если первое уведомление не ушло, и присылает «восстановлено».

**Повторные ALERT при деградации — это нормально (замечание З4 ревью 014).** Пока массив деградирован, `mdmonitor-oneshot.timer` (на `hood-rec` раз в сутки, около 09:05 UTC) запускает `mdadm --monitor --scan --oneshot`, а тот через `PROGRAM` снова присылает ALERT «RAID деградирован: /dev/mdN». Это ежедневное напоминание, а не новый отказ. От healthcheck в это время приходит только одно ALERT `raid` в начале и одно «восстановлено» в конце. Напоминания прекратятся, когда массив снова станет полным (`[UU]`). Если заменённый диск ещё синхронизируется, напоминание может прийти и во время recovery.

Проверка цепочки mdadm → notify.sh (по одному INFO на массив, на `hood-rec` их 4, в журнал и в Telegram, если он настроен):

```bash
srv# mdadm --monitor --scan --oneshot --test            # TestMessage для каждого массива из mdadm.conf
srv# journalctl -t hood-notify -n 4 -o cat              # [INFO] …: RAID: тестовое сообщение mdadm для /dev/md/N
```

`MAILADDR root` остаётся, поэтому mdadm может написать в stderr, что не смог отправить письмо (почтового агента нет). На уведомления это не влияет.

Что делать при ALERT: не перезагружать сервер и не трогать recorder; `cat /proc/mdstat`, `mdadm --detail /dev/mdN`, `journalctl -u mdmonitor`, `journalctl -k`. Замена диска — заявка в Hetzner Robot (решение Михаила). Пока массив без второго диска, сырьё фида лежит в одном экземпляре.

### SMART (задача 015)

RAID-алерт приходит, когда диск уже выпал. SMART показывает диск, который начинает портиться: растут переназначенные или ожидающие сектора, падают самотесты.

- Пакет `smartmontools` ставит bootstrap. Демон — `smartmontools.service` (алиас `smartd.service`). Свой конфиг лежит в `/etc/hoodchain/smartd.conf` (из `deploy/smartd-hood.conf`). Drop-in `smartmontools.service.d/hood.conf` добавляет к штатному `ExecStart` ключ `-c`. Пакетный `/etc/smartd.conf` — conffile dpkg, его не трогаем, чтобы обновления пакета не спрашивали про конфиг.
- Конфиг — одна строка `DEVICESCAN` для всех дисков (на `hood-rec` это 2 × Seagate ST4000NM0245, SATA):
  - `-a` — статус здоровья, пороги и изменения атрибутов, журналы ошибок и самотестов, ожидающие (197) и неисправимые (198) сектора;
  - `-R 5!` — любой рост `Reallocated_Sector_Ct` даёт предупреждение;
  - `-W 0,50,55` — запись в журнал от 50 °C, предупреждение от 55 °C;
  - самотесты: короткий каждый день в 05:00 UTC, длинный в субботу в 01:00 UTC, второй диск на 8 ч позже (`:008`), чтобы длинный тест не шёл на обоих дисках сразу. Длинный тест 4-ТБ диска идёт несколько часов (точное время — `smartctl -c`, «Extended self-test routine recommended polling time»). Ежемесячный mdcheck (Hetzner: 26-е число, 04:59 UTC) может совпасть с субботним тестом. Это только замедлит оба процесса;
  - `-m <nomailer> -M exec smartd-event.sh` — почты нет, предупреждение уходит в `notify.sh`; `-M diminishing` — повтор через 1, 2, 4, 8… суток, пока проблема держится.
- `smartd-event.sh` по `SMARTD_FAILTYPE`: `EmailTest` → INFO, всё остальное → ALERT: `Health`, `Usage` (в том числе рост переназначенных секторов), `CurrentPendingSector`, `OfflineUncorrectableSector`, `SelfTest`, `ErrorCount`, `Temperature`, `FailedOpenDevice`, `FailedReadSmart*`. В тексте — сообщение smartd, модель диска, номер повтора, `smartctl -x /dev/sdX`. Скрипт ничего не пишет в stdout/stderr (smartd записал бы это в журнал как ошибку) и всегда завершается с кодом 0. Если уведомление не ушло, это видно в `journalctl -t hood-smartd`.
- bootstrap перезапускает smartd, только если изменился конфиг или drop-in. После правки drop-in делается `daemon-reload` (перечитать файлы юнитов). Он ничего не перезапускает, юниты набора не меняются. Recorder, RAID и mdmonitor это не затрагивает. В VM и в контейнере юнит пропускается (`ConditionVirtualization=no`). Тогда healthcheck присылает ALERT `smartd`.
- **Не запускайте длинный тест руками (`smartctl -t long`), пока в `/proc/mdstat` идёт resync или recovery**: тест и синхронизация замедлят друг друга. Короткий тест (`-t short`, ~2 мин) безвреден. smartd не начинает тесты при первом опросе после старта, и перезапуск тестов не запускает.

Проверка цепочки smartd → notify.sh (по одному INFO на диск, на `hood-rec` их 2):

```bash
srv# smartd -q onecheck -s - -c /opt/hoodchain-mev/deploy/smartd-test.conf   # -M test для каждого диска, работающий демон не трогается
srv# journalctl -t hood-notify -n 2 -o cat     # [INFO] …: SMART: тестовое сообщение smartd для /dev/sdX [SAT]
srv# smartctl -H /dev/sda; smartctl -H /dev/sdb   # PASSED
srv# smartd -q showtests -s - -c /etc/hoodchain/smartd.conf | grep 'will do test 1 of type'   # расписание самотестов
```

Что делать при ALERT SMART: сервер не перезагружать, recorder не трогать. Посмотреть `smartctl -x /dev/sdX` и `cat /proc/mdstat`. Одиночный рост переназначенных секторов — повод следить. Рост ожидающих или неисправимых секторов, проваленный самотест или `Health` — повод заменить диск, пока RAID1 ещё полный. Замена — заявка в Hetzner Robot с выводом `smartctl -x` (решение Михаила).

### Часовой пояс

С задачи 014 сервер в `Etc/UTC` (bootstrap ставит его сам). Recorder пишет метки в UTC/нс, скрипты используют `date -u`, таймеры набора заданы с `UTC`. Смена пояса меняет только вид времени в `journalctl` и `systemctl list-timers` и ничего не перезапускает.

Чего этот мониторинг **не** ловит:
- залипание «соединение живо, но только ping» (З1 из отзыва 011) теперь закрыто с двух сторон (задача 012): recorder сам переподключается через 30 с без блоков (`block_idle`), а `feed` смотрит на `last_seq.txt`. Остаётся слепое пятно короче порога: залипания до 5 мин алерта не дают, они видны как `block_idle` в `connections.tsv` и как дыра, если бэклога не хватило;
- сервер целиком упал или пропала сеть. Тогда некому слать уведомления. Для этого нужен внешний «пульс» (dead-man switch), например check на healthchecks.io: `HC_HEARTBEAT_URL` в `healthcheck.env`, пинг на каждом прогоне. Это сторонний аккаунт, решение за Михаилом. Без него узнать о падении сервера можно только по отсутствию ежедневной сводки feed-audit.

Ручной просмотр:

```bash
journalctl -u healthcheck -n 20 -o cat      # итоговая строка каждого прогона
journalctl -t hood-notify --since today     # все уведомления
ls /var/lib/hoodchain/health/               # активные алерты (*.alert)
```

## Обновление бинарника без лишней дыры

```bash
mac$ rsync -a --delete --include .env.example --exclude target --exclude data --exclude .env --exclude '.env*' --exclude .idea --exclude '*.zip' ./ root@<IP>:/opt/hoodchain-mev/src/
srv# bash /opt/hoodchain-mev/src/deploy/build-on-server.sh   # запись идёт, бинарник подменяется rename
srv# bash /opt/hoodchain-mev/src/deploy/bootstrap.sh         # если менялись deploy/ или юниты (идемпотентно)
srv# systemctl restart recorder                              # SIGTERM → Close 1000 → fsync → старт нового
srv# journalctl -u recorder -f
```

Что ожидаемо после `restart` (по коду recorder и задаче 012; вживую на сервере не проверялось):
- остановка занимает 0–2.5 с: Close 1000, ожидание ответного Close до 2 с, закрытие TLS до 0.5 с, затем дописать очередь, закрыть фрейм, fsync, `last_seq.txt`. В `connections.tsv` — `shutdown`, затем `client_close server_replied`. `TimeoutStopSec=30` с большим запасом;
- `systemctl restart` не ждёт `RestartSec` (он только для падений), но новый процесс **сам ждёт ~120 с после остановки**: `--min-connect-interval-secs 120` с задачи 012 считается **от конца прошлой сессии** (позднее из двух: последняя строка `connections.tsv` любого типа после последнего `connected` — `shutdown`, `client_close`, `disconnected`…, и mtime последнего часового файла данных; второе важно после `kill -9`, когда строк конца сессии нет). В журнале и в `connections.tsv` — `startup_wait min_connect_interval` с остатком паузы. Это защита от бана (решение Cowork по вопросу 1 задачи 009), не ошибка. **Не запускайте `restart` повторно**: каждый новый старт снова отсчитает 120 с от конца предыдущего;
- отдельный `stop && sleep 120 && start` больше не нужен: `restart` делает то же самое сам. Пока процесс ждёт, юнит `active`, healthcheck не шлёт «recorder не работает»; `feed` (порог 300 с) за ~120 с паузы плюс ≤ 60 с до первого фрейма тоже не срабатывает;
- после паузы новый процесс просит у фида `last_seq + 1` (заголовок `Arbitrum-Requested-Sequence-Number`, задача 009; в `connections.tsv` — `connected … requested=<N> mode=header`, затем строка `backlog`). Фид досылает бэклог примерно за последние 60–70 с (два замера 005 и один 009, глубина может меняться). Простой ~120 с длиннее бэклога, поэтому **каждый рестарт даёт одну строку в `gaps.tsv` на ~50–60 с** (~500–600 блоков; в 009: простой 121 с → дыра 578 блоков вместо ~1200). healthcheck пришлёт INFO «новые дыры в фиде», дозаливка — через enricher-gaps.

Обновляйте бинарник пачкой изменений, а не по одному: цена каждого рестарта — около минуты дыры.

### Обновление только скриптов `deploy/` (без рестарта recorder)

Если менялись только скрипты и конфиги `deploy/` (healthcheck, notify, mdadm, smartd), а `recorder.service` и бинарник те же:

```bash
mac$ rsync -a --delete --include .env.example --exclude target --exclude data --exclude .env --exclude '.env*' --exclude .idea --exclude '*.zip' ./ root@<IP>:/opt/hoodchain-mev/src/
srv# chown -R hoodbuild:hoodbuild /opt/hoodchain-mev/src   # rsync с Mac ставит владельцем uid Mac
srv# bash /opt/hoodchain-mev/src/deploy/bootstrap.sh         # ставит изменённые файлы
srv# systemctl show recorder -p NRestarts -p ActiveEnterTimestamp -p MainPID   # должно совпасть с тем, что было до
```

bootstrap кладёт файл, только если он отличается (`cmp`). `daemon-reload` он делает, только если изменился какой-то юнит или drop-in smartd, и даже тогда ничего не перезапускает. Отдельно перезапускаются только `mdmonitor` и smartd, когда меняются их конфиги. recorder он никогда не стартует и не перезапускает. healthcheck подхватит новый скрипт на следующем запуске таймера.

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

Обновление ОС: unattended-upgrades ставит обновления безопасности, но recorder не перезапускается (`needrestart-hood.conf`). Перезагрузку сервера (`reboot`) делайте вручную, когда удобно. После неё recorder стартует сам и выждет остаток 120 с от конца прошлой сессии (последняя строка `connections.tsv`, обычно `shutdown` при остановке перед перезагрузкой). Если перезагрузка заняла больше 120 с, пауза не нужна и подключение сразу. Простой длиннее бэклога (~60–70 с) даст строку в `gaps.tsv`.

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
- `--max-calls 4000` — жёсткий бюджет вызовов на прогон, повторы включены, 2 вызова на блок. Это 2000 блоков за прогон и не больше 96 000 вызовов в сутки. При исчерпании enricher выходит с кодом **75** (задача 012). В юните `SuccessExitStatus=75`: это не ошибка, уведомления «юнит … завершился с ошибкой» нет, и при большой дозаливке оно не приходит каждый час. Прогресс сохраняется в `filled.tsv`, следующий прогон продолжит. Настоящие ошибки (код 1: RPC, разбор, нет `enricher.env`) по-прежнему уведомляют через `notify-failure@`. Если дозаливка не успевает, через 24 ч придёт `backfill` от healthcheck. Отличить «бюджет исчерпан» от «всё залито» можно только по журналу enricher (`journalctl -u enricher-gaps -n 50`, WARN вида `<что>: call budget exhausted: N calls sent, M more would exceed --max-calls 4000; stopping with exit code 75, the next run continues`). Остатка работы в этом сообщении нет: его показывает строка `gaps plan` (`todo_blocks`) следующего прогона или `enricher --gaps … --dry-run` (план без вызовов RPC). По systemd не отличить: systemd 255 после успешного oneshot показывает `ExecMainStatus=0` и для кода 75 (проверено в контейнере 2026-10-01).
- Если recorder дописывает строку `gaps.tsv` в момент чтения, последняя строка может быть без `\n`. С задачи 012 enricher такую строку пропускает с WARN в журнале и заливает остальные; следующий прогон возьмёт её целиком. Ошибка в строке, которая заканчивается `\n`, по-прежнему останавливает прогон (код 1, уведомление) — тогда смотреть `gaps.tsv` руками.
- Без `/etc/hoodchain/enricher.env` юнит падает намеренно. Иначе enricher молча взял бы публичный RPC. Плейсхолдер `YOUR-PROVIDER` тоже отвергается (проверено в контейнере).
- Включение: создать `enricher.env` (`RPC_URL=…`, `root:hood 0640`), при необходимости поправить бюджет через `systemctl edit enricher-gaps.service`, затем `systemctl enable --now enricher-gaps.timer`. Цену провайдера при выбранном бюджете — в `docs/costs.md`.

## Поведение recorder (задачи 008, 009)

### Почему `RestartSec=120`

2026-09-30 переподключение через 11 с после `kill -9` получило бан IP: `403 Forbidden` с `Retry-After: 3600`, то есть час дыры. Перед этим 429 не было. Причина не установлена; гипотезы — частота подключений или обрыв без close-фрейма. Поэтому (задача 008):
- systemd перезапускает упавший recorder не раньше чем через 120 с (`RestartSec=120`);
- recorder и сам не подключается раньше чем через 120 с после **конца** сессии предыдущего запуска (`--min-connect-interval-secs 120`, задача 012). Конец — позднее из двух: время последней строки `connections.tsv` любого типа после последнего `connected` и mtime последнего часового файла данных. Второе нужно после kill -9 или пропажи питания: строк конца сессии тогда нет, а последняя строка — ранняя `backlog` в начале сессии. Ждёт дольшее из двух: остаток паузы из `disconnected` и `120 − (сейчас − конец)`. Если в `connections.tsv` нет ни одного `connected` (новая папка, скопированные данные без журнала), интервал не ждётся, как до 012. Это действует и при ручном `systemctl restart`, который `RestartSec` не ждёт. После падения сначала проходит `RestartSec=120`, к этому моменту интервал обычно уже выдержан;
- когда соединение заканчивает сам recorder, он отправляет WebSocket Close 1000 и ждёт ответный Close до 2 с (кадры, пришедшие за это время, пишутся в сырьё), а уже потом закрывает TLS/TCP. С задачи 009 это не только остановка (`"recorder shutdown"`), но и idle-таймаут (`"idle timeout"`, нет кадров 10 с) и пропажа writer (`"recorder writer gone"`). С задачи 012 ещё два случая: нет блоков (`block_idle`, см. ниже) и ошибка writer'а (диск, fsync) — перед выходом с кодом 2 recorder посылает Close 1000 и ждёт ответ до 2 с, а не обрывает соединение, как при kill -9. Итог пишется в `connections.tsv` событием `client_close` (`server_replied`, `no_reply`, `stream_ended`, `send_failed`). При idle-таймауте и `block_idle` за ним идёт `disconnected`; при остановке и при ошибке writer'а `disconnected` нет: сначала строка `shutdown` (`SIGTERM`/`SIGINT` или `writer_error`), затем `client_close`, затем выход (0 или 2).

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
| `connections.tsv` | События `connected`, `backlog`, `client_close`, `disconnected`, `startup_wait`, `shutdown`, `writer_error` (с 012), `torn_repair`, `gap_reconciled`: причина, HTTP-код, `Retry-After`, выбранная пауза, число страйков, `detail`. Первая строка — заголовок, 11 столбцов (`.claude/skills/hoodchain-mev/references/data-model.md`). healthcheck читает столбцы 1–7 по позиции, поэтому новые поля добавляются только в `detail` (последний столбец). |
| `_torn/` | Оборванные хвосты zstd, отрезанные при старте после аварийного завершения. Хранятся для разбора, recorder их не удаляет. |

Строки с `seq_first = seq_last = 0` (конверты без `messages`, ping и прочие кадры в обёртке `recorderFrame`) — норма, отбрасывать их — задача разбора (`.claude/skills/hoodchain-mev/references/data-model.md`).

### Поведение при сбоях

- **`systemctl stop` / SIGTERM.** Close 1000 → ожидание ответа до 2 с → закрытие TLS до 0.5 с → дописать очередь, закрыть фрейм, fsync, `last_seq.txt` → выход 0. Если SIGTERM пришёл во время паузы, `startup_wait` или установки соединения — выход сразу. `TimeoutStopSec=30`, обычно хватает 0–2.5 с (внутренний предел сети — 5 с).
- **Idle-таймаут** (нет кадров 10 с): Close 1000 `"idle timeout"` → `client_close` → `disconnected idle_timeout` → пауза 1–5 с → переподключение с `requested=<last_seq+1>` из памяти. Короткий обрыв закрывается бэклогом без дыры.
- **Нет блоков при живом соединении** (задача 012, флаг `--block-idle-timeout-secs`, по умолчанию 30): ping и прочие кадры идут, а сообщений с seq > 0 нет дольше порога. Recorder отправляет Close 1000 `"block idle timeout"`, пишет `client_close` и `disconnected` с причиной `block_idle`, затем переподключается с досылкой (`requested=<last_seq+1>`). Пауза — не 1–5 с, а «осторожная» лестница: 5 → 10 → 20 → 40 → 80 → 160 → 300 с (±20 % джиттера, потолок 5 мин); сбрасывается после сессии от 10 мин или обычного закрытия здоровой сессии. Первое залипание: 30 с + 5 с паузы — меньше бэклога (~60–70 с), дыры обычно нет. Устойчивое залипание даёт до ~11 подключений в час, это поймает `reconnects` (порог 6) в healthcheck, а `feed` — по `last_seq.txt`. `--block-idle-timeout-secs 0` выключает проверку. Порог меняется drop-in'ом юнита (`ExecStart=` с `--block-idle-timeout-secs N`). Сильно уменьшать не стоит: распределение естественных пауз между блоками на этом фиде не измерялось, а лишние переподключения — риск бана.
- **Ошибка записи** (диск полон, fsync): строка `shutdown writer_error` (текст ошибки в `detail`) → Close 1000 `"recorder writer error"` (≤ 2 с) → `client_close` → выход с кодом 2, строки `disconnected` нет. Если writer упал на финальном commit уже после SIGTERM, пишется `writer_error final_commit`, код тоже 2. systemd перезапустит через `RestartSec=120`. healthcheck пришлёт `writer` (по строке в `connections.tsv`), `unit`, если прогон попал в эти 120 с, при полном диске — `disk`, а если падения повторяются — `feed` и `reconnects` (~1 подключение в 2 мин). Перезапуск без места на диске снова упадёт: сначала освободить место (сырьё не удалять, пока оно не в бэкапе).
- **kill -9, падение, пропало питание.** Теряется только открытый фрейм, то есть не больше последних 60 с. При следующем старте оборванный хвост уходит в `_torn/`. Простой попадает в `gaps.tsv` одной строкой, когда придёт первый новый блок.
- **Паузы переподключения.** Обычное закрытие: 1–5 с. 429: `Retry-After`, а без него 5 → 10 → 20 → 40 → 60 мин. 403 или отказ апгрейда: 15 → 30 → 60 мин, но не меньше `Retry-After`. Сетевые ошибки и 5xx: от 5 с до 5 мин. Сессия дольше 10 мин сбрасывает лестницу.
- **Пауза переживает перезапуск.** При старте recorder ждёт дольшее из двух: остаток паузы из последней строки `disconnected` и остаток 120 с от конца прошлой сессии (задача 012, см. «Почему `RestartSec=120`»; раньше считалось от `connected`, и рестарт после долгой сессии подключался сразу). Обойти можно `--ignore-pending-pause`, но только если точно известно, что бан снят.
- **Сверка дыр при старте.** Разрыв в двух последних часовых файлах, которого нет в `gaps.tsv`, дописывается туда, с событием `gap_reconciled`.

## Проверка набора локально (без сервера)

Всё ниже выполняется на Mac в Docker. Порты не публикуются. Хосты фида, RPC и Telegram в тестовом контейнере направлены на 127.0.0.1, recorder не запускается.

```bash
# shellcheck и синтаксис
docker run --rm --network none -e LANG=C.UTF-8 -v "$PWD/deploy":/mnt:ro koalaman/shellcheck:stable -x \
  $(cd deploy && ls *.sh test/*.sh | sed "s#^#/mnt/#")
for f in deploy/*.sh deploy/test/*.sh; do bash -n "$f"; done

# офлайн-тесты healthcheck, notify, mdadm-event и smartd-event (подставные данные, /proc/mdstat и окружение smartd, без сети); повторить с ubuntu:26.04
docker run --rm --network none -v "$PWD/deploy":/deploy:ro ubuntu:24.04 bash /deploy/test/test-healthcheck.sh
docker run --rm --network none -v "$PWD/deploy":/deploy:ro ubuntu:24.04 bash /deploy/test/test-notify.sh
docker run --rm --network none -v "$PWD/deploy":/deploy:ro ubuntu:24.04 bash /deploy/test/test-mdadm-event.sh
docker run --rm --network none -v "$PWD/deploy":/deploy:ro ubuntu:24.04 bash /deploy/test/test-smartd-event.sh

# bootstrap дважды под настоящим systemd, verify юнитов, тест бэкапа, chrony, ufw,
# часового пояса, PROGRAM для mdadm, smartd (конфиг, drop-in, smartd_warning.sh -> notify.sh -> journald)
bash deploy/test/run-systemd-container.sh                  # ubuntu:24.04; --build — проверить и сборку
bash deploy/test/run-systemd-container.sh --ubuntu 26.04   # как на сервере hood-rec
docker rmi hood-deploy-test-systemd:24.04 hood-deploy-test-systemd:26.04   # образы стенда после проверки
```

### Ubuntu 26.04: что отличается (проверено 2026-10-01 в контейнере ubuntu:26.04 и осмотром `hood-rec`)

- `stat`, `date`, `du`, `install`, `sort`, `tail` и другие — из uutils (Rust coreutils 0.8.0), `cp` — GNU. Все флаги, которые используют скрипты набора (`stat -c '%U:%G:%a'`/`%Y`, `date -u -d @N`/`-d yesterday`, `sort -V`, `install -D -o -g -m`, `tail -c`/`-n +N`, `head -c`, `df -P`, `touch -d`), дают тот же вывод, что GNU. Правок не понадобилось.
- `sudo` — sudo-rs. Набор `sudo` не использует (всё под root, пользователи — через `runuser`).
- systemd 259, chrony 4.8, ufw 0.36.2 (iptables-nft), needrestart 3.11, Python 3.14, rustup 1.27.1 в архиве. Формат `chronyc -n tracking` прежний, `hood.conf` для needrestart разбирается, recorder исключён.
- ssh запускается через `ssh.socket`; `sshd -T` возвращает порт, bootstrap берёт его оттуда. После правки `/etc/ssh/sshd_config.d/*.conf`: `sshd -t`, затем `systemctl reload ssh`.
- mdadm 4.5: `mdmonitor.service` — `static`, запускается udev-правилом (`SYSTEMD_WANTS+="mdmonitor.service"`), `ExecStart=/usr/sbin/mdadm --monitor --scan`; плюс `mdmonitor-oneshot.timer` (ежедневно). `PROGRAM` и `MAILADDR` по замыслу пакета задаются в `mdadm.conf`; файлы `/etc/mdadm/mdadm.conf.d/*.conf` mdadm читает (проверено 2026-10-01 в контейнере: без `MAILADDR` mdadm пишет «No mail address or alert command - not monitoring», с `hood.conf` — нет; файл без `.conf` не читается).
- smartmontools 7.5 (24.04: 7.4): юнит `smartmontools.service` (алиас `smartd.service`), `Type=notify`, `ExecStart=/usr/sbin/smartd -n $smartd_opts`, `ConditionVirtualization=no`. Пакет при установке включает и запускает его со штатным `/etc/smartd.conf`. Предупреждения smartd идут через `/usr/share/smartmontools/smartd_warning.sh`: при `-m <nomailer>` он делает `exec` нашего скрипта со stdin `/dev/null` и `PATH=/usr/local/bin:/usr/bin:/bin`, вывод скрипта smartd пишет в журнал как ошибку (проверено 2026-10-01 в контейнерах 24.04 и 26.04 и по исходникам smartmontools 7.5).
- В Docker `chrony.service` на 26.04 пропускается (`ConditionVirtualization=!container`). Тестовый стенд снимает это условие только внутри контейнера, на сервере chrony работает штатно.
