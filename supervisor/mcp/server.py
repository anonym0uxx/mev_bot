"""
Hermes-Supervisor MCP server.

Exposes the supervisor's verification/reinforcement capabilities to Hermes Agent
(Nous Research) as MCP tools over stdio JSON-RPC 2.0 (newline-delimited messages),
per the Model Context Protocol. Dependency-free protocol implementation so it runs
anywhere Python runs.

Register with Hermes via its MCP config (see hermes_mcp_config.example.json):
    command: python
    args:    ["-m", "supervisor.mcp.server", "--config", "<path>/supervisor.yaml"]

Tools exposed (16 registered):
  gate_verify        -> run the independent milestone/task gate battery (build/clippy/fmt/
                        no-stubs/tests[/secrets/bench/determinism]); returns pass/fail + reasons.
                        THIS is how a milestone gets certified — never by model self-report.
  check_tier0        -> scan a diff/paths for Tier-0 tripwires (keys, live capital, evaluator,
                        funds). Non-empty hits = STOP and escalate to the human.
  run_reinforcement  -> grind a HARD component leaf-by-leaf via best-of-N against its dossier
                        (the supervisor samples GLM directly for candidates).
  author_dossier     -> [design-model] author a missing HARD-component dossier via the
                        independent design model; validated by the real loader before install.
  propose_amendment  -> queue a constitution change proposal (intake only; evidence_ref required;
                        Tier-0 material refused).
  draft_amendment    -> [design-model] draft queued proposal text; human approves via CLI.
  amendment_status   -> list queued/drafted/approved/applied constitution amendments.
  record_infra_fact  -> append a provenance-stamped infrastructure fact to the manifest ledger.
  evidence_status    -> milestone/criteria/escalation dashboard from the evidence store.
  record_escalation  -> journal a human-needed escalation (Tier-0, stuck leaf, contradiction).
  register_artifact  -> self-bind a build-produced artifact (evaluator/research_runner/
                        live_status); evaluator hash is TOFU-pinned, never silently re-pinned.
  evaluator_verify   -> verify the frozen evaluator's hash against the pin (§44).
  experiment_run     -> invoke the sealed-experiment runner for a registered experiment (§56).
  promotion_check    -> report promotion preconditions; live scope always requires the human gate.
  live_status        -> read the running bot's exported status/metrics.
  bench_endpoint     -> measure real tokens/sec of the llama.cpp endpoint.

The server never advances anything itself; it returns verified facts. Hermes remains the
conductor; these tools are its truth layer.
"""
from __future__ import annotations

import argparse
import io
import json
import os  # repair: was missing — _dossier_authoring_brief used os.path.isfile and silently swallowed the NameError, blanking constitution_context
import sys
import time
import traceback
import uuid
from pathlib import Path
from typing import Any, Callable

# package imports (run as: python -m supervisor.mcp.server)
from ..core.config import SupervisorConfig
from ..core import safety
from ..gates.runner import GateRunner, GateConfig
from ..store.evidence import EvidenceStore

PROTOCOL_VERSION = "2024-11-05"
SERVER_INFO = {"name": "hermes-supervisor", "version": "0.2.0"}


