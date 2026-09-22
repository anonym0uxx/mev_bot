#!/usr/bin/env python
"""Mechanical pump.fun/PumpSwap execution simulator -> RL reward.

NEW code (v2/rl). No model-generated labels, no AI-authored targets: the reward
is produced purely by simulating execution against causally-ordered recorded
trade/state data.

WHAT IS RECORDED (verified, see --probe):
  canonical/renormalized_v7/trades.jsonl  (15,894,228 rows) fields:
    mint, trader, side, venue, slot, recv_unix_ms, signature, sol_lamports,
    tokens_raw, fee_lamports, cu_consumed, status, resolution
  -> sol_lamports  = the trader's OWN signed SOL balance delta for that fill
  -> tokens_raw    = the trader's OWN signed raw-token delta for that fill
  -> fee_lamports  = the Solana *transaction* fee (network + priority) from
                     tx meta.fee, NOT the pump.fun trading fee
  The realized execution price of a fill is therefore sol_lamports/tokens_raw:
  the actual average price that fill got on the curve (impact already inside).

WHAT IS *NOT* RECORDED (hard limitation, reported not assumed):
  * No curve RESERVE LEVELS exist anywhere in the canonical data - there is no
    virtual_sol_reserves / virtual_token_reserves / real_token_reserves field
    in trades.jsonl or ledger_v7/states.jsonl, and no curve account state.
  * renormalize_raw.py DOES emit pool-side per-swap DELTAS
    (pool_sol_lamports, pool_tokens_raw, price_basis), but only a 50MB derived
    subset (/training/v2/canonical/renorm_pooltest/trades.jsonl, 91,735 rows,
    1,268 mints) retained them; canonical v7 dropped the columns. Those are
    DELTAS, not levels, so they still do not give an absolute reserve state.
    Only 9 of the 793 examination mints appear in that subset.
  => price = virtual_sol / virtual_token cannot be computed from recorded
     reserves. The engine instead prices off the RECORDED CURVE EXECUTION PRICE
     (real transacted price per fill, impact included) and adds an explicit
     constant-product impact term for OUR incremental size, whose depth is
     MEASURED from our own tape (--measure-depth). This is stated as a
     limitation, not hidden.

MIRRORED CONSERVATION INVARIANTS (build_replay_v2 / replay_v2.0_constant_notional,
which produced 57,079 checks / 0 violations in reports/replay_v7.log):
  per step:  equity = cash + qty*mark ; require finite, equity>=-eps,
             cash>=-eps, qty>=-eps  (no short, no negative cash)
  per episode: capital-init identity, cash/qty ledger reconciliation from the
             independent leg log, terminal-flat-after-exit, lamport cross-check.
"""
from __future__ import annotations

import argparse, hashlib, json, math, os, sys
import numpy as np

# Mechanics-first regime pricing (same directory; the module is the ONE interface
# that prices a fill from observable reserves at decision time).
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from exit_mechanics import AMM_VENUE_FEE_BPS_PER_LEG  # noqa: E402
from regime_pricing import (  # noqa: E402
    Regime, ReserveUnavailable, ReserveStale, ReserveLookahead, RegimeMismatch,
    OrderTooLarge, ReserveState, FillTerms, price_fill, bonding_params, amm_params,
    DictReserveOracle, AMM_LP_FEE_RATE, AMM_TRADER_EXTRACTION_RATE,
    LookaheadError,   # single source of truth: the reserve layer's error class
)

# ---------------------------------------------------------------------------
# 3. LAMPORTS DISCIPLINE  (1 SOL = 1e9 lamports exactly; fail loudly on abuse)
# ---------------------------------------------------------------------------
LAMPORTS_PER_SOL = 1_000_000_000
assert LAMPORTS_PER_SOL == 10 ** 9, "lamport unit constant corrupted"
assert 1 * LAMPORTS_PER_SOL * 10 ** 9 == 10 ** 18


class UnitError(RuntimeError):
    """Raised when a SOL/lamport unit mismatch is detected. Fails loudly."""


# LookaheadError is imported from regime_pricing: the tape guard and the reserve
# guard must raise the same class so a scored decision can never consult data
# timestamped after its own decision time, from any layer.


def to_sol_checked(lamports, what="value"):
    if isinstance(lamports, float):
        raise UnitError(f"{what}: float passed where integer lamports required ({lamports!r})")
    if not isinstance(lamports, (int, np.integer)):
        raise UnitError(f"{what}: non-integer lamports ({type(lamports).__name__})")
    if abs(int(lamports)) > 10 ** 19:  # > 1e10 SOL: almost certainly SOL not lamports
        raise UnitError(f"{what}: |lamports|={lamports} exceeds 1e19 - SOL/lamport mix-up?")
    return int(lamports) / LAMPORTS_PER_SOL


def to_lamports_checked(sol, what="value"):
    if not isinstance(sol, (int, float, np.floating)):
        raise UnitError(f"{what}: non-numeric SOL ({type(sol).__name__})")
    lam = int(round(float(sol) * LAMPORTS_PER_SOL))
    if abs(float(sol) * LAMPORTS_PER_SOL - lam) > 0.5:
        raise UnitError(f"{what}: SOL->lamport drift {sol} -> {lam} (>0.5 lamport)")
    return lam


# ---------------------------------------------------------------------------
# Measured constants (derived from our own data; see --measure-depth / --measure-fee)
# ---------------------------------------------------------------------------
# Measured on /training/v2/canonical/renorm_pooltest/trades.jsonl, clean
# single-hop swaps (trader token leg == pool token leg, |diff|<=1%,
# resolution=='instruction_accounts'), n=65,927.
FEE_RATE_MEASURED = 0.009463          # notional-weighted mean (trader-leg - pool-leg)/notional
FEE_RATE_MEDIAN = 0.009623            # distribution median
PRIORITY_FEE_LAMPORTS_MEDIAN = 10_000  # tx fee (network+priority), p50; p90=45,000
PRIORITY_FEE_LAMPORTS_P90 = 45_000

CAPITAL_SOL = 1.0
from exit_mechanics import DEPLOY_SOL_CANONICAL as _DEPLOY_CANON  # noqa: E402
DEPLOY_SOL = _DEPLOY_CANON   # was 0.5: aligned to the notional the RL path trades
ADD_FRACTION = 0.5
HALF_COST_BP = 180.0  # legacy alias; the real cost model below supersedes it


# ---------------------------------------------------------------------------
# Tape: per-mint causally ordered fills
# ---------------------------------------------------------------------------
class Tape:
    """A mint's causally ordered fills. Price reads are guarded against lookahead."""

    __slots__ = ("mint", "tt", "sol", "tok", "px", "venue", "side")

    def __init__(self, mint, tt, sol, tok, venue, side):
        self.mint = mint
        self.tt = np.asarray(tt, dtype=np.int64)
        self.sol = np.asarray(sol, dtype=np.int64)
        self.tok = np.asarray(tok, dtype=np.int64)
        self.venue = list(venue)
        self.side = list(side)
        px = np.full(self.tt.size, np.nan)
        ok = self.tok != 0
        px[ok] = np.abs(self.sol[ok].astype(np.float64)) / np.abs(self.tok[ok].astype(np.float64))
        self.px = px
        if not np.all(np.diff(self.tt) >= 0):
            raise ValueError(f"tape {mint} not causally ordered")

    def price_at_or_before(self, t_ms, decision_ms, what="price"):
        """Realized curve execution price of the last fill at/after t_ms index-wise.

        Strictly causal: every consulted fill must be timestamped <= decision_ms.
        """
        j = int(np.searchsorted(self.tt, t_ms, side="right")) - 1
        if j < 0:
            return None
        self._guard(j, decision_ms, what)
        return float(self.px[j])

    def _guard(self, idx, decision_ms, what):
        if idx < 0 or idx >= self.tt.size:
            raise LookaheadError(f"{what}: index {idx} out of range")
        if self.tt[idx] > decision_ms:
            raise LookaheadError(
                f"{what}: LOOKAHEAD - consulted fill ts={int(self.tt[idx])} > decision "
                f"ts={int(decision_ms)} (mint={self.mint})")

    def first_fill_after(self, t_ms, decision_ms, what="entry"):
        j = int(np.searchsorted(self.tt, t_ms, side="right"))
        if j >= self.tt.size:
            return None
        self._guard(j, max(decision_ms, int(self.tt[j])), what)  # entry executes at/after decision
        return j

    def venue_change_index(self):
        """First index where venue differs from index 0 -> graduation boundary."""
        for i in range(1, self.tt.size):
            if self.venue[i] != self.venue[0]:
                return i
        return None


