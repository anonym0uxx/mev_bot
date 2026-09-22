#!/usr/bin/env python3
"""Unit test for the M8 IPCW censoring correction (:mod:`ipcw`).

Run directly (no pytest needed):  python3 test_ipcw.py

The KM numbers below are hand-computed so the test is a proof, not a snapshot.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from ipcw import (  # noqa: E402
    censoring_events, kaplan_meier_censoring_survival, ipcw_weights, ev_ipcw,
    DEFAULT_MAX_WEIGHT,
)

FAIL = []


def check(name, cond, detail=""):
    if cond:
        print("ok   %s" % name)
    else:
        print("FAIL %s %s" % (name, detail))
        FAIL.append(name)


def test_censoring_indicator_is_not_the_json_flag():
    # TP/SL are EVENTS; timeout/censored are right-censored. The row's own JSON `censored`
    # flag ("window cut short") is NOT the indicator.
    c = censoring_events(["tp", "sl", "timeout", "censored"])
    check("indicator", c == [False, False, True, True], repr(c))
    try:
        censoring_events(["refused_no_reserves"])
        check("indicator_rejects_unpriced", False, "should have raised")
    except ValueError:
        check("indicator_rejects_unpriced", True)


def test_kaplan_meier_censoring_survival_handworked():
    # rows (t_ms, censored?): A(10,True) B(20,False) C(20,True) D(30,False)
    #   t=10: d=1 of 4 at risk -> G = 1*(1-1/4) = 0.75 ; risk -> 3
    #   t=20: d=1 of 3 at risk -> G = 0.75*(1-1/3) = 0.50 ; risk -> 1
    #   t=30: d=0             -> G = 0.50
    times = [10, 20, 20, 30]
    cens = [True, False, True, False]
    g = kaplan_meier_censoring_survival(times, cens)
    check("km_g10", abs(g[10] - 0.75) < 1e-12, repr(g))
    check("km_g20", abs(g[20] - 0.50) < 1e-12, repr(g))
    check("km_g30", abs(g[30] - 0.50) < 1e-12, repr(g))


def test_weights_and_ev_move():
    times = [10, 20, 20, 30]
    cens = [True, False, True, False]
    w, _ = ipcw_weights(times, cens)
    # event rows weight 1; censored rows 1/G: A=1/0.75=4/3, C=1/0.5=2
    check("w", [round(x, 6) for x in w] == [round(4 / 3, 6), 1.0, 2.0, 1.0], repr(w))
    # r = [1,2,3,10]: unweighted 4.0 ; weighted = (4/3*1+2+2*3+10)/(4/3+1+2+1) = 58/16
    ev_u, ev_w, diag = ev_ipcw([1.0, 2.0, 3.0, 10.0], times, cens)
    check("ev_unweighted", abs(ev_u - 4.0) < 1e-12, repr(ev_u))
    check("ev_ipcw", abs(ev_w - 58.0 / 16.0) < 1e-9, repr(ev_w))
    check("censored_fraction", abs(diag["censored_fraction"] - 0.5) < 1e-12, repr(diag))
    check("correction_moved_ev", abs(ev_w - ev_u) > 1e-6, "%r vs %r" % (ev_w, ev_u))


def test_no_censoring_is_a_noop():
    # With no censored rows, every weight is 1 and the corrected EV == the plain mean.
    times = [5, 15, 25]
    cens = [False, False, False]
    ev_u, ev_w, diag = ev_ipcw([1.0, -2.0, 3.0], times, cens)
    check("noop_equal", abs(ev_u - ev_w) < 1e-12, "%r %r" % (ev_u, ev_w))
    check("noop_fraction", diag["censored_fraction"] == 0.0, repr(diag))


def test_weight_cap_is_reported_not_hidden():
    # A single censored row at the only censoring time collapses G to 1/n and 1/G exceeds the
    # cap; the cap must bind AND be counted.
    times = [1, 10, 10, 10, 10, 10, 10, 10, 10, 10]
    cens = [True] + [False] * 9
    w, diag = ipcw_weights(times, cens, max_weight=5.0)
    # t=1: G=1-1/10=0.9 -> 1/0.9=1.111 <= 5, no cap needed. Use a sharper case instead:
    times2 = [10] * 10
    cens2 = [True] + [False] * 9
    w2, diag2 = ipcw_weights(times2, cens2, max_weight=5.0)
    # t=10: d=1 of 10 -> G=0.9 -> 1/0.9=1.111
    check("cap_not_needed_here", diag2["capped_rows"] == 0, repr(diag2))
    # force a cap: 5 censored at one time of 5 rows -> G = 1-5/5 = 0 -> 1/G capped
    times3 = [7, 7, 7, 7, 7]
    cens3 = [True] * 5
    w3, diag3 = ipcw_weights(times3, cens3, max_weight=5.0)
    check("cap_binds", diag3["capped_rows"] == 5 and all(x == 5.0 for x in w3), repr(diag3))


def test_horizon_untouched():
    # M8 corrects the estimator ONLY: the horizon must be exactly as before.
    # Read it from the ACTUAL label rows (real-data evidence, and no import of the labeller —
    # importing label_barriers runs its module main as a side effect).
    import json
    bar = "/training/v2/reports/barrier_labels_c9.jsonl"
    with open(bar, encoding="utf-8") as fh:
        row = json.loads(fh.readline())
    check("horizon_unchanged", int(row["horizon_ms"]) == 1_800_000, repr(row.get("horizon_ms")))
    import ipcw
    check("ipcw_has_no_horizon_param",
          "HORIZON" not in dir(ipcw) and "horizon" not in dir(ipcw))


if __name__ == "__main__":
    for fn in (test_censoring_indicator_is_not_the_json_flag,
               test_kaplan_meier_censoring_survival_handworked,
               test_weights_and_ev_move,
               test_no_censoring_is_a_noop,
               test_weight_cap_is_reported_not_hidden,
               test_horizon_untouched):
        fn()
    print()
    if FAIL:
        print("FAILED: %d check(s): %s" % (len(FAIL), ", ".join(FAIL)))
        sys.exit(1)
    print("ALL CHECKS PASSED")