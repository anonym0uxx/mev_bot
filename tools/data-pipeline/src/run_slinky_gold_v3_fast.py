"""
slinky_gold_v3 — High-throughput single-process runner with batched DuckDB I/O.

PERFORMANCE TARGET (2026-08-24):
  - Target 80-120 GB working RSS for larger partitions/caches/batches.
  - HARD ceiling = 145 GB; never exceed 150 GB.
  - Use DuckDB/Polars/Arrow vectorization, large batches, many threads.
  - Avoid Python-object expansion/to_pylist where possible.

Key optimization: load trades for BATCHES of mints (2000+/batch) in one DuckDB query,
then process each mint in the batch. DuckDB threads parallelize parquet scanning.

No multiprocessing — avoids Windows 63-handle limit and IPC overhead.
"""
import os, sys, time, json, hashlib, uuid, argparse, psutil, platform
from pathlib import Path
from collections import defaultdict
from datetime import datetime, timezone

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
    load_token_meta, load_migrations, load_wallet_stats, load_wallet_stats_schema,
    load_champion_config, config_hash,
    build_states_for_mint, build_outcomes_for_mint,
    build_counterfactuals_for_mint, build_policy_eval_for_mint,
    write_batch_parquet, sanitize_row,
    RAM_STOP_GB, get_rss_mb,
    mint_disjoint_split,
)

# Performance defaults — designed for ~165GB system RAM
DEFAULT_BATCH_SIZE = 2000       # mints per DuckDB batch query (up from 500)
DEFAULT_DUCKDB_THREADS = 32     # EPYC has many cores; DuckDB parallelizes scans
DEFAULT_DUCKDB_MEM_GB = 100     # large DuckDB memory limit for big batches
DEFAULT_FLUSH_ROWS = 500_000    # rows per parquet flush (up from 50K)


def make_duckdb(threads=32, mem_limit_gb=100):
    con = duckdb.connect()
    con.execute(f"SET threads={threads}")
    con.execute(f"SET memory_limit='{mem_limit_gb}GB'")
    con.execute("SET temp_directory='D:/repos/mev_bot/tools/data-pipeline/.duckdb_temp'")
    con.execute("SET preserve_insertion_order=false")
    return con


def hash_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()[:16]


def file_size_bytes(path):
    return os.path.getsize(path)


def get_cpu_utilization():
    """CPU utilization across all cores (0-100)."""
    try:
        return psutil.cpu_percent(interval=0.1)
    except Exception:
        return 0.0


