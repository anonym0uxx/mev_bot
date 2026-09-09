# Stage 1 prospective reservation v3

**Status: PROSPECTIVE_COLLECTION_RESERVED_FIT_BLOCKED.** This is an additive fixed-date reservation, not Stage 1 closure, source admission, a sealed-capture receipt, sufficiency evidence or permission to fit.

## Fixed selection and immutable binding

- Primary split: `SEALED_ECONOMICS`, natural chronological prevalence.
- Local half-open window: **2026-09-16 00:00 PT through 2026-09-23 00:00 PT**, America/Los_Angeles (PDT, UTC−07:00).
- UTC half-open window: **[2026-09-16T07:00:00Z, 2026-09-23T07:00:00Z)**.
- UTC milliseconds: `[1789542000000, 1790146800000)`; duration `604800000` ms (seven days).
- Tool-observed selection time: `2026-09-09T18:23:53.782127+00:00`, `1788978233782` UTC ms. Python `datetime.now(timezone.utc)` and `ZoneInfo('America/Los_Angeles')` conversion returned exit 0; parent-agent-selected dates were still future. **No replacement dates were selected.** Initial tool clock observation was `2026-09-09T18:19:57.954554+00:00`.
- Canonical reservation SHA256, excluding the `reservation_sha256` field:
  `8d018de9c530b97595d5a3ec9c69d92df497be16c882df12662398b6a8476fb8`.
- Canonical v2 base policy SHA256:
  `2e192cc90ccc331202f511667a58aefef8ed9326d11f1a502cd928191378aeaa`.
- Unchanged `EVAL_FREEZE.json` byte SHA256:
  `309d4b305b3e9dc7a2fd851997613041ca2c96f72a6fb6ab6dfac3252bb86ed3`.

Canonical hashing is the existing `north_star.splits.canonical_sha256`: UTF-8 sorted-key compact JSON, `ensure_ascii=False`, no NaN. Supply the separately pinned digest above to `ProspectiveReservation`, not merely a recomputed digest from arbitrary input. Constructor copies its input, and the public document property returns a copy. Do not overwrite exercised v3 or move these dates. Any material change needs a separate version and explicit invalidation/assessment, not a rolling holdout or replacement chosen after outcomes.

This is local tool-clock evidence of preregistration before the window, **not a signed external timestamp, append-only storage attestation or deployed custody evidence**. Preserve this pin independently before production use. No future source identifiers or protected captured IDs have been fabricated; the materialized list is empty.

## Superseded precommit provenance metadata

The unpublished canonical digest `95e14af3a84e34c0fbe77049dcabc991212514617bf35abc0ab4b9cb5f70255f` is superseded by the current pin above solely to correct provenance before the first commit: the operator approved holdout separation in principle, and the parent agent selected the exact dates. The prior digest was exercised by local synthetic tests documented in the build receipt; it was not used for production collection or protected-data fitting. No protected data was collected or fitted. Dates, routing rules, permissions and pending fit/custody gates are unchanged.

## Routing contract and existing pipeline integration boundary

`src/north_star/reservation.py` provides `ProspectiveReservation(document, expected_sha256=..., base_boundary=...)`, `assign`, `inherit`, `assert_before_fit`, and `training_interval_allowed`. Its metadata-only policy protects the dates **across all future supported capture sources**, not a provider-specific allowlist. Actual exact source identifiers come from future acquisition metadata, when known. `supported_capture=True` expresses technical metadata support, **not source/provider approval**.

`assign` requires an exact source ID and entity-ID list plus exact integer UTC-ms event, availability, capture and collection-start clocks. Unknown, relative-only, malformed, bool-as-time or inconsistent clocks quarantine. Event ≤ availability ≤ capture must hold; all three must fall inside the half-open window. Selection must precede the documented collection start; capture must not precede collection. Unknown support or exposure quarantines. Known exposed IDs in v2 cannot be overridden by a caller claiming `previously_exposed=False`.

