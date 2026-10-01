# 015 — Сервер: SMART-мониторинг дисков + мелочи healthcheck из 014

date: 2026-10-01
executor: infra-ops
reviewer: indexer-engineer (ещё не запускался)

Статус задачи я не менял, коммитов не делал. На сервере я **только читал**: два вызова `ssh hood-rec`, второй через ControlMaster. Записей, установок и перезапусков на сервере не было. Подключений к фиду — 0, вызовов RPC — 0. IP сервера в репозитории нет. В рабочей копии есть чужие незакоммиченные изменения (`crates/decoders`, `sql/002_*`, `docs/STATE.md`), к 015 они не относятся. Я менял только `deploy/` и этот отчёт.

## Сделано (в репозитории)

1. **SMART через smartd.**
   - `deploy/smartd-hood.conf` (новый) ставится как `/etc/hoodchain/smartd.conf`. Это одна строка `DEVICESCAN` для всех дисков. Что в ней:
     - `-a` — здоровье, атрибуты, журналы ошибок и самотестов, сектора 197/198;
     - `-o on -S on`;
     - `-R 5!` — любой рост переназначенных секторов даёт предупреждение;
     - `-W 0,50,55` — запись в журнал от 50 °C, ALERT от 55 °C;
     - `-n standby,q`;
     - самотесты `-s (S/../.././05|L/../../6/01:008)`: короткий каждый день в 05:00 UTC, длинный в субботу в 01:00 UTC, у второго диска длинный тест на 8 ч позже (сдвиг `:008` только для длинного). Длинный тест никогда не идёт на обоих дисках сразу;
     - `-m <nomailer> -M exec /opt/hoodchain-mev/deploy/smartd-event.sh -M diminishing`.
   - `deploy/smartd-hood.service.conf` (новый) — drop-in `/etc/systemd/system/smartmontools.service.d/hood.conf`: `ExecStart=/usr/sbin/smartd -n -c /etc/hoodchain/smartd.conf $smartd_opts`. Пакетный `/etc/smartd.conf` — conffile dpkg, его я не трогал, иначе обновления пакета спрашивали бы про конфиг.
   - `deploy/smartd-event.sh` (новый) — обёртка над `notify.sh` для `-M exec`:
     - `EmailTest` (`-M test`) → INFO, всё остальное → ALERT. Это `Usage` (рост `Reallocated_Sector_Ct` по `-R 5!`), `Health`, `CurrentPendingSector`, `OfflineUncorrectableSector`, `SelfTest`, `ErrorCount`, `Temperature`, `FailedOpenDevice`, `FailedReadSmart*` и неизвестные типы;
     - в тексте — сообщение smartd, модель диска, номер повтора, срок следующего напоминания и `smartctl -x /dev/sdX`;
     - в stdout/stderr скрипт ничего не пишет: smartd записал бы это в журнал как LOG_CRIT «unexpected output». Код выхода всегда 0. Если уведомление не ушло, это пишется в `journalctl -t hood-smartd`.
   - `deploy/smartd-test.conf` (новый) ставится в `/opt/hoodchain-mev/deploy/`. Это проверка цепочки разовым запуском `smartd -q onecheck -s - -c …`: `-M test` для каждого диска, только `-H`, без расписания и без state-файлов. Работающий демон она не трогает.
   - `bootstrap.sh`, идемпотентно:
     - пакет `smartmontools` добавлен в список;
     - ставятся `smartd-event.sh`, `smartd-test.conf`, конфиг и drop-in;
     - `daemon-reload` делается, только если изменился drop-in. В выводе это отдельная строка: `systemd: daemon-reload for the smartmontools.service drop-in (no unit of the kit changed)`;
     - smartd перезапускается, только если изменился конфиг или drop-in и он запущен;
     - если smartd не включён — `enable`, если не запущен — `start`, при неудаче — WARN.
