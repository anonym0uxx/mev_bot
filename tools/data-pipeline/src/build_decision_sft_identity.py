#!/usr/bin/env python3
"""Forward-looking decision-SFT builder WITH identity + narrative fields.

Operator decision (2026-09-26): the narrative field may be added to
FORWARD-LOOKING corpora only. The corpus used for the sft-013 base model is
frozen and must not move — it is pinned by
`/training/rel/CANDIDATE_RELEASE_LINUX.json` and lives on a READ-ONLY mount
(`/mnt/data/.../astra_review_v1/revision_candidates/`), so this script never
writes into it.

How the frozen text is protected:

  * This module does NOT reimplement the prompt. It imports the frozen builder
    by path and calls ITS `decision_state` and `render_prompt` verbatim, then
    inserts an additive block. The base text therefore cannot drift from the
    frozen builder — if the frozen builder changes, this changes with it.
  * With no `--names` given, the emitted prompt is asserted BYTE-IDENTICAL to
    what the frozen builder would emit (`--verify-identical` does this over the
    real support set). That is the guarantee that the frozen path is untouched.

Leak discipline is inherited and extended: the added block is re-checked against
OUTCOME_FIELDS and the decision labels. A name that trips the check QUARANTINES
the record — it is never silently emitted, because a token literally named "BUY"
would otherwise inject the answer into the prompt.

Causality: only facts knowable at the decision time are added. The name comes
from the mint's creation metadata (strictly before any decision on it) and the
narrative fields come from a sidecar produced by the same causal resolver the
live path uses. A mint with no name is marked `name_inference_missing` — absence
is recorded as absence, never filled in.
"""
import argparse
import importlib.util
import json
import re
import sys
from collections import Counter
from pathlib import Path

FROZEN_BUILDER = Path(
    "/mnt/data/mev_bot-artifacts/north_star/aggregation/north_star_training_dataset_v1"
    "/corpus_builder_v1/integrated_data_v1/full_history_closure_v1/economics_v1"
    "/astra_review_v1/build_decision_sft.py"
)

# The frozen tree. Never written to; read for hashes/manifests only.
FROZEN_ROOT = FROZEN_BUILDER.parent / "revision_candidates"
FROZEN_SUPPORT = FROZEN_ROOT / "support_export"

MISSING = "NOT_RESOLVED"


def load_frozen():
    """Import the frozen builder so its functions are reused, not copied."""
    if not FROZEN_BUILDER.is_file():
        raise SystemExit("frozen builder not found: " + str(FROZEN_BUILDER))
    spec = importlib.util.spec_from_file_location("frozen_build_decision_sft", FROZEN_BUILDER)
    if spec is None or spec.loader is None:
        raise SystemExit("cannot load the frozen builder: " + str(FROZEN_BUILDER))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


F = load_frozen()

# Insertion point: right after the mint/wallet/decision lines, before the window.
MARK = "\n\nSTRICT PRIOR WINDOW"


def render_prompt_with_identity(state, name=None, symbol=None, narrative=None, forward=False):
    """Frozen prompt verbatim, plus an additive identity/narrative block.

    `forward=False` (the frozen posture) returns the frozen prompt EXACTLY —
    that equality is the contract the parity test asserts.

    `forward=True` (a forward-looking corpus) ALWAYS emits the block, and a mint
    with no name renders `NOT_RESOLVED`. That distinction matters: "the corpus is
    the frozen one" and "this mint has no name" are different facts, and merging
    them would either move the frozen text or silently omit an absence.
    """
    base = F.render_prompt(state)
    if not forward:
        return base
    head, sep, tail = base.partition(MARK)
    if not sep:
        raise ValueError("frozen prompt layout changed: insertion marker not found")
    block = [
        "",
        "IDENTITY AND NARRATIVE (knowable at the decision time: the name is the",
        "mint's creation metadata, the narrative fields are the resolved attachment",
        "and its crowding stage over the prior mint stream):",
        f"  token name: {name if name else MISSING}",
        f"  token symbol: {symbol if symbol else MISSING}",
        f"  narrative family: {narrative['family'] if narrative else MISSING}",
        f"  narrative stage: {narrative['stage'] if narrative else MISSING}",
        f"  narrative verdict: {narrative['verdict'] if narrative else MISSING}",
        f"  narrative lexicon version: {narrative['lexicon_version'] if narrative else MISSING}",
    ]
    return head + "\n" + "\n".join(block) + sep + tail


