#!/usr/bin/env python
"""
build_narrative_state_v1.py - Gold Layer 3: causal narrative state at time t

Transforms creator_content_v1 + creator_claim_v1 -> narrative_state_v1.

For each mint mentioned in content, computes causal narrative features
at multiple time points t. No future information leaks.

Features:
  - Mention velocity (1h, 6h, 24h windows)
  - Independent creator breadth (unique non-echo creators)
  - Platform diversity
  - Echo ratio / amplification
  - Consensus direction + strength
  - Novelty / saturation scores
  - Alpha call counts
  - Reliability-weighted scores
  - Coordinated shill / crowding detection
  - Control group matching (untreated mints)

Usage: python src/build_narrative_state_v1.py
"""

import json
import os
import re
import hashlib
import argparse
import time as time_module
from collections import Counter, defaultdict
from datetime import datetime

# Reuse entity extraction from claim builder
from build_creator_claim_v1 import (
    PUMP_MINT_RE, SOLANA_ADDR_RE, CASHTAG_RE,
    extract_direction, extract_narrative_themes
)


def extract_all_mints(text: str) -> list:
    """Extract all Solana mint addresses from text."""
    mints = PUMP_MINT_RE.findall(text)
    if not mints:
        # Also check for general Solana addresses (non-pump)
        mints = [a for a in SOLANA_ADDR_RE.findall(text) if len(a) >= 32]
    return list(set(mints))  # dedupe


def load_content_and_claims(content_path: str, claim_path: str) -> tuple:
    """Load content and claims, return as sorted lists."""
    with open(content_path, 'r', encoding='utf-8') as f:
        content = [json.loads(line) for line in f if line.strip()]
    with open(claim_path, 'r', encoding='utf-8') as f:
        claims = [json.loads(line) for line in f if line.strip()]
    # Sort by publish time
    content.sort(key=lambda x: x['publish_time_ms'])
    claims.sort(key=lambda x: x['publish_time_ms'])
    return content, claims


def build_mint_timeline(content: list, claims: list) -> dict:
    """Build a timeline of events per mint.
    Returns {mint: [{'time_ms': int, 'content': dict, 'claims': [...]}]}"""
    timeline = defaultdict(list)

    # Index claims by content_id
    claims_by_content = defaultdict(list)
    for claim in claims:
        claims_by_content[claim['content_id']].append(claim)

    # Process content
    for c in content:
        # Use raw_text for mint extraction to preserve base58 case
        raw_text = c.get('raw_text', '') or c.get('normalized_text', '')
        mints = extract_all_mints(raw_text)
        # Also check mentioned_mints field (already case-preserving from raw)
        if c.get('mentioned_mints'):
            for m in c['mentioned_mints'].split('|'):
                m = m.strip()
                if m and len(m) >= 32:
                    mints.append(m)

        for mint in set(mints):  # dedupe per content
            timeline[mint].append({
                'time_ms': c['publish_time_ms'],
                'content': c,
                'claims': claims_by_content.get(c['content_id'], []),
            })

    return dict(timeline)


