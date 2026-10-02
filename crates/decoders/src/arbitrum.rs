//! Arbitrum (Nitro) protocol constants: L1-message transaction types and L1 address aliasing.
//! These are protocol constants, not contract addresses (those live in [`crate::addresses`]).

use alloy_primitives::{address, Address, U256};

/// `0x64` `ArbitrumDepositTx` (feed kind 12, also kind 7).
pub const TX_TYPE_DEPOSIT: u8 = 0x64;
/// `0x68` `ArbitrumRetryTx` (auto-redeem after a `0x69`, or a manual redeem).
pub const TX_TYPE_RETRY: u8 = 0x68;
/// `0x69` `ArbitrumSubmitRetryableTx` (feed kind 9).
pub const TX_TYPE_SUBMIT_RETRYABLE: u8 = 0x69;

/// Nitro `AddressAliasOffset` (protocol constant, not a contract): L2 alias = L1 address + offset
/// mod 2^160. Nitro v3.11.4 `arbos/util/util.go` `init()` / `RemapL1Address`.
pub const ALIAS_OFFSET: Address = address!("1111000000000000000000000000000000001111");

/// L1 address behind an L2 alias: `(alias - 0x1111…1111) mod 2^160`
/// (Nitro `InverseRemapL1Address`).
#[must_use]
pub fn unalias(alias: Address) -> Address {
    let modulus = U256::from(1u8) << 160;
    let a = U256::from_be_slice(alias.as_slice());
    let off = U256::from_be_slice(ALIAS_OFFSET.as_slice());
    let r: U256 = (a + modulus - off) % modulus;
    Address::from_slice(&r.to_be_bytes::<32>()[12..])
}

/// Inverse of [`unalias`] (Nitro `RemapL1Address`).
#[must_use]
pub fn alias(l1: Address) -> Address {
    let modulus = U256::from(1u8) << 160;
    let a = U256::from_be_slice(l1.as_slice());
    let off = U256::from_be_slice(ALIAS_OFFSET.as_slice());
    let r: U256 = (a + off) % modulus;
    Address::from_slice(&r.to_be_bytes::<32>()[12..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_roundtrip_and_wrap() {
        // Block 77285531, tx 0x3f8e47b0…b522c1 (0x64): from = alias(to) for an EOA deposit.
        let from = address!("8c6ca3c268bb600e48bb5aaed5f9f701adc2d177");
        let to = address!("7b5ba3c268bb600e48bb5aaed5f9f701adc2c066");
        assert_eq!(unalias(from), to);
        assert_eq!(alias(to), from);
        // Wrap-around below the offset: unalias(0x…01) = 2^160 + 1 - offset.
        let low = address!("0000000000000000000000000000000000000001");
        assert_eq!(unalias(low), address!("eeeeffffffffffffffffffffffffffffffffeef0"));
        assert_eq!(alias(unalias(low)), low);
        // Wrap-around above: alias(0xfff…f) = offset - 1.
        let high = address!("ffffffffffffffffffffffffffffffffffffffff");
        assert_eq!(alias(high), address!("1111000000000000000000000000000000001110"));
    }
}
