#!/usr/bin/env python3
"""Probe j7tracker.io for a reachable API surface (proxyable endpoint discovery).

Method: load the SPA shell, enumerate its JS bundles, and grep them for API base
URLs / path prefixes / websocket hosts. Report candidates with the evidence that
produced them. Nothing is assumed: a candidate is only reported if a literal
appears in a served asset.
"""
import json
import re
import sys
import urllib.parse
import urllib.request

UA = ("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) "
      "Chrome/126 Safari/537.36")
BASE = "https://j7tracker.io/"

PATTERNS = [
    r"https?://[a-zA-Z0-9._-]*j7tracker[a-zA-Z0-9._-]*(?:/[A-Za-z0-9._~/-]*)?",
    r"https?://api\.[a-zA-Z0-9._-]+",
    r"https?://[a-zA-Z0-9._-]+\.workers\.dev",
    r"wss?://[a-zA-Z0-9._-]+(?::\d+)?(?:/[A-Za-z0-9._~/-]*)?",
    r"[\"'\`](/api/[A-Za-z0-9._~/{}$-]{1,80})[\"'\`]",
    r"[\"'\`](/v[0-9]/[A-Za-z0-9._~/{}$-]{1,80})[\"'\`]",
    r"baseURL\s*[:=]\s*[\"'\`]([^\"'\`]{1,120})[\"'\`]",
    r"[\"'\`](/metadata/[A-Za-z0-9._~/{}$-]{1,60})[\"'\`]",
]


def get(url, timeout=25):
    req = urllib.request.Request(url, headers={"User-Agent": UA, "Accept": "*/*"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return r.status, r.read().decode("utf-8", "replace")


def main():
    try:
        st, html = get(BASE)
    except Exception as e:
        print("shell fetch failed:", e)
        return 1
    print(f"shell: http={st} bytes={len(html)}")

    srcs = re.findall(r'<script[^>]+src=["\']([^"\']+)["\']', html)
    links = re.findall(r'<link[^>]+href=["\']([^"\']+)["\']', html)
    assets = []
    for s in srcs + links:
        if s.startswith("//"):
            s = "https:" + s
        elif s.startswith("/"):
            s = urllib.parse.urljoin(BASE, s)
        if s.startswith("http") and "j7tracker" in s or s.startswith("https://j7tracker"):
            assets.append(s)
    print(f"assets referenced: {len(assets)}")
    for a in assets[:20]:
        print("  ", a)

    hits = {}
    scanned = 0
    for a in assets[:25]:
        try:
            st, body = get(a)
        except Exception as e:
            print(f"  !! {a}: {e}")
            continue
        scanned += 1
        for p in PATTERNS:
            for m in re.finditer(p, body):
                v = m.group(1) if m.groups() else m.group(0)
                hits.setdefault(v, set()).add(a.split("/")[-1][:40])
    print(f"scanned {scanned} assets")
    print("\ncandidate endpoints / bases (literal -> seen in):")
    for v, where in sorted(hits.items(), key=lambda x: -len(x[1])):
        print(f"  {v:<70} {sorted(where)[:3]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
