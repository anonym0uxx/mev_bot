#!/usr/bin/env python
"""
build_creator_claim_v1.py - Gold Layer 2: extracted claims from creator content

Transforms creator_content_v1 -> creator_claim_v1.

EXTRACTS:
  1. Entry/exit calls (long/short, entry price, target, stop loss, sizing)
  2. Risk warnings (rug-pull warnings, liquidity warnings, concentration warnings)
  3. Narrative claims (market regime, sentiment, theme identification)
  4. Facts (on-chain observations, wallet movements, token launches)
  5. Opinions (judgment calls, strategy preferences, market read)
  6. Predictions (price targets, timeline estimates, outcome forecasts)

TAXONOMY:
  claim_type: entry|exit|risk|narrative|regime|observation
  claim_modality: fact|opinion|prediction|call|warning
  temporal_classification: EX_ANTE (before outcome known) | EX_POST (after) | AMBIGUOUS

Usage: python src/build_creator_claim_v1.py
"""

import json
import os
import re
import hashlib
import argparse
from collections import Counter

# ─── Claim extraction patterns ──────────────────────────────────────────────

DIRECTION_LONG_RE = re.compile(
    r'\b(long|bull|moon|send|pump|buy|accumulat|load|enter|snipe|grab|hold|bag)\b',
    re.IGNORECASE
)
DIRECTION_SHORT_RE = re.compile(
    r'\b(short|bear|dump|sell|exit|close|tp|take.?profit|rebalance|crash|nuked|rugged)\b',
    re.IGNORECASE
)
DIRECTION_WATCH_RE = re.compile(
    r'\b(watch|monitor|wait|observe|tracking|looking\s+at|considering|dca|dip)\b',
    re.IGNORECASE
)
DIRECTION_AVOID_RE = re.compile(
    r'\b(avoid|skip|stay\s+away|do\s+not\s+buy|don.?t\s+buy|danger|scam|rug|honeypot|trap|sketch)\b',
    re.IGNORECASE
)

SOL_AMOUNT_RE = re.compile(
    r'(\d+(?:\.\d+)?)\s*(?:sol|solana)\b', re.IGNORECASE
)
MULTIPLIER_RE = re.compile(
    r'\b(\d+)\s*x\b|\bx\s*(\d+)\b', re.IGNORECASE
)
PRICE_TARGET_RE = re.compile(
    r'(?:target|tp|goal|reach|hit|aim)\s*\$?(\d[\d.]*(?:k|m|b)?)|'
    r'mcap\s+(?:of\s+)?\$?(\d[\d.]*(?:k|m|b)?)',
    re.IGNORECASE
)

RUG_RE = re.compile(
    r'\b(rug|rug.?pull|pulled|dev\s+(?:sold|dumped|abandoned)|'
    r'liquidity\s+(?:pulled|removed)|honeypot|sniper|scam\s+(?:token|coin|dev))\b',
    re.IGNORECASE
)
LIQUIDITY_RE = re.compile(
    r'\b(liquidity|liq|pool\s+(?:depth|size)|low\s+liq|thin\s+liq|slippage)\b',
    re.IGNORECASE
)
CONCENTRATION_RE = re.compile(
    r'\b(concentrat|top\s+holders|whale|insider|dev\s+wallet|bundle|sniper\s+wallet)\b',
    re.IGNORECASE
)

NARRATIVE_THEME_RE = {
    'dev-buy': re.compile(r'\b(dev\s+(?:buy|bought|holding|accumulating)|dev\s+wallet)\b', re.IGNORECASE),
    'rug-pull': re.compile(r'\b(rug|rug.?pull|pulled\s+liq|dev\s+(?:sold|dumped|ran))\b', re.IGNORECASE),
    'graduation': re.compile(r'\b(graduate|graduation|bond|raydium|pump\.fun\s+exit)\b', re.IGNORECASE),
    'sniper': re.compile(r'\b(snipe|sniper|sniping|front.?run|mev)\b', re.IGNORECASE),
    'whale-movement': re.compile(r'\b(whale|large\s+wallet|big\s+wallet|whale\s+(?:buy|sell|move))\b', re.IGNORECASE),
    'kol-endorsement': re.compile(r'\b(kol|influencer|shill|endorse|call\s+out)\b', re.IGNORECASE),
    'rotation': re.compile(r'\b(rotat(?:e|ion)|shift|flow|inflow|outflow|capital\s+rotation)\b', re.IGNORECASE),
    'regime-shift': re.compile(r'\b(regime|risk.?on|risk.?off|sentiment\s+shift)\b', re.IGNORECASE),
}

