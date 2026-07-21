# Repo scaffold (copy into the bot repo at M0)

Copy `rust/` into the repo root (alongside the legacy code, which stays quarantined per M0).
It encodes the §24 performance-engineering law as build config:

- Release profile: opt3 / fat LTO / 1 CGU / strip / panic=abort (§24a).
- Money crates (`hot-core`, `evaluator`) keep `overflow-checks = true` in release — silent
  money wrap is prohibited under every profile (criterion 109).
- `-C target-cpu` comes from RUSTFLAGS via the infrastructure manifest's deploy-CPU entry.
  NEVER `native` on the build box.
- Crate boundaries match the supervisor's `hotpath_lint` gate scopes: `hot-*` crates are
  lint-enforced pure (no async/floats/clocks/panics); `ingest`/`enrich` are the tokio
  control plane; `evaluator` carries the money-float-cast ban.
- GLM fills crates via dossiers (`run_reinforcement`) and normal gated tasks; the scaffold
  itself contains no logic, only law-shaped structure.
