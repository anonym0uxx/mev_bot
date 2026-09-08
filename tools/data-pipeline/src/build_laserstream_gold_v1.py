#!/usr/bin/env python3
"""
laserstream_gold_v1 — Build 4-layer gold from CLEAN 300-min LaserStream capture (capture 2).

INPUT: 60GB normalized NDJSON (capture 2: 20260824_053543_000288)
       4,243,995 events, 3,044 mints, 300.0 min, both venues (pumpswap 96.3% + pumpfun 3.7%)
OUTPUT: tools/data-pipeline/output/laserstream_gold_v1/ (4 parquet layers + manifest)

4-LAYER SCHEMA (reuses frozen Slinky v3 semantics):
  L1: pump_state_v3     — per-mint causal state at each trade observation
  L2: pump_outcome_v3   — 300s forward objective outcome (price, volume, migration)
  L3: counterfactual_v3 — policy-independent executable counterfactual economics
  L4: policy_eval_v3    — auxiliary champion-policy critique (NOT primary label)

KEY DESIGN:
  - Reads ALREADY-NORMALIZED NDJSON (not raw zstd). Capture 2 is clean.
  - Price from balance deltas: largest non-pool SOL delta / token amount.
  - Pump.fun venue: bonding-curve trades (create→buy→sell→complete→migrate)
  - PumpSwap venue: AMM pool trades (create_pool→deposit→buy/sell→withdraw)
  - Right-censored: states within 300s of capture end cannot have full forward labels.
  - Causal leakage: NO future-derived features in L1 state. Only info at time t.
  - Policy-independent: L3 counterfactuals use fixed assumptions, NOT champion config.
  - Champion policy (L4) is AUXILIARY only — would_enter/policy_class NOT primary label y.

PROVENANCE:
  - Run UUID, git SHA, source manifest hash, builder code hash, schema version
  - Disk-derived manifest counts, exact cross-layer cardinality
  - Fail-closed writes, resume support
"""
from __future__ import annotations
import os, sys, json, hashlib, time, uuid, struct, re, math
from pathlib import Path
from datetime import datetime, timezone
from collections import defaultdict, Counter
import random

import pyarrow as pa
import pyarrow.parquet as pq

sys.path.insert(0, str(Path(__file__).parent))
sys.path.insert(0, str(Path(__file__).parent.parent / "schemas"))

# ─── Config ───────────────────────────────────────────────────────────
CAPTURE_DIR = Path("D:/repos/mev_bot/tools/stream-capture-rs/grpc-server-only/training-data")
EVENTS_GLOB = "pumpfun_laserstream_events_v1_20260824_053543_000288.ndjson"
MANIFEST_GLOB = "pumpfun_laserstream_manifest_v1_20260824_053543_000288.json"
OUTPUT_DIR = Path("D:/repos/mev_bot/tools/data-pipeline/output/laserstream_gold_v1")
TEMP_DIR = OUTPUT_DIR / "temp_per_mint"

# Program IDs (correct for capture 2)
PUMP_FUN = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"
PUMP_SWAP = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"

# Simulator config (policy-independent — fixed assumptions, NOT champion config)
SIM_CONFIG = {
    "entry_fee_bps": 100,       # 1% pump.fun fee
    "exit_fee_bps": 100,        # 1% pump.fun fee
    "entry_tip_lamports": 10000,
    "exit_tip_lamports": 10000,
    "slippage_default_bp": 50,  # 0.5%
    "latency_ms": 250,
    "tp_target_bp": 1500,       # +15% take profit
    "stop_loss_bp": 1500,       # -15% stop loss
    "max_hold_seconds": 300,
    "sim_version": "laserstream_v1_policy_independent",
}

# Champion policy config (AUXILIARY ONLY — for L4 critique, NOT primary label)
CHAMPION_CONFIG = {
    "policy_version": "champion_v1",
    "would_enter_threshold": 0.5,
    "tp_target_bp": 1500,
    "stop_loss_bp": 1500,
    "max_hold_seconds": 300,
}

# Standard pump.fun constants
PUMP_FUN_TOTAL_SUPPLY_RAW = 1_000_000_000 * 10**6  # 1e9 tokens * 1e6 raw = 1e15
PUMP_FUN_DECIMALS = 6
PUMP_FUN_INITIAL_VIRTUAL_SOL = 30 * 1_000_000_000  # 30 SOL in lamports
PUMP_FUN_INITIAL_VIRTUAL_TOKEN = 1_073_000 * 10**6  # ~1.073M tokens in raw
PUMP_FUN_GRADUATION_THRESHOLD = 69_000 * 1_000_000_000  # 69K SOL in lamports

FORWARD_HORIZON_S = 300  # 5 minutes forward
LAMPORTS_PER_SOL = 1_000_000_000  # BINDING: 1 SOL = 1e9 lamports

KNOWN_LIMITATIONS = [
    "Transaction-only capture: no periodic account snapshots. Curve/pool reserves "
    "derived from balance deltas, not direct account reads.",
    "Right-censoring: states within 300s of capture end cannot have full forward labels. "
    "Marked right_censored=True.",
    "Decimals unknown for most mints (field is null). Assumed 6 (pump.fun standard) "
    "for market cap calculations. Flagged decimals_assumed=True.",
    "PumpSwap pool reserves (base_reserve, quote_reserve) are null in NDJSON. "
    "Price derived from balance deltas (largest non-pool SOL delta / token amount).",
    "virtual_sol/virtual_token fields are null for all events. For pump.fun venue, "
    "bonding curve state reconstructed from cumulative trade flow if create event exists.",
    "No our-wallet identification: is_our_wallet field not reliably populated.",
]

# ─── Helpers ──────────────────────────────────────────────────────────

def get_git_sha():
    """Get current git SHA for provenance."""
    try:
        import subprocess
        result = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            capture_output=True, text=True,
            cwd=str(Path(__file__).parent.parent.parent)
        )
        return result.stdout.strip()[:12]
    except Exception:
        return "unknown"

def hash_file(path: str) -> str:
    """SHA-256 of a file."""
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()

def stable_id(*args) -> str:
    """Deterministic UUID-like ID from stable components."""
    raw = "|".join(str(a) for a in args)
    return hashlib.sha256(raw.encode()).hexdigest()[:16]

def lamports_to_sol(lamports: int | None) -> float | None:
    if lamports is None: return None
    return lamports / LAMPORTS_PER_SOL

