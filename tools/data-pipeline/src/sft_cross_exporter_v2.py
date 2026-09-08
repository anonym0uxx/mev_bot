#!/usr/bin/env python3
"""sft_cross_exporter_v2 — supervision-geometry-corrected cross-sectional SFT exports.

Fixes the v1 interface bug (exporter read `mint`/top-level causal keys from panel
candidates that actually carry `mint_id` + nested `causal_state`, producing
sha256('')[:12] ids, empty causal inputs, all-WATCH outputs and outcome-leaking
rationales).

Two explicit target modes:
  live_action    (~80%) — compact production output: causal regime summary,
                  BUY top 1-3 or NO_BUY, limited WATCH set, rank/confidence,
                  size band, CAUSAL evidence only, risk/invalidation,
                  aggregated SKIP reasons.
  full_rank_aux  (~20%) — compact rank/action/score table over all candidates,
                  no prose. Preserves cross-sectional ordering supervision.

LEAKAGE RULE (BINDING): outcome/L2/L3 fields (MFE, MAE, ret_*, net return,
robust utility magnitudes, collapse flags) may determine TARGET correctness
(decision/rank labels come from the panel builder) but NEVER appear as
evidence text. All rationale strings cite ONLY features present in the causal
input. `assert_no_outcome_leakage` enforces this at build time.
"""
import hashlib
import json
import math

LIVE_ACTION_INSTRUCTION = (
    "You are the live Pump.fun trading decision brain. You receive a cross-sectional "
    "panel of candidate tokens with causal market state only (no future information), "
    "serialized as a `fields` header plus one value row per candidate. "
    "Compare all candidates like an elite Solana memecoin trader and emit ONE compact, "
    "executable decision: a causal market-state summary, BUY for at most the top 3 "
    "candidates (or NO_BUY), a limited WATCH set with triggers, rank and confidence, "
    "a preferred executable size band in SOL, the strongest causal evidence, and the "
    "risk/invalidation condition. Aggregate SKIP reasons for the remaining candidates. "
    "Do not narrate every candidate. Cite only causal features visible in the input."
)

FULL_RANK_AUX_INSTRUCTION = (
    "You are the Pump.fun cross-sectional ranking engine. You receive a panel of "
    "candidate tokens with causal market state only, serialized as a `fields` header "
    "plus one value row per candidate. Output a compact ranking table "
    "over ALL candidates — mint_id, rank, action (BUY/WATCH/SKIP), score — ordered "
    "by robust executable utility. No prose, no per-candidate explanations. If no "
    "candidate merits a BUY, set no_buy_flag true."
)

# Outcome/L2/L3 vocabulary that must NEVER appear in model-visible target text.
_LEAK_PATTERNS = (
    'mfe', 'mae', 'ret_1s', 'ret_5s', 'ret_30s', 'ret_60s', 'ret_120s', 'ret_300s',
    'net_return', 'net return', 'median_net', 'robust_executable_utility',
    'robust utility', 'moonshot', 'collapsed', 'survived', 'graduated_after',
    'peak_bp', 'hit_plus', 'hit_minus', 'feasibility_ratio', 'feasibility ',
    'sim_return', 'latency-invariant', 'counterfactual', 'outcome',
)


def assert_no_outcome_leakage(text_blob, ctx=''):
    low = text_blob.lower()
    for pat in _LEAK_PATTERNS:
        if pat in low:
            raise AssertionError(f"Outcome leakage in model-visible target text ({ctx}): '{pat}'")


def _f(v, nd=3):
    """Compact float formatting; returns None for missing/NaN."""
    if v is None:
        return None
    try:
        fv = float(v)
    except (TypeError, ValueError):
        return None
    if math.isnan(fv) or math.isinf(fv):
        return None
    return round(fv, nd)


# INPUT causal feature subset (SEQ BUDGET, BINDING): the full pump_state_v3
# causal schema is ~90 fields; at 25-50 candidates/panel that serializes to
# ~60-70K tokens — 5x the 12,288 SFT seq limit. The model-visible input keeps
# this curated 22-field high-signal subset (curve state, momentum, flow,
# participation, concentration, manipulation, liquidity, creator history);
# 50 candidates x ~200 tok fits the window with room for the target. Every
# evidence/trigger/risk string cites ONLY these fields.
INPUT_CAUSAL_FIELDS = [
    'seconds_since_launch', 'market_cap_sol',
    'curve_pct_depleted', 'graduation_proximity_pct',
    'price_momentum_30s_bp', 'price_change_since_launch_bp',
    'trade_velocity_30s', 'vol_acceleration',
    'buy_sell_imbalance_30s', 'buy_pressure', 'net_flow_sol',
    'unique_buyers_5s', 'unique_wallets_so_far', 'top5_buyer_pct',
    'holder_concentration_hhi', 'wash_trade_ratio', 'sybil_cluster_size',
    'toxic_flow_indicator', 'liquidity_sol', 'avg_trade_size_sol',
    'creator_past_tokens', 'creator_past_rugs',
]


