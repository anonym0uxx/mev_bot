"""
Task dossiers — the "intelligence in the scaffolding" for HARD components.

A dossier is authored once (design-once / execute-forever) and reused every time GLM
touches that component. It contains the decomposition tree, per-leaf specs, reference
patterns, and the property-test battery that guards correctness independent of the code.

Dossiers are stored as YAML under supervisor/reinforcement/dossiers/<component>.yaml.
This module defines the schema and loader; authoring is a separate design pass.
"""
from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Optional

try:
    import yaml
except ImportError:
    yaml = None  # type: ignore


@dataclass
class Leaf:
    leaf_id: str
    responsibility: str                 # one-sentence single responsibility
    signature: str                      # exact Rust signature the model must fill
    invariants: list[str]               # machine-checkable properties (become tests)
    reference_pattern: str              # known-correct analogous snippet to imitate
    property_test: str                  # Rust test source that must pass
    max_lines: int = 40
    depends_on: list[str] = field(default_factory=list)
    temperature_band: str = "low"       # 'low' mechanical | 'high' needs-a-spark


@dataclass
class Dossier:
    component: str                      # e.g. 'reducer','shred','lockfree','fixedpoint'
    constitution_refs: list[str]
    spec: str                           # formal behavioral contract
    invariants: list[str]               # component-level invariants
    adversarial_checks: list[str]       # known failure modes -> tests
    benchmark: Optional[dict] = None    # {'bench_name':..., 'budgets_ns': {...}}
    leaves: list[Leaf] = field(default_factory=list)
    safety: list[dict] = field(default_factory=list)
    # Each safety entry is a dict with keys:
    #   block: {file, function, marker}  — LOCATABLE, not prose
    #   compiler_cannot_verify: str
    #   callee_preconditions: str
    #   safe_code_establishes: str
    #   failure_modes: str          — memory-safety failure modes only
    #   system_safety: str           — NON-memory hazards (starvation, quota, etc.)
    #   invalidation: str            — what future change would invalidate the argument

    def leaf_order(self) -> list[Leaf]:
        """Topological order over depends_on so leaves build in dependency sequence."""
        by_id = {l.leaf_id: l for l in self.leaves}
        seen: set[str] = set()
        order: list[Leaf] = []

        def visit(l: Leaf, stack: set[str]) -> None:
            if l.leaf_id in seen:
                return
            if l.leaf_id in stack:
                raise ValueError(f"cyclic leaf dependency at {l.leaf_id}")
            stack.add(l.leaf_id)
            for dep in l.depends_on:
                if dep in by_id:
                    visit(by_id[dep], stack)
            stack.discard(l.leaf_id)
            seen.add(l.leaf_id)
            order.append(l)

        for l in self.leaves:
            visit(l, set())
        return order


def load_dossier(path: str | Path) -> Dossier:
    if yaml is None:
        raise ImportError("pyyaml required to load dossiers: pip install pyyaml")
    data = yaml.safe_load(Path(path).read_text(encoding="utf-8"))
    leaves = [Leaf(**l) for l in data.get("leaves", [])]
    return Dossier(
        component=data["component"],
        constitution_refs=data.get("constitution_refs", []),
        spec=data["spec"],
        invariants=data.get("invariants", []),
        adversarial_checks=data.get("adversarial_checks", []),
        benchmark=data.get("benchmark"),
        leaves=leaves,
        safety=data.get("safety", []),
    )


# The SEED HARD component list (the components the constitution names in
# HERMES_HARD_TASK_REINFORCEMENT.md §1). This is a FLOOR, not the whole set:
# any *.yaml present in the dossiers directory is also a hard component. Use
# hard_components() to get the live set — never read this constant directly.
_SEED_HARD_COMPONENTS = [
    "reducer", "replay", "shred", "lockfree", "scalp_position",
    "fixedpoint", "exit_ladder", "evaluator_stats", "cpu_numa_tuning",
    "economic_gate",
]


def hard_components(constitution_path: str | None = None) -> list[str]:
    """Live HARD-component set = seed list UNION dossiers on disk UNION components the
    CONSTITUTION declares hard.

    Registration is fully automatic and requires neither a code edit nor a file drop:
    naming a component as hard in the committed constitution is sufficient. Discovery
    can only ADD (a parsing miss can never silently downgrade a component out of
    reinforcement + gating). Anything in the set without a dossier surfaces via
    missing_dossiers(), fails the milestone gate, and emits an authoring brief.
    """
    present = set(available_dossiers().keys())
    declared: set[str] = set()
    if constitution_path:
        try:
            from .discovery import discover_from_constitution
            declared = discover_from_constitution(constitution_path)
        except Exception:  # noqa: BLE001  (discovery must never break the build)
            declared = set()
    return sorted(set(_SEED_HARD_COMPONENTS) | present | declared)


def missing_dossiers(constitution_path: str | None = None) -> list[str]:
    """Hard components with no dossier yet — these escalate, never silently skip."""
    return sorted(set(hard_components(constitution_path)) - set(available_dossiers().keys()))


# Backwards-compatible alias: existing imports of HARD_COMPONENTS keep working,
# but now reflect the live filesystem-derived set at access time.
class _HardList:
    def __iter__(self):
        return iter(hard_components())

    def __contains__(self, x):
        return x in hard_components()

    def __len__(self):
        return len(hard_components())

    def __getitem__(self, i):
        return hard_components()[i]

    def __repr__(self):
        return repr(hard_components())


HARD_COMPONENTS = _HardList()


def dossier_dir() -> Path:
    return Path(__file__).parent / "dossiers"


def available_dossiers() -> dict[str, Path]:
    d = dossier_dir()
    if not d.exists():
        return {}
    return {p.stem: p for p in d.glob("*.yaml")}
