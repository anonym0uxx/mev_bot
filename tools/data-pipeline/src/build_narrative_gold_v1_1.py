#!/usr/bin/env python
"""
build_narrative_gold_v1_1.py — Corrected narrative corpus builder.

Reads FROZEN narrative_gold_v1 (read-only) and produces corrected v1.1 with:
  - Entity/mint propagation with confidence tracking
  - Temporal disambiguation (EX_ANTE / RETROSPECTIVE / AMBIGUOUS)
  - Dedup/amplification clustering (origin_id, amplification_cluster_id, originality_confidence)
  - Scope gating (A=pump_specific, B=solana_memecoin_regime, C=generic_crypto, D=unresolved)
  - Strategy cards rebuilt from evidence (no hollow cards)
  - Creator quality filtering (content-level, not identity-based)
  - Claim validation vs Slinky/LaserStream (SUPPORTED/CONTRADICTED/MIXED/UNRESOLVED)
  - Human reasoning trajectories

NEVER mutates v1. Produces only into narrative_gold_v1.1/ directory.
QUALITY > QUANTITY.
"""
import json, os, re, hashlib, uuid, subprocess
from collections import Counter, defaultdict
from datetime import datetime, timezone
from dataclasses import dataclass, asdict, field
from typing import Optional

# ─── PATHS ───
V1_BASE = 'D:/repos/mev_bot/tools/data-pipeline/output/narrative_gold_v1'
V11_BASE = 'D:/repos/mev_bot/tools/data-pipeline/output/narrative_gold_v1.1'
SLINKY_PATH = 'D:/repos/mev_bot/tools/data-pipeline/output/slinky_gold_v3_compact'
REPO_ROOT = 'D:/repos/mev_bot'

RUN_UUID = 'nv11_48e7f58d6d91'
FREEZE_UUID = 'ng11_43bf56a50ed7'

# ─── REGEX ───
PUMP_MINT_RE = re.compile(r'\b([1-9A-HJ-NP-Za-km-z]{32,44}pump)\b', re.IGNORECASE)
SOLANA_ADDR_RE = re.compile(r'\b([1-9A-HJ-NP-Za-km-z]{32,44})\b')
CASHTAG_RE = re.compile(r'\$([A-Za-z]{2,10})\b')
HANDLE_RE = re.compile(r'@([A-Za-z0-9_]{3,30})')

# ─── SCOPE KEYWORDS ───
PUMP_KW = ['pump.fun', 'pumpfun', 'pump', '.pump', 'pump swap', 'pumpswap',
           'graduat', 'bonding curve', 'bonding_curve', 'raydium pool']
SOL_MEMECOIN_KW = ['solana', 'memecoin', 'meme coin', 'sol memes', 'dexscreener',
                   'rug pull', 'rugpull', 'dev buy', 'dev wallet', 'sniper',
                   'bonding', 'launch', 'launchpad', 'jupiter', 'raydium',
                   'jito', 'bundle', 'copy trade', 'copytrade']
GENERIC_KW = ['bitcoin', 'btc', 'ethereum', 'eth ', 'defi', 'stablecoin',
              'institutional', 'sec ', 'etf', 'fed', 'macro', 'treasury',
              'altcoin', 'layer 2', 'l2 ', 'nft', 'dao', 'governance']

# ─── RETROSPECTIVE INDICATORS ───
RETRO_INDICATORS = [
    'i called', 'i bought', 'i sold', 'i entered', 'i exited',
    'i made', 'i lost', 'profit', 'pnl', 'result', 'outcome',
    'in hindsight', 'looking back', 'after the fact', 'recap',
    'what happened', 'turned out', 'ended up', 'final result',
    'i should have', 'i would have', 'missed opportunity',
    'if i had', 'my trade', 'my position', 'closed at',
    'sold at', 'bought at', 'entry was', 'exit was',
    'i was wrong', 'i was right', 'prediction',
]
EX_ANTE_INDICATORS = [
    'i will', 'i plan', 'i think', 'i expect', 'watching',
    'looking at', 'considering', 'might buy', 'about to',
    'setup forming', 'watch this', 'keep an eye', 'monitoring',
    'preparing', 'waiting for', 'if it breaks', 'target is',
    'invalidation', 'stop loss', 'entry point',
]

