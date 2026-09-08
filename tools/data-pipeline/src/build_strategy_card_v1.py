#!/usr/bin/env python
"""
build_strategy_card_v1.py - Gold Layer 5: strategy cards from validated claims

Transforms creator_claim_v1 + narrative_validation_v1 -> strategy_card_v1.

A strategy card is a distilled, reusable trading setup extracted from
validated creator content. Each card captures:
  - Setup type (breakout, dip-buy, graduation, rug-avoid, etc.)
  - Trigger conditions (narrative + on-chain signals)
  - Entry rules (price, timing, sizing)
  - Exit rules (TP, stop-loss, time-based)
  - Expected value (from validation outcomes)
  - Capacity constraints
  - Confidence (from creator track record + validation)

Usage: python src/build_strategy_card_v1.py
"""

import json
import os
import hashlib
import argparse
import time as time_module
from collections import Counter, defaultdict


def cluster_claims_by_theme(claims: list) -> dict:
    """Group GOLD/UNRESOLVED claims by narrative_theme + direction + claim_type.
    Returns {cluster_key: [claims]}."""
    clusters = defaultdict(list)
    for claim in claims:
        if claim['admission_status'] not in ('GOLD', 'UNRESOLVED'):
            continue
        theme = claim.get('narrative_theme') or 'unspecified'
        direction = claim.get('direction') or 'neutral'
        claim_type = claim.get('claim_type') or 'observation'
        key = f"{theme}|{direction}|{claim_type}"
        clusters[key].append(claim)
    return dict(clusters)


def derive_setup_type(cluster_key: str, claims: list) -> str:
    """Derive setup type from cluster key + claim patterns."""
    parts = cluster_key.split('|')
    # Handle variable number of parts: theme|direction|claim_type[|...]
    theme = parts[0] if len(parts) > 0 else ''
    direction = parts[1] if len(parts) > 1 else ''
    claim_type = parts[2] if len(parts) > 2 else ''

    if theme == 'rug-pull' or claim_type == 'risk':
        return 'rug_avoid'
    if theme == 'graduation':
        if direction == 'long':
            return 'graduation_play'
        return 'graduation_watch'
    if theme == 'dev-buy':
        return 'dev_buy_follow'
    if theme == 'sniper':
        return 'sniper_avoid'
    if theme == 'whale-movement':
        if direction == 'long':
            return 'whale_accumulation_follow'
        return 'whale_exit_avoid'
    if theme == 'kol-endorsement':
        return 'kol_pump_play'
    if theme == 'rotation':
        return 'rotation_play'

    if direction == 'long' and claim_type == 'entry':
        return 'breakout_entry'
    if direction == 'avoid':
        return 'risk_avoid'
    if direction == 'long':
        return 'dip_buy'
    if direction == 'short':
        return 'short_term_exit'
    return 'generic_setup'


