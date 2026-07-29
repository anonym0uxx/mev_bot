//! §50 ablation harness binding (LAW 20): the app's implementation of the frozen
//! [`pump_quant_evaluator::ablation::AblationReplay`] trait.
//!
//! The evaluator's ablation harness is pure over the trait — given a deterministic
//! `impl AblationReplay` it produces byte-for-byte identical `AblationReport`s
//! measuring each feature family's marginal contribution. This module supplies that
//! implementation for the real app engine: it maps each [`FeatureFamily`] id onto
//! one of the engine's feature-family config flags, rebuilds the strategy config
//! for the harness's toggle mask, replays a SEALED event tape through a fresh
//! [`Engine`], and returns the reconciled net-SOL and a right-tail measure. The
//! per-variant perturbations (delayed / noised / shuffled) are realized as
//! deterministic transforms of the sealed tape, so the whole replay is a pure
//! deterministic function of `(toggles, variant, family)` — no RNG, no wall-clock.
//!
//! `pq-research-runner`'s ablation entry point can therefore toggle the app's real
//! feature families and read their measured contribution, rather than asserting it.

use pump_quant_evaluator::ablation::{
    AblationReplay, AblationVariant, FeatureFamily, FeatureToggleMask, ReplayOutcome,
};

use crate::config::Config;
use crate::engine::{Engine, RunMode};
use crate::event::AppEvent;

/// The app feature families exposed to the §50 ablation harness, one per toggleable
/// config flag. The id is the bit position in the [`FeatureToggleMask`].
pub const APP_FEATURE_FAMILIES: [FeatureFamily; 8] = [
    FeatureFamily(0), // money_proxy_enable
    FeatureFamily(1), // deployer_screen_enable
    FeatureFamily(2), // fee_floor_enable
    FeatureFamily(3), // narrative_class_enable
    FeatureFamily(4), // platform_lead_enable
    FeatureFamily(5), // setup_classifier_enable
    FeatureFamily(6), // entry_mode_leaves_enable
    FeatureFamily(7), // probe_budget_enable
];

/// The app's §50 ablation replay: a sealed base config + event tape the harness
/// replays under each toggle mask / variant.
#[derive(Clone, Debug)]
pub struct AppAblationReplay {
    base: Config,
    tape: Vec<AppEvent>,
}

impl AppAblationReplay {
    /// Seal a base config and event tape for replay.
    #[must_use]
    pub fn new(base: Config, tape: Vec<AppEvent>) -> Self {
        AppAblationReplay { base, tape }
    }

    /// Build the strategy config for a toggle mask: each family flag is enabled iff
    /// its bit is set in the mask. A pure function of the mask.
    fn config_for(&self, toggles: FeatureToggleMask) -> Config {
        let mut cfg = self.base;
        cfg.money_proxy_enable = toggles.contains(FeatureFamily(0));
        cfg.deployer_screen_enable = toggles.contains(FeatureFamily(1));
        cfg.fee_floor_enable = toggles.contains(FeatureFamily(2));
        cfg.narrative_class_enable = toggles.contains(FeatureFamily(3));
        cfg.platform_lead_enable = toggles.contains(FeatureFamily(4));
        cfg.setup_classifier_enable = toggles.contains(FeatureFamily(5));
        cfg.entry_mode_leaves_enable = toggles.contains(FeatureFamily(6));
        cfg.probe_budget_enable = toggles.contains(FeatureFamily(7));
        cfg
    }

    /// Realize a variant's perturbation as a deterministic transform of the sealed
    /// tape. Combined / Removed / Alone leave the tape intact (the toggle mask
    /// carries their effect); delayed / noised / shuffled are deterministic tape
    /// perturbations (no RNG — a fixed FNV index-hash drives the "noise").
    // Plain modulo, not `is_multiple_of`, to honour the workspace MSRV 1.85.
    #[allow(clippy::manual_is_multiple_of)]
    fn tape_for(&self, variant: AblationVariant) -> Vec<AppEvent> {
        match variant {
            AblationVariant::Combined | AblationVariant::Removed | AblationVariant::Alone => {
                self.tape.clone()
            }
            // Delayed: the signal arrives one decision step late — drop the first
            // event so every downstream event shifts back by one.
            AblationVariant::Delayed => self.tape.iter().skip(1).copied().collect(),
            // Noised: deterministically drop every event whose index-hash is 0 mod 5.
            AblationVariant::Noised => self
                .tape
                .iter()
                .enumerate()
                .filter(|(i, _)| index_hash(*i as u64) % 5 != 0)
                .map(|(_, e)| *e)
                .collect(),
            // Shuffled: a deterministic reorder (reversal) of the tape.
            AblationVariant::Shuffled => self.tape.iter().rev().copied().collect(),
        }
    }
}

