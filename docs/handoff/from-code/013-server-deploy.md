# 013 — Развёртывание recorder на сервере. Отчёт
status: пункты 1–10 выполнены (привилегированные шаги 3–5 — Михаил сам); ждёт indexer-engineer и data-auditor
date: 2026-10-01
executor: infra-ops
reviewers: indexer-engineer (первые 15 мин), data-auditor (feed-audit и копия первого часа) — запускает основная сессия

Статус задачи не менял (`in-progress`), коммитов нет (кроме `1b13aa4` от основной сессии). Сам я на сервере только читал (`ssh hood-rec`); изменения на сервере (sshd, rsync, bootstrap, сборка, старт recorder) сделал Михаил. Подключений к фиду с Mac и тестовых с сервера — 0, вызовов RPC — 0. IP сервера в этом отчёте и в репозитории нет, сервер везде называется `hood-rec`.

## Итог

- **26.04 совместима с набором 011/012 без правок в поведении.** Полный прогон в `ubuntu:26.04` под systemd: ALL PASS, bootstrap 41 → 0 изменений, сборка, verify всех юнитов. Регрессия на 24.04 тоже ALL PASS.
- Правки в `deploy/`: стенд принимает `--ubuntu 26.04`, проверяет chrony и ufw; bootstrap считает 26.04 проверенной версией; `BUILD_INFO` помечает незакоммиченное дерево как `-dirty`; README дополнен.
- **Запись идёт с 2026-10-01T09:57:12Z.** Первое подключение с IP Hetzner — 101, без 403/429. Первый час: ≈ 35 704 блока за 3 600 с (норма 35 700 ± 3 %), 0 дыр, одно соединение, healthcheck без алертов, `feed-audit` PASS (подробно — п. 6 и п. 10).
- **Сервер `hood-rec` при осмотре, до пункта 3** (сейчас пароль выключен, ufw включён — см. п. 3):
  - Ubuntu 26.04 LTS, ядро 7.0, x86_64, 8 потоков, 62 ГБ RAM.
  - Публичный IPv4 на интерфейсе, NAT нет.
  - **`PasswordAuthentication yes`** (умолчание: в конфиге не задано), root входит только по ключу.
  - Идёт resync RAID1.
  - chrony синхронизирован, смещение < 0.1 мс.
  - Наружу слушает только ssh :22, ufw выключен.
  - rustup, build-essential и rclone не установлены, списки apt пусты.

## Пункт 1: совместимость с 26.04

### Проверено (2026-10-01, Docker на Mac arm64, как)

Образ `ubuntu:26.04` (26.04.1 LTS). Состав coreutils в нём тот же, что на `hood-rec`: rust-coreutils 0.8.0, `cp` — GNU (`gnucp`), findutils 4.10. Стенд `deploy/test/run-systemd-container.sh`, контейнеры `hood-deploy-test-boot-2604` / `-2404`. Порты не публиковались. Сеть была только у systemd-стенда, для apt, rustup и crates.io. Офлайн-тесты шли с `--network none`. Хосты фида, RPC и Telegram в стенде направлены на 127.0.0.1, recorder не запускался. Сборки шли после коммита 012 (`6b4ce3c`), то есть собирался код 012.

| Проверка | 26.04 | 24.04 (регрессия после правок) |
|---|---|---|
| `bootstrap.sh`, прогоны 1 / 2 | 41 / **0** изменений, прогон 2 не изменил ни одного файла | 41 / **0**, то же |
| `build-on-server.sh` | rustup 1.27.1-8 из архива, rustc 1.98.1, сборка ~42 с (14 ядер, arm64) | rustc 1.98.1, ~41 с |
| bootstrap после сборки, прогоны 3 / 4 | 2 (включены healthcheck/feed-audit timers) / 0 | 2 / 0 |
| `systemd-analyze verify`, 10 юнитов (systemd 259 / 255) | чисто | чисто |
| `healthcheck.service` под systemd от `hood` | ок, `clock=ok` | ок |
| enricher-gaps без `enricher.env` | отказ стартовать | отказ |
| `test-backup.sh` (rclone 1.60.1, от `hood`) | 15/15 | 15/15 |
| chrony активен после bootstrap (новая проверка) | да | да |
| ufw активен, единственное правило — `limit 22/tcp` (новая проверка; ufw 0.36.2, iptables-nft) | да | да |
| recorder не запускался | `inactive` | `inactive` |
| `test-healthcheck.sh`, `--network none` | 71/71 | 71/71 |
| `test-notify.sh`, `--network none` | 14/14 | 14/14 |
| shellcheck по всем `deploy/*.sh`, `deploy/test/*.sh` / `bash -n` | 0 замечаний / ок | — |

