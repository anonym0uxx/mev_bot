#!/usr/bin/env python3
"""Extract on-chain account-layout fixtures for the transaction builder.

WHY THIS EXISTS
---------------
On 2026-08-02 a live check falsified the shipped bonding-curve account lists:
the builder produced 17 accounts for `buy`, the chain carries 18. The extra
account is writable and owned by the fee program. The builder was derived from
`VENUE_TX_LAYOUTS.md` and corroborated against the official IDL, and *both
agreed with the wrong answer* -- because an IDL's named account list stops
before the trailing `remaining_accounts`, so a program can add a required
trailing account without the IDL changing.

The only authority that cannot be wrong about the current layout is a real
successful transaction. This script turns that authority into a machine-checked
fixture: it paginates real transactions, extracts each swap instruction's
account list with signer/writable flags in order, clusters the distinct layouts
it finds, and emits JSON that `pump_quant_protocol::layout::diff_layout`
consumes as the parity gate (criterion 77a).

THE MEASUREMENT TRAP THIS SCRIPT AVOIDS
---------------------------------------
There are two different "account lists" in a transaction and confusing them
produces a false finding in either direction:

  * the MESSAGE key list  -- every account the transaction touches, including
    accounts belonging to *other* instructions (compute budget, ATA creation,
    tip transfer) and, for a v0 transaction, everything resolved from address
    lookup tables.
  * the INSTRUCTION account list -- the ordered accounts passed to ONE
    instruction. This is what a builder must reproduce.

An ALT does not change how many accounts an instruction takes. It changes how
the message *stores* them. So "the extras are ALT-resolved accounts" is not by
itself evidence that a builder omits anything -- it may be evidence that the
message list was measured instead of the instruction list.

This script always reports the INSTRUCTION list, and separately reports whether
those accounts arrived statically or via ALT, so the two can never be conflated.

USAGE
-----
    export PQ_RPC_URL="https://mainnet.helius-rpc.com/?api-key=..."
    python3 scripts/extract_layout_fixtures.py --program pump --limit 200
    python3 scripts/extract_layout_fixtures.py --wallet <ADDR> --limit 1000
    python3 scripts/extract_layout_fixtures.py --program both --limit 400 \
        --out docs/fixtures/layouts.json

Never pass the RPC URL on the command line -- it carries the API key and the
shell history is a credential store nobody audits. Use the environment.

No third-party dependencies: stdlib only, so it runs on the server with no
install step. Research/extraction tooling only -- the production path is the
Rust parity test that consumes this output (no Python in production paths).
"""

import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.request
from collections import defaultdict

# --------------------------------------------------------------------------
# Venue constants. Cited, not remembered.
# --------------------------------------------------------------------------
PUMP_PROGRAM = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"
PUMPSWAP_PROGRAM = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"
FEE_PROGRAM = "pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ"
TOKEN_PROGRAM = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
TOKEN_2022_PROGRAM = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
WSOL_MINT = "So11111111111111111111111111111111111111112"
FEE_PROGRAM_GLOBAL = "CHqnuTkj6sXDFknM652aEFPECZh9qVsBXWkhPohmV9dA"

# sha256("global:<name>")[..8]. buy and sell are byte-identical ACROSS the two
# programs (Anchor namespaces by instruction name, not by program), so venue is
# always decided by program_id and never by discriminator.
DISC_BUY = bytes([102, 6, 61, 18, 1, 218, 235, 234]).hex()
DISC_SELL = bytes([51, 230, 133, 164, 1, 127, 131, 173]).hex()

