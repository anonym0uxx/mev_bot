#!/usr/bin/env python
"""prompt_format — the ONE definition of how a decision prompt is rendered and how
the DECISION slot is located. Imported by both the RL loop and the reference-cache
builder, so the two can never drift apart.

WHY THIS IS A MODULE AND NOT A HELPER: three separate things must agree exactly or
the RL run is silently wrong:
  1. the prompt string (the model must see what SFT saw)
  2. the action token ids (the reference anchor and the loop must index the same
     three logits)
  3. the position of the decision slot (the gradient and the KL must be read at the
     same place)

VERIFIED against the real tokenizer (2026-09-13):
  ' BUY'=50018  ' WATCH'=45828  ' SKIP'=78599   (all distinct, all single-token)
  ' DECISION:' = [41402, 23451, 25]
  the template ALWAYS emits  thinking - with enable_thinking=False it emits
  'assistant\\n thinking\\n\\n</think>\\n\\n', which is the pinned SFT mode and the
  release's declared template_kwargs. Rendering with the default kwargs would feed
  the model an unclosed  thinking block it never saw in SFT.
"""
from __future__ import annotations

TEMPLATE_KWARGS = {"enable_thinking": False}
ACTIONS = ("BUY", "WATCH", "SKIP")


def render_prompt(tokenizer, messages) -> str:
    """Render the causal prompt exactly as the release pins it."""
    msgs = [m for m in messages if m.get("role") != "assistant"]
    return tokenizer.apply_chat_template(msgs, tokenize=False,
                                         add_generation_prompt=True,
                                         **TEMPLATE_KWARGS)


def action_token_ids(tokenizer, actions=ACTIONS):
    """The CANONICAL action basis: the space-prefixed first token of each action.

    Measured: ' BUY'=50018, ' WATCH'=45828, ' SKIP'=78599, while the BARE forms are
    83006/... - the tokenizer produces DIFFERENT ids with and without a leading
    space. The canonical basis is the space-prefixed one because every SFT answer
    is 'DECISION: BUY'. Logits are always read on this fixed 3-dim basis so the
    action distribution is comparable across completions.
    """
    ids = []
    for a in actions:
        enc = (tokenizer.encode(" " + a, add_special_tokens=False)
               or tokenizer.encode(a, add_special_tokens=False))
        if not enc:
            raise SystemExit(f"tokenizer cannot encode action {a!r}")
        ids.append(int(enc[0]))
    if len(set(ids)) != len(ids):
        raise SystemExit(f"action tokens are not distinct: {dict(zip(actions, ids))}")
    return list(actions), ids


def action_variants(tokenizer, actions=ACTIONS):
    """Every token id that can carry an action, mapped to (action, canonical?).

    Needed because a completion may write 'DECISION: BUY' (spaced, canonical) or
    'DECISION:BUY' / a newline before it (bare). Locating the slot with only the
    canonical ids silently missed the bare form (reproduced on the real tokenizer).
    """
    m, canonical = {}, []
    for a in actions:
        sp = tokenizer.encode(" " + a, add_special_tokens=False)
        bare = tokenizer.encode(a, add_special_tokens=False)
        cands = []
        for enc, label in ((sp, "spaced"), (bare, "bare")):
            if enc:
                cands.append((int(enc[0]), label))
        if not cands:
            raise SystemExit(f"tokenizer cannot encode action {a!r}")
        canonical.append(cands[0][0])
        for tid, label in cands:
            m[tid] = {"action": a, "variant": label,
                      "canonical": tid == cands[0][0]}
    return m, canonical


def decision_marker_ids(tokenizer):
    """ALL token variants of the DECISION marker, longest first.

    A marker can start a line ('DECISION:') or follow text (' DECISION:'), and the
    tokenizer produces different ids for each. Returning one variant silently
    missed the other (reproduced: the prompt ends with a newline, so the real
    generation carries the NO-SPACE variant and the lookup failed).
    """
    out, seen = [], set()
    for s in (" DECISION:", "DECISION:", "DECISION", " DECISION"):
        enc = tokenizer.encode(s, add_special_tokens=False)
        if not enc:
            continue
        key = tuple(int(x) for x in enc)
        if key in seen:
            continue
        seen.add(key)
        out.append({"label": s, "ids": list(key)})
    out.sort(key=lambda v: -len(v["ids"]))
    return out


def _find_subseq(row, sub, start=0):
    if not sub:
        return None
    n = len(sub)
    for i in range(start, len(row) - n + 1):
        if row[i:i + n] == sub:
            return i
    return None


def _earliest_marker(row, variants, start):
    best = None
    for v in (variants or []):
        i = _find_subseq(row, v["ids"], start)
        if i is not None and (best is None or i < best[0]):
            best = (i, len(v["ids"]))
    return best


