//! SCOPE_TXCOST_R9 T1: the ONE versioned transaction-cost configuration. It is the only source of the PLANNED
//! base fee, priority fee and tip for the paper executor (`entry_cost`, exit/add legs, stop-table estimates, the v2
//! prompt cost block) AND the live builder (`pq_daemon` / `pq_sell` `ComputePlan` + `TipPlan`).
//!
//! # Why paper said 20,000 and the builder 10,600 (before T1, from code at 74ac8fe7)
//! * Paper, per LANDED LEG (buy and sell each): `exec_quote::landed_leg_cost(cfg.entry_tip_lamports)` =
//!   `NETWORK_FEE_P50_LAMPORTS` (10,000: a measured p50 of `meta.fee`, i.e. base + priority of OTHER traders'
//!   transactions, not our message) + `entry_tip_lamports` / `exit_tip_lamports` (config default and
//!   CHAMPION_CONFIG: 10,000) = 20,000. Call sites: `model_admit.rs` entry_cost, `model_manage.rs` exit/add legs,
//!   `model_stop.rs` / `model_shadow.rs` stop estimates, `exec_costs_v2` (`network_lamports_per_leg`).
//! * Builder, per transaction (= per leg): `pq_daemon.rs` "Sink config": 1 signature x 5,000 (base) +
//!   ceil(`SetComputeUnitLimit` 120,000 x `SetComputeUnitPrice` 5,000 micro-lamports / 1e6) = 600 (priority,
//!   charged on the requested limit) + `TipPlan.lamports` 5,000 (a System transfer, outside `meta.fee`) = 10,600.
//! * Neither side double-counted; they disagreed on INPUTS: a measured population p50 vs our own message's
//!   envelope (+4,400 network), and a 10,000 config tip vs the 5,000 SWQOS-only tip the builder sends (+5,000).
//!   Paper was 9,400 per leg (18,800 per round trip) more conservative than what the builder submits.
//!
//! # After T1
//! * [`TXCOST_V1_20261010`] is the builder's exact envelope; paper under `txcost_version = 1` books the same
//!   planned numbers (10,600 per leg), and the builder's `ComputePlan` / tip are built from it.
//! * [`TxCostConfig::legacy`] (`txcost_v0_legacy_p50`) is the pre-T1 paper arithmetic, kept ONLY so a run whose
//!   manifest (config dump) predates the `txcost_version` key replays with its own costs: an absent key parses as
//!   version 0. New runs pin version 1 in CHAMPION_CONFIG.txt; the config dump (and hence the config fingerprint
//!   in cumulative_pnl.json / session_history.jsonl) records the version and the id.
//! * Planned costs are a PLAN, never a ceiling: an executor's reported actual fee (meta.fee + tip transfer) is
//!   booked exactly as reported, overruns included (`model_mgmt_ingest_evidence`).
//! * [`TxCostConfig::stress_allowance_lamports`] is a separately named conservative allowance, reported beside the
//!   planned costs and NEVER part of [`TxCostConfig::planned_leg_lamports`] or any booked fee.
#![forbid(unsafe_code)]

use pump_quant_protocol::tx_build::ComputePlan;

/// One versioned transaction-cost configuration (per landed leg = one transaction).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TxCostConfig {
    /// Version id recorded with every run (`txcost_v1_20261010`, ...).
    pub id: &'static str,
    /// Base fee per required signature (Solana: 5,000).
    pub lamports_per_signature: u64,
    /// Required signatures of the constructed message.
    pub signatures: u64,
    /// `SetComputeUnitLimit` units (the priority fee is charged on this REQUESTED limit).
    pub cu_limit: u32,
    /// `SetComputeUnitPrice`, micro-lamports per CU.
    pub cu_price_micro_lamports: u64,
    /// Tip transfer per leg (outside `meta.fee`).
    pub tip_lamports: u64,
    /// Legacy only: a measured population p50 of `meta.fee` used INSTEAD of our message's base + priority.
    pub measured_network_p50_lamports: Option<u64>,
    /// Conservative stress allowance: reported beside planned costs, never booked as a fee.
    pub stress_allowance_lamports: u64,
}

/// The live builder's envelope (pq_daemon §5 Sink config): 1 sig, 120,000 CU at 5,000 micro-lamports, 5,000 tip.
pub const TXCOST_V1_20261010: TxCostConfig = TxCostConfig {
    id: "txcost_v1_20261010",
    lamports_per_signature: 5_000,
    signatures: 1,
    cu_limit: 120_000,
    cu_price_micro_lamports: 5_000,
    tip_lamports: 5_000,
    measured_network_p50_lamports: None,
    stress_allowance_lamports: 0,
};

/// Id of the pre-T1 paper arithmetic (measured p50 network + config tip).
pub const TXCOST_V0_LEGACY_ID: &str = "txcost_v0_legacy_p50";

impl TxCostConfig {
    /// Pre-T1 paper arithmetic: `NETWORK_FEE_P50_LAMPORTS` + the config tip of that leg. Replay of old runs only.
    #[must_use]
    pub const fn legacy(tip_lamports: u64) -> Self {
        Self {
            id: TXCOST_V0_LEGACY_ID,
            lamports_per_signature: 0,
            signatures: 0,
            cu_limit: 0,
            cu_price_micro_lamports: 0,
            tip_lamports,
            measured_network_p50_lamports: Some(crate::exec_quote::NETWORK_FEE_P50_LAMPORTS),
            stress_allowance_lamports: 0,
        }
    }

