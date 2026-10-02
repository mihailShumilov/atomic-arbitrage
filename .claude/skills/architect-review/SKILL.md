---
name: architect-review
description: Architect-level code review procedure for the hoodchain-mev repo — duplication, module and type design (Rust traits/composition, Python classes/dataclasses), design patterns in the right places, file/module split, lint (cargo fmt, clippy incl. pedantic as advisory, ruff, shellcheck, sqlfluff-style checks), error handling, testability, naming, dependencies and project conventions. Use whenever reviewing a code change before closing a task, when asked for a "детальное код ревью", "архитектурное ревью", "проверь архитектуру/дубли/паттерны/линт", or when the architect-reviewer agent runs.
---

# architect-review — ревью кода на уровне архитектора

Цель — код, который легко читать, менять и тестировать. Ревью не ищет «идеальную архитектуру»: каждое замечание должно убирать реальную проблему (дубль, связанность, риск ошибки, неудобство тестирования) и иметь конкретное исправление.

## Порядок

1. **Объём.** Определи дифф (`git diff --stat <base>..HEAD`, или рабочее дерево) и затронутые модули. Прочитай задачу (`docs/handoff/to-code/NNN-*.md`), чтобы знать намерение.
2. **Линт (автоматика).** Запусти команды ниже, приложи вывод. Ошибки линтера по изменённым файлам — минимум «важное».
3. **Структура.** Пройди чек-лист языка из `references/` по изменённым файлам и их соседям.
4. **Дубли.** Ищи повторяющуюся логику по всему репозиторию, а не только в диффе (см. «Поиск дублей»).
5. **Тесты.** Покрыты ли новые ветки; тестируется ли логика без сети и диска (чистые функции, моки на 127.0.0.1); нет ли хрупких тестов (sleep, порядок, реальное время).
6. **Отчёт** по формату ниже.

## Линт — команды

```bash
# Rust (блокирующее)
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
# Rust (рекомендательное: не FAIL, а список «стоит посмотреть»)
cargo clippy --workspace --all-targets -- -W clippy::pedantic -W clippy::nursery -A clippy::module_name_repetitions 2>&1 | grep -E '^(warning|error)' | sort | uniq -c | sort -rn | head -30
# Python (analytics/, .claude/skills/**/scripts/)
uvx ruff check --select E,F,W,B,UP,SIM,PL,RUF --line-length 120 <files>
uvx ruff format --check <files>
python3 -m py_compile <files>
# Bash (deploy/)
docker run --rm --network none -v "$PWD:/w" -w /w koalaman/shellcheck:stable -x deploy/*.sh deploy/test/*.sh
bash -n deploy/*.sh
# в конце: docker rmi koalaman/shellcheck:stable; cargo clean
```

Не устанавливай пакеты глобально (brew/pip): `uvx` и docker-образ достаточно. Если инструмент недоступен — так и напиши, проверь вручную.

## Поиск дублей

- Похожие функции: `grep -rn "fn <имя>\|def <имя>"` по ключевым словам (parse, decode, read_tsv, zstd, retry, backoff, hex, unalias, now_utc …).
- Повторяющиеся константы и магические числа (адреса, topic0, таймауты, пути): должны жить в одном месте (`hood-core`, модуль констант, конфиг).
- Одинаковые блоки ≥ 6–8 строк: `npx --yes jscpd --min-lines 8 --reporters console crates analytics deploy` (по желанию; если npx недоступен — вручную по grep).
- Дубли между Rust и Python (например, разбор фида в recorder и в `feed_audit.py`) — допустимы, если это намеренная независимая проверка; тогда должно быть явно сказано, какой источник истины и чем сверяются.

## Уровни серьёзности

- **Блокирующее (FAIL):** ошибки линтера/clippy `-D warnings`; падение тестов; тихая порча или потеря данных; паника/`unwrap` на внешних данных; дубль, уже разошедшийся по смыслу; нарушение правил проекта (адрес не `verified` в коде, код подписи транзакций, изменение формата сырья без задачи).
- **Важное:** дублирование логики; модуль/файл с несколькими несвязанными ответственностями; утечка деталей слоя (сеть/диск в доменной логике); нетестируемая логика; ошибки без контекста; публичный API шире нужного.
- **Рекомендация:** нейминг, мелкие упрощения, pedantic-предупреждения, документация, перестановка кода.

Ориентиры размера (не догма): файл > ~600 строк или функция > ~60 строк / > 4 уровней вложенности — повод проверить, не смешаны ли ответственности. Разбивать только по смысловым границам, не механически.

## Формат отчёта

```
# NNN — архитектурное ревью (architect-reviewer)
Дата, объём (коммиты/файлы), вердикт: PASS / PASS с замечаниями / FAIL

## Линт — вывод команд (кратко, проверено <дата>, как)
## Блокирующее        — [файл:строка] проблема → почему → исправление
## Важное              — то же
## Рекомендации        — то же
## Дубли               — где, насколько разошлись, куда вынести
## Структура и разбиение — предложенная раскладка модулей/файлов (если нужна)
## Что хорошо          — 2–5 пунктов, чтобы не сломать при правках
## Предполагается / не проверено
```

Чек-листы: `references/rust.md`, `references/python.md`, `references/bash-sql.md`.
