#!/usr/bin/env python
"""Build a per-fill pump.fun bonding-curve RESERVES table from raw LaserStream captures.

SOURCE OF TRUTH
---------------
pump.fun emits an Anchor `TradeEvent` (discriminator sha256("event:TradeEvent")[:8]
= bddb7fd34ee661ee) as a `Program data: <b64>` line inside tx meta.log_messages.
Verified layout (Borsh, little endian), validated against the trailing
"buy"/"sell" string tag and against pump.fun-owned account-state writes:

   off  size  field
   0    8     event discriminator (bddb7fd34ee661ee)
   8    32    mint (Pubkey)
   40   8     sol_amount          (lamports; the leg exchanged with the curve)
   48   8     token_amount        (raw token base units, 6 decimals)
   56   1     is_buy              (bool)
   57   32    user (Pubkey)
   89   8     timestamp           (i64 unix seconds, == recv_unix_ms//1000)
   97   8     virtual_sol_reserves   (lamports)   POST-fill
   105  8     virtual_token_reserves (raw units)  POST-fill
   113  8     real_sol_reserves      (lamports)   POST-fill
   121  8     real_token_reserves    (raw units)  POST-fill
   129  ...   fee_recipient / fee / creator / creator_fee / variant tail

So reserves are PRESENT, not derived.  We additionally invert the curve update
to recover the PRE-fill state on the same row:

  buy : pre_vsol = post_vsol - sol_amount ; pre_vtok = post_vtok + token_amount
  sell: pre_vsol = post_vsol + sol_amount ; pre_vtok = post_vtok - token_amount

UNITS ARE BINDING: every *_sol field is lamports (1 SOL = 1e9 lamports);
every *_token field is raw token base units (decimals 6).  price =
virtual_sol_reserves / virtual_token_reserves  [lamports / raw token unit].
The builder asserts the lamport magnitude bound and fails loudly on violation.

Output: one parquet per raw part in --outdir, plus a merged parquet.
"""
from __future__ import annotations

import argparse, base64, glob, json, os, struct, subprocess, sys
from concurrent.futures import ProcessPoolExecutor, as_completed

TRADE_DISC = bytes.fromhex("bddb7fd34ee661ee")
LAMPORTS_PER_SOL = 1_000_000_000
assert LAMPORTS_PER_SOL == 10 ** 9
MAX_LAMPORTS = 10 ** 19  # > 1e10 SOL: SOL/lamport mix-up

_B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def b58(b: bytes) -> str:
    n = int.from_bytes(b, "big")
    out = []
    while n:
        n, r = divmod(n, 58)
        out.append(_B58[r])
    pad = 0
    for c in b:
        if c == 0:
            pad += 1
        else:
            break
    return "1" * pad + ("".join(reversed(out)) or "")


def _lam(v, what):
    if v < 0 or v > MAX_LAMPORTS:
        raise ValueError(f"{what}: lamports out of range ({v}) - SOL/lamport mix-up?")
    return v


def _dec(v: int):
    """Exact curve constant k as decimal128 input (k ~ 1e15*1e10 = 1e25 > int64)."""
    from decimal import Decimal
    return Decimal(v)


