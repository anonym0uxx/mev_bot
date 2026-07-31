# Erratum and Corrected 18-Row Criteria Table

**Date:** 2026-07-31
**Author:** Hermes Agent (CONDUCTOR, constitution §69 Surface 2)
**Supersedes:** Task 4 table in `docs/TASK4_CI_MILESTONE_CONTRADICTION_2026-07-31.md` (commit 537575f)
**Base commit:** 54130681bd4a

## Errata

### 1. The Task 4 table invented criteria that do not exist in CRITERION_BINDINGS

My Task 4 report listed criteria 100, 101, 104, 105, 106, 107, 108 as if
they were in the authoritative binding table. They are not. The
authoritative table (`supervisor/gates/runner.py:92-116`) contains exactly
18 criteria: **52, 69, 81, 85, 96, 97, 98, 99, 102, 103, 109, 110, 111,
112, 113, 114, 115, 116.**

Criteria 100-108 do exist in the constitution's §63 acceptance-criteria
list (the constitution declares many more than 18). But the supervisor's
CRITERION_BINDINGS table — the authority for certification — maps only the
18 that the milestone gate evaluates. Criterion 100 (scalp time-stops) is
a valid constitutional requirement; it simply has no binding in the table
and was not among the 18 the prior audit enumerated.

I cannot explain how I produced criteria 101, 104, 105, 106, 107, 108 —
they appear nowhere in the codebase's binding table, the prior audit, or
the constitution's §63 list as standalone criteria. They were fabricated.

### 2. The Task 4 table claimed 11 MECHANICAL-PASS; the real count is 4

The binding table has exactly 4 MECHANICAL bindings:
- 109 → hotpath_lint (PARTIAL — Phase-A clauses only; deploy clauses need Phase-B)
- 114 → build
- 115 → test (dossier_narrative_nv_*)
- 116 → test (dossier_rank_wr_*)

The other 14 are all UNVERIFIED. My Task 4 report inflated this to 11
MECHANICAL-PASS by inventing criteria (101, 104, 105, 106, 107, 108) and
binding them to checks. This repeated the exact defect the operator
warned against: inventing mechanical checks for properties that are not
code properties.

### 3. Retraction: "rows 6-12: all PASS" (from the Task 3 report)

Row 6 (fmt) did NOT pass. It did not even run — the cargo fmt command
failed with OS error 206 (path too long) before rustfmt executed. The
output was cargo's usage/help text. "The tree is unformatted" and "the
check could not be invoked" are different facts. Row 6's correct status
was: **DID NOT RUN (harness defect), not PASS, not FAIL.**

This was corrected in Task C (this session) by wiring the per-crate
fallback. See Task C below.

### 4. No ARTIFACT or OPERATOR bindings appear in the table

The operator is correct. The CRITERION_BINDINGS table contains only
MECHANICAL and UNVERIFIED entries. No ARTIFACT or OPERATOR binding has
been authored. This is a gap — several criteria (52, 85, 96, 97, 98, 112)
could honestly be bound to ARTIFACT or OPERATOR bindings, but none have
been. The table collapsed to mechanical-or-nothing, which is the defect
the operator identified.

## The authoritative criterion set

**Source:** `supervisor/gates/runner.py:92-116` — `CRITERION_BINDINGS` dict.
This is the ONLY authority for criterion satisfaction in the supervisor
(per the docstring at lines 44-55).

**Constitution source:** §63 acceptance-criteria list in
`docs/HERMES_ONE_SHOT_PROMPT.md`. The constitution declares 60+ criteria
(1-60 plus later additions). The supervisor maps a SUBSET of these — the
18 that the milestone gate evaluates — into CRITERION_BINDINGS.

**Prior audit source:** `docs/GATE_INTEGRITY_AUDIT_2026-07-31.md:143-164`
(commit 5413068). This enumerated the same 18 criteria and is consistent
with the binding table.

**Reconciliation of criterion 100:** Criterion 100 (scalp time-stops,
hazard-estimated) exists in the constitution (§63, line 1599 area) but is
NOT in CRITERION_BINDINGS. It was not in the prior audit's 18-row table.
Its appearance in my Task 4 UNVERIFIED list was an error — I added a
criterion that the binding table does not track. The correct count is 18,
not 19.

## Corrected 18-row table

Each criterion in exactly one bucket. Binding type from the code.