REGIME_RE = re.compile(
    r'\b(risk.?on|risk.?off|bull\s+market|bear\s+market|chop|sideways|crab|'
    r'accumulation|distribution|meme\s+season|alt\s+season)\b',
    re.IGNORECASE
)

SOLANA_ADDR_RE = re.compile(r'\b([1-9A-HJ-NP-Za-km-z]{32,44})\b')
PUMP_MINT_RE = re.compile(r'\b([1-9A-HJ-NP-Za-km-z]{32,44}pump)\b', re.IGNORECASE)
CASHTAG_RE = re.compile(r'\$([A-Za-z]{2,10})\b')

SIZING_PCT_RE = re.compile(
    r'(\d+(?:\.\d+)?)\s*%?\s*(?:of\s+)?(?:bankroll|portfolio|bag|stack|cap|allocat)',
    re.IGNORECASE
)
SIZING_SOL_RE = re.compile(
    r'(\d+(?:\.\d+)?)\s*(?:sol|solana)\s*(?:worth|each|per|allocation)?',
    re.IGNORECASE
)

HORIZON_RE = re.compile(
    r'\b(\d+)\s*(min|hour|hr|day|week|month|s|m|h|d|w)\b|'
    r'(?:in|within|after)\s*(\d+)\s*(min|hour|hr|day|week|month)',
    re.IGNORECASE
)

CONFIDENCE_HIGH_RE = re.compile(
    r'\b(certain|guarantee|definitely|sure\s+thing|lock|easy|free\s+money|guaranteed|100%)\b',
    re.IGNORECASE
)
CONFIDENCE_LOW_RE = re.compile(
    r'\b(maybe|might|possibly|could|uncertain|risky|gamble|degen|yolo|not\s+sure|unsure|probably|likely)\b',
    re.IGNORECASE
)

PAST_TENSE_RE = re.compile(
    r'\b(was|were|had|did|went|came|became|turned\s+out|ended\s+up|'
    r'reached|failed|succeeded|worked|didn.?t|crashed|pumped\s+to|dumped|rugged|graduated|bonded)\b',
    re.IGNORECASE
)
FUTURE_TENSE_RE = re.compile(
    r'\b(will|going\s+to|gonna|should|could|might|probably|expect|forecast|predict|target\s+is)\b',
    re.IGNORECASE
)


# ─── Extraction functions ───────────────────────────────────────────────────

def extract_direction(text: str) -> tuple:
    """Extract direction from text. Returns (direction, confidence_stated)."""
    long_score = len(DIRECTION_LONG_RE.findall(text))
    short_score = len(DIRECTION_SHORT_RE.findall(text))
    watch_score = len(DIRECTION_WATCH_RE.findall(text))
    avoid_score = len(DIRECTION_AVOID_RE.findall(text))

    if avoid_score > 0:
        return ('avoid', 0.8)
    if short_score > long_score:
        return ('short', 0.6)
    if long_score > 0:
        conf = 0.9 if CONFIDENCE_HIGH_RE.search(text) else 0.5
        return ('long', conf)
    if watch_score > 0:
        return ('watch', 0.3)
    return ('neutral', 0.2)


def extract_price_targets(text: str) -> dict:
    """Extract entry price, target price from text."""
    targets = {}
    sol_matches = SOL_AMOUNT_RE.findall(text)
    if sol_matches:
        sols = [float(s) for s in sol_matches]
        if len(sols) >= 2:
            targets['entry_price_sol'] = min(sols)
            targets['target_price_sol'] = max(sols)
        elif len(sols) == 1:
            targets['entry_price_sol'] = sols[0]

    mult_match = MULTIPLIER_RE.search(text)
    if mult_match:
        mult_val = mult_match.group(1) or mult_match.group(2)
        if mult_val:
            try:
                targets['multiplier'] = float(mult_val)
            except ValueError:
                pass

    target_match = PRICE_TARGET_RE.search(text)
    if target_match:
        val = target_match.group(1) or target_match.group(2)
        if val:
            v = val.lower()
            try:
                if 'k' in v:
                    targets['target_mcap_k'] = float(v.replace('k', '')) * 1000
                elif 'm' in v:
                    targets['target_mcap_k'] = float(v.replace('m', '')) * 1000000
                elif 'b' in v:
                    targets['target_mcap_k'] = float(v.replace('b', '')) * 1000000000
                else:
                    targets['target_mcap_k'] = float(v)
            except ValueError:
                pass
    return targets


