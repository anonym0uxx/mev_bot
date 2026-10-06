#!/usr/bin/env python
"""Build candidate_sft_c6: the annotated, SOL-only, forward-wall-clean SFT corpus.

ONE pass over candidate_sft_c3 (the audit baseline, never modified, never
overwritten) applying, in this order:

  1. SOL-ONLY MEMECOIN FILTER. pump.fun has non-SOL quote assets (USDC pools plus
     xStocks), and for those rows the tape's `sol_lamports` holds the QUOTE leg, so
     USDC 6dp sits where lamports 9dp is expected - a 1000x unit error in every
     derived feature. Keep a mint iff it is not a quote asset itself and
     (it has a pump-amm pool quoted in WSOL OR it never graduated, i.e. it only
     ever had a bonding curve, which is SOL-denominated by construction).
  2. FORWARD WALL (D4). Nothing at or after wall_ms is ever trained on. The wall is
     the RL evaluation set; training on it would make the RL gate measure
     memorisation. Applied to every split.
  3. PROTECTED MINT EXCLUSION (192 holdout mints).
  4. CURVE RESERVE ANNOTATION (age-labelled, annotation cap 24h). The snapshot is
     the LAST LaserStream fill at or before t_dec. staleness_ms is ALWAYS carried.
     `pricing_eligible` is true only when staleness <= 60s: D3 measured curve
     pricing corr 0.978 at 60s vs 0.833 once joins cross capture sessions, so
     stale state may be ANNOTATED but must never be PRICED.
  5. AMM POOL RESERVE ANNOTATION from the Helius backfill, WSOL-quoted pools ONLY
     (a USDC pool's quote_reserve is 6dp USDC in a lamports slot - same unit trap).
     Row->pool attribution comes from the backfill itself: the pool that produced
     the last event at or before t_dec is the pool the mint was trading, which also
     resolves the dual-pool mints that a per-mint join cannot.

LABEL / STRUCTURE INTEGRITY (proven in the same pass, not assumed):
  - every record's ASSISTANT message content is byte-identical to c3 (sha256
    compared per candidate_id), because the completion is the label;
  - every record's ORIGINAL user prompt is recoverable byte-for-byte by removing
    the injected block - the annotation is additive, never a rewrite;
  - no duplicate candidate_id or prompt; split policy untouched; the forbidden
    future-derived inputs (seconds_to_graduation, graduation_proximity_pct) are
    never read, never emitted, and their presence in the source is fatal.

Nothing is padded, duplicated or retargeted: filtering only removes rows, and
annotation only adds text to prompts.
"""
from __future__ import annotations

import argparse
import bisect
import collections
import hashlib
import json
import os
import re
import sys

LAMPORTS_PER_SOL = 1_000_000_000
TOKEN_SCALE = 1_000_000
V_GRAD_LAMPORTS = 85 * LAMPORTS_PER_SOL

C3 = "/training/v2/candidate_sft_c3"
C6 = "/training/v2/candidate_sft_c6"
WALL = "/training/v2/reports/FORWARD_WALL_V1.json"
PROTECTED = "/training/v2/audit/factual_cpt_protected_eval_mints.json"
DROPSET = "/training/v2/reports/SOL_DROPSET_V2.json"
ZERO = "/training/v2/reports/AMM_ZERO_POOL_MINTS_V1.json"   # quote-asset identification only
BACKFILL = "/training/v2/reserves/amm_history_v1/amm_reserves.jsonl"
FORBIDDEN_INPUTS = ("seconds_to_graduation", "graduation_proximity_pct")
ANNOTATION_CAP_MS = 24 * 3600 * 1000
PRICING_BUDGET_MS = 60_000

QUOTE_ASSETS = {
    "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v": "USDC",
    "So11111111111111111111111111111111111111112": "WSOL",
    "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB": "USDT",
    "pumpCmXqMfrsAkQ5r49WcJnRayYRqmXz6ae8H7H9Dfn": "PUMP",
}
RE_TDEC = re.compile(r"(?:t_dec_ms=|DECISION TIME \(unix ms\):\s*)(\d+)")


