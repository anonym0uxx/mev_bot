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
}
