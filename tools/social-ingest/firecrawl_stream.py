#!/usr/bin/env python3
"""Firecrawl → normalized SocialEvent JSON (the `[S]` general-web breadth adapter).

The downstream / legibility tier: scrapes news, aggregators, and project pages to
markdown and turns each page into one `web` event whose text the core scans for
cashtags + contract addresses. This is NOT a real-time or primary source — it is
the "what has become legible / crowded" clock (a page listing a coin means it is
already surfaced). Legal, cheap ($0.0006–0.003/page); never used to scrape X/TikTok
and never with a personal logged-in session.

    export FIRECRAWL_API_KEY=fc-...
    python3 firecrawl_stream.py --url https://dexscreener.com/solana \
        | cargo run --quiet --manifest-path probe/Cargo.toml

`--selftest` needs no key. `[S]`: no decision, no sentiment label (§83).
"""
import argparse
import json
import os
import sys
import time
import urllib.parse
import urllib.request

import normalize

SCRAPE = "https://api.firecrawl.dev/v1/scrape"
MAX_TEXT = 20000  # cap page text so one NDJSON line stays manageable


def load_pages(path):
    try:
        import yaml
    except ImportError:
        return []
    try:
        with open(path, encoding="utf-8") as f:
            data = yaml.safe_load(f) or {}
        return ((data.get("web") or {}).get("pages")) or []
    except FileNotFoundError:
        return []


def domain_of(url: str) -> str:
    try:
        return urllib.parse.urlparse(url).netloc or "web"
    except ValueError:
        return "web"


def scrape(url: str, key: str) -> str:
    """Firecrawl scrape → markdown string (empty on failure)."""
    body = json.dumps({"url": url, "formats": ["markdown"]}).encode("utf-8")
    req = urllib.request.Request(
        SCRAPE,
        data=body,
        headers={"Authorization": f"Bearer {key}", "Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=60) as resp:
        data = json.loads(resp.read().decode("utf-8"))
    d = data.get("data") or data
    return (d.get("markdown") or d.get("content") or "")[:MAX_TEXT]


def normalize_page(url: str, markdown: str) -> dict:
    """A scraped page → one `web` event (author = domain; no engagement)."""
    return normalize.build(
        "web",
        author=domain_of(url),
        text=markdown,
        community=domain_of(url),
        likes=0,
        reposts=0,
        replies=0,
        echo=False,
    )


def selftest() -> int:
    md = "trending on solana: $WIF EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v and $BONK"
    return normalize.run_selftest([normalize_page("https://dexscreener.com/solana", md)])


def main() -> int:
    ap = argparse.ArgumentParser(description="Firecrawl → normalized SocialEvent NDJSON")
    ap.add_argument("--url", default="", help="single URL; else all from sources.yaml")
    ap.add_argument("--sources", default="sources.yaml")
    ap.add_argument("--watch", type=float, default=0.0)
    ap.add_argument("--selftest", action="store_true")
    args = ap.parse_args()
    if args.selftest:
        return selftest()

    key = os.environ.get("FIRECRAWL_API_KEY", "").strip()
    if not key:
        sys.stderr.write("error: set FIRECRAWL_API_KEY (https://firecrawl.dev)\n")
        return 2

    urls = [args.url] if args.url else load_pages(args.sources)
    if not urls:
        sys.stderr.write("no pages configured (sources.yaml web.pages) and no --url\n")
        return 2

    def one_pass() -> int:
        n = 0
        for url in urls:
            try:
                md = scrape(url, key)
            except Exception as e:  # noqa: BLE001
                sys.stderr.write(f"scrape error ({url}): {e}\n")
                continue
            if md:
                normalize.write(normalize_page(url, md))
                n += 1
        return n

    if args.watch > 0:
        while True:
            sys.stderr.write(f"[firecrawl] scraped {one_pass()} pages\n")
            time.sleep(args.watch)
    else:
        sys.stderr.write(f"scraped {one_pass()} pages\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
