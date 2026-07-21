# Hermes Supervisor — Drop-In Setup (minimal effort)

## Fully automated path (two commands total)

**Locally — assemble the repo from artifacts (once):**
```bash
python assemble_repo.py --dest ./pump-quant \
  --constitution ./HERMES_ONE_SHOT_FINAL.md --context ./PROJECT_CONTEXT.md \
  --setup ./SETUP.md --bootstrap ./HERMES_BOOTSTRAP_PROMPT.md \
  --supervisor-zip ./hermes-supervisor.zip --legacy ./mev_bot-main
```
This lays out `docs/HERMES_ONE_SHOT_PROMPT.md` (the one hardwired path), `docs/dossiers/`,
the `rust/` workspace scaffold, a secrets-blocking `.gitignore`, and quarantines legacy code —
then prints the exact `git add/commit/push` commands. It never clobbers existing files.

**On the server — install and wire everything (once):**
```bash
python install.py --repo-url <private-repo-url>   # or set env HERMES_REPO_URL + GITHUB_PAT
```
This clones the repo (PAT stored in the OS credential store, never a file), installs supervisor
deps, installs `mcp` into the Hermes interpreter, writes `supervisor.yaml`, merges the MCP block
into `~/.hermes/config.yaml` (with backup), copies the skill, **verifies all 9 dossiers load**,
places the scaffold if missing, runs the offline tests, probes llama.cpp, and prints the go/no-go
summary plus the exact message to send Hermes. Re-run `python install.py --check` any time to
re-verify the whole chain without changing anything.

Then: restart Hermes, send the bootstrap message. That's it.

---

## Manual path (if you prefer to place files yourself)

Goal: get from "files on disk" to "running the build loop" with the fewest steps.

## Prerequisites (one time)
1. **Python 3.11+** on the RTX 6000 Windows server.
2. **Rust toolchain** (the supervisor calls `cargo`; install rustup + the msvc target).
3. **GLM-5.2 GGUF** served by **llama-server** (llama.cpp). See `supervisor/config/llama_server.yaml`
   for the recommended launch flags — the one thing that matters is that the server exposes the
   OpenAI-compatible endpoint with `json_schema` support (all current llama.cpp builds do).
4. Your **Hermes bot repo** checked out, with the constitution at `docs/HERMES_ONE_SHOT_PROMPT.md`.

## Install (one command)
```bash
cd hermes-loop
pip install -e .            # installs deps + the `hermes-supervise` command
# or, no-install:  pip install -r requirements.txt
```

## Configure (edit ONE file)
Open `supervisor/config/supervisor.yaml` and set three paths:
```yaml
repo_path:          "C:/path/to/your/hermes-repo"
constitution_path:  "C:/path/to/your/hermes-repo/docs/HERMES_ONE_SHOT_PROMPT.md"
evidence_db:        "C:/path/to/supervisor/evidence.db"
```
Everything else has working defaults. (Model endpoint defaults to `http://127.0.0.1:8080`.)

If you want Telegram escalations instead of console:
```yaml
escalate_channel: "telegram"
```
then set env vars `SUPERVISOR_TG_TOKEN` and `SUPERVISOR_TG_CHAT` (never inline secrets).

## Run
```bash
# 1. confirm the model endpoint is alive
hermes-supervise health --config supervisor/config/supervisor.yaml

# 2. run the build loop from milestone M0
hermes-supervise build --from M0 --config supervisor/config/supervisor.yaml

# 3. after the build, run the standing research loop
hermes-supervise research --config supervisor/config/supervisor.yaml

# anytime: milestone / criteria / escalation dashboard
hermes-supervise status --config supervisor/config/supervisor.yaml
```

## What runs without you
- The loop proposes → gates → commits → advances autonomously while gates pass.
- It **stops and escalates** (console or Telegram) only on: retry exhaustion, a Tier-0 boundary
  (keys / live capital / evaluator / funds), an unreachable model, or a single hard leaf that
  can't pass at max scaffold. Those are the only times it needs you.

## Dossiers: authored and shipped (previously the one piece needing authoring)
All 9 HARD-component dossiers now ship in `supervisor/reinforcement/dossiers/` — reducer, replay,
shred, lockfree, scalp_position, fixedpoint, exit_ladder, evaluator_stats, cpu_numa_tuning —
39 leaves, loader-validated, dependency orders clean. Every `temperature_band: high` leaf carries a
full frontier reference implementation in its `reference_pattern`, explicitly labeled UNVERIFIED:
the per-leaf property test remains the sole authority, and the reinforcement engine injects both
into every leaf prompt with the test framed as judge. Nothing further needs authoring before any
milestone; the loader reports zero missing dossiers. If a dossier is later revised, keep the schema
exact (`Leaf(**l)` rejects unknown keys) and re-run `install.py --check`.

