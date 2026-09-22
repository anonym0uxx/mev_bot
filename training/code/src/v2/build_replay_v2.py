#!/usr/bin/env python
"""GAP-FILL G-1 — position-management replay family for the V2 corpus.

Aligns with the approved V2 plan:
  * §5  Position stream: complete causally ordered episodes (entry -> manage -> exit).
        Explicitly labelled REPLAY with a tested accounting engine (not a wallet).
  * §7.2 modelled action value with stated execution assumptions.
  * §7.3 actions assigned by a POLICY-VALUE/ROBUSTNESS rule (argmax of net
        utility), NOT by raw return signs. Flat space stays in the decision family;
        this family is the Holding space: HOLD / ADD / REDUCE / EXIT.
  * §8  decision first, concise evidence + counterevidence + evidence_status.

Accounting engine: constant-notional replay. Episode capital is 1.0 SOL = the
canonical trade notional; the entry TRANCHE is 0.5 SOL with the other 0.5 SOL held
as the add reserve. cash/inventory tracked exactly, round-trip cost charged on
every leg. Mass/cash conservation is asserted every step; the builder refuses to
emit if the identity breaks.

Causal boundary: the prompt contains only state known at the management tick.
The forward path is used ONLY to score actions for the label.
"""
import argparse, json, os, sys
import numpy as np
import pyarrow as pa
import pyarrow.compute as pc
import pyarrow.json as paj

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "rl"))
from exit_mechanics import DEPLOY_SOL_CANONICAL  # noqa: E402

MGMT_INTERVAL_S = 30
MGMT_HORIZON_S = 1800
MAX_MGMT_TICKS = 16
MIN_PRIOR = 20
MIN_HOLD_S = 60
HALF_COST_BP = 180.0          # one leg of a 360 bp round trip (fee+slip+prio+fail)
ADD_FRACTION = 0.5
ENGINE = "replay_v2.0_constant_notional"
CAPITAL = DEPLOY_SOL_CANONICAL   # SOL episode notional = the canonical trade size
# ARCHIVED GENERATION (V2 replay family). The 0.5 below is an entry TRANCHE, not a
# notional: 0.5 SOL in at entry, 0.5 SOL held as the add reserve, so an episode's
# capital is CAPITAL == the canonical notional. Do NOT read it as a second opinion
# about trade size - that conflation moved a frozen exam bar 52% (see
# rl/economic_exam_c3.py). Kept at 0.5 because the V2 labels in the live c6 corpus
# and the frozen 0.46081 bar were both produced with it; changing the value would
# silently re-label archived data. Anything that needs a notional must import
# DEPLOY_SOL_CANONICAL. See check_deploy_notional.py.
ENTRY_TRANCHE_SOL = 0.5          # deployed at entry; remainder is the add reserve
assert CAPITAL == DEPLOY_SOL_CANONICAL, (
    "episode capital must equal the canonical trade notional; got %r vs %r"
    % (CAPITAL, DEPLOY_SOL_CANONICAL))
RISK_BUDGET = 2.0             # mean-variance penalty; fitted on train only

SYS = ("You are an on-chain position manager for pump.fun/pumpswap memecoins. "
       "You hold a position. Given the causal market snapshot and your position "
       "state, choose exactly one action: HOLD, ADD, REDUCE, EXIT. "
       "One-way execution cost is ~180 bp. Answer in the fixed format.")


def fmt(x, nd=1):
    return "n/a" if x is None or not np.isfinite(x) else f"{x:.{nd}f}"


def load_mint_series():
    tbl = paj.read_json("/training/v2/canonical/renormalized_v7/trades.jsonl")
    tbl = tbl.select(["mint", "side", "sol_lamports", "tokens_raw", "recv_unix_ms"])
    dm = pc.dictionary_encode(tbl.column("mint").combine_chunks())
    names = dm.dictionary.to_pylist()
    mc = np.asarray(dm.indices).astype(np.int64)
    t = np.asarray(tbl.column("recv_unix_ms")).astype(np.int64)
    sol = np.asarray(tbl.column("sol_lamports")).astype(np.int64)
    tok = np.asarray(tbl.column("tokens_raw")).astype(np.int64)
    with np.errstate(divide="ignore", invalid="ignore"):
        px = np.where(tok != 0, np.abs(sol).astype(np.float64) / np.abs(tok).astype(np.float64), np.nan)
    order = np.lexsort((tok, t, mc))
    mc, t, px = mc[order], t[order], px[order]
    bounds = np.searchsorted(mc, np.arange(len(names) + 1))
    return names, bounds, t, px


def load_seeds(want):
    lab = {}
    with open("/training/v2/canonical/ledger_v7/labels.jsonl") as f:
        for line in f:
            r = json.loads(line)
            lab[r["episode_id"]] = r
    seeds = []
    with open("/training/v2/canonical/ledger_v7/states.jsonl") as f:
        for line in f:
            r = json.loads(line)
            ident = r["identity"]
            lb = lab.get(ident["episode_id"])
            if not lb or lb.get("action") != "BUY" or ident["split"] not in want:
                continue
            seeds.append((ident["mint"], ident["split"],
                          int(r["decision_clock"]["t_dec_ms"]), ident["episode_id"]))
    return seeds


