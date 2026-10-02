# Полное ревью, часть 3 из 4: crates/decoders. Архитектурное ревью (architect-reviewer)

- Дата: 2026-10-02.
- Объём: HEAD `d241243`, крейт целиком:
  - `crates/decoders/src/lib.rs` (151 строка), `src/l1_inflows.rs` (858);
  - `tests/l1_inflows.rs` (244), фикстура `tests/fixtures/l1-inflows-blocks.jsonl`;
  - `examples/l1_inflows_scan.rs` (167).
- Контекст: `sql/001_schema.sql`, `sql/002_funding_edges_l1.sql`, `crates/enricher/src/{blocks,logs}.rs`, `crates/hood-core/src/lib.rs`, `references/{data-model,contracts}.md`, `docs/reviews/017-decoder-l1-inflows-data-auditor.md`.
- **Вердикт: FAIL.** Единственная блокирующая причина — `cargo fmt --check`, исправляется одной механической правкой. По существу крейт в хорошем состоянии: clippy `-D warnings` чистый, 17 из 17 тестов проходят, паник на внешних данных нет. Для фазы 1b нужны перестройки из раздела «Важное», прежде всего В1–В3. В1 закрыть до задачи загрузчика.

## Линт: вывод команд (проверено 2026-10-02 на Mac, rustfmt 1.9.0, offline)

| команда | итог |
|---|---|
| `cargo fmt --all -- --check` | **exit 1**. В decoders расхождения в 4 файлах: `l1_inflows.rs` — 20 блоков, `tests/l1_inflows.rs` — 12, `lib.rs` — 5, `examples/l1_inflows_scan.rs` — 5. Тем же грешат `enricher` (все файлы) и `hood-core/src/lib.rs`: это уже отмечено в отчёте 008 («в enricher/decoders/hood-core» не чисто). `rustfmt.toml` в репозитории нет. |
| `cargo clippy -p decoders --all-targets -- -D warnings` | exit 0, чисто |
| `cargo test -p decoders` | 5 unit-тестов (lib + l1_inflows) + 12 интеграционных, все прошли, 0 упавших |
| clippy pedantic + nursery (рекомендательно) | ~75 предупреждений, сводка ниже |

Pedantic/nursery, сводка: 18 — long literal lacking separators; 16 — `Self` вместо имени типа; 10 — бэктики в доках; 12 — `#[must_use]`; 4 — `const fn`; 4 — нет `# Errors`; 2 — слишком длинная функция: `l1_inflows.rs:692` `retry` (111/100) и `examples/l1_inflows_scan.rs:26` `main` (138/100); по 1 — `&Option<T>` (`l1_inflows.rs:161`), `PartialEq` без `Eq` (`lib.rs:75`), регистрозависимое сравнение расширения (`l1_inflows_scan.rs:72`), `map().unwrap_or()`, `u8 as u64`.

Как проверял стиль. Код явно написан под ширину около 120 символов, самая длинная строка в `l1_inflows.rs` — 189. Проверил `rustfmt --check --config ...` на файлах decoders:
- `max_width=100` (по умолчанию) — 45 блоков расхождений;
- `max_width=120` — 25;
- `max_width=120,use_small_heuristics=Max` — 8.

## Блокирующее

**Б1. `cargo fmt --check` не проходит** во всех четырёх файлах крейта (вывод выше).
- Почему важно: `cargo fmt --check` в скилле — блокирующая проверка. Без единого конфига каждая следующая правка тянет в дифф переформатирование соседних строк, и ревью диффа декодеров 1b станет шумным.
- Исправление — отдельный коммит, в котором меняется только форматирование:
  ```toml
  # rustfmt.toml (корень репозитория) — ближе всего к текущему стилю
  max_width = 120
  use_small_heuristics = "Max"
  ```
  затем `cargo fmt --all`. Остаток (8 блоков в decoders) — переносы `assert_eq!` и выравнивание комментариев в `PoolSwap`. Поведение не меняется. Затрагивает и enricher/hood-core: согласовать с ревьюерами частей 1, 2 и 4, чтобы был один коммит, а не три.

## Важное

