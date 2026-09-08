#!/usr/bin/env python
"""
narrative_gold_v1 — Active Web Acquisition via Local Firecrawl
==============================================================
Replaces API-key standby with ACTIVE authorized public-web crawling.
Uses LOCAL Firecrawl Docker (http://127.0.0.1:3102) — zero external API keys.

DESIGN:
  - Quality > Quantity. Do NOT optimize scrape count.
  - RAW may be broad; GOLD is fail-closed. Reject/down-rank: empty chatter,
    copied calls, referral farming, repetitive shills, generic crypto,
    fake engagement, bot-like content, posts without substantive reasoning.
  - Preserve RAW HTML/markdown/metadata/hash.
  - Corrected first_seen: publish_time vs retrieval_time vs first_seen_time.
  - Historical backfill = retrospective; live recurring crawls = causal first_seen.
  - Separate ex-ante calls from ex-post recaps/PnL-brags/education.
  - Respect robots/TOS/access controls; never evade logins/CAPTCHAs/paywalls.
  - Unavailable = coverage gap, NOT zero signal.

SOURCE TYPES (all public, zero external API keys):
  1. Public Telegram web channels (t.me/s/<channel>)
  2. pump.fun board/explore/coin pages
  3. DexScreener token pages
  4. YouTube search results + video pages (transcripts via captions)
  5. Public web pages (blogs, newsletters, alpha pages)
  6. Public X/Twitter profile pages (best-effort, no login evasion)
  7. Firecrawl web search for discovery
  8. Twitch streamer pages (clips, about, videos — topic signals + identity enrichment)

QUALITY RANKING (empirically verified 2026-08-26):
  Tier 1 (elite reasoning, >100K followers, reasoning clips):
    - cented (336K, clips: "what is the main thing to look for if its a good coin", "how to make 10m")
    - megga (460K, clips: "what a snipe", "claiming")
  Tier 2 (active memecoin streamers, >10K followers):
    - decu (31K, "Trading memecoins on the Solana blockchain")
    - dvces (75.6K, memecoin trading)
  Note: Twitch clips are 6-20s video segments — NO transcripts. Value = topic
  signals (clip titles show trading concepts discussed) + identity enrichment
  (linked YouTube/Twitter channels, follower counts, live status). Real spoken
  reasoning comes from linked YouTube channels (descriptions, timestamps, captions).

USAGE:
  python src/acquire_narrative_web_v1.py [--sources all] [--dry-run] [--max-pages 20]
  python src/acquire_narrative_web_v1.py --sources telegram --max-pages 5
"""

import os
import sys
import json
import time
import hashlib
import argparse
import re
import urllib.parse
from pathlib import Path
from datetime import datetime, timezone
from typing import Optional

# ─── Paths ───────────────────────────────────────────────────────────

PIPELINE_ROOT = Path(__file__).parent.parent
OUTPUT_BASE = PIPELINE_ROOT / "output" / "narrative_gold_v1"
RAW_DIR = OUTPUT_BASE / "raw" / "raw_social_event_v1"
MANIFEST_PATH = OUTPUT_BASE / "raw" / "raw_manifest_v1.json"
FIRST_SEEN_LOG = OUTPUT_BASE / "raw" / "FIRST_SEEN.log"
ACQ_STATE_PATH = OUTPUT_BASE / "raw" / "acquisition_state.json"
SEEN_URLS_PATH = OUTPUT_BASE / "raw" / "seen_urls.json"

FIRECRAWL_URL = os.environ.get("FIRECRAWL_URL", "http://127.0.0.1:3102")
FC_API_KEY = os.environ.get("FC_API_KEY", "pq-local-test-key")

SCHEMA_VERSION = "1.0.0"
PIPELINE_VERSION = "1.1.0"  # bumped: corrected first_seen semantics + web acquisition

# ─── Seed URLs (public, zero external API keys) ─────────────────────

SEED_URLS = {
    "telegram": [
        # Public TG web channels (t.me/s/<channel> = public web preview, no login)
        # Primary call/alpha channels:
        "https://t.me/s/crypticannouncements",
        "https://t.me/s/chasescharts",
        "https://t.me/s/PikalosiCalls",
        "https://t.me/s/PikalosiLounge",
        "https://t.me/s/Marlonalpha",
        "https://t.me/s/jacalcooks",
        # Alpha group public previews (accessible but often thin — coverage gap noted):
        "https://t.me/s/potionalpha",        # Potion Alpha (public, 1.5KB)
        # Discovered high-content alpha channels:
        "https://t.me/s/solanaalpha",        # 21KB — "Solana Gambler" channel
        "https://t.me/s/alphacalls",         # 19KB — alpha call aggregator (mduzcalls etc)
    ],
    "pumpfun": [
        "https://pump.fun/board",
        "https://pump.fun/explore",
    ],
    "dexscreener": [
        "https://dexscreener.com/solana",
        "https://dexscreener.com/solana/pumpswap",
    ],
    "youtube": [
        # YouTube search for memecoin strategy content
        "https://www.youtube.com/results?search_query=solana+memecoin+trading+strategy",
        "https://www.youtube.com/results?search_query=pump+fun+trading+alpha",
        "https://www.youtube.com/results?search_query=solana+degen+strategy+2026",
        # Elite thinker-specific searches (highest quality reasoning):
        "https://www.youtube.com/results?search_query=Orangie+memecoin+pump+fun+strategy",
        "https://www.youtube.com/results?search_query=Cented+memecoin+trading+strategy",
        "https://www.youtube.com/results?search_query=Megga+memecoin+trading+interview",
        "https://www.youtube.com/results?search_query=Cupsey+memecoin+trading+strategy",
        "https://www.youtube.com/results?search_query=Prosper+memecoin+pump+fun+interview",
        "https://www.youtube.com/results?search_query=Ansem+blknoiz06+memecoin+strategy",
    ],
    "twitch": [
        # Twitch streamer pages — clips (topic signals), about (identity enrichment)
        # Quality-ranked (empirically verified 2026-08-26 via local Firecrawl):
        # Tier 1: elite reasoning streamers
        "https://www.twitch.tv/cented/clips?filter=clips&range=7d",   # 336K, 10 clips w/ reasoning
        "https://www.twitch.tv/cented/clips?filter=clips&range=30d",  # 30-day broader
        "https://www.twitch.tv/megga/clips?filter=clips&range=7d",    # 460K, memecoin trader
        "https://www.twitch.tv/megga/clips?filter=clips&range=30d",
        # Tier 2: active memecoin streamers
        "https://www.twitch.tv/decu/clips?filter=clips&range=7d",     # 31K, "Trading memecoins on Solana"
        "https://www.twitch.tv/dvces/clips?filter=clips&range=7d",    # 75.6K, memecoin trading
        # Identity enrichment pages (linked socials, follower counts):
        "https://www.twitch.tv/cented/about",
        "https://www.twitch.tv/megga/about",
        "https://www.twitch.tv/decu/about",
        "https://www.twitch.tv/dvces/about",
        # Twitch crypto category directory (discovers currently-live streamers):
        "https://www.twitch.tv/directory/category/crypto",
    ],
    "web": [
        # Public web pages — discovery via Firecrawl
        # Padre.gg landing page (accessible, marketing copy only — no signal content):
        "https://padre.gg",
    ],
    # Coverage gaps (private/login-walled — recorded but NOT scraped):
    # - heavenorhelldao: private TG group, no public message content
    # - prosperitydao: private TG group, no public message content
    # - GreekFnF / x.com/GreekFnF: X profile, login wall
    # - Cielo.fm: SPA, requires JS auth, returns empty
    # - fomo.wtf: domain parked/for sale, dead
}

