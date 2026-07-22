# Hermes Supervisor

Verification and reinforcement layer for the Hermes build. **Primary mode (Option A): Hermes Agent
(Nous Research) is the conductor** — you launch llama.cpp with GLM-5.2, start Hermes, and message it
(Telegram/CLI) to build from `docs/HERMES_ONE_SHOT_PROMPT.md`. The supervisor plugs into Hermes as an
**MCP server** (`supervisor/mcp/server.py`) exposing 16 independent verification tools: gate_verify,
check_tier0, run_reinforcement, author_dossier, propose_amendment, draft_amendment, amendment_status,
record_infra_fact, evidence_status, record_escalation, register_artifact, evaluator_verify,
experiment_run, promotion_check, live_status, bench_endpoint. Hermes calls them;
the constitution (§62) obligates it to. A standalone driver mode (`supervise.py build`) remains available
if you ever want the supervisor to drive GLM directly without Hermes.

GLM-5.2 is the sole builder either way. Hard components are ground through via best-of-N +
micro-decomposition + reference-pattern priming (see `HERMES_HARD_TASK_REINFORCEMENT.md`).

## Safety invariants (enforced in code, not convention)
1. Advancement requires a passing **gate result**, never a model report. (`gates/`)
2. Tier-0 actions (keys, live capital, evaluator release, fund movement) **break the loop and escalate**. (`core/safety.py`)
3. The research loop can propose/simulate but **never promotes to live capital** without a human gate.
4. Everything is journaled, hashed with the constitution's git commit, and **resumable**. (`store/`)
5. Bounded retries/wall-clock; the loop escalates rather than spinning forever.

## Module status (honest — same discipline the loop enforces)
| Module | Status | Notes |
|---|---|---|
| `core/schemas.py` | **real** | stdlib dict-schema control envelopes + GBNF export (deliberately no pydantic dependency) |
| `core/model_client.py` | **real** | llama.cpp OpenAI-compat client, GBNF-constrained control channel, retry/health |
| `core/safety.py` | **real** | Tier-0 tripwires, escalation triggers |
| `core/config.py` | **real** | typed config load/validate |
| `store/evidence.py` | **real** | SQLite journal: tasks, gates, commits, escalations, capability map |
| `gates/runner.py` | **real** | deterministic verifier: build/clippy/fmt/test/bench/secrets/criteria |
| `gates/checks.py` | **real** | individual check implementations (subprocess-based, cross-platform) |
| `reinforcement/engine.py` | **real** | micro-decompose → best-of-N → filter → select → integrate |
| `reinforcement/dossier.py` | **real** | dossier schema + loader for HARD components |
| `core/constitution.py` | **real** | parses milestone contract (§62) + acceptance criteria (§63) |
| `core/orchestrator.py` | **real** | build-loop FSM wiring the above |
| `research/loop.py` | **scaffold** | standing research cycle; real structure, integration points marked TODO(live) |
| `console/escalate.py` | **real** | CLI + pluggable Telegram escalation channel |
| `mcp/server.py` | **real** | MCP server for Hermes Agent (stdio JSON-RPC; 16 tools; tested) |
| `supervise.py` | **real** | entrypoint / CLI |

"scaffold" means: correct structure and interfaces, with clearly-marked `TODO(live)` where it must bind to
the actual bot processes that don't exist until the build produces them. Nothing is silently stubbed.

## Layout
```
supervisor/
  supervise.py            # entrypoint
  config/                 # yaml configs (llama_server, supervisor, gates, targets)
  core/                   # schemas, model client, safety, config, constitution, orchestrator
  gates/                  # gate runner + checks
  reinforcement/          # hard-task engine + dossiers
  research/               # standing research loop
  store/                  # evidence store (sqlite)
  console/                # escalation / operator channel
tests/                    # pytest suite for the supervisor itself (dogfooding)
```

## Quickstart (once GLM-5.2 is served)
```
python -m supervisor.supervise health          # check llama.cpp endpoint
python -m supervisor.supervise build --from M0  # run the build loop from a milestone
python -m supervisor.supervise research         # run the standing research loop (post-build)
python -m supervisor.supervise status           # milestone/criteria/evidence dashboard
```

Requires: Python 3.11+, a running `llama-server` (GLM-5.2 GGUF), and the Hermes repo checked out.
Install: `pip install -r requirements.txt`.
