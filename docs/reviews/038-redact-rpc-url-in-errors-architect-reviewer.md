# 038 — архитектурное ревью (architect-reviewer)

Дата: 2026-10-05. Объём: незакоммиченные изменения рабочего дерева по задаче 038:
`crates/hood-core/src/{redact.rs (новый), lib.rs}`, `crates/enricher/src/{rpc.rs, lib.rs}`,
`crates/enricher/tests/exit_codes.rs`, `crates/loader/src/{ch.rs, config.rs, lib.rs}`,
`crates/recorder/src/{session.rs, connlog.rs}`,
`.claude/skills/feed-audit/scripts/{feed_audit.py, test_feed_audit.py}`.
Изменения задач 036/037 (`crates/decoders`, `sql/`, `contracts.md`, `abi/`) не рассматривались.

**Вердикт: PASS с замечаниями.** Блокирующих замечаний нет. Путей, по которым ключ из пути `/v2/<key>`
попадает в логи, тексты ошибок, stats JSON или файлы, я не нашёл. Есть важные замечания: два новых нарушения ruff
в тестовом файле, дыра в заявленном контракте `redact_url` на некорректных значениях и пустой текст
транспортной ошибки (причина сбоя не видна). Их стоит исправить до коммита или отдельной мелкой задачей.

## Линт — вывод команд (проверено 2026-10-05)

Rust я собирал в отдельном `CARGO_TARGET_DIR` в scratchpad, чтобы не мешать параллельной сборке decoders. После
прогона этот каталог удалён.

- `cargo fmt --all -- --check`: код 0, чисто.
- `cargo clippy -p hood-core -p enricher -p loader -p recorder --all-targets -- -D warnings`: `Finished`, без предупреждений.
- `cargo test -p hood-core -p enricher -p loader -p recorder` (переменные `RPC_URL`/`FEED_URL`/`RECORDER_OUT_DIR`/`PROVIDER_RPC_URL`
  сняты, `ENRICHER_*` в окружении нет, `TMPDIR` указывает в scratchpad): 0 failed. hood-core 32, enricher lib 21,
  exit_codes 7, loader 18 + 3, recorder 71 + mock_feed 16, остальные наборы тоже ok. Сборка enricher тянет decoders,
  то есть в прогон попало текущее состояние 036.
- clippy pedantic+nursery (рекомендательно): по новому коду только `doc_markdown`/`doc_link_with_quotes`
  (redact.rs:32, :52) и `must_use_candidate` (redact.rs:39, :56; connlog.rs:144).
- `uvx ruff check .claude/skills/feed-audit/scripts/` (конфиг проекта из `pyproject.toml`): **2 ошибки, обе новые.**
  `UP031` в test_feed_audit.py:197 и :207. В HEAD тестовый файл был чистым: исключение UP031 в per-file-ignores есть
  только для `feed_audit.py`.
- `uvx ruff format --check`: 2 files already formatted.
- `python3 -m py_compile`: ок. `python3 -m unittest test_feed_audit`: 29 tests OK.
- jscpd не запускал: дубль один и намеренный, разобран вручную ниже.

## Блокирующее

Нет.

## Важное

**В1. [test_feed_audit.py:197, :207] Новые ошибки ruff `UP031`.** По правилам скилла ошибка линтера в изменённом
файле — минимум «важное». До правки этот файл проходил ruff чисто. Исправление:
```python
s = fa.scrub_url(f"<urlopen error for {url}> /v2/SECRETKEY u:PW", url)
...
url = f"http://127.0.0.1:{port}/v2/SECRETKEY"
```

**В2. [crates/hood-core/src/redact.rs:22-23 и копия feed_audit.py:326-334] Контракт «не URL маскируется целиком»
нарушается на двух видах некорректного значения, и тогда ключ печатается полностью.**
- `split_once("://")` берёт первое вхождение `://` в любом месте строки. Значение без схемы, у которого в query
  есть URL, например `h.io/v2/KEY?r=https://x`, превращается в scheme=`h.io/v2/KEY?r=https`, host=`x`. Строка
  выходит без изменений, ключ в стартовом логе `rpc=`.
- В качестве разделителя пути не учитывается `\`. При этом крейт `url`, а значит и reqwest, для http/https/ws/wss
  считает `\` разделителем пути. Значит, `https://h.io\v2\KEY` у reqwest работает, а `redact_url` считает
  `h.io\v2\KEY` хостом и печатает его.