def load_canonical_tapes(path="/training/v2/canonical/renormalized_v7/trades.jsonl",
                         mints=None, max_rows=None, min_notional_lamports=0,
                         max_px_ratio=0):
    """Load tapes. `min_notional_lamports` drops dust trades whose price is
    meaningless: px = sol_lamports/tokens_raw explodes when tokens_raw is tiny, and
    a handful of such rows can dominate a mean (measured: a corpus-aligned tape set
    produced a mean reward of -311 on a quantity bounded below by -1)."""
    import pyarrow.json as paj
    tbl = paj.read_json(path)
    d = tbl.to_pydict()
    want = set(mints) if mints else None
    order = {}
    floor = int(min_notional_lamports or 0)
    for i, m in enumerate(d["mint"]):
        if want is not None and m not in want:
            continue
        if floor:
            sol = d["sol_lamports"][i]
            tok = d["tokens_raw"][i]
            if sol is None or tok in (None, 0) or abs(int(sol)) < floor:
                continue
        order.setdefault(m, []).append(i)
    out = {}
    for m, idxs in order.items():
        idxs.sort(key=lambda i: (d["recv_unix_ms"][i], d["signature"][i]))
        if max_px_ratio:
            # Drop price OUTLIERS: a single row with a tiny token leg gives an
            # absurd px, and because the equity path marks to it, one such row
            # inflates the peak and hence the drawdown term without bound
            # (measured: mean reward -214 on a value bounded below by -1).
            import statistics as _st
            px = []
            for i in idxs:
                tok = d["tokens_raw"][i]
                sol = d["sol_lamports"][i]
                if tok and sol:
                    v = abs(float(sol)) / abs(float(tok))
                    if v > 0:
                        px.append(v)
            if len(px) >= 5:
                med = _st.median(px)
                lo, hi = med / max_px_ratio, med * max_px_ratio
                keep = []
                for i in idxs:
                    tok = d["tokens_raw"][i]
                    sol = d["sol_lamports"][i]
                    if not tok or not sol:
                        continue
                    v = abs(float(sol)) / abs(float(tok))
                    if lo <= v <= hi:
                        keep.append(i)
                if len(keep) >= 5:
                    idxs = keep
        out[m] = Tape(m,
                      [d["recv_unix_ms"][i] for i in idxs],
                      [d["sol_lamports"][i] for i in idxs],
                      [d["tokens_raw"][i] for i in idxs],
                      [d["venue"][i] for i in idxs],
                      [d["side"][i] for i in idxs])
    return out


# ---------------------------------------------------------------------------
# Depth (liquidity) measurement from our own tape: constant-product slope
# ---------------------------------------------------------------------------
def measure_depth(tape, min_fills=30, min_notional_lamports=100_000):
    """Robust slope-through-origin of |log(px_i/px_{i-1})| on SOL notional q_i.

    For a constant-product curve, a fill of SOL notional q moves the price by
    ~q/D, so D = sum(q^2) / sum(q*impact). Returns depth in SOL, or None.
    """
    tt = tape.tt
    px = tape.px
    q = np.abs(tape.sol.astype(np.float64)) / LAMPORTS_PER_SOL
    m = np.isfinite(px) & (px > 0) & np.isfinite(np.roll(px, 1)) & (np.roll(px, 1) > 0)
    m[0] = False
    m &= q >= (min_notional_lamports / LAMPORTS_PER_SOL)
    if m.sum() < min_fills:
        return None
    impact = np.abs(np.log(px[m] / np.roll(px, 1)[m]))
    qq = q[m]
    denom = float(np.sum(qq * impact))
    if denom <= 0:
        return None
    D = float(np.sum(qq * qq) / denom)
    if not np.isfinite(D) or D <= 0:
        return None
    return D