Что сверял отдельно:

- **uutils против GNU** в одном контейнере 26.04, по всем флагам, которые используют скрипты набора. Список: `stat -c '%U:%G:%a'` и `%Y`, `date -u -d @N`, `date -u -d yesterday`, `sort -V`, `install -D -o -g -m`, `install -d -m`, `tail -n +N`, `tail -c`, `head -c`, `tr -dc`, `df -P`, `sha256sum`, `seq`, `nproc`, `touch -d @N` и `touch -d "-600 seconds"`. Вывод совпал побайтно. Правок не понадобилось.
- **sudo-rs.** Набор `sudo` не использует: всё идёт под root, пользователи переключаются через `runuser`. Правок не нужно.
- **needrestart 3.11** (как на сервере; поставлен в контейнер вручную). `deploy/needrestart-hood.conf` разбирается вместе с `needrestart.conf`, `recorder.service` получает `override_rc = 0` (не перезапускать), остальные юниты — по умолчанию. `needrestart -b` отрабатывает без ошибок.
- **journald (systemd 259).** Drop-in `hood.conf` применяется (`SystemMaxUse=2G`, `SystemKeepFree=5G`, `MaxRetentionSec=90day`), рестарт journald прошёл.
- **chrony 4.8.** Формат `chronyc -n tracking` прежний (`Leap status : Normal`, `System time : N seconds …`). Его разбирают bootstrap и healthcheck, и на реальном выводе с `hood-rec` разбор тоже сходится.
- **Python 3.14.4** против 3.12.3: `feed_audit.py --rpc-sample 0` по локальной записи `data/feed/2026/09/30` даёт побайтно одинаковый вывод, `verdict PASS`, 9.969 блока/с. На обеих версиях есть `DeprecationWarning` на `datetime.utcfromtimestamp`/`utcnow` (строки 278, 315). Работе это не мешает, но в будущих версиях Python функцию уберут (см. вопросы).
- **rustup для amd64** в архиве 26.04: `apt-cache policy` в контейнере `--platform linux/amd64` даёт кандидатов rustup 1.27.1-8, build-essential 12.12ubuntu2.26.04.2, rclone 1.60.1, needrestart 3.11.
- **Признак `-dirty` в `BUILD_INFO`** (проверено в контейнере 26.04 на временном git-репозитории). Чистое дерево даёт `git=<sha>`. После правки отслеживаемого файла — `git=<sha>-dirty` и WARNING в выводе сборки.

### Найденные отличия 26.04 и что сделано

| Отличие | Влияние на набор | Что сделано |
|---|---|---|
| `chrony.service` в 26.04 имеет `ConditionVirtualization=!container`, в Docker служба пропускается (в 24.04 запускалась с `-x`) | Только тестовый стенд: на 26.04 ветку chrony в bootstrap не проверить, healthcheck показывает `clock=BAD`. На сервере chronyd работает (проверено) | Стенд кладёт в контейнер drop-in, снимающий условие. `chronyd-starter.sh` сам добавляет `-x`, поэтому часы Mac не трогаются. Набор и сервер не затронуты |
| bootstrap на 26.04 печатал WARN «tested on Ubuntu 24.04 only» | Лишнее предупреждение | `bootstrap.sh` принимает 24.04 и 26.04 без WARN |
| uutils coreutils, sudo-rs, systemd 259, ufw на nft, needrestart 3.11, Python 3.14 | Нет (см. выше) | Ничего |
| ssh через `ssh.socket` (так и в 24.04) | Порт ssh для ufw берётся из `sshd -T`, на сервере это 22 | Описано в README: после правки `sshd_config.d` — `sshd -t` и `systemctl reload ssh` |