Оба случая — опечатки в конфиге, на практике маловероятны. Но функция существует ровно для того, чтобы не
полагаться на корректность значения, а тест `not_a_url_is_masked_whole` обещает обратное. Исправление на 3 строки
(то же в Python):
```rust
fn split(url: &str) -> Option<Parts<'_>> {
    let (scheme, after) = url.split_once("://")?;
    let scheme_ok = scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if !scheme_ok { return None; }
    let end = after.find(['/', '\\', '?', '#']).unwrap_or(after.len());
    ...
}
```
И добавить оба значения в `not_a_url_is_masked_whole` и в `assertEqual(..., "***")` Python-теста. Для
`https://h.io\v2\KEY` ожидается `https://h.io/***`.

**В3. [crates/enricher/src/rpc.rs:505] Текст транспортной ошибки всегда `transport: error sending request`:
причины нет, а `scrub_url` на этом пути фактически ничего не делает.** `reqwest::Error` в `Display` (reqwest 0.12.28,
`src/error.rs:227`) печатает только вид ошибки и URL, а цепочку `source()` не печатает никогда; `{:#}` он
игнорирует. Проверено 2026-10-05 запуском бинарника: и на закрытом порту, и на несуществующем хосте WARN и
`Error:` одинаковые, `last error: transport: error sending request`. Отказ соединения, DNS и TLS неразличимы, а
именно это нужно будет смотреть в journald на сервере. Так было и до задачи, но теперь причину можно добавить
безопасно, потому что маскировка уже стоит. Отсюда второй пункт: тест `connection_error_does_not_leak_the_key`
проверяет `e.chain()` у `anyhow`, собранного из строки, то есть цепочку, которой нет. Он даёт ложное ощущение, что
защищена и цепочка источников. Исправление:
```rust
let e = e.without_url();
let mut text = e.to_string();
let mut src = std::error::Error::source(&e);
while let Some(s) = src {
    text.push_str(": ");
    text.push_str(&s.to_string());
    src = s.source();
}
Failure::retry(FailKind::Transient, format!("transport: {}", scrub_url(&text, &self.url)), None)
```
После этого существующие тесты (unit и `exit_codes::connection_error_does_not_print_the_key`) действительно
проверяют цепочку hyper-util/io на отсутствие ключа. В `exit_codes` стоит добавить проверку, что причина видна,
например `out.contains("onnection refused")` (на macOS и Linux текст io-ошибки отличается регистром первой буквы).

## Рекомендации

**Р1. [feed_audit.py:65] `import urllib.parse` не используется.** Ruff этого не ловит, потому что имя `urllib`
занято `urllib.request`. Удалить.

**Р2. [redact.rs:1-7] Не указано, что у функции есть копия в Python.** Docstring `feed_audit.py:337-341` ссылается
на Rust, а обратной ссылки нет. Добавить в doc модуля строку: «Mirrored in `.claude/skills/feed-audit/scripts/feed_audit.py`
(`redact_url`/`scrub_url`, stdlib-only); change both and their tests together».

**Р3. [test_feed_audit.py:188-199 и redact.rs:73-104] Наборы тестовых значений разошлись.** В Python нет случаев
`#fragment`, `[::1]:port`, значения с пробелами по краям и `ws://u:pw@…/`. Повторить в Python тот же список
значений, что и в `keeps_scheme_and_host_only`/`not_a_url_is_masked_whole`. Один список в обоих файлах — самый
дешёвый способ поймать расхождение копий.

**Р4. [redact.rs:63-66] `scrub_url` с короткой userinfo портит текст ошибки.** При userinfo `u` каждая буква `u` в
тексте становится `***`. Это безопасно (лишнее маскирование), но текст становится нечитаемым. Можно пропускать
части короче 4 символов, если они уже покрыты заменой полного URL, или просто описать это в doc. Не обязательно.

**Р5. [connlog.rs:146-147] Нет теста на то, что URL с путём в `connections.tsv` маскируется.** Сейчас тест
проверяет только неизменность для URL без пути (`mock_feed`). Нужен unit-тест на
`ConnEvent::connected("wss://h/p?k=S", …)`: detail начинается с `wss://h/***`. Заодно это зафиксирует решение по
вопросу исполнителя о формате сырья. Пометка: для URL фида с путём или query меняется поле `detail` файла
`connections.tsv`. Для текущих URL байты те же. Это допустимо, но решение Михаила по вопросу из отчёта нужно
записать.

**Р6. [loader/src/lib.rs:166] Полный путь `hood_core::redact::redact_url` при том, что в соседних файлах
(`ch.rs`, `config.rs`) функция импортирована.** Для единообразия добавить `use`.