## Test it (proves the supervisor itself works)
```bash
pip install pytest
pytest -q          # 18 tests, all offline (no model/cargo needed)
```

## Safety, always on
- No advancement on a model claim — only on a passing gate.
- Tier-0 (keys/live-capital/evaluator/funds) always halts and asks you.
- Research loop can simulate and propose but never promotes to live capital without your gate.
- Everything journaled + hashed to the constitution commit; every run is resumable.


## Building the majority on a laptop with Claude Code (Phase A)

The constitution's §9.5 two-phase boundary (criterion 113) lets you build most of the
production codebase on any machine — including a laptop — with Claude Code (running a frontier
model), and defer only hardware-specific work to the server. To do this:

1. Clone the repo on the laptop and confirm `CLAUDE.md` is at the repo root (it orients Claude
   Code to the constitution as authority, the dossiers as the correctness authority, and the
   Phase-A/Phase-B split — including the rule that it must never weaken the release profile to
   compile locally, and never mark a Phase-B criterion complete).
2. Copy `supervisor/config/infra_manifest.example.json` to `infra_manifest.json` and fill
   `current_machine` with the laptop's identity. Because it is NOT the deployment host, the
   supervisor automatically treats all work as Phase A: authoring passes, but any bench/latency/
   PGO/tuning gate fails closed with an explanation. This is the safety net that stops a laptop
   from certifying hardware-specific criteria.
3. Run Claude Code in the repo. It builds against the portable/dev profile (`cargo build`,
   `cargo test`), implements hard components to the dossier signatures, and makes the property
   tests pass. Commit and push in focused pieces.
4. On the server later, copy the same manifest with `current_machine` = the server's identity.
   Now `current_machine.machine_id == deployment_host.machine_id`, the supervisor recognizes
   Phase B, and the hardware-specific gates (deploy-CPU codegen, PGO, tuning measurement,
   criterion 103/109 latency budgets, live endpoint warmth) become certifiable.

Division of labor is unchanged: Claude Code (or GLM) writes Rust toward the property tests; the
supervisor's gates decide whether it passes. Neither model certifies its own milestones.

---

# The automated build: pick the repo, let Claude build it

This is the end-to-end automated flow. The driver runs Claude Code headlessly per milestone,
gates every result, commits on green, iterates on red, pushes, and opens a PR that CI gates
before merge. **The gate decides correctness, never the model** — that is what makes it safe to
walk away from.

## The two surfaces (§69) in one line

Claude Code on the laptop = Surface 1: writes Phase-A code, gated by driver+CI+materialized
dossier tests; never does infrastructure verification, never touches MCP supervisor tools,
marks server-only items SERVER-DEFERRED. Hermes on the server = Surface 2: verifies the pulled
repo as evidence, does M0 infra verification + Phase B + the §65 audit + the live loop. The
repo is the only seam.

## What is automated vs what stops for you

Automated, unattended: implement → `cargo test` + supervisor gate → commit → push → PR →
merge-on-CI-green, for every non-hardware milestone.

Stops for you (by design): (1) any Phase-B milestone off the deployment server — bench, latency,
PGO, tuning fail closed on a laptop per §9.5; (2) any gate failure Claude Code can't resolve in
N iterations — it escalates and halts rather than pressing on; (3) the final merge to main, if
you leave branch protection requiring your review. None of these are limitations to remove; they
are the guardrails that stop a confident-but-wrong trading system from shipping.

## PHASE 1 — Laptop build (the majority of the code)

**1.1** Install prerequisites on the laptop: Python 3.11+, Git, Rust (`rustup`, stable), and
Claude Code (`npm install -g @anthropic-ai/claude-code`; needs v2.1.52+ for the automation
flags). Optionally the GitHub CLI (`gh`) for automatic PRs. Authenticate Claude Code (`claude`,
follow the login) and `gh auth login` if using it.

**1.2** Clone your repo and assemble the constitution + supervisor into it:
```
git clone <your-repo> C:\hermes\pump-quant
cd C:\hermes\hermes-loop
python assemble_repo.py --dest C:\hermes\pump-quant \
  --constitution "C:\hermes\artifacts\HERMES ONE SHOT FINAL.md" \
  --context "C:\hermes\artifacts\PROJECT CONTEXT.md" \
  --setup "C:\hermes\artifacts\SETUP.md" \
  --supervisor-zip "C:\hermes\artifacts\hermes-supervisor.zip" \
  --force-docs --no-git
```
This places `docs/HERMES_ONE_SHOT_PROMPT.md`, the dossiers, the `rust/` workspace, and now the
automation scaffold: `.claude/settings.json` (the permission allowlist), `.claude/commands/`,
`.github/workflows/gate.yml` (CI), `scripts/`, `CLAUDE.md`, and `infra_manifest.example.json`.

