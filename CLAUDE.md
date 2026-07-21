# CLAUDE.md — Build orientation for Claude Code

You are building the **production Rust codebase** for an autonomous Solana scalping system.
This file tells you what to build, to whose specification, and — critically — what you may
complete on this machine versus what must wait for the deployment server.

## The authority is the constitution, not this file and not your judgment

The single authoritative specification is **`docs/HERMES_ONE_SHOT_PROMPT.md`**. Read it
before writing code. Every design decision, parameter, and architectural boundary is governed
there. Where this file and the constitution appear to differ, the constitution wins. Where your
instinct and the constitution differ, the constitution wins. A "cleaner" or "simpler" approach
that violates a constitutional requirement is a defect, not an improvement.

## What you are building toward: the goal system, not this laptop

The deployment target is a **bare-metal Windows server: 3× RTX 6000-class GPUs, AMD EPYC 9655
(96 cores), Windows-native**. You may be running on a different machine (a laptop). That is
fine and expected — but you are writing source for the server, under the constitution's
**§9.5 two-phase build boundary**:

- **Phase A (here, now — the majority of the work):** author all production source and its
  unit/property tests. It must compile and pass logic tests under the **portable/dev compile
  profile** (`cargo build`, `cargo test` — portable codegen, default `target-cpu`). This is
  where the crates, algorithms, fixed-point math, reducers, decoders, exit ladders, and risk
  gates are legitimately completed.
- **Phase B (deployment server only — do NOT attempt here):** deploy-CPU `target-cpu` pinning,
  replay-corpus PGO, Windows OS/runtime tuning *measurement*, all microsecond hot-path latency
  budgets (criteria 103/109), and live submission-surface warmth. **Write this code and its
  configuration now, but leave it inactive and never mark it complete from this machine.**

### The rules that keep Phase A honest (do not break these)

1. **Never weaken a release-profile setting to make a build succeed here.** The release profile
   in the workspace `Cargo.toml` carries the correct `target-cpu` placeholder and PGO wiring for
   the server. Your build target on this machine is the **portable/dev profile**. Do not edit the
   release profile to compile locally — build the dev profile instead.
2. **Never mark a Phase-B criterion satisfied.** If a task's completion depends on microsecond
   latency, PGO, hardware tuning measurement, or a live endpoint, author the code and STOP there.
   Its validation is a server task. Say so explicitly rather than claiming completion.
3. **Never represent a local benchmark as meeting a hot-path budget.** Local timings are not the
   criterion-103 budget. The supervisor's gate will (correctly) reject any latency claim that
   lacks deployment-hardware provenance.

## The supervisor judges correctness, not you

For every **hard component**, the definitions and correctness authority live in the dossiers
under `supervisor/reinforcement/dossiers/*.yaml`. Each dossier decomposes a component into
leaves with an exact Rust **signature**, **invariants**, and a **property test that is the sole
authority over correctness**. When you implement a hard component:

- Implement to the dossier's signature and satisfy its property test. Do **not** invent your own
  definition of "done," and do **not** author or weaken the property tests — an independent
  process owns them, and you grading your own work is the exact circularity the constitution
  forbids ("a model claim is never evidence").
- If no dossier exists for a component you believe is hard, flag it. Do not fabricate one.

Run the supervisor's gates rather than self-certifying. A milestone is complete when its gate
passes, not when you believe the code is right.

## Production code discipline (from the constitution — non-exhaustive)

- **Rust only** for production code, in the workspace crates. No Python in production paths.
- **Integer/fixed-point for all money and percentages.** No `f32`/`f64` in any outcome-controlling
  path (§22). Money math cannot silently wrap under any profile — use checked/saturating ops.
- **Hot path:** no `async`/`await`, no lock-guarded channels, no allocation on the hot path,
  no hot-path I/O (§24/§57). Pinned threads + SPSC rings, pre-armed transactions.
- **No hardcoded magic numbers** in strategy behavior — every parameter is derived,
  admission-tested, or declared static-by-design with rationale (§ hardcoded-parameter law).
- **Windows-native**: no `mlockall`, `io_uring`, `epoll`, or Linux CPU-isolation assumptions.
  The repo's `system/tuning.rs` is a Linux-ism to be replaced with a portable Windows
  implementation (authored in Phase A, measured in Phase B).
- **Fail closed** on unknown instruction versions, account orders, fee schedules, or quote-mint.

## Workflow

1. Read `docs/HERMES_ONE_SHOT_PROMPT.md` and the relevant dossier(s) before writing.
2. Implement to spec against the dev/portable profile; make the property tests pass.
3. Commit in focused, reviewable pieces with messages that reference the component/criterion.
4. Push to the remote repo. The deployment server pulls, activates the Phase-B profile, and runs
   the hardware-specific validation there.
5. For anything Phase-B-exclusive: author, wire, leave inactive, and hand off — do not complete.

State what is built and verified, what is authored-but-Phase-B-deferred, and what is unknown or
blocked. Do not describe Phase-B work as finished. Do not tell the operator what sounds good;
state what is actually true.


## Running under the autonomous build driver

When invoked by `auto_build.py` (headless, `-p` mode), you implement ONE task at a time and stop.
The driver — not you — runs the supervisor gate, commits on green, iterates with you on red (it
feeds you the gate findings), pushes, and opens the PR. You never commit, push, or merge; those
tools are denied to you in `.claude/settings.json` by design, and the release profile and
Cargo.toml are protected. This division is the point: you write code, the gates decide if it is
correct, CI authorizes the merge. A confident-but-wrong change from you is caught by a gate, not
shipped.

If a gate rejects your work, fix exactly what its findings identify and stop — do not broaden
scope, weaken a test, or touch a protected file to make it pass. If you believe the gate itself
is wrong, say so in your result rather than working around it.


## Your surface (§69): authoring agent, not conductor

You are Surface 1 of the constitution's §69 two-surface map. Binding consequences:

- **Do NOT perform Milestone M0's infrastructure verification** (Helius entitlements/credits/
  endpoints, Jito status, Docker boundaries, live-wallet controls). You cannot inspect those from
  this machine. Implement M0's *code* deliverables and mark each verification item
  **SERVER-DEFERRED** explicitly in your report — never claimed, never fabricated.
- **Do NOT call hermes-supervisor MCP tools** and do not claim MCP-verified status. Your
  certification path is the build driver's gate battery plus CI, which run the same checks.
- **Do NOT produce the §65 first-response audit.** That format binds Hermes on the server. Your
  first action: read `docs/HERMES_ONE_SHOT_PROMPT.md` and the dossiers, then begin the lowest
  incomplete milestone's Phase-A code.
- **Never edit a materialized dossier test** (`rust/**/tests/dossier_*.rs`). They are the
  correctness authority, hash-verified by every gate and CI; editing one is a build-integrity
  violation caught mechanically.
- Work in milestone order (M0 code → M1 → …). Anything requiring the live server, real
  endpoints, or deployment hardware is SERVER-DEFERRED, stated plainly in your result.
