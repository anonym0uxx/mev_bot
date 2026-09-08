#!/usr/bin/env python
"""
build_creator_content_v1.py — Gold Layer 1: cleaned/deduped chronological content

Transforms raw_social_event_v1 → creator_content_v1.

TRANSFORMATIONS:
  1. Text normalization (lowercase, URL-stripped, slang-preserved)
  2. Dedup via text_hash (collapse reposts/copies, preserve echo chain)
  3. Content type classification (post|thread|space|video|message|call|chart|tracker_alert)
  4. Slang/idiom detection
  5. Ex-ante vs ex-post temporal classification
  6. Quality scoring: signal_density, originality, memecoin_relevance
  7. Identity resolution against seeds registry
  8. Admission: GOLD | REJECTED | UNRESOLVED

ADMISSION GATE (fail-closed):
  - REJECTED: empty text, pure image/markdown artifacts, navigation noise
  - REJECTED: PROMOTIONAL_SHILL with no substantive reasoning
  - REJECTED: DUPLICATE (exact hash match already admitted)
  - GOLD: HIGH_SIGNAL_REASONING | STRATEGY | TRADE_THESIS | NARRATIVE_ANALYSIS |
          RISK_POSTMORTEM | TOOL_WALLET_SIGNAL (if substantive)
  - UNRESOLVED: insufficient evidence to classify

USAGE:
  python src/build_creator_content_v1.py [--raw-path ...] [--output-dir ...]
"""

from __future__ import annotations
import os, sys, json, hashlib, re, time, argparse, yaml
from pathlib import Path
from dataclasses import asdict
from datetime import datetime, timezone

# Add parent for schema imports
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from schemas.narrative_gold_v1 import (
    SCHEMA_VERSION, PIPELINE_VERSION, GENERATOR_VERSION,
    content_id, USEFULNESS_CLASSES, IDENTITY_CONFIDENCE, WALLET_CONFIDENCE,
    CreatorContentV1,
)

# ─── Constants ──────────────────────────────────────────────────────────────

RUN_UUID_SOURCE = "creator_content_v1"

# Slang/idiom patterns for memecoin trading
SLANG_PATTERNS = [
    (r'\bnfa\b', 'nfa'),
    (r'\bdytdy\b', 'dytdy'),
    (r'\bwagmi\b', 'wagmi'),
    (r'\bngmi\b', 'ngmi'),
    (r'\bgm\b', 'gm'),
    (r'\bgigabased\b', 'gigabased'),
    (r'\bbased\b', 'based'),
    (r'\bdegen\b', 'degen'),
    (r'\baped?\b', 'ape'),
    (r'\balpha\b', 'alpha'),
    (r'\bjeet(ed|ing)?\b', 'jeet'),
    (r'\brug(pull)?(ed|ing)?\b', 'rugpull'),
    (r'\bnfd\b', 'nfd'),
    (r'\bmaxi\b', 'maxi'),
    (r'\bglow\b', 'glow'),
    (r'\bser\b', 'ser'),
    (r'\bchad\b', 'chad'),
    (r'\bcope\b', 'cope'),
    (r'\bmoon(ship)?(ed|ing)?\b', 'moonship'),
    (r'\bply\b', 'ply'),
    (r'\bkol\b', 'kol'),
    (r'\bply\b', 'ply'),
    (r'\bszn\b', 'szn'),
    (r'\bfp\b', 'fp'),
    (r'\bmc\b', 'mc'),
    (r'\btp\b', 'tp'),
    (r'\bsl\b', 'sl'),
    (r'\bdca\b', 'dca'),
    (r'\bgwei\b', 'gwei'),
    (r'\bnfty\b', 'nfty'),
    (r'\bmilly\b', 'milly'),
    (r'\bpnl\b', 'pnl'),
    (r'\bmcap\b', 'mcap'),
]

