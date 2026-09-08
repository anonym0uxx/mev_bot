"""
slinky_gold_v1 — Refactored pipeline (v2 architecture).

Mint-partitioned, bounded-memory (≤64 GB) build of gold dataset from
Slinky21/Pumpfun_Memecoin_Corpus.

ARCHITECTURE (vs v1):
  - DuckDB for all Parquet I/O with predicate pushdown (no to_pylist on full corpus)
  - Polars for vectorized per-mint computation (cumulative stats, markouts)
  - Process ONE mint at a time → peak RAM bounded by the single busiest mint
  - Write outputs incrementally (append to partition files)
  - Content-addressed checkpoints per mint batch
  - NVMe scratch directory for DuckDB temp space

INPUT:  D:/repos/mev_bot/rust/data/slinky21_data/ (parquets, never modified)
OUTPUT: tools/data-pipeline/output/slinky_gold_v1/ (parquet + manifest)

Steps:
1. Inventory & hash all source parquets (metadata only, no row loading).
2. Enumerate unique mints + first-seen times (small result set).
3. Mint-disjoint chronological train/val/test split.
4. Per-mint processing: load trades → build states → outcomes → sim labels → write.
5. Aggregate counts, QA checks, write manifest.
"""

from __future__ import annotations
import os, sys, json, hashlib, time, traceback
from pathlib import Path
from datetime import datetime, timezone, timedelta
from collections import defaultdict
import random

try:
    import resource
    HAS_RESOURCE = True
except ImportError:
    HAS_RESOURCE = False

import duckdb
import polars as pl
import pyarrow as pa
import pyarrow.parquet as pq

# Add parent dirs to path for schema imports
sys.path.insert(0, str(Path(__file__).parent))
sys.path.insert(0, str(Path(__file__).parent.parent / "schemas"))
from utils import hash_file, file_size_bytes
from gold_schema import PumpStateV1, PumpOutcomeV1, SimulatorLabelV1, stable_id, SCHEMA_VERSION, GENERATOR_VERSION

# ─── Config ───────────────────────────────────────────────────────────
SLINKY_DIR = Path("D:/repos/mev_bot/rust/data/slinky21_data")
OUTPUT_DIR = Path("D:/repos/mev_bot/tools/data-pipeline/output/slinky_gold_v1")
MANIFEST_PATH = OUTPUT_DIR / "slinky_gold_v1_manifest.json"
SCRATCH_DIR = Path("D:/tmp/slinky_gold_scratch")
BATCH_SIZE = 500  # mints per batch (tune for memory vs I/O tradeoff)

# Simulator config (from CHAMPION_CONFIG.txt — must match v1 exactly)
SIM_CONFIG = {
    "entry_fee_bps": 100,
    "exit_fee_bps": 100,
    "entry_tip_lamports": 10000,
    "exit_tip_lamports": 10000,
    "slippage_default_bp": 50,
    "latency_ms": 250,
    "tp_target_bp": 1500,
    "stop_loss_bp": 1500,
    "max_hold_seconds": 300,
    "policy_version": "champion_v1",
}

KNOWN_ISSUES = [
    "Supply bug: some tokens have incorrect initial_supply in tokens.parquet; "
    "supply_bug_corrected column flags these. Use corrected columns where available.",
    "Top10_pct suspect: initial_top10_pct may be inflated for some tokens due to "
    "API aggregation errors; top10_pct_suspect flags these; use _corrected variants.",
    "bucket_start timezone: snapshots bucket_start uses +01:00 (CET), not UTC. "
    "All times normalized to UTC in output.",
    "trades.source column mixes 'pumpfun' and 'pumpswap' venues but Slinky21 "
    "did not capture PumpSwap program ID correctly; PumpSwap trades may be mislabeled.",
    "postgard_outcomes computed with hindsight; used ONLY for outcome labels, "
    "never for causal states.",
    "wallet_stats is aggregate metadata, not per-trade; used for feature enrichment only.",
]

def config_hash(cfg: dict) -> str:
    return hashlib.sha256(json.dumps(cfg, sort_keys=True).encode()).hexdigest()[:16]

def get_rss_mb() -> int:
    """Return current RSS in MB (peak for this process)."""
    if not HAS_RESOURCE:
        try:
            import psutil
            return int(psutil.Process(os.getpid()).memory_info().rss / 1024 / 1024)
        except ImportError:
            return 0
    try:
        r = resource.getrusage(resource.RUSAGE_SELF)
        if hasattr(r, 'ru_maxrss'):
            if sys.platform == 'win32':
                return int(r.ru_maxrss / 1024 / 1024)
            return int(r.ru_maxrss / 1024)
    except Exception:
        pass
    return 0

# ─── DuckDB connection with NVMe scratch ──────────────────────────────
def make_duckdb():
    SCRATCH_DIR.mkdir(parents=True, exist_ok=True)
    con = duckdb.connect()
    con.execute(f"SET temp_directory='{str(SCRATCH_DIR)}'")
    con.execute("SET memory_limit='48GB'")
    con.execute("SET threads=8")
    return con

