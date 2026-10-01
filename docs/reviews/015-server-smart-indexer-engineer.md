# Ревью 015 — SMART-мониторинг и правки healthcheck (indexer-engineer)

date: 2026-10-01
reviewer: indexer-engineer
commit: 5ad4d74 (`015 (part 1): SMART monitoring via smartd, healthcheck fixes`)

**Вердикт: PASS** по пп. 1–3. Recorder не затронут. Дефектов в `deploy/` нет, `deploy/` я не менял. Ниже предложения П1–П5, ни одно не блокирует.

## Как проверял

- Код: `git show 5ad4d74 -- deploy/`, полностью прочитаны `smartd-event.sh`, `smartd-hood.conf`, `smartd-hood.service.conf`, `smartd-test.conf`, блок `smartd` в `bootstrap.sh` вместе с `install_file`, разделы `raid` и `smartd` в `healthcheck.sh`, diff README. Рабочая копия `deploy/` совпадает с коммитом: `git status` чистый по `deploy/`.
- Семантику `-s …:NNN` сверил с `smartd.conf.5.in` (smartmontools 7.5, исходник из scratchpad infra-ops).
- Тесты на Mac в Docker, `--network none`, образ `ubuntu:24.04` (был на Mac до начала, оставлен). shellcheck — `koalaman/shellcheck:stable` 0.11.0, образ скачан для проверки и удалён. Контейнеры запускались с `--rm`, после проверки их нет.
- Сервер: `ssh -O check hood-rec` → master running. Четыре вызова `ssh hood-rec` через ControlMaster (18:07:15Z–18:08:10Z), все только на чтение: `sha256sum`, `stat`, `ls`, `cat`, `find`, `tail`, `grep`, `systemctl show`, `dpkg-query`, `md5sum`, `ps`, `journalctl`. Записей, рестартов, установок, подключений к фиду и вызовов RPC не было. `smartctl` и `smartd` я на сервере не запускал. IP и серийные номера дисков в выводе маскировал, здесь их нет.

## 1. Код — PASS

Проверено (2026-10-01, чтение кода и тесты):

- **`smartd-event.sh`.** `EmailTest` → `info`, все остальные `SMARTD_FAILTYPE`, включая неизвестные (`*)`), → `alert`. Вызов `notify.sh` идёт с `> /dev/null 2>&1`, `logger` тоже заглушён. Других выводов нет, так что smartd не получит «unexpected output». Все пути заканчиваются `exit 0`. Скрипт работает под `set -uo pipefail` без `-e`, поэтому сбой `notify.sh` не обрывает его, а пишется в `hood-smartd`. Без `SMARTD_FAILTYPE` — запись в журнал и `exit 0`. Номер повтора и срок напоминания добавляются только для ALERT и только при числовых значениях.
- **`smartd-hood.conf`.** Одна строка `DEVICESCAN -d removable -n standby,q -a -o on -S on -R 5! -W 0,50,55 -s (S/../.././05|L/../../6/01:008) -m <nomailer> -M exec …/smartd-event.sh -M diminishing`. Всё, что требует задача, на месте: `-a`, рост переназначенных секторов по `-R 5!`, ALERT по температуре от 55 °C, расписание, `-M exec` на обёртку, `-M diminishing`.
  - Про `:008` (по `smartd.conf.5.in`). Задержка применяется только к альтернативе, в которой стоит суффикс `:NNN`, и строка должна совпасть полностью. Значит, короткий тест идёт на обоих дисках в 05:xx, а длинный — в 01:xx на sda и в 09:xx на sdb. Это сходится с расписанием, которое видел координатор (short 2026-10-02 05:04 на обоих; long 2026-10-03 01:04 sda / 09:04 sdb). Короткий тест длится около 2 минут, одновременный запуск на двух дисках безвреден. Комментарий в конфиге («the second disk 8 h later») стоит в строке про длинный тест, то есть точен.
