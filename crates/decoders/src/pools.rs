//! Pool and token metadata that the `hood.swaps` mapping ([`crate::swap_rows`]) needs and a
//! `Swap` log does not carry: the two currencies of a pool, its venue, the v3 fee tier, token
//! decimals and which currencies are quotes.
//!
//! Everything here is caller input, like [`crate::l1_inflows::GatewayRegistry`]. Nothing is
//! hardcoded beyond the `verified` entries of [`crate::addresses`] (and native ETH). In
//! particular the Uniswap v4 `PoolManager` is `observed` (references/contracts.md, 2026-09-28)
//! and is NOT built in: a caller that wants pools from v4 `Initialize` logs names the manager
//! with [`PoolRegistry::allow_v4_manager`] and its status.
//!
//! The registry is also the emitter filter of `hood.swaps`: a swap gets a row only if its pool
//! (v3: the emitting pool; v4: the pair emitting `PoolManager` + pool id) is in the registry, so a
//! `Swap` log from an unknown contract never becomes a row.

use std::collections::HashMap;

use alloy_primitives::{Address, B256};
use alloy_sol_types::SolEvent;
use anyhow::{bail, ensure, Context, Result};

use crate::addresses::L2_WETH;
use crate::events::{v4, TOPIC_INITIALIZE_V4};
use crate::model::{parse_addr, parse_b256, Log};
use crate::registry::RegistryStatus;
use crate::rows::Venue;
use crate::swaps::{PoolSwap, SwapEvent};

/// Largest accepted token decimals. With it every `raw / 10^decimals` of a `U256` is a finite,
/// non-zero `f64` for a non-zero raw amount (`U256::MAX` < 1.2e77), so a price is never ±inf.
pub const MAX_DECIMALS: u8 = 77;

/// Largest fee in pips (100%), Uniswap v3/v4.
pub const MAX_FEE_PIPS: u32 = 1_000_000;

/// Identity of a pool as seen in a `Swap` log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PoolRef {
    /// v3 pool (the emitter of the `Swap`).
    V3(Address),
    /// v4 pool: the emitting `PoolManager` and the pool id.
    V4 { manager: Address, id: B256 },
}

impl PoolRef {
    /// Pool of a decoded swap; `None` only for a hand-built v4 [`PoolSwap`] without `pool_id`
    /// ([`crate::swaps::decode_swap`] always sets it).
    #[must_use]
    pub fn of(s: &PoolSwap) -> Option<Self> {
        match (s.event, s.pool_id) {
            (SwapEvent::V3, _) => Some(Self::V3(s.pool)),
            (SwapEvent::V4, Some(id)) => Some(Self::V4 { manager: s.pool, id }),
            (SwapEvent::V4, None) => None,
        }
    }
}

/// Where a [`PoolEntry`] comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolSource {
    /// Pool registry TSV (or a test).
    Registry,
    /// A v4 `Initialize` log of an allowed `PoolManager`, seen in the scanned blocks.
    Initialize,
}

/// One pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolEntry {
    pub pool: PoolRef,
    /// v3 `token0` / v4 `currency0` (`0x000…0` = native ETH in v4). `currency0 < currency1`.
    pub currency0: Address,
    pub currency1: Address,
    pub venue: Venue,
    /// v3 fee tier in pips. Not used for v4 (the `Swap` event carries the fee of each swap).
    pub fee_pips: Option<u32>,
    /// v4 hooks contract (`Some(0x000…0)` = no hooks); `None` = unknown or v3.
    pub hooks: Option<Address>,
    pub status: RegistryStatus,
    pub source: PoolSource,
}

/// What [`PoolRegistry::observe_log`] did with a log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InitOutcome {
    /// topic0 is not v4 `Initialize`.
    NotInitialize,
    /// `Initialize` from a contract that is not an allowed `PoolManager`: ignored.
    ForeignEmitter,
    /// New pool added.
    Added,
    /// Pool already known with the same currencies (e.g. from the TSV): kept as it was.
    Known,
    /// Pool already known with OTHER currencies: kept as it was, needs a look.
    Mismatch,
    /// `Initialize` topic0 from an allowed manager that does not decode (or has
    /// `currency0 >= currency1`).
    Malformed,
}

