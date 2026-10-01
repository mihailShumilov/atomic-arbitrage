# 014 — Пункты 1–3 (RAID-алерт, UTC, текст healthcheck). Ревью indexer-engineer

date: 2026-10-01
reviewer: indexer-engineer
объект: коммит `a25e2d2` (`deploy/`), раздел «Пункты 1–3 (infra-ops)» отчёта `docs/handoff/from-code/014-server-housekeeping-kind12.md`, состояние `hood-rec` после обновления Михаилом (~13:15Z)
**вердикт: PASS** по пп. 1–3; recorder не затронут (п. 4 — PASS). Замечания З1–З4 не блокируют.

## Как проверял

- Код: `git show a25e2d2 -- deploy/`, полное чтение `deploy/mdadm-event.sh`, `deploy/mdadm-hood.conf`, разделов `feed` и `raid` в `deploy/healthcheck.sh`, блоков `mdadm` и `timezone` в `deploy/bootstrap.sh`, тестов. `git diff a25e2d2 -- deploy` пуст: рабочая копия совпадает с коммитом.
- Тесты на Mac в Docker, `--network none`, образ `ubuntu:24.04` (был на Mac до начала, оставлен). shellcheck — `koalaman/shellcheck:stable` (0.11.0), образ скачан для проверки и удалён. Контейнеры запускались с `--rm`, после проверки их нет.
- Свои дополнительные проверки (скрипт в scratchpad, на том же стенде, что `test-healthcheck.sh`): ALERT без подсказки при несвежем файле часа; `/proc/mdstat` только с inactive-массивом; inactive + здоровый массив.
- Сервер: `ssh -O check hood-rec` → master running. Три вызова `ssh hood-rec` через ControlMaster, все только на чтение: `systemctl show/cat/list-timers`, `timedatectl`, `ps`, `cat`, `grep`, `ls`, `stat`, `sha256sum`, `journalctl`, `awk` по `connections.tsv`. Записей, рестартов, установок, подключений к фиду и вызовов RPC нет. `healthcheck.service` руками не запускал: смотрел запуск от таймера. IP в выводе и здесь не указан.
- Снимки сервера: 13:19:13Z, 13:19:44Z и финальный (см. п. 4).

## 1. RAID-алерт — PASS

### Код

Проверено (2026-10-01, чтение кода и тесты):
- `mdadm-event.sh`:
  - ALERT: `Fail`, `FailSpare`, `DegradedArray`, `DeviceDisappeared`, `SpareActive`.
  - INFO: `RebuildFinished`, `TestMessage`.
  - Остальные события (в том числе `RebuildNN`) идут только в журнал под тегом `hood-mdadm`.
  - Скрипт всегда завершается с кодом 0. Если уведомление не ушло, это пишется в журнал.
  - Имя ядра (`md2`) выводится из `/dev/md/2` через `readlink -f`, а без симлинка — регулярным выражением. Выдержка из `/proc/mdstat` берётся только по нужному массиву, не больше 4 строк.
  - Требования задачи (Fail, DegradedArray, SpareActive — ALERT, RebuildFinished — INFO) выполнены.
- `mdadm-hood.conf`: одна строка `PROGRAM /opt/hoodchain-mev/deploy/mdadm-event.sh`, ставится как `/etc/mdadm/mdadm.conf.d/hood.conf`. Основной `mdadm.conf` не правится.
- Проверка `raid` в `healthcheck.sh`:
  - `_` в карте дисков или `inactive` → `raise` (одно уведомление, пока условие держится). Полная карта → `resolved` со «восстановлено».
  - resync/recovery/reshape → одно INFO на новый ключ `mdN:тип`. Ключи хранятся в `raid_sync.keys`, если уведомление не ушло — повтор на следующем запуске. Окончание синхронизации проходит молча.
  - `check` попадает только в итоговую строку журнала.
  - Переход md2 из `DELAYED` в идущий resync ключ не меняет (`md2:resync`), поэтому второго INFO не будет. Это правильно.
- `bootstrap.sh` идемпотентен:
  - drop-in ставится через `install_file` (cmp);
  - `mdmonitor` перезапускается только если `FILE_CHANGED` и он активен;
  - если массивы есть, а монитор не запущен, он запускается;
  - пояс меняется только если он не `Etc/UTC`;
  - `daemon-reload` не вызывается: юниты не менялись.