def block_leaks(prompt):
    """Re-check the added block with the frozen builder's own leak rules.

    Returns the list of failures (empty = clean). Mirrors F.audit_prompt but for
    the identity block alone, so a bad name cannot hide behind a good prompt.
    """
    lowered = prompt.lower()
    return [f for f in F.OUTCOME_FIELDS if f in lowered]


def decision_word_in(text):
    """Any decision label appearing as a word in the identity block."""
    if text is None:
        return None
    low = text.lower()
    for label in F.DECISION.values():
        if re.search(r"\b" + re.escape(label.lower()) + r"\b", low):
            return label
    return None


def build_record_with_identity(record, events, name, symbol, narrative, forward=False):
    """Frozen record + identity/narrative. Returns (record, prompt, failures).

    `name_inference_missing` is only meaningful in forward mode: in the frozen
    posture the field is simply absent from the record, which is why the frozen
    corpus cannot be confused for one that tried and failed to resolve a name.
    """
    state = F.decision_state(record, events)
    prompt = render_prompt_with_identity(state, name, symbol, narrative, forward=forward)
    source_action = record["meta"]["source_action"]
    target = F.render_target(source_action, state, source_action)

    failures = F.audit_prompt(prompt, source_action)
    # The identity block must not smuggle the answer or an outcome field.
    for field in (name, symbol):
        hit = decision_word_in(field)
        if hit:
            failures.append("identity_name_contains_decision_word:" + hit)
    if narrative:
        for key in ("family", "stage", "verdict"):
            hit = decision_word_in(str(narrative.get(key)))
            if hit:
                failures.append(f"narrative_{key}_contains_decision_word:{hit}")

    meta = dict(record["meta"])
    meta.update(
        task="next_action_decision",
        target=F.DECISION[source_action],
        kind="decision",
        source_action=source_action,
        decision_time_block_second=state["decision_block_second"],
        decision_state=state,
        rationale_origin="deterministic_decision_state_restatement",
        policy_supervision=False,
        economic_truth_verified=False,
        training_authorized=False,
    )
    if forward:
        # Additive, forward-only. In the frozen posture these keys are ABSENT, so
        # a frozen record is indistinguishable from what the frozen builder emits.
        meta.update(
            token_name=name,
            token_symbol=symbol,
            name_inference_missing=name is None,
            narrative_family=(narrative or {}).get("family"),
            narrative_stage=(narrative or {}).get("stage"),
            narrative_verdict=(narrative or {}).get("verdict"),
            narrative_lexicon_version=(narrative or {}).get("lexicon_version"),
            schema="decision_sft_with_identity_v1",
        )
    built = dict(
        messages=[dict(role="user", content=prompt),
                  dict(role="assistant", content=target)],
        evidence=dict(decision_state=state,
                      source_action_label=source_action,
                      anchor_movement=record["evidence"]["anchor_movement"]),
        meta=meta,
    )
    return built, prompt, failures


