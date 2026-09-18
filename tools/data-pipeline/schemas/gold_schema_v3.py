"""
slinky_gold_v3 — Schema definitions for 4-layer gold dataset.
CORRECTED 2026-08-24: per-horizon censoring, multi-size exact curve economics.

CRITICAL LABEL CHANGE (v3):
  - champion config is NOT ground truth.
  - would_enter/policy_class from champion_v1 are NOT the primary y.
  - Qwen must discover profitable patterns the current bot misses.

FOUR LAYERS keyed by stable state_id:
  1. pump_state_v3     — ONLY causal info available at time t.
  2. pump_outcome_v3   — objective future truth (returns, MFE/MAE, barriers,
                         survival/collapse, graduation/migration, PER-HORIZON
                         censoring with observed-vs-censored distinction).
  3. counterfactual_trade_v3 — standardized feasible-entry economics for EVERY
                         eligible state regardless of champion decision.
                         Multi-size exact bonding-curve economics (0.05/0.1/
                         0.25/0.5/1 SOL) + exec_v3.0 benchmark (0.5 SOL).
                         Store continuous net PnL/return/risk FIRST; derive
                         STRONG/GOOD/MARGINAL/SKIP/BAD/TOXIC from economic
                         evidence, NOT champion_v1 entry.
  4. policy_eval_v3    — separately replay champion_v1 / current bot.
                         Store would_enter/action/config hash and compare vs
                         objective/counterfactual truth.
                         Auxiliary critique/context for Qwen.

CENSORING SEMANTICS (CORRECTED):
  CENSORING means future truth is unknowable because source observation ENDS
  before the requested horizon — NOT "no future trades." If source coverage
  reaches t+H and ZERO trades occur, that is an OBSERVED no-trade/illiquidity
  outcome, NOT censoring.

  For each H in [1,2,5,10,30,60,120,300]:
    observed_through_Hs: bool — does source coverage reach t+H?
    has_trade_within_Hs: bool — at least one trade in (t, t+H]?
    right_censored_Hs: bool   — NOT observed_through_Hs (true censoring)
    last_trade_age_at_Hs_s: Optional[float] — seconds since last trade at t+H

  Venue/coverage censoring: if mint migrates and post-migration trades are
  absent from source data, mark venue_censored for horizons past migration.

UNIT AUDIT:
  Raw monetary/token values get explicit names+units:
    *_lamports  int64  — SOL amounts in lamports (1 SOL = 1e9 lamports)
    *_raw       int64  — raw token amounts (not humanized)
  Derived SOL/token floats stored separately.

  Source field unit mapping (VERIFIED 2026-08-24):
    v_sol_bonding_curve    — LAMPORTS (p50=30.8e9, NOT SOL float)
    v_tokens_bonding_curve — raw micro-token units (~1e15)
    sol_amount             — SOL float (p50=0.097)
    market_cap_sol         — SOL float
    price_sol              — SOL float (very small, e.g. 2.8e-08)
    token_amount           — human token units (float)

  CONSTANT-PRODUCT CURVE (VERIFIED):
    k = v_sol_lamports * v_tok_micro is CONSTANT per mint (0% variance).
    price_sol = v_sol_lamports / (v_tok_micro * 1000.0), error ~1e-16.
    Buy size_sol: new_v_sol = v_sol + size_lam; new_v_tok = k/new_v_sol;
                  tokens_out = v_tok - new_v_tok.
    Sell tok_micro: new_v_tok = v_tok + tok; new_v_sol = k/new_v_tok;
                    sol_out_lam = v_sol - new_v_sol; sol_out = sol_out_lam/1e9.
"""

from __future__ import annotations
from dataclasses import dataclass, field
from typing import Optional
import hashlib

# ─── Provenance ──────────────────────────────────────────────────────

SCHEMA_VERSION = "3.1.0"
GENERATOR_VERSION = "slinky_gold_v3"
PIPELINE_VERSION = "3.1.0"
PRODUCER_VERSION = "slinky21"

# Horizons for per-horizon censoring and markout
HORIZONS_S = [1, 2, 5, 10, 30, 60, 120, 300]

