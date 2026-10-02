//! Gateway registry: the caller-supplied set of token gateways the L1 decoder trusts.
//! Nothing is hardcoded beyond the `verified` entries of [`crate::addresses`].

use alloy_primitives::Address;
use anyhow::{bail, Context, Result};

use crate::addresses::{L2_WETH, L2_WETH_GATEWAY};
use crate::model::parse_addr;
use crate::rows::GatewayStatus;

/// Status of a registry entry, mirrors `references/contracts.md`. Only `verified` may be used for
/// final conclusions; `observed` is research-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RegistryStatus {
    Observed,
    Verified,
}

impl RegistryStatus {
    /// Name as in `contracts.md` and in the registry TSV.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Verified => "verified",
        }
    }
}

impl From<Option<RegistryStatus>> for GatewayStatus {
    /// `None` = gateway not in the registry.
    fn from(s: Option<RegistryStatus>) -> Self {
        match s {
            None => Self::None,
            Some(RegistryStatus::Observed) => Self::Observed,
            Some(RegistryStatus::Verified) => Self::Verified,
        }
    }
}

/// One trusted gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayEntry {
    /// L2 gateway contract that emits `DepositFinalized`.
    pub gateway: Address,
    /// Expected L2 token minted/transferred by this gateway, if the gateway is single-token
    /// (e.g. the WETH gateway). `None` = any token (standard ERC-20 gateway).
    pub l2_token: Option<Address>,
    pub status: RegistryStatus,
}

/// Set of gateways, at most one entry per gateway address (every constructor checks it, so an
/// `observed` entry can never shadow a `verified` one).
#[derive(Debug, Clone, Default)]
pub struct GatewayRegistry {
    entries: Vec<GatewayEntry>,
}

impl GatewayRegistry {
    /// # Errors
    /// The same gateway appears twice.
    pub fn new(entries: Vec<GatewayEntry>) -> Result<Self> {
        let mut r = Self::default();
        for e in entries {
            r.push_unique(e)?;
        }
        Ok(r)
    }

    /// Only `verified` entries from references/contracts.md. The L2 Gateway Router and the
    /// L1 WETH gateway are `observed` (2026-10-01) and are deliberately absent; the router does
    /// not emit `DepositFinalized` on L2 anyway, and the L1 gateway is an Ethereum contract.
    #[must_use]
    pub fn builtin() -> Self {
        Self {
            entries: vec![GatewayEntry {
                gateway: L2_WETH_GATEWAY,
                l2_token: Some(L2_WETH),
                status: RegistryStatus::Verified,
            }],
        }
    }

    /// Adds entries from `other`; an entry for a gateway already present is an error
    /// (no silent override of a verified entry).
    ///
    /// # Errors
    /// A gateway of `other` is already present.
    pub fn extend(&mut self, other: GatewayRegistry) -> Result<()> {
        for e in other.entries {
            self.push_unique(e)?;
        }
        Ok(())
    }

    fn push_unique(&mut self, e: GatewayEntry) -> Result<()> {
        if self.get(&e.gateway).is_some() {
            bail!("gateway {} is already in the registry", e.gateway);
        }
        self.entries.push(e);
        Ok(())
    }

    /// All entries in insertion order.
    #[must_use]
    pub fn entries(&self) -> &[GatewayEntry] {
        &self.entries
    }

    /// Entry of `gateway`, if registered.
    #[must_use]
    pub fn get(&self, gateway: &Address) -> Option<&GatewayEntry> {
        self.entries.iter().find(|e| &e.gateway == gateway)
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` if there are no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// TSV: `gateway \t l2_token|- \t verified|observed [\t note…]`. `#` comments and blank lines
    /// are skipped. Statuses `todo`/`rejected` (or anything else) are an error: such addresses
    /// must not be fed to the decoder at all.
    ///
    /// # Errors
    /// Fewer than 3 columns, a bad address (`0x` + 40 hex digits required), a status other than
    /// `verified`/`observed`, a duplicate.
    pub fn parse_tsv(text: &str) -> Result<Self> {
        let mut r = Self::default();
        for (i, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let ln = i + 1;
            let cols: Vec<&str> = line.split('\t').map(str::trim).collect();
            if cols.len() < 3 {
                bail!("gateway registry line {ln}: expected >= 3 tab-separated columns");
            }
            let gateway = parse_addr(cols[0]).with_context(|| format!("line {ln}"))?;
            let l2_token = match cols[1] {
                "-" | "" => None,
                s => Some(parse_addr(s).with_context(|| format!("line {ln}"))?),
            };
            let status = match cols[2] {
                "verified" => RegistryStatus::Verified,
                "observed" => RegistryStatus::Observed,
                other => bail!("gateway registry line {ln}: status {other:?} not allowed (verified|observed)"),
            };
            r.push_unique(GatewayEntry { gateway, l2_token, status })
                .with_context(|| format!("gateway registry line {ln}: duplicate gateway"))?;
        }
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    #[test]
    fn builtin_registry_is_verified_weth_only() {
        let r = GatewayRegistry::builtin();
        assert_eq!(r.len(), 1);
        let e = r.get(&L2_WETH_GATEWAY).unwrap();
        assert_eq!((e.l2_token, e.status), (Some(L2_WETH), RegistryStatus::Verified));
        let mut r2 = GatewayRegistry::builtin();
        assert!(r2.extend(GatewayRegistry::builtin()).is_err());
    }

    #[test]
    fn new_rejects_duplicates() {
        let e = |status| GatewayEntry { gateway: L2_WETH_GATEWAY, l2_token: None, status };
        // An `observed` entry placed first must not shadow a `verified` one: it is an error.
        assert!(GatewayRegistry::new(vec![e(RegistryStatus::Observed), e(RegistryStatus::Verified)]).is_err());
        assert_eq!(GatewayRegistry::new(vec![e(RegistryStatus::Observed)]).unwrap().len(), 1);
    }

    #[test]
    fn registry_tsv() {
        let r = GatewayRegistry::parse_tsv(
            "# comment\n\n0x00000000000000000000000000000000000000aa\t0x00000000000000000000000000000000000000bb\tobserved\tnote\n0x00000000000000000000000000000000000000cc\t-\tverified\n",
        )
        .unwrap();
        assert_eq!(r.len(), 2);
        let a = r.get(&address!("00000000000000000000000000000000000000aa")).unwrap();
        assert_eq!(a.status, RegistryStatus::Observed);
        assert_eq!(a.l2_token, Some(address!("00000000000000000000000000000000000000bb")));
        assert_eq!(r.get(&address!("00000000000000000000000000000000000000cc")).unwrap().l2_token, None);
        assert!(GatewayRegistry::parse_tsv("0x00000000000000000000000000000000000000aa\t-\ttodo\n").is_err());
        assert!(GatewayRegistry::parse_tsv("00000000000000000000000000000000000000aa\t-\tobserved\n").is_err());
        assert!(GatewayRegistry::parse_tsv("0x00000000000000000000000000000000000000aa\t-\trejected\n").is_err());
        assert!(GatewayRegistry::parse_tsv(
            "0x00000000000000000000000000000000000000aa\t-\tobserved\n0x00000000000000000000000000000000000000aa\t-\tverified\n"
        )
        .is_err());
    }

    #[test]
    fn gateway_status_from_registry() {
        assert_eq!(GatewayStatus::from(None), GatewayStatus::None);
        assert_eq!(GatewayStatus::from(Some(RegistryStatus::Observed)), GatewayStatus::Observed);
        assert_eq!(GatewayStatus::from(Some(RegistryStatus::Verified)), GatewayStatus::Verified);
    }
}
