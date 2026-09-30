# 005 — Тест: досылает ли публичный фид пропущенное по Arbitrum-Requested-Sequence-Number. Отчёт
status: выполнено; ждёт data-auditor
date: 2026-09-30
executor: indexer-engineer
reviewers: data-auditor (не запускался, его запускает основная сессия)

Статус в задаче не менял: по указанию основной сессии он остаётся `in-progress`. Коммитов нет. `crates/recorder/src` не трогал.

## Итог

**Фид заголовок поддерживает, но только внутри своего бэклога. Бэклог — примерно последние 600–660 блоков (~60–65 с).**

- Если запросить N внутри бэклога, поток начинается ровно с N.
- Если N старше бэклога, сервер молча отдаёт весь бэклог с его начала: без ошибки, без закрытия и без HTTP-ошибки.
- Дыры длиннее ~1 минуты этим способом не закрыть, для них остаётся RPC. Короткие переподключения (секунды — десятки секунд) закрываются без RPC.

| Глубина | Запрошен N | tip (RPC) | Первый seq | Первый seq − N | Блоков бэклога (seq ≤ tip) | blockHash = RPC |
|---|---|---|---|---|---|---|
| 100 | 76661959 | 76662059 | **76661959** | 0 | 101 | 10/10 |
| 1 000 | 76663310 | 76664310 | 76663714 | +404 (tip−596) | 597 | 10/10 |
| 10 000 | 76656489 | 76666489 | 76665828 | +9339 (tip−661) | 662 | 10/10 |

- Соединений к фиду: **3**, по одному на глубину. Все с HTTP 101, 403 и 429 не было.
- Вызовов RPC: **33**, все HTTP 200, 429 не было. Это 3 × `eth_blockNumber` и 30 × `eth_getBlockByNumber(false)`.

## Сделано

1. Разобрал по исходникам Nitro, какие заголовки шлёт клиент и что делает сервер (раздел «Код Nitro»).
2. Написал зонд `crates/recorder/examples/feed_probe.rs`.
   - Код соединения скопирован минимально из `src/net.rs`: TCP/TLS с ALPN http/1.1, `HeadTap` для заголовков ответа, yawc с permessage-deflate. `src` не менял: recorder — бинарный крейт, его модули нельзя импортировать.
   - Заголовки ставятся через `yawc::WebSocket::handshake_with_request`.
   - Защиты срабатывают до TCP-connect. Зонд откажется подключаться, если в его журнале уже есть 403 или 429, если было 3 попытки или больше, если прошлая попытка была меньше 180 с назад, или если в `connections.tsv` recorder'а есть соединение моложе 3600 с. Попытка пишется в журнал до подключения.
   - Все три отказа проверены офлайн на поддельных журналах.
   - Каждый кадр пишется сырым. Сессия кончается на tip+30 блоков (при этом не раньше 5 с) или через 60 с. Закрытие: Close 1000, ожидание Close сервера до 2 с, затем `ws.close()`.
3. Три соединения с паузами ~3.5 мин. Перед первым проверил журналы: единственный `connections.tsv` с подключениями — `data/feed-test-002/connections.tsv`, последнее `connected` там в 13:57:32Z, `shutdown` в 14:03:00Z. Незакрытой паузы нет, до первого соединения (16:33:51Z) прошло 2.5 ч.
4. Для каждой глубины сверил `blockHash` 10 блоков бэклога с `eth_getBlockByNumber`. Блоки взяты равномерно от первого полученного до tip включительно.
5. Добавил факт в `.claude/skills/hoodchain-mev/references/chain-facts.md`, раздел «Фид секвенсора», после пункта задачи 006. Чужие строки не трогал.

`cargo build --workspace`, `cargo test --workspace` и `cargo clippy --workspace --all-targets -- -D warnings` проходят, `cargo fmt -p recorder --check` чистый.

## Ответ по глубинам — проверено (2026-09-30, соединения зонда с Mac, сверка через публичный RPC)

**100 блоков** (соединение 16:33:52–16:33:57Z):
- первый seq 76661959 — ровно запрошенный;
- первый кадр пришёл через 379 мс после 101;
- 101 блок до tip пришёл примерно за 0.5 с, дальше живой поток ~110 мс/блок;
- всего 167 блоков подряд, без дыр и дублей, по 1 сообщению в конверте;
- `blockHash` 10/10.