# --------------------------------------------------------------------------- tools
def _tool_schemas() -> list[dict]:
    return [
        {
            "name": "gate_verify",
            "description": (
                "Independently verify a milestone or task with the deterministic gate battery "
                "(cargo build, clippy -D warnings, fmt, no-stubs scan, tests; milestone mode adds "
                "secrets scan and optional bench/determinism). Returns pass/fail with reasons. "
                "A milestone may ONLY be declared complete if this returns passed=true. "
                "Never self-certify completion."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "scope": {"type": "string", "enum": ["task", "milestone"]},
                    "id": {"type": "string", "description": "task id or milestone key e.g. M2"},
                    "scoped_criteria": {"type": "array", "items": {"type": "string"},
                                         "description": "criteria numbers this milestone must satisfy"},
                },
                "required": ["scope", "id"],
            },
        },
        {
            "name": "check_tier0",
            "description": (
                "Scan a proposed diff and touched paths for Tier-0 tripwires: key material, "
                "fund movement, arming live trading, evaluator modification, live promotion. "
                "If any hits are returned you MUST stop and escalate to the human operator; "
                "never apply the change."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "diff": {"type": "string"},
                    "paths": {"type": "array", "items": {"type": "string"}},
                },
                "required": ["diff"],
            },
        },
        {
            "name": "record_infra_fact",
            "description": (
                "Append a provenance-stamped infrastructure fact (verified provider plan, "
                "endpoint status, CPU feature, source lifecycle note) to the manifest's facts "
                "ledger. Requires key, value, AND source. This lane is agent-writable by design "
                "and can never affect Phase-B certification: deployment identity is measured "
                "live and pinned by the operator."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "key": {"type": "string"},
                    "value": {"type": "string"},
                    "source": {"type": "string"},
                },
                "required": ["key", "value", "source"],
            },
        },
        {
            "name": "propose_amendment",
            "description": (
                "Queue a proposed change to the constitution (new component, law, strategy, "
                "correction). INTAKE ONLY: you cannot draft, approve, or apply. Requires an "
                "evidence_ref that resolves to a real record (gate:<id>, experiment:<id>, "
                "artifact:<name>, benchmark:<comp>/<metric>, criterion:<n>) — a model claim is "
                "not evidence. Tier-0 material cannot be proposed at all."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": {"type": "string",
                             "enum": ["new_component", "law", "strategy", "correction"]},
                    "title": {"type": "string"},
                    "rationale": {"type": "string"},
                    "evidence_ref": {"type": "string"},
                    "target_hint": {"type": "string"},
                },
                "required": ["kind", "title", "rationale", "evidence_ref"],
            },
        },
        {
            "name": "draft_amendment",
            "description": (
                "[design-model] Turn a queued proposal into constitution prose using the "
                "INDEPENDENT design model. Does not apply anything; a human approves via CLI."),
            "inputSchema": {
                "type": "object",
                "properties": {"amendment_id": {"type": "integer"}},
                "required": ["amendment_id"],
            },
        },
        {
            "name": "amendment_status",
            "description": "List queued/drafted/approved/applied constitution amendments.",
            "inputSchema": {
                "type": "object",
                "properties": {"state": {"type": "string"}},
            },
        },
        {
            "name": "author_dossier",
            "description": (
                "[design-model] Author a missing HARD-component dossier by routing the "
                "authoring brief to an INDEPENDENT frontier model (separate endpoint/key from "
                "the builder). Validates the result through the real loader before installing; "
                "an invalid dossier is never written. The builder model must never author its "
                "own property tests — that is the anti-agreeability circularity. Human review "
                "of the authored tests is still required."),
            "inputSchema": {
                "type": "object",
                "properties": {"component": {"type": "string"}},
                "required": ["component"],
            },
        },
        {
            "name": "run_reinforcement",
            "description": (
                "Grind a HARD component through the reinforcement pipeline: micro-decomposed "
                "leaves from its dossier, best-of-N candidate sampling from the local GLM "
                "endpoint, hard filtering against per-leaf property tests, simplest-survivor "
                "selection. Use for components flagged HARD (reducer, shred, lockfree, "
                "fixedpoint, scalp_position, exit_ladder, evaluator_stats, replay, "
                "cpu_numa_tuning). Returns assembled verified bodies or the single stuck leaf."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {"component": {"type": "string"}},
                "required": ["component"],
            },
        },
        {
            "name": "evidence_status",
            "description": "Dashboard: unsatisfied criteria per milestone, open escalations, recent gate results.",
            "inputSchema": {
                "type": "object",
                "properties": {"milestone": {"type": "string"}},
            },
        },
        {
            "name": "record_escalation",
            "description": (
                "Journal a human-needed escalation (Tier-0 hit, retry-exhausted leaf, "
                "constitution/repo contradiction). Returns the escalation id. The human "
                "resolves out-of-band; do not proceed past a Tier-0 escalation."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "milestone": {"type": "string"},
                    "task_id": {"type": "string"},
                    "domain": {"type": "string"},
                    "context": {"type": "string"},
                },
                "required": ["milestone", "task_id", "domain", "context"],
            },
        },
        {
            "name": "register_artifact",
            "description": (
                "Register a build-produced artifact so the system knows what it created and "
                "where (self-binding — the operator never fills paths). Call this the moment a "
                "milestone produces: the evaluator binary (name='evaluator'), the sealed-"
                "experiment runner (name='research_runner'), or when the bot starts exporting "
                "status (name='live_status'). The evaluator's hash is PINNED on first "
                "registration (frozen evaluator, §44); registering a changed evaluator is "
                "refused — that is a human-only re-pin."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": {"type": "string",
                             "enum": ["evaluator", "research_runner", "live_status"]},
                    "path": {"type": "string"},
                    "milestone": {"type": "string"},
                },
                "required": ["name", "path"],
            },
        },
        {
            "name": "evaluator_verify",
            "description": (
                "PRODUCTION: verify the frozen evaluator's binary hash matches the pinned hash "
                "(constitution §44). Run before trusting ANY experiment grade. A mismatch means "
                "the evaluator was modified — Tier-0: stop, escalate, do not trust grades."
            ),
            "inputSchema": {"type": "object", "properties": {}},
        },
        {
            "name": "experiment_run",
            "description": (
                "PRODUCTION: invoke the bot's sealed-experiment runner for a registered "
                "experiment id (§56). The experiment must be sealed before results are seen; "
                "grades come from the frozen evaluator, never from self-assessment."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {"experiment_id": {"type": "string"}},
                "required": ["experiment_id"],
            },
        },
        {
            "name": "promotion_check",
            "description": (
                "PRODUCTION: check promotion preconditions for a strategy/lane from the evidence "
                "store (gates passed, criteria satisfied, no open escalations). NOTE: live-capital "
                "promotion ALWAYS additionally requires the human gate — this tool never "
                "authorizes live capital; it reports readiness and always returns "
                "human_gate_required=true for live scope."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "strategy_id": {"type": "string"},
                    "scope": {"type": "string", "enum": ["shadow", "probe", "live"]},
                },
                "required": ["strategy_id", "scope"],
            },
        },
        {
            "name": "live_status",
            "description": (
                "PRODUCTION: read the running bot's exported status/metrics (reconciled net SOL, "
                "lane states, wallet floor headroom, open positions, circuit breakers). Use to "
                "ground research decisions in reconciled reality, never in assumptions."
            ),
            "inputSchema": {"type": "object", "properties": {}},
        },
        {
            "name": "bench_endpoint",
            "description": "Measure real generation tokens/sec of the configured llama.cpp endpoint.",
            "inputSchema": {
                "type": "object",
                "properties": {"gen_tokens": {"type": "integer", "minimum": 16, "maximum": 1024}},
            },
        },
    ]


