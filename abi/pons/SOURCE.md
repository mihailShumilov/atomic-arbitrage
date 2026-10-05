# Pons (ponsfamily.com) launchpad: V1 factory ABI and V1/V2 events

Files:
- `PonsLaunchFactoryV1.abi.json` — copied as is (sha256 `241fe299fecab0ff6cd9f150d41ce92d86b5acf5cbbb555fd2b7a49e06a52fd6`)
  from `abi.json` of github.com/ponsdotdev/pons-labs, branch `main`, commit `44a3db9193c365f6c25cf0d4c2efc396e6de0df5`
  (2026-10-01). Per the repo README it is the ABI of the live V1 factory.
- `PonsV1.events.json`, `PonsV2.events.json` — events only, generated on 2026-10-05 from the same commit:
  `contractsV1/src/interfaces/ILaunchpad.sol`, `contractsV1/src/PonsLaunchFactory.sol`,
  `contractsV2/src/v2/{PonsV2LaunchFactory,PonsV2BondingCurve,PonsV2LaunchLocker,PonsV2BuybackVault}.sol`,
  `contractsV2/src/v2/hooks/PonsV2MemeHook.sol`, `contractsV2/src/v2/interfaces/ILaunchpadV2*.sol`.

Address source (primary): official docs https://docs.ponsfamily.com/docs (V1) and https://docs.ponsfamily.com/docs/v2 (V2),
fetched 2026-10-05. The same V1/V2 factory addresses are in the repo README. The repo is treated as official because its
README and owner profile point to ponsfamily.com / @ponsdotfamily; the docs site itself does not link to it, and
`pons-beta/pons-beta.md` in the same repo names other GitHub accounts (`ponsdotfamily/ponsdotfamily`, `pons-labs`) —
open question. Topic0 of V1 `TokenLaunched` in the docs equals the keccak of the repo signature.

Applies to (registry status `observed` on 2026-10-05): V1 factory `0xa5aab3f0c6eeadf30ef1d3eb997108e976351feb`,
V2 factory `0x7ed598bcef8bd9edd8c97a195c6d13f40801ec7e`, V2 meme hook `0xe5e702641ea86f4ae6cc3cdaed2b886f976be044`
and the other V2 addresses listed in `contracts.md`. Not the Blockscout-verified ABI.

topic0 (computed 2026-10-05; data = local `data/blocks` + `data/samples`, 2 712 blocks):

V1
- `TokenLaunched(address,address,address,address,address,uint256,uint256,uint256,uint256,uint256)` = `0xdb51ea9ad51ab453a65a4cb7e60c3cb378c9501bb002609f8f97778fb6c4235a` (same value in docs; data: 0; `eth_getLogs` on the V1 factory over 50 000 blocks to 80 608 414: 0)
- `TokenDeployed(address,address,address,address,uint256,uint256)` = `0x1461370115e1c2be79cb529f8cfcbd11316e789d9c6099fc83417b0b4c48c62a` (data: 0)
- V1 trades are plain Uniswap v3 `Swap` of the token/WETH pool (fee 10000) — see `abi/uniswap-v3/`.

V2
- `TokenLaunched(address,address,address,address,uint256,uint256)` (factory) = `0x8d4aad4953d0ca700d468f3753aa14432d1b35b43ec6409f051fb6aa43a89607` (data: 34, all from the V2 factory; `eth_getLogs` last 5 000 blocks: 19)
- `CurveBuy(address,address,uint256,uint256,uint256,uint256)` (per-launch curve) = `0xec36bf571f136799e8dc0b0b8bea4b04d8bd3d43de838aab0d5fc21d4cbfc455` (data: 604 from 377 curves)
- `CurveSell(address,address,uint256,uint256,uint256,uint256)` (curve) = `0x8113d738abdcb6b38357e9d53a54a7157861a09031b453651f0fe7fe151f59df` (data: 475 from 323 curves)
- `CurveBuyRefunded(address,uint256)` = `0xa69e8258ccc7b9bbb70ab953fc2d1062b4ee28b8ca827534097e1732e87b0262`
- `CurveCompleted(address,uint256,uint256)` = `0xf8d37a90738ae063b8b8058b66f5880cf3cf7ab0c5d4fa78219696591dfbfb67`
- `AutoGraduationFailed(address,uint256)` = `0xe2cd2f31ebc05ec28640102987f4c8fc5f20e269e1b3aa82577f3f2f0e35c7c6`
- `FeesSwept(uint256,uint256,uint256)` (curve) = `0x9f4cd7c4ed99d08a797804560c9c5d71d2cf7e101f2e3b5e7d1ca8a24c370e4f` (data: 97)
- `BuybackLocked(uint256,uint256)` = `0x5feba9b0d52c92ada4b9c571c2bee52390c54f2947208ab250221e6ee32f12ff`
- `LaunchSwept(address,uint256,uint256)` (factory) = `0xcdb72f157fd3666758a6ce201387ffb52038c7562e4fff352828da1096c4b6b4`
- `PoolGraduated(address,uint256,uint256,uint256)` (factory) = `0x0a44ef75df69c534f43cd6c1aa3ef8983065fe5fe79ef9e79f6494e6f258c259`
- `PoolRegistered(bytes32,address,address,address)` (hook) = `0x01bf263a1db1652580721573296e1a1fa70b3d4c87f61d02a69c4e1109d2d573`
- `HookFeeCollected(bytes32,address,uint256,uint256)` (hook) = `0xc532c43b3423e14ef72748f1c8291238829ca0af8ba9b67975ad1483485a4b4d` (data: 1 840 from the V2 hook, 14 from `0x57387759ea3a3116330f4bd2cae48b03091a2044` — not in Pons docs, see contracts.md)
- `PoolFeesSwept(bytes32,uint256,uint256,uint256,uint256)` (hook) = `0x2f3c43579b9064b6f28edcf41608f3815792d274a56afe024359703cb4ea9b30` (data: 87 from the V2 hook, 1 from `0x332f85e7e323214b2d55286414b059ebb1f2a044`)
- `PoolBuybackSkipped(bytes32,uint256)` = `0xbdb9140e5a6bcb57cebdbf44a42f8c0f6c96af972d8f88cc3ae2b974f193bc0d`
- `PoolConversionSkipped(bytes32,uint256)` = `0xeed2d18eb96f3c2cb8c7b6993512a506c170e17d29355f2d7a0d5961f338de09`
- `Locked(address,address,uint256,uint256)` (buyback vault) = `0x967ad762aa9070ada8db64577288e214771e89667066ae38e8750cb8a86c5429` (generic name; the 1 log in data is from another contract)
- `Released(address,uint256,uint256)` (buyback vault) = `0x82e416ba72d10e709b5de7ac16f5f49ff1d94f22d55bf582d353d3c313a1e8dd` (data: 1, from the vault)

None of these topic0 are in `crates/decoders` `topics_are_canonical` yet (indexer-engineer). Curve and token addresses are
per launch (CREATE2); resolve them from V2 `TokenLaunched` (topic1 = token, topic2 = curve), never from `CurveBuy` alone.
