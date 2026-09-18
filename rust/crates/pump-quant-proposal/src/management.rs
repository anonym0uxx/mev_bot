//! The `management_replay` user prompt, byte-identical to the c11 corpus.
//!
//! Same contract as [`crate::decision`]: fixed section order, Python number types, and
//! the `.4g` / `.12g` / `.6g` / `.2f` prints the corpus used. The cost line is generated
//! by the one cost authority from the position's own notional and the pool depth, so the
//! market state and the cost sentence can never disagree.

#![forbid(unsafe_code)]

use crate::cost::{cost_line, Regime};
use crate::fmt::{py_fixed, py_g};
use crate::pynum::PyNum;
use crate::FlowState;

/// The management family's enrichment, in corpus field order.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct EnrichedManagement {
    /// Wallets holding at least one raw token at t.
    pub holders_at_t: Option<PyNum>,
    /// Largest holder's share of net wallet float.
    pub top1_float_share: Option<PyNum>,
    /// Herfindahl index over wallet float.
    pub holder_hhi: Option<PyNum>,
    /// Market cap in SOL.
    pub mcap_sol_at_t: Option<PyNum>,
    /// Wallets the bundles used.
    pub bundle_wallets: Option<PyNum>,
    /// Wallets that round-tripped the mint.
    pub round_trip_wallets: Option<PyNum>,
    /// Prior launches by the creator.
    pub creator_past_launches: Option<PyNum>,
    /// Whether the creator was resolvable.
    pub creator_known: Option<PyNum>,
    /// Share of volume judged wash.
    pub wash_ratio: Option<PyNum>,
}

/// The creator's observable history (management rendering — raw Python values, no
/// `int(bool(...))` coercion).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DevHistoryManagement {
    /// Prior launches by the creator.
    pub creator_past_launches: Option<PyNum>,
    /// Whether the creator was resolvable.
    pub creator_known: Option<PyNum>,
    /// Wallets the bundles used.
    pub bundle_wallets: Option<PyNum>,
    /// Share of volume judged wash.
    pub wash_ratio: Option<PyNum>,
}

/// Everything the `management_replay` user prompt states.
#[derive(Debug, Clone, PartialEq)]
pub struct ManagementBundle {
    /// The mint under management.
    pub mint: String,
    /// Decision clock, unix milliseconds.
    pub t_dec: i64,
    /// Which re-evaluation step this is.
    pub step: i64,
    /// Venue label at the clock.
    pub venue: String,
    /// `bonding_curve` or `graduated_amm`.
    pub market: String,
    /// Pool depth in SOL.
    pub depth_sol: f64,
    /// Spot price, lamports per raw token.
    pub mark: f64,
    /// Our entry price, lamports per raw token.
    pub entry_px: f64,
    /// Unrealised PnL, bp.
    pub upnl_bp: f64,
    /// Seconds held.
    pub held_s: f64,
    /// Maximum favourable excursion so far, bp.
    pub mfe_bp: f64,
    /// Maximum adverse excursion so far, bp.
    pub mae_bp: f64,
    /// Inventory held, raw tokens.
    pub qty_pre: f64,
    /// Free cash, SOL.
    pub cash_pre: f64,
    /// The as-of-t enrichment block.
    pub enriched: EnrichedManagement,
    /// The creator history block.
    pub dev: DevHistoryManagement,
    /// The 13 live flow features.
    pub flow: FlowState,
    /// True when the clock had zero prior tape events.
    pub flow_no_prior: bool,
}

fn kv(out: &mut String, key: &str, v: Option<PyNum>) {
    if let Some(x) = v {
        out.push(' ');
        out.push_str(key);
        out.push('=');
        out.push_str(&x.render());
    }
}

