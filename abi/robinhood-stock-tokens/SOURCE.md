# Robinhood Stock Tokens: ERC-8056 extension (minimal ABI)

Written by hand on 2026-10-05 (task 037) from the interface listings on
https://docs.robinhood.com/chain/building-with-stock-tokens/ (`IScaledUIAmount`, `IScaledUIAmountBalances`,
`IScaledUIAmountNewUIMultiplier`; page bundle `index-B6lvQvxw.js` of the docs site, read 2026-10-05).
Plus standard ERC-20 (`name`, `symbol`, `decimals` = 18 per the same page, `balanceOf`, `transfer`, `Transfer`).

Not the Blockscout-verified ABI (Blockscout is behind Cloudflare, 403 from here).

Applies to: Robinhood Stock Tokens, chain 4663 (registry status `observed`, see `contracts.md`, row
"Robinhood Stock Tokens"). On-chain shape (RPC, 2026-10-05): every token is a 283-byte BeaconProxy whose
runtime code embeds beacon `0xe10b6f6b275de231345c20d14ab812db62151b00`; `implementation()` of the beacon =
`0xb35490d6f9163de4f80d88dc75c3516eb64c5ae2` (11 614 bytes, contains selector `uiMultiplier()` = `0xa60bf13d`
and the `UIMultiplierUpdated` topic0).

Selectors / topic0 (keccak computed 2026-10-05; not in `topics_are_canonical`, not checked against logs):
- `uiMultiplier()` = `0xa60bf13d`
- `UIMultiplierUpdated(uint256,uint256,uint256)` = `0x2205df4534432b2f60654a3fdb48737ffdaf3e9edb1a498bd985bc026b15b055`
- `TransferWithScaledUI(address,address,uint256,uint256)` = `0x37e7f0db430edc9dd31bc66f25f8449353aa0818f503b906747dd8f286cd3802`
