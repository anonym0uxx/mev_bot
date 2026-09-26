#!/usr/bin/env python3
"""Deep-grep the j7tracker SPA bundle for the DATA plane (not the account API).

Writes the bundle to disk once, then extracts:
  * every j7tracker subdomain with surrounding context
  * every quoted path literal
  * keyword-adjacent strings (mint/token/search/metadata/trending/narrative/...)
"""
import re
import sys
import urllib.request

UA = ("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) "
      "Chrome/126 Safari/537.36")
BUNDLE = "https://j7tracker.io/static/js/main.f144aea5.js"
OUT = "/tmp/j7_main.js"

KEYS = ["mint", "token", "search", "metadata", "trending", "narrative", "call",
        "holder", "profile", "track", "watch", "alert", "kol", "smart"]


def main():
    try:
        req = urllib.request.Request(BUNDLE, headers={"User-Agent": UA})
        with urllib.request.urlopen(req, timeout=40) as r:
            body = r.read().decode("utf-8", "replace")
    except Exception as e:
        print("bundle fetch failed:", e)
        return 1
    open(OUT, "w").write(body)
    print(f"bundle bytes={len(body)} -> {OUT}")

    print("\n=== j7tracker hosts (deduped) ===")
    hosts = {}
    for m in re.finditer(r"([a-z0-9-]+)\.j7tracker\.io", body):
        hosts[m.group(1)] = hosts.get(m.group(1), 0) + 1
    for h, n in sorted(hosts.items(), key=lambda x: -x[1]):
        print(f"  {h}.j7tracker.io   x{n}")

    print("\n=== quoted path literals (top 60 by frequency) ===")
    paths = {}
    for m in re.finditer(r'["\'`](/[A-Za-z0-9._~/{}$-]{2,70})["\'`]', body):
        p = m.group(1)
        paths[p] = paths.get(p, 0) + 1
    for p, n in sorted(paths.items(), key=lambda x: -x[1])[:60]:
        print(f"  {p:<60} x{n}")

    print("\n=== keyword-adjacent literals ===")
    for k in KEYS:
        found = set()
        for m in re.finditer(r'["\'`]([A-Za-z0-9._~/{}$-]{0,60}' + k + r'[A-Za-z0-9._~/{}$-]{0,60})["\'`]',
                             body, re.I):
            s = m.group(1)
            if len(s) >= 4:
                found.add(s)
        if found:
            print(f"  [{k}] {sorted(found)[:12]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
