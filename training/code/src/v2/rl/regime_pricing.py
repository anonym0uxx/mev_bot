#!/usr/bin/env python
"""MECHANICS-FIRST PRICING FOR ANY pump.fun MARKET REGIME.

One interface, two explicitly selected regimes, both priced from OBSERVABLE
RESERVES AT DECISION TIME against OUR OWN ORDER SIZE:

    regime = BONDING_CURVE  ->  constant product on (virtual_sol_reserves,
                                virtual_token_reserves) of the pump.fun curve
    regime = AMM            ->  constant product on (pool_sol_reserves,
                                pool_token_reserves) of a post-graduation pool

WHY THIS EXISTS
  The previous engine priced a fill from the last realized tape price plus a
  linearly extrapolated "depth" slope.  That reproduces our captured tape but it
  cannot generalise: it needs a prior fill, it has no notion of how much of the
  curve is gone, and it silently reads a *tape* price whenever reserves are
  missing.  Mechanics-first pricing inverts that: the reservcs ARE the state, the
  price is an OUTPUT of the reserves, and the per-order price impact falls out of
  our own size.  See /training/v2/reports/FORWARD_RESERVE_CAPTURE_PLAN.md.

REFUSAL DISCIPLINE (binding)
  There is exactly ONE way to use a tape price, and it must be named:
  ``Regime.LEGACY_TAPE`` (opt-in).  Every mechanics call refuses loudly - it
  raises - when reserves are absent, non-positive, stale, future-dated, or of the
  wrong regime.  It never falls back to a tape price.  Silent fallback is what
  made a 64,636-fill pumpswap replay look like a 0.138 correlation instead of a
  measurement of the bonding curve (64,636 unpriceable fills were answered with a
  tape price and reported as one aggregate).

CAUSALITY
  A reserve state carries its own timestamp.  ``price_fill`` REFUSES any reserve
  stamped after the decision it is meant to inform (``ReserveLookahead``), and
  every quote records ``reserve_ts_ms`` + ``staleness_ms`` so a stale join can
  never masquerade as a decision-time state.  Episode reports expose the full
  list of reserves consulted per episode (see ``MechanicsEngine.ledger``).

UNITS (binding, no exceptions)
  1 SOL == 1_000_000_000 lamports exactly.
  token reserves are RAW base units (6 decimals).
  price = lamports per RAW token = sol_reserves_lamports / token_reserves_raw.

MEASURED CONSTANTS (from our own data, reused, not re-guessed)
  v0 = 30e9 lamports, t0 = 1.073e15 raw, k0 = 3.219e25
  graduation threshold = 85e9 lamports on the real-SOL leg
  bonding fee on the SOL leg = 0.009463 (notional-weighted measured mean)
  pumpswap fee = 20 bp LP (moves the reserves) + 5 bp protocol + 5 bp coin
  creator (paid by the trader, does not move the reserves) - the fee schedule is
  fixed by the pump-amm program, NOT calibrated on data we do not have.

CLI (keeps every reward_engine.py flag; adds the regime selector here too)
  --selftest --conserve --reconcile --measure-depth --reserves
  --regime {legacy_tape,bonding_curve,amm}
"""
from __future__ import annotations

import argparse
import json
import math
import os
import sys
import numpy as np
from dataclasses import dataclass, asdict, field
from enum import Enum
from typing import Callable, Iterable, Optional, Sequence

LAMPORTS_PER_SOL = 1_000_000_000
assert LAMPORTS_PER_SOL == 10 ** 9, "1 SOL must be exactly 1e9 lamports"

TOKEN_DECIMALS = 6
TOKEN_SCALE = 10 ** TOKEN_DECIMALS

# ---- measured bonding-curve constants -------------------------------------
V0_LAMPORTS = 30 * LAMPORTS_PER_SOL          #  30_000_000_000
T0_RAW = 1_073_000_000 * TOKEN_SCALE         #  1.073e15
K0 = float(V0_LAMPORTS) * float(T0_RAW)      #  3.219e25
V_GRAD_LAMPORTS = 85 * LAMPORTS_PER_SOL      #  85_000_000_000
FEE_RATE_BONDING = 0.009463                  # measured on 65,927 clean swaps
PUMPFUN_CREATOR_FEE_RATE = 0.0005            # 5 bp of the same SOL leg

# ---- pumpswap program constants (CALIBRATED from real on-chain events) -----
# Measured over 84,666 decoded BuyEvent/SellEvent from the 2026-08-24 capture:
#   lp_fee_basis_points = 25 (majority; 20 on a minority), protocol = 5
#   lp_fee / quote_in_with_lp_fee: p50 = 0.0025, p10 = 0.0020
# so the fee that MOVES THE POOL is 25 bp, and the trader-only extraction is 5 bp
# (protocol only - the coin-creator fee is not collected; see AMM_CREATOR_FEE_RATE).
AMM_LP_FEE_RATE = 0.0025        # measured: protocol/lp == 1/5 exactly over 84,319 rows
AMM_PROTOCOL_FEE_RATE = 0.0005  # measured (protocolFeeBasisPoints = 5)
# MEASURED ZERO over every one of the 84,352 rows of our own pumpswap forward
# capture (coin_creator_fee_lamports == 0 throughout). The 5bp figure came from the
# account LAYOUT (the field exists) - not from the pools actually charging it. Left
# in, it double-counted 5bp/leg of cost, i.e. 10bp per round trip on every AMM
# episode. AMM per-leg total is therefore 30bp (25 LP + 5 protocol), not 35bp.
AMM_CREATOR_FEE_RATE = 0.0
AMM_TRADER_EXTRACTION_RATE = AMM_PROTOCOL_FEE_RATE + AMM_CREATOR_FEE_RATE

# ---- staleness policy (D3 decision, 2026-09-13) ---------------------------
# For pump.fun the reserves only change when the mint TRADES, and every trade
# emits the new state, so "the newest state at or before t_dec" is the EXACT
# state at t_dec no matter how long the mint has been quiet. Wall-clock age is
# therefore only a proxy for a CAPTURE GAP, not for state invalidity - measured
# on the c4 corpus, a 60 s wall-clock cap threw away 34 points of train coverage
# (65.1% -> 99.5%) while the staleness p50 was only 20.7 s.
#   * OFFLINE PRICING (replay / reward): 60 s - relaxed caps measurably degrade the
#     replay (curve corr 0.978 -> 0.833 when a 24 h cap was tested), because our
#     capture is session-based and a large age correlates with fills we never saw.
#   * ANNOTATION/COVERAGE (SFT episode fields): up to 24 h, always labelled with the
#     age, never used to price a trade.
#   * LIVE (forward trading): 120 s, because a live feed can genuinely have missed
#     the newest event.
DEFAULT_MAX_STALE_MS_PRICING = 60_000      # validated: curve corr 0.978 / AMM 0.9866
DEFAULT_MAX_STALE_MS_ANNOTATION = 86_400_000  # coverage only, always report the age
DEFAULT_MAX_STALE_MS_LIVE = 120_000


class UnitError(ValueError):
    """A number is not in the units the interface declares (lamports vs SOL)."""


class LookaheadError(RuntimeError):
    """ENGINE-WIDE lookahead violation: a decision consulted data from its future.

    Defined HERE and re-exported by reward_engine so that the tape guard and the
    reserve guard raise the SAME failure class: a scored decision can never
    consult data timestamped after its own decision time, from any layer.
    """


class ReserveError(RuntimeError):
    """Base class: the engine refuses to price rather than guess."""


class ReserveUnavailable(ReserveError):
    """No reserves for this (mint, regime) at or before the decision time."""


class ReserveStale(ReserveError):
    """The newest usable reserve is older than the caller's staleness budget."""


