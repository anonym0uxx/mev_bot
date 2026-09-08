#!/usr/bin/env python
"""
acquire_slinky_mint_narrative.py - Search for narrative content about specific
Slinky v3 mints (tokens) from June 4 - July 14, 2026.

Uses multiple free sources to find historical mentions of Slinky tokens:
1. DexScreener API (free, no key) - token metadata + historical data
2. Web search via Firecrawl /v1/search - find articles/tweets/posts mentioning token symbols
3. YouTube search via Firecrawl - find videos about specific tokens
4. pump.fun API - token launch data, holder counts
5. Wayback CDX - search for archived tweets mentioning token symbols ($SYMBOL)

This bridges the gap: we have Slinky on-chain outcomes but need narrative content
from the SAME time period to validate claims against real price action.

Usage: python src/acquire_slinky_mint_narrative.py [--max-tokens N] [--graduated-only]
"""

import json
import os
import sys
import time
import hashlib
import argparse
import subprocess
import urllib.request
import urllib.parse
from datetime import datetime, timezone
from collections import Counter

# --- Config ---
FIRECRAWL_URL = "http://127.0.0.1:3102"
SLINKY_TARGETS = "output/narrative_gold_v1/slinky_target_mints_enriched.json"
OUTPUT_DIR = "output/narrative_gold_v1/raw/raw_social_event_v1"
RUN_UUID = f"sm_{int(time.time()):08x}"

def get_git_sha():
    try:
        return subprocess.check_output(
            ['git', 'rev-parse', '--short', 'HEAD'],
            cwd='D:/repos/mev_bot', stderr=subprocess.DEVNULL
        ).decode().strip()
    except Exception:
        return 'unknown'

GIT_SHA = get_git_sha()

def fetch_json(url, timeout=30):
    """Fetch JSON from a URL."""
    try:
        req = urllib.request.Request(url, headers={'User-Agent': 'Mozilla/5.0'})
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return json.loads(resp.read().decode())
    except Exception as e:
        return None

def fetch_text(url, timeout=30):
    """Fetch raw text from a URL."""
    try:
        req = urllib.request.Request(url, headers={'User-Agent': 'Mozilla/5.0'})
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return resp.read().decode('utf-8', errors='replace')
    except Exception as e:
        return None

def firecrawl_search(query, limit=5):
    """Search via local Firecrawl /v1/search."""
    try:
        payload = json.dumps({"query": query, "limit": limit, "scrapeOptions": {"formats": ["markdown"]}}).encode()
        req = urllib.request.Request(
            f"{FIRECRAWL_URL}/v1/search",
            data=payload,
            headers={'Content-Type': 'application/json'},
            method='POST'
        )
        with urllib.request.urlopen(req, timeout=60) as resp:
            return json.loads(resp.read().decode())
    except Exception as e:
        return None

def firecrawl_scrape(url, wait_ms=4000):
    """Scrape a URL via local Firecrawl /v1/scrape."""
    try:
        payload = json.dumps({
            "url": url,
            "formats": ["markdown"],
            "waitFor": wait_ms
        }).encode()
        req = urllib.request.Request(
            f"{FIRECRAWL_URL}/v1/scrape",
            data=payload,
            headers={'Content-Type': 'application/json'},
            method='POST'
        )
        with urllib.request.urlopen(req, timeout=90) as resp:
            return json.loads(resp.read().decode())
    except Exception as e:
        return None

def search_dexscreener(token_symbol_or_mint):
    """Search DexScreener for a token. Returns token data including price history."""
    # DexScreener free API: https://api.dexscreener.com/latest/dex/search?q=<query>
    url = f"https://api.dexscreener.com/latest/dex/search?q={urllib.parse.quote(token_symbol_or_mint)}"
    return fetch_json(url, timeout=20)

def search_wayback_cdx_symbol(symbol, from_date="20260604", to_date="20260714", limit=20):
    """Search Wayback CDX for tweets mentioning a token symbol ($SYMBOL).
    Uses urlsearch to find archived pages containing the symbol."""
    # CDX content search: search for twitter search URLs that contain the symbol
    queries = [
        f"twitter.com/search?q=%24{symbol}&filter=mimetype:application/json",
        f"twitter.com/search?q=%24{symbol}+pump&filter=mimetype:application/json",
    ]
    results = []
    for q in queries:
        cdx_url = f"https://web.archive.org/cdx/search/cx?url={q}&from={from_date}&to={to_date}&limit={limit}&filter=mimetype:application/json"
        data = fetch_text(cdx_url, timeout=30)
        if data and data.strip():
            for line in data.strip().split('\n'):
                parts = line.split()
                if len(parts) >= 3:
                    url_part = parts[2] if len(parts) > 2 else parts[1]
                    timestamp = parts[1]
                    results.append({'timestamp': timestamp, 'url': url_part})
        time.sleep(3)
    return results

