# 037 — архитектурное ревью (architect-reviewer)

- Дата: 2026-10-05.
- Объём: незакоммиченное рабочее дерево относительно `3a9eafb`: `crates/decoders/src/pools.rs` (`STOCK_QUOTE_RANK`, `ADDRESS_TIEBREAK_MIN_RANK`, const-проверка, doc), `crates/decoders/src/swap_rows.rs` (выбор котировки через `cmp`, doc, юнит-тест), `crates/decoders/tests/stock_quote.rs` и `tests/fixtures/stock-pair-block.jsonl` (новые), `.claude/skills/hoodchain-mev/references/data-model.md`. `docs/STATE.md` и файл задачи не ревьюились (не код).
- **Вердикт: PASS с замечаниями.** Блокирующих нет, важных нет. Есть 6 рекомендаций, все необязательные.

## Линт: вывод команд (проверено 2026-10-05, локально, без сети, `CARGO_TARGET_DIR` в scratchpad, каталог удалён после работы)

```
cargo fmt --all -- --check                                   -> чисто (exit 0)
cargo clippy -p decoders --all-targets --offline -- -D warnings  -> Finished, 0 предупреждений
cargo test -p decoders --offline
  lib 44 passed; l1_inflows 23; pons_v2_hook 2; rows 1; stock_quote 2; swap_rows 4; swaps 8; doctests 0 — 0 failed
```

Pedantic/nursery (рекомендательно), только по изменённым строкам:
- `pools.rs:438`, `:443`, `:451`: `too_long_first_doc_paragraph` (doc у `ETH_QUOTE_RANK`, `STOCK_QUOTE_RANK`, `ADDRESS_TIEBREAK_MIN_RANK`);
- `tests/stock_quote.rs:15`: `doc_markdown`, `PoolKey` и `PositionManager` без обратных кавычек.

Workspace-wide clippy и тесты я не запускал: изменён только `decoders`. Исполнитель сообщает, что `--workspace` проходит (272 passed).

Фикстура сверена побайтно: единственная строка `stock-pair-block.jsonl` совпадает со строкой из `data/samples/hourly-20260804-20260930.jsonl.zst` (`zstd -dc | grep | cmp`, IDENTICAL).

## Корректность выбора котировки (проверено чтением кода и тестами)

`swap_rows.rs:105-117`. `quote_of` возвращает запись, только если `quote_rank.is_some()`. Поэтому в ветке `(Some(q0), Some(q1))` оба ранга равны `Some`.

| currency0 | currency1 | результат | чем покрыто |
|---|---|---|---|
| не котировка | не котировка | `NoQuote` | `status_quote_and_pool_rules` (empty), `stock_quote.rs::stock_pair_without_stock_quotes_has_no_row` |
| котировка | не котировка | quote = c0 | `buy_and_sell…` (наоборот: Q = c1); прежние тесты |
| не котировка | котировка | quote = c1 | `buy_and_sell_from_pool_side_signs` |
| r0 < r1 | | quote = c0 (`Less => false`) | `status_quote_and_pool_rules` (ranked 1/2) |
| r0 > r1 | | quote = c1 (`Greater => true`) | новый тест, «mixed» (акция 3 на c0, ранг 2 на c1) |
| r0 = r1 ≥ 3 | | quote = меньший адрес = c0 | новый тест (3, 4, 255), `stock_quote.rs` на реальном блоке |
| r0 = r1 < 3 | | `AmbiguousQuote` | новый тест (1, 2), `status_quote_and_pool_rules` (2) |

- Порог `q0.quote_rank >= Some(ADDRESS_TIEBREAK_MIN_RANK)` верен: `None < Some(_)` в порядке `Option`, но `None` сюда не попадает.
- При равенстве `pool.currency1 < pool.currency0` по инварианту `PoolRegistry::insert` (`pools.rs:209`) и `observe_log` (Malformed при `c0 > c1`) всегда `false`, значит котировка = `currency0`. Выражение сравнивает адреса буквально, а не жёстко берёт «c0». Поэтому правило «меньший адрес» осталось бы верным и при нарушении инварианта. Это хорошо, так и оставить.
- Порядок загрузки реестра на выбор не влияет: выбор зависит только от рангов и адресов.
- После выбора `quote_entry` берётся заново через `expect` (`swap_rows.rs:123`). Инвариант не изменился: котировка найдена выше.

