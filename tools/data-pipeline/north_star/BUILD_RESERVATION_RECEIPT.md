# Stage 1 prospective reservation v3 — build receipt

## Outcome and scope

Implemented metadata-only fixed-date `SEALED_ECONOMICS` reservation and fail-closed routing; no collector deployment or actual sealed capture is claimed. Owned files only:

- `src/north_star/reservation.py`
- `tests/north_star/test_reservation.py`
- `north_star/EVAL_RESERVATION_V3.json`
- `north_star/EVAL_RESERVATION_V3.md`
- `north_star/BUILD_RESERVATION_RECEIPT.md`

All work performed in `D:/repos/mev_bot-north-star/tools/data-pipeline`, branch `task/north-star-build`. Concurrent worktree changes existed and were not edited. HEAD observed during selection was `61417ced9a56526a9e6f1d4502e00f2348f4a973`; parent/other workers can advance this shared worktree. This worker made no git commit, original-repository edit, collector change, source/provider request or paid call. Master Stages 0–8 DAG and `EVAL_FREEZE.json` are untouched.

## Tool-clock evidence and reservation

Initial terminal clock: `2026-09-09T18:19:57.954554+00:00`.
Selection terminal clock: `2026-09-09T18:23:53.782127+00:00`, epoch ms `1788978233782`.

The real terminal ran Python `datetime.now(timezone.utc)`, `ZoneInfo('America/Los_Angeles')` conversions and future comparison, returning:

```text
start 2026-09-16T07:00:00+00:00 1789542000000
end 2026-09-23T07:00:00+00:00 1790146800000
duration_ms 604800000
still_future True
```

Thus the parent-agent-selected Sep16 00:00 PT → Sep23 00:00 PT dates were retained, not replaced. The operator approved holdout separation in principle; the parent agent selected these exact dates in its bounded task, not by direct operator instruction. This is one conservative seven-day collection reservation, not a sufficiency guarantee. No future source IDs are invented. Source-neutral metadata protects these dates across future supported capture sources, while all observed/historical Megga stays exposed dev under v2 and cannot be reserved as pristine.

Canonical reservation SHA256 excluding its own digest field:
`8d018de9c530b97595d5a3ec9c69d92df497be16c882df12662398b6a8476fb8`.
This corrected pin is recorded in the Markdown policy. At the provenance-correction checkpoint the unchanged test file still asserted the superseded precommit digest; the subsequent routing repair below updates that literal pin and reruns the suite. Local clock/log/pin evidence is not an independently signed timestamp or deployed append-only custody.

## Superseded precommit provenance metadata

The unpublished canonical digest `95e14af3a84e34c0fbe77049dcabc991212514617bf35abc0ab4b9cb5f70255f` is superseded by the current pin above solely to correct provenance before the first commit: the operator approved holdout separation in principle, and the parent agent selected the exact dates. The prior digest was exercised by local synthetic tests documented in the build receipt; it was not used for production collection or protected-data fitting. No protected data was collected or fitted. Dates, routing rules, permissions and pending fit/custody gates are unchanged.

## Original implementation TDD execution (before provenance correction)

Command prefix for runs below:

```text
PYTHONDONTWRITEBYTECODE=1 python -B -m pytest
```

All runs disabled pytest cache with `-p no:cacheprovider`.

