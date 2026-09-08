#!/usr/bin/env python3
"""rebuild_sft_cross_v2 — regenerate ONLY the cross-sectional SFT shard with the
supervision-geometry-corrected exporter, splice into qwen_sft_v2.jsonl.

CPT untouched. Eval (v1 + v1.1) untouched. All non-cross SFT tasks carried over
byte-identical from qwen_sft_v1.jsonl.

Deterministic: same dense-window scan, same panel assembly, mode split by
sha256(panel_key) % 100 (<20 -> full_rank_aux).
NO degenerate LaserStream single-candidate fill: if slinky supplies fewer than
the v1 target, we report the shortfall instead of padding (quality > quota).
"""
import hashlib
import json
import os
import sys
import time

SRC = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, SRC)

import build_qwen_curriculum_v1 as B          # noqa: E402
import cpt_sft_exporters as X                 # noqa: E402
import sft_cross_exporter_v2 as V2            # noqa: E402

OUT_DIR = os.path.join(B.BASE, 'qwen_curriculum_v1', 'sft')
V1_PATH = os.path.join(OUT_DIR, 'qwen_sft_v1.jsonl')
V2_PATH = os.path.join(OUT_DIR, 'qwen_sft_v2.jsonl')
MANIFEST_PATH = os.path.join(OUT_DIR, 'SFT_MANIFEST_V2.json')
REPORT_PATH = os.path.join(OUT_DIR, 'CROSS_V2_BUILD_REPORT.json')

N_CROSS_TARGET = 1050  # int(3000 * 0.35), same as v1


def load_eval_exclusions():
    excl_mints, excl_states = set(), set()
    for p in [os.path.join(B.EVAL_DIR, 'qwen_eval_v1.jsonl'),
              os.path.join(B.EVAL_DIR_V1_1, 'qwen_eval_v1_1.jsonl')]:
        if not os.path.exists(p):
            continue
        with open(p, 'r', encoding='utf-8') as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    rec = json.loads(line)
                except json.JSONDecodeError:
                    continue
                for c in rec.get('candidates', []):
                    if 'mint_id' in c:
                        excl_mints.add(c['mint_id'])
                    cs = c.get('causal_state', {}) or {}
                    if 'state_id' in cs:
                        excl_states.add(str(cs['state_id']))
                    if 'mint_id' in cs:
                        excl_mints.add(cs['mint_id'])
                prov = rec.get('provenance', {}) or {}
                for k, v in prov.items():
                    if 'mint' in k.lower() and isinstance(v, str):
                        excl_mints.add(v)
                    if 'state_id' in k.lower() and isinstance(v, str):
                        excl_states.add(str(v))
    return excl_mints, excl_states