def extract_sizing(text: str) -> dict:
    """Extract position sizing from text."""
    sizing = {}
    pct_match = SIZING_PCT_RE.search(text)
    if pct_match:
        try:
            sizing['sizing_pct_bankroll'] = float(pct_match.group(1))
        except ValueError:
            pass

    sol_match = SIZING_SOL_RE.search(text)
    if sol_match:
        try:
            sizing['sizing_sol'] = float(sol_match.group(1))
        except ValueError:
            pass
    return sizing


def extract_horizon(text: str) -> int | None:
    """Extract time horizon in seconds."""
    lower = text.lower()
    if any(w in lower for w in ['scalp', 'flip', 'quick', 'short-term']):
        return 300
    if any(w in lower for w in ['long-term', 'hold long', 'diamond']):
        return 604800

    match = HORIZON_RE.search(text)
    if not match:
        return None
    num = match.group(1) or match.group(3)
    unit = match.group(2) or match.group(4)
    if not num or not unit:
        return None
    try:
        n = int(num)
    except ValueError:
        return None
    unit = unit.lower()
    mult = {'s': 1, 'min': 60, 'm': 60, 'h': 3600, 'hr': 3600, 'hour': 3600,
            'd': 86400, 'day': 86400, 'w': 604800, 'week': 604800, 'month': 2592000}
    return n * mult.get(unit, 3600)


def extract_narrative_themes(text: str) -> list:
    """Extract narrative themes from text."""
    return [theme for theme, pattern in NARRATIVE_THEME_RE.items() if pattern.search(text)]


def extract_regime(text: str) -> str | None:
    """Extract market regime label."""
    match = REGIME_RE.search(text)
    if match:
        return match.group(0).lower().replace('-', '_').replace(' ', '_')
    return None


def extract_entities(text: str) -> tuple:
    """Extract entities. Returns (entities_str, entity_type, primary_mint, primary_token)."""
    mints = PUMP_MINT_RE.findall(text)
    all_addrs = SOLANA_ADDR_RE.findall(text)
    cashtags = CASHTAG_RE.findall(text)

    primary_mint = mints[0] if mints else (all_addrs[0] if all_addrs else None)
    primary_token = cashtags[0] if cashtags else None

    entities = []
    entity_type = 'unknown'
    if mints:
        entities.extend(mints)
        entity_type = 'mint'
    elif all_addrs:
        entities.extend(all_addrs[:3])
        entity_type = 'mint'
    if cashtags:
        entities.extend([f"${c}" for c in cashtags])
        if entity_type == 'unknown':
            entity_type = 'token'

    return ('|'.join(entities[:5]) if entities else '',
            entity_type if entities else 'unknown',
            primary_mint,
            primary_token)


def classify_temporal(text: str) -> tuple:
    """Classify EX_ANTE / EX_POST / AMBIGUOUS. Returns (classification, reason)."""
    past_score = len(PAST_TENSE_RE.findall(text))
    future_score = len(FUTURE_TENSE_RE.findall(text))

    if past_score > future_score and past_score >= 2:
        return ('EX_POST', 'past_tense_dominant')
    if future_score > past_score and future_score >= 1:
        return ('EX_ANTE', 'future_tense_dominant')
    return ('AMBIGUOUS', 'no_clear_temporal_signal')


def classify_modality(text: str) -> str:
    """Classify claim modality: fact|opinion|prediction|call|warning."""
    lower = text.lower()

    if RUG_RE.search(text) or LIQUIDITY_RE.search(text) or CONCENTRATION_RE.search(text):
        if any(w in lower for w in ['warning', 'careful', 'watch out', 'beware', 'danger']):
            return 'warning'
        return 'warning'

    if FUTURE_TENSE_RE.search(text):
        return 'prediction'

    if DIRECTION_LONG_RE.search(text) or DIRECTION_SHORT_RE.search(text):
        if any(w in lower for w in ['buy', 'sell', 'long', 'short', 'enter', 'exit', 'call']):
            return 'call'

    if any(w in lower for w in ['i think', 'imo', 'my take', 'i believe', 'seems like', 'feel like']):
        return 'opinion'

    if PAST_TENSE_RE.search(text):
        return 'fact'

    return 'opinion'