**1 000 блоков** (16:37:36–16:37:41Z):
- первый seq 76663714, то есть N+404 или tip−596. Более старые блоки не пришли;
- 597 блоков бэклога примерно за 0.87 с, затем живой поток;
- всего 665 блоков, без дыр и дублей;
- `blockHash` 10/10.

**10 000 блоков** (16:41:16–16:41:22Z):
- первый seq 76665828, то есть N+9339 или tip−661;
- всего 729 блоков, 662 из них бэклог, без дыр и дублей;
- `blockHash` 10/10.

Общее по трём соединениям:
- В ответе 101 те же заголовки, что и без нашего заголовка: `x-backend-service: arb-relay`, permessage-deflate с `no_context_takeover`, Cloudflare. **Заголовков `Arbitrum-Feed-Server-Version` и `Arbitrum-Chain-Id` нет**, хотя stock-Nitro их шлёт.
- Бэклог приходит **по одному сообщению в конверте**. Stock-Nitro шлёт сегмент, до 240 сообщений, одним конвертом. Recorder'у менять разбор не нужно.
- На наш Close 1000 сервер ответил Close 1000 с эхом причины (`probe done`) через ~150 мс. За это время пришли 1–2 конверта, они есть в сырье как `recorderFrame:text`. Задача 008 проверяла ответ на Close только на моке, на настоящем сервере это подтверждено здесь.
- Ping от сервера: 3 штуки за ~5 с, как в 002 (раз в ~2 с).
- `confirmedSequenceNumberMessage` за 5-секундные сессии не пришли.

Для сравнения, сессии без заголовка из 001 и 002 (сырьё `data/feed/…-11`, `data/feed-test-002/…-13`). Поток начинался у tip: первая пачка 4–6 блоков за <10 мс, `header.timestamp` отставал от приёма на 0.6–1.0 с. Stock-Nitro при отсутствии заголовка (N=0) отдал бы весь бэклог, а этот сервер начинает у tip.

## Код Nitro

Тег `v3.11.4`, коммит `7d5ac271b400f710f6267ad759c6afc3e12d7059`, последний релиз на 2026-09-30. Файлы скачаны через raw.githubusercontent.com. Context7 в этой сессии недоступен, GitHub MCP лежит.

