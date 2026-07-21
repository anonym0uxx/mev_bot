"""
Constitution amendments — how the constitution becomes a LIVING document without
letting the builder rewrite its own laws.

Design review decisions encoded here (each one is load-bearing; read before editing):

  1. SEPARATION OF POWERS, ENFORCED BY CAPABILITY NOT POLICY.
       propose  -> builder model (MCP tool)         : may only ADD to a queue
       draft    -> independent design model (MCP)   : turns a proposal into diff text
       approve  -> HUMAN ONLY (CLI; NOT an MCP tool): the verb does not exist in the
                   model's tool surface, so no prompt injection or reasoning slip can
                   reach it. This is the single most important property of the module.
       apply    -> supervisor (CLI, post-approval)  : validated + atomic + reversible
  2. EVIDENCE OR NOTHING. Intake rejects a proposal whose evidence_ref does not resolve
     to a real record in the evidence store (gate result, experiment, artifact, benchmark).
     "The model reasoned X" is not evidence — the constitution's own law.
  3. TIER-0 IS FROZEN. Sections governing key custody, evaluator integrity, wallet floor,
     and promotion-gate integrity cannot be amended by this path at all. Enforced by
     byte-comparison at apply time, not by reviewer attention.
  4. CRITERIA MAY NOT DECREASE. An amendment can add acceptance criteria; it can never
     reduce the count or delete one. Weakening the gates requires a human editing the
     file directly, deliberately, outside this system.
  5. MILESTONE-BOUNDARY APPLICATION. Amendments never land mid-task: the builder must not
     be graded against a spec that changed under it. Apply re-pins the constitution hash.
  6. FAIL CLOSED. Any ambiguity — unparseable diff, failed structural check, missing
     backup target — results in NO WRITE.
"""
from __future__ import annotations

import hashlib
import re
import shutil
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Optional

# Sections that this path may never touch. Matched case-insensitively against the
# amendment's declared target and verified byte-identical after any apply.
TIER0_FROZEN_MARKERS = [
    "key custody",
    "evaluator integrity",
    "wallet floor",
    "promotion-gate integrity",
    "promotion gate integrity",
    "tier-0",
    "tier 0",
]

# Proposal lifecycle
STATES = ("proposed", "drafted", "approved", "applied", "rejected")


@dataclass
class Amendment:
    id: int
    kind: str                 # 'new_component' | 'law' | 'strategy' | 'correction'
    title: str
    rationale: str
    evidence_ref: str         # must resolve in the evidence store
    proposed_by: str          # 'builder' | 'human' | 'design'
    target_hint: str = ""     # section the proposer believes it belongs in
    diff_text: str = ""       # authored by the design model at draft time
    state: str = "proposed"
    created_at: float = 0.0
    decided_at: float = 0.0
    decided_by: str = ""
    note: str = ""


# --------------------------------------------------------------------- validation
def touches_tier0(text: str) -> Optional[str]:
    """Return the first Tier-0 marker the text appears to target, or None."""
    low = (text or "").lower()
    for m in TIER0_FROZEN_MARKERS:
        if m in low:
            return m
    return None


_SECTION_LINE = re.compile(r"^\s*(\d{1,2})\.\s+[A-Z][A-Z0-9 \-/&,'\.]{4,}\s*$", re.MULTILINE)


def criteria_numbers(constitution_text: str) -> set[int]:
    """Extract acceptance-criteria numbers from the §63 region.

    The region is bounded STRUCTURALLY — from the ACCEPTANCE CRITERIA header to the next
    top-level section header — rather than by searching for a literal successor title.
    An earlier version keyed off the string 'AUTHORITY'; deleting that header made the
    region run to end-of-file and swallow section numbers as criteria, producing a correct
    refusal with a misleading reason. Structure, not string literals.
    """
    idx = constitution_text.find("ACCEPTANCE CRITERIA")
    if idx == -1:
        region = constitution_text
    else:
        rest = constitution_text[idx + len("ACCEPTANCE CRITERIA"):]
        m = _SECTION_LINE.search(rest)
        region = rest[:m.start()] if m else rest
    return {int(n) for n in re.findall(r"(?:^|[ \n])(\d{1,3})\.\s", region)
            if 1 <= int(n) <= 999}


def section_headers(constitution_text: str) -> list[str]:
    """Numbered top-level section headers, used to detect structural damage.

    Titles may contain digits and punctuation (e.g. '5. TIER-0 RULES', '18.3 SOURCES'),
    so the title class must admit them — an earlier letters-only pattern silently failed
    to see exactly the Tier-0 section this module exists to protect.
    """
    return re.findall(r"^\s*(\d{1,2})\.\s+[A-Z][A-Z0-9 \-/&,'\.]{4,}\s*$",
                      constitution_text, re.MULTILINE)


