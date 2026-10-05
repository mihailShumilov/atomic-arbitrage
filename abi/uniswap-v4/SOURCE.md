# Uniswap v4: PoolManager events (minimal ABI)

Generated on 2026-10-05 from Uniswap/v4-core, branch `main`, commit `46c6834698c48bc4a463a86d8420f4eb1d7f3b75`
(2026-04-02), `src/interfaces/IPoolManager.sol` (`Initialize`, `ModifyLiquidity`, `Swap`, `Donate`).
`PoolKey`-typed parameters are flattened as in the source (`Currency` = address, `PoolId` = bytes32, `IHooks` = address).

Applies to (registry status `observed`, candidate for verified, on 2026-10-05):
- PoolManager `0x8366a39cc670b4001a1121b8f6a443a643e40951` (Robinhood Chain, per
  https://developers.uniswap.org/contracts/v4/deployments section "Robinhood Chain: 4663" and
  https://developers.uniswap.org/deployments.json record `v4 PoolManager`, sourceRef `56928a9`).

Not the Blockscout-verified ABI.

topic0 (computed 2026-10-05; data = local `data/blocks` + `data/samples`, 2 712 blocks):
- `Initialize(bytes32,address,address,uint24,int24,address,uint160,int24)` = `0xdd466e674ea557f56295e2d0218a125ea4b4f0f6f3307b95f85e6110838d6438` (in `topics_are_canonical`; data: 30, all from PoolManager)
- `Swap(bytes32,address,int128,int128,uint160,uint128,int24,uint24)` = `0x40e9cecb9f5f1f1c5b9c97dec2917b7ee92e57ba5563708daca94dd84ad7112f` (in `topics_are_canonical`; data: 6 668, all from PoolManager)
- `ModifyLiquidity(bytes32,address,int24,int24,int256,bytes32)` = `0xf208f4912782fd25c7f114ca3723a2d5dd6f3bcc3ac8db5af63baa85f711d5ec` (data: 681, all from PoolManager)
- `Donate(bytes32,address,uint256,uint256)` = `0x29ef05caaff9404b7cb6d1c0e9bbae9eaa7ab2541feba1a9c4248594c08156cb` (data: 5, all from PoolManager)

Venue of a v4 pool is decided by `Initialize.hooks` (+ fee/tickSpacing), not by the emitter: see `contracts.md`
(Pons v2 hook, Doppler hook, pools.trade = hookless 2500/25 via LiquidityLauncher).