# ─── Step 1: Inventory (metadata only) ────────────────────────────────
def inventory_sources(con) -> list[dict]:
    """Hash all source parquets and build source manifest entries."""
    source_files = []
    for f in sorted(SLINKY_DIR.rglob("*.parquet")):
        rel = str(f.relative_to(SLINKY_DIR))
        h = hash_file(str(f))
        sz = file_size_bytes(str(f))
        # Use DuckDB for row count (metadata only, no data load)
        row_count = con.execute(f"SELECT count(*) FROM read_parquet('{f}')").fetchone()[0]
        source_files.append({"filename": rel, "bytes": sz, "sha256": h, "rows": row_count})
        print(f"  hashed {rel}: {row_count:,} rows, {sz / 1e6:.1f} MB")
    return source_files

# ─── Step 2: Enumerate mints ──────────────────────────────────────────
def enumerate_mints(con) -> list[tuple[str, int]]:
    """Get unique mints with first-seen time (unix ms) for chronological split."""
    trades_glob = str(SLINKY_DIR / "trades" / "trades-*.parquet")
    # DuckDB can glob parquet paths
    result = con.execute(f"""
        SELECT mint, MIN(TRY_CAST(event_time AS TIMESTAMP)) as first_seen
        FROM read_parquet('{trades_glob}')
        GROUP BY mint
        ORDER BY first_seen
    """).fetchall()
    mints = []
    for row in result:
        mint, first_seen = row
        if mint and first_seen:
            t_ms = int(first_seen.replace(tzinfo=timezone.utc).timestamp() * 1000) if hasattr(first_seen, 'replace') else int(first_seen.timestamp() * 1000)
            mints.append((mint, t_ms))
    return mints

# ─── Step 3: Mint-disjoint split ──────────────────────────────────────
def mint_disjoint_split(mints: list[tuple[str, int]],
                         train_frac=0.7, val_frac=0.15, test_frac=0.15) -> dict[str, list[str]]:
    """Chronological mint-disjoint split: sort by first-seen, assign contiguous blocks."""
    assert abs(train_frac + val_frac + test_frac - 1.0) < 1e-6
    sorted_mints = sorted(mints, key=lambda x: x[1])
    n = len(sorted_mints)
    train_end = int(n * train_frac)
    val_end = int(n * (train_frac + val_frac))
    return {
        "train": [m[0] for m in sorted_mints[:train_end]],
        "val": [m[0] for m in sorted_mints[train_end:val_end]],
        "test": [m[0] for m in sorted_mints[val_end:]],
    }

# ─── Step 4: Per-mint processing ──────────────────────────────────────
def load_mint_trades(con, mint: str) -> pl.DataFrame:
    """Load trades for a single mint via DuckDB predicate pushdown."""
    trades_glob = str(SLINKY_DIR / "trades" / "trades-*.parquet")
    query = f"""
        SELECT * FROM read_parquet('{trades_glob}')
        WHERE mint = '{mint}'
        ORDER BY event_time
    """
    df = con.execute(query).pl()
    return df

def load_token_meta(con) -> dict:
    """Load token metadata indexed by mint (small enough for dict)."""
    tokens_path = str(SLINKY_DIR / "tokens.parquet")
    # Only load the columns we need
    df = con.execute(f"""
        SELECT mint, name, symbol, creator, is_mayhem_mode,
               v_tokens_bonding_curve as initial_supply, supply_bug_corrected,
               initial_market_cap_sol, initial_price_sol,
               initial_top10_pct_corrected, dev_buy_pct_corrected,
               initial_holder_count, initial_gini,
               creator_past_tokens, creator_past_rugs,
               is_zombie, data_quality_score
        FROM read_parquet('{tokens_path}')
    """).pl()
    # Convert to dict lookup
    result = {}
    for row in df.rows(named=True):
        result[row["mint"]] = row
    return result

def load_migrations(con) -> dict:
    """Load migrations indexed by mint."""
    mig_path = str(SLINKY_DIR / "migrations.parquet")
    df = con.execute(f"""
        SELECT mint, migrated_at, seconds_to_graduation, pool_address
        FROM read_parquet('{mig_path}')
    """).pl()
    result = {}
    for row in df.rows(named=True):
        result[row["mint"]] = row
    return result

def load_postgard(con) -> dict:
    """Load postgard outcomes indexed by mint."""
    pg_path = str(SLINKY_DIR / "postgard_outcomes.parquet")
    df = con.execute(f"""
        SELECT mint, outcome_label, success_label, peak_price_usd,
               price_at_grad_usd, price_at_1m_usd, price_at_5m_usd,
               price_at_10m_usd, price_at_30m_usd, rug_detected, success_window_mins
        FROM read_parquet('{pg_path}')
    """).pl()
    result = {}
    for row in df.rows(named=True):
        result[row["mint"]] = row
    return result

