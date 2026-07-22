# SERVER BUILD MANIFEST — Phase-B (deployment box) implementation tasks

This file is the **authoritative, actionable checklist for the server build**. Everything here is
`server_deferred` in `docs/criteria_coverage_map.json`: it is intentionally NOT implemented on the
laptop (Phase-A) because it needs real hardware, a live endpoint, the Windows OS, or fund custody.

**For the supervisor / build model (Hermes):** when building on the deployment box, treat every task
below as an open work item. Each names (a) exactly what to implement, (b) the portable trait/seam it
plugs into that is ALREADY built and tested on the laptop, and (c) the locked acceptance test or
contract it must satisfy. The laptop side deliberately left a trait boundary for each so the server
work is "implement this trait and wire it," never "design from scratch." Do not consider Phase-B
complete until every item's acceptance condition holds on the real box.

Cross-reference: criterion numbers are the acceptance-criteria ids in `criteria_coverage_map.json`.

---

## 1. CPU/NUMA tuning — implement `OsTune` for Windows  (criterion 21, §57, §24)

**Status on laptop:** portable logic DONE + locked. `pump_quant_core::cpu_numa_tuning` ships
`parse_topology` (cn_topology_model), `derive_plan` (cn_pin_plan), `jitter_stats` (cn_jitter_probe),
`apply_plan`, the `OsTune` TRAIT, and a `MockOs`. The decision logic is fully tested via the mock.

**Server task:** implement the REAL `impl OsTune` (e.g. `struct WindowsOsTune`) over the Windows APIs —
`GetLogicalProcessorInformationEx` (to feed `parse_topology`), `SetThreadGroupAffinity`,
`SetPriorityClass`/`SetThreadPriority`, `timeBeginPeriod`, and `VirtualLock`. NO libc / no
`sched_setaffinity` / no `/proc`. Each setter must READ BACK the applied value and return the observed
result so `apply_plan`'s verification surfaces a silent OS no-op as a `Mismatch` (never trusts it).

**Wire-up:** at boot, `parse_topology(records)` → `derive_plan(&topology, &hot_threads)` →
`apply_plan(&mut windows_os, &plan, Prio::High)`; assert `report.mismatches.is_empty() &&
report.errors.is_empty()`. Then run the jitter probe on the pinned hot thread and feed the TSC deltas
to `jitter_stats`; compare p999 against the §24 budget and against the unpinned baseline.

**Acceptance:** `rust/crates/pump-quant-core/tests/dossier_cpu_numa_tuning_cn_os_apply.rs` is the locked
contract the Windows impl must satisfy (behaviourally — it tests against `MockOs`, the real impl must
exhibit the same read-back-verified semantics). The pin plan + jitter budget must verify on the real
topology.

---

## 2. Windows-native build & runtime  (criteria 1, 2, 69)

Build and run the release binaries on the real Windows deployment box. No Linux / WSL / Docker on the
critical path. The shred-ingest decode path (`pump_quant_core::shred`: sh_header_decode / sh_fec_track /
sh_reassemble / sh_parity_gate) is built + tested portably; only the OS-native socket/runtime around it
is Phase-B. Verify the whole workspace builds and its tests pass under the Windows toolchain.

## 3. Helius LaserStream live ingest  (criteria 61, 62, 72, 63)

Wire the live Helius LaserStream gRPC mainnet endpoint into the ingest layer (the portable protocol/
decode/reducer logic is built + tested). Preserve raw Helius payloads for replay/audit (63). Put the
plan/budget in the infra manifest (62) and keep cost monitoring active (72). Acceptance: live frames
decode through the existing `pump-quant-protocol` / `pump-quant-ingest` / reducer path with parity to
the recorded-replay results.

## 4. Live-chain reconciliation  (criterion 14)

Reconcile executed trades to the finalized chain (the portable reconciliation/accounting — evaluator
`net_sol`, execution `reconcile_fill` — is built + tested). Server task: feed real finalized on-chain
outcomes into it and assert the reconciled net-SOL matches. This is the truth source the research loop
grounds on; never assume, always reconcile.

## 5. Release profile / PGO / deploy-CPU pinning / zero-alloc harness  (criterion 109, §24)

Already encoded as build config in `rust/Cargo.toml` (`[profile.release]` opt-level 3, fat LTO,
codegen-units 1, panic=abort, overflow-checks on for money crates). Server task: inject
`-C target-cpu=<deploy-cpu>` via RUSTFLAGS from the infra manifest's deploy-CPU entry (NEVER `native`
on a build box), run PGO, pre-warm connections, and validate the zero-alloc hot-path harness on the
real CPU.

## 6. Isolation & custody topology  (criteria 22, 70, 71)

Deployment topology, not code: model inference isolated from the trading process (22); no container/
process with raw-key access (70); nothing but the sanctioned path mutates the frozen evaluator (71).
Real key signing and fund movement remain **Tier-0 human-gated** — never automated by any binary or
MCP tool.

## 7. Two-phase discipline itself  (criterion 113, §9)

The Phase-A (laptop) → Phase-B (server) split is the process. This manifest is the Phase-A→Phase-B
handoff record; keeping it accurate is part of satisfying 113.

---

_The laptop build is complete for everything above the trait boundary. Each server task is "implement
the named trait / wire the named endpoint and satisfy the named locked test," so the deployment build
has precise targets rather than open-ended design._