def main():
    t0 = time.time()
    print("=== REBUILD sft_cross v2 (supervision-geometry fix) ===", flush=True)

    excl_mints, excl_states = load_eval_exclusions()
    print(f"  eval exclusion: {len(excl_mints)} mints, {len(excl_states)} state_ids", flush=True)

    train_lw_idx = B.build_train_filtered_lightweight_index(excl_mints)
    B.preload_slinky_outcomes()
    B.preload_slinky_counterfactuals()

    dense = B.find_dense_time_windows(train_lw_idx, min_candidates=B.SLINKY_PANEL_MIN,
                                      window_ms=60000, max_panels=N_CROSS_TARGET * 2)
    print(f"  dense windows: {len(dense)}", flush=True)

    new_cross, mode_counts = [], {'live_action': 0, 'full_rank_aux': 0}
    degenerate_dropped = 0
    overlength_dropped = 0
    SFT_SEQ_LEN = 12288          # trainer hard limit — overlength records are
    #                              silently unusable, so refuse them at build time
    for i in range(len(dense)):
        if len(new_cross) >= N_CROSS_TARGET:
            break
        anchor_ms, _, _ = dense[i]
        try:
            panel = B.assemble_slinky_panel(anchor_ms, i, lw_idx=train_lw_idx)
        except Exception as e:
            print(f"  panel {i} failed: {type(e).__name__}: {e}", flush=True)
            continue
        if not panel or len(panel.get('candidates', [])) < 3:
            continue
        # HARD GUARDS against the v1 degeneracy class
        cands = panel['candidates']
        empty_hash = hashlib.sha256(b'').hexdigest()[:16]
        ids = {c.get('mint_id') for c in cands}
        if len(ids) < max(3, len(cands) // 2) or empty_hash in ids:
            degenerate_dropped += 1
            continue
        nonempty_causal = sum(1 for c in cands if c.get('causal_state'))
        if nonempty_causal < len(cands) * 0.9:
            degenerate_dropped += 1
            continue
        # eval-mint belt & braces (panels are pre-filtered by construction)
        if any(c.get('mint_id') in excl_mints for c in cands):
            continue

        panel_key = f"panel_{i}"
        ex = V2.export_cross_v2(panel, panel_key)
        ex['token_count'] = X.estimate_tokens(json.dumps(ex))
        if ex['token_count'] > SFT_SEQ_LEN - 64:   # headroom for chat template
            overlength_dropped += 1
            continue
        ex['id'] = f"sft_cross_{len(new_cross):04d}"
        ex['source_corpus'] = 'slinky_gold_v3'
        ex['provenance'] = X.make_sft_provenance(
            'slinky_gold_v3', [panel_key], B.SLINKY_FREEZE_UUID,
            panel_id=panel_key, eval_split='train')
        ex['provenance']['cross_export_version'] = 'v2'
        mode_counts[ex['cross_mode']] += 1
        new_cross.append(ex)
        if len(new_cross) % 100 == 0:
            print(f"  built {len(new_cross)} panels ({time.time()-t0:.0f}s)", flush=True)

    print(f"  cross v2 built: {len(new_cross)} (target {N_CROSS_TARGET}, "
          f"degenerate dropped: {degenerate_dropped}, overlength dropped: "
          f"{overlength_dropped}) modes={mode_counts}", flush=True)

    # ── splice: carry over every non-cross v1 record byte-identical ──
    carried, dropped_old_cross = [], 0
    with open(V1_PATH, 'r', encoding='utf-8') as f:
        for line in f:
            line = line.rstrip('\n')
            if not line:
                continue
            rec = json.loads(line)
            rid = str(rec.get('id', ''))
            if rid.startswith('sft_cross'):
                dropped_old_cross += 1
                continue
            carried.append(line)

    with open(V2_PATH, 'w', encoding='utf-8', newline='\n') as f:
        for line in carried:
            f.write(line + '\n')
        for ex in new_cross:
            f.write(json.dumps(ex, ensure_ascii=False) + '\n')

    sha = hashlib.sha256(open(V2_PATH, 'rb').read()).hexdigest()

    # output-length stats (estimate; exact tokenizer percentiles come from the
    # WSL loss-token dry-run)
    la_lens = sorted(X.estimate_tokens(json.dumps(e['output'], ensure_ascii=False))
                     for e in new_cross if e['cross_mode'] == 'live_action')
    fr_lens = sorted(X.estimate_tokens(json.dumps(e['output'], ensure_ascii=False))
                     for e in new_cross if e['cross_mode'] == 'full_rank_aux')

    def pct(a, p):
        return a[min(len(a) - 1, int(len(a) * p))] if a else None

    report = {
        'built': len(new_cross), 'target': N_CROSS_TARGET,
        'degenerate_dropped': degenerate_dropped,
        'overlength_dropped': overlength_dropped,
        'dropped_old_cross_records': dropped_old_cross,
        'carried_non_cross_records': len(carried),
        'total_v2_records': len(carried) + len(new_cross),
        'mode_counts': mode_counts,
        'live_action_out_est_tokens': {'p50': pct(la_lens, .5), 'p90': pct(la_lens, .9),
                                       'p99': pct(la_lens, .99),
                                       'max': la_lens[-1] if la_lens else None},
        'full_rank_aux_out_est_tokens': {'p50': pct(fr_lens, .5), 'p90': pct(fr_lens, .9),
                                         'p99': pct(fr_lens, .99),
                                         'max': fr_lens[-1] if fr_lens else None},
        'sha256_qwen_sft_v2': sha,
        'elapsed_s': round(time.time() - t0, 1),
    }
    with open(REPORT_PATH, 'w', encoding='utf-8') as f:
        json.dump(report, f, indent=2)

    manifest = {
        'version': 'qwen_sft_v2',
        'parent_version': 'qwen_sft_v1',
        'change': 'cross-sectional shard rebuilt with sft_cross_exporter_v2 '
                  '(live_action ~80% / full_rank_aux ~20%); all other tasks '
                  'carried over byte-identical; no LS degenerate fill',
        'records': len(carried) + len(new_cross),
        'cross_records': len(new_cross),
        'mode_counts': mode_counts,
        'sha256': sha,
        'slinky_freeze_uuid': B.SLINKY_FREEZE_UUID,
        'built_unix': int(time.time()),
    }
    with open(MANIFEST_PATH, 'w', encoding='utf-8') as f:
        json.dump(manifest, f, indent=2)

    print(json.dumps(report, indent=2), flush=True)
    print("DONE", flush=True)


if __name__ == '__main__':
    main()