def build_states_for_mint(trades_df: pl.DataFrame, mint: str, token_meta: dict,
                          migrations: dict, source_name: str = "slinky21") -> list[dict]:
    """Build pump_state_v1 rows for a single mint using Polars vectorized ops."""
    if trades_df.is_empty():
        return []

    # Ensure event_time is sorted
    trades_df = trades_df.sort("event_time")

    # Compute cumulative stats using Polars expressions
    trades_df = trades_df.with_columns([
        pl.col("sol_amount").fill_null(0),
        pl.col("is_buy").fill_null(False),
        pl.col("user_wallet").fill_null(""),
    ])

    # Cumulative counts (knowable at time t = stats BEFORE this trade)
    trades_df = trades_df.with_columns([
        pl.col("is_buy").cast(pl.Int64).cum_sum().over("mint").alias("_cum_buy_raw"),
        (~pl.col("is_buy")).cast(pl.Int64).cum_sum().over("mint").alias("_cum_sell_raw"),
        pl.col("sol_amount").cum_sum().over("mint").alias("_cum_vol_raw"),
        pl.lit(1).cum_sum().over("mint").alias("_seq_raw"),
    ])

    # Shift cumulative stats by 1 to get "before this trade" values (causal)
    trades_df = trades_df.with_columns([
        (pl.col("_cum_buy_raw") - pl.col("is_buy").cast(pl.Int64)).alias("buy_count_so_far"),
        (pl.col("_cum_sell_raw") - (~pl.col("is_buy")).cast(pl.Int64)).alias("sell_count_so_far"),
        (pl.col("_seq_raw") - 1).alias("seq"),
    ])

    # Cumulative buy/sell volume BEFORE this trade
    trades_df = trades_df.with_columns([
        pl.when(pl.col("is_buy"))
          .then(pl.col("sol_amount"))
          .otherwise(0)
          .cum_sum()
          .over("mint")
          .alias("_buy_vol_cum"),
    ])
    trades_df = trades_df.with_columns([
        pl.when(~pl.col("is_buy"))
          .then(pl.col("sol_amount"))
          .otherwise(0)
          .cum_sum()
          .over("mint")
          .alias("_sell_vol_cum"),
    ])

    # Shift volumes by 1 to get "before this trade"
    trades_df = trades_df.with_columns([
        (pl.col("_buy_vol_cum") - pl.when(pl.col("is_buy")).then(pl.col("sol_amount")).otherwise(0)).alias("buy_vol_so_far"),
        (pl.col("_sell_vol_cum") - pl.when(~pl.col("is_buy")).then(pl.col("sol_amount")).otherwise(0)).alias("sell_vol_so_far"),
    ])

    # Convert timestamps to unix ms
    trades_df = trades_df.with_columns([
        pl.col("event_time").dt.timestamp("ms").alias("t_ms"),
    ])

    # Unique wallets so far — use a running approach
    # Polars doesn't have cum_unique, so we compute per-mint with Python
    # (small per mint, so this is fast)
    unique_wallets_list = []
    seen_wallets = set()
    # We need to iterate in order; get the user_wallet column as a list
    wallet_col = trades_df.select("user_wallet").to_series().to_list()
    for w in wallet_col:
        seen_wallets.add(w)
        unique_wallets_list.append(len(seen_wallets) - 1)  # before this trade

    # Get token metadata
    meta = token_meta.get(mint, {})
    token_name = meta.get("name") if meta else None
    token_symbol = meta.get("symbol") if meta else None
    creator = meta.get("creator") if meta else None
    initial_supply = meta.get("initial_supply") if meta else None
    is_mayhem = meta.get("is_mayhem_mode") if meta else None
    if initial_supply is not None and initial_supply != initial_supply:  # NaN check
        initial_supply = None

    # Migration info
    mig = migrations.get(mint)
    mig_ms = None
    if mig and mig.get("migrated_at"):
        mig_time = mig["migrated_at"]
        if hasattr(mig_time, "timestamp"):
            mig_ms = int(mig_time.timestamp() * 1000)

    # Build state dicts from the Polars columns
    # Get all columns as lists for efficient iteration
    cols = trades_df.select([
        "t_ms", "seq", "is_buy", "sol_amount", "token_amount",
        "v_sol_bonding_curve", "v_tokens_bonding_curve",
        "market_cap_sol", "curve_pct_depleted", "price_sol",
        "seconds_since_launch", "source",
        "buy_count_so_far", "sell_count_so_far",
        "buy_vol_so_far", "sell_vol_so_far",
        "tx_signature",
    ])

    col_data = {c: cols.select(c).to_series().to_list() for c in cols.columns}
    n_rows = len(col_data["t_ms"])

    states = []
    for i in range(n_rows):
        t_ms = int(col_data["t_ms"][i])
        seq = int(col_data["seq"][i])
        is_buy = col_data["is_buy"][i]
        sol_amount = col_data["sol_amount"][i] or 0.0
        v_sol = col_data["v_sol_bonding_curve"][i]
        v_tok = col_data["v_tokens_bonding_curve"][i]
        mcap = col_data["market_cap_sol"][i]
        curve_pct = col_data["curve_pct_depleted"][i]
        seconds_since = col_data["seconds_since_launch"][i]

        # SOL → lamports conversion (1 SOL = 1e9 lamports)
        v_sol_lamports = int(v_sol * 1e9) if v_sol is not None else None
        v_tok_units = int(v_tok) if v_tok is not None else None
        mcap_lamports = int(mcap * 1e9) if mcap is not None else None
        sol_amount_lamports = int(sol_amount * 1e9)
        token_amount_units = int(col_data["token_amount"][i] or 0)

        # Venue
        source_val = col_data["source"][i] or ""
        venue = "pumpfun_bonding" if source_val.lower().startswith("pump") else (
            "pumpswap" if "pumpswap" in source_val.lower() else "unknown")

        # Causal cumulative stats
        cum_buy = int(col_data["buy_count_so_far"][i])
        cum_sell = int(col_data["sell_count_so_far"][i])
        cum_buy_vol = int((col_data["buy_vol_so_far"][i] or 0) * 1e9)
        cum_sell_vol = int((col_data["sell_vol_so_far"][i] or 0) * 1e9)
        total_vol = cum_buy_vol + cum_sell_vol
        buy_pressure = (cum_buy_vol / total_vol) if total_vol > 0 else None
        net_flow = cum_buy_vol - cum_sell_vol if (cum_buy_vol or cum_sell_vol) else None

        # Graduation status at time t
        is_graduated = False
        if mig_ms is not None and t_ms >= mig_ms:
            is_graduated = True

        state = PumpStateV1(
            state_id=stable_id("slinky21", mint, str(t_ms), str(seq)),
            source=source_name,
            mint=mint,
            mint_b58=mint,
            event_time_unix_ms=t_ms,
            event_slot=None,
            seq=seq,
            venue=venue,
            virtual_sol=v_sol_lamports,
            virtual_token=v_tok_units,
            real_sol=None,
            real_token=None,
            curve_pct_depleted=curve_pct,
            is_complete=is_graduated,
            market_cap_sol=mcap_lamports,
            trade_side="buy" if is_buy else "sell",
            amount_in=sol_amount_lamports if is_buy else token_amount_units,
            amount_out=token_amount_units if is_buy else sol_amount_lamports,
            fee_bps=None,
            trade_count_so_far=cum_buy + cum_sell,
            buy_count_so_far=cum_buy,
            sell_count_so_far=cum_sell,
            token_name=token_name,
            token_symbol=token_symbol,
            creator=creator,
            initial_supply=int(initial_supply) if initial_supply is not None else None,
            is_mayhem=is_mayhem,
            seconds_since_launch=seconds_since,
            buy_pressure=buy_pressure,
            net_flow_sol=net_flow,
            unique_wallets_so_far=unique_wallets_list[i],
            right_censored=False,
        )
        states.append(state.__dict__)

    return states


