#!/usr/bin/env python
"""
acquire_twitter_wayback.py — Twitter/X content via Wayback Machine CDX API

BYPASS METHOD: Twitter/X profiles and tweet pages require login (return empty HTML
to anonymous requests). The Wayback Machine (web.archive.org) has archived millions
of tweet pages with full JSON content (the Twitter API v2 response format).

This module:
1. Queries the Wayback CDX API for archived tweet URLs per user
2. Fetches each archived tweet's JSON from Wayback's id_ endpoint (raw archived content)
3. Extracts: text, created_at, public_metrics, entities, note_tweet (long tweets)
4. Outputs raw_social_event_v1 format events

No login, no API keys, no CAPTCHA. Pure public archive access.

Usage:
    python src/acquire_twitter_wayback.py [--handles all] [--limit N] [--output PATH]
"""

import argparse
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path
from datetime import datetime, timezone

# ─── Configuration ──────────────────────────────────────────────────────────

FIRECRAWL_URL = os.environ.get("FIRECRAWL_URL", "http://127.0.0.1:3102")

# Elite memecoin trader Twitter handles (from narrative_seeds_v1.yaml + discovery)
# Quality-ranked by memecoin relevance and Wayback CDX availability
ELITE_HANDLES = {
    # Tier 1: Elite memecoin thinkers (from seeds, confirmed Wayback snapshots)
    "blknoiz06":  {"name": "Ansem",   "tier": 1, "memecoin_relevance": "high",
                   "notes": "Top memecoin influencer. 100+ Wayback snapshots confirmed."},
    "OrangeSBS":  {"name": "Orangie", "tier": 1, "memecoin_relevance": "high",
                   "notes": "Elite memecoin trader. 50+ Wayback snapshots confirmed."},
    "Cented7":    {"name": "Cented",  "tier": 1, "memecoin_relevance": "high",
                   "notes": "Alpha caller + Twitch streamer. 200+ Wayback snapshots confirmed."},
    "Megga":      {"name": "Megga",   "tier": 1, "memecoin_relevance": "high",
                   "notes": "Memecoin trader + Twitch streamer. 20 Wayback snapshots confirmed."},
    "kmoney":     {"name": "kmoney",  "tier": 1, "memecoin_relevance": "high",
                   "notes": "'attention gambler' - elite memecoin trader. 20 Wayback snapshots confirmed."},

    # Tier 2: Traders Ansem/Orangie interact with (discovered from their tweets)
    "based16z":      {"name": "based16z",      "tier": 2, "memecoin_relevance": "high",
                      "notes": "Longpost trader Ansem frequently engages with. 'anticlimactic'."},
    "MissionGains":  {"name": "MissionGains",  "tier": 2, "memecoin_relevance": "high",
                      "notes": "On-chain analyst. Ansem retweets about memecoin onboarding."},
    "socratesxbt":   {"name": "socratesxbt",   "tier": 2, "memecoin_relevance": "high",
                      "notes": "'high risk investor' - Ansem engages about $ANSEM coin."},
    "NotSoEasyMoney": {"name": "NotSoEasyMoney", "tier": 2, "memecoin_relevance": "medium",
                       "notes": "'i like to trade' - live streams. Ansem retweets."},
    "zeroxkyle":     {"name": "zeroxkyle",     "tier": 2, "memecoin_relevance": "medium",
                      "notes": "'discretionary investor' - DeFi research. Ansem retweets."},
    "cornd0gman":    {"name": "cornd0gman",    "tier": 3, "memecoin_relevance": "low",
                      "notes": "Fried commodities trader. 5 Wayback snapshots confirmed."},

    # Tier 2: From bonkbot influencer list (memecoin-specific)
    "lmrankhan":       {"name": "lmrankhan",       "tier": 2, "memecoin_relevance": "medium"},
    "MattWallace888":  {"name": "MattWallace888",  "tier": 2, "memecoin_relevance": "medium"},
    "Theunipcs":       {"name": "Theunipcs",       "tier": 2, "memecoin_relevance": "medium"},
    "DegenerateNews":  {"name": "DegenerateNews",  "tier": 2, "memecoin_relevance": "medium"},
}

# ─── Wayback CDX API ────────────────────────────────────────────────────────

def fetch_cdx(url_pattern: str, limit: int = 200,
              date_from: str = "20260101", date_to: str = "20260826",
              retries: int = 3) -> list:
    """Query Wayback CDX API for archived URLs matching the pattern.
    Returns list of [urlkey, timestamp, original_url, mimetype, status, digest, length].
    """
    cdx_url = (f"https://web.archive.org/cdx?url={url_pattern}"
               f"&output=json&limit={limit}&from={date_from}&to={date_to}")
    for attempt in range(retries):
        try:
            out = subprocess.run(
                ["curl", "-s", "-L", "--max-time", "30",
                 "-H", "User-Agent: Mozilla/5.0", cdx_url],
                capture_output=True, text=True
            )
            if out.returncode == 0 and len(out.stdout) > 5:
                return json.loads(out.stdout)
        except (json.JSONDecodeError, subprocess.TimeoutExpired):
            pass
        time.sleep(2)
    return []