class ToolBox:
    """Wires MCP tool calls to the real supervisor modules."""

    def __init__(self, cfg: SupervisorConfig, cfg_path: str | None = None):
        self.cfg = cfg
        self.cfg_path = cfg_path
        self.run_id = f"mcp-{uuid.uuid4().hex[:8]}"
        self.store = EvidenceStore(cfg.evidence_db)
        self.store.start_run(self.run_id, "mcp-session", SERVER_INFO["version"])
        self.gates = GateRunner(cfg.repo_path, self.store, self.run_id)

    # ------------------------------------------- production artifact auto-injection
    def _persist_binding(self, **fields: str) -> None:
        """Write discovered bindings back to supervisor.yaml so they survive restarts."""
        for k, v in fields.items():
            setattr(self.cfg, k, v)
        if not self.cfg_path:
            return
        try:
            import yaml
            data = yaml.safe_load(Path(self.cfg_path).read_text(encoding="utf-8")) or {}
            data.update(fields)
            Path(self.cfg_path).write_text(yaml.safe_dump(data, sort_keys=False),
                                           encoding="utf-8")
        except Exception:
            pass  # in-memory binding still active; persistence is best-effort

    def _resolve_evaluator(self) -> tuple[str, str, bool]:
        """Return (bin_path, pinned_sha, pinned_now). Auto-discovers and TOFU-pins on first
        sight of a released evaluator. NEVER re-pins on mismatch — that is Tier-0."""
        from ..core import artifacts
        binp = self.cfg.evaluator_bin
        if not binp or not Path(binp).is_file():
            found = artifacts.discover_evaluator(self.cfg.repo_path)
            if found:
                self._persist_binding(evaluator_bin=str(found))
                binp = str(found)
        pinned = self.cfg.evaluator_pinned_sha256
        pinned_now = False
        if binp and Path(binp).is_file() and not pinned:
            pinned = artifacts.sha256_of(binp)
            self._persist_binding(evaluator_pinned_sha256=pinned)
            pinned_now = True
        return binp, pinned, pinned_now

    def _resolve_runner(self) -> str:
        from ..core import artifacts
        binp = self.cfg.research_runner_bin
        if binp and Path(binp).is_file():
            return binp
        found = artifacts.discover_research_runner(self.cfg.repo_path)
        if found:
            self._persist_binding(research_runner_bin=str(found))
            return str(found)
        return ""

    def _resolve_status_file(self) -> str:
        from ..core import artifacts
        f = self.cfg.live_status_file
        if f and Path(f).is_file():
            return f
        found = artifacts.discover_status_file(self.cfg.repo_path)
        if found:
            self._persist_binding(live_status_file=str(found))
            return str(found)
        return ""

    def _register_artifact(self, args: dict) -> dict:
        """Explicit self-binding: the build declares what it produced and where. Bridges the
        evidence-store registry AND the persisted config bindings, under the frozen-pin law:
        an evaluator whose hash differs from an existing pin is REFUSED (human-only re-pin)."""
        from ..core import artifacts
        name, path = args["name"], args["path"]
        p = Path(path)
        if not p.exists():
            return {"error": f"path does not exist: {path}"}
        sha = artifacts.sha256_of(p) if p.is_file() else "dir"

        if name == "evaluator":
            existing_pin = self.cfg.evaluator_pinned_sha256
            reg = self.store.get_artifact("evaluator")
            if not existing_pin and reg:
                existing_pin = reg.get("pinned_sha256", "")
            if existing_pin and existing_pin != sha:
                return {"error": "REFUSED: evaluator hash differs from the frozen pin (§44)",
                        "pinned_sha256": existing_pin, "attempted_sha256": sha,
                        "instruction": "TIER-0: evaluator changes require the HUMAN re-pin "
                                       "command: python -m supervisor.supervise pin-evaluator. "
                                       "record_escalation and stop; do not trust grades."}
            self.store.register_artifact("evaluator", str(p.resolve()), sha,
                                         args.get("milestone", ""), "")
            self._persist_binding(evaluator_bin=str(p.resolve()),
                                  evaluator_pinned_sha256=(existing_pin or sha))
            return {"registered": True, "name": name, "path": str(p.resolve()),
                    "sha256": sha, "pinned_sha256": existing_pin or sha,
                    "instruction": "Evaluator registered; hash pinned (frozen, §44). Any future "
                                   "mismatch is Tier-0 and never silently re-pinned."}

        cfg_field = {"research_runner": "research_runner_bin",
                     "live_status": "live_status_file"}[name]
        self.store.register_artifact(name, str(p.resolve()), sha,
                                     args.get("milestone", ""), "")
        self._persist_binding(**{cfg_field: str(p.resolve())})
        return {"registered": True, "name": name, "path": str(p.resolve()), "sha256": sha,
                "instruction": "Artifact registered and bound; production tools now "
                               "self-resolve it (persisted across restarts)."}

    # ------------------------------------------------------------- dispatch
    def call(self, name: str, args: dict) -> dict:
        fn: Callable[[dict], dict] = {
            "gate_verify": self._gate_verify,
            "check_tier0": self._check_tier0,
            "run_reinforcement": self._run_reinforcement,
            "author_dossier": self._author_dossier,
            "propose_amendment": self._propose_amendment,
            "draft_amendment": self._draft_amendment,
            "amendment_status": self._amendment_status,
            "record_infra_fact": self._record_infra_fact,
            "evidence_status": self._evidence_status,
            "record_escalation": self._record_escalation,
            "bench_endpoint": self._bench_endpoint,
            "register_artifact": self._register_artifact,
            "evaluator_verify": self._evaluator_verify,
            "experiment_run": self._experiment_run,
            "promotion_check": self._promotion_check,
            "live_status": self._live_status,
        }.get(name, self._unknown)
        return fn(args)

    @staticmethod
    def _unknown(args: dict) -> dict:
        return {"error": "unknown tool"}

    # ---------------------------------------------------------------- tools
    def _gate_verify(self, args: dict) -> dict:
        scope = args["scope"]
        ident = args["id"]
        gcfg: GateConfig = self.cfg.gate
        if scope == "milestone":
            verdict = self.gates.milestone_gate(ident, gcfg, args.get("scoped_criteria", []))
        else:
            verdict = self.gates.task_gate(ident, gcfg)
        return {
            "passed": verdict.passed,
            "summary": verdict.summary(),
            "checks": [
                {"name": r.name, "passed": r.passed, "summary": r.summary}
                for r in verdict.results
            ],
            "trust_mismatches": verdict.trust_mismatches,
            "certified": verdict.passed,
            "note": ("Milestone may be declared complete." if verdict.passed
                     else "DO NOT declare complete. Fix failures and re-verify."),
        }

    def _check_tier0(self, args: dict) -> dict:
        hits = safety.is_blocked(args.get("diff", ""), args.get("paths", []))
        return {
            "blocked": bool(hits),
            "hits": [{"domain": h.domain.value, "reason": h.reason, "excerpt": h.excerpt}
                     for h in hits],
            "instruction": ("STOP. Escalate to the human via record_escalation; do not apply."
                            if hits else "No Tier-0 concerns; proceed under normal gates."),
        }

    def _dossier_authoring_brief(self, component: str, available: list) -> dict:
        """A hard component has no dossier. Do NOT let GLM author its own tests
        (that is the model grading itself — the anti-agreeability circularity the
        constitution forbids). Instead emit a structured authoring brief, record an
        escalation, and HALT reinforcement for this component until an independent
        dossier is committed.

        The brief pulls the component's spec context from the committed constitution
        so the human/design session has everything needed to author independently.
        """
        # Pull constitution context: lines mentioning the component, for the author.
        ctx_snippets: list[str] = []
        try:
            cpath = getattr(self.cfg, "constitution_path", "")
            if cpath and os.path.isfile(cpath):
                with open(cpath, encoding="utf-8", errors="replace") as fh:
                    for ln in fh:
                        if component.replace("_", " ") in ln.lower() or component in ln:
                            ctx_snippets.append(ln.strip()[:240])
                        if len(ctx_snippets) >= 12:
                            break
        except Exception:  # noqa: BLE001
            pass

        brief = {
            "halt": True,
            "error": f"no dossier for '{component}' — reinforcement halted, authoring brief below",
            "reason": f"HARD component '{component}' has no dossier — reinforcement cannot run.",
            "why_not_autogenerate": (
                "The dossier's property tests are the sole authority over correctness. "
                "If the builder model authors them, it grades its own work — the exact "
                "circularity the constitution's anti-agreeability law prohibits. The dossier "
                "must be authored independently (design session or human), then committed."
            ),
            "authoring_contract": {
                "path": f"supervisor/reinforcement/dossiers/{component}.yaml",
                "required_fields": ["component", "constitution_refs", "spec", "invariants",
                                    "adversarial_checks", "leaves[]"],
                "leaf_fields": ["leaf_id", "responsibility", "signature", "invariants",
                                "reference_pattern", "property_test", "max_lines",
                                "depends_on", "temperature_band"],
                "rules": [
                    "Each leaf is a single responsibility, <= ~60 lines.",
                    "property_test is the judge and must be authored independently of any "
                    "reference_pattern; the pattern guides, the test governs.",
                    "high temperature_band leaves get a full labeled UNVERIFIED reference impl.",
                    "Money math integer fixed-point; no floats in outcome-controlling logic.",
                ],
                "format_exemplar": "supervisor/reinforcement/dossiers/fixedpoint.yaml",
            },
            "constitution_context": ctx_snippets or ["(no direct mentions found; read the "
                                                     "component's governing section directly)"],
            "available_dossiers": available,
            "next_action": ("Escalation recorded. Bring this brief to a design session to author "
                            f"{component}.yaml independently, commit it, then re-run "
                            "run_reinforcement(component). The build should advance other work "
                            "meanwhile; it may not fabricate this dossier to proceed."),
        }
        # Record the escalation so it shows up in status / the operator channel.
        try:
            self._record_escalation({
                "milestone": "reinforcement",
                "task_id": f"dossier:{component}",
                "domain": "missing_dossier",
                "context": brief["reason"] + " " + brief["why_not_autogenerate"],
            })
        except Exception:  # noqa: BLE001
            pass
        return brief

    @staticmethod
    def _available_dossiers_safe() -> list:
        try:
            from ..reinforcement.dossier import available_dossiers
            return sorted(available_dossiers().keys())
        except Exception:  # noqa: BLE001
            return []


    # --------------------------------------------- constitution amendments (living doc)

    def _record_infra_fact(self, args: dict) -> dict:
        """Append a provenance-stamped infrastructure fact to the manifest's facts ledger.

        This is the AGENT-WRITABLE lane the constitution's manifest-recording obligations use
        (verified provider plans, endpoint statuses, CPU features, source lifecycle notes).
        It is safe to expose because certification never reads it: the phase gate trusts only
        the operator-pinned deployment_host declaration plus LIVE machine measurement. Every
        append is journaled to the evidence store.
        """
        import json as _json, time as _time
        from pathlib import Path as _P
        key = (args.get("key") or "").strip()
        value = (args.get("value") or "").strip()
        source = (args.get("source") or "").strip()
        if not key or not value or not source:
            return {"error": "key, value, and source are all required — a fact without "
                             "provenance is not a fact"}
        mpath = _P(getattr(self.cfg, "repo_path", ".")) / "infra_manifest.json"
        data = {}
        if mpath.is_file():
            try:
                data = _json.loads(mpath.read_text(encoding="utf-8"))
            except _json.JSONDecodeError:
                return {"error": f"{mpath} is not valid JSON; fix it before appending"}
        data.setdefault("facts", []).append(
            {"key": key, "value": value[:2000], "source": source, "at": _time.time(),
             "by": "mcp:record_infra_fact"})
        try:
            mpath.write_text(_json.dumps(data, indent=2), encoding="utf-8")
        except OSError as e:
            return {"error": f"could not write manifest: {e}"}
        try:
            self.store.journal_infra_fact(key, value, source, by="mcp")
        except Exception:  # noqa: BLE001  (journal failure must not lose the fact)
            pass
        return {"recorded": key, "facts_count": len(data["facts"]),
                "note": ("Appended to the facts ledger and journaled. This lane never affects "
                         "Phase-B certification — deployment identity is measured live and "
                         "pinned by the operator, so facts writes cannot forge provenance.")}

    def _propose_amendment(self, args: dict) -> dict:
        """Builder-facing INTAKE ONLY. Cannot draft, approve, or apply anything.

        The constitution becomes a living document through evidence + human approval,
        never through the builder narrating itself new permissions.
        """
        from ..core.amendment import touches_tier0
        title = args.get("title", "").strip()
        kind = args.get("kind", "law").strip()
        rationale = args.get("rationale", "").strip()
        evidence_ref = args.get("evidence_ref", "").strip()
        target_hint = args.get("target_hint", "").strip()

        t0 = touches_tier0(f"{title} {target_hint}")
        if t0:
            return {"accepted": False,
                    "tier0_blocked": t0,
                    "reason": (f"proposal targets Tier-0 material ('{t0}'). Tier-0 — key "
                               "custody, evaluator integrity, wallet floor, promotion-gate "
                               "integrity — cannot be amended through this path at all. "
                               "Raise it with the operator directly; it changes only by a "
                               "deliberate human edit.")}
        out = self.store.propose_amendment(kind, title, rationale, evidence_ref,
                                           proposed_by="builder", target_hint=target_hint)
        if out.get("accepted"):
            out["instruction"] = ("Queued. You cannot draft, approve, or apply it. A human "
                                  "reviews it; the independent design model drafts the text. "
                                  "Continue your current work.")
        return out

    def _draft_amendment(self, args: dict) -> dict:
        """Route a queued proposal to the INDEPENDENT design model for constitution text."""
        from ..core.design_client import DesignModelClient, DesignModelUnavailable
        from ..core.amendment import touches_tier0

        aid = int(args["amendment_id"])
        a = self.store.get_amendment(aid)
        if not a:
            return {"error": f"no amendment {aid}"}
        if a["state"] not in ("proposed", "drafted"):
            return {"error": f"amendment {aid} is '{a['state']}'; only proposed/drafted can be drafted"}

        dcfg = getattr(self.cfg, "design_model", None)
        if dcfg is None:
            return {"error": "design_model not configured"}
        try:
            client = DesignModelClient(dcfg)
        except DesignModelUnavailable as e:
            return {"error": f"design model unavailable: {e}",
                    "instruction": "The proposal stays queued for human drafting."}

        try:
            ctext = Path(self.cfg.constitution_path).read_text(encoding="utf-8",
                                                               errors="replace")
        except OSError as e:
            return {"error": f"cannot read constitution: {e}"}

        system = (
            "You draft amendments to a binding engineering constitution for an autonomous "
            "trading system. Output ONLY the new or replacement prose, in the document's "
            "existing voice and formatting conventions — no preamble, no fences, no meta "
            "commentary. Rules: never weaken or delete an acceptance criterion; never touch "
            "Tier-0 material (key custody, evaluator integrity, wallet floor, promotion-gate "
            "integrity); state the evidence the amendment rests on; prefer adding a narrowly "
            "scoped law over broadening an existing one. If the proposal is not justified by "
            "the cited evidence, say so in one line instead of drafting."
        )
        user = (
            f"PROPOSAL #{aid}\nkind: {a['kind']}\ntitle: {a['title']}\n"
            f"rationale: {a['rationale']}\nevidence_ref: {a['evidence_ref']}\n"
            f"target section hint: {a['target_hint'] or '(unspecified)'}\n\n"
            "CONSTITUTION EXCERPT (for voice, numbering, and placement):\n"
            + ctext[:12000] + "\n\n[...document continues...]\n\n"
            "Draft the amendment text now."
        )
        try:
            text = client.complete(system, user)
        except DesignModelUnavailable as e:
            return {"error": f"design model call failed: {e}"}

        if touches_tier0(text):
            return {"error": "drafted text touches Tier-0 material; refused and not stored",
                    "preview": text[:600]}
        res = self.store.set_amendment_draft(aid, text, drafted_by=f"{dcfg.provider}:{dcfg.model}")
        return {"amendment_id": aid, "state": res.get("state"), "drafted_text": text[:4000],
                "human_approval_required": True,
                "instruction": ("Drafted by the independent design model. It is NOT applied. "
                                "A human approves via the CLI: "
                                "`hermes-supervise amendments approve --id %d` — that verb is "
                                "deliberately absent from this tool surface." % aid)}

    def _amendment_status(self, args: dict) -> dict:
        state = args.get("state", "")
        items = self.store.list_amendments(state)
        return {
            "count": len(items),
            "amendments": [{k: v for k, v in a.items() if k != "diff_text"} for a in items],
            "note": ("Approval and application are human-only CLI actions "
                     "(`hermes-supervise amendments approve|apply`). No tool here can "
                     "approve or apply an amendment."),
        }

    def _author_dossier(self, args: dict) -> dict:
        """Route a dossier authoring brief to the INDEPENDENT design model (frontier API),
        validate the result through the real loader, and write it to disk.

        Independence: the design model is a different model, endpoint, and key from the
        builder (GLM). It reads the constitution and emits the dossier; it never sees the
        builder's code and never runs gates. The builder therefore does not author the
        tests that judge it. The human still reviews via the recorded escalation.
        """
        from ..core.design_client import DesignModelClient, DesignModelUnavailable
        from ..reinforcement.dossier import load_dossier, dossier_dir

        component = args["component"]
        dcfg = getattr(self.cfg, "design_model", None)
        if dcfg is None:
            return {"error": "design_model not configured",
                    "instruction": "Add a design_model block to supervisor.yaml (enabled, "
                                   "model, api_key_env) or author the dossier manually."}
        try:
            client = DesignModelClient(dcfg)
        except DesignModelUnavailable as e:
            return {"error": f"design model unavailable: {e}",
                    "instruction": "Set the API key env var and enable design_model, or "
                                   "author the dossier in a design session. The build must "
                                   "not fabricate it to proceed."}

        brief = self._dossier_authoring_brief(component, self._available_dossiers_safe())
        exemplar = ""
        try:
            ex = dossier_dir() / "fixedpoint.yaml"
            if ex.is_file():
                exemplar = ex.read_text(encoding="utf-8")[:6000]
        except OSError:
            pass

        system = (
            "You are authoring a task dossier for an autonomous Rust build supervisor. "
            "The dossier decomposes a HARD component into leaves; each leaf carries an exact "
            "Rust signature, machine-checkable invariants, a reference pattern, and a PROPERTY "
            "TEST that is the sole authority over correctness. You are independent of the "
            "builder model: author the property tests to catch a plausible-but-wrong "
            "implementation, never to rubber-stamp one. Integer fixed-point for money; no "
            "floats in outcome-controlling logic. Output ONLY valid YAML — no prose, no "
            "code fences."
        )
        user = (
            f"COMPONENT: {component}\n\n"
            f"AUTHORING CONTRACT:\n{json.dumps(brief['authoring_contract'], indent=2)}\n\n"
            f"CONSTITUTION CONTEXT (lines mentioning this component):\n"
            + "\n".join(brief["constitution_context"]) + "\n\n"
            f"FORMAT EXEMPLAR (match this schema exactly):\n{exemplar}\n\n"
            "Emit the complete dossier YAML for this component now."
        )

        try:
            text = client.complete(system, user)
        except DesignModelUnavailable as e:
            return {"error": f"design model call failed: {e}",
                    "instruction": "Escalation stands; author manually or retry."}

        # strip accidental fences, then validate through the REAL loader before writing
        body = text.strip()
        if body.startswith("```"):
            body = body.split("\n", 1)[-1]
            body = body.rsplit("```", 1)[0]
        target = dossier_dir() / f"{component}.yaml"
        tmp = dossier_dir() / f".{component}.yaml.candidate"
        try:
            tmp.write_text(body, encoding="utf-8")
            d = load_dossier(tmp)          # schema validation (Leaf(**l) rejects unknown keys)
            order = d.leaf_order()          # cycle detection
            if d.component != component:
                raise ValueError(f"component field is '{d.component}', expected '{component}'")
            if not order:
                raise ValueError("dossier has no leaves")
            for leaf in order:
                if not (leaf.signature and leaf.property_test):
                    raise ValueError(f"leaf {leaf.leaf_id} missing signature or property_test")
            tmp.replace(target)
        except Exception as e:  # noqa: BLE001
            try:
                tmp.unlink(missing_ok=True)
            except OSError:
                pass
            return {"error": f"authored dossier failed validation: {e}",
                    "raw_preview": body[:1200],
                    "instruction": "Not written. Retry author_dossier or author manually; "
                                   "an invalid dossier is never installed."}

        return {
            "component": component,
            "written": str(target),
            "leaves": [l.leaf_id for l in order],
            "authored_by": f"{dcfg.provider}:{dcfg.model} (independent of the builder model)",
            "human_review_required": True,
            "instruction": ("Dossier installed and schema-valid. A human should review the "
                            "property tests before trusting them as the correctness authority "
                            "— they now judge every implementation of this component. Then run "
                            "run_reinforcement(component)."),
        }

    def _run_reinforcement(self, args: dict) -> dict:
        # Imported lazily: needs model endpoint + dossier + a scratch-crate verifier.
        from ..reinforcement.dossier import available_dossiers, load_dossier
        from ..reinforcement.engine import ReinforcementEngine
        from ..core.model_client import ModelClient

        component = args["component"]
        dossiers = available_dossiers()
        if component not in dossiers:
            return self._dossier_authoring_brief(component, sorted(dossiers))
        dossier = load_dossier(dossiers[component])
        model = ModelClient(self.cfg.model)
        verifier = _make_scratch_verifier(self.cfg.repo_path)
        engine = ReinforcementEngine(model, self.store, verifier)
        ok, bodies, outcomes = engine.implement_component(dossier)
        result = {
            "component": component,
            "all_leaves_passed": ok,
            "leaves": [{"leaf_id": o.leaf_id, "passed": o.passed,
                        "attempts": o.attempts, "reason": o.reason} for o in outcomes],
        }
        if ok:
            result["bodies"] = bodies
            result["instruction"] = ("Integrate these verified bodies into the component, then "
                                     "run gate_verify(scope=task) before proceeding.")
        else:
            stuck = next(o for o in outcomes if not o.passed)
            result["stuck_leaf"] = stuck.leaf_id
            result["instruction"] = ("Escalate this single leaf to the human via "
                                     "record_escalation with full context; do not hand-write "
                                     "an unverified implementation.")
        return result

    def _evidence_status(self, args: dict) -> dict:
        ms = args.get("milestone", "")
        out: dict[str, Any] = {"open_escalations": self.store.open_escalations(self.run_id)}
        if ms:
            out["unsatisfied_criteria"] = self.store.unsatisfied_criteria(ms, self.run_id)
        return out

    def _record_escalation(self, args: dict) -> dict:
        eid = self.store.escalate(self.run_id, args["milestone"], args["task_id"],
                                  args["domain"], args["context"])
        return {"escalation_id": eid,
                "instruction": "Halted pending human resolution. Do not proceed on this item."}

    # ------------------------------------------------- production-phase tools
    def _evaluator_verify(self, args: dict) -> dict:
        from ..core import artifacts
        binp, pinned, pinned_now = self._resolve_evaluator()
        if not binp or not Path(binp).is_file():
            return {"error": "evaluator not built yet (searched target/release/pq-evaluator*)",
                    "instruction": "Nothing to configure — the supervisor auto-discovers the "
                                   "evaluator the moment M5 builds it at the canonical path. "
                                   "Until then no experiment grade is trustworthy."}
        actual = artifacts.sha256_of(binp)
        ok = actual == pinned
        out = {"verified": ok, "binary": binp, "actual_sha256": actual,
               "pinned_sha256": pinned}
        if pinned_now:
            out["pinned_now"] = True
            out["instruction"] = ("Evaluator discovered and hash PINNED (trust-on-first-use at "
                                  "release, §44). Future runs verify against this pin; any "
                                  "mismatch is Tier-0 and will never be silently re-pinned.")
        else:
            out["instruction"] = ("Evaluator integrity confirmed; grades may be trusted." if ok
                                  else "TIER-0: hash mismatch — evaluator was modified since "
                                       "pinning. STOP, record_escalation, do not trust any "
                                       "grade. Re-pinning requires the human.")
        return out

    def _experiment_run(self, args: dict) -> dict:
        import subprocess
        runner = self._resolve_runner()
        if not runner:
            return {"error": "research runner not built yet "
                             "(searched target/release/pq-research-runner*)",
                    "instruction": "Nothing to configure — auto-discovered the moment M7/M8 "
                                   "builds it at the canonical path."}
        exp_id = args["experiment_id"]
        try:
            p = subprocess.run([runner, "--experiment", exp_id, "--sealed"],
                               capture_output=True, text=True, timeout=7200)
            return {"experiment_id": exp_id, "returncode": p.returncode,
                    "stdout_tail": p.stdout[-2000:], "stderr_tail": p.stderr[-1000:],
                    "instruction": "Grade this only via the frozen evaluator (evaluator_verify "
                                   "first); never self-grade."}
        except subprocess.TimeoutExpired:
            return {"error": f"experiment {exp_id} timed out"}

    def _promotion_check(self, args: dict) -> dict:
        sid = args["strategy_id"]
        scope = args["scope"]
        open_esc = self.store.open_escalations(self.run_id)
        ready = not open_esc
        out = {"strategy_id": sid, "scope": scope,
               "open_escalations": len(open_esc), "preconditions_ready": ready,
               "human_gate_required": scope == "live"}
        if scope == "live":
            out["instruction"] = ("This tool NEVER authorizes live capital. If preconditions are "
                                  "ready, present the evidence to the human operator and WAIT for "
                                  "explicit approval (constitution §64).")
        else:
            out["instruction"] = ("Shadow/probe advancement permitted only via the constitution's "
                                  "promotion path with these preconditions satisfied.")
        return out

    def _live_status(self, args: dict) -> dict:
        f = self._resolve_status_file()
        if not f:
            return {"error": "live status export not found yet "
                             "(searched data/live_status.json)",
                    "instruction": "Nothing to configure — auto-discovered the moment the bot "
                                   "exports status at the canonical path (§60)."}
        try:
            return {"status": json.loads(Path(f).read_text(encoding="utf-8"))}
        except json.JSONDecodeError as e:
            return {"error": f"status file unreadable: {e}"}

    def _bench_endpoint(self, args: dict) -> dict:
        from ..core.model_client import ModelClient, ModelUnavailable
        n = int(args.get("gen_tokens", 128))
        client = ModelClient(self.cfg.model)
        try:
            client.health()
            t0 = time.time()
            text = client.freeform(
                "You are a benchmark. Output plain prose only.",
                f"Write approximately {n} tokens describing the Rust borrow checker.",
                max_tokens=n,
            )
            dt = time.time() - t0
            approx_tokens = max(1, len(text.split()))  # rough; server-side token count unavailable here
            return {"seconds": round(dt, 2),
                    "approx_tokens": approx_tokens,
                    "approx_tokens_per_sec": round(approx_tokens / dt, 2),
                    "note": "word-count approximation; for exact tok/s use llama-bench"}
        except ModelUnavailable as e:
            return {"error": f"endpoint unavailable: {e}"}