**1.3** Commit and push the assembled repo so CI exists on GitHub:
```
cd C:\hermes\pump-quant
git add -A && git commit -m "constitution v113 + dossiers + scaffold + automation" && git push
```
Then in GitHub → Settings → Branches, add a protection rule on `main` requiring the
`hermes-gate / portable-gate` check. This is what makes auto-merge safe.

**1.4** Generate the laptop's manifest (measured, not typed):
```
python scripts\gen_manifest.py
```
That's it — no editing. The phase gate MEASURES the live machine at check time, and since this
laptop is not the declared deployment host (declared later, on the server), all hardware gates
fail closed here automatically. Claude Code and Hermes can create/refresh this file and append
facts freely; none of it can forge Phase-B provenance, because identity is measured and the
declaration is operator-pinned.

**1.5** Preflight, then launch the unattended build:
```
python install.py --repo C:\hermes\pump-quant          # verifies dossiers load, places scaffold
python scripts\preflight.py --repo C:\hermes\pump-quant  # READY / NOT READY
python auto_build.py --repo C:\hermes\pump-quant --from M0 --dry-run   # see the plan first
python auto_build.py --repo C:\hermes\pump-quant --from M0 --max-usd 25 --max-hours 4
```
Add `--auto-merge` once you trust the loop and branch protection is on. The driver stops at the
first Phase-B milestone with a clear message; everything up to it is built, gated, pushed, and
PR'd. Re-run with `--from <milestone>` to resume after resolving any escalation.

## PHASE 2 — Server finalization (the hardware-specific remainder)

**2.1** On the finished server, install the same prerequisites plus CUDA 13.x (not 13.2), VS
2022 Build Tools, and CMake. Clone the repo (or pull, since the laptop pushed it):
```
git clone <your-repo> C:\hermes\pump-quant   (or: cd C:\hermes\pump-quant && git pull)
```

**2.2** Declare and pin the deployment host — this is what unlocks Phase B, and it is a
two-step act (measure, then human pin) so no agent can forge it:
```
python scripts\gen_manifest.py --declare-deployment-host --target-cpu znver5
hermes-supervise pin-manifest --manifest infra_manifest.json
```
The first command writes `deployment_host` from the server's MEASURED machine GUID (it refuses
hostname fallbacks). The second is HUMAN-ONLY — it hash-pins the declaration in the evidence
store, exactly like pin-evaluator. From now on the phase gate requires: live measurement ==
pinned declaration. Rewriting the manifest breaks the pin and fails closed; appending facts
(the agent-writable lane, via `gen_manifest.py --add-fact` or the `record_infra_fact` MCP tool)
never touches the pin. Criterion 109 derives `-C target-cpu` from the declared value.

**2.3** Preflight on the server (it should now report `phase B`):
```
python scripts\preflight.py --repo C:\hermes\pump-quant
```

**2.4** Finish the Phase-B milestones — the hardware-specific ones the laptop deferred:
```
python auto_build.py --repo C:\hermes\pump-quant --from <first-Phase-B-milestone>
```
On the server these no longer fail closed: the release-profile `target-cpu` codegen, replay-corpus
PGO, Windows OS/runtime tuning measurement, the criterion-103/109 microsecond latency budgets, and
live submission-surface warmth are all validated here with real hardware provenance in the
artifact record.

**2.5** Bring up the model and the live conductor (the GLM path from earlier in this doc): start
llama-server with the Q2 GLM config, run `recall_test.py`, then hand the running system to Hermes
with the bootstrap prompt. From here the supervisor's live loop and the standing research loop
take over.

## The safety model in one paragraph

Claude Code can edit source and run the portable toolchain, nothing more — it cannot commit,
push, merge, run the release build, or touch keys/Cargo.toml/the release profile (denied in
`.claude/settings.json`). The driver commits only after a gate passes, pushes only after a
milestone gate passes, and merges only after CI (which re-runs the gate) passes. Phase-B criteria
cannot be certified anywhere but the server. Every subprocess is checked and bounded; spend and
wall-clock are capped; a failed gate reverts the tree and a stuck agent hits a timeout. You can
start it and leave; it stops itself the moment it reaches something only you or the server can do.
