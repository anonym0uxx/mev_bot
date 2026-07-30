"""
Hard-task reinforcement engine.

Turns a HARD component (via its dossier) into clean, verified Rust through:
  micro-decompose -> per-leaf PRIME -> best-of-N GENERATE -> hard FILTER -> SELECT -> INTEGRATE.

The engine never trusts the model. Every candidate leaf body is compiled and run against
its property test in isolation; only survivors are eligible; the simplest correct survivor wins.
Adaptive difficulty (leaf size, N) is read from and written to the capability map so the loop
self-tunes to GLM-5.2's measured strengths per component.
"""
from __future__ import annotations

import os
import re
import threading
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Optional

from ..core.model_client import ModelClient, SchemaViolation
from ..core.schemas import get_schema
from ..store.evidence import EvidenceStore
from ..core.memory import BoundedList, MemoryGuard, under_pressure
from .dossier import Dossier, Leaf


@dataclass
class LeafOutcome:
    leaf_id: str
    body: Optional[str]
    passed: bool
    attempts: int
    reason: str = ""


# A callable the engine uses to verify a candidate leaf body in isolation:
#   verify(leaf, body) -> (passed: bool, detail: str)
# Provided by the orchestrator (wraps: write to a scratch crate, cargo test the property test,
# scan for float/lock/alloc as applicable). Kept injectable so the engine stays testable.
LeafVerifier = Callable[[Leaf, str], tuple[bool, str]]


LEAF_SYSTEM = (
    "You are implementing ONE small, single-responsibility Rust function body inside a heavily "
    "scaffolded, test-guarded skeleton. You are NOT designing the architecture — the signature, "
    "the invariants, and the property test are FIXED and given. Fill in ONLY the body so it "
    "satisfies every invariant and passes the property test. Imitate the reference pattern. "
    "No unwrap in hot paths, no float in outcome-controlling logic unless the spec allows it, "
    "no wall-clock, no allocation on hot paths unless the spec allows it. Return only the schema object."
)


def _leaf_user_prompt(dossier: Dossier, leaf: Leaf, prior_bodies: dict[str, str]) -> str:
    deps = "\n\n".join(
        f"// dependency already implemented and verified: {d}\n{prior_bodies.get(d, '// (body omitted)')}"
        for d in leaf.depends_on
    )
    return (
        f"COMPONENT: {dossier.component}\n"
        f"CONSTITUTION REFS: {', '.join(dossier.constitution_refs)}\n\n"
        f"COMPONENT SPEC:\n{dossier.spec}\n\n"
        f"LEAF: {leaf.leaf_id}\nRESPONSIBILITY: {leaf.responsibility}\n\n"
        f"EXACT SIGNATURE (fill the body, do not change the signature):\n{leaf.signature}\n\n"
        f"INVARIANTS (must all hold):\n" + "\n".join(f"- {i}" for i in leaf.invariants) + "\n\n"
        f"REFERENCE PATTERN (imitate this shape):\n{leaf.reference_pattern}\n\n"
        f"PROPERTY TEST THAT WILL JUDGE YOU (do not modify it; make it pass):\n{leaf.property_test}\n\n"
        f"{('ALREADY-VERIFIED DEPENDENCIES:' + chr(10) + deps) if deps else ''}\n"
        f"Return a JSON object: {{\"leaf_id\": \"{leaf.leaf_id}\", \"body\": \"<rust body only>\", "
        f"\"notes\": \"<short>\"}}"
    )


