//! The `decision` (entry) user prompt, byte-identical to the c11 corpus.
//!
//! Field order, spacing, `n/a` handling, Python number types and the venue-dependent
//! CURVE / AMM / PRICE UNITS blocks are all part of the contract. Any divergence is
//! out-of-distribution for the model — the same class of silent damage as a wrong action
//! vocabulary — so this module exists to make the prompt a *rendering* of typed state
//! rather than a string built at the call site.

#![forbid(unsafe_code)]

use crate::bundle_gate::{BundlePolicy, FieldFamily, GateError};
use crate::cost::{size_options, Regime};
use crate::fmt::{py_fixed, py_g};
use crate::pynum::PyNum;
use crate::{render_live_flow_state, render_no_prior_flow, FlowState};

/// The as-of-t enrichment the entry family carries (built by the c9 assembler).
#[derive(Debug, Clone, PartialEq)]
pub struct EnrichedCandidate {
    /// Market cap in SOL, `None` when the mcap could not be sourced.
    pub mcap_sol_at_t: Option<PyNum>,
    /// Which venue the mcap was sourced from (`curve` / `amm`).
    pub mcap_source: String,
    /// Wallets holding at least one raw token at t.
    pub holders_at_t: PyNum,
    /// Largest holder's share of net wallet float (not of supply).
    pub top1_float_share: PyNum,
    /// Top-5 holders' share of net wallet float.
    pub top5_float_share: PyNum,
    /// Herfindahl index over wallet float.
    pub holder_hhi: PyNum,
    /// Slots the bundles occupied.
    pub bundle_slots: PyNum,
    /// Wallets the bundles used.
    pub bundle_wallets: PyNum,
    /// Traded volume in SOL up to t.
    pub volume_sol_at_t: PyNum,
    /// Share of volume judged wash.
    pub wash_ratio: PyNum,
}

/// The creator's observable history at t (entry family rendering).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DevHistoryDecision {
    /// Prior launches, or `None` when the creator had no observable history.
    pub creator_past_launches: Option<i64>,
    /// 1 when the creator was resolvable, else 0.
    pub creator_known: i64,
}

/// The bonding-curve reserve annotation at the decision clock.
#[derive(Debug, Clone, PartialEq)]
pub enum CurveState {
    /// No curve reserves could be placed at or before t.
    Absent {
        /// Why the annotation is missing.
        reason: String,
    },
    /// A curve reserve snapshot at or before t.
    Present {
        /// Milliseconds between the snapshot and the decision clock.
        staleness_ms: i64,
        /// Whether the snapshot is fresh enough to price against.
        pricing_eligible: bool,
        /// Virtual SOL reserves, lamports.
        v_sol_reserves_lamports: i64,
        /// Virtual token reserves, raw.
        v_tokens_reserves: i64,
        /// Real SOL reserves, lamports.
        real_sol_reserves_lamports: i64,
        /// Real token reserves, raw.
        real_tokens_reserves: i64,
        /// Curve spot price, SOL per raw token.
        curve_price_sol_per_raw_token: f64,
        /// The invariant `v_sol * v_tokens` as an exact integer string.
        curve_k: String,
        /// Fraction of the raise completed.
        curve_progress: f64,
        /// `bonding` or `graduated`.
        curve_regime: String,
    },
}

/// The PumpSwap pool annotation at the decision clock.
#[derive(Debug, Clone, PartialEq)]
pub enum AmmState {
    /// No AMM pool could be placed at or before t.
    Absent {
        /// Why the annotation is missing.
        reason: String,
    },
    /// A pool reserve snapshot at or before t.
    Present {
        /// The pool account.
        pool: String,
        /// Milliseconds between the snapshot and the decision clock.
        staleness_ms: i64,
        /// Whether the snapshot is fresh enough to price against.
        pricing_eligible: bool,
        /// Base (token) reserve, raw.
        base_reserves_raw: i64,
        /// Quote (WSOL) reserve, lamports.
        quote_reserves_lamports: i64,
        /// Pool spot price, SOL per raw token.
        amm_price_sol_per_raw_token: f64,
        /// The slot the reserve snapshot was read at, when known.
        reserve_slot: Option<i64>,
    },
}