| Step | Actual result | Meaning |
|---|---|---|
| `tests/north_star/test_reservation.py -q --tb=short` before production module | **1 failed in 0.04s**, exit 1 | Explicit assertion `reservation implementation missing` |
| Minimal pinned snapshot implementation | **1 passed in 0.01s**, exit 0 | Snapshot/hash contract green |
| Added policy/authority/future finite window negatives before validation | **19 failed, 1 passed in 0.04s**, exit 1 | `DID NOT RAISE ValueError` |
| Implemented validation | **20 passed in 0.03s**, exit 0 | Validation green |
| Added routing/clock/exposure/interval/ancestry/fit tests before APIs | **78 failed, 20 passed in 0.16s**, exit 1 | Missing assign/inherit/fit APIs |
| Implemented routing using existing splits/dependencies | **98 passed in 0.10s**, exit 0 | Routing green |
| Added checked-in preregistration contract before artifacts | **1 failed, 98 passed in 0.14s**, exit 1 | Explicit assertion `preregistered reservation missing` |
| Wrote artifacts, combined reservation/splits/dependencies | **227 passed in 0.20s**, exit 0 | Artifact contract and existing integration green |
| Added regression-only composed routing→transitive ancestry, omitted-clock and passive-side-effect checks; final combined run | **230 passed in 0.22s**, exit 0 | No production change needed for additional regressions |
| Final reservation-only run | **102 passed in 0.12s**, exit 0 | Final owned suite green |

Final targeted commands:

```text
PYTHONDONTWRITEBYTECODE=1 python -B -m pytest tests/north_star/test_reservation.py tests/north_star/test_splits.py tests/north_star/test_dependencies.py -q -p no:cacheprovider --tb=short
PYTHONDONTWRITEBYTECODE=1 python -B -m pytest tests/north_star/test_reservation.py -q -p no:cacheprovider --tb=short
```

Coverage includes all before/start/inside/end/after points across three synthetic future source IDs; unknown/malformed event, availability, capture and collection clocks; chronological contradictions; interval boundary spans; all v2 exposed source IDs; input/document mutation and wrong hash; invalid authority/version/window; full declared transitive protected/challenge-parent conflicts, missing splits/nodes/cycles; composed routed protected parent into development context; forbidden outcome/approval arguments; no file/network side effects; and pending fit gates for every existing named fit stage. All routing examples are synthetic, not admitted or captured records.

## Concurrent broader-suite result, not concealed

```text
PYTHONDONTWRITEBYTECODE=1 python -B -m pytest tests/north_star -q -p no:cacheprovider --tb=short
43 failed, 985 passed in 7.16s
```

This broader snapshot returned exit 1. All failures were outside ownership in concurrent `tests/north_star/test_media_clock.py`: unknown/inexact clock handling and missing `eligible_in_window`/`assess_capture_metadata` APIs during another worker's implementation. No unrelated fixes were made. The narrower existing splits/dependencies plus reservation suite is green; this receipt does **not** certify the entire concurrently changing worktree green.

`git diff --check -- src/north_star/reservation.py tests/north_star/test_reservation.py north_star/EVAL_RESERVATION_V3.json north_star/EVAL_RESERVATION_V3.md` returned exit 0. These files were untracked at inspection, so this is not a substitute for their successful syntax/import/test checks.

## Precommit provenance correction verification

Correction scope was limited to `north_star/EVAL_RESERVATION_V3.json`, `north_star/EVAL_RESERVATION_V3.md` and this receipt. Only JSON `selection_evidence.selection_basis`, `permissions.operator_principle_evidence` and `reservation_sha256` changed. All dates, routing rules and authorization values are unchanged. The module, test file and `EVAL_FREEZE.json` were verified byte-for-byte unchanged.

Reran `PYTHONDONTWRITEBYTECODE=1 python -B -m pytest tests/north_star/test_reservation.py -q -p no:cacheprovider --tb=short`: **1 failed, 101 passed in 0.14s**, exit 1. The sole failure is `test_checked_in_reservation_is_fixed_prospective_and_not_capture_evidence` at line 215, which still asserts the superseded digest. Updating that test pin is outside the authorized three-file scope and remains required before a green first commit. Historical passing runs above apply to the prior precommit digest, not this corrected artifact.

