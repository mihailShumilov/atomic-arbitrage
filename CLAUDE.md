# hoodchain-mev

Исследовательский конвейер по Robinhood Chain: запись фида секвенсора, обогащение через RPC, декодирование свопов, ClickHouse, бэктесты. Владелец — Михаил. Общайся по-русски, код и комментарии в коде — на английском.

**Перед любой работой прочитай скилл `.claude/skills/hoodchain-mev/SKILL.md`.** Он главнее этого файла в вопросах фактов о сети, адресов и правил бэктеста.

## Фазы

**1a. Запись (сейчас).** recorder работает непрерывно; enricher дозаливает блоки и дыры. Готово, когда: 7 суток записи без незаполненных дыр, загрузка истории с 04.08.2026 (запуск Pons v2) завершена.

**1b. Декодирование.** Реестр контрактов доведён до `verified` для Pons v1/v2, pools.trade, WETH, USDG, Uniswap v3/v4. Декодеры → `hood.swaps`, `hood.tokens`. Готово, когда data-auditor даёт PASS.

**1c. Сущности и исследование.** Кошельки, граф финансирования, метки, исследование мем-импульса по `references/momentum-research.md`. Готово, когда skeptic-analyst дал вердикт по отчёту, а Михаил принял решение.

**2. Бумажная торговля.** Только по решению Михаила. **3. Живая торговля с малым бюджетом.** Только по решению Михаила.

В фазе 1 запрещено: код подписи и отправки транзакций, приватные ключи, обращение к кошелькам.

## Карта репозитория

```
crates/hood-core    общие типы, константы, разбор конверта фида, детектор дыр
crates/recorder     фид → data/feed/…/feed-*.tsv.zst, gaps.tsv, last_seq.txt
crates/enricher     RPC: блоки(full) + receipts → data/blocks/*.jsonl.zst
crates/decoders     события Uniswap v3/v4, ERC-20; launchpad'ы — TODO
sql/                схема ClickHouse (миграции: 001_, 002_, …)
analytics/          Python: исследования и бэктесты (после PASS от data-auditor)
docs/               общая папка с Cowork: STATE.md, costs.md, decisions/, research/,
                    handoff/to-code/ (задачи), handoff/from-code/ (отчёты), reviews/
.claude/skills/     hoodchain-mev — конституция проекта;
                    cowork-handoff — протокол работы с Cowork и шаблоны;
                    feed-audit — скрипт проверки записи фида;
                    architect-review — порядок и чек-листы архитектурного ревью кода;
                    clickhouse-best-practices, clickhouse-architecture-advisor —
                    сторонние (clickhouse/agent-skills, skills-lock.json); при
                    расхождении прав hoodchain-mev (data-model.md)
.claude/agents/     indexer-engineer, data-auditor, skeptic-analyst, arb-researcher,
                    engine-engineer, infra-ops, contract-reviewer, contract-registrar,
                    architect-reviewer
```

## Кто исполняет и кто проверяет

| Тип задачи | Исполнитель | Обязательная проверка |
|---|---|---|
| Конвейер: recorder, enricher, декодеры, загрузка, схема | indexer-engineer | data-auditor + architect-reviewer (код) |
| Аналитика: арбитраж, импульс, портреты конкурентов | arb-researcher | data-auditor (данные) + skeptic-analyst (выводы) |
| Движок: revm, состояние пулов, маршруты, симулятор повтора, бумажная торговля, латентность | engine-engineer | бенчмарки; skeptic-analyst для оценок прибыли |
| Серверы, мониторинг, бэкапы, деплой, учёт расходов | infra-ops | траты утверждает Михаил; architect-reviewer для скриптов `deploy/` |
| Реестр контрактов: адреса, ABI, статусы в `contracts.md` | contract-registrar | `verified` ставит Михаил |
| Смарт-контракт исполнителя (фаза 2+) | engine-engineer | contract-reviewer — обязательно до развёртывания |
| Любой вывод «стратегия прибыльна» | — | skeptic-analyst, затем решение Михаила |

## Команды

```bash
cp .env.example .env              # вписать RPC_URL и пароль ClickHouse
cargo build --release --workspace
cargo test --workspace
cargo run --release -p recorder -- --out-dir data/feed
cargo run --release -p enricher -- --from <N> --to <M> --out-dir data/blocks
docker compose up -d clickhouse && sql/apply.sh   # миграции sql/NNN_*.sql, журнал hood.schema_migrations
```

Docker: только уникальные порты хоста и только `127.0.0.1` — на машине работают другие проекты. ClickHouse: HTTP `127.0.0.1:18123`, native `127.0.0.1:19100` (`CLICKHOUSE_HTTP_PORT` / `CLICKHOUSE_TCP_PORT` в `.env`). Новые сервисы — тоже на свободных нестандартных портах, проверять `lsof -iTCP -sTCP:LISTEN`.

## Как работать

- Изменения кода — через индексатора (indexer-engineer); после изменения декодеров/загрузчиков — data-auditor; любой результат для Михаила — через skeptic-analyst.
- Любое нетривиальное изменение кода (Rust, Python, Bash, SQL) перед закрытием задачи — ревью architect-reviewer (скилл `architect-review`): дубли, структура типов и модулей, паттерны, разбиение на файлы, линт, лучшие практики. Блокирующие замечания исправляет исполнитель.
- Разделяй «проверено на данных (дата, как)» и «предполагается».
- Не добавляй зависимости без причины; предпочитай то, что уже в `[workspace.dependencies]`.
- Не коммить `data/`, `.env`, ключи провайдеров.

## Работа с Cowork

Стратегия, исследовательские планы и постановка задач — в Cowork. Протокол и шаблоны — скилл `.claude/skills/cowork-handoff/`.

- Задачи приходят файлами `docs/handoff/to-code/NNN-slug.md`. Прежде чем начать, проверь: `status: ready` и все задачи из `depends-on` имеют `status: done`. Если нет — остановись и скажи Михаилу.
- На время работы поставь в задаче `status: in-progress`, по окончании — `status: done` (или опиши в отчёте, почему не выполнено).
- Исполнителя и проверяющих бери из полей `executor` / `reviewers` задачи.
- Отчёт — в `docs/handoff/from-code/NNN-slug.md` (тот же номер и slug). Формат — раздел «Формат отчёта» задачи: сделано; что проверено и как; цифры; что не получилось; вопросы к Cowork/Михаилу. Разделяй «проверено (дата, как)» и «предполагается».
- Решения в `docs/decisions/` со статусом `accepted` обязательны к исполнению; `proposed` — только контекст, не обязательство.
- Текущее состояние проекта — `docs/STATE.md` (ведёт Cowork, не правь его).
- Новые проверенные факты о сети — в `references/chain-facts.md` скилла `hoodchain-mev`, с датой и способом проверки.

## Ближайшие задачи

Очередь — в `docs/handoff/to-code/` (по номерам). Сводка — в `docs/STATE.md`.