# ─── Utilities ──────────────────────────────────────────────────────


def now_utc_iso() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%fZ")


def now_unix_ms() -> int:
    return int(time.time() * 1000)


def sha256_hex(text: str) -> str:
    return hashlib.sha256(text.encode()).hexdigest()


def fc_scrape(url: str, fmt: str = "markdown", wait_ms: int = 0) -> Optional[dict]:
    """Scrape a URL via local Firecrawl. Returns parsed JSON or None.
    wait_ms: waitFor parameter for SPA pages (Twitch, YouTube) that need render time."""
    import subprocess
    payload = {"url": url, "formats": [fmt], "maxAge": 300000}
    if wait_ms > 0:
        payload["waitFor"] = wait_ms
    cmd = [
        "curl", "-s", "-X", "POST", f"{FIRECRAWL_URL}/v1/scrape",
        "-H", "Content-Type: application/json",
        "-H", f"Authorization: Bearer ***",
        "-d", json.dumps(payload),
        "--max-time", "90",
    ]
    try:
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=120)
        if result.returncode != 0:
            return None
        return json.loads(result.stdout)
    except Exception:
        return None


def fc_map(url: str, limit: int = 50) -> Optional[dict]:
    """Map a site's URLs via local Firecrawl."""
    import subprocess
    cmd = [
        "curl", "-s", "-X", "POST", f"{FIRECRAWL_URL}/v1/map",
        "-H", "Content-Type: application/json",
        "-H", f"Authorization: Bearer {FC_API_KEY}",
        "-d", json.dumps({"url": url, "limit": limit}),
        "--max-time", "60",
    ]
    try:
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=90)
        if result.returncode != 0:
            return None
        return json.loads(result.stdout)
    except Exception:
        return None


def normalize_text(text: str) -> str:
    """Normalize text for dedup: strip URLs, collapse whitespace, lowercase.
    NOTE: This lowercases text for keyword matching/dedup. Mint addresses
    extracted AFTER this function will be lowercase and CANNOT match
    Slinky v3 base58 (case-sensitive). Use extract_solana_addresses on
    ORIGINAL-CASE text before calling this function."""
    text = re.sub(r'https?://\S+', '', text)
    text = re.sub(r'pic\.x\.com/\S+', '', text)
    text = re.sub(r'!\[.*?\]\(.*?\)', '', text)  # markdown images
    text = re.sub(r'\[([^\]]+)\]\([^)]+\)', r'\1', text)  # markdown links → text
    text = text.lower().strip()
    text = re.sub(r'\s+', ' ', text)
    return text


def extract_mints_preserve_case(text: str) -> list:
    """Extract Solana/pump.fun mint addresses preserving original base58 case.
    MUST be called on ORIGINAL text (before normalize_text lowercases it)."""
    # pump.fun mints end with 'pump' (case-insensitive)
    pump_mints = re.findall(r'\b([1-9A-HJ-NP-Za-km-z]{32,44}pump)\b', text, re.IGNORECASE)
    # General Solana addresses
    all_addrs = re.findall(r'\b([1-9A-HJ-NP-Za-km-z]{32,44})\b', text)
    # Dedupe, preserve case
    result = []
    seen = set()
    for m in pump_mints + all_addrs:
        m_orig = m  # preserve original case
        if m_orig not in seen:
            seen.add(m_orig)
            result.append(m_orig)
    return result


def extract_cashtags(text: str) -> list:
    return list(set(re.findall(r'\$([A-Za-z0-9]+)', text)))


def extract_solana_addresses(text: str) -> list:
    matches = re.findall(r'\b([1-9A-HJ-NP-Za-km-z]{32,44})\b', text)
    return list(set(m for m in matches if len(m) >= 32))


def extract_urls(text: str) -> list:
    return re.findall(r'https?://[^\s)\]]+', text)


_MONTHS = {
    'january': 1, 'february': 2, 'march': 3, 'april': 4, 'may': 5, 'june': 6,
    'july': 7, 'august': 8, 'september': 9, 'october': 10, 'november': 11, 'december': 12,
    'jan': 1, 'feb': 2, 'mar': 3, 'apr': 4, 'may': 5, 'jun': 6,
    'jul': 7, 'aug': 8, 'sep': 9, 'oct': 10, 'nov': 11, 'dec': 12,
}


