#!/usr/bin/env python
"""rl_reward_v3 — the FINAL reward engine: reserve-priced, provenance-carrying,
entry/exit-split, with on-chain edge-finding targets.

WHAT THIS CLOSES (each defect was reproduced, not hypothesised):

  D1  NO LEGACY PRICING IN PRODUCTION. The engine core was built and validated
      against real reserves (k-drift p50 1.65e-5) but ZERO production call sites
      selected a mechanics regime: exit_policy.py:147, reward_engine.py:445,
      grpo_reward_bridge.py:93/:133, joint_entry_exit.py:126 all constructed
      RewardEngine() -> Regime.LEGACY_TAPE. Every RL reward to date was priced
      off the tape ratio. V3RefusesToPrice() makes legacy unreachable here.

  D2  PER-DECISION REGIME, not per-engine. The corpus contains mints that
      GRADUATE mid-episode (curve -> pumpswap pool). A single engine-wide regime
      is therefore wrong by construction for those episodes. Regime is resolved
      at each decision instant from the tape venue + reserve availability.

  D3  STALENESS != COVERAGE. The 60 s wall-clock cap was the wrong instrument:
      pump.fun curve state only changes when the mint TRADES, so the newest
      state at-or-before t_dec is EXACT no matter how quiet the mint has been.
      Wall-clock age only proxies a capture gap. V3 defaults to an ANY budget,
      always RECORDS the age, and treats "no state at all" as the only gap.
      Rationale is in reserve_join_coverage.py's own docstring.

  D4  ENTRY AND EXIT ARE NOT ONE REWARD. RewardConfig with
      lambda_drawdown=lambda_downside=1.0 -- calibrated for exits -- drags
      0.82-1.12 against a PnL of -0.12 on fresh entries (6-9x the PnL) and
      flips 12.4pp of positive entries negative. Every V3 score returns the
      (pure, penalised) PAIR, never one alone.

  D5  BUY -> ["EXIT"] graded entry as an instant flip. V3 always pairs an entry
      with a real exit policy and a minimum hold.

EDGE-FINDING (the point of the reward). The measured structure is two-sided:
26% of decisions reach >2x forward and ARE profitable (+0.148 of capital,
P(win)=34.6%), 74% are a crater mass at -0.213, netting -0.119. So the engine
scores, per decision, the objects that decision can actually act on:

  * fm_vw    notional-weighted forward max. A notional-blind max(px) is set by a
             single dust print (tape load only requires >=1e5 lamports) and peak
             DWELL does not rule that out -- a run of dust prints satisfies any
             dwell test. This was the blocking measurement artifact.
  * rho      capture ratio (1+pure)/fm_vw, reported per money bucket, because
             count-weighted it is 0.50 while the fm>5x bucket is 0.061.
  * pi*      the breakeven selection precision: pi* = -mu_norun/(mu_run-mu_norun).
             One scalar replacing "find an entry edge": measured 0.590 at a
             0.260 base rate (2.27x lift) under the best baseline policy.
  * rank     a bounded pairwise ranking target, because at Hill alpha=1.26 the
             sample mean is not estimable (SE ~ n^-0.206; ~2.4k mints for 20%)
             while ranking is bounded per pair and therefore learnable.

Run:
  python rl_reward_v3.py --selftest
  python rl_reward_v3.py --probe --mints 400            # real reserves, real tape
  python rl_reward_v3.py --probe --mints 400 --json out.json
"""
from __future__ import annotations

import argparse
import glob
import json
import math
import os
import statistics
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from regime_pricing import (  # noqa: E402
    DictReserveOracle, Regime, ReserveState, ReserveUnavailable, ReserveStale,
    ReserveLookahead, LookaheadError, RegimeMismatch, OrderTooLarge,
    build_curve_oracle, pool_reserve_oracle, slinky_curve_oracle,
    LAMPORTS_PER_SOL,
)
from reward_engine import (  # noqa: E402
    RewardEngine, Tape, load_canonical_tapes, measure_depth,
    to_lamports_checked, to_sol_checked,
)
from reward_terms import RewardConfig, episode_risk, reward_scalar  # noqa: E402
from exit_policy import TEMPLATES, simulate_policy, causal_state  # noqa: E402
from exit_mechanics import (DEPLOY_SOL_CANONICAL, TERMINAL_WRITE_TO_ZERO,
                           exit_proceeds_lamports, impairment_bps,
                           terminal_value_lamports)  # noqa: E402
# The M6 decision->fill drift: MEASURED (reports/FILL_DRIFT_C15.json) and charged on
# the entry fill. Imported by name so the readiness gate can prove the reward reads
# the artifact at call time rather than a literal in this file.
from fill_drift import (charge_entry_fill, entry_drift_bps,  # noqa: E402
                        entry_drift_cost_sol, fill_drift_source)

# The ACCOUNT, not the position. Sizing is only a decision if capital is finite
# and shared across decisions: with capital == deploy (the old accounting) the
# reward is invariant to size and no size can ever be learned.
SIZE_TIERS = (0.25, 0.5, 1.0)          # fractions of the account notional
FEE_BUFFER_FRAC = 0.01                 # free cash left to pay the priority fee
ACCOUNT_CAPITAL_SOL = DEPLOY_SOL_CANONICAL * (1.0 + FEE_BUFFER_FRAC)

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------
CURVE_HINTS = ("pumpfun_bonding", "bonding", "curve", "pumpfun")
AMM_HINTS = ("pumpswap", "amm", "pool", "raydium", "meteora")

# Staleness policy. ANY = the state at-or-before t_dec is exact for a market
# that only changes when it trades; the only real gap is no state at all.
STALE_ANY = None

# Reward configs. ENTRY gets a much smaller risk penalty than exit: a fresh
# entry has maximum time-in-market and maximum drawdown exposure BY
# CONSTRUCTION, so the exit-calibrated weights measure the act of entering
# rather than the quality of the entry.
ENTRY_LAMBDAS = {"lambda_drawdown": 0.10, "lambda_downside": 0.10, "lambda_time": 0.02,
                 # Size term. DERIVED, not chosen (reports/LAMBDA_KELLY_DERIVATION.json):
                 # lambda = E[r^2] / (2*A^2) is the second-order coefficient of the
                 # log-wealth (Kelly) objective, so the exposure penalty IS the Kelly
                 # risk term to second order. Measured on the 81,416 priced barrier
                 # outcomes of candidate_sft_c9_r2: E[r^2]=0.181252, A=1.01 ->
                 # lambda = 0.08884. The previously pinned 0.20 was 2.25x too punitive
                 # and traced to no measurement.
                 # The SFT size labels in c9_r2 were produced at THIS lambda, so the
                 # RL reward and the corpus supervision now share one risk aversion.
                 # Consequence, reported not hidden: at 0.08884 the Kelly-optimal size
                 # is FULL for any read with expected return > 1.5*lambda = 13.3% per
                 # unit notional, and the grid's BUY rows measure +17.2% mean, so most
                 # BUYs size FULL. The gradation lives in the 26 causal strata, whose
                 # measured expectancy spans +4% .. +28%.
                 "lambda_exposure": 0.08884}
EXIT_LAMBDAS = {"lambda_drawdown": 1.00, "lambda_downside": 1.00, "lambda_time": 0.05}
PURE_LAMBDAS = {"lambda_drawdown": 0.00, "lambda_downside": 0.00, "lambda_time": 0.00}

# Canonical reserve sources (the set that achieves the measured 2,184,596 rows /
# 34,532 mints, i.e. train coverage 0.9955 at an ANY budget).
CURVE_ALL = ["/training/v2/reserves/s0823r/s0823r_all.parquet",
             "/training/v2/reserves/s0824/s0824_all.parquet",
             "/training/v2/reserves/s0909a/s0909a_all.parquet",
             "/training/v2/reserves/s0909b/s0909b_all.parquet"]
# NOTE: this file is a 30-SECOND probe capture (window 1787549743000 ->
# 1787550044000) taken hours BEFORE the forward wall. It covers 819 mints the
# AMM backfill does not, so it is KEPT for that marginal coverage - but it does
# NOT cover the wall window and must never be cited as "the forward capture".
AMM_GLOB = "/training/v2/reserves/forward/*.parquet"
TAPES = "/training/v2/canonical/renorm_corpus_mints/trades.jsonl"
CORPUS = "/training/v2/candidate_sft_c6/train.jsonl"

HORIZON_MS_DEFAULT = 1_800_000
MIN_HOLD_MS = 60_000

# --- Exit impairment + terminal loss: the two costs the reward used to ignore -----
#
# TRAIN ON THE MEASURED ADVERSE QUANTILE, GATE ON THE STRESS LEVEL (see
# exit_mechanics.impairment_bps). The evaluation/shadow runner sets
# RL_IMPAIRMENT_LEVEL=pessimistic so a policy cannot be accepted on the friendlier
# level it trained under.
IMPAIRMENT_LEVEL = os.environ.get("RL_IMPAIRMENT_LEVEL", "realistic")
# An unexitable position is valued by a PREDECLARED rule, never at the displayed mark.
TERMINAL_LOSS_POLICY = os.environ.get("RL_TERMINAL_LOSS_POLICY", TERMINAL_WRITE_TO_ZERO)

# --- M6 decision->fill drift ------------------------------------------------------
# The prompt states a price at the DECISION clock; the fill lands on the next
# executable print. Charged on the entry (fewer tokens for the same notional), from
# the FROZEN measurement - reports/FILL_DRIFT_C15.json via fill_drift. Default ON:
# a cost that is measured and frozen is only real if the reward actually charges it,
# and the readiness gate's [M] class refuses a run whose episode lacks the label.
# `fill_drift=False` (or RL_FILL_DRIFT=0) is the exactness path the self-test uses:
# it must reproduce the pre-change arithmetic bit for bit.
FILL_DRIFT_LEVEL = os.environ.get("RL_FILL_DRIFT_LEVEL", "realistic")
FILL_DRIFT_ENABLED = os.environ.get("RL_FILL_DRIFT", "1").strip().lower() not in (
    "0", "false", "off", "no")
LAMPORTS_PER_SOL_F = 1_000_000_000.0
# The immutable forward wall (8 h) and the corpus partitions. RL trains only
# below wall_ms and never on a wall episode; the wall IS the RL evaluation set.
RL_WALL_JSON = "/training/v2/reports/FORWARD_WALL_V1.json"
SKIP_VALUE = 0.0                # value of holding no position: the number to beat
# The RL corpus MUST be the corpus SFT-009 is trained on: grpo_dataset's contract is
# that the RL prompt is byte-identical to the SFT prompt so the ref-logprob cache keys
# match. c9_r2 adds the ENRICHED CANDIDATE STATE / DEV HISTORY block and the rewritten
# SIZE lines, so building RL prompts from c6/c8 would put the policy off-distribution
# from its own SFT data (train != serve). Verified: c9_r2 train has ZERO overlap with
# the 11,825 wall episodes, so the whole 67,045-row split is legitimately trainable.
# The RL entry arm must be built from the SAME corpus the SFT entry family ships. This
# pointed at c9_r2 - the PRE-repair family, which still carried the 20.7% target/meta
# contradiction (17,553 rows) and the retired 210 bp cost statements. Building RL from it
# would have re-introduced exactly the train != serve split this work removes.
CORPUS_PARTS = ["/training/v2/candidate_sft_c12/train.jsonl",
                "/training/v2/candidate_sft_c12/validation.jsonl",
                "/training/v2/candidate_sft_c12/examination.jsonl"]
