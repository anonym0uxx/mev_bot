# Criterion 109 Resolution — PARTIAL, not MECHANICAL

**Date:** 2026-07-31
**Author:** Hermes Agent (CONDUCTOR, constitution §69 Surface 2)
**Commit base:** 54130681bd4a
**Task:** 2 (criterion 109 bucket resolution)

## Finding

Criterion 109 is **PARTIAL**. It must not read MECHANICAL.

The `hotpath_lint` check passes (875 files scanned, 0 violations, 11 LINT-ALLOW
exemptions), but it enforces only the **Phase-A code-pattern bans** — the
lintable subset of criterion 109's twelve clauses. The **deploy clauses** —
the Phase-B-exclusive requirements that can only be validated on deployment
hardware — do not execute. No mechanical check on this machine verifies them.

## Evidence: clause-by-clause decomposition

Criterion 109 text (constitution §109, verbatim summary):

> The Rust performance-engineering law is enforced end-to-end: release
> binaries are built with the pinned profile, deploy-CPU-pinned codegen
> (never build-box `native`), and replay-corpus PGO; the hot path passes
> a CI zero-allocation harness, contains no async/await or lock-guarded
> channels, and every `unsafe` block carries a dossier-registered
> property-tested safety argument; money arithmetic cannot silently wrap
> under any profile; Windows-native runtime tuning uses Windows APIs
> (VirtualLock, affinity, timer resolution) with no Linux-isms;
> submission-surface connections are pre-warmed monitored invariants;
> the named build defects (`/tmp` bin paths, monolithic SDK dependency,
> unpruned feature sets) are resolved; nightly compile accelerators never
> produce gate, bench, release, or replay artifacts; and every
> optimization is admitted only by measured p50/p95/p99/p99.9 movement on
> deployment-identical hardware against the criterion-103 budget.

| # | Clause | Phase | Checked by hotpath_lint? | Evidence |
|---|---|---|---|---|
| 1 | Pinned release profile | B | NO | Build config, not lint |
| 2 | Deploy-CPU-pinned codegen (never `native`) | B | NO | Build config, Phase-B per §9.5(i) |
| 3 | Replay-corpus PGO | B | NO | Phase-B per §9.5(ii) |
| 4 | CI zero-allocation harness | A/B | NO | Separate CI check; not in lint rules |
| 5 | No async/await or lock-guarded channels | A | **YES** | `hot_await`, `hot_tokio` rules |
| 6 | Every `unsafe` has dossier-registered safety argument | A | NO | Separate check; not in lint rules |
| 7 | Money arithmetic cannot silently wrap | A | **YES** | `money_float_cast` rule |
| 8 | Windows-native tuning, no Linux-isms | A/B | **PARTIAL** | `linuxism_mlock`, `linuxism_affinity` ban Linux-isms in code (Phase-A); measured tuning (VirtualLock, timer, affinity effect) is Phase-B per §9.5(iii) |
| 9 | Pre-warmed submission-surface connections | B | NO | Phase-B per §9.5(v) |
| 10 | Named build defects resolved (`/tmp` paths, etc.) | A | **YES** | `tmp_path` rule |
| 11 | Nightly accelerators never produce artifacts | B | NO | Artifact provenance, Phase-B |
| 12 | Optimization admitted by p50–p99.9 on deploy hardware | B | NO | Phase-B per §9.5(iv) |

**Covered by lint (Phase-A):** clauses 5, 7, 8 (code-level), 10 — four clauses.
**NOT covered (deploy/Phase-B):** clauses 1, 2, 3, 4, 6, 8 (measured), 9, 11, 12 — eight clauses.

## The defect in the binding

`CRITERION_BINDINGS` (runner.py:103-104) declares:

```
109: CriterionBinding(109, "MECHANICAL", check_name="hotpath_lint",
      note="§24 Rust perf law — Phase-A clauses enforced by lint; deploy clauses require Phase-B")
```

The binding_type is `MECHANICAL`, which means: *"satisfied only if the named
check exists in results AND passed"* (runner.py:266-282). When `hotpath_lint`
passes, the gate marks criterion 109 **fully satisfied** — but the check only
covers 4 of 12 clauses. The remaining 8 (the deploy clauses) are unchecked.

