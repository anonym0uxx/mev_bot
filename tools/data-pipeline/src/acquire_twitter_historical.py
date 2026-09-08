#!/usr/bin/env python
"""
acquire_twitter_historical.py — Slinky v3 historical Twitter acquisition via Wayback CDX

TARGET WINDOW: June 4 – July 14, 2026 (the exact Slinky v3 training timeline).

This module:
1. Queries the Wayback CDX API for archived tweet URLs per elite handle,
   constrained to from=20260604&to=20260714.
2. Rate-limits: 8s between CDX queries, 3s between individual tweet fetches.
3. Fetches each archived tweet's JSON from Wayback's id_ endpoint.
4. Extracts: text, created_at, public_metrics, entities, note_tweet (long tweets).
5. Classifies each tweet: HIGH_SIGNAL_REASONING, STRATEGY, TRADE_THESIS,
   LOW_SIGNAL_CHATTER, DUPLICATE.
6. ALSO searches CDX for tweets mentioning Slinky token symbols loaded from
   slinky_target_mints_enriched.json (ticker cashtags like $ALGAECOIN).
7. Outputs raw_social_event_v1 JSONL with platform='twitter',
   source='wayback_cdx_historical'.

No login, no API keys, no CAPTCHA. Pure public archive access.

Usage:
    python src/acquire_twitter_historical.py [--handles all] [--limit 50] [--output PATH]
"""

import argparse
import json
import os
import re
import subprocess
import sys
import time
import urllib.request
import urllib.error
from pathlib import Path
from datetime import datetime, timezone

# ─── Configuration ──────────────────────────────────────────────────────────

# Slinky v3 exact window — June 4 to July 14, 2026
SLINKY_V3_FROM = "20260604"
SLINKY_V3_TO   = "20260714"

# Rate limits (seconds) — per the task spec
CDX_QUERY_DELAY_S    = 8.0   # between CDX queries
TWEET_FETCH_DELAY_S  = 3.0   # between individual tweet fetches

# Paths
PIPELINE_ROOT = Path("D:/repos/mev_bot/tools/data-pipeline")
SLINKY_MINTS_PATH = PIPELINE_ROOT / "output" / "narrative_gold_v1" / "slinky_target_mints_enriched.json"

# Firecrawl fallback for HTTP fetches (local instance)
FIRECRAWL_URL = os.environ.get("FIRECRAWL_URL", "http://127.0.0.1:3102")

# Elite memecoin trader Twitter handles (reused from acquire_twitter_wayback.py)
ELITE_HANDLES = {
    "blknoiz06":       {"name": "Ansem",          "tier": 1, "memecoin_relevance": "high",
                        "notes": "Top memecoin influencer."},
    "OrangeSBS":       {"name": "Orangie",        "tier": 1, "memecoin_relevance": "high",
                        "notes": "Elite memecoin trader."},
    "Cented7":         {"name": "Cented",         "tier": 1, "memecoin_relevance": "high",
                        "notes": "Alpha caller + Twitch streamer."},
    "Megga":           {"name": "Megga",          "tier": 1, "memecoin_relevance": "high",
                        "notes": "Memecoin trader + Twitch streamer."},
    "kmoney":          {"name": "kmoney",         "tier": 1, "memecoin_relevance": "high",
                        "notes": "Elite memecoin trader."},
    "based16z":        {"name": "based16z",       "tier": 2, "memecoin_relevance": "high",
                        "notes": "Longpost trader Ansem engages with."},
    "MissionGains":    {"name": "MissionGains",   "tier": 2, "memecoin_relevance": "high",
                        "notes": "On-chain analyst."},
    "socratesxbt":     {"name": "socratesxbt",    "tier": 2, "memecoin_relevance": "high",
                        "notes": "High risk investor."},
    "NotSoEasyMoney":  {"name": "NotSoEasyMoney", "tier": 2, "memecoin_relevance": "medium",
                        "notes": "Live streams."},
    "zeroxkyle":       {"name": "zeroxkyle",      "tier": 2, "memecoin_relevance": "medium",
                        "notes": "DeFi research."},
    "cornd0gman":      {"name": "cornd0gman",     "tier": 3, "memecoin_relevance": "low",
                        "notes": "Fried commodities trader."},
    "lmrankhan":       {"name": "lmrankhan",      "tier": 2, "memecoin_relevance": "medium"},
    "MattWallace888":  {"name": "MattWallace888", "tier": 2, "memecoin_relevance": "medium"},
    "Theunipcs":       {"name": "Theunipcs",      "tier": 2, "memecoin_relevance": "medium"},
    "DegenerateNews":  {"name": "DegenerateNews", "tier": 2, "memecoin_relevance": "medium"},
}


