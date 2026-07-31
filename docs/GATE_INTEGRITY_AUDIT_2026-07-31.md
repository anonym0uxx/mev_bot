# Gate Integrity Audit — 2026-07-31

## Summary

This audit closes the gate-integrity investigation opened on 2026-07-29. It
delivers three corrections to the prior report, implements six authorized
patches (A-F), and invokes `milestone_gate` for the first time in the repo's
history. The gate returned **FAILED** — honestly — with 14 of 18 criteria
UNVERIFIED and 3 of 10 checks failing. No false certification was ever emitted;
the defect was LATENT, caught before the first run.

---

## Corrections to the 2026-07-29 report

### Correction 1 — The defect is LATENT, not realized

The evidence DB has **zero `criteria_map` rows**. `milestone_gate` had never
been invoked prior to this audit. All 14 `gate_results` in the DB are
`task_gate` runs (phase-b-preflight). The accurate framing is:

> The first milestone gate ever run would have issued a false certification
> for 14 of 18 criteria, and it was caught before that first run.

Not "every certified=true is unsupported." No `certified=true` was ever emitted.
Phase-B holds **no milestone certification of any kind**. Any ledger text
implying a certified milestone is wrong.

### Correction 2 — Criterion 113 is UNSUPPORTED (not SUPPORTED)

Resolved by inspection of `runner.py:194-217`:
- `check_phase_provenance` is called at line 203, **inside** the
  `if cfg.run_bench and cfg.bench_name:` block (line 194).
- `supervisor.yaml` has `bench_name: ''` → the block is skipped entirely.
- The `elif cfg.criteria_touched:` branch (line 218) also does not fire because
  `criteria_touched` defaults to `[]` (falsy).
- **Conclusion**: `check_phase_provenance` is NEVER called under current config.
  Criterion 113 is UNSUPPORTED. The supported count drops from 4 to 3
  (pre-correction; after Correction 3, to 4 MECHANICAL with 115/116 now bound).

### Correction 3 — Criteria 115 and 116 are now MECHANICAL, not UNVERIFIED

`required_tests` was `[]` in `supervisor.yaml`. With it empty, `check_tests`
only verified "cargo test exited 0" — it could not show the pq-narrative or
pq-watchlist tests ran at all. Delete them and the gate stayed green. This was
the same defect class: a control configured into a state where it cannot fail.

**Patch D** populated `required_tests` with the named integration tests:
- 115: `dossier_narrative_nv_attention_series`, `dossier_narrative_nv_candidate_score`,
  `dossier_narrative_nv_lifecycle_stage`, `dossier_narrative_nv_platform_lead`
- 116: `dossier_rank_wr_lane_weights`, `dossier_rank_wr_recency_factor`,
  `dossier_rank_wr_score_rank`

With these names populated, `check_tests` now verifies each appears in `cargo
test` output. 115 and 116 bind to MECHANICAL → `test`.

**Precision correction**: "14 have NO check" was not accurate for criterion 109
(1 partial). The accurate count is: 13 criteria have NO mechanical check, 1
(109) is partial (Phase-A clauses only), 4 are MECHANICAL (109, 114, 115, 116).

---

## Authorized patches (A-F)

### A — Typed per-criterion mapping (`runner.py:44-116, 243-309`)

Replaced the blanket loop (`for crit in scoped_criteria:
set_criterion(satisfied=gate_passed)`) with a typed `CRITERION_BINDINGS` table.
Each criterion binds to exactly one of:

- **MECHANICAL** — a named check in the battery that causally verifies it.
- **ARTIFACT** — a specific study/doc, pinned by content hash.
- **OPERATOR** — a human attestation recorded under the b5a3afc TTY guard.

A criterion with no binding is UNVERIFIED and blocks certification. An ARTIFACT
binding whose hash no longer matches is UNVERIFIED, not satisfied. The dead
`not unmet` conjunct is repaired in the same commit: `unmet` is now populated by
actual binding evaluation, not by the blanket set-all-true that made it
structurally empty.

**Most unsupported criteria are NOT code properties** (52 key custody, 85
capital allocation, 96 signal-horizon, 97 scalp-readiness, 98 no-edge rescoping,
110 attention spend, 112 size-viability band). They require ARTIFACT or OPERATOR
bindings. No mechanical check was invented for them — that would be a vacuous
gate, the exact defect being removed.

### B — Shape 3 fails closed (`runner.py:185-192, 212-217`)

`run_bench: true` with `bench_name: ''` now emits:
```
CheckResult("bench", False, {"declared": True, "bench_name": ""},
            "declared run_bench: true but bench_name is empty — check is a silent no-op")
```
Same for `run_determinism: true` with `replay_bin: ''`. The silent no-op is
replaced by a loud failing result that prevents the gate from passing while a
declared check is absent.

### C — Empty-set guards (`checks.py:222-249`, `hotpath_lint.py:333-369`)