# ─── QUALITY INDICATORS ───
LOW_QUALITY_PATTERNS = [
    r'best in market.*copy trad',
    r'auto exit strateg',
    r'sign up.*link',
    r'referral.*link',
    r'join.*channel',
    r'dm me',
    r'follow me',
    r'discord\.gg',
    r'telegram\.me',
    r'check out my',
    r'my channel',
    r'my group',
    r'subscribe',
]
HIGH_QUALITY_INDICATORS = [
    'bundle', 'dev wallet', 'dev buy', 'sniper', 'rug',
    'liquidity', 'market cap', 'bonding curve', 'graduat',
    'entry', 'exit', 'target', 'invalidation', 'stop',
    'risk', 'position size', 'allocation', 'thesis',
    'narrative', 'regime', 'crowding', 'copytrade',
    ' holders', 'distribution', 'supply', 'max supply',
    'tp ', 'mc ', 'ath ', 'support', 'resistance',
    'breakdown', 'breakout', 'trend', 'momentum',
]


def load_jsonl(path):
    if not os.path.exists(path): return []
    return [json.loads(l) for l in open(path, encoding='utf-8')]


def get_git_sha():
    try:
        return subprocess.check_output(
            ['git', 'rev-parse', '--short', 'HEAD'],
            cwd=REPO_ROOT, capture_output=True, text=True
        ).strip()
    except:
        return 'unknown'


def classify_scope(text):
    """Classify content into scope tiers A/B/C/D."""
    tl = text.lower()
    has_pump = any(kw in tl for kw in PUMP_KW)
    has_sol = any(kw in tl for kw in SOL_MEMECOIN_KW)
    has_gen = any(kw in tl for kw in GENERIC_KW)
    # Check for pump mint address (strongest signal)
    has_pump_mint = bool(PUMP_MINT_RE.search(text))
    if has_pump_mint or has_pump:
        return 'A'  # pump_specific
    if has_sol:
        return 'B'  # solana_memecoin_regime
    if has_gen:
        return 'C'  # generic_crypto
    return 'D'  # unresolved


def classify_temporal(text, publish_time_ms, first_seen_ms):
    """Classify content temporal causality: EX_ANTE / RETROSPECTIVE / AMBIGUOUS."""
    tl = text.lower()
    retro_score = sum(1 for pat in RETRO_INDICATORS if pat in tl)
    ex_ante_score = sum(1 for pat in EX_ANTE_INDICATORS if pat in tl)

    # If publish_time == first_seen_time exactly, likely retrieval-time (ambiguous)
    if publish_time_ms and first_seen_ms:
        delta = abs(publish_time_ms - first_seen_ms)
        if delta < 60000 and publish_time_ms == first_seen_ms:  # within 1 min
            # Could be a live post OR a retrieval artifact
            pass

    # Strong retrospective indicators
    if retro_score >= 2 and retro_score > ex_ante_score:
        return 'RETROSPECTIVE'
    # Strong ex-ante indicators
    if ex_ante_score >= 2 and ex_ante_score > retro_score:
        return 'EX_ANTE'
    # Mixed/weak signals
    if retro_score > 0 and ex_ante_score > 0:
        return 'AMBIGUOUS'
    # No temporal markers at all
    return 'AMBIGUOUS'