- **`smartd-hood.service.conf`.** `ExecStart=` сбрасывается, затем `ExecStart=/usr/sbin/smartd -n -c /etc/hoodchain/smartd.conf $smartd_opts`. Совпадает со штатной строкой пакета, добавлен только `-c`. Пакетный `/etc/smartd.conf` не трогается.
- **`bootstrap.sh`, идемпотентность.** `install_file` сравнивает содержимое (`cmp`) и владельца с правами и выставляет `FILE_CHANGED`. `daemon-reload` выполняется только при `FILE_CHANGED` после установки drop-in. Перезапуск smartd — только если менялся конфиг или drop-in и юнит активен. `enable` и `start` — только если юнит не включён или не запущен. Конструкции `(( … )) && …` под `set -e` не обрывают скрипт. Блок пропускается, если нет `/usr/sbin/smartd`.
- **`healthcheck.sh`, `smartd`.** Проверка включается при `HC_CHECK_SMARTD=1` или при `auto` и наличии бинарника. `systemctl is-active` не вызывает выход: в скрипте `set -uo pipefail` без `-e`, пустой ответ превращается в `unknown`. Дальше `raise`/`resolved` дают `smartd=BAD|ok` и одно ALERT с «восстановлено».
- **З3 исправлено.** В awk ветка `inactive` теперь делает `n_arr++`. Тест `test-healthcheck.sh`, стр. 467–475: только `md127 : inactive` → одно ALERT, `raid=BAD`, без повтора, «восстановлено». Это ровно мой сценарий из ревью 014.
- **README.** Есть строка в таблице файлов, ключ `smartd` в «Мониторинг», подраздел «SMART (задача 015)»: настройки, что делать при ALERT, запрет ручного длинного теста во время resync, команды проверки. Абзац З4 «Повторные ALERT при деградации — это нормально» стоит в разделе «RAID». Уточнён абзац про обновление скриптов (daemon-reload для drop-in, перезапуски mdmonitor и smartd).

## 2. Тесты и shellcheck — PASS

Проверено (2026-10-01, Docker на Mac, `ubuntu:24.04`, `--network none`):

| Набор | Результат |
|---|---|
| `test-healthcheck.sh` | 119 passed, 0 failed, rc=0 |
| `test-smartd-event.sh` | 19 passed, 0 failed, rc=0 (SKIP настоящего `smartd_warning.sh`: в образе нет smartmontools, а без сети его не поставить) |
| `test-mdadm-event.sh` | 20 passed, 0 failed, rc=0 |
| `test-notify.sh` | 14 passed, 0 failed, rc=0 |
| shellcheck 0.11.0, все `deploy/*.sh` и `deploy/test/*.sh` | 0 замечаний, rc=0 |
| `bash -n` по тем же файлам | чисто |

Стенд `run-systemd-container.sh` и ubuntu:26.04 я не запускал. Там нужна сеть для apt, и это уже сделал infra-ops (отчёт: ALL PASS, bootstrap 47 → 0). Цепочку через настоящий `smartd_warning.sh` я проверил по факту на сервере (п. 3).

## 3. Сервер — PASS

Проверено (2026-10-01 18:07–18:08Z, только чтение):

