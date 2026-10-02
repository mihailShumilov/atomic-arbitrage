//! Ethereum JSON-RPC hex quantities (`"0x1a"`), as used in block numbers,
//! log indexes and `eth_getLogs` filters.
//!
//! Addresses and hashes are not here yet: in the crates that use hood-core
//! (recorder, enricher) nothing parses them; the decoders have their own
//! helpers on top of alloy types.

/// Format a quantity the way JSON-RPC expects it: `0x` + lowercase hex, no
/// leading zeros (`0` is `"0x0"`).
pub fn quantity(n: u64) -> String {
    format!("0x{n:x}")
}

/// Parse a hex quantity: `0x` followed by one or more hex digits (either
/// case) whose value fits in `u64`. Leading zeros are tolerated and do not
/// count against the size (`0x` + 20 digits with leading zeros is fine;
/// some providers send them). A sign, whitespace, an empty body, `0X` or a
/// value above `u64::MAX` give `None`.
pub fn parse_quantity(s: &str) -> Option<u64> {
    let digits = s.strip_prefix("0x")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(digits, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_and_parse() {
        assert_eq!(quantity(0), "0x0");
        assert_eq!(quantity(77_200_000), "0x499fa80");
        assert_eq!(quantity(u64::MAX), "0xffffffffffffffff");
        for n in [0, 1, 255, 77_200_000, u64::MAX] {
            assert_eq!(parse_quantity(&quantity(n)), Some(n));
        }
        assert_eq!(parse_quantity("0x0A"), Some(10));
        assert_eq!(parse_quantity("0x000a"), Some(10));
        // More than 16 digits is fine while the value fits (task 025, doc fix).
        assert_eq!(parse_quantity("0x0000ffffffffffffffff"), Some(u64::MAX));
    }

    #[test]
    fn rejects_malformed() {
        for s in ["", "0x", "0X1", "1a", "0x+1", "0x-1", "0x 1", "0xg", "0x10000000000000000"] {
            assert_eq!(parse_quantity(s), None, "{s:?}");
        }
    }
}