**В1. Ключ `hood.funding_edges` схлопывает разные рёбра** (`sql/001_schema.sql:91`, замечание аудитора З1; `data-model.md:88`). **Закрыть до задачи загрузчика.**
- Что не так: `ReplacingMergeTree ORDER BY (to_addr, block_number, tx_index)`. Ключ сортировки здесь же служит ключом дедупликации, и в нём нет ни `kind`, ни позиции лога. Пример — блок 77312169, tx 2: ребро `l1_token` (log 4) и будущее ребро `weth` из `Transfer` шлюз → получатель (log 3) имеют один ключ `(0x07ae…, 77312169, 2)`. После слияния останется одно, причём то, что вставлено последним. Это тихая потеря ребра.
- Предлагаемый ключ:
  ```sql
  -- 003_funding_edges_key.sql. Table is empty (no loader yet): recreate instead of migrating data.
  -- BREAKS COMPATIBILITY of the (empty) table: log_index becomes non-Nullable.
  DROP TABLE IF EXISTS hood.funding_edges;
  CREATE TABLE hood.funding_edges (
      block_number   UInt64,
      tx_index       UInt32,
      log_index      UInt32 DEFAULT 4294967295,  -- u32::MAX = tx-level edge (0x64, 0x68 ETH, plain tx.value)
      kind           Enum8('eth' = 1, 'weth' = 2, 'internal' = 3, 'l1_eth' = 4, 'l1_token' = 5),
      from_addr      String,
      to_addr        String,
      value_wei      String,
      tx_hash        String,
      token          String DEFAULT '',
      l1_token       String DEFAULT '',
      gateway        String DEFAULT '',
      gateway_status Enum8('none' = 0, 'observed' = 1, 'verified' = 2) DEFAULT 'none',
      l2_alias       String DEFAULT '',
      tx_type        UInt8 DEFAULT 0,
      l1_request_id  String DEFAULT '',
      ticket_id      String DEFAULT ''
  ) ENGINE = ReplacingMergeTree
  ORDER BY (to_addr, block_number, tx_index, kind, log_index);
  ```
- Почему именно так:
  - `to_addr` остаётся первым: основной запрос графа — «кто финансировал X»;
  - `log_index` не Nullable, чтобы не включать `allow_nullable_key`. Пустое значение кодируется `u32::MAX`, а не 0, потому что 0 — валидный индекс первого лога блока;
  - `kind` защищает от совпадения двух рёбер уровня tx в одной tx. Например, будущий декодер `eth` может прочитать `tx.value` у `0x68` как обычный перевод на `tx.to`, и без `kind` ключ совпал бы с `l1_eth`.
- Когда появятся трассировки (рёбра `internal`), нескольким внутренним переводам одной tx на один адрес понадобится порядковый номер вызова. Колонку `trace_index` добавить тогда же, заранее не нужно.
- В Rust: `FundingEdge.log_index` оставить `Option<u32>` и превращать `None` в `u32::MAX` в одном месте, при сериализации строки (см. В3). Это же закрывает З3 аудитора (NULL в TSV): Nullable-колонки больше нет.
- Обновить `data-model.md:88` и комментарий в `sql/002`.

**В2. Модель блока, hex-разбор и тип лога заперты в `l1_inflows.rs`, и типов лога уже два** (`l1_inflows.rs:78-163`, `:563-582`; `lib.rs:66-70`).
- Что не так:
  - `BlockLine`/`RpcBlock`/`RpcTx`/`RpcReceipt`/`RpcLog` (поля — `String`) и функции `hex_u256`/`hex_u64`/`parse_addr`/`parse_b256` — это модель файла `blocks-*.jsonl.zst`, а не логика входов с L1;
  - в крейте два разных «лога»: `RawLog<'a>` в `lib.rs` (заимствованный, без `log_index` и без контекста tx) и `DecodedLog` в `l1_inflows.rs` (владеющий, с `log_index`);
  - `decode_swap(&RawLog)` не знает ни `block_number`/`tx_index`/`log_index`, ни `tx.from`/`tx.to`. Строка `hood.swaps` требует всё это: `ORDER BY (token, block_number, tx_index, log_index)`, `trader = tx.from`, `router = tx.to` (`sql/001:44-61`).