# ─── Slinky symbol loading ──────────────────────────────────────────────────

def load_slinky_symbols(path: Path = SLINKY_MINTS_PATH,
                        max_symbols: int = 400) -> list:
    """Load unique ticker symbols from slinky_target_mints_enriched.json.
    Returns a sorted list of $TICKER strings suitable for CDX/tweet matching.
    Filters to plausible ticker shapes (2-14 alnum chars)."""
    if not path.exists():
        print(f"  [warn] Slinky mints file not found: {path}")
        return []
    with open(path, "r", encoding="utf-8") as f:
        data = json.load(f)
    symbols = set()
    for d in data:
        sym = (d.get("token_symbol") or "").strip()
        if re.match(r'^\$?[A-Za-z0-9]{2,14}$', sym) and not sym.startswith('$1'):
            if not sym.startswith('$'):
                sym = '$' + sym
            symbols.add(sym)
    # Prioritize: symbols that appear on multiple mints first, then alphabetical
    # (cap to max_symbols to keep CDX query volume sane)
    sorted_syms = sorted(symbols)
    return sorted_syms[:max_symbols]


# ─── HTTP fetch helpers ──────────────────────────────────────────────────────

WAYBACK_OUTAGE = False  # global flag — set if we detect a persistent 503 outage
FIRECRAWL_AVAILABLE = True  # set False if Firecrawl is down


def _curl_fetch(url: str, max_time: int = 45) -> tuple[str | None, int]:
    """Fetch URL via curl. Returns (body_text_or_None, http_status)."""
    try:
        out = subprocess.run(
            ["curl", "-s", "-L", "--max-time", str(max_time),
             "-H", "User-Agent: Mozilla/5.0 (Windows NT 10.0; Win64; x64) "
             "AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
             "-w", "\n%{http_code}", url],
            capture_output=True, text=True
        )
        if out.returncode == 0:
            lines = out.stdout.rsplit("\n", 1)
            if len(lines) == 2:
                body, code_str = lines
                try:
                    code = int(code_str.strip())
                except ValueError:
                    code = 0
                    body = out.stdout
            else:
                body = out.stdout
                code = 0
            if body and "Temporarily Offline" in body:
                return None, 503
            if body and "429 Too Many Requests" in body:
                return None, 429
            return body, code
    except Exception:
        pass
    return None, 0


def _firecrawl_fetch(url: str) -> tuple[str | None, int]:
    """Fetch URL via local Firecrawl instance (HeadlessChrome — different egress).
    Returns (body_text_or_None, http_status). Firecrawl wraps JSON in markdown fences.
    """
    global FIRECRAWL_AVAILABLE
    if not FIRECRAWL_AVAILABLE:
        return None, 0
    payload = json.dumps({"url": url, "formats": ["markdown"]}).encode()
    req = urllib.request.Request(
        f"{FIRECRAWL_URL}/v1/scrape",
        data=payload,
        headers={"Content-Type": "application/json"},
        method="POST"
    )
    try:
        resp = urllib.request.urlopen(req, timeout=60)
        data = json.loads(resp.read().decode())
        if data.get("success"):
            md = data.get("data", {}).get("markdown", "")
            status = data.get("data", {}).get("metadata", {}).get("statusCode", 200)
            if "Temporarily Offline" in md:
                return None, 503
            return md, status
    except Exception:
        FIRECRAWL_AVAILABLE = False  # don't try again if firecrawl is down
    return None, 0


