# 038 — Не допускать ключа провайдера в журналы enricher: отчёт

Исполнитель: indexer-engineer. Дата: 2026-10-05. Статус задачи: `in-progress`. `done` ставится после PASS от architect-reviewer, коммит отдельный, его делает основная сессия.

Сеть: 0 вызовов. Сервер не трогал. Ключ не печатался: в тестах фигурирует только строка-заглушка `SECRETKEY`, при поиске утечек ключ читался из `.env` внутри конвейера оболочки, наружу выводились только счётчики. `crates/decoders`, `sql/`, `contracts.md` не трогал (там параллельно идёт задача 036).

## Сделано

1. **Одна функция маскировки в hood-core**: новый модуль `crates/hood-core/src/redact.rs`.
   - `redact_url(url)` оставляет только схему и хост с портом. `user:pw@` выбрасывается. Непустой путь, query или fragment заменяются на `/***`. Пустой путь и `/` остаются как есть, поэтому URL без секретов печатается без изменений. Строка не вида `scheme://host` целиком заменяется на `***`. Пример: `https://robinhood-mainnet.g.alchemy.com/v2/<key>` превращается в `https://robinhood-mainnet.g.alchemy.com/***`.
   - `scrub_url(text, url)` чистит чужой текст ошибки: сам URL заменяется на `redact_url(url)`, а userinfo и путь+query по отдельности на `***`. Это второй рубеж на случай, если URL попадёт в цепочку `source()` чужой библиотеки.
   - Старая локальная `enricher::rpc::redact_url` удалена (там вместо пути печаталось `/…`). Все вызовы переведены на hood-core.
2. **enricher** (`crates/enricher/src/rpc.rs`, `Rpc::transport`): вместо `format!("transport: {e:#}")` теперь `e.without_url()`, к результату применяется `scrub_url(.., &self.url)`. Через этот текст шли и WARN `retrying reason=…`, и итоговое `Error: …` (через `CallError::Failed`). Стартовый лог `rpc=` и сообщение `wrong network: the RPC endpoint …` (`lib.rs`) переведены на `hood_core::redact::redact_url`.
3. **loader** (`crates/loader/src/ch.rs`, `config.rs`, `lib.rs`): у ошибок `reqwest` при `send()` и `text()` убран URL через `without_url`. Раньше в текст попадал полный URL вместе с query, то есть со всем SQL. Контекст `POST …`, стартовый лог `ClickHouse … version …` и ошибка «только http:// на loopback» печатают `redact_url`. Ключа провайдера здесь нет (`CLICKHOUSE_URL` проверяется на loopback и отсутствие учётных данных), правка сделана ради единого правила.
4. **recorder** (`session.rs`, `connlog.rs`): лог `connected url=…` и поле `detail` строки `connected` в `connections.tsv` идут через `redact_url`. Для текущего публичного `wss://feed.mainnet.chain.robinhood.com` и тестового `ws://127.0.0.1:<port>/` вывод побайтно прежний: формат сырого файла не меняется, тест `mock_feed` с `ends_with("{url} requested=105 mode=header")` проходит. Разница появится только у URL фида с путём или query, то есть с возможным ключом.
5. **feed-audit** (`.claude/skills/feed-audit/scripts/feed_audit.py`): скрипт читает `RPC_URL`, и при сбое RPC текст исключения шёл в `fails` и в отчёт `deploy/feed-audit-daily.sh`. Добавлены `redact_url` и `scrub_url` по тому же правилу, что в hood-core (своя копия, потому что скрипт на чистом stdlib), и `check_rpc` теперь чистит текст исключения.

## Найденные места (все, где URL или текст `reqwest::Error` попадает в лог или ошибку)

| Место | Было | Стало |
|---|---|---|
| `enricher/src/rpc.rs` `Rpc::transport` | `transport: {e:#}`: URL с ключом в WARN `retrying` и в `Error:` | `without_url` + `scrub_url` |
| `enricher/src/lib.rs` стартовый `info!(rpc=…)`, `wrong network …` | локальная `redact_url` (уже маскировала) | `hood_core::redact::redact_url` |
| `loader/src/ch.rs` `post()`: `send()`, `text()`, контекст `POST {url}` | полный URL ClickHouse с SQL в query | `without_url`, `redact_url` |
| `loader/src/lib.rs` `ClickHouse {url} version` | URL как есть | `redact_url` |
| `loader/src/config.rs` `check_loopback` | `CLICKHOUSE_URL {url:?}` | `redact_url` |
| `recorder/src/session.rs` `info!(url=…)` | URL фида как есть | `redact_url` |
| `recorder/src/connlog.rs` `ConnEvent::connected` → `connections.tsv` | URL фида как есть | `redact_url` (для текущих URL без изменений) |
| `feed_audit.py` `check_rpc` | `rpc check failed: %s` из `str(e)` | `scrub_url(str(e), url)` |