impl InitOutcome {
    /// Name for reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotInitialize => "not_initialize",
            Self::ForeignEmitter => "foreign_emitter",
            Self::Added => "added",
            Self::Known => "known",
            Self::Mismatch => "mismatch",
            Self::Malformed => "malformed",
        }
    }
}

/// Set of pools, at most one entry per [`PoolRef`].
#[derive(Debug, Clone, Default)]
pub struct PoolRegistry {
    pools: HashMap<PoolRef, PoolEntry>,
    v4_managers: Vec<(Address, RegistryStatus)>,
}

impl PoolRegistry {
    /// Adds a pool.
    ///
    /// # Errors
    /// The pool is already present (no silent override), `currency0 >= currency1` (both v3 and v4
    /// sort the currencies), a fee above 100%, hooks on a v3 pool.
    pub fn insert(&mut self, e: PoolEntry) -> Result<()> {
        ensure!(e.currency0 < e.currency1, "pool {:?}: currency0 must be < currency1", e.pool);
        ensure!(e.fee_pips.is_none_or(|f| f <= MAX_FEE_PIPS), "pool {:?}: fee above {MAX_FEE_PIPS} pips", e.pool);
        ensure!(matches!(e.pool, PoolRef::V4 { .. }) || e.hooks.is_none(), "pool {:?}: hooks on a v3 pool", e.pool);
        ensure!(!self.pools.contains_key(&e.pool), "pool {:?} is already in the registry", e.pool);
        self.pools.insert(e.pool, e);
        Ok(())
    }

    /// Adds every pool of `other` (its allowed managers too).
    ///
    /// # Errors
    /// As [`Self::insert`]; a manager allowed in both with different statuses.
    pub fn extend(&mut self, other: PoolRegistry) -> Result<()> {
        let mut entries: Vec<_> = other.pools.into_values().collect();
        entries.sort_by_key(|e| e.pool);
        for e in entries {
            self.insert(e)?;
        }
        for (m, s) in other.v4_managers {
            self.allow_v4_manager(m, s)?;
        }
        Ok(())
    }

    /// Lets [`Self::observe_log`] add pools from `Initialize` logs emitted by `manager`; the
    /// entries get `status`.
    ///
    /// # Errors
    /// `manager` is already allowed with another status.
    pub fn allow_v4_manager(&mut self, manager: Address, status: RegistryStatus) -> Result<()> {
        match self.v4_managers.iter().find(|(m, _)| *m == manager) {
            Some((_, s)) if *s == status => Ok(()),
            Some((_, s)) => bail!("v4 manager {manager:#x} already allowed as {}", s.as_str()),
            None => {
                self.v4_managers.push((manager, status));
                Ok(())
            }
        }
    }

    /// Allowed `PoolManager`s with their statuses.
    #[must_use]
    pub fn v4_managers(&self) -> &[(Address, RegistryStatus)] {
        &self.v4_managers
    }

    /// Entry of `pool`, if registered.
    #[must_use]
    pub fn get(&self, pool: &PoolRef) -> Option<&PoolEntry> {
        self.pools.get(pool)
    }

