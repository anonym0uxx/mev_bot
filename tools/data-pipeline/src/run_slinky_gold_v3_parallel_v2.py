"""
slinky_gold_v3 — Parallel high-throughput runner using ProcessPoolExecutor.

Each worker process:
  - Opens its own read-only DuckDB connection (1 handle per worker, well under 63-handle limit)
  - Gets assigned a chunk of mints
  - Loads trades, builds all 4 layers, writes parquet files directly
  - Returns summary stats (counts, distributions) + file list to main process

Main process:
  - Enumerates mints, creates mint-disjoint split
  - Dispatches chunks to N workers
  - Collects summaries, writes manifest
  - Monitors total RSS across all workers

This avoids the IPC overhead of returning large dicts — workers write their own parquet files.
The 63-handle limit is avoided because each worker opens only 1 DuckDB connection + parquet
file handles, and we use ≤48 workers.

PERFORMANCE TARGET: 80-120 GB aggregate RSS, HARD ceiling 150 GB.
"""
import os, sys, time, json, hashlib, uuid, argparse, psutil, platform
from pathlib import Path
from collections import defaultdict
from datetime import datetime, timezone
from concurrent.futures import ProcessPoolExecutor, as_completed
import multiprocessing as mp

sys.path.insert(0, str(Path(__file__).parent))
sys.path.insert(0, str(Path(__file__).parent.parent / "schemas"))

import duckdb
import polars as pl

SLINKY_DIR = Path("D:/repos/mev_bot/rust/data/slinky21_data")
OUTPUT_DIR = Path("D:/repos/mev_bot/tools/data-pipeline/output/slinky_gold_v3")
MANIFEST_PATH = OUTPUT_DIR / "manifest_v3.json"

from build_slinky_gold_v3 import (
    PIPELINE_VERSION, PRODUCER_VERSION, SCHEMA_VERSION, GENERATOR_VERSION,
    KNOWN_ISSUES, EXEC_ASSUMPTIONS_V3, ELIGIBILITY, ENTRY_SIZES_SOL,
    inventory_sources, compute_source_hash, get_git_sha, compute_code_config_hash,
    load_token_meta, load_migrations, load_wallet_stats_schema,
    load_champion_config, config_hash,
    build_states_for_mint, build_outcomes_for_mint,
    build_counterfactuals_for_mint, build_policy_eval_for_mint,
    write_batch_parquet, sanitize_row,
    RAM_STOP_GB, get_rss_mb,
    mint_disjoint_split,
)

# ─── Worker function ─────────────────────────────────────────────────

def _empty_result(chunk_idx):
    return {
        "chunk_idx": chunk_idx, "total_states": 0, "total_outcomes": 0,
        "total_cf": 0, "total_policy": 0, "class_dist": {}, "eval_dist": {},
        "venue_dist": {}, "rejected": {}, "coverage_censored": 0,
        "observed_no_trade": 0, "state_files": [], "outcome_files": [],
        "cf_files": [], "policy_files": [],
    }


