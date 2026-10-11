//! Prompt contract **v2** (`pq_prompt_v2_exec_fee_20261010`): the execution-fee correction (SCOPE_ENG_R5 W2 F2/F3).
//!
//! v1 (the trained, served contract, [`crate::system`] + [`crate::render_decision`] + [`crate::render_management`])
//! tells the model a venue fee of 94.63 bp/side on the bonding curve and 30 bp/side on PumpSwap. The executor charges
//! the pump fee configuration in force at the trade's slot: 125 bp/side on the curve (95 protocol + 30
//! creator-or-cashback) and the pool's market-cap tier on PumpSwap (30-125 bp/side). Rule: the model is never told a
//! cost lower than the executor charges.
//!
//! v2 changes ONLY the cost statement:
//! * the system prompt states no number; it says the per-row execution fee is the executor's own rate;
//! * the per-row cost block uses an [`ExecCosts`] the CALLER derives from the same slot-aware fee authority that
//!   quotes and paper-executes the order (`pump_quant_protocol::fee_schedule` via `exec_quote`), plus network fee,
//!   tip and token-account rent as separate terms; cashback is named as an entitlement, never cash.
//!
//! Every other byte of the user prompt is v1's (rendered by the v1 renderer, then only the cost block is replaced),
//! so no trained feature changes meaning. v1 is untouched and stays the default; selecting v2 is explicit, and old
//! prompt hashes / old model gates are NOT acceptance for v2 (offline evaluation first).
#![forbid(unsafe_code)]

use crate::cost::{size_tiers, SizeMenu, BPS_ONE, LAMPORTS_PER_SOL};
use crate::decision::{render_decision, DecisionBundle};
use crate::fmt::{py_fixed, py_g, round_half_even};
use crate::management::{render_management, ManagementBundle};
use crate::system::PromptFamily;

/// The contract id carried with every v2 prompt.
pub const PROMPT_CONTRACT_V2_ID: &str = "pq_prompt_v2_exec_fee_20261010";

const SYSTEM_DECISION_V2: &str = "You are an on-chain opportunity assessor for pump.fun / pumpswap memecoins. You receive a strictly causal state snapshot measured at a decision time and must choose exactly one action: BUY, WATCH or SKIP. Execution cost is the round trip the executor will charge: 2 x execution fee + 2 x clip impact + 2 x (network fee + tip) + token-account rent not refunded. The execution fee is the venue's own per-side rate for THIS market at the decision slot (bonding curve or PumpSwap market-cap tier), stated per row; it is charged ON TOP of the quoted price. Cashback, when paid, is a claimable entitlement, not cash. Impact is clip size / pool depth. A BUY is only correct if the expected move can clear that cost. Answer in the fixed format: DECISION, then SIZE/PRICE LIMIT (BUY only), INVALIDATION, EVIDENCE (2-4 facts citing supplied fields), COUNTEREVIDENCE, EVIDENCE_STATUS.";

const SYSTEM_MANAGEMENT_V2: &str = "You are an on-chain position manager for pump.fun/pumpswap memecoins. You hold a position. Given the causal market snapshot and your position state, choose exactly one action: HOLD, ADD, REDUCE, EXIT. Execution cost is the round trip the executor will charge: 2 x execution fee + 2 x clip impact + 2 x (network fee + tip) + token-account rent not refunded. The execution fee is the venue's own per-side rate for THIS market at the decision slot (bonding curve or PumpSwap market-cap tier), stated per row; it is charged ON TOP of the quoted price. Cashback, when paid, is a claimable entitlement, not cash. Impact is clip size / pool depth. Answer in the fixed format.";

/// The v2 system prompt. Only the live families have a v2; the training-only utility families return `None`.
#[must_use]
pub fn system_prompt_v2(family: PromptFamily) -> Option<&'static str> {
    match family {
        PromptFamily::Decision => Some(SYSTEM_DECISION_V2),
        PromptFamily::Management => Some(SYSTEM_MANAGEMENT_V2),
        PromptFamily::UtilityReasoning | PromptFamily::UtilityRegression => None,
    }
}