# Entry sizes for multi-size exact curve economics
ENTRY_SIZES_SOL = [0.05, 0.10, 0.25, 0.50, 1.00]

# exec_v3.0 benchmark assumptions (0.5 SOL standardized)
EXEC_ASSUMPTIONS_V3 = {
    "version": "exec_v3.0",
    "entry_size_sol": 0.5,
    "latency_ms": 250,
    "entry_fee_bps": 100,
    "exit_fee_bps": 100,
    "slippage_default_bp": 50,
    "tp_target_bp": 1500,
    "stop_loss_bp": 1500,
    "max_hold_seconds": 300,
}

def stable_id(source: str, mint: str, event_time_ms: str, seq: str) -> str:
    """Deterministic state_id from source+mint+time+seq."""
    raw = f"{source}|{mint}|{event_time_ms}|{seq}"
    return hashlib.sha256(raw.encode()).hexdigest()[:16]

def config_hash(cfg: dict) -> str:
    import json
    return hashlib.sha256(
        json.dumps(cfg, sort_keys=True).encode()
    ).hexdigest()[:16]


# ─── Layer 1: pump_state_v3 — causal features only ──────────────────

@dataclass
class PumpStateV3:
    # ─── Provenance ─────────────────────────────────────────────
    state_id: str
    pipeline_version: str
    producer_version: str
    run_uuid: str
    source_hash: str
    code_config_hash: str
    git_sha: str

    # ─── Identity ───────────────────────────────────────────────
    mint: str
    event_time_unix_ms: int
    seq: int

    # ─── Venue / source ────────────────────────────────────────
    venue: str
    source: str

    # ─── Raw monetary values (explicit units) ──────────────────
    v_sol_bonding_curve_lamports: int
    v_tokens_bonding_curve_raw: int
    sol_amount_lamports: int
    token_amount_raw: int
    market_cap_sol_lamports: int
    price_sol_lamports: int

    # ─── Derived floats (separate from raw int64) ──────────────
    v_sol_bonding_curve_sol: float
    sol_amount_sol: float
    market_cap_sol: float
    price_sol: float
    token_amount_tokens: float

    # ─── Bonding curve state ──────────────────────────────────
    curve_pct_depleted: float

    # ─── Trade side ────────────────────────────────────────────
    trade_side: str
    is_buy: bool

    # ─── Causal cumulative stats (BEFORE this trade) ──────────
    trade_count_so_far: int
    buy_count_so_far: int
    sell_count_so_far: int
    buy_vol_sol: float
    sell_vol_sol: float
    total_vol_sol: float
    buy_pressure: Optional[float]
    net_flow_sol: Optional[float]
    unique_wallets_so_far: int

    # ─── Velocity / acceleration ──────────────────────────────
    trade_velocity_1s: Optional[float]
    trade_velocity_5s: Optional[float]
    trade_velocity_30s: Optional[float]
    vol_velocity_5s: Optional[float]
    vol_velocity_30s: Optional[float]
    vol_acceleration: Optional[float]
    buy_sell_imbalance_5s: Optional[float]
    buy_sell_imbalance_30s: Optional[float]

    # ─── Unique buyer/seller breadth ──────────────────────────
    unique_buyers_so_far: int
    unique_sellers_so_far: int
    unique_buyers_5s: Optional[int]
    unique_sellers_5s: Optional[int]

    # ─── Trade-size statistics ────────────────────────────────
    avg_trade_size_sol: Optional[float]
    median_trade_size_sol: Optional[float]
    max_trade_size_sol: Optional[float]
    min_trade_size_sol: Optional[float]
    trade_size_std_sol: Optional[float]
    largest_buy_pct_of_vol: Optional[float]
    largest_sell_pct_of_vol: Optional[float]

    # ─── Price / market-cap momentum + volatility ─────────────
    price_momentum_5s_bp: Optional[int]
    price_momentum_30s_bp: Optional[int]
    mcap_momentum_5s_bp: Optional[int]
    price_volatility_30s_bp: Optional[int]
    price_change_since_launch_bp: Optional[int]

    # ─── Reserve / curve depletion velocity ───────────────────
    curve_depletion_velocity_5s: Optional[float]
    curve_depletion_velocity_30s: Optional[float]
    v_sol_depletion_rate_5s: Optional[float]
    v_tokens_accumulation_rate_5s: Optional[float]

    # ─── Time since launch ────────────────────────────────────
    seconds_since_launch: float
    minutes_since_launch: float

    # ─── Graduation proximity ─────────────────────────────────
    is_graduated: bool
    seconds_to_graduation: Optional[float]
    graduation_proximity_pct: Optional[float]

    # ─── Liquidity ────────────────────────────────────────────
    liquidity_sol: Optional[float]
    liquidity_change_5s_sol: Optional[float]
    liquidity_change_30s_sol: Optional[float]

    # ─── Token metadata ───────────────────────────────────────
    token_name: Optional[str]
    token_symbol: Optional[str]
    creator: Optional[str]
    is_mayhem: Optional[bool]
    initial_supply_raw: Optional[int]
    initial_market_cap_sol: Optional[float]
    initial_price_sol: Optional[float]

    # ─── Creator activity / history ───────────────────────────
    creator_past_tokens: Optional[float]
    creator_past_rugs: Optional[float]
    dev_buy_pct_corrected: Optional[float]
    initial_holder_count: Optional[int]
    initial_gini: Optional[float]
    initial_top10_pct_corrected: Optional[float]
    top10_pct_suspect: Optional[bool]
    supply_bug_corrected: Optional[bool]

    # ─── Wallet concentration / cluster signals ───────────────
    buyer_concentration_ratio: Optional[float]
    seller_concentration_ratio: Optional[float]
    holder_concentration_hhi: Optional[float]
    top1_buyer_pct: Optional[float]
    top5_buyer_pct: Optional[float]
    repeat_buyer_ratio: Optional[float]
    repeat_seller_ratio: Optional[float]

    # ─── Repeated-wallet behavior ─────────────────────────────
    wallets_also_selling: Optional[int]
    avg_wallet_trade_count: Optional[float]

    # ─── Manipulation / sybil / toxic-flow indicators ────────
    sybil_cluster_size: Optional[int]
    coordinated_buy_ratio: Optional[float]
    wash_trade_ratio: Optional[float]
    rapid_rebuy_count: Optional[int]
    toxic_flow_indicator: Optional[float]

    # ─── Market / regime context ──────────────────────────────
    market_active_mints_5m: Optional[int]
    market_total_vol_5m_sol: Optional[float]
    market_avg_buy_pressure_5m: Optional[float]

    # ─── Data quality ─────────────────────────────────────────
    data_quality_score: Optional[float]
    is_zombie: Optional[bool]

    # NOTE: NO censoring fields on state layer — censoring is outcome-only.