def estimate_publish_time(text: str, platform: str) -> int:
    """Best-effort publish time extraction from scraped content.
    For TG web preview: parse 'Month DD, YYYY' or 'Month DD' patterns from the block.
    The TG web preview appends the month name and day after each message block.
    Falls back to retrieval time if no date found."""
    import calendar

    # TG web preview format: "Month DD, YYYY" or "Month DD" (no year = current year)
    # Also: "Month DD, YYYY" appears at end of message blocks
    # Try full date first: "June 5, 2026" or "June 5 2026"
    full_date_re = re.compile(
        r'(January|February|March|April|May|June|July|August|September|October|November|December)'
        r'\s+(\d{1,2})\s*,?\s*(\d{4})',
        re.IGNORECASE,
    )
    m = full_date_re.search(text)
    if m:
        month = _MONTHS[m.group(1).lower()]
        day = int(m.group(2))
        year = int(m.group(3))
        try:
            dt = datetime(year, month, day, tzinfo=timezone.utc)
            return int(dt.timestamp() * 1000)
        except (ValueError, calendar.error):
            pass

    # Try "Month DD" without year — assume current year (2026)
    short_date_re = re.compile(
        r'(January|February|March|April|May|June|July|August|September|October|November|December)'
        r'\s+(\d{1,2})\s*$',
        re.IGNORECASE | re.MULTILINE,
    )
    m = short_date_re.search(text)
    if m:
        month = _MONTHS[m.group(1).lower()]
        day = int(m.group(2))
        try:
            dt = datetime(2026, month, day, tzinfo=timezone.utc)
            return int(dt.timestamp() * 1000)
        except (ValueError, calendar.error):
            pass

    # Relative timestamps: "2h ago", "3d ago", "5 min ago"
    rel_re = re.compile(r'(\d+)\s*(min|minute|h|hr|hour|d|day|w|week)\s*ago', re.IGNORECASE)
    m = rel_re.search(text)
    if m:
        val = int(m.group(1))
        unit = m.group(2).lower()
        multipliers = {
            'min': 60_000, 'minute': 60_000,
            'h': 3_600_000, 'hr': 3_600_000, 'hour': 3_600_000,
            'd': 86_400_000, 'day': 86_400_000,
            'w': 604_800_000, 'week': 604_800_000,
        }
        return now_unix_ms() - (val * multipliers.get(unit, 0))

    # Fallback: retrieval time (conservative — honest unknown)
    return now_unix_ms()


def classify_usefulness(text: str, platform: str, account_handle: str) -> str:
    """Heuristic usefulness classification (will be refined in gold builder).
    Returns one of USEFULNESS_CLASSES."""
    normalized = normalize_text(text)

    if len(normalized) < 20:
        return "LOW_SIGNAL_CHATTER"

    # Promotional/shill indicators
    promo_patterns = [
        r'referral|ref link|use my link|sign up.*bonus',
        r'join.*tg|join.*discord.*earn',
        r'100x|guaranteed|easy money|free money',
        r'dm me|dm.*for.*access|private.*group',
    ]
    for p in promo_patterns:
        if re.search(p, normalized):
            return "PROMOTIONAL_SHILL"

    # Strategy/reasoning indicators (HIGH_SIGNAL)
    strategy_patterns = [
        r'entry|exit|stop.loss|take.profit|tp|sl',
        r'risk|position.size|sizing|bankroll',
        r'dev.wallet|bundle|first.buyer|holder',
        r'narrative|thesis|conviction|edge',
        r'rug|pull|graduat|moonshot',
        r'liquidity|depth|slippage|spread',
        r'if.*then|because|therefore|however|but.if',
        r'watch.*for|look.for|signal|trigger',
        r'mc|market.cap|fdv|bond.curve',
    ]
    strategy_hits = sum(1 for p in strategy_patterns if re.search(p, normalized))

    if strategy_hits >= 3:
        # Check for trade thesis specifically
        if re.search(r'(buy|long|short|avoid|skip|watch).*because', normalized) or \
           re.search(r'(entry|enter).*at|above|below', normalized):
            return "TRADE_THESIS"
        if re.search(r'(rug|pull|scam|dev.*dump|countertrade)', normalized):
            return "RISK_POSTMORTEM"
        if re.search(r'(narrative|meta|rotation|regime|cycle)', normalized):
            return "NARRATIVE_ANALYSIS"
        return "HIGH_SIGNAL_REASONING"
    elif strategy_hits >= 1:
        return "STRATEGY"

    # Generic crypto chatter
    if re.search(r'(gm|wagmi|ngmi|lfh|lfg|pump|to.the.moon)', normalized):
        return "LOW_SIGNAL_CHATTER"

    return "UNRESOLVED"


def compute_signal_density(text: str) -> float:
    """Estimate substantive content ratio (0.0-1.0)."""
    normalized = normalize_text(text)
    if not normalized:
        return 0.0
    # Count "substance" words (strategy/reasoning terms)
    substance_words = set('''
        entry exit stop loss target risk position size bankroll dev wallet bundle
        holder narrative thesis conviction edge rug pull graduation moonshot
        liquidity depth slippage spread market cap fdv bond curve because
        therefore however if then trigger signal watch look for buy sell long
        short avoid skip analysis postmortem alpha tracker onchain divergence
    '''.split())
    words = normalized.split()
    if not words:
        return 0.0
    substance_count = sum(1 for w in words if w in substance_words)
    return min(1.0, substance_count / max(len(words) * 0.15, 1))


# ─── Per-platform parsers ───────────────────────────────────────────


