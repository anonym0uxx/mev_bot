#!/usr/bin/env python3
"""
Monte Carlo harness v2 — Hermes memecoin scalper design validation (net-SOL only).

Fixes v1's two artifacts:
  * signal test now injects WASH TRADING (the real reason CVD/OFI beat the proxy:
    a manipulator posts buy-heavy round-trip wash to fake buy-pressure; the proxy
    buy/(buy+sell) is fooled, signed CVD nets ~0). We measure IC AND the net-SOL of
    the trades each signal actually takes (false pumps hurt the proxy).
  * exit test: lifecycle triggers act on SMOOTHED flow/price (not raw-tick noise),
    trailing is the primary winner-harvester, and BOTH policies are path-aware and
    face the SAME rug gaps. Baseline = hold-to-fixed-target-or-timeout at real price.

Offline research (float ok); production stays integer (§22). Base rates calibrated
to pump.fun literature: ~50% rug/gap (mostly unexitable), ~34% bleed, ~16% runner.
"""
import numpy as np
from scipy.stats import spearmanr

RNG = np.random.default_rng(20260722)

FEE_BPS = 100
TIP_LAMPORTS = 20_000
FIRST_SELL_PENALTY_BPS = 150
IMPACT_K_BPS = 50
NOTIONAL = 1_000_000
DEPTH = 120_000_000
WARMUP = 30
TAPE_LEN = 240
FIXED_TARGET = 0.30      # baseline take-profit +30%
FIXED_TIMEOUT = 45       # baseline max hold (trades)

P_RUG, P_BLEED, P_RUN = 0.50, 0.34, 0.16
N_TOKENS = 24_000


def simulate_tape(regime, wash_intensity):
    T = TAPE_LEN
    u = np.zeros(T); u[0] = RNG.normal()
    rho = 0.82
    for t in range(1, T):
        u[t] = rho*u[t-1] + RNG.normal()*np.sqrt(1-rho**2)
    r = np.zeros(T)
    meta = {"gap_t": None}
    if regime == "RUG":
        gap_t = int(RNG.integers(WARMUP+8, T-5))
        for t in range(1, T):
            r[t] = 0.010*max(u[t], 0) + 0.05*RNG.normal()
        price = 100*np.exp(np.cumsum(r))
        tail = np.cumsum(RNG.normal(-0.02, 0.03, T-gap_t))
        price[gap_t:] = price[gap_t]*0.08*np.exp(tail)
        meta["gap_t"] = gap_t
    elif regime == "BLEED":
        for t in range(1, T):
            r[t] = -0.004 + 0.03*u[t] + 0.045*RNG.normal()
        price = 100*np.exp(np.cumsum(r))
    else:  # RUN
        fade = int(RNG.integers(WARMUP+40, T))
        for t in range(1, T):
            d = 0.016 if t < fade else -0.012
            r[t] = d*(1+0.5*max(u[t], 0)) + 0.055*RNG.normal()
        price = 100*np.exp(np.cumsum(r))

    fwd = np.append(np.diff(np.log(price)), 0.0)
    informed = np.tanh(6*fwd)*RNG.uniform(0.5, 1.5, T)
    base_size = RNG.uniform(0.5, 2.0, T)
    signed = informed*base_size                         # true signed flow (CVD sees this)

    # wash: sign-balanced volume that TILTS the visible buy/sell SHARE (fakes buy
    # pressure) but nets ~0 in signed flow. Heavier on RUG tokens (manipulated pumps).
    wash = np.abs(RNG.normal(0, 1.0, T))*base_size*wash_intensity
    wash_buy_tilt = 0.72                                # manipulator fakes buys
    buy_vol = np.clip(signed, 0, None) + wash*wash_buy_tilt
    sell_vol = np.clip(-signed, 0, None) + wash*(1-wash_buy_tilt)
    return price, signed, buy_vol, sell_vol, meta


def signals_at(price, signed, buy_vol, sell_vol, t0, win=25):
    lo = max(0, t0-win)
    p = price[lo:t0+1]; sg = signed[lo:t0+1]
    bv = buy_vol[lo:t0+1].sum(); sv = sell_vol[lo:t0+1].sum()
    proxy = (bv-sv)/(bv+sv+1e-9)                         # buy-pressure proxy (wash-fooled)
    cvd = sg.sum(); cvd_slope = cvd/(np.abs(sg).sum()+1e-9)
    ofi = cvd_slope
    vwap = np.average(p)
    vwap_dev = (price[t0]-vwap)/(vwap+1e-9)
    dprice = price[t0]-price[lo]
    diverge_bearish = (dprice > 0) and (cvd < 0)
    combo = cvd_slope if (cvd_slope > 0.15 and vwap_dev > -0.02 and not diverge_bearish) else 0.0
    return dict(proxy=proxy, cvd=cvd_slope, ofi=ofi, vwap_dev=vwap_dev,
                combo=combo, diverge_bearish=diverge_bearish)


