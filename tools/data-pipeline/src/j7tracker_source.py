#!/usr/bin/env python3
"""j7tracker.io source adapter for narrative / contract-address resolution.

ROUTE MAP, recovered from the SPA bundle (static/js/main.f144aea5.js) -- the
endpoint surface is real, as suspected, and these are the base constants the
bundle assigns:

    Re/ze = https://nyc.<host>            -> https://nyc.j7tracker.io   (main API)
    Xe    = https://core.j7tracker.io     (session-check / prefs / commands)
    We    = https://util.j7tracker.io     (chat, ai, twitter, translate)
    He    = https://nj.j7tracker.io       (wallets + socket.io)
    Ue    = https://nj.j7tracker.io/wallets
    Et[]  = https://tx-{fra,nyc,lax,sgp}.j7tracker.io/deploy  (regional trade edge)
    bs    = https://edit-image-proxy.j7tracker.io

Routes that matter for us:

    GET  /api/pump/profile/<USERNAME>       (a pump.fun USERNAME, not a CA)
    GET  /utils/pump/users/search?term=<t>  (param is `term`, 1-64 chars)
    GET  /utils/scan/profile/<ADDRESS>      (address profile scan)
    GET  /api/coin-config/<MINT>
    util /twitter/{status,lookup/tweet/<id>,lookup/community/<id>}
    util /ai/ai-image

AUTH REALITY (measured 2026-09-26, live):
    /utils/scan/profile/<addr>   -> 401 {"error":"unauthorized"}
    util/twitter/status          -> 401 {"error":"No authentication token provided"}
    /api/feed/no-auto-add        -> 401
    core/api/session-check       -> 401
    /utils/pump/users/search     -> 502 (origin faults upstream without a session)
    /api/pump/profile/<X>        -> 404 on GET for real usernames

So the useful data plane is session-authenticated. This adapter therefore:
  * hits PUBLIC routes only, and
  * treats 401 as a COVERAGE GAP ("unavailable is not zero signal"), and
  * NEVER forges, guesses, reuses or extracts a session. A legitimate operator
    session may be supplied via J7_SESSION_TOKEN; the header name is NOT
    confirmed from the bundle (x-session-id is used by /api/wallet-tracker/*,
    Authorization by the deploy-stats route), so it stays configurable and is
    only trusted once confirmed against a real session.
"""
import json
import os
import urllib.error
import urllib.request

UA = ("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) "
      "Chrome/126 Safari/537.36")

BASES = {
    "api": "https://nyc.j7tracker.io",
    "core": "https://core.j7tracker.io",
    "util": "https://util.j7tracker.io",
    "wallets": "https://nj.j7tracker.io",
}

PUBLIC_PROBES = [
    ("/api/pump/profile/ansem", "api"),
    ("/utils/pump/users/search?term=ansem", "api"),
    ("/utils/scan/profile/8jayQDfRc3GYxftBJ2VAwjqmRBdYPC9KnjY15rc9pump", "api"),
    ("/twitter/status", "util"),
    ("/api/session-check", "core"),
]


def fetch(path, base="api", token=None, header="x-session-id", timeout=20):
    """GET a j7tracker route. Returns (status, body_bytes)."""
    url = BASES[base] + path
    h = {"User-Agent": UA, "Accept": "application/json, text/plain, */*",
         "Origin": "https://j7tracker.io", "Referer": "https://j7tracker.io/"}
    if token:
        h[header] = token
    req = urllib.request.Request(url, headers=h)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read()[:2000]
    except urllib.error.HTTPError as e:
        return e.code, e.read()[:400]
    except Exception as e:
        return None, str(e)[:200].encode()


def probe(token=None):
    """Probe the public surface. Returns (reachable, auth_required, detail)."""
    reachable = False
    auth = []
    detail = []
    for path, base in PUBLIC_PROBES:
        st, body = fetch(path, base, token=token)
        if st is not None:
            reachable = True
        snippet = body[:90].decode("utf-8", "replace").replace("\n", " ")
        detail.append(f"{st} {BASES[base]}{path} :: {snippet}")
        if st == 401:
            auth.append(path)
    return reachable, auth, detail


if __name__ == "__main__":
    tok = os.environ.get("J7_SESSION_TOKEN")
    reachable, auth, detail = probe(token=tok)
    print(f"reachable={reachable}  auth_required_routes={len(auth)}  token_supplied={bool(tok)}")
    for d in detail:
        print("  ", d)
    print()
    if not tok:
        print("COVERAGE GAP: no J7_SESSION_TOKEN -> data plane unusable. "
              "Not bypassing; 401 is recorded as a gap, never as zero signal.")
