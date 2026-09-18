//! `pump_quant_proposal` — render the Qwen decision-bundle so it tokenizes
//! byte-identically to the c11 training corpus.
//!
//! This crate owns ONE correctness invariant: the `LIVE FLOW STATE` (user) and
//! `FLOW CITATION` (assistant) lines emitted by the live builder must match,
//! byte-for-byte, what `inject_c11_flow.py` produced when it built the corpus.
//! Any divergence is out-of-distribution for the model — the same class of
//! silent damage as a wrong action vocabulary.
//!
//! The field order and formatting are the contract. Values are rendered exactly
//! as Python's `str()` renders them (`None` for null, `True`/`False` for bools,
//! shortest round-trip floats with a trailing `.0` on whole numbers).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::fmt::Write as _;

pub mod cost;
pub mod decision;
pub mod fmt;
pub mod management;
pub mod pynum;
pub mod system;

pub use cost::{cost_line, decompose, size_options, CostBreakdown, Regime, BPS_ONE, LAMPORTS_PER_SOL};
pub use decision::{render_decision, DecisionBundle};
pub use fmt::py_float;
pub use management::{render_management, ManagementBundle};
pub use pynum::PyNum;
pub use system::{system_prompt, PromptFamily};

/// The 13 as-of-t flow features, in the exact order `inject_c11_flow.py`
/// renders them. Integer fields are plain counts; shares and SOL flows are
/// `f64` already rounded to their corpus precision (6 dp, or 1 dp for
/// `flow_lookback_d`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlowState {
    /// Unique buyers in the last 60 s.
    pub entrants_60s: u64,
    /// Unique buyers in the last 300 s.
    pub entrants_300s: u64,
    /// Net SOL entering the pool from traders over 300 s (round 6 dp).
    pub net_flow_sol_300s: f64,
    /// Share of 300 s entrants whose first capture appearance is fresh (None when no entrants).
    pub fresh_wallet_share_300s: Option<f64>,
    /// Observed lookback depth at decision time, days, 1 dp, capped at 7.0.
    pub flow_lookback_d: f64,
    /// Share of entrants who sniped this mint (None when no entrants).
    pub sniper_share_300s: Option<f64>,
    /// Share of buys whose exact SOL amount repeats >= 3x (None when no buys).
    pub bot_uniform_share_300s: Option<f64>,
    /// Entrants with as-of-t extracted PnL >= +5 SOL over >= 5 mints.
    pub smart_entrants_300s: u64,
    /// Net SOL flow from smart wallets only (round 6 dp).
    pub smart_net_flow_sol_300s: f64,
    /// Entrants sharing >= 2 prior early co-entries with another entrant.
    pub coentry_wallets_300s: u64,
    /// Creator has >= 1 trade on its own mint before t.
    pub creator_trading_own_mint: bool,
    /// p90 priority fee of 300 s buys, lamports (None when no buys).
    pub entrant_fee_p90_lamports: Option<u64>,
    /// p50 compute units of 300 s buys (None when no buys).
    pub entrant_cu_p50: Option<u64>,
}

/// The fixed field order (must match `inject_c11_flow.py::FIELDS`).
pub const FIELD_ORDER: [&str; 13] = [
    "entrants_60s",
    "entrants_300s",
    "net_flow_sol_300s",
    "fresh_wallet_share_300s",
    "flow_lookback_d",
    "sniper_share_300s",
    "bot_uniform_share_300s",
    "smart_entrants_300s",
    "smart_net_flow_sol_300s",
    "coentry_wallets_300s",
    "creator_trading_own_mint",
    "entrant_fee_p90_lamports",
    "entrant_cu_p50",
];

/// Render one `k=v` token for the field list, mirroring Python's
/// `f"{k}={'None' if v is None else v}"`.
fn field_tokens(state: &FlowState) -> Vec<String> {
    let mut v = Vec::with_capacity(13);
    v.push(state.entrants_60s.to_string());
    v.push(state.entrants_300s.to_string());
    v.push(py_float(state.net_flow_sol_300s));
    v.push(opt_float(state.fresh_wallet_share_300s));
    v.push(py_float(state.flow_lookback_d));
    v.push(opt_float(state.sniper_share_300s));
    v.push(opt_float(state.bot_uniform_share_300s));
    v.push(state.smart_entrants_300s.to_string());
    v.push(py_float(state.smart_net_flow_sol_300s));
    v.push(state.coentry_wallets_300s.to_string());
    v.push(if state.creator_trading_own_mint {
        "True".to_string()
    } else {
        "False".to_string()
    });
    v.push(opt_u64(state.entrant_fee_p90_lamports));
    v.push(opt_u64(state.entrant_cu_p50));
    v
}

fn opt_float(v: Option<f64>) -> String {
    match v {
        Some(x) => py_float(x),
        None => "None".to_string(),
    }
}

fn opt_u64(v: Option<u64>) -> String {
    match v {
        Some(x) => x.to_string(),
        None => "None".to_string(),
    }
}

/// Render the `LIVE FLOW STATE: k=v …` user line.
pub fn render_live_flow_state(state: &FlowState) -> String {
    let mut out = String::with_capacity(512);
    out.push_str("LIVE FLOW STATE: ");
    let toks = field_tokens(state);
    for (i, k) in FIELD_ORDER.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let _ = write!(out, "{k}={}", toks[i]);
    }
    out
}

/// Render the `FLOW CITATION: k=v …` assistant line (same fields, same order).
pub fn render_flow_citation(state: &FlowState) -> String {
    let mut out = String::with_capacity(512);
    out.push_str("FLOW CITATION: ");
    let toks = field_tokens(state);
    for (i, k) in FIELD_ORDER.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let _ = write!(out, "{k}={}", toks[i]);
    }
    out
}

/// The `no_prior_flow` variant: a clock with zero prior tape events renders this
/// one line, never fabricated zeros.
pub fn render_no_prior_flow() -> &'static str {
    "LIVE FLOW STATE: no_prior_flow=true"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn py_float_matches_python_repr() {
        assert_eq!(py_float(14.025214), "14.025214");
        assert_eq!(py_float(1.0), "1.0");
        assert_eq!(py_float(0.0), "0.0");
        assert_eq!(py_float(0.131258), "0.131258");
        assert_eq!(py_float(7.0), "7.0");
        assert_eq!(py_float(0.9), "0.9");
        assert_eq!(py_float(-6.39046), "-6.39046");
    }
}
