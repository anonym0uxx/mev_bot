#!/usr/bin/env python
"""SLINKY-backed per-fill curve reserves for the reward engine.

NEW module (v2/reserves).  It exposes the SAME accessor surface the reward engine
already uses (`price_pre_lamports_per_raw_token(signature, mint, is_buy)`), so it
can be passed through `reconcile_predictive(..., reserves=)` with no engine-side
schema change.

Difference from `reserves_loader.ReserveTable`: the slinky_gold_v3_compact
pump_state_v3 layer carries NO transaction signature.  Its rows are
(mint, event_time_unix_ms, seq) curve snapshots with the virtual SOL and virtual
token reserves, so a fill can only be priced by a CAUSAL TIME JOIN:

    pre-state of a replay fill at t_dec  ==  POST-state of the last slinky row
                                              with event_time_unix_ms <= t_dec

That join needs the replay tape, so this class takes the tape path at construction
and binds the (signature, mint, is_buy) key the engine asks for.

If the tape's window and the slinky window do not overlap, the produced table is
EMPTY, and every fill falls back to the engine's prior-tape path.  That is
reported, not papered over.

UNITS: lamports (1 SOL = 1e9 exactly); token reserves are raw base units (6 dp).
"""
from __future__ import annotations

import collections, glob, json, os

import numpy as np
import pyarrow.parquet as pq

LAMPORTS_PER_SOL = 1_000_000_000
assert LAMPORTS_PER_SOL == 10 ** 9


def load_slinky(reserves_dir, keep_mints=None):
    """{mint: (ts[], vsol[], vtok[], rsol[], rtok[])} from the slinky reserves table.

    `keep_mints` bounds the scan: rows whose mint is not in the set are skipped
    before any per-row Python work (the month-long table has 601,578 mints; a
    replay tape usually needs a few thousand of them).
    """
    files = sorted(glob.glob(os.path.join(reserves_dir, "*.parquet")))
    if not files:
        raise FileNotFoundError("no parquet parts under %s" % reserves_dir)
    acc = collections.defaultdict(list)
    for f in files:
        t = pq.read_table(f, columns=["mint", "event_time_unix_ms",
                                      "virtual_sol_reserves_lamports", "virtual_token_reserves_raw",
                                      "real_sol_reserves_lamports", "real_token_reserves_raw"])
        mi = t["mint"].to_pylist()
        ts = t["event_time_unix_ms"].to_numpy(zero_copy_only=False)
        vs = t["virtual_sol_reserves_lamports"].to_numpy(zero_copy_only=False)
        vt = t["virtual_token_reserves_raw"].to_numpy(zero_copy_only=False)
        rs = t["real_sol_reserves_lamports"].to_numpy(zero_copy_only=False)
        rt = t["real_token_reserves_raw"].to_numpy(zero_copy_only=False)
        for j, m in enumerate(mi):
            if keep_mints is not None and m not in keep_mints:
                continue
            acc[m].append((int(ts[j]), int(vs[j]), int(vt[j]), int(rs[j]), int(rt[j])))
    out = {}
    for m, lst in acc.items():
        lst.sort(key=lambda x: x[0])
        out[m] = tuple(np.array([x[i] for x in lst], dtype=np.int64) for i in range(5))
    return out, len(files)


class SlinkyFillReserves:
    """Engine-compatible reserve table built from slinky + a replay tape."""

    def __init__(self, tape_path, reserves_dir, max_stale_ms=60_000):
        self.max_stale_ms = max_stale_ms
        self._idx = {}
        self.fills_seen = 0
        self.fills_bound = 0
        self.fills_no_mint = 0
        self.fills_no_event_before = 0
        self.fills_stale = 0
        self.max_staleness_ms = 0
        self.fills_bad_reserve = 0
        self.t_dec_min = None
        self.t_dec_max = None
        rows = [json.loads(l) for l in open(tape_path)]
        self.tape_mints = {r["mint"] for r in rows}
        self.tab, self.parts = load_slinky(reserves_dir, keep_mints=self.tape_mints)
        self.slinky_mints_scanned = len(self.tab)
        for r in rows:
                self.fills_seen += 1
                m = r["mint"]
                t = int(r["recv_unix_ms"])
                is_buy = (r.get("side") == "buy")
                if self.t_dec_min is None or t < self.t_dec_min:
                    self.t_dec_min = t
                if self.t_dec_max is None or t > self.t_dec_max:
                    self.t_dec_max = t
                ent = self.tab.get(m)
                if ent is None:
                    self.fills_no_mint += 1
                    continue
                ts = ent[0]
                i = int(np.searchsorted(ts, t, side="right")) - 1
                if i < 0:
                    self.fills_no_event_before += 1
                    continue
                if self.max_stale_ms and (t - int(ts[i])) > self.max_stale_ms:
                    self.max_staleness_ms = max(self.max_staleness_ms, t - int(ts[i]))
                    self.fills_stale += 1
                    continue
                vs, vt = int(ent[1][i]), int(ent[2][i])
                if vs <= 0 or vt <= 0:
                    self.fills_bad_reserve += 1
                    continue
                self._idx[(r.get("signature"), m, is_buy)] = (vs, vt)
                self.fills_bound += 1

    def __len__(self):
        return len(self._idx)

    def stats(self):
        return {"slinky_parts": self.parts, "slinky_mints": len(self.tab),
                "tape_fills_seen": self.fills_seen, "fills_bound_to_curve": self.fills_bound,
                "fills_mint_absent_from_slinky": self.fills_no_mint,
                "fills_no_slinky_event_before_t_dec": self.fills_no_event_before,
                "fills_dropped_stale": self.fills_stale,
                "max_stale_ms_enforced": self.max_stale_ms,
                "max_staleness_ms_observed": self.max_staleness_ms,
                "fills_dropped_nonpositive_reserve": self.fills_bad_reserve,
                "tape_t_dec_min_ms": self.t_dec_min, "tape_t_dec_max_ms": self.t_dec_max}

    def price_pre_lamports_per_raw_token(self, signature, mint, is_buy):
        hit = self._idx.get((signature, mint, bool(is_buy)))
        if hit is None:
            return None
        vs, vt = hit
        return vs / vt


class ChainReserveTable:
    """Try each table in order; first non-None pre-fill price wins."""

    def __init__(self, tables):
        self.tables = [t for t in tables if t is not None]
        self.hits = [0] * len(self.tables)

    def __len__(self):
        return sum(len(t) for t in self.tables)

    def price_pre_lamports_per_raw_token(self, signature, mint, is_buy):
        for i, t in enumerate(self.tables):
            p = t.price_pre_lamports_per_raw_token(signature, mint, is_buy)
            if p is not None:
                self.hits[i] += 1
                return p
        return None

    def stats(self):
        return {"tables": [len(t) for t in self.tables], "hits_by_table": self.hits}