## Блокирующее

Нет.

## Важное

Нет.

## Рекомендации

1. **[swap_rows.rs:104, 113] Сравнивать ранги как `u8`, а не как `Option<u8>`.** `q0.quote_rank >= Some(ADDRESS_TIEBREAK_MIN_RANK)` опирается на порядок `Option` (`None` меньше любого `Some`) и на то, что фильтр выше уже отсеял `None`. Из записей `q0`/`q1` дальше используется только ранг. Исправление: `quote_of` возвращает сам ранг, поведение то же.
   ```rust
   let rank_of = |c: &Address| inputs.tokens.get(c).and_then(|t| t.quote_rank);
   let quote_is_1 = match (rank_of(&pool.currency0), rank_of(&pool.currency1)) {
       (None, None) => return Err(SkipReason::NoQuote),
       (Some(_), None) => false,
       (None, Some(_)) => true,
       (Some(r0), Some(r1)) => match r0.cmp(&r1) {
           Ordering::Less => false,
           Ordering::Greater => true,
           // Same rank: the lower address is the quote (currency0 by the PoolRegistry invariant).
           Ordering::Equal if r0 >= ADDRESS_TIEBREAK_MIN_RANK => pool.currency1 < pool.currency0,
           Ordering::Equal => return Err(SkipReason::AmbiguousQuote),
       },
   };
   ```
   Если захочется табличного теста на все 9 комбинаций без сборки реестров, этот `match` можно вынести в приватную чистую функцию `fn quote_is_1(r0: Option<u8>, r1: Option<u8>, c0: Address, c1: Address) -> Result<bool, SkipReason>` в том же файле. Сейчас это необязательно: все ветки уже покрыты (таблица выше).

2. **[pools.rs:458] Const-проверка наполовину тавтологична.** `STOCK_QUOTE_RANK >= ADDRESS_TIEBREAK_MIN_RANK` всегда истинно, потому что `ADDRESS_TIEBREAK_MIN_RANK` определён как `STOCK_QUOTE_RANK`. Смысл несёт только `ETH_QUOTE_RANK < ADDRESS_TIEBREAK_MIN_RANK`: проверка не даёт понизить порог так, что пул ETH/WETH начнёт давать строки. Исправление: оставить одно условие с сообщением, чтобы при срабатывании было понятно, что сломано.
   ```rust
   const _: () = assert!(ETH_QUOTE_RANK < ADDRESS_TIEBREAK_MIN_RANK, "an ETH/WETH pool is a wrap, not a price");
   ```
   Если первую половину хочется сохранить как защиту на случай, когда порог когда-то отвяжут от `STOCK_QUOTE_RANK`, её можно оставить, но тогда об этом стоит сказать в комментарии.

3. **[pools.rs:455] Где жить порогу.** Ранги (`ETH_QUOTE_RANK`, `STOCK_QUOTE_RANK`) — это данные реестра, им место в `pools.rs`. Порог — политика построения строки: он используется только в `swap_rows::swap_row` и связан с `SkipReason::AmbiguousQuote`, который тоже живёт в `swap_rows.rs`. По слоям правильнее держать `ADDRESS_TIEBREAK_MIN_RANK` и const-проверку в `swap_rows.rs`, рядом с `match`, а в `pools.rs` оставить только ранги и ссылку `[`crate::swap_rows::ADDRESS_TIEBREAK_MIN_RANK`]` в doc `TokenEntry::quote_rank`. Ничего не ломает: внешних пользователей константы нет (grep по `crates`, `analytics` — только `decoders`). Если оставить как есть, это тоже допустимо: связь с `STOCK_QUOTE_RANK` явная, и `pools.rs` уже служит местом «правил рангов». Логику выбора (`match`) переносить в `pools.rs` **не нужно**: `TokenRegistry` должен остаться хранилищем без знания о `SkipReason`.