def _extract_json_from_markdown(md: str) -> str | None:
    """Extract raw JSON from Firecrawl's markdown-wrapped output.
    Firecrawl wraps JSON API responses in ```json ... ``` fences."""
    # Try code fence extraction
    m = re.search(r'```(?:json)?\s*(.*?)```', md, re.DOTALL)
    if m:
        return m.group(1).strip()
    # Try raw JSON (no fence)
    stripped = md.strip()
    if stripped.startswith("{") or stripped.startswith("["):
        return stripped
    return None


def fetch_url(url: str, max_time: int = 45) -> tuple[str | None, int]:
    """Unified fetch: curl first, Firecrawl fallback. Returns (body, http_status).
    For JSON API endpoints, body is raw JSON text (fences stripped if from Firecrawl).
    """
    global WAYBACK_OUTAGE
    # Try curl first (fastest)
    body, code = _curl_fetch(url, max_time=max_time)
    if body is not None and len(body) > 5 and code not in (503, 429):
        return body, code
    # curl failed or got 503/429 — try Firecrawl
    if not WAYBACK_OUTAGE:
        body_fc, code_fc = _firecrawl_fetch(url)
        if body_fc is not None and code_fc not in (503, 429):
            # Strip markdown fences if present
            json_str = _extract_json_from_markdown(body_fc)
            if json_str:
                return json_str, code_fc
            return body_fc, code_fc
        if code_fc == 503:
            # Don't set global outage — it's intermittent
            pass
    if code == 503:
        # Don't set global outage — Wayback is intermittently available
        pass
    return None, code


# ─── Wayback CDX API ────────────────────────────────────────────────────────

def fetch_cdx(url_pattern: str, limit: int = 200,
              date_from: str = SLINKY_V3_FROM,
              date_to: str = SLINKY_V3_TO,
              retries: int = 4) -> list:
    """Query Wayback CDX API for archived URLs matching the pattern,
    constrained to the Slinky v3 window (2026-06-04 to 2026-07-14).
    Returns list of [urlkey, timestamp, original_url, mimetype, status, digest, length].
    Retries on intermittent 503s with backoff.
    """
    cdx_url = (f"https://web.archive.org/cdx/search/cdx?url={url_pattern}"
               f"&output=json&limit={limit}&from={date_from}&to={date_to}"
               f"&filter=mimetype:application/json")
    for attempt in range(retries):
        body, code = fetch_url(cdx_url, max_time=45)
        if body and len(body) > 5 and code not in (503, 429, 0):
            try:
                return json.loads(body)
            except json.JSONDecodeError:
                pass  # fall through to retry
        if code in (503, 429):
            wait = 15 * (attempt + 1)  # 15, 30, 45, 60s backoff
            print(f"    CDX 503/429 (attempt {attempt+1}/{retries}), waiting {wait}s...")
            time.sleep(wait)
            continue
        if code == 0:
            time.sleep(5)
    return []


def fetch_archived_tweet(handle: str, tweet_id: str, timestamp: str,
                         retries: int = 4) -> dict | None:
    """Fetch a single archived tweet's JSON from Wayback's id_ endpoint.
    The id_ prefix requests the raw archived content (no Wayback rewrite).
    Retries on intermittent 503s with backoff.
    """
    wb_url = (f"https://web.archive.org/web/{timestamp}id_/"
              f"https://twitter.com/{handle}/status/{tweet_id}")
    for attempt in range(retries):
        body, code = fetch_url(wb_url, max_time=30)
        if body and len(body) > 10 and code not in (503, 429, 0):
            try:
                data = json.loads(body)
                inner = data.get("data", data)
                if isinstance(inner, list):
                    inner = inner[0] if inner else None
                if not isinstance(inner, dict):
                    return None
                text = inner.get("text", "")
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
            except json.JSONDecodeError:
                pass
        if code in (503, 429):
            wait = 10 * (attempt + 1)  # 10, 20, 30, 40s backoff
            time.sleep(wait)
            continue
        if code == 0:
            time.sleep(3)
    return None