    /// Number of pools.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pools.len()
    }

    /// `true` if there are no pools.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pools.is_empty()
    }

    /// Pools per source.
    #[must_use]
    pub fn count_by_source(&self, source: PoolSource) -> usize {
        self.pools.values().filter(|e| e.source == source).count()
    }

    /// Registers the pool of a v4 `Initialize` log of an allowed `PoolManager`. Venue: `uni_v4`
    /// for a pool without hooks, `other` with hooks (which launchpad a hook belongs to is not
    /// established: Pons v2 / pools.trade hooks are `todo` in references/contracts.md). Call it in
    /// chain order, before the swaps of the same block position: a pool is initialized before
    /// its first swap.
    pub fn observe_log(&mut self, log: &Log) -> InitOutcome {
        if log.topics.first() != Some(&TOPIC_INITIALIZE_V4) {
            return InitOutcome::NotInitialize;
        }
        let Some(&(manager, status)) = self.v4_managers.iter().find(|(m, _)| *m == log.address) else {
            return InitOutcome::ForeignEmitter;
        };
        let Ok(ev) = v4::Initialize::decode_raw_log(log.topics.iter().copied(), &log.data) else {
            return InitOutcome::Malformed;
        };
        let pool = PoolRef::V4 { manager, id: ev.id };
        if let Some(known) = self.pools.get(&pool) {
            return if (known.currency0, known.currency1) == (ev.currency0, ev.currency1) {
                InitOutcome::Known
            } else {
                InitOutcome::Mismatch
            };
        }
        let entry = PoolEntry {
            pool,
            currency0: ev.currency0,
            currency1: ev.currency1,
            venue: if ev.hooks == Address::ZERO { Venue::UniV4 } else { Venue::Other },
            fee_pips: None,
            hooks: Some(ev.hooks),
            status,
            source: PoolSource::Initialize,
        };
        match self.insert(entry) {
            Ok(()) => InitOutcome::Added,
            Err(_) => InitOutcome::Malformed,
        }
    }

    /// TSV, one pool per line:
    /// `venue \t emitter \t pool_id|- \t currency0 \t currency1 \t fee_pips|- \t hooks|- \t
    /// verified|observed [\t note…]`.
    /// v3: `emitter` = the pool, `pool_id` = `-`, `hooks` = `-`. v4: `emitter` = the
    /// `PoolManager`, `pool_id` = the id. `venue` is an Enum8 name of `hood.swaps.venue`. `#`
    /// comments and blank lines are skipped; `todo`/`rejected` statuses are an error.
    ///
    /// # Errors
    /// Fewer than 8 columns, a bad address/hash/number/venue/status, any [`Self::insert`] error.
    pub fn parse_tsv(text: &str) -> Result<Self> {
        let mut r = Self::default();
        for (ln, cols) in tsv_rows(text) {
            let parse = || -> Result<PoolEntry> {
                ensure!(cols.len() >= 8, "expected >= 8 tab-separated columns");
                let venue = Venue::parse(cols[0]).with_context(|| format!("venue {:?}", cols[0]))?;
                let emitter = parse_addr(cols[1])?;
                let pool = match cols[2] {
                    "-" => PoolRef::V3(emitter),
                    id => PoolRef::V4 { manager: emitter, id: parse_b256(id)? },
                };
                Ok(PoolEntry {
                    pool,
                    currency0: parse_addr(cols[3])?,
                    currency1: parse_addr(cols[4])?,
                    venue,
                    fee_pips: dash_or(cols[5], |s| s.parse::<u32>().with_context(|| format!("fee_pips {s:?}")))?,
                    hooks: dash_or(cols[6], parse_addr)?,
                    status: parse_status(cols[7])?,
                    source: PoolSource::Registry,
                })
            };
            let e = parse().with_context(|| format!("pool registry line {ln}"))?;
            r.insert(e).with_context(|| format!("pool registry line {ln}"))?;
        }
        Ok(r)
    }
}

/// One currency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenEntry {
    /// `0x000…0` = native ETH (v4).
    pub token: Address,
    /// `None` = unknown (the row's `price` is then `nan`). At most [`MAX_DECIMALS`].
    pub decimals: Option<u8>,
    /// `Some(rank)` = a quote currency; in a pool of two quotes the lower rank is the quote
    /// (e.g. USDG 1 < WETH 2: a WETH/USDG pool is priced in USDG). Requires `decimals`.
    pub quote_rank: Option<u8>,
    pub status: RegistryStatus,
}

/// Set of currencies, at most one entry per address.
#[derive(Debug, Clone, Default)]
pub struct TokenRegistry {
    tokens: HashMap<Address, TokenEntry>,
}

