# OSTUNE BUILD SPEC — the Windows adapter, specified to zero drift (2026-07-29)

**Audience.** The Phase-B builder, explicitly including a quantized GLM-5.2. This document exists
because manifest §1 said *"implement this trait and satisfy this named locked test, not design from
scratch"* and **the named locked test passed with zero Windows code written.** Everything below
replaces a sentence you had to interpret with something you can run.

**Class:** RESEARCH (`docs/PHASE_B_PREFLIGHT.md` §3) — operator sign-off required, with primary
evidence attached. Nothing here may be marked complete on a green gate alone.

---

## 1. Read this first: five things the older documents get wrong

Each was true-sounding, load-bearing, and false. They are corrected in place as of 2026-07-29, but
if you are reading an older checkout, these are the traps.

| # | The claim | The truth |
|---|---|---|
| 1 | manifest §1: *"the `OsTune` trait with a no-op/recording impl"* | **There was no recording impl.** Only `MockOs`, a test double with deliberate lying modes. `RecordingOs` now exists (`cpu_numa_tuning.rs`) and is what that sentence describes. |
| 2 | README: *"implement this trait and satisfy this named locked test"* | The named test, `dossier_cpu_numa_tuning_cn_os_apply`, exercises `MockOs`. It is green right now, with no adapter written. **`ostune_conformance` is the test that means something.** |
| 3 | manifest §1: *"Integration point: app startup applies the pin plan before the `evaluate()` loop starts"* | **No such call site exists.** `apply_plan`, `derive_plan`, `parse_topology`, `jitter_stats` have zero callers outside their own tests. Wiring the call site is part of your work, not something you are inheriting. |
| 4 | constitution §9.5: *"the release profile carries the correct `target-cpu` placeholder and PGO wiring, the tuning code exists, the CI latency harness exists — they are simply inactive"* | The release profile carries **no** `target-cpu` placeholder and **no** PGO wiring. There are **no criterion benches anywhere** (criterion is not a dependency of any crate), and `check_bench` ships with `bench_name: ''`, making the bench gate a **no-op**. Three of the four things that clause asserts already exist, do not. |
| 5 | manifest §1: *"Required production adapter: … `SetPriorityClass`"* | **`SetPriorityClass` takes a process handle and sets a process-wide priority class.** The trait method is `set_priority(th: ThreadId, …)` — per-thread. See §4.2; this is a real API mismatch you must resolve, not a naming quibble. |

Three referenced artifacts also do not exist: **`scripts/gen_manifest.py`** (named in three places as
the way to produce the deploy-CPU declaration), the **`cpu_numa_bench`** benchmark the dossier names,
and the **jitter-probe sampler** the dossier says "lives in the bench harness" (`bench/src/main.rs`
is 147 lines and contains no jitter, spin, TSC or pin code).

---

## 2. What already exists, and is correct

`rust/crates/pump-quant-core/src/cpu_numa_tuning.rs`. **The decision logic is pure, portable, tested
and complete.** You are not writing any of it:

* `parse_topology(&[ProcRecord]) -> Result<Topology, TopoErr>` — validates disjointness, rejects
  empty and overlapping masks.
* `derive_plan(&Topology, &[HotThreadSpec]) -> Result<PinPlan, PlanError>` — produces pairwise
  disjoint assignments plus a `control_mask` and `reserved_idle` mask (the SMT siblings §24(c)
  requires be left idle). Rejects `Insufficient` and `MultiGroupSpan`.
* `apply_plan(&mut dyn OsTune, &PinPlan, Prio) -> ApplyReport` — read-back verification per thread;
  a mismatch is recorded, never dropped; a hard error does not abort the remaining threads.
* `jitter_stats(&[u64]) -> JitterStats` — nearest-rank p50/p99/p999/max, integer only.
* `ostune_conformance(...) -> ConformanceReport` **(new)** — §3.
* `RecordingOs` **(new)** — the no-op recorder, for exercising a plan on a machine with no tuning
  privileges.

**Your entire deliverable is one `impl OsTune`, one call site, and the evidence.**

---

## 3. The acceptance test — `ostune_conformance`