def parse_telegram_web(markdown: str, url: str, run_uuid: str, git_sha: str,
                       seen_urls: dict) -> list:
    """Parse a t.me/s/<channel> page into raw social events.
    TG web preview shows message blocks with text, timestamps, and forward/reply metadata."""
    events = []
    retrieval_time = now_unix_ms()

    # TG web messages are typically separated by date headers and have
    # text content after image/link blocks. Extract message blocks.
    # Each message block in TG web preview has a relative timestamp link.

    # Split by common TG message separators (date dividers)
    # The markdown from TG is messy; extract text blocks between date markers
    message_blocks = re.split(r'\n(?=\d{2}:\d{2})', markdown)

    for block in message_blocks:
        normalized = normalize_text(block)
        if len(normalized) < 15:
            continue

        text_hash = sha256_hex(normalized)

        # Extract channel name from URL
        channel = url.split("/s/")[-1] if "/s/" in url else "unknown"

        # Extract TG message ID from URL patterns (if present in markdown)
        # TG web preview URLs contain message IDs: /s/channel/12345
        msg_id_match = re.search(r'/(\d{5,})', block)
        source_id = msg_id_match.group(1) if msg_id_match else sha256_hex(block)[:12]

        # Check if already seen
        event_key = f"telegram|{channel}|{source_id}"
        if event_key in seen_urls:
            continue
        seen_urls[event_key] = retrieval_time

        # Extract entities
        cashtags = extract_cashtags(block)
        addresses = extract_solana_addresses(block)
        urls = extract_urls(block)

        # Classify
        usefulness = classify_usefulness(block, "telegram", channel)
        signal_density = compute_signal_density(block)

        publish_time = estimate_publish_time(block, "telegram")

        event = {
            "event_id": sha256_hex(block),
            "schema_version": SCHEMA_VERSION,
            "pipeline_version": PIPELINE_VERSION,
            "run_uuid": run_uuid,
            "git_sha": git_sha,

            "platform": "telegram",
            "account_handle": channel,
            "account_id": channel,
            "account_type": "alpha" if "calls" in channel.lower() or "alpha" in channel.lower() else "community",
            "source_id": source_id,
            "url": url,

            "publish_time_ms": publish_time,
            "first_seen_ms": retrieval_time,  # first time WE observed it
            "retrieval_time_ms": retrieval_time,
            "ingestion_latency_ms": retrieval_time - publish_time,

            "text": block.strip()[:10000],  # cap at 10K chars
            "text_hash": text_hash,
            "engagement_likes": None,
            "engagement_reposts": None,
            "engagement_replies": None,
            "engagement_views": None,
            "is_echo": bool(re.search(r'forwarded from|→', block)),
            "is_edit": False,
            "is_delete": False,

            "cashtags": "|".join(cashtags) if cashtags else None,
            "contract_addresses": "|".join(addresses) if addresses else None,
            "mentioned_mints": "|".join(addresses) if addresses else None,
            "mentioned_tokens": "|".join(cashtags) if cashtags else None,
            "urls_in_text": "|".join(urls) if urls else None,

            "retrieval_method": "firecrawl",
            "raw_payload_path": "",
            "admission_status": "RAW",
            "contamination_flags": None,

            "usefulness_class": usefulness,
            "signal_density": round(signal_density, 3),
        }

        events.append(event)

    return events


def parse_pumpfun_board(markdown: str, url: str, run_uuid: str, git_sha: str,
                        seen_urls: dict) -> list:
    """Parse pump.fun board/explore page into raw events (token listings)."""
    events = []
    retrieval_time = now_unix_ms()

    # Extract token entries from the board page
    # pump.fun board shows token cards with names, mints, market caps, etc.
    # The markdown contains links to /coin/<mint> pages

    coin_links = re.findall(r'\[([^\]]+)\]\((https?://pump\.fun/coin/[^\)]+)\)', markdown)
    # Also extract mint addresses from URLs
    mints = list(set(re.findall(r'pump\.fun/coin/([A-Za-z0-9]{32,44})', markdown)))

    for mint in mints:
        normalized = f"pump_fun_coin_{mint}"
        text_hash = sha256_hex(normalized)
        event_key = f"pumpfun_board|{mint}"
        if event_key in seen_urls:
            continue
        seen_urls[event_key] = retrieval_time

        event = {
            "event_id": sha256_hex(normalized),
            "schema_version": SCHEMA_VERSION,
            "pipeline_version": PIPELINE_VERSION,
            "run_uuid": run_uuid,
            "git_sha": git_sha,

            "platform": "web",
            "account_handle": "pump.fun_board",
            "account_id": "pump.fun",
            "account_type": "tracker",
            "source_id": mint,
            "url": f"https://pump.fun/coin/{mint}",

            "publish_time_ms": retrieval_time,  # board is live; publish = retrieval
            "first_seen_ms": retrieval_time,
            "retrieval_time_ms": retrieval_time,
            "ingestion_latency_ms": 0,

            "text": f"Pump.fun board listing: {mint}",
            "text_hash": text_hash,
            "engagement_likes": None,
            "engagement_reposts": None,
            "engagement_replies": None,
            "engagement_views": None,
            "is_echo": False,
            "is_edit": False,
            "is_delete": False,

            "cashtags": None,
            "contract_addresses": mint,
            "mentioned_mints": mint,
            "mentioned_tokens": None,
            "urls_in_text": f"https://pump.fun/coin/{mint}",

            "retrieval_method": "firecrawl",
            "raw_payload_path": "",
            "admission_status": "RAW",
            "contamination_flags": None,

            "usefulness_class": "TOOL_WALLET_SIGNAL",
            "signal_density": 0.0,
        }
        events.append(event)

    return events


def parse_dexscreener(markdown: str, url: str, run_uuid: str, git_sha: str,
                      seen_urls: dict) -> list:
    """Parse DexScreener Solana page into raw events."""
    events = []
    retrieval_time = now_unix_ms()

    # Extract token pairs from DexScreener
    # URLs like dexscreener.com/solana/<pair_address>
    pairs = list(set(re.findall(r'dexscreener\.com/solana/([A-Za-z0-9]{32,44})', markdown)))

    for pair in pairs:
        normalized = f"dexscreener_pair_{pair}"
        text_hash = sha256_hex(normalized)
        event_key = f"dexscreener|{pair}"
        if event_key in seen_urls:
            continue
        seen_urls[event_key] = retrieval_time

        event = {
            "event_id": sha256_hex(normalized),
            "schema_version": SCHEMA_VERSION,
            "pipeline_version": PIPELINE_VERSION,
            "run_uuid": run_uuid,
            "git_sha": git_sha,

            "platform": "web",
            "account_handle": "dexscreener",
            "account_id": "dexscreener",
            "account_type": "tracker",
            "source_id": pair,
            "url": f"https://dexscreener.com/solana/{pair}",

            "publish_time_ms": retrieval_time,
            "first_seen_ms": retrieval_time,
            "retrieval_time_ms": retrieval_time,
            "ingestion_latency_ms": 0,

            "text": f"DexScreener listing: {pair}",
            "text_hash": text_hash,
            "engagement_likes": None,
            "engagement_reposts": None,
            "engagement_replies": None,
            "engagement_views": None,
            "is_echo": False,
            "is_edit": False,
            "is_delete": False,

            "cashtags": None,
            "contract_addresses": pair,
            "mentioned_mints": pair,
            "mentioned_tokens": None,
            "urls_in_text": f"https://dexscreener.com/solana/{pair}",

            "retrieval_method": "firecrawl",
            "raw_payload_path": "",
            "admission_status": "RAW",
            "contamination_flags": None,

            "usefulness_class": "TOOL_WALLET_SIGNAL",
            "signal_density": 0.0,
        }
        events.append(event)

    return events


