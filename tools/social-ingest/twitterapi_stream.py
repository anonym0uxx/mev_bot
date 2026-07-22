#!/usr/bin/env python3
"""twitterapi.io → normalized SocialEvent JSON (the `[S]` X capture adapter).

Reference live adapter for X/Twitter — the only place a clock and the network are
touched. It captures matching tweets and normalizes each into the vendor-agnostic
JSON the deterministic Rust decoder consumes (one NDJSON object per line). It runs
the constitution's three X filter classes, driven by `sources.yaml`:

  --class firehose   cashtag / contract-address firehose (breadth + mention velocity)
  --class amplifier  from: the KOL watchlist — PUBLIC_BURNED, for WAVE-TIMING + FADE,
                     never entry, never copy (§28/§29 pre-legibility)
  --class list       a curated CT list (e.g. the Greek-CT cluster)

    export TWITTERAPI_IO_KEY=sk-...            # twitterapi.io, pay-as-you-go
    python3 twitterapi_stream.py --class firehose --watch 5 \
        | cargo run --quiet --manifest-path probe/Cargo.toml

Cost: advanced_search ~$0.15/1000 tweets. Only stdlib (urllib). `--selftest` needs
no key. This file is `[S]`: it never computes a decision, and emits no sentiment
label (§22, §83).
"""
import argparse
import json
import os
import sys
import time
import urllib.parse
import urllib.request

import normalize

BASE = "https://api.twitterapi.io/twitter/tweet/advanced_search"

DEFAULT_FIREHOSE = '($SOL OR "pump.fun" OR url:pump.fun) -is:retweet'


def load_sources(path):
    try:
        import yaml
    except ImportError:
        return {}
    try:
        with open(path, encoding="utf-8") as f:
            return yaml.safe_load(f) or {}
    except FileNotFoundError:
        return {}


def build_query(cls: str, src: dict, override: str) -> str:
    """Compose the advanced-search query for the chosen filter class."""
    if override:
        return override
    x = src.get("x") or {}
    if cls == "firehose":
        return x.get("firehose_query") or DEFAULT_FIREHOSE
    if cls == "amplifier":
        accts = x.get("amplifier_accounts") or []
        if not accts:
            raise SystemExit("no amplifier_accounts in sources.yaml")
        return " OR ".join(f"from:{a}" for a in accts)
    if cls == "list":
        lists = x.get("lists") or []
        if not lists:
            raise SystemExit("no lists in sources.yaml")
        return f"list:{lists[0]['id']}"
    raise SystemExit(f"unknown class {cls!r}")


def fetch_page(query: str, query_type: str, cursor: str, key: str):
    params = urllib.parse.urlencode({"query": query, "queryType": query_type, "cursor": cursor})
    req = urllib.request.Request(f"{BASE}?{params}", headers={"X-API-Key": key})
    with urllib.request.urlopen(req, timeout=30) as resp:
        data = json.loads(resp.read().decode("utf-8"))
    return data.get("tweets", []), bool(data.get("has_next_page", False)), data.get("next_cursor", "")


def normalize_tweet(tweet: dict) -> dict:
    """twitterapi.io tweet object → normalized event. Reply/retweet/quote = echo."""
    author = (tweet.get("author") or {}).get("userName", "") or "unknown"
    is_echo = (
        bool(tweet.get("isReply"))
        or tweet.get("retweeted_tweet") is not None
        or tweet.get("quoted_tweet") is not None
    )
    return normalize.build(
        "x",
        author=author,
        text=tweet.get("text", "") or "",
        community="",
        likes=tweet.get("likeCount", 0),
        reposts=tweet.get("retweetCount", 0),
        replies=tweet.get("replyCount", 0),
        echo=is_echo,
    )


def selftest() -> int:
    samples = [
        normalize_tweet(
            {
                "author": {"userName": "cryptoKOL"},
                "text": "send it $WIF EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                "likeCount": 420,
                "retweetCount": 69,
                "replyCount": 12,
                "isReply": False,
            }
        ),
        normalize_tweet(
            {
                "author": {"userName": "echoAcct"},
                "text": "RT $WIF to the moon",
                "likeCount": 2,
                "retweeted_tweet": {"id": "x"},
            }
        ),
    ]
    return normalize.run_selftest(samples)


def main() -> int:
    ap = argparse.ArgumentParser(description="twitterapi.io → normalized SocialEvent NDJSON")
    ap.add_argument("--class", dest="cls", default="firehose", choices=["firehose", "amplifier", "list"])
    ap.add_argument("--sources", default="sources.yaml")
    ap.add_argument("--query", default="", help="override the class query")
    ap.add_argument("--type", default="Latest", choices=["Latest", "Top"])
    ap.add_argument("--pages", type=int, default=1)
    ap.add_argument("--watch", type=float, default=0.0, help="poll every N seconds if >0")
    ap.add_argument("--selftest", action="store_true", help="emit sample events; no key")
    args = ap.parse_args()

    if args.selftest:
        return selftest()

    key = os.environ.get("TWITTERAPI_IO_KEY", "").strip()
    if not key:
        sys.stderr.write("error: set TWITTERAPI_IO_KEY (https://twitterapi.io, pay-as-you-go)\n")
        return 2

    query = build_query(args.cls, load_sources(args.sources), args.query)
    sys.stderr.write(f"[x:{args.cls}] query: {query}\n")
    seen: set[str] = set()

    def one_pass() -> int:
        emitted, cursor = 0, ""
        for _ in range(max(1, args.pages)):
            try:
                tweets, has_next, cursor = fetch_page(query, args.type, cursor, key)
            except Exception as e:  # noqa: BLE001
                sys.stderr.write(f"fetch error: {e}\n")
                return emitted
            for t in tweets:
                tid = str(t.get("id", ""))
                if tid and tid in seen:
                    continue
                seen.add(tid)
                normalize.write(normalize_tweet(t))
                emitted += 1
            if not has_next or not cursor:
                break
        return emitted

    if args.watch > 0:
        sys.stderr.write(f"[watch] polling every {args.watch}s; Ctrl-C to stop\n")
        while True:
            n = one_pass()
            sys.stderr.write(f"[watch] {args.cls}: emitted {n} new (seen {len(seen)})\n")
            time.sleep(args.watch)
    else:
        sys.stderr.write(f"emitted {one_pass()} normalized events\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