```rust
pub fn ostune_conformance<T: OsTune + ?Sized>(
    os: &mut T,
    probe: ThreadId,
    honest_affinity: GroupAffinity,
    timer_ms: u32,
    region: &[u8],
) -> ConformanceReport
```

It exercises **all four** trait methods — including `set_timer_res_ms` and `lock_region`, which
`apply_plan` never calls and which therefore had **no caller anywhere in the repository**. Before
this battery you could have returned `Ok(0)` from both and kept a fully green workspace.

**The five obligations:**

1. `set_affinity` honoured by read-back → else `AffinityNotHonoured`
2. `set_priority(High)` honoured by read-back → else `PriorityNotHonoured`
3. `set_timer_res_ms(n)` grants **no coarser than** `n` → else `TimerResNotHonoured` **plus** a
   quantified `SurfaceMismatch::TimerRes { requested, observed }`. A *finer* grant passes.
4. `lock_region` locks **at least** `region.len()` → else `LockRegionShort` plus
   `SurfaceMismatch::LockRegion { requested, observed }`
5. **The anti-stub probe.** `set_affinity` with `mask = 0` — an affinity naming no processor. An
   adapter answering `Ok(mask: 0)` is echoing its argument rather than reading the OS back.
   `Err(Denied)` is correct; `Ok(something_else)` is also correct. **`Ok(mask: 0)` is
   `EchoesImpossibleRequest` and is the check that catches an implementation written to make the
   suite green.** Obligations 1–4 all pass for such a stub; only this one separates it from a real
   adapter.

**Errors versus failures — the distinction is operational, do not collapse it.** A denied request
lands in `errors`; a dishonest one lands in `failures`. `Denied` means *"run elevated / grant
`SeLockMemoryPrivilege`"*. A failure means *"your adapter is wrong."* Sending a builder to debug
code when the answer is a privilege token is a wasted day.

`rust/crates/pump-quant-core/tests/ostune_conformance.rs` (10 tests) proves the battery detects each
dishonesty specifically, and pins that `RecordingOs` and `MockOs::faithful()` are both
**deliberately non-conformant** — so neither can be wired into an acceptance run and read as a pass.

**THE BINDING RULE.** `ConformanceReport::conformant()` must be `true`, produced by the **real
adapter on the deploy box**, and the full report journaled, **before any tuned latency number may be
claimed.** A green workspace on the laptop is not evidence about Windows and never was.

---

## 4. The adapter — required behaviour, method by method

Target: `x86_64-pc-windows-msvc`. Bind via the `windows` / `windows-sys` crate. **No `libc`, no
`mlockall`, no `sched_setaffinity`** — `supervisor/gates/hotpath_lint.py` enforces the Linux-ism ban
across ALL Rust, so a stray `mlockall` fails the gate rather than reaching the box.

### 4.0 The `unsafe` problem — resolve this BEFORE writing the adapter

`lock_region(&mut self, ptr: *const u8, len: usize)` takes a raw pointer. Every Win32 binding here
requires `unsafe`. Constitution §24(b):

> *"an `unsafe` block is permitted only with a property-tested safety argument registered in the
> owning component's dossier, and any `unsafe` block without one is a gate failure."*

**`supervisor/reinforcement/dossiers/cpu_numa_tuning.yaml` registers no unsafe safety argument.** So
as the repository stands, writing the adapter is a gate failure by construction. This is a genuine
contradiction between two rules, not a thing to route around.

**Do not resolve it yourself.** Both available resolutions — registering a safety argument in the
dossier, or placing the adapter in a crate outside the dossier's authority — change what the
constitution enforces, which §68 / criterion 111 reserves to the operator. **STOP AND ASK.**

Note also: the eight crates carrying `#![forbid(unsafe_code)]` as of 2026-07-29 cannot host the
adapter at all — `forbid` cannot be locally overridden by `#[allow]`. That is deliberate.

### 4.1 `set_affinity` — and the trap in the Win32 signature

`SetThreadGroupAffinity(HANDLE, const GROUP_AFFINITY*, PGROUP_AFFINITY PreviousAffinity)`.