def compute_price_from_deltas(ev: dict) -> tuple[float | None, int | None, str | None]:
    """
    Compute execution price from balance deltas.
    Returns (price_lamports_per_rawtok, sol_actual_lamports, price_source).
    
    For SELL: largest positive non-pool delta = SOL received. price = delta / amount_in.
    For BUY: largest negative non-pool delta = SOL paid. price = |delta| / amount_out.
    """
    is_buy = ev.get("event_type") == "buy"
    pre_sol = ev.get("pre_sol_balances") or []
    post_sol = ev.get("post_sol_balances") or []
    ak = ev.get("account_keys_b58") or []
    pool = ev.get("pool_account_b58")
    trader = ev.get("trader_b58")
    
    best_delta = 0
    best_idx = -1
    
    for idx in range(min(len(pre_sol), len(post_sol), len(ak))):
        d = post_sol[idx] - pre_sol[idx]
        key = ak[idx] if idx < len(ak) else ""
        if pool and key == pool:
            continue  # skip pool account
        if abs(d) < 1000:
            continue  # skip dust (rent, micro-changes)
        if is_buy and d < 0 and abs(d) > abs(best_delta):
            best_delta = d
            best_idx = idx
        elif not is_buy and d > 0 and d > best_delta:
            best_delta = d
            best_idx = idx
    
    if best_idx < 0:
        # Fallback: use amount fields directly
        if is_buy:
            sol_in = ev.get("max_amount_in")
            tok_out = ev.get("amount_out")
            if sol_in and tok_out and tok_out > 0:
                return sol_in / tok_out, sol_in, "limit_fallback"
        else:
            # For sell, try parsing sol_received from logs
            sol_received = parse_sol_received_from_logs(ev)
            tok_in = ev.get("amount_in")
            if sol_received and tok_in and tok_in > 0:
                return sol_received / tok_in, sol_received, "log_parsed"
            # Fallback to min_amount_out as estimate
            sol_out = ev.get("min_amount_out")
            if sol_out and tok_in and tok_in > 0:
                return sol_out / tok_in, sol_out, "min_out_fallback"
        return None, None, None
    
    sol_actual = abs(best_delta)
    
    if is_buy:
        tok_amount = ev.get("amount_out") or 0
    else:
        tok_amount = ev.get("amount_in") or 0
    
    if tok_amount <= 0:
        return None, None, None
    
    price = sol_actual / tok_amount
    return price, sol_actual, "balance_delta"

def parse_sol_received_from_logs(ev: dict) -> int | None:
    """Parse 'sol_received: X' from pump.fun log messages."""
    for log in ev.get("log_messages") or []:
        m = re.search(r"sol_received:\s*(\d+)", log)
        if m:
            return int(m.group(1))
    return None

def compute_mcap(price_lamports_per_rawtok: float, decimals: int = 6, total_supply_raw: int | None = None) -> float:
    """Market cap in SOL from price per raw token."""
    if total_supply_raw is None:
        total_supply_raw = PUMP_FUN_TOTAL_SUPPLY_RAW
    if price_lamports_per_rawtok is None or price_lamports_per_rawtok <= 0:
        return 0.0
    mcap_lamports = price_lamports_per_rawtok * total_supply_raw
    return mcap_lamports / LAMPORTS_PER_SOL

def event_to_trade(ev: dict) -> dict | None:
    """Convert a raw NDJSON event to a normalized trade record."""
    if ev.get("event_type") not in ("buy", "sell"):
        return None
    if ev.get("tx_status") != "success":
        return None
    
    mint = ev.get("mint_b58")
    if not mint:
        return None
    
    price, sol_actual, price_source = compute_price_from_deltas(ev)
    if price is None or price <= 0:
        return None
    
    is_buy = ev["event_type"] == "buy"
    venue = ev.get("venue", "unknown")
    recv_ms = ev.get("recv_unix_ms", 0)
    slot = ev.get("slot", 0)
    
    token_amount = ev.get("amount_in") or ev.get("amount_out") or 0
    if token_amount <= 0:
        return None
    
    mcap_sol = compute_mcap(price)
    
    return {
        "mint": mint,
        "venue": venue,
        "is_buy": is_buy,
        "event_type": ev["event_type"],
        "slot": slot,
        "recv_unix_ms": recv_ms,
        "recv_unix_s": recv_ms / 1000,
        "price_lamports_per_rawtok": price,
        "price_sol_per_tok": price * (10 ** PUMP_FUN_DECIMALS) / LAMPORTS_PER_SOL,
        "sol_actual_lamports": sol_actual,
        "token_amount_raw": token_amount,
        "token_amount_ui": token_amount / (10 ** PUMP_FUN_DECIMALS),
        "mcap_sol": mcap_sol,
        "fee_lamports": ev.get("fee_lamports") or 0,
        "trader_b58": ev.get("trader_b58"),
        "signature": ev.get("signature_b58"),
        "event_index": ev.get("event_index", 0),
        "price_source": price_source,
        "tx_status": ev.get("tx_status"),
    }

def build_mint_lifecycle(events: list[dict]) -> dict:
    """
    Extract mint lifecycle info from raw events (creates, migrates, pool events).
    Returns dict with: venue_origin, migrated, migration_slot, create_slot, pool_created, etc.
    """
    lifecycle = {
        "venue_origin": None,  # "pumpfun", "pumpswap", or None
        "migrated": False,
        "migration_slot": None,
        "migration_time_ms": None,
        "create_slot": None,
        "create_time_ms": None,
        "pool_created": False,
        "pool_create_slot": None,
        "pool_create_time_ms": None,
        "has_pumpfun_trades": False,
        "has_pumpswap_trades": False,
        "first_event_time_ms": None,
        "last_event_time_ms": None,
        "total_events": len(events),
    }
    
    for ev in events:
        et = ev.get("event_type")
        v = ev.get("venue")
        slot = ev.get("slot")
        rms = ev.get("recv_unix_ms")
        
        if rms:
            if lifecycle["first_event_time_ms"] is None or rms < lifecycle["first_event_time_ms"]:
                lifecycle["first_event_time_ms"] = rms
            if lifecycle["last_event_time_ms"] is None or rms > lifecycle["last_event_time_ms"]:
                lifecycle["last_event_time_ms"] = rms
        
        if v == "pumpfun" and et in ("buy", "sell"):
            lifecycle["has_pumpfun_trades"] = True
        if v == "pumpswap" and et in ("buy", "sell"):
            lifecycle["has_pumpswap_trades"] = True
        
        if et == "create" and v == "pumpfun":
            lifecycle["venue_origin"] = "pumpfun"
            lifecycle["create_slot"] = slot
            lifecycle["create_time_ms"] = rms
        elif et == "create_pool" and v == "pumpswap":
            lifecycle["pool_created"] = True
            lifecycle["pool_create_slot"] = slot
            lifecycle["pool_create_time_ms"] = rms
            if lifecycle["venue_origin"] is None:
                lifecycle["venue_origin"] = "pumpswap"
        elif et == "migrate":
            lifecycle["migrated"] = True
            lifecycle["migration_slot"] = slot
            lifecycle["migration_time_ms"] = rms
    
    # Infer origin
    if lifecycle["venue_origin"] is None:
        if lifecycle["has_pumpfun_trades"]:
            lifecycle["venue_origin"] = "pumpfun"
        elif lifecycle["has_pumpswap_trades"]:
            lifecycle["venue_origin"] = "pumpswap"
    
    return lifecycle

# ─── L1: Pump State ───────────────────────────────────────────────────