# ----------------------------------------------------------------- SOL filter
def sol_only_gate():
    """The authoritative SOL keep/drop gate + the WSOL pool attribution set.

    Source is sol_dropset_v2.py, built on AMM_POOLS_RESOLVED_V2 (all 765 AMM mints
    resolved with memcmp base_mint@43 + a byte[0:32]==mint self-check). c4/c5 used the
    superseded V1 map, whose pool address was accounts[0] of the pump-amm instruction -
    a foreign 137/245-byte account for ~half the rows. That map is NOT used here.

    Returns (drop{mint:reason}, wsol_by_mint{mint:[pool,...]}, never_graduated:set).
    """
    d = json.load(open(DROPSET, encoding="utf-8"))
    return d["drop"], d["mint_to_wsol_pools"], set(d["never_graduated_mints"])


# ------------------------------------------------------------- curve reserves
def load_curve_reserves(dirs):
    import numpy as np
    import pyarrow.parquet as pq

    cols = ["virtual_sol_reserves_lamports", "virtual_token_reserves_raw",
            "real_sol_reserves_lamports", "real_token_reserves_raw"]
    acc = collections.defaultdict(list)
    total = 0
    files = []
    for d in dirs:
        files += sorted(f for f in os.listdir(d) if f.endswith(".parquet"))
        for f in os.listdir(d):
            if not f.endswith(".parquet"):
                continue
            t = pq.read_table(os.path.join(d, f), columns=["mint_b58", "recv_unix_ms"] + cols)
            total += t.num_rows
            mi = t["mint_b58"].to_pylist()
            ts = t["recv_unix_ms"].to_numpy(zero_copy_only=False)
            arrs = {c: t[c].to_numpy(zero_copy_only=False) for c in cols}
            for j, m in enumerate(mi):
                acc[m].append((int(ts[j]), {c: int(arrs[c][j]) for c in cols}))
    out = {}
    for m, lst in acc.items():
        lst.sort(key=lambda x: x[0])
        out[m] = ([x[0] for x in lst], lst)
    return out, total, len(files)


WALL_CURVE_BACKFILL = "/training/v2/reserves/curve_wall_v1/curve_reserves.jsonl"


def load_curve_backfill(path):
    """On-chain curve states (acquired by signature) in the SAME shape as
    load_curve_reserves, so curve_state() consumes either without special-casing."""
    acc = collections.defaultdict(list)
    n = 0
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            try:
                e = json.loads(line)
            except Exception:
                continue
            if not e.get("mint") or e.get("virtual_sol") is None:
                continue
            acc[e["mint"]].append((int(e["recv_unix_ms"]), {
                "virtual_sol_reserves_lamports": int(e["virtual_sol"]),
                "virtual_token_reserves_raw": int(e["virtual_token"]),
                "real_sol_reserves_lamports": int(e["real_sol"]),
                "real_token_reserves_raw": int(e["real_token"])}))
            n += 1
    out = {}
    for m, lst in acc.items():
        lst.sort(key=lambda x: x[0])
        out[m] = ([x[0] for x in lst], lst)
    return out, n


def curve_state(mint, t_dec, curve):
    ent = curve.get(mint)
    if ent is None:
        return None, "mint_absent"
    ts, lst = ent
    i = bisect.bisect_right(ts, t_dec) - 1
    if i < 0:
        return None, "no_event_before_t_dec"
    stale = int(t_dec - ts[i])
    if stale > ANNOTATION_CAP_MS:
        return None, "older_than_annotation_cap"
    d = lst[i][1]
    vsol, vtok = d["virtual_sol_reserves_lamports"], d["virtual_token_reserves_raw"]
    if vsol <= 0 or vtok <= 0:
        return None, "nonpositive_reserve"
    rsol, rtok = d["real_sol_reserves_lamports"], d["real_token_reserves_raw"]
    px = vsol / vtok
    return {
        "status": "present", "src": "laserstream_pumpfun_tradeevent_v1",
        "reserve_ts_ms": ts[i], "staleness_ms": stale,
        "virtual_sol_reserves_lamports": vsol, "virtual_token_reserves_raw": vtok,
        "real_sol_reserves_lamports": rsol, "real_token_reserves_raw": rtok,
        "curve_price_lamports_per_raw_token": px,
        # SAME UNIT AS THE PROMPT'S price_sol_per_raw. The prompt quotes SOL per RAW
        # token; a per-whole-token number is 1e6 larger and would read as a second,
        # contradictory price. Both are emitted, each explicitly named.
        "curve_price_sol_per_raw_token": px / LAMPORTS_PER_SOL,
        "curve_price_sol_per_whole_token": px * TOKEN_SCALE / LAMPORTS_PER_SOL,
        "curve_k": str(vsol * vtok),
        "curve_progress": min(1.0, max(0.0, rsol / V_GRAD_LAMPORTS)),
        "curve_regime": "graduated" if rsol >= V_GRAD_LAMPORTS else "bonding",
        "pricing_eligible": stale <= PRICING_BUDGET_MS,
    }, "matched"