# ─── Layer 2: pump_outcome_v3 — objective future truth ──────────────

@dataclass
class PumpOutcomeV3:
    # ─── Provenance ─────────────────────────────────────────────
    state_id: str
    pipeline_version: str
    run_uuid: str

    # ─── Identity ───────────────────────────────────────────────
    mint: str
    event_time_unix_ms: int

    # ─── Returns at horizons (in basis points, int) ────────────
    ret_1s_bp: Optional[int]
    ret_2s_bp: Optional[int]
    ret_5s_bp: Optional[int]
    ret_10s_bp: Optional[int]
    ret_30s_bp: Optional[int]
    ret_60s_bp: Optional[int]
    ret_120s_bp: Optional[int]
    ret_300s_bp: Optional[int]

    # ─── Continuous returns (float, for regression) ───────────
    ret_1s: Optional[float]
    ret_5s: Optional[float]
    ret_30s: Optional[float]
    ret_60s: Optional[float]
    ret_300s: Optional[float]

    # ─── MFE / MAE ────────────────────────────────────────────
    mfe_bp: Optional[int]
    mae_bp: Optional[int]
    mfe_time_seconds: Optional[float]
    mae_time_seconds: Optional[float]
    peak_bp: Optional[int]
    time_to_peak_seconds: Optional[float]

    # ─── First-hit barriers ───────────────────────────────────
    hit_plus_10_bp_time: Optional[float]
    hit_plus_25_bp_time: Optional[float]
    hit_plus_50_bp_time: Optional[float]
    hit_plus_100_bp_time: Optional[float]
    hit_plus_200_bp_time: Optional[float]
    hit_plus_500_bp_time: Optional[float]
    hit_plus_1000_bp_time: Optional[float]
    hit_plus_1500_bp_time: Optional[float]
    hit_plus_2000_bp_time: Optional[float]
    hit_plus_3000_bp_time: Optional[float]
    hit_minus_10_bp_time: Optional[float]
    hit_minus_20_bp_time: Optional[float]
    hit_minus_30_bp_time: Optional[float]
    hit_minus_50_bp_time: Optional[float]
    hit_minus_100_bp_time: Optional[float]
    hit_minus_200_bp_time: Optional[float]
    hit_minus_500_bp_time: Optional[float]
    hit_minus_1000_bp_time: Optional[float]
    hit_minus_1500_bp_time: Optional[float]
    hit_minus_2000_bp_time: Optional[float]

    # ─── Barrier precedence ───────────────────────────────────
    plus_100_before_minus_30: Optional[bool]

    # ─── Survival / collapse ──────────────────────────────────
    survived_60s: bool
    survived_300s: bool
    collapsed_50pct_within_300s: bool

    # ─── Graduation / migration ───────────────────────────────
    graduated_after_state: bool
    seconds_to_graduation: Optional[float]
    graduated_did_migrate: bool

    # ─── Post-graduation price ────────────────────────────────
    post_grad_price_change_300s_bp: Optional[int]

    # ─── Exit curve reserves (for multi-size exact exit economics) ──
    # Recorded at the exit trade (barrier-hit or time-stop trade).
    # NULL when no exit trade exists (observed no-trade or censored).
    exit_v_sol_lamports: Optional[int]
    exit_v_tok_raw: Optional[int]
    exit_event_time_ms: Optional[int]

    # ─── Per-horizon censoring (CORRECTED semantics) ──────────
    # For each H in [1,2,5,10,30,60,120,300]:
    #   observed_through_Hs: source coverage reaches t+H
    #   has_trade_within_Hs: at least one trade in (t, t+H]
    #   right_censored_Hs: NOT observed_through (true censoring)
    #   last_trade_age_at_Hs_s: seconds since last trade observed at t+H
    observed_through_1s: bool
    has_trade_within_1s: bool
    right_censored_1s: bool
    last_trade_age_at_1s_s: Optional[float]

    observed_through_2s: bool
    has_trade_within_2s: bool
    right_censored_2s: bool
    last_trade_age_at_2s_s: Optional[float]

    observed_through_5s: bool
    has_trade_within_5s: bool
    right_censored_5s: bool
    last_trade_age_at_5s_s: Optional[float]

    observed_through_10s: bool
    has_trade_within_10s: bool
    right_censored_10s: bool
    last_trade_age_at_10s_s: Optional[float]

    observed_through_30s: bool
    has_trade_within_30s: bool
    right_censored_30s: bool
    last_trade_age_at_30s_s: Optional[float]

    observed_through_60s: bool
    has_trade_within_60s: bool
    right_censored_60s: bool
    last_trade_age_at_60s_s: Optional[float]

    observed_through_120s: bool
    has_trade_within_120s: bool
    right_censored_120s: bool
    last_trade_age_at_120s_s: Optional[float]

    observed_through_300s: bool
    has_trade_within_300s: bool
    right_censored_300s: bool
    last_trade_age_at_300s_s: Optional[float]

    # ─── Venue / coverage censoring ───────────────────────────
    venue_censored: bool                      # true if mint migrated and post-mig data absent
    venue_censoring_reason: Optional[str]     # "migration_no_post_trade_data" | None
    observation_end_ms: int                   # when observation ends for this mint (global or migration)

    # ─── Markout availability / no-trade flags ───────────────
    time_to_next_trade_s: Optional[float]     # seconds until next trade (within 300s), NULL if none
    has_future_trade_300s: bool               # at least one trade within 300s (observed)