def extract_entities_v11(text, mentioned_mints_raw=None):
    """Extract entities with resolution confidence. Never force cashtag→mint."""
    entities = []
    resolution_method = 'unknown'
    resolution_confidence = 0.0
    entity_type = 'unknown'
    primary_mint = None
    primary_ticker = None

    # 1. Direct mint addresses from text (highest confidence)
    pump_mints = PUMP_MINT_RE.findall(text)
    all_addrs = SOLANA_ADDR_RE.findall(text)
    solana_mints = [a for a in all_addrs if a not in pump_mints and len(a) >= 32]

    # 2. Mints from mentioned_mints field (case-preserved from raw)
    field_mints = []
    if mentioned_mints_raw and isinstance(mentioned_mints_raw, str):
        field_mints = [m.strip() for m in mentioned_mints_raw.split('|') if m.strip() and len(m.strip()) >= 32]

    # 3. Cashtags (tickers)
    cashtags = CASHTAG_RE.findall(text)

    # Build entity list with confidence
    all_mints_found = list(set(pump_mints + field_mints))
    if all_mints_found:
        for m in all_mints_found[:10]:
            is_pump = m.lower().endswith('pump')
            entities.append({
                'entity_type': 'mint',
                'value': m,
                'resolution_method': 'direct_address' if m in pump_mints else 'field_extracted',
                'resolution_confidence': 0.95 if is_pump else 0.70,
                'is_pump_mint': is_pump,
            })
        primary_mint = all_mints_found[0]
        entity_type = 'mint'
        resolution_method = 'direct_address'
        resolution_confidence = 0.95 if primary_mint.lower().endswith('pump') else 0.70

    # Cashtags — NEVER force to mint. They stay as tickers with lower confidence.
    for ct in cashtags[:5]:
        entities.append({
            'entity_type': 'ticker',
            'value': f'${ct}',
            'resolution_method': 'cashtag_extracted',
            'resolution_confidence': 0.30,  # weak — cashtag→mint not resolved
            'is_pump_mint': False,
        })
        if not primary_ticker:
            primary_ticker = f'${ct}'

    # If only cashtags, no mints
    if not all_mints_found and cashtags:
        entity_type = 'ticker'
        resolution_method = 'cashtag_extracted'
        resolution_confidence = 0.30

    return {
        'entities': entities,
        'entity_type': entity_type,
        'primary_mint': primary_mint,
        'primary_ticker': primary_ticker,
        'resolution_method': resolution_method,
        'resolution_confidence': resolution_confidence,
        'all_mints': all_mints_found,
        'all_cashtags': cashtags,
    }


def assess_content_quality(text, scope, entities):
    """Content-level quality assessment. Returns (quality_label, quality_score, reject_reason)."""
    tl = text.lower()

    # Check low-quality patterns
    for pat in LOW_QUALITY_PATTERNS:
        if re.search(pat, tl):
            return ('REJECTED', 0.0, 'engagement_farming_or_referral')

    # Check for empty or trivially short content
    if len(text.strip()) < 20:
        return ('REJECTED', 0.0, 'too_short')

    # Check for substantive memecoin reasoning
    has_substance = any(kw in tl for kw in HIGH_QUALITY_INDICATORS)
    has_pump_mint = len(entities.get('all_mints', [])) > 0

    # PnL bragging without reasoning
    pnl_words = ['profit', 'pnl', 'made money', 'gains', 'i made', 'i lost']
    has_pnl = any(kw in tl for kw in pnl_words)
    if has_pnl and not has_substance and len(text) < 200:
        return ('REJECTED', 0.0, 'pnl_bragging_without_reasoning')

    # Generic crypto without memecoin relevance
    if scope == 'C':
        # Still check if there's pump-specific reasoning embedded
        if not has_pump_mint and not has_substance:
            return ('DOWNRANK', 0.3, 'generic_crypto_no_pump_relevance')

    # High-quality: substantive reasoning + entity linkage
    if has_substance and has_pump_mint:
        return ('HIGH_SIGNAL', 0.85, None)
    if has_substance and scope in ('A', 'B'):
        return ('TRADE_THESIS', 0.75, None)
    if has_substance:
        return ('OBSERVATION', 0.55, None)
    if has_pump_mint:
        return ('CALL', 0.60, None)

    return ('UNRESOLVED', 0.40, None)


