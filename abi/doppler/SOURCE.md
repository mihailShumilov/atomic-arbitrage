# Doppler (Whetstone Research): Airlock and DopplerHookInitializer events (minimal ABI)

Doppler is a launch protocol used by several front-ends on Robinhood Chain ("other launchpads" in `contracts.md`).
Addresses: official docs https://docs.doppler.lol/reference/contract-addresses, section "Robinhood Mainnet (4663)"
(fetched 2026-10-05; the page says contracts not listed there are not canonical Doppler).

Generated on 2026-10-05 (events only) from whetstoneresearch/doppler, branch `main`, commit
`5754c7ee01f1bdbd6f07c62be721e1223b725ecd` (2026-08-26): `src/Airlock.sol`, `src/base/BaseDopplerHookInitializer.sol`,
`src/initializers/DopplerHookInitializer.sol`, `src/interfaces/IDopplerHookInitializer.sol`. The Robinhood deployment is from
commit `bda077cf` (per docs), which may differ from `main`; the topic0 below that were seen on chain match.

Applies to (registry status `observed` on 2026-10-05): Airlock `0xeb7c034704ef8dcd2d32324c1545f62fb4ad0862`,
DopplerHookInitializer (v4 hook) `0x4e3468951d49f2eea976ed0d6e75ffcb44a9a544`. Not the Blockscout-verified ABI.

topic0 (computed 2026-10-05; data = local `data/blocks` + `data/samples`, 2 712 blocks):
- `Create(address,address,address,address)` (Airlock) = `0x68ff1cfcdcf76864161555fc0de1878d8f83ec6949bf351df74d8a4a1a2679ab` (data: 9, Airlock)
- `Migrate(address,address)` (Airlock) = `0x2a05bb717043f3a794e94382bf63f2e275ecafc41be9b63c34f16d58da9822ca`
- `Collect(address,address,uint256)` (Airlock) = `0x1314fd112a381beea61539dbd21ec04afcff2662ac7d1b83273aade1f53d1b97`
- `Swap(address,(address,address,uint24,int24,address),bytes32,(bool,int256,uint160),int128,int128,bytes)` (hook) = `0x1d9f7b5e406d8c887155e1a78e070d2d41c5d0444dab8b21612f846835c27183` (data: 687, hook)
- `ModifyLiquidity((address,address,uint24,int24,address),(int24,int24,int256,bytes32))` (hook) = `0xdb675a606e5aa8f039e93c54673258dc875053bdaa5dbb96de1670bfdece53b3` (data: 183, hook)
- `Lock(address,(address,uint96)[])` (hook) = `0x5be4f748347693e0500df872d81f7d96bce1b98e6f5adff0cfddfe3e9e415f20` (data: 9, hook)
- `Graduate(address)` (hook) = `0xbd2bd570c963e5fe6bdc6422e5741c710099e75c6d44b6c73e6acc397429bdf7`

Doppler pools: v4 `Initialize` with `hooks = 0x4e34…a544`, dynamic fee flag `0x800000` (9 of 9 in data). Each hook `Swap`
duplicates a PoolManager `Swap` of the same pool — do not count both.