def curve_line(cb, why):
    if cb is None:
        return ("CURVE STATE (at decision time): curve_reserves=absent reason=%s" % why)
    return ("CURVE STATE (at decision time): curve_reserves=present src=laserstream "
            "staleness_ms={st} pricing_eligible={pe} v_sol_reserves_sol={vs:.9f} "
            "v_tokens_reserves={vt} real_sol_reserves_sol={rs:.9f} real_tokens_reserves={rt} "
            "curve_price_sol_per_raw_token={praw:.18f} curve_k={k} curve_progress={pr:.6f} "
            "curve_regime={rg}").format(
        st=cb["staleness_ms"], pe=str(cb["pricing_eligible"]).lower(),
        vs=cb["virtual_sol_reserves_lamports"] / LAMPORTS_PER_SOL, vt=cb["virtual_token_reserves_raw"],
        rs=cb["real_sol_reserves_lamports"] / LAMPORTS_PER_SOL, rt=cb["real_token_reserves_raw"],
        praw=cb["curve_price_sol_per_raw_token"], k=cb["curve_k"], pr=cb["curve_progress"],
        rg=cb["curve_regime"])


# --------------------------------------------------------------- amm reserves
def load_amm_backfill(path):
    """mint -> (ts[], rows[]) sorted; only rows with decoded reserves."""
    ev = collections.defaultdict(list)
    n = 0
    n_null = 0
    with open(path, encoding="utf-8") as f:
        for line in f:
            n += 1
            try:
                r = json.loads(line)
            except Exception:
                continue
            if r.get("base_reserve") is None or r.get("quote_reserve") is None:
                n_null += 1
                continue
            ev[r["mint"]].append((int(r["recv_unix_ms"]), r["pool"], int(r["base_reserve"]),
                                  int(r["quote_reserve"]), r.get("slot"), r.get("event")))
    out = {}
    for m, lst in ev.items():
        lst.sort(key=lambda x: x[0])
        out[m] = ([x[0] for x in lst], lst)
    return out, n, n_null


def amm_state(mint, t_dec, amm, wsol_by_mint, never, npools):
    """AMM pool state at t_dec, WSOL-quoted pools ONLY.

    Attribution (operator rule): a mint with exactly ONE WSOL pool is attributed by
    MINT alone; a mint with several needs the backfill row's `pool` to match the
    authoritative WSOL set, else the row is annotated absent rather than guessed.
    A USDC pool's quote_reserve is 6dp USDC sitting in a lamports slot, so annotating
    from a non-WSOL pool would inject a 1000x unit error.
    """
    if mint in never or mint not in wsol_by_mint:
        # no pump-amm pool at all: bonding curve only, SOL-denominated by construction
        return None, "never_graduated"
    auth = list(wsol_by_mint[mint])
    sole_pool_in_total = (int(npools.get(mint, 0)) == 1)
    ent = amm.get(mint)
    if ent is None:
        return None, "no_amm_event_in_backfill"
    ts, lst = ent
    i = bisect.bisect_right(ts, t_dec) - 1
    if i < 0:
        return None, "no_event_before_t_dec"
    stale = int(t_dec - ts[i])
    if stale > ANNOTATION_CAP_MS:
        return None, "older_than_annotation_cap"
    recv, pool, base, quote, slot, event = lst[i]
    if sole_pool_in_total and len(auth) == 1:
        # the mint has ONE pool in total: attributing by mint cannot pick a non-WSOL
        # pool. (Before this guard, a mint with one WSOL pool PLUS a USDC pool would
        # accept the USDC pool's 6dp quote as WSOL -> the 1000x unit bug.)
        pool_used = auth[0]
        pool_field_agrees = (pool == auth[0])
    elif pool in auth:
        pool_used = pool
        pool_field_agrees = True
    else:
        # multi-pool mint and the last event is not from an authoritative WSOL pool
        return None, "pool_not_authoritative_wsol"
    if base <= 0 or quote <= 0:
        return None, "nonpositive_reserve"
    px = quote / base          # lamports per RAW base unit
    return {
        "status": "present", "src": "helius_amm_backfill_v1", "pool": pool_used,
        "quote": "WSOL", "n_wsol_pools": len(auth),
        "pool_field_agrees_with_authoritative": bool(pool_field_agrees),
        "backfill_event_pool": pool,
        "reserve_slot": slot, "last_event": event, "reserve_ts_ms": recv, "staleness_ms": stale,
        "base_reserve_raw": base, "quote_reserve_lamports": quote,
        "amm_price_lamports_per_raw_token": px,
        "amm_price_sol_per_raw_token": px / LAMPORTS_PER_SOL,
        "amm_price_sol_per_whole_token": px * TOKEN_SCALE / LAMPORTS_PER_SOL,
        "pricing_eligible": stale <= PRICING_BUDGET_MS,
    }, "matched"


