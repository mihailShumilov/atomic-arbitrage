//! Decoded swap ([`PoolSwap`]) + pool/token registries ([`crate::pools`]) -> `hood.swaps` row
//! ([`SwapRow`]). Pure: no IO, no network.
//!
//! Rules (references/data-model.md, "hood.swaps"; reviews 022 З2/З3):
//! - **No row without a registered pool.** The registry is the emitter filter: v3 rows only for
//!   registered pool addresses, v4 rows only for pools of a registered (or allowed) `PoolManager`.
//!   A row is built only if the pool entry and the quote entry are both at least `min_status`.
//! - **Quote**: the currency of the pool that is a quote in the token registry; two quotes -> the
//!   lower `quote_rank`; none, or two of the same rank -> no row.
//! - **Signs**: [`PoolSwap`] amounts are pool-side (positive = into the pool, v4 already
//!   normalized). Token out of the pool = the trader bought it (`buy`); amounts are written
//!   unsigned.
//! - **Zero amounts** (20 swaps on data/ in review 022: v4 `[0, 0]` x16, `±1` legs, one v3
//!   `[0, 3709]`): no row (`zero_amount`); a price is never a division by zero. Both amounts with
//!   the same sign: no row (`same_sign`).
//! - **Price** = quote per whole token; `nan` when the token decimals are unknown.
//! - **Fee**: `fee_quote = quote_amount * fee_pips / 1e6`; v4 `fee_pips` from the event (0 in a
//!   third of v4 swaps on data/: the event says 0, written as 0; a hook may still charge its own
//!   fee, which is not included); v3 from the pool entry, `nan` if unknown. The pool fee is charged
//!   on the input currency; for a sell it is valued at the swap's own price, so the formula is
//!   the same for both sides (approximation: rounding inside the pool is ignored).
//! - **v4 with hooks**: `Swap` is emitted before `afterSwap`, so amounts are the AMM leg, not
//!   necessarily what the trader paid/received (hook deltas are not in the event). Not a column;
//!   counted in [`SwapRowCounters::v4_rows_with_hooks`] when the pool's hooks are known.

use std::collections::BTreeMap;

use alloy_primitives::{Address, U256};

use crate::pools::{PoolEntry, PoolRef, PoolRegistry, TokenRegistry};
use crate::registry::RegistryStatus;
use crate::rows::{Side, SwapPool, SwapRow, Venue};
use crate::swaps::{PoolSwap, SwapEvent};

/// Why a swap has no `hood.swaps` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SkipReason {
    /// Pool (emitter, v4 id) not in the pool registry.
    NoPoolMeta,
    /// Pool or quote entry below the requested minimum status.
    BelowMinStatus,
    /// Neither currency of the pool is a quote.
    NoQuote,
    /// Both currencies are quotes of the same rank.
    AmbiguousQuote,
    /// The token or the quote amount is zero.
    ZeroAmount,
    /// Both amounts have the same sign (both into or both out of the pool).
    SameSign,
}

impl SkipReason {
    /// Every variant, in report order.
    pub const ALL: [Self; 6] =
        [Self::NoPoolMeta, Self::BelowMinStatus, Self::NoQuote, Self::AmbiguousQuote, Self::ZeroAmount, Self::SameSign];

    /// Name for reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoPoolMeta => "no_pool_meta",
            Self::BelowMinStatus => "below_min_status",
            Self::NoQuote => "no_quote",
            Self::AmbiguousQuote => "ambiguous_quote",
            Self::ZeroAmount => "zero_amount",
            Self::SameSign => "same_sign",
        }
    }
}

/// Registries and policy of the mapping.
#[derive(Debug, Clone, Copy)]
pub struct RowInputs<'a> {
    pub pools: &'a PoolRegistry,
    pub tokens: &'a TokenRegistry,
    /// Rows only from pool and quote entries with at least this status (`Verified` for final
    /// analytics, `Observed` for research).
    pub min_status: RegistryStatus,
}

/// A built row and the pool entry it comes from.
#[derive(Debug, Clone)]
pub struct MappedSwap<'a> {
    pub row: SwapRow,
    pub pool: &'a PoolEntry,
}