/// Quote rank of the built-in quotes (WETH and native ETH share it: an ETH/WETH pool has no
/// quote and is skipped as ambiguous). Rank 1 is left for a stablecoin (USDG is `todo`).
pub const ETH_QUOTE_RANK: u8 = 2;

impl TokenRegistry {
    /// Native ETH (`address(0)`, the v4 `Currency` of ETH; 18 decimals by protocol) and L2 WETH
    /// (`verified` in references/contracts.md). WETH decimals = 18 is ASSUMED (aeWETH source;
    /// not read on chain in this project, no RPC in task 033).
    ///
    /// # Panics
    /// Never: the two built-in entries are distinct and valid.
    #[must_use]
    pub fn builtin() -> Self {
        let mut r = Self::default();
        for token in [Address::ZERO, L2_WETH] {
            r.insert(TokenEntry {
                token,
                decimals: Some(18),
                quote_rank: Some(ETH_QUOTE_RANK),
                status: RegistryStatus::Verified,
            })
            .expect("built-in tokens are distinct and valid");
        }
        r
    }

    /// Adds a currency.
    ///
    /// # Errors
    /// Already present, decimals above [`MAX_DECIMALS`], a quote without decimals.
    pub fn insert(&mut self, e: TokenEntry) -> Result<()> {
        ensure!(e.decimals.is_none_or(|d| d <= MAX_DECIMALS), "token {:#x}: decimals above {MAX_DECIMALS}", e.token);
        ensure!(e.quote_rank.is_none() || e.decimals.is_some(), "token {:#x}: a quote needs decimals", e.token);
        ensure!(!self.tokens.contains_key(&e.token), "token {:#x} is already in the registry", e.token);
        self.tokens.insert(e.token, e);
        Ok(())
    }

    /// Adds every currency of `other`.
    ///
    /// # Errors
    /// As [`Self::insert`].
    pub fn extend(&mut self, other: TokenRegistry) -> Result<()> {
        let mut entries: Vec<_> = other.tokens.into_values().collect();
        entries.sort_by_key(|e| e.token);
        entries.into_iter().try_for_each(|e| self.insert(e))
    }

    /// Entry of `token`, if registered.
    #[must_use]
    pub fn get(&self, token: &Address) -> Option<&TokenEntry> {
        self.tokens.get(token)
    }

    /// Number of currencies.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    /// `true` if there are no currencies.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// TSV, one currency per line: `token \t decimals|- \t quote_rank|- \t verified|observed
    /// [\t note…]`. `#` comments and blank lines are skipped.
    ///
    /// # Errors
    /// Fewer than 4 columns, a bad address/number/status, any [`Self::insert`] error.
    pub fn parse_tsv(text: &str) -> Result<Self> {
        let mut r = Self::default();
        for (ln, cols) in tsv_rows(text) {
            let parse = || -> Result<TokenEntry> {
                ensure!(cols.len() >= 4, "expected >= 4 tab-separated columns");
                Ok(TokenEntry {
                    token: parse_addr(cols[0])?,
                    decimals: dash_or(cols[1], |s| s.parse::<u8>().with_context(|| format!("decimals {s:?}")))?,
                    quote_rank: dash_or(cols[2], |s| s.parse::<u8>().with_context(|| format!("quote_rank {s:?}")))?,
                    status: parse_status(cols[3])?,
                })
            };
            let e = parse().with_context(|| format!("token registry line {ln}"))?;
            r.insert(e).with_context(|| format!("token registry line {ln}"))?;
        }
        Ok(r)
    }
}

/// Non-comment, non-blank lines as `(1-based line number, trimmed tab-separated columns)`.
fn tsv_rows(text: &str) -> impl Iterator<Item = (usize, Vec<&str>)> {
    text.lines().enumerate().filter_map(|(i, raw)| {
        let line = raw.trim();
        (!line.is_empty() && !line.starts_with('#')).then(|| (i + 1, line.split('\t').map(str::trim).collect()))
    })
}

