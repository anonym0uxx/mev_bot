#!/usr/bin/env python
"""Documented loader for the reconstructed pump.fun bonding-curve reserves table.

NEW module (v2/reserves).  Nothing here edits or replaces existing code; the
reward engine consumes it through `reward_engine.reconcile_predictive(..., reserves=)`.

WHAT THIS PROVIDES
------------------
A per-fill reserves table built by `build_reserves.py` from raw LaserStream
captures.  Each row is one pump.fun `TradeEvent` (Borsh, disc bddb7fd34ee661ee)
and carries BOTH the post-fill curve state emitted by the program and the
pre-fill state recovered by inverting the curve update:

    pre_vsol = post_vsol -/+ sol_amount      (minus on buy, plus on sell)
    pre_vtok = post_vtok +/- token_amount    (plus on buy, minus on sell)

Row key: (signature, mint_b58, is_buy).

UNITS (binding)
    *_sol   : lamports, 1 SOL = 1e9 lamports exactly
    *_token : raw token base units (mint decimals 6)
    price   : lamports per raw token unit = vsol_lamports / vtok_raw
              to get SOL per whole token multiply by 1e6 / 1e9.

VALIDATION STATUS (see /training/v2/reports/RESERVES_*.json)
    * k = vsol*vtok preserved per fill: median relative drift 1.2e-11
      (integer rounding); 4.4% of fills drift >1e-3 (fee inside the SOL leg).
    * absolute levels: 87-96% of txs show an EXACT quadruple match between the
      event's reserves and the on-chain bonding-curve account write in the same
      transaction signature.
    * price: log-Pearson 0.977 / Spearman 0.986 against canonical v7 recorded
      execution prices, 53% within 5%, 96% within 2x.  Raw Pearson is ~0
      because price spans ~8 decades and ~1.4% dust fills (median notional
      0.002 SOL) carry fixed-fee/rent contamination in the recorded leg.
"""
from __future__ import annotations

import glob
import os

import numpy as np
import pyarrow.dataset as ds

LAMPORTS_PER_SOL = 1_000_000_000
assert LAMPORTS_PER_SOL == 10 ** 9
TOKEN_DECIMALS = 6


def _expand(paths):
    """Resolve files / globs / directories to concrete parquet files, avoiding
    double-counting a directory's per-part files together with its merge."""
    out = []
    for p in paths:
        if os.path.isdir(p):
            merged = sorted(glob.glob(os.path.join(p, "*_all.parquet")))
            if not merged:
                raise FileNotFoundError(f"{p}: no *_all.parquet in reserves dir")
            out.extend(merged)
        elif any(c in p for c in "*?["):
            out.extend(sorted(glob.glob(p)))
        else:
            out.append(p)
    return out


class ReserveTable:
    """Per-fill pump.fun bonding-curve reserves, indexed by (sig, mint, side)."""

    def __init__(self, paths, columns=None):
        paths = _expand(paths)
        if not paths:
            raise FileNotFoundError("ReserveTable: no reserves paths resolved")
        d = ds.dataset(paths, format="parquet")
        cols = columns or [
            "mint_b58", "signature", "slot", "recv_unix_ms", "tx_index",
            "event_index", "is_buy", "sol_amount_lamports", "token_amount_raw",
            "virtual_sol_reserves_lamports", "virtual_token_reserves_raw",
            "real_sol_reserves_lamports", "real_token_reserves_raw",
            "pre_virtual_sol_reserves_lamports", "pre_virtual_token_reserves_raw",
        ]
        t = d.to_table(columns=cols)
        self.R = {c: t[c].to_numpy(zero_copy_only=False) for c in t.column_names}
        self.n = len(self.R["signature"])
        if self.n == 0:
            raise ValueError("FATAL: reserves table is empty - refusing to proceed")
        self._index = {}
        for i in range(self.n):
            self._index.setdefault(
                (self.R["signature"][i], self.R["mint_b58"][i],
                 bool(self.R["is_buy"][i])), []).append(i)

    def __len__(self):
        return self.n

    def row_index(self, signature, mint, is_buy):
        v = self._index.get((signature, mint, bool(is_buy)))
        return v[0] if v else None

    # ---- price accessors -------------------------------------------------
    def price_pre_lamports_per_raw_token(self, signature, mint, is_buy):
        """Curve spot price immediately BEFORE the fill, lamports per raw unit."""
        i = self.row_index(signature, mint, is_buy)
        if i is None:
            return None
        vs = int(self.R["pre_virtual_sol_reserves_lamports"][i])
        vt = int(self.R["pre_virtual_token_reserves_raw"][i])
        if vt <= 0 or vs <= 0:
            return None
        if not (vs <= 10 ** 13):
            raise ValueError(f"price_pre: vsol {vs} out of lamport range")
        return vs / vt

    def price_post_lamports_per_raw_token(self, signature, mint, is_buy):
        i = self.row_index(signature, mint, is_buy)
        if i is None:
            return None
        vs = int(self.R["virtual_sol_reserves_lamports"][i])
        vt = int(self.R["virtual_token_reserves_raw"][i])
        return (vs / vt) if vt > 0 and vs > 0 else None

    def state_pre(self, signature, mint, is_buy):
        """(vsol_lamports, vtok_raw, rsol_lamports, rtok_raw) before the fill."""
        i = self.row_index(signature, mint, is_buy)
        if i is None:
            return None
        r = self.R
        return (int(r["pre_virtual_sol_reserves_lamports"][i]),
                int(r["pre_virtual_token_reserves_raw"][i]),
                None, int(r["real_token_reserves_raw"][i]))

    def coverage(self, signatures, mints, isbuys):
        """Fraction of the given (sig, mint, is_buy) triples present."""
        if len(signatures) == 0:
            return 0.0
        hit = sum(1 for s, m, b in zip(signatures, mints, isbuys)
                  if (s, m, bool(b)) in self._index)
        return hit / len(signatures)


def load_default():
    """Load every reconstructed reserves table under /training/v2/reserves."""
    import glob as _g
    paths = sorted(_g.glob("/training/v2/reserves/*/*_all.parquet"))
    if not paths:
        raise FileNotFoundError("no *_all.parquet under /training/v2/reserves/")
    return ReserveTable(paths)


if __name__ == "__main__":
    t = load_default()
    print("rows", len(t))
    print("distinct keys", len(t._index))
    import collections
    c = collections.Counter(t.R["mint_b58"])
    print("distinct_mints", len(c))
    print("slot_range", int(t.R["slot"].min()), int(t.R["slot"].max()))
    print("ms_range", int(t.R["recv_unix_ms"].min()), int(t.R["recv_unix_ms"].max()))