def fetch_archived_tweet(handle: str, tweet_id: str, timestamp: str,
                         retries: int = 2) -> dict | None:
    """Fetch a single archived tweet's JSON from Wayback's id_ endpoint.
    The id_ prefix requests the raw archived content (no Wayback rewrite).
    """
    wb_url = (f"https://web.archive.org/web/{timestamp}id_/"
              f"https://twitter.com/{handle}/status/{tweet_id}")
    for attempt in range(retries):
        try:
            out = subprocess.run(
                ["curl", "-s", "-L", "--max-time", "20",
                 "-H", "User-Agent: Mozilla/5.0", wb_url],
                capture_output=True, text=True
            )
            if len(out.stdout) > 10:
                data = json.loads(out.stdout)
                inner = data.get("data", data)
                if not isinstance(inner, dict):
                    return None
                text = inner.get("text", "")
                # Long tweets use note_tweet field
                note = inner.get("note_tweet", {})
                note_text = note.get("text", "") if isinstance(note, dict) else ""
                full_text = note_text if note_text else text

                entities = inner.get("entities", {})
                urls = entities.get("urls", [])
                expanded = [u.get("expanded_url", "") for u in urls if u.get("expanded_url")]

                return {
                    "tweet_id": str(tweet_id),
                    "handle": handle,
                    "text": full_text,
                    "created_at": inner.get("created_at", ""),
                    "metrics": inner.get("public_metrics", {}),
                    "urls": expanded,
                    "lang": inner.get("lang", ""),
                    "conversation_id": inner.get("conversation_id", ""),
                    "in_reply_to_user_id": inner.get("in_reply_to_user_id", ""),
                    "referenced_tweets": inner.get("referenced_tweets", []),
                }
        except (json.JSONDecodeError, subprocess.TimeoutExpired):
            pass
        time.sleep(1)
    return None


def classify_tweet(text: str, metrics: dict, urls: list) -> str:
    """Classify tweet usefulness for GOLD admission."""
    text_lower = text.lower()
    word_count = len(text.split())

    # Strategy/reasoning keywords
    strategy_kw = ["strategy", "entry", "exit", "thesis", "conviction",
                   "position", "sizing", "risk", "allocation", "portfolio",
                   "trade", "buy", "sell", "long", "short", "snipe",
                   "memecoin", "pump", "rug", "dev", "liquidity", "market cap",
                   "mc", "bonding", "curve", "holder", "distribution"]
    reasoning_kw = ["because", "since", "therefore", "implying", "means",
                    "reason", "logic", "analysis", "think", "expect",
                   "should", "would", "could", "predict", "forecast"]

    has_strategy = any(kw in text_lower for kw in strategy_kw)
    has_reasoning = any(kw in text_lower for kw in reasoning_kw)

    # Retweet / echo
    if text.startswith("RT @"):
        return "DUPLICATE"

    # Very short with no substance
    if word_count < 5 and not urls:
        return "LOW_SIGNAL_CHATTER"

    # Promotional / shill
    shill_kw = ["buy now", "don't miss", "presale", "referral", "join my",
                "discount", "code:", "affiliate"]
    if any(kw in text_lower for kw in shill_kw):
        return "PROMOTIONAL_SHILL"

    if has_strategy and has_reasoning and word_count > 20:
        return "HIGH_SIGNAL_REASONING"
    elif has_strategy:
        return "STRATEGY"
    elif has_reasoning and word_count > 15:
        return "TRADE_THESIS"
    elif word_count > 20:
        return "UNRESOLVED"
    else:
        return "LOW_SIGNAL_CHATTER"


def build_raw_event(tweet: dict, handle_info: dict, run_uuid: str,
                    git_sha: str) -> dict:
    """Build a raw_social_event_v1 event from a fetched tweet."""
    now_ms = int(time.time() * 1000)
    # Parse created_at to ms
    created_ms = 0
    if tweet["created_at"]:
        try:
            dt = datetime.fromisoformat(
                tweet["created_at"].replace("Z", "+00:00"))
            created_ms = int(dt.timestamp() * 1000)
        except:
            pass

    usefulness = classify_tweet(tweet["text"], tweet["metrics"], tweet["urls"])

    return {
        "event_id": f"twitter_{tweet['handle']}_{tweet['tweet_id']}",
        "platform": "twitter",
        "source_type": "twitter_wayback",
        "source_url": f"https://twitter.com/{tweet['handle']}/status/{tweet['tweet_id']}",
        "account_handle": tweet["handle"],
        "account_display_name": handle_info.get("name", tweet["handle"]),
        "account_tier": handle_info.get("tier", 2),
        "memecoin_relevance": handle_info.get("memecoin_relevance", "medium"),
        "text": tweet["text"],
        "lang": tweet["lang"],
        "publish_time_ms": created_ms,
        "first_seen_ms": now_ms,  # First time OUR collector observed it
        "retrieval_time_ms": now_ms,
        "retrieval_method": "wayback_cdx_api",
        "usefulness_class": usefulness,
        "engagement_metrics": tweet["metrics"],
        "external_urls": tweet["urls"],
        "conversation_id": tweet["conversation_id"],
        "run_uuid": run_uuid,
        "git_sha": git_sha,
    }


