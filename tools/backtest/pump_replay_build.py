#!/usr/bin/env python3
"""
pump_replay_build.py — turn REAL decoded pump.fun swaps into a `pump-quant-app`
replay events file.

This is the missing half of the backtest. The engine has always been able to replay
an events file (`pump-quant-app replay <config> <events>`); what did not exist was a
converter from real on-chain swap data into that format. Every measurement the
project has made to date came from SYNTHETIC tapes, which is exactly the gap this
closes.

# Input: the canonical pump.fun `TradeEvent`, one JSON object per line

Every free and paid source (Helius enhanced/decoded, PumpPortal WS, PumpAPI replay,
a Dune `pumpdotfun.trades` export, or your own decoder over solarchive/Old Faithful)
can be shaped into this. Field names are matched case-insensitively and accept both
camelCase and snake_case:

    mint                  base58 mint address (pump.fun mints conventionally end "pump")
    user                  base58 signer of the swap
    isBuy                 bool — true = buy (base in), false = sell (base out)
    solAmount             u64  — lamports of SOL on this swap
    tokenAmount           u64  — base units of the token on this swap
    virtualSolReserves    u64  — lamports; the curve's SOL side AFTER the swap
    virtualTokenReserves  u64  — base units; the curve's token side AFTER the swap
    slot                  u64  — Solana slot
    timestamp             i64  — unix seconds (optional; only used for ordering ties)

Nothing is invented. If a record is missing a field the converter needs, the record
is DROPPED and counted — never defaulted, never interpolated (§6: missing data is
UNKNOWN, not a guess). The drop ledger is printed so coverage is auditable.

# Output: the `trade` / `confirm` / `tokenmeta` grammar `main.rs::parse_events` reads

    tokenmeta <mint_hex> <category_id> <taxonomy_version> <creator> <slot>
    confirm   <mint_hex> <sellable_depth_lamports>
    trade     <mint_hex> <price_fp> <quote_lamports> <liquidity_lamports> \
              <signed_base> <buyer_entity> <age_slots>
    tick

# Why the market-cap filter matters

The strategy is calibrated for LOW-cap pump.fun markets. Replaying graduated
large-caps would measure a different game. `--min-mcap-sol` / `--max-mcap-sol` gate
on the curve-implied market cap at first sighting, using pump.fun's fixed 1e9 total
supply. Defaults bracket the pre/just-post graduation band.

Usage:
    python3 pump_replay_build.py --in swaps.jsonl --out events.txt \
        [--min-mcap-sol 5] [--max-mcap-sol 600] [--tick-every 25] [--max-mints 512]
"""

import argparse
import json
import sys
from collections import defaultdict

# Must match `pump_quant_app` / the tape modules.
PRICE_SCALE = 10_000_000
# pump.fun mints are minted with a fixed 1e9 supply at 6 decimals.
TOTAL_SUPPLY_TOKENS = 1_000_000_000
TOKEN_DECIMALS = 6
LAMPORTS_PER_SOL = 1_000_000_000

