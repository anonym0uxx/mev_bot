"""
Tier-0 safety: the supervisor may never let the loop cross these lines autonomously.
Derived from the constitution's Tier-0 hierarchy (§5), key custody (§41),
frozen evaluator (§44), and authority/promotion path (§64).

These are enforced structurally: any task, diff, or research action that matches a
tripwire halts the loop and escalates to a human, regardless of model confidence.
"""
from __future__ import annotations

import re
from dataclasses import dataclass
from enum import Enum
from typing import Iterable


class Tier0Domain(str, Enum):
    KEY_CUSTODY = "key_custody"
    LIVE_CAPITAL = "live_capital"
    EVALUATOR_RELEASE = "evaluator_release"
    FUND_MOVEMENT = "fund_movement"
    PROMOTION_TO_LIVE = "promotion_to_live"


@dataclass
class Tripwire:
    domain: Tier0Domain
    pattern: re.Pattern
    reason: str


# Patterns that must never appear in an autonomously-applied diff or be executed
# by the loop without a human gate. Intentionally broad; false positives escalate
# (safe) rather than pass (unsafe).
_TRIPWIRES: list[Tripwire] = [
    Tripwire(Tier0Domain.KEY_CUSTODY,
             re.compile(r"(private[_\s-]?key|secret[_\s-]?key|keypair|seed[_\s-]?phrase|"
                        r"WALLET_PRIVATE_KEY|signer\.sign|sign_transaction)\s*=", re.I),
             "assignment/derivation touching raw key material"),
    Tripwire(Tier0Domain.KEY_CUSTODY,
             re.compile(r"(read|load|decrypt|export)[_\s-]?(private|secret)[_\s-]?key", re.I),
             "reading/exporting key material"),
    Tripwire(Tier0Domain.FUND_MOVEMENT,
             re.compile(r"(transfer|withdraw|send)[_\s-]?(sol|funds|lamports)\b", re.I),
             "fund movement primitive"),
    Tripwire(Tier0Domain.LIVE_CAPITAL,
             re.compile(r"paper_mode\s*[:=]\s*false", re.I),
             "arming live trading in config"),
    Tripwire(Tier0Domain.LIVE_CAPITAL,
             re.compile(r"(enable|arm|go)[_\s-]?live\b", re.I),
             "enabling live trading"),
    Tripwire(Tier0Domain.EVALUATOR_RELEASE,
             re.compile(r"(pq-?evaluator|frozen[_\s-]?evaluator).*(release|sign|publish|hash\s*=)", re.I),
             "evaluator release/modification"),
    Tripwire(Tier0Domain.PROMOTION_TO_LIVE,
             re.compile(r"promotion_status\s*[:=]\s*[\"']?(CHAMPION|LIVE)", re.I),
             "promoting a strategy to live/champion"),
]


@dataclass
class Tier0Hit:
    domain: Tier0Domain
    reason: str
    excerpt: str


def scan_text(text: str) -> list[Tier0Hit]:
    """Return Tier-0 tripwire hits in an arbitrary text blob (diff, config, command)."""
    hits: list[Tier0Hit] = []
    for tw in _TRIPWIRES:
        m = tw.pattern.search(text)
        if m:
            start = max(0, m.start() - 40)
            end = min(len(text), m.end() + 40)
            hits.append(Tier0Hit(tw.domain, tw.reason, text[start:end].replace("\n", " ")))
    return hits


def scan_paths(paths: Iterable[str]) -> list[Tier0Hit]:
    """Flag edits to paths that hold Tier-0 authority (key stores, evaluator, live config)."""
    hits: list[Tier0Hit] = []
    sensitive = [
        (re.compile(r"(key|secret|wallet|signer).*\.(rs|toml|json|env)$", re.I), Tier0Domain.KEY_CUSTODY),
        (re.compile(r"pq-?evaluator", re.I), Tier0Domain.EVALUATOR_RELEASE),
        (re.compile(r"canary\.json$|live.*config", re.I), Tier0Domain.LIVE_CAPITAL),
    ]
    for p in paths:
        for rx, dom in sensitive:
            if rx.search(p):
                hits.append(Tier0Hit(dom, f"edit to Tier-0 path {p}", p))
    return hits


def is_blocked(diff_text: str, paths: Iterable[str]) -> list[Tier0Hit]:
    """Primary entry: returns all Tier-0 hits; non-empty => loop must halt and escalate."""
    return scan_text(diff_text) + scan_paths(paths)