def amm_line(cb, why):
    if cb is None:
        return "AMM POOL STATE (at decision time): amm_reserves=absent reason=%s" % why
    return ("AMM POOL STATE (at decision time): amm_reserves=present src=helius_amm_backfill "
            "pool={p} quote=WSOL staleness_ms={st} pricing_eligible={pe} "
            "base_reserves_raw={b} quote_reserves_lamports={q} quote_reserves_sol={qs:.9f} "
            "amm_price_sol_per_raw_token={px:.18f} reserve_slot={sl}").format(
        p=cb["pool"], st=cb["staleness_ms"], pe=str(cb["pricing_eligible"]).lower(),
        b=cb["base_reserve_raw"], q=cb["quote_reserve_lamports"],
        qs=cb["quote_reserve_lamports"] / LAMPORTS_PER_SOL,
        px=cb["amm_price_sol_per_raw_token"], sl=cb["reserve_slot"])


def inject(content, lines):
    block = "\n".join(lines)
    for pat in (r"\n+Choose exactly one action", r"\n+Answer in the fixed format"):
        m = re.search(pat, content)
        if m:
            return content[:m.start()] + "\n" + block + content[m.start():]
    return content + "\n" + block


V2 = "/training/v2/reports/AMM_POOLS_RESOLVED_V2.jsonl"
# The legacy prompt field is NAMED price_sol_per_raw but settlement proved its VALUE is
# the pool/curve spot price in LAMPORTS PER RAW TOKEN (median ratio vs quote/base =
# 1.0368e9 over 62,278 tight-joined rows, 94.9% inside [5e8,2e9]). So the number is
# right and the NAME plus the label's unit word are wrong. Everything downstream is
# rendered in the same, explicitly named unit family.
UNIT_WRONG = "SOL per raw token"
UNIT_RIGHT = "lamports per raw token"


def pool_count_by_mint():
    d = {}
    for l in open(V2, encoding="utf-8"):
        r = json.loads(l)
        d[r["mint"]] = int(r["n_pools"])
    return d


def rename_price_field(user_content):
    """Same number, honest name. No digit is touched."""
    return user_content.replace("price_sol_per_raw=",
                                "price_lamports_per_raw_token=")


def price_units_line(px_lamports_per_raw):
    """One measurement, three explicitly named forms, so the brain can never confuse
    a lamports-per-raw mark with a SOL price (the failure class that killed SFT-004)."""
    if not px_lamports_per_raw or px_lamports_per_raw <= 0:
        return "PRICE UNITS: price_lamports_per_raw_token=absent reason=no_supplied_price"
    return ("PRICE UNITS: price_lamports_per_raw_token={a:.10g} "
            "price_sol_per_raw_token={b:.10g} price_sol_per_whole_token={c:.10g}"
            ).format(a=px_lamports_per_raw,
                     b=px_lamports_per_raw / LAMPORTS_PER_SOL,
                     c=px_lamports_per_raw * TOKEN_SCALE / LAMPORTS_PER_SOL)