**Клиент** — `broadcastclient/broadcastclient.go`:
- [L231-L234](https://github.com/OffchainLabs/nitro/blob/v3.11.4/broadcastclient/broadcastclient.go#L231-L234): в запросе ровно два своих заголовка, `Arbitrum-Feed-Client-Version: 2` и `Arbitrum-Requested-Sequence-Number: <nextSeqNum>` в десятичном виде. Chain id клиент **не шлёт**.
- [L175](https://github.com/OffchainLabs/nitro/blob/v3.11.4/broadcastclient/broadcastclient.go#L175): начальный `nextSeqNum = currentMessageCount`, то есть следующее сообщение, которого у ноды ещё нет.
- [L478](https://github.com/OffchainLabs/nitro/blob/v3.11.4/broadcastclient/broadcastclient.go#L478): после каждого принятого сообщения `nextSeqNum = SequenceNumber + 1`.
- [L194](https://github.com/OffchainLabs/nitro/blob/v3.11.4/broadcastclient/broadcastclient.go#L194), [L530](https://github.com/OffchainLabs/nitro/blob/v3.11.4/broadcastclient/broadcastclient.go#L530): при переподключении клиент просит продолжение с последнего полученного.
- [L250-L286](https://github.com/OffchainLabs/nitro/blob/v3.11.4/broadcastclient/broadcastclient.go#L250-L286): из ответа клиент читает `Arbitrum-Feed-Server-Version` и `Arbitrum-Chain-Id` и сверяет их. [L317-L330](https://github.com/OffchainLabs/nitro/blob/v3.11.4/broadcastclient/broadcastclient.go#L317-L330): отсутствие этих заголовков — ошибка только при `require-chain-id` / `require-feed-version`, а они по умолчанию `false` ([L100-L101](https://github.com/OffchainLabs/nitro/blob/v3.11.4/broadcastclient/broadcastclient.go#L100-L101)).

**Сервер** — `wsbroadcastserver/wsbroadcastserver.go`:
- [L34-L38](https://github.com/OffchainLabs/nitro/blob/v3.11.4/wsbroadcastserver/wsbroadcastserver.go#L34-L38): имена заголовков, включая `CF-Connecting-IP`. [L44-L45](https://github.com/OffchainLabs/nitro/blob/v3.11.4/wsbroadcastserver/wsbroadcastserver.go#L44-L45): обе версии = 2.
- [L204-L208](https://github.com/OffchainLabs/nitro/blob/v3.11.4/wsbroadcastserver/wsbroadcastserver.go#L204-L208): в ответ шлёт `Arbitrum-Feed-Server-Version` и `Arbitrum-Chain-Id`.
- [L267-L292](https://github.com/OffchainLabs/nitro/blob/v3.11.4/wsbroadcastserver/wsbroadcastserver.go#L267-L292): разбор заголовков. Версия клиента меньше 2 — отказ 400. Кривой номер — 400. Номер уходит в `requestedSeqNum`.
- [L300-L305](https://github.com/OffchainLabs/nitro/blob/v3.11.4/wsbroadcastserver/wsbroadcastserver.go#L300-L305): отсутствие версии — отказ только при `require-version`, а по умолчанию это `false` ([L121](https://github.com/OffchainLabs/nitro/blob/v3.11.4/wsbroadcastserver/wsbroadcastserver.go#L121)).
- [L378](https://github.com/OffchainLabs/nitro/blob/v3.11.4/wsbroadcastserver/wsbroadcastserver.go#L378): номер передаётся в `NewClientConnection`.

**Отдача бэклога** — `wsbroadcastserver/clientconnection.go`:
- [L203-L213](https://github.com/OffchainLabs/nitro/blob/v3.11.4/wsbroadcastserver/clientconnection.go#L203-L213): если начало бэклога меньше requested, сегмент ищется через `Lookup(requested)`. Не нашёлся — `"sending the entire backlog instead"`. Если requested = 0 (заголовка нет), отдаётся весь бэклог.
- [L122-L165](https://github.com/OffchainLabs/nitro/blob/v3.11.4/wsbroadcastserver/clientconnection.go#L122-L165): `writeBacklog` отдаёт каждый сегмент одним конвертом, первый сегмент обрезан до requested.
- [L246-L263](https://github.com/OffchainLabs/nitro/blob/v3.11.4/wsbroadcastserver/clientconnection.go#L246-L263): добор пропущенного между бэклогом и живым потоком.

**Бэклог** — `broadcaster/backlog/backlog.go` и `config.go`:
- [backlog.go L125-L127](https://github.com/OffchainLabs/nitro/blob/v3.11.4/broadcaster/backlog/backlog.go#L125-L127), [L243-L303](https://github.com/OffchainLabs/nitro/blob/v3.11.4/broadcaster/backlog/backlog.go#L243-L303): при каждом `ConfirmedSequenceNumberMessage` удаляется всё до confirmed включительно. Бэклог хранит только неподтверждённый хвост.
- [L314-L321](https://github.com/OffchainLabs/nitro/blob/v3.11.4/broadcaster/backlog/backlog.go#L314-L321): `Lookup` по индексу.
- [config.go L23](https://github.com/OffchainLabs/nitro/blob/v3.11.4/broadcaster/backlog/config.go#L23): `SegmentLimit` = 240 сообщений на сегмент.
- [relay/relay.go L101-L102](https://github.com/OffchainLabs/nitro/blob/v3.11.4/relay/relay.go#L101-L102): relay пробрасывает confirmed из входного фида в свой broadcaster, то есть обрезает бэклог так же.

## Проверено / предполагается

**Проверено (2026-09-30, как):**
- Поведение по трём глубинам и цифры таблицы — зонд, 3 соединения, сырые кадры и журнал в scratchpad (пути ниже).
- 30/30 `blockHash` бэклога совпали с `eth_getBlockByNumber(false)` публичного RPC. Выборка: 10 равномерно на глубину, `hashcheck-depth*.tsv`.
- В бэклоге и следующем за ним живом потоке нет дыр и дублей. Все сообщения kind 3, по 1 в конверте. Скрипт `analyze.py` по сырым кадрам.
- Ответ сервера на Close 1000 — Close 1000 за ~150 мс, три раза из трёх.
- Три исключения из stock-Nitro: нет заголовков версии и chain id в 101, бэклог не пачками, без заголовка поток начинается у tip (последнее — по сырью 001/002).

**Предполагается:**
- Начало бэклога = confirmed+1 (как в Nitro). Глубина 596–661 блок согласуется с отставанием confirmed на 287–613 блоков из задачи 002, но в этих сессиях confirmed-сообщений не было, прямо не сверял. Тогда глубина бэклога плавает вместе с отставанием confirmed: примерно 30–65 с, возможно и меньше.
- `arb-relay` — изменённый Nitro relay или свой relay. Выводы только по поведению, код сервера мы не видим.
- Что поведение одинаково у всех бэкендов за балансировщиком `__cflb`: у трёх соединений были разные cookie, результат один.
- Что заголовок не повышает риск бана. За 3 соединения банов не было, но это мало.

## Что не получилось / ограничения

- Бэклог глубже ~660 блоков с публичного фида получить нельзя. Глубины 1 000 и 10 000 фактически дали одно и то же: весь бэклог.
- Context7 в сессии недоступен. Исходники Nitro взяты с GitHub raw по тегу — это первоисточник.
- Время в `rpc_calls.log` записано с суффиксом `.3NZ`: у `date` в macOS нет `%N`. Секунды верные, на результат не влияет.

## Предложение для recorder (не реализовано, ждёт разбора в Cowork)

1. При каждом переподключении слать `Arbitrum-Feed-Client-Version: 2` и `Arbitrum-Requested-Sequence-Number: last_seq + 1`, где `last_seq` из `last_seq.txt` или из памяти процесса. Если простой был короче ~30 с (ориентир — нижняя граница наблюдённого отставания confirmed), дыра закроется самим фидом. Потерь при переподключении не будет, `gaps.tsv` пустеет.
2. Дыра регистрируется по факту: если первый полученный seq > requested, в `gaps.tsv` пишется `[requested, first_seq − 1]`. Детектор дыр по разрыву seq в hood-core, по-видимому, даст то же самое без изменений. Это надо проверить тестом, код детектора в этой задаче я не читал.
3. После долгого простоя (например, пауза бана 3600 с) заголовок безвреден: придёт весь бэклог, дыра до его начала уйдёт в `gaps.tsv` и на RPC-дозаливку.
4. Разбор кадров не менять: бэклог идёт теми же конвертами по 1 сообщению. Повторы seq при перекрытии с уже записанным должен отбрасывать дедуп по seq на загрузке. Сырьё не трогаем.
5. К вопросу 3 из отчёта 008 (Close при idle-таймауте). Настоящий сервер отвечает на Close за ~150 мс, а не за 2 с, так что Close перед переподключением почти ничего не стоит. Вместе с п. 1 ожидание ответа Close к тому же не теряет данных: всё пропущенное придёт из бэклога.

Архитектурный вывод для Cowork: RPC-дозаливка остаётся для дыр длиннее ~30–60 с (простой процесса, бан, падение), но короткие обрывы соединения больше не будут давать дыр.

## Артефакты для data-auditor

Каталог `/private/tmp/claude-501/-Users-mihailshumilov-sites-my-crypto-atomic-arbitrage/a477d6fc-6af5-4e28-af3e-63445fb94d7a/scratchpad/005/`:
- `frames-depth100.tsv`, `frames-depth1000.tsv`, `frames-depth10000.tsv` — сырые кадры, без сжатия, 2.4 / 8.1 / 7.7 МБ.
  - Первые две строки — комментарии: `requested`, `tip`, `connected_ns`.
  - Дальше `recv_unix_ns \t first_seq \t last_seq \t n_msgs \t payload`. Текстовые кадры записаны как пришли. Ping, close и кадры, пришедшие после нашего Close, записаны как `{"recorderFrame":{"opcode":…,"payloadBase64":…}}`.
- `connections.tsv` — журнал зонда: attempt, connected с полным ответом 101, closed с итогом.
- `hashcheck-depth{100,1000,10000}.tsv` — seq, blockHash фида, hash RPC, совпадение.
- `rpc_calls.log` — все 33 вызова RPC с HTTP-кодом.
- `analyze.py`, `hashcheck.py`, `rpc.sh` — скрипты разбора и сверки.
- `nitro/` — скачанные исходники Nitro v3.11.4.

Зонд: `crates/recorder/examples/feed_probe.rs` (новый файл, `src` не менялся).

## Вопросы к Cowork / Михаилу

1. Принимаем ли предложение п. 1–4 для recorder отдельной задачей? Изменение небольшое: заголовки в `net.rs` плюс тест на моке, что requested уходит в запрос.
2. Оставлять ли `examples/feed_probe.rs` в репозитории? Он может пригодиться для замера глубины бэклога на delayed-фиде (задача 006) и на сервере. Или убрать до коммита?
3. Нужно ли прямо измерить связь «начало бэклога = confirmed+1»? Для этого нужно одно соединение на ~60 с с заголовком на глубину ~2 000, чтобы дождаться confirmed-сообщения.