DEFAULT_NOTIONAL_FLOOR_LAMPORTS = 1_000_000     # 0.001 SOL: below this a print
                                                # is not a tradeable price

# Violations that mean a QUOTE or LEDGER came back wrong, not that the trade was
# bad. Such an episode is refused and counted, never averaged in.
FATAL_VIOLATIONS = frozenset({
    "step_return_sane", "equity_finite", "equity_positive_while_held",
    "cash_ledger_reconcile", "qty_fully_exited", "no_lookahead",
})


class V3RefusesToPrice(ReserveUnavailable):
    """Raised when no regime can price a fill from reserves. Never degrade.

    Subclasses ReserveUnavailable on purpose: every existing `except ReserveError`
    / `except ReserveUnavailable` handler in the RL path already treats this as a
    refusal, so a coverage gap can never escape as an unhandled crash and can
    never be silently coerced into a zero return.
    """


# ---------------------------------------------------------------------------
# Registry: one oracle holding every reserve series we have
# ---------------------------------------------------------------------------
# --- AMM reserve backfill (Helius) -------------------------------------------------
# 2.81M pump-swap reserve events over 766 mints. MERGED into the AMM oracle only
# through the AUTHORITATIVE WSOL pool filter below: an event whose `pool` is a
# USDC/USDT-quoted pool carries a 6dp quote_reserve, and reading that as lamports
# (9dp) is the 1000x class. Non-authoritative events are DROPPED, never re-read.
AMM_BACKFILL = os.environ.get(
    "RL_AMM_BACKFILL", "/training/v2/reserves/amm_history_v1/amm_reserves.jsonl")
AMM_POOL_MAP = os.environ.get(
    "RL_AMM_POOL_MAP", "/training/v2/reports/AMM_POOLS_RESOLVED_V2.jsonl")


# On-chain pump.fun CURVE reserves acquired for the forward-wall window. The curve
# capture stream ENDS ~25 min before the wall starts, so without this the
# never-graduated mints (74% of wall decisions) can only be priced from
# pre-window snapshots. See acquire_curve_reserves.py.
CURVE_WALL_BACKFILL = os.environ.get(
    "RL_CURVE_WALL_BACKFILL", "/training/v2/reserves/curve_wall_v1/curve_reserves.jsonl")


def curve_backfill_oracle(path: str = CURVE_WALL_BACKFILL,
                          source: str = "helius_curve_backfill", mints=None,
                          verbose: bool = False):
    """DictReserveOracle over the on-chain CURVE backfill (the wall window).

    Uses the VIRTUAL reserves, matching the capture-fed curve oracles, so the
    reserve provenance the wall is scored on is the same KIND the corpus
    annotation uses (train == serve). Lookup is at-or-before t_dec, so a later
    state is never visible to an earlier decision."""
    if not os.path.exists(path):
        if verbose:
            print(json.dumps({"curve_backfill": {"status": "absent", "path": path}}),
                  flush=True)
        return None
    o = DictReserveOracle(default_source=source)
    kept = skipped = 0
    mints_kept = set()
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                e = json.loads(line)
            except Exception:
                skipped += 1
                continue
            if mints is not None and e.get("mint") not in mints:
                continue
            vs, vt = e.get("virtual_sol"), e.get("virtual_token")
            if vs is None or vt is None or int(vs) <= 0 or int(vt) <= 0:
                skipped += 1
                continue
            o.add(ReserveState(regime=Regime.BONDING_CURVE, mint=e["mint"],
                               sol_lamports=int(vs), token_raw=int(vt),
                               ts_unix_ms=int(e["recv_unix_ms"]), venue="pumpfun",
                               account=None, source=source))
            kept += 1
            mints_kept.add(e["mint"])
    o.loaded_rows = kept
    if verbose:
        print(json.dumps({"curve_backfill": {"rows": kept, "skipped": skipped,
                                             "mints": len(mints_kept),
                                             "path": path}}), flush=True)
    return o


