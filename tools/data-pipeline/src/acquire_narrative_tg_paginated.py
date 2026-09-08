#!/usr/bin/env python3
"""
acquire_narrative_tg_paginated.py — Deep-paginated Telegram channel scraper.

Scrapes public TG web preview channels backwards in time using ?before=<msg_id>
to reach the Slinky Gold v3 time window (June 5 – July 14, 2026).

Output goes to the same raw_social_event_v1 directory as the main acquisition,
so downstream gold builders can merge them.

Usage:
  python src/acquire_narrative_tg_paginated.py --max-pages 50 --target-date 2026-06-05
"""

import os
import sys
import re
import json
import time
import hashlib
import argparse
from datetime import datetime, timezone

# Reuse utilities from the main acquisition script
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from acquire_narrative_web_v1 import (
    SCHEMA_VERSION,
    PIPELINE_VERSION,
    FIRECRAWL_URL,
    sha256_hex,
    now_utc_iso,
    now_unix_ms,
    normalize_text,
    extract_cashtags,
    extract_solana_addresses,
    extract_urls,
    classify_usefulness,
    compute_signal_density,
    estimate_publish_time,
)

OUTPUT_BASE = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                           "output", "narrative_gold_v1")
RAW_DIR = os.path.join(OUTPUT_BASE, "raw", "raw_social_event_v1")
SEEN_URLS_PATH = os.path.join(OUTPUT_BASE, "raw", "seen_urls.json")

# Priority channels — tightly Pump.fun/Solana memecoin focused
PRIORITY_CHANNELS = [
    "PikalosiCalls",      # alpha calls, high-signal
    "Marlonalpha",        # narrative + risk
    "alphacalls",         # alpha aggregator
    "jacalcooks",         # community trader
    "PikalosiLounge",     # community
    "crypticannouncements",
    "chasescharts",
    "potionalpha",
    "solanaalpha",
]


def get_git_sha():
    try:
        import subprocess
        return subprocess.check_output(["git", "rev-parse", "--short", "HEAD"],
                                      cwd=os.path.dirname(os.path.dirname(os.path.dirname(
                                          os.path.abspath(__file__)))),
                                      text=True).strip()
    except Exception:
        return "unknown"


def fc_scrape(url: str, fmt: str = "markdown", wait_ms: int = 0):
    """Scrape a URL via local Firecrawl (uses /v1/scrape endpoint)."""
    import subprocess

    payload = json.dumps({
        "url": url,
        "formats": [fmt],
        "waitFor": wait_ms if wait_ms else 0,
    })

    try:
        result = subprocess.run(
            ["curl", "-s", "-X", "POST", f"{FIRECRAWL_URL}/v1/scrape",
             "-H", "Content-Type: application/json",
             "-d", json.dumps({
                 "url": url,
                 "formats": [fmt],
                 "waitFor": wait_ms if wait_ms else 0,
             })],
            capture_output=True, text=True, timeout=90,
        )
        if result.returncode != 0:
            print(f"    Firecrawl curl error: {result.stderr[:200]}")
            return None
        return json.loads(result.stdout)
    except Exception as e:
        print(f"    Firecrawl error: {e}")
        return None


def extract_pagination_id(markdown: str, channel: str) -> int:
    """Extract the ?before= pagination ID from a TG page.
    TG web preview has a link like: [](https://t.me/s/<channel>?before=588)
    Also extract message IDs from /<channel>/<id> patterns (any channel for forwarded msgs)."""
    # First try: explicit ?before= link (always uses the current channel)
    before_match = re.search(rf't\.me/s/{re.escape(channel)}\?before=(\d+)', markdown)
    if before_match:
        return int(before_match.group(1))

    # Second try: min message ID from ANY t.me/<channel>/<id> pattern
    # (forwarded messages have different channel names but IDs are still valid for pagination)
    msg_ids = re.findall(r't\.me/[^/]+/(\d+)', markdown)
    if msg_ids:
        # Use the min ID that belongs to the CURRENT channel for ?before= pagination
        own_ids = re.findall(rf't\.me/{re.escape(channel)}/(\d+)', markdown)
        if own_ids:
            return min(int(i) for i in own_ids)
        return min(int(i) for i in msg_ids)

    return 0