class ReinforcementEngine:
    def __init__(self, model: ModelClient, store: EvidenceStore, verify: LeafVerifier,
                 max_concurrency: Optional[int] = None):
        self.model = model
        self.store = store
        self.verify = verify
        # Candidate GENERATION runs concurrently; llama-server serves --parallel N slots and
        # decode on a 2-bit MoE is memory-bandwidth-bound, so batching reads the weights once
        # and produces many tokens. MEASURED on the deploy box 2026-07-30:
        #   1 stream 40.4 tok/s | 2 -> 67.5 | 4 -> 94.1 | 8 -> 123.2  (3.05x, still climbing)
        # Set this to the server's --parallel value. Exceeding it is harmless (llama-server
        # queues) but buys nothing. Below it, slots sit idle.
        self.max_concurrency = int(
            max_concurrency if max_concurrency is not None
            else os.environ.get("PQ_LEAF_CONCURRENCY", "8")
        )
        self._tls = threading.local()

    def _client(self) -> ModelClient:
        """One ModelClient per worker thread.

        ModelClient holds a requests.Session, which is not documented thread-safe; sharing one
        across concurrent generations risks connection-pool corruption that would surface as
        sporadic, unreproducible request failures — the worst possible failure mode in a loop
        whose whole job is telling capability apart from infrastructure.
        """
        c = getattr(self._tls, "client", None)
        if c is None:
            c = ModelClient(self.model.cfg)
            self._tls.client = c
        return c

    # ---------------------------------------------------- adaptive difficulty
    @staticmethod
    def _band_temperature(leaf) -> float:
        """Map the dossier's declared `temperature_band` onto a real sampling temperature.

        Every dossier leaf carries a temperature_band and NOTHING read it — candidates were
        generated at cfg.control_temperature (0.1), which is effectively greedy. Measured on
        this box: at 0.1, distinct seeds return byte-identical bodies, so best-of-N collapsed
        onto a single candidate and re-sampling could never escape a wrong answer. Even the
        "low" band must stay clear of that floor to sample at all.
        """
        return {"low": 0.5, "medium": 0.7, "high": 0.9}.get(
            str(getattr(leaf, "temperature_band", "low")).lower(), 0.7
        )

    def _pick_n(self, component: str, default_n: int = 8) -> int:
        rates = self.store.capability_rate(component)
        if not rates:
            return default_n
        # if best observed success rate is low, widen N (up to 12); if high, narrow (down to 3)
        best = max(rates.values())
        if best < 0.3:
            return min(16, default_n + 4)
        if best > 0.8:
            return max(3, default_n - 3)
        return default_n

    # --------------------------------------------------------------- one leaf
    def implement_leaf(self, dossier: Dossier, leaf: Leaf,
                       prior_bodies: dict[str, str], max_retries: int = 3) -> LeafOutcome:
        n = self._pick_n(dossier.component)
        attempts = 0
        prompt = _leaf_user_prompt(dossier, leaf, prior_bodies)
        distinct = 0
        for _ in range(max_retries):
            base = attempts
            attempts += n

            def _generate(k: int) -> Optional[str]:
                # Safety-second: if memory is under pressure, decline to add more in-flight work
                # and let the filter run on whatever came back (durable data already journaled).
                if under_pressure():
                    return None
                try:
                    # Three changes, two of them measured on this box 2026-07-30.
                    #
                    # temperature: THE diversity lever. Candidates were previously generated at
                    #   cfg.control_temperature (0.1). Measured: at 0.1 distinct seeds return
                    #   BYTE-IDENTICAL bodies, so the N candidates collapsed onto one and the
                    #   hard filter judged the same body N times. Every dossier leaf declares a
                    #   temperature_band and nothing read it; _band_temperature now does.
                    #
                    # max_tokens 2048 -> 8192: MEASURED finish_reason="length" at exactly 2048
                    #   with reasoning_content present. GLM-5.2 spends reasoning tokens from the
                    #   same budget, so the body truncated mid-function. GBNF still emits
                    #   syntactically valid JSON, so this did NOT raise — it produced a
                    #   well-formed object containing unusable code that failed to compile,
                    #   which is indistinguishable from the model being incapable.
                    #
                    # seed: distinct per candidate. NOT the fix — the same seed was already
                    #   observed to diverge (llama.cpp is not bit-reproducible under continuous
                    #   batching + cache reuse). Kept because a deterministic retry is a no-op.
                    obj = self._client().constrained(
                        LEAF_SYSTEM,
                        prompt,
                        get_schema("leaf"),
                        max_tokens=8192,
                        seed=1000 + base + k + 1,
                        temperature=self._band_temperature(leaf),
                    )
                except SchemaViolation:
                    return None
                except Exception:  # noqa: BLE001 - one bad candidate must not kill the batch
                    return None
                return (obj.get("body") or "").strip() or None

            # GENERATE concurrently. The prompt is identical for every candidate, so the server
            # prefills it once and the rest hit the prompt cache; the cost here is pure decode,
            # which is exactly what batching across slots accelerates.
            workers = max(1, min(self.max_concurrency, n))
            with ThreadPoolExecutor(max_workers=workers) as ex:
                raw = list(ex.map(_generate, range(n)))

            # DEDUPLICATE before verifying. Candidates collapse onto identical text more often
            # than raw inequality suggests (whitespace and identifier churn make near-duplicates
            # look distinct), and verification is the expensive serial step — compiling the same
            # body eight times is pure waste. The distinct count is also the honest diagnostic
            # for whether sampling is actually exploring.
            candidates: BoundedList = BoundedList(cap=n)
            seen: set[str] = set()
            for body in raw:
                if not body:
                    continue
                key = " ".join(body.split())
                if key in seen:
                    continue
                seen.add(key)
                candidates.append(body)
            distinct = len(seen)

            # HARD FILTER — deliberately SERIAL. The scratch verifier writes into a single
            # <repo>/.supervisor_scratch crate and runs cargo there; concurrent verification
            # would race on that directory and produce results belonging to another candidate.
            survivors: list[tuple[str, str]] = []  # (body, detail)
            for body in candidates:
                ok, detail = self.verify(leaf, body)
                self.store.record_capability(dossier.component, leaf.max_lines, n, ok)
                if ok:
                    survivors.append((body, detail))

            if survivors:
                best = self._select(survivors)
                return LeafOutcome(leaf.leaf_id, best, True, attempts, "verified")

            # no survivors -> tighten (smaller leaf implied by dossier author, or wider N) and retry
            n = min(16, n + 3)

        # Report the DISTINCT count, not just the attempt count. "8 attempts, 1 distinct" is a
        # sampling-collapse diagnosis; "8 attempts, 8 distinct" is a genuine capability limit.
        # Without this the two are indistinguishable and the escalation misattributes the cause.
        return LeafOutcome(
            leaf.leaf_id, None, False, attempts,
            f"no candidate passed property test at max granularity/N "
            f"({distinct} distinct of {attempts} generated, concurrency={self.max_concurrency})",
        )

    # ------------------------------------------------------- full component
    def implement_component(self, dossier: Dossier) -> tuple[bool, dict[str, str], list[LeafOutcome]]:
        """
        Build all leaves in dependency order. Returns (all_passed, {leaf_id: body}, outcomes).
        A failing leaf stops the component (caller escalates that scoped leaf).
        """
        bodies: dict[str, str] = {}
        outcomes: list[LeafOutcome] = []
        for leaf in dossier.leaf_order():
            outcome = self.implement_leaf(dossier, leaf, bodies)
            outcomes.append(outcome)
            if not outcome.passed or outcome.body is None:
                return False, bodies, outcomes
            bodies[leaf.leaf_id] = outcome.body
        return True, bodies, outcomes

    # --------------------------------------------------------------- select
    @staticmethod
    def _select(survivors: list[tuple[str, str]]) -> str:
        """Among correct survivors choose the simplest (fewest lines, then fewest branches)."""
        def complexity(body: str) -> tuple[int, int]:
            lines = len([l for l in body.splitlines() if l.strip()])
            branches = len(re.findall(r"\b(if|match|while|for|else)\b", body))
            return (lines, branches)
        return min(survivors, key=lambda s: complexity(s[0]))[0]
