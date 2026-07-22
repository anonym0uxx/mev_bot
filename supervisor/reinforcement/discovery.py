"""
Hard-component discovery — the supervisor determines which components are HARD by
READING THE CONSTITUTION, not from a hardcoded list and not from files you drop.

Three sources, unioned, in decreasing authority:
  1. Explicit declaration in the constitution: a line naming components as hard/
     reinforcement-governed (e.g. "HARD components: reducer, shred, lockfree, ...")
     or any `run_reinforcement`/"hard component" sentence listing snake_case names.
  2. The seed list the constitution's reinforcement doc names (floor; never shrinks).
  3. Dossiers already on disk (a component with a dossier is hard by definition).

Discovery is advisory-but-binding in one direction: it can only ADD components to the
hard set, never remove one. That asymmetry is deliberate — a parsing miss must never
silently downgrade a component out of reinforcement + gating.

Everything found here that lacks a dossier becomes a `missing_dossiers()` entry, which
fails the milestone gate and emits an authoring brief. So a NEW hard component named in
a future constitution edit is picked up automatically, with no code edit and no file drop.
"""
from __future__ import annotations

import re
from pathlib import Path

# snake_case identifiers, 3+ chars, that look like component names
_IDENT = re.compile(r"\b([a-z][a-z0-9]{2,}(?:_[a-z0-9]+)+|reducer|replay|shred|lockfree)\b")

# lines that declare hardness
_DECLARATIVE = re.compile(
    r"(hard[- ]component|hard components|reinforcement[- ]governed|"
    r"run_reinforcement|task dossier|dossier-governed|HARD:)",
    re.IGNORECASE)

# obvious non-components that show up in the same sentences
_STOPWORDS = {
    "run_reinforcement", "gate_verify", "check_tier0", "register_artifact",
    "evaluator_verify", "experiment_run", "promotion_check", "live_status",
    "evidence_status", "record_escalation", "bench_endpoint",
    "property_test", "reference_pattern", "temperature_band", "max_lines",
    "leaf_id", "depends_on", "constitution_refs", "acceptance_criteria",
    "hard_components", "one_shot", "net_sol", "take_profit", "max_hold",
    "stop_loss", "min_hold", "target_cpu", "overflow_checks", "opt_level",
    "codegen_units", "panic_abort", "lint_allow", "json_schema",
    "human_gate_required", "human_gate", "tier0_gate",
}


def discover_from_constitution(constitution_path: str | Path,
                               max_bytes: int = 4_000_000) -> set[str]:
    """Parse the committed constitution for declared hard components.

    Conservative by construction: only identifiers appearing on a line that also
    carries a hardness declaration are admitted, minus a stopword list of tool and
    field names. Returns an empty set on any read failure (callers union with the
    seed list, so a failure degrades to the known floor, never to nothing).
    """
    try:
        p = Path(constitution_path)
        if not p.is_file():
            return set()
        text = p.read_text(encoding="utf-8", errors="replace")[:max_bytes]
    except OSError:
        return set()

    found: set[str] = set()
    for line in text.splitlines():
        if not _DECLARATIVE.search(line):
            continue
        for m in _IDENT.finditer(line):
            name = m.group(1)
            if name in _STOPWORDS:
                continue
            if len(name) > 40:
                continue
            found.add(name)
    return found


def discovery_report(constitution_path: str | Path,
                     seed: list[str],
                     on_disk: list[str]) -> dict:
    """Explainable discovery: what came from where, and what still needs a dossier."""
    parsed = discover_from_constitution(constitution_path)
    live = sorted(set(seed) | set(on_disk) | parsed)
    return {
        "live_hard_components": live,
        "from_constitution": sorted(parsed),
        "from_seed": sorted(seed),
        "from_disk": sorted(on_disk),
        "newly_discovered": sorted(parsed - set(seed) - set(on_disk)),
        "needs_dossier": sorted(set(live) - set(on_disk)),
        "note": ("Discovery can only ADD to the hard set. A component named as hard in the "
                 "constitution but lacking a dossier fails the milestone gate and emits an "
                 "authoring brief — no file drop or code edit is required to register it."),
    }