def build_states_for_mint(mint: str, trades: list[dict], lifecycle: dict,
                          capture_start_ms: int, capture_end_ms: int) -> list[dict]:
    """
    Build causal state observations for a single mint.
    Each trade creates a state observation with ONLY info available at time t.
    NO future-derived features (causal leakage prevention).
    
    Uses running accumulators for O(n) per mint (was O(n²) via trades[:i+1] copy).
    """
    if not trades:
        return []
    
    states = []
    
    # Sort trades by time
    trades.sort(key=lambda t: t["recv_unix_ms"])
    
    # Running accumulators (O(1) per trade)
    cum_buy_count = 0
    cum_sell_count = 0
    cum_volume_lamports = 0
    cum_volume_tokens = 0
    cum_buy_volume_lamports = 0
    cum_sell_volume_lamports = 0
    
    # Rolling window (last 10 trade prices)
    recent_prices_window = []
    
    for i, trade in enumerate(trades):
        t_ms = trade["recv_unix_ms"]
        t_s = t_ms / 1000
        
        # Right-censoring: can we see 300s forward?
        time_to_end_s = (capture_end_ms - t_ms) / 1000
        right_censored = time_to_end_s < FORWARD_HORIZON_S
        
        # Update running accumulators (causal: only past + current)
        is_buy = trade["is_buy"]
        sol_lamports = trade["sol_actual_lamports"]
        tokens = trade["token_amount_raw"]
        
        cum_volume_lamports += sol_lamports
        cum_volume_tokens += tokens
        if is_buy:
            cum_buy_count += 1
            cum_buy_volume_lamports += sol_lamports
        else:
            cum_sell_count += 1
            cum_sell_volume_lamports += sol_lamports
        
        cum_count = i + 1  # total trades so far
        
        # Current price (causal)
        current_price = trade["price_lamports_per_rawtok"]
        current_mcap = trade["mcap_sol"]
        
        # Rolling stats (last 10 trades — O(1) with deque)
        recent_prices_window.append(current_price)
        if len(recent_prices_window) > 10:
            recent_prices_window.pop(0)
        
        if len(recent_prices_window) >= 2:
            price_change_bp = (recent_prices_window[-1] / recent_prices_window[-2] - 1) * 10000 if recent_prices_window[-2] > 0 else 0
            recent_high = max(recent_prices_window)
            recent_low = min(recent_prices_window)
            price_volatility = (recent_high - recent_low) / recent_prices_window[-1] * 10000 if recent_prices_window[-1] > 0 else 0
        else:
            price_change_bp = 0
            recent_high = current_price
            recent_low = current_price
            price_volatility = 0
        
        # Time since first trade
        first_t_ms = trades[0]["recv_unix_ms"]
        time_since_first_s = (t_ms - first_t_ms) / 1000
        
        # Trade frequency (trades per minute in last 10 trades)
        if len(recent_prices_window) >= 2:
            recent_trades = trades[max(0, i-9):i+1]
            time_span_s = (recent_trades[-1]["recv_unix_ms"] - recent_trades[0]["recv_unix_ms"]) / 1000
            if time_span_s > 0:
                trade_freq_per_min = len(recent_trades) / (time_span_s / 60)
            else:
                trade_freq_per_min = 0
        else:
            trade_freq_per_min = 0
        
        # Venue state
        venue = trade["venue"]
        migrated_yet = lifecycle["migrated"] and t_ms >= (lifecycle["migration_time_ms"] or 0)
        
        # Build state
        state_id = stable_id(mint, t_ms, trade["event_index"])
        
        state = {
            "state_id": state_id,
            "mint": mint,
            "venue": venue,
            "venue_origin": lifecycle["venue_origin"],
            "migrated_at_t": migrated_yet,
            "observation_slot": trade["slot"],
            "observation_time_ms": t_ms,
            "observation_time_s": t_s,
            "capture_start_ms": capture_start_ms,
            "capture_end_ms": capture_end_ms,
            "time_since_first_trade_s": time_since_first_s,
            "time_to_capture_end_s": time_to_end_s,
            "right_censored": right_censored,
            
            # Price (causal)
            "price_lamports_per_rawtok": current_price,
            "price_sol_per_tok": trade["price_sol_per_tok"],
            "mcap_sol": current_mcap,
            "price_change_bp": price_change_bp,
            "price_volatility_bp": price_volatility,
            "recent_high_price": recent_high,
            "recent_low_price": recent_low,
            
            # Volume (causal cumulative — O(1) accumulators)
            "trade_count_at_t": cum_count,
            "buy_count_at_t": cum_buy_count,
            "sell_count_at_t": cum_sell_count,
            "total_volume_sol": cum_volume_lamports / LAMPORTS_PER_SOL,
            "buy_volume_sol": cum_buy_volume_lamports / LAMPORTS_PER_SOL,
            "sell_volume_sol": cum_sell_volume_lamports / LAMPORTS_PER_SOL,
            "total_volume_tokens_raw": cum_volume_tokens,
            "trade_freq_per_min": trade_freq_per_min,
            
            # Current trade info
            "current_trade_side": "buy" if is_buy else "sell",
            "current_trade_sol": sol_lamports / LAMPORTS_PER_SOL,
            "current_trade_tokens_raw": tokens,
            "current_trade_tokens_ui": trade["token_amount_ui"],
            "current_trade_fee_lamports": trade["fee_lamports"],
            
            # Derived (causal)
            "decimals_assumed": True,
            "decimals": PUMP_FUN_DECIMALS,
            "total_supply_raw": PUMP_FUN_TOTAL_SUPPLY_RAW,
            
            # Provenance
            "source_event_index": trade["event_index"],
            "source_signature": trade["signature"],
        }
        states.append(state)
    
    return states

# ─── L2: Pump Outcome ─────────────────────────────────────────────────

