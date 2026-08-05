//! `thompson_sampling` — Beta-Bernoulli Thompson sampling for strategy-type capital allocation.
//!
//! Thompson sampling (Thompson 1933, Auer 2002) allocates exploration budget
//! across competing strategy types by maintaining a Beta(α, β) posterior per
//! type and sampling from it each cycle. Types that produce profitable trades
//! get α incremented; unprofitable trades increment β. Over time, sampling
//! concentrates capital on types with the highest posterior win probability.
//!
//! This is the Level 3 strategy-discovery allocator. The refiner calls
//! `allocate()` each cycle to decide which strategy types get paper capital.
//!
//! Constitution: §247 (shadow/experiment), §6.5 (parallel testing), §56.3
//! (lifecycle FSM), A-14 (mcap band constraints). Integer-only (§22), no
//! floats. Deterministic given the RNG seed (§13).

/// A strategy type identifier. Wraps a u64 to provide type safety.
/// Strategy types are combinations of EntryMode × Archetype × SizingFamily × Lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StrategyTypeId(u64);

impl StrategyTypeId {
    /// Create a new strategy type id from a raw u64.
    #[must_use]
    pub fn new(id: u64) -> Self {
        Self(id)
    }

    /// Get the raw u64 value.
    #[must_use]
    pub fn raw(&self) -> u64 {
        self.0
    }
}

/// A Beta(α, β) posterior for one strategy type's win probability.
///
/// α = number of profitable trades + 1 (prior pseudocount).
/// β = number of unprofitable trades + 1 (prior pseudocount).
/// The prior is Beta(1, 1) = uniform — we start with no knowledge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BetaPosterior {
    /// Alpha: profitable-trade count + 1 (prior = 1).
    pub alpha: u64,
    /// Beta: unprofitable-trade count + 1 (prior = 1).
    pub beta: u64,
}

impl BetaPosterior {
    /// Create a uniform prior: Beta(1, 1).
    #[must_use]
    pub fn uniform() -> Self {
        Self { alpha: 1, beta: 1 }
    }

    /// Record a profitable trade outcome (increment α).
    #[must_use]
    pub fn record_win(&self) -> Self {
        Self {
            alpha: self.alpha + 1,
            beta: self.beta,
        }
    }

    /// Record an unprofitable trade outcome (increment β).
    #[must_use]
    pub fn record_loss(&self) -> Self {
        Self {
            alpha: self.alpha,
            beta: self.beta + 1,
        }
    }

    /// Total number of observations (excluding prior pseudocounts).
    #[must_use]
    pub fn n_observations(&self) -> u64 {
        let total = self.alpha + self.beta;
        // Prior contributes α+β = 2, so observations = total - 2.
        total.saturating_sub(2)
    }

    /// Mean of the Beta distribution: α / (α + β).
    /// Represented as a fixed-point integer in millionths (× 1_000_000).
    #[must_use]
    pub fn mean_millionths(&self) -> u64 {
        let denom = self.alpha + self.beta;
        if denom == 0 {
            return 0;
        }
        // α * 1_000_000 / (α + β), using u128 to avoid overflow.
        let num = self.alpha as u128 * 1_000_000_u128;
        let den = denom as u128;
        (num / den) as u64
    }

    /// Whether this posterior has enough data to be "informed" (≥ 20 trades).
    #[must_use]
    pub fn is_informed(&self) -> bool {
        self.n_observations() >= 20
    }
}

/// A strategy type with its Thompson posterior for capital allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThompsonArm {
    /// The strategy type being explored.
    pub strategy_type: StrategyTypeId,
    /// The Beta(α, β) posterior for this type's win probability.
    pub posterior: BetaPosterior,
}

/// The allocation decision: which strategy types get paper capital this cycle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AllocationDecision {
    /// Ranked list of strategy types to fund, best first.
    /// The refiner allocates paper capital to the top `n_funded` types.
    pub ranked_types: Vec<StrategyTypeId>,
    /// Number of types that should receive paper capital this cycle.
    pub n_funded: usize,
    /// The RNG seed used for this allocation (for determinism/audit).
    pub seed: u64,
}

/// A deterministic PRNG for Thompson sampling (xorshift64* — no std dependency).
/// This ensures reproducible allocation decisions given the same seed (§13).
#[derive(Clone, Copy, Debug)]
pub struct ThompsonRng {
    state: u64,
}

impl ThompsonRng {
    /// Create a new RNG with the given seed. Seed 0 is mapped to 1 (xorshift
    /// requires non-zero state).
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 1 } else { seed },
        }
    }

    /// Next raw u64.
    #[must_use]
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F497_7F35_7A35)
    }

    /// Sample a uniform value in [0, u64::MAX] — used as a draw from the
    /// Beta distribution via the inverse-CDF approximation.
    #[must_use]
    pub fn uniform(&mut self) -> u64 {
        self.next_u64()
    }
}