class ReserveLookahead(ReserveError, LookaheadError):
    """A reserve stamped AFTER the decision time - refused, always.

    Also a LookaheadError, so `except LookaheadError` catches a reserve-side
    lookahead exactly like it catches a tape-side one.
    """


class RegimeMismatch(ReserveError):
    """Reserves of the wrong regime were handed to a market (curve vs pool)."""


class OrderTooLarge(ReserveError):
    """Our own order would take more than the reserves can absorb."""


class Regime(str, Enum):
    """Explicit market regime. Selected, never inferred silently."""

    BONDING_CURVE = "bonding_curve"
    AMM = "amm"
    LEGACY_TAPE = "legacy_tape"     # opt-in only: the old tape-price path


# ---------------------------------------------------------------------------
# Reserve state
# ---------------------------------------------------------------------------
@dataclass(frozen=True)
class ReserveState:
    """A snapshot of a market's reserves, stamped with when they were true."""

    regime: Regime
    mint: str
    sol_lamports: int          # virtual_sol (curve) or pool SOL vault (pool)
    token_raw: int             # virtual_token (curve) or pool token vault (pool)
    ts_unix_ms: int            # time the reserves were observed on chain
    venue: str = ""            # 'pumpfun_bonding' | 'pumpswap'
    account: Optional[str] = None   # curve PDA or pool / vault account
    source: str = ""           # provenance, e.g. 'slinky:pump_state_v3'
    seq: Optional[int] = None

    def __post_init__(self):
        if self.sol_lamports is None or self.token_raw is None:
            raise ReserveUnavailable(
                f"{self.mint}: reserve state has NULL reserves (sol={self.sol_lamports}, "
                f"tok={self.token_raw}) - refusing to guess")
        if int(self.sol_lamports) <= 0 or int(self.token_raw) <= 0:
            raise ReserveUnavailable(
                f"{self.mint}: non-positive reserves sol={self.sol_lamports} "
                f"tok={self.token_raw}")
        if int(self.ts_unix_ms) <= 0:
            raise ReserveUnavailable(f"{self.mint}: reserve state has no timestamp")

    @property
    def price_lamports_per_raw_token(self) -> float:
        return self.sol_lamports / self.token_raw

    def staleness_ms(self, decision_ts_ms: int) -> int:
        return int(decision_ts_ms) - int(self.ts_unix_ms)

    def checked_for(self, decision_ts_ms: int, regime: Optional[Regime] = None,
                    max_stale_ms: Optional[int] = None) -> "ReserveState":
        """Causality + freshness gate. Raises instead of returning something weak."""
        d = int(decision_ts_ms)
        if int(self.ts_unix_ms) > d:
            raise ReserveLookahead(
                f"LOOKAHEAD: reserve for {self.mint} is stamped {self.ts_unix_ms} > "
                f"decision {d} (+{int(self.ts_unix_ms) - d} ms in the future) - refused")
        if regime is not None and self.regime != regime:
            raise RegimeMismatch(
                f"{self.mint}: reserves are {self.regime.value} but the market is "
                f"{regime.value}")
        st = self.staleness_ms(d)
        if max_stale_ms is not None and st > int(max_stale_ms):
            raise ReserveStale(
                f"{self.mint}: newest reserve is {st} ms old at decision {d} "
                f"(budget {int(max_stale_ms)} ms) - refuse rather than price on a "
                f"stale join")
        return self

    def as_dict(self) -> dict:
        d = asdict(self)
        d["regime"] = self.regime.value
        return d


# ---------------------------------------------------------------------------
# Markets: one constant-product core, two explicit fee policies
# ---------------------------------------------------------------------------
def cp_buy_out(x: int, y: int, dx: int) -> int:
    """Constant product: tokens out for ``dx`` base units entering the pool."""
    if dx <= 0:
        raise ValueError("cp_buy_out: dx must be > 0")
    if dx >= x:
        raise OrderTooLarge(f"cp_buy_out: order {dx} >= reserve {x}")
    return y * dx // (x + dx)


def cp_sell_out(x: int, y: int, dy: int) -> int:
    """Constant product: base units out for ``dy`` tokens entering the pool."""
    if dy <= 0:
        raise ValueError("cp_sell_out: dy must be > 0")
    if dy >= y:
        raise OrderTooLarge(f"cp_sell_out: order {dy} >= reserve {y}")
    return x * dy // (y + dy)


@dataclass(frozen=True)
class MarketParams:
    """Fee policy + regime rules for one market regime."""

    regime: Regime
    fee_rate: float                    # charged on the SOL leg, both directions
    venue: str
    graduation_lamports: Optional[int] = None   # only BONDING_CURVE
    pool_fee_rate: Optional[float] = None       # AMM: fee that moves the reserves
    trader_extraction_rate: float = 0.0         # AMM: protocol+creator, no move


def bonding_params(fee_rate: float = FEE_RATE_BONDING) -> MarketParams:
    return MarketParams(regime=Regime.BONDING_CURVE, fee_rate=float(fee_rate),
                        venue="pumpfun_bonding",
                        graduation_lamports=V_GRAD_LAMPORTS,
                        pool_fee_rate=float(fee_rate))


def amm_params(lp_fee_rate: float = AMM_LP_FEE_RATE,
               trader_extraction_rate: float = AMM_TRADER_EXTRACTION_RATE) -> MarketParams:
    return MarketParams(regime=Regime.AMM, fee_rate=float(lp_fee_rate),
                        venue="pumpswap", graduation_lamports=None,
                        pool_fee_rate=float(lp_fee_rate),
                        trader_extraction_rate=float(trader_extraction_rate))


# ---------------------------------------------------------------------------
# Fill terms: the ONE output type for both regimes
# ---------------------------------------------------------------------------
@dataclass(frozen=True)
class FillTerms:
    regime: str
    side: str
    order_lamports_in: int          # what WE spend (buy)
    order_tokens_in_raw: int        # what WE sell  (sell)
    sol_delta_lamports: int         # signed: -spend on a buy, +proceeds on a sell
    token_delta_raw: int            # signed: +tokens on a buy, -tokens on a sell
    px_pre: float                   # marginal price before our order
    px_exec: float                  # average price our order actually pays
    impact_bp: float                # all-in (px_exec/px_pre) deviation, signed bp
    impact_only_bp: float           # our own size's price impact, FEE EXCLUDED
    fee_lamports: int               # fee kept by the venue on the SOL leg
    slippage_lamports: int          # cost of moving the price, vs px_pre
    notional_lamports: int          # |our SOL leg|
    reserves_used: dict             # exactly what we priced from
    reserve_ts_ms: int
    decision_ts_ms: int
    staleness_ms: int
    account: Optional[str]
    source: str
    k_pre: int
    k_post: int
    crosses_graduation: bool = False

    def as_dict(self) -> dict:
        return asdict(self)