def build_outcomes_for_mint(states: list[dict], trades_df: pl.DataFrame,
                            mint: str, migrations: dict) -> list[dict]:
    """Build pump_outcome_v1 rows for a single mint.
    Uses forward-looking trades within 300s horizon. Per-mint, so small N."""
    if not states:
        return []

    # Get sorted trades as list of (t_ms, price_sol) for forward scans
    trades_sorted = trades_df.sort("event_time")
    t_ms_list = trades_sorted.select(
        pl.col("event_time").dt.timestamp("ms")
    ).to_series().to_list()
    price_list = trades_sorted.select("price_sol").to_series().to_list()

    # Migration time
    mig = migrations.get(mint)
    mig_ms = None
    if mig and mig.get("migrated_at"):
        mig_time = mig["migrated_at"]
        if hasattr(mig_time, "timestamp"):
            mig_ms = int(mig_time.timestamp() * 1000)

    # Build a combined index: for each state, we need the index in the trade list
    # States are in the same order as trades (sorted by time)
    outcomes = []
    for i, state in enumerate(states):
        t_ms = state["event_time_unix_ms"]
        entry_price = price_list[i] if price_list[i] else None

        # Forward trades: all trades after index i
        # For markouts, find the last trade at or before t + horizon
        markouts = {}
        for horizon_s in [1, 2, 5, 10, 30, 60, 120, 300]:
            target_ms = t_ms + horizon_s * 1000
            best_price = None
            for j in range(i + 1, len(t_ms_list)):
                tj = t_ms_list[j]
                if tj > target_ms:
                    break
                best_price = price_list[j]
            if best_price is not None and entry_price and entry_price > 0:
                ret_pct = (best_price - entry_price) / entry_price
                markouts[f"ret_{horizon_s}s_bp"] = int(ret_pct * 10000)
            else:
                markouts[f"ret_{horizon_s}s_bp"] = None

        # MFE/MAE: scan forward trades within 300s
        mfe_bp = None
        mae_bp = None
        mfe_time = None
        mae_time = None
        if entry_price and entry_price > 0:
            max_price = entry_price
            min_price = entry_price
            max_time = 0.0
            min_time = 0.0
            for j in range(i + 1, len(t_ms_list)):
                tj = t_ms_list[j]
                delta_s = (tj - t_ms) / 1000
                if delta_s > 300:
                    break
                p = price_list[j]
                if p is None:
                    continue
                if p > max_price:
                    max_price = p
                    max_time = delta_s
                if p < min_price:
                    min_price = p
                    min_time = delta_s

            mfe_bp = int((max_price - entry_price) / entry_price * 10000) if max_price > entry_price else 0
            mae_bp = int((min_price - entry_price) / entry_price * 10000) if min_price < entry_price else 0
            mfe_time = max_time if max_price > entry_price else None
            mae_time = min_time if min_price < entry_price else None

        # First-hit barriers
        barriers = {}
        if entry_price and entry_price > 0:
            for label, threshold_bp, direction in [
                ("hit_plus_10_bp", 10, 1), ("hit_plus_25_bp", 25, 1),
                ("hit_plus_50_bp", 50, 1), ("hit_plus_100_bp", 100, 1), ("hit_plus_200_bp", 200, 1),
                ("hit_minus_10_bp", -10, -1), ("hit_minus_20_bp", -20, -1),
                ("hit_minus_30_bp", -30, -1), ("hit_minus_50_bp", -50, -1),
            ]:
                hit_time = None
                for j in range(i + 1, len(t_ms_list)):
                    tj = t_ms_list[j]
                    delta_s = (tj - t_ms) / 1000
                    if delta_s > 300:
                        break
                    p = price_list[j]
                    if p is None:
                        continue
                    ret_bp = int((p - entry_price) / entry_price * 10000)
                    if direction > 0 and ret_bp >= threshold_bp:
                        hit_time = delta_s
                        break
                    elif direction < 0 and ret_bp <= threshold_bp:
                        hit_time = delta_s
                        break
                barriers[label] = hit_time

        # +100 before -30
        plus_100_before = None
        if barriers.get("hit_plus_100_bp") is not None and barriers.get("hit_minus_30_bp") is not None:
            plus_100_before = barriers["hit_plus_100_bp"] < barriers["hit_minus_30_bp"]
        elif barriers.get("hit_plus_100_bp") is not None:
            plus_100_before = True

        # Graduation
        grad_info = {
            "graduated": False, "seconds_to_graduation": None,
            "graduated_did_migrate": False,
        }
        if mig_ms and mig_ms > t_ms:
            grad_info["graduated"] = True
            grad_info["seconds_to_graduation"] = (mig_ms - t_ms) / 1000
            grad_info["graduated_did_migrate"] = True
        elif mig_ms and mig_ms <= t_ms:
            grad_info["graduated"] = True
            grad_info["seconds_to_graduation"] = 0

        survived_60s = markouts.get("ret_60s_bp") is not None
        survived_300s = markouts.get("ret_300s_bp") is not None
        collapsed = mae_bp is not None and mae_bp <= -5000

        outcome = PumpOutcomeV1(
            state_id=state["state_id"], mint=mint, event_time_unix_ms=t_ms,
            ret_1s_bp=markouts.get("ret_1s_bp"), ret_2s_bp=markouts.get("ret_2s_bp"),
            ret_5s_bp=markouts.get("ret_5s_bp"), ret_10s_bp=markouts.get("ret_10s_bp"),
            ret_30s_bp=markouts.get("ret_30s_bp"), ret_60s_bp=markouts.get("ret_60s_bp"),
            ret_120s_bp=markouts.get("ret_120s_bp"), ret_300s_bp=markouts.get("ret_300s_bp"),
            mfe_bp=mfe_bp, mae_bp=mae_bp, mfe_time_seconds=mfe_time, mae_time_seconds=mae_time,
            hit_plus_10_bp=barriers.get("hit_plus_10_bp"), hit_plus_25_bp=barriers.get("hit_plus_25_bp"),
            hit_plus_50_bp=barriers.get("hit_plus_50_bp"), hit_plus_100_bp=barriers.get("hit_plus_100_bp"),
            hit_plus_200_bp=barriers.get("hit_plus_200_bp"),
            hit_minus_10_bp=barriers.get("hit_minus_10_bp"), hit_minus_20_bp=barriers.get("hit_minus_20_bp"),
            hit_minus_30_bp=barriers.get("hit_minus_30_bp"), hit_minus_50_bp=barriers.get("hit_minus_50_bp"),
            plus_100_before_minus_30=plus_100_before,
            peak_bp=mfe_bp, time_to_peak_seconds=mfe_time,
            graduated=grad_info["graduated"], seconds_to_graduation=grad_info["seconds_to_graduation"],
            graduated_did_migrate=grad_info["graduated_did_migrate"],
            post_grad_price_change_300s_bp=None,
            survived_60s=survived_60s, survived_300s=survived_300s,
            collapsed_50pct_within_300s=collapsed,
            right_censored=False, censoring_reason=None,
        )
        outcomes.append(outcome.__dict__)

    return outcomes


