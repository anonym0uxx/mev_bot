"""
Control-channel schemas for GLM-5.2 interaction.

Every *structured* model turn (status reports, milestone verdicts, tool/diff proposals,
hypotheses) is constrained by one of these schemas via llama.cpp's json_schema / GBNF
grammar so malformed control output is mechanically impossible. Free-form code generation
is unconstrained; only the control envelope is grammar-locked.

These are plain dataclasses with explicit JSON-schema builders (no hard pydantic dependency,
so the supervisor stays lightweight and portable on the Windows server). A pydantic adapter
is provided if available.
"""
from __future__ import annotations

import json
from dataclasses import dataclass, field, asdict
from enum import Enum
from typing import Any


# ---------------------------------------------------------------------------
# Enums (closed vocabularies -> GBNF enum rules -> model physically cannot drift)
# ---------------------------------------------------------------------------
class TaskAction(str, Enum):
    PROPOSE_DIFF = "PROPOSE_DIFF"
    ASK_CLARIFY = "ASK_CLARIFY"
    REPORT_BLOCKED = "REPORT_BLOCKED"


class Impact(str, Enum):
    NONE = "none"
    LOW = "low"
    MEDIUM = "medium"
    HIGH = "high"
    UNKNOWN = "unknown"


# ---------------------------------------------------------------------------
# JSON-schema builders
# ---------------------------------------------------------------------------
def _enum(values: list[str]) -> dict:
    return {"type": "string", "enum": values}


TASK_RESPONSE_SCHEMA: dict[str, Any] = {
    "type": "object",
    "additionalProperties": False,
    "required": ["task_id", "action", "files_changed", "self_check", "confidence"],
    "properties": {
        "task_id": {"type": "string"},
        "action": _enum([a.value for a in TaskAction]),
        "files_changed": {
            "type": "array",
            "items": {
                "type": "object",
                "additionalProperties": False,
                "required": ["path", "rationale"],
                "properties": {
                    "path": {"type": "string"},
                    "rationale": {"type": "string"},
                },
            },
        },
        "diff": {"type": "string"},
        "self_check": {
            "type": "object",
            "additionalProperties": False,
            "required": [
                "compiles_locally",
                "tests_added",
                "constitution_refs_satisfied",
                "determinism_impact",
                "latency_impact",
            ],
            "properties": {
                "compiles_locally": {"type": "boolean"},
                "tests_added": {"type": "array", "items": {"type": "string"}},
                "constitution_refs_satisfied": {"type": "array", "items": {"type": "string"}},
                "determinism_impact": _enum([i.value for i in Impact]),
                "latency_impact": _enum([i.value for i in Impact]),
            },
        },
        "confidence": {"type": "number", "minimum": 0.0, "maximum": 1.0},
    },
}

LEAF_RESPONSE_SCHEMA: dict[str, Any] = {
    "type": "object",
    "additionalProperties": False,
    "required": ["leaf_id", "body", "notes"],
    "properties": {
        "leaf_id": {"type": "string"},
        # For a scaffolded leaf, the model returns ONLY the function/impl body it was asked to fill.
        "body": {"type": "string"},
        "notes": {"type": "string"},
    },
}

MILESTONE_VERDICT_SCHEMA: dict[str, Any] = {
    "type": "object",
    "additionalProperties": False,
    "required": ["milestone", "self_assessment", "criteria_addressed", "known_gaps"],
    "properties": {
        "milestone": {"type": "string"},
        "self_assessment": _enum(["complete", "incomplete", "blocked"]),
        "criteria_addressed": {"type": "array", "items": {"type": "string"}},
        "known_gaps": {"type": "array", "items": {"type": "string"}},
    },
}

HYPOTHESIS_SCHEMA: dict[str, Any] = {
    "type": "object",
    "additionalProperties": False,
    "required": [
        "hypothesis_id",
        "statement",
        "causal_mechanism",
        "expected_net_sol_impact",
        "prior_probability",
        "cost_to_test",
        "edge_half_life",
        "competing_explanations",
        "disconfirming_evidence_sought",
    ],
    "properties": {
        "hypothesis_id": {"type": "string"},
        "statement": {"type": "string"},
        "causal_mechanism": {"type": "string"},
        "expected_net_sol_impact": {"type": "number"},
        "prior_probability": {"type": "number", "minimum": 0.0, "maximum": 1.0},
        "cost_to_test": _enum([i.value for i in Impact]),
        "edge_half_life": _enum(["hours", "days", "weeks", "months", "unknown"]),
        "competing_explanations": {"type": "array", "items": {"type": "string"}},
        "disconfirming_evidence_sought": {"type": "array", "items": {"type": "string"}},
    },
}


# ---------------------------------------------------------------------------
# Dataclasses (typed access after validation)
# ---------------------------------------------------------------------------
@dataclass
class FileChange:
    path: str
    rationale: str


@dataclass
class SelfCheck:
    compiles_locally: bool
    tests_added: list[str]
    constitution_refs_satisfied: list[str]
    determinism_impact: str
    latency_impact: str


@dataclass
class TaskResponse:
    task_id: str
    action: str
    files_changed: list[FileChange]
    self_check: SelfCheck
    confidence: float
    diff: str = ""

    @classmethod
    def from_json(cls, data: dict) -> "TaskResponse":
        return cls(
            task_id=data["task_id"],
            action=data["action"],
            files_changed=[FileChange(**f) for f in data.get("files_changed", [])],
            self_check=SelfCheck(**data["self_check"]),
            confidence=float(data["confidence"]),
            diff=data.get("diff", ""),
        )


# ---------------------------------------------------------------------------
# GBNF export — for llama.cpp servers that prefer a raw grammar over json_schema.
# The llama.cpp server also accepts json_schema directly; we ship both paths.
# ---------------------------------------------------------------------------
def schema_to_gbnf_via_server_hint(schema: dict) -> str:
    """
    Return the JSON string of the schema for use with llama.cpp's `json_schema` field
    (the server converts it to GBNF internally via json-schema-to-grammar). This is the
    preferred path; a local converter is unnecessary and error-prone to reimplement.
    """
    return json.dumps(schema, separators=(",", ":"))


SCHEMAS: dict[str, dict] = {
    "task": TASK_RESPONSE_SCHEMA,
    "leaf": LEAF_RESPONSE_SCHEMA,
    "milestone_verdict": MILESTONE_VERDICT_SCHEMA,
    "hypothesis": HYPOTHESIS_SCHEMA,
}


def get_schema(name: str) -> dict:
    if name not in SCHEMAS:
        raise KeyError(f"unknown control schema: {name!r} (have {list(SCHEMAS)})")
    return SCHEMAS[name]