Изменённые файлы: `deploy/test/Dockerfile.systemd` (`ARG UBUNTU`), `deploy/test/run-systemd-container.sh` (`--ubuntu`, имя контейнера с версией, drop-in chrony, вывод WARN bootstrap, проверки chrony и ufw), `deploy/bootstrap.sh` (проверка версии ОС), `deploy/build-on-server.sh` (`-dirty`), `deploy/README.md` (26.04 в тексте, команды теста, раздел «Ubuntu 26.04: что отличается»). Правки ревьюера 012 в `healthcheck.sh` / `test-healthcheck.sh` / `README.md` / `healthcheck.env.example` уже были в коммите `6b4ce3c`, они не тронуты. `crates/` не тронут.

Контейнеры удалены, остались образы `hood-deploy-test-systemd:24.04`, `hood-deploy-test-systemd:26.04`, `ubuntu:26.04`.

### Предполагается, не проверено

- Что на x86_64 сборка ведёт себя так же, как на arm64. Пакеты для amd64 есть, но сама сборка под amd64 не запускалась.
- Что ufw на реальном ядре 7.0 (не в сетевом пространстве контейнера) включится так же. В контейнере правила ставились через iptables-nft без ошибок.
- Что needrestart при реальном обновлении библиотек не перезапустит recorder. Проверен разбор конфига, не само событие.

## Пункт 2: осмотр сервера

### Проверено (2026-10-01 ~09:25 UTC, `ssh hood-rec`, только команды чтения)

Команды: `cat /etc/os-release`, `uname -r -m`, `cat /proc/mdstat`, `lsblk`, `df -h`, `free -g`, `nproc`, `timedatectl`, `ip -4 addr`, `ss -tlnp`, `ss -ulnp`, `sshd -T | grep …`, `grep` по `/etc/ssh/sshd_config`, `systemctl cat ssh.socket`, `which …`, `readlink -f` для coreutils, `apt-cache policy …`, `dpkg-query -W …`, `systemctl --version`, `systemctl is-enabled/is-active …`, `chronyc -n tracking`, `ufw status`, `ls -la /etc/ssh/sshd_config.d/ /srv /opt`, `cat /etc/fstab`, `uptime`. Ничего не ставилось, не писалось и не перезапускалось. IP в выводе ниже скрыты.

