//! Golden-file parity: the rendered lines must match real c11 corpus rows
//! byte-for-byte. Fixtures are copied verbatim from `candidate_sft_c11`.

use pump_quant_proposal::{
    render_flow_citation, render_live_flow_state, render_no_prior_flow, FlowState,
};

/// A real decision-family row (mint BHnsBYhzNrsEowdAt2QQ7Z39bi4qNhKS54xDBvyApump,
/// t_dec_ms 1788970423162) — populated, positive net flow.
const LIVE_FLOW_STATE_HAPPY: &str = "LIVE FLOW STATE: entrants_60s=30 entrants_300s=30 net_flow_sol_300s=14.025214 fresh_wallet_share_300s=1.0 flow_lookback_d=7.0 sniper_share_300s=0.0 bot_uniform_share_300s=0.131258 smart_entrants_300s=0 smart_net_flow_sol_300s=0.0 coentry_wallets_300s=3 creator_trading_own_mint=True entrant_fee_p90_lamports=8000 entrant_cu_p50=103088";

const FLOW_CITATION_HAPPY: &str = "FLOW CITATION: entrants_60s=30 entrants_300s=30 net_flow_sol_300s=14.025214 fresh_wallet_share_300s=1.0 flow_lookback_d=7.0 sniper_share_300s=0.0 bot_uniform_share_300s=0.131258 smart_entrants_300s=0 smart_net_flow_sol_300s=0.0 coentry_wallets_300s=3 creator_trading_own_mint=True entrant_fee_p90_lamports=8000 entrant_cu_p50=103088";

/// A real row with zero entrants: negative net flow, every share/fee field `None`.
const LIVE_FLOW_STATE_NONE: &str = "LIVE FLOW STATE: entrants_60s=0 entrants_300s=0 net_flow_sol_300s=-1.583444 fresh_wallet_share_300s=None flow_lookback_d=7.0 sniper_share_300s=None bot_uniform_share_300s=None smart_entrants_300s=0 smart_net_flow_sol_300s=0.0 coentry_wallets_300s=0 creator_trading_own_mint=True entrant_fee_p90_lamports=None entrant_cu_p50=None";

/// A real row with negative net flow at a "short" 3-dp value (`-0.824`).
const LIVE_FLOW_STATE_NEG: &str = "LIVE FLOW STATE: entrants_60s=30 entrants_300s=30 net_flow_sol_300s=-0.824 fresh_wallet_share_300s=1.0 flow_lookback_d=7.0 sniper_share_300s=0.0 bot_uniform_share_300s=0.0 smart_entrants_300s=0 smart_net_flow_sol_300s=0.0 coentry_wallets_300s=3 creator_trading_own_mint=True entrant_fee_p90_lamports=8000 entrant_cu_p50=104242";

fn happy() -> FlowState {
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

fn none_case() -> FlowState {
    FlowState {
        entrants_60s: 0,
        entrants_300s: 0,
        net_flow_sol_300s: -1.583444,
        fresh_wallet_share_300s: None,
        flow_lookback_d: 7.0,
        sniper_share_300s: None,
        bot_uniform_share_300s: None,
        smart_entrants_300s: 0,
        smart_net_flow_sol_300s: 0.0,
        coentry_wallets_300s: 0,
        creator_trading_own_mint: true,
        entrant_fee_p90_lamports: None,
        entrant_cu_p50: None,
    }
}

fn neg_flow() -> FlowState {
    FlowState {
        entrants_60s: 30,
        entrants_300s: 30,
        net_flow_sol_300s: -0.824,
        fresh_wallet_share_300s: Some(1.0),
        flow_lookback_d: 7.0,
        sniper_share_300s: Some(0.0),
        bot_uniform_share_300s: Some(0.0),
        smart_entrants_300s: 0,
        smart_net_flow_sol_300s: 0.0,
        coentry_wallets_300s: 3,
        creator_trading_own_mint: true,
        entrant_fee_p90_lamports: Some(8000),
        entrant_cu_p50: Some(104242),
    }
}

#[test]
fn live_flow_state_happy_path_byte_for_byte() {
    assert_eq!(render_live_flow_state(&happy()), LIVE_FLOW_STATE_HAPPY);
}

#[test]
fn flow_citation_happy_path_byte_for_byte() {
    assert_eq!(render_flow_citation(&happy()), FLOW_CITATION_HAPPY);
}

#[test]
fn live_flow_state_none_case_byte_for_byte() {
    assert_eq!(render_live_flow_state(&none_case()), LIVE_FLOW_STATE_NONE);
}

#[test]
fn live_flow_state_negative_flow_byte_for_byte() {
    assert_eq!(render_live_flow_state(&neg_flow()), LIVE_FLOW_STATE_NEG);
}

#[test]
fn no_prior_flow_matches_corpus() {
    assert_eq!(
        render_no_prior_flow(),
        "LIVE FLOW STATE: no_prior_flow=true"
    );
}

/// The shared cost model is verbatim in all four system prompts; each family
/// carries its own action vocabulary and answer-format tail.
const COST_CORE: &str = "The venue fee is 94.63 bp per side on the bonding curve (measured over 65,927 swaps) and 30 bp per side on the PumpSwap AMM";

#[test]
fn system_prompts_carry_shared_cost_core() {
    use pump_quant_proposal::{system_prompt, PromptFamily::*};
    for f in [Decision, Management, UtilityReasoning, UtilityRegression] {
        assert!(
            system_prompt(f).contains(COST_CORE),
            "family {f:?} missing cost core"
        );
    }
}

#[test]
fn system_prompts_have_family_specific_tails() {
    use pump_quant_proposal::{system_prompt, PromptFamily::*};
    assert!(system_prompt(Decision).contains("BUY, WATCH or SKIP"));
    assert!(system_prompt(Decision).ends_with("COUNTEREVIDENCE, EVIDENCE_STATUS."));
    assert!(system_prompt(Management).contains("HOLD, ADD, REDUCE, EXIT"));
    assert!(system_prompt(Management).ends_with("Answer in the fixed format."));
    assert!(
        system_prompt(UtilityReasoning).contains("Show the arithmetic. Never use any future price")
    );
    assert!(system_prompt(UtilityRegression).contains("FORECAST_NET_BP, FORECAST_MFE_BP, FORECAST_MAE_BP, FORECAST_OUTCOME, FORECAST_CENSORED, BASIS, EVIDENCE_STATUS."));
}