A `MECHANICAL` binding claims **causal verification of the entire criterion**.
It does not. The note itself acknowledges this ("deploy clauses require
Phase-B"), but the binding_type does not — the gate treats it as complete.

This is the same defect class as the audit's original "14 have NO check" error:
a criterion placed in a bucket that overstates what was verified.

## Corrected 18-row criterion table

Each criterion in exactly one bucket. 14 + 1 + 3 = 18.

| Criterion | Bucket | Check/Artifact | Status |
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
| **109** | **PARTIAL** | **hotpath_lint** | **Phase-A lint clauses PASS (4/12); deploy clauses (8/12) NOT checked** |
| 110 | UNVERIFIED | — | attention spend source (policy, not code) |
| 111 | UNVERIFIED | — | amendment subsystem (artifact study) |
| 112 | UNVERIFIED | — | size-viability band (artifact study) |
| 113 | UNVERIFIED | — | two-phase build boundary (inside bench block, skipped) |
| 114 | MECHANICAL | build | PASS |
| 115 | MECHANICAL | test | PASS (dossier_narrative_nv_* verified present) |
| 116 | MECHANICAL | test | PASS (dossier_rank_wr_* verified present) |

**Summary: 14 UNVERIFIED, 1 PARTIAL, 3 MECHANICAL. Total: 18. ✓**

## What the previous report got wrong

The GATE_INTEGRITY_AUDIT_2026-07-31.md placed 109 in two buckets simultaneously:

- Line 59 (text): "1 (109) is partial (Phase-A clauses only)"
- Line 157 (table): "109 | MECHANICAL | hotpath_lint | PASS"
- Line 166 (summary): "14 UNVERIFIED, 4 MECHANICAL (of which 4 PASS)"

109 appeared as both "partial" (in the text) and "MECHANICAL" (in the table
and the 4-MECHANICAL count). The 4 MECHANICAL included 109, but the text said
109 was partial. A criterion cannot be in two buckets. The corrected count is
3 MECHANICAL (114, 115, 116), not 4.

## What it would take to fix the binding

The `CRITERION_BINDINGS` enum has four types: MECHANICAL, ARTIFACT, OPERATOR,
UNVERIFIED. There is no PARTIAL type. The correct fix is one of:

1. **Split 109** into two bindings: a Phase-A portion (MECHANICAL → hotpath_lint)
   and a Phase-B portion (UNVERIFIED, requiring deployment-hardware provenance
   via the §9.5 phase gate). This is the principled fix — it makes the partial
   coverage explicit in the type system.

2. **Change 109's binding_type to UNVERIFIED** with a note that Phase-A lint
   clauses are enforced but the criterion as a whole is not verified. This is
   conservative but overstates the gap — the lint DOES verify real properties.

3. **Add a PARTIAL binding type** to the enum. This is the most expressive
   option but expands the type system.

Option 1 is recommended. It requires a constitution-level decision (the binding
table is part of the gate infrastructure) and is left for operator direction.

## Inspection method

- Read `supervisor/gates/hotpath_lint.py` (full, 369 lines) — identified all
  12 lint rules and their path globs.
- Read `supervisor/gates/runner.py:92-116` — CRITERION_BINDINGS table, confirmed
  109's binding_type is MECHANICAL.
- Read `supervisor/gates/runner.py:266-282` — MECHANICAL evaluation logic:
  pass if check exists and passed, else fail. No partial-coverage concept.
- Ran `check_hotpath_lint('.')` — confirmed PASS, 875 files, 0 violations,
  11 LINT-ALLOW exemptions.
- Ran `_files_for(repo, rule.path_globs)` for each rule — confirmed all 12
  rules match real files (37–731 files per rule). No empty-set no-ops.
- Read criterion 109 text in `docs/HERMES_ONE_SHOT_PROMPT.md` — decomposed
  into 12 clauses, classified each as Phase-A or Phase-B per §9.5.
- Read `docs/GATE_INTEGRITY_AUDIT_2026-07-31.md:57-166` — confirmed 109's
  double-bucket placement in the prior report.

No gate was run. No check was re-derived. This is inspection of existing code
and text, per the directive's prohibition on re-running the audit.

---

*Inspection-verified, not reasoned. Constitution-governed.*
