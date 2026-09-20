//! The ONE cost authority, in Rust.
//!
//! Ported 1:1 from `cost_authority.py` + `exit_mechanics.cost_floor_bps`, which are the
//! only cost statement in the pipeline built from our own measurements:
//!
//! ```text
//! round trip bp = 2 x venue_bp + 2 x clip_impact_bp + 2 x fixed_tx_bp
//! ```
//!
//! with the venue fee 94.63 bp/side on the bonding curve (measured over 65,927 clean
//! single-hop swaps) and 30 bp/side on the PumpSwap AMM (its own 25 bp LP + 5 bp protocol
//! schedule), `clip_impact_bp = clip_sol / depth_sol * 10_000`, and the measured fixed
//! network+priority fee of 10,000 lamports per leg. There is deliberately NO failure
//! charge: the reward engine does not charge one, and a cost the model is told about but
//! is never graded against is exactly the drift this module exists to remove.
//!
//! The arithmetic is `f64` throughout because that is what the Python source does; every
//! rounding step uses [`crate::fmt::round_half_even`] so the printed integers match.

#![forbid(unsafe_code)]

use crate::fmt::{py_fixed, py_g, round_half_even};

/// Basis points in one unit (10,000 bp = 1.0).
pub const BPS_ONE: f64 = 10_000.0;
/// Lamports in one SOL.
pub const LAMPORTS_PER_SOL: f64 = 1_000_000_000.0;
/// The canonical traded notional a size tier is a fraction of.
pub const DEPLOY_SOL_CANONICAL: f64 = 1.0;
/// Measured fixed network+priority fee per leg, lamports (p50).
pub const FIXED_LAMPORTS_PER_LEG_P50: f64 = 10_000.0;
/// PumpSwap AMM venue fee per side, bp: its own 25 bp LP + 5 bp protocol schedule.
pub const AMM_VENUE_FEE_BPS_PER_LEG: f64 = 30.0;
/// Bonding-curve venue fee per side, bp, as measured on our own fills.
pub const BONDING_FEE_BPS_MEASURED: f64 = 94.63;

/// Which market OUR next fill lands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Regime {
    /// The pump.fun bonding curve.
    BondingCurve,
    /// A live PumpSwap pool.
    Amm,
}

impl Regime {
    /// Map a tape/prompt venue label to a cost regime. Unknown is the conservative
    /// (fee-paying) curve assumption.
    pub fn for_venue(venue: &str) -> Regime {
        match venue.trim().to_ascii_lowercase().as_str() {
            "pumpswap" | "amm" => Regime::Amm,
            _ => Regime::BondingCurve,
        }
    }

    /// The venue fee this regime charges per side, bp.
    pub fn venue_bp_per_leg(self) -> f64 {
        match self {
            Regime::BondingCurve => BONDING_FEE_BPS_MEASURED,
            Regime::Amm => AMM_VENUE_FEE_BPS_PER_LEG,
        }
    }
}

/// Every term of one clip's round trip, in bp.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CostBreakdown {
    /// The traded notional, SOL.
    pub clip_sol: f64,
    /// Pool depth in SOL, when the book could be priced.
    pub depth_sol: Option<f64>,
    /// Venue fee per side, bp.
    pub venue_bp_per_leg: f64,
    /// Our own clip impact per side, bp (unrounded).
    pub impact_bp_per_leg: f64,
    /// The measured fixed tx fee per side, bp.
    pub fixed_tx_bp_per_leg: f64,
    /// The whole round trip, bp (integer, as printed).
    pub round_trip_bp: i64,
}