Optional explicit intervals use inclusive first/last dependency timestamps and must contain the event wholly inside the window. Missing explicit endpoints or crossing/touching the exclusive end quarantines; omitted endpoints mean a point record, **not permission to omit known episode/label/retrieval dependencies**. Those dependencies must be supplied as intervals and ancestry by the producer. Before/after-window records remain `QUARANTINED` in this reservation API; it does not invent development/calibration/validation/challenge membership or shift partitions indefinitely.

All historical/observed Megga remains exposed DEVELOPMENT under unchanged v2. Attempting to reserve such a source is QUARANTINED, not pristine. The v2 exposure registry is not exhaustive: the producer still needs affirmative unexposed evidence and an independent ancestry/exposure audit; a boolean is not that audit.

`inherit` uses existing `dependencies.parent_closure` and `EvaluationBoundary.inherit` over the **root plus every declared transitive parent**, including candidate alternatives, inventory episodes, duplicate/content families, retrieval and inherited CPT/SFT knowledge. Conflicting development/protected or challenge/economics ancestry, missing nodes or splits, and cycles quarantine. A dependency graph cannot prove absence of undeclared real-world parents.

`SEALED_ECONOMICS` here is a **prospective protected-custody routing restriction**, not proof the record was captured, remains pristine or is evaluable. This module is exercised by tests; no collector/loader call site was changed or deployed in this bounded task. Consumers must route both reserved records and quarantine away from fitting and enforce custody before collecting. Do not fall back from quarantine to development. Production custody deployment and external pin enforcement remain blockers.

## Capability coverage, outages and stops

Seven days is a conservative collection reservation, **not a sufficiency guarantee**. Use only acquisition-health metadata to record capability PRESENT/PARTIAL/ABSENT/UNKNOWN: market/executable account state, opportunity/portfolio/inventory context, narrative availability/media clocks/identity linkage, execution evidence and failed/censored exits. Record exact source/provider permission evidence, liveness and UTC intervals, clock uncertainty, disconnections, gaps/drop/backpressure counts, integrity/decoder/lineage status, custody enforcement and approved-budget evidence.

Retain partial sessions and outage/gap ledgers. Pause the affected acquisition on lost permission, approved-budget uncertainty/exhaustion, custody failure or unsafe acquisition health. Preserve acquired evidence and quarantine unsafe data. Resume only inside the original dates after the health/permission gate recovers. Do not add compensating days, extra streams or automatic replacement sessions. No returns, future labels, class yield, teacher/model performance or profitability may determine coverage decisions, stopping or replacement.

Assess economic sample sufficiency, capability support, gaps and censoring later under independent evaluation. If insufficient, report insufficient **without moving the dates**. Separate balanced action diagnostics remain separate and unresolved; never rebalance primary natural economics or replace it with a diagnostic suite.

## Permissions and residual gates

Operator approval of holdout separation in principle is recorded in `EVALUATION_RESERVATION_DECISION.md`; the **parent agent selected these exact dates** in its bounded task. The operator did not directly select or request the dates. Legacy JSON fields `requested_local_start`, `requested_local_end` and `requested_dates_changed` refer to that parent-agent task, not direct operator date instructions. This reservation grants **no new exact source/provider approvals and no collection spending**. Existing-pipeline collection still requires separately verified exact source/provider access, rights and budget approvals. No paid calls, extra acquisition service, collector changes, starts/restarts, source/media/outcome inspection, training or live orders were performed.

`EVAL_FREEZE.json` remains `POLICY_FROZEN_RESERVATION_PENDING`, byte-for-byte unchanged. The additive module delegates fit checks to that boundary and also refuses fitting if the base gate ever passed: **dependency horizons remain UNKNOWN and collector custody DEPLOYMENT_PENDING**. Teacher selection, feature/cluster fitting, execution calibration, curation, retrieval/CPT/SFT and retention fitting remain blocked. Actual source admission, complete ancestry, support/sufficiency and independent certification are still required. The master Stages 0–8 DAG is unchanged.