/// `-` (or empty) = `None`.
fn dash_or<T>(s: &str, f: impl FnOnce(&str) -> Result<T>) -> Result<Option<T>> {
    match s {
        "-" | "" => Ok(None),
        s => f(s).map(Some),
    }
}

fn parse_status(s: &str) -> Result<RegistryStatus> {
    RegistryStatus::parse(s).with_context(|| format!("status {s:?} not allowed (verified|observed)"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, Bytes};
    use alloy_sol_types::SolEvent;

    const A: Address = address!("00000000000000000000000000000000000000aa");
    const B: Address = address!("00000000000000000000000000000000000000bb");
    const M: Address = address!("00000000000000000000000000000000000000ee");

    fn v3(pool: Address) -> PoolEntry {
        PoolEntry {
            pool: PoolRef::V3(pool),
            currency0: A,
            currency1: B,
            venue: Venue::UniV3,
            fee_pips: Some(3000),
            hooks: None,
            status: RegistryStatus::Observed,
            source: PoolSource::Registry,
        }
    }

    #[test]
    fn insert_checks_invariants() {
        let mut r = PoolRegistry::default();
        r.insert(v3(Address::with_last_byte(1))).unwrap();
        assert!(r.insert(v3(Address::with_last_byte(1))).is_err(), "duplicate");
        assert!(r.insert(PoolEntry { currency0: B, currency1: A, ..v3(Address::with_last_byte(2)) }).is_err());
        assert!(r.insert(PoolEntry { currency0: A, currency1: A, ..v3(Address::with_last_byte(2)) }).is_err());
        assert!(r.insert(PoolEntry { fee_pips: Some(1_000_001), ..v3(Address::with_last_byte(2)) }).is_err());
        assert!(r.insert(PoolEntry { hooks: Some(Address::ZERO), ..v3(Address::with_last_byte(2)) }).is_err());
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn pool_tsv() {
        let text = format!(
            "# venue emitter pool_id c0 c1 fee hooks status\n\n\
             uni_v3\t{p}\t-\t{A:#x}\t{B:#x}\t500\t-\tverified\tnote\n\
             pons_v1\t{M:#x}\t0x{id}\t{z:#x}\t{B:#x}\t-\t{z:#x}\tobserved\n",
            p = "0x00000000000000000000000000000000000000cc",
            id = "11".repeat(32),
            z = Address::ZERO,
        );
        let r = PoolRegistry::parse_tsv(&text).unwrap();
        assert_eq!(r.len(), 2);
        let e = r.get(&PoolRef::V3(address!("00000000000000000000000000000000000000cc"))).unwrap();
        assert_eq!((e.venue, e.fee_pips, e.status), (Venue::UniV3, Some(500), RegistryStatus::Verified));
        let e = r.get(&PoolRef::V4 { manager: M, id: B256::repeat_byte(0x11) }).unwrap();
        assert_eq!((e.currency0, e.hooks, e.fee_pips), (Address::ZERO, Some(Address::ZERO), None));
        let bad = |line: &str| PoolRegistry::parse_tsv(line).is_err();
        assert!(bad(&format!("uniswap\t{A:#x}\t-\t{A:#x}\t{B:#x}\t-\t-\tobserved")), "unknown venue");
        assert!(bad(&format!("uni_v3\t{A:#x}\t-\t{A:#x}\t{B:#x}\t-\t-\ttodo")), "todo status");
        assert!(bad(&format!("uni_v3\t{A:#x}\t-\t{A:#x}\t{B:#x}\t-\t-")), "7 columns");
        assert!(bad(&format!("uni_v3\t{A:#x}\t-\t{A:#x}\t{B:#x}\tx\t-\tobserved")), "fee");
        assert!(bad(&format!("uni_v3\t{A:#x}\t0x11\t{A:#x}\t{B:#x}\t-\t-\tobserved")), "short pool id");
    }

    #[test]
    fn token_tsv_and_builtin() {
        let b = TokenRegistry::builtin();
        assert_eq!(b.len(), 2);
        assert_eq!(b.get(&L2_WETH).unwrap().quote_rank, Some(ETH_QUOTE_RANK));
        assert_eq!(b.get(&Address::ZERO).unwrap().decimals, Some(18));
        let r = TokenRegistry::parse_tsv(&format!("{A:#x}\t6\t1\tobserved\n{B:#x}\t-\t-\tobserved\n")).unwrap();
        assert_eq!(r.get(&A).unwrap().quote_rank, Some(1));
        assert_eq!(r.get(&B).unwrap().decimals, None);
        assert!(TokenRegistry::parse_tsv(&format!("{A:#x}\t-\t1\tobserved")).is_err(), "quote without decimals");
        assert!(TokenRegistry::parse_tsv(&format!("{A:#x}\t78\t-\tobserved")).is_err(), "decimals");
        assert!(TokenRegistry::parse_tsv(&format!("{A:#x}\t6\t-\trejected")).is_err(), "status");
        let mut b = TokenRegistry::builtin();
        assert!(b.extend(TokenRegistry::builtin()).is_err(), "no override of built-ins");
    }

    fn initialize_log(emitter: Address, c0: Address, c1: Address, hooks: Address) -> Log {
        let ev = v4::Initialize {
            id: B256::repeat_byte(0x22),
            currency0: c0,
            currency1: c1,
            fee: alloy_primitives::aliases::U24::from(3000u32),
            tickSpacing: alloy_primitives::aliases::I24::try_from(60i32).unwrap(),
            hooks,
            sqrtPriceX96: alloy_primitives::aliases::U160::from(1u8),
            tick: alloy_primitives::aliases::I24::ZERO,
        };
        let data = ev.encode_log_data();
        Log { address: emitter, topics: data.topics().to_vec(), data: Bytes::from(data.data.to_vec()), index: 0 }
    }

    /// Synthetic: no v4 `Initialize` of the fixture blocks has a swap in the same fixture.
    #[test]
    fn initialize_only_from_allowed_manager() {
        let mut r = PoolRegistry::default();
        let log = initialize_log(M, Address::ZERO, B, Address::ZERO);
        assert_eq!(r.observe_log(&log), InitOutcome::ForeignEmitter);
        r.allow_v4_manager(M, RegistryStatus::Observed).unwrap();
        assert!(r.allow_v4_manager(M, RegistryStatus::Verified).is_err());
        assert_eq!(r.observe_log(&log), InitOutcome::Added);
        let e = r.get(&PoolRef::V4 { manager: M, id: B256::repeat_byte(0x22) }).unwrap();
        assert_eq!((e.venue, e.status, e.source), (Venue::UniV4, RegistryStatus::Observed, PoolSource::Initialize));
        assert_eq!(r.observe_log(&log), InitOutcome::Known);
        assert_eq!(r.observe_log(&initialize_log(M, A, B, Address::ZERO)), InitOutcome::Mismatch);
        let mut r2 = PoolRegistry::default();
        r2.allow_v4_manager(M, RegistryStatus::Observed).unwrap();
        assert_eq!(r2.observe_log(&initialize_log(M, A, B, M)), InitOutcome::Added);
        assert_eq!(r2.get(&PoolRef::V4 { manager: M, id: B256::repeat_byte(0x22) }).unwrap().venue, Venue::Other);
        assert_eq!(r2.observe_log(&initialize_log(M, B, A, M)), InitOutcome::Mismatch);
        let mut r3 = PoolRegistry::default();
        r3.allow_v4_manager(M, RegistryStatus::Observed).unwrap();
        assert_eq!(r3.observe_log(&initialize_log(M, B, A, M)), InitOutcome::Malformed, "currency0 > currency1");
        let mut short = initialize_log(M, A, B, Address::ZERO);
        short.data = Bytes::from(short.data[..32].to_vec());
        assert_eq!(r3.observe_log(&short), InitOutcome::Malformed);
        assert!(r3.is_empty());
        let mut other = initialize_log(M, A, B, Address::ZERO);
        other.topics[0] = B256::ZERO;
        assert_eq!(r.observe_log(&other), InitOutcome::NotInitialize);
    }
}