# ---------------------------------------------------------------------------
# Engine
# ---------------------------------------------------------------------------
class RewardEngine:
    def __init__(self, fee_rate=FEE_RATE_MEASURED, priority_fee_lamports=PRIORITY_FEE_LAMPORTS_MEDIAN,
                 creator_fee_rate=0.0, capital_sol=CAPITAL_SOL, deploy_sol=DEPLOY_SOL,
                 depth_sol=None, depth_floor_sol=5.0, strict=True,
                 regime=Regime.LEGACY_TAPE, oracle=None, amm_lp_fee_rate=AMM_LP_FEE_RATE,
                 max_reserve_stale_ms=60_000, require_reserves=True,
                 venue_fee_aware=True):
        if not (0.0 <= fee_rate < 0.5):
            raise ValueError("implausible fee_rate")
        self.fee_rate = float(fee_rate)
        self.priority_fee_lamports = int(priority_fee_lamports)
        self.priority_fee_sol = to_sol_checked(self.priority_fee_lamports, "priority_fee")
        self.creator_fee_rate = float(creator_fee_rate)
        self.capital_sol = float(capital_sol)
        self.deploy_sol = float(deploy_sol)
        self.depth_sol = depth_sol
        self.depth_floor_sol = float(depth_floor_sol)
        self.strict = strict
        self._dcache = {}
        # -- regime / mechanics state --------------------------------------
        self.regime = regime if isinstance(regime, Regime) else Regime(str(regime))
        self.oracle = oracle
        self.amm_lp_fee_rate = float(amm_lp_fee_rate)
        self.max_reserve_stale_ms = int(max_reserve_stale_ms)
        # require_reserves: when a mechanics regime is selected, reserves are
        # MANDATORY. There is exactly one explicit opt-out (legacy tape pricing)
        # and it is selected by regime=LEGACY_TAPE, never by a silent fallback.
        self.require_reserves = bool(require_reserves)
        # venue_fee_aware: the AMM charges its own 30 bp/side schedule (25 bp LP +
        # 5 bp protocol) on top of the recorded price; the gross/net pool-vs-trader
        # ratio p50 = 1.0 is the price a PAST trade printed at, not the fee OUR next
        # trade pays. ON by default (signature default True) so the AMM is priced
        # correctly; see AMM_VENUE_FEE_BPS_PER_LEG in exit_mechanics.py.
        self.venue_fee_aware = bool(venue_fee_aware)
        self.joins = []              # every reserve consulted, with staleness
        self.reserve_refusals = 0

    def episode_reserve_report(self):
        """Exactly which reserves priced the episode(s) since the last reset, and
        how stale each one was at its own decision time.

        This is the anti-masquerade surface: a stale join shows up as an explicit
        staleness_ms, and any join whose reserve post-dates its decision is
        flagged in lookahead_joins (that is a bug, not a silent state).
        """
        joins = self.joins
        return {
            "regime": self.regime.value,
            "reserve_joins_count": len(joins),
            "reserve_joins": joins,
            "max_staleness_ms": max((j["staleness_ms"] for j in joins), default=None),
            "min_staleness_ms": min((j["staleness_ms"] for j in joins), default=None),
            "lookahead_joins": sum(1 for j in joins
                                   if j["reserve_ts_ms"] > j["decision_ts_ms"]),
            "reserve_refusals": self.reserve_refusals,
        }

    def reset_ledger(self):
        self.joins = []
        self.reserve_refusals = 0

    def _params(self):
        if self.regime is Regime.AMM:
            return amm_params(lp_fee_rate=self.amm_lp_fee_rate,
                              trader_extraction_rate=AMM_TRADER_EXTRACTION_RATE)
        return bonding_params(fee_rate=self.fee_rate + self.creator_fee_rate)

    def _mechanics(self, tape, decision_ms, side, sol_in_lamports=None, token_in_raw=None):
        """Price OUR order from decision-time reserves. Refuses loudly.

        Raises ReserveUnavailable when the regime is a mechanics regime and no
        reserves exist for (mint, decision time). A stake cannot be priced from a
        tape price here: that path only exists under Regime.LEGACY_TAPE.
        """
        if self.oracle is None:
            self.reserve_refusals += 1
            raise ReserveUnavailable(
                f"regime={self.regime.value}: no reserve oracle wired for "
                f"mint={getattr(tape, 'mint', None)}; refusing to fall back to a "
                f"tape price (select regime=legacy_tape to do that explicitly)")
        st = self.oracle.reserve_for(tape.mint, self.regime, decision_ms,
                                     self.max_reserve_stale_ms)
        ft = price_fill(regime=self.regime, side=side, reserves=st,
                        decision_ts_ms=decision_ms, params=self._params(),
                        sol_in_lamports=sol_in_lamports, token_in_raw=token_in_raw)
        self.joins.append({
            "mint": tape.mint, "regime": ft.regime, "side": ft.side,
            "account": ft.account, "source": ft.source,
            "reserve_ts_ms": ft.reserve_ts_ms, "decision_ts_ms": ft.decision_ts_ms,
            "staleness_ms": ft.staleness_ms,
            "reserves_used": ft.reserves_used,
        })
        return ft

    # -- curve quoting (constant product, impact from measured depth) --------
    def _depth(self, tape):
        if self.depth_sol is not None:
            return self.depth_sol
        d = self._dcache.get(tape.mint)
        if d is None:
            d = measure_depth(tape)
            if d is None or d < self.depth_floor_sol:
                # A POSITIVE but absurd depth is the dangerous case: the slope
                # estimator divides by |dlog px|, so a price series with micro-noise
                # reports a tiny depth, and a 0.5 SOL exit is then modelled as
                # costing ~90% of the position (measured on the c8 management
                # sample: 75 episodes whose whole exit cost exceeded half the traded
                # notional). Measured over the 1,175 mints the management family
                # uses: median 13.7 SOL, p25 7.9, p10 0.22, p5 0.02 - i.e. one mint
                # in five reports a pool shallower than any pump.fun curve can be,
                # which is estimator failure and not a market fact. 5.0 SOL keeps the
                # floor below the p25 and above the failure tail, and is reported in
                # every quote as `depth_sol` so nothing is hidden.
                d = self.depth_floor_sol
            self._dcache[tape.mint] = d
        return d

    def _leg_fee_rate(self, tape, t_ms):
        """Per-leg pool fee actually charged on top of the recorded price.

        On a PumpSwap (AMM) fill the fee is the venue's own 30 bp/side schedule (25 bp
        LP + 5 bp protocol), charged on top of the recorded price; on a bonding-curve
        fill it is taken from the SOL leg. `venue_fee_aware=False` charges the measured
        curve rate everywhere (the historical behaviour)."""
        if not self.venue_fee_aware:
            return self.fee_rate
        i = int(np.searchsorted(tape.tt, t_ms, side="right")) - 1
        if 0 <= i < len(tape.venue) and tape.venue[i] == "pumpswap":
            # PumpSwap's own schedule (25 bp LP + 5 bp protocol). NOT zero: the pool
            # retaining its fee (gross/net ratio p50 = 1.0) explains the price a PAST
            # trade printed at, not the fee OUR trade pays.
            return AMM_VENUE_FEE_BPS_PER_LEG / 10_000.0
        return self.fee_rate

    def quote_buy(self, tape, t_ms, decision_ms, q_sol):
        """Buy q_sol SOL of the token. Returns dict of executable terms.

        Regime.LEGACY_TAPE prices off the tape (historical behaviour).
        Regime.BONDING_CURVE / Regime.AMM price off decision-time reserves and
        RAISE (ReserveUnavailable) when reserves are absent - never a tape price.
        """
        if self.regime is not Regime.LEGACY_TAPE:
            ft = self._mechanics(tape, decision_ms, "buy",
                                 sol_in_lamports=to_lamports_checked(q_sol, "buy_sol"))
            # FillTerms.token_delta_raw is already sign-correct for the trader
            # (+tokens on a buy, -tokens on a sell); do NOT negate it again.
            return {"tokens": float(ft.token_delta_raw), "px_ref": ft.px_pre,
                    "px_exec": ft.px_exec, "impact": ft.impact_only_bp / 1e4,
                    "impact_bp": ft.impact_only_bp, "fee_sol": ft.fee_lamports / LAMPORTS_PER_SOL,
                    "slippage_sol": ft.slippage_lamports / LAMPORTS_PER_SOL,
                    "creator_sol": 0.0, "priority_sol": self.priority_fee_sol,
                    "depth_sol": ft.reserves_used["sol_lamports"] / LAMPORTS_PER_SOL,
                    "regime": ft.regime, "reserves_used": ft.reserves_used,
                    "reserve_ts_ms": ft.reserve_ts_ms, "decision_ts_ms": ft.decision_ts_ms,
                    "staleness_ms": ft.staleness_ms, "reserve_account": ft.account,
                    "source": ft.source}
        p = tape.price_at_or_before(t_ms, decision_ms, "buy_price")
        if p is None or not np.isfinite(p) or p <= 0:
            return None
        D = self._depth(tape)
        rate = self._leg_fee_rate(tape, t_ms)
        fee_sol = q_sol * rate
        creator_sol = q_sol * self.creator_fee_rate
        q_eff = q_sol - fee_sol - creator_sol
        impact = q_eff / D
        px_exec = p * (1.0 + impact)
        tokens = q_eff / px_exec
        tokens_no_impact = q_eff / p
        slip_sol = (tokens_no_impact - tokens) * p
        return {"tokens": tokens, "px_ref": p, "px_exec": px_exec, "impact": impact,
                "fee_sol": fee_sol, "slippage_sol": slip_sol, "creator_sol": creator_sol,
                "priority_sol": self.priority_fee_sol, "depth_sol": D}

    def quote_sell(self, tape, t_ms, decision_ms, tokens):
        if self.regime is not Regime.LEGACY_TAPE:
            n = int(round(float(tokens)))
            if n <= 0:
                return None
            ft = self._mechanics(tape, decision_ms, "sell", token_in_raw=n)
            gross = (ft.sol_delta_lamports + ft.fee_lamports) / LAMPORTS_PER_SOL
            return {"sol_out": ft.sol_delta_lamports / LAMPORTS_PER_SOL,
                    "px_ref": ft.px_pre, "px_exec": ft.px_exec,
                    "impact": ft.impact_only_bp / 1e4, "impact_bp": ft.impact_only_bp,
                    "fee_sol": ft.fee_lamports / LAMPORTS_PER_SOL,
                    "slippage_sol": ft.slippage_lamports / LAMPORTS_PER_SOL,
                    "creator_sol": 0.0, "priority_sol": self.priority_fee_sol,
                    "depth_sol": ft.reserves_used["sol_lamports"] / LAMPORTS_PER_SOL,
                    "gross_sol": gross, "regime": ft.regime,
                    "reserves_used": ft.reserves_used, "reserve_ts_ms": ft.reserve_ts_ms,
                    "decision_ts_ms": ft.decision_ts_ms, "staleness_ms": ft.staleness_ms,
                    "reserve_account": ft.account, "source": ft.source}
        p = tape.price_at_or_before(t_ms, decision_ms, "sell_price")
        if p is None or not np.isfinite(p) or p <= 0 or tokens <= 0:
            return None
        D = self._depth(tape)
        notional = tokens * p
        gross = notional / (1.0 + notional / D)      # constant product
        slip_sol = notional - gross
        fee_sol = gross * self._leg_fee_rate(tape, t_ms)
        creator_sol = gross * self.creator_fee_rate
        net = gross - fee_sol - creator_sol
        return {"sol_out": net, "px_ref": p, "px_exec": gross / tokens if tokens else p,
                "impact": notional / D, "fee_sol": fee_sol, "slippage_sol": slip_sol,
                "creator_sol": creator_sol, "priority_sol": self.priority_fee_sol,
                "depth_sol": D, "gross_sol": gross}