def compute_state_at_t(mint: str, t_ms: int, timeline: list) -> dict:
    """Compute narrative state for a mint at time t.
    Only uses events with time_ms <= t_ms (causal)."""
    # Filter to events before t
    causal_events = [e for e in timeline if e['time_ms'] <= t_ms]
    if not causal_events:
        return None

    # Time windows
    ONE_H = 3600 * 1000
    SIX_H = 6 * ONE_H
    ONE_DAY = 24 * ONE_H

    events_1h = [e for e in causal_events if e['time_ms'] >= t_ms - ONE_H]
    events_6h = [e for e in causal_events if e['time_ms'] >= t_ms - SIX_H]
    events_24h = [e for e in causal_events if e['time_ms'] >= t_ms - ONE_DAY]

    # Mention counts
    mention_count_1h = len(events_1h)
    mention_count_6h = len(events_6h)
    mention_count_24h = len(events_24h)

    # Mention velocity (mentions/hour)
    mention_velocity_1h = mention_count_1h  # 1h window / 1h = count
    mention_velocity_6h = mention_count_6h / 6.0

    # Mention acceleration (current 1h vs prior 1h)
    prior_1h_events = [e for e in timeline
                       if t_ms - 2 * ONE_H <= e['time_ms'] < t_ms - ONE_H]
    prior_velocity = len(prior_1h_events)
    mention_acceleration = mention_velocity_1h - prior_velocity if mention_velocity_1h > 0 else -prior_velocity

    # Independent creators (unique non-echo handles)
    def count_independent(events):
        handles = set()
        for e in events:
            h = e['content'].get('account_handle', '')
            if h and not e['content'].get('is_echo', False):
                handles.add(h)
        return len(handles)

    independent_creators_1h = count_independent(events_1h)
    independent_creators_24h = count_independent(events_24h)

    # Platform diversity
    def count_platforms(events):
        return len(set(e['content'].get('platform', '') for e in events))

    platform_diversity_1h = count_platforms(events_1h)
    platform_diversity_24h = count_platforms(events_24h)

    # Breadth score (weighted diversity)
    breadth_score = 0.0
    if independent_creators_24h > 0:
        breadth_score = (independent_creators_24h * 0.4 +
                         platform_diversity_24h * 0.3 +
                         min(mention_count_24h / 10.0, 1.0) * 0.3)

    # Echo ratio
    echo_count_1h = sum(1 for e in events_1h if e['content'].get('is_echo', False))
    total_1h = len(events_1h)
    echo_ratio_1h = echo_count_1h / total_1h if total_1h > 0 else 0.0

    # Amplification factor
    total_reach = total_1h
    independent_reach = independent_creators_1h
    amplification_factor = total_reach / independent_reach if independent_reach > 0 else 1.0

    # Consensus direction
    directions = []
    for e in events_24h:
        for claim in e.get('claims', []):
            d = claim.get('direction')
            if d:
                directions.append(d)
        # Also extract from content directly
        if not e.get('claims'):
            text = e['content'].get('normalized_text', '')
            d, _ = extract_direction(text)
            if d != 'neutral':
                directions.append(d)

    dir_counter = Counter(directions)
    if not dir_counter:
        consensus_direction = 'none'
        consensus_strength = 0.0
    else:
        top_dir, top_count = dir_counter.most_common(1)[0]
        total_dirs = sum(dir_counter.values())
        if top_count / total_dirs > 0.7:
            consensus_direction = 'bullish' if top_dir in ('long',) else \
                'bearish' if top_dir in ('short',) else \
                'cautious' if top_dir == 'avoid' else 'mixed'
            consensus_strength = top_count / total_dirs
        else:
            consensus_direction = 'mixed'
            consensus_strength = 0.5

    # First mention time and age
    first_mention_time = min(e['time_ms'] for e in causal_events)
    age_at_t_seconds = (t_ms - first_mention_time) / 1000.0

    # Novelty score (how new is this narrative)
    if age_at_t_seconds < 3600:  # < 1h
        novelty_score = 1.0
    elif age_at_t_seconds < 21600:  # < 6h
        novelty_score = 0.7
    elif age_at_t_seconds < 86400:  # < 24h
        novelty_score = 0.4
    elif age_at_t_seconds < 259200:  # < 3 days
        novelty_score = 0.2
    else:
        novelty_score = 0.1

    # Saturation score (how crowded)
    if mention_count_24h > 50:
        saturation_score = 1.0
    elif mention_count_24h > 20:
        saturation_score = 0.7
    elif mention_count_24h > 10:
        saturation_score = 0.4
    elif mention_count_24h > 3:
        saturation_score = 0.2
    else:
        saturation_score = 0.1

    # Alpha call count (entry calls)
    alpha_calls_1h = sum(1 for e in events_1h
                         for c in e.get('claims', [])
                         if c.get('claim_type') == 'entry')
    alpha_calls_24h = sum(1 for e in events_24h
                          for c in e.get('claims', [])
                          if c.get('claim_type') == 'entry')

    # Tracker alert count (platform=tracker or TOOL_WALLET_SIGNAL)
    tracker_alerts_1h = sum(1 for e in events_1h
                            if e['content'].get('account_type') in ('tracker', 'tool') or
                            e['content'].get('usefulness_class') == 'TOOL_WALLET_SIGNAL')

    # Reliability (as-of-t, only prior claims)
    # For now, use identity_confidence as a proxy (will be enriched in validation layer)
    reliability_scores = []
    for e in events_24h:
        conf = e['content'].get('identity_confidence', 'UNRESOLVED')
        if conf == 'HIGH':
            reliability_scores.append(0.8)
        elif conf == 'MEDIUM':
            reliability_scores.append(0.5)
        elif conf == 'UNRESOLVED':
            reliability_scores.append(0.3)
    weighted_reliability_1h = sum(reliability_scores[:mention_count_1h]) / mention_count_1h if mention_count_1h > 0 else 0
    weighted_reliability_24h = sum(reliability_scores) / len(reliability_scores) if reliability_scores else 0

    # High reliability creators count
    n_high_reliability = sum(1 for e in events_24h
                             if e['content'].get('identity_confidence') == 'HIGH')

    # Coordinated shill detection
    # Check for: same time window burst + same direction + low diversity
    if mention_count_1h > 5 and independent_creators_1h < 3 and echo_ratio_1h > 0.5:
        coordinated_shill_score = 0.8
    elif mention_count_1h > 10 and platform_diversity_1h <= 1:
        coordinated_shill_score = 0.6
    else:
        coordinated_shill_score = 0.0

    # Crowding score (how late/crowded)
    crowding_score = saturation_score * 0.5 + (1 - novelty_score) * 0.5

    # Copytrade risk
    if independent_creators_24h <= 2 and mention_count_24h > 10:
        copytrade_risk_score = 0.8  # few sources, many mentions = likely copytrade
    elif independent_creators_24h <= 5 and mention_count_24h > 20:
        copytrade_risk_score = 0.5
    else:
        copytrade_risk_score = 0.2

    return {
        'mention_count_1h': mention_count_1h,
        'mention_count_6h': mention_count_6h,
        'mention_count_24h': mention_count_24h,
        'mention_velocity_1h': float(mention_velocity_1h),
        'mention_velocity_6h': mention_velocity_6h,
        'mention_acceleration': float(mention_acceleration),
        'independent_creators_1h': independent_creators_1h,
        'independent_creators_24h': independent_creators_24h,
        'platform_diversity_1h': platform_diversity_1h,
        'platform_diversity_24h': platform_diversity_24h,
        'breadth_score': round(breadth_score, 4),
        'echo_ratio_1h': round(echo_ratio_1h, 4),
        'amplification_factor': round(amplification_factor, 4),
        'consensus_direction': consensus_direction,
        'consensus_strength': round(consensus_strength, 4),
        'first_mention_time_ms': first_mention_time,
        'age_at_t_seconds': round(age_at_t_seconds, 2),
        'novelty_score': round(novelty_score, 4),
        'saturation_score': round(saturation_score, 4),
        'alpha_call_count_1h': alpha_calls_1h,
        'alpha_call_count_24h': alpha_calls_24h,
        'tracker_alert_count_1h': tracker_alerts_1h,
        'weighted_reliability_1h': round(weighted_reliability_1h, 4),
        'weighted_reliability_24h': round(weighted_reliability_24h, 4),
        'n_high_reliability_creators': n_high_reliability,
        'coordinated_shill_score': round(coordinated_shill_score, 4),
        'crowding_score': round(crowding_score, 4),
        'copytrade_risk_score': round(copytrade_risk_score, 4),
    }