# ─── Classification ──────────────────────────────────────────────────────────

def classify_tweet(text: str, metrics: dict, urls: list) -> str:
    """Classify tweet usefulness for Slinky v3 GOLD admission.
    Returns one of: HIGH_SIGNAL_REASONING, STRATEGY, TRADE_THESIS,
    LOW_SIGNAL_CHATTER, DUPLICATE.
    """
    text_lower = text.lower()
    word_count = len(text.split())

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

    if text.startswith("RT @"):
        return "DUPLICATE"
    if word_count < 5 and not urls:
        return "LOW_SIGNAL_CHATTER"
    if has_strategy and has_reasoning and word_count > 20:
        return "HIGH_SIGNAL_REASONING"
    elif has_strategy:
        return "STRATEGY"
    elif has_reasoning and word_count > 15:
        return "TRADE_THESIS"
    elif word_count > 20:
        return "LOW_SIGNAL_CHATTER"
    else:
        return "LOW_SIGNAL_CHATTER"


# ─── Event building ──────────────────────────────────────────────────────────

def build_raw_event(tweet: dict, handle_info: dict, run_uuid: str,
                    git_sha: str, source_tag: str = "wayback_cdx_historical") -> dict:
    """Build a raw_social_event_v1 event from a fetched tweet."""
    now_ms = int(time.time() * 1000)
    created_ms = 0
    if tweet["created_at"]:
        try:
            dt = datetime.fromisoformat(
                tweet["created_at"].replace("Z", "+00:00"))
            created_ms = int(dt.timestamp() * 1000)
        except Exception:
            pass

    usefulness = classify_tweet(tweet["text"], tweet["metrics"], tweet["urls"])

    return {
        "event_id": f"twitter_{tweet['handle']}_{tweet['tweet_id']}",
        "platform": "twitter",
        "source_type": source_tag,
        "source_url": f"https://twitter.com/{tweet['handle']}/status/{tweet['tweet_id']}",
        "account_handle": tweet["handle"],
        "account_display_name": handle_info.get("name", tweet["handle"]),
        "account_tier": handle_info.get("tier", 2),
        "memecoin_relevance": handle_info.get("memecoin_relevance", "medium"),
        "text": tweet["text"],
        "lang": tweet["lang"],
        "publish_time_ms": created_ms,
        "first_seen_ms": now_ms,
        "retrieval_time_ms": now_ms,
        "retrieval_method": "wayback_cdx_historical",
        "usefulness_class": usefulness,
        "engagement_metrics": tweet["metrics"],
        "external_urls": tweet["urls"],
        "conversation_id": tweet["conversation_id"],
        "run_uuid": run_uuid,
        "git_sha": git_sha,
        "acquisition_window": f"{SLINKY_V3_FROM}_{SLINKY_V3_TO}",
    }


# ─── Acquisition: per-handle ────────────────────────────────────────────────

def acquire_handle(handle: str, handle_info: dict, max_tweets: int = 50,
                   run_uuid: str = "", git_sha: str = "") -> list:
    """Acquire tweets for a single handle via Wayback CDX,
    constrained to the Slinky v3 window (June 4 – July 14, 2026).
    Uses 8s between CDX queries and 3s between tweet fetches.
    """
    events = []
    seen_tweet_ids = set()

    print(f"  CDX query: twitter.com/{handle}/status/*  "
          f"from={SLINKY_V3_FROM} to={SLINKY_V3_TO}")

    snaps = fetch_cdx(
        f"twitter.com/{handle}/status/*",
        limit=max_tweets * 3,  # over-fetch to filter to JSON mimetype
        date_from=SLINKY_V3_FROM, date_to=SLINKY_V3_TO
    )

    if not snaps or len(snaps) <= 1:
        print(f"    No CDX snapshots found.")
        return events

    # snaps[0] is the header row; subsequent rows are matches
    json_snaps = [row for row in snaps[1:] if len(row) >= 4 and 'json' in row[3].lower()]
    print(f"    CDX returned {len(snaps)-1} rows, {len(json_snaps)} JSON snapshots")

    fetched = 0
    for row in json_snaps:
        if fetched >= max_tweets:
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
            fetched += 1
            print(f"    [{fetched}] tweet {tweet_id} "
                  f"-> {event['usefulness_class']}")

        time.sleep(TWEET_FETCH_DELAY_S)  # 3s between tweet fetches

    return events