**`PreviousAffinity` is the PREVIOUS affinity, not the current one.** Returning it as the observed
value satisfies the type and defeats the entire read-back design: on success you would report the
affinity the thread had *before* your call. **The observed value must come from a separate
`GetThreadGroupAffinity` call after the set.** This single mistake is the most likely way an
otherwise careful adapter silently fails, and `ostune_conformance` obligation 1 will catch it only
if the previous and requested affinities differ — so do not rely on the battery here; get it right.

Map `GroupAffinity { group: u16, mask: u64 }` to `GROUP_AFFINITY { Mask: KAFFINITY, Group: WORD }`.
`mask = 0` must return `Err(OsErr::Denied)` — Win32 rejects it, and obligation 5 depends on you
propagating that rejection rather than short-circuiting.

### 4.2 `set_priority` — the API named in the manifest is the wrong one

Manifest §1 and README both name `SetPriorityClass`. **That function takes a process handle and sets
a process-wide priority class.** The trait method is per-thread.

Required mapping (per-thread, `SetThreadPriority` + read back with `GetThreadPriority`):

| `Prio` | Win32 constant | Value |
|---|---|---|
| `Normal` | `THREAD_PRIORITY_NORMAL` | 0 |
| `High` | `THREAD_PRIORITY_HIGHEST` | 2 |
| `Realtime` | `THREAD_PRIORITY_TIME_CRITICAL` | 15 |

Effective priority is the pair (process class, thread priority), so a process left at
`NORMAL_PRIORITY_CLASS` bounds what any thread priority can achieve. If you conclude the process
class must also be raised, **that is a design decision beyond this trait — STOP AND ASK.** Do not
silently add a `SetPriorityClass` call inside a method whose contract is per-thread.

§24(c) requires the starvation risk of an elevated hot set be **recorded**, and `Realtime` /
`TIME_CRITICAL` can starve kernel work outright. Ship `High`. `Realtime` requires operator approval
with the starvation analysis attached.

**Verify every constant above against current Microsoft documentation before use and record the
verification** (§18.2: never accept a value because a document claims it — and this document is a
document).

### 4.3 `set_timer_res_ms`

`timeBeginPeriod(UINT)` from `winmm`. Query the valid range with `timeGetDevCaps` first; a request
outside `[wPeriodMin, wPeriodMax]` returns `TIMERR_NOCANDO` (97) → `Err(OsErr::Denied)`.

Return the **granted** period, which may be coarser than requested. Do not round it to the request —
obligation 3 exists precisely to catch that, and a coarser grant is a real operational fact.

Since Windows 10 2004 the effect is per-process, not global. Pair every `timeBeginPeriod` with
`timeEndPeriod` at shutdown.

### 4.4 `lock_region`

`VirtualLock(LPVOID, SIZE_T)` → BOOL. Bounded by the process working-set minimum; you will usually
need `SetProcessWorkingSetSize` first, and `SeLockMemoryPrivilege` for large pages.

Return **bytes actually locked**. A partial lock returns the partial count, never `len`. On failure
`GetLastError() == ERROR_NOT_ENOUGH_QUOTA` → `Err(OsErr::Denied)`. Pair with `VirtualUnlock`.

`OsErr` has only `Denied` and `NotFound`. Map anything privilege-, quota- or capability-shaped to
`Denied`; map an invalid handle to `NotFound`. **Do not add variants** — `OsErr` is in the SHA-locked
dossier signature for `cn_os_apply`.

### 4.5 Fail-closed — manifest §1, non-negotiable

> *"if any OsTune call fails, the bot runs unpinned and records the failure; it never claims tuned
> latency numbers."*

Unpinned-and-recorded is a correct outcome. Refusing to start is not. Silently continuing while
reporting tuned numbers is the failure this clause exists to forbid.

---

## 5. The call site — manifest §1's "integration point", which does not exist

Wire it at app startup, before the `evaluate()` loop, in this order:

1. Enumerate processors → `Vec<ProcRecord>` (`GetLogicalProcessorInformationEx`).
2. `parse_topology` → on `Err`, run unpinned, record, continue.
3. `derive_plan` with the hot-thread specs → on `Err`, same.
4. `apply_plan` → journal the full `ApplyReport`. **A non-empty `mismatches` or `errors` means
   unpinned-and-recorded, not tuned.**
5. `ostune_conformance` against the real adapter → journal the `ConformanceReport`.
6. Jitter probe before/after → journal both `JitterStats`.

### The `Config` question — read this before you add a single field

**`Config` currently contains no OsTune field**, so OsTune cannot move the golden digest today. The
§19 journal seed is `fnv1a_64(format!("{cfg:?}"))` over the **whole config identity**
(`engine.rs:1077`), so **adding any field moves the digest with zero decision change.**

This will fire. When it does, follow the activation directive's seed-only re-pin rule exactly, and
verify against **the code, not against any document**:

```
GOLDEN_DIGEST            = 13_693_021_370_354_439_552
GOLDEN_NET_LAMPORTS      = 31_111_528
GOLDEN_PROMOTED          = 504
GOLDEN_ADMITTED          = 11
GOLDEN_REJECTED          = 448
GOLDEN_UNIVERSE_FILTERED = 72
GOLDEN_ALPHACALL_NET     = +815_594
```

If all seven are byte-identical and only the digest moved, it is a **seed-only re-pin** — legitimate,
done 8+ times. Update `golden_digest.rs` AND `pq-regression/src/baselines.rs`, and add a ledger
entry. If **any** of the seven moved, it is a determinism break: revert and halt (§7).

`cargo test -p pq-regression --test hermes_doc_pins` proves the documents quote those seven
correctly, so you do not have to check by eye — and if it fails, **the document is wrong, never the
constant.**

**Strongly preferred: do not add a `Config` field at all.** OS tuning is deployment identity, not
strategy identity. Put it in the infra manifest (`infra_manifest.example.json`) or a startup
argument, and the digest never moves. A digest move you did not have to cause is a judgment you did
not have to make.

---

## 6. Evidence required for sign-off

Attach primary evidence — a command and its literal output, a journal entry, a screenshot. A
conclusion is not evidence.

1. `ConformanceReport` from the **real adapter on the deploy box**, `conformant() == true`.
2. `ApplyReport` with empty `mismatches` and `errors`.
3. `JitterStats` before and after pinning, from the same box, same load.
4. Proof that SMT siblings of hot cores are idle (`reserved_idle` honoured).
5. NIC IRQ/RSS steering configuration, and the constant-frequency power plan.
6. **The golden digest, unchanged** — manifest §1: *"tuning must be behaviour-preserving"*. If it
   moved, §5's seven-value check decides whether that was legitimate.
7. The `unsafe` resolution from §4.0, **signed off by the operator**.
8. Every Win32 constant in §4, verified against current Microsoft documentation, with the
   verification recorded.

---

## 7. Adjacent work this spec does NOT cover, and its state

| Item | State | Where |
|---|---|---|
| `RUSTFLAGS -C target-cpu=znver5` | Ban on `native` is real and lint-enforced. **The release profile carries no placeholder.** | `docs/LATENCY.md:95`, manifest §5 |
| PGO | **No wiring exists.** | manifest §5 |
| Criterion benches | **None exist.** Criterion is not a dependency of any crate. | — |
| `check_bench` | Ships `bench_name: ''` → **the bench gate is a no-op**, and it parses **p50 only**, so p99/p999 budgets are declared and never bind. | `supervisor/gates/checks.py` |
| `scripts/gen_manifest.py` | **Does not exist**, though named in three places. | — |
| Jitter probe sampler | **Does not exist.** `jitter_stats` aggregates samples nobody produces. | `bench/src/main.rs` |

Criteria 20 / 103 / 109 require measured p50/p95/p99/p99.9 on deployment-identical hardware. **With
no benches, a no-op bench gate, and p50-only parsing, none of that is currently measurable.**
Building the harness is Phase-B work of the same rank as the adapter — and until it exists, no
latency criterion may be marked satisfied. Absence is a recorded build state, never a silent
omission (§9.5).