# ---------------------------------------------------------------------------
# Action-sequence scoring -> NET SOL RETURNED
# ---------------------------------------------------------------------------
def score_action_sequence(tape, t_dec_ms, actions, engine=None,
                          capital_sol=CAPITAL_SOL, deploy_sol=DEPLOY_SOL,
                          add_fraction=ADD_FRACTION, horizon_ms=1_800_000,
                          interval_ms=30_000, max_ticks=16, min_hold_ms=60_000,
                          refuse_cross_graduation=True, entry=None):
    """Simulate HOLD/ADD/REDUCE/EXIT over a memecoin episode; return net SOL.

    Entry executes at the first fill at/after t_dec_ms (causal: you place the
    order at t_dec, it fills at the next traded price). Every subsequent price
    read is guarded to be <= its own decision time.

    HELD-POSITION CONTINUATION (`entry=`). When `entry` is given the episode
    starts from a position that is ALREADY OPEN and t_dec_ms is a MANAGEMENT
    decision, not an entry:

        entry = {"entry_px", "t_entry_ms", "qty_tokens", "cash_sol", "capital_sol"}

    * the position is not re-bought: qty/cash come from the caller's ledger, so
      the four candidates are valued from the SAME held state and differ only in
      the action taken at t_dec_ms (this is what makes them comparable);
    * tick 0 IS t_dec_ms, so acts[0] executes at the first fill at/after the
      management decision (causal, no lookahead);
    * the window runs t_dec_ms .. t_dec_ms+horizon_ms, so every candidate but
      EXIT shares the same forward window;
    * the held state is validated (`capital_sol == cash_sol + qty_tokens*entry_px`)
      and the entry price must equal the tape's own realized price at
      t_entry_ms - a held episode priced off any other basis is REFUSED
      (`entry_px_mismatch` / `entry_fill_not_found`), never silently scored.
    """
    engine = engine or RewardEngine()
    joins0 = len(engine.joins)
    acts = [a.upper() for a in actions]
    bad = [a for a in acts if a not in ("HOLD", "ADD", "REDUCE", "EXIT")]
    if bad:
        raise ValueError(f"invalid actions {bad}")

    gx = tape.venue_change_index()
    if refuse_cross_graduation and gx is not None and tape.tt[gx] > t_dec_ms:
        return {"status": "refused_cross_graduation", "net_sol_returned": None,
                "mint": tape.mint, "graduation_fill_index": gx,
                "graduation_ts": int(tape.tt[gx])}

    continuation = entry is not None
    if continuation:
        miss = [k for k in ("entry_px", "t_entry_ms", "qty_tokens", "cash_sol",
                            "capital_sol") if k not in entry]
        if miss:
            return {"status": "bad_entry_dict", "net_sol_returned": None,
                    "mint": tape.mint, "missing": miss}
        px0 = float(entry["entry_px"])
        t_entry = int(entry["t_entry_ms"])
        qty0 = float(entry["qty_tokens"])
        cash0 = float(entry["cash_sol"])
        capital_sol = float(entry["capital_sol"])
        if not (np.isfinite(px0) and px0 > 0 and np.isfinite(qty0) and qty0 >= 0
                and np.isfinite(cash0) and cash0 >= -1e-12
                and np.isfinite(capital_sol) and capital_sol > 0):
            return {"status": "bad_entry_dict", "net_sol_returned": None, "mint": tape.mint,
                    "reason": "non-finite or out-of-range held state"}
        # No cost-basis bound here: after an ADD the basis (qty*entry_px, with px0
        # the ORIGINAL entry price) legitimately grows past the account while the
        # position itself is still fully funded. The guards that matter are the
        # entry-fill/price match below (catches a wrong basis or a unit mix-up) and
        # the in-loop equity/cash/qty finiteness checks.
        if qty0 * px0 > capital_sol * 1_000_000:
            return {"status": "bad_entry_dict", "net_sol_returned": None, "mint": tape.mint,
                    "reason": "cost basis implausible against account capital",
                    "capital_sol": capital_sol, "qty_tokens": qty0, "entry_px": px0}
        je = int(np.searchsorted(tape.tt, t_entry, side="left"))
        if je >= tape.tt.size or int(tape.tt[je]) != t_entry:
            return {"status": "entry_fill_not_found", "net_sol_returned": None,
                    "mint": tape.mint, "t_entry_ms": t_entry}
        px_tape = float(tape.px[je])
        if not np.isfinite(px_tape) or px_tape <= 0:
            return {"status": "bad_entry_price", "net_sol_returned": None, "mint": tape.mint}
        if abs(px_tape - px0) > 1e-4 * px0:
            # 1e-4 relative: the archived/exported entry price is printed to as few as
            # 5 significant digits, so a value that agrees to 1e-4 is the same fill.
            # The failure this guard exists for (a held position priced off a different
            # basis, or a SOL/lamport mix-up) is off by >=1%.
            return {"status": "entry_px_mismatch", "net_sol_returned": None, "mint": tape.mint,
                    "entry_px": px0, "tape_px": px_tape, "t_entry_ms": t_entry}
        j0 = je
        deploy_sol = qty0 * px0
        end = min(t_dec_ms + horizon_ms, int(tape.tt[-1]))
        if end <= t_dec_ms:
            return {"status": "no_horizon", "net_sol_returned": None, "mint": tape.mint}
        # tick 0 == the management decision itself
        ticks = np.arange(t_dec_ms, end + 1, interval_ms, dtype=np.int64)
        npri = np.searchsorted(tape.tt, ticks, side="right") - 1
        valid = (npri >= 0) & (npri >= j0)
        ticks, npri = ticks[valid], npri[valid]
        if ticks.size == 0:
            return {"status": "no_ticks", "net_sol_returned": None, "mint": tape.mint}
        if ticks.size > max_ticks:
            sel = (np.arange(max_ticks) * (ticks.size / max_ticks)).astype(int)
            ticks, npri = ticks[sel], npri[sel]
        qty = qty0
        cash = cash0
        entry_fee = {}
    else:
        j0 = tape.first_fill_after(t_dec_ms, t_dec_ms, "entry")
        if j0 is None:
            return {"status": "no_entry", "net_sol_returned": None, "mint": tape.mint}
        t_entry = int(tape.tt[j0])
        px0 = float(tape.px[j0])
        if not np.isfinite(px0) or px0 <= 0:
            return {"status": "bad_entry_price", "net_sol_returned": None, "mint": tape.mint}

        end = min(t_entry + horizon_ms, int(tape.tt[-1]))
        if end <= t_entry:
            return {"status": "no_horizon", "net_sol_returned": None, "mint": tape.mint}

        # decision ticks after entry, guarded to have a real fill within 60s prior
        ticks = np.arange(t_entry + interval_ms, end + 1, interval_ms, dtype=np.int64)
        npri = np.searchsorted(tape.tt, ticks, side="right") - 1
        valid = npri >= j0
        ticks, npri = ticks[valid], npri[valid]
        if ticks.size == 0:
            return {"status": "no_ticks", "net_sol_returned": None, "mint": tape.mint}
        if ticks.size > max_ticks:
            sel = (np.arange(max_ticks) * (ticks.size / max_ticks)).astype(int)
            ticks, npri = ticks[sel], npri[sel]

        qty = deploy_sol / px0
        cash = capital_sol - deploy_sol
        try:
            entry_fee = engine.quote_buy(tape, t_entry, t_entry, deploy_sol) or {}
        except ReserveUnavailable as e:
            return {"status": "refused_no_reserves", "net_sol_returned": None,
                    "mint": tape.mint, "t_dec_ms": int(t_dec_ms), "regime": engine.regime.value,
                    "reason": str(e)[:300], "reserve_joins": engine.joins[joins0:]}
        qty0, cash0 = qty, cash
    legs = []
    costs = {"curve_fee_sol": 0.0, "slippage_sol": 0.0, "creator_fee_sol": 0.0,
             "priority_sol": 0.0}
    checks = 0
    violations = []
    # Risk terms (RL reward) are computed from the marked-to-market equity path,
    # so it is recorded causally: each point is the equity at a decision tick that
    # the episode actually consulted, plus the terminal mark.
    # A held-position decision is judged on the risk it takes FROM HERE. The
    # (t_entry, capital) seed point is the FRESH-entry accounting origin; keeping it
    # on a management episode charges every candidate the drawdown already suffered
    # before the decision (a large, common, history-only term that dominated the
    # reward scale) and makes the forward window look shorter than it is.
    equity_path = [] if continuation else [(t_entry, float(capital_sol))]
    max_consulted = int(tape.tt[j0])

    def chk(tag, cond):
        nonlocal checks
        checks += 1
        if not cond:
            violations.append(tag)

    for k in range(ticks.size):
        tM, i = int(ticks[k]), int(npri[k])
        if i < j0 or (tM - t_entry) < min_hold_ms:
            continue
        cur = tape.price_at_or_before(int(tape.tt[i]), tM, "mgmt_price")
        if cur is None:
            continue
        max_consulted = max(max_consulted, int(tape.tt[i]))
        act = acts[k] if k < len(acts) else "HOLD"
        executed = act
        if act == "ADD":
            # ADD is an ACCOUNT decision: deploy a fraction of the account's
            # capital, capped by the cash actually on hand. The old rule sized off
            # the existing position only, which made ADD a silent no-op (and hence
            # a tie with HOLD) whenever cash < 0.5*position notional.
            spend = min(max(cash, 0.0), add_fraction * capital_sol)
            if spend > 1e-12:
                q = engine.quote_buy(tape, int(tape.tt[i]), tM, spend)
                if q is not None:
                    cash -= spend
                    qty += q["tokens"]
                    costs["curve_fee_sol"] += q["fee_sol"]
                    costs["slippage_sol"] += q["slippage_sol"]
                    costs["creator_fee_sol"] += q["creator_sol"]
                    costs["priority_sol"] += q["priority_sol"]
                    legs.append(("ADD", tM, spend, q))
                else:
                    executed = "HOLD"
            else:
                executed = "HOLD"
        elif act == "REDUCE":
            amt = 0.5 * qty
            q = engine.quote_sell(tape, int(tape.tt[i]), tM, amt)
            if q is not None:
                cash += q["sol_out"]
                qty -= amt
                costs["curve_fee_sol"] += q["fee_sol"]
                costs["slippage_sol"] += q["slippage_sol"]
                costs["creator_fee_sol"] += q["creator_sol"]
                costs["priority_sol"] += q["priority_sol"]
                legs.append(("REDUCE", tM, q["sol_out"], q))
        elif act == "EXIT":
            q = engine.quote_sell(tape, int(tape.tt[i]), tM, qty)
            if q is not None:
                cash += q["sol_out"]
                costs["curve_fee_sol"] += q["fee_sol"]
                costs["slippage_sol"] += q["slippage_sol"]
                costs["creator_fee_sol"] += q["creator_sol"]
                costs["priority_sol"] += q["priority_sol"]
                legs.append(("EXIT", tM, q["sol_out"], q))
                qty = 0.0
        # -- mirrored replay conservation invariants ---------------------
        equity = cash + qty * cur
        chk("equity_finite", bool(np.isfinite(equity)))
        chk("equity_nonneg", equity >= -1e-9)
        chk("cash_nonneg", cash >= -1e-9)
        chk("qty_nonneg", qty >= -1e-12)
        if np.isfinite(equity):
            equity_path.append((tM, float(equity)))
        if qty <= 1e-12 and not continuation:
            # (fresh path) nothing left to trade: the episode is over.
            # A HELD-POSITION candidate does NOT end here: the accounting window is
            # fixed by the decision, not by the action, so EXIT/REDUCE-to-flat keeps
            # accruing flat equity to the window end. Otherwise EXIT would buy a
            # free ride on the -lambda_time*time_frac term (-0.05 of capital over
            # the pinned 1800 s horizon) purely by shortening its own episode,
            # which flipped the management argmax toward EXIT.
            break

    # terminal mark (causal: last fill at/before the decision ticks)
    jm = min(int(npri[-1]), tape.tt.size - 1)
    mark = float(tape.px[jm]) if np.isfinite(tape.px[jm]) else px0
    engine_tape_mark_ts = int(tape.tt[jm])
    if engine_tape_mark_ts > int(tape.tt[-1]):
        raise LookaheadError("terminal mark beyond tape")
    final_equity = cash + qty * mark
    net = final_equity - capital_sol
    equity_path.append((engine_tape_mark_ts, float(final_equity)))
    chk("final_equity_identity", abs((cash + qty * mark) - final_equity) < 1e-9)
    last_decision_ts = int(ticks[-1]) if ticks.size else t_entry
    chk("no_lookahead", max_consulted <= max(last_decision_ts, t_entry))
    # -- reserve-join provenance + causality (mechanics regimes) -------------
    joins = engine.joins[joins0:]
    chk("reserves_never_future_dated",
        all(j["reserve_ts_ms"] <= j["decision_ts_ms"] for j in joins))
    chk("reserves_within_stale_bound",
        all(j["staleness_ms"] <= engine.max_reserve_stale_ms for j in joins))

    # -- episode-level conservation -----------------------------------------
    spent = sum(l[2] for l in legs if l[0] == "ADD")
    recvd = sum(l[2] for l in legs if l[0] in ("REDUCE", "EXIT"))
    if continuation:
        # At a MID-POSITION decision the position's basis (qty x ORIGINAL entry px)
        # can legitimately exceed the account once adds have happened at higher
        # prices, so there is no invariant to assert here beyond finiteness, which
        # the in-loop checks already cover.
        recon_cash = (cash0) - spent + recvd
    else:
        chk("capital_init", abs((capital_sol - deploy_sol) + (deploy_sol / px0) * px0 - capital_sol) < 1e-9)
        recon_cash = (capital_sol - deploy_sol) - spent + recvd
    chk("cash_ledger_reconcile", abs(recon_cash - cash) < 1e-9)
    # lamport discipline: every SOL figure must round-trip to integer lamports
    for tag, val in (("final_equity", final_equity), ("net_sol", net)):
        lam = to_lamports_checked(val, tag)
        chk(f"lamport_roundtrip_{tag}", abs(lam / LAMPORTS_PER_SOL - val) <= 5e-10)
    for l in legs:
        chk("lamport_leg_roundtrip", abs(to_lamports_checked(l[2]) / LAMPORTS_PER_SOL - l[2]) < 1e-9)
    # cost itemisation: each leg's modelled curve fee must equal rate x its notional
    # cost_within_bounds is a UNIT/SCALE guard, not a physics bound: the modelled cost
    # of a sale can legitimately approach the full notional when the position is larger
    # than the pool (constant product pays out ~D, so slippage ~ notional - D). The half-
    # notional form it used to have tripped on a real 60x position sold into a 5 SOL
    # pool. The bound exists to catch a unit mix-up, and 1.0 x notional does that while
    # admitting every trade the model can actually produce.
    # traded notional for the cost sanity bound: for a held position the position's
    # value is marked at the DECISION price (not the entry basis), so an appreciated
    # position cannot make the bound look tighter than the trade actually was.
    marked = (qty0 * max(px0, float(tape.px[int(npri[0])])) if continuation else 0.0)
    notional = max(1e-12, deploy_sol + spent, marked)
    for l in legs:
        q = l[3]
        base = (l[2] if l[0] == "ADD" else q.get("gross_sol", l[2]))
        rate_leg = engine._leg_fee_rate(tape, l[1])
        chk("fee_matches_rate", abs(q["fee_sol"] - base * rate_leg) <= 1e-9 + 1e-9 * abs(base))
    chk("cost_nonneg", all(v >= -1e-15 for v in costs.values()))
    chk("cost_within_bounds", 0.0 <= sum(costs.values()) <= 1.0 * notional)
    total_cost = sum(costs.values())
    res = {
        "status": "ok",
        "mint": tape.mint,
        "t_dec_ms": int(t_dec_ms),
        "t_entry_ms": t_entry,
        "entry_px": px0,
        "entry_mode": "continuation" if continuation else "fresh",
        "deploy_sol": float(deploy_sol),
        "qty_at_decision": float(qty0),
        "cash_at_decision": float(cash0),
        "held_ms_at_decision": int(t_dec_ms) - t_entry,
        "depth_sol": engine._depth(tape),
        "actions": acts,
        "legs": [{"action": l[0], "t_ms": l[1], "sol": l[2]} for l in legs],
        "final_equity_sol": final_equity,
        "net_sol_returned": net,
        "cash_sol": cash,
        "qty_tokens": qty,
        "costs_sol": {k: round(v, 12) for k, v in costs.items()},
        "total_cost_sol": total_cost,
        "cost_pct_of_notional": 100.0 * total_cost / notional,
        "fills": len(legs),
        "conservation_checks": checks,
        "violations": violations,
        "max_consulted_ts": max_consulted,
        "last_decision_ts": int(ticks[-1]) if ticks.size else None,
        "lookahead_ms": max(0, max_consulted - (int(ticks[-1]) if ticks.size else max_consulted)),
        # -- which reserves priced this episode, and how stale they were ------
        "regime": engine.regime.value,
        "reserves_used_count": len(joins),
        "reserve_joins": joins,
        "max_staleness_ms": (max((j["staleness_ms"] for j in joins), default=None)),
        # Marked-to-market equity at every consulted decision tick + terminal mark.
        # Input to the RL risk terms (reward_terms.reward_scalar); causal by construction.
        "equity_path": [[int(t), float(v)] for t, v in equity_path],
    }
    return res


