"""D2 re-measure: are the 'stale-AMM' refusals recoverable, measured through score_entry?

The refusal audit emulated the LIVE rule from the PROMPT (pricing_eligible=false -> refuse).
That is a prompt-side view. The offline engine resolves the venue from its OWN oracle at
t_dec, so the honest question is: (a) which regime does the engine actually use for these
rows, (b) does it price them, and (c) what do they realize at the FULL vs SMALL clip?
If the engine already routes them to the curve, the RL data never lost them and D2 is a
LIVE-PATH question only.
"""
import json, re, sys
sys.path.insert(0, "/training/v2/code/src/v2/rl")
from rl_reward_v3 import (V3Engine, ReserveRegistry, score_entry, load_canonical_tapes,
                          STALE_ANY)

CORPUS = "/training/v2/candidate_sft_c12/train.jsonl"
TAPES = "/training/v2/canonical/renorm_corpus_mints/trades.jsonl"
RE_STALE = re.compile(r"AMM POOL STATE \(at decision time\): amm_reserves=present[^\n]*?"
                      r"staleness_ms=(\d+)[^\n]*?pricing_eligible=(true|false)")
RE_CURVE = re.compile(r"CURVE STATE \(at decision time\): curve_reserves=(\w+)")
RE_BT = re.compile(r'"outcome":\s*"(\w+)"')
N = 25

rows, seen = [], set()
with open(CORPUS, encoding="utf-8") as f:
    for line in f:
        try:
            r = json.loads(line)
        except Exception:
            continue
        if (r.get("meta") or {}).get("action") != "BUY":
            continue
        ms = r.get("messages") or []
        u = next((m["content"] for m in ms if m.get("role") == "user"), "")
        m = RE_STALE.search(u)
        if not m or m.group(2) != "false":
            continue
        if not RE_CURVE.search(u) or "absent" in RE_CURVE.search(u).group(1):
            continue
        mint = r.get("mint")
        eid = r.get("episode_id") or ""
        parts = eid.split(":")
        if not mint or len(parts) < 3 or mint in seen:
            continue
        seen.add(mint)
        bt = (r.get("meta") or {}).get("barrier_triplet") or {}
        rows.append({"mint": mint, "t": int(parts[2]), "stale_ms": int(m.group(1)),
                     "curve": RE_CURVE.search(u).group(1),
                     "bt_outcome": bt.get("outcome"), "bt_mfe": bt.get("mfe_bp"),
                     "bt_last": bt.get("last_bp")})
        if len(rows) >= N:
            break

print("stale-AMM BUY rows with a curve present: %d sampled" % len(rows))
reg = ReserveRegistry.build(verbose=False)
eng = V3Engine(reg, max_reserve_stale_ms=STALE_ANY, deploy_sol=1.0)
tapes = load_canonical_tapes(TAPES, mints={r["mint"] for r in rows},
                             min_notional_lamports=100_000, max_px_ratio=50)
print("tapes loaded: %d" % len(tapes))
print("%-13s %7s %-13s %-9s %-11s %-11s" % ("mint", "stale_ms", "engine regime",
                                            "bt out", "FULL@1.0", "SMALL@0.25"))
n_priced = n_curve = n_amm = 0
netf, nets = [], []
for r in rows:
    t = tapes.get(r["mint"])
    if t is None:
        print("%-13s %7d NO TAPE" % (r["mint"][:12], r["stale_ms"]))
        continue
    try:
        venue, _ = eng.tape_venue_at(t, r["t"])
        regname = eng._regime_of_venue(venue)
        regname = regname.value if regname is not None else "none"
    except Exception as e:                                       # noqa: BLE001
        regname = "err:" + type(e).__name__
    out = {}
    for name, dep in (("FULL", 1.0), ("SMALL", 0.25)):
        try:
            res = score_entry(t, r["t"], eng, policy_name="MOONSHOT_TAIL",
                              deploy_sol=dep, horizon_ms=1800000, min_hold_ms=0)
            ep = res.get("episode") or {}
            nb = None
            if res.get("status") == "ok" and ep.get("net_sol_returned") is not None:
                cap = float(ep["final_equity_sol"]) - float(ep["net_sol_returned"])
                nb = 10000.0 * float(ep["net_sol_returned"]) / cap if cap else None
            out[name] = (res.get("status"), nb)
        except Exception as e:                                   # noqa: BLE001
            out[name] = ("exc:" + type(e).__name__, None)
    if out["FULL"][0] == "ok" or out["SMALL"][0] == "ok":
        n_priced += 1
    n_curve += 1 if regname == "bonding_curve" else 0
    n_amm += 1 if regname == "amm" else 0
    if out["FULL"][1] is not None:
        netf.append(out["FULL"][1])
    if out["SMALL"][1] is not None:
        nets.append(out["SMALL"][1])
    print("%-13s %7d %-13s %-9s %-17s %-17s" % (
        r["mint"][:12], r["stale_ms"], regname, str(r["bt_outcome"]),
        "%s %s" % (out["FULL"][0], ("%.0f" % out["FULL"][1]) if out["FULL"][1] is not None else "-"),
        "%s %s" % (out["SMALL"][0], ("%.0f" % out["SMALL"][1]) if out["SMALL"][1] is not None else "-")))

def mean(x):
    return sum(x) / len(x) if x else float("nan")
print("\nSUMMARY: engine regime at t_dec: curve=%d amm=%d | priced=%d/%d"
      % (n_curve, n_amm, n_priced, len(rows)))
print("FULL@1.0 net bp: n=%d mean %.0f | SMALL@0.25 net bp: n=%d mean %.0f"
      % (len(netf), mean(netf), len(nets), mean(nets)))
print("barrier-proxy claim (for contrast): gross mean 3680.9 bp (mixed) / 4980.2 (pumpfun)")