def parse_youtube_search(markdown: str, url: str, run_uuid: str, git_sha: str,
                         seen_urls: dict) -> list:
    """Parse YouTube search results into raw events (video content)."""
    events = []
    retrieval_time = now_unix_ms()

    # Extract video IDs and titles from YouTube search results
    # YouTube video IDs are 11-char base64: /watch?v=<11chars>
    videos = re.findall(r'/watch\?v=([A-Za-z0-9_-]{11})', markdown)
    # Also extract titles from markdown links
    titles = re.findall(r'\[([^\]]{10,})\]\(https?://www\.youtube\.com/watch', markdown)

    for i, vid_id in enumerate(list(set(videos))):
        title = titles[i] if i < len(titles) else f"youtube_video_{vid_id}"
        normalized = normalize_text(title)
        text_hash = sha256_hex(normalized)
        event_key = f"youtube|{vid_id}"
        if event_key in seen_urls:
            continue
        seen_urls[event_key] = retrieval_time

        usefulness = classify_usefulness(title, "youtube", "search_result")

        event = {
            "event_id": sha256_hex(f"youtube_{vid_id}"),
            "schema_version": SCHEMA_VERSION,
            "pipeline_version": PIPELINE_VERSION,
            "run_uuid": run_uuid,
            "git_sha": git_sha,

            "platform": "youtube",
            "account_handle": "youtube_search",
            "account_id": "youtube",
            "account_type": "community",
            "source_id": vid_id,
            "url": f"https://www.youtube.com/watch?v={vid_id}",

            "publish_time_ms": retrieval_time,  # unknown without video page
            "first_seen_ms": retrieval_time,
            "retrieval_time_ms": retrieval_time,
            "ingestion_latency_ms": 0,

            "text": title,
            "text_hash": text_hash,
            "engagement_likes": None,
            "engagement_reposts": None,
            "engagement_replies": None,
            "engagement_views": None,
            "is_echo": False,
            "is_edit": False,
            "is_delete": False,

            "cashtags": "|".join(extract_cashtags(title)) if extract_cashtags(title) else None,
            "contract_addresses": None,
            "mentioned_mints": None,
            "mentioned_tokens": "|".join(extract_cashtags(title)) if extract_cashtags(title) else None,
            "urls_in_text": f"https://www.youtube.com/watch?v={vid_id}",

            "retrieval_method": "firecrawl",
            "raw_payload_path": "",
            "admission_status": "RAW",
            "contamination_flags": None,

            "usefulness_class": usefulness,
            "signal_density": compute_signal_density(title),
        }
        events.append(event)

    return events


