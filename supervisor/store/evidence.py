"""
Evidence store — the supervisor's memory and audit trail.

SQLite (WAL) journal of every model call, gate result, commit, escalation, and the
per-component capability map that drives adaptive difficulty. Everything is stamped
with the constitution git hash so any decision is reproducible and any run resumable.

No ORM dependency; plain sqlite3 with a thin typed layer for portability on Windows.
"""
from __future__ import annotations

import json
import sqlite3
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Optional


SCHEMA = """
CREATE TABLE IF NOT EXISTS runs (
    run_id            TEXT PRIMARY KEY,
    started_at        REAL,
    constitution_hash TEXT,
    supervisor_version TEXT,
    note              TEXT
);
CREATE TABLE IF NOT EXISTS tasks (
    task_id     TEXT,
    run_id      TEXT,
    milestone   TEXT,
    kind        TEXT,          -- 'task' | 'leaf' | 'refactor' | 'optimize'
    created_at  REAL,
    status      TEXT,          -- 'pending'|'proposed'|'gated_pass'|'gated_fail'|'integrated'|'escalated'
    attempts    INTEGER DEFAULT 0,
    payload     TEXT,          -- json: prompt context + model response
    PRIMARY KEY (task_id, run_id)
);
CREATE TABLE IF NOT EXISTS gate_results (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id     TEXT,
    run_id      TEXT,
    gate        TEXT,          -- 'build'|'clippy'|'fmt'|'test'|'bench'|'determinism'|'secrets'|'criteria'
    passed      INTEGER,       -- 0/1
    detail      TEXT,          -- json: stdout/stderr summary, numbers
    created_at  REAL
);
CREATE TABLE IF NOT EXISTS commits (
    sha         TEXT PRIMARY KEY,
    run_id      TEXT,
    milestone   TEXT,
    task_id     TEXT,
    message     TEXT,
    created_at  REAL
);
CREATE TABLE IF NOT EXISTS escalations (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id      TEXT,
    milestone   TEXT,
    task_id     TEXT,
    domain      TEXT,          -- tier0 domain or 'retry_exhausted' etc.
    context     TEXT,
    created_at  REAL,
    resolved_at REAL
);
CREATE TABLE IF NOT EXISTS capability (
    component   TEXT,          -- e.g. 'reducer','shred','lockfree','fixedpoint'
    leaf_size   INTEGER,       -- max lines granted
    best_of_n   INTEGER,
    successes   INTEGER DEFAULT 0,
    attempts    INTEGER DEFAULT 0,
    updated_at  REAL,
    PRIMARY KEY (component, leaf_size, best_of_n)
);
CREATE TABLE IF NOT EXISTS benchmarks (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    component   TEXT,
    metric      TEXT,          -- 'p50_ns','p99_ns','p999_ns','throughput'
    value       REAL,
    commit_sha  TEXT,
    created_at  REAL
);
CREATE TABLE IF NOT EXISTS artifacts (
    name        TEXT PRIMARY KEY,   -- 'evaluator' | 'research_runner' | 'live_status' | ...
    path        TEXT,
    sha256      TEXT,               -- content hash at registration
    pinned_sha256 TEXT,             -- for the evaluator: the FROZEN pin (§44); set once, human-repin only
    milestone   TEXT,
    commit_sha  TEXT,
    registered_at REAL
);
CREATE TABLE IF NOT EXISTS kv (k TEXT PRIMARY KEY, v TEXT);
CREATE TABLE IF NOT EXISTS amendments (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    kind          TEXT,          -- new_component | law | strategy | correction
    title         TEXT,
    rationale     TEXT,
    evidence_ref  TEXT,          -- MUST resolve to a real record; intake enforces
    proposed_by   TEXT,          -- builder | design | human
    target_hint   TEXT,
    diff_text     TEXT,          -- authored by the design model at draft time
    state         TEXT,          -- proposed | drafted | approved | applied | rejected
    created_at    REAL,
    decided_at    REAL,
    decided_by    TEXT,
    note          TEXT,
    dedup_key     TEXT           -- title+kind hash; blocks proposal flooding
);
CREATE UNIQUE INDEX IF NOT EXISTS amendments_dedup
    ON amendments(dedup_key) WHERE state IN ('proposed','drafted','approved');
CREATE TABLE IF NOT EXISTS criteria_map (
    criterion   TEXT,          -- e.g. '12','56','77'
    milestone   TEXT,
    evidence    TEXT,          -- gate ref or artifact path
    satisfied   INTEGER,
    run_id      TEXT,
    updated_at  REAL,
    PRIMARY KEY (criterion, run_id)
);
"""


