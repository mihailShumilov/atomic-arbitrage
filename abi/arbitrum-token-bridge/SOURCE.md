# Arbitrum token bridge: L2 WETH gateway and aeWETH (minimal ABI)

Hand-written minimal ABI (only the events and functions the pipeline reads), transcribed on 2026-10-01 from
OffchainLabs/token-bridge-contracts, branch `main`, commit `0746a71321cdb2d6df6b15158c7ecbb9ece84b12`
(2026-03-13):

- `contracts/tokenbridge/arbitrum/gateway/L2ArbitrumGateway.sol` — `DepositFinalized`, `WithdrawalInitiated`, `finalizeInboundTransfer`
- `contracts/tokenbridge/arbitrum/gateway/L2WethGateway.sol` — `l1Weth`, `l2Weth`, `inboundEscrowTransfer` (deposit + transfer)
- `contracts/tokenbridge/libraries/aeWETH.sol` — `deposit`, `withdraw`, `depositTo`, `withdrawTo`, `bridgeMint`/`bridgeBurn` (OZ ERC20 `_mint`/`_burn`)
- `contracts/tokenbridge/arbitrum/IArbToken.sol` — `l1Address`

Applies to (registry: `.claude/skills/hoodchain-mev/references/contracts.md`, status: L2 WETH gateway and L2 WETH `verified` since 2026-10-01 (proxies verified on Blockscout), router and L1 WETH gateway `observed`):
- L2 WETH gateway `0x1d187c3e2da52d72bc9c41e3aba0fdfa6a7bf055` (proxy, impl `0x0354a93fe0db94bb72ec053f43301746fc806edf` on 2026-10-01)
- L2 WETH `0x0bd7d308f8e1639fab988df18a8011f41eacad73` (proxy, impl `0xc6b81b429797e0f555440b70cd99e032d7ae947e` on 2026-10-01)

Not the Blockscout-verified ABI: Blockscout API returned a Cloudflare challenge (403) on 2026-10-01, so it is not
yet confirmed that the deployed implementations match this source code. The upstream code matches what was observed on
chain in block 77312169 (topic0 and selectors below).

topic0 (keccak of the signature, computed 2026-10-01):
- `DepositFinalized(address,address,address,uint256)` = `0xc7f2e9c55c40a50fbc217dfc70cd39a222940dfa62145aa0ca49eb9535d4fcb2` (matches the log in block 77312169)
- `WithdrawalInitiated(address,address,address,uint256,uint256,uint256)` = `0x3073a74ecb728d10be779fe19a74a1428e20468f5b4d167bf9c73d9067847d73` (not observed on chain yet)
- selector `finalizeInboundTransfer(address,address,address,uint256,bytes)` = `0x2e567b36` (matches `retryData` / 0x68 `input` in block 77312169)

aeWETH has no WETH9-style `Deposit`/`Withdrawal` events: wrapping/unwrapping emits `Transfer` from/to the zero address.