def _make_scratch_verifier(repo_path: str):
    """
    Leaf verifier: writes the candidate body + its property test into a scratch cargo crate
    under <repo>/.supervisor_scratch and runs `cargo test`. Falls back to a clear error if
    cargo is unavailable — never a silent pass.
    """
    import shutil
    import subprocess

    def verify(leaf, body: str) -> tuple[bool, str]:
        if shutil.which("cargo") is None:
            return False, "cargo not found; cannot verify leaf"
        scratch = Path(repo_path) / ".supervisor_scratch" / leaf.leaf_id
        src = scratch / "src"
        src.mkdir(parents=True, exist_ok=True)
        (scratch / "Cargo.toml").write_text(
            '[package]\nname = "leafcheck"\nversion = "0.0.0"\nedition = "2021"\n',
            encoding="utf-8",
        )
        (src / "lib.rs").write_text(
            f"{leaf.signature} {{\n{body}\n}}\n\n#[cfg(test)]\nmod tests {{\n"
            f"use super::*;\n{leaf.property_test}\n}}\n",
            encoding="utf-8",
        )
        try:
            p = subprocess.run(["cargo", "test", "--quiet"], cwd=scratch,
                               capture_output=True, text=True, timeout=600)
            return p.returncode == 0, (p.stderr[-500:] if p.returncode else "ok")
        except subprocess.TimeoutExpired:
            return False, "leaf verification timeout"

    return verify