def fix_label_units(label_text):
    """Repair the unit WORD only. Every number, decision and rationale byte survives:
    the caller re-checks by stripping the phrase from both versions."""
    n = label_text.count(UNIT_WRONG)
    if not n:
        return label_text, 0
    return label_text.replace(UNIT_WRONG, UNIT_RIGHT), n


RE_TAPE_PRICE = re.compile(r"price_sol_per_raw=([0-9eE.+\-]+)")


def tape_price(user_content):
    """The price the prompt ALREADY supplies, so the annotation can be checked
    against it instead of silently introducing a second, contradictory price."""
    m = RE_TAPE_PRICE.search(user_content)
    if not m:
        return None
    try:
        return float(m.group(1))
    except ValueError:
        return None


def market_structure(cb, ab, tape_px):
    """Structured companion to the injected text (meta, not prose). Keeps the
    annotation machine-readable for RL/reward/tooling and records the agreement
    between the pool price and the price the prompt already carried."""
    out = {"curve": cb, "amm": ab, "tape_price_sol_per_raw": tape_px}
    ag = {}
    for name, st in (("curve", cb), ("amm", ab)):
        if st and st.get("pricing_eligible") and tape_px:
            px = st.get(name + "_price_lamports_per_raw_token")
            if px and px > 0:
                d = abs(px - tape_px) / tape_px
                ag[name] = {"disagree_frac": d,
                            "disagree_bp": d * 10000.0,
                            "agree_within_1pct": d <= 0.01,
                            "agree_within_5pct": d <= 0.05}
    out["price_agreement"] = ag
    return out


def sha(s):
    return hashlib.sha256(s.encode("utf-8")).hexdigest()