2. **healthcheck: проверка `smartd`.** Когда установлен `smartmontools` (`HC_CHECK_SMARTD=auto`, бинарник `HC_SMARTD_BIN=/usr/sbin/smartd`), проверяется `systemctl is-active smartmontools.service`. Не `active` → одно ALERT «smartd не работает (systemd: …): SMART-мониторинг дисков выключен», при возврате — «восстановлено». В итоговой строке появляется `smartd=ok|BAD`. Значение `0` выключает проверку, `1` включает её всегда.
3. **З3.** В разборе `/proc/mdstat` строка `mdN : inactive` теперь тоже считается массивом (`n_arr++`). Если массивы только inactive, получится ALERT `raid` и `raid=BAD`, а не молчаливое `raid=none`.
4. **З4.** В README, раздел «RAID», добавлен абзац: пока массив деградирован, `mdmonitor-oneshot.timer` (~09:05 UTC) каждый день присылает повторное ALERT `DegradedArray`. Это напоминание, а не новый отказ. Напоминания прекращаются при `[UU]`.
5. **README.** Новые строки таблицы файлов, ключ `smartd` в «Мониторинг», подраздел «SMART (задача 015)» (настройки, что делать при ALERT, запрет на ручной длинный тест во время resync, команды проверки). Дополнены «Обновление только скриптов» и «Проверка набора локально», заметка о smartmontools 7.4/7.5. В `etc/healthcheck.env.example` добавлены `HC_CHECK_SMARTD` и `HC_SMARTD_UNIT`.
6. **Тесты.**
   - `deploy/test/test-smartd-event.sh` (новый): 19 проверок на подставном окружении `SMARTD_*` и ещё 2, если установлен smartmontools. Во втором случае обёртка запускается через настоящий `smartd_warning.sh`, ровно как это делает smartd.
   - `test-healthcheck.sh`: в shim `systemctl` у smartd своё состояние. Добавлены раздел 16 (З3: только inactive → ALERT, `raid=BAD`, «восстановлено») и раздел 17 `smartd` (auto без пакета, ok, failed → одно ALERT, без повторов, «восстановлено», `0`/`1`, раздельно с `unit`).
   - Стенд `run-systemd-container.sh`, раздел «Task 015»:
     - пакет установлен, `ExecStart` с `-c /etc/hoodchain/smartd.conf`, пакетный `/etc/smartd.conf` не изменён (md5 conffile);
     - оба конфига разбираются smartd;
     - в контейнере healthcheck присылает ALERT `smartd`;
     - при снятом условии виртуализации демон под systemd открывает наш конфиг;
     - `smartd_warning.sh` → `smartd-event.sh` → `notify.sh` → journald;
     - повторный bootstrap — 0 изменений.

`recorder.service`, остальные юниты, `notify.sh`, `mdadm-event.sh` и код recorder я не менял.

## Проверено (2026-10-01, как)

**Сервер, только чтение (~16:39Z и ~16:52Z):**
- Ubuntu 26.04 LTS, ядро 7.0.0-22, `systemd-detect-virt` = `none`, то есть условие `ConditionVirtualization=no` юнита smartd выполнится.
- Диски: `sda`, `sdb` — **ST4000NM0245-1Z2** (Seagate, SATA, ROTA=1, 3.6 TiB), в by-id `ata-ST4000NM0245-1Z2107_*` и `wwn-0x5000c500…`. Серийные номера в отчёт не пишу.
- `smartctl` и `smartd` не установлены, пакета `smartmontools` нет, юнитов `*smart*` нет (только `smartcard.target`). `/etc/smartd.conf` нет.
- `apt-cache policy`: кандидат 7.5-2 (зеркало Hetzner). `apt-get -s install --no-install-recommends smartmontools`: **1 новый пакет, 0 обновлений, 0 удалений**.
- needrestart 3.11-1ubuntu2:
  - `/etc/needrestart/conf.d/hood.conf` = `$nrconf{override_rc}{qr(^recorder\.service$)} = 0;`. Основной конфиг подгружает `conf.d/*.conf`.
  - `needrestart -b -r l` (только список, без перезапусков) называет два юнита с устаревшими библиотеками: `networkd-dispatcher.service` и `unattended-upgrades.service`. Оба исключены в штатном `override_rc` (`qr(^network)` и `qr(^unattended-upgrades\.service$)`). **recorder в списке нет, и он исключён нашим `hood.conf`.**
  - Ожидающего ядра нет (`KSTA 1`), микрокод актуален.
  - `/usr/sbin/policy-rc.d` нет, значит postinst пакета сам запустит smartd.
- `mdcheck_start.timer`: drop-in Hetzner `OnCalendar=*-*-26 04:59:00`. `mdcheck_continue` — ежедневно, `mdmonitor-oneshot` — ~09:05Z. Отсюда субботнее окно длинного теста.
- `/proc/mdstat`: все 4 массива `[UU]`. **md2 resync 74.1 % (16:39Z) → 80.1 % (16:52Z), finish ≈ 37 мин**, то есть закончится примерно в 17:30Z. md3 закончил в 13:56Z (INFO `RebuildFinished` в journald — живая проверка `PROGRAM` mdadm из 014).
- recorder (16:52Z): `NRestarts=0`, `MainPID=7347`, `ActiveEnterTimestamp=Thu 2026-10-01 09:57:12 UTC`, совпадает с базой.