# ---------------------------------------------------------------------------
# 7. DECISIVE VALIDATION - replay recorded trades
# ---------------------------------------------------------------------------
def reconcile_recorded(pooltest_path="/training/v2/canonical/renorm_pooltest/trades.jsonl",
                       max_mints=None, min_trades=2, sample=None):
    """Replay recorded (mint,trader) episodes; compare simulated vs recorded net SOL.

    Identity basis  : engine fed each fill's recorded execution price -> must be
                      ~exact (validates units/accounting; catches 1000x lamport bugs).
    Predictive basis: engine predicts each fill's price from the PRIOR tape only
                      (last realized price + constant-product impact + measured
                      fee) -> error here is genuine model error.
    """
    import collections
    rows = [json.loads(l) for l in open(pooltest_path)]
    grp = collections.defaultdict(list)
    for r in rows:
        grp[(r["mint"], r["trader"])].append(r)
    eps = [(k, v) for k, v in grp.items() if len(v) >= min_trades]
    eps.sort(key=lambda kv: (kv[0][0], kv[0][1]))
    if sample:
        eps = eps[:sample]
    if max_mints:
        eps = eps[:max_mints]
    eng = RewardEngine()
    id_err, pr_err, rec, sim = [], [], [], []
    n_fills = 0
    for (mint, trader), tr in eps:
        tr.sort(key=lambda r: r["recv_unix_ms"])
        # recorded net SOL for this trader on this mint
        recorded_net = to_sol_checked(sum(int(r["sol_lamports"]) for r in tr), "recorded_net")
        rec.append(recorded_net)
        # identity: rebuild by pricing at each fill's OWN recorded average price
        cash = 0.0
        for r in tr:
            sol = to_sol_checked(r["sol_lamports"], "sol")
            cash += sol
            n_fills += 1
        id_err.append(abs(cash - recorded_net))
        sim.append(cash)
    id_err = np.array(id_err)
    rec = np.array(rec)
    sim = np.array(sim)
    corr = float(np.corrcoef(rec, sim)[0, 1]) if rec.size > 1 else float("nan")
    return {"episodes_replayed": len(eps), "fills_replayed": n_fills,
            "identity_basis": "each fill priced at its own recorded execution price",
            "identity_mean_abs_err_sol": float(id_err.mean()) if id_err.size else None,
            "identity_max_abs_err_sol": float(id_err.max()) if id_err.size else None,
            "corr_recorded_vs_sim": corr,
            "note": ("identity basis reproduces recorded net by construction and "
                     "validates units/accounting only; a PREDICTIVE replay needs "
                     "per-fill curve reserves, which canonical v7 does not carry.")}


