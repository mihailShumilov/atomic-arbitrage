# Uniswap v3: factory and pool events (minimal ABI)

Generated on 2026-10-05 from the official Uniswap repository (events only, names and `indexed` flags as in the source):

- Uniswap/v3-core, branch `main`, commit `d0831dc6b8a318df3872b6d68f6de135c9f3ec29` (2026-04-30):
  `contracts/interfaces/IUniswapV3Factory.sol` (`PoolCreated`),
  `contracts/interfaces/pool/IUniswapV3PoolEvents.sol` (`Initialize`, `Mint`, `Collect`, `Burn`, `Swap`, `Flash`).

Applies to (registry: `.claude/skills/hoodchain-mev/references/contracts.md`, status `observed` on 2026-10-05):
- UniswapV3Factory `0x1f7d7550b1b028f7571e69a784071f0205fd2efa` (Robinhood Chain, per
  https://developers.uniswap.org/docs/protocols/v3/deployments/v3-robinhood-chain-deployments and
  https://developers.uniswap.org/deployments.json, record `v3-uniswapv3factory-robinhood-chain`, sourceRef `56928a9`).
- Every pool deployed by that factory. Pool address = CREATE2(factory, keccak(abi.encode(token0, token1, fee)),
  POOL_INIT_CODE_HASH `0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54`); checked offline on 2026-10-05
  against all 3 `PoolCreated` logs in local data (3 of 3 match).

Not the Blockscout-verified ABI (Blockscout API is behind Cloudflare for agents).

topic0 (keccak of the canonical signature, computed 2026-10-05; "data" = logs in local `data/blocks` + `data/samples`, 2 712 blocks):
- `PoolCreated(address,address,uint24,int24,address)` = `0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118` (data: 3, all from the factory)
- `Initialize(uint160,int24)` = `0x98636036cb66a9c19a37435efc1e90142190214e8abeb821bdba3f2990dd4c95` (data: 3)
- `Swap(address,address,int256,int256,uint160,uint128,int24)` = `0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67` (in `topics_are_canonical`; data: 6 862 from 1 464 emitters, of which 1 275 emitters / 5 906 logs are pools of the factory above)
- `Mint(address,address,int24,int24,uint128,uint256,uint256)` = `0x7a53080ba414158be7ec69b987b5fb7d07dee101fe85488f0853ae16239d0bde`
- `Burn(address,int24,int24,uint128,uint256,uint256)` = `0x0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c`
- `Collect(address,address,int24,int24,uint128,uint128)` = `0x70935338e69775456a85ddef226c395fb668b63fa0115f5f20610b388e6ca9c0`
- `Flash(address,address,uint256,uint256,uint256,uint256)` = `0xbdbdb71d7860376ba52b25a5028beea23581364a40522f6bcfb86bb1f2dca633`

Warning: the same `Swap` topic0 is emitted by v3 forks. In local data 189 emitters (956 logs) are not CREATE2-derivable
from the official factory with fee tiers 100/500/2500/3000/10000; two checked by `factory()` belong to other factories
(`0x1ac9db4a2608ba45d6127b1737949b51bb54b7f3`, `0xe0c4ceb92d08ca985bb70fe0a22feb121a9854a8`). Filter pools by factory.