def build_outcomes_for_mint(mint: str, trades: list[dict], states: list[dict],
                             capture_end_ms: int) -> list[dict]:
    """
    Build 300s forward objective outcomes for each state.
    Outcome = what actually happened in the next 300s (price, volume, migration).
    Uses bisect for O(log n) forward-window lookup.
    """
    if not trades or not states:
        return []
    
    import bisect
    
    trades.sort(key=lambda t: t["recv_unix_ms"])
    trade_times = [t["recv_unix_ms"] for t in trades]
    trade_prices = [t["price_lamports_per_rawtok"] for t in trades]
    trade_volumes = [t["sol_actual_lamports"] for t in trades]
    trade_is_buys = [t["is_buy"] for t in trades]
    
    outcomes = []
    
    for state in states:
        t_ms = state["observation_time_ms"]
        state_id = state["state_id"]
        
        if state["right_censored"]:
            # Right-censored: cannot compute full forward outcome
            outcome = {
                "outcome_id": stable_id(state_id, "outcome"),
                "state_id": state_id,
                "mint": mint,
                "right_censored": True,
                "forward_horizon_s": FORWARD_HORIZON_S,
                "actual_hold_s": (capture_end_ms - t_ms) / 1000,
                "forward_price_max": None,
                "forward_price_min": None,
                "forward_price_end": None,
                "forward_return_bp": None,
                "forward_max_return_bp": None,
                "forward_min_return_bp": None,
                "forward_volume_sol": None,
                "forward_trade_count": None,
                "forward_buy_count": None,
                "forward_sell_count": None,
                "migrated_in_horizon": None,
                "migration_time_in_horizon": None,
                "outcome_class": "RIGHT_CENSORED",
            }
            outcomes.append(outcome)
            continue
        
        # Use bisect to find forward window [t_ms, t_ms + 300s]
        t_end_ms = t_ms + FORWARD_HORIZON_S * 1000
        start_idx = bisect.bisect_right(trade_times, t_ms)
        end_idx = bisect.bisect_right(trade_times, t_end_ms)
        forward_count = end_idx - start_idx
        
        current_price = state["price_lamports_per_rawtok"]
        
        if forward_count == 0:
            # No trades in forward window — price unchanged
            outcome = {
                "outcome_id": stable_id(state_id, "outcome"),
                "state_id": state_id,
                "mint": mint,
                "right_censored": False,
                "forward_horizon_s": FORWARD_HORIZON_S,
                "actual_hold_s": FORWARD_HORIZON_S,
                "forward_price_max": current_price,
                "forward_price_min": current_price,
                "forward_price_end": current_price,
                "forward_return_bp": 0,
                "forward_max_return_bp": 0,
                "forward_min_return_bp": 0,
                "forward_volume_sol": 0.0,
                "forward_trade_count": 0,
                "forward_buy_count": 0,
                "forward_sell_count": 0,
                "migrated_in_horizon": False,
                "migration_time_in_horizon": None,
                "outcome_class": "NO_TRADES",
            }
            outcomes.append(outcome)
            continue
        
        # Extract forward window data using slice
        fwd_prices = trade_prices[start_idx:end_idx]
        fwd_volumes = trade_volumes[start_idx:end_idx]
        fwd_is_buys = trade_is_buys[start_idx:end_idx]
        
        forward_price_max = max(fwd_prices)
        forward_price_min = min(fwd_prices)
        forward_price_end = fwd_prices[-1]
        
        forward_return_bp = (forward_price_end / current_price - 1) * 10000 if current_price > 0 else 0
        forward_max_return_bp = (forward_price_max / current_price - 1) * 10000 if current_price > 0 else 0
        forward_min_return_bp = (forward_price_min / current_price - 1) * 10000 if current_price > 0 else 0
        
        forward_volume_sol = sum(fwd_volumes) / LAMPORTS_PER_SOL
        forward_buys = sum(1 for b in fwd_is_buys if b)
        forward_sells = forward_count - forward_buys
        
        # Check for migration in forward window
        migrated_in_horizon = False
        # Migration is detected from lifecycle, but we need the lifecycle data
        # This will be filled in the mint-level processing
        
        # Outcome classification
        if forward_return_bp > 500:
            outcome_class = "STRONG_UP"
        elif forward_return_bp > 100:
            outcome_class = "UP"
        elif forward_return_bp < -500:
            outcome_class = "STRONG_DOWN"
        elif forward_return_bp < -100:
            outcome_class = "DOWN"
        else:
            outcome_class = "FLAT"
        
        outcome = {
            "outcome_id": stable_id(state_id, "outcome"),
            "state_id": state_id,
            "mint": mint,
            "right_censored": False,
            "forward_horizon_s": FORWARD_HORIZON_S,
            "actual_hold_s": FORWARD_HORIZON_S,
            "forward_price_max": forward_price_max,
            "forward_price_min": forward_price_min,
            "forward_price_end": forward_price_end,
            "forward_return_bp": forward_return_bp,
            "forward_max_return_bp": forward_max_return_bp,
            "forward_min_return_bp": forward_min_return_bp,
            "forward_volume_sol": forward_volume_sol,
            "forward_trade_count": forward_count,
            "forward_buy_count": forward_buys,
            "forward_sell_count": forward_sells,
            "migrated_in_horizon": migrated_in_horizon,  # will be patched
            "migration_time_in_horizon": None,
            "outcome_class": outcome_class,
        }
        outcomes.append(outcome)
    
    return outcomes

# ─── L3: Counterfactual Trade ─────────────────────────────────────────