def reconcile_predictive(path="/training/v2/canonical/renorm_pooltest/trades.jsonl",
                         sample_mints=None, min_fills=2, reserves=None):
    """DECISIVE VALIDATION - replay recorded (mint,trader) episodes.

    For every episode we know the ACTUAL executed buys/sells and their realized
    SOL flows. The engine is given only each fill's SIDE + recorded SIZE, and the
    tape strictly BEFORE that fill; it must price the fill itself (last realized
    execution price + constant-product impact for the size + measured curve fee).
    We then compare episode-level simulated net SOL to recorded net SOL.

    `reserves` (optional): a v2/reserves.ReserveTable. When supplied, a fill whose
    (signature, mint, side) is present is priced from the RECONSTRUCTED BONDING
    CURVE - the curve spot price immediately BEFORE that fill
    (pre_virtual_sol_reserves / pre_virtual_token_reserves) - instead of the last
    realized tape price. Fills absent from the table fall back to the tape path.
    Passing reserves=None reproduces the historical baseline exactly.

    Returns episode count, mean/max absolute SOL error, and the correlation.
    """
    import collections
    rows = [json.loads(l) for l in open(path)]
    bymint = collections.defaultdict(list)
    for r in rows:
        bymint[r["mint"]].append(r)
    mints = sorted(bymint)
    if sample_mints:
        mints = mints[:sample_mints]
    eng = RewardEngine()
    rec_net, sim_net, per_fill_err = [], [], []
    fills = 0
    priced_from_curve = 0
    priced_from_tape = 0
    skipped_multi_venue = 0
    for m in mints:
        tr = sorted(bymint[m], key=lambda r: (r["recv_unix_ms"], r["signature"]))
        if any(r["venue"] != tr[0]["venue"] for r in tr):
            skipped_multi_venue += 1
            continue
        # group into (mint,trader) episodes
        bytr = collections.defaultdict(list)
        for r in tr:
            bytr[r["trader"]].append(r)
        px_hist = []
        for r in tr:
            if r["tokens_raw"]:
                px_hist.append((r["recv_unix_ms"], abs(r["sol_lamports"]) / abs(r["tokens_raw"])))
        px_hist.sort()
        tt = [x[0] for x in px_hist]
        for trader, ep in bytr.items():
            if len(ep) < min_fills:
                continue
            ep = sorted(ep, key=lambda r: r["recv_unix_ms"])
            recorded = to_sol_checked(sum(int(r["sol_lamports"]) for r in ep), "rec")
            simulated = 0.0
            local_err = []
            for r in ep:
                t = r["recv_unix_ms"]
                p = None
                if reserves is not None:
                    pc = reserves.price_pre_lamports_per_raw_token(
                        r.get("signature"), r["mint"], r["side"] == "buy")
                    if pc is not None and np.isfinite(pc) and pc > 0:
                        p = float(pc)
                        priced_from_curve += 1
                if p is None:
                    # causal predicted price = last realized price STRICTLY before
                    j = int(np.searchsorted(tt, t, side="left")) - 1
                    if j < 0:
                        continue
                    p = px_hist[j][1]
                    priced_from_tape += 1
                if not np.isfinite(p) or p <= 0:
                    continue
                tok = abs(r["tokens_raw"])
                notional = abs(r["sol_lamports"]) / LAMPORTS_PER_SOL
                # tok (raw tokens) x p (lamports per raw token) = LAMPORTS; convert
                # through the checked path so a SOL/lamport mix-up cannot slip past.
                pred_lamports = int(round(tok * p * (1.0 - eng.fee_rate)))
                pred = to_sol_checked(pred_lamports, "pred")
                sign = -1.0 if r["side"] == "buy" else 1.0
                simulated += sign * pred
                local_err.append(abs(pred - notional))
                fills += 1
            if not local_err:
                continue
            rec_net.append(recorded)
            sim_net.append(simulated)
            per_fill_err.append(float(np.mean(local_err)))
    rec_net = np.array(rec_net); sim_net = np.array(sim_net)
    err = np.abs(sim_net - rec_net)
    corr = float(np.corrcoef(rec_net, sim_net)[0, 1]) if rec_net.size > 1 else float("nan")
    return {"episodes_replayed": int(rec_net.size), "fills_replayed": fills,
            "mints_multi_venue_refused": skipped_multi_venue,
            "fills_priced_from_curve_reserves": priced_from_curve,
            "fills_priced_from_tape": priced_from_tape,
            "reserves_supplied": reserves is not None,
            "mean_abs_err_sol": float(err.mean()) if err.size else None,
            "max_abs_err_sol": float(err.max()) if err.size else None,
            "mean_abs_err_sol_per_fill": float(np.mean(per_fill_err)) if per_fill_err else None,
            "p90_abs_err_sol": float(np.percentile(err, 90)) if err.size else None,
            "corr_recorded_vs_sim": corr,
            "fee_rate_used": eng.fee_rate,
            "note": ("engine prices each fill from the tape strictly before it "
                     "(prior realized execution price + measured curve fee); no "
                     "reserve levels exist in the data (see module docstring)."
                     if reserves is None else
                     "engine prices each fill from the RECONSTRUCTED BONDING "
                     "CURVE state immediately before it (pre_virtual_sol / "
                     "pre_virtual_token) where the (signature, mint, side) key is "
                     "present in the v2/reserves table; remaining fills fall back "
                     "to the prior-tape path.")}


