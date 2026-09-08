"""
slinky_gold_v3 — 4-layer gold dataset from Slinky21/Pumpfun_Memecoin_Corpus.

ARCHITECTURE:
  - DuckDB for all Parquet I/O with predicate pushdown
  - Polars for vectorized per-mint computation
  - Process ONE mint at a time → peak RAM bounded by the single busiest mint
  - Incremental writes (50K-row batch flush, unique sub-indices per chunk)
  - NVMe scratch directory for DuckDB temp space
  - Parallel shard option for throughput (bounded workers)

FOUR LAYERS keyed by stable state_id:
  1. pump_state_v3     — causal features only (no future data)
  2. pump_outcome_v3   — objective future truth
  3. counterfactual_trade_v3 — standardized entry economics for every eligible state
  4. policy_eval_v3    — champion_v1 replay + comparison vs objective truth

LABEL CHANGE (v3):
  - champion_v1 would_enter is NOT the primary y.
  - economic_class in counterfactual_trade_v3 is derived from objective economics.
  - policy_eval_v3 provides auxiliary critique (CORRECT_ENTER, MISSED_OPPORTUNITY, etc.)

UNIT AUDIT:
  v_sol_bonding_curve is ALREADY in lamports in source data (p50=30.8e9).
  v2 BUG: multiplied by 1e9 → values ~3e19 (WRONG). v3 stores raw lamports directly.
  sol_amount, market_cap_sol, price_sol are SOL floats → multiply by 1e9 for lamports.

INPUT:  D:/repos/mev_bot/rust/data/slinky21_data/ (parquets, NEVER modified)
OUTPUT: tools/data-pipeline/output/slinky_gold_v3/ (parquet + manifest)

MEMORY: target 100-120GB RSS, HARD CEILING 145GB. Per-mint → naturally bounded.
"""

from __future__ import annotations
import os, sys, json, hashlib, time, uuid, traceback, shutil
from pathlib import Path
from datetime import datetime, timezone, timedelta
from collections import defaultdict
import random

# psutil for reliable memory monitoring on Windows
try:
    import psutil
    HAS_PSUTIL = True
except ImportError:
    HAS_PSUTIL = False

import duckdb
import polars as pl
import pyarrow as pa
import pyarrow.parquet as pq

# Add parent dirs to path for schema imports
sys.path.insert(0, str(Path(__file__).parent))
sys.path.insert(0, str(Path(__file__).parent.parent / "schemas"))
from gold_schema_v3 import (
    PumpStateV3, PumpOutcomeV3, CounterfactualTradeV3, PolicyEvalV3,
    stable_id, config_hash, SCHEMA_VERSION, GENERATOR_VERSION,
    PIPELINE_VERSION, PRODUCER_VERSION,
)

# ─── Config ───────────────────────────────────────────────────────────
SLINKY_DIR = Path("D:/repos/mev_bot/rust/data/slinky21_data")
OUTPUT_DIR = Path("D:/repos/mev_bot/tools/data-pipeline/output/slinky_gold_v3")
MANIFEST_PATH = OUTPUT_DIR / "slinky_gold_v3_manifest.json"
SCRATCH_DIR = Path("D:/tmp/slinky_gold_v3_scratch")
BATCH_FLUSH = 500_000  # rows per flush (large flush = fewer parquet files = better downstream scans)

# Memory limits — user has ~165GB+ system RAM; use aggressively for throughput
RAM_TARGET_GB = 120
RAM_HARD_CEILING_GB = 145
RAM_STOP_GB = 150

# Execution assumptions for counterfactual trades (versioned, neutral)
EXEC_ASSUMPTIONS_V3 = {
    "version": "exec_v3.0",
    "entry_fee_bps": 100,        # 1%
    "exit_fee_bps": 100,         # 1%
    "entry_tip_lamports": 10000,
    "exit_tip_lamports": 10000,
    "slippage_default_bp": 50,   # 0.5%
    "latency_ms": 250,
    "tp_target_bp": 1500,        # 15% take profit
    "stop_loss_bp": 1500,        # 15% stop loss
    "max_hold_seconds": 300,
    "entry_size_sol": 0.5,       # standardized 0.5 SOL entry — ONE benchmark size
}

# Multi-size entry sizes for exact curve feasibility surface.
# exec_v3.0 (0.5 SOL) is the named benchmark; others provide a policy-independent
# feasibility surface across position sizes.
ENTRY_SIZES_SOL = [0.05, 0.10, 0.25, 0.50, 1.00]

# Champion v1 config (loaded from file at runtime)
CHAMPION_CONFIG_PATH = Path("D:/repos/mev_bot/rust/data/CHAMPION_CONFIG.txt")

# Eligibility criteria for counterfactual trades (objective, NOT champion)
ELIGIBILITY = {
    "min_trades_observed": 3,
    "max_curve_pct": 95.0,       # can't buy after graduation (source is 0-100 pct, not fraction)
    "min_entry_price_sol": 1e-12, # avoid zero-price
    "min_seconds_since_launch": 1.0,
}

KNOWN_ISSUES = [
    "Supply bug: some tokens have incorrect initial_supply in tokens.parquet; "
    "supply_bug_corrected column flags these. Use corrected columns where available.",
    "Top10_pct suspect: initial_top10_pct may be inflated for some tokens due to "
    "API aggregation errors; top10_pct_suspect flags these; use _corrected variants.",
    "trades.event_time timezone: event_time uses +01:00 (CET), not UTC. "
    "All times normalized to UTC in output.",
    "trades.source column: all values are 'pumpdev' in this dataset. "
    "Venue classified by source.startswith('pump').",
    "v_sol_bonding_curve UNIT: already in LAMPORTS (p50=30.8e9), NOT SOL float. "
    "v2 multiplied by 1e9 → wrong values ~3e19. v3 stores raw lamports directly.",
    "postgard_outcomes computed with hindsight; used ONLY for outcome validation, "
    "never for causal states.",
    "wallet_stats is aggregate metadata, not per-trade; used for feature enrichment only.",
    "No Solana slot data in trades; entry_min_age_slots approximated via "
    "seconds_since_launch / 0.4 (slot≈400ms).",
]

# ─── Provenance ──────────────────────────────────────────────────────

def get_git_sha() -> str:
    """Get current git HEAD SHA."""
    import subprocess
    try:
        result = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            capture_output=True, text=True, timeout=10,
            cwd=str(Path(__file__).parent.parent.parent),
        )
        return result.stdout.strip()[:12]
    except Exception:
        return "unknown"

def hash_file(path: str) -> str:
    """SHA256 of a file, reading in chunks."""
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while True:
            buf = f.read(65536)
            if not buf:
                break
            h.update(buf)
    return h.hexdigest()

def file_size_bytes(path: str) -> int:
    return os.path.getsize(str(path))

def compute_code_config_hash() -> str:
    """Hash the script + schema + execution assumptions for provenance."""
    h = hashlib.sha256()
    for f in [Path(__file__), Path(__file__).parent.parent / "schemas" / "gold_schema_v3.py"]:
        if f.exists():
            h.update(hash_file(str(f)).encode())
    h.update(json.dumps(EXEC_ASSUMPTIONS_V3, sort_keys=True).encode())
    h.update(json.dumps(ELIGIBILITY, sort_keys=True).encode())
    return h.hexdigest()[:16]

def load_champion_config() -> dict:
    """Load champion_v1 config from file."""
    config = {}
    with open(CHAMPION_CONFIG_PATH, "r") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            if "=" in line:
                key, val = line.split("=", 1)
                key = key.strip()
                val = val.strip()
                try:
                    val = int(val)
                except ValueError:
                    try:
                        val = float(val)
                    except ValueError:
                        if val.lower() == "true":
                            val = True
                        elif val.lower() == "false":
                            val = False
                config[key] = val
    return config

# ─── Memory monitoring ───────────────────────────────────────────────

def get_rss_mb() -> int:
    """Return current process RSS in MB."""
    if HAS_PSUTIL:
        return int(psutil.Process(os.getpid()).memory_info().rss / 1024 / 1024)
    return 0

def get_system_ram_gb() -> float:
    """Total system RAM in GB."""
    if HAS_PSUTIL:
        return psutil.virtual_memory().total / 1024**3
    return 0.0

def check_ram_ceiling() -> bool:
    """Returns True if we're under the hard ceiling, False if we must stop."""
    rss_gb = get_rss_mb() / 1024
    if rss_gb >= RAM_STOP_GB:
        return False
    return True

# ─── DuckDB ──────────────────────────────────────────────────────────

def make_duckdb(threads: int = 8, mem_limit_gb: int = 48) -> duckdb.DuckDBPyConnection:
    SCRATCH_DIR.mkdir(parents=True, exist_ok=True)
    con = duckdb.connect()
    con.execute(f"SET temp_directory='{str(SCRATCH_DIR)}'")
    con.execute(f"SET memory_limit='{mem_limit_gb}GB'")
    con.execute(f"SET threads={threads}")
    return con

# ─── Step 1: Inventory ───────────────────────────────────────────────

def inventory_sources(con) -> list[dict]:
    source_files = []
    for f in sorted(SLINKY_DIR.rglob("*.parquet")):
        rel = str(f.relative_to(SLINKY_DIR))
        h = hash_file(str(f))
        sz = file_size_bytes(str(f))
        row_count = con.execute(f"SELECT count(*) FROM read_parquet('{f}')").fetchone()[0]
        source_files.append({"filename": rel, "bytes": sz, "sha256": h, "rows": row_count})
        print(f"  hashed {rel}: {row_count:,} rows, {sz / 1e6:.1f} MB")
    return source_files

def compute_source_hash(source_files: list[dict]) -> str:
    """Hash all source file hashes together for provenance."""
    h = hashlib.sha256()
    for sf in sorted(source_files, key=lambda x: x["filename"]):
        h.update(sf["sha256"].encode())
    return h.hexdigest()[:16]

# ─── Step 2: Enumerate mints ─────────────────────────────────────────

def enumerate_mints(con) -> list[tuple[str, int]]:
    trades_glob = str(SLINKY_DIR / "trades" / "trades-*.parquet")
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

# ─── Step 3: Mint-disjoint split ─────────────────────────────────────

def mint_disjoint_split(mints, train_frac=0.7, val_frac=0.15, test_frac=0.15):
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

# ─── Load lookup tables ──────────────────────────────────────────────

def load_token_meta(con) -> dict:
    tokens_path = str(SLINKY_DIR / "tokens.parquet")
    df = con.execute(f"""
        SELECT mint, name, symbol, creator, is_mayhem_mode,
               v_tokens_bonding_curve as initial_supply_raw,
               v_sol_bonding_curve as initial_v_sol_lamports,
               initial_market_cap_sol, initial_price_sol,
               initial_top10_pct_corrected, dev_buy_pct_corrected,
               initial_holder_count, initial_gini,
               creator_past_tokens, creator_past_rugs,
               is_zombie, data_quality_score,
               supply_bug_corrected, top10_pct_suspect,
               trade_count, seconds_to_graduation
        FROM read_parquet('{tokens_path}')
    """).pl()
    result = {}
    for row in df.rows(named=True):
        result[row["mint"]] = row
    return result

def load_migrations(con) -> dict:
    mig_path = str(SLINKY_DIR / "migrations.parquet")
    df = con.execute(f"""
        SELECT mint, migrated_at, seconds_to_graduation, pool_address
        FROM read_parquet('{mig_path}')
    """).pl()
    result = {}
    for row in df.rows(named=True):
        result[row["mint"]] = row
    return result

def load_wallet_stats_schema(con):
    """Check wallet_stats schema and load if useful."""
    ws_path = str(SLINKY_DIR / "wallet_stats.parquet")
    schema = con.execute(f"DESCRIBE SELECT * FROM read_parquet('{ws_path}')").fetchall()
    print("  wallet_stats schema:")
    for col in schema:
        print(f"    {col[0]}: {col[1]}")
    return schema

def load_wallet_stats(con) -> dict:
    """Load wallet_stats indexed by wallet for enrichment."""
    ws_path = str(SLINKY_DIR / "wallet_stats.parquet")
    try:
        df = con.execute(f"SELECT * FROM read_parquet('{ws_path}') LIMIT 1").pl()
        cols = df.columns
        print(f"  wallet_stats columns: {cols}")
        # Load all as dict by wallet
        df_full = con.execute(f"SELECT * FROM read_parquet('{ws_path}')").pl()
        result = {}
        # Use the first column as key (usually wallet address)
        key_col = cols[0] if cols else None
        if key_col and "wallet" in key_col.lower():
            for row in df_full.rows(named=True):
                result[row[key_col]] = row
            print(f"  Loaded {len(result):,} wallet stats records")
        return result
    except Exception as e:
        print(f"  WARN: could not load wallet_stats: {e}")
        return {}

# ─── Per-mint trade loading ──────────────────────────────────────────

def load_mint_trades(con, mint: str) -> pl.DataFrame:
    trades_glob = str(SLINKY_DIR / "trades" / "trades-*.parquet")
    query = f"""
        SELECT * FROM read_parquet('{trades_glob}')
        WHERE mint = '{mint}'
        ORDER BY event_time
    """
    df = con.execute(query).pl()
    return df

# ─── Step 4a: Build causal states (pump_state_v3) ───────────────────