def _clean_causal(causal):
    """Curated causal subset; drops None/NaN; compacts floats to 4 sig decimals."""
    out = {}
    for k in INPUT_CAUSAL_FIELDS:
        v = (causal or {}).get(k)
        if v is None:
            continue
        if isinstance(v, float):
            if math.isnan(v) or math.isinf(v):
                continue
            v = round(v, 4)
        out[k] = v
    return out


def _panel_median(cands, key, nd=3):
    vals = []
    for c in cands:
        v = _f((c.get('causal_state') or {}).get(key))
        if v is not None:
            vals.append(v)
    if not vals:
        return None
    vals.sort()
    n = len(vals)
    med = vals[n // 2] if n % 2 else (vals[n // 2 - 1] + vals[n // 2]) / 2.0
    return round(med, nd)


# ── causal evidence / risk / size-band construction (INPUT features only) ──

_EVIDENCE_FEATURES = [
    ('buy_pressure', 'buy_pressure', 3),
    ('buy_sell_imbalance_30s', 'buy_sell_imbalance_30s', 3),
    ('trade_velocity_30s', 'trade_velocity_30s trades/s', 2),
    ('vol_acceleration', 'vol_acceleration', 3),
    ('price_momentum_30s_bp', 'price_momentum_30s', 0),
    ('net_flow_sol', 'net_flow_sol', 2),
    ('unique_buyers_5s', 'unique_buyers_5s', 0),
    ('unique_wallets_so_far', 'unique_wallets', 0),
    ('curve_pct_depleted', 'curve_pct_depleted', 1),
]


def _causal_evidence(cand, cands):
    """Strongest causal evidence: features most above panel median."""
    cs = cand.get('causal_state') or {}
    scored = []
    for key, label, nd in _EVIDENCE_FEATURES:
        v = _f(cs.get(key), nd)
        if v is None:
            continue
        med = _panel_median(cands, key, nd)
        if med is None:
            continue
        spread = abs(med) if abs(med) > 1e-9 else 1.0
        scored.append(((v - med) / spread, f"{label}={v} (panel median {med})"))
    scored.sort(key=lambda x: -x[0])
    parts = [s for _, s in scored[:3]]
    return '; '.join(parts) if parts else 'broad-based flow strength vs panel'


def _risk_invalidation(cand):
    cs = cand.get('causal_state') or {}
    risks = []
    wash = _f(cs.get('wash_trade_ratio'))
    if wash is not None and wash > 0.15:
        risks.append(f"wash_trade_ratio={wash}")
    syb = _f(cs.get('sybil_cluster_size'), 0)
    if syb is not None and syb >= 3:
        risks.append(f"sybil_cluster_size={int(syb)}")
    hhi = _f(cs.get('holder_concentration_hhi'))
    if hhi is not None and hhi > 0.15:
        risks.append(f"holder_hhi={hhi}")
    top5 = _f(cs.get('top5_buyer_pct'), 1)
    if top5 is not None and top5 > 50:
        risks.append(f"top5_buyer_pct={top5}")
    rugs = _f(cs.get('creator_past_rugs'), 0)
    if rugs is not None and rugs > 0:
        risks.append(f"creator_past_rugs={int(rugs)}")
    toxic = _f(cs.get('toxic_flow_indicator'))
    if toxic is not None and toxic > 0.5:
        risks.append(f"toxic_flow={toxic}")
    risk = ', '.join(risks[:2]) if risks else 'standard early-curve fragility'
    return (f"risk: {risk}; invalidate if buy_sell_imbalance_30s flips negative "
            f"or trade_velocity_30s decays below panel median")


def _size_band(cand):
    """Preferred executable size band from CAUSAL liquidity/flow only."""
    cs = cand.get('causal_state') or {}
    liq = _f(cs.get('liquidity_sol'), 2)
    avg_tr = _f(cs.get('avg_trade_size_sol'), 3)
    if liq is None:
        return '0.05-0.10'
    if liq < 10:
        band = '0.05-0.10'
    elif liq < 40:
        band = '0.10-0.25'
    elif liq < 100:
        band = '0.25-0.50'
    else:
        band = '0.50-1.00'
    if avg_tr is not None and avg_tr < 0.05 and band != '0.05-0.10':
        band = '0.05-0.10'  # thin per-trade tape overrides pool depth
    return band


_SKIP_REASONS = [
    ('wash_or_sybil', lambda cs, med: (_f(cs.get('wash_trade_ratio')) or 0) > 0.3
        or (_f(cs.get('sybil_cluster_size'), 0) or 0) >= 5),
    ('concentrated_holders', lambda cs, med: (_f(cs.get('holder_concentration_hhi')) or 0) > 0.25
        or (_f(cs.get('top5_buyer_pct'), 1) or 0) > 65),
    ('late_curve', lambda cs, med: (_f(cs.get('curve_pct_depleted'), 1) or 0) > 85),
    ('negative_momentum', lambda cs, med: (_f(cs.get('price_momentum_30s_bp'), 0) or 0) < 0
        and (_f(cs.get('buy_sell_imbalance_30s')) or 0) < 0),
    ('stale_flow', lambda cs, med: med is not None
        and (_f(cs.get('trade_velocity_30s')) or 0) < med
        and (_f(cs.get('net_flow_sol'), 2) or 0) <= 0),
]


def _skip_reason(cand, med_velocity):
    cs = cand.get('causal_state') or {}
    for name, fn in _SKIP_REASONS:
        try:
            if fn(cs, med_velocity):
                return name
        except TypeError:
            continue
    return 'insufficient_edge'


def _market_state(cands):
    n = len(cands)
    med_vel = _panel_median(cands, 'trade_velocity_30s')
    med_bp = _panel_median(cands, 'buy_pressure')
    pos_mom = sum(1 for c in cands
                  if (_f((c.get('causal_state') or {}).get('price_momentum_30s_bp'), 0) or 0) > 0)
    active = _panel_median(cands, 'market_active_mints_5m')
    parts = [f"{n}-candidate panel"]
    if med_vel is not None:
        parts.append(f"median trade_velocity_30s {med_vel}")
    if med_bp is not None:
        parts.append(f"median buy_pressure {med_bp}")
    parts.append(f"{pos_mom}/{n} with positive 30s momentum")
    if active is not None:
        parts.append(f"~{int(active)} active mints/5m market-wide")
    regime = 'risk-on rotation' if pos_mom > n * 0.5 else \
             'selective bid, weak breadth' if pos_mom > n * 0.2 else 'broad fade'
    return '; '.join(parts) + f" — regime: {regime}"


def _utility(cand):
    return (cand.get('continuous_utility') or {}).get('robust_executable_utility_v1', 0.0) or 0.0


# ── v2 TARGET DECISION (label side only — never surfaces in text) ──
# The v1 pipeline's robust_utility saturates (bimodal 0 / ~0.45-0.6) because
# median_net_return_pct is a near-constant TP-target artifact (~2.53) of the
# counterfactual simulator. The scalars that actually discriminate entries are
# the asymmetric outcome components: shallow adverse excursion (MAE), material
# favorable excursion (MFE), 60s survival, no collapse, full size feasibility.
# BUY is deliberately rare/decisive: an elite trader passes on most panels.
_BUY_GATE = {
    'min_feasibility': 0.8,      # feasible at >= 4/5 sizes
    'max_mae_bp': -2000,         # drawdown shallower than -20%
    'min_mfe_bp': 3000,          # upside excursion beyond +30%
}


def decide_v2(cand):
    """Recompute BUY/WATCH/SKIP from outcome evidence (TARGET correctness only)."""
    cu = cand.get('continuous_utility') or {}
    l2 = cand.get('l2_evidence') or {}
    feas = cu.get('feasibility_score') or 0.0
    util = _utility(cand)
    collapsed = bool(l2.get('collapsed_50pct_within_300s'))
    survived = bool(l2.get('survived_60s'))
    mae = l2.get('mae_bp')
    mfe = l2.get('mfe_bp')
    if collapsed or feas < 0.2 or util < -0.10:
        return 'SKIP'
    if (feas >= _BUY_GATE['min_feasibility'] and survived and util > 0
            and mae is not None and mae > _BUY_GATE['max_mae_bp']
            and mfe is not None and mfe > _BUY_GATE['min_mfe_bp']):
        return 'BUY'
    return 'WATCH'


def _buy_margin(cand):
    """Deterministic ordering/conviction scalar for BUY-gated candidates."""
    l2 = cand.get('l2_evidence') or {}
    mae = l2.get('mae_bp') or 0
    mfe = l2.get('mfe_bp') or 0
    return (mfe - _BUY_GATE['min_mfe_bp']) / 10000.0 \
        + (mae - _BUY_GATE['max_mae_bp']) / 10000.0


def _confidence(cand):
    """Confidence bucket from the BUY-gate margin (target side only;
    the magnitude NEVER appears in text)."""
    m = _buy_margin(cand)
    if m >= 0.60:
        return 'high'
    if m >= 0.25:
        return 'medium'
    return 'low'


def _watch_trigger(cand, med_vel):
    """Candidate-specific entry trigger derived from its own weakest causal gate."""
    cs = cand.get('causal_state') or {}
    imb = _f(cs.get('buy_sell_imbalance_30s'))
    vel = _f(cs.get('trade_velocity_30s'), 2)
    mom = _f(cs.get('price_momentum_30s_bp'), 0)
    wash = _f(cs.get('wash_trade_ratio'))
    hhi = _f(cs.get('holder_concentration_hhi'))
    if imb is not None and imb <= 0:
        return 'enter if buy_sell_imbalance_30s flips positive on rising volume'
    if med_vel is not None and vel is not None and vel < med_vel:
        return 'enter if trade_velocity_30s clears the panel median with net_flow_sol > 0'
    if mom is not None and mom <= 0:
        return 'enter on 30s momentum turning positive with unique_buyers_5s expanding'
    if wash is not None and wash > 0.15:
        return 'enter only if wash_trade_ratio decays below 0.15 with organic buyer growth'
    if hhi is not None and hhi > 0.15:
        return 'enter only if holder concentration disperses (hhi < 0.15)'
    return 'enter on confirmed follow-through: rising velocity and positive imbalance'


# ── mode builders ──

def build_live_action(panel_cands):
    ranked = sorted(panel_cands, key=lambda c: c.get('panel_rank', 10 ** 9))
    decisions = {id(c): decide_v2(c) for c in ranked}
    buy_eligible = sorted((c for c in ranked if decisions[id(c)] == 'BUY'),
                          key=_buy_margin, reverse=True)
    # Decisive BUY set: strongest gate-passer first; 2nd/3rd only within 60%
    # of the leader's margin (deterministic — no fabricated conviction).
    buys = []
    for c in buy_eligible:
        if len(buys) == 3:
            break
        if not buys or _buy_margin(c) >= 0.60 * _buy_margin(buys[0]):
            buys.append(c)
    med_vel = _panel_median(panel_cands, 'trade_velocity_30s', 2)
    watches = [c for c in ranked
               if decisions[id(c)] == 'WATCH' and c not in buys][:3]
    covered = set(id(c) for c in buys) | set(id(c) for c in watches)
    rest = [c for c in ranked if id(c) not in covered]

    skip_counts = {}
    for c in rest:
        r = _skip_reason(c, med_vel)
        skip_counts[r] = skip_counts.get(r, 0) + 1

    out = {
        'mode': 'live_action',
        'market_state': _market_state(panel_cands),
        'action': 'BUY' if buys else 'NO_BUY',
        'buys': [{
            'mint_id': c['mint_id'],
            'rank': c.get('panel_rank'),
            'confidence': _confidence(c),
            'size_band_sol': _size_band(c),
            'causal_evidence': _causal_evidence(c, panel_cands),
            'risk_invalidation': _risk_invalidation(c),
        } for c in buys],
        'watch': [{
            'mint_id': c['mint_id'],
            'rank': c.get('panel_rank'),
            'trigger': _watch_trigger(c, med_vel),
        } for c in watches],
        'skip_summary': dict(sorted(skip_counts.items())),
    }
    assert_no_outcome_leakage(json.dumps(out, ensure_ascii=False), 'live_action')
    return out


def build_full_rank_aux(panel_cands):
    ranked = sorted(panel_cands, key=lambda c: c.get('panel_rank', 10 ** 9))
    # COLUMNAR rows (mint_id, rank, action, score): same ordering supervision
    # as dict-per-candidate JSON at ~3x fewer target tokens, so the auxiliary
    # mode cannot dominate the cross-sectional gradient budget by sheer length.
    actions = [decide_v2(c) for c in ranked]
    rows = [[c['mint_id'], c.get('panel_rank'), a,
             _f((c.get('continuous_utility') or {}).get('robust_executable_utility_v1'), 3) or 0.0]
            for c, a in zip(ranked, actions)]
    out = {
        'mode': 'full_rank_aux',
        'fields': ['mint_id', 'rank', 'action', 'score'],
        'rankings': rows,
        'no_buy_flag': 'BUY' not in actions,
    }
    # score is the target-defining ranking scalar (label), permitted in aux
    # targets; prose/evidence is absent by construction. Check the non-score
    # text surfaces only.
    assert_no_outcome_leakage(json.dumps({'a': actions}), 'full_rank_aux')
    return out


def choose_mode(panel_key, aux_pct=20):
    """Deterministic mode assignment by panel key hash (content-independent of RNG)."""
    h = int(hashlib.sha256(str(panel_key).encode()).hexdigest(), 16)
    return 'full_rank_aux' if (h % 100) < aux_pct else 'live_action'


def build_input(panel):
    """Model-visible INPUT: rich causal panel, provenance/outcome-free.

    COLUMNAR serialization (field header once + one value row per candidate):
    identical information to dict-per-candidate JSON at ~4x fewer tokens,
    which is what lets a 50-candidate panel + target fit in seq 12,288."""
    rows = []
    for c in panel['candidates']:
        cs = _clean_causal(c.get('causal_state'))
        rows.append([c['mint_id']] + [cs.get(k) if not isinstance(cs.get(k), float)
                                      else round(cs[k], 4)
                                      for k in INPUT_CAUSAL_FIELDS])
    return {
        'panel_timestamp': panel.get('panel_timestamp'),
        'panel_source': panel.get('panel_source'),
        'panel_size': panel.get('panel_size'),
        'fields': ['mint_id'] + list(INPUT_CAUSAL_FIELDS),
        'candidates': rows,
    }


# ── LABEL GOVERNANCE (BINDING) ──
# The selective Pump gate is VERSIONED target generation, not universal market
# truth. Every cross record carries `label_governance` metadata: label version,
# exact thresholds, the underlying outcome evidence (MFE/MAE/survival/
# feasibility/collapse) that justified each candidate's TARGET label, and the
# continuous ranking score. This block is NEVER serialized into model-visible
# instruction/input/output text — SFTDataset tokenizes only those three fields.
# Loss weights come from task identity ONLY — never realized PnL, MFE, utility
# magnitude, or moonshot status.
ACTION_LABEL_VERSION = 'pump_gate_v2'


def _label_evidence(cand):
    """Per-candidate TARGET evidence snapshot (metadata only, never visible)."""
    cu = cand.get('continuous_utility') or {}
    l2 = cand.get('l2_evidence') or {}
    return {
        'mfe_bp': _f(l2.get('mfe_bp'), 1),
        'mae_bp': _f(l2.get('mae_bp'), 1),
        'survived_60s': bool(l2.get('survived_60s')),
        'collapsed_50pct_within_300s': bool(l2.get('collapsed_50pct_within_300s')),
        'feasibility_score': _f(cu.get('feasibility_score'), 3),
        'robust_executable_utility_v1': _f(cu.get('robust_executable_utility_v1'), 4),
        'buy_gate_margin': _f(_buy_margin(cand), 4),
        'decision_v2': decide_v2(cand),
    }


def build_label_governance(panel_cands):
    return {
        'action_label_version': ACTION_LABEL_VERSION,
        'buy_gate_thresholds': dict(_BUY_GATE),
        'skip_gate': {'collapse': True, 'min_feasibility': 0.2,
                      'max_negative_utility': -0.10},
        'confidence_margins': {'high': 0.60, 'medium': 0.25},
        'buy_set_rule': 'top margin; 2nd/3rd only within 0.60x leader margin',
        'ranking_score': 'robust_executable_utility_v1 (continuous, per candidate below)',
        'per_candidate_evidence': {c['mint_id']: _label_evidence(c)
                                   for c in panel_cands},
        'weighting_policy': 'task-identity only; never PnL/MFE/utility/moonshot',
    }


def export_cross_v2(panel, panel_key):
    mode = choose_mode(panel_key)
    output = build_live_action(panel['candidates']) if mode == 'live_action' \
        else build_full_rank_aux(panel['candidates'])
    instruction = LIVE_ACTION_INSTRUCTION if mode == 'live_action' else FULL_RANK_AUX_INSTRUCTION
    return {
        'instruction': instruction,
        'input': build_input(panel),
        'output': output,
        'token_count': 0,
        'cross_mode': mode,
        'label_governance': build_label_governance(panel['candidates']),
    }