/// Maps one swap (see the module doc for the rules).
///
/// # Errors
/// The [`SkipReason`] when the swap has no row.
///
/// # Panics
/// Never: the `expect`s guard invariants of [`TokenRegistry::insert`] (a quote has decimals).
pub fn swap_row<'a>(s: &PoolSwap, inputs: &RowInputs<'a>) -> Result<MappedSwap<'a>, SkipReason> {
    let pool_ref = PoolRef::of(s).ok_or(SkipReason::NoPoolMeta)?;
    let pool = inputs.pools.get(&pool_ref).ok_or(SkipReason::NoPoolMeta)?;
    let quote_of = |c: &Address| inputs.tokens.get(c).filter(|t| t.quote_rank.is_some());
    // `quote_is_1`: currency1 is the quote.
    let quote_is_1 = match (quote_of(&pool.currency0), quote_of(&pool.currency1)) {
        (None, None) => return Err(SkipReason::NoQuote),
        (Some(_), None) => false,
        (None, Some(_)) => true,
        (Some(q0), Some(q1)) if q0.quote_rank == q1.quote_rank => return Err(SkipReason::AmbiguousQuote),
        (Some(q0), Some(q1)) => q1.quote_rank < q0.quote_rank,
    };
    let (token, quote, token_delta, quote_delta) = if quote_is_1 {
        (pool.currency0, pool.currency1, s.amount0, s.amount1)
    } else {
        (pool.currency1, pool.currency0, s.amount1, s.amount0)
    };
    let quote_entry = inputs.tokens.get(&quote).expect("the quote was found in the token registry above");
    if pool.status.min(quote_entry.status) < inputs.min_status {
        return Err(SkipReason::BelowMinStatus);
    }
    if token_delta.is_zero() || quote_delta.is_zero() {
        return Err(SkipReason::ZeroAmount);
    }
    if token_delta.is_negative() == quote_delta.is_negative() {
        return Err(SkipReason::SameSign);
    }
    let side = if token_delta.is_negative() { Side::Buy } else { Side::Sell };
    let token_amount_raw = token_delta.unsigned_abs();
    let quote_amount_raw = quote_delta.unsigned_abs();
    let quote_decimals = quote_entry.decimals.expect("TokenRegistry::insert requires decimals for a quote");
    let quote_amount = scaled(quote_amount_raw, quote_decimals);
    let token_decimals = inputs.tokens.get(&token).filter(|t| t.status >= inputs.min_status).and_then(|t| t.decimals);
    // Both amounts are non-zero and decimals <= MAX_DECIMALS: finite and > 0, no division by 0.
    let price = token_decimals.map_or(f64::NAN, |d| quote_amount / scaled(token_amount_raw, d));
    let fee_pips = match s.event {
        SwapEvent::V4 => s.fee_pips,
        SwapEvent::V3 => pool.fee_pips,
    };
    let fee_quote = fee_pips.map_or(f64::NAN, |f| quote_amount * f64::from(f) / 1e6);
    let row = SwapRow {
        block_number: s.block_number,
        tx_index: s.tx_index,
        log_index: s.log_index,
        venue: pool.venue,
        pool: match pool_ref {
            PoolRef::V3(a) => SwapPool::V3(a),
            PoolRef::V4 { id, .. } => SwapPool::V4(id),
        },
        token,
        quote,
        trader: s.trader,
        router: s.router,
        side,
        token_amount_raw,
        quote_amount_raw,
        quote_amount,
        price,
        fee_quote,
    };
    Ok(MappedSwap { row, pool })
}

/// `raw / 10^decimals` as the nearest `f64` (one correctly rounded decimal-to-binary conversion).
fn scaled(raw: U256, decimals: u8) -> f64 {
    format!("{raw}e-{decimals}").parse().expect("`<decimal digits>e-<n>` is a valid f64 literal")
}

/// Counters of the mapping. Invariant: `Σ swaps = Σ rows + Σ skipped` (every decoded swap is
/// either a row or a counted skip).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SwapRowCounters {
    /// Decoded swaps given to [`Self::record`], per event.
    pub swaps: BTreeMap<SwapEvent, u64>,
    /// Rows per venue.
    pub rows: BTreeMap<Venue, u64>,
    /// Skips per event and reason.
    pub skipped: BTreeMap<(SwapEvent, SkipReason), u64>,
    /// Rows with `price = nan` (token decimals unknown).
    pub price_unknown: u64,
    /// Rows with `fee_quote = nan` (v3 fee tier unknown).
    pub fee_unknown: u64,
    /// v4 rows whose event fee is 0.
    pub v4_fee_zero: u64,
    /// v4 rows from pools with non-zero hooks (amounts = AMM leg, see the module doc).
    pub v4_rows_with_hooks: u64,
    /// v4 rows from pools whose hooks are unknown (registry TSV with `-`).
    pub v4_rows_hooks_unknown: u64,
}

