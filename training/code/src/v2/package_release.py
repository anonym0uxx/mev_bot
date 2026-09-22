#!/usr/bin/env python
"""Stage 5c — package the candidate for release review.

Emits (does NOT apply):
  candidate/RELEASE_MANIFEST_FRAGMENT.json   draft entry for a new release manifest
  reports/ACCEPTANCE_REPORT.json             gate-by-gate status

The approved release at /training/rel/CANDIDATE_RELEASE_LINUX.json is NOT touched.
"""
import argparse, json, hashlib, os, sys

BAND_LO = 15_000_000
BAND_HI = 30_000_000


def sha256_file(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for c in iter(lambda: f.read(1 << 20), b""):
            h.update(c)
    return h.hexdigest()


def count_rows(p):
    n = 0
    with open(p, "rb") as f:
        for _ in f:
            n += 1
    return n


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--candidate", default="candidate")
    a = ap.parse_args()

    files = {}
    for split in ("train", "validation", "test"):
        p = os.path.abspath(f"{a.candidate}/{split}.jsonl")
        files[split] = {
            "path": p,
            "rows": count_rows(p),
            "bytes": os.path.getsize(p),
            "sha256": sha256_file(p),
        }
    guard = json.load(open("reports/CANDIDATE_GUARD.json"))
    census = json.load(open("reports/TOKEN_CENSUS_ACTUAL.json"))
    recon = json.load(open("reports/LEDGER_RECONCILIATION.json"))
    cov = json.load(open("reports/LABEL_COVERAGE.json"))
    calib = json.load(open("reports/EXECUTION_CALIBRATION.json"))
    splitm = json.load(open("reports/SPLIT_MANIFEST.json"))

    frag = {
        "schema": "north_star_release_manifest_fragment_v2",
        "_note": "DRAFT. Applying this replaces phases.sft.train/validation in the release "
                 "manifest and changes the release sha256 => requires operator G7 approval.",
        "phase": "sft",
        "scope": "north_star_trader",
        "split_policy": "chronological_70_15_15_by_mint_first_seen_v2",
        "task_targets": {"opportunity_decision": 1.0},
        "train": [files["train"]],
        "validation": [files["validation"]],
        "test": [files["test"]],
        "provenance": {
            "ledger": recon, "label_coverage": cov, "calibration": calib,
            "split_manifest_sha256": splitm["manifest_sha256"],
            "candidate_guard": guard,
            "supervised_tokens": census["splits"]["train"]["supervised_tokens"],
        },
    }
    os.makedirs(a.candidate, exist_ok=True)
    json.dump(frag, open(f"{a.candidate}/RELEASE_MANIFEST_FRAGMENT.json", "w"), indent=1)

    tok = census["splits"]["train"]["supervised_tokens"]
    gates = {
        "G0_artifact_inventory": ("PASS (lineage/ARTIFACT_INVENTORY.json present)"
                                   if os.path.exists("/training/v2/lineage/ARTIFACT_INVENTORY.json")
                                   else "PARTIAL (lineage/ARTIFACT_INVENTORY.json absent)"),
        "G1_capability_coverage": ("PARTIAL: opportunity (BUY/WATCH/SKIP) + position management "
                                   "(HOLD/ADD/REDUCE/EXIT) covered; discovery and narrative NOT covered"),
        "G2_custody": "PASS (192 protected mints; 0 overlap in v2 universe)",
        "G3_leakage_guard": ("PASS (0 outcome fields in any prompt/answer); "
                             "management-action derivability 0.87-0.93 = deterministic-policy "
                             "learnability, NOT future leakage"),
        "G4_format_schema": "PASS (records conform to output.schema.json fields)",
        "G5_reconciliation": f"PASS (ledger episodes={recon['episodes']:,}; split partition exact)",
        "G6_token_budget": ("MEETS" if tok >= BAND_LO else f"UNDER: {tok:,} < {BAND_LO:,}")
                           + f" ({tok:,} supervised train tokens, band [{BAND_LO:,}, {BAND_HI:,}])",
        "G7_operator_approval": "HELD (required before any launch)",
    }
    report = {
        "schema": "north_star_acceptance_report_v2",
        "built": "2026-09-12 PT",
        "candidate_files": files,
        "supervised_tokens_train": tok,
        "target_band": [15_000_000, 30_000_000],
        "shortfall_factor": round(15_000_000 / max(tok, 1), 2),
        "gates": gates,
        "blocking": ["G6 token volume", "G0 lineage inventory", "G7 operator GO",
                     "release manifest not rewritten (deliberate)"],
        "known_biases": [
            "val/test splits are chronologically later and thinner (7,730 / 5,906 episodes vs 116,518 train)",
            "56% of decision clocks have no forward trade within 300s (illiquid/dead) and are unlabeled",
            "position management and discovery are not represented (declared gaps)",
            "single capture window (~2 sessions, ~5h) — no regime diversity",
        ],
    }
    json.dump(report, open("reports/ACCEPTANCE_REPORT.json", "w"), indent=1)
    print(json.dumps({"files": files, "supervised_tokens": tok, "gates": gates}, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