| Что | Результат |
|---|---|
| ОС | Ubuntu 26.04 LTS (resolute), ядро `7.0.0-22-generic`, x86_64. Hostname по умолчанию из образа Hetzner (`Ubuntu-resolute-latest-amd64-base.zst`) |
| CPU / RAM / swap | 8 потоков / 62 ГБ, свободно 61 / swap 31 ГБ (RAID1 md0) |
| Диски | 2 × Seagate ST4000NM0245 (4 ТБ HDD, `ROTA=1`) |
| RAID1 | md0 swap 32 ГБ, md1 `/boot` 1 ГБ ext3, md2 `/` 2 ТБ ext4, md3 `/home` 1.6 ТБ ext4; все `[UU]` |
| Resync | **идёт**: md3 — 1.5 %, ~204 мин при 141 МБ/с; md2 (`/`) — `resync=DELAYED`, пойдёт следом. Предполагается ещё ~4 ч на md2 (2 ТБ при той же скорости), всего ~7–8 ч. Сервер не перезагружать |
| Свободно | `/` 1.9 ТБ (занято 2.3 ГБ), `/home` 1.6 ТБ (пусто). `/srv` и `/opt` пусты и лежат на `/` (md2). При ~2.3 ГБ/сутки фида места на `/` хватит на годы |
| Сеть | интерфейс `enp0s31f6`, `inet <IPv4>/32 scope global`: публичный адрес прямо на интерфейсе, NAT нет. IPv6 в выводе `ip -4` не смотрел |
| Время | `System clock synchronized: yes`, NTP active, chrony 4.8 активен (systemd-timesyncd не установлен). `chronyc tracking`: stratum 3, `Leap status: Normal`, System time 0.000008 с, RMS 0.0024 с |
| Часовой пояс | `Europe/Berlin` (CEST). Все таймеры набора заданы в `UTC`, скрипты используют `date -u`, так что на работу это не влияет; меняется только вид времени в журнале |
| Слушающие TCP | `0.0.0.0:22` и `[::]:22` (sshd через `ssh.socket`), `127.0.0.53/54:53` (resolved, только loopback). UDP: chronyd `127.0.0.1:323` (loopback) |
| ufw | установлен (0.36.2), `Status: inactive` |
| sshd (`sshd -T`) | `port 22`, `permitrootlogin prohibit-password` (в конфиге `without-password`), `pubkeyauthentication yes`, **`passwordauthentication yes`** (явно не задано, это умолчание OpenSSH), `kbdinteractiveauthentication no`, `usepam yes`, `authenticationmethods any`. `/etc/ssh/sshd_config.d/` пуст, `Include` стоит в строке 24, до остальных директив. OpenSSH 10.2p1. В `/root/.ssh/authorized_keys` один ключ |
| ssh-юниты | `ssh.socket` enabled/active (`ListenStream=<IPv4>:22`, `[::]:22`), `ssh.service` disabled (стартует от сокета), active |
| Есть | chronyc, ufw, needrestart 3.11, python3 3.14, zstd 1.5.7, curl, git 2.53, ca-certificates, runuser, nft/iptables, sudo + sudo-rs 0.2.13 |
| Нет | **rustup, cargo, rustc, build-essential (cc), rclone**. Их поставят `build-on-server.sh` и `bootstrap.sh` |
| apt | `/var/lib/apt/lists` пуст, поэтому `apt-cache policy rustup` ничего не показал. Источники: зеркало Hetzner и archive/security.ubuntu.com, main universe restricted multiverse. bootstrap и build-on-server делают `apt-get update` сами. Наличие пакетов для amd64 проверено в контейнере (п. 1) |
| Автообновления | `unattended-upgrades` и `apt-daily-upgrade.timer` включены. needrestart есть, исключение для recorder его касается |
| mdadm | `mdmonitor.service` — static, `MAILADDR root` (почта не настроена: о деградации RAID никто не узнает, см. вопросы) |
| cloud-init | отключён (`cloud-init.disabled`) |
| Пользователи `hood` / `hoodbuild` | ещё нет |
| Аптайм на момент осмотра | 6 мин |

### Предполагается, не проверено

- Есть ли файрвол Hetzner Robot перед сервером. Это смотрится в панели Hetzner, с сервера не видно.
- Что SMART у дисков в порядке: `smartctl` не установлен, ставить не стал (это запись).

## План пункта 3 (не выполнялся, ждёт разрешения)

Что нужно поменять: пароль для ssh выключить, ufw включить только для ssh. Порядок:

1. Сессия 1 (`ssh hood-rec`) остаётся открытой на всё время пункта.
2. Создать `/etc/ssh/sshd_config.d/10-hood.conf`:
   ```
   PasswordAuthentication no
   KbdInteractiveAuthentication no
   PermitRootLogin prohibit-password
   ```
   Файл из `sshd_config.d` подключается раньше основных директив, а в sshd побеждает первое значение, так что эти строки вступят в силу.
