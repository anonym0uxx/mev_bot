---
description: Implement one build milestone to constitution spec, portable profile only
---

You are implementing a single milestone of the Hermes production build.

**Authority:** `docs/HERMES_ONE_SHOT_PROMPT.md` is the specification. Read the milestone's
section and its scoped acceptance criteria before writing any code. For any HARD component,
read its dossier under `supervisor/reinforcement/dossiers/<component>.yaml` and implement to the
leaf signatures so the property tests pass — do not author or weaken those tests.

**Milestone to build:** $ARGUMENTS

**Rules (from the constitution — non-negotiable):**
- Rust only, in the workspace crates. Integer/fixed-point money (§22); no floats in
  outcome-controlling paths.
- Hot path: no async/await, no lock-guarded channels, no allocation, no I/O (§24/§57).
- No hardcoded magic numbers in strategy behavior (hardcoded-parameter law).
- Windows-native: no mlockall/io_uring/epoll/Linux CPU-isolation assumptions.
- Build and test against the **portable/dev profile only** (`cargo build`, `cargo test`).
  Never edit the release profile to make a local build pass. Never run `cargo build --release`.
- **Phase B is off-limits here (§9.5):** do not attempt or mark complete anything requiring
  deploy-CPU codegen, PGO, OS/runtime tuning measurement, microsecond latency budgets, or live
  endpoints. Author that code if the milestone calls for it, wire it, leave it inactive, and say
  so — its validation happens on the deployment server.

**Loop for each unit of work:**
1. Implement to spec.
2. Run `cargo fmt`, `cargo clippy`, `cargo test` for the crates you touched.
3. Fix what they surface. Repeat until green.
4. Stop and report: what is built and portable-profile-verified, what is authored-but-Phase-B-
   deferred, and anything that needs an operator decision.

Do not commit, push, or merge — the build driver and CI own those. State what is verified and
what is not; never describe Phase-B work as finished.
