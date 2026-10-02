# 026 — enricher: хвосты ревью
status: done
phase: 1a
depends-on: 025
executor: indexer-engineer
reviewers: architect-reviewer, data-auditor

## Зачем
`docs/reviews/020-*`, `024-feed-audit-analytics-data-auditor.md` (находка: файлы enricher без контрольной суммы zstd), `021-recorder-structure-architect-reviewer.md` (тесты enricher оставляют временные каталоги).

## Что сделать
1. Контрольная сумма zstd во всех файлах enricher (`blocks-*`, `logs-*`): включить checksum при записи; чтение старых файлов без неё не ломается. Тест: испорченный байт обнаруживается.
2. Режим диапазона: кусок больше бюджета — ошибка конфигурации (выход 1), как в `--gaps`; `--dry-run` в режиме диапазона работает (только план) или явно отвергается — выбрать и задокументировать.
3. Джиттер — из `hood-core` (задача 025), свой генератор удалить.
4. Интеграционные тесты убирают за собой временные каталоги.
5. `deploy/enricher-gaps.service` и `deploy/README.md`: `--max-calls` с учётом вызова `eth_chainId` — значение по решению Михаила (варианты 4001 / 4201), до решения — оставить 4000 и обновить только текст README («1 000 блоков за прогон при 4000»).

## Что НЕ делать
- Не менять сетку файлов и формат строк. RPC — 0 (мок).

## Критерии приёмки
- fmt/clippy/test чистые; data-auditor: PASS (побайтово распакованное содержимое = HEAD; checksum в `zstd -lv`); architect-reviewer: PASS.

## Формат отчёта
Сделано; проверено и как. `status: done`, отдельный коммит.
