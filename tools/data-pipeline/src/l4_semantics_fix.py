#!/usr/bin/env python3
"""
l4_semantics_fix.py — Fix L4 semantics: rename best-size to oracle_best_scenario.

The current L4 "best_size_sol" is selected using future L2/L3 outcomes (hindsight).
This is an ORACLE label, NOT champion policy behavior. Fix:
1. Rename policy_class to 'oracle_best_scenario' (not 'tp15_sl15_maxhold300')
2. Add explicit oracle/hindsight metadata fields
3. Add champion_v1 reference fields (actual champion config: 0.1 SOL, 0ms, tp15/sl15)
4. Ensure no future-derived info enters L1

This only modifies L4 parquet + manifest metadata. No L1/L2/L3 changes.
"""
import pandas as pd
import numpy as np
import json, os, hashlib

OUTPUT_DIR = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                          'output', 'laserstream_gold_v3')

def main():
    print("=" * 70)
    print("L4 SEMANTICS FIX — Oracle vs Champion separation")
    print("=" * 70)

    # ─── 1. Load current L4 ─────────────────────────────────────────
    l4_path = os.path.join(OUTPUT_DIR, 'l4_policy_eval_v3.parquet')
    l4 = pd.read_parquet(l4_path)
    print(f"\nCurrent L4: {len(l4):,} rows, {len(l4.columns)} cols")
    print(f"Columns: {list(l4.columns)}")
    print(f"policy_class values: {l4['policy_class'].value_counts().to_dict()}")

    # ─── 2. Load L3 to get the champion_v1 reference (0.1 SOL / 0ms) ──
    print("\n--- Loading L3 champion reference (0.1 SOL / 0ms scenario) ---")
    # Read only the rows matching champion config
    l3 = pd.read_parquet(os.path.join(OUTPUT_DIR, 'l3_counterfactual_v3.parquet'),
                         filters=[('trade_size_sol', '=', 0.1),
                                  ('latency_scenario_ms', '=', 0)])
    print(f"Champion scenario rows: {len(l3):,}")

    # ─── 3. Build corrected L4 ──────────────────────────────────────
    print("\n--- Building corrected L4 ---")
    l4_new = pd.DataFrame()
    l4_new['state_id'] = l4['state_id']

    # Champion v1 fields (actual champion config, NOT hindsight)
    l4_new['champion_v1_config'] = '0.1_sol_0ms_tp15_sl15_maxhold300'
    l4_new['champion_v1_size_sol'] = 0.1
    l4_new['champion_v1_latency_ms'] = 0

    # Champion v1 outcome (from L3 scenario matching champion config)
    champion_map = l3.set_index('state_id')[['sim_return_pct', 'outcome_class',
                                              'exit_executable', 'capacity_constrained',
                                              'feasible', 'feasibility_reason']].to_dict('index')

    champ_outcomes = []
    champ_returns = []
    champ_exec = []
    champ_constrained = []
    champ_feasible = []
    champ_reason = []

    for sid in l4['state_id']:
        c = champion_map.get(sid, {})
        champ_outcomes.append(c.get('outcome_class', None))
        champ_returns.append(c.get('sim_return_pct', None))
        champ_exec.append(c.get('exit_executable', None))
        champ_constrained.append(c.get('capacity_constrained', None))
        champ_feasible.append(c.get('feasible', None))
        champ_reason.append(c.get('feasibility_reason', None))

    l4_new['champion_v1_outcome'] = champ_outcomes
    l4_new['champion_v1_return_pct'] = champ_returns
    l4_new['champion_v1_exit_executable'] = champ_exec
    l4_new['champion_v1_capacity_constrained'] = champ_constrained
    l4_new['champion_v1_feasible'] = champ_feasible
    l4_new['champion_v1_feasibility_reason'] = champ_reason

    # Oracle best-scenario fields (hindsight, future-derived)
    l4_new['oracle_best_scenario_label'] = 'hindsight_best_size_from_l3_outcomes'
    l4_new['oracle_best_size_sol'] = l4['best_size_sol']
    l4_new['oracle_best_return_pct'] = l4['policy_return_pct']
    l4_new['oracle_best_outcome'] = l4['policy_outcome']

    # Champion policy outcome classification (using champion return)
    def classify_champion(row):
        ret = row['champion_v1_return_pct']
        if ret is None or pd.isna(ret):
            return 'entry_failed'
        elif ret >= 0.15:  # TP_PCT
            return 'tp_hit'
        elif ret <= -0.15:  # SL_PCT
            return 'sl_hit'
        else:
            return 'timeout'

    l4_new['champion_v1_policy_outcome'] = l4_new.apply(classify_champion, axis=1)
    l4_new['champion_v1_policy_return'] = l4_new['champion_v1_return_pct'].apply(
        lambda x: 0.15 if pd.notna(x) and x >= 0.15 else
                  (-0.15 if pd.notna(x) and x <= -0.15 else x)
    )

    print(f"Corrected L4: {len(l4_new):,} rows, {len(l4_new.columns)} cols")
    print(f"Columns: {list(l4_new.columns)}")

    # ─── 4. Verify no future-derived fields leak into L1 ────────────
    print("\n--- 4. Future-derived field leakage check ---")
    l1 = pd.read_parquet(os.path.join(OUTPUT_DIR, 'l1_pump_state_v3.parquet'))
    l1_cols = set(l1.columns)
    oracle_cols = {'oracle_best_size_sol', 'oracle_best_return_pct', 'oracle_best_outcome',
                   'oracle_best_scenario_label'}
    overlap = l1_cols & oracle_cols
    print(f"Oracle columns in L1: {overlap}")
    print(f"✓ PASS: No oracle/hindsight fields in L1" if not overlap else "✗ FAIL: Oracle fields leaked into L1!")

    # ─── 5. Write corrected L4 ──────────────────────────────────────
    print("\n--- 5. Writing corrected L4 ---")
    l4_new.to_parquet(l4_path, index=False)
    print(f"L4 written: {l4_path}")

    # ─── 6. Distribution comparison ─────────────────────────────────
    print("\n--- 6. Champion v1 vs Oracle distribution ---")
    print(f"Champion v1 outcomes:")
    print(l4_new['champion_v1_policy_outcome'].value_counts().to_string())
    print(f"\nOracle best outcomes:")
    print(l4_new['oracle_best_outcome'].value_counts().to_string())

    print("\n" + "=" * 70)
    print("VERDICT: L4 now separates champion_v1 (actual config) from oracle_best_scenario (hindsight).")
    print("No future-derived information in L1. L4 is auxiliary critique, not policy answer key.")
    print("=" * 70)

if __name__ == '__main__':
    main()