def determine_claim_type(text: str, modality: str) -> str:
    """Determine claim type: entry|exit|risk|narrative|regime|observation."""
    if RUG_RE.search(text) or LIQUIDITY_RE.search(text) or CONCENTRATION_RE.search(text):
        return 'risk'

    if DIRECTION_SHORT_RE.search(text) and any(w in text.lower() for w in ['exit', 'sell', 'close', 'tp']):
        return 'exit'

    if DIRECTION_LONG_RE.search(text) and any(w in text.lower() for w in ['buy', 'enter', 'long', 'load']):
        return 'entry'

    regime = extract_regime(text)
    if regime:
        return 'regime'

    themes = extract_narrative_themes(text)
    if themes:
        return 'narrative'

    if modality == 'fact':
        return 'observation'

    return 'narrative'


def extract_claims_from_content(content: dict, run_uuid: str, git_sha: str) -> list:
    """Extract all claims from a single content record."""
    # Use normalized_text for keyword matching, but raw_text for mint extraction
    # to preserve base58 case (critical for Slinky v3 onchain matching).
    text = content.get('normalized_text', '') or content.get('raw_text', '')
    raw_text = content.get('raw_text', '') or text  # original case for mint extraction
    if not text or len(text) < 20:
        return []

    # Skip content that's pure noise
    usefulness = content.get('usefulness_class', '')
    if usefulness in ('LOW_SIGNAL_CHATTER', 'PROMOTIONAL_SHILL', 'DUPLICATE'):
        return []

    claims = []
    content_id = content['content_id']

    # Extract all components (keyword matching uses normalized text)
    direction, conf_stated = extract_direction(text)
    price_targets = extract_price_targets(text)
    sizing = extract_sizing(text)
    horizon = extract_horizon(text)
    themes = extract_narrative_themes(text)
    regime = extract_regime(text)

    # Mint extraction: prefer mentioned_mints field (already case-preserving from raw),
    # then fall back to extracting from raw_text (original case)
    mentioned_mints_raw = content.get('mentioned_mints')
    if mentioned_mints_raw and isinstance(mentioned_mints_raw, str):
        # Split pipe-separated mints
        mint_list = [m for m in mentioned_mints_raw.split('|') if m and len(m) >= 32]
        if mint_list:
            primary_mint = mint_list[0]
            entities_str = '|'.join(mint_list[:5])
            entity_type = 'mint'
        else:
            entities_str, entity_type, primary_mint, primary_token = extract_entities(raw_text)
    else:
        entities_str, entity_type, primary_mint, primary_token = extract_entities(raw_text)

    # If no mint found from mentioned_mints or raw_text, try normalized as last resort
    if not primary_mint:
        _, _, primary_mint, _ = extract_entities(text)

    cashtag_matches = CASHTAG_RE.findall(text)
    primary_token = cashtag_matches[0] if cashtag_matches else None
    temporal_class, temporal_reason = classify_temporal(text)
    modality = classify_modality(text)
    claim_type = determine_claim_type(text, modality)

    # Skip if no actionable signal
    has_direction = direction != 'neutral'
    has_entities = bool(entities_str)
    has_themes = bool(themes)
    has_regime = bool(regime)
    has_targets = bool(price_targets)
    has_sizing = bool(sizing)

    if not any([has_direction, has_entities, has_themes, has_regime, has_targets]):
        return []

    # Create primary claim
    claim_id = hashlib.sha256(
        f"{content_id}|{claim_type}|{modality}".encode()
    ).hexdigest()[:16]

    # Extract claim text snippet (first 500 chars with context)
    claim_text = text[:500].replace('\n', ' ').strip()

    claim = {
        'claim_id': claim_id,
        'schema_version': '1.0.0',
        'pipeline_version': '1.1.0',
        'run_uuid': run_uuid,
        'git_sha': git_sha,

        'content_id': content_id,
        'platform': content['platform'],
        'account_handle': content['account_handle'],
        'account_id': content['account_id'],
        'account_type': content['account_type'],
        'publish_time_ms': content['publish_time_ms'],

        'claim_type': claim_type,
        'claim_modality': modality,
        'temporal_classification': temporal_class,
        'temporal_reason': temporal_reason,

        'claim_text': claim_text,
        'entities': entities_str,
        'entity_type': entity_type,
        'primary_mint': primary_mint,
        'primary_token': primary_token,

        'direction': direction if has_direction else None,
        'entry_price_sol': price_targets.get('entry_price_sol'),
        'target_price_sol': price_targets.get('target_price_sol'),
        'stop_loss_price_sol': None,  # rarely stated explicitly
        'sizing_sol': sizing.get('sizing_sol'),
        'sizing_pct_bankroll': sizing.get('sizing_pct_bankroll'),
        'horizon_seconds': horizon,
        'confidence_stated': conf_stated if has_direction else None,

        'narrative_theme': '|'.join(themes) if themes else None,
        'regime_label': regime,

        'creator_track_record_as_of_t': None,  # populated in validation layer
        'creator_n_prior_claims': None,
        'creator_n_prior_correct': None,
        'creator_n_prior_contradicted': None,

        'admission_status': 'UNRESOLVED',  # will be set below
        'rejection_reason': None,
        'contamination_flags': content.get('contamination_flags'),
    }

    # Admission logic
    # GOLD: clear direction + entities + (targets OR sizing OR themes)
    if has_direction and has_entities:
        claim['admission_status'] = 'GOLD'
    elif has_direction and (has_themes or has_regime):
        claim['admission_status'] = 'GOLD'
    elif modality == 'warning' and has_entities:
        claim['admission_status'] = 'GOLD'
    elif has_regime:
        claim['admission_status'] = 'GOLD'
    elif has_entities and (has_themes or has_targets):
        claim['admission_status'] = 'UNRESOLVED'
    elif has_direction and not has_entities:
        claim['admission_status'] = 'UNRESOLVED'
        claim['rejection_reason'] = 'direction_without_entity'
    else:
        claim['admission_status'] = 'REJECTED'
        claim['rejection_reason'] = 'no_actionable_signal'

    claims.append(claim)

    # If there are multiple themes, create secondary narrative claims
    if len(themes) > 1:
        for theme in themes[1:]:
            secondary_id = hashlib.sha256(
                f"{content_id}|narrative|{theme}".encode()
            ).hexdigest()[:16]
            secondary = dict(claim)
            secondary['claim_id'] = secondary_id
            secondary['claim_type'] = 'narrative'
            secondary['narrative_theme'] = theme
            secondary['direction'] = None
            secondary['confidence_stated'] = None
            secondary['admission_status'] = 'UNRESOLVED'
            claims.append(secondary)

    return claims