def fwd_return(price, t0, k=40):
    t1 = min(len(price)-1, t0+k)
    return (price[t1]-price[t0])/(price[t0]+1e-9)


def rt_cost(notional):
    fee = notional*FEE_BPS/10000*2
    impact = notional*(notional*IMPACT_K_BPS/DEPTH)/10000
    return fee + impact + TIP_LAMPORTS*2


def exit_fixed(price, t_entry, meta):
    """Baseline: hold until +FIXED_TARGET or FIXED_TIMEOUT, exit at real price;
    a rug gap inside the hold is unexitable (terminal)."""
    entry = price[t_entry]; gap = meta["gap_t"]
    t_exit = min(len(price)-1, t_entry+FIXED_TIMEOUT)
    hit = None
    for t in range(t_entry+1, t_exit+1):
        if gap is not None and t >= gap:
            hit = ("rug", t); break
        if price[t]/entry - 1 >= FIXED_TARGET:
            hit = ("tp", t); break
    if hit is None:
        mult = price[t_exit]/entry
    elif hit[0] == "rug":
        mult = 0.02
    else:
        mult = price[hit[1]]/entry
    gross = NOTIONAL*mult
    pen = NOTIONAL*FIRST_SELL_PENALTY_BPS/10000
    return gross - NOTIONAL - rt_cost(NOTIONAL) - pen


def exit_lifecycle(price, signed, t_entry, meta):
    """OPEN->UPDATE->CLOSE. Triggers on SMOOTHED signals; trailing harvests runners;
    precursor+thesis cut the left tail early."""
    entry = price[t_entry]; gap = meta["gap_t"]; T = len(price)
    peak = entry; cvd = 0.0; cvd_peak = 0.0
    ps = entry                       # EWMA smoothed price
    last_high = t_entry; remaining = 1.0; realized = 0.0
    cost = NOTIONAL + TIP_LAMPORTS
    tr = [False, False, False]; took_first = False

    def sell(frac, mult, first):
        nonlocal realized, remaining
        n = NOTIONAL*frac; gross = n*mult
        fee = gross*FEE_BPS/10000
        pen = n*FIRST_SELL_PENALTY_BPS/10000 if first else 0.0
        impact = n*(n*IMPACT_K_BPS/DEPTH)/10000
        realized += gross - fee - pen - impact - TIP_LAMPORTS
        remaining -= frac

    for t in range(t_entry+1, T):
        p = price[t]; mult = p/entry
        cvd += signed[t]; cvd_peak = max(cvd_peak, cvd)
        ps = 0.6*ps + 0.4*p                                  # smoothed price
        if p > peak: peak = p; last_high = t

        # P0 rug precursor: onset of collapse (big single-step drop) or at the gap
        if gap is not None and t >= gap:
            sell(remaining, max(mult*0.55, 0.05), not took_first); remaining = 0; break
        if t > t_entry+1 and p/price[t-1]-1 < -0.30:
            sell(remaining, mult*0.72, not took_first); remaining = 0; break

        # P2 thesis-invalidation on SMOOTHED evidence: flow rolled over hard AND
        # smoothed price below peak (momentum genuinely dead), or long stall in profit
        if cvd_peak > 0 and cvd < 0.45*cvd_peak and ps < peak*0.97:
            sell(remaining, mult, not took_first); remaining = 0; break
        if mult > 1.0 and (t-last_high) >= 30:
            sell(remaining, mult, not took_first); remaining = 0; break

        # P3 principal-recovery ladder
        if mult >= 1.35 and not tr[0]:
            frac = min(remaining, cost/(NOTIONAL*mult)); sell(frac, mult, not took_first); took_first = True; tr[0] = True
        if mult >= 2.5 and not tr[1]:
            sell(min(remaining, 0.30), mult, not took_first); took_first = True; tr[1] = True
        if mult >= 5.0 and not tr[2]:
            sell(min(remaining, 0.30), mult, not took_first); took_first = True; tr[2] = True

        # P4 vol-scaled trailing (primary winner harvester; widens as it runs)
        trail = min(0.55, max(0.22, (peak/entry-1.0)/3.5))
        if remaining > 0 and p <= peak*(1-trail):
            sell(remaining, mult, not took_first); remaining = 0; break

        # P5 time-stop only when not advancing
        if remaining > 0 and (t-last_high) >= 25 and (t-t_entry) >= 70:
            sell(remaining, mult, not took_first); remaining = 0; break
        if remaining <= 1e-9: break

    if remaining > 1e-9:
        mult = 0.02 if gap is not None else price[min(T-1, t_entry+90)]/entry
        sell(remaining, mult, not took_first)
    return realized - cost


