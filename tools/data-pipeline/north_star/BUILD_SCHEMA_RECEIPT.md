# Stage 2 initial schema / availability receipt

**Status: INITIAL PRIMITIVES IMPLEMENTED AND SYNTHETICALLY TESTED. Stage 2 remains incomplete. No corpus coverage or admission established.**

## Authority and scope

Read the complete master §6 (lines 366–385), v5 object inventory (118–127), and `north_star/BUILD_PLAN.md`, including S2.1 and the later D14 operator override. The actual master is under `tools/data-pipeline/north_star/master/`, not repository-root `north_star/master/`.

All paths below are relative to `tools/data-pipeline/`. This task created only:
- `src/north_star/schema.py`
- `src/north_star/availability.py`
- `schemas/north_star/CANONICAL_REGISTRY.json`
- `tests/north_star/test_schema.py`
- `tests/north_star/test_availability.py`
- `north_star/BUILD_SCHEMA_RECEIPT.md`

No commits, shared package initializer, raw parser, other worker modules, source data, labels, training or orders were created/changed by this task. Existing unrelated dirty files were left alone. Tests load the standalone files via importlib; no namespace initializer is needed.

## Implemented

- Registry mechanically checked against the master: **20 canonical tables + 8 v5 supplemental objects**, plus the plan's supporting `coverage_intervals` object separately counted. Canonical primary keys preserve the master inventory. Supplemental/supporting primary keys are explicit initial engineering proposals, not claims of master-prescribed keys.
- Shared provenance, per-null reasons, identity/revision, time/clock, unit, dependency, rights/split ancestry and input-versus-label contracts. Exact per-table reference candidates are enumerated. All consumer schemas remain `INCOMPLETE`; every corpus coverage value is null.
- `rust_market_bridge_episode_v1` remains present, linked to D14, in `engineering_evidence`, explicitly not Qwen SFT eligible.
- Exact unsigned u64 native monetary values: rejects binary floats, bools, strings, negative/overflow quantities; raw tokens require u8 mint decimals. This is not signed ledger/aggregate accounting. Source verification of declared decimals remains a producer responsibility.
- Identity validation requires a schema-version token and every table primary-key component, preserving literal strings. Revisions may start at zero; integer versions are positive. Unknown token UTC availability cannot fabricate that table's time-bearing key.
- `validate_null_reason` rejects unexplained nulls and null reasons attached to present values.
- Each field uses its own immutable dependency references and explicit nonnegative integer compute delay: `max(dependency available_at) + compute_delay_ms`.
- Empty/missing/unknown dependencies fail closed with null availability. Relative-only arithmetic requires one exact clock domain and never passes a default UTC timing cutoff. Mixed relative sessions or UTC/relative dependencies fail closed. Metadata/time numeric coercion is rejected.
- Timing predicate is **not** admission, source authenticity, event-order or rights/split enforcement. Same-time event ordering requires separate evidence. Dependencies must already be resolved; no recursive DAG implementation is claimed.

## Actual TDD execution

Working directory: `D:/repos/mev_bot-north-star/tools/data-pipeline`.

1. Native-money test before implementation: `python -m pytest tests/north_star/test_schema.py -q` → **1 failed**, missing schema primitive (exit 1); after minimal implementation → **1 passed** (exit 0).
2. Added registry test before registry: same command → **1 failed, 1 passed** (exit 1); after registry creation → **2 passed** (exit 0).
3. Added identity/null/availability fixtures before those APIs: `python -m pytest tests/north_star/test_schema.py tests/north_star/test_availability.py -q` → **23 failed, 8 passed** (exit 1), absent APIs/availability module.
4. Implemented primitives: same targeted command → **31 passed in 0.04s** (exit 0).
5. Final rerun after import formatting: same targeted command → **31 passed in 0.05s** (exit 0).
6. `git diff --check` → exit 0 for existing tracked diff; new files also received tool syntax/lint checks. This is not a full-repository suite run or independent acceptance review.

All event/identity/amount examples are explicitly synthetic software fixtures. The master/registry test reads specification text, not source data. No accepted/rejected/quarantined corpus rows were produced; no source rows were inspected by these tests. Corpus row counts remain unmeasured, not substituted with fixture counts.

## SHA256 of tested artifacts

- `src/north_star/schema.py`: `17ede8827c9f30ee75b5d3b50244137c80b3634e496521617982c21db8914419`
- `src/north_star/availability.py`: `5178edd974b8798ddc11023347cd7fa595eb4d5802227a6164dd7b20306a0470`
- `schemas/north_star/CANONICAL_REGISTRY.json`: `c204f940371f2f1bfe318b6b215614727da13d564a64017bdb8e8ea9ce062685`
- `tests/north_star/test_schema.py`: `1facf222fc961c157cf192c274a62c97834106d8e2bb14a75e57cf9f4a495b47`
- `tests/north_star/test_availability.py`: `ca5768e9f0009cfd400b1b6b7b49c462e156bf6155bb631d9704f4de95b14fa1`

## Remaining / limitations

Full typed per-table consumer schemas, source-to-field mappings/adapters, immutable payload validation, foreign-key/duplicate/revision/finality reconciliation, graph traversal/cycle checks, exact price/fraction and ledger consumers, enforced rights/split ancestry integration, raw parsing/Parquet production, source-backed fixtures and independent acceptance are not implemented here. Detailed provenance/unit/dependency contracts beyond the named APIs are descriptive, not enforced by a full-row validator. No completion of S2.1 or Stage 2 is claimed.