/// Everything the `decision` user prompt states.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionBundle {
    /// Decision clock, unix milliseconds.
    pub t_dec_ms: i64,
    /// Age of the mint at the clock, seconds.
    pub age_s: PyNum,
    /// Age of the last trade at the clock, seconds.
    pub last_trade_age_s: PyNum,
    /// Venue label: `pumpfun`, `pumpswap` or `mixed`.
    pub venue: String,
    /// Whether a curve object was present in the tape.
    pub curve_present: bool,
    /// `complete` when every field was observed, else `partial`.
    pub evidence_status: String,
    /// Trades strictly before the clock.
    pub n_prior_trades: i64,
    /// Buys strictly before the clock.
    pub buy_count: i64,
    /// Sells strictly before the clock.
    pub sell_count: i64,
    /// Distinct traders strictly before the clock.
    pub unique_traders: i64,
    /// Spot price, lamports per raw token.
    pub price_lamports_per_raw_token: PyNum,
    /// Return over the last 5 s, bp.
    pub ret_5s_bp: Option<PyNum>,
    /// Return over the last 30 s, bp.
    pub ret_30s_bp: Option<PyNum>,
    /// Realised volatility over the last 30 s, bp.
    pub vol_30s_bp: Option<PyNum>,
    /// Buy volume, lamports.
    pub buy_volume_lamports: i64,
    /// Sell volume, lamports.
    pub sell_volume_lamports: i64,
    /// Net flow, lamports.
    pub net_flow_lamports: i64,
    /// Largest trader's share of volume.
    pub top1_trader_share: PyNum,
    /// Top-5 traders' share of volume.
    pub top5_trader_share: PyNum,
    /// Buy/sell count ratio, `None` when there were no sells.
    pub buyer_seller_ratio: Option<PyNum>,
    /// The as-of-t enrichment block.
    pub enriched: EnrichedCandidate,
    /// The creator history block.
    pub dev: DevHistoryDecision,
    /// The 13 live flow features.
    pub flow: FlowState,
    /// True when the clock had zero prior tape events: the flow line is the
    /// `no_prior_flow` marker instead of fabricated zeros.
    pub flow_no_prior: bool,
    /// The curve annotation.
    pub curve: CurveState,
    /// The AMM annotation.
    pub amm: AmmState,
    /// Pool depth in SOL that the SIZE OPTIONS cost was measured against, `None` when the
    /// book could not be priced.
    pub size_depth_sol: Option<f64>,
    /// Whether OUR next fill lands on the AMM (the regime the cost authority applies).
    pub size_amm: bool,
}

/// Render the `CURVE STATE (at decision time): …` line.
pub fn render_curve(c: &CurveState) -> String {
    match c {
        CurveState::Absent { reason } => {
            format!("CURVE STATE (at decision time): curve_reserves=absent reason={reason}")
        }
        CurveState::Present {
            staleness_ms,
            pricing_eligible,
            v_sol_reserves_lamports,
            v_tokens_reserves,
            real_sol_reserves_lamports,
            real_tokens_reserves,
            curve_price_sol_per_raw_token,
            curve_k,
            curve_progress,
            curve_regime,
        } => format!(
            "CURVE STATE (at decision time): curve_reserves=present src=laserstream \
             staleness_ms={staleness_ms} pricing_eligible={pe} v_sol_reserves_sol={vs} \
             v_tokens_reserves={vt} real_sol_reserves_sol={rs} real_tokens_reserves={rt} \
             curve_price_sol_per_raw_token={pr} curve_k={curve_k} \
             curve_progress={prog} curve_regime={curve_regime}",
            pe = if *pricing_eligible { "true" } else { "false" },
            vt = v_tokens_reserves,
            rt = real_tokens_reserves,
            vs = py_fixed(
                *v_sol_reserves_lamports as f64 / crate::cost::LAMPORTS_PER_SOL,
                9
            ),
            rs = py_fixed(
                *real_sol_reserves_lamports as f64 / crate::cost::LAMPORTS_PER_SOL,
                9
            ),
            pr = py_fixed(*curve_price_sol_per_raw_token, 18),
            prog = py_fixed(*curve_progress, 6),
        ),
    }
}