def parse_telegram_page(markdown: str, channel: str, url: str,
                        run_uuid: str, git_sha: str, seen_urls: dict) -> tuple:
    """Parse a single TG page into events. Returns (events, pagination_id)."""
    events = []
    retrieval_time = now_unix_ms()

    # TG web preview structure:
    # Page header contains month/date headers like "December 17, 2024" or "June 5"
    # Messages are grouped by date — the date header appears BEFORE messages for that date
    # Each message ends with "Nviews[HH:MM](t.me/<channel>/<id>)"
    # We need to track the current date header and assign it to each message

    # Extract date headers from the markdown: "Month DD, YYYY" or "Month DD"
    date_header_re = re.compile(
        r'((?:January|February|March|April|May|June|July|August|September|October|November|December)'
        r'\s+(\d{1,2})\s*,?\s*(?:(\d{4}))?)',
        re.IGNORECASE,
    )

    # TG web preview has multiple message-terminator formats:
    # Format A: "292 views[13:28](https://t.me/PikalosiCalls/508)" — same-channel link
    # Format B: "204 viewsMarlonx1, [10:06](https://t.me/Marlonalpha/234)" — text between views and time
    # Format C: "4.7K views[10:21](https://t.me/mduzcalls/2029)" — forwarded from different channel
    message_pattern = rf'((?:\d+\.?\d*K?\s*)?views?(?:[^\s\[]*)?\s*,?\s*\[?(\d{{2}}:\d{{2}})\]?\((?:https?://t\.me/[^/]+/\d+)\))'

    # Also need a fallback: just match [HH:MM](t.me/<channel>/<id>) for any channel
    time_link_pattern = r'\[(\d{2}:\d{2})\]\(https?://t\.me/([^/]+)/(\d+)\)'

    # Split the markdown by the time-link pattern
    parts = re.split(message_pattern, markdown)

    # Reconstruct message blocks: each message is the text BEFORE a views+timestamp marker
    # plus the marker itself. Track date headers as we go.
    blocks = []
    current_block = ""
    current_date = None  # (month_name, day, year_or_None)

    for part in parts:
        # Check if this part is a message terminator
        term_match = re.match(message_pattern, part) if part else None
        if term_match:
            current_block = current_block + part
            # Extract time from terminator
            time_str = term_match.group(2) if term_match.lastindex >= 2 else None
            blocks.append((current_block, current_date, time_str))
            current_block = ""
        else:
            # Check for date header in this part
            if part:
                dh = date_header_re.search(part)
                if dh:
                    month_name = dh.group(1).split()[0]  # "June"
                    day = dh.group(2)
                    year = dh.group(3) if dh.group(3) else None
                    current_date = (month_name, day, year)
            current_block = current_block + part

    # Handle any remaining text
    if current_block.strip():
        blocks.append((current_block, current_date, None))

    for block_text, date_info, time_str in blocks:
        block = block_text.strip()
        if len(block) < 15:
            continue

        # Extract message ID from any t.me/<channel>/<id> pattern (may be forwarded)
        msg_id_match = re.search(r't\.me/[^/]+/(\d+)', block)

        if msg_id_match:
            source_id = msg_id_match.group(1)
        else:
            source_id = sha256_hex(block)[:12]

        # Dedup
        event_key = f"telegram|{channel}|{source_id}"
        if event_key in seen_urls:
            continue
        seen_urls[event_key] = retrieval_time

        # Extract entities from ORIGINAL block (preserve case)
        cashtags = extract_cashtags(block)
        addresses = extract_solana_addresses(block)
        urls_list = extract_urls(block)

        usefulness = classify_usefulness(block, "telegram", channel)
        signal_density = compute_signal_density(block)

        # Compute publish_time from the date header + time stamp
        publish_time = retrieval_time  # fallback: retrieval time
        if date_info:
            from datetime import datetime as _dt, timezone as _tz
            _MONTHS_MAP = {
                'january': 1, 'february': 2, 'march': 3, 'april': 4, 'may': 5, 'june': 6,
                'july': 7, 'august': 8, 'september': 9, 'october': 10, 'november': 11, 'december': 12,
                'jan': 1, 'feb': 2, 'mar': 3, 'apr': 4, 'may': 5, 'jun': 6,
                'jul': 7, 'aug': 8, 'sep': 9, 'oct': 10, 'nov': 11, 'dec': 12,
            }
            month_name, day, year = date_info
            month = _MONTHS_MAP.get(month_name.lower(), 0)
            if month and day:
                yr = int(year) if year else 2026
                hr, minute = 0, 0
                if time_str and ':' in time_str:
                    parts_t = time_str.split(':')
                    hr, minute = int(parts_t[0]), int(parts_t[1])
                try:
                    dt = _dt(yr, month, int(day), hr, minute, tzinfo=_tz.utc)
                    publish_time = int(dt.timestamp() * 1000)
                except ValueError:
                    pass

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
            "first_seen_ms": retrieval_time,
            "retrieval_time_ms": retrieval_time,
            "ingestion_latency_ms": retrieval_time - publish_time,

            "text": block[:10000],
            "text_hash": sha256_hex(normalize_text(block)),
            "engagement_likes": None,
            "engagement_reposts": None,
            "engagement_replies": None,
            "engagement_views": None,
            "is_echo": bool(re.search(r'forwarded from|→', block)),
            "is_edit": bool(re.search(r'edited', block)),
            "is_delete": False,

            "cashtags": "|".join(cashtags) if cashtags else None,
            "contract_addresses": "|".join(addresses) if addresses else None,
            "mentioned_mints": "|".join(addresses) if addresses else None,
            "mentioned_tokens": "|".join(cashtags) if cashtags else None,
            "urls_in_text": "|".join(urls_list) if urls_list else None,

            "retrieval_method": "firecrawl_paginated",
            "raw_payload_path": "",
            "admission_status": "RAW",
            "contamination_flags": None,
            "usefulness_class": usefulness,
            "signal_density": round(signal_density, 3),
        }

        events.append(event)

    # Get pagination ID for next page
    pagination_id = extract_pagination_id(markdown, channel)

    return events, pagination_id