/// Execution costs of one order as the executor will charge them, derived by the caller from the fee authority.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecCosts {
    /// Venue fee per side, bp: the total of the tier in force at the decision slot (all components).
    pub venue_bp_per_side: u64,
    /// The authority's stamp: `<venue>@<read_slot>:<provenance>:tier<i>:<bp>bp`.
    pub fee_stamp: String,
    /// Network fee per landed leg, lamports.
    pub network_lamports_per_leg: u64,
    /// Tip per landed leg, lamports.
    pub tip_lamports_per_leg: u64,
    /// Token-account rent NOT refunded over the round trip (deposit minus close refund), lamports.
    pub rent_unrefunded_lamports: u64,
}

/// Every term of one clip's v2 round trip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CostBreakdownV2 {
    /// Notional, SOL.
    pub clip_sol: f64,
    /// Pool depth, SOL (None = unknown, impact 0, never fabricated).
    pub depth_sol: Option<f64>,
    /// Execution fee per side, bp.
    pub venue_bp_per_side: f64,
    /// Own impact per side, bp (unrounded).
    pub impact_bp_per_leg: f64,
    /// Network + tip per leg, bp.
    pub landing_bp_per_leg: f64,
    /// Unrefunded rent over the round trip, bp.
    pub rent_bp: f64,
    /// Whole round trip, bp (integer as printed, round-half-even).
    pub round_trip_bp: i64,
}

/// v2 round trip for one clip: `2 x venue + 2 x round(impact) + 2 x landing + rent`.
#[must_use]
pub fn decompose_v2(clip_sol: f64, depth_sol: Option<f64>, c: &ExecCosts) -> CostBreakdownV2 {
    let notional_lam = (clip_sol * LAMPORTS_PER_SOL).max(1.0);
    let depth = depth_sol.filter(|d| *d != 0.0);
    let impact = depth.map_or(0.0, |d| clip_sol / d * BPS_ONE);
    let landing =
        (c.network_lamports_per_leg + c.tip_lamports_per_leg) as f64 / notional_lam * BPS_ONE;
    let rent = c.rent_unrefunded_lamports as f64 / notional_lam * BPS_ONE;
    let venue = c.venue_bp_per_side as f64;
    let total = 2.0 * venue + 2.0 * round_half_even(impact) + 2.0 * landing + rent;
    CostBreakdownV2 {
        clip_sol,
        depth_sol: depth,
        venue_bp_per_side: venue,
        impact_bp_per_leg: impact,
        landing_bp_per_leg: landing,
        rent_bp: rent,
        round_trip_bp: round_half_even(total) as i64,
    }
}

/// The v2 SIZE OPTIONS block (same menu as the trained v1 block; costs from [`ExecCosts`]).
#[must_use]
pub fn size_options_v2(depth_sol: Option<f64>, c: &ExecCosts) -> String {
    let mut out = String::with_capacity(512);
    out.push_str(&format!(
        "SIZE OPTIONS (choose one on BUY; round-trip cost at the execution fee {} bp/side [{}]):",
        c.venue_bp_per_side, c.fee_stamp
    ));
    for (name, clip) in size_tiers(SizeMenu::Trained) {
        let d = decompose_v2(*clip, depth_sol, c);
        let depth = match depth_sol {
            Some(dep) if dep != 0.0 => format!("pool depth {} SOL", py_fixed(dep, 1)),
            _ => "pool depth unknown".to_string(),
        };
        out.push_str(&format!(
            "\n  {name} = {} SOL - {} bp round trip ({depth})",
            py_fixed(*clip, 2),
            d.round_trip_bp
        ));
    }
    out.push_str("\n  Size is a judgement: a bigger clip pays more impact in a thinner book.");
    out
}