# ---------------------------------------------------------------------------
# Conservation re-run over the C3 examination holdout
# ---------------------------------------------------------------------------
SWEEP = ["ADD", "HOLD", "ADD", "HOLD", "REDUCE", "HOLD", "REDUCE", "EXIT"]


def _exam_seeds():
    """Reuse the frozen exam's seed definition so the engine is checked on the
    exact same holdout episodes the bar is measured on."""
    import importlib.util
    p = os.path.join(os.path.dirname(os.path.abspath(__file__)), "economic_exam_c3.py")
    spec = importlib.util.spec_from_file_location("_exam_c3", p)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod.load_exam_seeds(mod.load_exam_episode_ids())


def conserve_run(max_episodes=None):
    seeds = _exam_seeds()
    mints = sorted({s[0] for s in seeds})
    tapes = load_canonical_tapes(mints=mints)
    eng = RewardEngine()
    checks = 0
    violations = []
    acts = {}
    ok = refused = no_entry = 0
    statuses = {}
    for mint, split, t_dec, eid in seeds[:max_episodes] if max_episodes else seeds:
        tp = tapes.get(mint)
        if tp is None:
            statuses["no_tape"] = statuses.get("no_tape", 0) + 1
            continue
        n = 8
        actions = (SWEEP * 2)[:n]
        try:
            r = score_action_sequence(tp, t_dec, actions, engine=eng)
        except LookaheadError as e:
            violations.append({"episode": eid, "error": str(e)})
            continue
        statuses[r["status"]] = statuses.get(r["status"], 0) + 1
        if r["status"] == "ok":
            ok += 1
            checks += r["conservation_checks"]
            violations.extend([{"episode": eid, "check": v} for v in r["violations"]])
            for l in r["legs"]:
                acts[l["action"]] = acts.get(l["action"], 0) + 1
        elif r["status"] == "refused_cross_graduation":
            refused += 1
    return {"episodes_available": len(seeds), "episodes_scored": ok,
            "episodes_refused_cross_graduation": refused,
            "conservation_checks": checks,
            "violations": len(violations),
            "violation_examples": violations[:5],
            "actions": dict(sorted(acts.items())),
            "statuses": dict(sorted(statuses.items()))}


# ---------------------------------------------------------------------------
def _hash(obj):
    return hashlib.sha256(json.dumps(obj, sort_keys=True, default=str).encode()).hexdigest()


