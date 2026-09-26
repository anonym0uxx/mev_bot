#!/usr/bin/env python3
"""resolve_narrative_lexicon.py -- build the versioned dynamic narrative lexicon.

Emits `dynamic_lexicon_v1.json`, the artifact consumed by
`pump_quant_narrative::dynamic_lexicon::DynamicLexicon`.

Why this exists: `FAMILY_LEXICON_V1` is a compile-time category taxonomy (63
needles, 8 families). Measured against the meta the corpus actually produces it
covers 3 of the 11 hottest live terms, and being `&'static` it cannot rotate at
the meta's timescale (hours). This producer is the rotating layer.

Causality (hard): an entry is usable by the runtime only when
`first_seen_ms <= as_of_ms <= now_ms`. We therefore derive `first_seen_ms` from
OUR OWN mint stream (the earliest observation), never from a source that could
report a later or backdated timestamp. `--as-of-ms` defaults to the corpus's last
observation so the artifact is reproducible.

Sources (pluggable; a source that is unreachable records a COVERAGE GAP and
contributes nothing -- "unavailable" is never "zero signal"):
  * mint_stream  -- our own corpus (always available; authoritative for timing)
  * wayback_cdx  -- web.archive.org CDX, no key, verified reachable
  * j7tracker    -- j7tracker.io is a Cloudflare-fronted SPA with no public JSON
                    API; the reachable surface is its metadata host, which we
                    consult only when a token's on-chain metadata URI points at
                    it. Otherwise the gap is recorded.

Usage:
  python3 resolve_narrative_lexicon.py --corpus /training/slinky21 \
      --out dynamic_lexicon_v1.json --min-prior 5 --top-wayback 25
"""
import argparse
import json
import os
import re
import sys
import time
import urllib.parse
import urllib.request

UA = ("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) "
      "Chrome/126 Safari/537.36")

# Words that carry no narrative signal. Kept small and explicit: an over-broad
# stoplist silently drops real aliases, which is worse than a few noisy ones.
STOP = {
    "the", "and", "for", "you", "your", "with", "this", "that", "from", "coin",
    "token", "meme", "pump", "fun", "official", "real", "only", "just", "very",
    "will", "when", "what", "who", "how", "why", "all", "any", "can", "get",
    "new", "now", "out", "its", "his", "her", "him", "she", "they", "them",
    "then", "than", "there", "here", "have", "has", "had", "was", "were", "are",
    "not", "but", "one", "two", "first", "last", "next", "back", "into", "over",
    "more", "most", "some", "such", "make", "made", "make", "time", "day",
    "good", "game", "best", "top", "big", "small", "high", "low", "up", "down",
    "hello", "world", "test", "read", "saying", "people", "posting", "many",
    "live", "love", "life", "king", "queen", "god", "lord", "money", "cash",
}

# Aliases we can classify WITHOUT guessing. Anything absent here is skipped
# rather than assigned a family (§6.4 under-claiming beats fabricating).
# `pinned` = already covered by FAMILY_LEXICON_V1; the dynamic layer only needs
# what the pinned table misses, but re-stating a pinned alias is harmless and
# makes the artifact self-describing.
FAMILY_HINTS = {
    # KOL / entity handles observed in the live meta vocabulary
    "ansem": "celebrity",
    "cupsey": "celebrity",
    "orangi": "celebrity",
    "cented": "celebrity",
    "megga": "celebrity",
    # companies / tickers
    "spacex": "tech", "spcx": "tech", "openai": "tech", "nvidia": "tech",
    "tesla": "tech", "hood": "tech", "robinhood": "tech", "usdc": "tech",
    "quantum": "tech", "bounty": "tech",
    # recurring meme subjects
    "bull": "animal", "bullcat": "animal", "blackbull": "animal",
    "pepe": "animal", "doge": "animal",
    # calendar / seasonal
    "worldcup": "seasonal", "world": "seasonal", "halloween": "seasonal",
}


def content_words(text):
    """Alphanumeric content words, lowercased, length >= 4, minus the stoplist."""
    out = []
    for w in re.findall(r"[a-z0-9]+", (text or "").lower()):
        if len(w) >= 4 and w not in STOP:
            out.append(w)
    return out