# ------------------------------------------------------------------ JSON-RPC loop
def _rpc_result(id_: Any, result: dict) -> dict:
    return {"jsonrpc": "2.0", "id": id_, "result": result}


def _rpc_error(id_: Any, code: int, message: str) -> dict:
    return {"jsonrpc": "2.0", "id": id_, "error": {"code": code, "message": message}}


def handle_message(msg: dict, box: ToolBox) -> dict | None:
    """Pure message handler (also used by tests). Returns response dict or None for notifications."""
    method = msg.get("method", "")
    id_ = msg.get("id")
    if method == "initialize":
        return _rpc_result(id_, {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": SERVER_INFO,
        })
    if method in ("notifications/initialized", "initialized"):
        return None
    if method == "ping":
        return _rpc_result(id_, {})
    if method == "tools/list":
        return _rpc_result(id_, {"tools": _tool_schemas()})
    if method == "tools/call":
        params = msg.get("params", {})
        name = params.get("name", "")
        args = params.get("arguments", {}) or {}
        try:
            out = box.call(name, args)
            is_err = "error" in out
            return _rpc_result(id_, {
                "content": [{"type": "text", "text": json.dumps(out, indent=2)}],
                "isError": is_err,
            })
        except Exception as e:  # noqa: BLE001 — tool errors must round-trip, not kill the server
            return _rpc_result(id_, {
                "content": [{"type": "text",
                             "text": json.dumps({"error": str(e),
                                                 "trace": traceback.format_exc()[-800:]})}],
                "isError": True,
            })
    if id_ is not None:
        return _rpc_error(id_, -32601, f"method not found: {method}")
    return None


def serve(cfg_path: str) -> int:
    cfg = SupervisorConfig.load(cfg_path)
    box = ToolBox(cfg, cfg_path=cfg_path)
    stdin = io.TextIOWrapper(sys.stdin.buffer, encoding="utf-8")
    stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8", line_buffering=True)
    for line in stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        resp = handle_message(msg, box)
        if resp is not None:
            stdout.write(json.dumps(resp) + "\n")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--config", required=True)
    args = ap.parse_args()
    return serve(args.config)


if __name__ == "__main__":
    sys.exit(main())