**smartmontools (контейнеры ubuntu:24.04 / 26.04 и исходники 7.5 с GitHub, тег `RELEASE_7_5`):**
- Юнит `smartmontools.service`, алиас `smartd.service`, `Type=notify`, `ExecStart=/usr/sbin/smartd -n $smartd_opts`, `ConditionVirtualization=no`. Версии: 7.4-2build1 в 24.04, 7.5-2 в 26.04. Conffiles: `/etc/smartd.conf`, `/etc/default/smartmontools`.
- `smartd.cpp` `MailWarning`: имена `SMARTD_FAILTYPE` — 13 типов, `EmailTest`…`Temperature`; переменные окружения; `-R 5!` → тип `Usage`. smartd запускает `smartd_warning.sh 2>&1` через `popen` и любой вывод пишет как LOG_CRIT. `smartd_warning.sh` при пустом `SMARTD_ADDRESS` делает `exec "$SMARTD_MAILER" </dev/null` с `PATH=/usr/local/bin:/usr/bin:/bin`.
- `smartd -q onecheck -s - -c smartd-hood.conf` и то же с `smartd-test.conf`: «was parsed, found DEVICESCAN», дальше выход 17 только из-за отсутствия дисков. Контроль: конфиг с битым `-s` даёт «fatal syntax errors». Без директив мониторинга smartd подставляет `-a`, поэтому в тестовом конфиге стоит явное `-H`.

**Тесты:**

| Набор | ubuntu:26.04 | ubuntu:24.04 | 24.04 + gawk |
|---|---|---|---|
| shellcheck 0.11.0 (`koalaman/shellcheck:stable`, `--network none`), все `deploy/*.sh`, `deploy/test/*.sh` | 0 замечаний, rc=0 | — | — |
| `bash -n` по всем скриптам | чисто | — | — |
| `test-healthcheck.sh`, `--network none` | 119/119 | 119/119 | 119/119 |
| `test-smartd-event.sh`, `--network none` (без smartmontools) | 19/19 | 19/19 | 19/19 |
| `test-smartd-event.sh` с установленным smartmontools (через настоящий `smartd_warning.sh`) | 21/21 | 21/21 | — |
| `test-mdadm-event.sh`, `--network none` | 20/20 | 20/20 | — |
| `test-notify.sh`, `--network none` | 14/14 | 14/14 | — |
| `run-systemd-container.sh` (systemd; сеть только для apt; портов нет; хосты фида, RPC и Telegram → 127.0.0.1) | ALL PASS: bootstrap 47 → 0 | ALL PASS, то же | — |

Новые проверки З3 и `smartd` на старом `healthcheck.sh` (HEAD) дают 9 FAIL, на новом проходят все. Значит, тесты ловят именно эти дефекты.

**Симуляция обновления сервера** (контейнер ubuntu:26.04 под systemd, скрипт `…/scratchpad/015/simulate-upgrade.sh`).
- Исходное состояние: bootstrap из закоммиченного HEAD `1a455bb` (это состояние сервера после 014), подставной `recorder` (`sleep infinity`) запущен, таймеры включены.
- Только для теста: снят `ConditionVirtualization` у smartd и задан `smartd_opts=-q never`, чтобы демон жил без дисков. Удалён `policy-rc.d` образа Docker, чтобы postinst запускал службы, как на сервере.
- Затем новое дерево и bootstrap. Результат:
  - **8 CHANGED**: `packages installed: smartmontools`, `healthcheck.sh`, `smartd-event.sh`, `smartd-test.conf`, `README.md`, `healthcheck.env.example`, `/etc/hoodchain/smartd.conf`, `smartmontools.service.d/hood.conf`;
  - строки `systemd: daemon-reload for the smartmontools.service drop-in (no unit of the kit changed)` и `smartmontools.service restarted (config /etc/hoodchain/smartd.conf)`;
  - `smartd: active, config -c /etc/hoodchain/smartd.conf`, `done: 8 change(s), 0 warning(s)`;
  - в журнале smartd сначала `Opened configuration file /etc/smartd.conf` (старт из postinst), затем `/etc/hoodchain/smartd.conf` (наш);
  - у recorder `NRestarts`, `MainPID` и `ActiveEnterTimestamp` до и после одинаковые;
  - второй прогон — 0 изменений;
  - healthcheck: `smartd=ok`.