Проверены, правка не нужна:
- `enricher` `Rpc::new`: ошибка `Client::builder().build()` URL не содержит. `struct Rpc` не реализует `Debug`. `Args` реализует `Debug`, но нигде не печатается; у `--rpc-url` стоит `hide_env_values`.
- `enricher/src/stats.rs` и `--stats-json`: эндпоинта нет.
- `recorder/src/transport.rs`: `bad url: {e}` печатает только `url::ParseError`, без самого URL.
- `hood-core/src/http.rs`: там только разбор `Retry-After`, URL не печатается.
- `analytics/hourly_sample.py`: отказывается работать с любым хостом, кроме публичного RPC. `analytics/hoodlib.py`: RPC не вызывает. `PROVIDER_RPC_URL` в репозитории не читает ни один скрипт.
- `deploy/*.sh`: URL не печатают. `enricher-gaps.service` в `ExecStartPre` проверяет `$RPC_URL`, но не выводит его.

## Проверено (2026-10-05, как)

- **Тест п. 3, unit** `enricher::rpc::tests::connection_error_does_not_leak_the_key`: `Rpc::chain_id()` против `http://127.0.0.1:<только что освобождённый порт>/v2/SECRETKEY`, 1 попытка. В `{err}`, `{err:#}`, `{err:?}` и во всей цепочке `source()` нет `SECRETKEY`, есть `transport`. Мутационная проверка: если вернуть старый `format!("{e:#}")`, тест падает. Затем код восстановлен.
- **Тест п. 3, бинарник** `enricher/tests/exit_codes.rs::connection_error_does_not_print_the_key`: настоящий `enricher` с тем же URL, `--max-attempts 2`. Код выхода 1. В stdout+stderr (стартовый лог, WARN `retrying … transport`, `Error:`) нет ни `SECRETKEY`, ни `/v2/`. Хост `127.0.0.1:<port>` виден.
- **Тесты маскировки**: `hood-core::redact::tests` (3 теста: схема+хост, маскировка не-URL целиком, `scrub_url`). `feed_audit` получил `test_redact_and_scrub_url` и `test_rpc_failure_does_not_print_the_key` (закрытый порт на loopback, в `fails` нет ключа).
- `cargo fmt --all -- --check`: чисто.
- `cargo clippy --workspace --all-targets -- -D warnings` и `cargo clippy --workspace`: без предупреждений.
- `cargo build --workspace`: ок.
- `cargo test --workspace` (`TMPDIR` = scratchpad/038/tmp, `RPC_URL`/`FEED_URL`/`RECORDER_OUT_DIR`/`ENRICHER_*`/`PROVIDER_RPC_URL` сняты): 0 failed. hood-core 32, enricher lib 21 (старый `url_redaction` заменён новым тестом), exit_codes 7 (+1), recorder lib 71 + mock_feed 16, loader 18 + mock_rollback 3, decoders без изменений с моей стороны. В прогон попало и текущее состояние параллельной задачи 036 в `crates/decoders`.
- `python3 -m unittest test_feed_audit`: 29 OK (+2).
- **Поиск утечки ключа.** Ключ (26 символов) брался из `PROVIDER_RPC_URL` в `.env` внутри конвейера, печатались только счётчики:
  - файлы репозитория, отслеживаемые и неотслеживаемые, кроме `.env`: 0 файлов;
  - `git log --all -p`: 0 вхождений;
  - `docs/` вместе с игнорируемыми файлами: 0;
  - `deploy/`, `analytics/`, `.claude/`: 0;
  - `data/` без `*.zst`: 0;
  - scratchpad 035 (67 файлов, текст + распакованные `*.zst`): 0. В 8 файлах scratchpad 035 встречается `/v2/`. Это сохранённые страницы документации Alchemy (`docs/reference_*`, `docs/chains_*`) с их демо-ключами. Префикс нашего ключа с ними не совпадает (проверено через `case` без вывода).
  - Ключ есть только в `.env` (1 строка).

## Предполагается

- Цепочка `source()` у `reqwest::Error` (hyper-util connect error → io error) сама URL не содержит. Для отказа соединения это проверено тестом на настоящей ошибке reqwest, в текст ошибки цепочка теперь входит (В3). Для других видов ошибок (TLS, редиректы, обрыв тела) не проверено. На этот случай стоит `scrub_url`, он вырезает URL и путь с ключом из любого текста.
- В `data/**/*.zst` (сырьё фида и блоков) поиск не делался: это ответы RPC и фид, URL эндпоинта туда не пишется по построению. `journald` на сервере не проверялся: сервер в задаче не трогать, и enricher с `PROVIDER_RPC_URL` там ещё не запускался.

## Доработка по ревью architect-reviewer (2026-10-05, PASS с замечаниями)

Ревью: `docs/reviews/038-redact-rpc-url-in-errors-architect-reviewer.md`. Исправлены все важные замечания (В1–В3) и рекомендации Р1–Р3, Р5–Р7. Р4 (слишком короткая userinfo маскирует лишнее) описана в doc `scrub_url`, код не менял.

