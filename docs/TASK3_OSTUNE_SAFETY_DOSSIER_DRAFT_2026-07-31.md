# §24(b) Safety Dossier — OsTune Windows Adapter (DRAFT 2026-07-31)

**STATUS: DRAFT ONLY.** Registration is the operator's act under §68/criterion 111.
No unsafe blocks are written. No dossier is registered. This draft exists for
operator review before any code is produced.

Constitution §24(b):
> *"an `unsafe` block is permitted only with a property-tested safety argument
> registered in the owning component's dossier, and any `unsafe` block without
> one is a gate failure."*

The adapter implements `OsTune` over four Win32 calls, each requiring `unsafe`.
This dossier argues each block separately. All four are **startup-time calls** —
none may run on the hot path. `hotpath_lint` covers `pump-quant-core/src/**/*.rs`
and must cover the adapter module before it is written.

---

## Block 1: SetThreadGroupAffinity

### What the compiler cannot verify

`SetThreadGroupAffinity(HANDLE, *const GROUP_AFFINITY, *mut GROUP_AFFINITY)`
takes three raw pointers: a thread handle, a pointer to the requested affinity,
and an optional out-pointer for the previous affinity. The compiler cannot
verify that:

1. The `HANDLE` refers to a real thread the caller owns.
2. The `GROUP_AFFINITY*` points to a valid, initialized struct.
3. The `Group` field is a valid processor group index (< GetActiveProcessorGroupCount()).
4. The `Mask` field names at least one processor in that group.
5. The out-pointer (if non-null) is writable and large enough.

### Preconditions established in SAFE code

Before the `unsafe` block:

- **Handle:** `GetCurrentThread()` returns a pseudo-handle (0xFFFFFFF6) that
  is always valid for the calling thread and never needs `CloseHandle`. This
  removes the lifetime question entirely — there is no handle to leak or
  close. The pseudo-handle is interpreted in the context of the calling
  thread, so the adapter must call `SetThreadGroupAffinity` from the thread
  being pinned (or use `DuplicateHandle` to obtain a real handle for a
  different thread). The adapter must state which.

- **Group validation:** `GetActiveProcessorGroupCount()` returns the number
  of processor groups. The adapter asserts `group < count` in safe code
  before entering the `unsafe` block. On this box: 192 logical processors =
  3 processor groups of 64. The group-aware API is REQUIRED, not stylistic —
  `SetThreadAffinityMask` (the legacy API) cannot address groups > 0.

- **Mask validation:** `GetActiveProcessorCount(group)` returns the number of
  active processors in the group. The adapter validates that the requested
  `mask` has exactly one bit set and that bit falls within the active
  processor count. A `mask = 0` is rejected with `Err(OsErr::Denied)` in
  safe code before the block (obligation 5).

- **Reserved[3] fields:** The `GROUP_AFFINITY` struct has a `Reserved[3]`
  field. The adapter zeroes it explicitly in safe code before passing the
  struct to the FFI. Uninitialized padding would be a soundness bug.

### SMT siblings

`GetLogicalProcessorInformationEx(RelationProcessorCore)` enumerates logical
processors and their SMT relationships. The adapter MUST derive SMT siblings
from this enumeration — NOT assume enumeration order. Assuming order would
pin two hot threads to one physical core, halving effective parallelism.

The adapter verifies the pin landed correctly by asserting
`GetCurrentProcessorNumberEx()` returns a logical processor number within
the requested group and mask AFTER the call.

### Failure modes

- `GetLastError()` non-zero → `Err(OsErr::Denied)`. Fail closed — the
  tuning run aborts, the bot runs unpinned and records the failure (§4.5).
- The out-pointer returns the PREVIOUS affinity, NOT the current one. The
  adapter MUST call `GetThreadGroupAffinity` separately to read back the
  observed value. Reporting the previous value as observed would satisfy
  the type and defeat the read-back verification design (OSTUNE_BUILD_SPEC
  §4.1).

### What future change invalidates this

- Calling `SetThreadGroupAffinity` from a different thread than the one
  being pinned without `DuplicateHandle`.
- Removing the `group < count` or mask validation checks.
- Using the legacy `SetThreadAffinityMask` instead of the group-aware API.
- Passing the `PreviousAffinity` out-pointer's value as the "observed"
  result instead of calling `GetThreadGroupAffinity`.

### // SAFETY: comment

```rust
// SAFETY: `handle` is `GetCurrentThread()` (pseudo-handle, always valid,
// never closed). `group` was validated < `GetActiveProcessorGroupCount()`
// and `mask` was validated against `GetActiveProcessorCount(group)` in safe
// code above. `Reserved[3]` was zeroed. The out-pointer is null (we read
// back via a separate `GetThreadGroupAffinity` call instead).
unsafe { SetThreadGroupAffinity(handle, &req, ptr::null_mut()) }
```