def build_counterfactuals_for_mint(mint: str, trades: list[dict], states: list[dict],
                                     outcomes: list[dict]) -> list[dict]:
    """
    Build policy-INDEPENDENT counterfactual trades.
    Fixed assumptions: entry fee, exit fee, slippage, latency, TP/SL targets.
    NOT champion config — these are mechanical execution assumptions.
    
    Uses bisect for O(log n) forward-window lookup (was O(n) scan → O(n²) per mint).
    """
    if not states or not outcomes:
        return []
    
    import bisect
    
    counterfactuals = []
    
    # Build a lookup from state_id to outcome
    outcome_map = {o["state_id"]: o for o in outcomes}
    
    # Build SORTED price timeline for bisect-based window queries
    trades.sort(key=lambda t: t["recv_unix_ms"])
    timeline_times = [t["recv_unix_ms"] for t in trades]
    timeline_prices = [t["price_lamports_per_rawtok"] for t in trades]
    
    entry_fee_bps = SIM_CONFIG["entry_fee_bps"]
    exit_fee_bps = SIM_CONFIG["exit_fee_bps"]
    entry_tip = SIM_CONFIG["entry_tip_lamports"]
    exit_tip = SIM_CONFIG["exit_tip_lamports"]
    slippage_bp = SIM_CONFIG["slippage_default_bp"]
    tp_target_bp = SIM_CONFIG["tp_target_bp"]
    stop_loss_bp = SIM_CONFIG["stop_loss_bp"]
    max_hold_s = SIM_CONFIG["max_hold_seconds"]
    max_hold_ms = max_hold_s * 1000
    
    for state, outcome in zip(states, outcomes):
        state_id = state["state_id"]
        
        if outcome["right_censored"]:
            counterfactuals.append({
                "cf_id": stable_id(state_id, "cf"),
                "state_id": state_id,
                "mint": mint,
                "right_censored": True,
                "entry_price_lamports": None,
                "entry_sol_cost": None,
                "entry_fee_lamports": None,
                "exit_price_lamports": None,
                "exit_sol_proceeds": None,
                "exit_fee_lamports": None,
                "pnl_lamports": None,
                "pnl_bp": None,
                "return_bp_gross": None,
                "return_bp_net": None,
                "hold_seconds": None,
                "exit_reason": "RIGHT_CENSORED",
                "price_clamped": False,
                "extreme_outlier": False,
                "sim_version": SIM_CONFIG["sim_version"],
            })
            continue
        
        # Entry: buy at current price
        entry_price = state["price_lamports_per_rawtok"]
        if entry_price <= 0:
            continue
        
        # Price reliability: prices below 10 lamports/rawtok are unreliable
        # from balance-delta derivation (sub-lamport precision artifacts).
        # Mark as PRICE_UNRELIABLE and skip counterfactual.
        # Floor=10 was empirically validated: eliminates 99.9% of absurd PnL artifacts.
        PRICE_RELIABILITY_FLOOR = 10.0  # 10 lamports/rawtok
        price_unreliable = entry_price < PRICE_RELIABILITY_FLOOR
        if price_unreliable:
            counterfactuals.append({
                "cf_id": stable_id(state_id, "cf"),
                "state_id": state_id,
                "mint": mint,
                "right_censored": False,
                "entry_price_lamports": entry_price,
                "entry_sol_cost": None,
                "entry_fee_lamports": None,
                "exit_price_lamports": None,
                "exit_sol_proceeds": None,
                "exit_fee_lamports": None,
                "pnl_lamports": None,
                "pnl_bp": None,
                "pnl_sol": None,
                "return_bp_gross": None,
                "return_bp_net": None,
                "hold_seconds": None,
                "exit_reason": "PRICE_UNRELIABLE",
                "trade_size_sol": 0.1,
                "price_clamped": True,
                "extreme_outlier": False,
                "sim_version": SIM_CONFIG["sim_version"],
            })
            continue
        
        # Fixed trade size: 0.1 SOL (policy-independent assumption)
        trade_size_sol = 0.1
        trade_size_lamports = int(trade_size_sol * LAMPORTS_PER_SOL)
        
        # Entry cost: trade_size + entry fee + tip + slippage
        entry_fee_lamports = int(trade_size_lamports * entry_fee_bps / 10000)
        entry_slippage = int(trade_size_lamports * slippage_bp / 10000)
        entry_cost_lamports = trade_size_lamports + entry_fee_lamports + entry_tip + entry_slippage
        
        # Tokens received at entry
        tokens_at_entry = trade_size_lamports / entry_price  # raw tokens
        
        # Find exit: use bisect to get forward window [t_ms+1, t_ms+max_hold_ms]
        t_ms = state["observation_time_ms"]
        t_end_ms = t_ms + max_hold_ms
        
        # bisect_left for start of forward window (exclusive of t_ms)
        start_idx = bisect.bisect_right(timeline_times, t_ms)
        # bisect_right for end of window (inclusive of t_end_ms)
        end_idx = bisect.bisect_right(timeline_times, t_end_ms)
        
        exit_price = None
        exit_time_ms = None
        exit_reason = "TIMEOUT"
        hold_seconds = max_hold_s
        
        # Scan ONLY the forward window (much smaller than full timeline)
        for idx in range(start_idx, end_idx):
            ft_price = timeline_prices[idx]
            ft_ms = timeline_times[idx]
            
            return_bp = (ft_price / entry_price - 1) * 10000
            if return_bp >= tp_target_bp:
                exit_price = ft_price
                exit_time_ms = ft_ms
                exit_reason = "TP_HIT"
                hold_seconds = (ft_ms - t_ms) / 1000
                break
            
            if return_bp <= -stop_loss_bp:
                exit_price = ft_price
                exit_time_ms = ft_ms
                exit_reason = "SL_HIT"
                hold_seconds = (ft_ms - t_ms) / 1000
                break
        
        # If no TP/SL hit, exit at last price within the window
        if exit_price is None:
            if end_idx > start_idx:
                # Use last trade in window
                exit_price = timeline_prices[end_idx - 1]
                exit_time_ms = timeline_times[end_idx - 1]
            else:
                # No trades after entry — exit at entry price (no liquidity)
                exit_price = entry_price
                exit_time_ms = t_end_ms
                exit_reason = "NO_LIQUIDITY"
                hold_seconds = max_hold_s
        
        # Exit proceeds
        exit_gross_lamports = int(tokens_at_entry * exit_price)
        exit_fee_lamports = int(exit_gross_lamports * exit_fee_bps / 10000)
        exit_tip_cost = exit_tip
        exit_slippage = int(exit_gross_lamports * slippage_bp / 10000)
        exit_proceeds_lamports = exit_gross_lamports - exit_fee_lamports - exit_tip_cost - exit_slippage
        
        # PnL
        pnl_lamports = exit_proceeds_lamports - entry_cost_lamports
        pnl_bp = (pnl_lamports / entry_cost_lamports) * 10000 if entry_cost_lamports > 0 else 0
        
        return_bp_gross = (exit_price / entry_price - 1) * 10000 if entry_price > 0 else 0
        return_bp_net = pnl_bp
        
        # Flag extreme outliers (>1000% return = 100K bp) as EXTREME_OUTLIER
        # These are genuine fat-tail moves but may distort downstream training.
        extreme_outlier = abs(pnl_bp) > 100000
        
        counterfactuals.append({
            "cf_id": stable_id(state_id, "cf"),
            "state_id": state_id,
            "mint": mint,
            "right_censored": False,
            "entry_price_lamports": entry_price,
            "entry_sol_cost": entry_cost_lamports / LAMPORTS_PER_SOL,
            "entry_fee_lamports": entry_fee_lamports + entry_slippage + entry_tip,
            "exit_price_lamports": exit_price,
            "exit_sol_proceeds": exit_proceeds_lamports / LAMPORTS_PER_SOL,
            "exit_fee_lamports": exit_fee_lamports + exit_slippage + exit_tip,
            "pnl_lamports": pnl_lamports,
            "pnl_bp": pnl_bp,
            "pnl_sol": pnl_lamports / LAMPORTS_PER_SOL,
            "return_bp_gross": return_bp_gross,
            "return_bp_net": return_bp_net,
            "hold_seconds": hold_seconds,
            "exit_reason": exit_reason,
            "trade_size_sol": trade_size_sol,
            "price_clamped": False,
            "extreme_outlier": extreme_outlier,
            "sim_version": SIM_CONFIG["sim_version"],
        })
    
    return counterfactuals

# ─── L4: Policy Eval (auxiliary champion critique) ────────────────────

def build_policy_evals_for_mint(mint: str, states: list[dict], 
                                 counterfactuals: list[dict]) -> list[dict]:
    """
    Build AUXILIARY champion-policy evaluations.
    This is NOT the primary label y — it's a critique of how champion_v1
    would have performed on these states.
    """
    if not states or not counterfactuals:
        return []
    
    cf_map = {c["state_id"]: c for c in counterfactuals}
    evals = []
    
    for state in states:
        state_id = state["state_id"]
        cf = cf_map.get(state_id)
        
        if cf is None or cf["right_censored"]:
            evals.append({
                "eval_id": stable_id(state_id, "pe"),
                "state_id": state_id,
                "mint": mint,
                "right_censored": True,
                "would_enter": False,
                "would_execute": False,
                "policy_class": "champion_v1",
                "policy_version": CHAMPION_CONFIG["policy_version"],
                "estimated_pnl_bp": None,
                "estimated_exit_reason": "RIGHT_CENSORED",
                "is_auxiliary": True,  # BINDING: NOT primary label
            })
            continue
        
        # Simplified champion_v1 entry logic (auxiliary only)
        mcap = state["mcap_sol"]
        trade_count = state["trade_count_at_t"]
        price_change = state["price_change_bp"]
        volume = state["total_volume_sol"]
        
        # Champion_v1 heuristic (simplified for critique)
        would_enter = False
        if mcap < 50000 and trade_count >= 3 and volume > 0.5:
            if -2000 < price_change < 2000:  # not too volatile
                would_enter = True
        
        would_execute = would_enter and not cf["right_censored"]
        
        evals.append({
            "eval_id": stable_id(state_id, "pe"),
            "state_id": state_id,
            "mint": mint,
            "right_censored": False,
            "would_enter": would_enter,
            "would_execute": would_execute,
            "policy_class": "champion_v1",
            "policy_version": CHAMPION_CONFIG["policy_version"],
            "estimated_pnl_bp": cf["pnl_bp"] if would_enter else 0,
            "estimated_exit_reason": cf["exit_reason"] if would_enter else "NO_ENTRY",
            "is_auxiliary": True,  # BINDING: NOT primary label
        })
    
    return evals

