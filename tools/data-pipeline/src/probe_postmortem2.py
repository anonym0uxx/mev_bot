#!/usr/bin/env python
"""Probe stage 2: replicate the main build's postmortem loop WITHOUT the bare except."""
import sys, json, traceback
import build_qwen_curriculum_v1 as B
import cpt_sft_exporters as E

B.build_slinky_lightweight_index()
B.preload_slinky_outcomes()
B.preload_slinky_counterfactuals()
train_lw = B.build_train_filtered_lightweight_index(set())
wins = B.find_dense_time_windows(train_lw, min_candidates=10, window_ms=60000, max_panels=30)

built = 0
for i, (anchor_ms, lo, hi) in enumerate(wins):
    if built >= 3:
        break
    panel = B.assemble_slinky_panel(anchor_ms, i, lw_idx=train_lw)
    if not panel or len(panel.get('candidates', [])) < 5:
        continue
    pairs = B.find_divergent_pairs(panel)
    print(f"panel {i}: {len(panel['candidates'])} cands, {len(pairs)} pairs")
    for pair in pairs[:2]:
        idx_a = pair.get('candidate_a_idx'); idx_b = pair.get('candidate_b_idx')
        cands_a = [c for c in panel['candidates'] if c.get('panel_rank') == idx_a]
        cands_b = [c for c in panel['candidates'] if c.get('panel_rank') == idx_b]
        print(f"  pair idx_a={idx_a} idx_b={idx_b} -> match_a={len(cands_a)} match_b={len(cands_b)}")
        if not cands_a or not cands_b:
            ranks = sorted([c.get('panel_rank') for c in panel['candidates']])[:10]
            print(f"    NO MATCH. first panel_ranks present: {ranks}")
            continue
        mint_id_a = cands_a[0].get('mint_id',''); mint_id_b = cands_b[0].get('mint_id','')
        od_a = B._SLINKY_OUTCOMES_BY_MINT.get(mint_id_a, {})
        od_b = B._SLINKY_OUTCOMES_BY_MINT.get(mint_id_b, {})
        try:
            ex = E.sft_postmortem_example(pair, cands_a, cands_b, od_a, od_b, f"postmortem_{built}")
            ex['token_count'] = E.estimate_tokens(json.dumps(ex))
            print(f"    OK: built example, {ex['token_count']} tokens")
            built += 1
        except Exception:
            print("    EXCEPTION in sft_postmortem_example:")
            traceback.print_exc()
            built += 1  # don't loop forever on the same failure
print(f"built: {built}")