После тестов удалены контейнеры и образы `hood-deploy-test-systemd:24.04/26.04`, `koalaman/shellcheck:stable`, `ubuntu:26.04`. `ubuntu:24.04` был на Mac до начала, его я оставил. Контейнеров `hood*` нет.

## Предполагается, не проверено

- **Живой smartd на этих дисках.** В Docker нет SMART-устройств, поэтому регистрация дисков, `-M test` от самого демона, расписание `:008` и реальные значения атрибутов вживую не проверялись. Проверено, что конфиг разбирается, что демон под systemd читает наш конфиг через drop-in и что цепочка `smartd_warning.sh` → обёртка → `notify.sh` → journald работает. На сервере это покажут шаги 4–6 ниже.
- Что `-q onecheck` с отдельным конфигом не мешает работающему демону. По документации это разовый процесс, а `-s -` отключает state-файлы. Вживую не проверялось.
- Длительность длинного самотеста ST4000NM0245 — несколько часов (по классу диска). Точное значение покажет `smartctl -c` (Extended self-test polling time).
- Что `apt-get install` не перезапустит ничего, кроме того, что видно в `needrestart -b -r l`. Список кандидатов и исключения я проверил, но сам прогон needrestart после apt — нет.

## Команды для Михаила

Сначала координатор коммитит и делает rsync чистого клона (как в 014). Список ниже начинается после этого. Все `ssh` идут через ControlMaster. Если master закрыт, вводить не чаще 6 команд за 30 с (лимит ufw). Длинный SMART-тест руками не запускать: md2 ещё синхронизируется, примерно до 17:30Z.

```
! ssh hood-rec 'chown -R hoodbuild:hoodbuild /opt/hoodchain-mev/src'
! ssh hood-rec 'grep -A2 "^md2" /proc/mdstat; systemctl show recorder -p NRestarts -p ActiveEnterTimestamp -p MainPID'
! ssh hood-rec 'bash /opt/hoodchain-mev/src/deploy/bootstrap.sh'
! ssh hood-rec 'bash /opt/hoodchain-mev/src/deploy/bootstrap.sh | tail -n 3'
! ssh hood-rec 'set -o pipefail; smartd -q onecheck -s - -c /opt/hoodchain-mev/deploy/smartd-test.conf | tail -n 4; echo rc=$?'
! ssh hood-rec 'journalctl -t hood-notify -n 2 -o cat --no-pager'
! ssh hood-rec 'smartctl -H /dev/sda'
! ssh hood-rec 'smartctl -H /dev/sdb'
! ssh hood-rec 'systemctl is-active smartmontools; journalctl -u smartmontools -n 20 -o cat --no-pager'
! ssh hood-rec 'smartd -q showtests -s - -c /etc/hoodchain/smartd.conf | grep "will do test 1 of type"'
! ssh hood-rec 'systemctl start healthcheck.service; journalctl -u healthcheck -n 1 -o cat --no-pager'
! ssh hood-rec 'journalctl -t hood-smartd -o cat --no-pager; journalctl -p warning --since -20min -o short-iso --no-pager | tail -n 20'
! ssh hood-rec 'systemctl show recorder -p NRestarts -p ActiveEnterTimestamp -p MainPID'
```

Что ожидается:
- **Строка 2 (база):** `NRestarts=0`, `MainPID=7347`, `ActiveEnterTimestamp=Thu 2026-10-01 09:57:12 UTC`.
- **Строка 3 (bootstrap).** Сначала вывод apt: 1 пакет `smartmontools`. needrestart может перечислить пропущенные службы — `networkd-dispatcher`, `unattended-upgrades`. Затем 8 строк `CHANGED`:
  - `packages installed: smartmontools`;
  - `file /opt/hoodchain-mev/deploy/healthcheck.sh`, `…/smartd-event.sh`, `…/smartd-test.conf`, `…/README.md`;
  - `file /etc/hoodchain/healthcheck.env.example`, `file /etc/hoodchain/smartd.conf`;
  - `file /etc/systemd/system/smartmontools.service.d/hood.conf`.

  Дальше строки `systemd: daemon-reload for the smartmontools.service drop-in (no unit of the kit changed)`, `smartmontools.service restarted (config /etc/hoodchain/smartd.conf)`, `smartd: active, config -c /etc/hoodchain/smartd.conf`, `recorder.service: enabled / active`, `done: 8 change(s), 0 warning(s)`.

  **Не должно быть** строки `systemd: daemon-reload` без пояснения: это означало бы, что изменился юнит набора. Не должно быть `mdmonitor.service restarted` и любых действий с recorder. Если postinst не запустил smartd, вместо `restarted` будет `CHANGED: smartmontools.service started`, и итог станет 9 — это тоже нормально.

  Про `daemon-reload`: его делает и postinst пакета, и bootstrap (из-за drop-in). Это перечитывание файлов юнитов. Файл `recorder.service` не менялся (sha256 совпадал в 014), поэтому recorder не перезагружается и не перезапускается. В симуляции `MainPID`/`NRestarts`/`ActiveEnterTimestamp` не изменились.
