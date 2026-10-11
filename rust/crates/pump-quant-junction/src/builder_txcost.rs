//! FEES R10 F1: the live builder's compute-budget envelope and tip, derived from the ONE versioned
//! [`pump_quant_app::txcost::TxCostConfig`]. `pq_daemon` and `pq_sell` both build their `LiveSinkConfig`
//! `compute` / `tip` from [`builder_plans`], so paper's planned leg cost and what the builder submits come
//! from one place and a named test (below) catches any divergence.
#![forbid(unsafe_code)]

use pump_quant_app::txcost::{TxCostConfig, TXCOST_V1_20261010};
use pump_quant_protocol::tx_build::{ComputePlan, TipPlan};

/// Helius Sender SWQOS-only tip account 2nyhqdwKcJZR2vcqCyrYsaPVdAnFoJjiksCXJ7hfEYgD (first of the 12
/// designated tip accounts). See https://www.helius.dev/docs/sending-transactions/sender-swqos-only
pub const SENDER_TIP_ACCOUNT: [u8; 32] = [
    0x1a, 0xa2, 0xf0, 0x5a, 0x6f, 0x89, 0x50, 0xfc, 0xbf, 0x5d, 0xf9, 0xca, 0x39, 0x48, 0x1c, 0x6d,
    0xf1, 0x33, 0x05, 0xc8, 0xb8, 0x7c, 0x64, 0x4f, 0x4d, 0x8c, 0x6d, 0x82, 0x0b, 0x37, 0x89, 0xa6,
];

/// The cost configuration the live builder sends (always v1: the builder's own envelope).
pub const BUILDER_TXCOST: TxCostConfig = TXCOST_V1_20261010;

/// The builder's `ComputePlan` and `TipPlan` from a cost configuration.
#[must_use]
pub const fn builder_plans(tx: &TxCostConfig) -> (ComputePlan, TipPlan) {
    (
        tx.compute_plan(),
        TipPlan {
            to: SENDER_TIP_ACCOUNT,
            lamports: tx.tip_lamports,
        },
    )
}

/// The builder's planned cost of one transaction from the plans it actually hands the sink:
/// `meta.fee` (5,000 x signatures + ceil(limit x price / 1e6)) + the tip transfer. Computed from the PLANS,
/// not from the `TxCostConfig`, so it is an independent check of [`builder_plans`].
#[must_use]
pub fn builder_planned_leg_lamports(signatures: u64, compute: ComputePlan, tip: TipPlan) -> u64 {
    let pri = u128::from(compute.unit_limit)
        .saturating_mul(u128::from(compute.unit_price_micro_lamports))
        .div_ceil(1_000_000);
    5_000_u64
        .saturating_mul(signatures)
        .saturating_add(u64::try_from(pri).unwrap_or(u64::MAX))
        .saturating_add(tip.lamports)
}

/// FEES R10 F2: the startup line naming the active paper txcost id beside the builder's, so a run that
/// parsed version 0 (an absent `txcost_version` key) is visible, never silent.
#[must_use]
pub fn startup_txcost_line(cfg: &pump_quant_app::config::Config) -> String {
    let paper = cfg.txcost_for(false);
    let (c, t) = builder_plans(&BUILDER_TXCOST);
    let builder_leg = builder_planned_leg_lamports(BUILDER_TXCOST.signatures, c, t);
    let mut s = format!(
        "[pq-daemon] txcost: paper txcost_version={} id={} planned_leg={} (entry) / {} (exit); builder id={} planned_leg={}",
        cfg.txcost_version,
        paper.id,
        paper.planned_leg_lamports(),
        cfg.txcost_for(true).planned_leg_lamports(),
        BUILDER_TXCOST.id,
        builder_leg,
    );
    if paper.id != BUILDER_TXCOST.id {
        s.push_str(" WARNING: paper_txcost_differs_from_builder (legacy v0 parse: absent or 0 txcost_version)");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use pump_quant_app::config::Config;

    #[test]
    fn builder_tip_and_compute_equal_the_v1_paper_plan() {
        let (c, t) = builder_plans(&BUILDER_TXCOST);
        assert_eq!(t.lamports, TXCOST_V1_20261010.tip_lamports);
        assert_eq!(t.lamports, 5_000, "SWQOS-only minimum tip");
        assert_eq!(c, TXCOST_V1_20261010.compute_plan());
        let builder = builder_planned_leg_lamports(1, c, t);
        assert_eq!(builder, 10_600);
        let mut cfg = Config::dev_portable();
        cfg.apply("txcost_version", 1).unwrap();
        for exit in [false, true] {
            assert_eq!(cfg.txcost_for(exit).planned_leg_lamports(), builder);
        }
    }

    #[test]
    fn startup_line_names_the_active_id_and_flags_a_v0_parse() {
        let mut v1 = Config::dev_portable();
        v1.apply("txcost_version", 1).unwrap();
        let l1 = startup_txcost_line(&v1);
        assert!(
            l1.contains("txcost_version=1 id=txcost_v1_20261010 planned_leg=10600"),
            "{l1}"
        );
        assert!(!l1.contains("WARNING"), "{l1}");
        // An absent key (pre-T1 manifest / missing CHAMPION_CONFIG line) parses as v0: named, never silent.
        let v0 = Config::from_str_over_default("").unwrap();
        let l0 = startup_txcost_line(&v0);
        assert!(
            l0.contains("txcost_version=0 id=txcost_v0_legacy_p50"),
            "{l0}"
        );
        assert!(
            l0.contains("WARNING: paper_txcost_differs_from_builder"),
            "{l0}"
        );
        assert!(
            l0.contains("builder id=txcost_v1_20261010 planned_leg=10600"),
            "{l0}"
        );
    }
}
