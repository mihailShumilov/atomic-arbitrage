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

/// Every address of this module with its role, for audits.
pub const VERIFIED: [(&str, Address); 2] = [("L2 WETH Gateway", L2_WETH_GATEWAY), ("L2 WETH", L2_WETH)];

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
            ]
        );
    }
}