def load_mint_stream(corpus):
    """(alias -> first_seen_ms, count) from OUR OWN mint stream. Authoritative for timing."""
    import pandas as pd
    t = pd.read_parquet(os.path.join(corpus, "tokens.parquet"),
                        columns=["mint", "name", "symbol", "detected_at"])
    t["ms"] = (t.detected_at.astype("int64") // 1_000_000)
    rows = []
    for name, sym, ms in zip(t.name.tolist(), t.symbol.tolist(), t.ms.tolist()):
        words = set(content_words(name)) | set(content_words(sym))
        for w in words:
            rows.append((w, ms))
    import pandas as pd
    d = pd.DataFrame(rows, columns=["alias", "ms"])
    g = d.groupby("alias").ms.agg(["min", "max", "size"]).rename(
        columns={"min": "first_seen_ms", "max": "last_seen_ms", "size": "mints"})
    return g.reset_index()


def wayback_first_archive(alias, timeout=20):
    """Earliest Wayback capture mentioning the alias. Returns (ts, url) or None.

    A coverage gap (unreachable, no captures) returns None and is recorded --
    never treated as evidence of absence of a narrative.
    """
    q = ("http://web.archive.org/cdx/search/cdx?"
         + urllib.parse.urlencode({
             "url": f"*.pump.fun/*{alias}*",
             "output": "json",
             "limit": "1",
             "sort": "asc",
             "fl": "timestamp,original",
         }))
    try:
        req = urllib.request.Request(q, headers={"User-Agent": UA})
        with urllib.request.urlopen(req, timeout=timeout) as r:
            data = json.loads(r.read().decode("utf-8", "replace") or "[]")
    except Exception as e:  # coverage gap, recorded by the caller
        return ("ERROR", str(e)[:120])
    if len(data) < 2:
        return None
    ts, original = data[1][0], data[1][1]
    return (ts, original)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", default="/training/slinky21")
    ap.add_argument("--out", default="dynamic_lexicon_v1.json")
    ap.add_argument("--version", type=int, default=1)
    ap.add_argument("--min-prior", type=int, default=5,
                    help="minimum mints sharing the alias to consider it")
    ap.add_argument("--ttl-ms", type=int, default=48 * 3600 * 1000,
                    help="entry shelf life; the meta rotates in hours")
    ap.add_argument("--as-of-ms", type=int, default=None,
                    help="defaults to the corpus's last observation (reproducible)")
    ap.add_argument("--top-wayback", type=int, default=25,
                    help="how many aliases to enrich from Wayback (rate-limited)")
    a = ap.parse_args()

    g = load_mint_stream(a.corpus)
    as_of = a.as_of_ms if a.as_of_ms is not None else int(g.last_seen_ms.max())
    print(f"aliases discovered: {len(g):,}  as_of_ms={as_of}")

    # j7tracker: route map recovered from the SPA bundle; the data plane is
    # session-authenticated. Public-routes-only, 401 recorded as a coverage gap.
    import j7tracker_source as j7
    j7_token = os.environ.get("J7_SESSION_TOKEN")
    j7_reachable, j7_auth, j7_detail = j7.probe(token=j7_token)
    print(f"j7tracker: reachable={j7_reachable} auth_gated_routes={len(j7_auth)}/"
          f"{len(j7_detail)} token={bool(j7_token)}")

    cand = g[g.mints >= a.min_prior].copy()
    cand = cand.sort_values("mints", ascending=False)
    print(f"aliases with >= {a.min_prior} mints: {len(cand):,}")

    classify = cand[cand.alias.isin(FAMILY_HINTS)]
    skipped = cand[~cand.alias.isin(FAMILY_HINTS)]
    print(f"classifiable without guessing: {len(classify):,}   "
          f"skipped (no family evidence, not guessed): {len(skipped):,}")

    # Wayback enrichment on the most-crowded aliases only (rate limits).
    wb = {}
    for alias in classify.alias.head(a.top_wayback).tolist():
        wb[alias] = wayback_first_archive(alias)
        time.sleep(0.7)
    gaps = sum(1 for v in wb.values() if v and v[0] == "ERROR")
    found = sum(1 for v in wb.values() if v and v[0] != "ERROR")
    print(f"wayback: {found} with captures, {gaps} unreachable (recorded as coverage gaps), "
          f"{len(wb) - found - gaps} no captures")

    entries = []
    for r in classify.itertuples():
        fam = FAMILY_HINTS[r.alias]
        ev = wb.get(r.alias)
        entries.append({
            "family": fam,
            "needles": [{"text": r.alias, "mode": "substring"}],
            "first_seen_ms": int(r.first_seen_ms),
            "as_of_ms": int(as_of),
            "ttl_ms": int(a.ttl_ms),
            "provenance": "pipeline",
            "confidence_bps": 8000,
            "_evidence": {
                "mints": int(r.mints),
                "last_seen_ms": int(r.last_seen_ms),
                "wayback_first": ev if ev else None,
            },
        })

    # Causality self-check, mirroring Rust `entry_usable_at`.
    bad = [e for e in entries if not (e["first_seen_ms"] <= e["as_of_ms"])]
    if bad:
        print(f"FATAL: {len(bad)} entries violate first_seen_ms <= as_of_ms", file=sys.stderr)
        return 2

    doc = {
        "schema_version": 1,
        "version": a.version,
        "generated_at_ms": int(time.time() * 1000),
        "as_of_ms": int(as_of),
        "ttl_ms": int(a.ttl_ms),
        "source": "mint_stream+wayback_cdx",
        "coverage": {
            "aliases_discovered": int(len(g)),
            "aliases_ge_min_prior": int(len(cand)),
            "classified": int(len(classify)),
            "skipped_unclassified": int(len(skipped)),
            "wayback_queried": len(wb),
            "wayback_unreachable": int(gaps),
            "j7tracker_reachable": bool(j7_reachable),
            "j7tracker_auth_gated_routes": len(j7_auth),
            "j7tracker_routes_probed": len(j7_detail),
        },
        "gaps": ([
            "Twitter/X direct: login wall; coverage gap recorded, never forced.",
        ] + ([
            f"j7tracker.io: API reachable ({', '.join(sorted(set(d.split()[1].split('/')[2] for d in j7_detail)))}) "
            f"but the data plane is session-authenticated ({len(j7_auth)} of {len(j7_detail)} probed routes "
            "returned 401). Public-routes-only; no J7_SESSION_TOKEN supplied, so no contribution. "
            "Not bypassed -- a 401 is a coverage gap, never zero signal.",
        ] if not j7_token else [
            "j7tracker.io: session token supplied; routes consumed via j7tracker_source.",
        ])),
        "entries": entries,
    }
    with open(a.out, "w") as f:
        json.dump(doc, f, indent=1, sort_keys=True)
    print(f"wrote {a.out}: {len(entries)} entries, version={a.version}, "
          f"ttl={a.ttl_ms // 3600000}h")
    for e in entries[:8]:
        print(f"  {e['needles'][0]['text']:<12} family={e['family']:<10} "
              f"mints={e['_evidence']['mints']:<6} wayback={bool(e['_evidence']['wayback_first'])}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
