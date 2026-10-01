# 013 — Развёртывание recorder на сервере и старт записи
status: done
phase: 1a
depends-on: 011, 012
executor: infra-ops
reviewers: indexer-engineer (первые 15 мин записи, пути и флаги), data-auditor (feed-audit первой записи)

## Зачем
Начать круглосуточную запись фида — с неё идёт отсчёт 7 суток критерия фазы 1a. Сервер получен 2026-10-01.

## Контекст
- Сервер: Hetzner, HEL1-DC2, i7-7700, 64 ГБ, 2 × 4 ТБ HDD (software RAID1 из установщика Hetzner), 1 Гбит/с, трафик без лимита, €64.70/мес без НДС. IP Михаил даёт в промпте; **в репозиторий IP не записывать** (локально: `~/.ssh/config` Host `hood-rec` или `.env` `RECORDER_HOST`, `.env` не в git).
- **ОС: Ubuntu 26.04, а набор 011 проверен на 24.04.** Возможные отличия (предполагается, проверить): `sudo-rs`, Rust-coreutils (uutils — флаги `stat`/`date`/`du`), версии chrony/ufw/needrestart/journald, наличие `rustup` в архиве, Python.
- Runbook: `deploy/README.md`. Решения 0003 (accepted), отчёты 011/012.

## Что сделать
1. **Совместимость с 26.04 — до сервера.** Прогнать `deploy/test/` и `bootstrap.sh` (дважды, второй — 0 изменений) в контейнере `ubuntu:26.04` под systemd на Mac, `systemd-analyze verify` всех юнитов, тесты healthcheck. Найденные несовместимости исправить в `deploy/` (с тестом), сохранив работу на 24.04.
2. **Осмотр сервера (только чтение):** `os-release`, `uname -r`, `/proc/mdstat` (идёт ли resync RAID — ожидаемо несколько часов, не мешает), `lsblk`, `df -h`, `free -g`, `timedatectl`, публичный IPv4 на интерфейсе, `ss -tlnp`, `sshd -T | grep -i -E 'passwordauth|permitroot|port'`. Итог — в отчёт.
3. **Доступ.** Вход только по ключу: если `PasswordAuthentication yes` — выключить, **проверив вход ключом из второй ssh-сессии до закрытия первой**. ufw — только ssh (через `bootstrap.sh`).
4. **`bootstrap.sh`** (без `--with-docker`), затем **`build-on-server.sh`**; сверить `BUILD_INFO` (коммит = HEAD репозитория, sha256). `chronyc tracking` — синхронизировано, смещение < 10 мс.
5. **Старт:** `systemctl enable --now recorder` — ровно одно соединение. Каталог данных пустой, `--out-dir /srv/hood/data/feed`.
6. **Первые 15 мин** по таблице runbook: `connections.tsv` (`connected` 101, `mode=no_data` при первом старте), рост `last_seq.txt`, минутные фреймы, `gaps.tsv` пуст, `journalctl -u recorder`, healthcheck зелёный, ~10 блоков/с. Затем `feed-audit --rpc-sample 0` по `/srv/hood/data/feed` — PASS (с флагами `--now`/`--frame-secs` из 009, если нужно).
7. **Если первое подключение с IP Hetzner получило 403/429:** не перезапускать, не проверять руками — recorder выждет сам; сразу описать в отчёте (время, код, `Retry-After`) и в `chain-facts.md` как факт. Если после выжидания снова 403 — остановить recorder (`systemctl stop`) и вынести вопрос Михаилу (Cloudflare может блокировать IP дата-центра).
8. **Таймеры:** healthcheck и feed-audit включены; уведомления — пока только journald (Telegram Михаил даст отдельно); `backup.timer`, `enricher-gaps.timer` — выключены.
9. **Учёт:** строка в `docs/costs.md` (сервер Hetzner, €64.70/мес без НДС, с 2026-10-01, решение 0003, утвердил Михаил). Факт «первое подключение с IP дата-центра Hetzner (HEL1)» — в `chain-facts.md` с датой.
10. Последний шаг — сверка через 1 ч после старта: блоков ≈ 35 700 ± 3%, `gaps.tsv` пуст (или объяснён), healthcheck без алертов.

## Что НЕ делать
- С сервера: никаких тестовых подключений к фиду (websocat, curl на `feed.*`, второй recorder, delayed-feed), никаких вызовов RPC; enricher не запускать.
- Не перезапускать recorder без причины (каждый рестарт — пауза ≥ 120 с и дыра); при обновлении бинарника — только по runbook.
- Docker/ClickHouse на сервер не ставить; бэкап не включать (хранилище не выбрано).
- Не коммитить IP, ключи, токены.
- Не перезагружать сервер, пока идёт resync RAID, без необходимости.

## Критерии приёмки
- Набор 011 проходит в контейнере `ubuntu:26.04`; правки не ломают 24.04.
- Recorder `active`, запись идёт ≥ 1 ч, `feed-audit` PASS, `gaps.tsv` пуст или каждая дыра объяснена.
- indexer-engineer: PASS по первым 15 мин (пути, флаги, заголовок досылки, строки `connections.tsv`).
- data-auditor: PASS по `feed-audit` и выборочной офлайн-проверке файлов за первый час (копия на Mac через `rsync` — только закрытые часовые файлы).

## Стоимость и риски
$0 сверх сервера. Риски: несовместимость 26.04 (п. 1), бан Cloudflare на IP дата-центра (п. 7), блокировка себя ufw/sshd (п. 3).

## Формат отчёта
Сделано; осмотр сервера; что поменяно для 26.04; цифры первого часа; что не получилось; что Михаилу сделать (Telegram-бот, хранилище бэкапа). `status: done`, отдельный коммит.