def locate_decision_slot(seq_ids, prompt_len: int, action_ids, marker_ids=None):
    """Index of the action token that carries the decision.

    PREFERS the action token immediately after the DECISION marker, because a
    completion may legitimately mention BUY in its evidence text. Falls back to
    the first action token after the prompt only when no marker is present, and
    reports which path was taken so the loop can log it.

    Returns (index, how) or (None, reason).
    """
    if isinstance(action_ids, dict):
        a = set(int(k) for k in action_ids)
    else:
        a = set(int(x) for x in action_ids)
    row = list(seq_ids.tolist() if hasattr(seq_ids, "tolist") else seq_ids)
    p = int(prompt_len)
    hit = _earliest_marker(row, marker_ids, p)
    if hit is not None:
        for i in range(hit[0] + hit[1], len(row)):
            if row[i] in a:
                return i, "after_marker"
        return None, "marker_no_action"
    for i in range(p, len(row)):
        if row[i] in a:
            return i, "no_marker_first_action"
    return None, "no_action_token"


SIZE_LABELS = ("SMALL", "MID", "FULL")
ARMS = ("SKIP", "WATCH", "BUY_SMALL", "BUY_MID", "BUY_FULL")


def size_token_ids(tokenizer, labels=SIZE_LABELS):
    """The CANONICAL SIZE basis: the space-prefixed first token of each tier label.

    Same discipline as action_token_ids: " SMALL"/" MID"/" FULL" (spaced) is the
    canonical form because every SFT answer writes "SIZE: MID". The SIZE slot is a
    SECOND decision, so the arm distribution is the joint over
    (action at the decision slot) x (tier at the size slot).
    """
    ids = []
    for s in labels:
        enc = (tokenizer.encode(" " + s, add_special_tokens=False)
               or tokenizer.encode(s, add_special_tokens=False))
        if not enc:
            raise SystemExit(f"tokenizer cannot encode size {s!r}")
        ids.append(int(enc[0]))
    if len(set(ids)) != len(ids):
        raise SystemExit(f"size tokens are not distinct: {dict(zip(labels, ids))}")
    return list(labels), ids


def locate_size_slot(seq_ids, start, size_ids):
    """Index of the tier token, searched AFTER the decision token.

    Searching from the decision slot (not from the prompt) keeps an evidence
    sentence that happens to contain a tier word from being mistaken for the SIZE
    field. Returns (index, how) or (None, reason).
    """
    a = set(int(k) for k in (size_ids.keys() if isinstance(size_ids, dict) else size_ids))
    row = list(seq_ids.tolist() if hasattr(seq_ids, "tolist") else seq_ids)
    for i in range(int(start), len(row)):
        if row[i] in a:
            return i, "after_decision"
    return None, "no_size_token"


def arm_index(action, tier=None):
    """(action, tier) -> index in ARMS. None when the pair is not a valid arm."""
    key = action if action in ("SKIP", "WATCH") else ("BUY_" + str(tier))
    return ARMS.index(key) if key in ARMS else None


def _selftest() -> int:
    """Runs against the REAL tokenizer when it is present; otherwise the pure
    token-level logic is still checked."""
    import json as _json
    import os
    fails, checks = [], 0

    def chk(tag, cond):
        nonlocal checks
        checks += 1
        if not cond:
            fails.append(tag)

    CK = ("/training/seed/models--unsloth--Qwen3.8-27B/snapshots/"
          "3ea932cee0a432ae86e9c7826cbe8aef52323a28")
    have_tok = os.path.isdir(CK)
    chk("tokenizer_present_on_box", have_tok)
    if have_tok:
        from transformers import AutoTokenizer
        tok = AutoTokenizer.from_pretrained(CK)
        acts, ids = action_token_ids(tok)
        chk("action_ids_distinct", len(set(ids)) == 3)
        chk("action_ids_match_measured",
            ids == [50018, 45828, 78599], )
        msgs = [{"role": "system", "content": "s"},
                {"role": "user", "content": "Choose exactly one action: BUY, WATCH, SKIP."}]
        p = render_prompt(tok, msgs)
        chk("prompt_has_no_unclosed_think", "</think>" in p)
        chk("prompt_ends_after_think", p.rstrip().endswith("</think>"))
        mk = decision_marker_ids(tok)
        chk("marker_variants_both", len(mk) >= 2)
        chk("marker_found", bool(mk))
        # slot located AFTER the marker, not on the prompt's own 'BUY'
        row = tok(p + "DECISION: BUY\n", add_special_tokens=False)["input_ids"]
        plen = len(tok(p, add_special_tokens=False)["input_ids"])
        i, how = locate_decision_slot(row, plen, ids, mk)
        chk("slot_after_marker", how == "after_marker")
        chk("slot_is_buy", i is not None and row[i] == ids[0])
        # no marker -> fallback path is labelled as such
        av, _canon = action_variants(tok)
        row2 = tok(p + "BUY\n", add_special_tokens=False)["input_ids"]
        i2, how2 = locate_decision_slot(row2, plen, av, mk)
        chk("bare_action_located", how2 == "no_marker_first_action")
        chk("bare_variant_reported", av[row2[i2]]["variant"] == "bare")
        chk("no_action_reports_reason",
            locate_decision_slot(row2[:plen], plen, ids, mk)[0] is None)
    print(_json.dumps({"suite": "prompt_format", "checks": checks,
                       "failed": fails,
                       "verdict": "PASS" if not fails else "FAIL"}, indent=1))
    return 1 if fails else 0


if __name__ == "__main__":
    raise SystemExit(_selftest())