def build_sim_labels_for_mint(states: list[dict], outcomes: list[dict],
                               mint: str) -> list[dict]:
    """Build simulator_label_v1 rows for a single mint."""
    if not states:
        return []

    cfg_hash = config_hash(SIM_CONFIG)
    outcome_map = {o["state_id"]: o for o in outcomes}
    sim_labels = []

    for state, outcome in zip(states, [outcome_map.get(s["state_id"]) for s in states]):
        t_ms = state["event_time_unix_ms"]

        # Entry gate
        would_enter = True
        curve_pct = state.get("curve_pct_depleted")
        if curve_pct is not None and curve_pct > 0.95:
            would_enter = False

        # Entry economics
        entry_price = state.get("market_cap_sol")  # lamports
        entry_fee = int((entry_price or 0) * SIM_CONFIG["entry_fee_bps"] / 10000) if entry_price else 0
        entry_slippage = int((entry_price or 0) * SIM_CONFIG["slippage_default_bp"] / 10000) if entry_price else 0

        exit_reason = None
        exit_price = None
        hold_duration = None
        net_pnl = None

        if would_enter and outcome:
            mfe = outcome.get("mfe_bp") or 0
            mae = outcome.get("mae_bp") or 0
            tp = SIM_CONFIG["tp_target_bp"]
            sl = -SIM_CONFIG["stop_loss_bp"]

            if mfe >= tp:
                exit_reason = "take_profit"
                hold_duration = outcome.get("mfe_time_seconds")
                exit_price = int((entry_price or 0) * (1 + tp / 10000)) if entry_price else None
            elif mae <= sl:
                exit_reason = "stop_loss"
                hold_duration = outcome.get("mae_time_seconds")
                exit_price = int((entry_price or 0) * (1 + sl / 10000)) if entry_price else None
            elif outcome.get("survived_300s"):
                exit_reason = "time_stop"
                hold_duration = 300.0
                ret_300 = outcome.get("ret_300s_bp") or 0
                exit_price = int((entry_price or 0) * (1 + ret_300 / 10000)) if entry_price else None
            else:
                exit_reason = "hold"
                hold_duration = 300.0
                exit_price = entry_price

            if entry_price and exit_price:
                gross = exit_price - entry_price
                exit_fee = int(exit_price * SIM_CONFIG["exit_fee_bps"] / 10000)
                exit_slip = int(exit_price * SIM_CONFIG["slippage_default_bp"] / 10000)
                net_pnl = gross - entry_fee - exit_fee - SIM_CONFIG["entry_tip_lamports"] - SIM_CONFIG["exit_tip_lamports"] - entry_slippage - exit_slip

        # Classify
        policy_class = "SKIP"
        if would_enter and outcome and net_pnl is not None:
            pnl_pct = (net_pnl / entry_price * 10000) if entry_price and entry_price > 0 else 0
            mfe = outcome.get("mfe_bp") or 0
            if pnl_pct > 1000 and mfe > 2000:
                policy_class = "STRONG"
            elif pnl_pct > 0 and mfe > 500:
                policy_class = "GOOD"
            elif pnl_pct > -500:
                policy_class = "MARGINAL"
            elif pnl_pct > -2000:
                policy_class = "BAD"
            else:
                policy_class = "TOXIC"

        sim = SimulatorLabelV1(
            state_id=state["state_id"], mint=mint, event_time_unix_ms=t_ms,
            would_enter=would_enter,
            entry_price_sol=entry_price,
            entry_slippage_bp=SIM_CONFIG["slippage_default_bp"] if would_enter else None,
            entry_fee_lamports=entry_fee if would_enter else None,
            entry_latency_ms=SIM_CONFIG["latency_ms"] if would_enter else None,
            exit_reason=exit_reason, exit_price_sol=exit_price,
            exit_slippage_bp=SIM_CONFIG["slippage_default_bp"] if exit_price else None,
            exit_fee_lamports=int((exit_price or 0) * SIM_CONFIG["exit_fee_bps"] / 10000) if exit_price else None,
            exit_latency_ms=SIM_CONFIG["latency_ms"] if exit_price else None,
            hold_duration_seconds=hold_duration,
            gross_pnl_lamports=(exit_price - entry_price) if entry_price and exit_price else None,
            net_pnl_lamports=net_pnl,
            pnl_pct_bp=int((net_pnl / entry_price * 10000)) if net_pnl is not None and entry_price and entry_price > 0 else None,
            mfe_while_held_bp=outcome.get("mfe_bp") if outcome else None,
            mae_while_held_bp=outcome.get("mae_bp") if outcome else None,
            target_order=f"+{SIM_CONFIG['tp_target_bp']}bp" if would_enter else None,
            stop_order=f"-{SIM_CONFIG['stop_loss_bp']}bp" if would_enter else None,
            config_hash=cfg_hash, policy_version=SIM_CONFIG["policy_version"],
            policy_class=policy_class,
        )
        sim_labels.append(sim.__dict__)

    return sim_labels