| # | Criterion | Binding type | Binds to | Verdict |
|---|---|---|---|---|
| 1 | 52 | UNVERIFIED | (no binding) | BLOCKS — key-custody election is an operator process, not a code property. No OPERATOR attestation recorded. |
| 2 | 69 | UNVERIFIED | (no binding) | BLOCKS — native Windows / no WSL dependency. Requires artifact study. No ARTIFACT pinned. |
| 3 | 81 | UNVERIFIED | (no binding) | BLOCKS — taxonomy forward-only. Requires artifact study. |
| 4 | 85 | UNVERIFIED | (no binding) | BLOCKS — capital allocation is a policy decision, not a code property. |
| 5 | 96 | UNVERIFIED | (no binding) | BLOCKS — signal-horizon matching law. Requires artifact study. |
| 6 | 97 | UNVERIFIED | (no binding) | BLOCKS — scalp-readiness. Requires artifact study. |
| 7 | 98 | UNVERIFIED | (no binding) | BLOCKS — no-edge rescoping. Requires artifact study. |
| 8 | 99 | UNVERIFIED | (no binding) | BLOCKS — memory soak. check_memory_soak exists but soak_bin not built. |
| 9 | 102 | UNVERIFIED | (no binding) | BLOCKS — safety constants static. Requires artifact study. |
| 10 | 103 | UNVERIFIED | (no binding) | BLOCKS — latency budgets. check_bench exists but bench_name empty (Shape 3 fail-closed). |
| 11 | 109 | MECHANICAL | `check_hotpath_lint` | PARTIAL — Phase-A code bans enforced (no async/await, no money float casts, no Linux-isms, no /tmp). Deploy clauses (pinned profile, PGO, unsafe dossiers, pre-warmed connections, nightly accelerators, p50/p99 budgets) require Phase-B. |
| 12 | 110 | UNVERIFIED | (no binding) | BLOCKS — attention spend source is a policy decision, not a code property. |
| 13 | 111 | UNVERIFIED | (no binding) | BLOCKS — amendment subsystem. Requires artifact study. |
| 14 | 112 | UNVERIFIED | (no binding) | BLOCKS — size-viability band. Requires artifact study. |
| 15 | 113 | UNVERIFIED | (no binding) | BLOCKS — two-phase build boundary. check_phase_provenance exists but is inside the bench block (bench_name='' → skipped). |
| 16 | 114 | MECHANICAL | `check_build` | PASS — cargo build compiles all 26 workspace members. |
| 17 | 115 | MECHANICAL | `check_tests` (dossier_narrative_nv_*) | PASS — required_tests list populated and verified present. |
| 18 | 116 | MECHANICAL | `check_tests` (dossier_rank_wr_*) | PASS — required_tests list populated and verified present. |

**Totals:**

| Bucket | Count | Criteria |
|---|---|---|
| MECHANICAL-PASS | 3 | 114, 115, 116 |
| MECHANICAL-PARTIAL | 1 | 109 |
| UNVERIFIED (blocks) | 14 | 52, 69, 81, 85, 96, 97, 98, 99, 102, 103, 110, 111, 112, 113 |
| **Total** | **18** | |

No ARTIFACT or OPERATOR bindings exist in the table. The 14 UNVERIFIED
criteria include properties that are genuinely not code properties
(52 key-custody, 85 capital allocation, 110 attention spend) and
properties that require artifact studies (69, 81, 96, 97, 98, 102, 111,
112) or Phase-B hardware (99, 103, 113).

## Per-criterion analysis: 52, 85, 96, 97, 98, 112

The operator asked: for each of these, which check did I bind it to, and
is that check's pass condition causally connected to the criterion?

**Answer: I bound NONE of them to any check.** All six are UNVERIFIED in
the binding table. Here is why each is correct:

### 52 — Key custody (key-custody election, operator process)
**Binding: UNVERIFIED. Correct.** This is an operator-process property:
trading keys are non-exportable to the agent and all signing flows
through the policy-enforcing signing boundary. No code check can verify
key custody — it is a human-process attestation. The honest binding
would be OPERATOR (operator attests that keys are non-exportable and
signing flows through the boundary). No OPERATOR binding has been
recorded. UNVERIFIED is correct until one is.

### 85 — Capital allocation (policy, not code)
**Binding: UNVERIFIED. Correct.** The CapitalAllocator's capital-
allocation envelope is a policy decision governed by registered
envelopes and reconciled-bankroll thresholds. No mechanical check
verifies capital-allocation policy correctness — it is an economics
governance property. The honest binding would be ARTIFACT (a study
document showing the allocation envelope is within registered bounds)
or OPERATOR (operator attests the envelope). Neither exists.

### 96 — Signal-horizon matching (research/economics, not code)
**Binding: UNVERIFIED. Correct.** The signal-horizon matching law
requires that every feature's measured latency is recorded and
mechanically enforced against its decision horizon. This is partly
code (latency measurement exists) and partly research (the horizon
classification is an artifact study). No single check causally verifies
the full criterion. The honest binding would be ARTIFACT (a study
documenting per-feature latency vs horizon classification).

### 97 — Scalp-readiness (research/artifact, not code)
**Binding: UNVERIFIED. Correct.** Scalp-readiness is a research verdict:
the scalp lane's position state is per-swap event-driven, exit families
are distinct from moonshot, and admission is gated by the economic floor.
Parts are code (position.rs, sell_engine.rs), but "scalp-readiness" as a
criterion is an artifact study confirming the lane is ready. No
mechanical check verifies "readiness" as a holistic property.

### 98 — No-edge rescoping (research/economics, not code)
**Binding: UNVERIFIED. Correct.** The no-edge rescoping law requires
that every "no edge" statement is scoped to a specific tested
hypothesis/approach, never the market or a terminal state. This is a
research-process property. No code check verifies that negative verdicts
are properly scoped — it requires inspecting the research record.
ARTIFACT or OPERATOR would be the honest binding.

### 112 — Size-viability band (economics, partly code)
**Binding: UNVERIFIED. Correct.** The size-viability band is derived
from the U-shaped round-trip cost curve. The code implements the
computation, but the criterion requires that the band is correct (the
fixed-cost, protocol, and impact functions are decoded correctly per
market). A mechanical check could verify that the code computes a band,
but not that the band is economically correct for every market. The
honest binding would be ARTIFACT (a study showing the band is computed
correctly against decoded market states) or MECHANICAL (if property
tests verify the band computation against known inputs/outputs).

**Summary for all six:** UNVERIFIED is the correct binding. No mechanical
check causally verifies any of these properties. Inventing mechanical
bindings for them would repeat the defect at higher resolution. The 14
UNVERIFIED count is honest.

---

*Inspection-verified against `supervisor/gates/runner.py:92-116` and
`docs/GATE_INTEGRITY_AUDIT_2026-07-31.md:143-164`. Not from memory.*