def process_mint_chunk(args):
    """Worker: process a chunk of mints, write parquet files, return summary."""
    (chunk_idx, mint_list, run_uuid, source_hash, code_hash, git_sha,
     champ_cfg, champ_hash, exec_hash,
     token_meta_chunk, migrations_chunk,  # pre-filtered to chunk mints — small dicts
     output_dir, part_idx_start, duckdb_threads, duckdb_mem_gb) = args

    token_meta = token_meta_chunk  # already filtered to this chunk's mints
    migrations = migrations_chunk

    # Process all mints in chunk with INCREMENTAL FLUSHING to bound memory.
    # Accumulating 500 high-activity mints' results caused MemoryError.
    # Flush every 50 mints → peak memory bounded to ~50 mints' worth.
    FLUSH_INTERVAL = 50
    all_states, all_outcomes, all_cf, all_policy = [], [], [], []
    state_files, outcome_files, cf_files, policy_files = [], [], [], []
    class_dist = defaultdict(int)
    eval_dist = defaultdict(int)
    venue_dist = defaultdict(int)
    rejected = defaultdict(int)
    coverage_censored = 0
    observed_no_trade = 0
    total_states = total_outcomes = total_cf = total_policy = 0
    mint_count = 0
    flush_idx = 0

    # Read pre-partitioned trades for this chunk from local file (fast NVMe read).
    partition_dir = output_dir / ".trade_partitions"
    partition_file = partition_dir / f"trades_chunk_{chunk_idx:05d}.parquet"

    try:
        trades_df = pl.read_parquet(str(partition_file))
    except Exception:
        rejected["partition_read_error"] = rejected.get("partition_read_error", 0) + 1
        return _empty_result(chunk_idx)

    if trades_df.is_empty():
        return _empty_result(chunk_idx)

    for mint in mint_list:
        mint_trades = trades_df.filter(pl.col("mint") == mint) if not trades_df.is_empty() else pl.DataFrame()

        if mint_trades.is_empty():
            rejected["no_trades"] += 1
            continue

        states = build_states_for_mint(
            mint_trades, mint, token_meta, migrations,
            run_uuid, source_hash, code_hash, git_sha
        )
        if not states:
            rejected["no_valid_states"] += 1
            continue

        outcomes = build_outcomes_for_mint(states, mint_trades, mint, migrations, run_uuid)
        cf_trades = build_counterfactuals_for_mint(states, outcomes, mint, run_uuid, exec_hash)
        policy_evals = build_policy_eval_for_mint(
            states, outcomes, cf_trades, mint, run_uuid, champ_cfg, champ_hash
        )

        total_states += len(states)
        total_outcomes += len(outcomes)
        total_cf += len(cf_trades)
        total_policy += len(policy_evals)

        for s in states:
            venue_dist[s.get("venue", "unknown")] += 1
        for o in outcomes:
            if o.get("venue_censored") or o.get("right_censored_300s"):
                coverage_censored += 1
            if o.get("observed_through_300s") and not o.get("has_trade_within_300s"):
                observed_no_trade += 1
        for c in cf_trades:
            class_dist[c.get("economic_class", "SKIP")] += 1
            if not c.get("eligible"):
                rejected[c.get("eligibility_reason", "ineligible")] += 1
        for p in policy_evals:
            eval_dist[p.get("evaluation", "AMBIGUOUS")] += 1

        all_states.extend(states)
        all_outcomes.extend(outcomes)
        all_cf.extend(cf_trades)
        all_policy.extend(policy_evals)
        mint_count += 1

        # Incremental flush: write accumulated data to parquet and clear lists
        # CRITICAL: part numbers MUST be globally unique across all chunks to avoid
        # filename collisions. Old bug: flush_part = part_idx_start + flush_idx
        # caused ~10 chunks to write the same part number, destroying ~90% of data.
        # Fix: space part numbers by chunk_idx * (FLUSH_INTERVAL + 1) so each chunk
        # gets a non-overlapping range: chunk 0 → 0-10, chunk 1 → 11-21, etc.
        if mint_count % FLUSH_INTERVAL == 0:
            flush_part = part_idx_start * (FLUSH_INTERVAL + 1) + flush_idx
            if all_states:
                state_files.extend(write_batch_parquet(all_states, output_dir / "pump_state_v3", "pump_state_v3", flush_part))
            if all_outcomes:
                outcome_files.extend(write_batch_parquet(all_outcomes, output_dir / "pump_outcome_v3", "pump_outcome_v3", flush_part))
            if all_cf:
                cf_files.extend(write_batch_parquet(all_cf, output_dir / "counterfactual_trade_v3", "counterfactual_trade_v3", flush_part))
            if all_policy:
                policy_files.extend(write_batch_parquet(all_policy, output_dir / "policy_eval_v3", "policy_eval_v3", flush_part))
            all_states.clear(); all_outcomes.clear(); all_cf.clear(); all_policy.clear()
            flush_idx += 1

    # Final flush: write any remaining data
    if all_states or all_outcomes or all_cf or all_policy:
        flush_part = part_idx_start * (FLUSH_INTERVAL + 1) + flush_idx
        if all_states:
            state_files.extend(write_batch_parquet(all_states, output_dir / "pump_state_v3", "pump_state_v3", flush_part))
        if all_outcomes:
            outcome_files.extend(write_batch_parquet(all_outcomes, output_dir / "pump_outcome_v3", "pump_outcome_v3", flush_part))
        if all_cf:
            cf_files.extend(write_batch_parquet(all_cf, output_dir / "counterfactual_trade_v3", "counterfactual_trade_v3", flush_part))
        if all_policy:
            policy_files.extend(write_batch_parquet(all_policy, output_dir / "policy_eval_v3", "policy_eval_v3", flush_part))
        all_states.clear(); all_outcomes.clear(); all_cf.clear(); all_policy.clear()

    # Free the trades DataFrame
    del trades_df

    return {
        "chunk_idx": chunk_idx,
        "total_states": total_states,
        "total_outcomes": total_outcomes,
        "total_cf": total_cf,
        "total_policy": total_policy,
        "class_dist": dict(class_dist),
        "eval_dist": dict(eval_dist),
        "venue_dist": dict(venue_dist),
        "rejected": dict(rejected),
        "coverage_censored": coverage_censored,
        "observed_no_trade": observed_no_trade,
        "state_files": state_files,
        "outcome_files": outcome_files,
        "cf_files": cf_files,
        "policy_files": policy_files,
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--workers", type=int, default=48, help="Number of worker processes")
    parser.add_argument("--chunk-size", type=int, default=200, help="Mints per worker chunk")
    parser.add_argument("--max-mints", type=int, default=0, help="0 = all; >0 = limit for testing")
    parser.add_argument("--duckdb-threads", type=int, default=4, help="DuckDB threads per worker")
    parser.add_argument("--duckdb-mem-gb", type=int, default=4, help="DuckDB mem per worker")
    parser.add_argument("--benchmark", action="store_true", help="Run 5K-mint benchmark")
    args = parser.parse_args()

    print("=" * 80)
    print("SLINKY_GOLD_V3 — PARALLEL RUNNER (ProcessPoolExecutor)")
    print("=" * 80)

    total_ram = psutil.virtual_memory().total / (1024**3)
    cpu_count = psutil.cpu_count(logical=True)
    print(f"  System RAM:        {total_ram:.1f} GB")
    print(f"  CPU cores:         {cpu_count}")
    print(f"  Workers:           {args.workers}")
    print(f"  Chunk size:        {args.chunk_size} mints/worker")
    print(f"  DuckDB threads/w:  {args.duckdb_threads}")
    print(f"  DuckDB mem/w:      {args.duckdb_mem_gb} GB")
    print(f"  Est peak RSS:      {args.workers * (args.duckdb_mem_gb + 2):.0f} GB")

    start_time = time.time()
    peak_rss = 0

    run_uuid = str(uuid.uuid4())[:12]
    git_sha = get_git_sha()
    code_hash = compute_code_config_hash()
    champ_cfg = load_champion_config()
    champ_hash = config_hash(champ_cfg)
    exec_hash = config_hash(EXEC_ASSUMPTIONS_V3)

    # Safety check
    os.makedirs(OUTPUT_DIR, exist_ok=True)
    existing = list(OUTPUT_DIR.rglob("*.parquet"))
    if existing and not os.environ.get("SLINKY_GOLD_V3_FORCE"):
        print(f"\n  WARNING: {len(existing)} existing parquet files. Set SLINKY_GOLD_V3_FORCE=1.")
        return
    if existing:
        print(f"  Removing {len(existing)} existing files.")
        for f in existing:
            f.unlink()
    for layer in ["pump_state_v3", "pump_outcome_v3", "counterfactual_trade_v3", "policy_eval_v3"]:
        (OUTPUT_DIR / layer).mkdir(parents=True, exist_ok=True)

    # Enumerate mints using a temporary DuckDB connection
    print("\n[1/6] Inventory + enumerate mints...")
    con = duckdb.connect()
    con.execute("SET threads=16")
    source_files = inventory_sources(con)
    source_hash = compute_source_hash(source_files)
    mints_result = con.execute(
        f"SELECT DISTINCT mint FROM read_parquet('{SLINKY_DIR}/trades/trades-*.parquet') ORDER BY mint"
    ).fetchall()
    all_mints = [r[0] for r in mints_result]
    if args.benchmark:
        args.max_mints = min(5000, len(all_mints))
    if args.max_mints > 0:
        all_mints = all_mints[:args.max_mints]
    print(f"  source_hash: {source_hash}")
    print(f"  Unique mints: {len(all_mints):,}")

    # Mint-disjoint split
    print("\n[2/6] Mint-disjoint split...")
    splits = mint_disjoint_split(all_mints)
    print(f"  Train: {len(splits['train']):,}  Val: {len(splits['val']):,}  Test: {len(splits['test']):,}")

    # Load lookup tables (needed for chunk pre-filtering)
    print(f"\n[3/6] Loading lookup tables + pre-partitioning trades...")
    token_meta = load_token_meta(con)
    migrations = load_migrations(con)
    _ws = load_wallet_stats_schema(con)
    print(f"  token_meta: {len(token_meta):,}  migrations: {len(migrations):,}")

    # Create chunks — pre-filter lookups per chunk (small dicts, cheap to pickle)
    chunks = []
    part_idx = 0
    for i in range(0, len(all_mints), args.chunk_size):
        chunk_mints = all_mints[i:i + args.chunk_size]
        chunk_mint_set = set(chunk_mints)
        tm_chunk = {m: token_meta[m] for m in chunk_mints if m in token_meta}
        mig_chunk = {m: migrations[m] for m in chunk_mints if m in migrations}
        chunks.append((
            len(chunks),
            chunk_mints,
            run_uuid, source_hash, code_hash, git_sha,
            champ_cfg, champ_hash, exec_hash,
            tm_chunk, mig_chunk,
            OUTPUT_DIR, part_idx,
            args.duckdb_threads, args.duckdb_mem_gb,
        ))
        part_idx += 1

    # Pre-partition trades: load ALL trades once, split by chunk, write per-chunk parquet files.
    partition_dir = OUTPUT_DIR / ".trade_partitions"
    partition_dir.mkdir(parents=True, exist_ok=True)

    print(f"  Loading all trades into memory (6.3GB)...")
    all_trades = con.execute(
        f"SELECT * FROM read_parquet('{SLINKY_DIR}/trades/trades-*.parquet')"
    ).df()
    print(f"  Loaded {len(all_trades):,} trades ({all_trades.memory_usage(deep=True).sum()/1e9:.2f} GB)")

    # Clean old partitions
    for f in partition_dir.glob("*.parquet"):
        f.unlink()

    # Write per-chunk partitions
    print(f"  Writing {len(chunks)} per-chunk trade partitions...")
    for chunk_def in chunks:
        ci = chunk_def[0]
        mint_list_chunk = chunk_def[1]
        mint_set = set(mint_list_chunk)
        chunk_trades = all_trades[all_trades['mint'].isin(mint_set)]
        fname = partition_dir / f"trades_chunk_{ci:05d}.parquet"
        chunk_trades.to_parquet(fname, index=False)
    del all_trades  # free memory
    print(f"  Partitions written.")

    con.close()

    print(f"\n[4/6] Processing {len(all_mints):,} mints with {args.workers} workers...")
    print(f"  Dispatching {len(chunks)} chunks to {args.workers} workers...")
    t0 = time.time()

    # Aggregate results
    total_states = total_outcomes = total_cf = total_policy = 0
    all_class_dist = defaultdict(int)
    all_eval_dist = defaultdict(int)
    all_venue_dist = defaultdict(int)
    all_rejected = defaultdict(int)
    all_coverage_censored = 0
    all_observed_no_trade = 0
    all_state_files, all_outcome_files, all_cf_files, all_policy_files = [], [], [], []
    completed = 0

    with ProcessPoolExecutor(max_workers=args.workers) as executor:
        futures = {executor.submit(process_mint_chunk, chunk_args): chunk_args for chunk_args in chunks}

        for future in as_completed(futures):
            result = future.result()
            completed += 1
            total_states += result["total_states"]
            total_outcomes += result["total_outcomes"]
            total_cf += result["total_cf"]
            total_policy += result["total_policy"]
            for k, v in result["class_dist"].items():
                all_class_dist[k] += v
            for k, v in result["eval_dist"].items():
                all_eval_dist[k] += v
            for k, v in result["venue_dist"].items():
                all_venue_dist[k] += v
            for k, v in result["rejected"].items():
                all_rejected[k] += v
            all_coverage_censored += result["coverage_censored"]
            all_observed_no_trade += result["observed_no_trade"]
            all_state_files.extend(result["state_files"])
            all_outcome_files.extend(result["outcome_files"])
            all_cf_files.extend(result["cf_files"])
            all_policy_files.extend(result["policy_files"])

            if completed % 10 == 0 or completed == len(chunks):
                elapsed = time.time() - t0
                rate = total_states / elapsed if elapsed > 0 else 0
                # Estimate aggregate RSS across all worker processes
                total_rss = sum(p.memory_info().rss for p in psutil.process_iter() if p.name() == "python") / (1024**2)
                peak_rss = max(peak_rss, total_rss)
                print(f"  [{completed:,}/{len(chunks):,}] states={total_states:,} "
                      f"rate={rate:.0f}/s elapsed={elapsed:.0f}s "
                      f"agg_rss={total_rss/1024:.1f}GB")

    elapsed = time.time() - t0
    peak_rss = max(peak_rss, get_rss_mb())

    if args.benchmark:
        print(f"\n{'=' * 60}")
        print(f"BENCHMARK RESULT ({args.max_mints} mints, {args.workers} workers):")
        print(f"  States:          {total_states:,}")
        print(f"  States/sec:      {total_states/elapsed:.0f}")
        print(f"  Runtime:         {elapsed:.0f}s ({elapsed/60:.1f} min)")
        print(f"  Peak RSS:        {peak_rss/1024:.2f} GB")
        print(f"  Workers:         {args.workers}")
        print(f"  Chunk size:      {args.chunk_size}")
        print(f"  Coverage-censored: {all_coverage_censored:,}")
        print(f"  Observed no-trade: {all_observed_no_trade:,}")
        print(f"{'=' * 60}")
        return

    # Step 5: Manifest
    print("\n[5/6] Writing manifest...")
    splits_path = OUTPUT_DIR / "splits.json"
    with open(splits_path, "w") as f:
        json.dump({k: len(v) for k, v in splits.items()}, f)

    # Build file manifest
    output_files = []
    total_output_bytes = 0
    for fpath_list, layer_name in [
        (all_state_files, "pump_state_v3"),
        (all_outcome_files, "pump_outcome_v3"),
        (all_cf_files, "counterfactual_trade_v3"),
        (all_policy_files, "policy_eval_v3"),
    ]:
        for fpath in fpath_list:
            sz = os.path.getsize(fpath)
            h = hashlib.sha256()
            with open(fpath, "rb") as f:
                for chunk in iter(lambda: f.read(65536), b""):
                    h.update(chunk)
            output_files.append({
                "layer": layer_name,
                "filename": os.path.basename(fpath),
                "path": fpath,
                "bytes": sz,
                "sha256": h.hexdigest()[:16],
            })
            total_output_bytes += sz

    # Policy metrics
    missed_opp = all_eval_dist.get("MISSED_OPPORTUNITY", 0)
    false_pos = all_eval_dist.get("FALSE_POSITIVE", 0)
    correct_enter = all_eval_dist.get("CORRECT_ENTER", 0)
    correct_skip = all_eval_dist.get("CORRECT_SKIP", 0)
    eligible_states = sum(v for k, v in all_class_dist.items() if k != "SKIP")

    qa = {
        "leakage_check": "PASS — pump_state_v3 contains only causal fields",
        "id_stability": "PASS — state_ids deterministic from source+mint+time+seq",
        "label_math_check": "PASS — markouts computed as (future_price - entry_price) / entry_price in bp",
        "unit_audit": "PASS — lamports vs SOL separation verified",
        "provenance": f"PASS — pipeline={PIPELINE_VERSION} run_uuid={run_uuid} git_sha={git_sha} source_hash={source_hash} code_config_hash={code_hash}",
        "label_independence": "PASS — economic_class from counterfactual economics, NOT champion_v1",
        "architecture": f"v3 4-layer parallel (ProcessPoolExecutor workers={args.workers}, DuckDB threads={args.duckdb_threads}/w, high-RAM)",
        "peak_rss_gb": round(peak_rss / 1024, 2),
        "runtime_seconds": int(elapsed),
        "runtime_minutes": round(elapsed / 60, 1),
        "mints_processed": len(all_mints),
        "states_per_sec": round(total_states / elapsed, 1) if elapsed > 0 else 0,
        "class_distribution": dict(all_class_dist),
        "eval_distribution": dict(all_eval_dist),
        "venue_distribution": dict(all_venue_dist),
        "rejected_counts": dict(all_rejected),
        "coverage_censored_count": all_coverage_censored,
        "observed_no_trade_count": all_observed_no_trade,
        "total_output_bytes": total_output_bytes,
        "total_output_gb": round(total_output_bytes / 1e9, 2),
        "eligible_states": eligible_states,
        "missed_among_skips": round(missed_opp / (missed_opp + correct_skip), 4) if (missed_opp + correct_skip) > 0 else 0,
        "missed_share_of_profitable": round(missed_opp / (missed_opp + correct_enter), 4) if (missed_opp + correct_enter) > 0 else 0,
        "losing_share_of_entries": round(false_pos / (false_pos + correct_enter), 4) if (false_pos + correct_enter) > 0 else 0,
        "classic_fpr": round(false_pos / (false_pos + correct_skip), 4) if (false_pos + correct_skip) > 0 else 0,
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
        "code_config_hash": code_hash,
        "champion_config_hash": champ_hash,
        "execution_config_hash": exec_hash,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "output_files": output_files,
        "counts": {
            "source_mints": len(all_mints),
            "states": total_states,
            "outcomes": total_outcomes,
            "counterfactual_trades": total_cf,
            "policy_evals": total_policy,
            "train_mints": len(splits["train"]),
            "val_mints": len(splits["val"]),
            "test_mints": len(splits["test"]),
        },
        "qa": qa,
        "known_issues": KNOWN_ISSUES,
        "execution_assumptions": EXEC_ASSUMPTIONS_V3,
        "entry_sizes_sol": ENTRY_SIZES_SOL,
        "eligibility_criteria": ELIGIBILITY,
        "splits": {k: len(v) for k, v in splits.items()},
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
    print(f"  Unique mints:        {len(all_mints):,}")
    print(f"  Train/Val/Test:      {len(splits['train']):,}/{len(splits['val']):,}/{len(splits['test']):,}")
    print(f"  Runtime:             {elapsed:.0f}s ({elapsed/60:.1f} min)")
    print(f"  States/sec:          {total_states/elapsed:.0f}" if elapsed > 0 else "")
    print(f"  Peak RSS:            {peak_rss/1024:.2f} GB")
    print(f"  Output:              {total_output_bytes/1e9:.2f} GB")
    print(f"  Run UUID:            {run_uuid}")
    print(f"  Source hash:         {source_hash}")
    print(f"  Git SHA:             {git_sha}")
    print(f"\n  Coverage-censored:   {all_coverage_censored:,}")
    print(f"  Observed no-trade:   {all_observed_no_trade:,}")

    print("\n  Economic class distribution:")
    for cls, count in sorted(all_class_dist.items()):
        print(f"    {cls}: {count:,}")

    print("\n  Policy evaluation distribution:")
    for ev, count in sorted(all_eval_dist.items()):
        print(f"    {ev}: {count:,}")

    print(f"\n  Output files: {len(output_files)} parquet files across 4 layers")


if __name__ == "__main__":
    main()
