//! `reflection_cadence` — per-mint terminal-state reflection cadence
//! (constitution §47a).
//!
//! [`crate::evaluator_stats::label_terminal`] answers "is this token dead?" for a
//! single swap series and a single `(delta_t, window_end)` parameterization. §47a
//! asks for that judgement to be applied on a *cadence*, per mint, under a
//! **versioned** δT criterion, so that a fleet of mints can be reflected in one
//! deterministic pass and every resulting label carries the exact criterion
//! version that produced it. This module is that ergonomic wrapper — it adds no
//! new statistics, it packages the existing leaf behind a clean per-mint API.
//!
//! Pure and deterministic (§22): the wrapper only forwards to `label_terminal`
//! and sorts output by mint id. No floats, no wall-clock, no RNG.

use crate::evaluator_stats::{label_terminal, TerminalLabel};

/// Stable per-mint identifier. Ordering drives deterministic output order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MintId(pub u64);

/// A versioned δT reflection criterion (§47a).
///
/// The `version` travels with every label so that a label produced under one
/// criterion is never silently conflated with one produced under another — a
/// mint reflected "dead" at `version` 1 does not retroactively re-grade when the
/// cadence's δT is later revised to `version` 2.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReflectionCadence {
    /// Inactivity gap, ns, that marks a mint terminal (the δT criterion).
    pub delta_t_ns: u64,
    /// Monotonic criterion version this cadence represents.
    pub version: u32,
}

impl ReflectionCadence {
    /// Construct a cadence.
    pub fn new(delta_t_ns: u64, version: u32) -> Self {
        ReflectionCadence {
            delta_t_ns,
            version,
        }
    }

    /// Reflect a single mint's swap series against this cadence (§47a).
    ///
    /// Forwards to [`label_terminal`] with the cadence's δT and the supplied
    /// window end, then stamps the criterion version onto the result. Pure.
    pub fn reflect(&self, mint: MintId, swap_ts_ns: &[u64], window_end_ns: u64) -> MintReflection {
        let label = label_terminal(swap_ts_ns, window_end_ns, self.delta_t_ns);
        MintReflection {
            mint,
            label,
            criterion_version: self.version,
        }
    }
}

/// One mint's terminal-state reflection, tagged with the criterion version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MintReflection {
    /// The mint reflected.
    pub mint: MintId,
    /// The versioned terminal-state label from `label_terminal`.
    pub label: TerminalLabel,
    /// The [`ReflectionCadence::version`] under which this label was produced.
    pub criterion_version: u32,
}

impl MintReflection {
    /// True iff this mint was judged terminal (dead) under the cadence.
    pub fn is_dead(&self) -> bool {
        self.label.dead
    }
}

/// One mint's input for a batch reflection pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MintSwaps {
    /// The mint.
    pub mint: MintId,
    /// Non-decreasing swap timestamps, ns.
    pub swap_ts_ns: Vec<u64>,
    /// The reflection window end, ns.
    pub window_end_ns: u64,
}

/// Reflect a whole fleet of mints in one deterministic pass (§47a).
///
/// Each mint is reflected under `cadence`; output is sorted by [`MintId`] so the
/// result order is a deterministic function of the mint ids regardless of input
/// order. Pure.
pub fn reflect_mints(cadence: ReflectionCadence, mints: &[MintSwaps]) -> Vec<MintReflection> {
    let mut out: Vec<MintReflection> = mints
        .iter()
        .map(|m| cadence.reflect(m.mint, &m.swap_ts_ns, m.window_end_ns))
        .collect();
    out.sort_by_key(|r| r.mint);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dead_mint_labeled_with_version() {
        let cadence = ReflectionCadence::new(1_000, 7);
        // swaps at 0 and 100, window_end 5000 -> trailing gap 4900 >= 1000 -> dead.
        let r = cadence.reflect(MintId(1), &[0, 100], 5_000);
        assert!(r.is_dead());
        assert_eq!(r.criterion_version, 7);
        assert_eq!(r.label.params_version, (1_000, 5_000));
    }

    #[test]
    fn live_mint_not_dead() {
        let cadence = ReflectionCadence::new(10_000, 1);
        // dense swaps, small trailing gap -> not dead.
        let r = cadence.reflect(MintId(2), &[0, 100, 200, 300], 400);
        assert!(!r.is_dead());
        assert_eq!(r.label.died_at_ns, None);
    }

    #[test]
    fn batch_is_sorted_by_mint() {
        let cadence = ReflectionCadence::new(1_000, 3);
        let mints = vec![
            MintSwaps {
                mint: MintId(5),
                swap_ts_ns: vec![0],
                window_end_ns: 5_000,
            },
            MintSwaps {
                mint: MintId(1),
                swap_ts_ns: vec![0, 10],
                window_end_ns: 20,
            },
        ];
        let out = reflect_mints(cadence, &mints);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].mint, MintId(1));
        assert_eq!(out[1].mint, MintId(5));
    }

    #[test]
    fn version_pins_the_criterion() {
        // Same swaps, two versions -> distinct labels by version tag.
        let c1 = ReflectionCadence::new(1_000, 1);
        let c2 = ReflectionCadence::new(50, 2);
        let a = c1.reflect(MintId(9), &[0, 100], 200);
        let b = c2.reflect(MintId(9), &[0, 100], 200);
        // Under c1 (dt=1000) the 100-gap is alive; under c2 (dt=50) it is dead.
        assert!(!a.is_dead());
        assert!(b.is_dead());
        assert_eq!(a.criterion_version, 1);
        assert_eq!(b.criterion_version, 2);
    }

    #[test]
    fn empty_fleet_is_empty() {
        let cadence = ReflectionCadence::new(1_000, 1);
        assert!(reflect_mints(cadence, &[]).is_empty());
    }

    #[test]
    fn deterministic_repeat() {
        let cadence = ReflectionCadence::new(1_000, 4);
        let mints = vec![MintSwaps {
            mint: MintId(3),
            swap_ts_ns: vec![0, 2_000],
            window_end_ns: 2_000,
        }];
        assert_eq!(
            reflect_mints(cadence, &mints),
            reflect_mints(cadence, &mints)
        );
    }
}
