"""
pump_state_v1 / pump_outcome_v1 / simulator_label_v1 — canonical gold schemas.
Shared by slinky_gold_v1 and laserstream_gold_v1.

DESIGN PRINCIPLES (from task spec):
  - Stable IDs: deterministic, content-hashed, never reused.
  - Integer / raw units: lamports and token base units as u64/i64. No floats for money.
  - No future leakage: pump_state_v1 contains ONLY info knowable at time t.
  - Objective truth (pump_outcome_v1) is INDEPENDENT of policy labels (simulator_label_v1).
  - Right-censored states are marked, not fabricated.
  - Versioned: every schema carries schema_version + generator_version.
"""

from __future__ import annotations
import hashlib, json, struct
from dataclasses import dataclass, field, asdict
from typing import Optional, List
from datetime import datetime

# ─── Schema versions ──────────────────────────────────────────────────
SCHEMA_VERSION = "1"
GENERATOR_VERSION = "1.0.0"

# ─── Stable ID generation ─────────────────────────────────────────────
def stable_id(*parts: str) -> str:
    """Deterministic content-hashed ID. Never reuses, never collides
    unless the inputs are identical."""
    h = hashlib.blake2b("|".join(str(p) for p in parts).encode(), digest_size=16)
    return h.hexdigest()

# ─── pump_state_v1 ────────────────────────────────────────────────────
# One row per causal observation of a token's bonding-curve state at time t.
# Contains ONLY info knowable at/before time t. No hindsight.

@dataclass
class PumpStateV1:
    # ── Identity ──
    state_id: str              # stable_id(source, mint, event_time_unix_ms, seq)
    source: str                # "slinky21" | "laserstream"
    mint: str                  # base58 mint address
    mint_b58: str              # alias for clarity
    event_time_unix_ms: int    # when this observation occurred
    event_slot: Optional[int]  # Solana slot (None for Slinky snapshots)
    seq: int                   # per-mint monotonic sequence (0 = first observation)
    venue: str                 # "pumpfun_bonding" | "pumpswap" | "unknown"

    # ── Curve state (causal: knowable at time t) ──
    virtual_sol: Optional[int]    # lamports
    virtual_token: Optional[int]  # base units
    real_sol: Optional[int]       # lamports
    real_token: Optional[int]     # base units
    curve_pct_depleted: Optional[float]  # derived, but causal
    is_complete: Optional[bool]
    market_cap_sol: Optional[int]  # lamports (causal: derived from curve reserves)

    # ── Trade context (causal: this trade or aggregate up to t) ──
    trade_side: Optional[str]      # "buy" | "sell" | None (for non-trade states)
    amount_in: Optional[int]       # lamports or token base units
    amount_out: Optional[int]
    fee_bps: Optional[int]
    trade_count_so_far: Optional[int]
    buy_count_so_far: Optional[int]
    sell_count_so_far: Optional[int]

    # ── Token metadata (causal: from create event) ──
    token_name: Optional[str]
    token_symbol: Optional[str]
    creator: Optional[str]
    initial_supply: Optional[int]
    is_mayhem: Optional[bool]

    # ── Derived features (causal) ──
    seconds_since_launch: Optional[float]
    buy_pressure: Optional[float]       # buy_vol / (buy_vol + sell_vol) up to t
    net_flow_sol: Optional[int]         # cumulative net SOL inflow, lamports
    unique_wallets_so_far: Optional[int]

    # ── Censoring ──
    right_censored: bool = False        # True if forward horizon insufficient for labels
    censoring_reason: Optional[str] = None  # "capture_end" | "token_graduated" | etc.

    # ── Provenance ──
    schema_version: str = SCHEMA_VERSION
    generator_version: str = GENERATOR_VERSION

# ─── pump_outcome_v1 ──────────────────────────────────────────────────
# Objective future-truth labels for every eligible pump_state_v1 row.
# INDEPENDENT of any trading policy. Contains continuous facts, not just GOOD/BAD.