- **Файлы.** У всех файлов sha256 одинаковый в трёх местах: коммит `5ad4d74` у меня, `/opt/hoodchain-mev/src/deploy` и установленная копия. Это `smartd-event.sh` (root:root 755), `smartd-test.conf` (644), `healthcheck.sh` (755), `README.md`, `notify.sh`, `mdadm-event.sh`, `/etc/hoodchain/smartd.conf` (644), `/etc/systemd/system/smartmontools.service.d/hood.conf` (644, mtime 18:02, единственный файл в каталоге) и `/etc/hoodchain/healthcheck.env.example` (root:hood 640). `bootstrap.sh` в src совпадает с коммитом.
- **smartd.** Пакет `smartmontools 7.5-2`. Юнит `active/running`, `enabled`, `NRestarts=0`, `MainPID=16300`, вход в active в 18:02:19. `DropInPaths` указывает на наш `hood.conf`, `ExecStart` — `/usr/sbin/smartd -n -c /etc/hoodchain/smartd.conf $smartd_opts`, процесс работает именно с этим `-c`. Пакетный `/etc/smartd.conf` не изменён: md5 совпадает с conffile dpkg.
- **Журнал smartd.** В 18:02:09 postinst запустил smartd со штатным `/etc/smartd.conf`. В 18:02:13 bootstrap его перезапустил, и демон открыл `/etc/hoodchain/smartd.conf`. Дальше: оба диска `[SAT] ST4000NM0245-1Z2107` найдены в базе («Seagate Enterprise Capacity 3.5 HDD»), включены Attribute Autosave и Automatic Offline Testing, «Monitoring 2 ATA/SATA, 0 SCSI/SAS and 0 NVMe devices», температура 31 °C и 30 °C. Ошибок нет, `journalctl -t smartd -p warning` с 18:00 пуст. Есть одна информационная строка `sdb … 195 Hardware_ECC_Recovered changed from 71 to 42` (см. П1).
- **hood-notify.** В 18:04:02 и 18:04:07 пришли два INFO «SMART: тестовое сообщение smartd для /dev/sda [SAT]» и то же для sdb, с телом «TEST EMAIL from smartd…» и моделью диска. Значит, цепочка `smartd` → `smartd_warning.sh` → `smartd-event.sh` → `notify.sh` → journald работает на настоящих дисках. Там же видны INFO RAID: `RebuildFinished` md3 в 13:56:23 и md2 в 17:30:32. Цепочка 014 подтверждена вживую. `journalctl -t hood-smartd` пуст, то есть сбоев отправки не было.
- **healthcheck.** Последняя строка (таймер, 18:04:16): `unit=ok ban=ok feed=ok disk=ok reconnects=ok writer=ok gaps_rows=0 clock=ok raid=ok smartd=ok`. Ключ `smartd` появился с этого запуска, в 17:59 его ещё не было.
- **`/proc/mdstat`.** Все 4 массива `[UU]`, resync нет.
- **apt и needrestart.** Судя по `/var/log/apt/history.log`, ставился один пакет: `Install: smartmontools:amd64 (7.5-2)`, 18:02:07–18:02:11. Полный журнал 18:01:30–18:03:00 показывает 3 `daemon-reload` (2 от postinst, 1 от bootstrap) и запуск, остановку и новый запуск только `smartmontools.service`. Остальное — штатные `apt-news`, `esm-cache`, `packagekit`, `timedated`. Остановок и перезапусков других служб нет. Recorder в это окно писал без перерыва: frame committed в 18:02:00 и в 18:02:59. Вывода needrestart в `/var/log/apt/term.log` нет, поэтому список его кандидатов я напрямую не видел. Вывод «needrestart recorder не тронул» сделан по журналу и по неизменному состоянию recorder.
- **Recorder.** `active/running`, `NRestarts=0`, `MainPID=7347`, `ActiveEnterTimestamp=Thu 2026-10-01 09:57:12 UTC` — совпадает с базой.
- **Запись идёт.** `last_seq.txt` = 77576847 (mtime 18:06:59), затем 77577443 (mtime 18:07:59), около 600 блоков в минуту. В журнале frame committed идут каждую минуту. Файлов `gaps.tsv` под `/srv/hood/data/feed` нет. В 17:30:52 было переподключение: `server_closed`, сессия 16221 с; `connected` в 17:30:55; backlog `first_minus_requested=0`, то есть дыры нет. Оно случилось внутри процесса и до применения 015, к задаче отношения не имеет.

Со слов координатора, сам не проверял: второй bootstrap — 0 изменений; `smartctl -H` PASSED на обоих дисках; расписание тестов из `showtests`. Косвенно первое подтверждается тем, что у drop-in mtime 18:02 и smartd после 18:02:19 не перезапускался.

## Предложения (не блокируют)

- **П1 (шум в журнале, предполагается).** `-a` включает отслеживание изменений атрибутов, и smartd пишет в журнал (уровень info, не уведомление) каждое изменение нормализованного значения. У Seagate атрибуты 1 `Raw_Read_Error_Rate`, 7 `Seek_Error_Rate` и 195 `Hardware_ECC_Recovered` обычно «гуляют», первая такая строка уже есть. Через сутки стоит посчитать: `journalctl -u smartmontools --since -1d | grep -c 'changed from'`. Если строк сотни — добавить в `smartd-hood.conf` `-I 1 -I 7 -I 195` (игнорировать изменения, но не пороги). На алерты это не влияет.
- **П2 (информационное).** В тело уведомления попадает `SMARTD_DEVICEINFO`, а в нём серийный номер диска. Сейчас это только journald. Когда настроят Telegram, серийники уйдут и туда. Для заявки в Hetzner это даже полезно. Если не нужно — убрать строку «Диск: $info» или вырезать `S/N:…`.
- **П3.** Telegram (`/etc/hoodchain/notify.env`) не настроен, поэтому SMART- и RAID-алерты до Михаила не доходят. Повтор из 014, решение Михаила.
- **П4 (оформление).** В отчёте `docs/handoff/from-code/015-server-smart.md` раздел «Применение на сервере» не заполнен, у задачи `status: in-progress`. Это дело координатора и infra-ops при закрытии.
- **П5 (мелочь в документации).** В README и отчёте сказано: «короткий каждый день в 05:00 UTC… второй диск на 8 ч позже». Стоит уточнить, что сдвиг `:008` относится только к длинному тесту, а короткий идёт на обоих дисках одновременно. Так и задумано, но формулировку легко прочитать иначе.