def acquire_handle(handle: str, handle_info: dict, max_tweets: int = 30,
                   run_uuid: str = "", git_sha: str = "") -> list:
    """Acquire tweets for a single handle via Wayback CDX.
    Uses long pauses between requests to avoid Wayback rate limiting."""
    events = []

    # Query CDX across date ranges to avoid rate limits and get broader coverage
    date_ranges = [
        ("20260701", "20260826"),  # Most recent first (highest value)
        ("20260401", "20260630"),
        ("20260101", "20260331"),
    ]

    seen_tweet_ids = set()
    total_fetched = 0

    for date_from, date_to in date_ranges:
        if total_fetched >= max_tweets:
            break

        snaps = fetch_cdx(
            f"twitter.com/{handle}/status/*",
            limit=50, date_from=date_from, date_to=date_to
        )

        if not snaps or len(snaps) <= 1:
            time.sleep(3)  # Rate limit courtesy even on empty results
            continue

        json_snaps = [row for row in snaps[1:] if 'json' in row[3]]

        for row in json_snaps:
            if total_fetched >= max_tweets:
                break
            m = re.search(r'/status/(\d+)', row[2])
            if not m:
                continue
            tweet_id = m.group(1)
            if tweet_id in seen_tweet_ids:
                continue
            seen_tweet_ids.add(tweet_id)

            tweet = fetch_archived_tweet(handle, tweet_id, row[1])
            if tweet and len(tweet["text"]) > 5:
                event = build_raw_event(tweet, handle_info, run_uuid, git_sha)
                events.append(event)
                total_fetched += 1

            time.sleep(1.5)  # Rate limit courtesy between tweet fetches

        time.sleep(3)  # Longer pause between CDX date range queries

    return events


def main():
    parser = argparse.ArgumentParser(
        description="Acquire Twitter/X content via Wayback Machine CDX API")
    parser.add_argument("--handles", default="all",
                        help="Comma-separated handles or 'all'")
    parser.add_argument("--limit", type=int, default=30,
                        help="Max tweets per handle")
    parser.add_argument("--output", default=None,
                        help="Output JSONL path")
    args = parser.parse_args()

    # Determine handles
    if args.handles == "all":
        handles = ELITE_HANDLES
    else:
        handles = {}
        for h in args.handles.split(","):
            h = h.strip()
            if h in ELITE_HANDLES:
                handles[h] = ELITE_HANDLES[h]
            else:
                handles[h] = {"name": h, "tier": 2, "memecoin_relevance": "medium"}

    run_uuid = f"tw_{int(time.time()):012x}"
    git_sha = subprocess.run(
        ["git", "rev-parse", "--short", "HEAD"],
        capture_output=True, text=True
    ).stdout.strip() or "unknown"

    print(f"{'='*70}")
    print(f"TWITTER WAYBACK ACQUISITION")
    print(f"{'='*70}")
    print(f"Run UUID:    {run_uuid}")
    print(f"Git SHA:     {git_sha}")
    print(f"Handles:     {list(handles.keys())}")
    print(f"Max/handle:  {args.limit}")
    print()

    all_events = []
    for handle, info in handles.items():
        print(f"  @{handle} ({info['name']}, tier {info['tier']}):")
        events = acquire_handle(handle, info, max_tweets=args.limit,
                                run_uuid=run_uuid, git_sha=git_sha)
        print(f"    Fetched {len(events)} tweets")

        # Show usefulness breakdown
        from collections import Counter
        classes = Counter(e["usefulness_class"] for e in events)
        for cls, count in classes.most_common():
            print(f"      {cls}: {count}")

        all_events.extend(events)
        print()

    # Write output
    if args.output:
        output_path = args.output
    else:
        output_path = ("D:/repos/mev_bot/tools/data-pipeline/"
                       f"output/narrative_gold_v1/raw/raw_social_event_v1/"
                       f"twitter_wayback_{run_uuid}.jsonl")

    os.makedirs(os.path.dirname(output_path), exist_ok=True)
    with open(output_path, "w", encoding="utf-8") as f:
        for event in all_events:
            f.write(json.dumps(event, ensure_ascii=False) + "\n")

    print(f"{'='*70}")
    print(f"TOTAL: {len(all_events)} tweet events")
    print(f"Output: {output_path}")
    print(f"{'='*70}")


if __name__ == "__main__":
    main()