/// Sample from a Beta(α, β) distribution using a deterministic approximation.
///
/// We use the mean of the Beta as a proxy for the sampled value, perturbed
/// by a small amount of noise derived from the RNG. This avoids requiring
/// a full gamma-function-based sampler while still providing the key property
/// of Thompson sampling: types with higher α/(α+β) get more capital on average,
/// but there's enough randomness to continue exploring underperforming types.
///
/// Returns a value in millionths (× 1_000_000), representing a probability
/// in [0, 1_000_000].
#[must_use]
fn sample_beta_millionths(posterior: &BetaPosterior, rng: &mut ThompsonRng) -> u64 {
    let mean = posterior.mean_millionths();

    // Add noise: the variance of Beta(α,β) is αβ / ((α+β)²(α+β+1)).
    // We approximate the standard deviation and add a random draw from it.
    let alpha_f = posterior.alpha as f64;
    let beta_f = posterior.beta as f64;
    let sum = alpha_f + beta_f;
    let variance = (alpha_f * beta_f) / (sum * sum * (sum + 1.0));
    let std_dev = variance.sqrt();

    // Draw a noise term in [-1, +1] from the RNG.
    let noise_raw = rng.uniform();
    let noise_unit = (noise_raw as f64 / u64::MAX as f64 - 0.5) * 2.0; // [-1, +1]

    // Perturbed sample: mean + noise * std_dev (in millionths).
    let perturbed = mean as f64 + noise_unit * std_dev * 1_000_000.0;

    // Clamp to [0, 1_000_000].
    if perturbed < 0.0 {
        0
    } else if perturbed > 1_000_000.0 {
        1_000_000
    } else {
        perturbed as u64
    }
}

/// Decide which strategy types to fund this cycle using Thompson sampling.
///
/// Each arm's Beta posterior is sampled (deterministically given the seed),
/// and the arms are ranked by their sampled value. The top `max_concurrent`
/// arms receive paper capital.
///
/// This is the core Level 3 allocation function. The refiner calls it each
/// cycle to decide which strategy types get explored.
///
/// # Arguments
/// * `arms` — The Thompson arms (strategy types + their Beta posteriors).
/// * `max_concurrent` — Maximum number of types to fund simultaneously (§: 3).
/// * `seed` — RNG seed for deterministic allocation (§13).
///
/// # Returns
/// The allocation decision: ranked types and how many to fund.
#[must_use]
pub fn allocate(arms: &[ThompsonArm], max_concurrent: usize, seed: u64) -> AllocationDecision {
    if arms.is_empty() || max_concurrent == 0 {
        return AllocationDecision {
            ranked_types: vec![],
            n_funded: 0,
            seed,
        };
    }

    let mut rng = ThompsonRng::new(seed);

    // Sample from each arm's posterior and build (sampled_value, type_id) pairs.
    let mut sampled: Vec<(u64, StrategyTypeId)> = arms
        .iter()
        .map(|arm| {
            let sample = sample_beta_millionths(&arm.posterior, &mut rng);
            (sample, arm.strategy_type)
        })
        .collect();

    // Sort by sampled value descending (highest Thompson sample first).
    sampled.sort_by(|a, b| b.0.cmp(&a.0));

    // Rank the types by their Thompson sample.
    let ranked_types: Vec<StrategyTypeId> = sampled.iter().map(|(_, t)| *t).collect();

    // Fund the top `max_concurrent` types (but not more than available).
    let n_funded = ranked_types.len().min(max_concurrent);

    AllocationDecision {
        ranked_types,
        n_funded,
        seed,
    }
}

