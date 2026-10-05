# 034 — healthcheck: `backfill` молчит при отсутствующем/пустом `filled.tsv`
status: in-progress
phase: 1a
depends-on: 028
executor: infra-ops
reviewers: architect-reviewer, data-auditor

## Зачем
data-auditor 030 (Б3, `docs/reviews/030-fill-restart-gaps-public-rpc-data-auditor.md`): `deploy/healthcheck.sh` (~299) подставляет `/dev/null` вместо отсутствующего `filled.tsv`, а awk с `FNR == NR` на пустом первом файле читает строки `gaps.tsv` как «залитые» → 0 незалитых, алерт `backfill` не срабатывает. Тест (`deploy/test/test-healthcheck.sh` ~74) всегда создаёт `filled.tsv` с комментарием — случай «нет файла»/«пустой файл» не покрыт.

## Что сделать
1. Исправить подсчёт (например, `FILENAME == ARGV[1]` или явная передача пути/флаг), чтобы отсутствующий или пустой `filled.tsv` давал «всё незалито».
2. Тесты: нет `filled.tsv`; пустой `filled.tsv`; частичное покрытие; полное покрытие — ожидаемые алерты/восстановления. На старом коде новые тесты падают.
3. Проверить `feed-audit`/README на тот же приём `FNR==NR` с возможно пустым первым файлом; поправить при наличии.
4. Выкладка через `bootstrap.sh` — командами для Михаила, recorder не трогается.

## Критерии приёмки
- shellcheck и deploy/test чистые (ubuntu:24.04 --network none); architect-reviewer и data-auditor: PASS.

## Формат отчёта
Сделано; проверено и как; команды. `status: done`, отдельный коммит.