# ---------------------------------------------------------------------------
# THE interface: price one fill from observable reserves
# ---------------------------------------------------------------------------
def price_fill(*, regime: Regime, side: str, reserves: Optional[ReserveState],
               decision_ts_ms: int, params: Optional[MarketParams] = None,
               sol_in_lamports: Optional[int] = None,
               token_in_raw: Optional[int] = None,
               max_stale_ms: Optional[int] = None,
               refuse_cross_graduation: bool = False) -> FillTerms:
    """Price OUR OWN order against the reserves that existed at ``decision_ts_ms``.

    Exactly one of ``sol_in_lamports`` (buy) / ``token_in_raw`` (sell) is required.
    Raises:
      ReserveUnavailable  - no reserves, NULL or non-positive reserves
      ReserveLookahead    - reserves stamped after the decision time
      ReserveStale        - reserves older than ``max_stale_ms``
      RegimeMismatch      - reserves of the other regime
      OrderTooLarge       - our order exceeds what the reserves can absorb
    Never returns a tape-price estimate.
    """
    side = str(side).lower()
    if side not in ("buy", "sell"):
        raise ValueError(f"side must be buy|sell, got {side!r}")
    if reserves is None:
        raise ReserveUnavailable(
            f"regime={regime.value}: no reserve state supplied for this fill - "
            f"refusing to price from a tape price (mechanics-first, no fallback)")
    p = params or (bonding_params() if regime == Regime.BONDING_CURVE else amm_params())
    if p.regime != regime:
        raise RegimeMismatch(f"params are {p.regime.value} but regime={regime.value}")
    res = reserves.checked_for(decision_ts_ms, regime=regime, max_stale_ms=max_stale_ms)

    x, y = int(res.sol_lamports), int(res.token_raw)
    k_pre = x * y
    fee_rate = float(p.fee_rate)
    grad = None
    if regime == Regime.BONDING_CURVE:
        grad = int(p.graduation_lamports if p.graduation_lamports is not None
                   else V_GRAD_LAMPORTS)

    if side == "buy":
        if sol_in_lamports is None or sol_in_lamports <= 0:
            raise ValueError("buy requires sol_in_lamports > 0")
        dx_total = int(sol_in_lamports)
        quote_in = 0.0
        # fee handling. BONDING_CURVE: pump.fun takes its fee from the SOL the
        # trader pays, so only (1-fee)*dx reaches the curve. AMM: the trader also
        # pays protocol + coin-creator extraction, and the pool keeps the LP fee
        # inside the quote leg (so the pool receives more than quote_in).
        if regime == Regime.BONDING_CURVE:
            fee = int(round(dx_total * fee_rate))
            dx_pool = dx_total - fee
        else:
            denom = 1.0 + fee_rate + float(p.trader_extraction_rate)
            quote_in = dx_total / denom
            dx_pool = int(round(quote_in * (1.0 + fee_rate)))
            fee = dx_total - int(round(quote_in))
        if dx_pool <= 0 or dx_pool >= x:
            raise OrderTooLarge(
                f"buy of {dx_total} lamports on reserves x={x}: usable input "
                f"{dx_pool} is not absorbable ({res.mint})")
        dy = cp_buy_out(x, y, dx_pool)
        x_post, y_post = x + dx_pool, y - dy
        px_pre = x / y
        px_exec = dx_total / dy
        impact_bp = (px_exec / px_pre - 1.0) * 1e4
        # slippage = SOL paid above the pre-trade marginal price, FEE EXCLUDED
        # (the fee is reported separately). For the AMM this measures our own
        # order's price impact on the pool, not the LP fee that rode along.
        dx_impact = int(round(dx_pool))
        slip = max(0, dx_impact - int(round(dy * px_pre)))
        impact_only_bp = (dx_impact / dy / px_pre - 1.0) * 1e4
        crosses = bool(grad is not None and x_post >= grad)
        if crosses and refuse_cross_graduation:
            raise OrderTooLarge(
                f"buy would graduate the curve (post vsol {x_post} >= {grad}) - refused")
        return FillTerms(
            regime=regime.value, side="buy", order_lamports_in=dx_total,
            order_tokens_in_raw=0, sol_delta_lamports=-dx_total, token_delta_raw=int(dy),
            px_pre=px_pre, px_exec=px_exec, impact_bp=impact_bp,
            impact_only_bp=impact_only_bp, fee_lamports=fee,
            slippage_lamports=slip, notional_lamports=dx_total,
            reserves_used={"sol_lamports": x, "token_raw": y, "regime": regime.value,
                           "mint": res.mint, "venue": res.venue or p.venue,
                           "account": res.account, "ts_unix_ms": int(res.ts_unix_ms)},
            reserve_ts_ms=int(res.ts_unix_ms), decision_ts_ms=int(decision_ts_ms),
            staleness_ms=res.staleness_ms(decision_ts_ms), account=res.account,
            source=res.source, k_pre=k_pre, k_post=x_post * y_post,
            crosses_graduation=crosses)

    # ---- sell: we deliver raw tokens, the venue pays us SOL -----------------
    if token_in_raw is None or token_in_raw <= 0:
        raise ValueError("sell requires token_in_raw > 0")
    dy_in = int(token_in_raw)
    if dy_in >= y:
        raise OrderTooLarge(f"sell of {dy_in} raw tokens >= reserve {y} ({res.mint})")
    gross = cp_sell_out(x, y, dy_in)          # lamports leaving the pool
    x_post, y_post = x - gross, y + dy_in
    if regime == Regime.BONDING_CURVE:
        # pump.fun takes its fee out of the SOL proceeds.
        fee = int(round(gross * fee_rate))
        sol_out = gross - fee
    else:
        # pool quote leg leaves gross; the trader then pays the LP + extraction
        # haircut on what the pool released.
        net_frac = (1.0 - float(p.trader_extraction_rate)) / (1.0 + fee_rate)
        sol_out = int(round(gross * net_frac))
        fee = gross - sol_out
    if sol_out <= 0:
        raise OrderTooLarge(f"sell of {dy_in} raw tokens nets <= 0 lamports ({res.mint})")
    px_pre = x / y
    px_exec = sol_out / dy_in
    impact_bp = (1.0 - px_exec / px_pre) * 1e4
    slip = int(round(dy_in * px_pre - gross))
    impact_only_bp = (1.0 - (gross / dy_in) / px_pre) * 1e4
    return FillTerms(
        regime=regime.value, side="sell", order_lamports_in=0,
        order_tokens_in_raw=dy_in, sol_delta_lamports=int(sol_out),
        token_delta_raw=-dy_in, px_pre=px_pre, px_exec=px_exec, impact_bp=impact_bp,
        impact_only_bp=impact_only_bp, fee_lamports=int(fee), slippage_lamports=slip, notional_lamports=int(sol_out),
        reserves_used={"sol_lamports": x, "token_raw": y, "regime": regime.value,
                       "mint": res.mint, "venue": res.venue or p.venue,
                       "account": res.account, "ts_unix_ms": int(res.ts_unix_ms)},
        reserve_ts_ms=int(res.ts_unix_ms), decision_ts_ms=int(decision_ts_ms),
        staleness_ms=res.staleness_ms(decision_ts_ms), account=res.account,
        source=res.source, k_pre=k_pre, k_post=x_post * y_post,
        crosses_graduation=False)


def price_fill_lamports_per_raw_token(*, regime: Regime, side: str,
                                      reserves: Optional[ReserveState],
                                      token_amount_raw: int, decision_ts_ms: int,
                                      params: Optional[MarketParams] = None,
                                      max_stale_ms: Optional[int] = None) -> float:
    """Predictive price of a fill of size ``token_amount_raw`` - the replay entry point.

    The replay knows each recorded fill's own size, so the predicted price is
    priced from reserves with THAT size, not from a tape price.
    """
    res = reserves
    if res is None:
        raise ReserveUnavailable("no reserves - refusing to price from tape")
    if str(side).lower() == "buy":
        # invert the size: how many lamports did this buy spend, at the reserves?
        # price_fill needs the SOL leg, so derive it from the curve itself:
        # dx such that tokens out == token_amount_raw.
        x, y = int(res.sol_lamports), int(res.token_raw)
        dy = int(token_amount_raw)
        if dy >= y:
            raise OrderTooLarge(f"{res.mint}: recorded buy size {dy} >= reserve {y}")
        # dy = (y - dy) is post;  y*dx/(x+dx) = dy  ->  dx = x*dy/(y-dy)
        dx_pool = x * dy // max(y - dy, 1)
        p = params or (bonding_params() if regime == Regime.BONDING_CURVE else amm_params())
        if regime == Regime.BONDING_CURVE:
            dx_total = int(round(dx_pool / (1.0 - float(p.fee_rate))))
        else:
            dx_total = int(round(dx_pool / (1.0 + float(p.fee_rate))
                                 * (1.0 + float(p.fee_rate) + float(p.trader_extraction_rate))))
        t = price_fill(regime=regime, side="buy", reserves=res, decision_ts_ms=decision_ts_ms,
                       params=p, sol_in_lamports=max(dx_total, 1), max_stale_ms=max_stale_ms)
        return t.px_exec
    t = price_fill(regime=regime, side="sell", reserves=res, decision_ts_ms=decision_ts_ms,
                   params=params, token_in_raw=int(token_amount_raw),
                   max_stale_ms=max_stale_ms)
    return t.px_exec