A separate real `python -B -c` metadata-only check with the corrected literal pin passed (exit 0): canonical digest, `ProspectiveReservation` construction, both Markdown pins, before/start/last/end routing (`QUARANTINED`, `SEALED_ECONOMICS`, `SEALED_ECONOMICS`, `QUARANTINED`) and the still-pending fit gate. This check is not a substitute for updating and rerunning the pinned test. No production collection or protected-data fitting occurred.

## Historical byte hashes (provenance-correction checkpoint)

```text
fb00cd66b9aa6f822d2d813a053781571b28b8a5e50298828fffee0c68183164  src/north_star/reservation.py
e7baafd926f595e3427cdbcec490ec8cbf9ed338186f1c26a6adea3c5d20f0bf  tests/north_star/test_reservation.py
a78b6efc2df16a69e52dc7fe33728fba2a2768572d62bc05cde978e375cbcb82  north_star/EVAL_RESERVATION_V3.json
f077964e2b17c725d75857dad8200301e2c2f8fd38830f5f0e97a9627ef92fd8  north_star/EVAL_RESERVATION_V3.md
309d4b305b3e9dc7a2fd851997613041ca2c96f72a6fb6ab6dfac3252bb86ed3  north_star/EVAL_FREEZE.json
freeze_unchanged True
```

The JSON byte hash differs intentionally from the canonical reservation hash. Base v2 canonical hash remains `2e192cc90ccc331202f511667a58aefef8ed9326d11f1a502cd928191378aeaa`.

## Independent-review routing repair (after provenance correction)

Read the **complete** `deleg_68a3a9bf` reviewer result, not only its truncated live-log preview. Its two must-fix findings were (P1) exposed/source/entity restriction bypass in inheritance and (P2) stale test digest. Both are addressed in this repair. Repair ownership is **only** `src/north_star/reservation.py`, `tests/north_star/test_reservation.py`, and this receipt. Reservation JSON/Markdown, base freeze, collectors and real data were not edited; no commit was made. HEAD at repair start was `dac3e1e1a1f56e925a0ea312cf1e31fb4a0cdc4d`.

`inherit()` now checks the root and complete declared transitive closure against exact pinned base identities before resolving splits. Known exposed identities always quarantine, even if the supplied split says DEVELOPMENT, SEALED_ECONOMICS, CHALLENGE or TIMELESS_REFERENCE. Known source and entity assignments must each agree with the caller's split; a conflict cannot be overwritten or disguised as a timeless reference. Compatible assignments, ordinary artifact IDs and valid registered timeless references retain their existing behavior.

**Boundary of this fix:** an artifact/record ID absent from the source registry is not automatically an unknown source. `inherit()` propagates restrictions only; it cannot discover aliases, producer source metadata, record-to-source/entity bindings, temporal window applicability, exposure or undeclared edges. Callers must independently verify those bindings and complete lineage, route source metadata through `assign()`, and preserve QUARANTINED results. Unknown source support/exposure/clocks still quarantine in `assign()`; successful artifact inheritance is not source admission or pristine-data certification. No production binding registry or collector integration is claimed.

### Actual test-first evidence

All commands ran in `D:/repos/mev_bot-north-star/tools/data-pipeline` with `PYTHONDONTWRITEBYTECODE=1 python -B -m pytest`, `-p no:cacheprovider` and `-q`.

| Checkpoint | Actual result |
|---|---|
| Before repair, original reservation suite | **1 failed, 101 passed in 0.14s**; sole failure was superseded literal pin |
| New inheritance regressions before production edit: `tests/north_star/test_reservation.py -k 'inherit_' --tb=no -rN` | **102 failed, 103 passed, 102 deselected in 0.31s**, exit 1 |
| Focused RED: `-k 'inherit_known_exposed_identity' -x --tb=short` | **1 failed, 202 deselected in 0.08s**: exposed slinky root returned `SEALED_ECONOMICS`, expected `QUARANTINED` |
| Production reconciliation and corrected literal pin; reservation/splits/dependencies | **435 passed in 0.41s**, exit 0 |
| Added regression-only source/entity `assign()` restriction checks (no further production change); final reservation/splits/dependencies | **443 passed in 0.42s**, exit 0 |
| Broader concurrent snapshot before the last assign regressions | **1297 passed in 7.58s**, exit 0 |
| Broader concurrent snapshot after the last assign regressions | **1 failed, 1305 passed in 7.65s**, exit 1; unrelated new narrative-context test `test_declarations_without_separate_registry_are_structural_only` expected false but got true |