impl SwapRowCounters {
    /// Counts the outcome of [`swap_row`] for `s`.
    pub fn record(&mut self, s: &PoolSwap, outcome: &Result<MappedSwap<'_>, SkipReason>) {
        let ev = s.event;
        *self.swaps.entry(ev).or_default() += 1;
        match outcome {
            Err(reason) => *self.skipped.entry((ev, *reason)).or_default() += 1,
            Ok(m) => {
                *self.rows.entry(m.row.venue).or_default() += 1;
                self.price_unknown += u64::from(m.row.price.is_nan());
                self.fee_unknown += u64::from(m.row.fee_quote.is_nan());
                if ev == SwapEvent::V4 {
                    self.v4_fee_zero += u64::from(s.fee_pips == Some(0));
                    match m.pool.hooks {
                        None => self.v4_rows_hooks_unknown += 1,
                        Some(h) => self.v4_rows_with_hooks += u64::from(h != Address::ZERO),
                    }
                }
            }
        }
    }

    /// Adds `o` to `self`.
    pub fn merge(&mut self, o: &SwapRowCounters) {
        let SwapRowCounters {
            swaps,
            rows,
            skipped,
            price_unknown,
            fee_unknown,
            v4_fee_zero,
            v4_rows_with_hooks,
            v4_rows_hooks_unknown,
        } = o;
        for (k, v) in swaps {
            *self.swaps.entry(*k).or_default() += v;
        }
        for (k, v) in rows {
            *self.rows.entry(*k).or_default() += v;
        }
        for (k, v) in skipped {
            *self.skipped.entry(*k).or_default() += v;
        }
        self.price_unknown += price_unknown;
        self.fee_unknown += fee_unknown;
        self.v4_fee_zero += v4_fee_zero;
        self.v4_rows_with_hooks += v4_rows_with_hooks;
        self.v4_rows_hooks_unknown += v4_rows_hooks_unknown;
    }