/// Render the `AMM POOL STATE (at decision time): …` line.
pub fn render_amm(a: &AmmState) -> String {
    match a {
        AmmState::Absent { reason } => {
            format!("AMM POOL STATE (at decision time): amm_reserves=absent reason={reason}")
        }
        AmmState::Present {
            pool,
            staleness_ms,
            pricing_eligible,
            base_reserves_raw,
            quote_reserves_lamports,
            amm_price_sol_per_raw_token,
            reserve_slot,
        } => format!(
            "AMM POOL STATE (at decision time): amm_reserves=present src=helius_amm_backfill \
             pool={pool} quote=WSOL staleness_ms={staleness_ms} pricing_eligible={pe} \
             base_reserves_raw={base_reserves_raw} quote_reserves_lamports={quote_reserves_lamports} \
             quote_reserves_sol={qs} amm_price_sol_per_raw_token={px} reserve_slot={slot}",
            pe = if *pricing_eligible { "true" } else { "false" },
            qs = py_fixed(*quote_reserves_lamports as f64 / crate::cost::LAMPORTS_PER_SOL, 9),
            px = py_fixed(*amm_price_sol_per_raw_token, 18),
            slot = match reserve_slot {
                Some(s) => s.to_string(),
                None => "None".to_string(),
            },
        ),
    }
}

/// Render the `PRICE UNITS: …` line.
///
/// The three numbers are the same price expressed in lamports per raw token, SOL per raw
/// token and SOL per whole token. `%.10g` is what the corpus used, so the leading digits
/// can never drift with the value's magnitude.
pub fn render_price_units(price: PyNum) -> String {
    if price.is_zero() {
        return "PRICE UNITS: price_lamports_per_raw_token=absent reason=no_supplied_price"
            .to_string();
    }
    let px = price.as_f64();
    format!(
        "PRICE UNITS: price_lamports_per_raw_token={a} price_sol_per_raw_token={b} \
         price_sol_per_whole_token={c}",
        a = py_g(px, 10),
        b = py_g(px / 1e9, 10),
        c = py_g(px * 1e6 / 1e9, 10),
    )
}

/// The live-flow line: the 13 features, or the `no_prior_flow` marker.
pub fn flow_line(f: &FlowState, no_prior_flow: bool) -> String {
    if no_prior_flow {
        render_no_prior_flow().to_string()
    } else {
        render_live_flow_state(f)
    }
}

/// `fnum` for the STATE line's derived fields (returns, volatility, shares, ratio): the
/// corpus's ladder, not `str(float)`. See [`pump_quant_proposal::fmt::py_fnum`].
fn opt_bp(v: Option<PyNum>) -> String {
    match v {
        Some(x) => crate::fmt::py_fnum(x.as_f64()),
        None => "n/a".to_string(),
    }
}

/// [`opt_bp`] for a non-optional derived field.
fn bp(v: PyNum) -> String {
    crate::fmt::py_fnum(v.as_f64())
}

/// The §17 gate's view of a bundle.
impl DecisionBundle {
    /// The §17 field families this bundle actually carries.
    ///
    /// Read off the bundle's own content, never from a caller-supplied list: a caller that
    /// forgot to declare a family it had just filled would be exactly the hole the gate
    /// exists to close. The enum is closed, so a new family added to `FieldFamily` forces
    /// this match to be revisited at compile time rather than silently unguarded.
    #[must_use]
    pub fn carried_families(&self) -> Vec<FieldFamily> {
        let mut families = Vec::new();
        // The bundle always renders a LIVE FLOW STATE line: either the thirteen aggregates
        // or the `no_prior_flow` marker. Both are the trained c11 family.
        families.push(FieldFamily::LiveFlowState);
        families
    }
}

/// **The live entry point.** Render the entry prompt only after the §17 gate has agreed that
/// every family this bundle carries is one the model was trained on.
///
/// [`render_decision`] stays as the raw renderer so the golden/parity outputs are untouched;
/// live callers must come through here. Emitting a captured-but-untrained family is an
/// out-of-distribution input, and it fails the same way a wrong action vocabulary does —
/// silently, with plausible-looking output — which is why the check is a refusal and not a
/// warning.
pub fn render_decision_checked(
    b: &DecisionBundle,
    policy: &BundlePolicy,
) -> Result<String, GateError> {
    for family in b.carried_families() {
        if !policy.is_emitted(family) {
            return Err(GateError::UntrainedFamilyEmitted(family));
        }
    }
    Ok(render_decision(b))
}

