//! Contract addresses used by the decoders. ONLY entries with status `verified` in
//! `.claude/skills/hoodchain-mev/references/contracts.md` may appear here, each with its status,
//! date and source. `observed` addresses are never hardcoded: research callers pass them as
//! input (e.g. a gateway registry TSV). The test below lists every address of this module, so a
//! diff of this file is the review point against `contracts.md`.

use alloy_primitives::{address, Address};

/// L2 WETH gateway (proxy). `verified` in references/contracts.md (Mihail, 2026-10-01: docs.robinhood.com
/// /chain/protocol-contracts "L2 Weth Gateway" + Blockscout proxy verified; implementation not checked).
pub const L2_WETH_GATEWAY: Address = address!("1d187c3e2da52d72bc9c41e3aba0fdfa6a7bf055");

/// L2 WETH (aeWETH, proxy). `verified` in references/contracts.md (Mihail, 2026-10-01: docs.robinhood.com
/// /chain/contracts "WETH" + Blockscout proxy verified; implementation not checked).
pub const L2_WETH: Address = address!("0bd7d308f8e1639fab988df18a8011f41eacad73");

/// Pons v2 meme hook (`PonsV2MemeHook`, Uniswap v4 hook singleton). `verified` in
/// references/contracts.md (Mihail, 2026-10-05: Blockscout exact match `PonsV2MemeHook`, constructor
/// poolManager = v4 PoolManager `0x8366…0951`; docs.ponsfamily.com/docs/v2 "Meme hook"). Used only
/// to tell its v4 pools apart (venue `pons_v2_hook`, task 036); see [`crate::pools::BUILTIN_V4_HOOKS`].
pub const PONS_V2_MEME_HOOK: Address = address!("e5e702641ea86f4ae6cc3cdaed2b886f976be044");

/// Every address of this module with its role, for audits.
pub const VERIFIED: [(&str, Address); 3] =
    [("L2 WETH Gateway", L2_WETH_GATEWAY), ("L2 WETH", L2_WETH), ("Pons v2 meme hook", PONS_V2_MEME_HOOK)];

#[cfg(test)]
mod tests {
    use super::*;

    /// Update together with references/contracts.md: an address is added here only after
    /// Mihail sets it to `verified` there.
    #[test]
    fn only_verified_addresses() {
        assert_eq!(
            VERIFIED,
            [
                ("L2 WETH Gateway", address!("1d187c3e2da52d72bc9c41e3aba0fdfa6a7bf055")),
                ("L2 WETH", address!("0bd7d308f8e1639fab988df18a8011f41eacad73")),
                ("Pons v2 meme hook", address!("e5e702641ea86f4ae6cc3cdaed2b886f976be044")),
            ]
        );
    }
}