B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def b58decode(s):
    n = 0
    for c in s:
        n = n * 58 + B58.index(c)
    b = n.to_bytes((n.bit_length() + 7) // 8, "big")
    return b"\0" * (len(s) - len(s.lstrip("1"))) + b


# --------------------------------------------------------------------------
# RPC
# --------------------------------------------------------------------------
class Rpc:
    """Minimal JSON-RPC client with backoff. Never logs the URL (API key)."""

    def __init__(self, url, sleep=0.12):
        self.url = url
        self.sleep = sleep
        self.calls = 0

    def call(self, method, params, retries=5):
        body = json.dumps(
            {"jsonrpc": "2.0", "id": 1, "method": method, "params": params}
        ).encode()
        backoff = 0.5
        for attempt in range(retries):
            try:
                req = urllib.request.Request(
                    self.url, data=body, headers={"Content-Type": "application/json"}
                )
                with urllib.request.urlopen(req, timeout=45) as r:
                    self.calls += 1
                    time.sleep(self.sleep)
                    out = json.loads(r.read())
                if "error" in out:
                    # -32429 / rate limit style errors are worth retrying.
                    msg = str(out["error"])
                    if "rate" in msg.lower() or "429" in msg:
                        time.sleep(backoff)
                        backoff *= 2
                        continue
                    raise RuntimeError("rpc error: %s" % msg)
                return out.get("result")
            except urllib.error.HTTPError as e:
                if e.code in (429, 500, 502, 503, 504) and attempt < retries - 1:
                    time.sleep(backoff)
                    backoff *= 2
                    continue
                raise
            except (urllib.error.URLError, TimeoutError):
                if attempt < retries - 1:
                    time.sleep(backoff)
                    backoff *= 2
                    continue
                raise
        raise RuntimeError("rpc gave up after %d retries: %s" % (retries, method))


def paginate_signatures(rpc, address, limit, only_successful=True):
    """Page through getSignaturesForAddress until `limit` or exhaustion.

    Pagination is by `before`, which is the ONLY correct cursor -- an offset
    would silently skip or duplicate rows as new transactions land.
    """
    out, before = [], None
    while len(out) < limit:
        page = min(1000, limit - len(out))
        params = [address, {"limit": page}]
        if before:
            params[1]["before"] = before
        res = rpc.call("getSignaturesForAddress", params)
        if not res:
            break
        for row in res:
            if only_successful and row.get("err") is not None:
                continue
            out.append(row)
        before = res[-1]["signature"]
        if len(res) < page:
            break
        sys.stderr.write("  ...%d signatures\n" % len(out))
    return out[:limit]


# --------------------------------------------------------------------------
# Account-list extraction -- the part that must be exactly right
# --------------------------------------------------------------------------
def resolve_account_keys(tx):
    """Full ordered key list plus per-index (is_signer, is_writable, source).

    SVM ordering for a v0 transaction is:
        static keys, then ALT-writable, then ALT-readonly.

    Flags follow the message header, and the rule differs by region -- getting
    this wrong is the single easiest way to produce a false FlagMismatch:
      * signers occupy [0, numRequiredSignatures)
      * within signers, the LAST numReadonlySignedAccounts are read-only
      * within static non-signers, the LAST numReadonlyUnsignedAccounts are
        read-only
      * ALT-loaded accounts are never signers; writability is which bucket the
        RPC returned them in
    """
    msg = tx["transaction"]["message"]
    header = msg["header"]
    n_sig = header["numRequiredSignatures"]
    n_ro_signed = header["numReadonlySignedAccounts"]
    n_ro_unsigned = header["numReadonlyUnsignedAccounts"]

    static = msg["accountKeys"]
    # Some RPCs return accountKeys as objects when jsonParsed is used; we ask
    # for "json" encoding so they are plain strings. Guard anyway.
    static = [k["pubkey"] if isinstance(k, dict) else k for k in static]

    loaded = (tx.get("meta") or {}).get("loadedAddresses") or {}
    alt_w = loaded.get("writable") or []
    alt_r = loaded.get("readonly") or []

    keys = list(static) + list(alt_w) + list(alt_r)
    n_static = len(static)
    flags = []
    for i in range(len(keys)):
        if i < n_sig:
            signer = True
            writable = i < (n_sig - n_ro_signed)
            source = "static"
        elif i < n_static:
            signer = False
            writable = i < (n_static - n_ro_unsigned)
            source = "static"
        elif i < n_static + len(alt_w):
            signer, writable, source = False, True, "alt"
        else:
            signer, writable, source = False, False, "alt"
        flags.append((signer, writable, source))
    return keys, flags


def iter_instructions(tx):
    """Yield (program_id, data_b58, account_indices, kind) for every instruction.

    Includes INNER instructions: a swap routed through an aggregator appears as
    a CPI, and its account list is just as authoritative as a top-level one.
    Excluding inner instructions would systematically miss every routed trade.
    """
    msg = tx["transaction"]["message"]
    keys, _ = resolve_account_keys(tx)
    for ix in msg.get("instructions", []):
        pid = keys[ix["programIdIndex"]]
        yield pid, ix.get("data", ""), ix.get("accounts", []), "top"
    for grp in (tx.get("meta") or {}).get("innerInstructions", []) or []:
        for ix in grp.get("instructions", []):
            if "programIdIndex" not in ix:
                continue
            pid = keys[ix["programIdIndex"]]
            yield pid, ix.get("data", ""), ix.get("accounts", []), "inner"


def classify(pid, data_b58):
    """Return (venue, side) or None. Venue by program_id, never by discriminator."""
    if pid == PUMP_PROGRAM:
        venue = "pumpfun"
    elif pid == PUMPSWAP_PROGRAM:
        venue = "pumpswap"
    else:
        return None
    try:
        raw = b58decode(data_b58)
    except (ValueError, IndexError):
        return None
    if len(raw) < 8:
        return None
    disc = raw[:8].hex()
    if disc == DISC_BUY:
        return venue, "buy", raw
    if disc == DISC_SELL:
        return venue, "sell", raw
    return None


def extract(tx, sig, slot):
    """Every pump/pumpswap buy/sell instruction in one transaction."""
    keys, flags = resolve_account_keys(tx)
    found = []
    for pid, data, idxs, kind in iter_instructions(tx):
        c = classify(pid, data)
        if not c:
            continue
        venue, side, raw = c
        accounts = []
        ok = True
        for i in idxs:
            if i >= len(keys):
                ok = False
                break
            s, w, src = flags[i]
            accounts.append(
                {"pubkey": keys[i], "is_signer": s, "is_writable": w, "source": src}
            )
        if not ok:
            continue
        found.append(
            {
                "signature": sig,
                "slot": slot,
                "venue": venue,
                "side": side,
                "ix_kind": kind,
                "data_len": len(raw),
                "data_hex": raw.hex(),
                "account_count": len(accounts),
                "alt_count": sum(1 for a in accounts if a["source"] == "alt"),
                "accounts": accounts,
            }
        )
    return found


# --------------------------------------------------------------------------
# Variant classification -- the permutation axes
# --------------------------------------------------------------------------
def fetch_account_b64(rpc, pubkey):
    res = rpc.call("getAccountInfo", [pubkey, {"encoding": "base64"}])
    if not res or not res.get("value"):
        return None, None
    import base64

    val = res["value"]
    return base64.b64decode(val["data"][0]), val.get("owner")


def classify_variant(rpc, rec, cache):
    """Decode the market state that changes the account list.

    Cashback is byte 82 of the bonding curve and byte 244 of the pool. It is
    NEVER inferred from the token program -- many Token-2022 mints have
    cashback disabled, and that inference is exactly the mistake the pump docs
    call out.
    """
    v = {
        "cashback": None,
        "token_2022": None,
        "non_sol_quote": None,
        "reversed_pool": None,
    }
    try:
        if rec["venue"] == "pumpfun":
            # [2] mint, [3] bonding_curve in the section 4.1 order.
            if len(rec["accounts"]) < 4:
                return v
            mint = rec["accounts"][2]["pubkey"]
            curve = rec["accounts"][3]["pubkey"]
            if mint not in cache:
                _, owner = fetch_account_b64(rpc, mint)
                cache[mint] = owner
            v["token_2022"] = cache[mint] == TOKEN_2022_PROGRAM
            if curve not in cache:
                data, _ = fetch_account_b64(rpc, curve)
                cache[curve] = data
            data = cache[curve]
            if data and len(data) > 82:
                v["cashback"] = data[82] == 1
            if data and len(data) >= 115:
                qm = data[83:115]
                v["non_sol_quote"] = qm != b58decode(WSOL_MINT)
        else:
            # [3] base_mint, [4] quote_mint in the section 4.2 order.
            if len(rec["accounts"]) < 5:
                return v
            base_mint = rec["accounts"][3]["pubkey"]
            quote_mint = rec["accounts"][4]["pubkey"]
            v["non_sol_quote"] = quote_mint != WSOL_MINT
            # A reversed pool is one where the TRADED token is the quote side.
            v["reversed_pool"] = base_mint == WSOL_MINT
            if quote_mint not in cache:
                _, owner = fetch_account_b64(rpc, quote_mint)
                cache[quote_mint] = owner
            v["token_2022"] = cache[quote_mint] == TOKEN_2022_PROGRAM
    except Exception as e:  # noqa: BLE001 - classification is best-effort
        sys.stderr.write("  variant classify failed for %s: %s\n" % (rec["signature"], e))
    return v


# --------------------------------------------------------------------------
# Reporting
# --------------------------------------------------------------------------
def layout_signature(rec):
    """A layout's identity: venue, side, count, and the full flag pattern.

    The flag pattern is part of the identity because two layouts with the same
    account count but different writability are different layouts, and only one
    of them lands.
    """
    flags = "".join(
        ("S" if a["is_signer"] else "-") + ("W" if a["is_writable"] else "-")
        for a in rec["accounts"]
    )
    return (rec["venue"], rec["side"], rec["account_count"], flags)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--program", choices=["pump", "pumpswap", "both"], default=None)
    ap.add_argument("--wallet", default=None, help="also scan this wallet's history")
    ap.add_argument("--limit", type=int, default=200)
    ap.add_argument("--out", default="docs/fixtures/layouts.json")
    ap.add_argument("--no-variant", action="store_true",
                    help="skip variant classification (fewer RPC calls)")
    args = ap.parse_args()

    url = os.environ.get("PQ_RPC_URL")
    if not url:
        sys.stderr.write(
            "PQ_RPC_URL is not set. Export it; do not pass the key on the "
            "command line.\n"
        )
        return 2
    if not args.program and not args.wallet:
        sys.stderr.write("need --program and/or --wallet\n")
        return 2

    rpc = Rpc(url)
    targets = []
    if args.program in ("pump", "both"):
        targets.append(("pump program", PUMP_PROGRAM))
    if args.program in ("pumpswap", "both"):
        targets.append(("pumpswap program", PUMPSWAP_PROGRAM))
    if args.wallet:
        targets.append(("wallet", args.wallet))

    records, seen_sigs = [], set()
    for label, addr in targets:
        sys.stderr.write("Paginating %s (%s)...\n" % (label, addr))
        sigs = paginate_signatures(rpc, addr, args.limit)
        sys.stderr.write("  %d successful signatures\n" % len(sigs))
        for i, row in enumerate(sigs):
            sig = row["signature"]
            if sig in seen_sigs:
                continue
            seen_sigs.add(sig)
            tx = rpc.call(
                "getTransaction",
                [sig, {"encoding": "json", "maxSupportedTransactionVersion": 0}],
            )
            if not tx or (tx.get("meta") or {}).get("err") is not None:
                continue
            records.extend(extract(tx, sig, tx.get("slot")))
            if (i + 1) % 25 == 0:
                sys.stderr.write("  ...%d/%d txs, %d instructions\n"
                                 % (i + 1, len(sigs), len(records)))

    if not records:
        sys.stderr.write("\nNo pump/pumpswap buy or sell instructions found.\n")
        return 1

    cache = {}
    if not args.no_variant:
        sys.stderr.write("Classifying variants...\n")
        for r in records:
            r["variant"] = classify_variant(rpc, r, cache)

    # Cluster
    clusters = defaultdict(list)
    for r in records:
        clusters[layout_signature(r)].append(r)

    print("=" * 78)
    print(" DISTINCT LAYOUTS OBSERVED  (%d instructions, %d transactions)"
          % (len(records), len(seen_sigs)))
    print("=" * 78)
    for key in sorted(clusters, key=lambda k: (k[0], k[1], k[2])):
        venue, side, count, flags = key
        group = list(clusters[key])
        print("\n%s %s  ->  %d accounts   (%d samples, %.0f%% of %s %s)"
              % (venue, side.upper(), count, len(group),
                 100.0 * len(group) / sum(len(v) for k2, v in clusters.items()
                                          if k2[0] == venue and k2[1] == side),
                 venue, side))
        ex = group[0]
        print("   example : %s  slot %s" % (ex["signature"], ex["slot"]))
        print("   ix data : %d bytes  %s" % (ex["data_len"], ex["data_hex"][:32]))
        print("   from ALT: %d of %d accounts" % (ex["alt_count"], ex["account_count"]))
        if "variant" in ex:
            print("   variant : %s" % json.dumps(ex["variant"]))
        for i, a in enumerate(ex["accounts"]):
            tags = []
            if a["is_signer"]:
                tags.append("SIGNER")
            if a["is_writable"]:
                tags.append("WRITABLE")
            if a["source"] == "alt":
                tags.append("via-ALT")
            note = ""
            if a["pubkey"] == FEE_PROGRAM_GLOBAL:
                note = "   <== fee-program-global (CONSTANT across mints)"
            print("      [%2d] %-44s %s%s" % (i, a["pubkey"], ",".join(tags), note))

    # The discriminating test for the 2026-08-02 finding.
    print("\n" + "=" * 78)
    print(" DISCRIMINATING TEST: is the trailing fee-program account per-mint?")
    print("=" * 78)
    for venue in ("pumpfun", "pumpswap"):
        for side in ("buy", "sell"):
            grp = [r for r in records if r["venue"] == venue and r["side"] == side]
            if len(grp) < 2:
                continue
            tails = {}
            for r in grp:
                if r["accounts"]:
                    mint_idx = 2 if venue == "pumpfun" else 3
                    if len(r["accounts"]) > mint_idx:
                        tails.setdefault(r["accounts"][-1]["pubkey"], set()).add(
                            r["accounts"][mint_idx]["pubkey"]
                        )
            if not tails:
                continue
            print("\n  %s %s -- distinct trailing accounts: %d" % (venue, side, len(tails)))
            for tail, mints in list(tails.items())[:6]:
                print("     %s   seen with %d distinct mint(s)" % (tail, len(mints)))
            if len(tails) == 1 and sum(len(m) for m in tails.values()) > 1:
                print("     VERDICT: CONSTANT across mints -> fee-program-global shape")
            elif len(tails) > 1:
                print("     VERDICT: VARIES with mint -> per-mint PDA (sharing-config shape)")
            else:
                print("     VERDICT: inconclusive, need samples on >1 mint")

    # Coverage matrix
    if not args.no_variant:
        print("\n" + "=" * 78)
        print(" PERMUTATION COVERAGE")
        print("=" * 78)
        cov = defaultdict(int)
        for r in records:
            v = r.get("variant", {})
            cov[(r["venue"], r["side"], v.get("cashback"), v.get("token_2022"),
                 v.get("non_sol_quote"), v.get("reversed_pool"))] += 1
        print("  %-10s %-5s %-9s %-10s %-9s %-9s %s"
              % ("venue", "side", "cashback", "token2022", "nonSOL", "reversed", "n"))
        for k in sorted(cov, key=lambda x: (x[0], x[1], str(x[2:]))):
            print("  %-10s %-5s %-9s %-10s %-9s %-9s %d"
                  % (k[0], k[1], k[2], k[3], k[4], k[5], cov[k]))
        print("\n  Any permutation NOT listed above is UNVERIFIED and, under")
        print("  LayoutRegistry, UNBUILDABLE. Absence here is the work list.")

    os.makedirs(os.path.dirname(args.out) or ".", exist_ok=True)
    payload = {
        "generated_from_records": len(records),
        "transactions": len(seen_sigs),
        "rpc_calls": rpc.calls,
        "layouts": [
            {
                "venue": k[0],
                "side": k[1],
                "account_count": k[2],
                "flag_pattern": k[3],
                "samples": len(v),
                "example_signature": v[0]["signature"],
                "example_slot": v[0]["slot"],
                "variant": v[0].get("variant"),
                "accounts": v[0]["accounts"],
            }
            for k, v in sorted(clusters.items(), key=lambda kv: (kv[0][0], kv[0][1], kv[0][2]))
        ],
    }
    with open(args.out, "w") as f:
        json.dump(payload, f, indent=2)
    print("\nWrote %s  (%d distinct layouts, %d rpc calls)"
          % (args.out, len(clusters), rpc.calls))
    print("Feed it to the Rust parity gate: layout::diff_layout + "
          "LayoutRegistry::record_verified.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
