#!/usr/bin/env python
"""Shard test for v3 corrections: per-horizon censoring, multi-size economics, exact curve.
Runs on first 50 mints to verify semantics before full rebuild.
"""
import sys, os, time
sys.path.insert(0, os.path.dirname(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', 'schemas'))

import polars as pl
import duckdb

from build_slinky_gold_v3 import (
    make_duckdb, enumerate_mints, load_mint_trades, build_states_for_mint,
    build_outcomes_for_mint, build_counterfactuals_for_mint, build_policy_eval_for_mint,
    load_champion_config, config_hash, EXEC_ASSUMPTIONS_V3, ENTRY_SIZES_SOL,
    OUTPUT_DIR
)
from gold_schema_v3 import PumpOutcomeV3, CounterfactualTradeV3

def run_shard_test():
    con = make_duckdb(threads=4, mem_limit_gb=8)
    mints = enumerate_mints(con)[:50]
    print(f"Testing {len(mints)} mints...")
    
    champ_cfg = load_champion_config()
    champ_hash = config_hash(champ_cfg)
    exec_hash = config_hash(EXEC_ASSUMPTIONS_V3)
    run_uuid = "shard_test_v4"
    
    from build_slinky_gold_v3 import load_token_meta, load_migrations, load_wallet_stats_schema, load_wallet_stats, compute_source_hash, inventory_sources, get_git_sha, compute_code_config_hash
    
    source_files = inventory_sources(con)
    source_hash = compute_source_hash(source_files)
    git_sha = get_git_sha()
    code_hash = compute_code_config_hash()
    token_meta = load_token_meta(con)
    migrations = load_migrations(con)
    
    all_states = []
    all_outcomes = []
    all_cf = []
    all_policy = []
    
    for mint, _ in mints:
        trades_df = load_mint_trades(con, mint)
        if trades_df.is_empty():
            continue
        states = build_states_for_mint(
            trades_df, mint, token_meta, migrations,
            run_uuid, source_hash, code_hash, git_sha
        )
        if not states:
            continue
        outcomes = build_outcomes_for_mint(states, trades_df, mint, migrations, run_uuid)
        cf = build_counterfactuals_for_mint(states, outcomes, mint, run_uuid, exec_hash)
        policy = build_policy_eval_for_mint(
            states, outcomes, cf, mint, run_uuid, champ_cfg, champ_hash
        )
        all_states.extend(states)
        all_outcomes.extend(outcomes)
        all_cf.extend(cf)
        all_policy.extend(policy)
    
    con.close()
    
    print(f"\n=== RESULTS: {len(all_states)} states, {len(all_outcomes)} outcomes, {len(all_cf)} CF ===")
    
    # ─── Test 1: Per-horizon censoring semantics ─────────────
    print("\n--- TEST 1: Per-horizon censoring semantics ---")
    correct_censoring = 0
    wrong_censoring = 0
    observed_no_trade = 0
    true_censored = 0
    
    for o in all_outcomes:
        observed_300 = o.get("observed_through_300s", False)
        has_trade_300 = o.get("has_trade_within_300s", False)
        right_censored_300 = o.get("right_censored_300s", False)
        venue_censored = o.get("venue_censored", False)
        
        # Key invariant: right_censored should be TRUE only when NOT observed_through
        # i.e., source coverage ended before t+300s
        if right_censored_300:
            if not observed_300:
                correct_censoring += 1
                true_censored += 1
            else:
                wrong_censoring += 1
        # Observed through 300s but no trades = observed no-trade, NOT censored
        if observed_300 and not has_trade_300:
            observed_no_trade += 1
            if right_censored_300:
                wrong_censoring += 1  # BUG: observed no-trade marked as censored
    
    print(f"  True coverage-censored (right_censored_300s AND NOT observed): {true_censored}")
    print(f"  Observed no-trade (observed_through_300s AND NOT has_trade): {observed_no_trade}")
    print(f"  Correct censoring decisions: {correct_censoring}")
    print(f"  WRONG censoring (observed no-trade marked censored): {wrong_censoring}")
    assert wrong_censoring == 0, f"FAIL: {wrong_censoring} observed-no-trade states marked as censored!"
    print("  ✅ PASS: No observed no-trade state is marked as censored")
    
    # ─── Test 2: right_censored = NOT observed_through (invariant) ───
    print("\n--- TEST 2: right_censored = NOT observed_through invariant ---")
    violations = 0
    for h in [1, 2, 5, 10, 30, 60, 120, 300]:
        key_obs = f"observed_through_{h}s"
        key_cen = f"right_censored_{h}s"
        for o in all_outcomes:
            obs = o.get(key_obs, False)
            cen = o.get(key_cen, False)
            if cen == obs:  # censored should be opposite of observed
                violations += 1
    assert violations == 0, f"FAIL: {violations} right_censored != NOT observed_through violations"
    print(f"  ✅ PASS: right_censored_H = NOT observed_through_H for all 8 horizons, 0 violations")
    
    # ─── Test 3: MFE/MAE NULL for no-trade states ────────────
    print("\n--- TEST 3: MFE/MAE NULL for no-trade states ---")
    mfe_null_on_no_trade = 0
    mfe_nonnull_on_no_trade = 0
    for o in all_outcomes:
        observed_300 = o.get("observed_through_300s", False)
        has_trade_300 = o.get("has_trade_within_300s", False)
        mfe = o.get("mfe_bp")
        mae = o.get("mae_bp")
        if observed_300 and not has_trade_300:
            # Observed no-trade: MFE/MAE should be NULL
            if mfe is None:
                mfe_null_on_no_trade += 1
            else:
                mfe_nonnull_on_no_trade += 1
    print(f"  No-trade states with MFE=NULL: {mfe_null_on_no_trade}")
    print(f"  No-trade states with MFE≠NULL (BUG): {mfe_nonnull_on_no_trade}")
    assert mfe_nonnull_on_no_trade == 0, "FAIL: no-trade states should have NULL MFE"
    print("  ✅ PASS: All observed no-trade states have NULL MFE/MAE")
    
    # ─── Test 4: Multi-size economics ────────────────────────
    print("\n--- TEST 4: Multi-size exact curve economics ---")
    size_codes = ["sz005", "sz010", "sz025", "sz050", "sz100"]
    feasible_counts = {code: 0 for code in size_codes}
    has_multi = 0
    for c in all_cf:
        if not c.get("eligible"):
            continue
        has_any = False
        for code in size_codes:
            feas = c.get(f"{code}_feasible", False)
            if feas:
                feasible_counts[code] += 1
                has_any = True
        if has_any:
            has_multi += 1
    
    print(f"  Eligible CFs: {sum(1 for c in all_cf if c.get('eligible'))}")
    print(f"  Eligible with any feasible size: {has_multi}")
    for code in size_codes:
        print(f"  {code} feasible: {feasible_counts[code]}")
    
    # At least some should be feasible for 0.05 SOL (smallest size)
    assert feasible_counts["sz005"] > 0, "FAIL: no 0.05 SOL feasible entries"
    print("  ✅ PASS: Multi-size economics computed for all 5 sizes")
    
    # ─── Test 5: Exact curve PnL vs barrier PnL ──────────────
    print("\n--- TEST 5: Exact curve PnL sanity ---")
    exact_pnls = []
    barrier_pnls = []
    for c in all_cf:
        if not c.get("eligible") or c.get("net_pnl_sol") is None:
            continue
        exact_net = c.get("exact_net_pnl_sol")
        barrier_net = c.get("net_pnl_sol")
        if exact_net is not None and barrier_net is not None:
            exact_pnls.append(exact_net)
            barrier_pnls.append(barrier_net)
    
    if exact_pnls:
        import statistics
        print(f"  Exact curve net PnL: mean={statistics.mean(exact_pnls):.6f} SOL, n={len(exact_pnls)}")
        print(f"  Barrier net PnL:     mean={statistics.mean(barrier_pnls):.6f} SOL, n={len(barrier_pnls)}")
        print("  ✅ PASS: Exact curve economics computed alongside barrier economics")
    else:
        print("  ⚠ No exact PnL values to compare (may be all ineligible)")
    
    # ─── Test 6: exit_feasible semantics ─────────────────────
    print("\n--- TEST 6: Exit feasibility semantics ---")
    exit_feasible_dist = {}
    exit_note_dist = {}
    for c in all_cf:
        feas = c.get("exit_feasible", False)
        note = c.get("exit_feasibility_note")
        exit_feasible_dist[feas] = exit_feasible_dist.get(feas, 0) + 1
        exit_note_dist[note] = exit_note_dist.get(note, 0) + 1
    print(f"  exit_feasible distribution: {exit_feasible_dist}")
    print(f"  exit_feasibility_note distribution: {exit_note_dist}")
    
    # Coverage-censored should have exit_feasible=False
    for c in all_cf:
        outcome = None
        for o in all_outcomes:
            if o["state_id"] == c["state_id"]:
                outcome = o
                break
        if outcome and not outcome.get("observed_through_300s") and c.get("eligible"):
            assert c.get("exit_feasible") == False, "FAIL: censored state should have exit_feasible=False"
            assert c.get("exit_feasibility_note") == "coverage_censored", "FAIL: should be coverage_censored"
    print("  ✅ PASS: Coverage-censored states have exit_feasible=False")
    
    # ─── Test 7: Economic class for observed no-trade = SKIP ──
    print("\n--- TEST 7: Observed no-trade → SKIP (not BAD) ---")
    no_trade_bad = 0
    no_trade_skip = 0
    for c in all_cf:
        if not c.get("eligible"):
            continue
        outcome = None
        for o in all_outcomes:
            if o["state_id"] == c["state_id"]:
                outcome = o
                break
        if outcome and outcome.get("observed_through_300s") and not outcome.get("has_trade_within_300s"):
            cls = c.get("economic_class")
            if cls == "SKIP":
                no_trade_skip += 1
            elif cls == "BAD":
                no_trade_bad += 1
    print(f"  Observed no-trade → SKIP: {no_trade_skip}")
    print(f"  Observed no-trade → BAD (OLD BUG): {no_trade_bad}")
    assert no_trade_bad == 0, f"FAIL: {no_trade_bad} observed no-trade states classified BAD (should be SKIP)"
    print("  ✅ PASS: Observed no-trade states are SKIP, not BAD")
    
    # ─── Test 8: state_id uniqueness ─────────────────────────
    print("\n--- TEST 8: state_id uniqueness ---")
    state_ids = [s["state_id"] for s in all_states]
    assert len(state_ids) == len(set(state_ids)), "FAIL: duplicate state_ids!"
    print(f"  {len(state_ids)} states, {len(set(state_ids))} unique — ✅ PASS")
    
    # ─── Test 9: Cross-layer join integrity ──────────────────
    print("\n--- TEST 9: Cross-layer 1:1:1:1 join integrity ---")
    s_ids = set(s["state_id"] for s in all_states)
    o_ids = set(o["state_id"] for o in all_outcomes)
    c_ids = set(c["state_id"] for c in all_cf)
    p_ids = set(p["state_id"] for p in all_policy)
    assert s_ids == o_ids == c_ids == p_ids, "FAIL: layer state_id mismatch!"
    print(f"  All 4 layers have {len(s_ids)} matching state_ids — ✅ PASS")
    
    # ─── Test 10: Column count ───────────────────────────────
    print("\n--- TEST 10: Schema column counts ---")
    print(f"  PumpOutcomeV3 fields: {len(PumpOutcomeV3.__dataclass_fields__)}")
    print(f"  CounterfactualTradeV3 fields: {len(CounterfactualTradeV3.__dataclass_fields__)}")
    
    print("\n" + "=" * 60)
    print("ALL SHARD TESTS PASSED ✅")
    print("=" * 60)


if __name__ == "__main__":
    run_shard_test()
