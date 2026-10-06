"""Inverse-probability-of-censoring weighting (IPCW) for horizon-censored barrier rows.

WHY THIS EXISTS (M8). Every barrier row is observed over
``[t_entry, min(t_entry + H, tape_end)]`` where ``H`` is the vertical barrier (the horizon).
The observation can end two ways:

  * **EVENT** — the price TOUCHED a barrier (TP or SL). The terminal outcome is OBSERVED at a
    known time, so the row needs no correction.
  * **CENSORED** — the window ended first (TIMEOUT at the horizon, or the tape ran out) without
    touching a barrier. The row's recorded return is the mark at censoring, NOT the process's
    terminal value. Averaging observed returns over ALL rows therefore samples a distribution
    in which long, tail-forming paths are systematically truncated: the reward over-weights or
    under-weights the tail depending on the tail's sign.

The standard correction (Horvitz-Thompson under right censoring) is to re-weight each CENSORED
row by ``1 / G(t_i)`` where ``G(t) = P(censoring time > t)`` is the Kaplan-Meier estimate of the
CENSORING survival function — the complement of the usual event survival, obtained by swapping
the roles of event and censoring.

    EV_ipcw = sum_i w_i * r_i / sum_i w_i ,   w_i = 1              for an EVENT row
                                              w_i = 1 / G(t_i)     for a CENSORED row

SCOPE. This corrects the ESTIMATOR only. The horizon ``H`` is deliberately left unchanged: its
value is a separate, owner-ruled decision (M7). Nothing here reads or writes ``H``.

INTEGER/UNITS. Returns ``r`` are dimensionless per-unit-notional fractions (the convention
``barrier_vs_action`` already uses). Times are milliseconds since entry. Weights are floats by
construction (a probability reciprocal); the WEIGHTED SUM is a statistic, not a money path, so
no lamport rounding happens here. Money paths elsewhere stay integer.
"""
import bisect
import math

__all__ = [
    "kaplan_meier_censoring_survival",
    "ipcw_weights",
    "ev_ipcw",
    "censoring_events",
    "DEFAULT_MAX_WEIGHT",
]

# Cap a single censored row's weight. At the last censoring time G can be small, and 1/G can
# explode on a thin stratum; an unbounded weight would let one row dominate the mean. The cap
# is reported (``capped_rows``) rather than hidden, so a stratum that hits it is visible.
DEFAULT_MAX_WEIGHT = 20.0


def censoring_events(outcomes):
    """The right-censoring INDICATOR, derived from barrier outcomes.

    A row is right-censored iff NO barrier was touched within the observed window
    (``timeout`` = the horizon elapsed; ``censored`` = the tape ran out first). TP/SL rows are
    events even when the row's own ``censored`` field is True (the tape may end after the
    barrier was touched, which does not censor the outcome).

    This is deliberately NOT the row's ``censored`` JSON field: that flag marks "window cut
    short", which is a different thing from "outcome not observed".
    """
    c = []
    for o in outcomes:
        if o in ("tp", "sl"):
            c.append(False)
        elif o in ("timeout", "censored"):
            c.append(True)
        else:
            raise ValueError("outcome %r is neither an event nor a censoring: "
                             "filter unpriced rows before calling" % (o,))
    return c


def kaplan_meier_censoring_survival(times, censored):
    """Kaplan-Meier estimate of the CENSORING survival function ``G(t)``.

    ``times``   observation end time, ms since entry (event time for an event row, censoring
                time for a censored row).
    ``censored`` True where the row is right-censored (see :func:`censoring_events`).

    The roles are SWAPPED relative to a standard K-M: a censored row is the "event" whose
    survival we estimate, and an event row leaves the risk set as a censored observation.

    Returns a dict ``{t: G_at_t}`` for every distinct time with a censoring event, plus the
    terminal time. ``G`` is a non-increasing step function; look it up at an arbitrary time
    with :func:`g_at` below (or the module's ``_G_at``).
    """
    if len(times) != len(censored):
        raise ValueError("times and censored must be the same length")
    n = len(times)
    if n == 0:
        return {}
    order = sorted(range(n), key=lambda i: times[i])
    ts = [int(times[i]) for i in order]
    cs = [bool(censored[i]) for i in order]

    at_risk = n
    g = 1.0
    out = {}
    i = 0
    while i < n:
        t = ts[i]
        j = i
        d = 0
        while j < n and ts[j] == t:
            if cs[j]:
                d += 1
            j += 1
        if d > 0:
            g *= (1.0 - d / at_risk)
        out[t] = g
        at_risk -= (j - i)
        i = j
    return out


def _g_at(gdict, keys, t):
    """``G(t)`` = the step value at the largest key <= t (1.0 before the first key)."""
    k = bisect.bisect_right(keys, t) - 1
    if k < 0:
        return 1.0
    return gdict[keys[k]]


def ipcw_weights(times, censored, max_weight=DEFAULT_MAX_WEIGHT):
    """Per-row IPCW weight: 1 for an event row, ``1/G(t_i)`` (capped) for a censored row.

    Returns ``(weights, diag)`` where ``diag`` carries the KM curve and the diagnostics a
    report must quote (censored fraction, capped count, min G).
    """
    if len(times) != len(censored):
        raise ValueError("times and censored must be the same length")
    gdict = kaplan_meier_censoring_survival(times, censored)
    keys = sorted(gdict)
    w = []
    capped = 0
    for t, c in zip(times, censored):
        if not c:
            w.append(1.0)
            continue
        g = _g_at(gdict, keys, int(t))
        if g <= 0.0:
            wi = max_weight
            capped += 1
        else:
            wi = 1.0 / g
            if wi > max_weight:
                wi = max_weight
                capped += 1
        w.append(wi)
    n = len(times)
    n_cens = sum(1 for c in censored if c)
    diag = {
        "n": n,
        "n_censored": n_cens,
        "censored_fraction": (n_cens / n) if n else None,
        "max_weight": max_weight,
        "capped_rows": capped,
        "min_g": (min(gdict.values()) if gdict else None),
        "weight_sum": sum(w),
    }
    return w, diag


def ev_ipcw(returns, times, censored, max_weight=DEFAULT_MAX_WEIGHT):
    """EV of ``returns`` over identical rows, WITH and WITHOUT the IPCW correction.

    ``returns`` per-row per-unit-notional return (the quantity ``mean_r`` averages today).
    Returns ``(ev_unweighted, ev_ipcw, diag)``. ``ev_unweighted`` is the plain arithmetic mean
    — the number the current report already prints, reproduced here so the correction is
    compared on the SAME rows.
    """
    if not (len(returns) == len(times) == len(censored)):
        raise ValueError("returns/times/censored must be the same length")
    n = len(returns)
    if n == 0:
        return None, None, {"n": 0, "n_censored": 0, "censored_fraction": None,
                            "max_weight": max_weight, "capped_rows": 0, "min_g": None,
                            "weight_sum": 0.0}
    ev_unw = math.fsum(returns) / n
    w, diag = ipcw_weights(times, censored, max_weight=max_weight)
    sw = math.fsum(w)
    ev_w = (math.fsum(wi * ri for wi, ri in zip(w, returns)) / sw) if sw > 0 else None
    diag["ev_unweighted"] = ev_unw
    diag["ev_ipcw"] = ev_w
    return ev_unw, ev_w, diag