/// Render the management-side `ENRICHED CANDIDATE STATE: …` / `DEV HISTORY: …` lines.
///
/// A field with no value is OMITTED (the corpus's `darkish` rule) — never rendered as a
/// fabricated zero.
pub fn render_enriched(e: &EnrichedManagement) -> String {
    let mut out = String::from("ENRICHED CANDIDATE STATE:");
    kv(&mut out, "holders_at_t", e.holders_at_t);
    kv(&mut out, "top1_float_share", e.top1_float_share);
    kv(&mut out, "holder_hhi", e.holder_hhi);
    kv(&mut out, "mcap_sol_at_t", e.mcap_sol_at_t);
    kv(&mut out, "bundle_wallets", e.bundle_wallets);
    kv(&mut out, "round_trip_wallets", e.round_trip_wallets);
    kv(&mut out, "creator_past_launches", e.creator_past_launches);
    kv(&mut out, "creator_known", e.creator_known);
    kv(&mut out, "wash_ratio", e.wash_ratio);
    out
}

/// Render the management-side `DEV HISTORY: …` line.
pub fn render_dev(d: &DevHistoryManagement) -> String {
    let mut out = String::from("DEV HISTORY:");
    kv(&mut out, "creator_past_launches", d.creator_past_launches);
    kv(&mut out, "creator_known", d.creator_known);
    kv(&mut out, "bundle_wallets", d.bundle_wallets);
    kv(&mut out, "wash_ratio", d.wash_ratio);
    out
}