4. **[swap_rows.rs:426-475] Тест: дубли значений и шаблонные вставки.** В массиве `[STOCK_QUOTE_RANK, ADDRESS_TIEBREAK_MIN_RANK, …]` оба элемента равны 3. Четыре вставки `TokenEntry { … }` в этом модуле (`registries`, `both`, `ranked`, `mixed`, `pair`) повторяют один шаблон. Исправление: небольшой хелпер в `mod tests`
   ```rust
   fn tokens(entries: &[(Address, u8, Option<u8>)]) -> TokenRegistry {
       let mut t = TokenRegistry::default();
       for &(token, decimals, quote_rank) in entries {
           t.insert(TokenEntry { token, decimals: Some(decimals), quote_rank, status: RegistryStatus::Verified }).unwrap();
       }
       t
   }
   ```
   и циклы `for rank in [ADDRESS_TIEBREAK_MIN_RANK, ADDRESS_TIEBREAK_MIN_RANK + 1, u8::MAX]` / `for rank in 1..ADDRESS_TIEBREAK_MIN_RANK`. Тест проверяет три разных правила, и его можно разбить на три (`same_rank_from_threshold_…`, `same_rank_below_threshold_is_ambiguous`, `stock_against_lower_rank_is_token`), чтобы при падении было видно, какое правило сломалось. Проверка `status_quote_and_pool_rules` «ранг 2 + ранг 2 → Ambiguous» теперь дублируется новым тестом. Её можно убрать, но это не обязательно.

5. **[tests/stock_quote.rs:44-50 и tests/pons_v2_hook.rs:66-69, tests/swap_rows.rs:75] Чтение одноблочной фикстуры повторяется.** Повтор из 3–4 строк (`parse_block_line(FIXTURE.lines().next().unwrap())` → `decode_block` → `swaps`) встречается в трёх файлах. Это меньше порога из скилла (6–8 строк), и общий `tests/common/mod.rs` ради этого пока не оправдан. Пункт записан на будущее: при пятом повторе стоит вынести `fn only_swap(fixture: &str, block: u64) -> PoolSwap`.

6. **[pools.rs:438-455, swap_rows.rs:9-11, :49, data-model.md:32-34] Правило описано в четырёх местах.** Сейчас все четыре описания совпадают с кодом. Чтобы при следующей смене правила не разошлись: подробное описание оставить в doc `ADDRESS_TIEBREAK_MIN_RANK` и в `data-model.md`, а в doc модуля `swap_rows` и у `SkipReason::AmbiguousQuote` давать только ссылку (`see [ADDRESS_TIEBREAK_MIN_RANK]`). Заодно уйдёт pedantic `too_long_first_doc_paragraph`: первая строка doc — короткое определение, подробности — отдельным абзацем. И добавить обратные кавычки в `stock_quote.rs:15`.

## Сверка документации с кодом

- `data-model.md:32-34` (ранги 1/2/3, правило «один ранг → меньший адрес с ранга 3», ETH/WETH → `ambiguous_quote`, инвариант `currency0 < currency1`, независимость от порядка загрузки, отсылка к «Перезагрузке») совпадает с `swap_rows.rs:105-117` и `pools.rs:209`.
- Заметка про `uiMultiplier` (`data-model.md:35`, `:141`; doc `STOCK_QUOTE_RANK`) совпадает с кодом. `swap_row` нигде не масштабирует суммы акций: `price` = `scaled(quote)/scaled(token)` только по decimals, `*_raw` — сырые единицы. Интеграционный тест прямо проверяет цену в сырых единицах (`stock_quote.rs:72-73`). Формулировки «за 1 целый токен, не за акцию» и «перевод по множителю на момент блока, а не по `currentMultiplier`» верные и нужные. Наблюдение «в блоке 39645654 множитель SPY был 1» помечено как наблюдение по одной tx, это правильно.
- Мелочь в doc `STOCK_QUOTE_RANK` (`pools.rs:446-447`): «is the quote against any other token» верно только для токенов без ранга или с рангом > 3. Против другой акции котировкой становится меньший адрес. Можно дописать «… against any non-quote token; stock/stock: see [`ADDRESS_TIEBREAK_MIN_RANK`]».
- В `data-model.md` есть числа прогона (10 610 → 11 916 и т. п.). Это сложившаяся практика (так же было в 036), не замечание.

