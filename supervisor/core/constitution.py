"""
Constitution loader — parses the committed build constitution
(docs/HERMES_ONE_SHOT_PROMPT.md) into a machine-checkable structure:

  * milestones M0..M9 with their descriptions
  * acceptance criteria 1..N
  * a (best-effort) mapping of criteria -> milestones for the criteria->evidence gate

The constitution is prose, so parsing is heuristic and conservative: anything it cannot
confidently map is surfaced for human confirmation rather than guessed. The git hash of the
file is captured so every decision references the exact constitution in force.
"""
from __future__ import annotations

import hashlib
import re
import subprocess
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional


@dataclass
class Milestone:
    key: str                    # 'M0'..'M9'
    title: str
    body: str
    scoped_criteria: list[str] = field(default_factory=list)


@dataclass
class Constitution:
    path: str
    git_hash: str
    content_hash: str
    milestones: dict[str, Milestone]
    criteria: dict[str, str]    # number -> text

    def milestone_order(self) -> list[str]:
        return sorted(self.milestones, key=lambda k: int(k[1:]))


_MILESTONE_RE = re.compile(r"\*\*(M\d)\s*[—-]\s*([^:*]+?):?\*\*", re.M)
# criteria appear as "N. text" inside §63; capture number + first sentence
_CRITERION_RE = re.compile(r"(?:^|\s)(\d{1,3})\.\s+([A-Z][^.]*\.)")


def _git_hash(path: Path) -> str:
    try:
        r = subprocess.run(["git", "log", "-1", "--format=%H", "--", path.name],
                           cwd=path.parent, capture_output=True, text=True, timeout=15)
        return r.stdout.strip() or "uncommitted"
    except Exception:
        return "unknown"


def load_constitution(path: str | Path) -> Constitution:
    p = Path(path)
    text = p.read_text(encoding="utf-8")
    content_hash = hashlib.sha256(text.encode("utf-8")).hexdigest()[:16]

    # milestones
    milestones: dict[str, Milestone] = {}
    matches = list(_MILESTONE_RE.finditer(text))
    for i, m in enumerate(matches):
        key = m.group(1)
        title = m.group(2).strip()
        start = m.end()
        end = matches[i + 1].start() if i + 1 < len(matches) else min(len(text), start + 4000)
        body = text[start:end].strip()
        # keep the first (milestone-contract) definition if duplicated later
        if key not in milestones:
            milestones[key] = Milestone(key, title, body)

    # criteria (scope to the acceptance-criteria section if findable)
    crit_section = text
    sec = re.search(r"63\.\s*ACCEPTANCE CRITERIA.*?\n(.*?)\n=+\n64\.", text, re.S)
    if sec:
        crit_section = sec.group(1)
    criteria: dict[str, str] = {}
    for m in _CRITERION_RE.finditer(crit_section):
        num, body = m.group(1), m.group(2).strip()
        if 1 <= int(num) <= 200:
            criteria.setdefault(num, body)

    # heuristic criteria->milestone scoping by keyword overlap (conservative)
    _scope_criteria(milestones, criteria)

    return Constitution(str(p), _git_hash(p), content_hash, milestones, criteria)


_KEYWORDS = {
    "M0": ["quarantine", "config", "custody", "manifest", "seed", "entitlement"],
    "M1": ["capture", "laserstream", "journal", "canonical", "decoder", "shred", "narrative"],
    "M2": ["reducer", "deterministic", "replay", "byte-equivalent", "candidate lifecycle"],
    "M3": ["feature", "wallet graph", "market state", "regime", "holdout"],
    "M4": ["execution", "signing", "reconcil", "template", "guard"],
    "M5": ["simulator", "evaluator", "baseline", "markout", "capacity"],
    "M6": ["calibration", "landing", "slippage", "mode-c", "mode c"],
    "M7": ["governance", "registry", "counterfactual", "retirement", "fdr", "pbo"],
    "M8": ["entrymode", "entry mode", "arena", "hazard", "scalp", "experiment"],
    "M9": ["shadow", "probe", "scale", "reconcil"],
}


def _scope_criteria(milestones: dict[str, Milestone], criteria: dict[str, str]) -> None:
    for num, text in criteria.items():
        low = text.lower()
        best: Optional[str] = None
        best_score = 0
        for mk, kws in _KEYWORDS.items():
            if mk not in milestones:
                continue
            score = sum(1 for kw in kws if kw in low)
            if score > best_score:
                best, best_score = mk, score
        # only assign if there is a clear keyword hit; otherwise leave unscoped (human confirms)
        if best and best_score > 0:
            milestones[best].scoped_criteria.append(num)