/// Render the full `management_replay` user prompt exactly as the corpus stores it,
/// trailing newline included.
pub fn render_management(m: &ManagementBundle) -> String {
    let regime = Regime::for_venue(&m.venue);
    let clip = m.qty_pre * m.mark;
    let mut out = String::with_capacity(3072);
    out.push_str(
        "Decide the next action for a position you already hold. Only causal information \
         is shown.\n",
    );
    out.push_str(&format!("MINT: {}\n", m.mint));
    out.push_str(&format!("DECISION TIME (unix ms): {}\n", m.t_dec));
    out.push_str(&format!("STEP: {}\n\n", m.step));
    out.push_str("MARKET STATE (at decision time):\n");
    out.push_str(&format!("  venue: {}  market: {}\n", m.venue, m.market));
    out.push_str(&format!("  pool depth: {} SOL\n", py_g(m.depth_sol, 4)));
    out.push_str(&format!(
        "  mark price (lamports per raw token): {}\n",
        py_g(m.mark, 12)
    ));
    out.push_str(&format!(
        "  mark price (SOL per raw token): {}\n",
        py_g(m.mark / 1e9, 12)
    ));
    out.push_str(&format!(
        "  mark price (SOL per whole token): {}\n",
        py_g(m.mark * 1e6 / 1e9, 12)
    ));
    out.push_str(&format!("  {}\n\n", cost_line(clip, Some(m.depth_sol), regime)));
    out.push_str("POSITION STATE (at the decision instant, before any action):\n");
    out.push_str(&format!(
        "  entry price (lamports per raw token): {}\n",
        py_g(m.entry_px, 12)
    ));
    out.push_str(&format!("  unrealized PnL: {} bp\n", py_fixed(m.upnl_bp, 1)));
    out.push_str(&format!("  holding time: {} s\n", py_fixed(m.held_s, 0)));
    out.push_str(&format!(
        "  max favourable so far: {} bp\n",
        py_fixed(m.mfe_bp, 1)
    ));
    out.push_str(&format!("  max adverse so far: {} bp\n", py_fixed(m.mae_bp, 1)));
    out.push_str(&format!(
        "  inventory: {} raw tokens\n",
        py_g(m.qty_pre, 6)
    ));
    out.push_str(&format!("  cash: {} SOL\n", py_g(m.cash_pre, 6)));
    out.push_str(&format!(
        "  position value at mark: {} SOL\n",
        py_g(m.qty_pre * m.mark, 6)
    ));
    out.push_str(&render_enriched(&m.enriched));
    out.push('\n');
    out.push_str(&render_dev(&m.dev));
    out.push('\n');
    out.push_str(&crate::decision::flow_line(&m.flow, m.flow_no_prior));
    out.push_str(
        "\n\nChoose exactly one action: HOLD, ADD, REDUCE, EXIT. Answer in the fixed \
         format.\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flow() -> FlowState {
        FlowState {
            entrants_60s: 7,
            entrants_300s: 877,
            net_flow_sol_300s: -142.882533,
            fresh_wallet_share_300s: Some(1.0),
            flow_lookback_d: 7.0,
            sniper_share_300s: Some(0.0),
            bot_uniform_share_300s: Some(0.0),
            smart_entrants_300s: 0,
            smart_net_flow_sol_300s: 0.0,
            coentry_wallets_300s: 0,
            creator_trading_own_mint: true,
            entrant_fee_p90_lamports: Some(5000),
            entrant_cu_p50: Some(118392),
        }
    }

    fn bundle() -> ManagementBundle {
        ManagementBundle {
            mint: "ozuUP6r26BjZkgv8eGDR6ZPVLGqfDGvKF5MGKq4pump".into(),
            t_dec: 1788970583820,
            step: 0,
            venue: "pumpswap".into(),
            market: "graduated_amm".into(),
            depth_sol: 1864.65,
            mark: 0.0245422000546,
            entry_px: 0.023292756397,
            upnl_bp: 536.4,
            held_s: 120.0,
            mfe_bp: 19351.6,
            mae_bp: -2332.2,
            qty_pre: 21.4659,
            cash_pre: 0.5,
            enriched: EnrichedManagement {
                holders_at_t: Some(PyNum::Int(968)),
                top1_float_share: Some(PyNum::Float(0.68258)),
                holder_hhi: Some(PyNum::Float(0.566494)),
                mcap_sol_at_t: Some(PyNum::Float(24542.200055)),
                bundle_wallets: Some(PyNum::Int(1020)),
                round_trip_wallets: Some(PyNum::Int(12)),
                creator_past_launches: Some(PyNum::Int(0)),
                creator_known: Some(PyNum::Bool(true)),
                wash_ratio: Some(PyNum::Float(0.01217)),
            },
            dev: DevHistoryManagement {
                creator_past_launches: Some(PyNum::Int(0)),
                creator_known: Some(PyNum::Bool(true)),
                bundle_wallets: Some(PyNum::Int(1020)),
                wash_ratio: Some(PyNum::Float(0.01217)),
            },
            flow: flow(),
            flow_no_prior: false,
        }
    }

    #[test]
    fn management_prompt_matches_the_hand_checked_corpus_row() {
        let s = render_management(&bundle());
        assert!(s.starts_with(
            "Decide the next action for a position you already hold. Only causal information is shown.\nMINT: ozuUP6r26BjZkgv8eGDR6ZPVLGqfDGvKF5MGKq4pump\nDECISION TIME (unix ms): 1788970583820\nSTEP: 0\n\n"
        ), "{s}");
        assert!(s.contains("  pool depth: 1865 SOL\n"));
        assert!(s.contains("  mark price (lamports per raw token): 0.0245422000546\n"));
        assert!(s.contains("  mark price (SOL per raw token): 2.45422000546e-11\n"));
        assert!(s.contains("  mark price (SOL per whole token): 2.45422000546e-05\n"));
        assert!(s.contains("for 0.53 SOL into 1865 SOL"), "{s}");
        assert!(s.contains("  inventory: 21.4659 raw tokens\n"));
        assert!(s.contains("  cash: 0.5 SOL\n"));
        assert!(s.contains(
            "ENRICHED CANDIDATE STATE: holders_at_t=968 top1_float_share=0.68258 \
             holder_hhi=0.566494 mcap_sol_at_t=24542.200055 bundle_wallets=1020 \
             round_trip_wallets=12 creator_past_launches=0 creator_known=True \
             wash_ratio=0.01217\n"
        ));
        assert!(s.contains(
            "DEV HISTORY: creator_past_launches=0 creator_known=True bundle_wallets=1020 \
             wash_ratio=0.01217\n"
        ));
        assert!(s.ends_with(
            "\n\nChoose exactly one action: HOLD, ADD, REDUCE, EXIT. Answer in the fixed format.\n"
        ));
    }

    #[test]
    fn a_dark_field_is_omitted_never_zeroed() {
        let e = EnrichedManagement {
            holders_at_t: Some(PyNum::Int(5)),
            ..Default::default()
        };
        assert_eq!(render_enriched(&e), "ENRICHED CANDIDATE STATE: holders_at_t=5");
        assert_eq!(render_dev(&DevHistoryManagement::default()), "DEV HISTORY:");
    }
}
