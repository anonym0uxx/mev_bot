"""Turn real training-run receipts into distributed_loss_runtime evidence.

The gate wants four things: world_size in (2, 3), every rank exited zero, the
weighted loss mass reconciled against an independent reference to better than
1e-4 relative error, and native DeepSpeed actually verified.

`build_gate_evidence.distributed_loss_gate` accepts a mapping with exactly those
keys. This collects them from what a REAL run produced:

  * one `runtime_evidence.json` per rank (written by train_qwen27b with
    `--runtime_evidence_out`), and
  * the independent single-process reference from `dryrun_loss_tokens.py`,
    which computes the supervised-token mass without a model.

Nothing here is synthesised: if a rank is missing, or its receipt disagrees, the
gate is reported unsatisfied rather than filled in.

Usage:
  python collect_runtime_evidence.py --rank-receipts DIR \
      --loss-dryrun DRYRUN_LOSS_TOKENS.json --phase cpt --out EVIDENCE.json
"""
import argparse
import json
import sys
from pathlib import Path


RANK_SCHEMA = "qwen27b_runtime_evidence_v1"


def load_rank_receipts(directory):
    """Only genuine per-rank receipts. Anything else (including this collector's
    own output) is skipped rather than parsed as a rank."""
    receipts = []
    for path in sorted(Path(directory).glob("**/*.json")):
        try:
            doc = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, ValueError) as exc:
            print(f"[skip] {path}: not JSON ({exc})")
            continue
        if not isinstance(doc, dict) or doc.get("schema") != RANK_SCHEMA:
            continue
        if doc.get("global_rank") is None or doc.get("world_size") is None:
            print(f"[skip] {path}: not a rank receipt (missing rank/world_size)")
            continue
        receipts.append((path, doc))
    return receipts


def reference_mass(loss_dryrun, phase):
    """Independent supervised-token mass from the loader dry-run, no model."""
    doc = json.loads(Path(loss_dryrun).read_text(encoding="utf-8"))
    node = doc.get(phase) or doc
    for key in ("validation", "val"):
        if key in node:
            block = node[key]
            for field in ("weighted_loss_token_mass", "shifted_loss_tokens"):
                if block.get(field) is not None:
                    return float(block[field]), f"{phase}.{key}.{field}"
    raise ValueError(f"no reference supervised-token mass for phase {phase!r}")


def finish(result):
    """Every return path must carry the verdict; a missing key is not falsy-safe."""
    result["passed"] = not result["failures"]
    return result


def collect(rank_receipts, loss_dryrun, phase, all_ranks_exit_zero):
    receipts = load_rank_receipts(rank_receipts)
    result = {
        "schema": "qwen27b_distributed_loss_runtime_v1",
        "phase": phase,
        "rank_receipts_found": len(receipts),
        "world_size": None,
        "all_ranks_exit_zero": bool(all_ranks_exit_zero),
        "native_deepspeed_verified": False,
        "weighted_loss_relative_error": None,
        "failures": [],
    }
    if not receipts:
        result["failures"].append("no rank receipts found")
        return finish(result)

    worlds = {r.get("world_size") for _, r in receipts}
    ranks = sorted(int(r["global_rank"]) for _, r in receipts)
    stages = {r.get("deepspeed_zero_stage") for _, r in receipts}
    observed = [r.get("validation_loss_token_mass") for _, r in receipts
                if r.get("validation_loss_token_mass") is not None]

    result["world_size"] = worlds.pop() if len(worlds) == 1 else None
    result["ranks_present"] = ranks
    result["zero_stages"] = sorted(s for s in stages if s is not None)

    if len(worlds) > 0:
        result["failures"].append("ranks disagree on world_size")
    if result["world_size"] not in (2, 3):
        result["failures"].append(f"world_size {result['world_size']} not a verified multi-rank run")
    if len(ranks) != (result["world_size"] or 0):
        result["failures"].append(
            f"expected one receipt per rank, found {len(ranks)} for world_size {result['world_size']}")
    if result["zero_stages"] != [3]:
        result["failures"].append(f"ZeRO stage(s) {result['zero_stages']}, expected [3]")
    else:
        result["native_deepspeed_verified"] = True

    ref, ref_key = reference_mass(loss_dryrun, phase)
    result["reference_supervised_tokens"] = ref
    result["reference_source"] = ref_key
    if not observed:
        result["failures"].append("no rank reported a validation token mass")
        return finish(result)
    # Ranks compute the same global denominator; disagreement means a broken gather.
    if len(set(observed)) != 1:
        result["failures"].append(f"ranks disagree on validation mass: {sorted(set(observed))}")
        return finish(result)
    rel = abs(observed[0] - ref) / ref if ref else None
    result["observed_supervised_tokens"] = observed[0]
    result["weighted_loss_relative_error"] = rel
    if rel is None or abs(rel) >= 1e-4:
        result["failures"].append(
            f"weighted loss mass not reconciled: observed {observed[0]} vs reference {ref} "
            f"(relative error {rel})")
    return finish(result)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--rank-receipts", required=True,
                    help="directory containing the per-rank runtime_evidence.json files")
    ap.add_argument("--loss-dryrun", required=True,
                    help="DRYRUN_LOSS_TOKENS.json from dryrun_loss_tokens.py")
    ap.add_argument("--phase", choices=["cpt", "sft"], required=True)
    ap.add_argument("--ranks-exited-zero", action="store_true",
                    help="set only if you actually observed all ranks exit 0")
    ap.add_argument("--out", required=True)
    args = ap.parse_args(argv)

    result = collect(args.rank_receipts, args.loss_dryrun, args.phase,
                     args.ranks_exited_zero)
    Path(args.out).write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({k: result[k] for k in
                      ("world_size", "all_ranks_exit_zero", "native_deepspeed_verified",
                       "weighted_loss_relative_error", "passed", "failures")}, indent=2))
    return 0 if result.get("passed") else 2


if __name__ == "__main__":
    sys.exit(main())
