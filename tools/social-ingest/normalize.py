"""Shared normalization for every `[S]` social adapter.

Every adapter (X, Telegram, TikTok, web) captures its vendor-specific objects and
calls `build(...)` here to emit the ONE vendor-agnostic JSON the deterministic Rust
core (`pump_quant_ingest::social_parse::parse_social_event`) consumes — one compact
object per line (NDJSON) on stdout. Keeping this in one module guarantees all
adapters speak the identical schema, so the core never learns a vendor's quirks.

Schema (exactly the fields the Rust parser reads):
    {"platform": "x|telegram|tiktok|web",
     "author": "<origin id>", "community": "<channel or ''>",
     "text": "<raw text w/ cashtags + contract addrs left intact>",
     "likes": int, "reposts": int, "replies": int, "echo": bool}

`echo=True` is the single "not an originator" signal (reply / retweet / forward /
quote) the core uses for fade-first breadth — reach is not alpha. The capture
instant is stamped downstream by the probe/engine at parse time (the `[S]`
boundary); production adapters may stamp it at capture for exact Signal-Horizon
latency. No sentiment/opinion field is ever emitted — that would be a research
artifact, never a decision input (§83).
"""
import json
import sys

VALID_PLATFORMS = ("x", "telegram", "tiktok", "web")


def build(
    platform,
    author,
    text,
    *,
    community="",
    likes=0,
    reposts=0,
    replies=0,
    echo=False,
):
    """Assemble one normalized event dict. Non-int engagement coerces to 0."""
    if platform not in VALID_PLATFORMS:
        raise ValueError(f"unknown platform {platform!r}; use one of {VALID_PLATFORMS}")

    def _int(x):
        try:
            return max(0, int(x))
        except (TypeError, ValueError):
            return 0

    return {
        "platform": platform,
        "author": str(author or "unknown"),
        "community": str(community or ""),
        "text": str(text or ""),
        "likes": _int(likes),
        "reposts": _int(reposts),
        "replies": _int(replies),
        "echo": bool(echo),
    }


def dumps(ev):
    """Compact single-line JSON for one event."""
    return json.dumps(ev, separators=(",", ":"), ensure_ascii=False)


def write(ev, out=sys.stdout):
    """Emit one event as an NDJSON line and flush (real-time friendly)."""
    out.write(dumps(ev) + "\n")
    out.flush()


def run_selftest(samples):
    """Print normalized JSON for a list of pre-built events (no network/keys).

    Adapters expose `--selftest` by mapping a couple of baked vendor objects
    through their own normalizer into `build(...)` and passing the results here,
    proving the adapter emits schema-correct JSON before any key exists."""
    for ev in samples:
        # Round-trip through build() so the schema is enforced even in selftest.
        write(
            build(
                ev["platform"],
                ev["author"],
                ev["text"],
                community=ev.get("community", ""),
                likes=ev.get("likes", 0),
                reposts=ev.get("reposts", 0),
                replies=ev.get("replies", 0),
                echo=ev.get("echo", False),
            )
        )
    sys.stderr.write(f"[selftest] emitted {len(samples)} normalized events\n")
    return 0