- Почему важно: все декодеры 1b (свопы v3/v4, ERC-20, `tokens`, launchpad'ы, `wallets`) читают тот же файл. Без общего слоя каждый скопирует модель и hex-хелперы или импортирует их из `l1_inflows`, и слои перепутаются.
- Исправление: вынести в `src/model.rs` один типизированный слой, общий для всех декодеров:
  ```rust
  /// One line of data/blocks/blocks-*.jsonl.zst, typed once at the boundary.
  pub struct Block { pub number: u64, pub txs: Vec<Tx> }          // tx and its receipt merged
  pub struct Tx {
      pub index: u32, pub hash: B256, pub ty: u8,
      pub from: Address, pub to: Option<Address>, pub value: U256,
      pub status: bool,                       // required, see B6
      pub arb: ArbFields,                     // requestId, ticketId, maxRefund, refundTo, depositValue, retryValue
      pub logs: Vec<Log>,
  }
  pub struct Log { pub address: Address, pub topics: Vec<B256>, pub data: Bytes, pub index: u32 }
  /// Position + tx context every row needs (hood.* ordering key and trader/router).
  #[derive(Clone, Copy)] pub struct TxCtx<'a> { pub block: u64, pub tx: &'a Tx }
  pub fn parse_block_line(line: &str) -> anyhow::Result<Block>; // checks number, receipts alignment, hashes
  ```
  - Проверки согласованности, которые сейчас стоят в `decode_block` (`l1_inflows.rs:592-615`), переезжают в `parse_block_line`. Их получат все декодеры, а не только L1.
  - Два способа сделать типизированный разбор:
    - (а) оставить `String`-структуры serde приватными в `model.rs` и переводить их в типы один раз;
    - (б) включить у `alloy-primitives` feature `serde` (новой зависимости нет, только feature), чтобы `Address`/`B256`/`U256`/`Bytes` десериализовались прямо из hex.

    (б) короче, но то, что `U256` из serde принимает именно RPC-формат quantity, не проверено. Если выбрать (б), закрепить это тестом на фикстуре.
  - `RawLog` и `DecodedLog` удалить, все декодеры принимают `&Log` (+ `TxCtx`).
  - Для будущих логов из файлов `logs-*.jsonl.zst` (`{"number","logs"}`) использовать тот же `Log`, а `TxCtx` строить из `transactionIndex`/`transactionHash`. Но `from`/`to` там нет, поэтому `hood.swaps` надо строить из `blocks` (или джойном с `hood.txs`). Это решение для задачи 1b, его стоит записать в задаче.

**В3. Отображение «строка → ClickHouse» живёт в example, а список колонок продублирован в 4 местах** (`examples/l1_inflows_scan.rs:53`, `:87-106`, `:61`, `:111-121`; `l1_inflows.rs:348-370`; `sql/001:84-91` + `sql/002`).
- Что не так:
  - порядок колонок `funding_edges` задан четырежды: в SQL, в структуре `FundingEdge`, в строке заголовка example и в строке формата `writeln!` с 16 позиционными `{}`;
  - форматирование адресов (`{:#x}`), `Option` → `''` и NULL — тоже только в example.
- Почему важно: загрузчик (следующая задача) либо скопирует этот код, либо будет звать example. Если переставить поле в одном месте, колонки тихо сдвинутся. Тесты этого не ловят: example не тестируется.
- Исправление: перенести строковое представление в библиотеку, в `src/rows.rs`, рядом с типом строки:
  ```rust
  impl FundingEdge {
      /// Column order of hood.funding_edges (sql/001 + sql/002/003). Single source for TSV header and loader.
      pub const COLUMNS: [&'static str; 16] = ["block_number", "tx_index", /* … */ "ticket_id"];
      /// One TSV line in COLUMNS order: addresses/hashes lowercase 0x, U256 decimal, None -> '' (or u32::MAX for log_index).
      pub fn write_tsv(&self, w: &mut impl std::io::Write) -> std::io::Result<()>;
  }
  ```
  - Тест 1: golden-строка на блоке 77312169 (её уже сверял аудитор в `edges-rpc.tsv`).
  - Тест 2: `include_str!("../../../sql/002_funding_edges_l1.sql")` содержит каждое значение `EdgeKind::as_str()` и `GatewayStatus::as_str()`, чтобы Enum8 в SQL и enum в Rust не разошлись.
  - То же сделать для `UnaccountedFlow`.
  - Example сокращается до разбора аргументов и цикла, `main` перестаёт быть 138-строчным.
- Trait `ChRow { const COLUMNS; fn write_tsv }` вводить, только когда появится вторая таблица (`swaps`), и только если загрузчик будет обобщённым. Сейчас хватит inherent-методов.

**В4. Строковые типы в `FundingEdge`, хотя enum'ы уже есть** (`l1_inflows.rs:358`, `:365`, `:374-384`).
- Что не так: `kind: &'static str` (`"l1_eth"`/`"l1_token"`) и `gateway_status: &'static str` (`"none"`/…) при уже существующих `InflowKind` и `RegistryStatus`.
- Почему важно: в 1b рёбра `eth`/`weth`/`internal` будут строить другие декодеры. С `&str` опечатка `"l1eth"` компилируется и упадёт только на вставке в Enum8, а то и запишется значением по умолчанию.
- Исправление:
  ```rust
  /// Mirrors hood.funding_edges.kind Enum8 (sql/001 + sql/002).
  pub enum EdgeKind { Eth, Weth, Internal, L1Eth, L1Token }
  /// Mirrors gateway_status Enum8; `None` for ETH rows and unregistered gateways.
  pub struct FundingEdge { pub kind: EdgeKind, pub gateway_status: Option<RegistryStatus>, /* … */ }
  ```
  `as_str()` вызывать только в `write_tsv`. Формат TSV не меняется. В тестах `e.kind == "l1_eth"` заменить на `EdgeKind::L1Eth`.

**В5. `decode_swap` — база `hood.swaps`, но не протестирован и прячет ошибки** (`lib.rs:88-119`; тесты `lib.rs:121-151`).
- Что не так:
  - тестов разбора нет совсем: есть только проверка topic0 и `print_topics` без единого assert;
  - любая ошибка декодирования (`.ok()?`) неотличима от «не своп». Лог с topic0 `Swap`, но с испорченными данными или чужой ABI с тем же topic0 молча пропадёт;
  - нет позиции и контекста tx (см. В2).
- Почему важно: data-auditor для 1b должен видеть число отброшенных логов. Без него нельзя доказать, что `hood.swaps` полна.
- Исправление:
  ```rust
  pub enum SwapDecode { NotSwap, Swap(PoolSwap), Malformed(alloy_sol_types::Error) }
  pub fn decode_swap(ctx: TxCtx<'_>, log: &Log) -> SwapDecode;
  ```
  или `Result<Option<PoolSwap>, _>`; `Malformed` считать в счётчиках, как сделано в `l1_inflows::Counters`.
  - Тесты: фикстура из 1–2 реальных блоков с v3 и v4 `Swap` (источник и дата в doc-комментарии, как в `tests/l1_inflows.rs:1-17`), проверка знаков amount и `fee_pips`.
  - `print_topics` удалить.
  - Что `decode_raw_log` в alloy 1.7 не проверяет лишние или недостающие topics, не проверено. Закрепить тестом на лог с 2 topics.

**В6. Отсутствующий `status` в чеке молча считается неуспехом** (`l1_inflows.rs:126-127`, `:631`).
- Что не так: `status: Option<String>` с `#[serde(default)]`, затем `ok = … == Some(1)`. Если чек без `status` (испорченная строка, другой формат RPC), успешный `0x64` уходит в `DepositNotSucceeded`, и ребро пропадает из `funding_edges`. Это не ошибка ввода, а «семантика», причём неверная.
- Почему важно: модуль сам заявляет (`:588-589`), что битый ввод даёт ошибку, а странности — счётчики. Здесь битый ввод становится счётчиком.
- Исправление: `pub status: String` без `default` (тогда serde выдаст ошибку с путём) или `bail!("block {n} tx {i}: receipt without status")`. На Robinhood Chain все чеки содержат `status`: в фикстуре 4 из 4 блоков, у аудитора 2 712 блоков прошли без ошибок.

**В7. `GatewayRegistry::new` обходит проверку дублей** (`l1_inflows.rs:202-204` против `:221-229` и `:271-273`).
- Что не так: `extend` и `parse_tsv` запрещают повтор шлюза, а `new(vec![…])` нет. `get` (`:235`) возвращает первое совпадение, и запись `observed`, стоящая раньше, перекроет `verified` для того же адреса. Это ровно тот инвариант, ради которого сделан запрет («no silent override of a verified entry»).
- Исправление: одна приватная `fn push_unique(&mut self, e) -> Result<()>`, через которую идут `new` (→ `Result<Self>`), `extend` и `parse_tsv`. Поле `entries` уже приватное, так что больше путей нет. `new` вызывается только в тестах (`tests/l1_inflows.rs:49`), поэтому правка дешёвая.

## Рекомендации

- **Р1. Разбить `retry`** (`l1_inflows.rs:692-808`, 117 строк, pedantic too_many_lines). Естественные границы:
  - `fn gateway_token_rows(b: &TxBase, to: Address, logs: &[Log], reg: &GatewayRegistry, c: &mut Counters) -> Result<Vec<L1Inflow>>` — строки 713-778;
  - чистая `fn matching_l2_transfer(logs: &[Log], before: u32, gateway: Address, to: Address, amount: U256) -> Option<Address>` — строки 726-737.

  Правило поиска `Transfer` (последний перед `DepositFinalized`, от шлюза или от 0x0, точная сумма) получит свои unit-тесты: сейчас ветка mint `0x0 → to` стандартного шлюза не покрыта ничем, даже синтетикой.
- **Р2. Три литерала `L1Inflow` по 14 полей** (`:646-661`, `:755-777`, `:788-803`) → `impl TxBase { fn inflow(&self, kind, to, amount) -> L1Inflow }`, отличающиеся поля (`log_index`, `ticket_id`, `token`, `l1_request_id`) дописывать после. Минус ~30 строк и одно место для новых общих полей.
- **Р3. `Counters::merge` перечисляет поля вручную** (`:507-526`). Если добавить счётчик и забыть его здесь, он тихо пропадёт из итогов example. Исправление: `let Counters { blocks, txs, tx_types, … } = o;` — деструктуризация без `..`, тогда компилятор заставит обновить `merge`.
- **Р4. Суммы `U256` молча переполняются по модулю.**
  - Факт: в ruint 1.20.1 `Add` = `wrapping_add`, проверено в `src/add.rs:234` в реестре cargo.
  - Где: `token_sum` (`:781`) и `Agg::add` для `token_rows_unregistered`. Суммы берутся из `DepositFinalized` любого эмиттера (З2 аудитора), то есть их контролирует атакующий. Переполнение может подделать равенство `tx.value == token_sum` и спрятать `gateway_eth_unexplained`.
  - Исправление: `checked_add` и, при переполнении, запись «не учтено» или счётчик.
  - Кроме того, `token_rows_*.sum` складывает сырые единицы разных токенов. В документации это сказано (`:487`), но число бессмысленно. Лучше оставить только `n` или разбить по `l2_token`.
- **Р5. `hex_u256` принимает `""`, `"0x"` и строку без префикса** (`:140-146`), пустое значение становится нулём. В RPC quantity всегда `0x` + хотя бы одна цифра. Ноль для пустого поля маскирует битый ввод, к тому же enricher (`logs.rs:77`) префикс требует. Отклонять пустое значение и отсутствие `0x`. После В2 вопрос уходит в `model.rs`.
- **Р6. Ошибки без контекста tx** (`:621-631`, `:670-672`, `:693-695`). `parse_addr(&tx.from)?` даст «bad address …» без номера tx. Example добавляет `file:line`, то есть блок, но не tx. Обернуть разбор tx в `.with_context(|| format!("block {n} tx {i} {}", tx.hash))`.
- **Р7. Предварительный проход разбирает `tx_type` и `ticketId` повторно** (`:606` и `:616`, `:607` и `:693`). `unwrap_or_default()` на `:693` корректен только благодаря тому, что предварительный проход уже выдал ошибку. После В2 тип tx и `ticket_id` разбираются один раз в `model`.
- **Р8. Тесты.**
  - `tests/l1_inflows.rs:196-198`: `hashes.dedup()` без сортировки удаляет только соседние повторы. Заменить на `HashSet` или `sort` + `dedup`.
  - Добавить негативные тесты «ошибка, а не паника»: `0x68` без `ticketId`, `0x69` без `depositValue`, битый hex в `logs[].data`, чек без `status` (после В6).
- **Р9. Pedantic, по делу:**
  - `opt_u256(&Option<String>)` → `Option<&str>` (`:161`);
  - `#[derive(Eq)]` у `PoolSwap` (`lib.rs:75`);
  - doc-комментарии у публичных элементов без них: `decode_swap`, `RawLog`-поля, `InflowKind::as_str`, `RpcTx`… (после В2 часть станет `pub(crate)`);
  - `path.ends_with(".zst")` без учёта регистра (`l1_inflows_scan.rs:72`).

  Остальное (разделители в литералах, `Self`, `#[must_use]`, `const fn`) — по желанию, механически.
- **Р10. Видимость.** `RpcBlock`/`RpcTx`/`RpcReceipt`/`RpcLog`, `parse_line` и `BlockLine` публичны, со строковыми полями. После В2 наружу отдавать только `model::{Block, Tx, Log, parse_block_line}`. Поля `Agg`/`Counters` можно оставить `pub`: ими пользуется отчёт example.

## Дубли

| что | где | расхождение | куда |
|---|---|---|---|
| разбор hex-quantity → u64 | `decoders/src/l1_inflows.rs:148` (`&str`, префикс не обязателен, `"0x"` = 0); `enricher/src/logs.rs:77` (`&Value`, префикс обязателен, `Option`); `enricher/tests/common/mod.rs:52` (`unwrap`) | **разошлись** по префиксу и пустому значению | `decoders::model` (enricher уже зависит от decoders). Низкий приоритет: enricher намеренно работает с сырым `Value`, но поведение для пустого и беспрефиксного ввода должно быть одним |
| проверка «блок ↔ чеки» | `enricher/src/blocks.rs:31-50` (`validate_block`: номер, длины, `blockHash`, хэши) и `decoders/src/l1_inflows.rs:592-615` (номер, длины, хэши без учёта регистра; `blockHash` не проверяется) | намеренная двойная проверка: декодер читает файлы с диска, возможно старых запусков | оставить. В `model::parse_block_line` (В2) написать, что источник истины — enricher, а декодер перепроверяет защитно. По желанию добавить проверку `blockHash` |
| тип лога | `lib.rs:66` `RawLog<'a>` и `l1_inflows.rs:563` `DecodedLog` | разные поля (`log_index` есть только во втором) | один `model::Log` (В2) |
| список колонок `funding_edges` | `sql/001` + `sql/002`, `FundingEdge`, заголовок и формат в example | пока совпадают, но тестом не связаны | `FundingEdge::COLUMNS` + `write_tsv` + тест (В3) |
| topic0 v3/v4 Swap | `decoders/src/lib.rs:48-49` и `analytics/hourly_metrics.py:29-30` | совпадают | допустимо как независимая проверка. Источник истины — таблица topic0 в `contracts.md`, Rust-тест `topics_are_canonical` с ней сверяется. В Python достаточно комментария со ссылкой на `contracts.md` |
| `unalias`/`alias`, `ALIAS_OFFSET` | только `l1_inflows.rs:53-72` | дублей нет | перенести в `arbitrum.rs` (см. раскладку). В hood-core не переносить: там нет `alloy-primitives`, и тянуть его туда ради одной функции не стоит, пока нет второго потребителя вне decoders |

## Структура и разбиение

Сейчас константы разбросаны так:
- topic0 — в корне `lib.rs`;
- типы tx Arbitrum, адреса `verified` и `ALIAS_OFFSET` — в `l1_inflows.rs`;
- модель RPC — тоже в `l1_inflows.rs`.

Для 1b добавятся адреса PoolManager, фабрик и launchpad'ов (после `verified`), события Pons и pools.trade. Если класть их рядом с «первым потребителем», через месяц нельзя будет одним `grep` проверить, что в коде только `verified`-адреса.

Предлагаемая раскладка. Разбиение по смыслу: каждый файл соответствует одному слою или одной таблице.

```
crates/decoders/src/
  lib.rs          crate docs, `pub mod`, short re-exports; no logic
  model.rs        Block / Tx / Log / TxCtx, parse_block_line (+ LogsLine for logs-*.jsonl.zst);
                  all hex/quantity parsing lives here and only here                         [B2, P5, P7]
  arbitrum.rs     TX_TYPE_DEPOSIT/RETRY/SUBMIT_RETRYABLE, ALIAS_OFFSET, alias/unalias (+ tests)
  addresses.rs    ONLY `verified` addresses from references/contracts.md, each with status/date/source
                  in a doc comment; a unit test that lists them (review point for contracts.md diffs)
  events.rs       sol! modules v3 / v4 / erc20 / token_bridge (+ pons, pools_trade later), TOPIC_*,
                  ALL_TOPIC0 with its comment, topics_are_canonical test
  rows.rs         ClickHouse row types and their TSV: FundingEdge (+EdgeKind), UnaccountedFlow,
                  later SwapRow, TokenRow; COLUMNS + write_tsv + golden/enum-vs-sql tests       [B1 Rust side, B3, B4]
  l1_inflows/
    mod.rs        decode_block (deposit / submit / retry, gateway_token_rows, matching_l2_transfer)  [P1, P2]
    registry.rs   RegistryStatus, GatewayEntry, GatewayRegistry, parse_tsv                     [B7]
    types.rs      InflowKind, L1Inflow, TokenInflow, UnaccountedKind, Counters, Agg             [P3, P4]
  swaps.rs        PoolSwap, decode_swap(ctx, log) -> SwapDecode; v3 + v4                        [B5]   (1b)
  transfers.rs    ERC-20 Transfer -> transfer/weth edges (feeds funding_edges kind weth)               (1b)
```

Примерная разбивка `l1_inflows.rs` по строкам:
- 78–163 → `model.rs`;
- 40–72 → `arbitrum.rs` и `addresses.rs`;
- 165–278 → `l1_inflows/registry.rs`;
- 280–549 → `l1_inflows/types.rs` и `rows.rs` (`FundingEdge`);
- 551–808 остаются в `l1_inflows/mod.rs`.

В итоге ни один файл не превысит ~300 строк.

Чего **не** делать сейчас:
- **Trait `Decoder`/`BlockDecoder` с `dyn`-реестром.** Входы у декодеров разные: L1 нужен весь блок с типами tx и статусами, свопам — лог с `TxCtx`, `tokens` — пары событий. Общий интерфейс свёлся бы к `fn(&Block) -> Vec<Box<dyn Any>>`. Реальное дублирование будет в драйвере: прочитать `jsonl.zst`, распаковать, разобрать строку, раздать декодерам, записать TSV. Его решает одна функция в будущем загрузчике, которая за один проход вызывает `l1_inflows::decode_block`, `swaps::decode_block` и остальные по очереди.
- **Диспетчер по topic0.** Если появится 3+ лог-декодера, хватит `match log.topics[0] { … }` в одной функции или `enum Event`, без регистрации в runtime.
- **Типизированные ошибки на весь крейт.** `anyhow` уместен для фатальных структурных ошибок строки: их всё равно оборачивают в `file:line`. Типизированный результат нужен только там, где ошибку надо посчитать, а не остановиться (В5, `SwapDecode::Malformed`).

Порядок рефакторинга по приоритету:
1. Б1: `rustfmt.toml` + `cargo fmt --all`, отдельный коммит (общий с частями 1, 2, 4).
2. В1: миграция `003` с новым ключом и обновление `data-model.md`. Сделать до задачи загрузчика.
3. В3 + В4: `rows.rs`, `EdgeKind`, `COLUMNS`/`write_tsv`, golden-тест, тест «Enum8 в SQL = enum в Rust»; example на них.
4. В2: `model.rs` (+ решение (а) или (б) по serde), перевод `l1_inflows` на `model::Log`, удаление `RawLog`/`DecodedLog`. Вместе с этим В6, Р5, Р6, Р7.
5. В7, Р3, Р4: мелкие правки инвариантов (реестр, `merge`, `checked_add`).
6. Р1, Р2: разбиение `retry` и тесты `matching_l2_transfer`, включая mint от 0x0.
7. В5: при старте задачи декодера свопов (1b), вместе с фикстурой реальных v3/v4 `Swap`.
8. Раскладка модулей (`arbitrum.rs`, `addresses.rs`, `events.rs`, `l1_inflows/`): заодно с пунктами 3–4, без изменения поведения.

Ни один пункт, кроме В1 (схема пустой таблицы), не меняет формат сырья, имена файлов данных, CLI-флаги example или содержимое TSV-вывода.

## Что хорошо (не сломать при правках)

- **Суммы типизированы.** wei и сырые суммы токенов — `U256`, суммы свопов — `I256`, `f64` встречается только в выводе example для чтения (`eth()`, с подписью «Display only», wei печатается рядом). Номера блоков, tx и логов — `u64`/`u32` с `try_from`, а не `as`.
- **Нет паник на внешних данных.** Индексы `topics[0..2]` защищены `len() == 3` (`:730`), `cols[0..2]` — `len() >= 3` (`:258`). Срез `[12..]` берётся от 32-байтного буфера. Все `unwrap` — только в тестах. Битый ввод даёт `Err` с текстом, а несовпадение чеков с tx проверяется (`:592-615`, тест `misaligned_receipts_are_an_error`).
- **Реестр шлюзов — вход декодера, а не хардкод.** В `builtin()` только `verified`; `todo`/`rejected` из TSV отклоняются; `extend` не перезаписывает записи. Строки от неизвестных шлюзов не теряются, а помечаются (`registry: None`). Это правильная граница между декодером и политикой.
- **«Не учтено» — отдельный выход.** `UnaccountedFlow` + `Counters` делают слепые зоны графа видимыми и проверяемыми. Каждый вид задокументирован с указанием «по коду / наблюдали».
- **Фикстура — неизменённые ответы RPC.** Источник, блоки, tx и дата указаны (`tests/l1_inflows.rs:1-17`). Синтетические мутации явно помечены `synthetic_`. topic0 закреплены и литералом, и `keccak256` сигнатуры со ссылкой на коммит исходника.

## Предполагается / не проверено

- **Проверено 2026-10-02:**
  - все команды линта и тестов из раздела «Линт» на HEAD `d241243`, offline;
  - прогон `rustfmt --check --config …` с разными `max_width` на файлах decoders;
  - семантика `+` у `U256` по исходнику ruint 1.20.1 из локального реестра cargo;
  - включённые feature `alloy-primitives` по `cargo tree -e features` (`serde` не включён);
  - отсутствие дублей `unalias`/`ALIAS_OFFSET` в `crates/` и `analytics/`, дубли hex-разбора и topic0 — по `grep`.
- **Предполагается, не проверено:**
  - что `U256` через serde в alloy 1.7 принимает RPC-quantity (`0x…` без ведущих нулей) — для варианта (б) в В2;
  - что `decode_raw_log` в alloy-sol-types 1.7 не проверяет число topics строго (В5);
  - что миграция `003` в предложенном виде применяется в ClickHouse: ClickHouse не запускал, сеть и docker не использовал;
  - что таблица `hood.funding_edges` на сервере пуста. Так написано в отчёте аудитора 017 («таблица пуста, загрузчика нет»), на сервер я не заходил. Перед `DROP` в `003` это нужно проверить (`SELECT count() FROM hood.funding_edges`).
- **Вне объёма** (семантика, за data-auditor): З2 аудитора — «шлюз» вне реестра забирает ETH-строку. Архитектурно правка ложится в `gateway_token_rows` из Р1 (ветка `registry == None`), отдельного места не требует.
- `cargo clean` не выполнял по указанию координатора: target общий с параллельными ревьюерами.
