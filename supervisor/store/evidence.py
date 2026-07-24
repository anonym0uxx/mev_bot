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
CREATE TABLE IF NOT EXISTS hypotheses (
    id                            TEXT PRIMARY KEY,   -- hypothesis_id from HYPOTHESIS_SCHEMA
    statement                     TEXT,
    causal_mechanism              TEXT,
    competing_explanations        TEXT,               -- json array of strings (§56.10)
    disconfirming_evidence_sought TEXT,
    expected_net_sol_impact       REAL,
    prior_probability             REAL,
    cost_to_test                  TEXT,
    edge_half_life                TEXT,
    inference_state               TEXT DEFAULT 'Hypothesis'
        CHECK (inference_state IN ('Observation','Hypothesis','ProvisionalInference',
                                   'ValidatedInference','RejectedInference',
                                   'ExpiredInference','RegimeSpecificInference')),
    created_run                   TEXT,
    updated_at                    REAL,
    labels                        TEXT DEFAULT '',    -- §45.2: e.g. 'BIAS_AUDIT_REQUIRED'
    -- §68/§111: the record this hypothesis was derived FROM. Empty for a model-proposed
    -- hypothesis; for a brain-grounded one it is a 'brain*:<tick>/<row key>' ref that
    -- `evidence_ref_resolves` can check against the brain tables below.
    evidence_ref                  TEXT DEFAULT ''
);
CREATE TABLE IF NOT EXISTS reconciled_outcomes (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id          TEXT,
    source_path     TEXT,          -- trade-JSONL artifact the row was ingested from
    digest          TEXT,          -- sha256 of the source artifact
    net_lamports    INTEGER,
    trades          INTEGER,
    fill_mode       TEXT,
    evidence_status TEXT,
    created_at      REAL
);
-- §45.1 ResearchKnowledgeBase seeding: every finding imported from prior repository
-- evidence preserves its full provenance and an explicit evidence-status label. Imported
-- markdown conclusions are NEVER presented as verified facts (§45.1) — `conclusion` is the
-- raw claim text and `status` is the honest reproduction label; nothing here is edge until
-- reproduced through the frozen-evaluator pipeline.
CREATE TABLE IF NOT EXISTS seeded_findings (
    id                       TEXT PRIMARY KEY,   -- stable finding key (source+claim slug)
    source_file              TEXT,               -- §45.1: source document
    finding_date             TEXT,               -- §45.1: date of the finding
    dataset                  TEXT,               -- §45.1: dataset / trade corpus
    sample_size              INTEGER,            -- §45.1: sample size (n)
    strategy_version         TEXT,               -- §45.1: strategy version
    cost_assumptions         TEXT,               -- §45.1: cost assumptions in force
    known_bias               TEXT,               -- §45.1: known bias
    known_missingness        TEXT,               -- §45.1: known missingness
    chain_reconciled         INTEGER,            -- §45.1: whether chain-reconciled (0/1)
    reproducible             TEXT,               -- §45.1: whether reproducible (yes/no/unknown)
    subsequently_contradicted INTEGER,           -- §45.1: whether later contradicted (0/1)
    status                   TEXT NOT NULL       -- §45.1 evidence-status enum (CHECK below)
        CHECK (status IN ('REPRODUCED','PARTIALLY_REPRODUCED','UNREPRODUCED',
                          'BIASED_SAMPLE','SUPERSEDED','FALSIFIED','UNKNOWN')),
    conclusion               TEXT,               -- raw imported claim; never a verified fact
    labels                   TEXT,               -- json array, e.g. ["HISTORICAL_CANDIDATE","BIAS_AUDIT_REQUIRED"]
    created_run              TEXT,
    imported_at              REAL
);
-- §56.5 RootCauseEngine — persisted classifications so the reflection report aggregates
-- DISTRIBUTIONS, not anecdotes. `evidence_ref` links each classification to its source record
-- (reconciled_outcome id, journal/exit row, or gate_result). §43 names this table.
CREATE TABLE IF NOT EXISTS root_cause_classifications (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id       TEXT,
    evidence_ref TEXT,          -- linked source record (e.g. 'reconciled:12', 'gate:<task>')
    root_cause   TEXT,          -- one of ROOT_CAUSE_CLASSES (§56.5)
    detail       TEXT,          -- json: the classifier's matched field + raw evidence echo
    created_at   REAL
);
-- ---------------------------------------------------------------------------------------
-- Brain evidence (`brain_analysis_v1`, emitted by the Rust engine). Persisting the artifact
-- makes every brain-grounded hypothesis AUDITABLE and REPRODUCIBLE: the hypothesis carries an
-- evidence_ref of the form 'brain_setup:<tick>/<sig>/<phase>' and that ref resolves here
-- (§68/§111 — a model claim is not evidence; a stored row is).
--
-- NULLABILITY IS LOAD-BEARING. A `confidence='unknown'` row is a REFUSAL: the engine declined
-- to estimate because evidence was thin. Its estimate columns are SQL NULL — never 0. Any
-- writer here that coerces a Python None to 0 destroys the system's core safety property, so
-- `ingest_brain_analysis` passes the loader's Optional[int] straight through, untouched.
CREATE TABLE IF NOT EXISTS brain_snapshots (
    run_id            TEXT,
    tick              INTEGER,
    info_time_ns      INTEGER,
    schema_version    INTEGER,
    episodes_total    INTEGER,
    episodes_admitted INTEGER,
    setup_classes_known   INTEGER,   -- counts of rows, not estimates: never null
    setup_classes_unknown INTEGER,
    lenses_known          INTEGER,
    lenses_unknown        INTEGER,
    source_path       TEXT,
    ingested_at       REAL,
    PRIMARY KEY (run_id, tick)
);
CREATE TABLE IF NOT EXISTS brain_setup_classes (
    run_id              TEXT,
    tick                INTEGER,
    signature           TEXT,        -- u128 as a decimal STRING (exact; never float-rounded)
    venue_phase         TEXT,
    meta_category       INTEGER,
    discovery_lane      TEXT,
    confidence          TEXT,        -- 'known' | 'unknown'
    unknown_reason      TEXT,        -- NULL when known
    n                   INTEGER,     -- NULL on a refusal
    median_net_lamports INTEGER,     -- NULL on a refusal
    mean_net_lamports   INTEGER,     -- NULL on a refusal
    win_rate_bp         INTEGER,     -- NULL on a refusal
    p25_net_lamports    INTEGER,     -- NULL on a refusal
    p75_net_lamports    INTEGER,     -- NULL on a refusal
    median_hold_ns      INTEGER,     -- NULL on a refusal
    nearest_distance    INTEGER,     -- NULL on a refusal
    PRIMARY KEY (run_id, tick, signature, venue_phase)
);
-- Meta saturation state at the snapshot tick. Persisted so a "reduce exposure to a decaying
-- meta" hypothesis has a row its evidence_ref ('brain_meta:<tick>/<category>') resolves to.
CREATE TABLE IF NOT EXISTS brain_meta_state (
    run_id                   TEXT,
    tick                     INTEGER,
    meta_category            INTEGER,
    phase                    TEXT,   -- emerging|hot|saturated|decaying|unknown
    n                        INTEGER,
    participation_decline_bp INTEGER,
    outcome_decline_bp       INTEGER,
    PRIMARY KEY (run_id, tick, meta_category)
);
-- Engine NOMINATIONS for retirement. A nomination is not a retirement (§56 sequential
-- retirement needs the §51 FDR/PBO and §52 baseline verdicts) — the row records the ask.
CREATE TABLE IF NOT EXISTS brain_retirement_flags (
    run_id               TEXT,
    tick                 INTEGER,
    subject              TEXT,       -- lane|archetype|setup_class|source
    key                  TEXT,
    reason               TEXT,
    n                    INTEGER,
    realized_net_lamports INTEGER,
    PRIMARY KEY (run_id, tick, subject, key)
);
CREATE TABLE IF NOT EXISTS brain_caller_trust (
    run_id      TEXT,
    tick        INTEGER,
    author_id   INTEGER,
    platform    TEXT,                -- NULL when the engine has no platform for the author
    tier        TEXT,                -- unproven|watch|trusted|demoted
    score_bp    INTEGER,             -- NULL when unproven (a refusal, not a zero score)
    n_markouts  INTEGER,             -- NULL when unproven
    exposure    TEXT,
    PRIMARY KEY (run_id, tick, author_id)
);
CREATE TABLE IF NOT EXISTS brain_follow_reco (
    run_id                  TEXT,
    tick                    INTEGER,
    author_id               INTEGER,
    direction               TEXT,    -- 'follow' | 'unfollow'
    platform                TEXT,
    n_calls                 INTEGER,
    realized_net_attributed INTEGER,
    median_lead_ns          INTEGER, -- NULL for an unfollow row: the artifact does not carry it
    trust_tier              TEXT,    -- NULL for an unfollow row: likewise not carried
    PRIMARY KEY (run_id, tick, author_id, direction)
);
"""

# Brain-evidence tables, in ingest order. Named once so the migration, the ingest, and the
# per-(run_id, tick) replace-before-insert all agree on the set.
BRAIN_TABLES: tuple[str, ...] = (
    "brain_snapshots", "brain_setup_classes", "brain_meta_state",
    "brain_retirement_flags", "brain_caller_trust", "brain_follow_reco",
)

# Constitution §56.10 inference ladder — the only states a hypothesis may occupy.
VALID_INFERENCE_STATES: tuple[str, ...] = (
    "Observation", "Hypothesis", "ProvisionalInference", "ValidatedInference",
    "RejectedInference", "ExpiredInference", "RegimeSpecificInference",
)

# Constitution §45.1 evidence-status enum — the ONLY reproduction labels a seeded knowledge-base
# finding may carry. Quoted verbatim from the one-shot prompt (§45.1):
#   "status ∈ {REPRODUCED, PARTIALLY_REPRODUCED, UNREPRODUCED, BIASED_SAMPLE, SUPERSEDED,
#              FALSIFIED, UNKNOWN}"
# Imported markdown conclusions are never presented as verified facts (§45.1): a finding is edge
# only once REPRODUCED through the frozen-evaluator pipeline (§44/§56).
SEEDED_FINDING_STATES: tuple[str, ...] = (
    "REPRODUCED", "PARTIALLY_REPRODUCED", "UNREPRODUCED",
    "BIASED_SAMPLE", "SUPERSEDED", "FALSIFIED", "UNKNOWN",
)

# §45.2 mandates that until the enrichment-selection bias audit clears, every graduation-cohort
# claim carries this label. It is stamped on the first registered KB experiment's hypothesis row.
BIAS_AUDIT_LABEL: str = "BIAS_AUDIT_REQUIRED"


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
        self._migrate()
        self._db.commit()

    def _migrate(self) -> None:
        """Additive, idempotent migrations for stores created by an earlier schema.

        `CREATE TABLE IF NOT EXISTS` never adds columns to a table that already exists, so any
        column added after a store was first created must be back-filled here. Only additive
        `ADD COLUMN` with a default is used — no destructive change, no data loss — consistent
        with the evidence store's append-only, reproducible-audit design.
        """
        def _cols(table: str) -> set[str]:
            return {r[1] for r in self._db.execute(f"PRAGMA table_info({table})").fetchall()}
        tables = {r[0] for r in self._db.execute(
            "SELECT name FROM sqlite_master WHERE type='table'").fetchall()}
        # §45.2: hypotheses.labels carries BIAS_AUDIT_REQUIRED on the first KB experiment.
        if "hypotheses" in tables:
            if "labels" not in _cols("hypotheses"):
                self._db.execute("ALTER TABLE hypotheses ADD COLUMN labels TEXT DEFAULT ''")
            # §68/§111: brain-grounded hypotheses cite the artifact row they came from. Added
            # after `labels` so the column order matches a freshly-created SCHEMA table exactly
            # (record_hypothesis inserts positionally).
            if "evidence_ref" not in _cols("hypotheses"):
                self._db.execute(
                    "ALTER TABLE hypotheses ADD COLUMN evidence_ref TEXT DEFAULT ''")

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
        'benchmark:<component>/<metric>', 'criterion:<n>', and the brain-evidence family
        (§68/§111) written by `ingest_brain_analysis`:

          brain:<tick>                              -> the snapshot itself
          brain_setup:<tick>/<signature>/<phase>    -> one setup-class row
          brain_meta:<tick>/<meta_category>         -> one meta-state row
          brain_retire:<tick>/<subject>/<key>       -> one retirement NOMINATION row
          brain_caller:<tick>/<author_id>           -> one caller trust / follow-reco row

        Anything else — including a model's prose — does not resolve, and intake rejects the
        proposal.
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
            if kind.startswith("brain"):
                return self._brain_ref_resolves(kind, val)
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

    # ------------------------------------------- hypotheses (durable memory, §43/§56.10)
    def record_hypothesis(self, hyp: dict[str, Any], created_run: str = "") -> dict:
        """Persist a generated hypothesis. `hyp` uses HYPOTHESIS_SCHEMA field names
        (hypothesis_id, statement, causal_mechanism, competing_explanations,
        disconfirming_evidence_sought, expected_net_sol_impact, prior_probability,
        cost_to_test, edge_half_life) plus optional inference_state."""
        state = hyp.get("inference_state", "Hypothesis")
        if state not in VALID_INFERENCE_STATES:
            raise ValueError(f"invalid inference_state {state!r}; "
                             f"must be one of {VALID_INFERENCE_STATES} (§56.10)")
        hid = hyp.get("hypothesis_id") or hyp.get("id")
        if not hid:
            raise ValueError("hypothesis requires 'hypothesis_id'")
        comp = hyp.get("competing_explanations", [])
        if not isinstance(comp, list):
            raise ValueError("competing_explanations must be a list (§56.10)")
        labels = hyp.get("labels", "")
        if isinstance(labels, (list, tuple)):
            labels = ",".join(str(x) for x in labels)
        self._db.execute(
            "INSERT OR REPLACE INTO hypotheses VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (hid, hyp.get("statement", ""), hyp.get("causal_mechanism", ""),
             json.dumps(comp), hyp.get("disconfirming_evidence_sought", ""),
             float(hyp.get("expected_net_sol_impact", 0.0)),
             float(hyp.get("prior_probability", 0.0)),
             hyp.get("cost_to_test", "unknown"), hyp.get("edge_half_life", "unknown"),
             state, created_run, time.time(), str(labels),
             str(hyp.get("evidence_ref", ""))))
        self._db.commit()
        return {"recorded": True, "id": hid, "inference_state": state, "labels": str(labels),
                "evidence_ref": str(hyp.get("evidence_ref", ""))}

    def set_inference_state(self, hypothesis_id: str, state: str) -> dict:
        """Move a hypothesis along the §56.10 inference ladder. Validated; never silent."""
        if state not in VALID_INFERENCE_STATES:
            raise ValueError(f"invalid inference_state {state!r}; "
                             f"must be one of {VALID_INFERENCE_STATES} (§56.10)")
        cur = self._db.execute(
            "UPDATE hypotheses SET inference_state=?, updated_at=? WHERE id=?",
            (state, time.time(), hypothesis_id))
        self._db.commit()
        if cur.rowcount == 0:
            return {"ok": False, "reason": f"no hypothesis with id '{hypothesis_id}'"}
        return {"ok": True, "id": hypothesis_id, "inference_state": state}

    def get_hypothesis(self, hypothesis_id: str) -> Optional[dict]:
        row = self._db.execute(
            "SELECT id,statement,causal_mechanism,competing_explanations,"
            "disconfirming_evidence_sought,expected_net_sol_impact,prior_probability,"
            "cost_to_test,edge_half_life,inference_state,created_run,updated_at,labels,"
            "evidence_ref FROM hypotheses WHERE id=?", (hypothesis_id,)).fetchone()
        if not row:
            return None
        cols = ["id", "statement", "causal_mechanism", "competing_explanations",
                "disconfirming_evidence_sought", "expected_net_sol_impact",
                "prior_probability", "cost_to_test", "edge_half_life",
                "inference_state", "created_run", "updated_at", "labels", "evidence_ref"]
        d = dict(zip(cols, row))
        try:
            d["competing_explanations"] = json.loads(d["competing_explanations"] or "[]")
        except json.JSONDecodeError:
            pass  # return raw text rather than losing the record
        return d

    def list_hypotheses(self, inference_state: str = "", limit: int = 500) -> list[dict]:
        q = ("SELECT id,statement,causal_mechanism,competing_explanations,"
             "disconfirming_evidence_sought,expected_net_sol_impact,prior_probability,"
             "cost_to_test,edge_half_life,inference_state,created_run,updated_at,labels "
             "FROM hypotheses")
        args: tuple = ()
        if inference_state:
            if inference_state not in VALID_INFERENCE_STATES:
                raise ValueError(f"invalid inference_state filter {inference_state!r}")
            q += " WHERE inference_state=?"
            args = (inference_state,)
        q += " ORDER BY updated_at DESC LIMIT ?"
        args = args + (limit,)
        cols = ["id", "statement", "causal_mechanism", "competing_explanations",
                "disconfirming_evidence_sought", "expected_net_sol_impact",
                "prior_probability", "cost_to_test", "edge_half_life",
                "inference_state", "created_run", "updated_at", "labels"]
        out = []
        for row in self._db.execute(q, args).fetchall():
            d = dict(zip(cols, row))
            try:
                d["competing_explanations"] = json.loads(d["competing_explanations"] or "[]")
            except json.JSONDecodeError:
                pass
            out.append(d)
        return out

    # -------------------------------------- reconciled outcomes (durable memory, §43)
    def record_reconciled_outcome(self, run_id: str, source_path: str, digest: str,
                                  net_lamports: int, trades: int, fill_mode: str,
                                  evidence_status: str) -> int:
        cur = self._db.execute(
            "INSERT INTO reconciled_outcomes (run_id,source_path,digest,net_lamports,"
            "trades,fill_mode,evidence_status,created_at) VALUES (?,?,?,?,?,?,?,?)",
            (run_id, source_path, digest, int(net_lamports), int(trades),
             fill_mode, evidence_status, time.time()))
        self._db.commit()
        return cur.lastrowid

    def list_reconciled_outcomes(self, run_id: str = "", limit: int = 500) -> list[dict]:
        q = ("SELECT id,run_id,source_path,digest,net_lamports,trades,fill_mode,"
             "evidence_status,created_at FROM reconciled_outcomes")
        args: tuple = ()
        if run_id:
            q += " WHERE run_id=?"
            args = (run_id,)
        q += " ORDER BY created_at DESC LIMIT ?"
        args = args + (limit,)
        cols = ["id", "run_id", "source_path", "digest", "net_lamports", "trades",
                "fill_mode", "evidence_status", "created_at"]
        return [dict(zip(cols, r)) for r in self._db.execute(q, args).fetchall()]

    def journal_infra_fact(self, key: str, value: str, source: str, by: str) -> None:
        """Evidence-side journal of a manifest facts append (agent-writable lane)."""
        # repair: previously inserted into a nonexistent `resolved` column (schema has
        # `resolved_at`), so every call raised sqlite3.OperationalError — silently
        # swallowed by the MCP server, losing the journal entry. Rows are stamped
        # created_at/resolved_at=now so they never appear as open escalations
        # (open_escalations filters on resolved_at IS NULL).
        now = time.time()
        self._db.execute(
            "INSERT INTO escalations (run_id, milestone, task_id, domain, context, "
            "created_at, resolved_at) VALUES ('', 'infra', ?, 'infra_fact', ?, ?, ?)",
            (key, json.dumps({"value": value[:500], "source": source, "by": by,
                              "at": now}), now, now))
        self._db.commit()

    # ------------------------------------------- seeded findings (§45.1 KB seeding)
    def record_seeded_finding(self, finding: dict[str, Any], created_run: str = "") -> dict:
        """Persist one prior-evidence finding into the ResearchKnowledgeBase (§45.1).

        `finding` preserves the §45.1 provenance fields (source_file, date, dataset,
        sample_size, strategy_version, cost_assumptions, known_bias, known_missingness,
        chain_reconciled, reproducible, subsequently_contradicted) plus a `status` in
        SEEDED_FINDING_STATES and the raw `conclusion` text. The conclusion is stored as a
        claim, never as verified fact — reproduction is proven only through the pipeline.
        """
        fid = finding.get("id")
        if not fid:
            raise ValueError("seeded finding requires a stable 'id'")
        status = finding.get("status", "UNKNOWN")
        if status not in SEEDED_FINDING_STATES:
            raise ValueError(f"invalid seeded-finding status {status!r}; "
                             f"must be one of {SEEDED_FINDING_STATES} (§45.1)")
        labels = finding.get("labels", [])
        if not isinstance(labels, list):
            labels = [str(labels)]
        self._db.execute(
            "INSERT OR REPLACE INTO seeded_findings VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (fid, finding.get("source_file", ""), finding.get("date", finding.get("finding_date", "")),
             finding.get("dataset", ""),
             int(finding.get("sample_size", 0) or 0), finding.get("strategy_version", ""),
             finding.get("cost_assumptions", ""), finding.get("known_bias", ""),
             finding.get("known_missingness", ""),
             1 if finding.get("chain_reconciled") else 0,
             str(finding.get("reproducible", "unknown")),
             1 if finding.get("subsequently_contradicted") else 0,
             status, finding.get("conclusion", ""), json.dumps(labels),
             created_run, time.time()))
        self._db.commit()
        return {"recorded": True, "id": fid, "status": status, "labels": labels}

    def set_finding_status(self, finding_id: str, status: str) -> dict:
        """Transition a seeded finding's reproduction status (§45.1). Validated; never silent.

        The legitimate lifecycle is UNREPRODUCED/UNKNOWN → {REPRODUCED, PARTIALLY_REPRODUCED,
        FALSIFIED, BIASED_SAMPLE, SUPERSEDED} as the audit runs; any target must be a member of
        the §45.1 enum.
        """
        if status not in SEEDED_FINDING_STATES:
            raise ValueError(f"invalid seeded-finding status {status!r}; "
                             f"must be one of {SEEDED_FINDING_STATES} (§45.1)")
        cur = self._db.execute(
            "UPDATE seeded_findings SET status=? WHERE id=?", (status, finding_id))
        self._db.commit()
        if cur.rowcount == 0:
            return {"ok": False, "reason": f"no seeded finding with id '{finding_id}'"}
        return {"ok": True, "id": finding_id, "status": status}

    def get_seeded_finding(self, finding_id: str) -> Optional[dict]:
        row = self._db.execute(
            "SELECT id,source_file,finding_date,dataset,sample_size,strategy_version,"
            "cost_assumptions,known_bias,known_missingness,chain_reconciled,reproducible,"
            "subsequently_contradicted,status,conclusion,labels,created_run,imported_at "
            "FROM seeded_findings WHERE id=?", (finding_id,)).fetchone()
        return self._finding_row(row) if row else None

    def list_seeded_findings(self, status: str = "", limit: int = 500) -> list[dict]:
        q = ("SELECT id,source_file,finding_date,dataset,sample_size,strategy_version,"
             "cost_assumptions,known_bias,known_missingness,chain_reconciled,reproducible,"
             "subsequently_contradicted,status,conclusion,labels,created_run,imported_at "
             "FROM seeded_findings")
        args: tuple = ()
        if status:
            if status not in SEEDED_FINDING_STATES:
                raise ValueError(f"invalid seeded-finding status filter {status!r}")
            q += " WHERE status=?"
            args = (status,)
        q += " ORDER BY imported_at ASC, id ASC LIMIT ?"
        args = args + (limit,)
        return [self._finding_row(r) for r in self._db.execute(q, args).fetchall()]

    @staticmethod
    def _finding_row(row: tuple) -> dict:
        cols = ["id", "source_file", "finding_date", "dataset", "sample_size",
                "strategy_version", "cost_assumptions", "known_bias", "known_missingness",
                "chain_reconciled", "reproducible", "subsequently_contradicted", "status",
                "conclusion", "labels", "created_run", "imported_at"]
        d = dict(zip(cols, row))
        d["chain_reconciled"] = bool(d["chain_reconciled"])
        d["subsequently_contradicted"] = bool(d["subsequently_contradicted"])
        try:
            d["labels"] = json.loads(d["labels"] or "[]")
        except json.JSONDecodeError:
            d["labels"] = []
        return d

    # ---------------------------------------- root-cause classifications (§56.5)
    def record_root_cause(self, run_id: str, evidence_ref: str, root_cause: str,
                          detail: dict[str, Any] | None = None) -> int:
        """Persist one §56.5 root-cause classification linked to its source record.

        Aggregated later into DISTRIBUTIONS (never anecdotes) for the reflection report.
        Validation of `root_cause` against ROOT_CAUSE_CLASSES lives in the classifier
        (supervisor/analysis/root_cause.py) to keep the store free of the taxonomy import.
        """
        cur = self._db.execute(
            "INSERT INTO root_cause_classifications (run_id,evidence_ref,root_cause,detail,"
            "created_at) VALUES (?,?,?,?,?)",
            (run_id, evidence_ref, root_cause, json.dumps(detail or {}), time.time()))
        self._db.commit()
        return cur.lastrowid

    def list_root_causes(self, run_id: str = "", limit: int = 2000) -> list[dict]:
        q = ("SELECT id,run_id,evidence_ref,root_cause,detail,created_at "
             "FROM root_cause_classifications")
        args: tuple = ()
        if run_id:
            q += " WHERE run_id=?"
            args = (run_id,)
        q += " ORDER BY created_at ASC, id ASC LIMIT ?"
        args = args + (limit,)
        cols = ["id", "run_id", "evidence_ref", "root_cause", "detail", "created_at"]
        out = []
        for r in self._db.execute(q, args).fetchall():
            d = dict(zip(cols, r))
            try:
                d["detail"] = json.loads(d["detail"] or "{}")
            except json.JSONDecodeError:
                pass
            out.append(d)
        return out

    # ------------------------------------------------ brain evidence (`brain_analysis_v1`)
    def _brain_ref_resolves(self, kind: str, val: str) -> bool:
        """Resolve one 'brain*:' evidence_ref against the persisted artifact rows.

        Existence-only, unscoped by run — the same idiom as the 'gate:'/'artifact:' kinds
        above. A ref whose tick was never ingested does not resolve, so a hypothesis quoting
        a brain row the store has never seen cannot pass amendment intake.
        """
        parts = val.split("/", 2)
        try:
            tick = int(parts[0], 10)
        except ValueError:
            return False
        if kind == "brain":
            return bool(self._db.execute(
                "SELECT 1 FROM brain_snapshots WHERE tick=? LIMIT 1", (tick,)).fetchone())
        if kind == "brain_setup" and len(parts) == 3:
            return bool(self._db.execute(
                "SELECT 1 FROM brain_setup_classes WHERE tick=? AND signature=? AND "
                "venue_phase=? LIMIT 1", (tick, parts[1], parts[2])).fetchone())
        if kind == "brain_meta" and len(parts) == 2:
            try:
                cat = int(parts[1], 10)
            except ValueError:
                return False
            return bool(self._db.execute(
                "SELECT 1 FROM brain_meta_state WHERE tick=? AND meta_category=? LIMIT 1",
                (tick, cat)).fetchone())
        if kind == "brain_retire" and len(parts) == 3:
            return bool(self._db.execute(
                "SELECT 1 FROM brain_retirement_flags WHERE tick=? AND subject=? AND key=? "
                "LIMIT 1", (tick, parts[1], parts[2])).fetchone())
        if kind == "brain_caller" and len(parts) == 2:
            try:
                author = int(parts[1], 10)
            except ValueError:
                return False
            row = self._db.execute(
                "SELECT 1 FROM brain_caller_trust WHERE tick=? AND author_id=? LIMIT 1",
                (tick, author)).fetchone()
            if row:
                return True
            return bool(self._db.execute(
                "SELECT 1 FROM brain_follow_reco WHERE tick=? AND author_id=? LIMIT 1",
                (tick, author)).fetchone())
        return False

    def ingest_brain_analysis(self, run_id: str, analysis: Any) -> int:
        """Persist one parsed `brain_analysis_v1` artifact. Returns the number of rows written.

        `analysis` is a `supervisor.store.brain_analysis.BrainAnalysis`. It is taken as an
        object rather than a dict so the refusal semantics survive the trip: an estimate that
        the engine refused to make arrives here as `None` and is written as SQL NULL.

        NOTHING in this method may substitute a value for a None. There is no `or 0`, no
        `int(x or 0)`, no COALESCE. A refusal is stored as a refusal, or the whole evidence
        chain becomes a fiction.

        Idempotent per (run_id, tick): the tick's rows are cleared and rewritten, so
        re-ingesting the same artifact leaves exactly the same table contents (and a shrunken
        re-emission cannot leave stale rows behind).
        """
        tick = int(analysis.tick)
        for table in BRAIN_TABLES:
            self._db.execute(f"DELETE FROM {table} WHERE run_id=? AND tick=?", (run_id, tick))

        known_c = len(analysis.known_setup_classes())
        known_l = len(analysis.known_lenses())
        self._db.execute(
            "INSERT INTO brain_snapshots VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
            (run_id, tick, int(analysis.info_time_ns), int(analysis.schema_version),
             int(analysis.episodes_total), int(analysis.episodes_admitted),
             known_c, len(analysis.setup_classes) - known_c,
             known_l, len(analysis.lens_scoreboard) - known_l,
             str(analysis.source_path), time.time()))
        rows = 1

        for c in analysis.setup_classes:
            self._db.execute(
                "INSERT INTO brain_setup_classes VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                (run_id, tick, str(c.signature), c.venue_phase, c.meta_category,
                 c.discovery_lane, c.confidence, c.unknown_reason,
                 c.n, c.median_net_lamports, c.mean_net_lamports, c.win_rate_bp,
                 c.p25_net_lamports, c.p75_net_lamports, c.median_hold_ns, c.nearest_distance))
            rows += 1
        for m in analysis.meta_state:
            self._db.execute(
                "INSERT INTO brain_meta_state VALUES (?,?,?,?,?,?,?)",
                (run_id, tick, m.meta_category, m.phase, m.n,
                 m.participation_decline_bp, m.outcome_decline_bp))
            rows += 1
        for f in analysis.retirement_flags:
            self._db.execute(
                "INSERT INTO brain_retirement_flags VALUES (?,?,?,?,?,?,?)",
                (run_id, tick, f.subject, f.key, f.reason, f.n, f.realized_net_lamports))
            rows += 1
        for t in analysis.caller_trust:
            self._db.execute(
                "INSERT INTO brain_caller_trust VALUES (?,?,?,?,?,?,?,?)",
                (run_id, tick, t.author_id, t.platform, t.tier,
                 t.score_bp, t.n_markouts, t.exposure))
            rows += 1
        for fr in analysis.follow_recommendations:
            self._db.execute(
                "INSERT OR REPLACE INTO brain_follow_reco VALUES (?,?,?,?,?,?,?,?,?)",
                (run_id, tick, fr.author_id, "follow", fr.platform, fr.n_calls,
                 fr.realized_net_attributed, fr.median_lead_ns, fr.trust_tier))
            rows += 1
        for u in analysis.unfollow_candidates:
            # median_lead_ns / trust_tier are NOT carried on an unfollow row by the artifact.
            # They are written NULL — "the artifact did not say" — never 0 and never ''.
            self._db.execute(
                "INSERT OR REPLACE INTO brain_follow_reco VALUES (?,?,?,?,?,?,?,?,?)",
                (run_id, tick, u.author_id, "unfollow", u.platform, u.n_calls,
                 u.realized_net_attributed, None, None))
            rows += 1
        self._db.commit()
        return rows

    def latest_brain_snapshot(self, run_id: str = "") -> Optional[dict]:
        q = ("SELECT run_id,tick,info_time_ns,schema_version,episodes_total,episodes_admitted,"
             "setup_classes_known,setup_classes_unknown,lenses_known,lenses_unknown,"
             "source_path,ingested_at FROM brain_snapshots")
        args: tuple = ()
        if run_id:
            q += " WHERE run_id=?"
            args = (run_id,)
        q += " ORDER BY tick DESC LIMIT 1"
        row = self._db.execute(q, args).fetchone()
        if not row:
            return None
        cols = ["run_id", "tick", "info_time_ns", "schema_version", "episodes_total",
                "episodes_admitted", "setup_classes_known", "setup_classes_unknown",
                "lenses_known", "lenses_unknown", "source_path", "ingested_at"]
        return dict(zip(cols, row))

    # Column order per brain table, for the generic reader below.
    _BRAIN_COLS: dict[str, list[str]] = {
        "brain_setup_classes": [
            "run_id", "tick", "signature", "venue_phase", "meta_category", "discovery_lane",
            "confidence", "unknown_reason", "n", "median_net_lamports", "mean_net_lamports",
            "win_rate_bp", "p25_net_lamports", "p75_net_lamports", "median_hold_ns",
            "nearest_distance"],
        "brain_meta_state": [
            "run_id", "tick", "meta_category", "phase", "n", "participation_decline_bp",
            "outcome_decline_bp"],
        "brain_retirement_flags": [
            "run_id", "tick", "subject", "key", "reason", "n", "realized_net_lamports"],
        "brain_caller_trust": [
            "run_id", "tick", "author_id", "platform", "tier", "score_bp", "n_markouts",
            "exposure"],
        "brain_follow_reco": [
            "run_id", "tick", "author_id", "direction", "platform", "n_calls",
            "realized_net_attributed", "median_lead_ns", "trust_tier"],
    }

    def list_brain_rows(self, table: str, run_id: str = "",
                        tick: Optional[int] = None, limit: int = 2000) -> list[dict]:
        """Read back persisted brain rows. NULLs come back as Python None, unchanged."""
        cols = self._BRAIN_COLS.get(table)
        if cols is None:
            raise ValueError(f"unknown brain table {table!r}; have {sorted(self._BRAIN_COLS)}")
        q = f"SELECT {','.join(cols)} FROM {table}"
        clauses: list[str] = []
        args: list[Any] = []
        if run_id:
            clauses.append("run_id=?")
            args.append(run_id)
        if tick is not None:
            clauses.append("tick=?")
            args.append(int(tick))
        if clauses:
            q += " WHERE " + " AND ".join(clauses)
        q += f" ORDER BY {cols[1]} ASC, {cols[2]} ASC LIMIT ?"
        args.append(limit)
        return [dict(zip(cols, r)) for r in self._db.execute(q, tuple(args)).fetchall()]

    def close(self) -> None:
        self._db.close()