`check_no_stubs` now counts matched files and fails on zero:
```
CheckResult("no_stubs", False, {"matched_files": 0, "globs": ...},
            "EMPTY-SET: production_globs matched 0 .rs files — glob may be typo'd")
```
`check_hotpath_lint` does the same. The matched count (253 / 875) is now an
assertion in the result detail, not an observation. A typo'd glob that matches
zero files can no longer silently pass.

### D — `required_tests` populated (`supervisor.yaml:38-45`)

See Correction 3 above. The 7 named integration tests are now required.

### E — Empirical secrets test

**VERBATIM result**: `check_secrets` returned `passed=False`,
`summary="1 possible secrets"`. The Helius API key at
`docs/RPC-RATE-LIMIT-SPEC.md` IS flagged by the regex scan. The secrets check is
NOT vacuous — it catches the known-positive control. The `gitleaks` path was not
taken (gitleaks not installed on this host); the regex scan ran on all tracked
files and found the key. The key was NOT rotated, removed, or redacted —
operator decision.

### F — `milestone_gate` invoked for the first time

Run ID: `milestone_gate_first_invocation_2026-07-31`
Elapsed: 335.2s (cargo build + cargo test)

**Verdict: FAILED**

| Check | Result | Detail |
|---|---|---|
| build | PASS | compiled |
| fmt | PASS | formatted (clean, per-crate) |
| clippy | PASS | clean |
| no_stubs | PASS | no stubs (253 files scanned) |
| test | PASS | tests pass (7 required tests verified present) |
| secrets | **FAIL** | 1 possible secrets (Helius key at RPC-RATE-LIMIT-SPEC.md) |
| dossier_test_integrity | PASS | 191 dossier tests intact and unmodified |
| hotpath_lint | PASS | clean (11 LINT-ALLOW exemptions; 875 files scanned) |
| determinism | **FAIL** | declared run_determinism: true but replay_bin empty (Shape 3) |
| bench | **FAIL** | declared run_bench: true but bench_name empty (Shape 3) |

**Criterion board (18 criteria):**

| Criterion | Binding | Check/Artifact | Status |
|---|---|---|---|
| 52 | UNVERIFIED | — | key-custody election (operator process) |
| 69 | UNVERIFIED | — | native Windows / no WSL (artifact study) |
| 81 | UNVERIFIED | — | taxonomy forward-only (artifact study) |
| 85 | UNVERIFIED | — | capital allocation (policy, not code) |
| 96 | UNVERIFIED | — | signal-horizon matching (artifact study) |
| 97 | UNVERIFIED | — | scalp-readiness (artifact study) |
| 98 | UNVERIFIED | — | no-edge rescoping (artifact study) |
| 99 | UNVERIFIED | — | memory soak (soak_bin not built) |
| 102 | UNVERIFIED | — | safety constants static (artifact study) |
| 103 | UNVERIFIED | — | latency budgets (bench_name empty, Shape 3) |
| 109 | MECHANICAL | hotpath_lint | PASS (Phase-A clauses; deploy clauses need Phase-B) |
| 110 | UNVERIFIED | — | attention spend source (policy, not code) |
| 111 | UNVERIFIED | — | amendment subsystem (artifact study) |
| 112 | UNVERIFIED | — | size-viability band (artifact study) |
| 113 | UNVERIFIED | — | two-phase build boundary (inside bench block, skipped) |
| 114 | MECHANICAL | build | PASS |
| 115 | MECHANICAL | test | PASS (dossier_narrative_nv_* verified present) |
| 116 | MECHANICAL | test | PASS (dossier_rank_wr_* verified present) |

**14 UNVERIFIED, 4 MECHANICAL (of which 4 PASS). Gate: FAILED.**

This is the honest board. The first milestone gate ever run failed — correctly
— because 14 criteria have no binding and 3 checks failed. The system now tells
the truth about what it has and hasn't verified.

---

## Files modified

| File | Change |
|---|---|
| `supervisor/gates/runner.py` | Typed criterion mapping (A), Shape 3 fail-closed (B), dead conjunct repair (A) |
| `supervisor/gates/checks.py` | Empty-set guard on check_no_stubs (C) |
| `supervisor/gates/hotpath_lint.py` | Empty-set guard on check_hotpath_lint (C) |
| `supervisor/config/supervisor.yaml` | required_tests populated (D) |
| `docs/GATE_INTEGRITY_AUDIT_2026-07-31.md` | This report |
| `docs/milestone_gate_result.json` | First milestone gate output |
| `scripts/run_milestone_gate.py` | Gate invocation script |

## Not authorized / not done

- Soak harness NOT built (criterion 99 stays UNVERIFIED).
- PrivateUsage NOT wired.
- Helius key NOT touched (rotated/removed/redacted).
- No thresholds widened.
- No mechanical checks invented for non-code criteria.

---

*Audit by Hermes Agent, 2026-07-31. Constitution-governed. All findings
inspection-verified, not reasoned.*