# Content type detection patterns
CONTENT_TYPE_PATTERNS = [
    (r'youtube\.com|youtu\.be', 'video'),
    (r'twitch\.tv', 'video'),
    (r't\.me/s/', 'message'),
    (r'dexscreener\.com', 'chart'),
    (r'pump\.fun', 'tracker_alert'),
    (r'twitter\.com|x\.com', 'post'),
]

# Ex-ante indicators (content published BEFORE the outcome would be known)
EX_ANTE_INDICATORS = [
    r'will\s+', r'going\s+to\s+', r'about\s+to\s+', r'expect',
    r'predict', r'forecast', r'target\s+', r'entry\s+', r'buying\s+zone',
    r'accumulate', r'loading\s+up', r'getting\s+in', r'preparing',
]
EX_POST_INDICATORS = [
    r'sold\s+', r'exited\s+', r'took\s+profit', r'stopped\s+out',
    r'rekt\b', r'loss\b', r'profit\s+taken', r'bag\s+held',
    r'postmortem', r'review', r'recap', r'retrospect',
    r'missed\s+', r'should\s+have', r'if\s+only',
]


def get_git_sha() -> str:
    """Get current git SHA."""
    import subprocess
    try:
        result = subprocess.run(['git', 'rev-parse', '--short=7', 'HEAD'],
                                capture_output=True, text=True,
                                cwd=os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
        return result.stdout.strip() or "unknown"
    except Exception:
        return "unknown"


def load_seeds(seeds_path: str) -> dict:
    """Load seeds YAML for identity resolution."""
    try:
        with open(seeds_path, 'r', encoding='utf-8') as f:
            return yaml.safe_load(f) or {}
    except Exception as e:
        print(f"  [WARN] Could not load seeds: {e}")
        return {}


def normalize_text(raw: str) -> str:
    """Lowercase, strip URLs, preserve slang."""
    text = raw.lower()
    # Strip markdown image/link artifacts but keep visible text
    text = re.sub(r'!\?\[([^\]]*)\]\([^)]+\)', r'\1', text)  # [alt](url)
    text = re.sub(r'!\[([^\]]*)\]\([^)]+\)', '', text)       # ![alt](url) images
    text = re.sub(r'\[([^\]]*)\]\([^)]+\)', r'\1', text)     # [text](url)
    # Strip bare URLs
    text = re.sub(r'https?://\S+', '', text)
    # Strip telegram image placeholders
    text = re.sub(r'\[_?[^_\]]*\]\s*$', '', text)
    # Collapse whitespace
    text = re.sub(r'\s+', ' ', text).strip()
    return text


def detect_slang(text: str) -> list:
    """Detect slang/idioms in text."""
    found = []
    lower = text.lower()
    for pattern, term in SLANG_PATTERNS:
        if re.search(pattern, lower):
            found.append(term)
    return list(set(found))


def classify_content_type(text: str, url: str, platform: str) -> str:
    """Classify content type based on URL and text patterns."""
    url_lower = url.lower()
    for pattern, ctype in CONTENT_TYPE_PATTERNS:
        if re.search(pattern, url_lower):
            return ctype
    if platform == 'telegram':
        return 'message'
    if platform == 'youtube':
        return 'video'
    if platform == 'twitch':
        return 'video'
    if platform == 'x':
        return 'post'
    if platform == 'web':
        return 'post'
    return 'post'


def classify_temporal(text: str, publish_time_ms: int, first_seen_ms: int) -> tuple:
    """Classify as EX_ANTE, EX_POST, or AMBIGUOUS."""
    lower = text.lower()
    has_ex_ante = any(re.search(p, lower) for p in EX_ANTE_INDICATORS)
    has_ex_post = any(re.search(p, lower) for p in EX_POST_INDICATORS)
    if has_ex_ante and not has_ex_post:
        return ('EX_ANTE', 'ex-ante indicators present')
    if has_ex_post and not has_ex_ante:
        return ('EX_POST', 'ex-post indicators present')
    if has_ex_ante and has_ex_post:
        return ('AMBIGUOUS', 'mixed ex-ante/ex-post indicators')
    return ('AMBIGUOUS', 'no clear temporal indicators')


def score_signal_density(text: str, usefulness_class: str) -> float:
    """Score signal density: substantive content ratio."""
    if not text:
        return 0.0
    # High-signal classes get baseline boost
    baseline = {
        'HIGH_SIGNAL_REASONING': 0.6,
        'STRATEGY': 0.5,
        'TRADE_THESIS': 0.5,
        'NARRATIVE_ANALYSIS': 0.5,
        'RISK_POSTMORTEM': 0.6,
        'TOOL_WALLET_SIGNAL': 0.3,
        'LOW_SIGNAL_CHATTER': 0.05,
        'PROMOTIONAL_SHILL': 0.0,
        'DUPLICATE': 0.0,
        'UNRESOLVED': 0.1,
    }.get(usefulness_class, 0.1)

    # Adjust based on text characteristics
    word_count = len(text.split())
    if word_count < 3:
        return max(0.0, baseline - 0.3)
    # Longer content with substance words scores higher
    substance_words = re.findall(
        r'\b( entry|exit|stop|target|risk|size|position|accumulate|'
        r'rug|dev|wallet|bond|curve|market|cap|liquidity|graduat|'
        r'solana|pump|swap|bonding|strategy|thesis|reason|because|'
        r'analysis|signal|divergence|momentum|volume|price|trend)\b',
        text.lower()
    )
    substance_ratio = min(1.0, len(substance_words) / max(1, word_count) * 3)
    return min(1.0, baseline * 0.5 + substance_ratio * 0.5)


def score_originality(text: str, text_hash: str, seen_hashes: dict) -> float:
    """Score originality: 1=original, 0=exact copy."""
    if text_hash in seen_hashes:
        return 0.0  # exact duplicate
    return 1.0


def score_memecoin_relevance(text: str, cashtags: str, contract_addresses: str,
                              platform: str) -> float:
    """Score memecoin relevance: 0=irrelevant, 1=directly relevant."""
    lower = text.lower()
    score = 0.0

    # Direct memecoin keywords
    memecoin_keywords = [
        'memecoin', 'memecoins', 'pump.fun', 'pumpfun', 'pump and dump',
        'rugpull', 'rug pull', 'jeet', 'dev buy', 'dev wallet',
        'bonding curve', 'graduate', 'graduation', 'pumpswap',
        'solana', '$sol', 'milly', 'mcap', 'alpha call',
        'degen', 'ape', 'nfd', 'entry', 'exit',
    ]
    for kw in memecoin_keywords:
        if kw in lower:
            score += 0.1

    # Has contract addresses (Solana mint)
    if contract_addresses and 'pump' in (contract_addresses.lower() or ''):
        score += 0.3

    # Has cashtags
    if cashtags and len(cashtags) > 1:
        score += 0.1

    return min(1.0, score)


def resolve_identity(account_handle: str, account_id: str, platform: str,
                     seeds: dict) -> tuple:
    """Resolve identity against seeds. Returns (identity_confidence, wallet_confidence).
    Matches against x_handle, twitch_handle, and aliases across elite_creators and additional_kols."""
    if not seeds:
        return ('UNRESOLVED', 'NONE')

    handle_lower = account_handle.lower()

    # Check creators (elite creators are under 'creators' key)
    elite_creators = seeds.get('creators', seeds.get('elite_creators', {}))
    for name, info in elite_creators.items():
        # Check all possible handle fields
        handle_fields = ['x_handle', 'twitch_handle', 'twitch_twitter', 'handle']
        for field in handle_fields:
            val = info.get(field)
            if val and isinstance(val, str) and val.lower() == handle_lower:
                wallets = info.get('wallets', [])
                return ('HIGH', 'HIGH' if wallets else 'NONE')

        # Check aliases
        aliases = info.get('aliases', [])
        for alias in aliases:
            if alias.lower() == handle_lower:
                wallets = info.get('wallets', [])
                return ('HIGH', 'HIGH' if wallets else 'NONE')

        # Check twitch handle specifically (twitch.tv/<handle> → <handle>)
        twitch_handle = info.get('twitch_handle')
        if twitch_handle:
            # Extract just the handle part if it's a URL
            th = twitch_handle.split('/')[-1] if '/' in twitch_handle else twitch_handle
            if th.lower() == handle_lower:
                wallets = info.get('wallets', [])
                return ('HIGH', 'HIGH' if wallets else 'NONE')

    # Check additional_kols
    additional_kols = seeds.get('additional_kols', {})
    for name, info in additional_kols.items():
        x_handle = info.get('x_handle', info.get('handle', ''))
        if x_handle and isinstance(x_handle, str) and x_handle.lower() == handle_lower:
            return ('MEDIUM', 'NONE')
        # Also check the key name itself
        if name.lower() == handle_lower:
            return ('MEDIUM', 'NONE')

    # Check twitch_streamers (may be a list or dict)
    twitch_streamers = seeds.get('twitch_streamers', {})
    if isinstance(twitch_streamers, dict):
        for name, info in twitch_streamers.items():
            handle = info.get('handle', name)
            if handle and isinstance(handle, str) and handle.lower() == handle_lower:
                return ('MEDIUM', 'NONE')
    elif isinstance(twitch_streamers, list):
        for info in twitch_streamers:
            if isinstance(info, dict):
                handle = info.get('handle', '')
                if handle and isinstance(handle, str) and handle.lower() == handle_lower:
                    return ('MEDIUM', 'NONE')

    return ('UNRESOLVED', 'NONE')


def is_rejectable(raw_event: dict, normalized: str) -> tuple:
    """Check if event should be rejected. Returns (should_reject, reason)."""
    if not normalized or len(normalized.strip()) < 10:
        return (True, 'empty_or_trivial_text')
    # Navigation/link-only noise
    if re.match(r'^\[?_?\]?\s*$', normalized):
        return (True, 'navigation_noise')
    # Pure image placeholder
    if re.match(r'^\[.*?\]\s*$', normalized) and len(normalized) < 20:
        return (True, 'image_placeholder_only')
    usefulness = raw_event.get('usefulness_class', 'UNRESOLVED')
    if usefulness == 'PROMOTIONAL_SHILL':
        return (True, 'promotional_shill')
    if usefulness == 'DUPLICATE':
        return (True, 'duplicate_flagged')
    return (False, '')


def build_creator_content(raw_path: str, output_dir: str, seeds_path: str) -> dict:
    """Build creator_content_v1 from raw_social_event_v1."""
    git_sha = get_git_sha()
    run_uuid = f"cc_{int(time.time())}"
    seeds = load_seeds(seeds_path)

    # Load raw events - support both single file and directory
    if os.path.isdir(raw_path):
        raw_events = []
        raw_files = sorted([f for f in os.listdir(raw_path) if f.endswith('.jsonl')])
        for rf in raw_files:
            rfp = os.path.join(raw_path, rf)
            with open(rfp, 'r', encoding='utf-8') as f:
                for line in f:
                    if line.strip():
                        try:
                            raw_events.append(json.loads(line))
                        except json.JSONDecodeError:
                            continue
        print(f"  Loaded {len(raw_events)} raw events from {len(raw_files)} files")
    else:
        with open(raw_path, 'r', encoding='utf-8') as f:
            raw_events = [json.loads(line.strip()) for line in f if line.strip()]
        print(f"  Loaded {len(raw_events)} raw events")

    output_records = []
    rejected_count = 0
    duplicate_count = 0
    seen_text_hashes = {}  # text_hash → first content_id (for echo detection)
    stats = {'GOLD': 0, 'REJECTED': 0, 'UNRESOLVED': 0}

    for i, raw in enumerate(raw_events):
        raw_text = raw.get('text', '') or raw.get('content_text', '')
        text_hash = raw.get('text_hash', raw.get('content_hash', hashlib.sha256(raw_text.encode()).hexdigest()))
        normalized = normalize_text(raw_text)

        # Rejection check
        should_reject, reject_reason = is_rejectable(raw, normalized)
        if should_reject:
            rejected_count += 1
            stats['REJECTED'] += 1
            continue

        # Dedup via text_hash
        is_original = text_hash not in seen_text_hashes
        echo_of = None
        echo_chain = 1
        dup_group = None

        if not is_original:
            duplicate_count += 1
            first_content = seen_text_hashes[text_hash]
            echo_of = first_content
            echo_chain = 2  # At minimum a repost
            dup_group = first_content[:8]
            # Skip duplicates for GOLD admission but record them
            stats['REJECTED'] += 1
            continue

        # Build content_id
        source = raw.get('platform', 'unknown')
        source_id = raw.get('source_id', str(i))
        cid = content_id(source, source_id, text_hash)
        seen_text_hashes[text_hash] = cid

        # Classification
        url = raw.get('url', '') or raw.get('source_url', '')
        platform = raw.get('platform', 'unknown')
        content_type = classify_content_type(raw_text, url, platform)
        slang = detect_slang(normalized)
        temporal_class, temporal_reason = classify_temporal(normalized,
                                                            raw.get('publish_time_ms', 0),
                                                            raw.get('first_seen_ms', 0))

        # Quality scoring
        usefulness = raw.get('usefulness_class', 'UNRESOLVED')
        signal_density = score_signal_density(normalized, usefulness)
        originality = score_originality(normalized, text_hash, seen_text_hashes)
        memecoin_rel = score_memecoin_relevance(normalized,
                                                raw.get('cashtags', ''),
                                                raw.get('contract_addresses', ''),
                                                platform)

        # Identity resolution
        identity_conf, wallet_conf = resolve_identity(
            raw.get('account_handle', ''),
            raw.get('account_id', ''),
            platform,
            seeds
        )

        # Admission decision
        if usefulness in ('HIGH_SIGNAL_REASONING', 'STRATEGY', 'TRADE_THESIS',
                          'NARRATIVE_ANALYSIS', 'RISK_POSTMORTEM', 'TOOL_WALLET_SIGNAL'):
            if signal_density > 0.15 or len(normalized) > 100:
                admission = 'GOLD'
                stats['GOLD'] += 1
            else:
                admission = 'UNRESOLVED'
                stats['UNRESOLVED'] += 1
        elif usefulness == 'UNRESOLVED':
            # Potential GOLD if substantive
            if len(normalized) > 50 and memecoin_rel > 0.2:
                admission = 'UNRESOLVED'  # Keep for further analysis
                stats['UNRESOLVED'] += 1
            else:
                admission = 'REJECTED'
                stats['REJECTED'] += 1
                rejected_count += 1
        else:
            admission = 'REJECTED'
            stats['REJECTED'] += 1
            rejected_count += 1

        if admission == 'REJECTED':
            continue

        # Build record
        record = CreatorContentV1(
            content_id=cid,
            schema_version=SCHEMA_VERSION,
            pipeline_version=PIPELINE_VERSION,
            run_uuid=run_uuid,
            source_hash=text_hash[:16],
            code_config_hash=git_sha,
            git_sha=git_sha,

            platform=platform,
            account_handle=raw.get('account_handle', '') or raw.get('author_handle', ''),
            account_id=raw.get('account_id', ''),
            account_type=raw.get('account_type', 'anonymous'),
            source_id=source_id,
            url=url,

            publish_time_ms=raw.get('publish_time_ms', 0),
            first_seen_ms=raw.get('first_seen_ms', 0),
            ingestion_latency_ms=raw.get('ingestion_latency_ms', 0),

            raw_text=raw_text[:50000],  # Cap at 50K for YouTube transcripts
            normalized_text=normalized[:50000],
            text_hash=text_hash,
            language='en' if not re.search(r'[\u0400-\u04FF]', normalized) else 'ru',
            slang_terms='|'.join(slang) if slang else None,
            content_type=content_type,

            is_original=is_original,
            echo_of_content_id=echo_of,
            echo_chain_length=echo_chain,
            duplicate_group_id=dup_group,

            cashtags=raw.get('cashtags'),
            contract_addresses=raw.get('contract_addresses'),
            mentioned_mints=raw.get('mentioned_mints'),
            mentioned_tokens=raw.get('mentioned_tokens'),

            engagement_likes=raw.get('engagement_likes'),
            engagement_reposts=raw.get('engagement_reposts'),
            engagement_replies=raw.get('engagement_replies'),
            engagement_views=raw.get('engagement_views'),

            temporal_classification=temporal_class,
            temporal_reason=temporal_reason,

            source_provenance=f"{platform}:{raw.get('retrieval_method', 'unknown')}",
            raw_payload_path=raw.get('raw_payload_path', ''),

            admission_status=admission,
            rejection_reason=None if admission != 'REJECTED' else reject_reason,
            contamination_flags=raw.get('contamination_flags'),

            usefulness_class=usefulness,
            signal_density=round(signal_density, 3),
            originality_score=round(originality, 3),
            memecoin_relevance=round(memecoin_rel, 3),
            identity_confidence=identity_conf,
            wallet_confidence=wallet_conf,
        )

        if admission in ('GOLD', 'UNRESOLVED'):
            output_records.append(asdict(record))

    # Write output
    os.makedirs(output_dir, exist_ok=True)
    output_path = os.path.join(output_dir, 'creator_content_v1.jsonl')
    with open(output_path, 'w', encoding='utf-8') as f:
        for rec in output_records:
            f.write(json.dumps(rec) + '\n')

    # Manifest
    manifest = {
        'layer': 'creator_content_v1',
        'schema_version': SCHEMA_VERSION,
        'pipeline_version': PIPELINE_VERSION,
        'run_uuid': run_uuid,
        'git_sha': git_sha,
        'input_raw_path': raw_path,
        'output_path': output_path,
        'total_raw_events': len(raw_events),
        'total_output_records': len(output_records),
        'rejected_count': rejected_count,
        'duplicate_count': duplicate_count,
        'admission_stats': stats,
        'created_at': datetime.now(timezone.utc).isoformat(),
    }
    manifest_path = os.path.join(output_dir, 'creator_content_v1_manifest.json')
    with open(manifest_path, 'w', encoding='utf-8') as f:
        json.dump(manifest, f, indent=2)

    return manifest


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--raw-path', default='output/narrative_gold_v1/raw/raw_social_event_v1')
    parser.add_argument('--output-dir', default='output/narrative_gold_v1/gold/creator_content_v1')
    parser.add_argument('--seeds-path', default='schemas/narrative_seeds_v1.yaml')
    args = parser.parse_args()

    print("=" * 70)
    print("CREATOR_CONTENT_V1 — Gold Layer 1 Builder")
    print("=" * 70)

    manifest = build_creator_content(args.raw_path, args.output_dir, args.seeds_path)

    print(f"\n  Total raw events:   {manifest['total_raw_events']}")
    print(f"  Output records:     {manifest['total_output_records']}")
    print(f"  Rejected:           {manifest['rejected_count']}")
    print(f"  Duplicates skipped: {manifest['duplicate_count']}")
    print(f"  Admission stats:    {manifest['admission_stats']}")
    print(f"\n  Output: {manifest['output_path']}")
    print(f"  Manifest: {manifest['manifest_path'] if 'manifest_path' in manifest else 'N/A'}")
    print(f"  Run UUID: {manifest['run_uuid']}")