def load_seen_urls():
    if os.path.exists(SEEN_URLS_PATH):
        with open(SEEN_URLS_PATH, "r") as f:
            return json.loads(f.read())
    return {}


def save_seen_urls(seen):
    with open(SEEN_URLS_PATH, "w") as f:
        json.dump(seen, f)


def run_paginated_acquisition(max_pages: int, target_date: str, channels: list):
    """Scrape TG channels backwards using ?before=<msg_id> pagination."""
    run_uuid = hashlib.sha256(str(now_unix_ms()).encode()).hexdigest()[:12]
    git_sha = get_git_sha()

    os.makedirs(RAW_DIR, exist_ok=True)

    target_ts = int(datetime.strptime(target_date, "%Y-%m-%d").timestamp() * 1000)
    print(f"Target date: {target_date} (unix_ms: {target_ts})")

    seen_urls = load_seen_urls()
    total_events = 0
    total_new_events = 0

    # Use append mode for the paginated raw file
    raw_file_path = os.path.join(RAW_DIR, f"raw_paginated_{run_uuid}.jsonl")
    raw_file = open(raw_file_path, "w", encoding="utf-8")

    for channel in channels:
        print(f"\n=== Channel: {channel} ===")
        channel_events = 0
        before_id = None
        consecutive_empty = 0

        for page in range(max_pages):
            # Construct URL with pagination
            if before_id:
                url = f"https://t.me/s/{channel}?before={before_id}"
            else:
                url = f"https://t.me/s/{channel}"

            print(f"  Page {page+1}/{max_pages}: {url}")

            result = fc_scrape(url)
            if not result or not result.get("success"):
                print(f"    FAILED")
                consecutive_empty += 1
                if consecutive_empty >= 3:
                    print(f"    3 consecutive failures, stopping channel")
                    break
                time.sleep(1)
                continue

            markdown = result.get("data", {}).get("markdown", "")
            if not markdown or len(markdown) < 50:
                print(f"    EMPTY ({len(markdown)} bytes)")
                consecutive_empty += 1
                if consecutive_empty >= 3:
                    print(f"    3 consecutive empty, stopping channel")
                    break
                time.sleep(1)
                continue

            consecutive_empty = 0
            events, pagination_id = parse_telegram_page(markdown, channel, url,
                                                      run_uuid, git_sha, seen_urls)

            new_events = 0
            for event in events:
                raw_file.write(json.dumps(event) + "\n")
                new_events += 1

            channel_events += new_events
            total_events += len(events)
            total_new_events += new_events

            print(f"    Parsed {len(events)} events ({new_events} new), pagination_id={pagination_id}")

            # Pagination: use pagination_id as the next before parameter
            if pagination_id and pagination_id > 1:
                # Prevent infinite loops: if before_id doesn't change, stop
                if before_id == pagination_id:
                    print(f"    Pagination ID unchanged, stopping channel")
                    break
                before_id = pagination_id
            else:
                print(f"    No pagination ID found, stopping channel")
                break

            # Check if we've gone far enough back in time
            # Estimate: each message is ~1 hour apart on average for active channels
            # If we've collected enough pages, we should be in the target window
            time.sleep(0.5)  # rate limit

        print(f"  Channel total: {channel_events} new events")
        save_seen_urls(seen_urls)

    raw_file.close()
    save_seen_urls(seen_urls)

    print(f"\n=== SUMMARY ===")
    print(f"Total events parsed: {total_events}")
    print(f"Total new events: {total_new_events}")
    print(f"Output: {raw_file_path}")
    print(f"Run UUID: {run_uuid}")
    print(f"Git SHA: {git_sha}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Paginated TG channel acquisition")
    parser.add_argument("--max-pages", type=int, default=50,
                        help="Max pages per channel")
    parser.add_argument("--target-date", type=str, default="2026-06-05",
                        help="Target date to scrape back to (YYYY-MM-DD)")
    parser.add_argument("--channels", nargs="*", default=PRIORITY_CHANNELS,
                        help="Channels to scrape")
    args = parser.parse_args()

    run_paginated_acquisition(args.max_pages, args.target_date, args.channels)