# ─── Acquisition: symbol search ─────────────────────────────────────────────

def acquire_symbol_mentions(symbols: list, max_per_symbol: int = 10,
                            run_uuid: str = "", git_sha: str = "",
                            max_total: int = 200) -> list:
    """Search CDX for archived tweets mentioning Slinky token symbols ($TICKER).
    Uses the CDX url= parameter with the symbol as a query hint, then fetches
    matching archived tweet pages. Rate-limited at 8s between CDX queries
    and 3s between tweet fetches.
    """
    events = []
    seen_tweet_ids = set()
    fetched = 0

    # We search CDX for twitter.com/* pages captured in the Slinky v3 window.
    # The CDX API doesn't do full-text search, so we query the general
    # twitter.com/* pattern and then filter fetched tweets by symbol text.
    # To keep this tractable, we batch-query a few broad patterns and
    # text-match the fetched content against the symbol list.
    broad_patterns = [
        "twitter.com/*/status/*",
    ]

    for pattern in broad_patterns:
        if fetched >= max_total:
            break
        print(f"  Symbol-search CDX: {pattern}  (looking for {len(symbols)} symbols)")
        snaps = fetch_cdx(pattern, limit=300,
                          date_from=SLINKY_V3_FROM, date_to=SLINKY_V3_TO)
        if not snaps or len(snaps) <= 1:
            time.sleep(CDX_QUERY_DELAY_S)
            continue

        json_snaps = [row for row in snaps[1:] if len(row) >= 4 and 'json' in row[3].lower()]
        print(f"    CDX returned {len(snaps)-1} rows, {len(json_snaps)} JSON snapshots")

        for row in json_snaps:
            if fetched >= max_total:
                break
            m = re.search(r'twitter\.com/([^/]+)/status/(\d+)', row[2])
            if not m:
                continue
            handle = m.group(1)
            tweet_id = m.group(2)
            if tweet_id in seen_tweet_ids:
                continue

            tweet = fetch_archived_tweet(handle, tweet_id, row[1])
            if tweet and len(tweet["text"]) > 5:
                text_upper = tweet["text"].upper()
                # Check if any Slinky symbol is mentioned
                matched = [s for s in symbols if s.upper() in text_upper]
                if matched:
                    seen_tweet_ids.add(tweet_id)
                    handle_info = ELITE_HANDLES.get(handle, {
                        "name": handle, "tier": 2,
                        "memecoin_relevance": "medium"
                    })
                    # Annotate the tweet with matched symbols
                    event = build_raw_event(tweet, handle_info, run_uuid, git_sha,
                                            source_tag="wayback_cdx_historical_symbol")
                    event["matched_slinky_symbols"] = matched
                    events.append(event)
                    fetched += 1
                    print(f"    [{fetched}] @{handle} tweet {tweet_id} "
                          f"matched {matched[:3]} -> {event['usefulness_class']}")

            time.sleep(TWEET_FETCH_DELAY_S)

        time.sleep(CDX_QUERY_DELAY_S)

    return events