/// The measured round-trip cost floor in bp: the number a take-profit must clear.
///
/// `expected_impact_bps` is the already-rounded per-side impact, exactly as the Python
/// authority passes it.
pub fn cost_floor_bps(regime: Regime, notional_sol: f64, expected_impact_bps: i64) -> i64 {
    let notional_lamports = (notional_sol * LAMPORTS_PER_SOL).max(1.0);
    let fixed_bps_per_leg = FIXED_LAMPORTS_PER_LEG_P50 / notional_lamports * BPS_ONE;
    let floor =
        2.0 * (regime.venue_bp_per_leg() + fixed_bps_per_leg) + 2.0 * expected_impact_bps as f64;
    round_half_even(floor) as i64
}

/// Every term of the round trip for one clip in one market.
///
/// `depth_sol` of `None` (or zero) means the book could not be priced: the impact term is
/// zero, never a fabricated number.
pub fn decompose(clip_sol: f64, depth_sol: Option<f64>, regime: Regime) -> CostBreakdown {
    let lam = FIXED_LAMPORTS_PER_LEG_P50;
    let notional = clip_sol.max(1e-9);
    let fixed_bp_per_leg = lam / (notional * LAMPORTS_PER_SOL) * BPS_ONE;
    let depth = depth_sol.filter(|d| *d != 0.0);
    let impact_bp_per_leg = match depth {
        Some(d) => clip_sol / d * BPS_ONE,
        None => 0.0,
    };
    let total = cost_floor_bps(regime, clip_sol, round_half_even(impact_bp_per_leg) as i64);
    CostBreakdown {
        clip_sol,
        depth_sol: depth,
        venue_bp_per_leg: regime.venue_bp_per_leg(),
        impact_bp_per_leg,
        fixed_tx_bp_per_leg: fixed_bp_per_leg,
        round_trip_bp: total,
    }
}

/// The size tiers the entry family offers, as (label, fraction of the canonical notional).
pub const SIZE_TIERS: [(&str, f64); 3] = [("SMALL", 0.25), ("MID", 0.50), ("FULL", 1.00)];

/// The SIZE OPTIONS block, generated from the same authority as the cost line so the
/// numbers in a row can never disagree with its own system prompt.
pub fn size_options(regime: Regime, depth_sol: Option<f64>, uniform_venue_fee: bool) -> String {
    let mut out = String::with_capacity(512);
    out.push_str("SIZE OPTIONS (choose one on BUY; round-trip cost from the cost authority):");
    for (name, clip) in SIZE_TIERS {
        let d = if uniform_venue_fee {
            decompose(clip, depth_sol, Regime::BondingCurve)
        } else {
            decompose(clip, depth_sol, regime)
        };
        let depth = match depth_sol {
            Some(dep) if dep != 0.0 => format!("pool depth {} SOL", py_fixed(dep, 1)),
            _ => "pool depth unknown".to_string(),
        };
        out.push_str(&format!(
            "\n  {name} = {} SOL - {} bp round trip ({depth})",
            py_fixed(clip, 2),
            d.round_trip_bp
        ));
    }
    out.push_str("\n  Size is a judgement: a bigger clip pays more impact in a thinner book.");
    out
}

/// The ONE sentence every prompt uses to state cost. Names the components so the model can
/// reason about size and venue instead of memorising a scalar.
pub fn cost_line(clip_sol: f64, depth_sol: Option<f64>, regime: Regime) -> String {
    let d = decompose(clip_sol, depth_sol, regime);
    let venue = "94.63 bp/side measured on the bonding curve, 30 bp/side on the PumpSwap \
                 AMM (its own LP + protocol schedule)";
    let depth = match d.depth_sol {
        Some(dep) => format!("{} SOL", py_g(dep, 4)),
        None => "unknown".to_string(),
    };
    format!(
        "execution cost: round trip {} bp = 2 x venue ({venue}) + 2 x clip impact \
         ({} bp/leg for {} SOL into {depth}) + 2 x tx fee ({} bp/leg, measured)",
        d.round_trip_bp,
        py_fixed(d.impact_bp_per_leg, 2),
        py_fixed(d.clip_sol, 2),
        py_fixed(d.fixed_tx_bp_per_leg, 3)
    )
}