def fill_from_token_leg(*, regime, side, reserves, token_leg_raw, decision_ts_ms,
                        params=None, max_stale_ms=None, refuse_cross_graduation=False):
    """Inverse direction: OUR ORDER SIZE is a TOKEN leg, solve for the SOL leg.

    Used by the replay (the recorded tape gives the token leg, we must predict the
    SOL notional) and by any forward caller that knows the token amount but not the
    SOL. It solves the constant-product relation and then DELEGATES to
    price_fill, so guards, fee model, reserve/staleness bookkeeping and impact
    accounting are literally the same code path - there is no second pricing
    implementation to drift.

    buy:  dy given -> dx_pool = x*dy/(y-dy) -> dx_total = dx_pool/(1-fee)
    sell: dy given -> price_fill(token_in_raw=dy)
    """
    regime = Regime(regime)
    side = str(side).lower()
    p = params or (bonding_params() if regime == Regime.BONDING_CURVE else amm_params())
    x = int(getattr(reserves, "sol_lamports", 0) or 0)
    y = int(getattr(reserves, "token_raw", 0) or 0)
    dy = int(token_leg_raw or 0)
    if side == "sell":
        return price_fill(regime=regime, side="sell", reserves=reserves,
                          decision_ts_ms=decision_ts_ms, token_in_raw=dy, params=p,
                          max_stale_ms=max_stale_ms,
                          refuse_cross_graduation=refuse_cross_graduation)
    if side != "buy":
        raise ValueError(f"side must be buy|sell, got {side!r}")
    if reserves is None:
        raise ReserveUnavailable("no reserves - refusing to price from tape")
    if dy <= 0 or x <= 0 or y <= 0:
        raise ReserveUnavailable(
            f"unusable reserves order size (x={x}, y={y}, token_leg={dy})")
    if dy >= y:
        raise OrderTooLarge(
            f"token leg {dy} >= token reserve {y} (mint={getattr(reserves, 'mint', '?')})")
    dx_pool = x * dy / (y - dy)                     # exact constant-product inverse
    if regime == Regime.BONDING_CURVE:
        dx_total = dx_pool / (1.0 - float(p.fee_rate))
    else:
        # AMM: price_fill takes dx_total and keeps dx_pool = quote_in*(1+lp)
        # with quote_in = dx_total/(1+lp+extraction). Invert EXACTLY that:
        #   dx_total = dx_pool*(1+lp+extraction)/(1+lp)
        # The earlier form dropped the LP fee from the numerator
        # (dx_pool*(1+extraction)/(1+lp)), which under-priced every AMM fill by
        # exactly the LP fee on the token-leg->SOL-leg replay path. Measured by
        # validate_forward_amm.py on real captured pool reserves: a 0.5 SOL buy
        # round-tripped to 0.498754 SOL (rel 2.49e-3 == the 25 bp LP fee).
        dx_total = dx_pool * (1.0 + float(p.pool_fee_rate) + float(p.trader_extraction_rate)) \
            / (1.0 + float(p.pool_fee_rate))
    return price_fill(regime=regime, side="buy", reserves=reserves,
                      decision_ts_ms=decision_ts_ms,
                      sol_in_lamports=max(1, int(round(dx_total))), params=p,
                      max_stale_ms=max_stale_ms,
                      refuse_cross_graduation=refuse_cross_graduation)


def sol_lamports_for_token_leg(**kw) -> int:
    """Predicted |SOL leg| for a recorded token leg - the replay's prediction."""
    t = fill_from_token_leg(**kw)
    return abs(int(t.sol_delta_lamports))


# ---------------------------------------------------------------------------
# Reserve oracles: decision-time lookup with an audit ledger
# ---------------------------------------------------------------------------
@dataclass
class ReserveJoin:
    """One reserve consultation, recorded for the episode audit trail."""

    mint: str
    regime: str
    account: Optional[str]
    reserve_ts_ms: int
    decision_ts_ms: int
    staleness_ms: int
    source: str
    order_ref: str = ""

    def as_dict(self) -> dict:
        return asdict(self)


class ReserveOracle:
    """Latest reserve state at or before a decision time. Never a future state."""

    name = "oracle"

    def reserve_for(self, mint: str, regime: Regime, decision_ts_ms: int,
                    max_stale_ms: Optional[int] = None,
                    order_ref: str = "") -> ReserveState:
        raise NotImplementedError

    def ledger(self) -> list:
        return []


class DictReserveOracle(ReserveOracle):
    """In-memory per-(mint, regime) time series. The forward-trading oracle.

    Series must be appended in any order; lookups search the time axis, so a
    reserve that arrives late (out of order) is still usable for a LATER decision
    and is never visible to an earlier one.
    """

    name = "dict"

    def __init__(self, series: Optional[dict] = None, default_source: str = "in-memory"):
        self._series: dict = {}
        self._joins: list = []
        self.default_source = default_source
        for k, v in (series or {}).items():
            for s in v:
                self.add(s)

    def add(self, state: ReserveState) -> None:
        key = (state.mint, state.regime)
        lst = self._series.setdefault(key, [])
        lst.append(state)

    def _sorted(self, key):
        lst = self._series.get(key)
        if lst is None:
            return None
        if len(lst) > 1 and any(lst[i].ts_unix_ms > lst[i + 1].ts_unix_ms
                                for i in range(len(lst) - 1)):
            lst.sort(key=lambda s: int(s.ts_unix_ms))
        return lst

    def reserve_for(self, mint: str, regime: Regime, decision_ts_ms: int,
                    max_stale_ms: Optional[int] = None,
                    order_ref: str = "", strict_before: bool = False) -> ReserveState:
        """Newest state at/before the decision. strict_before excludes a state
        stamped at exactly the decision ms (used in replays where our own fill's
        post-state event shares our decision timestamp)."""
        d = int(decision_ts_ms)
        lst = self._sorted((mint, regime))
        if not lst:
            raise ReserveUnavailable(
                f"{self.name}: no {regime.value} reserves held for mint {mint} - "
                f"refusing to price from a tape price")
        # binary search the newest state with ts <= decision
        lo, hi, best = 0, len(lst), None
        while lo < hi:
            mid = (lo + hi) // 2
            ts_mid = int(lst[mid].ts_unix_ms)
            if ts_mid < d or (ts_mid == d and not strict_before):
                best = mid
                lo = mid + 1
            else:
                hi = mid
        if best is None:
            raise ReserveUnavailable(
                f"{self.name}: {regime.value} reserves for {mint} all post-date the "
                f"decision {d} (earliest={lst[0].ts_unix_ms}) - refusing (lookahead)")
        st = lst[best]
        st.checked_for(d, max_stale_ms=max_stale_ms)   # raises ReserveStale
        self._joins.append(ReserveJoin(mint=mint, regime=regime.value, account=st.account,
                                       reserve_ts_ms=int(st.ts_unix_ms), decision_ts_ms=d,
                                       staleness_ms=st.staleness_ms(d),
                                       source=st.source or self.default_source,
                                       order_ref=order_ref))
        return st

    def ledger(self) -> list:
        return list(self._joins)

    def clear_ledger(self) -> None:
        self._joins = []