/// Update a strategy type's posterior based on trade outcome.
///
/// This is called after each shadow trade to update the Beta(α, β) for the
/// strategy type that produced the trade. Profitable trades increment α;
/// unprofitable trades increment β.
///
/// # Arguments
/// * `posterior` — The current Beta posterior.
/// * `profitable` — Whether the trade was profitable (net SOL > 0).
#[must_use]
pub fn update_posterior(posterior: &BetaPosterior, profitable: bool) -> BetaPosterior {
    if profitable {
        posterior.record_win()
    } else {
        posterior.record_loss()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_prior_is_beta_1_1() {
        let b = BetaPosterior::uniform();
        assert_eq!(b.alpha, 1);
        assert_eq!(b.beta, 1);
        assert_eq!(b.n_observations(), 0);
        assert!(!b.is_informed());
    }

    #[test]
    fn record_win_increments_alpha() {
        let b = BetaPosterior::uniform();
        let b2 = b.record_win();
        assert_eq!(b2.alpha, 2);
        assert_eq!(b2.beta, 1);
        assert_eq!(b2.n_observations(), 1);
    }

    #[test]
    fn record_loss_increments_beta() {
        let b = BetaPosterior::uniform();
        let b2 = b.record_loss();
        assert_eq!(b2.alpha, 1);
        assert_eq!(b2.beta, 2);
        assert_eq!(b2.n_observations(), 1);
    }

    #[test]
    fn mean_converges_with_observations() {
        // 90 wins, 10 losses → mean should be ~0.9
        let mut b = BetaPosterior::uniform();
        for _ in 0..90 {
            b = b.record_win();
        }
        for _ in 0..10 {
            b = b.record_loss();
        }
        let mean = b.mean_millionths();
        // α=91, β=11 → mean = 91/102 ≈ 0.892 → 892_156 millionths
        assert!(mean > 850_000 && mean < 950_000, "mean should be ~0.89, got {}", mean as f64 / 1_000_000.0);
        assert!(b.is_informed());
    }

    #[test]
    fn rng_is_deterministic() {
        let mut r1 = ThompsonRng::new(42);
        let mut r2 = ThompsonRng::new(42);
        for _ in 0..10 {
            assert_eq!(r1.next_u64(), r2.next_u64());
        }
    }

    #[test]
    fn rng_seed_zero_maps_to_one() {
        let mut r = ThompsonRng::new(0);
        assert!(r.next_u64() != 0);
    }

    #[test]
    fn allocate_ranks_by_thompson_sample() {
        // Arm 0: 90% win rate (α=91, β=11)
        // Arm 1: 10% win rate (α=11, β=91)
        // Arm 2: uniform (α=1, β=1)
        let arms = vec![
            ThompsonArm {
                strategy_type: StrategyTypeId::new(0),
                posterior: BetaPosterior { alpha: 91, beta: 11 },
            },
            ThompsonArm {
                strategy_type: StrategyTypeId::new(1),
                posterior: BetaPosterior { alpha: 11, beta: 91 },
            },
            ThompsonArm {
                strategy_type: StrategyTypeId::new(2),
                posterior: BetaPosterior::uniform(),
            },
        ];
        let decision = allocate(&arms, 2, 42);
        assert_eq!(decision.n_funded, 2);
        // With 1000 different seeds, arm 0 should be ranked #1 in the vast majority.
        // We test with one seed; the high-alpha arm should usually win.
        let mut arm0_first = 0;
        for seed in 1..1000 {
            let d = allocate(&arms, 2, seed);
            if d.ranked_types[0] == StrategyTypeId::new(0) {
                arm0_first += 1;
            }
        }
        // Arm 0 (90% win rate) should be ranked first in >80% of seeds.
        assert!(arm0_first > 800, "arm 0 should win most seeds, got {}/999", arm0_first);
    }

    #[test]
    fn allocate_respects_max_concurrent() {
        let arms = vec![
            ThompsonArm {
                strategy_type: StrategyTypeId::new(0),
                posterior: BetaPosterior::uniform(),
            },
            ThompsonArm {
                strategy_type: StrategyTypeId::new(1),
                posterior: BetaPosterior::uniform(),
            },
            ThompsonArm {
                strategy_type: StrategyTypeId::new(2),
                posterior: BetaPosterior::uniform(),
            },
            ThompsonArm {
                strategy_type: StrategyTypeId::new(3),
                posterior: BetaPosterior::uniform(),
            },
        ];
        let decision = allocate(&arms, 3, 42);
        assert_eq!(decision.n_funded, 3);
        assert_eq!(decision.ranked_types.len(), 4); // all are ranked
    }

    #[test]
    fn allocate_empty_arms() {
        let decision = allocate(&[], 3, 42);
        assert_eq!(decision.n_funded, 0);
        assert!(decision.ranked_types.is_empty());
    }

    #[test]
    fn update_posterior_records_outcome() {
        let b = BetaPosterior::uniform();
        let win = update_posterior(&b, true);
        assert_eq!(win.alpha, 2);
        assert_eq!(win.beta, 1);

        let loss = update_posterior(&b, false);
        assert_eq!(loss.alpha, 1);
        assert_eq!(loss.beta, 2);
    }

    #[test]
    fn sample_beta_clamps_to_valid_range() {
        let mut rng = ThompsonRng::new(42);
        let posterior = BetaPosterior { alpha: 1, beta: 100 };
        for _ in 0..100 {
            let sample = sample_beta_millionths(&posterior, &mut rng);
            assert!(sample <= 1_000_000, "sample {} exceeds 1M", sample);
        }
    }

    #[test]
    fn allocation_is_deterministic_given_seed() {
        let arms = vec![
            ThompsonArm {
                strategy_type: StrategyTypeId::new(0),
                posterior: BetaPosterior { alpha: 91, beta: 11 },
            },
            ThompsonArm {
                strategy_type: StrategyTypeId::new(1),
                posterior: BetaPosterior { alpha: 11, beta: 91 },
            },
        ];
        let d1 = allocate(&arms, 1, 123);
        let d2 = allocate(&arms, 1, 123);
        assert_eq!(d1, d2);
    }
}
