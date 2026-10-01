# 015 — Сервер: SMART-мониторинг дисков + мелочи healthcheck из 014
status: done
phase: 1a
depends-on: 014
executor: infra-ops
reviewers: indexer-engineer

## Зачем
RAID-алерт (014) срабатывает, только когда диск уже выпал. SMART показывает диск, который начинает портиться. Решение Михаила 2026-10-01: ставить, хотя идёт 7-суточная запись.

## Контекст
- Отчёт и ревью 014 (`docs/handoff/from-code/014-server-housekeeping-kind12.md`, `docs/reviews/014-*`): smartmontools не установлен; замечания ревьюера З3 (healthcheck молчит, если в mdstat только inactive-массивы) и З4 (`mdmonitor-oneshot.timer` шлёт ежедневное повторное ALERT при деградации — описать в README).
- Шум `sendmail: not found` от `MAILADDR root` — решение Михаила: оставить как есть.
- Сервер `hood-rec`: 2 × HDD Seagate 4 ТБ в RAID1; запись идёт с 2026-10-01 09:57:12Z.

## Что сделать
1. `bootstrap.sh` (идемпотентно): пакет `smartmontools`; конфиг `smartd` (оба диска, короткий тест раз в сутки и длинный раз в неделю в спокойные часы, пороги температуры), `-M exec` → обёртка над `deploy/notify.sh` (ALERT на сбой/рост переназначенных секторов, INFO на тестовое сообщение `-M test`).
2. Проверка `smart` в `healthcheck.sh` не нужна, если smartd шлёт сам; но healthcheck должен видеть, что `smartd` активен (ALERT, если нет).
3. З3: `raid` учитывает inactive-массивы; тест. З4: абзац в README.
4. Тесты в `deploy/test/` (подставной smartctl/smartd-событие), shellcheck; стенд 24.04/26.04 по возможности.
5. Применение на сервере — командами для Михаила (привилегированные шаги агентам заблокированы): rsync закоммиченного состояния, bootstrap, тестовое уведомление smartd, проверки.

## Что НЕ делать
- Не перезапускать recorder, не перезагружать сервер; длинный SMART-тест не запускать вручную во время resync RAID.
- Никаких подключений к фиду и вызовов RPC.

## Критерии приёмки
- shellcheck и тесты `deploy/test/` чистые; второй bootstrap — 0 изменений.
- На сервере: `smartd` active, `smartctl -H` по обоим дискам PASSED, тестовое уведомление smartd в journald; `systemctl show recorder -p NRestarts,ActiveEnterTimestamp,MainPID` без изменений.
- indexer-engineer: PASS.

## Стоимость и риски
$0. Риск — рестарт recorder при apt (needrestart исключает recorder — проверить).

## Формат отчёта
Сделано; проверено и как; команды для Михаила; что не получилось. `status: done`, отдельный коммит.