def make_event(event_type, mint, token_symbol, content, source_url, platform,
               publish_time_ms, extra=None):
    """Create a raw_social_event_v1 record."""
    now_ms = int(time.time() * 1000)
    event = {
        'event_id': hashlib.sha256(f"{platform}|{source_url}|{content[:100]}".encode()).hexdigest()[:16],
        'schema_version': '1.0.0',
        'pipeline_version': '1.1.0',
        'run_uuid': RUN_UUID,
        'git_sha': GIT_SHA,

        'platform': platform,
        'source': source_url,
        'event_type': event_type,
        'source_type': 'slinky_mint_narrative',

        'content': content[:5000],
        'content_hash': hashlib.sha256(content.encode()).hexdigest()[:16],

        'primary_mint': mint,
        'token_symbol': token_symbol,

        'publish_time_ms': publish_time_ms,
        'first_seen_ms': now_ms,
        'retrieval_time_ms': now_ms,

        'usefulness_class': classify_content(content),
        'quality_score': 0,
        'provenance': 'historical_backfill_slinky_overlap',
        'slinky_overlap': True,
    }
    if extra:
        event.update(extra)
    event['quality_score'] = compute_quality(event)
    return event

def classify_content(text):
    """Classify content usefulness."""
    text_lower = text.lower()
    if len(text) < 20:
        return 'LOW_SIGNAL_CHATTER'
    # Look for trade reasoning keywords
    reasoning_kw = ['because', 'analysis', 'thesis', 'entry', 'exit', 'stop loss',
                    'target', 'support', 'resistance', 'breakout', 'rug', 'dev',
                    'liquidity', 'bonding curve', 'graduat', 'market cap',
                    'reasoning', 'conviction', 'accumulate', 'distribution']
    has_reasoning = sum(1 for kw in reasoning_kw if kw in text_lower)
    if has_reasoning >= 3:
        return 'HIGH_SIGNAL_REASONING'
    elif has_reasoning >= 1:
        return 'STRATEGY'
    # Shill/promotional
    shill_kw = ['buy now', 'gem', 'moon', 'pump', 'don\'t miss', 'fomo', 'referral',
                'join my', 'telegram channel', 'early', 'next 100x']
    has_shill = sum(1 for kw in shill_kw if kw in text_lower)
    if has_shill >= 2:
        return 'PROMOTIONAL_SHILL'
    return 'UNRESOLVED'

def compute_quality(event):
    """Compute quality score 0-100."""
    score = 0
    cls = event.get('usefulness_class', '')
    if cls in ('HIGH_SIGNAL_REASONING', 'STRATEGY', 'TRADE_THESIS'):
        score += 40
    elif cls == 'UNRESOLVED':
        score += 10
    elif cls == 'PROMOTIONAL_SHILL':
        score += 5
    # Content length bonus
    content_len = len(event.get('content', ''))
    if content_len > 200:
        score += 20
    elif content_len > 100:
        score += 10
    # Slinky overlap bonus
    if event.get('slinky_overlap'):
        score += 20
    # Token symbol present
    if event.get('token_symbol'):
        score += 10
    return min(score, 100)