@dataclass
class ApplyReport:
    ok: bool
    reason: str = ""
    checks: dict[str, Any] = field(default_factory=dict)
    backup_path: str = ""
    new_hash: str = ""


def validate_candidate(original: str, candidate: str) -> ApplyReport:
    """Structural gate between the current constitution and a proposed replacement.

    Every check fails closed. None of these are style opinions — each corresponds to a
    way a text patch can silently destroy the document or weaken the system.
    """
    checks: dict[str, Any] = {}

    # Check ordering is deliberate: the most security-relevant invariants are evaluated
    # FIRST so a refusal names the real reason. (An earlier ordering let the generic size
    # check mask a Tier-0 tamper — defense-in-depth still refused, but the diagnostic lied.)

    # 1) Tier-0 paragraphs byte-identical — the hardest freeze in the system
    def tier0_lines(t: str) -> list[str]:
        return [ln.strip() for ln in t.splitlines() if touches_tier0(ln)]

    o_t0, c_t0 = tier0_lines(original), tier0_lines(candidate)
    checks["tier0_lines_before"] = len(o_t0)
    checks["tier0_lines_after"] = len(c_t0)
    changed = [ln for ln in o_t0 if ln not in c_t0]
    if changed:
        return ApplyReport(False, "amendment alters or removes Tier-0 text "
                                  f"({len(changed)} line(s)); Tier-0 is frozen to this path",
                           checks)

    # 2) acceptance criteria may never decrease or lose a number
    o_nums, c_nums = criteria_numbers(original), criteria_numbers(candidate)
    checks["criteria_before"] = len(o_nums)
    checks["criteria_after"] = len(c_nums)
    missing = sorted(o_nums - c_nums)
    checks["criteria_removed"] = missing
    if missing:
        return ApplyReport(False, f"amendment removes acceptance criteria {missing} — "
                                  "weakening the gates is not permitted by this path", checks)

    # 3) section structure preserved (headers may be added, never dropped)
    o_secs, c_secs = section_headers(original), section_headers(candidate)
    dropped = sorted(set(o_secs) - set(c_secs), key=lambda s: int(s))
    checks["sections_before"] = len(o_secs)
    checks["sections_after"] = len(c_secs)
    checks["sections_dropped"] = dropped
    if dropped:
        return ApplyReport(False, f"amendment drops section header(s) {dropped}", checks)

    # 4) non-trivial and not truncated: a patch that loses most of the file is a bug
    checks["size_ratio"] = round(len(candidate) / max(len(original), 1), 4)
    if len(candidate) < len(original) * 0.98:
        return ApplyReport(False, "candidate is materially shorter than the current "
                                  "constitution (possible truncation or deletion)", checks)

    return ApplyReport(True, "structural validation passed", checks)


def apply_amendment(constitution_path: str | Path, candidate_text: str,
                    dry_run: bool = False) -> ApplyReport:
    """Validated, atomic, reversible replacement of the constitution file.

    Never git-commits: the human commits, so the amendment is reviewable in a diff and
    the constitution hash is re-pinned by a deliberate act.
    """
    p = Path(constitution_path)
    if not p.is_file():
        return ApplyReport(False, f"constitution not found at {p}")
    original = p.read_text(encoding="utf-8", errors="replace")

    rep = validate_candidate(original, candidate_text)
    if not rep.ok or dry_run:
        rep.new_hash = hashlib.sha256(candidate_text.encode("utf-8")).hexdigest()
        if dry_run and rep.ok:
            rep.reason = "dry run: validation passed, nothing written"
        return rep

    stamp = time.strftime("%Y%m%d-%H%M%S")
    backup = p.with_suffix(p.suffix + f".{stamp}.bak")
    tmp = p.with_suffix(p.suffix + ".candidate")
    try:
        shutil.copy2(p, backup)
        tmp.write_text(candidate_text, encoding="utf-8")
        tmp.replace(p)                      # atomic on the same filesystem
    except OSError as e:
        try:
            tmp.unlink(missing_ok=True)
        except OSError:
            pass
        return ApplyReport(False, f"write failed, constitution untouched: {e}", rep.checks)

    rep.backup_path = str(backup)
    rep.new_hash = hashlib.sha256(candidate_text.encode("utf-8")).hexdigest()
    rep.reason = ("applied; commit the change so the constitution hash re-pins "
                  "(the supervisor records the hash per run)")
    return rep