/// The v2 per-row cost sentence (management).
#[must_use]
pub fn cost_line_v2(clip_sol: f64, depth_sol: Option<f64>, c: &ExecCosts) -> String {
    let d = decompose_v2(clip_sol, depth_sol, c);
    let depth = match d.depth_sol {
        Some(dep) => format!("{} SOL", py_g(dep, 4)),
        None => "unknown".to_string(),
    };
    format!(
        "execution cost: round trip {} bp = 2 x execution fee ({} bp/side [{}]) + 2 x clip impact \
         ({} bp/leg for {} SOL into {depth}) + 2 x network+tip ({} bp/leg) + rent not refunded ({} bp); \
         cashback, if any, is an entitlement, not cash",
        d.round_trip_bp,
        c.venue_bp_per_side,
        c.fee_stamp,
        py_fixed(d.impact_bp_per_leg, 2),
        py_fixed(d.clip_sol, 2),
        py_fixed(d.landing_bp_per_leg, 3),
        py_fixed(d.rent_bp, 3)
    )
}

const SIZE_HEADER_V1: &str =
    "SIZE OPTIONS (choose one on BUY; round-trip cost from the cost authority):";

/// Render the v2 entry prompt: v1's bytes with ONLY the SIZE OPTIONS block replaced.
#[must_use]
pub fn render_decision_v2(b: &DecisionBundle, c: &ExecCosts) -> String {
    let v1 = render_decision(b);
    let at = v1
        .rfind(SIZE_HEADER_V1)
        .expect("v1 render always ends with its SIZE OPTIONS block"); // LINT-ALLOW(hot_panic): v1 renderer invariant
    let mut out = String::with_capacity(v1.len() + 64);
    out.push_str(&v1[..at]);
    out.push_str(&size_options_v2(b.size_depth_sol, c));
    out.push('\n');
    out
}

/// Render the v2 management prompt: v1's bytes with ONLY the execution-cost line replaced.
#[must_use]
pub fn render_management_v2(m: &ManagementBundle, c: &ExecCosts) -> String {
    let v1 = render_management(m);
    let clip = m.qty_pre * m.mark;
    let old = crate::cost::cost_line(
        clip,
        Some(m.depth_sol),
        crate::cost::Regime::for_venue(&m.venue),
    );
    debug_assert_eq!(v1.matches(old.as_str()).count(), 1);
    v1.replacen(old.as_str(), &cost_line_v2(clip, Some(m.depth_sol), c), 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn curve_costs() -> ExecCosts {
        ExecCosts {
            venue_bp_per_side: 125,
            fee_stamp: "curve@454754100:read_at_slot:tier0:125bp".to_string(),
            network_lamports_per_leg: 10_000,
            tip_lamports_per_leg: 10_000,
            rent_unrefunded_lamports: 5_000,
        }
    }

    #[test]
    fn v2_round_trip_is_never_below_the_executor_venue_fee() {
        let c = curve_costs();
        for clip in [0.25, 0.5, 1.0] {
            let d = decompose_v2(clip, Some(30.0), &c);
            assert!(d.round_trip_bp >= 250, "{clip} {d:?}");
            // v1 told 2 x 94.63 for the same clip: strictly lower than v2.
            let v1 = crate::cost::decompose(clip, Some(30.0), crate::cost::Regime::BondingCurve);
            assert!(v1.round_trip_bp < d.round_trip_bp);
        }
        // 1 SOL into 30 SOL: 250 + 2 x 333 + 2 x 0.2 + 0.05 = 916.45 -> 916.
        assert_eq!(decompose_v2(1.0, Some(30.0), &c).round_trip_bp, 916);
    }

    #[test]
    fn v2_system_prompts_state_no_fee_number_and_v1_is_unchanged() {
        for f in [PromptFamily::Decision, PromptFamily::Management] {
            let s = system_prompt_v2(f).unwrap();
            assert!(!s.contains("94.63") && !s.contains("30 bp"), "{s}");
            assert!(
                crate::system::system_prompt(f).contains("94.63"),
                "v1 untouched"
            );
        }
        assert!(system_prompt_v2(PromptFamily::UtilityReasoning).is_none());
    }
}