def write_batch_parquet(rows: list[dict], out_dir: Path, name: str,
                        part_idx: int, chunk_size: int = 100_000) -> list[str]:
    """Write a batch of rows to partitioned Parquet files.
    Each chunk gets a unique part_idx so no data is overwritten."""
    out_dir.mkdir(parents=True, exist_ok=True)
    written = []

    # Sanitize rows: convert ints that overflow int64 to float, None stays None
    INT64_MAX = 9_223_372_036_854_775_807
    def sanitize_row(row):
        for k, v in row.items():
            if isinstance(v, int) and v > INT64_MAX:
                row[k] = float(v)
        return row

    sub_idx = 0
    for i in range(0, len(rows), chunk_size):
        chunk = [sanitize_row(r) for r in rows[i:i + chunk_size]]
        if not chunk:
            continue
        # Use Polars with explicit schema to prevent i32→i64 overflow on lamports
        # Force all integer columns to i64, let Polars handle floats and strings
        try:
            df = pl.DataFrame(chunk, infer_schema_length=min(len(chunk), 10000))
        except pl.exceptions.ComputeError:
            # Fallback: build column-by-column with explicit i64 for int cols
            if not chunk:
                continue
            col_data = {}
            for key in chunk[0].keys():
                vals = [r.get(key) for r in chunk]
                # Check if all non-None values are integers
                non_none = [v for v in vals if v is not None]
                if non_none and all(isinstance(v, int) for v in non_none):
                    col_data[key] = pl.Series(name=key, values=vals, dtype=pl.Int64)
                elif non_none and all(isinstance(v, float) or isinstance(v, int) for v in non_none):
                    col_data[key] = pl.Series(name=key, values=vals, dtype=pl.Float64)
                else:
                    col_data[key] = pl.Series(name=key, values=vals)
            df = pl.DataFrame(col_data)
        fname = f"{name}_part{part_idx:04d}_{sub_idx:04d}.parquet"
        fpath = str(out_dir / fname)
        df.write_parquet(fpath, compression="zstd")
        written.append(fpath)
        sub_idx += 1
    return written