### Тесты

Проверено (2026-10-01, ubuntu:24.04, `--network none`):

| Набор | Результат |
|---|---|
| `test-mdadm-event.sh` | 20 passed, 0 failed |
| `test-healthcheck.sh` (включая раздел 16 «raid») | 100 passed, 0 failed |
| `test-notify.sh` | 14 passed, 0 failed |
| shellcheck 0.11.0: все `deploy/*.sh` и `deploy/test/*.sh` | 0 замечаний, rc=0 |

Ubuntu 26.04 и systemd-стенд (`run-systemd-container.sh`) я сам не гонял: стенду нужна сеть для apt. По этим прогонам опираюсь на отчёт infra-ops.

### Сервер

Проверено (2026-10-01 13:19Z, только чтение):
- `mdmonitor.service`: `static`, `active (running)`. MainPID 10385, процесс `/usr/sbin/mdadm --monitor --scan` запущен 13:15:21Z, то есть после установки drop-in (mtime `hood.conf` 13:15). В журнале юнита только штатные Stopping/Stopped/Started в 13:15:21Z, ошибок нет.
- `/etc/mdadm/mdadm.conf.d/hood.conf`: в каталоге только этот файл, root:root 644. sha256 совпадает с `src/deploy/mdadm-hood.conf`.
- sha256 установленных `healthcheck.sh`, `mdadm-event.sh`, `notify.sh` совпадают с `/opt/hoodchain-mev/src/deploy/`. `src` принадлежит `hoodbuild:hoodbuild`.
- `journalctl -t hood-notify` после 13:16:
  - 13:16:52Z — **4 INFO** «RAID: тестовое сообщение mdadm для /dev/md/{3,2,1,0}», у каждого своя выдержка из mdstat (у md3 — resync 88.8 %, у md2 — `resync=DELAYED`);
  - 13:16:17Z — одно INFO от healthcheck: «RAID: идёт синхронизация: md3 resync 88.6%, осталось ~37 мин; md2 resync ожидает (DELAYED)».
- Журнал `healthcheck` (запуск от таймера в 13:16:17Z): `raid: sync notified: md2:resync md3:resync`, итог `… raid=ok raid_sync=md3_resync_88.6%,_осталось_~37_мин;_md2_resync_ожидает_(DELAYED)`. Запуски до 13:15 ключа `raid` не содержат: это ещё старый скрипт.
- `/proc/mdstat` в 13:19Z: 4 × RAID1, все `[2/2] [UU]`. md3 resync 89.5 %, finish ≈ 33.9 мин. md2 `resync=DELAYED`.
- `journalctl -t hood-mdadm`: пусто. Так и должно быть: событий «только в журнал» не было.

Предполагается, не проверено:
- Что **работающий демон** (PID 10385) взял `PROGRAM`. Прямо это не видно. Косвенно: он стартовал после записи drop-in, и тот же бинарник с тем же конфигом в режиме `--oneshot --test` отработал цепочку. Первая живая проверка — INFO «RAID: синхронизация /dev/md/3 завершена» (`RebuildFinished`) примерно в 13:50–13:55Z. Михаилу стоит посмотреть `journalctl -t hood-notify -n 3` после этого времени. Если сообщения не будет — это дефект.
- Настоящие `Fail`/`DegradedArray` на этом железе не проверялись (в Docker нет md).

### Шум sendmail (З1, не блокирует)