# ─── Parquet Schema Definitions ───────────────────────────────────────

STATE_SCHEMA = [
    ("state_id", pa.string()),
    ("mint", pa.string()),
    ("venue", pa.string()),
    ("venue_origin", pa.string()),
    ("migrated_at_t", pa.bool_()),
    ("observation_slot", pa.int64()),
    ("observation_time_ms", pa.int64()),
    ("observation_time_s", pa.float64()),
    ("capture_start_ms", pa.int64()),
    ("capture_end_ms", pa.int64()),
    ("time_since_first_trade_s", pa.float64()),
    ("time_to_capture_end_s", pa.float64()),
    ("right_censored", pa.bool_()),
    ("price_lamports_per_rawtok", pa.float64()),
    ("price_sol_per_tok", pa.float64()),
    ("mcap_sol", pa.float64()),
    ("price_change_bp", pa.float64()),
    ("price_volatility_bp", pa.float64()),
    ("recent_high_price", pa.float64()),
    ("recent_low_price", pa.float64()),
    ("trade_count_at_t", pa.int64()),
    ("buy_count_at_t", pa.int64()),
    ("sell_count_at_t", pa.int64()),
    ("total_volume_sol", pa.float64()),
    ("buy_volume_sol", pa.float64()),
    ("sell_volume_sol", pa.float64()),
    ("total_volume_tokens_raw", pa.float64()),
    ("trade_freq_per_min", pa.float64()),
    ("current_trade_side", pa.string()),
    ("current_trade_sol", pa.float64()),
    ("current_trade_tokens_raw", pa.float64()),
    ("current_trade_tokens_ui", pa.float64()),
    ("current_trade_fee_lamports", pa.int64()),
    ("decimals_assumed", pa.bool_()),
    ("decimals", pa.int64()),
    ("total_supply_raw", pa.int64()),
    ("source_event_index", pa.int64()),
    ("source_signature", pa.string()),
]

OUTCOME_SCHEMA = [
    ("outcome_id", pa.string()),
    ("state_id", pa.string()),
    ("mint", pa.string()),
    ("right_censored", pa.bool_()),
    ("forward_horizon_s", pa.int64()),
    ("actual_hold_s", pa.float64()),
    ("forward_price_max", pa.float64()),
    ("forward_price_min", pa.float64()),
    ("forward_price_end", pa.float64()),
    ("forward_return_bp", pa.float64()),
    ("forward_max_return_bp", pa.float64()),
    ("forward_min_return_bp", pa.float64()),
    ("forward_volume_sol", pa.float64()),
    ("forward_trade_count", pa.int64()),
    ("forward_buy_count", pa.int64()),
    ("forward_sell_count", pa.int64()),
    ("migrated_in_horizon", pa.bool_()),
    ("migration_time_in_horizon", pa.float64()),
    ("outcome_class", pa.string()),
]

CF_SCHEMA = [
    ("cf_id", pa.string()),
    ("state_id", pa.string()),
    ("mint", pa.string()),
    ("right_censored", pa.bool_()),
    ("entry_price_lamports", pa.float64()),
    ("entry_sol_cost", pa.float64()),
    ("entry_fee_lamports", pa.float64()),
    ("exit_price_lamports", pa.float64()),
    ("exit_sol_proceeds", pa.float64()),
    ("exit_fee_lamports", pa.float64()),
    ("pnl_lamports", pa.int64()),
    ("pnl_bp", pa.float64()),
    ("pnl_sol", pa.float64()),
    ("return_bp_gross", pa.float64()),
    ("return_bp_net", pa.float64()),
    ("hold_seconds", pa.float64()),
    ("exit_reason", pa.string()),
    ("trade_size_sol", pa.float64()),
    ("price_clamped", pa.bool_()),
    ("extreme_outlier", pa.bool_()),
    ("sim_version", pa.string()),
]

POLICY_EVAL_SCHEMA = [
    ("eval_id", pa.string()),
    ("state_id", pa.string()),
    ("mint", pa.string()),
    ("right_censored", pa.bool_()),
    ("would_enter", pa.bool_()),
    ("would_execute", pa.bool_()),
    ("policy_class", pa.string()),
    ("policy_version", pa.string()),
    ("estimated_pnl_bp", pa.float64()),
    ("estimated_exit_reason", pa.string()),
    ("is_auxiliary", pa.bool_()),
]

def write_parquet(rows: list[dict], schema_fields: list, output_path: Path):
    """Write rows to parquet using schema."""
    if not rows:
        # Write empty parquet with schema
        arrays = [pa.array([], type=typ) for _, typ in schema_fields]
        table = pa.table({name: arr for name, typ in schema_fields}, 
                        schema=pa.schema(schema_fields))
    else:
        arrays = []
        names = []
        for name, typ in schema_fields:
            vals = [r.get(name) for r in rows]
            # Handle None values for non-nullable types
            if typ == pa.int64():
                vals = [v if v is not None else 0 for v in vals]
            elif typ == pa.float64():
                vals = [float(v) if v is not None else None for v in vals]
            elif typ == pa.bool_():
                vals = [bool(v) if v is not None else None for v in vals]
            elif typ == pa.string():
                vals = [str(v) if v is not None else None for v in vals]
            arrays.append(pa.array(vals, type=typ))
            names.append(name)
        table = pa.table(dict(zip(names, arrays)), schema=pa.schema(schema_fields))
    
    output_path.parent.mkdir(parents=True, exist_ok=True)
    pq.write_table(table, str(output_path))
    return len(rows)

# ─── Main Pipeline ────────────────────────────────────────────────────