    /// `Σ swaps == Σ rows + Σ skipped`.
    #[must_use]
    pub fn is_balanced(&self) -> bool {
        let sum = |m: &mut dyn Iterator<Item = &u64>| m.sum::<u64>();
        sum(&mut self.swaps.values()) == sum(&mut self.rows.values()) + sum(&mut self.skipped.values())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pools::{PoolSource, TokenEntry};
    use alloy_primitives::{address, B256, I256};

    const T: Address = address!("00000000000000000000000000000000000000aa");
    const Q: Address = address!("00000000000000000000000000000000000000bb");
    const S: Address = address!("00000000000000000000000000000000000000cc");
    const POOL: Address = address!("00000000000000000000000000000000000000dd");

    fn i(v: i64) -> I256 {
        I256::try_from(v).unwrap()
    }

    fn swap(amount0: i64, amount1: i64) -> PoolSwap {
        PoolSwap {
            block_number: 10,
            tx_index: 1,
            log_index: 2,
            tx_hash: B256::ZERO,
            trader: address!("0000000000000000000000000000000000000001"),
            router: None,
            event: SwapEvent::V3,
            pool: POOL,
            pool_id: None,
            sender: Address::ZERO,
            amount0: i(amount0),
            amount1: i(amount1),
            sqrt_price_x96: U256::ZERO,
            liquidity: 0,
            tick: 0,
            fee_pips: None,
        }
    }

    /// Pool T (currency0) / Q (currency1); Q is a quote with 6 decimals, T has 18.
    fn registries(pool_status: RegistryStatus, fee: Option<u32>) -> (PoolRegistry, TokenRegistry) {
        let mut p = PoolRegistry::default();
        p.insert(PoolEntry {
            pool: PoolRef::V3(POOL),
            currency0: T,
            currency1: Q,
            venue: Venue::PonsV1,
            fee_pips: fee,
            hooks: None,
            status: pool_status,
            source: PoolSource::Registry,
        })
        .unwrap();
        let mut t = TokenRegistry::default();
        t.insert(TokenEntry { token: Q, decimals: Some(6), quote_rank: Some(1), status: RegistryStatus::Verified })
            .unwrap();
        t.insert(TokenEntry { token: T, decimals: Some(18), quote_rank: None, status: RegistryStatus::Verified })
            .unwrap();
        (p, t)
    }

    fn map(s: &PoolSwap, p: &PoolRegistry, t: &TokenRegistry, min: RegistryStatus) -> Result<SwapRow, SkipReason> {
        swap_row(s, &RowInputs { pools: p, tokens: t, min_status: min }).map(|m| m.row)
    }

    #[test]
    fn buy_and_sell_from_pool_side_signs() {
        let (p, t) = registries(RegistryStatus::Verified, Some(10_000));
        // 2 tokens left the pool, 3 USDG came in: the trader bought at 1.5 USDG per token.
        let r = map(&swap(-2_000_000_000_000_000_000, 3_000_000), &p, &t, RegistryStatus::Verified).unwrap();
        assert_eq!((r.side, r.token, r.quote, r.venue), (Side::Buy, T, Q, Venue::PonsV1));
        assert_eq!(
            (r.token_amount_raw, r.quote_amount_raw),
            (U256::from(2_000_000_000_000_000_000u64), U256::from(3_000_000u64))
        );
        assert_eq!((r.quote_amount, r.price, r.fee_quote), (3.0, 1.5, 0.03));
        assert_eq!(r.pool, SwapPool::V3(POOL));
        let r = map(&swap(4_000_000_000_000_000_000, -2_000_000), &p, &t, RegistryStatus::Verified).unwrap();
        assert_eq!((r.side, r.price), (Side::Sell, 0.5));
    }

    #[test]
    fn zero_and_same_sign_amounts_have_no_row() {
        let (p, t) = registries(RegistryStatus::Verified, None);
        let m = |a0, a1| map(&swap(a0, a1), &p, &t, RegistryStatus::Verified).map(|_| ());
        assert_eq!(m(0, 0), Err(SkipReason::ZeroAmount));
        assert_eq!(m(0, 3709), Err(SkipReason::ZeroAmount)); // the v3 case of review 022
        assert_eq!(m(-1, 0), Err(SkipReason::ZeroAmount));
        assert_eq!(m(5, 5), Err(SkipReason::SameSign));
        assert_eq!(m(-5, -5), Err(SkipReason::SameSign));
    }

    #[test]
    fn unknown_fee_and_decimals_are_nan_not_zero() {
        let (p, mut t) = registries(RegistryStatus::Verified, None);
        let r = map(&swap(-1, 1), &p, &t, RegistryStatus::Verified).unwrap();
        assert!(r.fee_quote.is_nan() && r.price.is_finite());
        t = {
            let mut t2 = TokenRegistry::default();
            t2.insert(TokenEntry {
                token: Q,
                decimals: Some(6),
                quote_rank: Some(1),
                status: RegistryStatus::Verified,
            })
            .unwrap();
            t2
        };
        let r = map(&swap(-1, 1), &p, &t, RegistryStatus::Verified).unwrap();
        assert!(r.price.is_nan());
        assert_eq!(r.quote_amount, 1e-6);
        // Token decimals below min_status are treated as unknown.
        t.insert(TokenEntry { token: T, decimals: Some(18), quote_rank: None, status: RegistryStatus::Observed })
            .unwrap();
        assert!(map(&swap(-1, 1), &p, &t, RegistryStatus::Verified).unwrap().price.is_nan());
        assert!(map(&swap(-1, 1), &p, &t, RegistryStatus::Observed).unwrap().price.is_finite());
    }

    #[test]
    fn extreme_amounts_stay_finite() {
        let (p, mut t) = registries(RegistryStatus::Verified, Some(1_000_000));
        let mut s = swap(0, 0);
        s.amount0 = I256::MIN + I256::ONE; // max |token|
        s.amount1 = I256::ONE;
        let r = map(&s, &p, &t, RegistryStatus::Verified).unwrap();
        assert!(r.price > 0.0 && r.price.is_finite() && r.fee_quote.is_finite());
        t = TokenRegistry::default();
        t.insert(TokenEntry { token: Q, decimals: Some(0), quote_rank: Some(1), status: RegistryStatus::Verified })
            .unwrap();
        t.insert(TokenEntry { token: T, decimals: Some(77), quote_rank: None, status: RegistryStatus::Verified })
            .unwrap();
        s.amount0 = -I256::ONE;
        s.amount1 = I256::MAX;
        let r = map(&s, &p, &t, RegistryStatus::Verified).unwrap();
        assert!(r.price.is_finite() && r.quote_amount.is_finite());
    }

    #[test]
    fn status_quote_and_pool_rules() {
        let (p, t) = registries(RegistryStatus::Observed, None);
        assert_eq!(map(&swap(-1, 1), &p, &t, RegistryStatus::Verified).map(|_| ()), Err(SkipReason::BelowMinStatus));
        assert!(map(&swap(-1, 1), &p, &t, RegistryStatus::Observed).is_ok());
        let unknown = PoolSwap { pool: S, ..swap(-1, 1) };
        assert_eq!(map(&unknown, &p, &t, RegistryStatus::Observed).map(|_| ()), Err(SkipReason::NoPoolMeta));
        let v4_without_id = PoolSwap { event: SwapEvent::V4, ..swap(-1, 1) };
        assert_eq!(map(&v4_without_id, &p, &t, RegistryStatus::Observed).map(|_| ()), Err(SkipReason::NoPoolMeta));
        // A v4 swap from the same address is a different pool key: no row.
        let v4 = PoolSwap { event: SwapEvent::V4, pool_id: Some(B256::ZERO), ..swap(-1, 1) };
        assert_eq!(map(&v4, &p, &t, RegistryStatus::Observed).map(|_| ()), Err(SkipReason::NoPoolMeta));
        let empty = TokenRegistry::default();
        assert_eq!(map(&swap(-1, 1), &p, &empty, RegistryStatus::Observed).map(|_| ()), Err(SkipReason::NoQuote));
        let mut both = TokenRegistry::default();
        for token in [T, Q] {
            both.insert(TokenEntry {
                token,
                decimals: Some(18),
                quote_rank: Some(2),
                status: RegistryStatus::Verified,
            })
            .unwrap();
        }
        assert_eq!(map(&swap(-1, 1), &p, &both, RegistryStatus::Observed).map(|_| ()), Err(SkipReason::AmbiguousQuote));
        // Two quotes of different ranks: the lower rank (here currency0 = T) is the quote.
        let mut ranked = TokenRegistry::default();
        ranked
            .insert(TokenEntry { token: T, decimals: Some(6), quote_rank: Some(1), status: RegistryStatus::Verified })
            .unwrap();
        ranked
            .insert(TokenEntry { token: Q, decimals: Some(18), quote_rank: Some(2), status: RegistryStatus::Verified })
            .unwrap();
        let r = map(&swap(-1, 1), &p, &ranked, RegistryStatus::Observed).unwrap();
        assert_eq!((r.token, r.quote, r.side), (Q, T, Side::Sell));
    }

    #[test]
    fn counters_balance() {
        let (p, t) = registries(RegistryStatus::Verified, None);
        let inputs = RowInputs { pools: &p, tokens: &t, min_status: RegistryStatus::Verified };
        let mut c = SwapRowCounters::default();
        for s in [swap(-1, 1), swap(0, 0), PoolSwap { pool: S, ..swap(-1, 1) }] {
            let out = swap_row(&s, &inputs);
            c.record(&s, &out);
        }
        assert_eq!(c.rows.get(&Venue::PonsV1), Some(&1));
        assert_eq!(c.skipped.get(&(SwapEvent::V3, SkipReason::ZeroAmount)), Some(&1));
        assert_eq!(c.skipped.get(&(SwapEvent::V3, SkipReason::NoPoolMeta)), Some(&1));
        assert_eq!(c.fee_unknown, 1);
        assert!(c.is_balanced());
        let mut total = SwapRowCounters::default();
        total.merge(&c);
        total.merge(&c);
        assert_eq!(total.swaps.get(&SwapEvent::V3), Some(&6));
        assert!(total.is_balanced());
    }
}