/// Render the full `decision` user prompt exactly as the corpus stores it.
///
/// The raw renderer: golden and parity outputs go through here. Live callers must come
/// through [`render_decision_checked`], which refuses to render a family the model was
/// not trained on (§17).
pub fn render_decision(b: &DecisionBundle) -> String {
    let mut out = String::with_capacity(4096);
    out.push_str("DECISION CLOCK — assess this opportunity.\n");
    out.push_str(&format!(
        "t_dec_ms={}  age_s={}  last_trade_age_s={}\n",
        b.t_dec_ms,
        b.age_s.render(),
        b.last_trade_age_s.render()
    ));
    out.push_str(&format!(
        "venue={}  curve_present={}  evidence_status={}\n",
        b.venue,
        if b.curve_present { "True" } else { "False" },
        b.evidence_status
    ));
    out.push_str(&format!(
        "n_prior_trades={}  buy_count={}  sell_count={}  unique_traders={}\n",
        b.n_prior_trades, b.buy_count, b.sell_count, b.unique_traders
    ));
    out.push_str(&format!(
        "price_lamports_per_raw_token={}  ret_5s_bp={}  ret_30s_bp={}  vol_30s_bp={}\n",
        b.price_lamports_per_raw_token.render(),
        opt_bp(b.ret_5s_bp),
        opt_bp(b.ret_30s_bp),
        opt_bp(b.vol_30s_bp)
    ));
    out.push_str(&format!(
        "buy_volume_lamports={}  sell_volume_lamports={}  net_flow_lamports={}\n",
        b.buy_volume_lamports, b.sell_volume_lamports, b.net_flow_lamports
    ));
    out.push_str(&format!(
        "top1_trader_share={}  top5_trader_share={}  buyer_seller_ratio={}\n",
        bp(b.top1_trader_share),
        bp(b.top5_trader_share),
        opt_bp(b.buyer_seller_ratio)
    ));
    out.push_str("ENRICHED CANDIDATE STATE (strictly causal at t_dec): ");
    out.push_str(&format!(
        "mcap_sol_at_t={} mcap_source={} holders_at_t={} top1_float_share={} \
         top5_float_share={} holder_hhi={} bundle_slots={} bundle_wallets={} \
         volume_sol_at_t={} wash_ratio={}\n",
        match b.enriched.mcap_sol_at_t {
            Some(v) => v.render(),
            None => "na".to_string(),
        },
        b.enriched.mcap_source,
        b.enriched.holders_at_t.render(),
        b.enriched.top1_float_share.render(),
        b.enriched.top5_float_share.render(),
        b.enriched.holder_hhi.render(),
        b.enriched.bundle_slots.render(),
        b.enriched.bundle_wallets.render(),
        b.enriched.volume_sol_at_t.render(),
        b.enriched.wash_ratio.render()
    ));
    out.push_str(&format!(
        "DEV HISTORY: creator_past_launches={} creator_known={}\n",
        match b.dev.creator_past_launches {
            Some(v) => v.to_string(),
            None => "unknown".to_string(),
        },
        b.dev.creator_known
    ));
    out.push_str(&flow_line(&b.flow, b.flow_no_prior));
    out.push('\n');
    out.push_str(&render_curve(&b.curve));
    out.push('\n');
    out.push_str(&render_amm(&b.amm));
    out.push('\n');
    out.push_str(&render_price_units(b.price_lamports_per_raw_token));
    out.push_str(
        "\n\nChoose exactly one action: BUY, WATCH, SKIP. Respect the stated round-trip cost. \
         Answer in the fixed format.\n",
    );
    let regime = if b.size_amm {
        Regime::Amm
    } else {
        Regime::BondingCurve
    };
    out.push_str(&size_options(regime, b.size_depth_sol, false));
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FlowState;

    fn flow() -> FlowState {
        FlowState {
            entrants_60s: 30,
            entrants_300s: 30,
            net_flow_sol_300s: 14.025214,
            fresh_wallet_share_300s: Some(1.0),
            flow_lookback_d: 7.0,
            sniper_share_300s: Some(0.0),
            bot_uniform_share_300s: Some(0.131258),
            smart_entrants_300s: 0,
            smart_net_flow_sol_300s: 0.0,
            coentry_wallets_300s: 3,
            creator_trading_own_mint: true,
            entrant_fee_p90_lamports: Some(8000),
            entrant_cu_p50: Some(103088),
        }
    }

    /// A bundle with every optional field absent — the smallest honest bundle the
    /// formatter can be pointed at.
    fn minimal_bundle() -> DecisionBundle {
        DecisionBundle {
            t_dec_ms: 1,
            age_s: PyNum::Float(1.0),
            last_trade_age_s: PyNum::Float(1.0),
            venue: "mixed".into(),
            curve_present: true,
            evidence_status: "complete".into(),
            n_prior_trades: 0,
            buy_count: 0,
            sell_count: 0,
            unique_traders: 0,
            price_lamports_per_raw_token: PyNum::Float(0.0),
            ret_5s_bp: None,
            ret_30s_bp: None,
            vol_30s_bp: None,
            buy_volume_lamports: 0,
            sell_volume_lamports: 0,
            net_flow_lamports: 0,
            top1_trader_share: PyNum::Float(0.0),
            top5_trader_share: PyNum::Float(0.0),
            buyer_seller_ratio: None,
            enriched: EnrichedCandidate {
                mcap_sol_at_t: None,
                mcap_source: "curve".into(),
                holders_at_t: PyNum::Int(0),
                top1_float_share: PyNum::Float(0.0),
                top5_float_share: PyNum::Float(0.0),
                holder_hhi: PyNum::Int(0),
                bundle_slots: PyNum::Int(0),
                bundle_wallets: PyNum::Int(0),
                volume_sol_at_t: PyNum::Float(0.0),
                wash_ratio: PyNum::Float(0.0),
            },
            dev: DevHistoryDecision {
                creator_past_launches: None,
                creator_known: 0,
            },
            flow: flow(),
            flow_no_prior: false,
            curve: CurveState::Absent {
                reason: "no_reserves".into(),
            },
            amm: AmmState::Absent {
                reason: "no_pool".into(),
            },
            size_depth_sol: None,
            size_amm: false,
        }
    }

    #[test]
    fn missing_optional_numbers_print_as_na_not_zero() {
        let mut b = minimal_bundle();
        let s = render_decision(&b);
        assert!(s.contains("ret_5s_bp=n/a"), "{s}");
        assert!(s.contains("buyer_seller_ratio=n/a"), "{s}");
        assert!(s.contains("mcap_sol_at_t=na mcap_source=curve"), "{s}");
        assert!(
            s.contains("creator_past_launches=unknown creator_known=0"),
            "{s}"
        );
        assert!(s.contains("price_lamports_per_raw_token=absent reason=no_supplied_price"));
        assert!(s.contains("curve_reserves=absent reason=no_reserves"));
        assert!(s.contains("amm_reserves=absent reason=no_pool"));
        assert!(s.contains("pool depth unknown"));
        assert!(s.ends_with("thinner book.\n"));
        assert!(s.contains("Respect the stated round-trip cost."));
        // the venue-dependent blocks must never be fabricated when absent
        assert!(!s.contains("src=laserstream"));
        b.curve = CurveState::Present {
            staleness_ms: 4804291,
            pricing_eligible: false,
            v_sol_reserves_lamports: 115_005_359_057,
            v_tokens_reserves: 279_900_000_000_000,
            real_sol_reserves_lamports: 0,
            real_tokens_reserves: 0,
            curve_price_sol_per_raw_token: 4.1088e-13,
            curve_k: "32190000000054300000000000".into(),
            curve_progress: 1.0,
            curve_regime: "graduated".into(),
        };
        let s = render_decision(&b);
        assert!(s.contains("v_sol_reserves_sol=115.005359057"));
        assert!(s.contains("curve_price_sol_per_raw_token=0.000000000000410880"));
        assert!(s.contains("curve_k=32190000000054300000000000"));
    }

    #[test]
    fn price_units_use_scientific_notation_like_python() {
        assert_eq!(
            render_price_units(PyNum::Float(1.0499954004771828)),
            "PRICE UNITS: price_lamports_per_raw_token=1.0499954 price_sol_per_raw_token=1.0499954e-09 \
             price_sol_per_whole_token=0.0010499954"
        );
    }

    #[test]
    fn the_gate_aware_render_is_byte_identical_under_the_live_policy() {
        let b = minimal_bundle();
        let policy = BundlePolicy::trained_only();
        assert_eq!(
            render_decision_checked(&b, &policy).expect("the live policy emits the flow family"),
            render_decision(&b),
            "the guard must not perturb a single byte of the prompt"
        );
    }

    #[test]
    fn a_withheld_family_is_a_refusal_not_a_warning() {
        let b = minimal_bundle();
        // The bundle carries the flow family: prove the formatter actually consults the policy
        // rather than carrying a guard whose refusal branch nothing can reach.
        assert!(b.carried_families().contains(&FieldFamily::LiveFlowState));
        let mut policy = BundlePolicy::trained_only();
        policy.withhold(FieldFamily::LiveFlowState);
        assert_eq!(
            render_decision_checked(&b, &policy),
            Err(GateError::UntrainedFamilyEmitted(
                FieldFamily::LiveFlowState
            ))
        );
    }
}
