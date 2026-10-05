//! Status of an entry of a caller-supplied registry (token gateways, pools, tokens). Mirrors the
//! statuses of `references/contracts.md`; `todo` and `rejected` addresses must never reach a
//! decoder, so they have no variant here.

/// Status of a registry entry. Only `verified` may be used for final conclusions; `observed` is
/// research-only. `Observed < Verified`, so "at least `min`" is `status >= min`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RegistryStatus {
    Observed,
    Verified,
}

impl RegistryStatus {
    /// Name as in `contracts.md` and in the registry TSV files.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Verified => "verified",
        }
    }

    /// Inverse of [`Self::as_str`]; anything else (`todo`, `rejected`, …) is `None`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "observed" => Some(Self::Observed),
            "verified" => Some(Self::Verified),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_round_trips_and_rejects_other_statuses() {
        for s in [RegistryStatus::Observed, RegistryStatus::Verified] {
            assert_eq!(RegistryStatus::parse(s.as_str()), Some(s));
        }
        for s in ["todo", "rejected", "Verified", ""] {
            assert_eq!(RegistryStatus::parse(s), None);
        }
        assert!(RegistryStatus::Observed < RegistryStatus::Verified);
    }
}