- **Строка 4:** `done: 0 change(s), 0 warning(s)`.
- **Строки 5–6:** `rc=0` и 2 INFO: `[INFO] …: SMART: тестовое сообщение smartd для /dev/sda [SAT]` и то же для `/dev/sdb`. Строки smartd про отправку письма при `<nomailer>` не важны.
- **Строки 7–8:** `SMART overall-health self-assessment test result: PASSED`.
- **Строка 9:** `active`. В журнале: `Opened configuration file /etc/hoodchain/smartd.conf`, оба диска добавлены в мониторинг, `Monitoring 2 ATA/SATA…`, ошибок нет.
- **Строка 10:** короткий тест у обоих дисков 2026-10-02 около 05:00 UTC; длинный у первого диска 2026-10-03 около 01:00 UTC, у второго около 09:00 UTC.
- **Строка 11:** в итоговой строке `smartd=ok`, `raid=ok`. Если md2 ещё синхронизируется — `raid_sync=md2_resync_…`.
- **Строка 12:** `hood-smartd` пусто. Среди warning-сообщений нет ошибок smartd и `hood-notify`.
- **Строка 13:** то же, что в базе: `NRestarts=0`, `MainPID=7347`, `09:57:12 UTC`.

Telegram по-прежнему не настроен (`/etc/hoodchain/notify.env` нет), поэтому SMART-алерты, как и остальные, остаются в journald сервера.

## Применение на сервере

**Проверено 2026-10-01 ~18:00–18:08Z** (команды вводил Михаил через `! ssh hood-rec`, проверки — координатор и ревьюер только чтением):
- исходники: rsync чистого клона коммита `5ad4d74` в `/opt/hoodchain-mev/src`, затем `chown -R hoodbuild:hoodbuild`;
- bootstrap №1: 8 изменений — `smartmontools` 7.5-2, `/etc/hoodchain/smartd.conf`, drop-in `smartmontools.service.d/hood.conf`, скрипты; `daemon-reload` только для drop-in smartd, `smartmontools.service restarted`; bootstrap №2 — `done: 0 change(s)`;
- `smartd -q onecheck -c smartd-test.conf`: exit 0, в `hood-notify` 2 INFO «SMART: тестовое сообщение smartd» для `/dev/sda` и `/dev/sdb`;
- `smartctl -H`: PASSED на обоих дисках; smartd active, «Monitoring 2 ATA/SATA», работает с `-c /etc/hoodchain/smartd.conf`;
- расписание (`smartd -q showtests`): короткий тест 2026-10-02 05:04 UTC (оба диска), длинный 2026-10-03 01:04 UTC (sda) и 09:04 UTC (sdb);
- healthcheck: `raid=ok smartd=ok`, остальные проверки ok;
- попутно подтверждён RAID-алерт 014 вживую: `mdadm --monitor` прислал INFO «синхронизация завершена» для md3 (13:56Z) и md2 (17:30Z); все 4 массива `[UU]`;
- recorder не затронут: `NRestarts=0`, `MainPID=7347`, `ActiveEnterTimestamp` 2026-10-01 09:57:12 UTC — как до начала; запись идёт, `gaps.tsv` нет.

**Предполагается:** что needrestart не трогал recorder — по журналу и неизменному состоянию recorder (вывода needrestart в `term.log` нет).

**Предложения ревьюера (не блокируют):** через сутки посчитать строки «changed from» в журнале smartd и при шуме добавить `-I 1 -I 7 -I 195`; решить, нужен ли серийный номер диска в теле уведомления (уйдёт и в Telegram, когда он появится).

## Вопросы к Cowork / Михаилу

1. **Telegram** (повтор из 014). Без `notify.env` SMART- и RAID-алерты до Михаила не доходят, требование «алерт доходит до Михаила» не выполнено. Настройка бесплатная.
2. Окно длинного самотеста — суббота 01:00 и 09:00 UTC. Если удобнее другое время, меняется одна строка `-s` в `deploy/smartd-hood.conf` и повторяется bootstrap. smartd перезапустится сам, recorder это не затрагивает.