def parse_twitch_clips(markdown: str, url: str, run_uuid: str, git_sha: str,
                       seen_urls: dict) -> list:
    """Parse Twitch clips/about/directory pages into raw events.
    Twitch clips are 6-20s video segments — NO transcripts. We extract:
      - Clip titles (topic signals: what trading concepts the streamer discusses)
      - Streamer identity (follower count, linked socials, live status)
      - For about pages: linked YouTube/Twitter channels (identity enrichment)
    """
    events = []
    retrieval_time = now_unix_ms()

    # Extract streamer handle from URL
    handle_match = re.search(r'twitch\.tv/([a-zA-Z0-9_]+)/?(?:[/?]|$)', url)
    streamer = handle_match.group(1) if handle_match else "unknown"

    # Determine page type
    is_clips = "/clips" in url
    is_about = "/about" in url
    is_directory = "/directory/" in url

    if is_clips:
        # Extract clip titles: [title\n-----](clip_url)
        clip_pattern = re.compile(
            r'\[([^\]]+)\s*\n[-]+\s*\]\((https://www\.twitch\.tv/[^/]+/clip/[^\)]+)\)'
        )
        clips = clip_pattern.findall(markdown)

        seen_clip_urls = set()
        for title, clip_url in clips:
            clip_url = clip_url.strip()
            if clip_url in seen_clip_urls:
                continue
            seen_clip_urls.add(clip_url)
            title = title.strip()
            if not title or title == '.':
                continue

            normalized = normalize_text(f"{streamer} clip: {title}")
            text_hash = sha256_hex(normalized)
            event_key = f"twitch_clip|{streamer}|{clip_url}"
            if event_key in seen_urls:
                continue
            seen_urls[event_key] = retrieval_time

            usefulness = classify_usefulness(title, "twitch", streamer)
            # Clip titles are short — bump reasoning clips to STRATEGY
            reasoning_keywords = ['how to', 'what is', 'why', 'good coin', 'scam', 'dev',
                                  'strategy', 'entry', 'exit', 'analyze', 'trade', 'pnl']
            if any(kw in title.lower() for kw in reasoning_keywords):
                usefulness = "STRATEGY"

            event = {
                "event_id": sha256_hex(f"twitch_{clip_url}"),
                "schema_version": SCHEMA_VERSION,
                "pipeline_version": PIPELINE_VERSION,
                "run_uuid": run_uuid,
                "git_sha": git_sha,

                "platform": "twitch",
                "account_handle": streamer,
                "account_id": streamer,
                "account_type": "elite_trader" if streamer in ("cented", "megga") else "trader",
                "source_id": clip_url.split("/")[-1],
                "url": clip_url,

                "publish_time_ms": retrieval_time,  # clip publish unknown without API
                "first_seen_ms": retrieval_time,
                "retrieval_time_ms": retrieval_time,
                "ingestion_latency_ms": 0,

                "text": f"Twitch clip by {streamer}: {title}",
                "text_hash": text_hash,
                "engagement_likes": None,
                "engagement_reposts": None,
                "engagement_replies": None,
                "engagement_views": None,
                "is_echo": False,
                "is_edit": False,
                "is_delete": False,

                "cashtags": None,
                "contract_addresses": None,
                "mentioned_mints": None,
                "mentioned_tokens": None,
                "urls_in_text": clip_url,

                "retrieval_method": "firecrawl",
                "raw_payload_path": "",
                "admission_status": "RAW",
                "contamination_flags": None,

                "usefulness_class": usefulness,
                "signal_density": compute_signal_density(title),
            }
            events.append(event)

    elif is_about:
        # Extract identity enrichment: followers, linked socials
        followers = re.search(r'(\d+[\d,.]*[K|M]?)\s*followers', markdown)
        twitter = re.search(r'twitter\.com/(\w+)', markdown)
        youtube = re.search(r'youtube\.com/channel/([A-Za-z0-9_-]+)', markdown)
        instagram = re.search(r'instagram\.com/(\w+)', markdown)
        live_match = re.search(r'Live\s+(?:with\s+\d+\s+viewers|Now)', markdown)
        stream_title = re.search(r'(?:streaming|streams)\s+(.+?)(?:\.|\n)', markdown)

        # Build enrichment record
        enrichment = {
            "followers": followers.group(1) if followers else None,
            "twitter": twitter.group(1) if twitter else None,
            "youtube_channel": youtube.group(1) if youtube else None,
            "instagram": instagram.group(1) if instagram else None,
            "is_live": bool(live_match),
            "stream_title": stream_title.group(1).strip()[:200] if stream_title else None,
        }

        normalized = normalize_text(f"{streamer} twitch profile: {json.dumps(enrichment)}")
        text_hash = sha256_hex(normalized)
        event_key = f"twitch_about|{streamer}"
        if event_key in seen_urls:
            return events
        seen_urls[event_key] = retrieval_time

        event = {
            "event_id": sha256_hex(f"twitch_about_{streamer}"),
            "schema_version": SCHEMA_VERSION,
            "pipeline_version": PIPELINE_VERSION,
            "run_uuid": run_uuid,
            "git_sha": git_sha,

            "platform": "twitch",
            "account_handle": streamer,
            "account_id": streamer,
            "account_type": "elite_trader" if streamer in ("cented", "megga") else "trader",
            "source_id": f"about_{streamer}",
            "url": url,

            "publish_time_ms": retrieval_time,
            "first_seen_ms": retrieval_time,
            "retrieval_time_ms": retrieval_time,
            "ingestion_latency_ms": 0,

            "text": json.dumps(enrichment, ensure_ascii=False),
            "text_hash": text_hash,
            "engagement_likes": None,
            "engagement_reposts": None,
            "engagement_replies": None,
            "engagement_views": None,
            "is_echo": False,
            "is_edit": False,
            "is_delete": False,

            "cashtags": None,
            "contract_addresses": None,
            "mentioned_mints": None,
            "mentioned_tokens": None,
            "urls_in_text": None,

            "retrieval_method": "firecrawl",
            "raw_payload_path": "",
            "admission_status": "RAW",
            "contamination_flags": None,

            "usefulness_class": "TOOL_WALLET_SIGNAL",  # identity enrichment
            "signal_density": 0.0,
        }
        events.append(event)

    elif is_directory:
        # Parse the Twitch crypto directory for currently-live streamers
        # Directory format: [stream_title\n----](streamer_url) then viewer count
        # The URL format is: https://www.twitch.tv/<streamer_name>
        streamer_pattern = re.compile(
            r'\[([^\]]{5,})\s*\n[-]+\s*\]\(https://www\.twitch\.tv/([a-zA-Z0-9_]+)\)'
        )
        streamers = streamer_pattern.findall(markdown)

        # Deduplicate — same streamer may appear multiple times
        seen_streamers = set()
        for stream_title, streamer_name in streamers:
            if streamer_name in seen_streamers:
                continue
            seen_streamers.add(streamer_name)
            stream_title = stream_title.strip()
            normalized = normalize_text(f"twitch_live: {streamer_name} streaming: {stream_title}")
            text_hash = sha256_hex(normalized)
            event_key = f"twitch_dir|{streamer_name}"
            if event_key in seen_urls:
                continue
            seen_urls[event_key] = retrieval_time

            event = {
                "event_id": sha256_hex(f"twitch_dir_{streamer_name}"),
                "schema_version": SCHEMA_VERSION,
                "pipeline_version": PIPELINE_VERSION,
                "run_uuid": run_uuid,
                "git_sha": git_sha,

                "platform": "twitch",
                "account_handle": streamer_name,
                "account_id": streamer_name,
                "account_type": "trader",
                "source_id": f"dir_{streamer_name}",
                "url": f"https://www.twitch.tv/{streamer_name}",

                "publish_time_ms": retrieval_time,
                "first_seen_ms": retrieval_time,
                "retrieval_time_ms": retrieval_time,
                "ingestion_latency_ms": 0,

                "text": f"Twitch live: {streamer_name} — {stream_title}",
                "text_hash": text_hash,
                "engagement_likes": None,
                "engagement_reposts": None,
                "engagement_replies": None,
                "engagement_views": None,
                "is_echo": False,
                "is_edit": False,
                "is_delete": False,

                "cashtags": None,
                "contract_addresses": None,
                "mentioned_mints": None,
                "mentioned_tokens": None,
                "urls_in_text": f"https://www.twitch.tv/{streamer_name}",

                "retrieval_method": "firecrawl",
                "raw_payload_path": "",
                "admission_status": "RAW",
                "contamination_flags": None,

                "usefulness_class": "TOOL_WALLET_SIGNAL",
                "signal_density": compute_signal_density(stream_title),
            }
            events.append(event)

    return events