def tdec_of(o):
    m = RE_TDEC.search(o["messages"][1]["content"])
    if m:
        return int(m.group(1))
    mm = re.search(r":(\d{10,})$", o.get("episode_id") or "")
    return int(mm.group(1)) if mm else None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--curve-dirs", nargs="+",
                    default=["/training/v2/reserves/s0823r", "/training/v2/reserves/s0824",
                             "/training/v2/reserves/s0909a", "/training/v2/reserves/s0909b"])
    ap.add_argument("--amm", default=BACKFILL)
    ap.add_argument("--out", default=C6)
    ap.add_argument("--wall-eval-out", default="",
                    help="also emit rows at/after wall_ms to this file, annotated by "
                         "THIS builder so the eval set matches the training format "
                         "(never trained on)")
    ap.add_argument("--report", action="store_true", help="count only, write nothing")
    ap.add_argument("--wall-only", action="store_true",
                    help="with --wall-eval-out: annotate ONLY wall rows and NEVER open the corpus outputs. Required while a training run is streaming c6 - rewriting it in place would tear the live read.")
    ap.add_argument("--limit", type=int, default=0, help="debug: rows per split")
    a = ap.parse_args()

    wall = json.load(open(WALL, encoding="utf-8"))["wall_ms"]
    protected = set(json.load(open(PROTECTED, encoding="utf-8")))
    drop, wsol_by_mint, never = sol_only_gate()
    npools = pool_count_by_mint()
    print(json.dumps({"wall_ms": wall, "protected": len(protected),
                      "sol_drop_set": len(drop),
                      "drop_reasons": dict(collections.Counter(drop.values()))}), flush=True)

    curve, curve_rows, curve_files = load_curve_reserves(a.curve_dirs)
    # WALL-ONLY curve source. The capture stream ends BEFORE the wall, so wall rows
    # would be annotated curve_reserves=absent / pricing_eligible=false while the RL
    # scorer prices them from the fresh on-chain backfill - the prompt would misstate
    # the very reserves the reward is computed from. Kept in a SEPARATE dict consulted
    # only for wall rows, so the corpus splits cannot change.
    curve_wall = curve
    if a.wall_eval_out and os.path.isfile(WALL_CURVE_BACKFILL):
        bf, bf_rows = load_curve_backfill(WALL_CURVE_BACKFILL)
        curve_wall = {m: (list(v[0]), list(v[1])) for m, v in curve.items()}
        added = 0
        for m, (ts, lst) in bf.items():
            if m in curve_wall:
                pairs = sorted(list(zip(curve_wall[m][0], curve_wall[m][1])) + list(zip(ts, lst)),
                               key=lambda x: x[0])
                curve_wall[m] = ([q[0] for q in pairs], [q[1] for q in pairs])
            else:
                curve_wall[m] = (ts, lst)
            added += len(ts)
        print(json.dumps({"wall_curve_source": WALL_CURVE_BACKFILL,
                          "wall_curve_mints": len(bf), "wall_curve_states": added}), flush=True)
    print(json.dumps({"curve_rows": curve_rows, "curve_files": curve_files,
                      "curve_mints": len(curve)}), flush=True)
    amm, amm_rows, amm_null = load_amm_backfill(a.amm)
    print(json.dumps({"amm_rows": amm_rows, "amm_null": amm_null, "amm_mints": len(amm)}), flush=True)

    report = {"schema": "sft_c6_build_v1", "source": C3, "out": a.out,
              "wall_ms": wall, "annotation_cap_ms": ANNOTATION_CAP_MS,
              "pricing_budget_ms": PRICING_BUDGET_MS,
              "forbidden_inputs": list(FORBIDDEN_INPUTS),
              "sol_drop_set": len(drop), "protected_mints": len(protected),
              "curve_rows_loaded": curve_rows, "amm_rows_loaded": amm_rows,
              "curve_dirs": a.curve_dirs, "amm_source": a.amm, "splits": {}}
    if not a.report:
        os.makedirs(a.out, exist_ok=True)
    wall_fh = None
    if a.wall_eval_out and not a.report:
        os.makedirs(os.path.dirname(a.wall_eval_out), exist_ok=True)
        wall_fh = open(a.wall_eval_out, "w", encoding="utf-8")
    wall_c = collections.Counter(); wall_fam = collections.Counter()
    wall_stale_c = []; wall_stale_a = []

    label_hashes = {}
    for split in ("train", "validation", "examination"):
        src = os.path.join(C3, split + ".jsonl")
        dst = os.path.join(a.out, split + ".jsonl")
        c = collections.Counter()
        fam = collections.Counter()
        stale_c, stale_a = [], []
        seen_cid, seen_prompt = set(), set()
        fo = None if (a.report or a.wall_only) else open(dst, "w", encoding="utf-8")
        n_in = 0
        with open(src, encoding="utf-8") as fi:
            for line in fi:
                n_in += 1
                if a.limit and n_in > a.limit:
                    break
                o = json.loads(line)
                meta0 = o.get("meta") or {}
                mint = o.get("mint") or meta0.get("mint")
                t_dec = tdec_of(o)
                if meta0.get("split") and meta0["split"] != "train" and split == "train":
                    pass
                # integrity: forbidden future-derived inputs must never be present
                low = line.lower()
                for f in FORBIDDEN_INPUTS:
                    if f in low:
                        raise SystemExit("FATAL leakage: %s present in %s" % (f, src))
                # 1. SOL-only
                if mint in drop:
                    c["drop_" + drop[mint]] += 1
                    continue
                # 2. forward wall - never TRAINED on. With --wall-eval-out the row is
                # instead annotated by THIS builder into its own file, so the eval set
                # is in the identical format as the training corpus.
                is_wall = False
                if t_dec is not None and t_dec >= wall:
                    if wall_fh is None:
                        c["drop_forward_wall"] += 1
                        continue
                    is_wall = True
                    wall_c["wall_rows"] += 1
                # 3. protected
                if mint in protected:
                    c["drop_protected"] += 1
                    continue

                cid = meta0.get("candidate_id")
                user0 = o["messages"][1]["content"]
                asst0 = o["messages"][-1]["content"]
                ph = sha(json.dumps(o["messages"][:-1], sort_keys=True, ensure_ascii=False))
                if not is_wall:                    # wall rows are NOT corpus rows
                    if cid in seen_cid:
                        raise SystemExit("FATAL duplicate candidate_id %s" % cid)
                    if ph in seen_prompt:
                        raise SystemExit("FATAL duplicate prompt in %s" % split)
                    seen_cid.add(cid)
                    seen_prompt.add(ph)
                    label_hashes[cid] = sha(asst0)

                # 4/5. annotations
                cb = why_c = None
                if mint is not None and t_dec is not None:
                    cb, why_c = curve_state(mint, t_dec, (curve_wall if is_wall else curve))
                ab = why_a = None
                if mint is not None and t_dec is not None:
                    ab, why_a = amm_state(mint, t_dec, amm, wsol_by_mint, never,
                                          npools)
                if cb is not None:
                    c["curve_present"] += 1
                    stale_c.append(cb["staleness_ms"])
                    if cb["pricing_eligible"]:
                        c["curve_pricing_eligible"] += 1
                else:
                    c["curve_absent_" + str(why_c)] += 1
                if ab is not None:
                    c["amm_present"] += 1
                    stale_a.append(ab["staleness_ms"])
                    if ab["pricing_eligible"]:
                        c["amm_pricing_eligible"] += 1
                    if ab["last_event"]:
                        c["amm_last_event_" + str(ab["last_event"])] += 1
                    if not ab["pool_field_agrees_with_authoritative"]:
                        c["amm_single_pool_attributed_by_mint_only"] += 1
                    if ab["n_wsol_pools"] > 1:
                        c["amm_multi_pool_matched"] += 1
                else:
                    c["amm_absent_" + str(why_a)] += 1

                # ---- market-structure annotation, ENTRY-DECISION rows only -------
                # A mint-market block injected into a position-management or cost-math
                # prompt is off-task: the label there reasons about entry/mark/hold, so
                # the block can only distract. task==decision_action is the entry task.
                _task = (o.get("meta") or {}).get("task")
                annotate = (_task == "decision_action")
                if not annotate:
                    c["not_annotated_task_" + str(_task)] += 1
                tape_px = tape_price(user0)
                user_base = rename_price_field(user0)
                if user_base != user0:
                    c["prompt_price_field_renamed"] += 1
                ms = market_structure(cb, ab, tape_px)
                for k, v in (ms.get("price_agreement") or {}).items():
                    c["price_agree_%s_1pct" % k] += int(bool(v["agree_within_1pct"]))
                    c["price_agree_%s_5pct" % k] += int(bool(v["agree_within_5pct"]))
                    if v["disagree_frac"] > 0.50:
                        c["price_disagree_%s_over_50pct" % k] += 1
                lines = ([curve_line(cb, why_c), amm_line(ab, why_a),
                          price_units_line(tape_px)] if annotate else [])
                new_user = inject(user_base, lines) if lines else user_base
                if lines:
                    # additive proof against the renamed base: nothing else may move
                    if new_user.replace("\n" + "\n".join(lines), "", 1) != user_base and \
                            new_user.replace("\n".join(lines) + "\n", "", 1) != user_base:
                        raise SystemExit("FATAL prompt rewrite: %s" % cid)
                o["messages"][1]["content"] = new_user
                # UNIT HONESTY IS NOT TASK-DEPENDENT. Renaming the field or repairing
                # the label's unit word only on annotated rows would present two names
                # and two units for one quantity depending on the task - the exact
                # train/serve confusion this remediation exists to remove. The market
                # block (lines/meta) stays task-gated; the unit fix applies to every row.
                lab0 = o["messages"][-1].get("content") or ""
                lab1, nfix = fix_label_units(lab0)
                if nfix:
                    # audit: strip the phrase from both; anything else differing
                    # (any digit, decision or rationale byte) aborts the build.
                    if lab1.replace(UNIT_RIGHT, "") != lab0.replace(UNIT_WRONG, ""):
                        raise SystemExit("FATAL label edit beyond unit span: %s" % cid)
                    o["messages"][-1]["content"] = lab1
                    c["label_unit_words_fixed"] += nfix
                if annotate:
                    meta = o.setdefault("meta", {})
                    meta["curve_reserves"] = cb if cb is not None else {"status": "absent", "reason": why_c}
                    meta["amm_reserves"] = ab if ab is not None else {"status": "absent", "reason": why_a}
                    meta["market_structure"] = ms
                    meta["price_units"] = {
                        "field": "price_lamports_per_raw_token",
                        "note": "legacy name price_sol_per_raw carried lamports-per-raw; "
                                "renamed, value unchanged",
                        "unit_kind": "lamports_per_raw_token"}
                meta = o.setdefault("meta", {})
                meta["builder"] = "build_sft_c6.v2"
                meta["reserve_causality"] = "reserve_ts_ms <= t_dec_ms (strict, no lookahead)"
                (wall_fam if is_wall else fam)[o.get("family") or "?"] += 1
                if is_wall:
                    wall_c["wall_emitted"] += 1
                    if cb is not None: wall_stale_c.append(cb["staleness_ms"])
                    if ab is not None: wall_stale_a.append(ab["staleness_ms"])
                    wall_fh.write(json.dumps(o) + "\n")
                else:
                    c["kept"] += 1
                    if fo is not None:
                        fo.write(json.dumps(o) + "\n")
        if fo is not None:
            fo.close()
            h = hashlib.sha256()
            n = 0
            with open(dst, "rb") as fh:
                for chunk in iter(lambda: fh.read(1 << 20), b""):
                    h.update(chunk)
            n = sum(1 for _ in open(dst, encoding="utf-8"))
            bytes_ = os.path.getsize(dst)
            report["splits"][split] = {"rows_in": n_in, "rows_out": n, "bytes": bytes_,
                                       "sha256": h.hexdigest()}
        def st(x):
            x = sorted(x)
            if not x:
                return None
            return {"p50": x[len(x) // 2], "p90": x[int(.9 * (len(x) - 1))], "max": x[-1]}
        report["splits"].setdefault(split, {})["counts"] = dict(c)
        report["splits"][split]["families"] = dict(fam)
        report["splits"][split]["curve_staleness_ms"] = st(stale_c)
        report["splits"][split]["amm_staleness_ms"] = st(stale_a)
        report["splits"][split]["curve_coverage"] = round(
            c["curve_present"] / max(c["kept"], 1), 6)
        report["splits"][split]["amm_coverage"] = round(
            c["amm_present"] / max(c["kept"], 1), 6)
        print(split, json.dumps(report["splits"][split]), flush=True)

    if wall_fh is not None:
        wall_fh.close()
        def _wst(x):
            x = sorted(x)
            if not x:
                return None
            return {"p50": x[len(x)//2], "p90": x[int(.9*(len(x)-1))], "max": x[-1]}
        import hashlib as _hl
        _h = _hl.sha256()
        with open(a.wall_eval_out, "rb") as _fh:
            for _c in iter(lambda: _fh.read(1 << 20), b""):
                _h.update(_c)
        report["wall_eval"] = {"path": a.wall_eval_out,
                               "rows": sum(1 for _ in open(a.wall_eval_out, encoding="utf-8")),
                               "bytes": os.path.getsize(a.wall_eval_out),
                               "sha256": _h.hexdigest(),
                               "wall_ms": wall,
                               "never_trained_on": True,
                               "families": dict(wall_fam),
                               "curve_staleness_ms": _wst(wall_stale_c),
                               "amm_staleness_ms": _wst(wall_stale_a),
                               "counts": dict(wall_c)}
        print("WALL_EVAL", json.dumps(report["wall_eval"]), flush=True)

    if not a.report:
        # label identity: every c6 label must equal c3's, byte for byte
        mism = 0
        checked = 0
        for split in ("train", "validation", "examination"):
            with open(os.path.join(C3, split + ".jsonl"), encoding="utf-8") as fh:
                for line in fh:
                    o = json.loads(line)
                    cid = (o.get("meta") or {}).get("candidate_id")
                    if cid not in label_hashes:
                        continue
                    checked += 1
                    if sha(o["messages"][-1]["content"]) != label_hashes[cid]:
                        mism += 1
        report["label_identity"] = {"checked": checked, "mismatched": mism,
                                    "verdict": "PASS" if mism == 0 else "FAIL"}
        report_path = "/training/v2/reports/SFT_C6_BUILD_REPORT.json"
        with open(report_path, "w", encoding="utf-8") as fh:
            json.dump(report, fh, indent=1)
        print("WROTE", report_path, flush=True)
    print(json.dumps({k: v for k, v in report.items() if k != "splits"}, indent=1), flush=True)


if __name__ == "__main__":
    main()