- `sh: /usr/sbin/sendmail: not found` вывелся в терминал Михаила, потому что `--oneshot --test` запускался из ssh. В общем журнале с 13:00 строк `sendmail` нет. Доставку `PROGRAM` шум не ломает: все 4 тестовых сообщения дошли, при том что почта падала.
- mdadm шлёт почту только на `Fail`, `FailSpare`, `DegradedArray`, `SparesMissing`, `TestMessage`. У демона stderr уходит в журнал `mdmonitor`, а у `mdmonitor-oneshot.timer` — в журнал своего юнита. Значит, при настоящей деградации рядом с нашим алертом в журнале будет строка-ошибка. Это шум, а не потеря алерта.
- Переопределить `MAILADDR` из drop-in, скорее всего, нельзя. Предполагается по устройству mdadm (`config.c`, `mailline`: адрес берётся из первой встреченной строки `MAILADDR`, а `mdadm.conf.d` читается после основного файла). Вживую не проверял: на Mac нет mdadm, ставить его по сети ради этого не стал.
- **Предложение** (не к срочному обновлению, а к следующему плановому прогону bootstrap): в `bootstrap.sh` идемпотентно закомментировать строку `MAILADDR root` в `/etc/mdadm/mdadm.conf`, если на хосте нет `/usr/sbin/sendmail`. Например, заменить её на `#MAILADDR root  # hoodchain: no MTA, alerts via PROGRAM in mdadm.conf.d/hood.conf` и перезапустить `mdmonitor` по тому же правилу «только при изменении». Что учесть:
  - `mdadm.conf` в Ubuntu генерирует `mkconf` при установке, это не dpkg-conffile, и существующий файл при обновлении пакета не перезаписывается (предполагается, проверить на сервере по `dpkg-query -W -f='${Conffiles}' mdadm`);
  - копия в initramfs для сборки массивов от `MAILADDR` не зависит, пересобирать initramfs не нужно;
  - без `MAILADDR`, но с `PROGRAM` монитор работает: infra-ops проверил это в контейнере 26.04 (сообщение «No mail address or alert command» пропадает).

  Вариант «ничего не делать» тоже приемлем: вреда нет, только лишняя строка в журнале при отказе диска.

## 2. UTC — PASS

Проверено (2026-10-01 13:19Z):
- `timedatectl`: `Time zone: Etc/UTC (UTC, +0000)`, `System clock synchronized: yes`, `RTC in local TZ: no`. `/etc/localtime` → `../usr/share/zoneinfo/Etc/UTC` (mtime 13:15), `/etc/timezone` = `Etc/UTC`. Расхождение, которое было до обновления (`/etc/timezone` UTC, а `/etc/localtime` Berlin), устранено.
- Таймеры:
  - `healthcheck.timer`: `OnBootSec=3min`, `OnUnitActiveSec=5min`. Последний запуск 13:16:16Z, следующий 13:21:16Z, интервал 5 мин сохранился.
  - `feed-audit.timer`: `OnCalendar=*-*-* 00:10:00 UTC`, `Persistent=true`, следующий запуск Fri 2026-10-02 00:10:00 UTC.
  - Системные таймеры (`mdcheck_*`, `mdmonitor-oneshot`, `dpkg-db-backup`) теперь показаны в UTC. Это ожидаемо и безвредно.
- Метки recorder — в UTC:
  - текущий файл часа `feed-20261001-13.tsv.zst` (mtime 13:19:12Z) соответствует 13-му часу UTC;
  - в `connections.tsv` `ts_utc` и `ts_unix_ns` согласованы: `1790859631129663541` ns = 2026-10-01T13:00:31Z, в строке стоит `2026-10-01T13:00:31.129Z`; `1790848633282286683` = 09:57:13Z, в строке `09:57:13.282Z`.
  - Смена пояса в 13:15 на имена файлов и метки не повлияла: recorder пишет UTC сам и от пояса не зависит.

## 3. Текст healthcheck — PASS

Проверено (2026-10-01, код и тесты):
- В `healthcheck.sh` (раздел `feed`) подсказка «…блоков нет (только ping?)» добавляется только при условии `feed_age >= HC_FEED_MAX_AGE_S && hour_newest > 0 && now_s - hour_newest < HC_FEED_MAX_AGE_S`. То есть `last_seq.txt` устарел, а файл часа свежий. Только в этом случае `raise feed` (ALERT) несёт подсказку.
- Если `last_seq.txt` свежий, текст такой: «last_seq.txt обновлялся N с назад, last_seq=…». Он уходит в `resolved feed` и становится телом «восстановлено».
- `test-healthcheck.sh`, раздел 13:
  - ALERT при свежем файле часа и `last_seq.txt` возрастом 600 с содержит «только ping» — PASS;
  - «восстановлено» (файл часа свежий) не содержит ни «только ping», ни «блоков нет», а содержит «last_seq.txt обновлялся» — PASS.
- Моя доп. проверка:
  - `last_seq.txt` возрастом 600 с и файл часа возрастом 900 с → ALERT **без** подсказки — PASS;
  - тело «восстановлено» после этого: `last_seq.txt обновлялся 1 с назад, last_seq=76600000`.
