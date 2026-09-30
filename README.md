# hoodchain-mev

Конвейер данных и аналитики по Robinhood Chain. Фаза 1: только данные и исследование, без торговли. Подробности — в `CLAUDE.md`.

## Быстрый старт

```bash
# 1. Rust (если нет)
curl https://sh.rustup.rs -sSf | sh

# 2. Репозиторий
git init && git add . && git commit -m "starter kit"
cp .env.example .env    # вписать RPC_URL провайдера и пароль ClickHouse

# 3. Сборка и тесты
cargo build --release --workspace
cargo test --workspace

# 4. Запись фида (оставить работать постоянно: tmux / systemd)
cargo run --release -p recorder -- --out-dir data/feed

# 5. Проверка enricher на коротком диапазоне
cargo run --release -p enricher -- --from 74755960 --to 74755980

# 6. ClickHouse
docker compose up -d clickhouse
```

## Что уже проверено

- recorder подключается к живому фиду (permessage-deflate, HTTP/1.1) и пишет сырые конверты: 24 с записи = 246 блоков подряд без пропусков.
- После перезапуска recorder продолжает с `last_seq.txt` и записывает дыру простоя в `gaps.tsv`; enricher дозаливает ровно этот диапазон через RPC.
- `sequenceNumber` фида = номер L2-блока (сверено по `blockHash`).
- `eth_getBlockReceipts` работает на публичном RPC.
- topic0 событий Uniswap v3/v4 и ERC-20 закреплены тестами.

## Первые запросы в Claude Code

Открой папку в Claude Code и начни, например, так:

1. «Прочитай CLAUDE.md и скилл hoodchain-mev. Собери и запусти тесты, запусти recorder на 10 минут и покажи, что записалось и есть ли дыры.»
2. «Как indexer-engineer: добавь в enricher режим `--gaps` и загрузку blocks/txs/logs в ClickHouse. Потом попроси data-auditor проверить загрузку на 10 000 блоков.»
3. «Помоги заполнить references/contracts.md по процедуре верификации: найди официальные адреса Pons v1/v2, pools.trade, WETH, USDG, Uniswap v3/v4 и проверь их на Blockscout. Ничего не помечай verified без моего подтверждения.»

## Безопасность

- `.env` и `data/` в `.gitignore`. Ключи провайдеров не должны попадать в код и логи.
- Адреса контрактов — только из реестра со статусом `verified`. Вокруг сети много фишинговых копий.