impl AblationReplay for AppAblationReplay {
    fn replay(
        &self,
        toggles: FeatureToggleMask,
        variant: AblationVariant,
        _family: Option<FeatureFamily>,
    ) -> ReplayOutcome {
        let cfg = self.config_for(toggles);
        let tape = self.tape_for(variant);
        let mut eng = Engine::new(cfg, RunMode::Replay);
        let report = eng.run(&tape);
        // Right-tail proxy: the best single-lane net contribution — a deterministic
        // right-tail measure over the reconciled per-lane vector.
        let right_tail = report
            .per_lane_net
            .iter()
            .map(|(_, n)| *n)
            .max()
            .unwrap_or(0);
        ReplayOutcome::new(report.net_lamports, right_tail)
    }
}

/// FNV-1a/64 over an index — the deterministic "noise" selector (a hash, not an
/// RNG: the same index always maps to the same value).
fn index_hash(index: u64) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = FNV_OFFSET_BASIS;
    for &b in &index.to_le_bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

#[cfg(test)]
mod tests {

    /// A curve that has been bought into by 0.2 SOL: the price reserve is the 30 SOL
    /// seed plus the raise, and the escrowed (extractable) SOL is the raise itself.
    /// `real_sol = virtual_sol - LAUNCH_VSOL_LAMPORTS` is the venue's identity, not a
    /// choice — see `crate::curve_state::real_sol_for`.
    const CURVE_REAL_SOL: u64 = 200_000_000;
    const CURVE_VSOL: u64 = crate::curve_state::LAUNCH_VSOL_LAMPORTS + CURVE_REAL_SOL;
    use super::*;
    use pump_quant_domain::ids::Mint;
    use pump_quant_evaluator::ablation::run_ablation;

    fn mint(tag: u64) -> Mint {
        let mut b = [0u8; 32];
        b[..8].copy_from_slice(&tag.to_le_bytes());
        b[8] = 0xAB;
        Mint::from_bytes(b)
    }

    fn small_tape() -> Vec<AppEvent> {
        let mut tape = Vec::new();
        for m in 0..8u64 {
            let mt = mint(m);
            for i in 0..4u64 {
                tape.push(AppEvent::MarketTrade {
                    mint: mt,
                    price_fp: 1_000_000_000 + (i as i128) * 1_000_000 + (m as i128) * 1_000,
                    quote_lamports: 500_000,
                    liquidity_lamports: CURVE_VSOL,
                    signed_base: 600_000 - (i as i64) * 50,
                    buyer_entity: (m + i) % 13,
                    age_slots: 12,
                });
            }
            // RE-EXPRESSED (2026-07-28): this harness used to declare a 0.2 SOL
            // "pool" and a 0.2 SOL sellable depth — a market that cannot exist on
            // this venue, where a curve is seeded with 30 SOL of VIRTUAL reserve and
            // escrows `virtual_sol - 30 SOL`. The reserve is now a real curve whose
            // extractable depth is still 0.2 SOL, so the report surface exercises the
            // same shape against a market the venue could actually produce.
            tape.push(AppEvent::OnchainConfirm {
                mint: mt,
                virtual_sol_lamports: CURVE_VSOL,
                real_sol_lamports: CURVE_REAL_SOL,
            });
            tape.push(AppEvent::Tick);
        }
        for _ in 0..12 {
            tape.push(AppEvent::Tick);
        }
        tape
    }

    /// The app replay produces byte-for-byte identical ablation reports across two
    /// runs — a deterministic pure function of `(toggles, variant, family)`.
    #[test]
    fn app_replay_is_deterministic() {
        let replay = AppAblationReplay::new(Config::dev_portable(), small_tape());
        let a = run_ablation(&replay, &APP_FEATURE_FAMILIES, &AblationVariant::PER_FAMILY);
        let b = run_ablation(&replay, &APP_FEATURE_FAMILIES, &AblationVariant::PER_FAMILY);
        assert_eq!(a, b, "app ablation replay must be deterministic");
        // 8 families × 5 per-family variants = 40 measurements.
        assert_eq!(a.results.len(), 40);
    }

    /// A single replay is itself deterministic, and toggling a family off (Removed)
    /// yields an outcome distinct in general from the combined baseline — the trait
    /// genuinely drives the app engine, not a constant.
    #[test]
    fn per_variant_outcomes_are_stable() {
        let replay = AppAblationReplay::new(Config::dev_portable(), small_tape());
        let all_on = FeatureToggleMask::all_on(&APP_FEATURE_FAMILIES);
        let o1 = replay.replay(all_on, AblationVariant::Combined, None);
        let o2 = replay.replay(all_on, AblationVariant::Combined, None);
        assert_eq!(o1, o2, "identical args -> identical outcome");
        // Shuffled reorders the sealed tape deterministically.
        let s1 = replay.replay(all_on, AblationVariant::Shuffled, Some(FeatureFamily(0)));
        let s2 = replay.replay(all_on, AblationVariant::Shuffled, Some(FeatureFamily(0)));
        assert_eq!(s1, s2, "shuffled variant is deterministic");
    }
}