def acquire_for_token(token, all_events):
    """Acquire narrative content for a single Slinky token."""
    mint = token['mint']
    symbol = token.get('token_symbol', '')
    name = token.get('token_name', '')
    time_str = token.get('time_str', '')
    graduated = token.get('graduated', False)
    ret_bp = token.get('ret_300s_bp', 0)
    mfe_bp = token.get('mfe_bp', 0)

    if not symbol or symbol == 'None':
        return 0

    events_added = 0
    publish_ts = token.get('time_ms', int(time.time() * 1000))

    # 1. DexScreener - get token data (price history, liquidity, volume)
    ds_data = search_dexscreener(symbol)
    if ds_data and ds_data.get('pairs'):
        for pair in ds_data['pairs'][:2]:
            dex_text = f"Token: ${symbol} | DEX: {pair.get('dexId', '?')} | "
            dex_text += f"Price: ${pair.get('priceUsd', '?')} | "
            dex_text += f"Liquidity: ${pair.get('liquidity', '?')} | "
            dex_text += f"Volume 24h: ${pair.get('volume', {}).get('h24', '?')} | "
            dex_text += f"TXNs 24h: {pair.get('txns', {}).get('h24', {}).get('buys', 0)} buys / {pair.get('txns', {}).get('h24', {}).get('sells', 0)} sells | "
            dex_text += f"Mcap: ${pair.get('marketCap', '?')} | "
            dex_text += f"URL: {pair.get('url', '')}"
            ev = make_event(
                'token_dexscreener_data', mint, symbol, dex_text,
                pair.get('url', f"dexscreener:{symbol}"), 'dexscreener',
                publish_ts, {'pair_data': pair}
            )
            all_events.append(ev)
            events_added += 1
        time.sleep(0.5)  # DexScreener rate limit

    # 2. Web search for token mentions (articles, tweets, posts)
    search_queries = [
        f"${symbol} pump.fun memecoin solana {time_str}",
        f"${symbol} token solana meme coin",
    ]
    for query in search_queries:
        results = firecrawl_search(query, limit=3)
        if results and results.get('success') and results.get('data'):
            for item in results['data']:
                content = item.get('markdown', '') or item.get('content', '')
                if content and len(content) > 50:
                    # Check if it mentions the symbol
                    if symbol.lower() in content.lower()[:2000]:
                        url = item.get('url', item.get('metadata', {}).get('sourceURL', ''))
                        ev = make_event(
                            'token_web_mention', mint, symbol, content[:5000],
                            url, 'web_search', publish_ts,
                            {'search_query': query}
                        )
                        all_events.append(ev)
                        events_added += 1
        time.sleep(2)

    # 3. Wayback CDX - search for archived Twitter search results for this symbol
    cdx_results = search_wayback_cdx_symbol(symbol)
    for r in cdx_results[:5]:
        # Fetch the archived search results page
        wb_url = f"https://web.archive.org/web/{r['timestamp']}id_/{r['url']}"
        data = fetch_json(wb_url, timeout=30)
        if data and 'data' in data:
            for tweet in data['data'][:5]:
                tweet_text = tweet.get('text', '')
                if tweet_text and len(tweet_text) > 20:
                    created_at = tweet.get('created_at', '')
                    try:
                        pub_ms = int(datetime.strptime(created_at, '%a %b %d %H:%M:%S %Y').timestamp() * 1000)
                    except Exception:
                        pub_ms = publish_ts
                    ev = make_event(
                        'token_twitter_mention', mint, symbol, tweet_text,
                        wb_url, 'twitter', pub_ms,
                        {'engagement': {
                            'likes': tweet.get('favorite_count', 0),
                            'retweets': tweet.get('retweet_count', 0),
                            'replies': tweet.get('reply_count', 0),
                        }, 'tweet_id': tweet.get('id_str', '')}
                    )
                    all_events.append(ev)
                    events_added += 1
        time.sleep(3)

    return events_added

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--max-tokens', type=int, default=50, help='Max tokens to search')
    parser.add_argument('--graduated-only', action='store_true', help='Only search graduated mints')
    parser.add_argument('--min-mfe', type=int, default=5000, help='Min MFE in bp')
    parser.add_argument('--output', default=None, help='Output file override')
    args = parser.parse_args()

    print("=" * 70)
    print("SLINKY MINT NARRATIVE ACQUISITION")
    print("Historical content search for Slinky v3 tokens (June 4 - July 14, 2026)")
    print("=" * 70)
    print(f"  Run UUID:  {RUN_UUID}")
    print(f"  Git SHA:   {GIT_SHA}")
    print(f"  Max tokens: {args.max_tokens}")
    print(f"  Graduated only: {args.graduated_only}")
    print(f"  Min MFE: {args.min_mfe} bp")

    # Load enriched targets
    with open(SLINKY_TARGETS) as f:
        targets = json.load(f)
    print(f"\n  Total Slinky targets loaded: {len(targets)}")

    # Filter
    if args.graduated_only:
        targets = [t for t in targets if t.get('graduated')]
        print(f"  After graduated filter: {len(targets)}")
    else:
        # Filter to high-quality: graduated OR high MFE
        targets = [t for t in targets if t.get('graduated') or t.get('mfe_bp', 0) >= args.min_mfe]
        print(f"  After MFE>={args.min_mfe}bp or graduated filter: {len(targets)}")

    # Sort by MFE descending (most explosive tokens first)
    targets.sort(key=lambda x: x.get('mfe_bp', 0), reverse=True)

    # Limit
    targets = targets[:args.max_tokens]
    print(f"  Processing first {len(targets)} tokens (sorted by MFE desc)")

    all_events = []
    os.makedirs(OUTPUT_DIR, exist_ok=True)

    for i, token in enumerate(targets):
        symbol = token.get('token_symbol', '?')
        mint_short = token['mint'][:12]
        grad = "GRAD" if token.get('graduated') else "SURV"
        mfe = token.get('mfe_bp', 0)
        print(f"\n  [{i+1}/{len(targets)}] ${symbol} ({grad}, MFE={mfe}bp) {mint_short}...")

        n = acquire_for_token(token, all_events)
        print(f"    Events: {n}")
        time.sleep(1)  # Courtesy pause between tokens

    # Write output
    print(f"\n{'=' * 70}")
    print(f"  Total events acquired: {len(all_events)}")

    if all_events:
        classes = Counter(e['usefulness_class'] for e in all_events)
        print(f"  Quality distribution: {dict(classes)}")

        platforms = Counter(e['platform'] for e in all_events)
        print(f"  Platforms: {dict(platforms)}")

        out_file = args.output or os.path.join(OUTPUT_DIR, f"slinky_mint_narrative_{RUN_UUID}.jsonl")
        with open(out_file, 'w', encoding='utf-8') as f:
            for ev in all_events:
                f.write(json.dumps(ev) + '\n')
        print(f"\n  Output: {out_file}")
        print(f"  File size: {os.path.getsize(out_file)} bytes")

    print(f"\n  DONE.")

if __name__ == '__main__':
    main()