def slinky_curve_oracle(reserves_dir: str, mints: Optional[Iterable[str]] = None,
                        source: str = "slinky:pump_state_v3") -> DictReserveOracle:
    """BONDING_CURVE oracle from the built slinky reserve parts (read-only)."""
    import glob
    import pyarrow.parquet as pq
    want = set(mints) if mints else None
    o = DictReserveOracle(default_source=source)
    files = sorted(glob.glob(os.path.join(reserves_dir, "*.parquet")))
    if not files:
        raise FileNotFoundError(f"no reserve parquet under {reserves_dir}")
    n = 0
    for f in files:
        t = pq.read_table(f, columns=["mint", "event_time_unix_ms", "venue",
                                      "virtual_sol_reserves_lamports",
                                      "virtual_token_reserves_raw", "is_graduated"])
        d = t.to_pydict()
        for i, m in enumerate(d["mint"]):
            if want is not None and m not in want:
                continue
            vs, vt = d["virtual_sol_reserves_lamports"][i], d["virtual_token_reserves_raw"][i]
            if not vs or not vt or vs <= 0 or vt <= 0:
                continue
            o.add(ReserveState(regime=Regime.BONDING_CURVE, mint=m, sol_lamports=int(vs),
                               token_raw=int(vt), ts_unix_ms=int(d["event_time_unix_ms"][i]),
                               venue=d["venue"][i] or "pumpfun_bonding",
                               account=None, source=source))
            n += 1
    return o


def pool_reserve_oracle(path_glob: str, source: str = "capture:pumpswap_pool_reserves",
                        max_files: int = 0) -> DictReserveOracle:
    """AMM oracle over the pool-reserve artifact the forward capture will write.

    Expected schema (see FORWARD_RESERVE_CAPTURE_PLAN.md):
        mint, pool, event_time_unix_ms, pool_sol_lamports, pool_token_reserves_raw,
        venue, slot
    """
    import glob
    import pyarrow.parquet as pq
    o = DictReserveOracle(default_source=source)
    files = sorted(glob.glob(path_glob))
    if not files:
        raise FileNotFoundError(f"AMM pool-reserve path matched nothing: {path_glob}")
    if max_files:
        files = files[:max_files]
    for f in files:
        t = pq.read_table(f)
        d = t.to_pydict()
        for i, m in enumerate(d["mint"]):
            ps, pt = d["pool_sol_lamports"][i], d["pool_token_reserves_raw"][i]
            if ps is None or pt is None or ps <= 0 or pt <= 0:
                continue
            o.add(ReserveState(regime=Regime.AMM, mint=m, sol_lamports=int(ps),
                               token_raw=int(pt),
                               ts_unix_ms=int(d["event_time_unix_ms"][i]),
                               venue=d.get("venue", ["pumpswap"] * len(d["mint"]))[i],
                               account=d.get("pool", [None] * len(d["mint"]))[i],
                               source=source))
    return o


# ---------------------------------------------------------------------------
# The engine: regime + oracle + staleness budget, with an episode audit trail
# ---------------------------------------------------------------------------
class MechanicsEngine:
    """Regime-aware, reserve-driven quoting with a per-episode reserve ledger."""

    def __init__(self, regime: Regime = Regime.BONDING_CURVE, oracle: Optional[ReserveOracle] = None,
                 params: Optional[MarketParams] = None, priority_fee_lamports: int = 10_000,
                 creator_fee_rate: float = 0.0, max_reserve_stale_ms: int = 60_000,
                 require_reserves: bool = True):
        if regime == Regime.LEGACY_TAPE:
            raise ValueError(
                "MechanicsEngine is mechanics-only; LEGACY_TAPE belongs to RewardEngine")
        self.regime = regime
        self.oracle = oracle
        self.params = params or (bonding_params() if regime == Regime.BONDING_CURVE
                                 else amm_params())
        self.priority_fee_lamports = int(priority_fee_lamports)
        self.priority_fee_sol = self.priority_fee_lamports / LAMPORTS_PER_SOL
        self.creator_fee_rate = float(creator_fee_rate)
        self.max_reserve_stale_ms = int(max_reserve_stale_ms)
        self.require_reserves = bool(require_reserves)
        self.joins: list = []
        self.refusals: list = []

    def _reserves(self, mint: str, decision_ts_ms: int, order_ref: str) -> ReserveState:
        if self.oracle is None:
            if self.require_reserves:
                raise ReserveUnavailable(
                    f"regime={self.regime.value}: no reserve oracle configured - "
                    f"refusing to price {mint} from a tape price")
            raise ReserveUnavailable("no oracle")
        return self.oracle.reserve_for(mint, self.regime, decision_ts_ms,
                                       self.max_reserve_stale_ms, order_ref)

    def quote_buy(self, mint: str, decision_ts_ms: int, q_sol: float, *, order_ref: str = ""):
        if q_sol is None or q_sol <= 0:
            raise ValueError("q_sol must be > 0")
        res = self._reserves(mint, decision_ts_ms, order_ref or "buy")
        t = price_fill(regime=self.regime, side="buy", reserves=res, decision_ts_ms=decision_ts_ms,
                       params=self.params, sol_in_lamports=int(round(q_sol * LAMPORTS_PER_SOL)),
                       max_stale_ms=self.max_reserve_stale_ms)
        self.joins.append(ReserveJoin(mint=mint, regime=self.regime.value, account=res.account,
                                      reserve_ts_ms=t.reserve_ts_ms,
                                      decision_ts_ms=int(decision_ts_ms),
                                      staleness_ms=t.staleness_ms,
                                      source=res.source, order_ref=order_ref or "buy"))
        return t

    def quote_sell(self, mint: str, decision_ts_ms: int, tokens_raw: int, *, order_ref: str = ""):
        res = self._reserves(mint, decision_ts_ms, order_ref or "sell")
        t = price_fill(regime=self.regime, side="sell", reserves=res, decision_ts_ms=decision_ts_ms,
                       params=self.params, token_in_raw=int(tokens_raw),
                       max_stale_ms=self.max_reserve_stale_ms)
        self.joins.append(ReserveJoin(mint=mint, regime=self.regime.value, account=res.account,
                                      reserve_ts_ms=t.reserve_ts_ms,
                                      decision_ts_ms=int(decision_ts_ms),
                                      staleness_ms=t.staleness_ms,
                                      source=res.source, order_ref=order_ref or "sell"))
        return t

    # -- RewardEngine-compatible dicts (same keys the scorer already reads) ----
    def quote_buy_dict(self, mint, decision_ts_ms, q_sol, *, order_ref=""):
        t = self.quote_buy(mint, decision_ts_ms, q_sol, order_ref=order_ref)
        return {"tokens": t.token_delta_raw, "px_ref": t.px_pre, "px_exec": t.px_exec,
                "impact": t.impact_bp / 1e4, "impact_bp": t.impact_bp,
                "fee_sol": (t.fee_lamports + t.order_lamports_in * self.creator_fee_rate)
                           / LAMPORTS_PER_SOL,
                "slippage_sol": t.slippage_lamports / LAMPORTS_PER_SOL,
                "creator_sol": t.order_lamports_in * self.creator_fee_rate / LAMPORTS_PER_SOL,
                "priority_sol": self.priority_fee_sol,
                "depth_sol": t.reserves_used["sol_lamports"] / LAMPORTS_PER_SOL,
                "reserves_used": t.reserves_used, "reserve_ts_ms": t.reserve_ts_ms,
                "staleness_ms": t.staleness_ms, "account": t.account,
                "source": t.source, "regime": t.regime, "k_pre": t.k_pre, "k_post": t.k_post,
                "crosses_graduation": t.crosses_graduation, "terms": t}

    def quote_sell_dict(self, mint, decision_ts_ms, tokens_raw, *, order_ref=""):
        t = self.quote_sell(mint, decision_ts_ms, tokens_raw, order_ref=order_ref)
        return {"sol_out": t.sol_delta_lamports / LAMPORTS_PER_SOL, "px_ref": t.px_pre,
                "px_exec": t.px_exec, "impact": t.impact_bp / 1e4, "impact_bp": t.impact_bp,
                "fee_sol": t.fee_lamports / LAMPORTS_PER_SOL,
                "slippage_sol": t.slippage_lamports / LAMPORTS_PER_SOL,
                "creator_sol": 0.0, "priority_sol": self.priority_fee_sol,
                "gross_sol": t.sol_delta_lamports / LAMPORTS_PER_SOL
                             + t.fee_lamports / LAMPORTS_PER_SOL,
                "depth_sol": t.reserves_used["sol_lamports"] / LAMPORTS_PER_SOL,
                "reserves_used": t.reserves_used, "reserve_ts_ms": t.reserve_ts_ms,
                "staleness_ms": t.staleness_ms, "account": t.account,
                "source": t.source, "regime": t.regime, "k_pre": t.k_pre, "k_post": t.k_post,
                "terms": t}

    def top_up_reserves(self, state: ReserveState) -> None:
        """Feed the oracle a new on-chain reserve snapshot (forward trading)."""
        if self.oracle is None:
            raise ReserveUnavailable("no oracle to top up")
        add = getattr(self.oracle, "add", None)
        if add is None:
            raise TypeError(f"{type(self.oracle).__name__} is read-only; cannot top up")
        add(state)

    def episode_reserve_report(self) -> dict:
        joins = self.joins + list(self.oracle.ledger() if self.oracle else [])
        seen = {}
        for j in joins:
            seen[(j.mint, j.regime, j.reserve_ts_ms, j.decision_ts_ms)] = j
        joins = list(seen.values())
        stale = [j for j in joins if j.staleness_ms < 0]
        return {
            "regime": self.regime.value,
            "reserve_joins": [j.as_dict() for j in joins],
            "reserve_joins_count": len(joins),
            "reserves_used_distinct": len({(j.mint, j.reserve_ts_ms) for j in joins}),
            "max_staleness_ms": max([j.staleness_ms for j in joins], default=None),
            "min_staleness_ms": min([j.staleness_ms for j in joins], default=None),
            "lookahead_joins": len(stale),
            "max_lookahead_ms": max([max(0, -j.staleness_ms) for j in joins], default=0),
            "max_reserve_stale_ms": self.max_reserve_stale_ms,
            "refusals": list(self.refusals),
        }


