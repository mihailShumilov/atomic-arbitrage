//! Jitter for retry pauses, shared by the recorder (reconnect ladder) and the
//! enricher (RPC backoff). Task 025 item 3:
//! before it each binary had its own generator (recorder: splitmix64 of the
//! clock on every call, enricher: xorshift64 seeded from the clock).
//!
//! Not for anything that needs unpredictability: the only job is to spread
//! retries so that restarts and parallel tasks do not hit the server in
//! lockstep. Only std, no RNG dependency.
//!
//! The generator is splitmix64 (Steele, Lea, Flood 2014; the reference
//! `splitmix64.c` by S. Vigna): a Weyl sequence `state += GAMMA` passed
//! through a 64-bit finaliser. The API is one function, [`rand01`]: one
//! stream per process, seeded from the clock and the process id. Callers
//! that need fixed values in tests take the jitter as a parameter (as the
//! recorder's reconnect ladder does) instead of seeding a generator.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// Weyl increment of splitmix64 (2^64 / golden ratio, odd).
const GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

/// The splitmix64 output function.
const fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The top 53 bits as a float in `[0, 1)` (every value exactly representable).
fn to_unit(z: u64) -> f64 {
    (z >> 11) as f64 / (1u64 << 53) as f64
}

/// `n`-th value (0-based) of the stream that starts at `seed`, without
/// walking it: the state after `n + 1` steps is `seed + (n + 1) * GAMMA`.
const fn nth(seed: u64, n: u64) -> u64 {
    mix(seed.wrapping_add(n.wrapping_add(1).wrapping_mul(GAMMA)))
}

fn process_seed() -> u64 {
    let ns = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    // The pid separates two processes started within the same clock tick.
    ns ^ u64::from(std::process::id()).rotate_left(32)
}

static SEED: OnceLock<u64> = OnceLock::new();
static CALLS: AtomicU64 = AtomicU64::new(0);

/// Jitter value in `[0, 1)` from the process-wide stream. Thread-safe: every
/// call takes its own position in the stream (an atomic counter), so
/// concurrent callers get different positions (distinct 64-bit values; the
/// `f64` keeps 53 bits, so equal floats are possible, if vanishingly rare).
pub fn rand01() -> f64 {
    let seed = *SEED.get_or_init(process_seed);
    to_unit(nth(seed, CALLS.fetch_add(1, Ordering::Relaxed)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Step-by-step splitmix64 (as in `splitmix64.c`), the reference for `nth`.
    struct SplitMix64 {
        state: u64,
    }

    impl SplitMix64 {
        /// Stream starting from `seed` (any value, zero included).
        const fn new(seed: u64) -> Self {
            Self { state: seed }
        }

        /// Next 64-bit value.
        fn next_u64(&mut self) -> u64 {
            self.state = self.state.wrapping_add(GAMMA);
            mix(self.state)
        }

        /// Next value in `[0, 1)`.
        fn next_f64(&mut self) -> f64 {
            to_unit(self.next_u64())
        }
    }

    /// Reference values of `splitmix64.c` for seed 1234567.
    #[test]
    fn matches_reference_splitmix64() {
        let mut g = SplitMix64::new(1_234_567);
        let got: Vec<u64> = (0..3).map(|_| g.next_u64()).collect();
        assert_eq!(got, vec![6_457_827_717_110_365_317, 3_203_168_211_198_807_973, 9_817_491_932_198_370_423]);
    }

    #[test]
    fn nth_equals_walking_the_stream() {
        for seed in [0, 1, 1_234_567, u64::MAX] {
            let mut g = SplitMix64::new(seed);
            for n in 0..50 {
                assert_eq!(nth(seed, n), g.next_u64(), "seed {seed} n {n}");
            }
        }
    }

    /// The recorder's generator before task 025 was `nth(now_ns, 0)`: the
    /// output function is the same.
    #[test]
    fn same_finaliser_as_the_old_recorder_jitter() {
        let n: u64 = 1_790_837_152_631_000_000;
        let mut z = n.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        assert_eq!(nth(n, 0), z);
    }

    #[test]
    fn unit_values_stay_in_range_and_spread() {
        assert_eq!(to_unit(0), 0.0);
        assert!(to_unit(u64::MAX) < 1.0);
        let mut g = SplitMix64::new(42);
        let v: Vec<f64> = (0..10_000).map(|_| g.next_f64()).collect();
        assert!(v.iter().all(|x| (0.0..1.0).contains(x)));
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        assert!((mean - 0.5).abs() < 0.02, "mean {mean}");
        // Every tenth of the interval is hit.
        for k in 0..10 {
            let lo = f64::from(k) / 10.0;
            assert!(v.iter().any(|x| (lo..lo + 0.1).contains(x)), "no value in [{lo}, {})", lo + 0.1);
        }
    }

    #[test]
    fn process_stream_gives_distinct_values_across_threads() {
        let handles: Vec<_> =
            (0..4).map(|_| std::thread::spawn(|| (0..250).map(|_| rand01()).collect::<Vec<_>>())).collect();
        let mut all: Vec<u64> = handles.into_iter().flat_map(|h| h.join().unwrap()).map(f64::to_bits).collect();
        assert!(all.iter().all(|&b| (0.0..1.0).contains(&f64::from_bits(b))));
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), 1000);
    }
}