def build_narrative_state(content_path: str, claim_path: str, output_dir: str) -> dict:
    """Build narrative_state_v1 from content + claims."""
    print("=" * 70)
    print("NARRATIVE_STATE_V1 - Gold Layer 3 Builder")
    print("=" * 70)

    content, claims = load_content_and_claims(content_path, claim_path)
    print(f"  Loaded {len(content)} content records, {len(claims)} claims")

    # Build per-mint timeline
    mint_timeline = build_mint_timeline(content, claims)
    print(f"  Unique mints with mentions: {len(mint_timeline)}")

    run_uuid = f"ns_{int(time_module.time())}"
    git_sha = __import__('subprocess').check_output(
        ['git', 'rev-parse', '--short', 'HEAD'],
        cwd='D:/repos/mev_bot'
    ).decode().strip()
    print(f"  Run UUID: {run_uuid}")
    print(f"  Git SHA:  {git_sha}")

    # Compute states at each mention event time (causal)
    all_states = []
    for mint, timeline in mint_timeline.items():
        # Sort timeline by time
        timeline.sort(key=lambda x: x['time_ms'])

        # Compute state at each event time (causal - only uses past events)
        seen_state_times = set()
        for event in timeline:
            t_ms = event['time_ms']

            # Skip duplicate state times (same mint, same time)
            if t_ms in seen_state_times:
                continue
            seen_state_times.add(t_ms)

            state = compute_state_at_t(mint, t_ms, timeline)
            if state is None:
                continue

            state_id = hashlib.sha256(
                f"{mint}|{t_ms}".encode()
            ).hexdigest()[:16]

            state.update({
                'narrative_state_id': state_id,
                'schema_version': '1.0.0',
                'pipeline_version': '1.1.0',
                'run_uuid': run_uuid,
                'git_sha': git_sha,
                'mint': mint,
                'state_time_ms': t_ms,
                # On-chain fields (populated in validation layer when joined with Slinky)
                'social_onchain_divergence': None,
                'divergence_direction': None,
                'divergence_magnitude': None,
                'dev_wallet_active': None,
                'connected_wallet_count': None,
                'precursor_wallet_lineage': None,
                'first_buyer_count': None,
                'bundle_count': None,
                'first_buyer_overlap_count': None,
                'remake_count': None,
                'og_beta_competition': None,
                'account_reuse_count': None,
                'tracker_divergence': None,
                'control_group_mints': None,
                'control_same_regime': None,
                'treatment_differential': None,
                'coverage_note': None,
                'contamination_flags': None,
            })

            # Admission logic
            if state['mention_count_24h'] >= 3 and state['independent_creators_24h'] >= 2:
                state['admission_status'] = 'GOLD'
            elif state['mention_count_24h'] >= 1:
                state['admission_status'] = 'UNRESOLVED'
            else:
                state['admission_status'] = 'REJECTED'
                state['rejection_reason'] = 'insufficient_mentions'

            # Add coverage note
            state['coverage_note'] = (
                "Missing source != zero mentions. "
                "Coverage gaps in Twitter/X (login wall) may undercount. "
                "Wayback CDX provides historical snapshots but not exhaustive."
            )

            all_states.append(state)

    print(f"\n  Total narrative states: {len(all_states)}")

    # Stats
    from collections import Counter
    admission = Counter(s['admission_status'] for s in all_states)
    print(f"  Admission: {dict(admission)}")

    consensus = Counter(s['consensus_direction'] for s in all_states)
    print(f"  Consensus: {dict(consensus)}")

    # Write output
    os.makedirs(output_dir, exist_ok=True)
    out_path = os.path.join(output_dir, 'narrative_state_v1.jsonl')
    with open(out_path, 'w', encoding='utf-8') as f:
        for state in all_states:
            f.write(json.dumps(state) + '\n')

    print(f"\n  Output: {out_path}")
    print(f"  Total states: {len(all_states)}")
    return {'total_states': len(all_states), 'output_path': out_path}


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--content-path',
                        default='output/narrative_gold_v1/gold/creator_content_v1/creator_content_v1.jsonl')
    parser.add_argument('--claim-path',
                        default='output/narrative_gold_v1/gold/creator_claim_v1/creator_claim_v1.jsonl')
    parser.add_argument('--output-dir',
                        default='output/narrative_gold_v1/gold/narrative_state_v1')
    args = parser.parse_args()
    build_narrative_state(args.content_path, args.claim_path, args.output_dir)