def process_part(path: str):
    """Return (rows, stats) for one raw .ndjson.zst part."""
    rows = []
    st = dict(records=0, tx=0, trade_events=0, short_events=0,
              no_meta=0, bad_b64=0, other_program_data=0,
              reject_implausible=0, reject_pre_nonpositive=0,
              reject_isbuy_byte=0, trade_events_seen=0)
    cache = {}
    p = subprocess.Popen(["zstd", "-dc", path], stdout=subprocess.PIPE)
    for line in p.stdout:
        st["records"] += 1
        if b'"record_type":"transaction"' not in line:
            continue
        st["tx"] += 1
        try:
            o = json.loads(line)
        except Exception:
            continue
        pl = o.get("payload") or {}
        meta = pl.get("meta") or {}
        if not meta:
            st["no_meta"] += 1
            continue
        logs = meta.get("log_messages") or []
        if not logs:
            continue
        sig = pl.get("signature_b58")
        slot = o.get("slot")
        rms = o.get("recv_unix_ms")
        txi = pl.get("tx_index")
        ei = 0
        for l in logs:
            if not l or "Program data: " not in l:
                continue
            try:
                raw = base64.b64decode(l.split("Program data: ", 1)[1])
            except Exception:
                st["bad_b64"] += 1
                continue
            if raw[:8] != TRADE_DISC:
                st["other_program_data"] += 1
                continue
            st["trade_events_seen"] += 1
            if len(raw) < 129:
                st["short_events"] += 1
                continue
            if raw[56] > 1:
                st["reject_isbuy_byte"] += 1
                continue
            sol_amt, tok_amt = struct.unpack_from("<QQ", raw, 40)
            is_buy = raw[56] != 0
            ts = struct.unpack_from("<q", raw, 89)[0]
            vsol, vtok, rsol, rtok = struct.unpack_from("<QQQQ", raw, 97)
            # plausibility gate: reject variant/garbage payloads, count them.
            if not (0 < vsol <= 10 ** 13 and 0 < vtok <= 10 ** 18
                    and 0 <= rsol <= vsol and 0 <= rtok <= vtok):
                st["reject_implausible"] = st.get("reject_implausible", 0) + 1
                continue
            pre_vsol = vsol - sol_amt if is_buy else vsol + sol_amt
            pre_vtok = vtok + tok_amt if is_buy else vtok - tok_amt
            if pre_vsol <= 0 or pre_vtok <= 0:
                st["reject_pre_nonpositive"] = st.get("reject_pre_nonpositive", 0) + 1
                continue
            hx = raw[8:40].hex()
            mb = cache.get(hx)
            if mb is None:
                mb = b58(raw[8:40]); cache[hx] = mb
            rows.append((
                mb, hx, sig, slot, rms, txi, ei,
                bool(is_buy), ts,
                int(sol_amt), int(tok_amt),
                int(vsol), int(vtok), int(rsol), int(rtok),
                int(pre_vsol), int(pre_vtok),
                _dec(int(vsol) * int(vtok)),
                _dec(int(pre_vsol) * int(pre_vtok)) if pre_vtok > 0 else None,
            ))
            ei += 1
            st["trade_events"] += 1
    p.stdout.close()
    rc = p.wait()
    st["zstd_rc"] = rc
    return os.path.basename(path), rows, st


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--glob", required=True, help="glob of raw .ndjson.zst parts")
    ap.add_argument("--outdir", required=True)
    ap.add_argument("--workers", type=int, default=16)
    ap.add_argument("--limit", type=int, default=0, help="max parts (0=all)")
    ap.add_argument("--tag", default="reserves")
    a = ap.parse_args()

    parts = sorted(glob.glob(a.glob))
    if a.limit:
        parts = parts[:a.limit]
    if not parts:
        print("FATAL: glob matched ZERO files:", a.glob, file=sys.stderr)
        return 2
    print(f"parts_matched={len(parts)}")

    import pyarrow as pa
    import pyarrow.parquet as pq
    os.makedirs(a.outdir, exist_ok=True)

    schema = pa.schema([
        ("mint_b58", pa.string()), ("mint_hex", pa.string()),
        ("signature", pa.string()), ("slot", pa.int64()),
        ("recv_unix_ms", pa.int64()), ("tx_index", pa.int32()),
        ("event_index", pa.int32()), ("is_buy", pa.bool_()),
        ("trade_ts_unix", pa.int64()),
        ("sol_amount_lamports", pa.int64()), ("token_amount_raw", pa.int64()),
        ("virtual_sol_reserves_lamports", pa.int64()),
        ("virtual_token_reserves_raw", pa.int64()),
        ("real_sol_reserves_lamports", pa.int64()),
        ("real_token_reserves_raw", pa.int64()),
        ("pre_virtual_sol_reserves_lamports", pa.int64()),
        ("pre_virtual_token_reserves_raw", pa.int64()),
        ("k_post", pa.decimal128(38, 0)), ("k_pre", pa.decimal128(38, 0)),
    ])
    tot = dict(records=0, tx=0, trade_events=0, short_events=0, no_meta=0,
               bad_b64=0, other_program_data=0, parts=0, parts_failed=0,
               reject_implausible=0, reject_pre_nonpositive=0, reject_isbuy_byte=0,
               trade_events_seen=0)
    nrows = 0
    failed = []
    with ProcessPoolExecutor(max_workers=a.workers) as ex:
        futs = {ex.submit(process_part, p): p for p in parts}
        for i, f in enumerate(as_completed(futs), 1):
            try:
                name, rows, st = f.result()
            except Exception as e:
                failed.append((futs[f], repr(e)))
                tot["parts_failed"] += 1
                continue
            tot["parts"] += 1
            for k in ("records", "tx", "trade_events", "short_events",
                      "no_meta", "bad_b64", "other_program_data",
                      "reject_implausible", "reject_pre_nonpositive",
                      "reject_isbuy_byte", "trade_events_seen"):
                tot[k] += st.get(k, 0)
            if rows:
                cols = list(zip(*rows))
                tbl = pa.table({s.name: pa.array(c, s.type)
                                for s, c in zip(schema, cols)}, schema=schema)
                out = os.path.join(a.outdir, name.replace(".ndjson.zst", "") + ".parquet")
                pq.write_table(tbl, out, compression="zstd")
                nrows += len(rows)
            if i % 25 == 0 or i == len(parts):
                print(f"  done {i}/{len(parts)} rows_so_far={nrows}", flush=True)
    print("STATS", json.dumps(tot))
    print("failed_parts", len(failed))
    for p, e in failed[:10]:
        print("  FAIL", p, e)

    # merge
    import pyarrow.dataset as ds
    d = ds.dataset(a.outdir, format="parquet")
    pq.write_table(d.to_table(), os.path.join(a.outdir, f"{a.tag}_all.parquet"),
                   compression="zstd")
    print("merged_rows", nrows, "->", os.path.join(a.outdir, f"{a.tag}_all.parquet"))
    return 0


if __name__ == "__main__":
    sys.exit(main())