def amm_backfill_oracle(path: str = AMM_BACKFILL, pool_map_path: str = AMM_POOL_MAP,
                        source: str = "helius_amm_backfill"):
    """DictReserveOracle over the backfill, restricted to authoritative WSOL pools.

    Same attribution rule the SFT annotation uses, so RL prices and SFT prompts
    are derived from one pool map (train == serve)."""
    wsol: dict = {}
    with open(pool_map_path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            r = json.loads(line)
            ws = list(r.get("wsol_pools") or [])
            if ws:
                wsol[r["mint"]] = set(ws)
    o = DictReserveOracle(default_source=source)
    kept = drop_pool = drop_mint = drop_bad = 0
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            e = json.loads(line)
            m, pool = e.get("mint"), e.get("pool")
            auth = wsol.get(m)
            if not auth:
                drop_mint += 1
                continue
            if pool not in auth:
                drop_pool += 1
                continue
            br, qr, ts = e.get("base_reserve"), e.get("quote_reserve"), e.get("recv_unix_ms")
            if br is None or qr is None or ts is None or br <= 0 or qr <= 0:
                drop_bad += 1
                continue
            o.add(ReserveState(regime=Regime.AMM, mint=m, sol_lamports=int(qr),
                               token_raw=int(br), ts_unix_ms=int(ts),
                               venue="pumpswap", account=pool, source=source))
            kept += 1
    o.loaded_rows = kept
    o.audit = {"kept": kept, "mints_with_wsol_pool": len(wsol),
               "dropped_non_wsol_pool": drop_pool,
               "dropped_mint_without_wsol_pool": drop_mint,
               "dropped_bad_values": drop_bad}
    return o


class ReserveRegistry:
    """Merged BONDING_CURVE + AMM reserve series, with load accounting.

    `DictReserveOracle` keys on (mint, regime), so a single oracle can carry a
    mint's curve history AND its post-graduation pool history. That is what
    makes the per-decision regime resolution in V3Engine possible.
    """

    def __init__(self, oracle: DictReserveOracle, stats: dict):
        self.oracle = oracle
        self.stats = stats

    @staticmethod
    def merge(*oracles):
        out = DictReserveOracle(default_source="v3:merged")
        stats = {}
        for o in oracles:
            if o is None:
                continue
            for key, lst in o._series.items():          # noqa: SLF001 (intentional)
                for st in lst:
                    out.add(st)
                stats[f"{key[1].value}"] = stats.get(f"{key[1].value}", 0) + len(lst)
        return out, stats

    @classmethod
    def build(cls, curve_paths=None, slinky_dirs=None, amm_glob=AMM_GLOB,
              mints=None, verbose=False) -> "ReserveRegistry":
        """Load every reserve source that exists. Missing sources are skipped
        LOUDLY (in stats), never silently substituted."""
        built, sources, stats = [], {}, {"curve_rows": 0, "amm_rows": 0,
                                        "mints_curve": set(), "mints_amm": set()}
        paths = CURVE_ALL if curve_paths is None else curve_paths
        have = [p for p in paths if os.path.exists(p)]
        if have:
            co = build_curve_oracle(have)
            built.append(co)
            stats["curve_rows"] += int(getattr(co, "loaded_rows", 0) or 0)
            sources["curve"] = have
            for (m, r) in co._series:                     # noqa: SLF001
                if r is Regime.BONDING_CURVE:
                    stats["mints_curve"].add(m)
        else:
            sources["curve"] = []
        cbo = curve_backfill_oracle(verbose=verbose)
        if cbo is not None:
            built.append(cbo)
            sources["curve_wall_backfill"] = cbo.default_source
            for (m, r) in cbo._series:                    # noqa: SLF001
                if r is Regime.BONDING_CURVE:
                    stats["mints_curve"].add(m)
                    stats["curve_rows"] += len(cbo._series[(m, r)])
        else:
            sources["curve_wall_backfill"] = []
        for d in (slinky_dirs or []):
            if os.path.isdir(d) and glob.glob(os.path.join(d, "*.parquet")):
                so = slinky_curve_oracle(d, mints=mints)
                built.append(so)
                sources.setdefault("slinky", []).append(d)
                for (m, r) in so._series:                 # noqa: SLF001
                    if r is Regime.BONDING_CURVE:
                        stats["mints_curve"].add(m)
        if amm_glob and glob.glob(amm_glob):
            ao = pool_reserve_oracle(amm_glob)
            built.append(ao)
            sources["amm"] = sorted(glob.glob(amm_glob))
            for (m, r) in ao._series:                     # noqa: SLF001
                if r is Regime.AMM:
                    stats["mints_amm"].add(m)
                    stats["amm_rows"] += len(ao._series[(m, r)])
        else:
            sources["amm"] = []
        bf = (os.environ.get("RL_AMM_BACKFILL", AMM_BACKFILL) or "")
        if bf and os.path.exists(bf):
            bo = amm_backfill_oracle(bf)
            if bo.loaded_rows:
                built.append(bo)
                sources["amm_backfill"] = [bf]
                for (m, r) in bo._series:                # noqa: SLF001
                    if r is Regime.AMM:
                        stats["mints_amm"].add(m)
                        stats["amm_rows"] += len(bo._series[(m, r)])
                stats["amm_backfill_audit"] = bo.audit
        else:
            sources["amm_backfill"] = []
        if not built:
            raise V3RefusesToPrice(
                "no reserve source resolved: refusing to construct an engine "
                "that would have to price from the tape")
        merged, counts = cls.merge(*built)
        stats["series_counts"] = counts
        stats["mints_curve"] = len(stats["mints_curve"])
        stats["mints_amm"] = len(stats["mints_amm"])
        stats["sources"] = sources
        if verbose:
            print(json.dumps({"registry": {k: v for k, v in stats.items()
                                           if k != "sources"}}, default=str))
        return cls(merged, stats)

    def has(self, mint: str, regime: Regime, t_ms: int) -> bool:
        """True iff a state of this regime exists AT OR BEFORE t_ms. The
        at-or-before test is the causality guarantee, so a future state never
        counts as coverage."""
        lst = self.oracle._series.get((mint, regime))        # noqa: SLF001
        if not lst:
            return False
        ts = np.asarray([int(s.ts_unix_ms) for s in lst], dtype=np.int64)
        i = int(np.searchsorted(ts, int(t_ms), side="right")) - 1
        return i >= 0

    def regimes_for(self, mint: str, t_ms: int) -> list:
        return [r for r in (Regime.BONDING_CURVE, Regime.AMM) if self.has(mint, r, t_ms)]


# ---------------------------------------------------------------------------
# The engine: per-decision regime, ANY staleness, provenance on every fill
# ---------------------------------------------------------------------------
# Train-window decisions may only be priced from the SAME reserve series the SFT
# prompt states, or the model is graded on a market it never saw (train != serve).
# The prompt cites laserstream (curve) and helius_amm_backfill (AMM); the merged
# registry additionally holds two SUPPLEMENTARY series with a DIFFERENT acquisition
# that lift coverage but can supersede the prompt's stated series. assert_priced
# refuses any episode whose pricing touched one of them.
SUPPLEMENTARY_RESERVE_SOURCES = frozenset({"helius_curve_backfill",
                                           "capture:pumpswap_pool_reserves"})

class V3Engine(RewardEngine):
    """Reserve-priced, per-decision-regime, provenance-carrying engine.

    LEGACY_TAPE is unreachable: `regime` is only ever a mechanics regime and is
    resolved per decision from the tape venue, cross-checked against reserve
    availability. `allow_legacy=True` exists for the reconciliation harness and
    stamps every episode `legacy_priced=True` so it can never be reported as a
    production number.
    """

    UNBOUNDED_MS = 10 ** 15

    def __init__(self, registry: ReserveRegistry, *,
                 max_reserve_stale_ms=STALE_ANY, allow_legacy: bool = False,
                 **kw):
        kw.setdefault("oracle", registry.oracle)
        kw.setdefault("regime", Regime.BONDING_CURVE)   # replaced per decision
        kw.setdefault("require_reserves", True)
        # RewardEngine coerces its budget with int(), so an "any" policy is
        # carried as an explicit huge bound while the POLICY is kept separately
        # and reported as None (= unbounded) in provenance.
        policy = max_reserve_stale_ms
        kw["max_reserve_stale_ms"] = (self.UNBOUNDED_MS if policy is None
                                      else int(policy))
        super().__init__(**kw)
        self.registry = registry
        self.stale_policy = policy
        self.max_reserve_stale_ms = kw["max_reserve_stale_ms"]
        self.allow_legacy = bool(allow_legacy)
        self.legacy_episodes = 0
        self.refusals_by_reason = {}
        self.regime_decisions = {"bonding_curve": 0, "amm": 0, "legacy_tape": 0}

    # -- regime resolution (D2) --------------------------------------------
    def tape_venue_at(self, tape: Tape, t_ms: int):
        i = int(np.searchsorted(tape.tt, int(t_ms), side="right")) - 1
        if i < 0:
            return None, None
        v = tape.venue[i] if hasattr(tape, "venue") else None
        return (str(v).lower() if v is not None else None), int(tape.tt[i])

    @staticmethod
    def _regime_of_venue(v):
        if v is None:
            return None
        if any(h in v for h in AMM_HINTS):
            return Regime.AMM
        if any(h in v for h in CURVE_HINTS):
            return Regime.BONDING_CURVE
        return None

    def resolve_regime(self, tape: Tape, t_ms: int) -> Regime:
        """Resolve the regime from what the tape says the market IS, then require
        reserves for exactly that market.

        THE BUG THIS FIXES (reproduced 2026-09-13): the first version of this
        method cross-checked graduation with `tape.venue_change_index()`. That
        returns None when a tape is ENTIRELY one venue - which is the normal case
        for a mint that graduated before our tape window starts. With gx=None the
        guard never fired and a pumpswap-venue mint was silently priced against
        BONDING_CURVE reserves. Measured on
        BHnsBYhzNrsEowdAt2QQ7Z39bi4qNhKS54xDBvyApump: tape px 19.72 vs joined
        curve price 4.18e-4 (a factor of ~47,000), so the position was sized off
        one price and filled at another, and every sell was refused as
        OrderTooLarge.

        A venue that names a market we hold no reserves for is a COVERAGE GAP.
        It refuses. It never falls back to the other market.
        """
        venue, _ = self.tape_venue_at(tape, t_ms)
        want = self._regime_of_venue(venue)
        avail = self.registry.regimes_for(tape.mint, t_ms)
        if want is not None:
            if want in avail:
                return want
            raise V3RefusesToPrice(
                f"{tape.mint}@{t_ms}: tape venue={venue!r} -> {want.value} but no "
                f"{want.value} reserves at or before t_dec "
                f"(held: {[r.value for r in avail] or 'none'}) - refusing rather "
                f"than pricing the wrong market")
        if len(avail) == 1:
            return avail[0]
        if len(avail) > 1:
            # ambiguous venue label, both markets held: the later state wins,
            # because graduation is monotone in time for a pump.fun mint
            return Regime.AMM
        raise V3RefusesToPrice(
            f"{tape.mint}@{t_ms}: no curve or pool reserves at or before t_dec")

    def _bind_regime(self, tape, t_ms):
        if self.allow_legacy:
            self.legacy_episodes += 1
            self.regime_decisions["legacy_tape"] += 1
            self.regime = Regime.LEGACY_TAPE
            return Regime.LEGACY_TAPE
        r = self.resolve_regime(tape, t_ms)
        self.regime = r
        self.regime_decisions[r.value] += 1
        return r

    def _budget(self, regime) -> int:
        """Per-regime override honoured if the policy is a dict; None (= any)
        becomes the unbounded sentinel."""
        b = self.stale_policy
        if isinstance(b, dict):
            b = b.get(regime.value, None)
        return self.UNBOUNDED_MS if b is None else int(b)

    # -- quote overrides: resolve regime first, then delegate --------------
    def quote_buy(self, tape, t_ms, decision_ms, q_sol):
        self._bind_regime(tape, decision_ms)
        self.max_reserve_stale_ms = self._budget(self.regime)
        try:
            out = super().quote_buy(tape, t_ms, decision_ms, q_sol)
        except (ReserveUnavailable, ReserveStale, ReserveLookahead,
                RegimeMismatch, OrderTooLarge) as e:
            k = type(e).__name__
            self.refusals_by_reason[k] = self.refusals_by_reason.get(k, 0) + 1
            raise
        if out is not None:
            out["regime"] = self.regime.value
        return out

    def quote_sell(self, tape, t_ms, decision_ms, tokens):
        self._bind_regime(tape, decision_ms)
        self.max_reserve_stale_ms = self._budget(self.regime)
        try:
            out = super().quote_sell(tape, t_ms, decision_ms, tokens)
        except (ReserveUnavailable, ReserveStale, ReserveLookahead,
                RegimeMismatch, OrderTooLarge) as e:
            k = type(e).__name__
            self.refusals_by_reason[k] = self.refusals_by_reason.get(k, 0) + 1
            raise
        if out is not None:
            out["regime"] = self.regime.value
        return out

    # -- provenance (D3) ---------------------------------------------------
    def provenance(self, *, legacy_priced: bool = False) -> dict:
        rep = self.episode_reserve_report()
        joins = rep["reserve_joins"]
        return {
            "regime": rep["regime"],
            "legacy_priced": bool(legacy_priced or self.allow_legacy),
            "reserve_joins_count": rep["reserve_joins_count"],
            "staleness_ms_max": rep["max_staleness_ms"],
            "staleness_ms_p50": (statistics.median([j["staleness_ms"] for j in joins])
                                 if joins else None),
            "lookahead_joins": rep["lookahead_joins"],
            "stale_budget_ms": self.max_reserve_stale_ms,
            "reserve_sources": sorted({j["source"] for j in joins}),
            "reserve_refusals": rep["reserve_refusals"],
            "refusals_by_reason": dict(self.refusals_by_reason),
        }

    def assert_priced(self, prov: dict):
        """Hard gate: no episode may be scored without a declared, non-legacy,
        lookahead-free pricing provenance."""
        if prov["legacy_priced"]:
            raise V3RefusesToPrice("episode was priced off the tape (legacy)")
        if prov["reserve_joins_count"] <= 0:
            raise V3RefusesToPrice("episode has zero reserve joins: unpriced")
        if prov["lookahead_joins"]:
            raise V3RefusesToPrice(
                f"{prov['lookahead_joins']} reserve join(s) post-date their own "
                f"decision - lookahead violation")
        bad_src = sorted(set(prov.get("reserve_sources", [])) & SUPPLEMENTARY_RESERVE_SOURCES)
        if bad_src:
            raise V3RefusesToPrice(
                f"reserve source(s) {bad_src} are a different acquisition than the "
                f"SFT prompt's stated series (train != serve)")
        return True


# ---------------------------------------------------------------------------
# Reward: the (pure, penalised) pair, entry and exit split (D4)
# ---------------------------------------------------------------------------
def _cfg(l: dict) -> RewardConfig:
    """Pinned reward config from a lambda dict.

    CVaR TAIL TERM (2026-09-21): `lambda_cvar`/`cvar_alpha` are read with defaults of
    0.0/0.05, so NO existing pinned number moves. Enabling the tail term is an explicit
    pin change - add "lambda_cvar": <x> to the relevant LAMBDAS dict in this file - and
    it is a RISK-PREFERENCE decision (operator's call), not a tuning knob.

    Read the reward_terms module docstring first: the per-episode form is a no-op
    under GRPO normalisation. What lambda_cvar actually drives is (A) tail
    amplification of the counterfactual returns and (C) a per-group loss multiplier,
    both in grpo_reward_bridge._tail_adjust / reward_terms.

    alpha 0.05 is FORCED by the acceptance gate (mint-clustered 95% LB), not chosen.
    """
    return RewardConfig(lambda_drawdown=l["lambda_drawdown"],
                        lambda_downside=l["lambda_downside"],
                        lambda_time=l["lambda_time"],
                        lambda_cvar=l.get("lambda_cvar", 0.0),
                        cvar_alpha=l.get("cvar_alpha", 0.05),
                        ruin_penalty=1.0, horizon_ms=HORIZON_MS_DEFAULT)


PURE_CFG, ENTRY_CFG, EXIT_CFG = _cfg(PURE_LAMBDAS), _cfg(ENTRY_LAMBDAS), _cfg(EXIT_LAMBDAS)


def entry_drift_for_pricing(level: str | None = None) -> dict:
    """The frozen decision->fill drift this module will charge, as a dict.

    Exists as its own call so the readiness gate can PROVE the reward reads the
    artifact: [M] replaces this module's `entry_drift_bps` with a sentinel and
    requires the sentinel to come back. A hardcoded magnitude in simulate_v3 would
    survive that substitution, and a hardcoded magnitude is unre-derivable from the
    tape and therefore un-recalibratable.

    The charge is deliberately NOT applied here as a hard clamp at the decision
    price. That alternative was rejected on two measurements: clamping the SIZED
    quantity re-introduces the deploy/tape-price sizing error that self-test 13
    pins (a fake tape whose tape px is 4,108x the reserve price would have filled
    4,108x too small), and clamping only the trigger basis while leaving qty alone
    would raise `stop_lvl` and exit losses EARLIER, which is optimistic. Charging
    the fill and reporting the decision price keeps every number a real quote.
    """
    return entry_drift_bps(level or FILL_DRIFT_LEVEL)


def score_episode_v3(episode: dict, mode: str = "entry") -> dict:
    """Return BOTH rewards for one episode. `mode` picks the penalty weights

    Never returns a single-sided number: a penalised entry reward without its
    pure twin is a statement about lambda, not about the market.
    """
    if episode.get("status") != "ok":
        return {"status": episode.get("status"), "refused": True,
                "pure": None, "penalised": None, "reason": episode.get("reason")}
    cfg = ENTRY_CFG if mode == "entry" else EXIT_CFG
    pen = _scalar(episode, cfg)
    pure = _scalar(episode, PURE_CFG)
    return {"status": "ok", "refused": False,
            "pure": pure["reward"], "penalised": pen["reward"],
            "penalty_drag": pen["reward"] - pure["reward"],
            "components": pen["components"], "risk": pen["risk"],
            "mode": mode}


def counterfactual_stats(group: dict, eps: float = 1e-6):
    """Leave-one-out advantages over an explicit {action: value} group.

    Advantage_i = v_i - mean(v_j, j != i). Identities: the advantages sum to zero,
    and the best arm always has the largest advantage. A group with fewer than 2
    scored arms is NOT usable (it is masked downstream, never zero-filled).
    """
    items = [(k, float(v)) for k, v in group.items() if v is not None]
    if len(items) < 2:
        return {}, {"usable": False, "n_arms": len(items), "best_action": None}
    vals = np.asarray([v for _, v in items], dtype=float)
    loo = (vals.sum() - vals) / (len(items) - 1)
    adv = vals - loo
    return ({k: float(a) for (k, _), a in zip(items, adv)},
            {"usable": True, "n_arms": len(items),
             "best_action": items[int(np.argmax(vals))][0],
             "zero_sum": bool(abs(float(adv.sum())) < 1e-9)})


def _scalar(episode: dict, cfg: RewardConfig) -> dict:
    """Delegate to reward_terms.reward_scalar so the V3 numbers cannot diverge
    from the canonical definition by a re-implementation."""
    return reward_scalar(episode, cfg)


# ---------------------------------------------------------------------------
# Scoring: entry always paired with a real exit policy (D5)
# ---------------------------------------------------------------------------
def _tape_price_at(tape, t_ms, decision_ms=None):
    """The prompt's own price at `t_ms`, in lamports per raw token, or None.

    Causal by construction on a real Tape (price_at_or_before guards lookahead); the
    fallback index path exists only so the self-test stand-ins work unchanged.
    """
    dm = int(t_ms if decision_ms is None else decision_ms)
    fn = getattr(tape, "price_at_or_before", None)
    if fn is not None:
        try:
            v = fn(int(t_ms), dm)
            return float(v) if v is not None else None
        except Exception:
            return None
    i = int(np.searchsorted(tape.tt, int(t_ms), side="right")) - 1
    return float(tape.px[i]) if i >= 0 else None


def simulate_v3(tape: Tape, t_dec_ms: int, engine: "V3Engine", *,
                policy_name: str = "MOONSHOT_TAIL",
                capital_sol: float | None = None, deploy_sol: float = 1.0,
                horizon_ms: int | None = None,
                add_fraction: float = 0.0,
                fill_drift: bool | None = None) -> dict:
    """Entry + exit on ONE episode, reserve-priced end to end.

    WHY THIS EXISTS (reproduced defect, 2026-09-13): exit_policy.simulate_policy
    sizes the position as `qty = deploy_sol / px0` where px0 comes from the TAPE
    price, then fills the buy from RESERVES. Under legacy tape pricing those
    coincide by construction, so the bug is invisible. Under reserve pricing they
    are different numbers and the episode is incoherent: measured on a
    pumpswap-venue mint, tape px 19.72 vs joined curve price 4.18e-4, so the
    position was ~47,000x too small and every sell was refused as OrderTooLarge
    (753 of 1,058 decisions in the first reserve-priced probe, 78%).

    Two invariants this enforces:
      I1  qty comes from the FILL (quote_buy token_delta_raw), never from a tape
          price divided into the notional.
      I2  every mark-to-market and every trigger comparison uses an EXECUTABLE
          engine price (a sell quote at that instant), never tape.px. Mixing the
          two is a unit error, and a unit error here is a 1e9-class mistake.
    """
    pol = TEMPLATES[policy_name]
    horizon = int(horizon_ms if horizon_ms is not None else pol.horizon_ms)
    # CAPITAL IS THE ACCOUNT, NOT THE POSITION. `deploy_sol` is the SIZE of the
    # bet (a tier of the account). The buffer exists because a book with zero free
    # cash cannot pay the priority fee, which would push cash marginally negative
    # and poison the equity path's returns.
    capital_sol = (ACCOUNT_CAPITAL_SOL if capital_sol is None
                   else float(capital_sol))
    if float(deploy_sol) > capital_sol + 1e-12:
        return {"status": "refused_size_exceeds_account", "refused": True,
                "net_sol_returned": None, "pure": None, "penalised": None,
                "mint": tape.mint, "t_dec_ms": int(t_dec_ms),
                "reason": f"deploy_sol {deploy_sol} > account {capital_sol}"}

    j0 = tape.first_fill_after(t_dec_ms, t_dec_ms, "entry")
    if j0 is None:
        return {"status": "no_entry", "net_sol_returned": None, "mint": tape.mint}
    t_entry = int(tape.tt[j0])

    try:
        qb = engine.quote_buy(tape, t_entry, t_dec_ms, deploy_sol)
    except (V3RefusesToPrice, ReserveUnavailable, ReserveStale, ReserveLookahead,
            RegimeMismatch, OrderTooLarge) as e:
        return {"status": "refused_no_reserves", "net_sol_returned": None,
                "mint": tape.mint, "t_dec_ms": int(t_dec_ms),
                "regime": getattr(engine, "regime", None) and engine.regime.value,
                "reason": f"{type(e).__name__}: {str(e)[:200]}"}
    if not qb or not qb.get("tokens") or qb["tokens"] <= 0:
        return {"status": "no_fill", "net_sol_returned": None, "mint": tape.mint,
                "t_dec_ms": int(t_dec_ms)}

    qty = float(qb["tokens"])                       # I1: the real fill
    prio = float(qb.get("priority_sol", 0.0))
    cash = capital_sol - deploy_sol - prio
    entry_eff_raw = deploy_sol / qty                # effective price, engine units
    # M6: the MEASURED decision->fill drift (reports/FILL_DRIFT_C15.json, read through
    # fill_drift) is charged HERE, on the tokens received: the same notional buys fewer
    # tokens, so the entry basis worsens by exactly the frozen bound and every
    # downstream mark stays a real quote. `entry_drift_for_pricing` records the clamp
    # alternative that was rejected and the measurements that rejected it.
    use_drift = FILL_DRIFT_ENABLED if fill_drift is None else bool(fill_drift)
    drift = entry_drift_for_pricing() if use_drift else {
        "entry_slippage_bps": 0, "source": "disabled", "level": FILL_DRIFT_LEVEL,
        "quantile": None, "population": None}
    drift_bp = int(drift.get("entry_slippage_bps") or 0)
    if drift_bp > 0:
        qty = charge_entry_fill(qty, drift_bp)
    qty_init = qty
    entry_eff = deploy_sol / qty                    # effective price, engine units
    # The price the PROMPT stated at the decision clock. Units differ from entry_eff
    # (prompt: lamports per raw token; entry_eff: SOL per raw token), so the gap is
    # converted once, here, and reported - never compared across units.
    dec_px = _tape_price_at(tape, t_dec_ms)
    entry_gap_vs_decision_bp = None
    if dec_px and dec_px > 0 and entry_eff > 0:
        entry_gap_vs_decision_bp = float((entry_eff / (dec_px / LAMPORTS_PER_SOL_F)
                                          - 1.0) * 1e4)
    legs = [("BUY", t_entry, deploy_sol, qb)]
    costs = {"fee_sol": float(qb.get("fee_sol", 0.0)),
             "slippage_sol": float(qb.get("slippage_sol", 0.0)),
             "priority_sol": prio,
             # The two costs the fee-only model left out. Separate buckets, because a
             # diagnostic must be able to say WHICH one ate the trade.
             "impairment_sol": 0.0,
             "terminal_loss_sol": 0.0,
             # The measured entry drift, as its own bucket for the same reason.
             "fill_drift_sol": entry_drift_cost_sol(deploy_sol, drift_bp)}

    end = min(t_entry + horizon, int(tape.tt[-1]))
    if end <= t_entry:
        return {"status": "no_horizon", "net_sol_returned": None, "mint": tape.mint}
    ticks = np.arange(t_entry + pol.tick_ms, end + 1, pol.tick_ms, dtype=np.int64)
    npri = np.searchsorted(tape.tt, ticks, side="right") - 1
    keep = npri >= j0
    ticks, npri = ticks[keep], npri[keep]
    if ticks.size == 0:
        return {"status": "no_ticks", "net_sol_returned": None, "mint": tape.mint}
    if ticks.size > pol.max_ticks:
        sel = np.linspace(0, ticks.size - 1, pol.max_ticks).astype(int)
        ticks, npri = ticks[sel], npri[sel]

    checks, violations = 0, []

    def chk(tag, cond):
        nonlocal checks
        checks += 1
        if not cond:
            violations.append(tag)

    equity_path = [(t_entry, float(capital_sol))]
    peak = entry_eff
    trail_armed = bool(pol.trail_distance_bp > 0 and pol.trail_activate_bp <= 0)
    rungs = list(pol.tp_ladder)
    stop_lvl = entry_eff * (1.0 - pol.stop_bp / 1e4) if pol.stop_bp > 0 else None
    exit_reason = "horizon"
    last_tick, last_i = int(tape.tt[int(npri[-1])]), int(npri[-1])
    # The minimum-hold rule is a rule about the DECISION clock. On a sparse tape
    # t_fill can be far behind the tick we decided on, so measuring the hold from
    # the fill timestamp falsely condemned 524 of 1,318 real decisions as
    # "violated_min_hold" (measured 2026-09-13). Execution latency is recorded
    # separately on each leg.
    last_decide = int(ticks[-1]) if ticks.size else int(t_entry)
    retries = 0
    # WHY a sell was refused: a missing RESERVE is a data gap (refuse the episode, it
    # cannot be valued); a capacity refusal is the MARKET failing to absorb us (grade the
    # declared terminal loss). Conflating the two either masks the crash tail or prices a
    # data gap as a total loss.
    last_refusal: dict = {"cause": None}

    def do_sell(t_fill, tM, amount, tag):
        nonlocal cash, qty, retries
        amt = min(float(amount), qty)
        if amt <= 1e-12:
            return False
        try:
            qs = engine.quote_sell(tape, t_fill, tM, amt)
        except OrderTooLarge:
            last_refusal["cause"] = "liquidity"
            retries += 1
            return False
        except (ReserveUnavailable, ReserveStale, ReserveLookahead,
                RegimeMismatch):
            last_refusal["cause"] = "data"
            return False
        if not qs or float(qs.get("sol_out", 0.0)) <= 0:
            last_refusal["cause"] = "liquidity"
            retries += 1
            return False
        last_refusal["cause"] = None
        gross = float(qs["sol_out"])
        first_sell = not any(l[0] not in ("BUY", "ADD") for l in legs)
        # IMPAIRMENT (measured, frozen): a real exit pays more than the quoted mark -
        # the market takes its cut, and every retry costs the move it waited through.
        imp = exit_proceeds_lamports(int(round(gross * LAMPORTS_PER_SOL_F)),
                                     first_sell=first_sell, retries=retries,
                                     level=IMPAIRMENT_LEVEL)
        net = imp["net_lamports"] / LAMPORTS_PER_SOL_F
        costs["impairment_sol"] += gross - net
        p = float(qs.get("priority_sol", 0.0))
        cash += net - p
        qty -= amt
        retries = 0
        costs["fee_sol"] += float(qs.get("fee_sol", 0.0))
        costs["slippage_sol"] += float(qs.get("slippage_sol", 0.0))
        costs["priority_sol"] += p
        legs.append((tag, t_fill, net, qs))
        return True

    for k in range(ticks.size):
        tM, i = int(ticks[k]), int(npri[k])
        if (tM - t_entry) < pol.min_hold_ms:
            continue
        t_fill = int(tape.tt[i])
        max_consulted = t_fill
        # I2: the only price we ever compare against is an executable one
        try:
            qm = engine.quote_sell(tape, t_fill, tM, qty)
        except (ReserveUnavailable, ReserveStale, ReserveLookahead,
                RegimeMismatch, OrderTooLarge):
            continue
        if not qm:
            continue
        # UNIT DISCIPLINE: `px_exec` is a PRICE in lamports per raw token while
        # `sol_out` is in SOL. Marking the position as qty*px_exec therefore
        # valued a ~1 SOL position at 7.7e8 (reproduced, selftest 13). The only
        # unit-consistent mark in SOL per raw token is the realized ratio.
        try:
            sold_ref = float(qty)
            cur = float(qm["sol_out"]) / sold_ref if sold_ref > 0 else 0.0
        except (KeyError, TypeError, ZeroDivisionError):
            continue
        if not np.isfinite(cur) or cur <= 0:
            continue
        peak = max(peak, cur)
        last_tick, last_i = t_fill, i
        last_decide = tM
        if qty <= 1e-12:
            break
        if pol.name == "EXIT_NOW":
            if do_sell(t_fill, tM, qty, "EXIT"):
                exit_reason = "exit_now"
            break
        if stop_lvl is not None and cur <= stop_lvl:
            if do_sell(t_fill, tM, qty, "STOP"):
                exit_reason = "stop"
            break
        if pol.trail_distance_bp > 0 and trail_armed:
            trig = peak * (1.0 - pol.trail_distance_bp / 1e4)
            if cur <= trig:
                if do_sell(t_fill, tM, qty, "TRAIL"):
                    exit_reason = "trail"
                break
        if (pol.trail_activate_bp > 0 and not trail_armed
                and cur >= entry_eff * (1.0 + pol.trail_activate_bp / 1e4)):
            trail_armed = True
        while rungs:
            gain_bp, frac = rungs[0]
            if cur < entry_eff * (1.0 + gain_bp / 1e4):
                break
            amt = min(qty, frac * qty_init)
            if amt > 1e-12:
                if do_sell(t_fill, tM, amt, f"TP{gain_bp:g}"):
                    if pol.move_stop_to_breakeven:
                        stop_lvl = max(stop_lvl or entry_eff, entry_eff)
            rungs.pop(0)
        equity = cash + qty * cur
        chk("equity_finite", bool(np.isfinite(equity)))
        chk("equity_nonneg", equity >= -1e-9)
        chk("cash_nonneg", cash >= -1e-9)
        chk("qty_nonneg", qty >= -1e-12)
        # PHYSICAL BOUND: a spot long's equity cannot move by more than the price
        # move in one tick. An implied step beyond 100x means a quote came back in
        # the wrong units, and such an episode must be REFUSED, never averaged in
        # (measured: a single such episode put the mean penalised reward at -8.5e7
        # across 947 decisions, i.e. every other number was invisible).
        prev = float(equity_path[-1][1]) if equity_path else float(capital_sol)
        step = (equity / prev - 1.0) if prev > 1e-12 else 0.0
        chk("step_return_sane", abs(step) < 100.0)
        chk("equity_positive_while_held", (qty <= 1e-12) or equity > 0.0)
        if np.isfinite(equity):
            equity_path.append((tM, float(equity)))

    # Force the remainder out at the last consulted tick: under reserve pricing
    # there is no unit-consistent "mark" to hold to, so the episode must END in
    # an executed exit. Marking to tape.px here would reintroduce the unit error.
    #
    # A refusal is NOT all one thing:
    #  * a MISSING RESERVE (data gap) still refuses - the position cannot be valued
    #    at all, and the caller masks it rather than grading a fabricated zero.
    #  * a CAPACITY refusal (the market cannot absorb the exit) is a real, gradable
    #    outcome: the remainder is valued by the DECLARED terminal rule, capped at
    #    basis, never at the displayed mark. Masking this case is what let the policy
    #    believe every held position was exitable.
    terminal_loss_applied = False
    terminal_loss_sol = 0.0
    if qty > 1e-12:
        if not do_sell(last_tick, last_tick, qty, "HORIZON"):
            if last_refusal["cause"] != "liquidity":
                return {"status": "unexitable_at_horizon", "net_sol_returned": None,
                        "mint": tape.mint, "t_dec_ms": int(t_dec_ms),
                        "reason": "no executable exit at the horizon bound",
                        "refusal_cause": last_refusal["cause"] or "unknown"}
            basis_lamports = int(round(deploy_sol * (qty / qty_init)
                                       * LAMPORTS_PER_SOL_F)) if qty_init > 0 else 0
            recovered = terminal_value_lamports(basis_lamports, TERMINAL_LOSS_POLICY)
            cash += recovered / LAMPORTS_PER_SOL_F
            terminal_loss_sol = (basis_lamports - recovered) / LAMPORTS_PER_SOL_F
            costs["terminal_loss_sol"] += terminal_loss_sol
            legs.append(("TERMINAL_LOSS", last_tick, recovered / LAMPORTS_PER_SOL_F,
                         {"terminal_loss_policy": TERMINAL_LOSS_POLICY,
                          "basis_lamports": basis_lamports}))
            qty = 0.0
            terminal_loss_applied = True
            exit_reason = "terminal_loss"
        else:
            exit_reason = "horizon" if exit_reason == "horizon" else exit_reason

    final_equity = cash
    net = final_equity - capital_sol
    equity_path.append((int(last_tick), float(final_equity)))
    # Cash conservation, ex ante vs ex post: every leg is a recorded amount and
    # priority fees are the only flow not visible in the leg amounts themselves.
    buys_out = sum(l[2] for l in legs if l[0] in ("BUY", "ADD"))
    sells_in = sum(l[2] for l in legs if l[0] not in ("BUY", "ADD"))
    expected_cash = capital_sol - buys_out + sells_in - costs["priority_sol"]
    chk("cash_ledger_reconcile", abs(expected_cash - final_equity) < 1e-9)
    chk("no_lookahead", int(last_tick) <= int(tape.tt[-1]))
    chk("qty_fully_exited", qty <= 1e-12)
    return {"status": "ok", "mint": tape.mint, "policy": pol.name,
            "t_dec_ms": int(t_dec_ms), "t_entry_ms": t_entry,
            "entry_px": float(entry_eff), "entry_px_tape": float(tape.px[j0]),
            # THE M6 EVIDENCE: which frozen drift graded this entry, the un-charged
            # fill price it moved from, and the gap against the prompt's own
            # decision-clock price (positive = the reward's basis is WORSE than the
            # price the model was shown).
            "entry_px_undrift": float(entry_eff_raw),
            "fill_drift_bps": int(drift_bp),
            "fill_drift_source": drift.get("source"),
            "fill_drift_level": drift.get("level"),
            "fill_drift_sol": float(costs["fill_drift_sol"]),
            "entry_px_decision": dec_px,
            "entry_gap_vs_decision_bp": entry_gap_vs_decision_bp,
            "qty_init": float(qty_init), "qty_tokens_left": float(qty),
            "exit_reason": exit_reason, "exit_ts_ms": int(last_decide),
            "exit_fill_ts_ms": int(last_tick),
            "execution_latency_ms": int(last_decide) - int(last_tick),
            "final_equity_sol": float(final_equity), "net_sol_returned": float(net),
            "deploy_sol": float(deploy_sol), "capital_sol": float(capital_sol),
            "costs_sol": costs, "total_cost_sol": float(sum(costs.values())),
            # THE EXIT-COST EVIDENCE: which impairment level graded this episode, where
            # the values came from, and whether a trapped remainder took the declared
            # terminal loss (never the displayed mark).
            "impairment_level": IMPAIRMENT_LEVEL,
            "impairment_source": impairment_bps(IMPAIRMENT_LEVEL)["source"],
            "terminal_loss_applied": bool(terminal_loss_applied),
            "terminal_loss_sol": float(terminal_loss_sol),
            "terminal_loss_policy": TERMINAL_LOSS_POLICY,
            "equity_path": [[int(t), float(v)] for t, v in equity_path],
            "fatal": sorted(set(violations) & FATAL_VIOLATIONS),
            "legs": [(a, int(t), float(s)) for a, t, s, _q in legs],
            "conservation_checks": checks, "violations": violations,
            "regime": engine.regime.value}


def score_entry(tape: Tape, t_dec_ms: int, engine: V3Engine, *,
                policy_name: str = "MOONSHOT_TAIL", mode: str = "entry",
                horizon_ms: int = HORIZON_MS_DEFAULT, min_hold_ms: int = MIN_HOLD_MS,
                deploy_sol: float = 1.0, capital_sol: float | None = None) -> dict:
    """One entry decision, graded through a real exit policy over a real hold.

    Uses simulate_v3 (correct sizing/marking) rather than the shared
    simulate_policy, which sizes off the tape price. Every refusal is returned
    as a refusal: never coerced to a zero return.
    """
    try:
        ep = simulate_v3(tape, t_dec_ms, engine, policy_name=policy_name,
                         capital_sol=capital_sol, deploy_sol=deploy_sol,
                         horizon_ms=horizon_ms)
    except (V3RefusesToPrice, ReserveUnavailable, ReserveStale, ReserveLookahead,
            RegimeMismatch, OrderTooLarge) as e:
        return {"status": "refused_no_reserves", "refused": True, "pure": None,
                "penalised": None, "mint": tape.mint, "t_dec_ms": int(t_dec_ms),
                "policy": policy_name, "reason": f"{type(e).__name__}: {str(e)[:200]}"}
    if ep.get("status") != "ok":
        return {"status": ep.get("status"), "refused": True, "pure": None,
                "penalised": None, "mint": tape.mint, "t_dec_ms": int(t_dec_ms),
                "policy": policy_name, "reason": ep.get("reason")}
    if ep.get("fatal"):
        return {"status": "refused_implausible_path", "refused": True, "pure": None,
                "penalised": None, "mint": tape.mint, "t_dec_ms": int(t_dec_ms),
                "policy": policy_name, "reason": ",".join(ep["fatal"]),
                "episode": {"fatal": ep["fatal"]}}
    held_ms = int(ep["exit_ts_ms"]) - int(ep["t_entry_ms"])
    if held_ms < min_hold_ms:
        return {"status": "violated_min_hold", "refused": True, "pure": None,
                "penalised": None, "mint": tape.mint, "t_dec_ms": int(t_dec_ms),
                "policy": policy_name, "held_ms": held_ms}
    out = score_episode_v3(ep, mode=mode)
    out.update({"mint": tape.mint, "t_dec_ms": int(t_dec_ms),
                "policy": policy_name, "held_ms": held_ms,
                "t_entry_ms": int(ep["t_entry_ms"]), "entry_px": ep["entry_px"],
                "exit_ts_ms": int(ep["exit_ts_ms"]), "exit_reason": ep.get("exit_reason"),
                "execution_latency_ms": ep.get("execution_latency_ms"),
                "regime": ep.get("regime"), "episode": ep})
    return out


def _with_horizon(pol, horizon_ms):
    import dataclasses
    if dataclasses.is_dataclass(pol):
        return dataclasses.replace(pol, horizon_ms=int(horizon_ms))
    setattr(pol, "horizon_ms", int(horizon_ms))       # noqa: B010
    return pol


def score_exit_continuation(tape: Tape, t_dec_ms: int, engine: V3Engine, *,
                            policy_name: str = "MOONSHOT_TAIL", entry: dict,
                            mode: str = "exit",
                            horizon_ms: int = HORIZON_MS_DEFAULT) -> dict:
    """Mid-position management: score continued management of the ACTUAL fill,
    so a continuation is not silently repriced as a fresh entry (basis
    mismatch)."""
    pol = TEMPLATES[policy_name]
    ep = simulate_policy(tape, t_dec_ms, _with_horizon(pol, horizon_ms),
                         engine=engine, entry=entry, refuse_cross_graduation=False)
    if ep.get("status") != "ok":
        return {"status": ep.get("status"), "refused": True, "pure": None,
                "penalised": None, "mint": tape.mint, "t_dec_ms": int(t_dec_ms),
                "policy": policy_name}
    out = score_episode_v3(ep, mode=mode)
    out.update({"mint": tape.mint, "t_dec_ms": int(t_dec_ms),
                "policy": policy_name, "episode": ep})
    return out


# ---------------------------------------------------------------------------
# EDGE FINDING (on-chain): the objects a decision can act on
# ---------------------------------------------------------------------------
def notional_weighted_forward_max(tape: Tape, t_dec_ms: int, *,
                                  t_entry_ms: int | None = None,
                                  horizon_ms: int = HORIZON_MS_DEFAULT,
                                  floor_lamports: int = DEFAULT_NOTIONAL_FLOOR_LAMPORTS):
    """Notional-weighted forward max multiple from the entry fill.

    A notional-blind max(px) is set by a single dust print. Requiring a minimum
    NOTIONAL per row, and additionally reporting the notional at the peak, is
    what distinguishes a real run from a dust spike. Returns None when the
    window has no qualifying row.

    UNITS (a bug this signature exists to make impossible): the base and the max
    are BOTH taken from tape.px. An earlier version accepted an entry price from
    the caller, and when that price came from the reserve engine (SOL per raw
    token, ~4e-13) while the max came from tape.px (~1e-12), every multiple was
    ~1e3-1e9 and P(fm>2x) came out at 1.00 - an impossible base rate. There is
    no parameter here that can carry a foreign-unit price.
    """
    tt, px, sol = tape.tt, tape.px, tape.sol
    base_ms = int(t_entry_ms if t_entry_ms is not None else t_dec_ms)
    b = int(np.searchsorted(tt, base_ms, side="left"))
    if b >= px.size:
        return None
    px0 = float(px[b])
    i = int(np.searchsorted(tt, int(t_dec_ms), side="left"))
    j = int(np.searchsorted(tt, int(t_dec_ms) + int(horizon_ms), side="right"))
    if j - i < 1 or px0 is None or not np.isfinite(px0) or px0 <= 0:
        return None
    notional = np.abs(sol[i:j].astype(np.float64))
    seg = np.asarray(px[i:j], dtype=np.float64)
    ok = np.isfinite(seg) & (seg > 0) & (notional >= float(floor_lamports))
    if not ok.any():
        return None
    k = int(np.argmax(np.where(ok, seg, -np.inf)))
    return {"fm_vw": float(seg[k] / px0),
            "peak_notional_lamports": float(notional[k]),
            "peak_frac_of_own": float(notional[k] / max(1.0, float(floor_lamports))),
            "n_qualifying": int(ok.sum()),
            "fm_blind": float(np.nanmax(seg) / px0)}

def peak_dwell_s(tape: Tape, t_dec_ms: int, *, horizon_ms: int = HORIZON_MS_DEFAULT,
                 tau: float = 0.25,
                 floor_lamports: int = DEFAULT_NOTIONAL_FLOOR_LAMPORTS):
    """Seconds spent within (1-tau) of the notional-qualified peak (label-side)."""
    tt, px, sol = tape.tt, tape.px, tape.sol
    i = int(np.searchsorted(tt, int(t_dec_ms), side="left"))
    j = int(np.searchsorted(tt, int(t_dec_ms) + int(horizon_ms), side="right"))
    notional = np.abs(sol[i:j].astype(np.float64))
    seg = np.asarray(px[i:j], dtype=np.float64)
    ok = np.isfinite(seg) & (seg > 0) & (notional >= float(floor_lamports))
    if ok.sum() < 2:
        return None
    m = float(seg[ok].max())
    t_ok = tt[i:j][ok]
    inzone = seg[ok] >= (1.0 - tau) * m
    return float((t_ok[inzone].max() - t_ok[inzone].min()) / 1000.0)


def capture_ratio(pure_return: float, fm_vw: float):
    """rho = (1 + pure) / fm_vw. Count-weighted this hides the tail; always
    report it per money bucket."""
    if fm_vw is None or fm_vw <= 0 or pure_return is None:
        return None
    return float((1.0 + pure_return) / fm_vw)


def money_buckets(records: list, run_mult: float = 2.0) -> dict:
    """Split scored decisions by whether the coin actually ran.

    This is the decomposition that inverts the naive reading: entry is
    profitable on the run bucket and the unconditional loss is the crater mass.
    """
    runs = [r for r in records if r.get("fm_vw") is not None and r["fm_vw"] > run_mult]
    nors = [r for r in records if r.get("fm_vw") is not None and r["fm_vw"] <= run_mult]
    out = {"n": len(records), "n_run": len(runs), "n_norun": len(nors),
           "base_rate_run": round(len(runs) / len(records), 4) if records else None}

    def agg(rs):
        v = np.asarray([r["pure"] for r in rs if r.get("pure") is not None], dtype=float)
        if v.size == 0:
            return None
        return {"n": int(v.size), "mean": round(float(v.mean()), 5),
                "median": round(float(np.median(v)), 5),
                "p_positive": round(float((v > 0).mean()), 4),
                "p05": round(float(np.percentile(v, 5)), 5),
                "p95": round(float(np.percentile(v, 95)), 5)}

    out["run"], out["norun"] = agg(runs), agg(nors)
    caps = [r["rho"] for r in runs if r.get("rho") is not None]
    out["capture_p50_run"] = round(float(np.median(caps)), 5) if caps else None
    if out["run"] and out["norun"]:
        mu_r, mu_n = out["run"]["mean"], out["norun"]["mean"]
        if mu_r > mu_n:
            out["pi_star"] = round(-mu_n / (mu_r - mu_n), 4)
            out["required_lift"] = round(out["pi_star"] / out["base_rate_run"], 3)
    return out


def rank_targets(records: list, bucket_key=None) -> list:
    """Bounded pairwise ranking target: within each (lane, horizon) bucket,
    map the pure return to its percentile rank. Bounded per observation, so it
    remains learnable at Hill alpha=1.26 where the mean is not estimable
    (SE ~ n^-0.206; ~2.4k mints for a 20% standard error)."""
    key = bucket_key or (lambda r: (r.get("regime"), r.get("policy")))
    groups = {}
    for r in records:
        if r.get("pure") is None:
            continue
        groups.setdefault(key(r), []).append(r)
    for _, rs in groups.items():
        vals = np.asarray([r["pure"] for r in rs], dtype=float)
        order = vals.argsort().argsort()
        for r, o in zip(rs, order):
            r["rank_target"] = round(float(o) / max(1, len(rs) - 1), 6)
    return records


def ips_weights(strata: list, max_weight: float = 20.0) -> list:
    """Inverse-propensity weights for the DECISION SAMPLER.

    The sampler is part of the policy. If decisions are drawn uniformly, the
    correct action is SKIP ~98% of the time and the entry head collapses to
    always-SKIP; the learned rule then just reproduces the sampler's prior.
    Weighting by 1/P(select t_dec) is what makes the entry advantage a property
    of the market rather than of the sampler.
    """
    n = len(strata)
    if n == 0:
        return []
    counts = {}
    for s in strata:
        counts[s] = counts.get(s, 0) + 1
    w = [min(max_weight, n / counts[s]) for s in strata]
    m = sum(w) / len(w)
    return [x / m for x in w]


# ---------------------------------------------------------------------------
# Sampler: stratified, with a floor that survives collapse
# ---------------------------------------------------------------------------
def stratify(tape: Tape, t_dec_ms: int, entry_px: float) -> str:
    """Observable-at-decision strata. Uses only causal_state, so the strata are
    a property of the behaviour policy and can be corrected for with IPS."""
    st = causal_state(tape, t_dec_ms, entry_px)
    age = st["age_s"]
    age_b = ("sniper" if age < 30 else "early" if age < 300
             else "mid" if age < 3600 else "late")
    dens = ("thin" if st["n_prior"] < 20 else "normal" if st["n_prior"] < 200
            else "dense")
    lane = "grad" if st["graduated"] else "curve"
    return f"{age_b}/{dens}/{lane}"


def explore_action(p_buy: float, p_min: float = 0.02) -> float:
    """Logit offset that guarantees p(BUY) >= p_min. A mechanical exploration
    floor, not a loss-shaping term, so it cannot be gamed by the policy."""
    pb = max(float(p_buy), float(p_min))
    if pb >= 1.0:
        pb = 1.0 - 1e-9
    return math.log(pb / (1.0 - pb))


# ---------------------------------------------------------------------------
# Self-test
# ---------------------------------------------------------------------------
def _selftest() -> int:
    checks, fails = 0, []

    def chk(tag, cond):
        nonlocal checks
        checks += 1
        if not cond:
            fails.append(tag)

    from regime_pricing import MarketParams, bonding_params, amm_params, price_fill

    # 1. merged registry holds both regimes for one mint
    o = DictReserveOracle()
    T0 = 1_700_000_000_000
    o.add(ReserveState(regime=Regime.BONDING_CURVE, mint="M1", sol_lamports=30_000_000_000,
                       token_raw=1_000_000_000_000_000, ts_unix_ms=T0))
    o.add(ReserveState(regime=Regime.AMM, mint="M1", sol_lamports=50_000_000_000,
                       token_raw=900_000_000_000_000, ts_unix_ms=T0 + 60_000))
    reg = ReserveRegistry(o, {"sources": {}})
    chk("both_regimes_held", reg.has("M1", Regime.BONDING_CURVE, T0)
        and reg.has("M1", Regime.AMM, T0 + 60_000))
    chk("no_future_state", not reg.has("M1", Regime.AMM, T0 - 1))
    chk("regimes_for_two", len(reg.regimes_for("M1", T0 + 60_000)) == 2)

    # 2. ANY budget prices a very stale join, and REPORTS the age
    eng = V3Engine(reg, max_reserve_stale_ms=STALE_ANY, deploy_sol=1.0)
    t1 = T0 + 3_600_000                      # one hour later: still exact state
    q = eng.quote_buy(_FakeTape("M1", T0 + 60_000), t1, t1, 1.0)
    chk("any_budget_prices_stale", q is not None and q["regime"] == "amm")
    prov = eng.provenance()
    chk("staleness_reported", prov["staleness_ms_max"] is not None
        and prov["staleness_ms_max"] > 3_000_000)
    chk("provenance_not_legacy", prov["legacy_priced"] is False)
    chk("provenance_asserts", eng.assert_priced(prov) is True)

    # 3. a tight budget refuses rather than pricing
    eng2 = V3Engine(reg, max_reserve_stale_ms=1_000)
    refused = False
    try:
        eng2.quote_buy(_FakeTape("M1", T0 + 60_000), t1, t1, 1.0)
    except (ReserveStale, ReserveUnavailable):
        refused = True
    chk("tight_budget_refuses", refused)

    # 4. uncovered mint refuses; NEVER falls back to the tape
    eng3 = V3Engine(reg, max_reserve_stale_ms=STALE_ANY)
    refused = False
    try:
        eng3.quote_buy(_FakeTape("UNKNOWN", T0), T0, T0, 1.0)
    except (ReserveUnavailable, V3RefusesToPrice):
        refused = True
    chk("uncovered_refuses_no_tape_fallback", refused)

    # 5. legacy is unreachable unless explicitly opted in, and is stamped
    eng4 = V3Engine(reg, allow_legacy=True)
    q4 = eng4.quote_buy(_FakeTape("M1", T0), T0 + 60_000, T0 + 60_000, 1.0)
    chk("legacy_optin_stamped", eng4.provenance()["legacy_priced"] is True)
    blocked = False
    try:
        eng4.assert_priced(eng4.provenance())
    except V3RefusesToPrice:
        blocked = True
    chk("legacy_blocked_by_gate", blocked)

    # 6. reward pair: pure is invariant to lambda, penalised is not
    ep = {"status": "ok", "final_equity_sol": 0.98, "net_sol_returned": -0.02,
          "equity_path": [[T0, 1.0], [T0 + 600_000, 0.70], [T0 + 1_200_000, 0.98]]}
    a = score_episode_v3(ep, mode="entry")
    b = score_episode_v3(ep, mode="exit")
    chk("pure_pair_identical", abs(a["pure"] - b["pure"]) < 1e-12)
    chk("entry_penalty_softer", a["penalised"] > b["penalised"])
    chk("pure_is_return", abs(a["pure"] - (-0.02 / (0.98 + 0.02))) < 1e-9)
    chk("drag_reported", a["penalty_drag"] < 0)

    # 7. notional-weighted max ignores a dust spike
    t = _FakeTape("M2", T0, px=[1.0, 1.0, 100.0, 1.0, 2.0],
                  sol=[10**9, 10**9, 50_000, 10**9, 10**9],
                  tt=[T0, T0 + 1000, T0 + 2000, T0 + 3000, T0 + 4000])
    r = notional_weighted_forward_max(t, T0, t_entry_ms=T0, floor_lamports=1_000_000)
    chk("dust_spike_ignored", abs(r["fm_vw"] - 2.0) < 1e-9)
    chk("blind_max_still_shows_spike", r["fm_blind"] > 90)

    # 8. money buckets + pi*
    recs = ([{"pure": 0.15, "fm_vw": 3.0, "rho": 0.5}] * 30
            + [{"pure": -0.20, "fm_vw": 1.1, "rho": 0.2}] * 70)
    mb = money_buckets(recs)
    chk("bucket_base_rate", abs(mb["base_rate_run"] - 0.30) < 1e-9)
    chk("pi_star_formula", abs(mb["pi_star"] - (0.20 / 0.35)) < 1e-4)
    chk("lift_reported", mb["required_lift"] > 1.9)

    # 9. ranking is bounded and ordered
    r2 = rank_targets([{"pure": v, "regime": "curve", "policy": "P"} for v in (-0.5, 0.1, 0.9)])
    chk("rank_bounded", all(0.0 <= x["rank_target"] <= 1.0 for x in r2))
    chk("rank_ordered", r2[0]["rank_target"] < r2[1]["rank_target"] < r2[2]["rank_target"])

    # 10. IPS normalises and up-weights the rare stratum
    w = ips_weights(["a"] * 9 + ["b"])
    chk("ips_mean_one", abs(sum(w) / len(w) - 1.0) < 1e-9)
    chk("ips_rare_upweighted", w[-1] > w[0])

    # 11. exploration floor is mechanical and monotone
    chk("floor_applied", explore_action(0.0) > explore_action(0.02) - 1e-12)
    chk("floor_preserves_high", explore_action(0.8) > explore_action(0.1))

    # 12. unit discipline: SOL <-> lamports
    chk("lamport_roundtrip", abs(to_sol_checked(to_lamports_checked(1.5)) - 1.5) < 1e-12)

    # 13. REGRESSION: position size comes from the FILL, not deploy/px0.
    # The tape says px=1.0 while the reserves are priced at ~4.18e-4, exactly the
    # 47,000x mismatch that made 753 of 1,058 decisions OrderTooLarge.
    o2 = DictReserveOracle()
    vs, vt = 115_005_359_057, 279_900_000_000_000          # realistic curve state
    o2.add(ReserveState(regime=Regime.BONDING_CURVE, mint="M3", sol_lamports=vs,
                        token_raw=vt, ts_unix_ms=T0))
    reg2 = ReserveRegistry(o2, {"sources": {}})
    eng5 = V3Engine(reg2, max_reserve_stale_ms=STALE_ANY)
    n = 40
    tt5 = [T0 + k * 30_000 for k in range(n)]
    t5 = _FakeTape("M3", T0, px=[1.0] * n, sol=[10**9] * n, tt=tt5)
    t5.venue = ["pumpfun_bonding"] * n            # curve venue: curve reserves held
    ep5 = simulate_v3(t5, T0, eng5, policy_name="MOONSHOT_TAIL", horizon_ms=600_000)
    chk("sizing_from_fill_status_ok", ep5.get("status") == "ok")
    if ep5.get("status") == "ok":
        # fill-derived: qty = deploy_SOL / (reserve price in SOL per raw token)
        expected_qty = 1.0 / ((vs / vt) / LAMPORTS_PER_SOL)
        wrong_qty = 1.0 / 1.0                              # tape-derived
        chk("sizing_from_fill_not_tape",
            abs(ep5["qty_init"] - expected_qty) / expected_qty < 0.5
            and abs(ep5["qty_init"] - wrong_qty) / wrong_qty > 10)
        chk("sells_execute", ep5["exit_reason"] in
            ("horizon", "trail", "stop", "exit_now")
            or any(l[0] not in ("BUY",) for l in ep5["legs"]))
        chk("conservation_clean", not ep5["violations"])
        chk("episode_fully_exited", ep5["qty_tokens_left"] <= 1e-12)
        # UNIT REGRESSION: marking qty*px_exec (lamports/token) valued a 1 SOL
        # position at 7.7e8. A one-SOL spot episode must stay O(1).
        eq = [v for _, v in ep5["equity_path"]]
        chk("equity_path_is_O_capital", max(eq) < 2.0 and min(eq) > 0.0)

    # 13b. THE TWO EXIT COSTS THAT USED TO BE MISSING.
    #   (a) IMPAIRMENT: a real exit nets LESS than the quote it fills at, charged in its
    #       own bucket so a diagnostic can name it, and labelled with its level/source.
    if ep5.get("status") == "ok":
        chk("impairment_charged_on_exit", ep5["costs_sol"]["impairment_sol"] > 0.0)
        chk("impairment_level_and_source_reported",
            ep5["impairment_level"] == IMPAIRMENT_LEVEL
            and ep5["impairment_source"] in ("frozen", "reference_unfrozen"))
        chk("no_terminal_loss_when_exitable", not ep5["terminal_loss_applied"])

    #   (b) TERMINAL LOSS: when the only refusal is CAPACITY, the episode is GRADED by the
    #       declared rule (capped at basis, never the displayed mark) instead of masked.
    #       The negative control is (c): a missing RESERVE must still refuse.
    class _Wall:
        """Delegates everything, then forces one refusal class on every sell."""

        def __init__(self, inner, exc):
            self.inner, self.exc, self.regime = inner, exc, inner.regime

        def quote_buy(self, *a, **kw):
            return self.inner.quote_buy(*a, **kw)

        def quote_sell(self, *a, **kw):
            raise self.exc

    ep_tl = simulate_v3(t5, T0, _Wall(eng5, OrderTooLarge("forced capacity refusal")),
                        policy_name="MOONSHOT_TAIL", horizon_ms=600_000)
    chk("capacity_refusal_graded_not_masked", ep_tl.get("status") == "ok"
        and ep_tl.get("terminal_loss_applied") is True)
    if ep_tl.get("status") == "ok":
        chk("terminal_loss_is_the_declared_policy",
            ep_tl["terminal_loss_policy"] == TERMINAL_LOSS_POLICY
            and ep_tl["terminal_loss_sol"] > 0.0)
        # write_to_zero: a trapped 1 SOL position returns nothing, so the episode is a
        # ~total loss - NOT a positive mark (which is what the old path would have shown
        # if it had valued the remainder at the last quote).
        chk("terminal_loss_is_not_the_displayed_mark",
            ep_tl["net_sol_returned"] < -0.5 * ep_tl["deploy_sol"])
        chk("terminal_loss_conservation_clean", not ep_tl["violations"])
    ep_data = simulate_v3(t5, T0, _Wall(eng5, ReserveUnavailable("forced data gap")),
                          policy_name="MOONSHOT_TAIL", horizon_ms=600_000)
    chk("data_gap_still_refuses",
        ep_data.get("status") == "unexitable_at_horizon"
        and ep_data.get("net_sol_returned") is None)

    # 14. a venue with no reserves for that market REFUSES (the gx=None hole)
    eng6 = V3Engine(ReserveRegistry(o2, {"sources": {}}), max_reserve_stale_ms=STALE_ANY)
    t_amm = _FakeTape("M3", T0, tt=tt5)
    t_amm.venue = ["pumpswap"] * n                        # entirely AMM, curve-only reserves
    refused14 = False
    try:
        eng6.quote_buy(t_amm, tt5[0], tt5[0], 1.0)
    except (V3RefusesToPrice, ReserveUnavailable, RegimeMismatch):
        refused14 = True
    chk("whole_tape_amm_refuses_without_pool_reserves", refused14)

    # 15. M6 DECISION->FILL DRIFT (measured, frozen). Three things must hold and each
    # is a distinct failure mode the artifact can have:
    #   (a) DISABLED reproduces the pre-change arithmetic exactly - the charge is not
    #       smuggled into any other term (an episode score that moved with the flag
    #       off would mean the "measured" number had leaked into the accounting).
    #   (b) ENABLED worsens the entry basis by EXACTLY the frozen bound - the reported
    #       number is the charged number, not a decoration.
    #   (c) the magnitude comes from the ARTIFACT: substituting the module's reader
    #       (as readiness [M] does) must move the charge, so a literal here is
    #       impossible.
    src_d = fill_drift_source()
    chk("fill_drift_source_measured", src_d["source"] == "frozen")
    drift_d = entry_drift_for_pricing()
    dbp = int(drift_d["entry_slippage_bps"])
    chk("fill_drift_bound_positive", dbp > 0)
    # (c) the reward reads the artifact, not a literal: the substitute reader wins.
    _orig_reader = entry_drift_bps
    try:
        globals()["entry_drift_bps"] = lambda level="realistic": {
            "entry_slippage_bps": 4242, "source": "sentinel", "level": level}
        chk("fill_drift_reads_artifact_not_literal",
            int(entry_drift_for_pricing()["entry_slippage_bps"]) == 4242)
    finally:
        globals()["entry_drift_bps"] = _orig_reader
    chk("fill_drift_reader_restored",
        int(entry_drift_for_pricing()["entry_slippage_bps"]) == dbp)
    # (a) and (b) on the same real reserves/tape as check 13.
    if ep5.get("status") == "ok":
        ep_off = simulate_v3(t5, T0, eng5, policy_name="MOONSHOT_TAIL",
                             horizon_ms=600_000, fill_drift=False)
        ep_on = simulate_v3(t5, T0, eng5, policy_name="MOONSHOT_TAIL",
                            horizon_ms=600_000, fill_drift=True)
        chk("fill_drift_status_stable", ep_off.get("status") == "ok"
            and ep_on.get("status") == "ok")
        if ep_off.get("status") == "ok" and ep_on.get("status") == "ok":
            # (a) disabled == zero charge, term for term.
            chk("fill_drift_disabled_is_unpriced",
                ep_off["fill_drift_bps"] == 0 and ep_off["fill_drift_sol"] == 0.0
                and ep_off["fill_drift_source"] == "disabled")
            chk("fill_drift_disabled_sizing_unchanged",
                abs(ep_off["qty_init"] - (1.0 / ep_off["entry_px"])) < 1e-9
                and ep_off["qty_init"] > ep_on["qty_init"])
            chk("fill_drift_disabled_reproduces_fill",
                abs(ep_off["entry_px"] - ep_off["entry_px_undrift"]) < 1e-15)
            # (b) enabled: the basis is worse by EXACTLY the bound, never better.
            chk("fill_drift_entry_never_better",
                ep_on["entry_px"] >= ep_off["entry_px"] * (1.0 + dbp / 1e4) - 1e-9)
            chk("fill_drift_charged_is_the_bound",
                abs(ep_on["entry_px"] / ep_off["entry_px"] - (1.0 + dbp / 1e4)) < 1e-9)
            chk("fill_drift_reported_matches_charged",
                ep_on["fill_drift_bps"] == dbp
                and ep_on["fill_drift_source"] == "frozen")
            chk("fill_drift_conservation_clean", not ep_on["violations"])
            # (d) the bound is a LOWER bound on how much better than the prompt's own
            # price the reward may ever look: with the engine fill sitting exactly ON
            # the decision price, the charged basis is that price moved adversely, so
            # an entry is never credited better than the decision price beyond the
            # measured bound. This is the only case where the two prices coincide, so
            # it is the case that isolates the drift term.
            px_m = float(vs) / float(vt)              # reserve marginal price
            t6 = _FakeTape("M3", T0, px=[px_m] * n, sol=[10**9] * n, tt=tt5)
            t6.venue = ["pumpfun_bonding"] * n
            ep_m = simulate_v3(t6, T0, eng5, policy_name="MOONSHOT_TAIL",
                               horizon_ms=600_000, fill_drift=True)
            if ep_m.get("status") == "ok" and ep_m.get("entry_px_decision"):
                dec_sol = float(ep_m["entry_px_decision"]) / LAMPORTS_PER_SOL_F
                chk("fill_drift_never_better_than_decision_price",
                    ep_m["entry_px"] >= dec_sol * (1.0 - dbp / 1e4))
                chk("fill_drift_gap_reported",
                    ep_m["entry_gap_vs_decision_bp"] is not None)

    print(json.dumps({"checks": checks, "failed": fails,
                      "verdict": "PASS" if not fails else "FAIL"}, indent=1))
    return 0 if not fails else 1


class _FakeTape:
    """Minimal Tape stand-in for self-tests (no I/O)."""

    def __init__(self, mint, ts, px=None, sol=None, tt=None):
        self.mint = mint
        if tt is None:
            tt = [ts]
        self.tt = np.asarray(tt, dtype=np.int64)
        self.px = np.asarray(px if px is not None else [1.0] * len(self.tt), dtype=float)
        self.sol = np.asarray(sol if sol is not None else [10**9] * len(self.tt), dtype=np.int64)
        self.tok = np.asarray([10**12] * len(self.tt), dtype=np.int64)
        self.venue = ["pumpswap"] * len(self.tt)

    def venue_change_index(self):
        for k in range(1, len(self.venue)):
            if self.venue[k] != self.venue[0]:
                return k
        return None

    def price_at_or_before(self, t_ms, decision_ms, what="price"):
        i = int(np.searchsorted(self.tt, int(t_ms), side="right")) - 1
        return float(self.px[i]) if i >= 0 else None

    def first_fill_after(self, t_ms, decision_ms, what="entry"):
        i = int(np.searchsorted(self.tt, int(t_ms), side="left"))
        return int(i) if i < self.tt.size else None


# ---------------------------------------------------------------------------
# Real-data probe
# ---------------------------------------------------------------------------
def probe(n_mints=300, seed=7, max_decisions_per_mint=6, policy="MOONSHOT_TAIL",
          horizon_ms=HORIZON_MS_DEFAULT, mints=None, verbose=True) -> dict:
    """Score real entries on real reserves. Refusals are counted, never zeroed."""
    reg = ReserveRegistry.build(mints=None, verbose=verbose)
    eng = V3Engine(reg, max_reserve_stale_ms=STALE_ANY, deploy_sol=1.0)
    tapes = load_canonical_tapes(TAPES, min_notional_lamports=100_000, max_px_ratio=50)
    if mints is None:
        rng = np.random.default_rng(seed)
        keys = sorted(tapes)
        mints = list(rng.choice(keys, size=min(n_mints, len(keys)), replace=False))

    # decisions come from the SFT corpus (the distribution the model is asked on)
    want = set(mints)
    per_mint = {}
    for line in open(CORPUS, encoding="utf-8"):
        try:
            r = json.loads(line)
        except Exception:
            continue
        if r.get("family") != "decision":
            continue
        parts = (r.get("episode_id") or "").split(":")
        if len(parts) < 3:
            continue
        m = parts[1]
        if m not in want:
            continue
        try:
            per_mint.setdefault(m, []).append(int(parts[2]))
        except ValueError:
            continue

    recs, refused, strata = [], {}, []
    n_ok_decisions = 0
    for mint in mints:
        tape = tapes.get(mint)
        tdecs = per_mint.get(mint)
        if tape is None or not tdecs:
            refused["no_tape_or_no_decision"] = refused.get("no_tape_or_no_decision", 0) + 1
            continue
        t0, tN = int(tape.tt[0]), int(tape.tt[-1])
        inwin = [t for t in tdecs if t0 <= t <= tN]
        if not inwin:
            refused["decision_outside_tape_window"] = \
                refused.get("decision_outside_tape_window", 0) + 1
            continue
        step = max(1, len(inwin) // max_decisions_per_mint)
        for t_dec in inwin[::step][:max_decisions_per_mint]:
            n_ok_decisions += 1
            res = score_entry(tape, t_dec, eng, policy_name=policy,
                              horizon_ms=horizon_ms)
            if res.get("refused"):
                k = res.get("status") or "refused"
                refused[k] = refused.get(k, 0) + 1
                continue
            fm = notional_weighted_forward_max(tape, t_dec,
                                               t_entry_ms=res["t_entry_ms"],
                                               horizon_ms=horizon_ms)
            rec = {"mint": mint, "t_dec_ms": int(t_dec), "regime": None,
                   "policy": policy, "pure": res["pure"], "penalised": res["penalised"],
                   "penalty_drag": res["penalty_drag"], "held_ms": res["held_ms"],
                   "exit_reason": res["exit_reason"],
                   "fm_vw": (fm or {}).get("fm_vw"), "rho": None,
                   "dwell_s": peak_dwell_s(tape, t_dec, horizon_ms=horizon_ms)}
            rec["rho"] = capture_ratio(rec["pure"], rec["fm_vw"])
            recs.append(rec)
            strata.append(stratify(tape, t_dec, res["entry_px"]))

    rank_targets(recs)
    ips = ips_weights(strata)
    for r, w in zip(recs, ips):
        r["ips_weight"] = round(w, 4)
    out = {"policy": policy, "horizon_ms": horizon_ms,
           "mints_requested": len(mints), "mints_with_decisions": len(per_mint),
           "decisions_attempted": n_ok_decisions, "decisions_scored": len(recs),
           "refusals": refused,
           "registry": {k: v for k, v in reg.stats.items() if k != "sources"},
           "regime_decisions": dict(eng.regime_decisions),
           "refusals_by_reason": dict(eng.refusals_by_reason),
           "money_buckets": money_buckets(recs),
           "strata": {s: strata.count(s) for s in sorted(set(strata))}}
    if recs:
        pure = np.asarray([r["pure"] for r in recs], float)
        out["overall"] = {"n": int(pure.size),
                          "pure_mean": round(float(pure.mean()), 5),
                          "pure_median": round(float(np.median(pure)), 5),
                          "pure_p_positive": round(float((pure > 0).mean()), 4),
                          "penalised_mean": round(float(np.mean([r["penalised"] for r in recs])), 5)}
    if verbose:
        print(json.dumps(out, indent=1, default=str))
    return out


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--selftest", action="store_true")
    ap.add_argument("--probe", action="store_true")
    ap.add_argument("--mints", type=int, default=300)
    ap.add_argument("--per-mint", type=int, default=6)
    ap.add_argument("--policy", default="MOONSHOT_TAIL")
    ap.add_argument("--horizon-ms", type=int, default=HORIZON_MS_DEFAULT)
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--json", default="")
    a = ap.parse_args(argv)
    if a.selftest:
        return _selftest()
    if a.probe:
        out = probe(n_mints=a.mints, seed=a.seed, max_decisions_per_mint=a.per_mint,
                    policy=a.policy, horizon_ms=a.horizon_ms)
        if a.json:
            with open(a.json, "w", encoding="utf-8") as fh:
                json.dump(out, fh, indent=1, default=str)
            print(f"wrote {a.json}")
        return 0
    ap.print_help()
    return 0


if __name__ == "__main__":
    sys.exit(main())