def load_jsonl_map(path, keys):
    """mint -> dict of `keys`, from a jsonl file. Missing file -> empty map."""
    out = {}
    p = Path(path)
    if not p.is_file():
        return out
    with p.open(encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            row = json.loads(line)
            mint = row.get("mint")
            if mint:
                out[mint] = {k: row.get(k) for k in keys}
    return out


def load_names_parquet(path):
    """mint -> (name, symbol) from a tokens parquet, if pandas is available."""
    try:
        import pandas as pd
    except ImportError:
        return {}
    p = Path(path)
    if not p.is_file():
        return {}
    df = pd.read_parquet(p, columns=["mint", "name", "symbol"])
    df = df.drop_duplicates(subset=["mint"], keep="first")
    out = {}
    for mint, name, symbol in zip(df["mint"], df["name"], df["symbol"]):
        n = name if isinstance(name, str) and name.strip() else None
        s = symbol if isinstance(symbol, str) and symbol.strip() else None
        out[mint] = {"name": n, "symbol": s}
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--support", type=Path, default=FROZEN_SUPPORT)
    ap.add_argument("--out", type=Path, required=True,
                    help="output dir; MUST NOT be inside the frozen tree")
    ap.add_argument("--families", nargs="*", default=["observed_sft_train"])
    ap.add_argument("--names", type=Path, default=None,
                    help="jsonl {mint,name,symbol} or tokens parquet")
    ap.add_argument("--names-format", choices=["jsonl", "parquet"], default="jsonl")
    ap.add_argument("--narrative", type=Path, default=None,
                    help="jsonl {mint,family,stage,verdict,lexicon_version}")
    ap.add_argument("--limit", type=int, default=0, help="0 = no limit")
    ap.add_argument("--verify-identical", action="store_true",
                    help="assert the no-identity prompt equals the frozen builder's")
    args = ap.parse_args()

    out_dir = args.out.resolve()
    if FROZEN_ROOT.resolve() in out_dir.parents or out_dir == FROZEN_ROOT.resolve():
        raise SystemExit("REFUSING to write inside the frozen sft-013 corpus tree")

    args.out.mkdir(parents=True, exist_ok=True)

    # Events come from the same sqlite index the frozen builder uses.
    import sqlite3
    manifest = json.loads((args.support / "MANIFEST.json").read_text(encoding="utf-8"))
    index = args.support / manifest["index_file"]
    db = sqlite3.connect(index.as_uri() + "?mode=ro", uri=True)
    events = {}
    for mint, payload in db.execute("SELECT mint,payload FROM events"):
        events.setdefault(mint, []).append(json.loads(payload))
    db.close()

    names = {}
    if args.names:
        names = (load_names_parquet(args.names) if args.names_format == "parquet"
                 else load_jsonl_map(args.names, ("name", "symbol")))
    narratives = (load_jsonl_map(args.narrative, ("family", "stage", "verdict", "lexicon_version"))
                  if args.narrative else {})

    # FORWARD posture is declared by supplying an identity/narrative source. With
    # none supplied the build is the frozen build, byte for byte — that is what
    # keeps the sft-013 corpus path provably unmoved.
    forward = bool(args.names or args.narrative)

    counts, leaks, identical_mismatch = Counter(), [], 0
    for family in args.families:
        src = args.support / (family + ".jsonl")
        if not src.is_file():
            raise SystemExit("missing family: " + str(src))
        dst = out_dir / (family.replace("observed_sft_", "decision_sft_") + ".jsonl")
        n = 0
        with src.open(encoding="utf-8") as f, dst.open("w", encoding="utf-8", newline="\n") as out:
            for line in f:
                if args.limit and n >= args.limit:
                    break
                record = json.loads(line)
                mint = record["meta"]["mint"]
                ident = names.get(mint, {})
                given = names.get(mint) if args.names else None
                nm = given["name"] if given else None
                sy = given["symbol"] if given else None
                nar = narratives.get(mint)

                if args.verify_identical:
                    state = F.decision_state(record, events.get(mint, []))
                    frozen_prompt = F.render_prompt(state)
                    mine = render_prompt_with_identity(state, nm, sy, nar, forward=forward)
                    if frozen_prompt != mine:
                        identical_mismatch += 1
                        if identical_mismatch == 1:
                            print("MISMATCH on " + mint, file=sys.stderr)

                built, prompt, failures = build_record_with_identity(
                    record, events.get(mint, []), nm, sy, nar, forward=forward)
                if failures:
                    leaks.append(dict(candidate_id=record["meta"]["candidate_id"],
                                      failures=failures))
                    continue
                out.write(json.dumps(built, sort_keys=True, ensure_ascii=False,
                                     separators=(",", ":")) + "\n")
                counts[family] += 1
                n += 1
        print(f"{family}: kept={counts[family]} quarantined={len(leaks)}")

    print(json.dumps(dict(
        kept=dict(counts), quarantined=len(leaks),
        names_loaded=len(names), narratives_loaded=len(narratives),
        verify_identical_requested=bool(args.verify_identical),
        identical_mismatch=identical_mismatch,
    ), indent=1))
    if leaks:
        print("first quarantines:", json.dumps(leaks[:3]), file=sys.stderr)
    if args.verify_identical and identical_mismatch:
        raise SystemExit("FROZEN PARITY FAILED: prompt text diverged")
    return 0


if __name__ == "__main__":
    sys.exit(main())