3. `sshd -t` (синтаксис), затем `systemctl reload ssh`. Сокет не трогать: порт не меняется.
4. **Проверка из второй, новой сессии.** `~/.ssh/config` для `hood-rec` использует ControlMaster, и обычный `ssh hood-rec` пойдёт по уже открытому соединению, ничего не проверив. Поэтому:
   - `ssh -o ControlMaster=no -o ControlPath=none hood-rec true` — должен войти по ключу;
   - `ssh -o ControlMaster=no -o ControlPath=none -o PubkeyAuthentication=no -o PreferredAuthentications=password hood-rec true` — должен получить `Permission denied (publickey)`;
   - `sshd -T | grep -E 'passwordauth|kbdinteractive|permitroot'` — `no`, `no`, `prohibit-password`.

   Только после этого закрывать сессию 1. Если что-то пошло не так — удалить файл в сессии 1, затем `systemctl reload ssh`.
5. ufw — через `bootstrap.sh` (пункт 4). Он возьмёт порт 22 из `sshd -T` и поставит `ufw limit 22/tcp` (v4 и v6), политики `deny incoming` / `allow outgoing`, затем `ufw --force enable`. Без ssh-правила ufw не включится. Снова проверить вход новой сессией без ControlMaster.

   `limit` блокирует IP после 6 новых соединений за 30 с: не гонять подряд много `ssh -o ControlPath=none` и rsync в цикле. Существующие соединения не рвутся.
6. Порты 53 (resolved) и 323 (chronyd) слушают только loopback, правил для них не нужно.

Запасной путь при блокировке — rescue-система или KVM-консоль Hetzner Robot (бесплатно). Предполагается, не проверялось.

## Пункты 3–10

012 закоммичена (`6b4ce3c`), правки пункта 1 — `1b13aa4`.

**Кто что делал:**
- Привилегированные шаги (sshd, rsync, bootstrap, сборка, старт recorder) выполнил Михаил сам. У меня такие команды заблокированы системой разрешений Claude Code: попытка этапа A отклонена и не выполнялась.
- Я делал только чтение на сервере (`ssh hood-rec` через ControlMaster) и правки в репозитории.
- Ниже «проверено мной» — то, что я сам прочитал на сервере; «со слов Михаила / основной сессии» — то, что не перепроверял.
- Подключений к фиду с Mac и тестовых подключений с сервера — 0; вызовов RPC — 0; recorder не перезапускался.

### П. 3: доступ (sshd, ufw)

**Проверено мной, 2026-10-01 ~10:01Z, чтением:**
- `/etc/ssh/sshd_config.d/10-hood.conf` содержит три строки: `PasswordAuthentication no`, `KbdInteractiveAuthentication no`, `PermitRootLogin prohibit-password`.
- `sshd -T`: `passwordauthentication no`, `kbdinteractiveauthentication no`, `permitrootlogin prohibit-password`.
- `ufw status verbose`:
  - `Status: active`;
  - `Default: deny (incoming), allow (outgoing), disabled (routed)`;
  - единственное правило — `22/tcp LIMIT IN` (v4 и v6, комментарий `ssh`).

**Со слов основной сессии:** вход ключом из независимой сессии (`ControlPath=none`) прошёл, вход по паролю получил `Permission denied`.

Моя первая попытка этапа A заблокирована, на сервер не попала. До правок Михаила проверил чтением, что `sshd_config.d` пуст и `passwordauthentication yes`.

### П. 4: bootstrap и сборка

**Проверено мной, ~10:01Z:**
- `/opt/hoodchain-mev/bin/BUILD_INFO`:
  - `installed_utc=2026-10-01T09:55:15Z`;
  - `git=1b13aa4` без `-dirty`, совпадает с HEAD репозитория на Mac;
  - `rustc=1.98.1`.
- `sha256sum recorder enricher` совпадает с `BUILD_INFO`: recorder `f65c4fab…59761`, enricher `5c78ddb8…cdfb9`.
- `git log -1` в `/opt/hoodchain-mev/src` — `1b13aa4`.
- `chronyc -n tracking`: stratum 3, `Leap status: Normal`, смещение 0.000025 с, то есть < 10 мс.