**Р7. Pedantic.** `#[must_use]` на `redact_url`/`scrub_url`. В doc `[`redact_url`]`(url)` заменить на
`` `redact_url(url)` ``.

## Дубли

- `hood_core::redact` и `feed_audit.py::{_url_parts, redact_url, scrub_url}`. Дубль намеренный и допустимый:
  скрипт работает на чистом stdlib на сервере и на Mac, тянуть туда Rust нельзя. Источник истины назван в Python
  docstring, сейчас поведение совпадает: я сверил ветки `split`/`_url_parts`, `rsplit_once`/`rpartition`, условие
  `rest in ("", "/")` и пустой хост. Не хватает обратной ссылки (Р2) и общего набора тестовых значений (Р3). После
  В2 править обе копии.
- Старая `enricher::rpc::redact_url` удалена, других реализаций маскировки в `crates/` нет (grep 2026-10-05).
  `loader::config::check_loopback` по-прежнему разбирает authority сам. Это проверка, а не маскировка, выносить
  не нужно.

## Структура и разбиение

Перестройка не нужна. Небольшой модуль в hood-core с двумя чистыми функциями, без зависимостей, лучше крейта
`url`: не надо добавлять зависимость в hood-core, и функция работает на невалидных значениях, где `Url::parse`
бы упал. Публичный API минимален (`MASK`, `redact_url`, `scrub_url`), `Parts`/`split` приватные.

## Что хорошо

- Двойная защита: `without_url()` убирает URL из ошибки reqwest, `scrub_url` чистит то, что останется в чужих
  текстах. Исправление в одном месте (`Rpc::transport`) закрывает и WARN `retrying`, и итоговое `Error:`.
- Интеграционный тест на настоящем бинарнике проверяет весь вывод (стартовый лог, WARN, `Error:`) на отсутствие и
  `SECRETKEY`, и `/v2/`, и на наличие хоста. Это правильная проверка «снаружи».
- `redact_url` не меняет URL без секретов (публичный фид, `ws://127.0.0.1:<port>/`), поэтому сырьё
  `connections.tsv` и тест `mock_feed` для текущих конфигураций остались прежними.
- В loader заодно убран SQL из текста сетевой ошибки, контекст `query: <first line>` при этом сохранён.
- Исполнитель отдельно показал мутационной проверкой, что тест падает со старым `format!("{e:#}")`.

## Проверено (2026-10-05, как)

- Все места печати URL и `RPC_URL`/`FEED_URL`/`CLICKHOUSE_URL` проверены grep по `crates/`, `analytics/`, `deploy/`,
  `.claude/skills/`: `info!/warn!/format!/bail!/context` с url, `Debug` у `Rpc` (не реализован), у `Args` (не
  печатается, `hide_env_values`). `stats.rs` и `--stats-json` эндпоинта не содержат. `recorder/src/transport.rs`
  печатает только `ParseError`, ошибки tcp/tls/upgrade, но не URL. `hourly_sample.py` печатает только хост, и
  только публичный. Ключ провайдера ни в один файл или лог не попадает.
- Display и Debug у `reqwest::Error`: прочитан исходник reqwest 0.12.28 (`src/error.rs:211-260`). Debug включает
  url, но после `without_url` его нет. В loader `anyhow` хранит уже очищенную ошибку.
- `urllib` (feed_audit): `URLError`/`HTTPError` в `str()` URL не содержат, `scrub_url` там — запасной рубеж.
  `--rpc-url` в отчёт и в `--help` не выводится.
- Поведение бинарника: `enricher --rpc-url http://127.0.0.1:<закрытый порт>/v2/SECRETKEY`. Стартовый лог
  `rpc=http://127.0.0.1:<port>/***`, WARN и `Error:` без ключа. Второй прогон с
  `https://u:PW@nonexistent.invalid/v2/SECRETKEY?x=Q` дал `rpc=https://nonexistent.invalid/***`, ни `PW`, ни ключа,
  ни `x=Q` в выводе. Оговорка о сети: этот прогон сделал один DNS-запрос имени в зарезервированной зоне `.invalid`
  через системный резолвер. Других сетевых обращений не было.
- Значение `PROVIDER_RPC_URL` из `.env` не читалось и не печаталось.

## Предполагается / не проверено

- Что цепочка `source()` у hyper-util/rustls (после исправления В3) не содержит пути URL. По устройству этих
  библиотек там только вид ошибки, io-ошибка и имя хоста для TLS. После В3 это проверят существующие тесты для
  случая отказа соединения. TLS и редиректы не проверены (у `Client` политика редиректов по умолчанию; при
  редиректе URL цели мог бы попасть в источник, `scrub_url` его не знает. Для JSON-RPC POST редиректы на практике
  не встречаются).
- Утечки в уже записанных файлах (`docs/`, scratchpad 035, git-история) я не перепроверял, взял результат из отчёта
  исполнителя (0 вхождений).
- `journald` на сервере не проверялся: enricher с провайдерским URL там не запускался.