---

## Block 2: SetPriorityClass / SetThreadPriority

### What the compiler cannot verify

The trait method `set_priority(th: ThreadId, prio: Prio)` is per-thread. The
manifest named `SetPriorityClass`, which is process-wide — a real API
mismatch (OSTUNE_BUILD_SPEC §4.2, item #5). The adapter uses
`SetThreadPriority` for the per-thread mapping and, if operator-approved,
`SetPriorityClass` for the process class.

### Hazard: system-safety, NOT memory-safety

**This call's hazard is system-safety, not memory-safety.** Conflating them
hides the real risk. `HIGH_PRIORITY_CLASS` (0x80) is safe for the trading
path. `REALTIME_PRIORITY_CLASS` (0x100) can starve kernel disk and network
threads — the very path the bot trades through — on a box already hosting
238 GB of llama.cpp. This is the one place a plausible-looking safety
argument can take the machine down by causing kernel thread starvation,
not a memory violation.

### Preconditions established in SAFE code

- **Priority value:** `HIGH_PRIORITY_CLASS` (0x80), NOT `REALTIME` (0x100).
  If realtime is required, the operator must approve with a starvation
  analysis attached (§24(c)).
- **Thread priority mapping:** `SetThreadPriority(handle, prio_const)` where
  `Normal → THREAD_PRIORITY_NORMAL (0)`, `High → THREAD_PRIORITY_HIGHEST (2)`.
  The adapter verifies the constant values against current Microsoft
  documentation before use (§18.2).

### Failure modes

- `SetThreadPriority` returns 0 (failure) → `Err(OsErr::Denied)`.
- Read-back via `GetThreadPriority` must match the requested value. A
  mismatch is a `Mismatch::Priority`, not an error — the OS may have
  clamped the priority.

### What future change invalidates this

- Using `REALTIME_PRIORITY_CLASS` without operator approval.
- Adding a `SetPriorityClass` call inside `set_priority` without operator
  approval (the trait contract is per-thread; process class is a separate
  design decision — §4.2 says STOP AND ASK).

### // SAFETY: comment

```rust
// SAFETY: `handle` is `GetCurrentThread()` (pseudo-handle). `prio_const` is
// a verified Win32 constant (THREAD_PRIORITY_HIGHEST = 2, verified against
// Microsoft docs 2026-07-31). The hazard is system-safety (kernel thread
// starvation under REALTIME), NOT memory-safety; we use HIGH only.
unsafe { SetThreadPriority(handle, prio_const) }
```

---

## Block 3: timeBeginPeriod / timeEndPeriod

### What the compiler cannot verify

`timeBeginPeriod(UINT)` from `winmm` sets the global (pre-Win10 2004) or
per-process (Win10 2004+) timer resolution. The compiler cannot verify that
the requested period is within the valid range, or that the call is paired
with `timeEndPeriod` at shutdown.

### Preconditions established in SAFE code

- **Range query:** `timeGetDevCaps(&caps, sizeof(caps))` returns
  `wPeriodMin` and `wPeriodMax`. The adapter requests `wPeriodMin`, NOT a
  hardcoded 1. A request outside `[wPeriodMin, wPeriodMax]` returns
  `TIMERR_NOCANDO` (97) → `Err(OsErr::Denied)`.
- **RAII pairing:** A guard struct holds the granted period and calls
  `timeEndPeriod` in `Drop`. The adapter states that `Drop` is NOT
  guaranteed (abort, `mem::forget`, panic during unwind), and the failure
  mode is raised timer resolution until process exit, NOT undefined
  behavior. Windows 10 2004+ scopes the effect largely per-process.

### Alternative evaluation (required in dossier)

The adapter MUST evaluate the alternatives in this dossier:

1. **`CreateWaitableTimerExW` with `CREATE_WAITABLE_TIMER_HIGH_RESOLUTION`**
   (Win10 1803+): gives sub-millisecond waits without touching global timer
   resolution. This avoids the system-wide side effect entirely.

2. **`spin_loop()` on an already-pinned core**: tighter still — the thread
   is already pinned to a physical core, so a spin-loop wait has
   deterministic jitter without any OS timer involvement.

**Justification for keeping `timeBeginPeriod`:** The trading loop needs
precise sleep intervals for pacing between ticks. `CreateWaitableTimerExW`
requires an OS callback or wait mechanism (async, which §24 bans on the
hot path). `spin_loop()` burns CPU cycles that could be used for
computation. `timeBeginPeriod` with per-process scoping (Win10 2004+) is
the least-surprising option that stays on the hot-path law's right side.
The adapter states this justification explicitly.

### What future change invalidates this

- Removing the `timeGetDevCaps` range check and hardcoding a period.
- Failing to pair `timeBeginPeriod` with `timeEndPeriod` (leaked
  high-resolution timer).
- Using this on the hot path (it is a startup-time call only).

### // SAFETY: comment

```rust
// SAFETY: `period` was validated against `timeGetDevCaps.wPeriodMin` in
// safe code above. The call is paired with `timeEndPeriod` via the RAII
// guard's Drop (which is best-effort, not guaranteed under abort/forget).
// The failure mode is raised timer resolution, not UB. Win10 2004+ scopes
// the effect per-process.
unsafe { timeBeginPeriod(period) }
```

---

## Block 4: VirtualLock

### What the compiler cannot verify

`VirtualLock(LPVOID, SIZE_T)` locks pages resident in physical memory. The
compiler cannot verify that the pointer is valid, the length is correct, or
that the working set quota is sufficient.

### Preconditions established in SAFE code

- **Provenance:** The pointer and length are derived from an allocation the
  guard OWNS (a `Box<[u8]>` or `Vec<u8>`). Provenance is provable: the
  guard holds the allocation, so the pointer cannot outlive the locked
  region. The adapter does NOT accept external raw pointers for locking —
  it owns the allocation.

- **Page alignment:** `GetSystemInfo().dwPageSize` gives the page size. The
  adapter page-aligns the region (rounds the pointer down and the length
  up to page boundaries).

- **Working set:** `SetProcessWorkingSetSizeEx(handle, min, max, flags)`
  raises the working set minimum BEFORE `VirtualLock`, or
  `ERROR_WORKING_SET_QUOTA` (1453) is the expected failure. The adapter
  calls this first and fails closed on error.

### The llama.cpp interaction — the one place this can take the machine down

This box hosts 238 GB of llama.cpp commit. The system commit limit is
383.6 GB. `VirtualLock` pins pages **non-pageable** — they cannot be paged
out under memory pressure. If the locked region is large, it reduces the
 pageable pool available to llama.cpp and every other process.

The adapter MUST state:
- The bounded region size (justified — e.g., the hot-path ring buffers
  and the engine's bounded maps, NOT the entire process).
- The llama.cpp interaction: ~257 GB of host commit against a 383.6 GB
  limit, and VirtualLock pins pages non-pageable.
- The fail-closed behavior: on `ERROR_WORKING_SET_QUOTA` or any failure,
  the region is NOT locked, the tuning run records the failure, and the
  bot runs without memory pinning (§4.5).

### Failure modes

- `GetLastError() == ERROR_WORKING_SET_QUOTA` (1453) → `Err(OsErr::Denied)`.
  The region is not locked. Fail closed.
- Partial lock: `VirtualLock` may lock fewer pages than requested. The
  adapter returns the actual count locked, never `len`. Obligation 4
  catches a partial lock.
- `VirtualUnlock` in `Drop` releases the pages. Not guaranteed under
  abort/forget, but the failure mode is pages remaining resident (not UB).

### What future change invalidates this

- Accepting an external raw pointer not owned by the guard (provenance
  break).
- Locking an unbounded region (the production memory leak we just fixed).
- Skipping `SetProcessWorkingSetSizeEx` (guaranteed 1453 on large regions).
- Using this on the hot path (startup-time only).

### // SAFETY: comment

```rust
// SAFETY: `ptr` and `len` are derived from a `Box<[u8]>` the guard owns
// (provenance provable). The region was page-aligned via
// `GetSystemInfo().dwPageSize`. The working set was raised via
// `SetProcessWorkingSetSizeEx` in safe code above. The region is bounded
// (justified: hot-path ring buffers only, NOT the process). The llama.cpp
// interaction is stated: ~257 GB host commit / 383.6 GB limit; VirtualLock
// pins non-pageable pages.
unsafe { VirtualLock(ptr, len) }
```

---

## Cross-cutting: all four blocks

### Fail-closed (§4.5)

A failed pin, priority set, timer resolution, or lock aborts the tuning
run. The bot runs unpinned and records the failure. It NEVER continues
silently while reporting tuned numbers. All four are startup-time calls;
none may run on the hot path.

### hotpath_lint coverage

`hotpath_lint` covers `rust/crates/pump-quant-core/src/**/*.rs`. The
adapter module, when written, MUST be inside this glob. The adapter must
contain NO `async`, NO `tokio`, NO `serde_json`, NO floats, NO panics, NO
per-event allocation, NO syscall clocks on the hot path. All four calls
are startup-time; the hot path never touches them.

### Registration

This dossier is a DRAFT. Registration in
`supervisor/reinforcement/dossiers/cpu_numa_tuning.yaml` is the operator's
act under §68/criterion 111. No unsafe blocks are written until the
operator signs off.