- На сервере установлен именно этот скрипт (sha256 совпадает с `src`). Живых ALERT/«восстановлено» по `feed` после обновления не было, текст на сервере вживую не наблюдался.

## 4. Recorder не затронут — PASS

Проверено (2026-10-01, `systemctl show`, чтение файлов):

| Снимок | NRestarts | MainPID | ActiveEnterTimestamp | last_seq.txt | gaps.tsv |
|---|---|---|---|---|---|
| база координатора (до 13:15Z) | 0 | 7347 | 09:57:12Z | — | — |
| 13:19:13Z | 0 | 7347 | Thu 2026-10-01 09:57:12 UTC | 77404719 (mtime 13:19:04.76Z) | нет |
| 13:19:44Z | 0 | 7347 | Thu 2026-10-01 09:57:12 UTC | 77404719 | нет |
| 13:21:00Z (финальный) | 0 | 7347 | Thu 2026-10-01 09:57:12 UTC | 77405319 (mtime 13:20:05.03Z) | нет |

- Между 13:19:44Z и 13:21:00Z `last_seq` вырос на 600 (77404719 → 77405319). `last_seq.txt` переписывается на коммите фрейма, раз в ≤ 60 с, поэтому одинаковое значение в первых двух снимках ожидаемо.

- Запись идёт:
  - `last_seq` вырос с `live_seq=77284408` (09:57:13Z) до 77404719 (13:19:04Z): +120 311 блоков за 3 ч 22 мин ≈ 9.9 блока/с;
  - файл часа растёт (23.9 МБ к 13:19Z);
  - `healthcheck` показывает `feed=ok`, `reconnects=ok`, `writer=ok`, `gaps_rows=0`.
- `gaps.tsv` нет, то есть дыр не было.
- **Отклонение от ожидания «один connected»** (З2, не дефект и не следствие обновления). В `connections.tsv` две строки `connected`:
  - 13:00:25.918Z — `disconnected server_closed`, сессия 10 992.965 с, 109 173 конвертов;
  - 13:00:31.129Z — `connected requested=77393581`;
  - затем `backlog done`: 43 блока, `first_minus_requested=0`.

  Сервер фида закрыл соединение, recorder переподключился через 5.2 с (`rule=jitter_1_5s`) и забрал пропущенное из backlog без дыры. Процесс recorder при этом не перезапускался (MainPID и NRestarts те же). Произошло это за 15 минут до обновления (13:15Z), так что с пп. 1–3 не связано. Похоже на плановую ротацию соединений на стороне фида (~3 ч сессии). Это предположение: одного случая для вывода мало, стоит смотреть по следующим разрывам.

## Замечания

- **З1** (шум sendmail) — см. п. 1, предложение закомментировать `MAILADDR` через bootstrap или оставить как есть. Решать Михаилу/infra-ops.
- **З2** (переподключение 13:00Z) — см. п. 4. Информационное.
- **З3** (мелкий дефект `healthcheck.sh`, проверено моим тестом). Если в `/proc/mdstat` есть только inactive-массивы и ни одного активного, `n_arr` = 0 и проверка выдаёт `raid=none`, то есть молчит. Тест: один `md127 : inactive sdb3[1](S)` → 0 уведомлений, `raid=none`. При inactive + активный ALERT есть (`raid=BAD`). На `hood-rec` корень на md0, поэтому «все inactive» на работающей системе практически невозможны. Исправление в одну строку: считать `n_arr++` и в ветке `inactive` awk. Рядом нужен тест. Делать к следующему обновлению `deploy/`, отдельного выезда на сервер это не стоит. `deploy/` я не менял.
- **З4** (информационное). `mdmonitor-oneshot.timer` (ежедневно, следующий запуск 09:05Z) теперь тоже идёт через `PROGRAM`. При деградации это даст ежедневное повторное ALERT `DegradedArray` от mdadm, помимо однократного ALERT от healthcheck. Это полезное напоминание, не спам. Стоит упомянуть в README, раздел «RAID».

## Что осталось проверить Михаилу

- Примерно после 13:55Z: `journalctl -t hood-notify -n 3 -o short-iso --no-pager` должен показать INFO «RAID: синхронизация /dev/md/3 завершена». Это первое живое событие через работающий демон. После окончания resync md2 (несколько часов) — такое же для md2.
