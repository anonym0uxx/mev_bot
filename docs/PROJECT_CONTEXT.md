# PROJECT CONTEXT — Hermes Solana Memecoin Scalping Bot

Compact orientation doc. Read this first; it points at the authoritative artifacts.
(Everything here is a summary. The constitution is the binding source of truth.)

---

## 1. What is being built

An **autonomous Solana low-market-cap memecoin SCALPING bot.** The goal is to harvest **net SOL
per unit time across many short opportunities** — NOT to ride rare moonshots. Starting wallet
~0.75 SOL with a hard survival floor of `max(0.5 SOL, floor_fraction × verified_balance)`.

The overarching lens is scalping; moonshot/tail capture belongs only to the early-entry and
graduation lanes, and the two objectives are constitutionally forbidden from blending.

## 2. The three artifacts

| Artifact | What it is | Where it lives |
|---|---|---|
| **HERMES_ONE_SHOT_FINAL.md** | The build constitution: ~395KB, 69 sections, 114 acceptance criteria. Binding spec. | Commit to the private repo as `docs/HERMES_ONE_SHOT_PROMPT.md` |
| **hermes-supervisor.zip** | Python MCP server + gate runner + reinforcement engine + installer. 25 modules, 40 tests. | Unzip to e.g. `C:\hermes\hermes-loop\` |
| **HERMES_BOOTSTRAP_PROMPT.md** | Short prompt to invoke the constitution in a session. | Repo alongside the constitution |

Supporting docs: `HERMES_SUPERVISOR_ARCHITECTURE.md`, `HERMES_HARD_TASK_REINFORCEMENT.md`,
`SETUP.md`, `llama_server_tuned.yaml`.

## 3. The build stack (who does what)

```
You --Telegram/CLI--> Hermes Agent (Nous Research; the conductor)
                          |  reasons, writes code, commits
                          |  powered by:
                          v
                    GLM-5.2 (754B glm-dsa MoE, UD-Q3_K_XL) on local llama.cpp
                          |
                          |  calls for verification (MCP tools):
                          v
                    hermes-supervisor  --> gates, Tier-0 checks, reinforcement,
                                           artifact registry, production tools
                          |
                          v
                    The bot (Rust) — what actually gets built and trades