# ─── Layer 3: counterfactual_trade_v3 — multi-size exact economics ──

@dataclass
class CounterfactualTradeV3:
    # ─── Provenance ─────────────────────────────────────────────
    state_id: str
    pipeline_version: str
    run_uuid: str
    execution_assumptions_version: str        # "exec_v3.0" for the 0.5 SOL benchmark
    execution_config_hash: str

    # ─── Identity ───────────────────────────────────────────────
    mint: str
    event_time_unix_ms: int

    # ─── Entry economics — exec_v3.0 benchmark (0.5 SOL) ──────
    entry_price_sol: float
    entry_price_lamports: int
    entry_size_sol: float                      # 0.5 (benchmark)
    entry_size_lamports: int
    entry_fee_sol: float
    entry_fee_lamports: int
    entry_slippage_bp: int
    entry_slippage_sol: float
    entry_tip_lamports: int
    entry_total_cost_sol: float

    # ─── Exit economics — exec_v3.0 benchmark ─────────────────
    exit_reason: Optional[str]                 # "take_profit" | "stop_loss" | "time_stop" | "hold" | "no_exit"
    exit_price_sol: Optional[float]
    exit_price_lamports: Optional[int]
    exit_fee_sol: Optional[float]
    exit_fee_lamports: Optional[int]
    exit_slippage_bp: Optional[int]
    exit_slippage_sol: Optional[float]
    exit_tip_lamports: Optional[int]
    exit_total_revenue_sol: Optional[float]

    # ─── Continuous PnL / return / risk — exec_v3.0 benchmark ─
    gross_pnl_sol: Optional[float]
    gross_pnl_lamports: Optional[int]
    net_pnl_sol: Optional[float]
    net_pnl_lamports: Optional[int]
    net_return_bp: Optional[int]
    net_return_pct: Optional[float]
    hold_duration_seconds: Optional[float]
    risk_reward_ratio: Optional[float]

    # ─── Exact curve economics for exec_v3.0 (0.5 SOL) ────────
    # Computed from constant-product AMM math, NOT flat slippage.
    exact_entry_tokens: Optional[float]        # tokens bought via exact curve
    exact_entry_eff_price_sol: Optional[float] # effective entry price = size / tokens
    exact_entry_impact_bp: Optional[int]       # price impact vs spot
    exact_exit_sol_out: Optional[float]        # SOL received selling all tokens at exit curve
    exact_exit_impact_bp: Optional[int]        # sell price impact vs spot exit
    exact_gross_pnl_sol: Optional[float]       # exact_exit_sol - entry_size
    exact_gross_return_bp: Optional[int]       # exact gross return in bp
    exact_net_pnl_sol: Optional[float]         # after entry+exit fees
    exact_net_return_bp: Optional[int]

    # ─── Multi-size feasibility surface (policy-independent) ──
    # For each size in [0.05, 0.10, 0.25, 0.50, 1.00]:
    #   sz{SIZE}_entry_tokens, sz{SIZE}_entry_eff_price, sz{SIZE}_entry_impact_bp,
    #   sz{SIZE}_exit_sol, sz{SIZE}_exit_impact_bp,
    #   sz{SIZE}_gross_pnl, sz{SIZE}_gross_return_bp,
    #   sz{SIZE}_net_pnl, sz{SIZE}_net_return_bp, sz{SIZE}_feasible
    # Size codes: 005=0.05, 010=0.10, 025=0.25, 050=0.50, 100=1.00
    # (0.50 duplicates exec_v3.0 exact economics for convenience)

    sz005_entry_tokens: Optional[float]
    sz005_entry_eff_price_sol: Optional[float]
    sz005_entry_impact_bp: Optional[int]
    sz005_exit_sol: Optional[float]
    sz005_exit_impact_bp: Optional[int]
    sz005_gross_pnl_sol: Optional[float]
    sz005_gross_return_bp: Optional[int]
    sz005_net_pnl_sol: Optional[float]
    sz005_net_return_bp: Optional[int]
    sz005_feasible: bool

    sz010_entry_tokens: Optional[float]
    sz010_entry_eff_price_sol: Optional[float]
    sz010_entry_impact_bp: Optional[int]
    sz010_exit_sol: Optional[float]
    sz010_exit_impact_bp: Optional[int]
    sz010_gross_pnl_sol: Optional[float]
    sz010_gross_return_bp: Optional[int]
    sz010_net_pnl_sol: Optional[float]
    sz010_net_return_bp: Optional[int]
    sz010_feasible: bool

    sz025_entry_tokens: Optional[float]
    sz025_entry_eff_price_sol: Optional[float]
    sz025_entry_impact_bp: Optional[int]
    sz025_exit_sol: Optional[float]
    sz025_exit_impact_bp: Optional[int]
    sz025_gross_pnl_sol: Optional[float]
    sz025_gross_return_bp: Optional[int]
    sz025_net_pnl_sol: Optional[float]
    sz025_net_return_bp: Optional[int]
    sz025_feasible: bool

    sz050_entry_tokens: Optional[float]
    sz050_entry_eff_price_sol: Optional[float]
    sz050_entry_impact_bp: Optional[int]
    sz050_exit_sol: Optional[float]
    sz050_exit_impact_bp: Optional[int]
    sz050_gross_pnl_sol: Optional[float]
    sz050_gross_return_bp: Optional[int]
    sz050_net_pnl_sol: Optional[float]
    sz050_net_return_bp: Optional[int]
    sz050_feasible: bool

    sz100_entry_tokens: Optional[float]
    sz100_entry_eff_price_sol: Optional[float]
    sz100_entry_impact_bp: Optional[int]
    sz100_exit_sol: Optional[float]
    sz100_exit_impact_bp: Optional[int]
    sz100_gross_pnl_sol: Optional[float]
    sz100_gross_return_bp: Optional[int]
    sz100_net_pnl_sol: Optional[float]
    sz100_net_return_bp: Optional[int]
    sz100_feasible: bool

    # ─── Derived economic classification ──────────────────────
    # Summary ONLY. Qwen export must retain rich continuous outcomes above.
    # Derived from exact economics, NOT champion_v1.
    economic_class: str                        # "STRONG" | "GOOD" | "MARGINAL" | "SKIP" | "BAD" | "TOXIC"
    economic_class_reason: str

    # ─── Exit feasibility ─────────────────────────────────────
    exit_feasible: bool                        # True if exit has a real trade to sell into
    exit_feasibility_note: Optional[str]       # "liquid" | "illiquid_observed" | "coverage_censored"

    # ─── Eligibility ──────────────────────────────────────────
    eligible: bool
    eligibility_reason: Optional[str]

    # ─── Execution assumptions (versioned) ────────────────────
    latency_ms: int
    entry_fee_bps: int
    exit_fee_bps: int
    slippage_default_bp: int
    tp_target_bp: int
    stop_loss_bp: int
    max_hold_seconds: int