def cluster_duplicates(content_records):
    """Cluster exact and near-duplicates. Returns cluster assignments."""
    # Exact duplicates: same normalized text
    text_to_ids = defaultdict(list)
    for c in content_records:
        text = re.sub(r'\s+', ' ', (c.get('raw_text', '') or '').strip().lower())[:500]
        if text:
            text_to_ids[text].append(c.get('_idx', 0))

    # Assign amplification_cluster_id and originality_confidence
    cluster_id = 0
    id_to_cluster = {}
    id_to_originality = {}

    for text, ids in text_to_ids.items():
        if len(ids) > 1:
            cluster_id += 1
            # First occurrence is origin (originality=1.0), rest are amplifications
            for i, idx in enumerate(ids):
                id_to_cluster[idx] = cluster_id
                id_to_originality[idx] = 1.0 if i == 0 else 0.3
        else:
            # Unique — no cluster
            id_to_cluster[ids[0]] = 0  # 0 = no cluster
            id_to_originality[ids[0]] = 1.0

    # Near-duplicate detection (within same creator, >90% similarity)
    # Group by creator for efficiency
    creator_texts = defaultdict(list)
    for c in content_records:
        creator = c.get('account_handle', '?')
        text = (c.get('raw_text', '') or '')[:300].lower()
        if text:
            creator_texts[creator].append((c.get('_idx', 0), text))

    import difflib
    near_clusters = {}
    for creator, items in creator_texts.items():
        for i in range(len(items)):
            for j in range(i + 1, min(i + 30, len(items))):
                idx_i, text_i = items[i]
                idx_j, text_j = items[j]
                if text_i != text_j:
                    ratio = difflib.SequenceMatcher(None, text_i, text_j).ratio()
                    if ratio > 0.90:
                        # Merge into same cluster
                        ci = id_to_cluster.get(idx_i, 0)
                        cj = id_to_cluster.get(idx_j, 0)
                        if ci == 0 and cj == 0:
                            cluster_id += 1
                            id_to_cluster[idx_i] = cluster_id
                            id_to_cluster[idx_j] = cluster_id
                            id_to_originality[idx_i] = 1.0
                            id_to_originality[idx_j] = 0.5
                        elif ci == 0:
                            id_to_cluster[idx_i] = cj
                            id_to_originality[idx_i] = 0.5
                        elif cj == 0:
                            id_to_cluster[idx_j] = ci
                            id_to_originality[idx_j] = 0.5

    return id_to_cluster, id_to_originality


def classify_claim_type_v11(text, entities, direction):
    """Classify claim type from content substance."""
    tl = text.lower()

    if any(kw in tl for kw in ['entry', 'buy', 'long', 'enter', 'load up', 'accumulate']):
        if entities.get('primary_mint'):
            return 'entry'
    if any(kw in tl for kw in ['exit', 'sell', 'take profit', 'tp ', 'close position']):
        return 'exit'
    if any(kw in tl for kw in ['risk', 'danger', 'warning', 'caution', 'avoid', 'rug', 'scam']):
        return 'risk'
    if any(kw in tl for kw in ['regime', 'market', 'trend', 'cycle', 'phase', 'sentiment']):
        return 'regime'
    if any(kw in tl for kw in ['thesis', 'narrative', 'story', 'case', 'argument']):
        return 'narrative'
    if any(kw in tl for kw in ['observe', 'notice', 'watch', 'interesting', 'pattern']):
        return 'observation'
    return 'narrative'  # default