def parse_generic_web(markdown: str, url: str, run_uuid: str, git_sha: str,
                      seen_urls: dict, source_type: str = "web") -> list:
    """Generic web page parser — extracts text blocks and classifies."""
    events = []
    retrieval_time = now_unix_ms()

    # Split markdown into paragraphs
    paragraphs = re.split(r'\n\n+', markdown)

    for para in paragraphs:
        normalized = normalize_text(para)
        if len(normalized) < 30:
            continue

        text_hash = sha256_hex(normalized)
        event_key = f"web|{sha256_hex(normalized)[:16]}"
        if event_key in seen_urls:
            continue
        seen_urls[event_key] = retrieval_time

        usefulness = classify_usefulness(para, "web", "unknown")
        if usefulness == "LOW_SIGNAL_CHATTER":
            continue  # skip empty chatter in web pages

        cashtags = extract_cashtags(para)
        addresses = extract_solana_addresses(para)

        event = {
            "event_id": sha256_hex(para),
            "schema_version": SCHEMA_VERSION,
            "pipeline_version": PIPELINE_VERSION,
            "run_uuid": run_uuid,
            "git_sha": git_sha,

            "platform": "web",
            "account_handle": "web_crawl",
            "account_id": url,
            "account_type": "anonymous",
            "source_id": sha256_hex(para)[:16],
            "url": url,

            "publish_time_ms": retrieval_time,  # unknown for generic web
            "first_seen_ms": retrieval_time,
            "retrieval_time_ms": retrieval_time,
            "ingestion_latency_ms": 0,

            "text": para[:10000],
            "text_hash": text_hash,
            "engagement_likes": None,
            "engagement_reposts": None,
            "engagement_replies": None,
            "engagement_views": None,
            "is_echo": False,
            "is_edit": False,
            "is_delete": False,

            "cashtags": "|".join(cashtags) if cashtags else None,
            "contract_addresses": "|".join(addresses) if addresses else None,
            "mentioned_mints": "|".join(addresses) if addresses else None,
            "mentioned_tokens": "|".join(cashtags) if cashtags else None,
            "urls_in_text": "|".join(extract_urls(para)) if extract_urls(para) else None,

            "retrieval_method": "firecrawl",
            "raw_payload_path": "",
            "admission_status": "RAW",
            "contamination_flags": None,

            "usefulness_class": usefulness,
            "signal_density": round(compute_signal_density(para), 3),
        }
        events.append(event)

    return events


# ─── Writer ─────────────────────────────────────────────────────────


class RawWriter:
    """Writes raw events to immutable JSONL files."""

    BATCH_SIZE = 5000
    TIME_WINDOW_MS = 3600 * 1000

    def __init__(self, run_uuid: str, git_sha: str):
        self.run_uuid = run_uuid
        self.git_sha = git_sha
        self.events_written = 0
        self.files_written = []
        self.current_batch = []
        self.batch_start_time = now_unix_ms()
        self.file_index = 0
        RAW_DIR.mkdir(parents=True, exist_ok=True)

    def add(self, event: dict):
        self.current_batch.append(event)
        self.events_written += 1
        if len(self.current_batch) >= self.BATCH_SIZE or \
           (now_unix_ms() - self.batch_start_time) >= self.TIME_WINDOW_MS:
            self.flush()

    def flush(self):
        if not self.current_batch:
            return
        fname = f"raw_{self.file_index:04d}.jsonl"
        fpath = RAW_DIR / fname
        tmp_path = RAW_DIR / f"{fname}.tmp"
        with open(tmp_path, "w", encoding="utf-8") as f:
            for event in self.current_batch:
                event["raw_payload_path"] = str(fpath)
                f.write(json.dumps(event, ensure_ascii=False) + "\n")
        # Atomic temp→final (os.replace works on both Windows and Linux)
        os.replace(tmp_path, fpath)
        file_hash = sha256_hex(open(fpath, "r", encoding="utf-8").read())
        file_size = os.path.getsize(fpath)
        self.files_written.append({
            "filename": fname,
            "path": str(fpath),
            "rows": len(self.current_batch),
            "bytes": file_size,
            "sha256": file_hash,
        })
        self.current_batch = []
        self.batch_start_time = now_unix_ms()
        self.file_index += 1
        print(f"  Wrote {fname}: {self.files_written[-1]['rows']} rows, "
              f"{file_size / 1024:.1f} KB")

    def finalize(self):
        self.flush()
        manifest = {
            "schema_version": SCHEMA_VERSION,
            "pipeline_version": PIPELINE_VERSION,
            "run_uuid": self.run_uuid,
            "git_sha": self.git_sha,
            "created_at": now_utc_iso(),
            "total_events": self.events_written,
            "total_files": len(self.files_written),
            "files": self.files_written,
        }
        with open(MANIFEST_PATH, "w", encoding="utf-8") as f:
            json.dump(manifest, f, indent=2)
        print(f"\nManifest: {MANIFEST_PATH}")
        print(f"Total events: {self.events_written}")
        print(f"Total files: {len(self.files_written)}")


# ─── Seen-URLs tracking (change detection / dedup) ──────────────────


def load_seen_urls() -> dict:
    if SEEN_URLS_PATH.exists():
        with open(SEEN_URLS_PATH, "r") as f:
            return json.load(f)
    return {}


def save_seen_urls(seen: dict):
    with open(SEEN_URLS_PATH, "w") as f:
        json.dump(seen, f, indent=2)


# ─── Main acquisition ───────────────────────────────────────────────


def get_git_sha() -> str:
    import subprocess
    try:
        result = subprocess.run(
            ["git", "rev-parse", "--short", "HEAD"],
            capture_output=True, text=True, timeout=10,
            cwd=str(PIPELINE_ROOT)
        )
        return result.stdout.strip() if result.returncode == 0 else "unknown"
    except Exception:
        return "unknown"


SOURCE_PARSERS = {
    "telegram": ("telegram", parse_telegram_web),
    "pumpfun": ("pumpfun", parse_pumpfun_board),
    "dexscreener": ("dexscreener", parse_dexscreener),
    "youtube": ("youtube", parse_youtube_search),
    "twitch": ("twitch", parse_twitch_clips),
    "web": ("web", parse_generic_web),
}