@dataclass
class GateRecord:
    task_id: str
    gate: str
    passed: bool
    detail: dict[str, Any]


class EvidenceStore:
    def __init__(self, path: str | Path = "supervisor_evidence.db"):
        self.path = str(path)
        self._db = sqlite3.connect(self.path)
        self._db.execute("PRAGMA journal_mode=WAL;")
        self._db.executescript(SCHEMA)
        self._db.commit()

    # -------------------------------------------------------------------- runs
    def start_run(self, run_id: str, constitution_hash: str, version: str, note: str = "") -> None:
        self._db.execute(
            "INSERT OR REPLACE INTO runs VALUES (?,?,?,?,?)",
            (run_id, time.time(), constitution_hash, version, note),
        )
        self._db.commit()

    # ------------------------------------------------------------------- tasks
    def upsert_task(self, task_id: str, run_id: str, milestone: str, kind: str,
                    status: str, payload: dict) -> None:
        cur = self._db.execute(
            "SELECT attempts FROM tasks WHERE task_id=? AND run_id=?", (task_id, run_id)
        ).fetchone()
        attempts = (cur[0] + 1) if cur else 0
        self._db.execute(
            "INSERT OR REPLACE INTO tasks VALUES (?,?,?,?,?,?,?,?)",
            (task_id, run_id, milestone, kind, time.time(), status, attempts, json.dumps(payload)),
        )
        self._db.commit()

    def task_attempts(self, task_id: str, run_id: str) -> int:
        row = self._db.execute(
            "SELECT attempts FROM tasks WHERE task_id=? AND run_id=?", (task_id, run_id)
        ).fetchone()
        return row[0] if row else 0

    # ------------------------------------------------------------------- gates
    def record_gate(self, run_id: str, rec: GateRecord) -> None:
        self._db.execute(
            "INSERT INTO gate_results (task_id,run_id,gate,passed,detail,created_at) VALUES (?,?,?,?,?,?)",
            (rec.task_id, run_id, rec.gate, int(rec.passed), json.dumps(rec.detail), time.time()),
        )
        self._db.commit()

    # ----------------------------------------------------------------- commits
    def record_commit(self, sha: str, run_id: str, milestone: str, task_id: str, message: str) -> None:
        self._db.execute(
            "INSERT OR REPLACE INTO commits VALUES (?,?,?,?,?,?)",
            (sha, run_id, milestone, task_id, message, time.time()),
        )
        self._db.commit()

    # -------------------------------------------------------------- escalations
    def escalate(self, run_id: str, milestone: str, task_id: str, domain: str, context: str) -> int:
        cur = self._db.execute(
            "INSERT INTO escalations (run_id,milestone,task_id,domain,context,created_at) VALUES (?,?,?,?,?,?)",
            (run_id, milestone, task_id, domain, context, time.time()),
        )
        self._db.commit()
        return cur.lastrowid

    def open_escalations(self, run_id: str, limit: int = 500) -> list[dict]:
        rows = self._db.execute(
            "SELECT id,milestone,task_id,domain,context,created_at FROM escalations "
            "WHERE run_id=? AND resolved_at IS NULL ORDER BY created_at LIMIT ?", (run_id, limit)
        ).fetchall()
        cols = ["id", "milestone", "task_id", "domain", "context", "created_at"]
        return [dict(zip(cols, r)) for r in rows]

    # -------------------------------------------------------------- capability
    def record_capability(self, component: str, leaf_size: int, best_of_n: int, success: bool) -> None:
        row = self._db.execute(
            "SELECT successes,attempts FROM capability WHERE component=? AND leaf_size=? AND best_of_n=?",
            (component, leaf_size, best_of_n),
        ).fetchone()
        succ, att = (row[0], row[1]) if row else (0, 0)
        succ += 1 if success else 0
        att += 1
        self._db.execute(
            "INSERT OR REPLACE INTO capability VALUES (?,?,?,?,?,?)",
            (component, leaf_size, best_of_n, succ, att, time.time()),
        )
        self._db.commit()

    def capability_rate(self, component: str) -> dict[tuple[int, int], float]:
        rows = self._db.execute(
            "SELECT leaf_size,best_of_n,successes,attempts FROM capability WHERE component=?",
            (component,),
        ).fetchall()
        return {(ls, n): (s / a if a else 0.0) for ls, n, s, a in rows}

    # -------------------------------------------------------------- benchmarks
    def record_benchmark(self, component: str, metric: str, value: float, commit_sha: str) -> None:
        self._db.execute(
            "INSERT INTO benchmarks (component,metric,value,commit_sha,created_at) VALUES (?,?,?,?,?)",
            (component, metric, value, commit_sha, time.time()),
        )
        self._db.commit()

    def best_benchmark(self, component: str, metric: str) -> Optional[float]:
        row = self._db.execute(
            "SELECT MIN(value) FROM benchmarks WHERE component=? AND metric=?", (component, metric)
        ).fetchone()
        return row[0] if row and row[0] is not None else None

    # ------------------------------------------------------------ criteria map
    def set_criterion(self, criterion: str, milestone: str, evidence: str,
                      satisfied: bool, run_id: str) -> None:
        self._db.execute(
            "INSERT OR REPLACE INTO criteria_map VALUES (?,?,?,?,?,?)",
            (criterion, milestone, evidence, int(satisfied), run_id, time.time()),
        )
        self._db.commit()

    def unsatisfied_criteria(self, milestone: str, run_id: str) -> list[str]:
        rows = self._db.execute(
            "SELECT criterion FROM criteria_map WHERE milestone=? AND run_id=? AND satisfied=0",
            (milestone, run_id),
        ).fetchall()
        return [r[0] for r in rows]

    # --------------------------------------------------------------- artifacts
    def register_artifact(self, name: str, path: str, sha256: str,
                          milestone: str = "", commit_sha: str = "") -> dict:
        """Register a build-produced artifact. For 'evaluator': first registration pins the
        hash (frozen evaluator, §44); a later registration with a DIFFERENT hash is refused —
        re-pin is a human-only action (pin_evaluator)."""
        row = self._db.execute(
            "SELECT sha256, pinned_sha256 FROM artifacts WHERE name=?", (name,)).fetchone()
        if name == "evaluator" and row and row[1] and row[1] != sha256:
            return {"registered": False, "reason": "evaluator hash differs from frozen pin",
                    "pinned_sha256": row[1], "attempted_sha256": sha256}
        pinned = sha256 if name == "evaluator" else (row[1] if row else "")
        if name == "evaluator" and row and row[1]:
            pinned = row[1]  # keep existing pin
        self._db.execute(
            "INSERT OR REPLACE INTO artifacts VALUES (?,?,?,?,?,?,?)",
            (name, path, sha256, pinned, milestone, commit_sha, time.time()))
        self._db.commit()
        return {"registered": True, "pinned_sha256": pinned}

    def get_artifact(self, name: str) -> Optional[dict]:
        row = self._db.execute(
            "SELECT name,path,sha256,pinned_sha256,milestone,commit_sha,registered_at "
            "FROM artifacts WHERE name=?", (name,)).fetchone()
        if not row:
            return None
        cols = ["name", "path", "sha256", "pinned_sha256", "milestone", "commit_sha", "registered_at"]
        return dict(zip(cols, row))

    def pin_evaluator(self, sha256: str) -> None:
        """HUMAN-ONLY re-pin (invoked via supervise.py pin-evaluator, never via MCP)."""
        self._db.execute("UPDATE artifacts SET pinned_sha256=? WHERE name='evaluator'", (sha256,))
        self._db.commit()

    # ------------------------------------------------------- constitution amendments
    def evidence_ref_resolves(self, ref: str) -> bool:
        """True only if `ref` names a real record already in this store.

        Accepted forms: 'gate:<task_or_milestone>', 'experiment:<id>', 'artifact:<name>',
        'benchmark:<component>/<metric>', 'criterion:<n>'. Anything else — including a
        model's prose — does not resolve, and intake rejects the proposal.
        """
        if not ref or ":" not in ref:
            return False
        kind, _, val = ref.partition(":")
        kind, val = kind.strip().lower(), val.strip()
        if not val:
            return False
        try:
            if kind == "gate":
                r = self._db.execute(
                    "SELECT 1 FROM gate_results WHERE task_id=? LIMIT 1", (val,)).fetchone()
                return bool(r)
            if kind == "artifact":
                return self.get_artifact(val) is not None
            if kind == "benchmark":
                comp, _, metric = val.partition("/")
                q = "SELECT 1 FROM benchmarks WHERE component=?"
                args: tuple = (comp,)
                if metric:
                    q += " AND metric=?"
                    args = (comp, metric)
                return bool(self._db.execute(q + " LIMIT 1", args).fetchone())
            if kind == "criterion":
                return bool(self._db.execute(
                    "SELECT 1 FROM criteria_map WHERE criterion=? LIMIT 1", (val,)).fetchone())
            if kind == "experiment":
                # experiments land as tasks or gate rows depending on runner wiring
                r = self._db.execute(
                    "SELECT 1 FROM tasks WHERE task_id=? LIMIT 1", (val,)).fetchone()
                if r:
                    return True
                return bool(self._db.execute(
                    "SELECT 1 FROM gate_results WHERE task_id=? LIMIT 1", (val,)).fetchone())
        except Exception:  # noqa: BLE001  (a broken query must not admit a proposal)
            return False
        return False

    def propose_amendment(self, kind: str, title: str, rationale: str, evidence_ref: str,
                          proposed_by: str, target_hint: str = "") -> dict:
        """Intake. Enforces evidence resolution and dedup. Never applies anything."""
        import hashlib as _h
        if not self.evidence_ref_resolves(evidence_ref):
            return {"accepted": False,
                    "reason": f"evidence_ref '{evidence_ref}' does not resolve to a record in "
                              "the evidence store. A model claim is not evidence: cite "
                              "gate:<id>, experiment:<id>, artifact:<name>, "
                              "benchmark:<component>/<metric>, or criterion:<n>."}
        dedup = _h.sha256(f"{kind}|{title.strip().lower()}".encode("utf-8")).hexdigest()[:32]
        try:
            cur = self._db.execute(
                "INSERT INTO amendments (kind,title,rationale,evidence_ref,proposed_by,"
                "target_hint,diff_text,state,created_at,decided_at,decided_by,note,dedup_key) "
                "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
                (kind, title, rationale, evidence_ref, proposed_by, target_hint, "",
                 "proposed", time.time(), 0.0, "", "", dedup))
            self._db.commit()
            return {"accepted": True, "amendment_id": cur.lastrowid, "state": "proposed"}
        except Exception as e:  # noqa: BLE001 (unique index -> duplicate proposal)
            return {"accepted": False,
                    "reason": f"duplicate or invalid proposal (dedup guard): {e}"}

    def set_amendment_draft(self, amendment_id: int, diff_text: str, drafted_by: str) -> dict:
        r = self._db.execute("SELECT state FROM amendments WHERE id=?",
                             (amendment_id,)).fetchone()
        if not r:
            return {"ok": False, "reason": "no such amendment"}
        if r[0] not in ("proposed", "drafted"):
            return {"ok": False, "reason": f"cannot draft from state '{r[0]}'"}
        self._db.execute("UPDATE amendments SET diff_text=?, state='drafted', note=? WHERE id=?",
                         (diff_text, f"drafted_by={drafted_by}", amendment_id))
        self._db.commit()
        return {"ok": True, "state": "drafted"}

    def approve_amendment(self, amendment_id: int, human: str) -> dict:
        """HUMAN-ONLY approval (invoked via the CLI, never exposed as an MCP tool).

        Mirrors pin_evaluator: the capability simply does not exist in the model's surface,
        so no amount of model reasoning or prompt injection can reach it.
        """
        r = self._db.execute("SELECT state FROM amendments WHERE id=?",
                             (amendment_id,)).fetchone()
        if not r:
            return {"ok": False, "reason": "no such amendment"}
        if r[0] != "drafted":
            return {"ok": False, "reason": f"only a drafted amendment can be approved "
                                           f"(state is '{r[0]}')"}
        self._db.execute("UPDATE amendments SET state='approved', decided_at=?, decided_by=? "
                         "WHERE id=?", (time.time(), human, amendment_id))
        self._db.commit()
        return {"ok": True, "state": "approved"}

    def reject_amendment(self, amendment_id: int, human: str, why: str) -> dict:
        self._db.execute("UPDATE amendments SET state='rejected', decided_at=?, decided_by=?, "
                         "note=? WHERE id=?", (time.time(), human, why, amendment_id))
        self._db.commit()
        return {"ok": True, "state": "rejected"}

    def mark_amendment_applied(self, amendment_id: int, new_hash: str) -> None:
        self._db.execute("UPDATE amendments SET state='applied', note=? WHERE id=?",
                         (f"applied; new_sha256={new_hash}", amendment_id))
        self._db.commit()

    def list_amendments(self, state: str = "") -> list[dict]:
        q = ("SELECT id,kind,title,rationale,evidence_ref,proposed_by,target_hint,diff_text,"
             "state,created_at,decided_at,decided_by,note FROM amendments")
        args: tuple = ()
        if state:
            q += " WHERE state=?"
            args = (state,)
        q += " ORDER BY created_at ASC, id ASC"
        cols = ["id", "kind", "title", "rationale", "evidence_ref", "proposed_by", "target_hint",
                "diff_text", "state", "created_at", "decided_at", "decided_by", "note"]
        return [dict(zip(cols, row)) for row in self._db.execute(q, args).fetchall()]

    def get_amendment(self, amendment_id: int) -> Optional[dict]:
        rows = [a for a in self.list_amendments() if a["id"] == amendment_id]
        return rows[0] if rows else None

    # ---------------------------------------------------- manifest pin (HUMAN-ONLY set)
    def pin_manifest(self, declaration_sha: str, human: str) -> None:
        """Pin the deployment_host declaration hash. Set only via the CLI (pin-manifest),
        never exposed as an MCP tool — mirrors pin_evaluator exactly."""
        self._db.execute(
            "INSERT OR REPLACE INTO kv (k, v) VALUES ('manifest_pin', ?)",
            (json.dumps({"sha": declaration_sha, "by": human, "at": time.time()}),))
        self._db.commit()

    def get_pinned_manifest(self) -> str:
        row = self._db.execute("SELECT v FROM kv WHERE k='manifest_pin'").fetchone()
        if not row:
            return ""
        try:
            return json.loads(row[0]).get("sha", "")
        except Exception:  # noqa: BLE001
            return ""

    def journal_infra_fact(self, key: str, value: str, source: str, by: str) -> None:
        """Evidence-side journal of a manifest facts append (agent-writable lane)."""
        self._db.execute(
            "INSERT INTO escalations (run_id, milestone, task_id, domain, context, resolved) "
            "VALUES ('', 'infra', ?, 'infra_fact', ?, 1)",
            (key, json.dumps({"value": value[:500], "source": source, "by": by,
                               "at": time.time()})))
        self._db.commit()

    def close(self) -> None:
        self._db.close()