- **В1.** `test_feed_audit.py`: `%`-форматирование заменено на f-строки. `ruff check` с конфигом проекта проходит чисто.
- **В2.** Схема теперь проверяется по RFC 3986 (`ALPHA *( ALPHA / DIGIT / + / - / . )`), а `\` считается разделителем пути, как в крейте `url`/reqwest. Исправлено в `hood_core::redact` и в копии в `feed_audit.py`. Результат: `h.io/v2/SECRETKEY?r=https://x` маскируется целиком в `***`, `https://h.io\v2\SECRETKEY` превращается в `https://h.io/***`.
- **Р3.** Общий набор тестовых значений вынесен в `crates/hood-core/src/redact_vectors.tsv` (17 строк). Rust читает его через `include_str!`, Python-тест по пути от корня репозитория (тест пропускается, если рядом нет `crates/`, например в копии на сервере). Сюда вошли и оба новых случая из В2, и все прежние значения из Rust (fragment, `[::1]:port`, пробелы по краям, `ws://u:pw@…/`).
- **В3.** Новая функция `transport_text` в `enricher/src/rpc.rs`. Она берёт ошибку после `without_url()`, вручную склеивает цепочку `source()` через `: ` и пропускает результат через `scrub_url`. Раньше `Display` у reqwest давал только `error sending request`. Теперь причина видна: `transport: error sending request: client error (Connect): tcp connect error: Connection refused (os error 61)` (текст источника зависит от ОС), и в journald отказ, DNS и TLS будут различимы. Тесты переписаны:
  - unit `transport_text_keeps_the_cause_and_drops_the_key` работает на настоящей ошибке reqwest (закрытый порт на loopback). Он проверяет, что цепочка `source()` есть и что `Debug` самого reqwest содержит ключ (это предусловие, иначе тест ничего не доказывает). В тексте `transport_text` нет ни `SECRETKEY`, ни `/v2/`, а `onnection refused` есть;
  - unit `connection_error_does_not_leak_the_key` проверяет то же через `Rpc`/`CallError` в Display, `{:#}` и Debug;
  - в `exit_codes::connection_error_does_not_print_the_key` добавлена проверка, что в выводе бинарника есть `onnection refused`.
- **Р1.** Удалён неиспользуемый `import urllib.parse` в `feed_audit.py`.
- **Р2.** В doc модуля `redact.rs` добавлена ссылка на Python-копию и на общий файл тестовых значений.
- **Р5.** Добавлен тест `recorder::connlog::tests::connected_detail_masks_url_path_and_query`: `wss://u:PW@feed.example.com/v2/SECRETKEY?k=Q` пишется в `detail` как `wss://feed.example.com/*** requested=7 mode=header`, а `ws://127.0.0.1:9/` сохраняется побайтно. Golden-тест строк `connections.tsv` не менялся и проходит.
- **Р6.** В `loader/src/lib.rs` полный путь заменён на `use hood_core::redact::redact_url`.
- **Р7.** На `redact_url` и `scrub_url` стоит `#[must_use]`, ссылка в doc поправлена.
- Мои файлы после `cargo fmt --all` от исполнителя 036 проверены: `cargo fmt --all -- --check` чист, содержимое моих правок на месте.

Проверено после доработки (2026-10-05):
- `cargo fmt --all -- --check`: чисто.
- `cargo clippy --workspace --all-targets -- -D warnings` и `cargo clippy --workspace`: без предупреждений.
- `cargo build --workspace`: ок.
- `cargo test --workspace` (окружение то же): 0 failed. hood-core 31 (три теста маскировки сведены в общий по файлу значений + `scrub`), enricher lib 22 (+1), exit_codes 7, recorder 72 (+1) + mock_feed 16, loader 18 + 3. decoders, включая текущий `pons_v2_hook` из 036, зелёные.
- `uvx --offline ruff check .claude/skills/feed-audit/scripts/`: All checks passed. `ruff format --check`: 2 files already formatted. `python3 -m unittest test_feed_audit`: 30 OK. `test_redact_shared_vectors` действительно выполнился, а не пропущен.
- Повторный поиск ключа (тем же способом, только счётчики): файлы репозитория, кроме `.env`, — 0, `docs/` — 0.
- Сеть: 0 вызовов. Тесты используют только закрытый порт на 127.0.0.1, ruff запускался через `uvx --offline`.

## Вопросы

- Изменение `connections.tsv` (п. 4) байт-в-байт не затрагивает текущие URL. Если по правилу «формат сырых файлов только с согласования» Михаил хочет оставить там URL как есть, правку легко откатить: одна строка в `connlog.rs`.
- После PASS от architect-reviewer: поставить `status: done` и сделать коммит. data-auditor не требуется: декодеры и форма загружаемых данных не менялись.