```

**Hardware:** 3× RTX 6000 Pro (96GB each = 288GB VRAM), AMD EPYC 9655 (96C/192T),
256GB DDR5-4800 → ~544GB combined. GLM-5.2 runs 3–4-bit with modest MoE offload
(`--n-cpu-moe`). Expect ~10–15 tok/s generation.

## 4. Load-bearing principles (do not erode these)

1. **A model claim is never evidence.** Only a passing `gate_verify` certifies a milestone.
   Self-assessment is a claim; the gate battery (build/clippy/fmt/no-stubs/tests/secrets/
   bench/determinism) is truth.
2. **Tier-0 always halts for a human:** wallet keys, live capital, fund movement, frozen-evaluator
   release, promotion to live. Never auto-resumes.
3. **The agent cannot re-pin its own grader.** The evaluator hash is auto-pinned on first release
   (TOFU); any mismatch is Tier-0. Re-pinning requires the human-only `supervise pin-evaluator`.
4. **Build everything day 0; gate live capital behind proof.** (§62 build-consumption separation.)
   Capture runs immediately; unproven signals may not size live capital until they pass admission.
5. **Continuous-Improvement Mandate (§62).** Relentless autonomous hypothesize → branch → sweep for
   net-SOL edge. "No edge" is only ever a scoped verdict on a *specific tested approach*, obligating
   redirection — never terminal, never the market as a whole. But never fabricate edge either.
6. **Memory safety precedence (absolute):** durability > safety > optimization. Never drop
   reconciled/evidence data to save memory; never OOM-crash when shedding is possible.
7. **Fade-first on social.** Nearly all catalogued caller groups are paid whop/Discord communities
   posting survivorship-marketing. Every source enters at INSUFFICIENT_SAMPLE with the
   PUBLIC_BURNED presumption; most end up as *distribution* signals to fade.

## 5. Scalp-readiness (repo-grounded findings)

Verified by direct source inspection of the existing `rust/pump-quant-core`:
- Position state currently updates via `on_tick()` throttled to `check_ms` with ~500ms RPC polling
  and ~10s evaluation cadence → **must be rebuilt as per-swap event-driven** through the §22 reducer.
- `position.rs::evaluate_phase()` enforces a hard **1500ms minimum hold** → must become
  **lane-parametric** (near-zero for scalps; emergency/sellability exits always exempt).
- `TrailConfig` is tuned to "let moonshots run" (~11% trail at 40%+) → scalp lane needs its **own
  exit family**: fast fixed targets, per-swap hazard reversal, second-scale time-stops, dead-flow cuts.
- **Salvage, don't rebuild:** `sell_engine.rs` 5-level escalation ladder, integer scorer, velocity
  detectors, reconciler. Exit reliability *is* the scalp edge.
- **MinimumEconomicTradeGate is the primary scalp filter** — quote-mint round-trip cost floor at
  depth-supported size; judged on net SOL per unit time, never gross win rate or trade count.

## 6. Setup (condensed; full detail in SETUP.md)

```powershell
# prereqs: Python 3.11+, Git, Rust+msvc, llama.cpp, Hermes Agent, GLM-5.2 GGUF
$env:HERMES_GITHUB_PAT = "<fine-grained PAT, Contents: read/write, THIS repo only>"
python install.py --repo-url https://github.com/YOU/YOUR-REPO
# then: launch llama-server, `python install.py --check`, restart Hermes Agent
```
Kickoff message to Hermes:
> Build from docs/HERMES_ONE_SHOT_PROMPT.md. Follow the constitution exactly, including the
> §62 supervisor MCP tool mandate. Start at M0 and report gate results verbatim.

The installer handles: deps, `mcp` into Hermes's interpreter, repo clone, config writing,
MCP registration in `~/.hermes/config.yaml`, skill install, tests, health probe.
**The PAT is stored encrypted in the OS credential store — never in any file.**

## 7. Open items

- [x] ~~Two scalping upgrades~~ — DONE, stress-tested and patched as constitutional law:
      hold-horizon calibration (hazard from OWN fills, phase-conditioned, opportunity-cost-anchored
      stopping, hierarchical estimation), flow-authenticity (exit-liquidity-bearing vs fabricated,
      single-entry sizing, anti-pin), the hardcoded-parameter law (ten named repository defects
      incl. the frozen-peak trail bug and self-refuting TrailConfig), and the second-scale peak law
      (landing-state evaluation, pre-armed execution, exit-into-strength, burst features,
      leader-aware submission). Criteria 100–103, Experiments #9–#10.
- [x] Research-integration adds — DONE, stress-tested and patched (Reddit reconciliation + MemeTrans,
      Midsummer/LPI, Kalacheva first-block-bribe, Kamat PRFS/fragility literature): §21.7 launch-sale
      trajectory + creation-window competition families; LPI fabrication signature + manipulation-
      sequencing hazard; §24 entry-conviction covariate (covariate-never-dimension); §48 MFE-capture
      efficiency law (cost-floor-derived, no hardcoded ratio); §47 inactivity terminal-state labeling,
      per-lane top-k winner-excision fragility, explicit PRFS rejection forward-sampling; §18.3.4
      Triton/bloXroute/Astralane seeds. Criteria 104–108, Experiments #11–#12, §66 rules.
- [x] Build execution surfaces (§69, criterion 114) — DONE: two-agent/two-machine map made
      binding (Claude Code = Surface 1 authoring on laptop, no infra verification / no MCP tools /
      no §65 audit, SERVER-DEFERRED marking; Hermes = Surface 2 conductor on server, verifies
      gate-passing repo work as evidence and owns M0 infra + Phase B + §65 + live loop; repo is
      the only seam; §69.4 degenerate one-machine case collapses harmlessly). §62 HARD list fixed
      to 10 (economic_gate). §65 surface-scoped. Bootstrap prompt updated for verify-don't-rebuild
      resume. Dossier-test materializer (scripts/materialize_tests.py): renders all 46 leaf
      property tests into locked rust/**/tests/dossier_*.rs with hash manifest; --verify wired
      into task gate, milestone gate, and CI; .claude settings + hook deny edits; auto_build
      materializes at run start. Claude Code provably implements against tests it did not write.
- [x] Manifest hardening (v2) — DONE after the "can agents write the infra doc?" question
      exposed that the phase gate trusted a file's self-report (a laptop could forge deployment-
      host provenance by editing two strings). Rebuilt on a trust model that lets agents freely
      create/copy/refresh the manifest and append facts, while forgery is impossible:
      (1) build_phase.measure_machine() fingerprints the LIVE machine (Windows MachineGuid /
      Linux machine-id) at gate time — editing a file can't change what machine you're on;
      (2) the deployment_host declaration is hash-pinned by a HUMAN-ONLY CLI (hermes-supervise
      pin-manifest, mirrors pin-evaluator, absent from every MCP surface) — rewriting the
      manifest breaks the pin and fails closed; (3) an agent-writable facts ledger with the
      record_infra_fact MCP tool (15th tool) for Hermes's future provenance writes (Helius plan,
      Jito status, CPU features), journaled, and provably unable to affect certification;
      (4) scripts/gen_manifest.py measures-not-types (refuses hostname fallbacks for declaration,
      has an inline measurement fallback so it runs standalone). .claude/settings.json denies
      direct manifest edits and allows gen_manifest.py. Adversarial tests prove the edit-to-own-id
      attack fails on the pin. 97/97 tests green.
- [x] Automated build pipeline (laptop -> server) — DONE: closes the orchestrator's live
      VCS/exec seam. supervisor/core/live_build.py (real git-backed GitVcs adapter + headless
      ClaudeCodeDriver using `claude -p --permission-mode dontAsk --allowedTools/--disallowedTools
      --max-turns --output-format json`, + BudgetGuard for spend/time). auto_build.py runs the
      whole loop unattended: per milestone -> branch -> (Claude Code task -> supervisor gate ->
      commit on green / iterate on red / escalate+STOP) -> milestone gate -> push -> PR (gh) ->
      auto-merge on CI green. Repo scaffold adds .claude/settings.json (fail-loud allowlist: model
      may edit source + run portable toolchain, may NOT commit/push/merge/release-build/touch
      keys+Cargo.toml), .claude/commands/build-milestone.md, .github/workflows/gate.yml (portable
      CI gate, no bench/release — Phase B excluded), scripts/ci_gate.py + preflight.py. CLAUDE.md
      extended with autonomous-loop section. Assembler places all of it. THE GATE DECIDES, NOT THE
      MODEL: commit requires passing gate, push requires milestone gate, merge requires CI; main
      only via CI-gated PR; Phase-B criteria fail closed off the deployment host. 94/94 tests green.
- [x] Two-phase build boundary (criterion 113) — DONE: §9.5 lets the majority of the
      codebase be authored on any machine (Phase A — e.g. a laptop with Claude Code running a
      frontier model, sidestepping the GLM-Q2 recall risk for cold-start authoring) while a
      closed enumerated set (deploy-CPU target-cpu codegen, replay-corpus PGO, Windows
      OS/runtime tuning measurement, criterion 103/109 microsecond latency budgets, live
      submission-surface warmth) activates only on the server (Phase B). Supervisor enforces it:
      supervisor/gates/build_phase.py compares an infrastructure manifest's deployment-host
      fingerprint to the current machine; Phase-B gates (bench/latency) fail closed without
      deployment-hardware provenance; Phase-A authoring passes anywhere. CLAUDE.md orients Claude
      Code (spec authority = constitution, correctness authority = dossiers, no release-profile
      weakening, no self-certification of Phase-B criteria). infra_manifest.example.json shipped.
      78/78 tests green.
- [x] Size-viability band / fixed-cost floor (criterion 112) — DONE after Monte Carlo
      viability analysis on the repo's OWN reconciled data (on-chain audit 167 round trips
      −0.40 SOL + paper audit 3303 trades −8.47 SOL both imply ~3–5% real floor; config
      fixed costs made the traded 0.01-SOL size pay 6–11% fixed alone — position size, not
      entry quality, was the dominant killer; 55% of on-chain loss was unsellable inventory).
      Integrated intimately, scrutiny-hardened first (cost-min ≠ profit-max kept distinct;
      failure-rate attempt multiplier on fixed cost; rung-count cost pricing; probe-below-
      floor only as paid information; sub-x_min refused not shrunk): §34.4 size band, §49
      Layer 1 constraint, defect #6 strengthened, §24(c) rung cost-pricing, §66 rules. New
      economic_gate dossier (4 leaves) auto-registered via constitution discovery — proved
      the no-code-edit registration path end to end. exit_ladder el_partial_ladder revised
      to cost-price rungs. 10 dossiers now; 71/71 tests green.
- [x] Constitutional amendment subsystem — DONE (§68, criterion 111): separation of powers
      (builder proposes with resolvable evidence → independent design model drafts → operator
      alone approves via CLI verb absent from every model tool surface → supervisor applies
      validated/atomic/backed-up), Tier-0 byte-frozen, gates cannot be weakened, dedup against
      approval-fatigue flooding, milestone-boundary application with hash re-pinning. Supervisor:
      amendments table + lifecycle in evidence store, core/amendment.py validation, 3 MCP tools
      (propose/draft/status — no approve), `hermes-supervise amendments` CLI.
- [x] Design-model routing + auto-discovery — DONE: design_client.py (Anthropic API, env-key
      only, disabled by default), author_dossier MCP tool with loader validation before install,
      discovery.py derives HARD components from the constitution itself (no code edit, no file
      drop), repo-loaded lint rules via rust/lint_rules.yaml.
- [x] Rust performance-engineering law — DONE, patched into §24 (criterion 109): deploy-CPU-pinned
      codegen (never build-box native), replay-corpus PGO, panic-free hot set with panic=abort,
      money-wrap prohibition vs overflow-checks=false, CI zero-allocation hot path (no async/tokio/
      lock-channels; pinned threads + SPSC rings), measured allocator, byte-level pre-armed txs,
      Windows-native tuning (VirtualLock — libc mlockall named porting defect), connection warmth
      as monitored invariant, build-loop latency doctrine (sccache/workspace split//tmp bin-path
      defect/SDK narrowing; Cranelift+parallel-frontend dev-loop-only), tail-measured admission.
- [x] Hard-component dossiers — DONE, all 9 authored, loader-validated, and shipped inside
      hermes-supervisor.zip at supervisor/reinforcement/dossiers/ (39 leaves total; all 9
      temperature_band:high leaves carry full frontier reference implementations labeled
      UNVERIFIED — property tests remain the authority; criteria 104–109 obligations encoded
      in evaluator_stats / exit_ladder / scalp_position / cpu_numa_tuning). Superseded item:
      dossiers beyond `fixedpoint.yaml` (reducer, replay, shred, lockfree,
      scalp_position, exit_ladder, evaluator_stats, cpu_numa_tuning). Highest-value prep work —
      design once, GLM executes forever. Note: the evaluator_stats dossier must encode the new
      criteria 107–108 obligations (MFE-capture ratios, top-k excision fragility, inactivity
      terminal-state labeling, PRFS scheduled forward sampling); exit_ladder and scalp_position
      dossiers must reference the §24 entry-conviction covariate and manipulation-history hazard.
- [x] DexScreener boost-spend signal spec — DONE, authored by Fable and patched into the
      constitution as §29.10 (criterion 110, Experiment #13, §66 attention-spend rules):
      AttentionSpendSource neutral contract, D-class/off-hot-path/Missing-on-stale, versioned
      price tables, capture-forward evidence only, two-sided wiring (attention-injection
      catalyst class + persistent extraction-hazard input), crowding law with fade-the-boost
      registered at equal standing, and an absolute Tier-0-severity self-purchase prohibition.
      (Original rationale stands: money-behind-promotion, on-chain-adjacent, no ToS/account risk.)
- [ ] Discord research-identity credentials into the §29.7 placeholder at deployment.
- [ ] Server arrival → measure real tok/s (`bench_endpoint`), tune `--n-cpu-moe`.

## 8. Honest framing (kept deliberately)

The constitution is an **excellent build specification**. It does **not** prove an edge exists.
A good outcome may be the system cheaply reporting "no edge in these tested approaches" while
relentlessly searching — that saves capital rather than burning it. Reconciled net SOL is the
only judge; when the data disagrees with the document, believe the data.