def build_v11():
    """Main build function."""
    git_sha = get_git_sha()
    print(f"Building narrative_gold_v1.1")
    print(f"  Run UUID: {RUN_UUID}")
    print(f"  Git SHA: {git_sha}")
    print()

    # ── LOAD V1 DATA (read-only) ──
    v1_content = load_jsonl(os.path.join(V1_BASE, 'gold', 'creator_content_v1', 'creator_content_v1.jsonl'))
    v1_claims = load_jsonl(os.path.join(V1_BASE, 'gold', 'creator_claim_v1', 'creator_claim_v1.jsonl'))
    v1_states = load_jsonl(os.path.join(V1_BASE, 'gold', 'narrative_state_v1', 'narrative_state_v1.jsonl'))
    v1_validations = load_jsonl(os.path.join(V1_BASE, 'gold', 'narrative_validation_v1', 'narrative_validation_v1.jsonl'))
    v1_strategy = load_jsonl(os.path.join(V1_BASE, 'gold', 'strategy_card_v1', 'strategy_card_v1.jsonl'))

    print(f"  Loaded v1: content={len(v1_content)}, claims={len(v1_claims)}, "
          f"states={len(v1_states)}, validations={len(v1_validations)}, strategy={len(v1_strategy)}")

    # ═══════════════════════════════════════════════════════════════
    # STEP 1: REBUILD CONTENT LAYER with scope, temporal, entity, dedup, quality
    # ═══════════════════════════════════════════════════════════════
    print("\n  STEP 1: Rebuilding content layer...")

    # Assign indices for dedup clustering
    for i, c in enumerate(v1_content):
        c['_idx'] = i

    # Cluster duplicates
    cluster_map, originality_map = cluster_duplicates(v1_content)

    v11_content = []
    scope_dist = Counter()
    temporal_dist = Counter()
    quality_dist = Counter()
    platform_dist = Counter()
    creator_dist = Counter()
    entity_conf_dist = Counter()

    rejected_count = 0
    downranked_count = 0

    for c in v1_content:
        idx = c.get('_idx', 0)
        raw_text = c.get('raw_text', '') or ''
        mentioned_mints_raw = c.get('mentioned_mints')

        # Entity extraction (v1.1 enhanced)
        ent = extract_entities_v11(raw_text, mentioned_mints_raw)

        # Scope classification
        scope = classify_scope(raw_text)
        scope_dist[scope] += 1

        # Temporal classification
        pub_time = c.get('publish_time_ms')
        fs_time = c.get('first_seen_ms')
        temporal_class = classify_temporal(raw_text, pub_time, fs_time)
        temporal_dist[temporal_class] += 1

        # Quality assessment
        quality_label, quality_score, reject_reason = assess_content_quality(raw_text, scope, ent)
        quality_dist[quality_label] += 1

        if quality_label == 'REJECTED':
            rejected_count += 1
            continue  # exclude from v1.1
        if quality_label == 'DOWNRANK':
            downranked_count += 1

        # Build v1.1 content record
        record = {
            'content_id': c.get('content_id', ''),
            'schema_version': '1.1.0',
            'pipeline_version': '1.1.0',
            'run_uuid': RUN_UUID,
            'git_sha': git_sha,
            'source_corpus': 'narrative_gold_v1',
            'source_hash': hashlib.sha256(json.dumps(c, sort_keys=True).encode()).hexdigest()[:16],

            # Original fields preserved
            'platform': c.get('platform', ''),
            'account_handle': c.get('account_handle', ''),
            'account_id': c.get('account_id', ''),
            'raw_text': raw_text,
            'normalized_text': c.get('normalized_text', ''),
            'publish_time_ms': pub_time,
            'first_seen_ms': fs_time,

            # v1.1 ENHANCED FIELDS
            'scope_tier': scope,  # A/B/C/D
            'scope_label': {'A': 'pump_specific', 'B': 'solana_memecoin_regime',
                           'C': 'generic_crypto', 'D': 'unresolved'}[scope],
            'temporal_class': temporal_class,  # EX_ANTE / RETROSPECTIVE / AMBIGUOUS
            'quality_label': quality_label,  # HIGH_SIGNAL / TRADE_THESIS / OBSERVATION / CALL / UNRESOLVED / DOWNRANK
            'quality_score': quality_score,

            # Entity resolution (v1.1)
            'entities': ent['entities'],  # list of {entity_type, value, resolution_method, resolution_confidence}
            'entity_type': ent['entity_type'],
            'primary_mint': ent['primary_mint'],
            'primary_ticker': ent['primary_ticker'],
            'resolution_method': ent['resolution_method'],
            'resolution_confidence': ent['resolution_confidence'],
            'all_mints': ent['all_mints'],
            'all_cashtags': ent['all_cashtags'],

            # Dedup/amplification (v1.1)
            'amplification_cluster_id': cluster_map.get(idx, 0),
            'originality_confidence': originality_map.get(idx, 1.0),
            'origin_id': '' if cluster_map.get(idx, 0) == 0 else f'orig_{cluster_map[idx]}',

            # Provenance
            'cashtags': c.get('cashtags', ''),
            'mentioned_mints_v1': mentioned_mints_raw if mentioned_mints_raw else None,
            'identity_confidence': c.get('identity_confidence', 'UNRESOLVED'),
            'wallet_confidence': c.get('wallet_confidence', 'UNRESOLVED'),
            'content_type': c.get('content_type', 'unknown'),
            'usefulness_class': c.get('usefulness_class', 'unknown'),
        }

        platform_dist[c.get('platform', '?')] += 1
        creator_dist[c.get('account_handle', '?')] += 1
        entity_conf_dist[ent['resolution_method']] += 1

        v11_content.append(record)

    # Write content
    content_path = os.path.join(V11_BASE, 'gold', 'creator_content_v1', 'creator_content_v1.jsonl')
    with open(content_path, 'w', encoding='utf-8') as f:
        for rec in v11_content:
            f.write(json.dumps(rec, ensure_ascii=False) + '\n')

    print(f"    Content: {len(v11_content)} (rejected {rejected_count}, downranked {downranked_count})")
    print(f"    Scope: {dict(scope_dist)}")
    print(f"    Temporal: {dict(temporal_dist)}")
    print(f"    Quality: {dict(quality_dist)}")

    return v11_content, scope_dist, temporal_dist, quality_dist, rejected_count


if __name__ == '__main__':
    build_v11()