**Со слов основной сессии:**
- bootstrap: прогон 1 — 40 изменений, ufw активен, chrony synchronised; прогон 2 включил `healthcheck.timer` и `feed-audit.timer`.
- Сборка заняла 1 мин 13 с.
- **Ошибка в runbook, найдена Михаилом.** Команда rsync исключала `'.env*'` и вместе с ним удаляла на сервере отслеживаемый `.env.example`, из-за чего `BUILD_INFO` показал бы `-dirty`. Михаил пересинхронизировал с `--include .env.example`.
- Исправил обе команды rsync в `deploy/README.md`: `--include .env.example` теперь стоит перед исключениями, добавлено пояснение. Поведение проверено локально на временных каталогах: `.env.example` копируется, `.env` и `.env.local` нет.

### П. 5: старт

**Проверено мной (`connections.tsv`, журнал):**
- Каталог данных перед стартом был пуст: `recovery done state=None data=None resume=None torn_repairs=0`.
- Юнит: `ExecStart=/opt/hoodchain-mev/bin/recorder --out-dir /srv/hood/data/feed`, `User=hood`, `Restart=always`, `RestartSec=120`, enabled/active.
- Подключение `2026-10-01T09:57:12.952Z connected 101`, `detail` — `wss://feed.mainnet.chain.robinhood.com requested=- mode=no_data`. Строка `backlog done` в 09:57:13.282Z: `first_seq=77284408`, `first_lag_ms=1282`, `backlog_blocks=0`, `complete=true`.

### П. 6: первые 15 минут (снимок 2026-10-01T10:13:19Z, 16 мин после старта)

| Что | Результат | Норма |
|---|---|---|
| `connections.tsv` | 2 строки: `connected` 101 `mode=no_data` и `backlog done`; `disconnected` нет | да |
| `last_seq.txt` | 77293807 (mtime 10:13:01Z): 9 400 блоков за 948 с, **9.92 блока/с**. В журнале строки `alive` раз в минуту дают +587…+606 seq/мин | ~600/мин |
| Файлы | `feed-20261001-09.tsv.zst` 1.98 МБ (закрыт в 10:00), `feed-20261001-10.tsv.zst` 12.8 МБ (пишется) | да |
| Минутные фреймы | `zstd -lv`: XXH64 в каждом фрейме; `zstd -t` по часу 09 ок. Час 10 — `premature end` только на открытом последнем фрейме (так и должно быть) | да |
| `gaps.tsv` | файла нет, дыр нет | да |
| Журнал recorder | `frame committed` 16 раз, `alive` раз в минуту; WARN и ERROR нет | да |
| healthcheck | в 09:55:17 (до старта recorder) ушли ALERT `unit` и `feed`. В 10:00:27 — `восстановлено` по обоим, дальше `unit=ok ban=ok feed=ok disk=ok reconnects=ok writer=ok gaps_rows=0 clock=ok`, `feed_age_s` 26–45. Файлов `*.alert` нет | да |
| `feed-audit` | см. ниже | PASS |

`feed-audit` запускался на сервере от `hood` (`runuser -u hood --`, cwd `/tmp`) копией `/opt/hoodchain-mev/deploy/feed_audit.py`. Она совпадает с `src/.claude/skills/feed-audit/scripts/feed_audit.py` (`cmp`). Флаги: `--feed-root /srv/hood/data/feed --rpc-sample 0 --frame-secs 60`. Скрипт пишет только в stdout: в коде нет открытия файлов на запись.
- **verdict PASS**.
- 2 файла, 16 фреймов; открытый фрейм текущего часа (211 462 байт) не проверяется.
- 9 400 блоков из 9 400 ожидаемых, 0 пропущенных, `gaps []`, 1 сессия.
- 9 400 конвертов, в каждом 1 сообщение; плюс 26 `confirmedSequenceNumberMessage` и 475 ping.
- kinds: 3 — 9 359, 13 — 31, 9 — 9, **12 — 1**. kind 12 в записях 001/002/009 не встречался. По нумерации Nitro это, предположительно, `L1MessageType_EthDeposit` — не проверял.
- 9.913 блока/с.
- 14.81 МБ zstd, 100.9 МБ в распакованном виде, **56.2 МБ/ч**. В 001/002 было 90–97 МБ/ч; объём на блок меняется от сессии к сессии, как и отмечено в 009.
- Интервал между конвертами: p50 130.9 мс, p99 190.7 мс, max 749 мс.
- `DeprecationWarning` (`utcnow`/`utcfromtimestamp`) ушли в stderr, на результат не влияют.