The final targeted command was:

```text
PYTHONDONTWRITEBYTECODE=1 python -B -m pytest tests/north_star/test_reservation.py tests/north_star/test_splits.py tests/north_star/test_dependencies.py -q -p no:cacheprovider --tb=short
```

Regression coverage enumerates every real pinned exposed identity at root/direct/deep ancestry; source/entity assignment and claimed-split combinations; cross-registry conflict; timeless-reference disguise; unrelated graph nodes; unknown metadata versus artifact restrictions; and unchanged base `assign()` restrictions. Fixtures are synthetic metadata, not admitted records. Existing fit-blocking, clock, interval, malformed closure and no-I/O tests also remain green. AST parsing passed, unsafe eval/exec/shell/pickle pattern scan found no matches, and both owned Python files had no trailing whitespace. `git diff --check` exited 0 (files remain untracked, so AST/test checks are the substantive verification).

Final measured byte SHA256s for the repair:

```text
c56db3bcfc19836917e38ee3bc6866e4134401573df490ad9e18f890b845c2fa  src/north_star/reservation.py
fe0a14d28dd0cddb495a7b8f21a2d5bbb22ae5ed77c56be130b83e0aa04ce8ea  tests/north_star/test_reservation.py
a78b6efc2df16a69e52dc7fe33728fba2a2768572d62bc05cde978e375cbcb82  north_star/EVAL_RESERVATION_V3.json
f077964e2b17c725d75857dad8200301e2c2f8fd38830f5f0e97a9627ef92fd8  north_star/EVAL_RESERVATION_V3.md
309d4b305b3e9dc7a2fd851997613041ca2c96f72a6fb6ab6dfac3252bb86ed3  north_star/EVAL_FREEZE.json
base_matches_HEAD True
```

The literal test pin now matches corrected canonical reservation hash `8d018de9c530b97595d5a3ec9c69d92df497be16c882df12662398b6a8476fb8`; the historical supersession evidence above is preserved. The unrelated concurrently changing narrative suite was not repaired here. Post-repair independent review belongs to the parent workflow; this receipt is execution evidence, not that review's approval.

## Final bounded independent review

`deleg_cbb7de21` returned boundedpass/no must-fix. It reran443 reservation/split/dependency tests plus240 independent negative probes and verified current source bytes and canonical pin. Parent separately ran315 reservation tests successfully. This closes component review only; collector custody, admission and Stage1 completion remain unestablished.

## Residual blockers and no implied authority

- Base v2 remains `POLICY_FROZEN_RESERVATION_PENDING`; this additive version never enables fitting. Dependency horizons are UNKNOWN and collector custody deployment is pending.
- No collector/loader call site was modified. Production integration, independent pin storage, producer clock evidence and protected custody enforcement remain necessary before claims of actual protected acquisition.
- Exact source/provider, rights and approved budget evidence must be separately verified. No new approvals, paid calls, spending, training or live-order authority are granted here.
- Source/ancestry declarations need independent completeness/exposure auditing; a support/exposure flag is not proof. No pristine capture occurred or was certified in this task.
- Capability and stop/outage decisions are preregistered from acquisition health only. Retain all partials/outages; no outcome-conditioned dropping, moving dates, compensating days or automatic replacements. Sufficiency is assessed later and may remain insufficient.
- Independent review and complete Stage 1/data/capability certification remain outstanding. No protected model-fit, performance or economic result is claimed.