def entry_fires(s, which):
    if which == "proxy": return s["proxy"] > 0.15
    if which == "cvd":   return s["cvd"] > 0.15 and not s["diverge_bearish"]
    if which == "ofi":   return s["ofi"] > 0.15 and not s["diverge_bearish"]
    if which == "vwap":  return s["vwap_dev"] > 0.0 and s["cvd"] > 0.0
    if which == "combo": return s["combo"] > 0.15
    return False


def run(wash_lo, wash_hi, label):
    regimes = RNG.choice(["RUG","BLEED","RUN"], N_TOKENS, p=[P_RUG,P_BLEED,P_RUN])
    ic = {k: [] for k in ["proxy","cvd","ofi","vwap_dev","combo"]}; fwd = []
    entries = ["proxy","cvd","ofi","vwap","combo"]
    net = {(e,x): [] for e in entries for x in ["fixed","lifecycle"]}
    for i in range(N_TOKENS):
        # wash heavier on RUG (manipulated pumps), light elsewhere
        wi = RNG.uniform(wash_lo, wash_hi) * (2.2 if regimes[i]=="RUG" else 0.6)
        price, signed, bv, sv, meta = simulate_tape(regimes[i], wi)
        t0 = WARMUP
        s = signals_at(price, signed, bv, sv, t0)
        fwd.append(fwd_return(price, t0))
        for k in ic: ic[k].append(s[k])
        for e in entries:
            if entry_fires(s, e):
                net[(e,"fixed")].append(exit_fixed(price, t0, meta))
                net[(e,"lifecycle")].append(exit_lifecycle(price, signed, t0, meta))
    fwd = np.array(fwd)
    print("\n" + "="*76)
    print(f"SCENARIO: {label}")
    print("-"*76)
    print("Q1  Signal IC (Spearman vs fwd return) + net-SOL of trades it takes:")
    for k in ["proxy","cvd","ofi","vwap_dev","combo"]:
        icv, _ = spearmanr(np.array(ic[k]), fwd)
        tag = "  <-- current proxy" if k=="proxy" else ""
        print(f"    {k:9s} IC={icv:+.4f}{tag}")

    def st(v):
        a = np.array(v) if v else np.array([0.0])
        cvar = np.mean(np.sort(a)[:max(1,len(a)//20)])
        return len(a), a.mean(), (a>0).mean()*100, cvar, a.sum()
    print("\nQ2  net SOL by ENTRY x EXIT (lamports):")
    print(f"    {'entry':7s}{'exit':11s}{'trades':>7s}{'mean':>12s}{'win%':>7s}{'CVaR5%':>12s}{'TOTAL':>15s}")
    for e in entries:
        for x in ["fixed","lifecycle"]:
            n,m,w,cv,tot = st(net[(e,x)])
            print(f"    {e:7s}{x:11s}{n:7d}{m:12.0f}{w:7.1f}{cv:12.0f}{tot:15.0f}")
    b = np.array(net[("proxy","fixed")]).sum()
    full = np.array(net[("combo","lifecycle")]).sum()
    print(f"\n    baseline (proxy+fixed) TOTAL   = {b:15.0f}")
    print(f"    FULL     (combo+lifecycle)     = {full:15.0f}   delta={full-b:+.0f}")
    # tail: worst-5% mean, baseline vs full
    def cvar(v): a=np.array(v); return np.mean(np.sort(a)[:max(1,len(a)//20)])
    print(f"    left-tail CVaR5%: baseline={cvar(net[('proxy','fixed')]):.0f}  "
          f"full={cvar(net[('combo','lifecycle')]):.0f}")


if __name__ == "__main__":
    run(0.0, 0.0, "NO wash (clean tape) — isolates path/exit effects")
    run(0.6, 1.4, "WITH wash trading (manipulated pumps) — realistic pump.fun")