### П. 7: 403/429

Не было: подключение одно, ответ 101. Факт записан в `chain-facts.md` (см. п. 9).

### П. 8: таймеры и уведомления

**Проверено мной, 10:00:11Z (`systemctl list-timers --all`, `is-enabled`/`is-active`):**
- `healthcheck.timer` — enabled/active, раз в 5 мин;
- `feed-audit.timer` — enabled/active, следующий запуск 2026-10-02 00:10 UTC (02:10 CEST);
- `backup.timer` и `enricher-gaps.timer` — disabled/inactive;
- `recorder.service` — enabled/active.

В `/etc/hoodchain/` только `*.example`, `notify.env` нет. Значит, уведомления идут только в journald (`hood-notify`), Telegram не настроен: **до Михаила алерты пока не доходят.**

### П. 9: учёт и факты

- `docs/costs.md`: строка «Сервер recorder `hood-rec`, Hetzner, €64.70/мес без НДС, ежемесячно с 2026-10-01, решение 0003 / задача 013, утвердил Михаил». Ссылки на счёт пока нет, она ждёт Михаила. Добавлен столбец «Счёт», суммы — в валюте счёта. В итогах — строка 2026-10.
- `.claude/skills/hoodchain-mev/references/chain-facts.md`: факт «первое подключение с IP дата-центра Hetzner HEL1 — 101, без 403/429 и `Retry-After`», с датой, временем и способом проверки. IP не записан.

### П. 10: сверка через 1 час

**Проверено мной, снимок 2026-10-01T11:03:18Z.**

| Что | Результат | Норма |
|---|---|---|
| Блоков за первый час | строка `alive` в 10:57:16.93Z даёт `last_seq=77320147`, то есть 35 740 блоков за 3 603.6 с от первого кадра (09:57:13.28Z). В пересчёте на 3 600 с — **≈ 35 704** (+0.01 % к 35 700) | 35 700 ± 3 % — да |
| Recorder | active, `NRestarts=0`, `ActiveEnterTimestamp` 09:57:12Z | да |
| `connections.tsv` | те же 2 строки (`connected` 101, `backlog done`), `disconnected` нет | да |
| `gaps.tsv` | файла нет | пусто — да |
| healthcheck c 10:00Z | 13 прогонов, все `unit=ok ban=ok feed=ok disk=ok reconnects=ok writer=ok gaps_rows=0 clock=ok`; `hood-notify` после 10:01Z — ни одной записи; файлов `*.alert` нет | без алертов — да |
| Диск | `/` (md2): занято 4.8 ГБ из 2.0 ТБ, 1 % | да |
| `zstd -t` по закрытым часам 09 и 10 | ок, 457.2 МБ в распакованном виде | да |
| `feed-audit` по закрытым часам 09 и 10 | см. ниже | PASS |

`feed-audit` (как в п. 6, от `hood`, `--rpc-sample 0 --frame-secs 60`, только `feed-20261001-09` и `-10`):
- **verdict PASS**;
- 63 фрейма, `open_tail []` (оба часа закрыты целиком);
- seq 77284408..77321764: 37 357 блоков из 37 357, 0 пропущенных, `gaps []`, 1 сессия 3 766.7 с;
- **9.918 блока/с**;
- kinds: 3 — 37 223, 13 — 102, 9 — 28, 12 — 4;
- 57.08 МБ zstd, 457.2 МБ в распакованном виде, **54.5 МБ/ч**;
- интервал между конвертами: p50 130.5 мс, p99 187.8 мс, max 834 мс.