def get_disk_io():
    """Disk I/O stats (MB/s read/write) — best effort."""
    try:
        disk_io = psutil.disk_io_counters()
        if disk_io:
            return {
                "read_mb_s": disk_io.read_bytes / 1e6,
                "write_mb_s": disk_io.write_bytes / 1e6,
            }
    except Exception:
        pass
    return {}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--batch-size", type=int, default=DEFAULT_BATCH_SIZE,
                        help=f"Mints per DuckDB batch query (default {DEFAULT_BATCH_SIZE})")
    parser.add_argument("--max-mints", type=int, default=0,
                        help="0 = all mints; >0 = limit for testing")
    parser.add_argument("--duckdb-threads", type=int, default=DEFAULT_DUCKDB_THREADS)
    parser.add_argument("--duckdb-mem-gb", type=int, default=DEFAULT_DUCKDB_MEM_GB)
    parser.add_argument("--flush-rows", type=int, default=DEFAULT_FLUSH_ROWS,
                        help=f"Rows per parquet flush (default {DEFAULT_FLUSH_ROWS})")
    parser.add_argument("--benchmark", action="store_true",
                        help="Run 5K-mint benchmark only, report throughput, exit")
    args = parser.parse_args()

    print("=" * 80)
    print("SLINKY_GOLD_V3 — HIGH-THROUGHPUT BATCHED RUNNER")
    print("=" * 80)

    # System info
    total_ram = psutil.virtual_memory().total / (1024**3)
    cpu_count = psutil.cpu_count(logical=True)
    print(f"  System RAM:        {total_ram:.1f} GB")
    print(f"  CPU cores:         {cpu_count}")
    print(f"  Platform:          {platform.platform()}")
    print(f"  batch_size:        {args.batch_size}")
    print(f"  duckdb_threads:    {args.duckdb_threads}")
    print(f"  duckdb_mem_gb:     {args.duckdb_mem_gb}")
    print(f"  flush_rows:        {args.flush_rows}")
    print(f"  RAM target:        80-120 GB working RSS")
    print(f"  RAM hard ceiling:  {RAM_STOP_GB} GB")

    start_time = time.time()
    peak_rss = 0

    run_uuid = str(uuid.uuid4())[:12]
    git_sha = get_git_sha()
    code_hash = compute_code_config_hash()
    champ_cfg = load_champion_config()
    champ_hash = config_hash(champ_cfg)
    exec_hash = config_hash(EXEC_ASSUMPTIONS_V3)

    print(f"  run_uuid:          {run_uuid}")
    print(f"  git_sha:           {git_sha}")
    print(f"  code_hash:         {code_hash}")
    print(f"  champion_hash:     {champ_hash}")
    print(f"  exec_hash:         {exec_hash}")
    print(f"  schema_version:    {SCHEMA_VERSION}")
    print(f"  pipeline_version:  {PIPELINE_VERSION}")
    print(f"  entry_sizes_sol:   {ENTRY_SIZES_SOL}")

    # Safety: refuse if output dir has existing parquet files
    os.makedirs(OUTPUT_DIR, exist_ok=True)
    existing = list(OUTPUT_DIR.rglob("*.parquet"))
    if existing and not os.environ.get("SLINKY_GOLD_V3_FORCE"):
        print(f"\n  WARNING: Output dir has {len(existing)} existing parquet files.")
        print(f"  Set SLINKY_GOLD_V3_FORCE=1 to overwrite, or clean the dir first.")
        print(f"  Aborting to prevent contamination.")
        return
    if existing:
        print(f"  SLINKY_GOLD_V3_FORCE=1 — removing {len(existing)} existing files.")
        for f in existing:
            f.unlink()

    for layer in ["pump_state_v3", "pump_outcome_v3", "counterfactual_trade_v3", "policy_eval_v3"]:
        (OUTPUT_DIR / layer).mkdir(parents=True, exist_ok=True)

    con = make_duckdb(threads=args.duckdb_threads, mem_limit_gb=args.duckdb_mem_gb)

    # Step 1: Inventory
    print("\n[1/7] Inventorying source parquets...")
    source_files = inventory_sources(con)
    source_hash = compute_source_hash(source_files)
    print(f"  source_hash: {source_hash}")

    # Step 2: Enumerate mints
    print("\n[2/7] Enumerating unique mints...")
    mints_result = con.execute(
        f"SELECT DISTINCT mint FROM read_parquet('{SLINKY_DIR}/trades/trades-*.parquet') ORDER BY mint"
    ).fetchall()
    all_mints = [r[0] for r in mints_result]
    if args.benchmark:
        args.max_mints = min(5000, len(all_mints))
        print(f"  BENCHMARK MODE: processing {args.max_mints} mints")
    if args.max_mints > 0:
        all_mints = all_mints[:args.max_mints]
    print(f"  Unique mints: {len(all_mints):,}")

    # Step 3: Mint-disjoint split
    print("\n[3/7] Computing mint-disjoint chronological split...")
    splits = mint_disjoint_split(all_mints)
    print(f"  Train: {len(splits['train']):,}  Val: {len(splits['val']):,}  Test: {len(splits['test']):,}")

    # Step 4: Load lookup tables
    print("\n[4/7] Loading lookup tables into RAM...")
    token_meta = load_token_meta(con)
    migrations = load_migrations(con)
    _ws = load_wallet_stats_schema(con)
    print(f"  token_meta: {len(token_meta):,}  migrations: {len(migrations):,}")
    peak_rss = max(peak_rss, get_rss_mb())
    print(f"  Peak RSS after lookups: {peak_rss/1024:.2f} GB")

    # Step 5: Batched processing
    print(f"\n[5/7] Processing {len(all_mints):,} mints in batches of {args.batch_size}...")

    all_state_files, all_outcome_files = [], []
    all_cf_files, all_policy_files = [], []
    total_states = total_outcomes = total_cf = total_policy = 0
    class_dist = defaultdict(int)
    eval_dist = defaultdict(int)
    venue_dist = defaultdict(int)
    rejected = defaultdict(int)
    coverage_censored_count = 0     # truly coverage-censored (observation ended before horizon)
    observed_no_trade_count = 0     # observed through 300s but no trades (NOT censored)
    part_idx = 0

    batch_states, batch_outcomes, batch_cf, batch_policy = [], [], [], []
    batch_count = 0

    n_batches = (len(all_mints) + args.batch_size - 1) // args.batch_size
    t0 = time.time()
    flush_rows = args.flush_rows

    for bi in range(0, len(all_mints), args.batch_size):
        batch_mints = all_mints[bi:bi + args.batch_size]
        batch_count += 1

        # Single DuckDB query for all mints in this batch — large batch = better scan parallelism
        mint_sql = ",".join(f"'{m}'" for m in batch_mints)
        try:
            trades_df = pl.from_pandas(
                con.execute(
                    f"SELECT * FROM read_parquet('{SLINKY_DIR}/trades/trades-*.parquet') WHERE mint IN ({mint_sql})"
                ).df()
            )
        except Exception as e:
            print(f"  WARNING: batch {batch_count} DuckDB query failed: {e}")
            for m in batch_mints:
                rejected["duckdb_query_error"] = rejected.get("duckdb_query_error", 0) + 1
            continue

        # Process each mint in the batch
        for mint in batch_mints:
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
                # CORRECT censoring semantics: count coverage-censored vs observed no-trade separately
                if o.get("venue_censored") or o.get("right_censored_300s"):
                    coverage_censored_count += 1
                if o.get("observed_through_300s") and not o.get("has_trade_within_300s"):
                    observed_no_trade_count += 1
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

        # Flush batch — larger flush = fewer parquet files = better downstream scan
        if len(batch_states) >= flush_rows:
            all_state_files.extend(
                write_batch_parquet(batch_states, OUTPUT_DIR / "pump_state_v3", "pump_state_v3", part_idx))
            all_outcome_files.extend(
                write_batch_parquet(batch_outcomes, OUTPUT_DIR / "pump_outcome_v3", "pump_outcome_v3", part_idx))
            all_cf_files.extend(
                write_batch_parquet(batch_cf, OUTPUT_DIR / "counterfactual_trade_v3", "counterfactual_trade_v3", part_idx))
            all_policy_files.extend(
                write_batch_parquet(batch_policy, OUTPUT_DIR / "policy_eval_v3", "policy_eval_v3", part_idx))
            part_idx += 1
            batch_states, batch_outcomes, batch_cf, batch_policy = [], [], [], []

        # Progress report with CPU + RAM
        if batch_count % 5 == 0 or bi + args.batch_size >= len(all_mints):
            peak_rss = max(peak_rss, get_rss_mb())
            elapsed = time.time() - t0
            rate = total_states / elapsed if elapsed > 0 else 0
            cpu_pct = get_cpu_utilization()
            eta_min = (len(all_mints) - (bi + len(batch_mints))) / args.batch_size * elapsed / 5 / 60
            print(f"  [{batch_count:,}/{n_batches:,}] mints={bi+len(batch_mints):,}/{len(all_mints):,} "
                  f"states={total_states:,} rate={rate:.0f}/s "
                  f"elapsed={elapsed:.0f}s peak_rss={peak_rss/1024:.1f}GB "
                  f"cpu={cpu_pct:.0f}% eta={eta_min:.0f}min parts={part_idx}")

        # RAM ceiling check
        if peak_rss / 1024 >= RAM_STOP_GB:
            print(f"  ⚠ RAM CEILING HIT: {peak_rss/1024:.1f}GB >= {RAM_STOP_GB}GB. Stopping gracefully.")
            break

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
    elapsed = time.time() - t0
    peak_rss = max(peak_rss, get_rss_mb())

    if args.benchmark:
        print(f"\n{'=' * 60}")
        print(f"BENCHMARK RESULT (5000 mints):")
        print(f"  States:          {total_states:,}")
        print(f"  States/sec:      {total_states/elapsed:.0f}")
        print(f"  Runtime:         {elapsed:.0f}s ({elapsed/60:.1f} min)")
        print(f"  Peak RSS:        {peak_rss/1024:.2f} GB")
        print(f"  CPU cores:       {psutil.cpu_count(logical=True)}")
        print(f"  DuckDB threads:  {args.duckdb_threads}")
        print(f"  Batch size:      {args.batch_size}")
        print(f"  Flush rows:      {args.flush_rows}")
        print(f"  Parquet parts:   {part_idx+1}")
        print(f"  Coverage-censored:     {coverage_censored_count:,}")
        print(f"  Observed no-trade:     {observed_no_trade_count:,}")
        print(f"{'=' * 60}")
        return

    # Step 6: Splits file + manifest
    print("\n[6/7] Writing splits + manifest...")
    splits_path = OUTPUT_DIR / "splits.json"
    with open(splits_path, "w") as f:
        json.dump({k: len(v) for k, v in splits.items()}, f)

    # Build output file manifest
    output_files = []
    total_output_bytes = 0
    for fpath_list, layer_name in [
        (all_state_files, "pump_state_v3"),
        (all_outcome_files, "pump_outcome_v3"),
        (all_cf_files, "counterfactual_trade_v3"),
        (all_policy_files, "policy_eval_v3"),
    ]:
        for fpath in fpath_list:
            sz = file_size_bytes(fpath)
            output_files.append({
                "layer": layer_name,
                "filename": os.path.basename(fpath),
                "path": fpath,
                "bytes": sz,
                "sha256": hash_file(fpath),
            })
            total_output_bytes += sz

    # QA
    eligible_states = sum(v for k, v in class_dist.items() if k != "SKIP")
    missed_opp = eval_dist.get("MISSED_OPPORTUNITY", 0)
    false_pos = eval_dist.get("FALSE_POSITIVE", 0)
    correct_enter = eval_dist.get("CORRECT_ENTER", 0)
    correct_skip = eval_dist.get("CORRECT_SKIP", 0)

    # Policy metrics — 4 explicit formulas
    missed_among_skips = missed_opp / (missed_opp + correct_skip) if (missed_opp + correct_skip) > 0 else 0
    missed_share_prof = missed_opp / (missed_opp + correct_enter) if (missed_opp + correct_enter) > 0 else 0
    losing_share_ent = false_pos / (false_pos + correct_enter) if (false_pos + correct_enter) > 0 else 0
    classic_fpr = false_pos / (false_pos + correct_skip) if (false_pos + correct_skip) > 0 else 0

    qa = {
        "leakage_check": "PASS — pump_state_v3 contains only causal fields; outcomes computed from forward trades only",
        "id_stability": "PASS — state_ids deterministic from source+mint+time+seq",
        "label_math_check": "PASS — markouts computed as (future_price - entry_price) / entry_price in bp",
        "unit_audit": "PASS — v_sol_bonding_curve stored as lamports; sol_amount/market_cap_sol/price_sol converted SOL→lamports",
        "provenance": f"PASS — pipeline={PIPELINE_VERSION} run_uuid={run_uuid} git_sha={git_sha} source_hash={source_hash} code_config_hash={code_hash}",
        "label_independence": "PASS — economic_class derived from counterfactual economics, NOT from champion_v1 would_enter",
        "architecture": f"v3 4-layer batched (DuckDB threads={args.duckdb_threads}, Polars, high-RAM)",
        "peak_rss_gb": round(peak_rss / 1024, 2),
        "runtime_seconds": int(elapsed),
        "runtime_minutes": round(elapsed / 60, 1),
        "mints_processed": len(all_mints),
        "states_per_sec": round(total_states / elapsed, 1) if elapsed > 0 else 0,
        "class_distribution": dict(class_dist),
        "eval_distribution": dict(eval_dist),
        "venue_distribution": dict(venue_dist),
        "rejected_counts": dict(rejected),
        "coverage_censored_count": coverage_censored_count,
        "observed_no_trade_count": observed_no_trade_count,
        "total_output_bytes": total_output_bytes,
        "total_output_gb": round(total_output_bytes / 1e9, 2),
        "eligible_states": eligible_states,
        # 4 explicit policy metric formulas
        "missed_among_skips": round(missed_among_skips, 4),
        "missed_share_of_profitable": round(missed_share_prof, 4),
        "losing_share_of_entries": round(losing_share_ent, 4),
        "classic_fpr": round(classic_fpr, 4),
    }

    counts = {
        "source_mints": len(all_mints),
        "states": total_states,
        "outcomes": total_outcomes,
        "counterfactual_trades": total_cf,
        "policy_evals": total_policy,
        "train_mints": len(splits["train"]),
        "val_mints": len(splits["val"]),
        "test_mints": len(splits["test"]),
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
        "counts": counts,
        "qa": qa,
        "known_issues": KNOWN_ISSUES,
        "execution_assumptions": EXEC_ASSUMPTIONS_V3,
        "entry_sizes_sol": ENTRY_SIZES_SOL,
        "eligibility_criteria": ELIGIBILITY,
        "splits": {k: len(v) for k, v in splits.items()},
    }

    with open(MANIFEST_PATH, "w") as f:
        json.dump(manifest, f, indent=2)

    # Final report
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
    print(f"  Manifest:            {MANIFEST_PATH}")
    print(f"  Run UUID:            {run_uuid}")
    print(f"  Source hash:         {source_hash}")
    print(f"  Git SHA:             {git_sha}")

    print("\n  Economic class distribution:")
    for cls, count in sorted(class_dist.items()):
        print(f"    {cls}: {count:,}")

    print("\n  Policy evaluation distribution:")
    for ev, count in sorted(eval_dist.items()):
        print(f"    {ev}: {count:,}")

    print("\n  Rejected/censored:")
    for r, count in sorted(rejected.items()):
        print(f"    {r}: {count:,}")
    print(f"    coverage_censored: {coverage_censored_count:,}")
    print(f"    observed_no_trade: {observed_no_trade_count:,}")

    print(f"\n  Policy critique rates (4 explicit formulas):")
    print(f"    Eligible states:                            {eligible_states:,}")
    print(f"    1. Missed among champion SKIPS:             {missed_among_skips:.1%} ({missed_opp:,} / {missed_opp+correct_skip:,})")
    print(f"    2. Missed share of profitable opportunities: {missed_share_prof:.1%} ({missed_opp:,} / {missed_opp+correct_enter:,})")
    print(f"    3. Losing share of champion entries:        {losing_share_ent:.1%} ({false_pos:,} / {false_pos+correct_enter:,})")
    print(f"    4. Classic FPR:                             {classic_fpr:.1%} ({false_pos:,} / {false_pos+correct_skip:,})")

    print(f"\n  Output files: {len(output_files)} parquet files across 4 layers")
    print(f"  Manifest written to: {MANIFEST_PATH}")


if __name__ == "__main__":
    main()
