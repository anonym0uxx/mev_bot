# LATENCY — hot-path optimization record (Phase-A, laptop build box)

Authoritative record of the code-level latency work on the trade hot path. Every change
here is **behaviour-preserving**: the decision-journal digest, realized net-SOL, and the
191 SHA-locked dossier tests are byte-identical before and after (proven, not asserted —
see *Behaviour proof*). Absolute-CPU tuning (`target-cpu`, PGO, core/NUMA pinning) is
Phase-B and is specified at the end; it is deliberately not applied on the build box (§24).

## Where the time goes

The bot's steady state is the `pump-quant-app` `evaluate()` loop run once per `Tick`:
discovery (four lanes emit → union-dedup) → watchlist insert → prune → promote → gate →
scalp → reflect. A dependency-free harness (`bench/`, a standalone crate that is **not** a
workspace member, so the gated workspace never sees its wall-clock code) measures the three
integer kernels plus the whole-tick cost at 64 / 256 / 1024 tracked mints against a
capacity-64 watchlist.

The baseline profile showed the per-tick cost scaling **super-linearly** — 64→256 mints (4×
the mints) cost ~18× the time. The cause was structural, not micro:

- **`WatchlistState::insert` full path** recomputed the rank of *every* retained entry to
  find the weakest, on *every* incoming candidate — O(candidates × capacity) per tick. This
  dominated at scale (at 1024 mints, ~60k rank evaluations per tick).
- **The merge path** (a re-discovered mint, which after warm-up is *every* live candidate
  every tick) allocated a throwaway 2-element `BTreeMap` via `ingest_union([existing, cand])`
  just to run the evidence comparator — one heap allocation per live candidate per tick.
- **Discovery** allocated five `Vec`s per tick: one per lane's `emit()` plus the union
  buffer, all thrown away immediately.
- **`decade()`** ran a divide-by-10 digit-count loop where an intrinsic exists.

## What changed (all behaviour-preserving)

1. **Memoized weakest-entry eviction** (`watchlist::state`). The weakest entry by rank is a
   pure function of the entry set at a fixed `now`. A `(now, rank, mint)` memo is filled by
   the first full-path scan of a tick's insert batch and reused by every subsequent
   non-evicting insert; any mutation (eviction, record replacement, growth, prune) or a new
   `now` clears it. In steady state most incoming candidates do **not** outrank the retained
   top-`capacity` (the ones that do are already in the set → merge path), so the O(capacity)
   scan runs ~once per batch instead of once per candidate: per-tick insert collapses from
   O(candidates × capacity) toward O(candidates + capacity). This is the win that removes the
   super-linear scaling.
2. **Alloc-free watchlist merge** (`watchlist::state` + `lane_ingest::evidence_cmp` made
   `pub`). The merge path now calls the shared comparator directly instead of building a
   scratch `BTreeMap` — removes one heap allocation per live candidate per tick, identical
   result.
3. **Scratch-buffer discovery** (`app::engine` + `lane::emit_into`). The four lanes append
   into one reused buffer (cleared, not freed) that the union consumes by reference; `emit()`
   is retained as the owning wrapper. Steady-state discovery allocates nothing.
4. **`decade()` → `checked_ilog10`** and `#[inline]` on the hot leaves (`evidence_cmp`,
   `score_rank`, `recency_factor`, `evidence_strength`, `buy_pressure_bp`, `decade`).

## Before / after (build box, release; absolute ns are box-specific)

Compare deltas, not absolutes — this laptop is the temporary build box, not the deploy CPU.

| scenario (per-tick) | p50 before | p50 after | speed-up |
|---|---:|---:|---:|
| engine tick, 64 mints   |    20,957 ns |    16,848 ns | ~1.24× |
| engine tick, 256 mints  |   372,440 ns |   149,729 ns | ~2.49× |
| engine tick, 1024 mints |  1,551,412 ns |   406,415 ns | ~3.82× |

`min` at 1024 mints fell 1,439,929 → 368,139 ns (3.9×). The integer kernels were already
optimal and are unchanged: `mul_div_u128` ~4 ns, `decode_pump_curve` ~1 ns. The 64-mint case
gains less because at capacity-64 the watchlist is never over-full, so the eviction scan —
the thing that was quadratic — barely fires; its win there is purely the removed allocations
(which also cut allocator-driven tail jitter).

## Behaviour proof

`rust/crates/pump-quant-app/tests/golden_digest.rs` drives a 72-tick, all-four-lanes,
eviction-heavy (512 mints vs capacity 64), confirm/prune/promote/reflect scenario and pins
the exact outcome: `journal_digest = 14000818526377800221`, `net_lamports = 8785954`,
`promoted/admitted/rejected = 432/15/417` (re-pin #4, `tests/golden_digest.rs`). This digest
was re-verified **unchanged** across the optimization work. Plus: 433 workspace test binaries
are green, `materialize_tests.py --verify` confirms all 191 SHA-locked dossier tests intact,
and `clippy -D warnings` + the supervisor portable gate pass.
(machine-verified 2026-07-22; regenerate via
`cargo test -p pump-quant-app --test golden_digest -- --nocapture`)

## Deferred — spec-gated, NOT drop-in

Two further speed-ups change observable semantics and therefore require a spec decision and
re-materialized dossier tests before implementation; they are **not** included above:

- **Dirty-set discovery** — only re-emit lanes for mints touched since the last tick. Changes
  which candidates carry which `discovered_at`/recency and so would move the digest.
- **Event-driven confirm fast path** — gate a mint immediately on its on-chain confirm rather
  than at the next `Tick`. Changes decision ordering and timing in the journal.

## Phase-B — absolute-CPU tuning on the EPYC 9655P (deploy box)

The deploy target is an **EPYC 9655P** (Zen 5, 96 cores, AVX-512 + VNNI, ~384 MB L3,
12-channel DDR5) with 3× RTX 6000 WS. Apply on the server, never on a build box:

- **`RUSTFLAGS="-C target-cpu=znver5"`** (fall back to `znver4` if the toolchain lacks the
  Zen 5 model) — unlocks AVX-512/VNNI and the correct scheduling model. Inject from the infra
  manifest's deploy-CPU entry; **never `-C target-cpu=native` on a build box** (§24, and
  SERVER_BUILD_MANIFEST §5).
- **PGO** on a recorded-replay profiling run, then rebuild.
- **Core/NUMA pinning** via the already-built `pump_quant_core::cpu_numa_tuning` planner +
  the Windows `OsTune` impl (SERVER_BUILD_MANIFEST §1); park the hot decision thread on one
  CCD so the huge shared L3 keeps the watchlist + confirmed set resident, isolated from the
  model-inference process (criterion 22).
- **The 3× RTX 6000 GPUs are irrelevant to the CPU trade hot path.** The decision loop is
  integer, single-threaded, and cache-resident; GPUs serve only research, backtest,
  Monte-Carlo, and LLM inference — off the critical path. No hot-path code should touch CUDA.

Re-run `bench/` on the deploy CPU after these to capture the real deploy-box numbers; the
laptop figures above exist only to prove the *algorithmic* deltas.