/// The venue-independent MODEL, for a system prompt that must cover every row. Per-row
/// numbers belong in the per-row block, never in the system prompt.
pub fn model_statement() -> &'static str {
    "Execution cost is the measured round trip: 2 x venue fee + 2 x clip impact + 2 x tx \
     fee. The venue fee is 94.63 bp per side on the bonding curve (measured over 65,927 \
     swaps) and 30 bp per side on the PumpSwap AMM (its own 25 bp LP + 5 bp protocol \
     schedule). Both are charged ON TOP of the quoted price: the pool retaining its fee \
     explains the price a past trade printed at, not the fee your next trade pays. Impact \
     is clip size / pool depth. The tx fee is the measured network+priority cost per leg."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_floor_is_the_measured_number() {
        // 1 SOL, 4000 SOL depth, bond 0.0% curve: 2*(94.63+0.1) + 2*0 = 189.46 -> 189
        assert_eq!(cost_floor_bps(Regime::BondingCurve, 1.0, 0), 189);
        // 1 SOL, 50 bp/leg impact: 2*(94.63+0.1) + 100 = 289.46 -> 289
        assert_eq!(cost_floor_bps(Regime::BondingCurve, 1.0, 50), 289);
        // AMM at 1 SOL, 50 bp/leg impact: 2*(30+0.1) + 100 = 160.2 -> 160
        assert_eq!(cost_floor_bps(Regime::Amm, 1.0, 50), 160);
        // AMM, no impact: 60.2 -> 60
        assert_eq!(cost_floor_bps(Regime::Amm, 1.0, 0), 60);
        // the retired 210 bp constant is not re-derivable here
        assert_ne!(cost_floor_bps(Regime::Amm, 1.0, 50), 210);
    }

    #[test]
    fn amm_is_cheaper_than_the_curve_and_impact_is_monotone() {
        let a = decompose(1.0, Some(4000.0), Regime::Amm);
        let c = decompose(1.0, Some(4000.0), Regime::BondingCurve);
        assert!(a.round_trip_bp < c.round_trip_bp);
        assert!(
            decompose(1.0, Some(10.0), Regime::Amm).round_trip_bp
                > decompose(1.0, Some(1000.0), Regime::Amm).round_trip_bp
        );
        assert!(
            decompose(1.0, Some(100.0), Regime::Amm).round_trip_bp
                > decompose(0.25, Some(100.0), Regime::Amm).round_trip_bp
        );
    }

    #[test]
    fn unknown_venue_is_the_conservative_one() {
        assert_eq!(Regime::for_venue("pumpfun"), Regime::BondingCurve);
        assert_eq!(Regime::for_venue("pumpswap"), Regime::Amm);
        assert_eq!(Regime::for_venue("mixed"), Regime::BondingCurve);
        assert_eq!(Regime::for_venue(""), Regime::BondingCurve);
    }

    #[test]
    fn size_options_reproduce_a_known_corpus_block() {
        let block = size_options(Regime::Amm, Some(4277.289364175), false);
        assert!(block.starts_with("SIZE OPTIONS (choose one on BUY;"));
        assert!(block.contains("SMALL = 0.25 SOL - 63 bp round trip (pool depth 4277.3 SOL)"));
        assert!(block.contains("MID = 0.50 SOL - 62 bp round trip (pool depth 4277.3 SOL)"));
        assert!(block.contains("FULL = 1.00 SOL - 64 bp round trip (pool depth 4277.3 SOL)"));
        assert!(block.ends_with("a thinner book."));
    }

    #[test]
    fn cost_line_reproduces_a_known_corpus_line() {
        let line = cost_line(21.4659 * 0.0245422000546, Some(1864.65), Regime::Amm);
        assert!(line.starts_with("execution cost: round trip "), "{line}");
        assert!(line.contains("for 0.53 SOL into 1865 SOL"), "{line}");
        assert!(!line.contains("failure"), "{line}");
    }
}