@dataclass
class PumpOutcomeV1:
    # ── Linkage ──
    state_id: str              # FK to pump_state_v1.state_id
    mint: str
    event_time_unix_ms: int

    # ── Markout returns (pct change from price at t to price at t+horizon) ──
    # All in basis points (1 bp = 0.01%). None if right-censored.
    ret_1s_bp: Optional[int]
    ret_2s_bp: Optional[int]
    ret_5s_bp: Optional[int]
    ret_10s_bp: Optional[int]
    ret_30s_bp: Optional[int]
    ret_60s_bp: Optional[int]
    ret_120s_bp: Optional[int]
    ret_300s_bp: Optional[int]

    # ── MFE / MAE (maximum favorable / adverse excursion) ──
    # In basis points relative to entry price at time t
    mfe_bp: Optional[int]          # max high minus entry, in bp
    mae_bp: Optional[int]          # min low minus entry, in bp (negative)
    mfe_time_seconds: Optional[float]   # when MFE was hit
    mae_time_seconds: Optional[float]   # when MAE was hit

    # ── First-hit barrier labels ──
    # Time in seconds to first hit, None if never hit within horizon
    hit_plus_10_bp: Optional[float]     # first time price rose +10 bp
    hit_plus_25_bp: Optional[float]
    hit_plus_50_bp: Optional[float]
    hit_plus_100_bp: Optional[float]
    hit_plus_200_bp: Optional[float]
    hit_minus_10_bp: Optional[float]    # first time price fell -10 bp
    hit_minus_20_bp: Optional[float]
    hit_minus_30_bp: Optional[float]
    hit_minus_50_bp: Optional[float]

    # ── Special condition: +100bp before -30bp ──
    plus_100_before_minus_30: Optional[bool]

    # ── Peak / time-to-peak ──
    peak_bp: Optional[int]            # max price in bp above entry within 300s
    time_to_peak_seconds: Optional[float]

    # ── Graduation / migration ──
    graduated: Optional[bool]
    seconds_to_graduation: Optional[float]
    graduated_did_migrate: Optional[bool]
    post_grad_price_change_300s_bp: Optional[int]  # price change 300s after grad

    # ── Survival / collapse ──
    survived_60s: Optional[bool]
    survived_300s: Optional[bool]
    collapsed_50pct_within_300s: Optional[bool]  # price dropped >50% within 300s

    # ── Censoring ──
    right_censored: bool
    censoring_reason: Optional[str]

    # ── Provenance ──
    schema_version: str = SCHEMA_VERSION
    generator_version: str = GENERATOR_VERSION

# ─── simulator_label_v1 ───────────────────────────────────────────────
# Second label layer: policy-dependent. Versioned. Separate from objective truth.

@dataclass
class SimulatorLabelV1:
    # ── Linkage ──
    state_id: str              # FK to pump_state_v1
    mint: str
    event_time_unix_ms: int

    # ── Simulated trade ──
    would_enter: bool          # would the policy have entered at this state?
    entry_price_sol: Optional[int]   # lamports per token unit (fixed-point)
    entry_slippage_bp: Optional[int]
    entry_fee_lamports: Optional[int]
    entry_latency_ms: Optional[int]  # simulated fill latency

    # ── Simulated exit ──
    exit_reason: Optional[str]       # "take_profit" | "stop_loss" | "trailing" | "time_stop" | "hold" | "liquidity_abort"
    exit_price_sol: Optional[int]
    exit_slippage_bp: Optional[int]
    exit_fee_lamports: Optional[int]
    exit_latency_ms: Optional[int]
    hold_duration_seconds: Optional[float]

    # ── PnL ──
    gross_pnl_lamports: Optional[int]
    net_pnl_lamports: Optional[int]   # after fees, tips, slippage
    pnl_pct_bp: Optional[int]         # net PnL in bp

    # ── MFE/MAE while held ──
    mfe_while_held_bp: Optional[int]
    mae_while_held_bp: Optional[int]

    # ── Order config ──
    target_order: Optional[str]       # take-profit target
    stop_order: Optional[str]         # stop-loss level
    config_hash: Optional[str]        # hash of the config used
    policy_version: Optional[str]     # e.g. "champion_v1"

    # ── Classification ──
    policy_class: Optional[str]       # STRONG | GOOD | MARGINAL | SKIP | BAD | TOXIC

    # ── Provenance ──
    schema_version: str = SCHEMA_VERSION
    generator_version: str = GENERATOR_VERSION

# ─── Git trajectory schemas (rust_gold_v1) ────────────────────────────

@dataclass
class GitTrajectoryV1:
    commit_sha: str
    parent_shas: List[str]
    child_shas: List[str]            # filled in second pass
    commit_time_iso: str
    author: str
    intent: str                      # commit message (first line)
    full_message: str
    files_changed: List[str]
    lines_added: int
    lines_removed: int
    classification: str              # FEATURE | BUG_FIX | REGRESSION_FIX | REFACTOR | PERFORMANCE | TEST | REVERT | FAILED_APPROACH
    classification_confidence: Optional[float]
    classification_evidence: Optional[str]   # why this classification
    later_fixed_by: Optional[str]    # SHA of a later commit that fixes/reverts this
    later_reverted_by: Optional[str] # SHA of a later commit that reverts this
    tests_pass: Optional[bool]       # from gate history if available
    fmt_pass: Optional[bool]
    clippy_pass: Optional[bool]