Единственный WARN — `last_seq.txt=77323554` больше последнего seq в проверенных файлах. Так и должно быть: час 11 в проверку не входил.

**Копия для data-auditor (только чтение на сервере, rsync через `hood-rec`):**
- куда: `/private/tmp/claude-501/-Users-mihailshumilov-sites-my-crypto-atomic-arbitrage/a477d6fc-6af5-4e28-af3e-63445fb94d7a/scratchpad/013-firsthour/`;
- файлы: `2026/10/01/feed-20261001-09.tsv.zst` (1 975 796 Б) и `feed-20261001-10.tsv.zst` (55 099 746 Б), плюс `connections.tsv` и `last_seq.txt` (снимок 11:03Z). `gaps.tsv` на сервере нет;
- sha256 копий совпадает с `sha256sum` на сервере: `6b2ad824…69d45` (час 09) и `8fc85e65…1c332` (час 10); `zstd -t` локально — ок.

### Что Михаилу сделать

1. **Telegram-бот** (токен и chat id) → `/etc/hoodchain/notify.env` по runbook, шаг 4. Сейчас алерты остаются только в journald.
2. **Хранилище для бэкапа** — выбрать (предложение с расчётом — в отчёте 011). До этого сырьё лежит только на RAID1 одного сервера, а живой фид не досылается.
3. Прислать ссылку на счёт Hetzner для `docs/costs.md`.
4. Решить вопросы ниже (алерт деградации RAID, часовой пояс).

### Что осталось по задаче

- Проверка indexer-engineer по первым 15 минутам и data-auditor по `feed-audit` и копии первого часа: их запускает основная сессия.
- `status: done` и коммит — после этих проверок, по указанию основной сессии.

## Вопросы к Cowork и Михаилу

1. **Почта mdadm.** `MAILADDR root` никуда не доставляется: о выпавшем диске RAID1 никто не узнает. Предлагаю проверку `/proc/mdstat` (`[U_]`, `degraded`) в healthcheck с уведомлением через notify.sh. Это бесплатно, отдельная небольшая задача для infra-ops. Решать Cowork.
2. **Часовой пояс.** Сервер в `Europe/Berlin`. На набор это не влияет: таймеры в UTC, скрипты с `date -u`. Для однозначного вида журнала предлагаю в пункте 3 сделать `timedatectl set-timezone UTC`. Это изменение на сервере, нужно согласие.
3. **`/home` (md3, 1.6 ТБ) не используется.** Данные по умолчанию идут в `/srv/hood` на `/` (1.9 ТБ свободно), этого хватает. Если позже захочется держать сырьё отдельно от системы, можно перемонтировать md3 под `/srv/hood`. Делать это нужно до старта записи, иначе — с переносом данных. Сейчас не предлагаю менять.
4. **`feed_audit.py`** использует `datetime.utcfromtimestamp`/`utcnow`, они устаревают (предупреждение на 3.12 и 3.14). Работает, но стоит заменить на timezone-aware вызовы отдельной задачей для индексатора. Скрипт не в `deploy/`, я его не трогал.
5. **Перед rsync на сервер** закоммитить нужное состояние. Иначе `BUILD_INFO` покажет `-dirty`, и критерий «коммит = HEAD» в пункте 4 не выполнится.

## Решения Михаила после отчёта (2026-10-01)

- **Telegram — в план, но пока не делаем.** Уведомления нужны: без них алерты healthcheck и feed-audit остаются только в journald на сервере и до Михаила не доходят. Настройка — `/etc/hoodchain/notify.env` по runbook (шаг 4), когда Михаил даст токен и chat id. Cowork: внести в план отдельным пунктом.
- **Бэкап — пока не делаем.** До решения сырьё лежит только на RAID1 сервера `hood-rec`.
- **Hetzner** — строка в `docs/costs.md` подтверждена Михаилом (€64.70/мес без НДС, с 2026-10-01); ссылка на счёт не нужна.