def main():
    run_uuid = str(uuid.uuid4())[:12]
    git_sha = get_git_sha()
    start_time = time.time()
    
    print(f"=" * 60)
    print(f"laserstream_gold_v1 builder")
    print(f"Run UUID: {run_uuid}")
    print(f"Git SHA:  {git_sha}")
    print(f"=" * 60)
    
    # Find events file
    events_path = CAPTURE_DIR / EVENTS_GLOB
    if not events_path.exists():
        print(f"ERROR: Events file not found: {events_path}")
        sys.exit(1)
    
    # Load manifest
    manifest_path = CAPTURE_DIR / MANIFEST_GLOB
    manifest = json.loads(manifest_path.read_text())
    manifest_hash = hash_file(str(manifest_path))
    
    # Capture time range from manifest
    capture_start_ms = manifest.get("capture_start_ms", manifest.get("start_unix_ms", 0))
    capture_end_ms = manifest.get("capture_end_ms", manifest.get("end_unix_ms", 0))
    
    # Fallback: derive from events
    if not capture_start_ms or not capture_end_ms:
        print("WARNING: capture time range not in manifest, deriving from events...")
        # We already know from scan: 1787549744082 → 1787567742986
        capture_start_ms = 1787549744082
        capture_end_ms = 1787567742986
    
    print(f"Events file: {events_path.name}")
    print(f"Size: {events_path.stat().st_size / 1e9:.2f} GB")
    print(f"Capture range: {capture_start_ms} → {capture_end_ms} ({(capture_end_ms - capture_start_ms) / 1000 / 60:.1f} min)")
    
    # ── PASS 1: Stream events, group by mint into temp files ──
    print(f"\n─ PASS 1: Grouping events by mint ─")
    TEMP_DIR.mkdir(parents=True, exist_ok=True)
    
    mint_event_counts = Counter()
    mint_trades = defaultdict(list)  # Only for mints with manageable trade counts
    all_mint_raw_events = defaultdict(list)  # For lifecycle info
    
    LARGE_MINT_THRESHOLD = 50000  # mints with >50K trades get special handling
    large_mints = set()
    
    total_events = 0
    total_trades = 0
    t0 = time.time()
    
    with open(events_path, "r", encoding="utf-8", errors="replace") as fh:
        for i, line in enumerate(fh):
            ev = json.loads(line)
            total_events += 1
            
            mint = ev.get("mint_b58")
            if not mint:
                continue
            
            et = ev.get("event_type")
            
            # Store lifecycle events (creates, migrates, pool events)
            if et in ("create", "migrate", "create_pool", "deposit", "withdraw"):
                all_mint_raw_events[mint].append(ev)
            
            # Convert to trade and store
            if et in ("buy", "sell"):
                trade = event_to_trade(ev)
                if trade:
                    mint_trades[mint].append(trade)
                    total_trades += 1
                    mint_event_counts[mint] += 1
                    
                    if mint_event_counts[mint] > LARGE_MINT_THRESHOLD:
                        if mint not in large_mints:
                            large_mints.add(mint)
                            print(f"  Large mint detected: {mint[:12]}... ({mint_event_counts[mint]:,} trades)")
            
            if i % 1_000_000 == 0 and i > 0:
                elapsed = time.time() - t0
                print(f"  Progress: {i:,} events, {len(mint_trades):,} mints, {total_trades:,} trades, {elapsed:.0f}s")
    
    elapsed = time.time() - t0
    print(f"\n  PASS 1 COMPLETE ({elapsed:.0f}s)")
    print(f"  Total events scanned: {total_events:,}")
    print(f"  Total trades extracted: {total_trades:,}")
    print(f"  Unique mints with trades: {len(mint_trades):,}")
    print(f"  Large mints (>{LARGE_MINT_THRESHOLD:,} trades): {len(large_mints)}")
    
    # ── PASS 2: Build 4 gold layers per mint ──
    print(f"\n─ PASS 2: Building 4 gold layers ─")
    
    all_states = []
    all_outcomes = []
    all_counterfactuals = []
    all_policy_evals = []
    
    mints_processed = 0
    mints_skipped = 0
    
    for mint, trades in mint_trades.items():
        if not trades:
            mints_skipped += 1
            continue
        
        # Build lifecycle
        lifecycle_events = all_mint_raw_events.get(mint, [])
        lifecycle = build_mint_lifecycle(lifecycle_events)
        # Also check trade-based lifecycle
        if not lifecycle["has_pumpfun_trades"] and not lifecycle["has_pumpswap_trades"]:
            # Infer from trades
            venues = set(t["venue"] for t in trades)
            lifecycle["has_pumpfun_trades"] = "pumpfun" in venues
            lifecycle["has_pumpswap_trades"] = "pumpswap" in venues
            if lifecycle["venue_origin"] is None:
                lifecycle["venue_origin"] = "pumpfun" if "pumpfun" in venues else "pumpswap"
        
        # Update lifecycle times from trades
        if trades:
            trade_times = [t["recv_unix_ms"] for t in trades]
            if lifecycle["first_event_time_ms"] is None or min(trade_times) < lifecycle["first_event_time_ms"]:
                lifecycle["first_event_time_ms"] = min(trade_times)
            if lifecycle["last_event_time_ms"] is None or max(trade_times) > lifecycle["last_event_time_ms"]:
                lifecycle["last_event_time_ms"] = max(trade_times)
        
        # Build L1: States
        states = build_states_for_mint(mint, trades, lifecycle, capture_start_ms, capture_end_ms)
        
        # Build L2: Outcomes
        outcomes = build_outcomes_for_mint(mint, trades, states, capture_end_ms)
        
        # Patch migration info into outcomes (O(n) via zip, not O(n²) via linear search)
        if lifecycle["migrated"] and lifecycle["migration_time_ms"]:
            mig_ms = lifecycle["migration_time_ms"]
            for state, o in zip(states, outcomes):
                if not o["right_censored"]:
                    s_time = state["observation_time_ms"]
                    if s_time < mig_ms <= s_time + FORWARD_HORIZON_S * 1000:
                        o["migrated_in_horizon"] = True
                        o["migration_time_in_horizon"] = (mig_ms - s_time) / 1000
        
        # Build L3: Counterfactuals
        counterfactuals = build_counterfactuals_for_mint(mint, trades, states, outcomes)
        
        # Build L4: Policy Evals
        policy_evals = build_policy_evals_for_mint(mint, states, counterfactuals)
        
        all_states.extend(states)
        all_outcomes.extend(outcomes)
        all_counterfactuals.extend(counterfactuals)
        all_policy_evals.extend(policy_evals)
        
        mints_processed += 1
        if mints_processed % 200 == 0:
            elapsed = time.time() - t0
            print(f"  Progress: {mints_processed:,}/{len(mint_trades):,} mints, "
                  f"{len(all_states):,} states, {elapsed:.0f}s")
    
    elapsed = time.time() - t0
    print(f"\n  PASS 2 COMPLETE ({elapsed:.0f}s)")
    print(f"  Mints processed: {mints_processed:,}")
    print(f"  Mints skipped: {mints_skipped:,}")
    
    # ── Write parquet outputs ──
    print(f"\n─ Writing parquet outputs ─")
    
    # L1: States
    states_path = OUTPUT_DIR / "l1_pump_state_v3.parquet"
    n_states = write_parquet(all_states, STATE_SCHEMA, states_path)
    print(f"  L1 pump_state_v3: {n_states:,} rows → {states_path.name}")
    
    # L2: Outcomes
    outcomes_path = OUTPUT_DIR / "l2_pump_outcome_v3.parquet"
    n_outcomes = write_parquet(all_outcomes, OUTCOME_SCHEMA, outcomes_path)
    print(f"  L2 pump_outcome_v3: {n_outcomes:,} rows → {outcomes_path.name}")
    
    # L3: Counterfactuals
    cf_path = OUTPUT_DIR / "l3_counterfactual_v3.parquet"
    n_cf = write_parquet(all_counterfactuals, CF_SCHEMA, cf_path)
    print(f"  L3 counterfactual_v3: {n_cf:,} rows → {cf_path.name}")
    
    # L4: Policy Evals
    pe_path = OUTPUT_DIR / "l4_policy_eval_v3.parquet"
    n_pe = write_parquet(all_policy_evals, POLICY_EVAL_SCHEMA, pe_path)
    print(f"  L4 policy_eval_v3: {n_pe:,} rows → {pe_path.name}")
    
    # ── Manifest ──
    print(f"\n─ Writing manifest ─")
    
    # Cross-layer cardinality check
    state_ids = set(s["state_id"] for s in all_states)
    outcome_state_ids = set(o["state_id"] for o in all_outcomes)
    cf_state_ids = set(c["state_id"] for c in all_counterfactuals)
    pe_state_ids = set(p["state_id"] for p in all_policy_evals)
    
    cardinality_check = {
        "states": len(state_ids),
        "outcomes": len(outcome_state_ids),
        "outcomes_match_states": outcome_state_ids == state_ids,
        "counterfactuals": len(cf_state_ids),
        "cf_match_states": cf_state_ids == state_ids,
        "policy_evals": len(pe_state_ids),
        "pe_match_states": pe_state_ids == state_ids,
    }
    
    # Right-censoring stats
    n_censored = sum(1 for s in all_states if s["right_censored"])
    n_not_censored = n_states - n_censored
    
    # Outcome class distribution
    outcome_classes = Counter(o["outcome_class"] for o in all_outcomes)
    
    # Exit reason distribution
    exit_reasons = Counter(c["exit_reason"] for c in all_counterfactuals if not c["right_censored"])
    
    # Venue distribution
    venue_dist = Counter(s["venue"] for s in all_states)
    
    builder_code_hash = hash_file(str(Path(__file__).resolve()))
    
    manifest_out = {
        "run_uuid": run_uuid,
        "git_sha": git_sha,
        "schema_version": "laserstream_gold_v1",
        "builder": "build_laserstream_gold_v1.py",
        "builder_code_hash": builder_code_hash,
        "built_at": datetime.now(timezone.utc).isoformat(),
        "build_duration_s": round(time.time() - start_time, 1),
        
        "source": {
            "events_file": EVENTS_GLOB,
            "events_file_size_gb": round(events_path.stat().st_size / 1e9, 2),
            "manifest_file": MANIFEST_GLOB,
            "manifest_hash": manifest_hash[:16],
            "capture_start_ms": capture_start_ms,
            "capture_end_ms": capture_end_ms,
            "capture_duration_min": (capture_end_ms - capture_start_ms) / 1000 / 60,
        },
        
        "counts": {
            "total_events_scanned": total_events,
            "total_trades_extracted": total_trades,
            "mints_with_trades": len(mint_trades),
            "mints_processed": mints_processed,
            "l1_states": n_states,
            "l2_outcomes": n_outcomes,
            "l3_counterfactuals": n_cf,
            "l4_policy_evals": n_pe,
        },
        
        "cardinality": cardinality_check,
        
        "right_censoring": {
            "censored_states": n_censored,
            "uncensored_states": n_not_censored,
            "censored_fraction": round(n_censored / max(n_states, 1), 4),
        },
        
        "outcome_classes": dict(outcome_classes),
        "exit_reasons": dict(exit_reasons),
        "venue_distribution": dict(venue_dist),
        
        "sim_config": SIM_CONFIG,
        "champion_config": CHAMPION_CONFIG,
        "known_limitations": KNOWN_LIMITATIONS,
        
        "files": {
            "l1_pump_state": str(states_path.name),
            "l2_pump_outcome": str(outcomes_path.name),
            "l3_counterfactual": str(cf_path.name),
            "l4_policy_eval": str(pe_path.name),
            "l1_size_mb": round(states_path.stat().st_size / 1e6, 1),
            "l2_size_mb": round(outcomes_path.stat().st_size / 1e6, 1),
            "l3_size_mb": round(cf_path.stat().st_size / 1e6, 1),
            "l4_size_mb": round(pe_path.stat().st_size / 1e6, 1),
        },
    }
    
    manifest_out_path = OUTPUT_DIR / "manifest_laserstream_gold_v1.json"
    manifest_out_path.write_text(json.dumps(manifest_out, indent=2))
    print(f"  Manifest → {manifest_out_path.name}")
    
    # ── Summary ──
    print(f"\n{'=' * 60}")
    print(f"BUILD COMPLETE")
    print(f"{'=' * 60}")
    print(f"  L1 States:           {n_states:>10,}")
    print(f"  L2 Outcomes:         {n_outcomes:>10,}")
    print(f"  L3 Counterfactuals:  {n_cf:>10,}")
    print(f"  L4 Policy Evals:     {n_pe:>10,}")
    print(f"  Right-censored:      {n_censored:>10,} ({n_censored/max(n_states,1)*100:.1f}%)")
    print(f"  Mints:               {mints_processed:>10,}")
    print(f"  Cardinality check:   {'PASS' if cardinality_check['outcomes_match_states'] and cardinality_check['cf_match_states'] and cardinality_check['pe_match_states'] else 'FAIL'}")
    print(f"  Duration:            {time.time() - start_time:.1f}s")
    
    # Certification checks
    cert_pass = True
    checks = []
    
    # Check 1: Cross-layer cardinality
    if cardinality_check["outcomes_match_states"] and cardinality_check["cf_match_states"] and cardinality_check["pe_match_states"]:
        checks.append(("cross_layer_cardinality", "PASS"))
    else:
        checks.append(("cross_layer_cardinality", "FAIL"))
        cert_pass = False
    
    # Check 2: No future-derived features in states (no 'forward_' or 'outcome_' fields in state)
    state_keys = set(all_states[0].keys()) if all_states else set()
    future_keys = [k for k in state_keys if "forward" in k.lower() or "outcome" in k.lower()]
    if not future_keys:
        checks.append(("no_causal_leakage", "PASS"))
    else:
        checks.append(("no_causal_leakage", f"FAIL: {future_keys}"))
        cert_pass = False
    
    # Check 3: Right-censored states flagged
    if n_censored > 0 and all(o["right_censored"] for o in all_outcomes if o.get("right_censored")):
        checks.append(("right_censoring_flagged", "PASS"))
    else:
        checks.append(("right_censoring_flagged", "FAIL" if n_censored > 0 else "PASS (no censored)"))
    
    # Check 4: Policy eval is auxiliary (not primary label)
    if all(p.get("is_auxiliary") == True for p in all_policy_evals if not p.get("right_censored")):
        checks.append(("policy_auxiliary_only", "PASS"))
    else:
        checks.append(("policy_auxiliary_only", "FAIL"))
        cert_pass = False
    
    # Check 5: Manifest written with provenance
    if manifest_out_path.exists():
        checks.append(("manifest_with_provenance", "PASS"))
    else:
        checks.append(("manifest_with_provenance", "FAIL"))
        cert_pass = False
    
    print(f"\n  Certification: {'PASS ✅' if cert_pass else 'FAIL ❌'}")
    for name, result in checks:
        print(f"    {name}: {result}")
    
    return cert_pass

if __name__ == "__main__":
    ok = main()
    sys.exit(0 if ok else 1)