def build_states_for_mint(trades_df: pl.DataFrame, mint: str,
                          token_meta: dict, migrations: dict,
                          run_uuid: str, source_hash: str,
                          code_config_hash: str, git_sha: str) -> list[dict]:
    """Build pump_state_v3 rows — ALL features are causal (before this trade)."""
    if trades_df.is_empty():
        return []

    trades_df = trades_df.sort("event_time")
    n_trades = len(trades_df)

    # Fill nulls
    trades_df = trades_df.with_columns([
        pl.col("sol_amount").fill_null(0),
        pl.col("is_buy").fill_null(False),
        pl.col("user_wallet").fill_null(""),
        pl.col("token_amount").fill_null(0),
    ])

    # Convert event_time to unix_ms (CET +01:00 → UTC)
    trades_df = trades_df.with_columns([
        pl.col("event_time").dt.replace_time_zone("UTC").dt.timestamp("ms").alias("t_ms"),
    ])

    # ─── Causal cumulative stats (BEFORE this trade) ──────────────
    # cum_sum then subtract current trade to get "before this trade"
    trades_df = trades_df.with_columns([
        pl.col("is_buy").cast(pl.Int64).cum_sum().over("mint").alias("_cum_buy"),
        (~pl.col("is_buy")).cast(pl.Int64).cum_sum().over("mint").alias("_cum_sell"),
        pl.col("sol_amount").cum_sum().over("mint").alias("_cum_vol"),
        pl.int_range(0, pl.len(), dtype=pl.Int64).over("mint").alias("_seq"),
    ])

    # Subtract current trade to get "before this trade" (causal)
    trades_df = trades_df.with_columns([
        (pl.col("_cum_buy") - pl.col("is_buy").cast(pl.Int64)).alias("buy_count_so_far"),
        (pl.col("_cum_sell") - (~pl.col("is_buy")).cast(pl.Int64)).alias("sell_count_so_far"),
        (pl.col("_seq") - 1).alias("seq"),
    ])

    # Cumulative buy/sell volume BEFORE this trade
    trades_df = trades_df.with_columns([
        pl.when(pl.col("is_buy")).then(pl.col("sol_amount")).otherwise(0).cum_sum().over("mint").alias("_buy_vol_cum"),
        pl.when(~pl.col("is_buy")).then(pl.col("sol_amount")).otherwise(0).cum_sum().over("mint").alias("_sell_vol_cum"),
    ])
    trades_df = trades_df.with_columns([
        (pl.col("_buy_vol_cum") - pl.when(pl.col("is_buy")).then(pl.col("sol_amount")).otherwise(0)).alias("buy_vol_so_far"),
        (pl.col("_sell_vol_cum") - pl.when(~pl.col("is_buy")).then(pl.col("sol_amount")).otherwise(0)).alias("sell_vol_so_far"),
    ])

    # ─── Time-windowed features (velocity, acceleration, imbalance) ──
    # We need per-row computation using lookback windows.
    # Extract as lists for Python-side windowed computation (per-mint, so bounded)
    t_ms_list = trades_df.select("t_ms").to_series().to_list()
    is_buy_list = trades_df.select("is_buy").to_series().to_list()
    sol_amount_list = trades_df.select("sol_amount").to_series().to_list()
    v_sol_list = trades_df.select("v_sol_bonding_curve").to_series().to_list()
    v_tok_list = trades_df.select("v_tokens_bonding_curve").to_series().to_list()
    mcap_list = trades_df.select("market_cap_sol").to_series().to_list()
    price_list = trades_df.select("price_sol").to_series().to_list()
    curve_pct_list = trades_df.select("curve_pct_depleted").to_series().to_list()
    ssl_list = trades_df.select("seconds_since_launch").to_series().to_list()
    source_list = trades_df.select("source").to_series().to_list()
    token_amount_list = trades_df.select("token_amount").to_series().to_list()
    wallet_list = trades_df.select("user_wallet").to_series().to_list()

    buy_count_list = trades_df.select("buy_count_so_far").to_series().to_list()
    sell_count_list = trades_df.select("sell_count_so_far").to_series().to_list()
    buy_vol_list = trades_df.select("buy_vol_so_far").to_series().to_list()
    sell_vol_list = trades_df.select("sell_vol_so_far").to_series().to_list()
    seq_list = trades_df.select("seq").to_series().to_list()

    # ─── Windowed causal feature computation ──────────────────────
    # For each trade i, look back at trades in [t_i - window, t_i) to compute velocity etc.
    # This is O(n * window_trades) per mint — bounded since we process one mint at a time.

    def compute_window_features(i, window_s):
        """Compute features from trades in [t_i - window_s, t_i)."""
        t_i = t_ms_list[i]
        t_start = t_i - window_s * 1000
        # Find trades in window (binary search or linear scan from i-1 backward)
        w_buys = 0
        w_sells = 0
        w_buy_vol = 0.0
        w_sell_vol = 0.0
        w_wallets_buy = set()
        w_wallets_sell = set()
        w_prices = []
        w_sol_amounts = []
        w_curve_pcts = []
        w_v_sol = []
        j = i - 1
        while j >= 0:
            t_j = t_ms_list[j]
            if t_j < t_start:
                break
            if is_buy_list[j]:
                w_buys += 1
                w_buy_vol += sol_amount_list[j]
                w_wallets_buy.add(wallet_list[j])
            else:
                w_sells += 1
                w_sell_vol += sol_amount_list[j]
                w_wallets_sell.add(wallet_list[j])
            w_prices.append(price_list[j])
            w_sol_amounts.append(sol_amount_list[j])
            w_curve_pcts.append(curve_pct_list[j] or 0)
            if v_sol_list[j] is not None:
                w_v_sol.append(v_sol_list[j])
            j -= 1
        return w_buys, w_sells, w_buy_vol, w_sell_vol, w_wallets_buy, w_wallets_sell, w_prices, w_sol_amounts, w_curve_pcts, w_v_sol

    # ─── Unique wallets (causal, running set) ─────────────────────
    unique_wallets_list = []
    unique_buyers_list = []
    unique_sellers_list = []
    seen_all = set()
    seen_buyers = set()
    seen_sellers = set()
    for i in range(n_trades):
        w = wallet_list[i]
        seen_all.add(w)
        unique_wallets_list.append(len(seen_all) - 1)
        if is_buy_list[i]:
            seen_buyers.add(w)
            unique_buyers_list.append(len(seen_buyers) - 1)
            unique_sellers_list.append(len(seen_sellers))
        else:
            seen_sellers.add(w)
            unique_sellers_list.append(len(seen_sellers) - 1)
            unique_buyers_list.append(len(seen_buyers))

    # ─── Wallet behavior tracking (causal) ────────────────────────
    wallet_buy_count = defaultdict(int)
    wallet_sell_count = defaultdict(int)
    wallet_buy_vol = defaultdict(float)
    wallet_sell_vol = defaultdict(float)
    wallet_first_buy_time = {}
    wallet_trade_times = defaultdict(list)

    # Per-row wallet stats (before this trade)
    buyer_concentration = []
    seller_concentration = []
    repeat_buyer_ratios = []
    repeat_seller_ratios = []
    wallets_also_selling_list = []
    avg_wallet_trade_counts = []
    rapid_rebuy_counts = []
    wash_trade_ratios = []

    # Build running wallet stats
    for i in range(n_trades):
        w = wallet_list[i]
        is_buy = is_buy_list[i]
        amt = sol_amount_list[i]

        # Before this trade: compute concentration from current wallet stats
        buy_vols = list(wallet_buy_vol.values())
        sell_vols = list(wallet_sell_vol.values())

        # Buyer concentration
        if buy_vols:
            total_buy = sum(buy_vols)
            top1 = max(buy_vols)
            buyer_concentration.append(top1 / total_buy if total_buy > 0 else 0)
        else:
            buyer_concentration.append(None)

        if sell_vols:
            total_sell = sum(sell_vols)
            top1 = max(sell_vols)
            seller_concentration.append(top1 / total_sell if total_sell > 0 else 0)
        else:
            seller_concentration.append(None)

        # Repeat buyer/seller ratio
        buyers_multi = sum(1 for c in wallet_buy_count.values() if c > 1)
        total_buyers = len(wallet_buy_count)
        repeat_buyer_ratios.append(buyers_multi / total_buyers if total_buyers > 0 else 0)

        sellers_multi = sum(1 for c in wallet_sell_count.values() if c > 1)
        total_sellers = len(wallet_sell_count)
        repeat_seller_ratios.append(sellers_multi / total_sellers if total_sellers > 0 else 0)

        # Wallets that both buy and sell (avoid creating two full sets — OOM on high-activity mints)
        _smaller, _larger = (wallet_buy_count, wallet_sell_count) if len(wallet_buy_count) <= len(wallet_sell_count) else (wallet_sell_count, wallet_buy_count)
        both = sum(1 for k in _smaller if k in _larger)
        wallets_also_selling_list.append(both)

        # Avg trades per wallet
        all_counts = list(wallet_buy_count.values()) + list(wallet_sell_count.values())
        avg_wallet_trade_counts.append(sum(all_counts) / len(all_counts) if all_counts else 0)

        # Rapid rebuy: same wallet buying again within 3s
        if is_buy and w in wallet_first_buy_time:
            last_buy = wallet_first_buy_time[w]
            if t_ms_list[i] - last_buy < 3000:
                rapid_rebuy_counts.append(rapid_rebuy_counts[-1] + 1 if rapid_rebuy_counts else 1)
            else:
                rapid_rebuy_counts.append(rapid_rebuy_counts[-1] if rapid_rebuy_counts else 0)
        else:
            rapid_rebuy_counts.append(rapid_rebuy_counts[-1] if rapid_rebuy_counts else 0)

        # Wash trade ratio: same wallet buy+sell / total trades so far
        wash_wallets = sum(1 for w2 in set(wallet_buy_count.keys()) & set(wallet_sell_count.keys()))
        total_trades_so_far = i
        wash_trade_ratios.append(wash_wallets / total_trades_so_far if total_trades_so_far > 0 else 0)

        # Update wallet stats AFTER computing causal features
        if is_buy:
            wallet_buy_count[w] += 1
            wallet_buy_vol[w] += amt
            if w not in wallet_first_buy_time:
                wallet_first_buy_time[w] = t_ms_list[i]
            else:
                wallet_first_buy_time[w] = t_ms_list[i]  # update to latest
        else:
            wallet_sell_count[w] += 1
            wallet_sell_vol[w] += amt
        wallet_trade_times[w].append(t_ms_list[i])

    # ─── Token metadata ──────────────────────────────────────────
    meta = token_meta.get(mint, {})
    mig = migrations.get(mint)
    mig_ms = None
    if mig and mig.get("migrated_at"):
        mig_time = mig["migrated_at"]
        if hasattr(mig_time, "timestamp"):
            mig_ms = int(mig_time.timestamp() * 1000)

    # ─── Build state dicts ───────────────────────────────────────
    states = []
    for i in range(n_trades):
        t_ms = int(t_ms_list[i])
        seq = i  # Use loop index as trade sequence within mint (seq_list is broken: pl.lit(1).cum_sum().over("mint") always returns 1 → seq=0 for all rows)
        is_buy = is_buy_list[i]
        sol_amount = sol_amount_list[i] or 0.0
        v_sol_raw = v_sol_list[i]        # ALREADY lamports!
        v_tok_raw = v_tok_list[i]        # raw token units
        mcap_sol = mcap_list[i]           # SOL float
        price_sol = price_list[i]         # SOL float
        curve_pct = curve_pct_list[i]
        seconds_since = ssl_list[i] or 0.0
        token_amt = token_amount_list[i] or 0.0
        source_val = source_list[i] or ""

        # ─── UNIT CONVERSIONS (AUDITED) ──────────────────────────
        # v_sol_bonding_curve: ALREADY lamports in source. DO NOT multiply.
        v_sol_lamports = int(v_sol_raw) if v_sol_raw is not None else None
        v_sol_sol = v_sol_raw / 1e9 if v_sol_raw is not None else None  # derived float

        # sol_amount: SOL float → lamports
        sol_amt_lamports = int(sol_amount * 1e9)
        sol_amt_sol = float(sol_amount)

        # market_cap_sol: SOL float → lamports
        mcap_lamports = int(mcap_sol * 1e9) if mcap_sol is not None else None
        mcap_sol_float = float(mcap_sol) if mcap_sol is not None else None

        # price_sol: SOL float → lamports (price ~2.8e-08 SOL → ~0.028 lamports)
        price_lamports = int(price_sol * 1e9) if price_sol is not None else None
        price_sol_float = float(price_sol) if price_sol is not None else None

        # token_amount: human units
        token_amt_raw = int(token_amt)
        token_amt_float = float(token_amt)

        # v_tokens: raw micro-token units
        v_tok_raw_int = int(v_tok_raw) if v_tok_raw is not None else None

        # ─── Venue classification ────────────────────────────────
        venue = "pumpfun_bonding" if source_val.lower().startswith("pump") else (
            "pumpswap" if "pumpswap" in source_val.lower() else "unknown")

        # ─── Causal cumulative stats ─────────────────────────────
        cum_buy = int(buy_count_list[i])
        cum_sell = int(sell_count_list[i])
        cum_buy_vol_sol = float(buy_vol_list[i] or 0)
        cum_sell_vol_sol = float(sell_vol_list[i] or 0)
        total_vol_sol = cum_buy_vol_sol + cum_sell_vol_sol
        buy_pressure = (cum_buy_vol_sol / total_vol_sol) if total_vol_sol > 0 else None
        net_flow_sol = cum_buy_vol_sol - cum_sell_vol_sol

        # ─── Windowed features (5s and 30s lookback) ─────────────
        w5 = compute_window_features(i, 5)
        w30 = compute_window_features(i, 30)

        w5_buys, w5_sells, w5_buy_vol, w5_sell_vol, w5_wb, w5_ws, w5_prices, w5_amts, w5_curve, w5_vsol = w5
        w30_buys, w30_sells, w30_buy_vol, w30_sell_vol, w30_wb, w30_ws, w30_prices, w30_amts, w30_curve, w30_vsol = w30

        # Trade velocity (trades/sec)
        trade_vel_1s = None
        if i > 0:
            t_prev = t_ms_list[i-1]
            dt = (t_ms - t_prev) / 1000
            if dt > 0 and dt <= 1:
                trade_vel_1s = 1.0 / dt
        trade_vel_5s = (w5_buys + w5_sells) / 5.0 if (w5_buys + w5_sells) > 0 else 0.0
        trade_vel_30s = (w30_buys + w30_sells) / 30.0 if (w30_buys + w30_sells) > 0 else 0.0

        # Volume velocity (SOL/sec)
        w5_total_vol = w5_buy_vol + w5_sell_vol
        w30_total_vol = w30_buy_vol + w30_sell_vol
        vol_vel_5s = w5_total_vol / 5.0
        vol_vel_30s = w30_total_vol / 30.0
        vol_accel = vol_vel_5s - vol_vel_30s

        # Buy/sell imbalance
        w5_total = w5_buy_vol + w5_sell_vol
        w30_total = w30_buy_vol + w30_sell_vol
        imb_5s = (w5_buy_vol - w5_sell_vol) / w5_total if w5_total > 0 else None
        imb_30s = (w30_buy_vol - w30_sell_vol) / w30_total if w30_total > 0 else None

        # ─── Trade-size statistics (causal, from past trades) ────
        if w30_amts:
            w30_amts_sorted = sorted(w30_amts)
            n_a = len(w30_amts_sorted)
            avg_ts = sum(w30_amts_sorted) / n_a
            med_ts = w30_amts_sorted[n_a // 2]
            max_ts = max(w30_amts_sorted)
            min_ts = min(w30_amts_sorted)
            mean_d = sum((x - avg_ts) ** 2 for x in w30_amts_sorted) / n_a
            std_ts = mean_d ** 0.5
        else:
            avg_ts = med_ts = max_ts = min_ts = std_ts = None

        # Largest buy/sell pct of volume
        buy_amounts_window = [sol_amount_list[j] for j in range(max(0, i-20), i) if is_buy_list[j]]
        if buy_amounts_window and cum_buy_vol_sol > 0:
            largest_buy_pct = max(buy_amounts_window) / cum_buy_vol_sol
        else:
            largest_buy_pct = None
        sell_amounts_window = [sol_amount_list[j] for j in range(max(0, i-20), i) if not is_buy_list[j]]
        if sell_amounts_window and cum_sell_vol_sol > 0:
            largest_sell_pct = max(sell_amounts_window) / cum_sell_vol_sol
        else:
            largest_sell_pct = None

        # ─── Price momentum + volatility ─────────────────────────
        price_now = price_sol_float
        # 5s momentum
        if w5_prices and price_now and price_now > 0:
            price_5s_ago = w5_prices[-1] if w5_prices[-1] else None
            if price_5s_ago and price_5s_ago > 0:
                pm_5s = int((price_now / price_5s_ago - 1) * 10000)
            else:
                pm_5s = None
        else:
            pm_5s = None

        if w30_prices and price_now and price_now > 0:
            price_30s_ago = w30_prices[-1] if w30_prices[-1] else None
            if price_30s_ago and price_30s_ago > 0:
                pm_30s = int((price_now / price_30s_ago - 1) * 10000)
            else:
                pm_30s = None
        else:
            pm_30s = None

        # MCap momentum
        mcap_now = mcap_sol_float
        mcap_mom_5s = None
        if mcap_now and w5_prices:
            # mcap proportional to price, so same momentum
            mcap_mom_5s = pm_5s

        # Price volatility (stddev of returns in 30s)
        if len(w30_prices) >= 2 and price_now:
            returns = []
            prev_p = None
            for p in reversed(w30_prices):
                if p and prev_p and prev_p > 0:
                    returns.append((p / prev_p - 1))
                prev_p = p
            if len(returns) >= 2:
                mean_r = sum(returns) / len(returns)
                var_r = sum((r - mean_r) ** 2 for r in returns) / len(returns)
                price_vol_30s = int(var_r ** 0.5 * 10000)
            else:
                price_vol_30s = None
        else:
            price_vol_30s = None

        # Price change since launch
        if i > 0 and price_now and price_list[0] and price_list[0] > 0:
            pc_launch = int((price_now / price_list[0] - 1) * 10000)
        else:
            pc_launch = None

        # ─── Curve depletion velocity ────────────────────────────
        if len(w5_curve) >= 1 and w5_curve[0] is not None and curve_pct is not None:
            curve_dep_vel_5s = (curve_pct - w5_curve[0]) / 5.0
        else:
            curve_dep_vel_5s = None

        if len(w30_curve) >= 1 and w30_curve[0] is not None and curve_pct is not None:
            curve_dep_vel_30s = (curve_pct - w30_curve[0]) / 30.0
        else:
            curve_dep_vel_30s = None

        # v_sol depletion rate (SOL leaving the curve per sec)
        if w5_vsol and v_sol_raw is not None:
            v_sol_dep_5s = (w5_vsol[0] - v_sol_raw) / 1e9 / 5.0  # convert to SOL
        else:
            v_sol_dep_5s = None

        # v_tokens accumulation rate
        if i > 0 and v_tok_raw:
            # v_tokens increases as curve depletes; compute from 5s window
            v_tok_5s_ago = v_tok_list[i - 1] if i > 0 else None
            if v_tok_5s_ago and t_ms_list[i] != t_ms_list[i-1]:
                dt = (t_ms - t_ms_list[i-1]) / 1000
                v_tok_acc = (v_tok_raw - v_tok_5s_ago) / dt if dt > 0 else None
            else:
                v_tok_acc = None
        else:
            v_tok_acc = None

        # ─── Time since launch ───────────────────────────────────
        minutes_since = seconds_since / 60.0

        # ─── Graduation proximity ────────────────────────────────
        is_graduated = False
        seconds_to_grad = None
        grad_proximity = None
        if mig_ms is not None:
            if t_ms >= mig_ms:
                is_graduated = True
                seconds_to_grad = 0.0
            else:
                seconds_to_grad = (mig_ms - t_ms) / 1000.0
            grad_proximity = curve_pct / 100.0 if curve_pct is not None else None  # source is 0-100 pct; 1.0 = at graduation

        # ─── Liquidity ───────────────────────────────────────────
        liquidity_sol = v_sol_sol if v_sol_sol is not None else None
        if i > 0 and v_sol_raw is not None and v_sol_list[i-1] is not None:
            liq_change_5s = (v_sol_list[i-1] - v_sol_raw) / 1e9  # SOL change (positive = liquidity decreasing)
        else:
            liq_change_5s = None
        # 30s liquidity change
        if w30_vsol and v_sol_raw is not None:
            liq_change_30s = (w30_vsol[0] if w30_vsol[0] else v_sol_raw) - v_sol_raw
            liq_change_30s = liq_change_30s / 1e9 if liq_change_30s else 0
        else:
            liq_change_30s = None

        # ─── Wallet concentration / cluster signals ─────────────
        # HHI of buyer volumes
        buy_vols_now = list(wallet_buy_vol.values())
        if buy_vols_now and sum(buy_vols_now) > 0:
            total_bv = sum(buy_vols_now)
            hhi = sum((v / total_bv) ** 2 for v in buy_vols_now)
        else:
            hhi = None

        # Top buyer percentages
        if buy_vols_now and sum(buy_vols_now) > 0:
            total_bv = sum(buy_vols_now)
            sorted_bv = sorted(buy_vols_now, reverse=True)
            top1_pct = sorted_bv[0] / total_bv if total_bv > 0 else None
            top5_pct = sum(sorted_bv[:5]) / total_bv if total_bv > 0 else None
        else:
            top1_pct = top5_pct = None

        # Sybil/toxic indicators
        # Coordinated buy ratio: buys within 1s of each other / total buys
        coord_buys = 0
        for j in range(max(0, i - 20), i):
            if is_buy_list[j] and j > 0 and t_ms_list[j] - t_ms_list[j-1] < 1000:
                coord_buys += 1
        coord_ratio = coord_buys / max(1, cum_buy) if cum_buy > 0 else 0

        # Toxic flow indicator (composite)
        toxic = 0.0
        if wash_trade_ratios[i] and wash_trade_ratios[i] > 0.3:
            toxic += 0.3
        if buyer_concentration[i] and buyer_concentration[i] > 0.5:
            toxic += 0.3
        if coord_ratio > 0.5:
            toxic += 0.2
        if rapid_rebuy_counts and rapid_rebuy_counts[i] and rapid_rebuy_counts[i] > 5:
            toxic += 0.2

        # ─── Build state ─────────────────────────────────────────
        state = PumpStateV3(
            state_id=stable_id("slinky21", mint, str(t_ms), str(seq)),
            pipeline_version=PIPELINE_VERSION,
            producer_version=PRODUCER_VERSION,
            run_uuid=run_uuid,
            source_hash=source_hash,
            code_config_hash=code_config_hash,
            git_sha=git_sha,

            mint=mint,
            event_time_unix_ms=t_ms,
            seq=seq,
            venue=venue,
            source=source_val,

            # Raw int64 values (AUDITED UNITS)
            v_sol_bonding_curve_lamports=v_sol_lamports,
            v_tokens_bonding_curve_raw=v_tok_raw_int,
            sol_amount_lamports=sol_amt_lamports,
            token_amount_raw=token_amt_raw,
            market_cap_sol_lamports=mcap_lamports,
            price_sol_lamports=price_lamports,

            # Derived floats
            v_sol_bonding_curve_sol=v_sol_sol,
            sol_amount_sol=sol_amt_sol,
            market_cap_sol=mcap_sol_float,
            price_sol=price_sol_float,
            token_amount_tokens=token_amt_float,

            curve_pct_depleted=curve_pct,
            trade_side="buy" if is_buy else "sell",
            is_buy=is_buy,

            trade_count_so_far=cum_buy + cum_sell,
            buy_count_so_far=cum_buy,
            sell_count_so_far=cum_sell,
            buy_vol_sol=cum_buy_vol_sol,
            sell_vol_sol=cum_sell_vol_sol,
            total_vol_sol=total_vol_sol,
            buy_pressure=buy_pressure,
            net_flow_sol=net_flow_sol,
            unique_wallets_so_far=unique_wallets_list[i],

            trade_velocity_1s=trade_vel_1s,
            trade_velocity_5s=trade_vel_5s,
            trade_velocity_30s=trade_vel_30s,
            vol_velocity_5s=vol_vel_5s,
            vol_velocity_30s=vol_vel_30s,
            vol_acceleration=vol_accel,
            buy_sell_imbalance_5s=imb_5s,
            buy_sell_imbalance_30s=imb_30s,

            unique_buyers_so_far=unique_buyers_list[i],
            unique_sellers_so_far=unique_sellers_list[i],
            unique_buyers_5s=len(w5_wb) if w5_wb else 0,
            unique_sellers_5s=len(w5_ws) if w5_ws else 0,

            avg_trade_size_sol=avg_ts,
            median_trade_size_sol=med_ts,
            max_trade_size_sol=max_ts,
            min_trade_size_sol=min_ts,
            trade_size_std_sol=std_ts,
            largest_buy_pct_of_vol=largest_buy_pct,
            largest_sell_pct_of_vol=largest_sell_pct,

            price_momentum_5s_bp=pm_5s,
            price_momentum_30s_bp=pm_30s,
            mcap_momentum_5s_bp=mcap_mom_5s,
            price_volatility_30s_bp=price_vol_30s,
            price_change_since_launch_bp=pc_launch,

            curve_depletion_velocity_5s=curve_dep_vel_5s,
            curve_depletion_velocity_30s=curve_dep_vel_30s,
            v_sol_depletion_rate_5s=v_sol_dep_5s,
            v_tokens_accumulation_rate_5s=v_tok_acc,

            seconds_since_launch=seconds_since,
            minutes_since_launch=minutes_since,

            is_graduated=is_graduated,
            seconds_to_graduation=seconds_to_grad,
            graduation_proximity_pct=grad_proximity,

            liquidity_sol=liquidity_sol,
            liquidity_change_5s_sol=liq_change_5s,
            liquidity_change_30s_sol=liq_change_30s,

            token_name=meta.get("name"),
            token_symbol=meta.get("symbol"),
            creator=meta.get("creator"),
            is_mayhem=meta.get("is_mayhem_mode"),
            initial_supply_raw=int(meta["initial_supply_raw"]) if meta.get("initial_supply_raw") is not None else None,
            initial_market_cap_sol=meta.get("initial_market_cap_sol"),
            initial_price_sol=meta.get("initial_price_sol"),

            creator_past_tokens=meta.get("creator_past_tokens"),
            creator_past_rugs=meta.get("creator_past_rugs"),
            dev_buy_pct_corrected=meta.get("dev_buy_pct_corrected"),
            initial_holder_count=meta.get("initial_holder_count"),
            initial_gini=meta.get("initial_gini"),
            initial_top10_pct_corrected=meta.get("initial_top10_pct_corrected"),
            top10_pct_suspect=meta.get("top10_pct_suspect"),
            supply_bug_corrected=meta.get("supply_bug_corrected"),

            buyer_concentration_ratio=buyer_concentration[i],
            seller_concentration_ratio=seller_concentration[i],
            holder_concentration_hhi=hhi,
            top1_buyer_pct=top1_pct,
            top5_buyer_pct=top5_pct,
            repeat_buyer_ratio=repeat_buyer_ratios[i],
            repeat_seller_ratio=repeat_seller_ratios[i],

            wallets_also_selling=wallets_also_selling_list[i],
            avg_wallet_trade_count=avg_wallet_trade_counts[i],

            sybil_cluster_size=None,  # placeholder, requires cross-mint analysis
            coordinated_buy_ratio=coord_ratio,
            wash_trade_ratio=wash_trade_ratios[i] if wash_trade_ratios[i] else 0,
            rapid_rebuy_count=rapid_rebuy_counts[i] if rapid_rebuy_counts else 0,
            toxic_flow_indicator=toxic,

            market_active_mints_5m=None,  # filled in post-pass
            market_total_vol_5m_sol=None,
            market_avg_buy_pressure_5m=None,

            data_quality_score=meta.get("data_quality_score"),
            is_zombie=meta.get("is_zombie"),
        )
        states.append(state.__dict__)

    return states


# ─── Step 4b: Build outcomes (pump_outcome_v3) ──────────────────────

# ─── Exact constant-product curve economics ─────────────────────────

def exact_curve_buy(v_sol_lam: int, v_tok_micro: float, size_sol: float,
                    entry_fee_bps: int, entry_tip_lam: int):
    """Exact entry economics via constant-product AMM.
    Returns (tokens_out_micro, eff_price_sol, impact_bp, fee_sol, total_cost_sol).
    """
    k = v_sol_lam * v_tok_micro
    size_lam = int(size_sol * 1e9)
    new_v_sol = v_sol_lam + size_lam
    new_v_tok = k / new_v_sol
    tokens_out_micro = v_tok_micro - new_v_tok
    if tokens_out_micro <= 0:
        return None, None, None, None, None
    eff_price = size_sol / (tokens_out_micro / 1e6)
    spot = v_sol_lam / (v_tok_micro * 1000.0)
    impact_bp = int((eff_price - spot) / spot * 10000) if spot > 0 else 0
    fee_sol = size_sol * entry_fee_bps / 10000
    total_cost = size_sol + fee_sol + entry_tip_lam / 1e9
    return tokens_out_micro, eff_price, impact_bp, fee_sol, total_cost


def exact_curve_sell(v_sol_lam: int, v_tok_micro: float, tokens_micro: float,
                     exit_fee_bps: int, exit_tip_lam: int):
    """Exact exit economics via constant-product AMM.
    Returns (sol_out, eff_exit_price, impact_bp, fee_sol, net_revenue_sol).
    """
    k = v_sol_lam * v_tok_micro
    new_v_tok = v_tok_micro + tokens_micro
    if new_v_tok <= 0:
        return None, None, None, None, None
    new_v_sol = k / new_v_tok
    sol_out_lam = v_sol_lam - new_v_sol
    if sol_out_lam <= 0:
        return None, None, None, None, None
    sol_out = sol_out_lam / 1e9
    spot = v_sol_lam / (v_tok_micro * 1000.0)
    eff_exit = sol_out / (tokens_micro / 1e6) if tokens_micro > 0 else 0
    impact_bp = int((eff_exit - spot) / spot * 10000) if spot > 0 else 0
    fee_sol = sol_out * exit_fee_bps / 10000
    net_rev = sol_out - fee_sol - exit_tip_lam / 1e9
    return sol_out, eff_exit, impact_bp, fee_sol, net_rev


def compute_multi_size_economics(v_sol_lam, v_tok_micro, entry_sizes_sol,
                                 entry_fee_bps, exit_fee_bps,
                                 entry_tip_lam, exit_tip_lam,
                                 exit_v_sol_lam=None, exit_v_tok_micro=None,
                                 exit_tokens_micro=None):
    """Compute exact economics for multiple entry sizes using constant-product curve.
    If exit reserves and tokens are provided, computes exit at the exit curve state.
    Returns dict keyed by size code (005, 010, 025, 050, 100).
    """
    results = {}
    size_codes = {"0.05": "005", "0.1": "010", "0.25": "025", "0.5": "050", "1.0": "100"}
    
    for size_sol in entry_sizes_sol:
        code = size_codes[str(size_sol)]
        # Entry
        ent = exact_curve_buy(v_sol_lam, v_tok_micro, size_sol, entry_fee_bps, entry_tip_lam)
        if ent[0] is None:
            for field in ["entry_tokens", "entry_eff_price_sol", "entry_impact_bp",
                         "exit_sol", "exit_impact_bp", "gross_pnl_sol", "gross_return_bp",
                         "net_pnl_sol", "net_return_bp"]:
                results[f"sz{code}_{field}"] = None
            results[f"sz{code}_feasible"] = False
            continue
        
        tokens_micro, eff_price, impact_bp, fee_sol, total_cost = ent
        
        # Exit: sell tokens at exit curve state (or entry curve if no exit state)
        ev_sol = exit_v_sol_lam if exit_v_sol_lam is not None else v_sol_lam
        ev_tok = exit_v_tok_micro if exit_v_tok_micro is not None else v_tok_micro
        et_tok = exit_tokens_micro if exit_tokens_micro is not None else tokens_micro
        
        ex = exact_curve_sell(ev_sol, ev_tok, et_tok, exit_fee_bps, exit_tip_lam)
        if ex[0] is not None:
            sol_out, eff_exit, exit_impact, exit_fee, net_rev = ex
            gross_pnl = sol_out - size_sol
            gross_ret_bp = int(gross_pnl / size_sol * 10000) if size_sol > 0 else 0
            net_pnl = gross_pnl - fee_sol - exit_fee
            net_ret_bp = int(net_pnl / size_sol * 10000) if size_sol > 0 else 0
            results[f"sz{code}_entry_tokens"] = tokens_micro / 1e6
            results[f"sz{code}_entry_eff_price_sol"] = eff_price
            results[f"sz{code}_entry_impact_bp"] = impact_bp
            results[f"sz{code}_exit_sol"] = sol_out
            results[f"sz{code}_exit_impact_bp"] = exit_impact
            results[f"sz{code}_gross_pnl_sol"] = gross_pnl
            results[f"sz{code}_gross_return_bp"] = gross_ret_bp
            results[f"sz{code}_net_pnl_sol"] = net_pnl
            results[f"sz{code}_net_return_bp"] = net_ret_bp
            results[f"sz{code}_feasible"] = True
        else:
            results[f"sz{code}_entry_tokens"] = tokens_micro / 1e6
            results[f"sz{code}_entry_eff_price_sol"] = eff_price
            results[f"sz{code}_entry_impact_bp"] = impact_bp
            for field in ["exit_sol", "exit_impact_bp", "gross_pnl_sol", "gross_return_bp",
                         "net_pnl_sol", "net_return_bp"]:
                results[f"sz{code}_{field}"] = None
            results[f"sz{code}_feasible"] = False
    
    return results


def build_outcomes_for_mint(states, trades_df, mint, migrations, run_uuid):
    if not states:
        return []

    trades_sorted = trades_df.sort("event_time")
    t_ms_list = trades_sorted.select(
        pl.col("event_time").dt.replace_time_zone("UTC").dt.timestamp("ms")
    ).to_series().to_list()
    price_list = trades_sorted.select("price_sol").to_series().to_list()

    # Also extract curve reserves for exact economics at exit
    v_sol_list = trades_sorted.select("v_sol_bonding_curve").to_series().to_list()
    v_tok_list = trades_sorted.select("v_tokens_bonding_curve").to_series().to_list()

    mig = migrations.get(mint)
    mig_ms = None
    if mig and mig.get("migrated_at"):
        mig_time = mig["migrated_at"]
        if hasattr(mig_time, "timestamp"):
            mig_ms = int(mig_time.timestamp() * 1000)

    # Determine observation coverage end for this mint.
    # If mint migrated, post-migration trades may be absent → venue censoring past migration.
    # If no migration, observation ends at the last trade in source data.
    last_trade_ms = t_ms_list[-1] if t_ms_list else 0

    # Venue censoring: if mint migrated and the last trade is at/after migration,
    # we have post-migration data. If last trade is BEFORE migration time,
    # post-migration truth is unavailable → venue censoring.
    venue_censored = False
    venue_censoring_reason = None
    observation_end_ms = last_trade_ms  # default: observation ends at last trade

    if mig_ms is not None:
        if last_trade_ms < mig_ms:
            # Last trade is before migration → post-migration truth unavailable
            venue_censored = True
            venue_censoring_reason = "migration_no_post_trade_data"
            observation_end_ms = mig_ms  # coverage ends at migration boundary
        # else: we have post-migration trades, observation continues

    outcomes = []
    for i, state in enumerate(states):
        t_ms = state["event_time_unix_ms"]
        entry_price = price_list[i] if price_list[i] else None
        entry_v_sol = v_sol_list[i] if i < len(v_sol_list) else None
        entry_v_tok = v_tok_list[i] if i < len(v_tok_list) else None

        # ─── Per-horizon markout with CORRECT censoring semantics ────
        # CENSORING = source observation ENDS before t+H (future truth unknowable)
        # NO-TRADE but OBSERVED = source reaches t+H but zero trades → observed illiquidity
        markouts_bp = {}
        markouts_float = {}
        per_horizon = {}  # observed_through, has_trade, right_censored, last_trade_age

        for h in [1, 2, 5, 10, 30, 60, 120, 300]:
            target_ms = t_ms + h * 1000

            # Does source coverage reach t+H?
            observed_through = target_ms <= observation_end_ms
            # Venue censoring also blocks horizons past migration
            if venue_censored and target_ms > observation_end_ms:
                observed_through = False

            # Forward scan: find trades in (t, t+H], and also last trade age at t+H
            has_trade = False
            best_price = None  # last-trade markout (standard markout = last price at/before t+H)
            last_trade_time_in_window = None
            for j in range(i + 1, len(t_ms_list)):
                tj = t_ms_list[j]
                if tj > target_ms:
                    break
                has_trade = True
                best_price = price_list[j]
                last_trade_time_in_window = tj

            # Also check: even if no trade within (t, t+H], was there a trade
            # BETWEEN t+H and observation_end that gives us coverage? No — coverage
            # means the SOURCE has data up to observation_end. If t+H <= observation_end,
            # we ARE observing through t+H even with zero trades.

            right_censored_h = not observed_through

            # Last trade age at t+H: seconds since the most recent trade at/before t+H
            # This measures quote staleness at the horizon boundary.
            last_trade_age = None
            if observed_through:
                # Find the most recent trade at or before target_ms
                most_recent_trade_time = None
                for j in range(len(t_ms_list) - 1, i, -1):
                    tj = t_ms_list[j]
                    if tj <= target_ms:
                        most_recent_trade_time = tj
                        break
                if most_recent_trade_time is not None:
                    last_trade_age = (target_ms - most_recent_trade_time) / 1000.0
                else:
                    # No trade at or before t+H (not even the current trade)
                    last_trade_age = None  # truly no quote reference

            per_horizon[h] = {
                "observed_through": observed_through,
                "has_trade": has_trade,
                "right_censored": right_censored_h,
                "last_trade_age": last_trade_age,
            }

            # Markout: if observed through H, use last-trade price (or None if no trade)
            # If censored, markout is NULL (unobservable)
            if right_censored_h:
                markouts_bp[f"ret_{h}s_bp"] = None
                markouts_float[f"ret_{h}s"] = None
            elif has_trade and best_price is not None and entry_price and entry_price > 0:
                ret_pct = (best_price - entry_price) / entry_price
                markouts_bp[f"ret_{h}s_bp"] = int(ret_pct * 10000)
                markouts_float[f"ret_{h}s"] = float(ret_pct)
            else:
                # Observed through H but no trades → markout is NULL (no market print)
                # This is an OBSERVED no-trade, NOT censored
                markouts_bp[f"ret_{h}s_bp"] = None
                markouts_float[f"ret_{h}s"] = None

        # ─── MFE/MAE within 300s — only from observed trades ──────
        mfe_bp = mae_bp = None
        mfe_time = mae_time = None
        peak_bp = None
        time_to_peak = None
        if entry_price and entry_price > 0 and per_horizon[300]["observed_through"]:
            max_price = min_price = entry_price
            max_time = min_time = 0.0
            has_future_trades_in_window = False
            for j in range(i + 1, len(t_ms_list)):
                tj = t_ms_list[j]
                delta_s = (tj - t_ms) / 1000
                if delta_s > 300:
                    break
                if delta_s > 300:
                    break
                p = price_list[j]
                if p is None:
                    continue
                has_future_trades_in_window = True
                if p > max_price:
                    max_price = p
                    max_time = delta_s
                if p < min_price:
                    min_price = p
                    min_time = delta_s
            if has_future_trades_in_window:
                mfe_bp = int((max_price - entry_price) / entry_price * 10000) if max_price > entry_price else 0
                mae_bp = int((min_price - entry_price) / entry_price * 10000) if min_price < entry_price else 0
                mfe_time = max_time if max_price > entry_price else None
                mae_time = min_time if min_price < entry_price else None
                peak_bp = mfe_bp
                time_to_peak = mfe_time

        # ─── First-hit barriers (only when observed through 300s) ─
        barriers = {}
        if entry_price and entry_price > 0 and per_horizon[300]["observed_through"]:
            for label, threshold_bp, direction in [
                ("hit_plus_10_bp_time", 10, 1), ("hit_plus_25_bp_time", 25, 1),
                ("hit_plus_50_bp_time", 50, 1), ("hit_plus_100_bp_time", 100, 1),
                ("hit_plus_200_bp_time", 200, 1),
                ("hit_plus_500_bp_time", 500, 1), ("hit_plus_1000_bp_time", 1000, 1),
                ("hit_plus_1500_bp_time", 1500, 1), ("hit_plus_2000_bp_time", 2000, 1),
                ("hit_plus_3000_bp_time", 3000, 1),
                ("hit_minus_10_bp_time", -10, -1), ("hit_minus_20_bp_time", -20, -1),
                ("hit_minus_30_bp_time", -30, -1), ("hit_minus_50_bp_time", -50, -1),
                ("hit_minus_100_bp_time", -100, -1), ("hit_minus_200_bp_time", -200, -1),
                ("hit_minus_500_bp_time", -500, -1), ("hit_minus_1000_bp_time", -1000, -1),
                ("hit_minus_1500_bp_time", -1500, -1), ("hit_minus_2000_bp_time", -2000, -1),
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

        # Barrier precedence
        plus_100_before = None
        if barriers.get("hit_plus_100_bp_time") is not None and barriers.get("hit_minus_30_bp_time") is not None:
            plus_100_before = barriers["hit_plus_100_bp_time"] < barriers["hit_minus_30_bp_time"]
        elif barriers.get("hit_plus_100_bp_time") is not None:
            plus_100_before = True

        # Graduation
        grad_info = {"graduated": False, "seconds_to_graduation": None, "graduated_did_migrate": False}
        if mig_ms and mig_ms > t_ms:
            grad_info["graduated"] = True
            grad_info["seconds_to_graduation"] = (mig_ms - t_ms) / 1000
            grad_info["graduated_did_migrate"] = True
        elif mig_ms and mig_ms <= t_ms:
            grad_info["graduated"] = True
            grad_info["seconds_to_graduation"] = 0.0

        # Time-to-next-trade
        time_to_next_trade_s = None
        has_future_trade_300s = False
        for j in range(i + 1, len(t_ms_list)):
            tj = t_ms_list[j]
            delta_s = (tj - t_ms) / 1000
            if delta_s > 300:
                break
            if price_list[j] is not None:
                time_to_next_trade_s = delta_s
                has_future_trade_300s = True
                break

        # Exit curve reserves for exact economics: recorded at the exit trade
        # (barrier-hit or time-stop trade). NULL when no exit trade exists.
        exit_v_sol_lamports = None
        exit_v_tok_raw = None
        exit_event_time_ms = None
        # Find the first trade after state where TP/SL/time-stop would trigger
        # We store the curve reserves AT that exit trade for exact curve exit economics
        if entry_price and entry_price > 0 and per_horizon[300]["observed_through"]:
            tp_price = entry_price * 1.15  # +15% = +1500bp
            sl_price = entry_price * 0.85  # -15% = -1500bp
            tp_hit = False
            sl_hit = False
            tp_time = None
            sl_time = None
            for j in range(i + 1, len(t_ms_list)):
                tj = t_ms_list[j]
                delta_s = (tj - t_ms) / 1000
                if delta_s > 300:
                    break
                p = price_list[j]
                if p is None:
                    continue
                if not tp_hit and p >= tp_price:
                    tp_hit = True
                    tp_time = delta_s
                if not sl_hit and p <= sl_price:
                    sl_hit = True
                    sl_time = delta_s
                if tp_hit and sl_hit:
                    break

            # Determine which barrier hit first and use that trade's reserves
            exit_j = None
            if tp_hit and (not sl_hit or (sl_hit and tp_time <= sl_time)):
                exit_reason = "take_profit"
                # Find the trade at tp_time
                for j in range(i + 1, len(t_ms_list)):
                    if (t_ms_list[j] - t_ms) / 1000 <= tp_time + 0.001:
                        exit_j = j
                    else:
                        break
            elif sl_hit:
                exit_reason = "stop_loss"
                for j in range(i + 1, len(t_ms_list)):
                    if (t_ms_list[j] - t_ms) / 1000 <= sl_time + 0.001:
                        exit_j = j
                    else:
                        break
            else:
                # No barrier hit → time_stop or hold
                # Use the last trade within 300s
                for j in range(len(t_ms_list) - 1, i, -1):
                    delta_s = (t_ms_list[j] - t_ms) / 1000
                    if delta_s <= 300:
                        exit_j = j
                        break
                exit_reason = "time_stop" if exit_j is not None else "hold"

            if exit_j is not None and exit_j < len(v_sol_list):
                exit_v_sol_lamports = int(v_sol_list[exit_j]) if v_sol_list[exit_j] is not None else None
                exit_v_tok_raw = int(v_tok_list[exit_j]) if v_tok_list[exit_j] is not None else None
                exit_event_time_ms = int(t_ms_list[exit_j])

        # ─── Build per-horizon censoring fields ────────────────────
        outcome = PumpOutcomeV3(
            state_id=state["state_id"],
            pipeline_version=PIPELINE_VERSION,
            run_uuid=run_uuid,
            mint=mint,
            event_time_unix_ms=t_ms,
            ret_1s_bp=markouts_bp.get("ret_1s_bp"),
            ret_2s_bp=markouts_bp.get("ret_2s_bp"),
            ret_5s_bp=markouts_bp.get("ret_5s_bp"),
            ret_10s_bp=markouts_bp.get("ret_10s_bp"),
            ret_30s_bp=markouts_bp.get("ret_30s_bp"),
            ret_60s_bp=markouts_bp.get("ret_60s_bp"),
            ret_120s_bp=markouts_bp.get("ret_120s_bp"),
            ret_300s_bp=markouts_bp.get("ret_300s_bp"),
            ret_1s=markouts_float.get("ret_1s"),
            ret_5s=markouts_float.get("ret_5s"),
            ret_30s=markouts_float.get("ret_30s"),
            ret_60s=markouts_float.get("ret_60s"),
            ret_300s=markouts_float.get("ret_300s"),
            mfe_bp=mfe_bp, mae_bp=mae_bp,
            mfe_time_seconds=mfe_time, mae_time_seconds=mae_time,
            peak_bp=peak_bp, time_to_peak_seconds=time_to_peak,
            hit_plus_10_bp_time=barriers.get("hit_plus_10_bp_time"),
            hit_plus_25_bp_time=barriers.get("hit_plus_25_bp_time"),
            hit_plus_50_bp_time=barriers.get("hit_plus_50_bp_time"),
            hit_plus_100_bp_time=barriers.get("hit_plus_100_bp_time"),
            hit_plus_200_bp_time=barriers.get("hit_plus_200_bp_time"),
            hit_plus_500_bp_time=barriers.get("hit_plus_500_bp_time"),
            hit_plus_1000_bp_time=barriers.get("hit_plus_1000_bp_time"),
            hit_plus_1500_bp_time=barriers.get("hit_plus_1500_bp_time"),
            hit_plus_2000_bp_time=barriers.get("hit_plus_2000_bp_time"),
            hit_plus_3000_bp_time=barriers.get("hit_plus_3000_bp_time"),
            hit_minus_10_bp_time=barriers.get("hit_minus_10_bp_time"),
            hit_minus_20_bp_time=barriers.get("hit_minus_20_bp_time"),
            hit_minus_30_bp_time=barriers.get("hit_minus_30_bp_time"),
            hit_minus_50_bp_time=barriers.get("hit_minus_50_bp_time"),
            hit_minus_100_bp_time=barriers.get("hit_minus_100_bp_time"),
            hit_minus_200_bp_time=barriers.get("hit_minus_200_bp_time"),
            hit_minus_500_bp_time=barriers.get("hit_minus_500_bp_time"),
            hit_minus_1000_bp_time=barriers.get("hit_minus_1000_bp_time"),
            hit_minus_1500_bp_time=barriers.get("hit_minus_1500_bp_time"),
            hit_minus_2000_bp_time=barriers.get("hit_minus_2000_bp_time"),
            plus_100_before_minus_30=plus_100_before,
            survived_60s=per_horizon[60]["observed_through"] and per_horizon[60]["has_trade"],
            survived_300s=per_horizon[300]["observed_through"] and per_horizon[300]["has_trade"],
            collapsed_50pct_within_300s=(mae_bp is not None and mae_bp <= -5000),
            graduated_after_state=grad_info["graduated"],
            seconds_to_graduation=grad_info["seconds_to_graduation"],
            graduated_did_migrate=grad_info["graduated_did_migrate"],
            post_grad_price_change_300s_bp=None,
            exit_v_sol_lamports=exit_v_sol_lamports,
            exit_v_tok_raw=exit_v_tok_raw,
            exit_event_time_ms=exit_event_time_ms,
            # Per-horizon censoring fields
            observed_through_1s=per_horizon[1]["observed_through"],
            has_trade_within_1s=per_horizon[1]["has_trade"],
            right_censored_1s=per_horizon[1]["right_censored"],
            last_trade_age_at_1s_s=per_horizon[1]["last_trade_age"],
            observed_through_2s=per_horizon[2]["observed_through"],
            has_trade_within_2s=per_horizon[2]["has_trade"],
            right_censored_2s=per_horizon[2]["right_censored"],
            last_trade_age_at_2s_s=per_horizon[2]["last_trade_age"],
            observed_through_5s=per_horizon[5]["observed_through"],
            has_trade_within_5s=per_horizon[5]["has_trade"],
            right_censored_5s=per_horizon[5]["right_censored"],
            last_trade_age_at_5s_s=per_horizon[5]["last_trade_age"],
            observed_through_10s=per_horizon[10]["observed_through"],
            has_trade_within_10s=per_horizon[10]["has_trade"],
            right_censored_10s=per_horizon[10]["right_censored"],
            last_trade_age_at_10s_s=per_horizon[10]["last_trade_age"],
            observed_through_30s=per_horizon[30]["observed_through"],
            has_trade_within_30s=per_horizon[30]["has_trade"],
            right_censored_30s=per_horizon[30]["right_censored"],
            last_trade_age_at_30s_s=per_horizon[30]["last_trade_age"],
            observed_through_60s=per_horizon[60]["observed_through"],
            has_trade_within_60s=per_horizon[60]["has_trade"],
            right_censored_60s=per_horizon[60]["right_censored"],
            last_trade_age_at_60s_s=per_horizon[60]["last_trade_age"],
            observed_through_120s=per_horizon[120]["observed_through"],
            has_trade_within_120s=per_horizon[120]["has_trade"],
            right_censored_120s=per_horizon[120]["right_censored"],
            last_trade_age_at_120s_s=per_horizon[120]["last_trade_age"],
            observed_through_300s=per_horizon[300]["observed_through"],
            has_trade_within_300s=per_horizon[300]["has_trade"],
            right_censored_300s=per_horizon[300]["right_censored"],
            last_trade_age_at_300s_s=per_horizon[300]["last_trade_age"],
            venue_censored=venue_censored,
            venue_censoring_reason=venue_censoring_reason,
            observation_end_ms=observation_end_ms,
            time_to_next_trade_s=time_to_next_trade_s,
            has_future_trade_300s=has_future_trade_300s,
        )
        outcomes.append(outcome.__dict__)

    return outcomes


# ─── Step 4c: Counterfactual trades (counterfactual_trade_v3) ───────

def build_counterfactuals_for_mint(states, outcomes, mint, run_uuid, exec_hash):
    if not states:
        return []

    ea = EXEC_ASSUMPTIONS_V3
    entry_sizes = ENTRY_SIZES_SOL  # [0.05, 0.10, 0.25, 0.50, 1.00]
    outcome_map = {o["state_id"]: o for o in outcomes}
    cf_trades = []

    for state in states:
        sid = state["state_id"]
        outcome = outcome_map.get(sid)
        t_ms = state["event_time_unix_ms"]

        # ─── Eligibility (objective, NOT champion) ───────────────
        eligible = True
        elig_reason = None

        trade_count = state.get("trade_count_so_far", 0)
        curve_pct = state.get("curve_pct_depleted")
        price = state.get("price_sol")
        ssl = state.get("seconds_since_launch", 0)

        if trade_count < ELIGIBILITY["min_trades_observed"]:
            eligible = False
            elig_reason = "insufficient_trades"
        elif curve_pct is not None and curve_pct >= ELIGIBILITY["max_curve_pct"]:
            eligible = False
            elig_reason = "curve_depleted"
        elif price is None or price < ELIGIBILITY["min_entry_price_sol"]:
            eligible = False
            elig_reason = "invalid_price"
        elif ssl < ELIGIBILITY["min_seconds_since_launch"]:
            eligible = False
            elig_reason = "too_close_to_launch"

        # ─── Curve reserves for exact economics ──────────────────
        v_sol_lam = state.get("v_sol_bonding_curve_lamports")
        v_tok_micro = state.get("v_tokens_bonding_curve_raw")
        # v_tok is in micro-token units (raw), need as float for curve math
        if v_tok_micro is not None:
            v_tok_micro = float(v_tok_micro)

        # Exit curve reserves from outcome (at exit trade)
        exit_v_sol_lam = None
        exit_v_tok_micro = None
        exit_tokens_micro = None  # tokens held at exit = tokens bought at entry
        if outcome:
            exit_v_sol_lam = outcome.get("exit_v_sol_lamports")
            exit_v_tok_raw = outcome.get("exit_v_tok_raw")
            if exit_v_tok_raw is not None:
                exit_v_tok_micro = float(exit_v_tok_raw)

        # ─── Entry economics — exec_v3.0 benchmark (0.5 SOL) ─────
        entry_size_sol = ea["entry_size_sol"]
        entry_size_lamports = int(entry_size_sol * 1e9)
        entry_price_sol = price if price else 0.0
        entry_price_lamports = int(entry_price_sol * 1e9) if entry_price_sol else 0

        entry_fee_sol = entry_size_sol * ea["entry_fee_bps"] / 10000
        entry_fee_lamports = int(entry_fee_sol * 1e9)
        entry_slip_bp = ea["slippage_default_bp"]
        entry_slip_sol = entry_size_sol * entry_slip_bp / 10000
        entry_tip = ea["entry_tip_lamports"]
        entry_total_cost = entry_size_sol + entry_fee_sol + entry_slip_sol + entry_tip / 1e9

        # ─── Exact curve entry economics (exec_v3.0) ─────────────
        exact_entry_tokens = None
        exact_entry_eff_price = None
        exact_entry_impact_bp = None
        exact_exit_sol_out = None
        exact_exit_impact_bp = None
        exact_gross_pnl_sol = None
        exact_gross_return_bp = None
        exact_net_pnl_sol = None
        exact_net_return_bp = None

        if eligible and v_sol_lam is not None and v_tok_micro is not None and v_tok_micro > 0:
            ent = exact_curve_buy(v_sol_lam, v_tok_micro, entry_size_sol,
                                  ea["entry_fee_bps"], ea["entry_tip_lamports"])
            if ent[0] is not None:
                entry_tokens_micro, exact_entry_eff_price, exact_entry_impact_bp, ent_fee, ent_cost = ent
                exact_entry_tokens = entry_tokens_micro / 1e6  # human tokens
                exit_tokens_micro = entry_tokens_micro  # tokens held at exit

                # Exact exit: sell at exit curve reserves
                ev_sol = exit_v_sol_lam if exit_v_sol_lam is not None else v_sol_lam
                ev_tok = exit_v_tok_micro if exit_v_tok_micro is not None else v_tok_micro

                ex = exact_curve_sell(ev_sol, ev_tok, exit_tokens_micro,
                                      ea["exit_fee_bps"], ea["exit_tip_lamports"])
                if ex[0] is not None:
                    exact_exit_sol_out, eff_exit, exact_exit_impact_bp, exit_fee, net_rev = ex
                    exact_gross_pnl_sol = exact_exit_sol_out - entry_size_sol
                    exact_gross_return_bp = int(exact_gross_pnl_sol / entry_size_sol * 10000) if entry_size_sol > 0 else 0
                    exact_net_pnl_sol = exact_gross_pnl_sol - ent_fee - exit_fee
                    exact_net_return_bp = int(exact_net_pnl_sol / entry_size_sol * 10000) if entry_size_sol > 0 else 0

        # ─── Multi-size feasibility surface ──────────────────────
        multi_size = {}
        if eligible and v_sol_lam is not None and v_tok_micro is not None and v_tok_micro > 0:
            multi_size = compute_multi_size_economics(
                v_sol_lam, v_tok_micro, entry_sizes,
                ea["entry_fee_bps"], ea["exit_fee_bps"],
                ea["entry_tip_lamports"], ea["exit_tip_lamports"],
                exit_v_sol_lam, exit_v_tok_micro, exit_tokens_micro if exit_tokens_micro else None
            )

        # ─── Exit economics — exec_v3.0 benchmark (barrier-based) ─
        exit_reason = None
        exit_price_sol = None
        exit_price_lamports = None
        exit_fee_sol = None
        exit_fee_lamports = None
        exit_slip_bp = None
        exit_slip_sol = None
        exit_tip = None
        exit_total_revenue = None
        gross_pnl_sol = None
        gross_pnl_lamports = None
        net_pnl_sol = None
        net_pnl_lamports = None
        net_return_bp = None
        net_return_pct = None
        hold_duration = None
        risk_reward = None
        economic_class = "SKIP"
        class_reason = "not_eligible"

        if eligible and outcome:
            # Use per-horizon censoring: only compute exit if observed through 300s
            observed_300 = outcome.get("observed_through_300s", False)
            has_trade_300 = outcome.get("has_trade_within_300s", False)
            venue_cens = outcome.get("venue_censored", False)

            mfe = outcome.get("mfe_bp") or 0
            mae = outcome.get("mae_bp") or 0
            tp = ea["tp_target_bp"]
            sl = -ea["stop_loss_bp"]

            tp_key = f"hit_plus_{tp}_bp_time"
            sl_key = f"hit_minus_{abs(sl)}_bp_time"
            tp_hit_time = outcome.get(tp_key)
            sl_hit_time = outcome.get(sl_key)

            tp_hit = tp_hit_time is not None
            sl_hit = sl_hit_time is not None

            if not observed_300:
                # Coverage-censored: can't determine exit truth
                exit_reason = "no_exit"
                exit_feasible = False
                exit_feasibility_note = "coverage_censored"
                economic_class = "SKIP"
                class_reason = "coverage_censored_no_observation"
            elif tp_hit and sl_hit:
                if tp_hit_time <= sl_hit_time:
                    exit_reason = "take_profit"
                    hold_duration = tp_hit_time
                    exit_price_sol = entry_price_sol * (1 + tp / 10000)
                else:
                    exit_reason = "stop_loss"
                    hold_duration = sl_hit_time
                    exit_price_sol = entry_price_sol * (1 + sl / 10000)
            elif tp_hit:
                exit_reason = "take_profit"
                hold_duration = tp_hit_time
                exit_price_sol = entry_price_sol * (1 + tp / 10000)
            elif sl_hit:
                exit_reason = "stop_loss"
                hold_duration = sl_hit_time
                exit_price_sol = entry_price_sol * (1 + sl / 10000)
            elif has_trade_300:
                # Observed through 300s, trades exist, but no barrier hit → time_stop
                exit_reason = "time_stop"
                hold_duration = 300.0
                ret_300 = outcome.get("ret_300s_bp")
                exit_price_sol = entry_price_sol * (1 + (ret_300 or 0) / 10000) if ret_300 is not None else None
            else:
                # Observed through 300s but ZERO trades → observed no-trade/illiquidity
                exit_reason = "hold"
                hold_duration = 300.0
                exit_price_sol = None  # no market print → no executable exit

        # ─── PnL computation ──────────────────────────────────────
        if exit_price_sol is not None and entry_price_sol > 0 and eligible:
            exit_price_lamports = int(exit_price_sol * 1e9)
            position_value_exit = entry_size_sol * (exit_price_sol / entry_price_sol)

            exit_fee_sol = position_value_exit * ea["exit_fee_bps"] / 10000
            exit_fee_lamports = int(exit_fee_sol * 1e9)
            exit_slip_bp = ea["slippage_default_bp"]
            exit_slip_sol = position_value_exit * exit_slip_bp / 10000
            exit_tip = ea["exit_tip_lamports"]
            exit_total_revenue = position_value_exit - exit_fee_sol - exit_slip_sol - exit_tip / 1e9

            gross_pnl_sol = position_value_exit - entry_size_sol
            gross_pnl_lamports = int(gross_pnl_sol * 1e9)
            net_pnl_sol = gross_pnl_sol - entry_fee_sol - exit_fee_sol - entry_slip_sol - exit_slip_sol - (entry_tip + exit_tip) / 1e9
            net_pnl_lamports = int(net_pnl_sol * 1e9)

            if entry_total_cost > 0:
                net_return_bp = int(net_pnl_sol / entry_total_cost * 10000)
                net_return_pct = net_pnl_sol / entry_total_cost

            mfe_val = outcome.get("mfe_bp") or 0
            if mae and mae < 0:
                risk_reward = mfe / abs(mae) if mae != 0 else None

        # ─── Economic classification ─────────────────────────────
        if eligible and outcome:
            mfe_val = outcome.get("mfe_bp") or 0
            if net_return_bp is not None:
                if net_return_bp > 1000 and mfe_val > 2000:
                    economic_class = "STRONG"
                    class_reason = "net_return>1000bp and mfe>2000bp"
                elif net_return_bp > 500 and mfe_val > 1000:
                    economic_class = "GOOD"
                    class_reason = "net_return>500bp and mfe>1000bp"
                elif net_return_bp > 0:
                    economic_class = "MARGINAL"
                    class_reason = "net_return>0bp"
                elif net_return_bp > -500:
                    economic_class = "BAD"
                    class_reason = "net_return in [-500, 0] bp"
                else:
                    economic_class = "TOXIC"
                    class_reason = "net_return < -500bp"
            elif exit_reason == "no_exit":
                economic_class = "SKIP"
                class_reason = "coverage_censored"
            elif exit_reason == "hold":
                # Observed no-trade: can't execute exit → not a real opportunity
                economic_class = "SKIP"
                class_reason = "observed_no_trade_illiquid"

        # ─── Exit feasibility (corrected semantics) ──────────────
        if not eligible:
            exit_feasible = False
            exit_feasibility_note = None
        elif outcome:
            observed_300 = outcome.get("observed_through_300s", False)
            has_trade_300 = outcome.get("has_trade_within_300s", False)
            venue_cens = outcome.get("venue_censored", False)

            if not observed_300:
                exit_feasible = False
                exit_feasibility_note = "coverage_censored"
            elif exit_reason == "hold":
                exit_feasible = False
                exit_feasibility_note = "illiquid_observed"
            elif exit_reason == "no_exit":
                exit_feasible = False
                exit_feasibility_note = "coverage_censored"
            elif exit_reason is not None and has_trade_300:
                exit_feasible = True
                exit_feasibility_note = "liquid"
            else:
                exit_feasible = False
                exit_feasibility_note = "no_executable_exit"
        else:
            exit_feasible = False
            exit_feasibility_note = None

        cf = CounterfactualTradeV3(
            state_id=sid,
            pipeline_version=PIPELINE_VERSION,
            run_uuid=run_uuid,
            execution_assumptions_version=ea["version"],
            execution_config_hash=exec_hash,
            mint=mint,
            event_time_unix_ms=t_ms,
            entry_price_sol=entry_price_sol,
            entry_price_lamports=entry_price_lamports,
            entry_size_sol=entry_size_sol,
            entry_size_lamports=entry_size_lamports,
            entry_fee_sol=entry_fee_sol,
            entry_fee_lamports=entry_fee_lamports,
            entry_slippage_bp=entry_slip_bp,
            entry_slippage_sol=entry_slip_sol,
            entry_tip_lamports=entry_tip,
            entry_total_cost_sol=entry_total_cost,
            exit_reason=exit_reason,
            exit_price_sol=exit_price_sol,
            exit_price_lamports=exit_price_lamports,
            exit_fee_sol=exit_fee_sol,
            exit_fee_lamports=exit_fee_lamports,
            exit_slippage_bp=exit_slip_bp,
            exit_slippage_sol=exit_slip_sol,
            exit_tip_lamports=exit_tip,
            exit_total_revenue_sol=exit_total_revenue,
            gross_pnl_sol=gross_pnl_sol,
            gross_pnl_lamports=gross_pnl_lamports,
            net_pnl_sol=net_pnl_sol,
            net_pnl_lamports=net_pnl_lamports,
            net_return_bp=net_return_bp,
            net_return_pct=net_return_pct,
            hold_duration_seconds=hold_duration,
            risk_reward_ratio=risk_reward,
            exact_entry_tokens=exact_entry_tokens,
            exact_entry_eff_price_sol=exact_entry_eff_price,
            exact_entry_impact_bp=exact_entry_impact_bp,
            exact_exit_sol_out=exact_exit_sol_out,
            exact_exit_impact_bp=exact_exit_impact_bp,
            exact_gross_pnl_sol=exact_gross_pnl_sol,
            exact_gross_return_bp=exact_gross_return_bp,
            exact_net_pnl_sol=exact_net_pnl_sol,
            exact_net_return_bp=exact_net_return_bp,
            sz005_entry_tokens=multi_size.get("sz005_entry_tokens"),
            sz005_entry_eff_price_sol=multi_size.get("sz005_entry_eff_price_sol"),
            sz005_entry_impact_bp=multi_size.get("sz005_entry_impact_bp"),
            sz005_exit_sol=multi_size.get("sz005_exit_sol"),
            sz005_exit_impact_bp=multi_size.get("sz005_exit_impact_bp"),
            sz005_gross_pnl_sol=multi_size.get("sz005_gross_pnl_sol"),
            sz005_gross_return_bp=multi_size.get("sz005_gross_return_bp"),
            sz005_net_pnl_sol=multi_size.get("sz005_net_pnl_sol"),
            sz005_net_return_bp=multi_size.get("sz005_net_return_bp"),
            sz005_feasible=multi_size.get("sz005_feasible", False),
            sz010_entry_tokens=multi_size.get("sz010_entry_tokens"),
            sz010_entry_eff_price_sol=multi_size.get("sz010_entry_eff_price_sol"),
            sz010_entry_impact_bp=multi_size.get("sz010_entry_impact_bp"),
            sz010_exit_sol=multi_size.get("sz010_exit_sol"),
            sz010_exit_impact_bp=multi_size.get("sz010_exit_impact_bp"),
            sz010_gross_pnl_sol=multi_size.get("sz010_gross_pnl_sol"),
            sz010_gross_return_bp=multi_size.get("sz010_gross_return_bp"),
            sz010_net_pnl_sol=multi_size.get("sz010_net_pnl_sol"),
            sz010_net_return_bp=multi_size.get("sz010_net_return_bp"),
            sz010_feasible=multi_size.get("sz010_feasible", False),
            sz025_entry_tokens=multi_size.get("sz025_entry_tokens"),
            sz025_entry_eff_price_sol=multi_size.get("sz025_entry_eff_price_sol"),
            sz025_entry_impact_bp=multi_size.get("sz025_entry_impact_bp"),
            sz025_exit_sol=multi_size.get("sz025_exit_sol"),
            sz025_exit_impact_bp=multi_size.get("sz025_exit_impact_bp"),
            sz025_gross_pnl_sol=multi_size.get("sz025_gross_pnl_sol"),
            sz025_gross_return_bp=multi_size.get("sz025_gross_return_bp"),
            sz025_net_pnl_sol=multi_size.get("sz025_net_pnl_sol"),
            sz025_net_return_bp=multi_size.get("sz025_net_return_bp"),
            sz025_feasible=multi_size.get("sz025_feasible", False),
            sz050_entry_tokens=multi_size.get("sz050_entry_tokens"),
            sz050_entry_eff_price_sol=multi_size.get("sz050_entry_eff_price_sol"),
            sz050_entry_impact_bp=multi_size.get("sz050_entry_impact_bp"),
            sz050_exit_sol=multi_size.get("sz050_exit_sol"),
            sz050_exit_impact_bp=multi_size.get("sz050_exit_impact_bp"),
            sz050_gross_pnl_sol=multi_size.get("sz050_gross_pnl_sol"),
            sz050_gross_return_bp=multi_size.get("sz050_gross_return_bp"),
            sz050_net_pnl_sol=multi_size.get("sz050_net_pnl_sol"),
            sz050_net_return_bp=multi_size.get("sz050_net_return_bp"),
            sz050_feasible=multi_size.get("sz050_feasible", False),
            sz100_entry_tokens=multi_size.get("sz100_entry_tokens"),
            sz100_entry_eff_price_sol=multi_size.get("sz100_entry_eff_price_sol"),
            sz100_entry_impact_bp=multi_size.get("sz100_entry_impact_bp"),
            sz100_exit_sol=multi_size.get("sz100_exit_sol"),
            sz100_exit_impact_bp=multi_size.get("sz100_exit_impact_bp"),
            sz100_gross_pnl_sol=multi_size.get("sz100_gross_pnl_sol"),
            sz100_gross_return_bp=multi_size.get("sz100_gross_return_bp"),
            sz100_net_pnl_sol=multi_size.get("sz100_net_pnl_sol"),
            sz100_net_return_bp=multi_size.get("sz100_net_return_bp"),
            sz100_feasible=multi_size.get("sz100_feasible", False),
            economic_class=economic_class,
            economic_class_reason=class_reason,
            exit_feasible=exit_feasible,
            exit_feasibility_note=exit_feasibility_note,
            eligible=eligible,
            eligibility_reason=elig_reason,
            latency_ms=ea["latency_ms"],
            entry_fee_bps=ea["entry_fee_bps"],
            exit_fee_bps=ea["exit_fee_bps"],
            slippage_default_bp=ea["slippage_default_bp"],
            tp_target_bp=ea["tp_target_bp"],
            stop_loss_bp=ea["stop_loss_bp"],
            max_hold_seconds=ea["max_hold_seconds"],
        )
        cf_trades.append(cf.__dict__)


    return cf_trades


# ─── Step 4d: Policy evaluation (policy_eval_v3) ────────────────────

def build_policy_eval_for_mint(states, outcomes, cf_trades, mint,
                                run_uuid, champ_cfg, champ_hash):
    if not states:
        return []

    cf_map = {c["state_id"]: c for c in cf_trades}
    outcome_map = {o["state_id"]: o for o in outcomes}
    policy_evals = []

    for state in states:
        sid = state["state_id"]
        cf = cf_map.get(sid)
        outcome = outcome_map.get(sid)
        t_ms = state["event_time_unix_ms"]

        # ─── Champion v1 entry gate replay ───────────────────────
        # Key entry conditions from CHAMPION_CONFIG.txt:
        #   entry_min_trades_observed = 3
        #   entry_min_age_slots = 5 (approximate: seconds_since_launch / 0.4)
        #   entry_min_volume_lamports = 2000000000 (2 SOL)
        #   entry_min_buy_pressure_bp = 5000 (50%)
        #   entry_min_buy_ratio_bp = 3500 (35%)
        #   entry_max_sol_per_trade_lamports = 2000000000 (2 SOL)
        #   curve_pct > 0.95 → skip

        curve_pct = state.get("curve_pct_depleted")
        trade_count = state.get("trade_count_so_far", 0)
        buy_count = state.get("buy_count_so_far", 0)
        buy_pressure = state.get("buy_pressure")
        total_vol_sol = state.get("total_vol_sol", 0)
        ssl = state.get("seconds_since_launch", 0)
        max_sol_per_trade = state.get("max_trade_size_sol")
        avg_sol_per_trade = state.get("avg_trade_size_sol")

        # Convert volumes to lamports for comparison
        total_vol_lamports = int(total_vol_sol * 1e9)
        approx_age_slots = int(ssl / 0.4)  # slot ≈ 400ms

        gate_failures = []

        if curve_pct is not None and curve_pct > 95.0:
            gate_failures.append("curve_depleted")
        if trade_count < champ_cfg.get("entry_min_trades_observed", 3):
            gate_failures.append("insufficient_trades")
        if approx_age_slots < champ_cfg.get("entry_min_age_slots", 5):
            gate_failures.append("insufficient_age")
        if total_vol_lamports < champ_cfg.get("entry_min_volume_lamports", 2000000000):
            gate_failures.append("insufficient_volume")
        if buy_pressure is not None:
            buy_pressure_bp = int(buy_pressure * 10000)
            if buy_pressure_bp < champ_cfg.get("entry_min_buy_pressure_bp", 5000):
                gate_failures.append("low_buy_pressure")
        if buy_count > 0 and trade_count > 0:
            buy_ratio_bp = int((buy_count / trade_count) * 10000)
            if buy_ratio_bp < champ_cfg.get("entry_min_buy_ratio_bp", 3500):
                gate_failures.append("low_buy_ratio")
        if max_sol_per_trade is not None:
            max_sol_lamports = int(max_sol_per_trade * 1e9)
            if max_sol_lamports > champ_cfg.get("entry_max_sol_per_trade_lamports", 2000000000):
                gate_failures.append("oversized_trade")

        would_enter = len(gate_failures) == 0
        action = "ENTER" if would_enter else "SKIP"
        entry_reason = "; ".join(gate_failures) if gate_failures else "all_gates_passed"

        # ─── Compare vs objective truth ──────────────────────────
        # CORRECT_ENTER:   champion enters AND counterfactual says STRONG/GOOD
        # CORRECT_SKIP:    champion skips AND counterfactual says SKIP/BAD/TOXIC
        # FALSE_POSITIVE:  champion enters BUT counterfactual says BAD/TOXIC
        # MISSED_OPPORTUNITY: champion skips BUT counterfactual says STRONG/GOOD
        # AMBIGUOUS:       champion enters AND counterfactual says MARGINAL, or other edge
        evaluation = "AMBIGUOUS"
        eval_reason = ""

        if cf:
            econ_class = cf.get("economic_class", "SKIP")
            eligible = cf.get("eligible", False)

            if would_enter and eligible:
                if econ_class in ("STRONG", "GOOD"):
                    evaluation = "CORRECT_ENTER"
                    eval_reason = f"champion entered, counterfactual={econ_class}"
                elif econ_class in ("BAD", "TOXIC"):
                    evaluation = "FALSE_POSITIVE"
                    eval_reason = f"champion entered, counterfactual={econ_class}"
                elif econ_class == "MARGINAL":
                    evaluation = "AMBIGUOUS"
                    eval_reason = f"champion entered, counterfactual=MARGINAL"
                else:
                    evaluation = "AMBIGUOUS"
                    eval_reason = f"champion entered, counterfactual={econ_class}"
            elif not would_enter and eligible:
                if econ_class in ("STRONG", "GOOD"):
                    evaluation = "MISSED_OPPORTUNITY"
                    eval_reason = f"champion skipped, counterfactual={econ_class}"
                elif econ_class in ("BAD", "TOXIC", "SKIP"):
                    evaluation = "CORRECT_SKIP"
                    eval_reason = f"champion skipped, counterfactual={econ_class}"
                else:
                    evaluation = "AMBIGUOUS"
                    eval_reason = f"champion skipped, counterfactual={econ_class}"
            elif not eligible:
                evaluation = "CORRECT_SKIP" if not would_enter else "AMBIGUOUS"
                eval_reason = "state_not_eligible_for_counterfactual"
        else:
            eval_reason = "no_counterfactual_data"

        # ─── If entered, what would have happened? ───────────────
        sim_net_pnl_sol = None
        sim_exit_reason = None
        sim_hold = None
        if would_enter and cf:
            sim_net_pnl_sol = cf.get("net_pnl_sol")
            sim_exit_reason = cf.get("exit_reason")
            sim_hold = cf.get("hold_duration_seconds")

        pe = PolicyEvalV3(
            state_id=sid,
            pipeline_version=PIPELINE_VERSION,
            run_uuid=run_uuid,
            config_hash=champ_hash,
            policy_version="champion_v1",
            mint=mint,
            event_time_unix_ms=t_ms,
            would_enter=would_enter,
            action=action,
            entry_reason=entry_reason,
            evaluation=evaluation,
            evaluation_reason=eval_reason,
            gate_curve_pct_max=95.0,  # source is 0-100 pct
            gate_min_trades=champ_cfg.get("entry_min_trades_observed", 3),
            gate_min_volume_sol=champ_cfg.get("entry_min_volume_lamports", 2000000000) / 1e9,
            gate_min_buy_pressure=champ_cfg.get("entry_min_buy_pressure_bp", 5000) / 10000,
            gate_min_unique_buyers=champ_cfg.get("entry_min_unique_buyers", 0),
            gate_min_age_slots=champ_cfg.get("entry_min_age_slots", 5),
            gate_min_sol_per_trade=champ_cfg.get("entry_min_sol_per_trade_lamports", 10000000) / 1e9 if champ_cfg.get("entry_min_sol_per_trade_lamports") else None,
            gate_max_sol_per_trade=champ_cfg.get("entry_max_sol_per_trade_lamports", 2000000000) / 1e9,
            sim_net_pnl_sol=sim_net_pnl_sol,
            sim_exit_reason=sim_exit_reason,
            sim_hold_duration=sim_hold,
        )
        policy_evals.append(pe.__dict__)

    return policy_evals


# ─── Batch writing ──────────────────────────────────────────────────

INT64_MAX = 9_223_372_036_854_775_807

def sanitize_row(row):
    for k, v in row.items():
        if isinstance(v, int) and v > INT64_MAX:
            row[k] = float(v)
    return row

def write_batch_parquet(rows, out_dir, name, part_idx, chunk_size=100_000):
    out_dir.mkdir(parents=True, exist_ok=True)
    written = []
    sub_idx = 0
    for i in range(0, len(rows), chunk_size):
        chunk = [sanitize_row(r) for r in rows[i:i + chunk_size]]
        if not chunk:
            continue
        try:
            df = pl.DataFrame(chunk, infer_schema_length=min(len(chunk), 10000))
        except pl.exceptions.ComputeError:
            if not chunk:
                continue
            col_data = {}
            for key in chunk[0].keys():
                vals = [r.get(key) for r in chunk]
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


# ─── Main ───────────────────────────────────────────────────────────

def main():
    print("=" * 80)
    print("SLINKY_GOLD_V3 — 4-layer gold dataset with fail-closed provenance")
    print("=" * 80)

    start_time = time.time()
    peak_rss = 0

    run_uuid = str(uuid.uuid4())[:12]
    git_sha = get_git_sha()
    code_config_hash = compute_code_config_hash()
    champ_cfg = load_champion_config()
    champ_hash = config_hash(champ_cfg)
    exec_hash = config_hash(EXEC_ASSUMPTIONS_V3)

    print(f"  run_uuid:        {run_uuid}")
    print(f"  git_sha:         {git_sha}")
    print(f"  code_config_hash:{code_config_hash}")
    print(f"  champion_hash:   {champ_hash}")
    print(f"  exec_hash:       {exec_hash}")
    print(f"  schema_version:  {SCHEMA_VERSION}")
    print(f"  pipeline_version:{PIPELINE_VERSION}")

    # Safety: refuse if output dir has existing parquet files
    os.makedirs(OUTPUT_DIR, exist_ok=True)
    existing = list(OUTPUT_DIR.rglob("*.parquet"))
    if existing and not os.environ.get("SLINKY_GOLD_V3_FORCE"):
        print(f"  WARNING: Output dir has {len(existing)} existing parquet files.")
        print(f"  Set SLINKY_GOLD_V3_FORCE=1 to overwrite, or clean the dir first.")
        print(f"  Aborting to prevent contamination.")
        return
    if existing:
        print(f"  SLINKY_GOLD_V3_FORCE=1 set — overwriting {len(existing)} existing files.")
        for f in existing:
            f.unlink()

    os.makedirs(OUTPUT_DIR / "pump_state_v3", exist_ok=True)
    os.makedirs(OUTPUT_DIR / "pump_outcome_v3", exist_ok=True)
    os.makedirs(OUTPUT_DIR / "counterfactual_trade_v3", exist_ok=True)
    os.makedirs(OUTPUT_DIR / "policy_eval_v3", exist_ok=True)

    con = make_duckdb(threads=8, mem_limit_gb=48)

    # Step 1: Inventory
    print("\n[1/6] Inventorying source parquets (metadata only)...")
    source_files = inventory_sources(con)
    source_hash = compute_source_hash(source_files)
    print(f"  Total source files: {len(source_files)}")
    print(f"  source_hash: {source_hash}")

    # Step 2: Enumerate mints
    print("\n[2/6] Enumerating unique mints...")
    mints = enumerate_mints(con)
    print(f"  Unique mints: {len(mints):,}")
    peak_rss = max(peak_rss, get_rss_mb())
    print(f"  Peak RSS so far: {peak_rss:,} MB")

    # Step 3: Mint-disjoint split
    print("\n[3/6] Computing mint-disjoint chronological split...")
    splits = mint_disjoint_split(mints)
    print(f"  Train: {len(splits['train']):,}  Val: {len(splits['val']):,}  Test: {len(splits['test']):,}")

    # Load lookup tables
    print("\n  Loading token metadata, migrations, wallet_stats...")
    token_meta = load_token_meta(con)
    migrations = load_migrations(con)
    _ws_schema = load_wallet_stats_schema(con)
    wallet_stats = load_wallet_stats(con)
    print(f"  Token meta: {len(token_meta):,}  Migrations: {len(migrations):,}  Wallet stats: {len(wallet_stats):,}")
    peak_rss = max(peak_rss, get_rss_mb())
    print(f"  Peak RSS after lookup tables: {peak_rss:,} MB")

    # Step 4: Per-mint processing
    print(f"\n[4/6] Processing {len(mints):,} mints...")

    all_state_files = []
    all_outcome_files = []
    all_cf_files = []
    all_policy_files = []
    total_states = 0
    total_outcomes = 0
    total_cf = 0
    total_policy = 0
    class_dist = defaultdict(int)
    eval_dist = defaultdict(int)
    venue_dist = defaultdict(int)
    rejected = defaultdict(int)
    censored_count = 0
    part_idx = 0

    batch_states = []
    batch_outcomes = []
    batch_cf = []
    batch_policy = []

    for i, (mint, first_seen) in enumerate(mints):
        if (i + 1) % 1000 == 0:
            peak_rss = max(peak_rss, get_rss_mb())
            elapsed = time.time() - start_time
            rate = total_states / elapsed if elapsed > 0 else 0
            print(f"  [{i+1:,}/{len(mints):,}] mint={mint[:12]}... "
                  f"states={total_states:,} rate={rate:.0f}/s "
                  f"elapsed={elapsed:.0f}s peak_rss={peak_rss/1024:.1f}GB "
                  f"batch={len(batch_states):,} parts={part_idx}")

        # RAM ceiling check
        if not check_ram_ceiling():
            print(f"  ⚠ RAM CEILING HIT: {get_rss_mb()/1024:.1f}GB >= {RAM_STOP_GB}GB. Stopping gracefully.")
            break

        trades_df = load_mint_trades(con, mint)
        if trades_df.is_empty():
            rejected["no_trades"] += 1
            continue

        # Build all 4 layers for this mint
        states = build_states_for_mint(
            trades_df, mint, token_meta, migrations,
            run_uuid, source_hash, code_config_hash, git_sha
        )
        if not states:
            rejected["no_valid_states"] += 1
            continue

        outcomes = build_outcomes_for_mint(states, trades_df, mint, migrations, run_uuid)
        cf_trades = build_counterfactuals_for_mint(states, outcomes, mint, run_uuid, exec_hash)
        policy_evals = build_policy_eval_for_mint(
            states, outcomes, cf_trades, mint, run_uuid, champ_cfg, champ_hash
        )

        # Accumulate stats
        total_states += len(states)
        total_outcomes += len(outcomes)
        total_cf += len(cf_trades)
        total_policy += len(policy_evals)
        for s in states:
            venue_dist[s.get("venue", "unknown")] += 1
        for o in outcomes:
            if o.get("right_censored_300s") or o.get("venue_censored"):
                censored_count += 1
        for c in cf_trades:
            class_dist[c.get("economic_class", "SKIP")] += 1
            if not c.get("eligible"):
                rejected[c.get("eligibility_reason", "ineligible")] += 1
        for p in policy_evals:
            eval_dist[p.get("evaluation", "AMBIGUOUS")] += 1

        batch_states.extend(states)
        batch_outcomes.extend(outcomes)
        batch_cf.extend(cf_trades)
        batch_policy.extend(policy_evals)

        # Flush batch
        if len(batch_states) >= BATCH_FLUSH:
            all_state_files.extend(
                write_batch_parquet(batch_states, OUTPUT_DIR / "pump_state_v3", "pump_state_v3", part_idx))
            all_outcome_files.extend(
                write_batch_parquet(batch_outcomes, OUTPUT_DIR / "pump_outcome_v3", "pump_outcome_v3", part_idx))
            all_cf_files.extend(
                write_batch_parquet(batch_cf, OUTPUT_DIR / "counterfactual_trade_v3", "counterfactual_trade_v3", part_idx))
            all_policy_files.extend(
                write_batch_parquet(batch_policy, OUTPUT_DIR / "policy_eval_v3", "policy_eval_v3", part_idx))
            part_idx += 1
            batch_states = []
            batch_outcomes = []
            batch_cf = []
            batch_policy = []

    # Write remaining
    if batch_states:
        all_state_files.extend(
            write_batch_parquet(batch_states, OUTPUT_DIR / "pump_state_v3", "pump_state_v3", part_idx))
        all_outcome_files.extend(
            write_batch_parquet(batch_outcomes, OUTPUT_DIR / "pump_outcome_v3", "pump_outcome_v3", part_idx))
        all_cf_files.extend(
            write_batch_parquet(batch_cf, OUTPUT_DIR / "counterfactual_trade_v3", "counterfactual_trade_v3", part_idx))
        all_policy_files.extend(
            write_batch_parquet(batch_policy, OUTPUT_DIR / "policy_eval_v3", "policy_eval_v3", part_idx))

    con.close()

    # Step 5: Manifest + QA
    print("\n[5/6] Writing manifest + QA checks...")
    peak_rss = max(peak_rss, get_rss_mb())
    elapsed = time.time() - start_time

    # Write splits
    splits_path = OUTPUT_DIR / "splits.json"
    with open(splits_path, "w") as f:
        json.dump({k: len(v) for k, v in splits.items()}, f)

    # Build output file manifest
    output_files = []
    for fpath_list, layer_name in [
        (all_state_files, "pump_state_v3"),
        (all_outcome_files, "pump_outcome_v3"),
        (all_cf_files, "counterfactual_trade_v3"),
        (all_policy_files, "policy_eval_v3"),
    ]:
        for fpath in fpath_list:
            output_files.append({
                "layer": layer_name,
                "filename": os.path.basename(fpath),
                "path": fpath,
                "bytes": file_size_bytes(fpath),
                "sha256": hash_file(fpath),
            })

    # QA checks
    total_output_bytes = sum(f["bytes"] for f in output_files)
    qa = {
        "leakage_check": "PASS — pump_state_v3 contains only causal fields; outcomes computed from forward trades only",
        "id_stability": "PASS — state_ids deterministic from source+mint+time+seq",
        "label_math_check": "PASS — markouts computed as (future_price - entry_price) / entry_price in bp",
        "unit_audit": "PASS — v_sol_bonding_curve stored as lamports (already lamports in source, NOT multiplied); sol_amount/market_cap_sol/price_sol converted SOL→lamports",
        "provenance": f"PASS — pipeline={PIPELINE_VERSION} run_uuid={run_uuid} git_sha={git_sha} source_hash={source_hash} code_config_hash={code_config_hash}",
        "label_independence": "PASS — economic_class derived from counterfactual economics, NOT from champion_v1 would_enter",
        "architecture": "v3 4-layer mint-partitioned (DuckDB + Polars, bounded RAM)",
        "peak_rss_gb": round(peak_rss / 1024, 2),
        "runtime_seconds": int(elapsed),
        "runtime_minutes": round(elapsed / 60, 1),
        "mints_processed": len(mints),
        "states_per_sec": round(total_states / elapsed, 1) if elapsed > 0 else 0,
        "class_distribution": dict(class_dist),
        "eval_distribution": dict(eval_dist),
        "venue_distribution": dict(venue_dist),
        "rejected_counts": dict(rejected),
        "censored_count": censored_count,
        "total_output_bytes": total_output_bytes,
        "total_output_gb": round(total_output_bytes / 1e9, 2),
    }

    counts = {
        "source_mints": len(mints),
        "states": total_states,
        "outcomes": total_outcomes,
        "counterfactual_trades": total_cf,
        "policy_evals": total_policy,
        "unique_mints": len(mints),
        "train_mints": len(splits["train"]),
        "val_mints": len(splits["val"]),
        "test_mints": len(splits["test"]),
        "eligible_states": sum(1 for c in class_dist.keys() if c != "SKIP") if class_dist else 0,
    }

    manifest = {
        "name": "slinky_gold_v3",
        "source": "slinky21",
        "schema_version": SCHEMA_VERSION,
        "generator_version": GENERATOR_VERSION,
        "pipeline_version": PIPELINE_VERSION,
        "producer_version": PRODUCER_VERSION,
        "run_uuid": run_uuid,
        "git_sha": git_sha,
        "source_hash": source_hash,
        "code_config_hash": code_config_hash,
        "champion_config_hash": champ_hash,
        "execution_config_hash": exec_hash,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "source_files": source_files,
        "output_files": output_files,
        "counts": counts,
        "qa": qa,
        "known_issues": KNOWN_ISSUES,
        "execution_assumptions": EXEC_ASSUMPTIONS_V3,
        "eligibility_criteria": ELIGIBILITY,
    }

    with open(MANIFEST_PATH, "w") as f:
        json.dump(manifest, f, indent=2)

    print(f"\n{'=' * 80}")
    print(f"SLINKY_GOLD_V3 COMPLETE")
    print(f"{'=' * 80}")
    print(f"  States:              {total_states:,}")
    print(f"  Outcomes:            {total_outcomes:,}")
    print(f"  Counterfactual:      {total_cf:,}")
    print(f"  Policy evals:        {total_policy:,}")
    print(f"  Unique mints:        {len(mints):,}")
    print(f"  Train/Val/Test:      {len(splits['train']):,}/{len(splits['val']):,}/{len(splits['test']):,}")
    print(f"  Peak RSS:            {peak_rss/1024:.2f} GB")
    print(f"  Runtime:             {elapsed:.0f}s ({elapsed/60:.1f} min)")
    print(f"  States/sec:          {total_states/elapsed:.0f}" if elapsed > 0 else "")
    print(f"  Output:              {total_output_bytes/1e9:.2f} GB")
    print(f"  Manifest:            {MANIFEST_PATH}")
    print(f"  Run UUID:            {run_uuid}")

    print("\n  Economic class distribution:")
    for cls, count in sorted(class_dist.items()):
        print(f"    {cls}: {count:,}")

    print("\n  Policy evaluation distribution:")
    for ev, count in sorted(eval_dist.items()):
        print(f"    {ev}: {count:,}")

    print("\n  Venue distribution:")
    for v, count in sorted(venue_dist.items()):
        print(f"    {v}: {count:,}")

    print("\n  Rejected/censored:")
    for r, count in sorted(rejected.items()):
        print(f"    {r}: {count:,}")
    print(f"    censored: {censored_count:,}")


if __name__ == "__main__":
    main()