# ─── Layer 4: policy_eval_v3 — champion_v1 replay ──────────────────

@dataclass
class PolicyEvalV3:
    # ─── Provenance ─────────────────────────────────────────────
    state_id: str
    pipeline_version: str
    run_uuid: str
    config_hash: str
    policy_version: str

    # ─── Identity ───────────────────────────────────────────────
    mint: str
    event_time_unix_ms: int

    # ─── Champion decision ────────────────────────────────────
    would_enter: bool
    action: str                                 # "ENTER" | "SKIP"
    entry_reason: Optional[str]

    # ─── Comparison vs objective truth ────────────────────────
    evaluation: str                             # "CORRECT_ENTER" | "CORRECT_SKIP" | "FALSE_POSITIVE"
                                                # | "MISSED_OPPORTUNITY" | "AMBIGUOUS"
    evaluation_reason: str

    # ─── Entry gate conditions ────────────────────────────────
    gate_curve_pct_max: Optional[float]
    gate_min_trades: Optional[int]
    gate_min_volume_sol: Optional[float]
    gate_min_buy_pressure: Optional[float]
    gate_min_unique_buyers: Optional[int]
    gate_min_age_slots: Optional[int]
    gate_min_sol_per_trade: Optional[float]
    gate_max_sol_per_trade: Optional[float]

    # ─── If entered, what would have happened? ────────────────
    sim_net_pnl_sol: Optional[float]
    sim_exit_reason: Optional[str]
    sim_hold_duration: Optional[float]