def run_acquisition(sources: list, max_pages: int, dry_run: bool):
    run_uuid = sha256_hex(str(now_unix_ms())).hex()[:12] if False else hashlib.sha256(str(now_unix_ms()).encode()).hexdigest()[:12]
    git_sha = get_git_sha()

    OUTPUT_BASE.mkdir(parents=True, exist_ok=True)
    RAW_DIR.mkdir(parents=True, exist_ok=True)

    # Record first_seen (framework start, NOT first_seen for individual items)
    first_seen_utc = now_utc_iso()
    first_seen_ms = now_unix_ms()

    with open(FIRST_SEEN_LOG, "w") as f:
        f.write(f"narrative_gold_v1 acquisition FIRST_SEEN (framework start)\n")
        f.write(f"UTC: {first_seen_utc}\n")
        f.write(f"Unix_ms: {first_seen_ms}\n")
        f.write(f"Run_UUID: {run_uuid}\n")
        f.write(f"Git_SHA: {git_sha}\n")
        f.write(f"Schema: {SCHEMA_VERSION}\n")
        f.write(f"Pipeline: {PIPELINE_VERSION}\n")

    print("=" * 70)
    print("NARRATIVE_GOLD_V1 — ACTIVE WEB ACQUISITION (Local Firecrawl)")
    print("=" * 70)
    print(f"Run UUID:        {run_uuid}")
    print(f"Git SHA:         {git_sha}")
    print(f"Firecrawl URL:   {FIRECRAWL_URL}")
    print(f"Framework start: {first_seen_utc}")
    print(f"Sources:         {', '.join(sources)}")
    print(f"Max pages:       {max_pages}")
    print(f"Dry run:         {dry_run}")
    print()

    writer = RawWriter(run_uuid, git_sha)
    seen_urls = load_seen_urls()
    coverage = {"scraped": 0, "succeeded": 0, "failed": 0, "events": 0, "gaps": []}

    for source_type in sources:
        if source_type not in SEED_URLS:
            print(f"  Unknown source type: {source_type}")
            continue

        urls = SEED_URLS[source_type]
        parser_key, parser_fn = SOURCE_PARSERS.get(source_type, ("web", parse_generic_web))

        print(f"\n--- {source_type.upper()} ({len(urls)} URLs) ---")

        for url in urls:
            coverage["scraped"] += 1
            print(f"  Scraping: {url}")

            if dry_run:
                coverage["succeeded"] += 1
                continue

            # SPA pages need waitFor for dynamic content to render
            wait_ms = 0
            if source_type == "twitch":
                wait_ms = 4000  # Twitch clips/directory are SPAs, need render time
            elif source_type == "youtube":
                wait_ms = 3000  # YouTube search results need render time

            result = fc_scrape(url, wait_ms=wait_ms)
            if not result or not result.get("success"):
                coverage["failed"] += 1
                coverage["gaps"].append({"url": url, "reason": "scrape_failed"})
                print(f"    FAILED (coverage gap)")
                continue

            markdown = result.get("data", {}).get("markdown", "")
            if not markdown or len(markdown) < 10:
                coverage["failed"] += 1
                coverage["gaps"].append({"url": url, "reason": "empty_content"})
                print(f"    EMPTY (coverage gap)")
                continue

            coverage["succeeded"] += 1
            print(f"    Got {len(markdown)} bytes of markdown")

            # Parse into events
            events = parser_fn(markdown, url, run_uuid, git_sha, seen_urls)
            print(f"    Parsed {len(events)} events")

            for event in events:
                writer.add(event)
            coverage["events"] += len(events)

    # Save seen URLs for change detection
    save_seen_urls(seen_urls)

    # Finalize
    if not dry_run:
        writer.finalize()

    # Update acquisition state
    acq_state = {
        "run_uuid": run_uuid,
        "git_sha": git_sha,
        "first_seen_utc": first_seen_utc,
        "first_seen_ms": first_seen_ms,
        "status": "COMPLETED_WEB_ACQUISITION",
        "last_update": now_utc_iso(),
        "events_acquired": writer.events_written,
        "coverage": coverage,
    }
    with open(ACQ_STATE_PATH, "w") as f:
        json.dump(acq_state, f, indent=2)

    # Report
    print("\n" + "=" * 70)
    print("ACQUISITION REPORT")
    print("=" * 70)
    print(f"Scraped:    {coverage['scraped']}")
    print(f"Succeeded:  {coverage['succeeded']}")
    print(f"Failed:     {coverage['failed']}")
    print(f"Events:     {coverage['events']}")
    print(f"Coverage gaps: {len(coverage['gaps'])}")
    for gap in coverage["gaps"]:
        print(f"  - {gap['url']}: {gap['reason']}")

    # Usefulness breakdown
    if writer.events_written > 0 and not dry_run:
        print("\nUSEFULNESS BREAKDOWN:")
        # Read back the first file for a quick sample
        if writer.files_written:
            with open(writer.files_written[0]["path"], "r") as f:
                classes = {}
                for line in f:
                    event = json.loads(line)
                    cls = event.get("usefulness_class", "UNRESOLVED")
                    classes[cls] = classes.get(cls, 0) + 1
                for cls, count in sorted(classes.items(), key=lambda x: -x[1]):
                    print(f"  {cls}: {count}")


def main():
    parser = argparse.ArgumentParser(
        description="narrative_gold_v1 active web acquisition via local Firecrawl"
    )
    parser.add_argument("--sources", nargs="+", default=["all"],
                        help="Source types: telegram pumpfun dexscreener youtube web all")
    parser.add_argument("--max-pages", type=int, default=20,
                        help="Max pages to scrape per source")
    parser.add_argument("--dry-run", action="store_true",
                        help="Don't write, just test connectivity")
    args = parser.parse_args()

    if "all" in args.sources:
        sources = list(SEED_URLS.keys())
    else:
        sources = args.sources

    run_acquisition(sources, args.max_pages, args.dry_run)


if __name__ == "__main__":
    main()
