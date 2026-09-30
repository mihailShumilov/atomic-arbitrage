# deploy — установка recorder как сервиса systemd

Юнит: `deploy/recorder.service`. Ниже раскладка, на которую он рассчитан: код в `/opt/hoodchain-mev`, бинарник `/opt/hoodchain-mev/bin/recorder`, сырьё фида в `/srv/hood/data/feed`, пользователь `hood`. Другая раскладка — поправьте `User`, `ExecStart`, `ReadWritePaths` в юните.

Требования к серверу — `docs/decisions/0003-recorder-server.md`. Главное: выделенный IPv4, chrony/NTP (по `recv_unix_ns` считаются задержки) и **никаких тестовых подключений к фиду с IP продакшен-сервера**. Лимит «2 соединения с IP, третье получает 429» **не проверен**: это факт из потерянной сессии, повторно его не наблюдали. Наблюдался бан около часа: 2026-09-30 ответ 403 с `Retry-After: 3600`, см. `docs/handoff/from-code/002-recorder-hardening.md`.

## Почему `RestartSec=120`

2026-09-30 переподключение через 11 с после `kill -9` получило бан IP: `403 Forbidden` с `Retry-After: 3600`, то есть час дыры. Перед этим 429 не было. Причина не установлена; гипотезы — частота подключений или обрыв без close-фрейма. Поэтому (задача 008):
- systemd перезапускает упавший recorder не раньше чем через 120 с (`RestartSec=120`);
- recorder и сам не подключается раньше чем через 120 с после последнего успешного подключения предыдущего запуска (`--min-connect-interval-secs 120`, время берётся из события `connected` в `connections.tsv`). Это действует и при ручном `systemctl restart`, который `RestartSec` не ждёт. В журнале видно `waiting before the first connect reason=min_connect_interval`, в `connections.tsv` — событие `startup_wait` с причиной `min_connect_interval`;
- при остановке recorder отправляет WebSocket Close 1000 и ждёт ответный Close до 2 с, а уже потом закрывает TCP. Итог пишется в `connections.tsv` событием `client_close` (`server_replied` или `no_reply`).

Цена: каждый рестарт — дыра не меньше ~2 мин (~1200 блоков). Она попадает в `gaps.tsv` и дозаливается через RPC.

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
- нетекстовые фреймы (ping и другие) и текст, который не является JSON, в обёртке `{"recorderFrame":{"opcode":"ping","payloadBase64":"..."}}` (для текста `opcode` = `text`).

Отбрасывать их — задача разбора.

## Поведение при сбоях

- **`systemctl stop` / SIGTERM** (так же SIGINT). Recorder отправляет WebSocket Close 1000 и ждёт ответ сервера до 2 с (всё, что пришло за это время, тоже записывается), затем дописывает очередь, закрывает фрейм, делает fsync, пишет `last_seq.txt` и выходит с кодом 0. На это есть 30 с (`TimeoutStopSec`), обычно хватает 0–2.5 с.
- **kill -9, падение, пропало питание.** Теряется только открытый фрейм, то есть не больше последних 60 с: фрейм фиксируется через `--frame-secs` минус 200 мс запаса на fsync после его первой строки. При следующем старте оборванный хвост уходит в `_torn/`, файл обрезается до последнего целого фрейма. Точка продолжения берётся из самих данных. Простой и потерянный хвост попадают в `gaps.tsv` одной строкой, когда придёт первый новый блок.
- **Паузы переподключения.**
  - Обычное закрытие: 1–5 с.
  - 429: `Retry-After`, а если его нет — 5 → 10 → 20 → 40 → 60 мин.
  - 403 или отказ апгрейда: 15 → 30 → 60 мин, но не меньше `Retry-After`.
  - Сетевые ошибки и 5xx: 5 с → … → 5 мин.
  - Сессия дольше 10 мин сбрасывает лестницу.
- **Пауза переживает перезапуск.** При старте recorder читает `connections.tsv` и ждёт дольшее из двух: остаток паузы из последней строки `disconnected` (например, `Retry-After` бана) и остаток 120 с с последнего `connected`. Поэтому `Restart=always` и ручной рестарт не долбят сервер во время бана. В журнале это видно как `waiting before the first connect` с `reason=pending_pause` или `reason=min_connect_interval`. SIGTERM прерывает ожидание, выход 0. Обойти оба ожидания можно флагом `--ignore-pending-pause`, но только если точно знаете, что бан снят.
- **Сверка дыр при старте.** Recorder проходит по seq в двух последних часовых файлах и на стыке с предыдущим файлом. Разрыв, которого нет в `gaps.tsv`, дописывается туда (fsync), в `connections.tsv` пишется событие `gap_reconciled`, в журнал — WARN. Так закрывается узкое окно «данные уже на диске, строка `gaps.tsv` ещё нет» при аварии.

## Обновление бинарника

```bash
cd /opt/hoodchain-mev && git pull && cargo build --release -p recorder
sudo install -m 0755 target/release/recorder /opt/hoodchain-mev/bin/recorder
sudo systemctl restart recorder     # SIGTERM → чистая остановка → старт
```

Ручной `restart` не ждёт `RestartSec`, но recorder сам выдержит 120 с с последнего подключения (см. выше). Дыра на время рестарта (~2 мин) попадёт в `gaps.tsv` и дозальётся через RPC.

## Проверка записи

```bash
cd /srv/hood/data/feed
for f in $(find . -name 'feed-*.tsv.zst' -newermt '-2 hours'); do zstd -t "$f" || echo "BROKEN $f"; done
cat last_seq.txt; tail -n 5 gaps.tsv; tail -n 5 connections.tsv
```

Полная проверка — скилл `feed-audit` (`.claude/skills/feed-audit/`).
