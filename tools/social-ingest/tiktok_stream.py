#!/usr/bin/env python3
"""TikTok → normalized SocialEvent JSON (the `[S]` TikTok adapter).

The slow-meta / broad-narrative-emergence tier: TikTok is higher-latency and less
useful for fast entries, but good for detecting a meme going broadly viral before
it saturates CT. Polled, not real-time. Provider-agnostic: point it at whichever
scraper you subscribe to (Data365 ~$0.60/1k multi-platform, or ScrapeBadger) via
`TIKTOK_API_BASE` + `TIKTOK_API_KEY`; the normalizer maps the common video fields.

    export TIKTOK_API_KEY=...   TIKTOK_API_BASE=https://<provider>/tiktok/hashtag
    python3 tiktok_stream.py --hashtag solana \
        | cargo run --quiet --manifest-path probe/Cargo.toml

Reliability note: TikTok scrapers degrade after platform updates — treat gaps as
missing data, never fabricate. `--selftest` needs no key. `[S]`: no decision, no
sentiment label (§83).
"""
import argparse
import json
import os
import sys
import time
import urllib.parse
import urllib.request

import normalize


def load_hashtags(path):
    try:
        import yaml
    except ImportError:
        return ["solana", "memecoin"]
    try:
        with open(path, encoding="utf-8") as f:
            data = yaml.safe_load(f) or {}
        return ((data.get("tiktok") or {}).get("hashtags")) or ["solana", "memecoin"]
    except FileNotFoundError:
        return ["solana", "memecoin"]


def fetch(hashtag: str, base: str, key: str):
    """One provider call for a hashtag feed. Returns a list of video objects.
    Defensive to provider shape: accepts {videos|data|items|results:[...]}"""
    params = urllib.parse.urlencode({"hashtag": hashtag, "count": 50})
    req = urllib.request.Request(f"{base}?{params}", headers={"Authorization": f"Bearer {key}"})
    with urllib.request.urlopen(req, timeout=30) as resp:
        data = json.loads(resp.read().decode("utf-8"))
    for k in ("videos", "data", "items", "results"):
        if isinstance(data.get(k), list):
            return data[k]
    return data if isinstance(data, list) else []


def normalize_video(v: dict) -> dict:
    """TikTok video object → normalized event. Description + hashtags form the text
    the core scans for cashtags/contract addresses. Digg=likes, share, comment; a
    duet/stitch is an echo."""
    author = (v.get("author") or {})
    handle = author.get("uniqueId") or author.get("nickname") or v.get("authorName") or "unknown"
    desc = v.get("desc") or v.get("description") or v.get("title") or ""
    tags = v.get("hashtags") or v.get("challenges") or []
    tagtext = " ".join(f"#{t.get('name', t) if isinstance(t, dict) else t}" for t in tags)
    stats = v.get("stats") or v
    is_echo = bool(v.get("duetInfo") or v.get("stitchInfo") or v.get("isDuet") or v.get("isStitch"))
    return normalize.build(
        "tiktok",
        author=handle,
        text=f"{desc} {tagtext}".strip(),
        community="",
        likes=stats.get("diggCount", stats.get("likeCount", 0)),
        reposts=stats.get("shareCount", 0),
        replies=stats.get("commentCount", 0),
        echo=is_echo,
    )


def selftest() -> int:
    samples = [
        normalize_video(
            {
                "author": {"uniqueId": "memelord"},
                "desc": "this $WIF coin is insane EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                "hashtags": [{"name": "solana"}, {"name": "memecoin"}],
                "stats": {"diggCount": 12000, "shareCount": 800, "commentCount": 300},
            }
        ),
        normalize_video(
            {
                "author": {"uniqueId": "duetguy"},
                "desc": "duet reacting to $WIF",
                "isDuet": True,
                "stats": {"diggCount": 50, "shareCount": 1, "commentCount": 2},
            }
        ),
    ]
    return normalize.run_selftest(samples)


def main() -> int:
    ap = argparse.ArgumentParser(description="TikTok → normalized SocialEvent NDJSON")
    ap.add_argument("--hashtag", default="", help="single hashtag; else all from sources.yaml")
    ap.add_argument("--sources", default="sources.yaml")
    ap.add_argument("--watch", type=float, default=0.0)
    ap.add_argument("--selftest", action="store_true")
    args = ap.parse_args()
    if args.selftest:
        return selftest()

    key = os.environ.get("TIKTOK_API_KEY", "").strip()
    base = os.environ.get("TIKTOK_API_BASE", "").strip()
    if not (key and base):
        sys.stderr.write("error: set TIKTOK_API_KEY and TIKTOK_API_BASE (your provider endpoint)\n")
        return 2

    tags = [args.hashtag] if args.hashtag else load_hashtags(args.sources)
    seen: set[str] = set()

    def one_pass() -> int:
        n = 0
        for tag in tags:
            try:
                vids = fetch(tag, base, key)
            except Exception as e:  # noqa: BLE001
                sys.stderr.write(f"fetch error ({tag}): {e}\n")
                continue
            for v in vids:
                vid = str(v.get("id", v.get("aweme_id", "")))
                if vid and vid in seen:
                    continue
                seen.add(vid)
                normalize.write(normalize_video(v))
                n += 1
        return n

    if args.watch > 0:
        while True:
            sys.stderr.write(f"[tiktok] emitted {one_pass()} (seen {len(seen)})\n")
            time.sleep(args.watch)
    else:
        sys.stderr.write(f"emitted {one_pass()} normalized events\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
