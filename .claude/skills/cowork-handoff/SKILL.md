---
name: cowork-handoff
description: Working protocol between Cowork (strategy and research) and console Claude Code (implementation) for Michael's Robinhood Chain project in /Users/mihailshumilov/sites/my/crypto/atomic-arbitrage (hoodchain-mev, atomic arbitrage, meme momentum). Use whenever writing a task for Claude Code, reviewing a Claude Code report, recording a decision, updating docs/STATE.md, writing the weekly summary, or choosing which subagent should do a job — even for small tasks.
---

# Протокол Cowork ↔ Claude Code

Cowork думает, исследует, решает и ставит задачи. Claude Code пишет, запускает и проверяет код. Михаил утверждает решения, переходы между фазами, траты и адреса контрактов.

Корень репозитория: `/Users/mihailshumilov/sites/my/crypto/atomic-arbitrage`. Факты о сети, адреса, правила данных и бэктеста — в скилле `hoodchain-mev` (`.claude/skills/hoodchain-mev/`). При расхождении с этим скиллом прав `hoodchain-mev`.

## Папки

| Путь | Кто пишет | Что |
|---|---|---|
| `docs/STATE.md` | Cowork | Текущее состояние, одна страница |
| `docs/decisions/NNNN-slug.md` | Cowork, утверждает Михаил | Записи решений |
| `docs/research/slug.md` | Cowork | Исследовательские планы и выводы |
| `docs/handoff/to-code/NNN-slug.md` | Cowork | Задачи для Claude Code |
| `docs/handoff/from-code/NNN-slug.md` | Claude Code | Отчёты |
| `docs/reviews/NNN-slug.md` | Cowork | Разборы отчётов |
| `docs/costs.md` | infra-ops | Учёт расходов |

Номер задачи, отчёта и разбора совпадает, нумерация сквозная.

## Цикл

1. Cowork пишет задачу (`status: ready`) по шаблону из `references/templates.md`.
2. Михаил запускает: `cd <корень> && claude "Выполни задачу docs/handoff/to-code/NNN-slug.md, отчёт положи в docs/handoff/from-code/NNN-slug.md"`.
3. Claude Code выполняет, пишет отчёт, ставит в задаче `status: done`.
4. Cowork пишет разбор (принять / доработать / отклонить), обновляет `STATE.md`, при необходимости — следующую задачу или запись решения.

## Кто исполняет и кто проверяет

| Тип задачи | Исполнитель (субагент Claude Code) | Обязательная проверка |
|---|---|---|
| Конвейер: recorder, enricher, декодеры, загрузка, схема | indexer-engineer | data-auditor |
| Аналитика: арбитраж, импульс, портреты конкурентов | arb-researcher | data-auditor (данные) + skeptic-analyst (выводы) |
| Движок: revm, состояние, маршруты, симулятор, бумажная торговля, латентность | engine-engineer | бенчмарки; skeptic-analyst для любых оценок прибыли |
| Серверы, мониторинг, бэкапы, деплой, расходы | infra-ops | Михаил утверждает траты |
| Смарт-контракт исполнителя (фаза 2+) | engine-engineer | contract-reviewer — обязательно до развёртывания |
| Любой вывод «стратегия прибыльна» | — | skeptic-analyst, затем решение Михаила |

В каждой задаче явно указывай исполнителя и проверяющих.

## Жёсткие границы

- До объявления Михаилом фазы 3: никакой подписи и отправки транзакций, ключей, кошельков. Бумажная торговля только записывает.
- Адреса протоколов — только из `contracts.md` со статусом `verified`.
- Тесты фида — никогда с IP продакшен-сервера.
- Факт помечается «проверено (дата, как)», иначе это гипотеза.
- Налоги и право — не решаем, отмечаем «уточнить у бухгалтера/юриста».

## Шаблоны

`references/templates.md`: задача, разбор, запись решения, STATE.md, еженедельная сводка.