B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def b58_decode(s):
    """Minimal base58 -> bytes. Returns None on any invalid character."""
    n = 0
    for ch in s:
        idx = B58.find(ch)
        if idx < 0:
            return None
        n = n * 58 + idx
    raw = n.to_bytes((n.bit_length() + 7) // 8, "big") if n else b""
    pad = len(s) - len(s.lstrip("1"))
    return b"\x00" * pad + raw


def mint_hex(addr):
    """32-byte mint as hex, which is what `DomainMint::from_hex` expects."""
    b = b58_decode(addr)
    if b is None:
        return None
    if len(b) < 32:
        b = b"\x00" * (32 - len(b)) + b
    return b[:32].hex()


def fnv1a_64(data):
    """Stable entity id, same hash family the engine uses elsewhere.

    Masked to 63 bits: the replay grammar parses `buyer_entity` and `creator` as
    i64, so a full 64-bit hash would overflow the parser on ~half of all inputs.
    Collision probability at 63 bits is irrelevant at our entity counts.
    """
    h = 0xCBF29CE484222325
    for byte in data:
        h ^= byte
        h = (h * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return h & 0x7FFF_FFFF_FFFF_FFFF


def get(rec, *names):
    """Case/style-insensitive field lookup."""
    for n in names:
        for k in (n, n.lower(), n.upper()):
            if k in rec:
                return rec[k]
    low = {k.lower().replace("_", ""): v for k, v in rec.items()}
    for n in names:
        v = low.get(n.lower().replace("_", ""))
        if v is not None:
            return v
    return None


def to_int(v):
    if v is None:
        return None
    if isinstance(v, bool):
        return None
    if isinstance(v, (int,)):
        return v
    if isinstance(v, float):
        return int(v)
    if isinstance(v, str):
        try:
            return int(v)
        except ValueError:
            try:
                return int(float(v))
            except ValueError:
                return None
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--in", dest="inp", required=True, help="decoded swaps JSONL")
    ap.add_argument("--out", dest="out", required=True, help="events file to write")
    ap.add_argument("--min-mcap-sol", type=float, default=5.0)
    ap.add_argument("--max-mcap-sol", type=float, default=600.0)
    ap.add_argument("--tick-every", type=int, default=25,
                    help="emit a `tick` every N trades (drives the time-based backstops)")
    ap.add_argument("--max-mints", type=int, default=512)
    ap.add_argument("--universe-manifest", default=None,
                    help="newline-delimited list of EVERY mint created in the window "
                         "(the launch-time universe). Required unless "
                         "--unaudited-survivorship is passed.")
    ap.add_argument("--unaudited-survivorship", action="store_true",
                    help="proceed with NO survivorship audit. The output is stamped "
                         "UNAUDITED and any net from it is not admissible evidence.")
    args = ap.parse_args()

    if not args.universe_manifest and not args.unaudited_survivorship:
        print(
            "REFUSED: no --universe-manifest.\n"
            "  A corpus assembled from tokens that still have data is conditioned on\n"
            "  those tokens having MATTERED. The tokens that died in minutes -- the\n"
            "  majority, and this strategy's modal case -- are silently missing, and a\n"
            "  backtest over the survivors will look profitable when the strategy is not.\n"
            "  This is the single most likely way this measurement lies to you.\n\n"
            "  Supply the launch-time universe: every mint created in the window,\n"
            "  enumerated from the pump.fun program's `create` instructions by slot\n"
            "  range -- NOT a list of pairs that exist today.\n\n"
            "  To proceed anyway, pass --unaudited-survivorship. The events file will be\n"
            "  stamped UNAUDITED and its net is not admissible as evidence.",
            file=sys.stderr)
        return 3

    drops = defaultdict(int)
    per_mint = defaultdict(list)
    kept_records = 0

    with open(args.inp, "r", encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                rec = json.loads(line)
            except json.JSONDecodeError:
                drops["unparseable_json"] += 1
                continue

            mint = get(rec, "mint", "mintAddress", "token")
            user = get(rec, "user", "trader", "owner", "signer")
            is_buy = get(rec, "isBuy", "is_buy", "side")
            sol = to_int(get(rec, "solAmount", "sol_amount", "solLamports"))
            tok = to_int(get(rec, "tokenAmount", "token_amount"))
            vsol = to_int(get(rec, "virtualSolReserves", "virtual_sol_reserves"))
            vtok = to_int(get(rec, "virtualTokenReserves", "virtual_token_reserves"))
            slot = to_int(get(rec, "slot", "blockSlot", "block_slot"))

            if mint is None or user is None:
                drops["missing_mint_or_user"] += 1
                continue
            if is_buy is None:
                drops["missing_side"] += 1
                continue
            if isinstance(is_buy, str):
                is_buy = is_buy.strip().lower() in ("true", "buy", "1", "b")
            if sol is None or tok is None:
                drops["missing_amounts"] += 1
                continue
            if vsol is None or vtok is None or vtok == 0:
                # The reserves ARE the price and the depth. Without them we would be
                # guessing both — refuse (§6).
                drops["missing_reserves"] += 1
                continue
            if slot is None:
                drops["missing_slot"] += 1
                continue

            mh = mint_hex(mint)
            uh = b58_decode(user)
            if mh is None or uh is None:
                drops["bad_base58"] += 1
                continue

            # Price: SOL-per-token from the curve reserves, in PRICE_SCALE units.
            # Integer throughout (§22) — reserves are already integers.
            price_fp = (vsol * PRICE_SCALE) // vtok
            if price_fp <= 0:
                drops["nonpositive_price"] += 1
                continue

            per_mint[mint].append({
                "slot": slot,
                "mh": mh,
                "price_fp": price_fp,
                "quote": abs(sol),
                "liq": vsol,
                "signed_base": abs(tok) if is_buy else -abs(tok),
                "entity": fnv1a_64(uh),
                "vsol": vsol,
                "vtok": vtok,
                "user": user,
            })
            kept_records += 1

    if not per_mint:
        print("FATAL: no usable records. Drop ledger:", dict(drops), file=sys.stderr)
        return 2

    # ---- market-cap gate, evaluated at FIRST sighting (no look-ahead) ----
    selected = []
    for mint, rows in per_mint.items():
        rows.sort(key=lambda r: r["slot"])
        first = rows[0]
        # price_fp is SOL-per-token-base-unit scaled; convert to whole-token SOL.
        price_sol_per_token = (first["price_fp"] / PRICE_SCALE) * (10 ** TOKEN_DECIMALS)
        mcap_sol = price_sol_per_token * TOTAL_SUPPLY_TOKENS / LAMPORTS_PER_SOL
        if not (args.min_mcap_sol <= mcap_sol <= args.max_mcap_sol):
            drops["outside_mcap_band"] += 1
            continue
        selected.append((mint, rows, mcap_sol))

    selected.sort(key=lambda x: x[1][0]["slot"])
    n_in_band = len(selected)
    if args.max_mints > 0 and len(selected) > args.max_mints:
        # DETERMINISTIC, UNBIASED subsample. Taking the first N after a slot sort --
        # which this script originally did -- silently truncates to the EARLIEST
        # mints, i.e. one market regime, and that is a selection bias in its own
        # right. Hash-ordering by mint is reproducible, independent of launch time,
        # and independent of outcome.
        selected.sort(key=lambda x: fnv1a_64(x[0].encode()))
        selected = selected[: args.max_mints]
        selected.sort(key=lambda x: x[1][0]["slot"])
        sampling = f"hash-subsampled {args.max_mints} of {n_in_band} in-band mints (unbiased)"
    else:
        sampling = f"all {n_in_band} in-band mints"

    # ---- SURVIVORSHIP AUDIT ----
    universe_n = None
    covered = None
    if args.universe_manifest:
        with open(args.universe_manifest, "r", encoding="utf-8") as fh:
            universe = {ln.strip() for ln in fh if ln.strip()}
        universe_n = len(universe)
        seen = set(per_mint.keys())
        covered = len(seen & universe)

    # A corpus that was pre-filtered to "active" tokens shows up as an implausibly
    # high MINIMUM trade count -- real launch universes are dominated by mints with a
    # handful of trades and nothing after.
    tpm = sorted(len(rows) for _, rows, _ in selected)
    min_tpm = tpm[0] if tpm else 0
    prefilter_smell = min_tpm >= 25

    if not selected:
        print("FATAL: every mint fell outside the market-cap band. Widen "
              "--min-mcap-sol/--max-mcap-sol. Ledger:", dict(drops), file=sys.stderr)
        return 2

    # ---- emit, strictly in slot order across all mints ----
    stream = []
    for mint, rows, _ in selected:
        creation_slot = rows[0]["slot"]
        creator = fnv1a_64(b58_decode(rows[0]["user"]))
        stream.append((creation_slot, 0, f"tokenmeta {rows[0]['mh']} 0 1 {creator} {creation_slot}"))
        # Confirmed sellable depth = the curve's SOL side at first sighting. This is
        # a MEASURED reserve, not an assumption.
        stream.append((creation_slot, 1, f"confirm {rows[0]['mh']} {rows[0]['liq']}"))
        for r in rows:
            age = max(0, r["slot"] - creation_slot)
            stream.append((r["slot"], 2,
                           f"trade {r['mh']} {r['price_fp']} {r['quote']} {r['liq']} "
                           f"{r['signed_base']} {r['entity']} {age}"))

    stream.sort(key=lambda x: (x[0], x[1]))

    n_trades = 0
    with open(args.out, "w", encoding="utf-8") as out:
        out.write(f"# generated by pump_replay_build.py from {args.inp}\n")
        out.write(f"# mints={len(selected)} trades={kept_records} "
                  f"mcap_band_sol=[{args.min_mcap_sol},{args.max_mcap_sol}]\n")
        out.write(f"# sampling={sampling}\n")
        if universe_n is not None:
            pct = 100.0 * covered / universe_n if universe_n else 0.0
            out.write(f"# SURVIVORSHIP: corpus covers {covered}/{universe_n} "
                      f"launched mints ({pct:.2f}%)\n")
            if pct < 50.0:
                out.write("# SURVIVORSHIP WARNING: under half the launch universe is "
                          "present; the missing mints are disproportionately the ones "
                          "that died, so any net from this run is BIASED UPWARD\n")
        else:
            out.write("# SURVIVORSHIP: *** UNAUDITED *** no launch universe supplied; "
                      "net from this run is NOT admissible evidence\n")
        if prefilter_smell:
            out.write(f"# SURVIVORSHIP WARNING: minimum trades-per-mint is {min_tpm}; a "
                      "real launch universe is dominated by mints with only a handful "
                      "of trades, so this input looks PRE-FILTERED to active tokens\n")
        for i, (_, _, line) in enumerate(stream):
            out.write(line + "\n")
            if line.startswith("trade "):
                n_trades += 1
                if args.tick_every > 0 and n_trades % args.tick_every == 0:
                    out.write("tick\n")
        # Let every open position see a final clock so time stops can resolve.
        for _ in range(8):
            out.write("tick\n")

    caps = [m for _, _, m in selected]
    print(f"wrote {args.out}")
    print(f"  mints selected     : {len(selected)}")
    print(f"  trades emitted     : {n_trades}")
    print(f"  mcap band (SOL)    : {min(caps):.2f} .. {max(caps):.2f}")
    print(f"  records kept       : {kept_records}")
    print(f"  sampling           : {sampling}")
    print(f"  trades/mint min/med: {min_tpm} / {tpm[len(tpm)//2] if tpm else 0}")
    print("  --- SURVIVORSHIP AUDIT ---")
    if universe_n is not None:
        pct = 100.0 * covered / universe_n if universe_n else 0.0
        print(f"  launch universe    : {universe_n} mints created in window")
        print(f"  corpus coverage    : {covered} ({pct:.2f}%)")
        if pct < 50.0:
            print("  *** WARNING: under half the launch universe is present. The absent "
                  "mints are disproportionately the ones that DIED -- net is biased UPWARD.")
    else:
        print("  *** UNAUDITED -- no launch universe supplied. Net is NOT admissible evidence.")
    if prefilter_smell:
        print(f"  *** WARNING: min trades/mint = {min_tpm}. Input looks PRE-FILTERED to "
              "active tokens, which is survivorship bias entering upstream of this tool.")
    print(f"  DROP LEDGER        : {dict(drops) if drops else 'none'}")
    if drops:
        total = kept_records + sum(drops.values())
        print(f"  coverage           : {100.0 * kept_records / total:.2f}% of input rows used")
    return 0


if __name__ == "__main__":
    sys.exit(main())