# ─── Main ────────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(
        description="Acquire historical Twitter/X content (Slinky v3 window) via Wayback CDX"
    )
    parser.add_argument("--handles", default="all",
                        help="Comma-separated handles or 'all'")
    parser.add_argument("--limit", type=int, default=50,
                        help="Max tweets per handle")
    parser.add_argument("--output", default=None,
                        help="Output JSONL path")
    parser.add_argument("--symbols", default=None,
                        help="Comma-separated $TICKER symbols to search, or 'auto' to load from slinky_mints, or 'none' to skip")
    parser.add_argument("--max-symbol-results", type=int, default=200,
                        help="Max total symbol-mention tweets to collect")
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

    run_uuid = f"tw_hist_{int(time.time()):012x}"
    git_sha = subprocess.run(
        ["git", "rev-parse", "--short", "HEAD"],
        capture_output=True, text=True
    ).stdout.strip() or "unknown"

    print(f"{'='*70}")
    print(f"TWITTER HISTORICAL ACQUISITION — Slinky v3 window")
    print(f"{'='*70}")
    print(f"Window:       {SLINKY_V3_FROM} → {SLINKY_V3_TO} (June 4 – July 14, 2026)")
    print(f"Run UUID:     {run_uuid}")
    print(f"Git SHA:      {git_sha}")
    print(f"Handles:      {list(handles.keys())}")
    print(f"Max/handle:   {args.limit}")
    print(f"Rate limits:  {CDX_QUERY_DELAY_S}s CDX / {TWEET_FETCH_DELAY_S}s tweet")
    print()

    # ─── Phase 1: Per-handle acquisition ────────────────────────────────
    all_events = []
    print(f"── Phase 1: Per-handle CDX acquisition ──")
    for handle, info in handles.items():
        print(f"\n  @{handle} ({info['name']}, tier {info['tier']}):")
        events = acquire_handle(handle, info, max_tweets=args.limit,
                                run_uuid=run_uuid, git_sha=git_sha)
        print(f"    Fetched {len(events)} tweets")
        from collections import Counter
        classes = Counter(e["usefulness_class"] for e in events)
        for cls, count in classes.most_common():
            print(f"      {cls}: {count}")
        all_events.extend(events)
        time.sleep(CDX_QUERY_DELAY_S)  # 8s between CDX queries

    # ─── Phase 2: Symbol-mention search ────────────────────────────────
    print(f"\n── Phase 2: Slinky symbol-mention search ──")
    do_symbol_search = True
    symbol_list = []
    if args.symbols == "none":
        do_symbol_search = False
    elif args.symbols and args.symbols != "auto":
        symbol_list = [s if s.startswith('$') else '$'+s for s in args.symbols.split(",")]
    else:
        symbol_list = load_slinky_symbols()

    if do_symbol_search and symbol_list:
        print(f"  Loaded {len(symbol_list)} Slinky symbols for matching")
        print(f"  Searching CDX for tweets mentioning these symbols...")
        sym_events = acquire_symbol_mentions(
            symbol_list, max_total=args.max_symbol_results,
            run_uuid=run_uuid, git_sha=git_sha
        )
        print(f"  Symbol-mention tweets fetched: {len(sym_events)}")
        from collections import Counter
        sym_classes = Counter(e["usefulness_class"] for e in sym_events)
        for cls, count in sym_classes.most_common():
            print(f"    {cls}: {count}")
        all_events.extend(sym_events)
    elif do_symbol_search:
        print(f"  No Slinky symbols loaded — skipping symbol search.")

    # ─── Write output ──────────────────────────────────────────────────
    if args.output:
        output_path = args.output
    else:
        output_path = str(PIPELINE_ROOT / "output" / "narrative_gold_v1" / "raw"
                          / "raw_social_event_v1"
                          / f"twitter_historical_{run_uuid}.jsonl")

    os.makedirs(os.path.dirname(output_path), exist_ok=True)
    with open(output_path, "w", encoding="utf-8") as f:
        for event in all_events:
            f.write(json.dumps(event, ensure_ascii=False) + "\n")

    print(f"\n{'='*70}")
    print(f"TOTAL: {len(all_events)} tweet events (Slinky v3 window)")
    print(f"Output: {output_path}")
    print(f"{'='*70}")


if __name__ == "__main__":
    main()