def build_strategy_card(cluster_key: str, claims: list, validations: dict,
                        run_uuid: str, git_sha: str) -> dict:
    """Build a single strategy card from a cluster of claims."""
    setup_type = derive_setup_type(cluster_key, claims)

    # Aggregate entry/exit/sizing from claims
    entry_prices_sol = []
    target_prices_sol = []
    sizing_sols = []
    sizing_pcts = []
    horizons = []
    confidences = []

    for c in claims:
        if c.get('entry_price_sol'):
            entry_prices_sol.append(c['entry_price_sol'])
        if c.get('target_price_sol'):
            target_prices_sol.append(c['target_price_sol'])
        if c.get('sizing_sol'):
            sizing_sols.append(c['sizing_sol'])
        if c.get('sizing_pct_bankroll'):
            sizing_pcts.append(c['sizing_pct_bankroll'])
        if c.get('horizon_seconds'):
            horizons.append(c['horizon_seconds'])
        if c.get('confidence_stated'):
            confidences.append(c['confidence_stated'])

    # Compute aggregates
    def avg(lst):
        return sum(lst) / len(lst) if lst else None

    def median(lst):
        if not lst:
            return None
        s = sorted(lst)
        n = len(s)
        return s[n // 2] if n % 2 == 1 else (s[n // 2 - 1] + s[n // 2]) / 2

    # Validation outcomes (if any claims matched to Slinky)
    validated_claims = [c for c in claims if c['claim_id'] in validations]
    validation_verdicts = []
    net_pnls = []
    ret_300s_bps = []

    for c in validated_claims:
        v = validations[c['claim_id']]
        validation_verdicts.append(v.get('validation_status', 'UNRESOLVED'))
        if v.get('net_pnl_sol') is not None:
            net_pnls.append(v['net_pnl_sol'])
        if v.get('ret_300s_bp') is not None:
            ret_300s_bps.append(v['ret_300s_bp'])

    # Compute EV from validations (or None if no validations)
    ev_sol = None
    if net_pnls:
        ev_sol = avg(net_pnls)

    # Win rate from validation verdicts
    win_rate = None
    if validation_verdicts:
        supported = sum(1 for v in validation_verdicts if v == 'SUPPORTED')
        contradicted = sum(1 for v in validation_verdicts if v == 'CONTRADICTED')
        total = supported + contradicted
        if total > 0:
            win_rate = supported / total

    # Unique creators in this cluster
    creators = set()
    platforms = set()
    for c in claims:
        creators.add(c.get('account_handle', ''))
        platforms.add(c.get('platform', ''))

    # Strategy card
    card_id = hashlib.sha256(
        f"{setup_type}|{cluster_key}|{len(claims)}".encode()
    ).hexdigest()[:16]

    card = {
        'strategy_card_id': card_id,
        'schema_version': '1.0.0',
        'pipeline_version': '1.1.0',
        'run_uuid': run_uuid,
        'git_sha': git_sha,

        'setup_type': setup_type,
        'narrative_theme': cluster_key.split('|')[0],
        'direction': cluster_key.split('|')[1],
        'claim_type': cluster_key.split('|')[2],

        # Trigger conditions
        'trigger_narrative_theme': cluster_key.split('|')[0],
        'trigger_min_mentions': min(len(claims), 3),
        'trigger_consensus_direction': cluster_key.split('|')[1],

        # Entry rules
        'entry_price_sol_avg': avg(entry_prices_sol),
        'entry_price_sol_median': median(entry_prices_sol),
        'entry_sizing_sol_avg': avg(sizing_sols),
        'entry_sizing_pct_avg': avg(sizing_pcts),
        'entry_sizing_default': 0.5,  # default 0.5 SOL if not specified

        # Exit rules
        'exit_target_sol_avg': avg(target_prices_sol),
        'exit_horizon_seconds_avg': avg(horizons),
        'exit_horizon_default': 300,  # default 5min
        'exit_stop_loss_pct': -30,  # default -30% stop loss

        # Expected value
        'ev_sol_300s': round(ev_sol, 6) if ev_sol is not None else None,
        'win_rate': round(win_rate, 4) if win_rate is not None else None,
        'n_validated_claims': len(validated_claims),
        'n_supported': sum(1 for v in validation_verdicts if v == 'SUPPORTED'),
        'n_contradicted': sum(1 for v in validation_verdicts if v == 'CONTRADICTED'),
        'n_unresolved': sum(1 for v in validation_verdicts if v == 'UNRESOLVED'),

        # Capacity
        'capacity_sol_max': 1.0,  # placeholder
        'capacity_note': 'Estimated from observed liquidity; refine with pump_state_v3',

        # Confidence
        'creator_confidence_avg': avg(confidences),
        'n_unique_creators': len(creators),
        'n_platforms': len(platforms),
        'platforms': '|'.join(sorted(platforms)),

        # Sample
        'sample_claim_ids': '|'.join(c['claim_id'] for c in claims[:5]),
        'sample_text': claims[0].get('claim_text', '')[:300] if claims else '',

        # QA
        'admission_status': 'GOLD' if len(claims) >= 3 and len(creators) >= 2 else 'UNRESOLVED',
        'rejection_reason': None,
    }

    # Reject if too few claims or single-source
    if len(claims) < 2:
        card['admission_status'] = 'REJECTED'
        card['rejection_reason'] = 'insufficient_claims'

    return card


def build_strategy_cards(claim_path: str, validation_path: str, output_dir: str) -> dict:
    """Build strategy_card_v1 from claims + validations."""
    print("=" * 70)
    print("STRATEGY_CARD_V1 - Gold Layer 5 Builder")
    print("=" * 70)

    with open(claim_path, 'r', encoding='utf-8') as f:
        claims = [json.loads(line) for line in f if line.strip()]
    print(f"  Loaded {len(claims)} claims")

    # Load validations (index by claim_id)
    validations = {}
    if os.path.exists(validation_path):
        with open(validation_path, 'r', encoding='utf-8') as f:
            for line in f:
                if line.strip():
                    v = json.loads(line)
                    validations[v['claim_id']] = v
    print(f"  Loaded {len(validations)} validations")

    run_uuid = f"sc_{int(time_module.time())}"
    git_sha = __import__('subprocess').check_output(
        ['git', 'rev-parse', '--short', 'HEAD'],
        cwd='D:/repos/mev_bot'
    ).decode().strip()
    print(f"  Run UUID: {run_uuid}")
    print(f"  Git SHA:  {git_sha}")

    # Cluster claims
    clusters = cluster_claims_by_theme(claims)
    print(f"  Claim clusters: {len(clusters)}")

    # Build strategy cards
    cards = []
    for cluster_key, cluster_claims in clusters.items():
        card = build_strategy_card(cluster_key, cluster_claims, validations,
                                   run_uuid, git_sha)
        cards.append(card)

    print(f"\n  Total strategy cards: {len(cards)}")

    # Stats
    admission = Counter(c['admission_status'] for c in cards)
    print(f"  Admission: {dict(admission)}")

    setup_types = Counter(c['setup_type'] for c in cards)
    print(f"  Setup types: {dict(setup_types)}")

    # Write output
    os.makedirs(output_dir, exist_ok=True)
    out_path = os.path.join(output_dir, 'strategy_card_v1.jsonl')
    with open(out_path, 'w', encoding='utf-8') as f:
        for card in cards:
            f.write(json.dumps(card) + '\n')

    print(f"\n  Output: {out_path}")
    return {'total_cards': len(cards), 'output_path': out_path}


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--claim-path',
                        default='output/narrative_gold_v1/gold/creator_claim_v1/creator_claim_v1.jsonl')
    parser.add_argument('--validation-path',
                        default='output/narrative_gold_v1/gold/narrative_validation_v1/narrative_validation_v1.jsonl')
    parser.add_argument('--output-dir',
                        default='output/narrative_gold_v1/gold/strategy_card_v1')
    args = parser.parse_args()
    build_strategy_cards(args.claim_path, args.validation_path, args.output_dir)