def selftest():
    out = {}
    # determinism: same input -> identical hash across two computing passes
    tapes = load_canonical_tapes(mints=None, max_rows=None)
    # pick a deterministic sample of mints
    ks = sorted(tapes.keys())[:40]
    acts = ["HOLD", "ADD", "HOLD", "REDUCE", "HOLD", "EXIT"]
    def run():
        res = []
        for m in ks:
            tp = tapes[m]
            if tp.tt.size < 10:
                continue
            t0 = int(tp.tt[tp.tt.size // 2])
            r = score_action_sequence(tp, t0, acts)
            res.append({"m": m, "net": r.get("net_sol_returned"), "st": r["status"],
                        "ck": r.get("conservation_checks"),
                        "status": r["status"],
                        "lookahead_ms": r.get("lookahead_ms"),
                        "reserve_joins": len(r.get("reserve_joins") or []),
                        "max_staleness_ms": r.get("max_staleness_ms")})
        return res
    a = run(); b = run()
    out["determinism_hash_a"] = _hash(a)
    out["determinism_hash_b"] = _hash(b)
    out["determinism_ok"] = _hash(a) == _hash(b)
    # lookahead guard must FAIL LOUDLY (pick a tape whose last fill is genuinely
    # after the mid fill, so the negative control is not degenerate)
    fire = None
    for m in ks:
        tp = tapes[m]
        if tp.tt.size < 20:
            continue
        t_mid = int(tp.tt[tp.tt.size // 2])
        if int(tp.tt[-1]) > t_mid:
            try:
                tp._guard(tp.tt.size - 1, t_mid, "selftest")
                fire = (False, None)
            except LookaheadError as e:
                fire = (True, str(e)[:200])
            break
    out["lookahead_guard_fired"] = bool(fire and fire[0])
    out["lookahead_guard_msg"] = fire[1] if fire else "no suitable tape"
    # every scored episode must report zero lookahead (max consulted ts <= decision)
    lk = [r.get("lookahead_ms") for r in a if r.get("status") == "ok"]
    out["max_lookahead_ms_observed"] = max([x for x in lk if x is not None], default=None)
    out["scored_episodes"] = len(lk)
    out["lookahead_violations"] = sum(1 for x in lk if x and x > 0)
    # unit guard must FAIL LOUDLY
    try:
        to_sol_checked(1.5, "selftest")
        out["unit_guard_fired"] = False
    except UnitError as e:
        out["unit_guard_fired"] = True
        out["unit_guard_msg"] = str(e)[:120]
    out["mints_available"] = len(tapes)
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--selftest", action="store_true")
    ap.add_argument("--reconcile", action="store_true")
    ap.add_argument("--conserve", action="store_true")
    ap.add_argument("--measure-depth", action="store_true")
    ap.add_argument("--reserves", nargs="*", default=None,
                    help="reconstructed curve-reserves parquet files/dirs "
                         "(v2/reserves). With --reconcile, fills whose "
                         "(signature,mint,side) key is present are priced off the "
                         "real bonding curve instead of the prior-tape price.")
    ap.add_argument("--reserves-slinky", default=None, metavar="DIR",
                    help="slinky_gold_v3_compact-derived curve reserves dir "
                         "(v2/reserves/slinky_v1). Bound to the replayed tape by a "
                         "STRICT CAUSAL TIME JOIN (slinky event_time <= fill time); "
                         "tried before --reserves. If the slinky window does not "
                         "overlap the tape window the binding yields 0 fills and the "
                         "engine falls back to --reserves / prior-tape, which is "
                         "reported in reconcile_predictive.reserves_slinky_stats.")
    ap.add_argument("--out")
    # ---- regime selector (mechanics-first). Default keeps history unchanged ----
    ap.add_argument("--regime", default="legacy_tape",
                    choices=["legacy_tape", "bonding_curve", "amm"],
                    help="Pricing regime. legacy_tape (default) reproduces the "
                         "historical tape path exactly. bonding_curve / amm price "
                         "every fill from DECISION-TIME reserves (curve virtual "
                         "reserves / pumpswap pool reserves) with our own order "
                         "size; fills with no reserves are REFUSED, never "
                         "tape-priced, unless --allow-tape-fallback is explicit.")
    ap.add_argument("--amm-lp-fee", type=float, default=AMM_LP_FEE_RATE,
                    help="pumpswap LP fee that moves the pool (default 20 bp)")
    ap.add_argument("--amm-extraction-fee", type=float, default=AMM_TRADER_EXTRACTION_RATE,
                    help="pumpswap protocol+coin-creator fee the trader also pays "
                         "(default 10 bp, does not move the pool)")
    ap.add_argument("--max-reserve-stale-ms", type=int, default=60_000)
    ap.add_argument("--allow-tape-fallback", action="store_true",
                    help="explicitly permit tape pricing when reserves are absent "
                         "(non-mechanics; the report says so)")
    ap.add_argument("--reserves-pool", default=None, metavar="GLOB",
                    help="pumpswap pool-reserve parquet glob written by the "
                         "forward reserve capture (AMM oracle)")
    ap.add_argument("--reconcile-mechanics", action="store_true",
                    help="A/B replay: price every fill from decision-time reserves "
                         "under --regime instead of from the tape")
    a = ap.parse_args()
    res = {}
    if a.selftest:
        res["selftest"] = selftest()
    if a.conserve:
        res["conserve"] = conserve_run()
    if a.measure_depth:
        tapes = load_canonical_tapes()
        ds = [measure_depth(t) for t in tapes.values()]
        ds = np.array([d for d in ds if d is not None], float)
        res["depth"] = {"mints": int(ds.size), "p10_sol": float(np.percentile(ds, 10)),
                        "p50_sol": float(np.percentile(ds, 50)),
                        "p90_sol": float(np.percentile(ds, 90))}
    if a.reconcile:
        sys.path.insert(0, "/training/v2/code/src/v2/reserves")
        tables = []
        slinky_stats = None
        if a.reserves_slinky:
            from slinky_reserves_loader import SlinkyFillReserves
            st = SlinkyFillReserves(
                "/training/v2/canonical/renorm_pooltest/trades.jsonl", a.reserves_slinky)
            slinky_stats = st.stats()
            slinky_stats["table_rows"] = len(st)
            tables.append(st)
        if a.reserves:
            from reserves_loader import ReserveTable
            tables.append(ReserveTable(a.reserves))
        rtab = None
        if len(tables) == 1:
            rtab = tables[0]
        elif len(tables) > 1:
            from slinky_reserves_loader import ChainReserveTable
            rtab = ChainReserveTable(tables)
        res["reconcile_identity"] = reconcile_recorded()
        res["reconcile_predictive"] = reconcile_predictive(reserves=rtab)
        if slinky_stats is not None:
            res["reconcile_predictive"]["reserves_slinky_stats"] = slinky_stats
        if isinstance(rtab, object) and hasattr(rtab, "stats") and not slinky_stats:
            res["reconcile_predictive"]["reserves_chain_stats"] = rtab.stats()
    if a.reconcile_mechanics or a.regime != "legacy_tape":
        import regime_pricing as RP
        reg = RP.Regime(a.regime if a.regime != "legacy_tape" else "bonding_curve")
        mech = {"regime": reg.value, "max_reserve_stale_ms": a.max_reserve_stale_ms,
                "require_reserves": not a.allow_tape_fallback,
                "tape_fallback_allowed": bool(a.allow_tape_fallback),
                "amm_lp_fee_rate": a.amm_lp_fee,
                "amm_extraction_fee_rate": a.amm_extraction_fee}
        oracle = RP.DictReserveOracle(default_source="cli")
        n_states = 0
        if a.reserves:
            try:
                o = RP.build_curve_oracle(list(a.reserves))
                for lst in o._series.values():
                    for st in lst:
                        oracle.add(st)
                n_states += getattr(o, "loaded_rows", 0)
            except Exception as e:
                mech["curve_oracle_error"] = str(e)[:200]
        if a.reserves_slinky:
            o2 = RP.slinky_curve_oracle(a.reserves_slinky)
            for lst in o2._series.values():
                for st in lst:
                    oracle.add(st)
                n_states += 1
        if a.reserves_pool:
            o3 = RP.pool_reserve_oracle(a.reserves_pool)
            for lst in o3._series.values():
                for st in lst:
                    oracle.add(st)
                n_states += len(lst)
        mech["reserve_states"] = n_states or None
        if not n_states and not a.allow_tape_fallback:
            raise SystemExit(
                f"--regime {reg.value} requires reserves and none were supplied "
                f"(--reserves / --reserves-slinky / --reserves-pool). Refusing to "
                f"fall back to a tape price; pass --allow-tape-fallback to opt in.")
        params = RP.amm_params(lp_fee_rate=a.amm_lp_fee,
                               trader_extraction_rate=a.amm_extraction_fee) \
            if reg is RP.Regime.AMM else RP.bonding_params(fee_rate=0.009463)
        for strat in ("curve", "all"):
            mech["%s_%s" % (reg.value, strat)] = RP.reconcile_mechanics(
                "/training/v2/canonical/renorm_pooltest/trades.jsonl", oracle,
                regime=reg, max_stale_ms=a.max_reserve_stale_ms, params=params,
                stratum=strat)
        res["mechanics_reconcile"] = mech
    print(json.dumps(res, indent=1, default=str))
    if a.out:
        json.dump(res, open(a.out, "w"), indent=1, default=str)


if __name__ == "__main__":
    main()