# ---------------------------------------------------------------------------
# CLI: same flags as reward_engine.py plus the regime selector
# ---------------------------------------------------------------------------
def _selftest() -> int:
    """Mechanics invariants for BOTH regimes. No data files required."""
    checks = []

    def chk(tag, cond):
        checks.append((tag, bool(cond)))
        if not cond:
            raise AssertionError(f"SELFTEST FAILED: {tag}")

    # --- bonding curve, initial state ---
    r0 = ReserveState(regime=Regime.BONDING_CURVE, mint="m", sol_lamports=V0_LAMPORTS,
                      token_raw=T0_RAW, ts_unix_ms=1_700_000_000_000, venue="pumpfun_bonding")
    k0_int = V0_LAMPORTS * T0_RAW
    chk("k0 == V0*T0 (integer form)", int(r0.sol_lamports) * int(r0.token_raw) == k0_int)
    chk("float K0 agrees with integer k0 to 1e-12",
        abs(K0 - k0_int) / k0_int < 1e-12)
    t1 = price_fill(regime=Regime.BONDING_CURVE, side="buy", reserves=r0,
                    decision_ts_ms=1_700_000_000_000, sol_in_lamports=LAMPORTS_PER_SOL)
    chk("buy 1 SOL returns tokens", t1.token_delta_raw > 0)
    chk("buy pays impact", t1.impact_bp > 0)
    chk("buy sol_delta negative", t1.sol_delta_lamports == -LAMPORTS_PER_SOL)
    chk("k does not fall", t1.k_post >= t1.k_pre)
    # impact must scale with OUR OWN order size against the same reserves
    t1b = price_fill(regime=Regime.BONDING_CURVE, side="buy", reserves=r0,
                     decision_ts_ms=1_700_000_000_000, sol_in_lamports=10 * LAMPORTS_PER_SOL)
    chk("10x order -> larger impact", t1b.impact_bp > t1.impact_bp)
    # constant-product, fee-excluded impact: average execution price of an order
    # that puts dx into the curve is (x+dx)/y, so impact == dx/x exactly.
    dx_imp = t1.order_lamports_in - t1.fee_lamports
    an1 = 1e4 * dx_imp / V0_LAMPORTS
    chk("1 SOL impact_only_bp == 1e4*dx/x exactly (%.4f vs %.4f)" % (t1.impact_only_bp, an1),
        abs(t1.impact_only_bp - an1) < 1e-6)
    # and the slippage lamports must equal dx - dy*px_pre
    dy1 = t1.token_delta_raw
    chk("slippage == dx - dy*px_pre", t1.slippage_lamports == dx_imp - int(round(dy1 * t1.px_pre)))
    chk("1e4 bp bound holds at 10x size", t1b.impact_only_bp < 1e4)
    s1 = price_fill(regime=Regime.BONDING_CURVE, side="sell", reserves=r0,
                    decision_ts_ms=1_700_000_000_000, token_in_raw=T0_RAW // 1000)
    chk("sell returns sol", s1.sol_delta_lamports > 0)
    chk("sell pays impact", s1.impact_bp > 0)
    chk("sell fee on the SOL leg", s1.fee_lamports > 0)
    # graduation refusal
    t_grad = price_fill(regime=Regime.BONDING_CURVE, side="buy",
                        reserves=ReserveState(regime=Regime.BONDING_CURVE, mint="m",
                                              sol_lamports=V_GRAD_LAMPORTS - 1_000_000,
                                              token_raw=T0_RAW,
                                              ts_unix_ms=1_700_000_000_000),
                        decision_ts_ms=1_700_000_000_000,
                        sol_in_lamports=LAMPORTS_PER_SOL)
    chk("buy across graduation is flagged", t_grad.crosses_graduation)
    try:
        price_fill(regime=Regime.BONDING_CURVE, side="buy", reserves=r0,
                   decision_ts_ms=1_700_000_000_000, sol_in_lamports=LAMPORTS_PER_SOL,
                   params=bonding_params(), refusal_unknown=True)  # type: ignore[call-arg]
        chk("unknown kwarg rejected", False)
    except TypeError:
        chk("unknown kwarg rejected", True)

    # --- AMM ---
    pool = ReserveState(regime=Regime.AMM, mint="m", sol_lamports=100 * LAMPORTS_PER_SOL,
                        token_raw=10 ** 15, ts_unix_ms=1_700_000_000_000, venue="pumpswap",
                        account="POOLBASE...")
    a1 = price_fill(regime=Regime.AMM, side="buy", reserves=pool,
                    decision_ts_ms=1_700_000_000_000, sol_in_lamports=LAMPORTS_PER_SOL)
    chk("AMM buy returns tokens", a1.token_delta_raw > 0)
    chk("AMM buy pays impact", a1.impact_bp > 0)
    chk("AMM buy fee > 0", a1.fee_lamports > 0)
    chk("AMM k does not fall", a1.k_post >= a1.k_pre)
    a2 = price_fill(regime=Regime.AMM, side="buy", reserves=pool,
                    decision_ts_ms=1_700_000_000_000, sol_in_lamports=2 * LAMPORTS_PER_SOL)
    chk("AMM impact grows with our size", a2.impact_bp > a1.impact_bp)
    chk("AMM order of same SOL gets fewer tokens than curve? (depth differs)",
        a1.token_delta_raw != t1.token_delta_raw)
    a3 = price_fill(regime=Regime.AMM, side="sell", reserves=pool,
                    decision_ts_ms=1_700_000_000_000, token_in_raw=10 ** 12)
    chk("AMM sell returns sol", a3.sol_delta_lamports > 0)
    chk("AMM sell pays impact", a3.impact_bp > 0)

    # --- refusals: the whole point ---
    for tag, kw in (
        ("no reserves -> ReserveUnavailable", dict(regime=Regime.BONDING_CURVE, side="buy",
                                                  sol_in_lamports=10 ** 9)),
        ("no reserves AMM -> ReserveUnavailable", dict(regime=Regime.AMM, side="sell",
                                                       token_in_raw=10 ** 6)),
    ):
        try:
            price_fill(reserves=None, decision_ts_ms=1_700_000_000_000, **kw)
            chk(tag, False)
        except ReserveUnavailable:
            chk(tag, True)
    try:
        price_fill(regime=Regime.AMM, side="buy", reserves=r0,
                   decision_ts_ms=1_700_000_000_000, sol_in_lamports=10 ** 9)
        chk("curve reserves on an AMM market -> RegimeMismatch", False)
    except RegimeMismatch:
        chk("curve reserves on an AMM market -> RegimeMismatch", True)
    try:
        price_fill(regime=Regime.BONDING_CURVE, side="buy", reserves=pool,
                   decision_ts_ms=1_700_000_000_000, sol_in_lamports=10 ** 9)
        chk("pool reserves on a curve market -> RegimeMismatch", False)
    except RegimeMismatch:
        chk("pool reserves on a curve market -> RegimeMismatch", True)
    future = ReserveState(regime=Regime.BONDING_CURVE, mint="m", sol_lamports=V0_LAMPORTS,
                          token_raw=T0_RAW, ts_unix_ms=1_700_000_000_001)
    try:
        price_fill(regime=Regime.BONDING_CURVE, side="buy", reserves=future,
                   decision_ts_ms=1_700_000_000_000, sol_in_lamports=10 ** 9)
        chk("future-dated reserves -> ReserveLookahead", False)
    except ReserveLookahead:
        chk("future-dated reserves -> ReserveLookahead", True)
    try:
        price_fill(regime=Regime.BONDING_CURVE, side="buy", reserves=r0,
                   decision_ts_ms=1_700_000_000_000 + 120_000, sol_in_lamports=10 ** 9,
                   max_stale_ms=30_000)
        chk("stale reserves -> ReserveStale", False)
    except ReserveStale:
        chk("stale reserves -> ReserveStale", True)
    try:
        price_fill(regime=Regime.BONDING_CURVE, side="buy", reserves=r0,
                   decision_ts_ms=1_700_000_000_000,
                   sol_in_lamports=int(r0.sol_lamports * 2))
        chk("order larger than reserves -> OrderTooLarge", False)
    except OrderTooLarge:
        chk("order larger than reserves -> OrderTooLarge", True)
    for bad in (dict(sol_lamports=0, token_raw=T0_RAW), dict(sol_lamports=V0_LAMPORTS,
                                                            token_raw=None)):
        try:
            ReserveState(regime=Regime.BONDING_CURVE, mint="m", ts_unix_ms=1,
                         **bad)
            chk(f"bad reserve state refused {bad}", False)
        except ReserveUnavailable:
            chk(f"bad reserve state refused {bad}", True)

    # --- oracle causality + ledger ---
    o = DictReserveOracle()
    o.add(ReserveState(regime=Regime.BONDING_CURVE, mint="m", sol_lamports=V0_LAMPORTS,
                       token_raw=T0_RAW, ts_unix_ms=1000, source="t"))
    o.add(ReserveState(regime=Regime.BONDING_CURVE, mint="m", sol_lamports=V0_LAMPORTS * 2,
                       token_raw=T0_RAW, ts_unix_ms=2000, source="t"))
    got = o.reserve_for("m", Regime.BONDING_CURVE, 1500)
    chk("oracle returns the decision-time state, not the future one",
        int(got.ts_unix_ms) == 1000 and int(got.sol_lamports) == V0_LAMPORTS)
    got2 = o.reserve_for("m", Regime.BONDING_CURVE, 2500)
    chk("oracle advances", int(got2.ts_unix_ms) == 2000)
    chk("oracle ledger records both joins", len(o.ledger()) == 2)
    try:
        o.reserve_for("m", Regime.BONDING_CURVE, 500)
        chk("oracle refuses before its first state", False)
    except ReserveUnavailable:
        chk("oracle refuses before its first state", True)
    try:
        o.reserve_for("nope", Regime.BONDING_CURVE, 5000)
        chk("oracle refuses an unknown mint", False)
    except ReserveUnavailable:
        chk("oracle refuses an unknown mint", True)
    eng = MechanicsEngine(regime=Regime.BONDING_CURVE, oracle=o, max_reserve_stale_ms=10_000)
    eng.quote_buy("m", 2500, 0.05)
    rep = eng.episode_reserve_report()
    chk("episode report exposes reserves + staleness",
        rep["reserve_joins_count"] >= 1 and rep["max_staleness_ms"] is not None)
    chk("no lookahead joins", rep["lookahead_joins"] == 0)

    n = sum(1 for _, ok in checks if ok)
    print(json.dumps({"regime": "both", "checks": n, "failed": 0,
                      "checks_detail": [{"tag": t, "ok": ok} for t, ok in checks]},
                     indent=1))
    return 0


def to_sol_checked(lamports, what="value"):
    """Integer lamports -> SOL, refusing anything that is not integer lamports.

    Local to this module on purpose: regime_pricing must not import reward_engine
    (reward_engine imports this module).
    """
    try:
        v = int(lamports)
    except (TypeError, ValueError):
        raise UnitError(f"{what}: {lamports!r} is not integer lamports")
    if v != lamports:
        raise UnitError(f"{what}: non-integer lamports {lamports!r} (SOL/lamport mix-up?)")
    return v / LAMPORTS_PER_SOL


def build_curve_oracle(paths) -> DictReserveOracle:
    """Decision-time BONDING_CURVE oracle from the reconstructed reserve tables.

    Every row of a v2/reserves `*_all.parquet` is one pump.fun TradeEvent: the
    POST state of the curve at `recv_unix_ms`. A decision at time t therefore
    reads the newest POST state with ts < t (strict_before=True), which is the
    pre-state of the next fill - i.e. exactly what a live trader sees. Only
    rows whose reserves are positive are loaded.
    """
    import glob
    import pyarrow.parquet as pq
    files = []
    for p in paths:
        files.extend(sorted(glob.glob(p)) if any(c in p for c in "*?[") else [p])
    if not files:
        raise ReserveUnavailable("build_curve_oracle: no reserve parquet resolved")
    o = DictReserveOracle(default_source="reconstructed_curve_reserves")
    n = 0
    for f in files:
        t = pq.read_table(f, columns=["mint_b58", "recv_unix_ms",
                                      "virtual_sol_reserves_lamports",
                                      "virtual_token_reserves_raw", "is_buy"])
        mi = t["mint_b58"].to_pylist()
        ts = t["recv_unix_ms"].to_numpy(zero_copy_only=False).astype("int64")
        vs = t["virtual_sol_reserves_lamports"].to_numpy(zero_copy_only=False).astype("int64")
        vt = t["virtual_token_reserves_raw"].to_numpy(zero_copy_only=False).astype("int64")
        for i, m in enumerate(mi):
            if int(vs[i]) <= 0 or int(vt[i]) <= 0:
                continue
            o.add(ReserveState(regime=Regime.BONDING_CURVE, mint=m, sol_lamports=int(vs[i]),
                               token_raw=int(vt[i]), ts_unix_ms=int(ts[i]),
                               venue="pumpfun_bonding", source="reconstructed_curve_reserves"))
            n += 1
    o.loaded_rows = n
    return o


def reconcile_mechanics(trade_path="/training/v2/canonical/renorm_pooltest/trades.jsonl",
                        oracle=None, regime=Regime.BONDING_CURVE, min_fills=2,
                        max_stale_ms=DEFAULT_MAX_STALE_MS_PRICING, params=None,
                        sample_mints=None, stratum="curve", venue=None) -> dict:
    """Predictive replay where EVERY fill is priced from decision-time reserves.

    Mirrors the legacy strata accounting (episode net SOL, corr, per-fill abs
    error) so the numbers are directly comparable, but the price of each fill is
    now produced by `price_fill` from (a) the newest reserve state strictly
    before that fill's own timestamp and (b) that fill's OWN recorded size.
    Fills with no reserves are counted as REFUSED - they never fall back to tape.
    """
    import collections
    params = params or (bonding_params() if regime == Regime.BONDING_CURVE else amm_params())
    rows = [json.loads(l) for l in open(trade_path)]
    bymint = collections.defaultdict(list)
    for r in rows:
        bymint[r["mint"]].append(r)
    mints = sorted(bymint)
    if sample_mints:
        mints = mints[:sample_mints]
    rec_net, sim_net, per_fill = [], [], []
    fills = refused_fills = 0
    staleness = []
    episodes_refused = 0
    first_refusal = None
    for m in mints:
        tr = sorted(bymint[m], key=lambda r: (r["recv_unix_ms"], r["signature"]))
        if len({r["venue"] for r in tr}) > 1:
            continue
        if venue and tr[0]["venue"] != venue:
            continue
        bytr = collections.defaultdict(list)
        for r in tr:
            bytr[r["trader"]].append(r)
        for trader, ep in bytr.items():
            if len(ep) < min_fills:
                continue
            ep = sorted(ep, key=lambda r: r["recv_unix_ms"])
            rec = 0
            sim = 0.0
            le = []
            ok = True
            for r in ep:
                t = int(r["recv_unix_ms"])
                tok = abs(int(r["tokens_raw"]))
                if not tok:
                    ok = False
                    break
                try:
                    st = oracle.reserve_for(m, regime, t, max_stale_ms,
                                            order_ref=r.get("signature") or "", strict_before=True)
                    p = price_fill_lamports_per_raw_token(
                        regime=regime, side=r["side"], reserves=st,
                        token_amount_raw=tok, decision_ts_ms=t, params=params,
                        max_stale_ms=max_stale_ms)
                    staleness.append(st.staleness_ms(t))
                except (ReserveUnavailable, ReserveStale, RegimeMismatch, OrderTooLarge) as e:
                    refused_fills += 1
                    if first_refusal is None:
                        first_refusal = f"{type(e).__name__}: {e}"[:300]
                    if stratum == "curve":
                        ok = False
                        break
                    continue
                pred = to_sol_checked(int(round(tok * p)), "pred")
                sim += (-1.0 if r["side"] == "buy" else 1.0) * pred
                rec += int(r["sol_lamports"])
                le.append(abs(pred - abs(int(r["sol_lamports"])) / LAMPORTS_PER_SOL))
                fills += 1
            if not ok or not le:
                if not ok:
                    episodes_refused += 1
                continue
            rec_net.append(to_sol_checked(rec, "rec"))
            sim_net.append(sim)
            per_fill.append(float(np.mean(le)))
    rec_net = np.array(rec_net)
    sim_net = np.array(sim_net)
    err = np.abs(sim_net - rec_net)
    import statistics as _st
    return {
        "stratum": stratum, "regime": regime.value, "venue": venue or "all",
        "episodes_replayed": int(rec_net.size), "fills_replayed": fills,
        "fills_refused_no_reserves": refused_fills,
        "episodes_refused_no_reserves": episodes_refused,
        "first_refusal": first_refusal,
        "corr_recorded_vs_sim": (round(float(np.corrcoef(rec_net, sim_net)[0, 1]), 6)
                                 if rec_net.size > 1 else None),
        "mean_abs_err_sol": round(float(err.mean()), 6) if err.size else None,
        "p90_abs_err_sol": round(float(np.percentile(err, 90)), 6) if err.size else None,
        "mean_abs_err_sol_per_fill": round(float(np.mean(per_fill)), 6) if per_fill else None,
        "reserve_staleness_ms": {
            "n": len(staleness),
            "p50": int(_st.median(staleness)) if staleness else None,
            "max": int(max(staleness)) if staleness else None,
        } if staleness else None,
        "max_reserve_stale_ms": max_stale_ms,
        "fee_rate_used": float(params.fee_rate),
        "note": ("every fill priced from the newest reserve state STRICTLY before its "
                 "own timestamp, with that fill's own recorded size; fills without "
                 "reserves are refused, never tape-priced"),
    }


def _cli():
    ap = argparse.ArgumentParser(description="mechanics-first regime pricing")
    ap.add_argument("--selftest", action="store_true")
    ap.add_argument("--reconcile-mechanics", action="store_true",
                    help="predictive replay where every fill is priced from "
                         "decision-time reserves (never from the tape)")
    ap.add_argument("--reserves-curve", nargs="*", default=None,
                    help="reconstructed curve-reserve parquet files/dirs for the "
                         "BONDING_CURVE oracle (v2/reserves/*/*_all.parquet)")
    ap.add_argument("--trade-path",
                    default="/training/v2/canonical/renorm_pooltest/trades.jsonl")
    ap.add_argument("--conserve", action="store_true", help="delegates to reward_engine")
    ap.add_argument("--reconcile", action="store_true", help="delegates to reward_engine")
    ap.add_argument("--measure-depth", action="store_true", help="delegates to reward_engine")
    ap.add_argument("--reserves", nargs="*", default=None)
    ap.add_argument("--regime", default="bonding_curve",
                    choices=["legacy_tape", "bonding_curve", "amm"])
    ap.add_argument("--max-reserve-stale-ms", type=int, default=60_000)
    ap.add_argument("--amm-fee", type=float, default=AMM_LP_FEE_RATE)
    ap.add_argument("--out")
    a, rest = ap.parse_known_args()

    if a.reconcile_mechanics:
        if not a.reserves_curve:
            raise SystemExit("--reconcile-mechanics needs --reserves-curve")
        oracle = build_curve_oracle(list(a.reserves_curve))
        out = {"oracle_rows": getattr(oracle, "loaded_rows", None),
               "trade_path": a.trade_path, "max_stale_ms": a.max_reserve_stale_ms}
        for reg in ("bonding_curve", "amm"):
            R = Regime(reg)
            if R is Regime.AMM:
                # no historical pumpswap pool levels exist anywhere in the corpus:
                # the AMM path is exercised by unit tests + the forward capture.
                out["amm"] = {"status": "not_attempted",
                              "why": "no historical pumpswap pool reserves in the corpus"}
                continue
            for strat in ("curve", "all"):
                out["%s_%s" % (reg, strat)] = reconcile_mechanics(
                    a.trade_path, oracle, regime=R, max_stale_ms=a.max_reserve_stale_ms,
                    stratum=strat)
        print(json.dumps(out, indent=1, default=str))
        if a.out:
            json.dump(out, open(a.out, "w"), indent=1, default=str)
        return 0
    if not any([a.selftest, a.conserve, a.reconcile, a.measure_depth, a.reconcile_mechanics]):
        a.selftest = True
    if a.selftest:
        return _selftest()
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import reward_engine as RE
    argv = ["reward_engine.py"] + [x for x in rest]
    if a.conserve:
        argv.append("--conserve")
    if a.reconcile:
        argv.append("--reconcile")
    if a.measure_depth:
        argv.append("--measure-depth")
    if a.reserves:
        argv += ["--reserves"] + list(a.reserves)
    if a.out:
        argv += ["--out", a.out]
    argv += ["--regime", a.regime]
    sys.argv = argv
    return RE.main()


if __name__ == "__main__":
    sys.exit(_cli())
