#!/usr/bin/env python3
"""Render the Stage 1 capability/gap documents from the census JSON.

Derived, not hand-written: every number here traces to
SOURCE_RIGHTS_COVERAGE.json. Run after source_inventory.py.

  python3 render_capability_report.py --census /training/v2/reports/SOURCE_RIGHTS_COVERAGE.json \
      --out /training/v2/reports
"""
import argparse
import json
from pathlib import Path


def pct(n, d):
    return f"{100.0 * n / d:.1f}%" if d else "n/a"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--census', type=Path, default=Path('/training/v2/reports/SOURCE_RIGHTS_COVERAGE.json'))
    ap.add_argument('--out', type=Path, default=Path('/training/v2/reports'))
    a = ap.parse_args()
    c = json.loads(a.census.read_text())
    L, G, R, C, P = (c['sources']['labels'], c['sources']['economics'],
                     c['sources']['roundtrips'], c['sources']['context'], c['sources']['prices'])
    total = L['examples_total']

    cap = []
    cap.append('# North Star v2 — Capability Coverage\n')
    cap.append('Derived from `SOURCE_RIGHTS_COVERAGE.json` (Stage 1 census). '
               'Existence of a column is not coverage; these are measured counts.\n')

    cap.append('## Measured population\n')
    cap.append(f'- Label examples: **{total:,}** (admitted {L["admitted"]["values"].get("1", 0):,} / '
               f'not admitted {L["admitted"]["values"].get("0", 0):,})')
    cap.append(f'- Splits: {L["split"]["values"]}')
    cap.append(f'- Action labels: {L["action_label"]["values"]}')
    cap.append(f'- Group outcome: {L["group_outcome"]["values"]}')
    cap.append(f'- Outcome label: {L["outcome_label"]["values"]}\n')

    cap.append('## The negative population (WATCH/SKIP/no-entry)\n')
    npc = L['action_label']['values'].get('no_position_change', 0)
    cap.append(f'- `no_position_change` rows: **{npc:,}** of {total:,} = **{pct(npc, total)}**')
    cap.append('- Interpretation: the corpus is action-conditioned. A row exists because a tracked '
               'wallet traded, so absent-action states are essentially unrepresented. The model '
               'cannot learn or be scored on *not* entering from this population.\n')

    cap.append('## Outcome coverage by column\n')
    cov = L['outcome_column_coverage']
    for col in ('realized_lamports', 'rt', 'episode_return_bp', 'mfe_bp', 'mae_bp', 'exit_quality',
                'h30s_bp', 'h5m_bp', 'h30m_bp', 'h1h_bp'):
        cap.append(f'- `{col}`: {cov.get(col) or 0:,} / {total:,} = {pct(cov.get(col) or 0, total)}')
    cap.append('- Horizon coverage decays with horizon length; the longest horizons are the thinnest, '
               'which is exactly where exit-policy supervision would be needed.\n')

    cap.append('## Source-declared gate status\n')
    cap.append('`labels.sqlite`:')
    for g in L['label_gate']:
        cap.append(f'- {"PASS" if g["satisfied"] else "**FAIL**"} `{g["gate"]}` — {g["detail"]}')
    cap.append('\n`economics.sqlite`:')
    for g in G['label_gate']:
        cap.append(f'- {"PASS" if g["satisfied"] else "**FAIL**"} `{g["gate"]}` — {g["detail"]}')
    cap.append('')

    cap.append('## Execution evidence\n')
    cap.append(f'- `executable` rows: {C["executable"]:,}; lifecycle {C["lifecycle"]["values"]}')
    cap.append(f'- errors: {C["err"]["values"]}; priority-fee coverage {C["priority_fee_coverage"]:,}')
    cap.append('- This is evidence of *observed* transactions (fees, compute, curve vs AMM). It is '
               'not a modeled payoff for an action we did not take.\n')

    cap.append('## Price observations\n')
    cap.append(f'- Points: {P["price_points"]:,}; distinct mints: {P["distinct_mints"]:,}; '
               f'wallets: {P["distinct_wallets"]:,}')
    cap.append(f'- Points per mint (min/avg/max): {P["points_per_mint"]}')
    cap.append(f'- Semantic: {P["price_semantics"]}')
    cap.append('- Consequence: mints with a single observation have no path, so no return is computable '
               'for them regardless of which horizon column is requested.\n')

    cap.append('## Verdict\n')
    cap.append('A **normative policy corpus cannot be built from these sources as they stand**. '
               'The blocking facts are measured above: no untraded opportunity population, roughly a '
               'third of outcomes unknown, decaying horizon coverage, observed-only execution '
               'evidence, and unverified protected-holdout custody.\n')
    cap.append('**Buildable now, honestly labeled:** a decision-first *imitation* corpus plus a '
               'mechanics/observation corpus and a reconciled-episode review corpus. These are '
               'genuine improvements over v1 and can be trained, but they are not a policy and must '
               'not be reported as one.\n')
    (a.out / 'CAPABILITY_COVERAGE.md').write_text('\n'.join(cap) + '\n', encoding='utf-8')

    gaps = []
    gaps.append('# North Star v2 — Acquisition Gaps\n')
    gaps.append('What must be obtained before a normative policy corpus can be built and scored. '
               'Each item is a blocker, not a preference.\n')
    gaps.append('## 1. Untraded opportunity population (BLOCKER)\n')
    gaps.append(f'Current negative population is {npc:,} rows ({pct(npc, total)}). Required: a '
               'predeclared, live-reproducible decision clock sampled from the as-of discoverable '
               'universe independently of later action or outcome, so WATCH/SKIP/WAIT and failed '
               'candidates are represented. Without it, entry *judgment* is untrainable and any '
               'accuracy figure is inflated by action-conditioned sampling.\n')
    gaps.append('## 2. Verified protected-holdout custody (BLOCKER)\n')
    gaps.append('Source gate `historical_exposure_controlled` is FAIL: CPT/SFT/teacher/retrieval '
               'exposure and holdout custody are unverified. Until custody is proven, evaluation of '
               'any new corpus is circular — training data may already contain the test items.\n')
    gaps.append('## 3. Outcome completeness (BLOCKER for outcome-graded targets)\n')
    gaps.append(f'`outcomes_evidence_supported` FAIL with {G["outcome_status"]["values"].get("unknown", 0):,} '
               f'economics unknowns and {L["group_outcome"]["values"].get("indeterminate_transfer", 0):,} '
               'indeterminate-transfer groups. Outcome-graded labels must not be derived from these '
               'until resolution or explicit censoring treatment.\n')
    gaps.append('## 4. Executable price/route reconstruction (REQUIRED for action values)\n')
    gaps.append('Modeled alternative-action payoffs need reconciled curve/AMM rules, contemporaneous '
               'reserves, integer rounding, fees/tips without double charging, observation-to-order '
               'latency, and failure modes. Observed transaction rows do not supply this.\n')
    gaps.append('## 5. Rights and licence evidence (REQUIRED before inclusion)\n')
    gaps.append('Per the standing data standard, every external source needs permissive rights '
               'recorded before it enters training. The census records paths and counts; it does not '
               'certify rights, and no source here should be included on the strength of this report.\n')
    gaps.append('## 6. Narrative sources (REQUIRED for the narrative channel)\n')
    gaps.append('No narrative/social corpus was located in this census. Narrative reasoning targets '
               'need a licensed, timestamped source with availability proven at the decision cutoff.\n')
    (a.out / 'ACQUISITION_GAPS.md').write_text('\n'.join(gaps) + '\n', encoding='utf-8')

    print(json.dumps({'capability': str(a.out / 'CAPABILITY_COVERAGE.md'),
                      'gaps': str(a.out / 'ACQUISITION_GAPS.md')}, indent=2))


if __name__ == '__main__':
    main()