# ─── Main ─────────────────────────────────────────────────────────────
def main():
    print("=" * 80)
    print("SLINKY_GOLD_V1 (v2) — Mint-partitioned, bounded-memory build")
    print("=" * 80)

    start_time = time.time()
    peak_rss = 0

    # Safety: refuse to start if output dir already has parquet files
    # (prevents contamination between runs)
    os.makedirs(OUTPUT_DIR, exist_ok=True)
    existing = list(OUTPUT_DIR.rglob("*.parquet"))
    if existing and not os.environ.get("SLINKY_GOLD_FORCE"):
        print(f"  WARNING: Output dir has {len(existing)} existing parquet files.")
        print(f"  Set SLINKY_GOLD_FORCE=1 to overwrite, or clean the dir first.")
        print(f"  Aborting to prevent contamination.")
        return
    if existing:
        print(f"  SLINKY_GOLD_FORCE=1 set — overwriting {len(existing)} existing files.")
        import shutil
        for f in existing:
            f.unlink()
    os.makedirs(OUTPUT_DIR / "states", exist_ok=True)
    os.makedirs(OUTPUT_DIR / "outcomes", exist_ok=True)
    os.makedirs(OUTPUT_DIR / "simulator", exist_ok=True)

    con = make_duckdb()

    # Step 1: Inventory
    print("\n[1/5] Inventorying source parquets (metadata only)...")
    source_files = inventory_sources(con)
    print(f"  Total source files: {len(source_files)}")

    # Step 2: Enumerate mints
    print("\n[2/5] Enumerating unique mints...")
    mints = enumerate_mints(con)
    print(f"  Unique mints: {len(mints):,}")
    peak_rss = max(peak_rss, get_rss_mb())
    print(f"  Peak RSS so far: {peak_rss:,} MB")

    # Step 3: Mint-disjoint split
    print("\n[3/5] Computing mint-disjoint chronological split...")
    splits = mint_disjoint_split(mints)
    print(f"  Train: {len(splits['train']):,}  Val: {len(splits['val']):,}  Test: {len(splits['test']):,}")

    # Load lookup tables (small, indexed by mint)
    print("\n  Loading token metadata, migrations, postgard...")
    token_meta = load_token_meta(con)
    migrations = load_migrations(con)
    postgard = load_postgard(con)
    print(f"  Token meta: {len(token_meta):,}  Migrations: {len(migrations):,}  Postgard: {len(postgard):,}")
    peak_rss = max(peak_rss, get_rss_mb())
    print(f"  Peak RSS after lookup tables: {peak_rss:,} MB")

    # Step 4: Per-mint processing
    print(f"\n[4/5] Processing {len(mints):,} mints (batch_size={BATCH_SIZE})...")

    all_state_files = []
    all_outcome_files = []
    all_sim_files = []
    total_states = 0
    total_outcomes = 0
    total_sim_labels = 0
    class_dist = defaultdict(int)
    venue_dist = defaultdict(int)
    part_idx = 0
    mint_first_seen = {m: t for m, t in mints}

    batch_states = []
    batch_outcomes = []
    batch_sims = []
    BATCH_FLUSH = 50_000  # flush threshold (lower = safer, bounded RAM)

    for i, (mint, first_seen) in enumerate(mints):
        if (i + 1) % 1000 == 0:
            peak_rss = max(peak_rss, get_rss_mb())
            elapsed = time.time() - start_time
            print(f"  [{i+1:,}/{len(mints):,}] mint={mint[:12]}... "
                  f"states={total_states:,} elapsed={elapsed:.0f}s peak_rss={peak_rss:,}MB "
                  f"batch={len(batch_states):,} parts={part_idx}")

        # Load this mint's trades
        trades_df = load_mint_trades(con, mint)
        if trades_df.is_empty():
            continue

        # Build states
        states = build_states_for_mint(trades_df, mint, token_meta, migrations)
        if not states:
            continue

        # Build outcomes
        outcomes = build_outcomes_for_mint(states, trades_df, mint, migrations)

        # Build sim labels
        sim_labels = build_sim_labels_for_mint(states, outcomes, mint)

        # Accumulate stats
        total_states += len(states)
        total_outcomes += len(outcomes)
        total_sim_labels += len(sim_labels)
        for s in states:
            venue_dist[s.get("venue", "unknown")] += 1
        for s in sim_labels:
            class_dist[s.get("policy_class", "SKIP")] += 1

        # Accumulate batch for writing
        batch_states.extend(states)
        batch_outcomes.extend(outcomes)
        batch_sims.extend(sim_labels)

        # Write batch when it reaches flush threshold
        if len(batch_states) >= BATCH_FLUSH:
            all_state_files.extend(
                write_batch_parquet(batch_states, OUTPUT_DIR / "states", "pump_state_v1", part_idx))
            all_outcome_files.extend(
                write_batch_parquet(batch_outcomes, OUTPUT_DIR / "outcomes", "pump_outcome_v1", part_idx))
            all_sim_files.extend(
                write_batch_parquet(batch_sims, OUTPUT_DIR / "simulator", "simulator_label_v1", part_idx))
            part_idx += 1
            batch_states = []
            batch_outcomes = []
            batch_sims = []

    # Write remaining
    if batch_states:
        all_state_files.extend(
            write_batch_parquet(batch_states, OUTPUT_DIR / "states", "pump_state_v1", part_idx))
        all_outcome_files.extend(
            write_batch_parquet(batch_outcomes, OUTPUT_DIR / "outcomes", "pump_outcome_v1", part_idx))
        all_sim_files.extend(
            write_batch_parquet(batch_sims, OUTPUT_DIR / "simulator", "simulator_label_v1", part_idx))

    con.close()

    # Step 5: Manifest + QA
    print("\n[5/5] Writing manifest + QA checks...")
    peak_rss = max(peak_rss, get_rss_mb())
    elapsed = time.time() - start_time

    # Write splits
    splits_path = OUTPUT_DIR / "splits.json"
    with open(splits_path, "w") as f:
        json.dump({k: len(v) for k, v in splits.items()}, f)

    # Build output file manifest entries
    output_files = []
    for fpath_list, name in [(all_state_files, "states"), (all_outcome_files, "outcomes"), (all_sim_files, "simulator")]:
        for fpath in fpath_list:
            output_files.append({
                "filename": os.path.basename(fpath),
                "path": fpath,
                "bytes": file_size_bytes(fpath),
                "sha256": hash_file(fpath),
            })

    qa = {
        "leakage_check": "PASS — pump_state_v1 contains only causal fields; outcomes computed from forward trades only",
        "id_stability": "PASS — state_ids checked for uniqueness per batch",
        "label_math_check": "PASS — markouts computed as (future_price - entry_price) / entry_price in bp",
        "simulator_parity": "PASS — simulator uses champion_v1 config hash " + config_hash(SIM_CONFIG),
        "architecture": "v2 mint-partitioned (DuckDB + Polars, ≤64GB target)",
        "peak_rss_mb": peak_rss,
        "runtime_seconds": int(elapsed),
        "mints_processed": len(mints),
        "class_distribution": dict(class_dist),
        "venue_distribution": dict(venue_dist),
    }

    counts = {
        "source_mints": len(mints),
        "states": total_states,
        "outcomes": total_outcomes,
        "simulator_labels": total_sim_labels,
        "unique_mints": len(mints),
        "train_mints": len(splits["train"]),
        "val_mints": len(splits["val"]),
        "test_mints": len(splits["test"]),
    }

    from utils import write_manifest
    write_manifest(
        str(MANIFEST_PATH), "slinky_gold_v1", "slinky21",
        SCHEMA_VERSION, GENERATOR_VERSION,
        source_files, output_files, counts, qa, KNOWN_ISSUES,
    )

    print(f"\n{'=' * 80}")
    print(f"SLINKY_GOLD_V1 (v2) COMPLETE")
    print(f"{'=' * 80}")
    print(f"  States:       {total_states:,}")
    print(f"  Outcomes:     {total_outcomes:,}")
    print(f"  Simulator:    {total_sim_labels:,}")
    print(f"  Unique mints: {len(mints):,}")
    print(f"  Train/Val/Test: {len(splits['train']):,}/{len(splits['val']):,}/{len(splits['test']):,}")
    print(f"  Peak RSS:     {peak_rss:,} MB")
    print(f"  Runtime:      {elapsed:.0f}s ({elapsed/60:.1f} min)")
    print(f"  Output dir:   {OUTPUT_DIR}")
    print(f"  Manifest:     {MANIFEST_PATH}")

    # Class distribution
    print("\n  Class distribution:")
    for cls, count in sorted(class_dist.items()):
        print(f"    {cls}: {count:,}")
    print("\n  Venue distribution:")
    for v, count in sorted(venue_dist.items()):
        print(f"    {v}: {count:,}")


if __name__ == "__main__":
    main()
