# pools.trade (now pools.xyz, Uniswap Labs): Liquidity Launchpad events (minimal ABI)

pools.trade is built by Uniswap Labs on the Uniswap Liquidity Launchpad (https://blog.uniswap.org/pools-trade-a-new-way-to-launch-on-robinhood-chain,
2026-08-05): "Instant Launch" = `InstantLaunchStrategy` (hookless native-ETH v4 pool, LP fee 2500, tick spacing 25, called through
`LiquidityLauncher.distributeToken()`, https://developers.uniswap.org/docs/liquidity/liquidity-launchpad/concepts/instant-launch);
"Crowd Launch" = Continuous Clearing Auction (CCA). `pools.trade` answers 301 → `https://pools.xyz/` (checked 2026-10-05).
The blog does not list addresses; addresses come from https://developers.uniswap.org/docs/liquidity/liquidity-launchpad/deployments
and https://developers.uniswap.org/deployments.json (generatedAt 2026-09-22, source Uniswap/contracts @ `a677c0d4`).

Generated on 2026-10-05 (events only) from:
- Uniswap/liquidity-launcher, branch `main`, commit `4007258d29b6240711429001d5ef6ddf44ec2851` (2026-09-29):
  `src/interfaces/ILiquidityLauncher.sol`, `src/interfaces/IFeeSplitter.sol`, `src/strategies/InstantLaunchStrategy.sol`.
- Uniswap/continuous-clearing-auction, branch `main`, commit `6c9e559e63a7a141a4fe4bd5aa0f47fee1354b58` (2026-07-13):
  `src/interfaces/IContinuousClearingAuction.sol`, `src/interfaces/IContinuousClearingAuctionFactory.sol`.
The deployed versions (v3.2.0 / v3.3.0, CCA v2.x) may differ from `main`; the topic0 below that were seen on chain match.

Applies to (registry status `observed` on 2026-10-05): LiquidityLauncher `0x0000ffffbe8efe702c8703ae3477ff5de3d319c0`,
InstantLaunchStrategy v3.2.0 `0x23f8209572b4a1c2ad88a42749e830791fb027f1` / `0xad44d55e7f8337c3ce113fbb591486e85be104b2`,
v3.3.0 `0x7c48dde3b447381f4d986334679b3afc7f2d35c2` / `0xc9566675b1ea42861546f3c5b74ace2c79c49572`, FeeSplitters, CCA factory
`0x000000001f26a0044baa66024e7b6599c61963f8` — see `contracts.md`. Not the Blockscout-verified ABI.

topic0 (computed 2026-10-05; data = local `data/blocks` + `data/samples`, 2 712 blocks):
- `TokenCreated(address)` (LiquidityLauncher) = `0x2e2b3f61b70d2d131b2a807371103cc98d51adcaa5e9a8f9c32658ad8426e74e` (data: 2, LiquidityLauncher)
- `TokenDistributed(address,address,uint256)` (LiquidityLauncher) = `0x67226bacccef969dab310a9e55dc1cf821363658e433fd330344f5cc00c79ac8` (data: 3, LiquidityLauncher)
- `TokenLaunched(bytes32,address,address,(address,address,uint24,int24,address))` (InstantLaunchStrategy) = `0x3b3d2bafdcae274a232217e1f80ee4305d3af6aa25c8b14b1681bd68d18042a4` (data: 1 from v3.2.0 `0x23f8…27f1`, 1 from v3.3.0 `0x7c48…35c2`)
- `FeesCollected(uint256,address,uint256,uint256)` (FeeSplitter) = `0x1df95d0058852523ea13aa1809224af942bd446ce86d3e431bf193c2bec269f1` (data: 6; 3 from FeeSplitter `0xeff1…acdf`, 3 from two unlisted contracts)
- `FeesForwarded(address,address,uint256)` (FeeSplitter) = `0x8d6dc9ddf16486b9c0142a50ed03c92af9e6a98d91c3c6ea900ef44b3d602e3b` (data: 7, FeeSplitter `0xeff1…acdf`)
- `AuctionCreated(address,address,uint256,bytes)` (CCA factory) = `0x7ede475fad18ccf0039f2b956c4d43a8b4ed0853de4daaa8ae25299f331ae3b9` (data: 0)
- `BidSubmitted(uint256,address,uint256,uint128)` (auction) = `0x650baad5cd8ca09b8f580be220fa04ce2ba905a041f764b6a3fe2c848eb70540` (data: 1, auction `0x9a36f2f37d1e865d86f23d07d0bb119e6658caee`)
- `CheckpointUpdated(uint256,uint256,uint24)` = `0xf1e4b6d7d0d7c5deb6393a39862d66a2f2ecb034f3283a8a597f9bf0c36f76fa` (data: 1, same auction)
- `ClearingPriceUpdated(uint256,uint256)` = `0x30adbe996d7a69a21fdebcc1f8a46270bf6c22d505a7d872c1ab4767aa707609` (data: 1, same auction)
- `BidExited(uint256,address,uint256,uint256)` = `0x054fe6469466a0b4d2a6ae4b100e5f9c494c958f04b4000f44d470088dd97930`
- `TokensClaimed(uint256,address,uint256)` = `0x880f2ef2613b092f1a0a819f294155c98667eb294b7e6bf7a3810278142c1a1c`
- `TokensReceived(uint128)` = `0x468160b6769cb8abc9324bc14fe70ee0ce87f1e92087186c6ae22a964a04c572`

Trading after an Instant Launch is a plain v4 `Swap` on PoolManager (hooks = 0, fee 2500, tickSpacing 25) — the pool is not
distinguishable from any other hookless 2500/25 pool by `Swap` alone; tie it to the launch via `TokenLaunched` / `Initialize`
in the same tx. None of these topic0 are in `topics_are_canonical` yet.