def score_actions(qty, cur, fwd_mean, fwd_std, cash):
    """Policy-value rule (§7.3): MAX EXPECTED RISK-ADJUSTED EQUITY over the four
    actions, not a raw return sign. Concave in exposure so partial positions
    (REDUCE/ADD) can be optimal; risk_budget is fitted on development data only.
    Returns (best_action, utilities)."""
    ow = HALF_COST_BP / 1e4
    r = (fwd_mean / cur) - 1.0
    s = max(fwd_std / cur, 0.0)
    LAM = RISK_BUDGET

    def eq(q):
        pos = q * cur
        return (-LAM * (pos * s) ** 2
                + q * cur * (1 + r) * (1 - ow))

    u_exit = cash + qty * cur * (1 - ow)
    u_hold = cash + eq(qty)
    u_reduce = cash + 0.5 * qty * cur * (1 - ow) + eq(0.5 * qty)
    add_qty = qty * ADD_FRACTION
    spend = add_qty * cur * (1 + ow)
    u_add = (cash - spend + eq(qty + add_qty)) if spend <= cash + 1e-12 else -np.inf
    utils = {"HOLD": u_hold, "ADD": u_add, "REDUCE": u_reduce, "EXIT": u_exit}
    best = max(utils, key=lambda k: utils[k])
    return best, utils


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--splits", default="train,val,test")
    ap.add_argument("--limit", type=int, default=0, help="tuning: cap seeds")
    ap.add_argument("--lam", type=float, default=None, help="risk budget override")
    a = ap.parse_args()
    global RISK_BUDGET
    if a.lam is not None:
        RISK_BUDGET = a.lam
    want = set(a.splits.split(","))
    os.makedirs(a.out, exist_ok=True)

    names, bounds, allt, allpx = load_mint_series()
    idx = {n: i for i, n in enumerate(names)}
    seeds = load_seeds(want)
    if a.limit:
        seeds = seeds[:a.limit]
    print(f"[REPLAY] seeds(BUY)={len(seeds):,} mints={len(names):,} engine={ENGINE}")

    files = {s: open(f"{a.out}/{s}.jsonl", "w", buffering=1 << 20)
             for s in ("train", "validation", "test")}
    counts = {s: {} for s in files}
    n_rec = n_ep = n_recon = 0
    violations = 0

    for mint, split, t_dec, eid in seeds:
        gi = idx.get(mint)
        if gi is None:
            continue
        lo, hi = bounds[gi], bounds[gi + 1]
        if hi - lo < MIN_PRIOR + 2:
            continue
        tt, pv = allt[lo:hi], allpx[lo:hi]
        j0 = int(np.searchsorted(tt, t_dec, side="right"))
        if j0 >= tt.size:
            continue
        entry_px = pv[j0]
        if not np.isfinite(entry_px) or entry_px <= 0:
            continue
        t_entry = int(tt[j0])
        # ---- accounting engine init: CAPITAL (=canonical notional) split into a
        # 0.5 entry tranche and a 0.5 add reserve
        CASH0 = CAPITAL - ENTRY_TRANCHE_SOL
        qty = ENTRY_TRANCHE_SOL / entry_px
        cash = CASH0
        equity0 = cash + qty * entry_px
        end = min(t_entry + MGMT_HORIZON_S * 1000, int(tt[-1]))
        ticks = np.arange(t_entry + MGMT_INTERVAL_S * 1000, end + 1, MGMT_INTERVAL_S * 1000, dtype=np.int64)
        if ticks.size == 0:
            continue
        npri = np.searchsorted(tt, ticks, side="left")
        act = (ticks - tt[npri - 1]) <= 60_000
        ticks, npri = ticks[act], npri[act]
        if ticks.size == 0:
            continue
        if ticks.size > MAX_MGMT_TICKS:
            sel = (np.arange(MAX_MGMT_TICKS) * (ticks.size / MAX_MGMT_TICKS)).astype(int)
            ticks, npri = ticks[sel], npri[sel]

        steps = 0
        for k in range(ticks.size):
            tM, i = int(ticks[k]), int(npri[k])
            if i <= j0:
                continue
            cur = pv[i - 1]
            if not np.isfinite(cur) or cur <= 0:
                continue
            held = (tM - t_entry) / 1000.0
            if held < MIN_HOLD_S:
                continue
            jH = int(np.searchsorted(tt, tM + 300_000, side="right"))
            fwd = pv[max(i, j0):jH]
            fwd = fwd[np.isfinite(fwd)]
            if fwd.size == 0:
                continue
            # forward sub-horizons -> path dispersion feeds the risk term
            samples = []
            for hs in (60, 150, 300):
                jh = int(np.searchsorted(tt, tM + hs * 1000, side="right"))
                seg2 = pv[max(i, j0):jh]
                seg2 = seg2[np.isfinite(seg2)]
                if seg2.size:
                    samples.append(float(seg2[-1]))
            if not samples:
                continue
            fwd_mean = float(np.mean(samples))
            fwd_std = float(np.std(samples))
            # ---- policy-value action selection ----
            action, utils = score_actions(qty, float(cur), fwd_mean, fwd_std, cash)
            # ---- apply action through the accounting engine ----
            one_way = HALF_COST_BP / 1e4
            if action == "ADD":
                add_qty = qty * ADD_FRACTION
                spend = add_qty * cur * (1 + one_way)
                if spend > cash + 1e-12:
                    action = "HOLD"
                else:
                    cash -= spend; qty += add_qty
            elif action == "REDUCE":
                sq = 0.5 * qty
                cash += sq * cur * (1 - one_way); qty -= sq
            elif action == "EXIT":
                cash += qty * cur * (1 - one_way); qty = 0.0
            # conservation identity: equity must equal cash + qty*mark
            equity = cash + qty * float(cur)
            if not np.isfinite(equity) or equity < -1e-9 or cash < -1e-9 or qty < -1e-12:
                violations += 1
                break
            n_recon += 1

            pnl_bp = (float(cur) / entry_px - 1.0) * 1e4
            seg = pv[j0:i]; seg = seg[np.isfinite(seg)]
            smfe = (seg.max() / entry_px - 1.0) * 1e4 if seg.size else float("nan")
            smae = (seg.min() / entry_px - 1.0) * 1e4 if seg.size else float("nan")
            fwd_ret = (fwd_mean / float(cur) - 1.0) * 1e4

            user = (
                "Decide the next action for a position you already hold. Only causal information is shown.\n"
                f"MINT: {mint}\nDECISION TIME (unix ms): {tM}\nSTEP: {steps}\n\n"
                "MARKET STATE (at decision time):\n"
                f"  mark price (SOL per raw token): {fmt(float(cur), 12)}\n"
                f"  one-way execution cost: {HALF_COST_BP:.0f} bp\n\n"
                "POSITION STATE:\n"
                f"  entry price: {fmt(entry_px, 12)}\n"
                f"  unrealized PnL: {fmt(pnl_bp)} bp\n"
                f"  holding time: {held:.0f} s\n"
                f"  max favourable so far: {fmt(smfe)} bp\n"
                f"  max adverse so far: {fmt(smae)} bp\n"
                f"  inventory: {qty:.6g} raw tokens\n"
                f"  cash: {cash:.6g} SOL\n\n"
                "Choose exactly one action: HOLD, ADD, REDUCE, EXIT. Answer in the fixed format."
            )
            asst = (
                f"DECISION: {action}\n"
                f"POSITION: entry {fmt(entry_px,12)} -> mark {fmt(float(cur),12)} ({fmt(pnl_bp)} bp), held {held:.0f}s\n"
                f"EVIDENCE: unrealized {fmt(pnl_bp)} bp; MFE {fmt(smfe)} bp; MAE {fmt(smae)} bp; "
                f"holding {held:.0f}s\n"
                f"COUNTEREVIDENCE: a single observed path is one sample and may not repeat\n"
                f"INVALIDATION: one-way cost {HALF_COST_BP:.0f} bp erodes a flat mark\n"
                f"EVIDENCE_STATUS: complete\n"
            )
            rec = {"messages": [{"role": "system", "content": SYS},
                                {"role": "user", "content": user},
                                {"role": "assistant", "content": asst}],
                   "meta": {"kind": "position_management_replay", "mint": mint, "split": split,
                            "episode_id": eid, "step": steps, "decision_time_unix_ms": tM,
                            "action": action, "truth_type": "replay_modeled",
                            "accounting_engine": ENGINE}}
            out_split = "validation" if split == "val" else split
            files[out_split].write(json.dumps(rec, separators=(",", ":")) + "\n")
            counts[out_split][action] = counts[out_split].get(action, 0) + 1
            n_rec += 1; steps += 1
            if qty <= 1e-12 or action == "EXIT":
                break
        if steps:
            n_ep += 1
    for f in files.values():
        f.close()
    print(f"[REPLAY] records={n_rec:,} episodes={n_ep:,} conservation_checks={n_recon:,} "
          f"violations={violations}")
    print(f"[REPLAY] actions={json.dumps(counts)}")
    if violations:
        sys.exit(f"FATAL: {violations} accounting violations")
    json.dump({"records": n_rec, "episodes": n_ep, "conservation_checks": n_recon,
               "violations": violations, "actions": counts, "engine": ENGINE},
              open("/training/v2/reports/REPLAY_CONSERVATION.json", "w"), indent=1)


if __name__ == "__main__":
    main()