## Открытый вопрос исполнителя: разрешать ли два стейблкоина (ранг 1) по адресу?

**Мнение: нет, сейчас ничего не менять. А когда появится второй стейблкоин, разрешать пару явным рангом, а не адресом.**

- Сейчас такого случая нет: в реестре один стейблкоин (USDG), `ambiguous_quote` после 037 = 0. По правилу 4 менять порог на 1 «на вырост» не нужно.
- Для акций выбор по адресу оправдан: среди 194 акций нет естественной базовой валюты, и важна только детерминированность. Для стейблкоинов базовая валюта есть: вся аналитика в долларах считается в USDG. Пул USDG/USDC должен котироваться в USDG по смыслу, а не в том, у кого адрес меньше. Иначе направление ряда цены (USDC за USDG или USDG за USDC) будет случайным свойством адресов и поменяется от пула к пулу.
- Если пары стейблкоинов понадобятся (например, для депега или стейбл-арбитража), чище дать второму стейблкоину отдельный ранг, ниже приоритета USDG. Учтите, что ранги — целые числа, и между USDG = 1 и `ETH_QUOTE_RANK` = 2 свободного значения нет. Вставка потребует перенумерации: `ETH_QUOTE_RANK`, `STOCK_QUOTE_RANK` и значения в `tokens-*.tsv`. **Это ломает совместимость реестров TSV**, поэтому решение за Михаилом, а при перезаливке нужно удалить старые строки (раздел «Перезагрузка»). Понижать порог до 1 я бы не стал: это заодно заденет ETH/WETH (ранг 2 окажется выше порога, и пул ETH/WETH получит строку по адресу), и const-проверка из рекомендации 2 это правильно запретит.

## Дубли

- Логика выбора котировки есть только в `swap_rows::swap_row`. В `analytics/`, `sql/`, `loader` её копий нет (grep `quote_rank|AmbiguousQuote|ambiguous_quote`). Python-сравнение исполнителя (`compare.py`) лежит в scratchpad, вне репозитория, и как независимая проверка допустимо.
- В тестах есть мелкие повторы (рекомендации 4 и 5), разошедшихся по смыслу дублей нет.

## Структура и разбиение

Перекладка не нужна. `swap_rows.rs` — 496 строк, `pools.rs` — 794 (из них около 230 строк тестов): так было и до 037, задача объём почти не добавила. Возможный перенос порога в `swap_rows.rs` описан в рекомендации 3.

## Что хорошо (не сломать при правках)

- При равенстве рангов сравниваются адреса буквально (`currency1 < currency0`), а не берётся жёстко `currency0`. Правило «меньший адрес» не зависит от того, где проверяется инвариант.
- Адреса 194 акций не зашиты в код: они идут во вход вызывающего через TSV по образцу USDG. Ранг — одна константа.
- Интеграционный тест на неизменённой строке сырья с указанием источника (блок, tx, хэш блока, строка реестра) проверяет обе сырые суммы, сторону, цену в сырых единицах и `fee_quote`. Есть и обратный случай: без реестра акций — `no_quote`.
- Ниже порога поведение прежнее (ETH/WETH → `ambiguous_quote`), и это закреплено тестом и const-проверкой.
- Документация честно разделяет, где цена в токенах, а где в акциях, и запрещает переводить по `currentMultiplier`.

## Предполагается / не проверено

- Clippy и тесты по всему workspace я не запускал (только `-p decoders`). Для `--workspace` полагаюсь на отчёт исполнителя: изменения затрагивают только `decoders`, а другие крейты используют его публичный API, который не менялся (добавлены только константы).
- Числа `swaps_scan` (+1 306 строк, 66 «акция/акция», прежние строки не изменились) я не перепроверял: это зона data-auditor.
- Сеть не использовалась, docker не запускался. Временный каталог сборки `scratchpad/target-ar037` удалён.