    /// The configuration a config's `txcost_version` selects (`legacy_tip` is only read by version 0).
    #[must_use]
    pub const fn by_version(version: u32, legacy_tip: u64) -> Self {
        match version {
            1 => TXCOST_V1_20261010,
            _ => Self::legacy(legacy_tip),
        }
    }

    /// Planned base fee: `lamports_per_signature x signatures`.
    #[must_use]
    pub const fn planned_base_lamports(&self) -> u64 {
        self.lamports_per_signature.saturating_mul(self.signatures)
    }

    /// Planned priority fee: `ceil(cu_limit x cu_price_micro_lamports / 1e6)`.
    #[must_use]
    pub fn planned_priority_lamports(&self) -> u64 {
        let p = (u128::from(self.cu_limit) * u128::from(self.cu_price_micro_lamports))
            .div_ceil(1_000_000);
        u64::try_from(p).unwrap_or(u64::MAX)
    }

    /// Planned network fee of one leg (`meta.fee`): base + priority, or the legacy measured p50.
    #[must_use]
    pub fn planned_network_lamports(&self) -> u64 {
        match self.measured_network_p50_lamports {
            Some(n) => n,
            None => self
                .planned_base_lamports()
                .saturating_add(self.planned_priority_lamports()),
        }
    }

    /// Planned cost of one landed leg: network + tip. The stress allowance is NOT included.
    #[must_use]
    pub fn planned_leg_lamports(&self) -> u64 {
        self.planned_network_lamports()
            .saturating_add(self.tip_lamports)
    }

    /// The builder's compute-budget envelope from this configuration.
    #[must_use]
    pub const fn compute_plan(&self) -> ComputePlan {
        ComputePlan {
            unit_limit: self.cu_limit,
            unit_price_micro_lamports: self.cu_price_micro_lamports,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    /// Independent `meta.fee` formula on the BUILDER's plan (CHAIN_READONLY_EVIDENCE: 4/4 txs to the lamport).
    fn builder_meta_fee(sigs: u64, plan: ComputePlan) -> u64 {
        let pri = u128::from(plan.unit_limit) * u128::from(plan.unit_price_micro_lamports);
        5_000 * sigs + u64::try_from(pri.div_ceil(1_000_000)).unwrap()
    }

    #[test]
    fn v1_paper_planned_equals_builder_planned_10600() {
        let tx = TXCOST_V1_20261010;
        assert_eq!(tx.planned_base_lamports(), 5_000);
        assert_eq!(tx.planned_priority_lamports(), 600);
        assert_eq!(
            tx.planned_network_lamports(),
            builder_meta_fee(1, tx.compute_plan())
        );
        assert_eq!(tx.planned_leg_lamports(), 10_600);
        // Paper under the pinned config reads exactly this, for both legs.
        let mut c = Config::dev_portable();
        c.apply("txcost_version", 1).unwrap();
        for exit in [false, true] {
            let p = c.txcost_for(exit);
            assert_eq!(p.id, "txcost_v1_20261010");
            assert_eq!(p.planned_network_lamports(), 5_600);
            assert_eq!(p.planned_leg_lamports(), 10_600);
        }
    }

    #[test]
    fn priority_fee_is_ceil_over_1e6_on_the_requested_limit() {
        let tx = TxCostConfig {
            signatures: 2,
            cu_limit: 200_001,
            cu_price_micro_lamports: 1_234,
            ..TXCOST_V1_20261010
        };
        // 200,001 x 1,234 = 246,801,234 -> ceil / 1e6 = 247.
        assert_eq!(tx.planned_priority_lamports(), 247);
        assert_eq!(tx.planned_network_lamports(), 10_247);
        assert_eq!(
            tx.planned_network_lamports(),
            builder_meta_fee(2, tx.compute_plan())
        );
    }

    #[test]
    fn legacy_is_the_pre_t1_20000_and_an_old_manifest_replays_with_it() {
        assert_eq!(TxCostConfig::legacy(10_000).planned_leg_lamports(), 20_000);
        // A pre-T1 run manifest (config dump) has no txcost_version line: it replays with its own costs.
        let old: String = Config::dev_portable()
            .dump_to_text()
            .lines()
            .filter(|l| !l.contains("txcost"))
            .collect::<Vec<_>>()
            .join("\n");
        let c = Config::from_str_over_default(&old).unwrap();
        assert_eq!(c.txcost_for(false).id, TXCOST_V0_LEGACY_ID);
        assert_eq!(c.txcost_for(false).planned_leg_lamports(), 20_000);
        // A new manifest records the version and the id, and round-trips.
        let mut n = Config::dev_portable();
        n.apply("txcost_version", 1).unwrap();
        let text = n.dump_to_text();
        assert!(text.contains("txcost_version = 1"));
        assert!(text.contains("txcost_id = txcost_v1_20261010"));
        let back = Config::from_str_over_default(&text).unwrap();
        assert_eq!(back.txcost_for(true).planned_leg_lamports(), 10_600);
        assert!(Config::dev_portable().apply("txcost_version", 2).is_err());
    }

    #[test]
    fn a_stress_allowance_is_never_a_planned_or_booked_fee() {
        let s = TxCostConfig {
            stress_allowance_lamports: 50_000,
            ..TXCOST_V1_20261010
        };
        assert_eq!(s.planned_leg_lamports(), 10_600);
        assert_eq!(s.planned_network_lamports(), 5_600);
    }
}