# ─── Main builder ───────────────────────────────────────────────────────────

def build_creator_claim(content_path: str, output_dir: str) -> dict:
    """Build creator_claim_v1 from creator_content_v1."""
    print("=" * 70)
    print("CREATOR_CLAIM_V1 - Gold Layer 2 Builder")
    print("=" * 70)

    # Load content
    with open(content_path, 'r', encoding='utf-8') as f:
        content_records = [json.loads(line) for line in f if line.strip()]
    print(f"  Loaded {len(content_records)} content records")

    # Filter to GOLD/UNRESOLVED only (skip REJECTED)
    eligible = [r for r in content_records if r['admission_status'] in ('GOLD', 'UNRESOLVED')]
    print(f"  Eligible (GOLD+UNRESOLVED): {len(eligible)}")

    run_uuid = f"cl_{int(__import__('time').time())}"
    git_sha = __import__('subprocess').check_output(
        ['git', 'rev-parse', '--short', 'HEAD'],
        cwd='D:/repos/mev_bot'
    ).decode().strip()
    print(f"  Run UUID: {run_uuid}")
    print(f"  Git SHA:  {git_sha}")

    # Extract claims
    all_claims = []
    for content in eligible:
        claims = extract_claims_from_content(content, run_uuid, git_sha)
        all_claims.extend(claims)

    print(f"\n  Total claims extracted: {len(all_claims)}")

    # Stats
    admission = Counter(c['admission_status'] for c in all_claims)
    print(f"  Admission: {dict(admission)}")

    claim_types = Counter(c['claim_type'] for c in all_claims)
    print(f"  Claim types: {dict(claim_types)}")

    modalities = Counter(c['claim_modality'] for c in all_claims)
    print(f"  Modalities: {dict(modalities)}")

    temporal = Counter(c['temporal_classification'] for c in all_claims)
    print(f"  Temporal: {dict(temporal)}")

    directions = Counter(c['direction'] for c in all_claims if c['direction'])
    print(f"  Directions: {dict(directions)}")

    # Write output
    os.makedirs(output_dir, exist_ok=True)
    out_path = os.path.join(output_dir, 'creator_claim_v1.jsonl')
    with open(out_path, 'w', encoding='utf-8') as f:
        for claim in all_claims:
            f.write(json.dumps(claim) + '\n')

    print(f"\n  Output: {out_path}")
    print(f"  Total claims: {len(all_claims)}")
    return {'total_claims': len(all_claims), 'output_path': out_path}


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--content-path', default='output/narrative_gold_v1/gold/creator_content_v1/creator_content_v1.jsonl')
    parser.add_argument('--output-dir', default='output/narrative_gold_v1/gold/creator_claim_v1')
    args = parser.parse_args()
    build_creator_claim(args.content_path, args.output_dir)
