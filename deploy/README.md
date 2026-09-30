# deploy — установка recorder как сервиса systemd

Юнит: `deploy/recorder.service`. Ниже раскладка, на которую он рассчитан: код в `/opt/hoodchain-mev`, бинарник `/opt/hoodchain-mev/bin/recorder`, сырьё фида в `/srv/hood/data/feed`, пользователь `hood`. Другая раскладка — поправьте `User`, `ExecStart`, `ReadWritePaths` в юните.

Требования к серверу — `docs/decisions/0003-recorder-server.md`. Главное: выделенный IPv4, chrony/NTP (по `recv_unix_ns` считаются задержки) и **никаких тестовых подключений к фиду с IP продакшен-сервера**. Лимит фида — 2 соединения с IP. Бан длится около часа: это наблюдалось 2026-09-30, ответ 403 с `Retry-After: 3600`, см. `docs/handoff/from-code/002-recorder-hardening.md`.

## Установка

```bash
# 1. Пользователь и каталоги
sudo useradd --system --home /opt/hoodchain-mev --shell /usr/sbin/nologin hood
sudo mkdir -p /opt/hoodchain-mev/bin /srv/hood/data/feed
sudo chown -R hood:hood /srv/hood/data

# 2. Сборка (на сервере; нужен Rust >= 1.85 и ~4 ГБ RAM)
git clone <repo> /opt/hoodchain-mev   # или rsync рабочей копии
cd /opt/hoodchain-mev
cargo build --release -p recorder
sudo install -m 0755 target/release/recorder /opt/hoodchain-mev/bin/recorder
#   Можно собрать на другой машине под целевую архитектуру и скопировать только бинарник.

# 3. Юнит
sudo cp deploy/recorder.service /etc/systemd/system/recorder.service
sudo systemctl daemon-reload
sudo systemctl enable --now recorder

# 4. Проверка
systemctl status recorder
journalctl -u recorder -f          # ждём "connected", затем раз в минуту "frame committed"
```

## Что пишет recorder (`/srv/hood/data/feed`)

| Файл | Что внутри |
|---|---|
| `YYYY/MM/DD/feed-YYYYMMDD-HH.tsv.zst` | Сырьё: `recv_unix_ns \t seq_first \t seq_last \t <JSON>`. Внутри часового файла много zstd-фреймов: фрейм закрывается не реже раза в 60 с (`--frame-secs`), при ротации часа и при остановке. Читать `zstd -dc` или ридером, который понимает несколько фреймов. |
| `gaps.tsv` | `from \t to \t recv_ns` — пропущенные L2-блоки для дозаливки через RPC (enricher). Пишется только после fsync данных. |
| `last_seq.txt` | Последний seq, который уже на диске (fsync). Заменяется атомарно. |
| `connections.tsv` | Подключения, отключения, причина, HTTP-код, `Retry-After`, выбранная пауза, число страйков. Первая строка — заголовок. |
| `_torn/` | Оборванные хвосты zstd, отрезанные при старте после аварийного завершения. Хранятся для разбора, recorder их не удаляет. |

Строки без номеров блоков пишутся с `seq_first = seq_last = 0`:
- конверты без `messages` (например, `{"version":1,"confirmedSequenceNumberMessage":{...}}`);
- нетекстовые фреймы (ping и другие) в обёртке `{"recorderFrame":{"opcode":"ping","payloadBase64":"..."}}`.

Отбрасывать их — задача разбора.

## Поведение при сбоях

- **`systemctl stop` / SIGTERM** (так же SIGINT). Recorder дописывает очередь, закрывает фрейм, делает fsync, пишет `last_seq.txt` и выходит с кодом 0. На это есть 30 с (`TimeoutStopSec`), обычно хватает долей секунды.
- **kill -9, падение, пропало питание.** Теряется только открытый фрейм, то есть не больше последних ~60 с. При следующем старте оборванный хвост уходит в `_torn/`, файл обрезается до последнего целого фрейма. Точка продолжения берётся из самих данных. Простой и потерянный хвост попадают в `gaps.tsv` одной строкой, когда придёт первый новый блок.
- **Паузы переподключения.**
  - Обычное закрытие: 1–5 с.
  - 429: `Retry-After`, а если его нет — 5 → 10 → 20 → 40 → 60 мин.
  - 403 или отказ апгрейда: 15 → 30 → 60 мин, но не меньше `Retry-After`.
  - Сетевые ошибки и 5xx: 5 с → … → 5 мин.
  - Сессия дольше 10 мин сбрасывает лестницу.
- **Пауза переживает перезапуск.** При старте recorder читает последнюю строку `disconnected` в `connections.tsv` и дожидается конца паузы. Поэтому `Restart=always` и ручной рестарт не долбят сервер во время бана. В журнале это видно как `pause from before restart still active, waiting`. Обойти можно флагом `--ignore-pending-pause`, но только если точно знаете, что бан снят.

## Обновление бинарника

```bash
cd /opt/hoodchain-mev && git pull && cargo build --release -p recorder
sudo install -m 0755 target/release/recorder /opt/hoodchain-mev/bin/recorder
sudo systemctl restart recorder     # SIGTERM → чистая остановка → старт
```

Ручной `restart` не ждёт `RestartSec`. Дыра на время рестарта (1–2 с) попадёт в `gaps.tsv` и дозальётся через RPC.

## Проверка записи

```bash
cd /srv/hood/data/feed
for f in $(find . -name 'feed-*.tsv.zst' -newermt '-2 hours'); do zstd -t "$f" || echo "BROKEN $f"; done
cat last_seq.txt; tail -n 5 gaps.tsv; tail -n 5 connections.tsv
```

Полная проверка — скилл `feed-audit` (`.claude/skills/feed-audit/`).
