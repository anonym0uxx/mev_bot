# Task 1 Readouts — 2026-07-31 (Session 20260731_140756_4fe58f60)

**Repository commit at session start:** 98c87ce
**Binding docs read:** HERMES_ONE_SHOT_PROMPT.md (§41, §64–§69, Amendments A-1..A-13),
HERMES_PHASE_B_ACTIVATION_ONESHOT.md (full), GATE_INTEGRITY_AUDIT_2026-07-31.md,
ERRATUM_AND_CORRECTED_TABLE_2026-07-31.md.

---

## Task 1(a) — Compression and context readout

### compression_count

`compression_count` is an **in-memory counter** on the `ContextCompressor` object
(`agent/context_compressor.py:2126`, `self.compression_count = 0`). It is NOT
persisted to `state.db` — only the ineffective/streak/failure-cooldown counts
are persisted (sessions table columns `compression_ineffective_count`,
`compression_fallback_streak`, `compression_failure_cooldown_until`).

**Current session (20260731_140756_4fe58f60):** compression_count = **7**
(7 compression_attempt events in `agent.log`, all committed, 0 aborted).
All 7 are from THIS session — the counter resets to 0 at session start.

**Prior session (20260731_073526_2b49c213):** 26 compression attempts (all committed).
**First session (20260730_183852_fb513941):** not logged with the same granularity
(prior to the experiment change). 800 messages compacted in that session.

### context_length

**131,072** — set in `config.yaml` under `model.context_length: 131072`.

### Compression settings actually in force

All sourced from **`C:\Users\Alon\AppData\Local\hermes\config.yaml`**:

| Setting | Value |
|---|---|
| `compression.enabled` | `true` |
| `compression.threshold` | `0.35` |
| `compression.target_ratio` | `0.2` |
| `compression.protect_last_n` | `20` |
| `compression.protect_first_n` | `3` |
| `compression.max_attempts` | `3` |
| `compression.proactive_prune_tokens` | `48000` |
| `compression.proactive_prune_min_result_chars` | `8000` |
| `compression.proactive_prune_min_reclaim_tokens` | `4096` |
| `compression.min_tail_user_messages` | `1` |
| `compression.idle_compact_after_seconds` | `0` |
| `model.context_length` | `131072` |

**Context experiment acknowledgment:** `proactive_prune_tokens` went 0 → 48000
between sessions; `threshold: 0.35` and `model.context_length: 131072` are now in
force. No compression settings were changed by this agent.

### Current prompt token count

The most recent llama.cpp `prompt eval` line (task 434266, the API call that
produced this turn's response) shows **1,273 input tokens** — but that is the
prompt for the *last completed* turn, not the one being assembled now.

The most recent `stop processing: n_tokens` for the current session is **61,776**
(task 434266). This is the total processed token count (prompt + generated) for
that task. The prompt portion was 1,273 tokens.

**Largest `stop processing: n_tokens` in the log:** 127,785 (tasks 346277, 379046) —
these are from the **prior session** (20260731_073526), not the current one.
The log file `llama_20260730-232216.err.log` spans both sessions (created Jul 30
23:22, last modified Jul 31 14:14).

### common_chat_peg_parse

**0 failures** across the entire log (510 stop_processing entries). Zero
`common_chat_peg_parse` errors — consistent with the 400+ task track record.

---

## Task 1(b) — Paper-trading / synthetic-fill mode in the CURRENT Rust workspace

### Finding: paper mode EXISTS and is BUILT-IN to the Rust engine

**It is NOT a credential item. It is NOT a build item. It already exists.**

The Rust engine (`rust/crates/pump-quant-app`) has a first-class `RunMode` enum
(`engine.rs:391-396`):

```rust
pub enum RunMode {
    Paper,    // Drive the calibrated fill model; no capital moves.
    Replay,   // Re-run a recorded event journal for determinism checking.
}
```

### What enables it

**CLI argument, not a config field or env var.** The binary takes its mode from
`argv[1]` (`main.rs:46-59`):

```
pump-quant-app paper <config-file> [--trade-jsonl <path>] [--config-ledger <path>]
pump-quant-app replay <config-file>
```

- `"paper"` → `RunMode::Paper` (calibrated fill model, no capital)
- `"replay"` → `RunMode::Replay` (determinism check on a recorded journal)
- `"live"` → **hard-refused**: `exit code 3`, "live capital is Tier-0 human-gated
  and is not available from this binary"

There is **no `paper_mode` config field** in the Rust `Config` struct. The legacy
`.env.example` line `PAPER_MODE=false` is **stale** — the Rust engine does not
read it. Paper mode is the *default* mode of the binary; live is structurally
unreachable.

### Fill semantics (what "synthetic fill" means here)

The paper engine uses a `FillModeCfg` enum (`config.rs:22-31`) with four modes,
parsed from a config integer:

| Code | Mode | Meaning |
|---|---|---|
| 0 | `SignalReplay` | Causal signal replay, makes no profitability claim |
| 1 | `OptimisticCeiling` | Deterministic optimistic mechanical ceiling |
| 2 | `AdversarialRealistic` | Calibrated adversarial execution at realistic severity |
| 3 | `AdversarialPessimistic` | Calibrated adversarial execution at pessimistic (stress) severity |

The `fill_mode` config field (`config.rs:183`) selects which fill semantics the
paper engine uses. This IS a config field (not a CLI arg) — the operator chooses
the epistemic mode without recompiling.

### Bankroll safety (structural, not remembered)

The engine enforces paper/live separation structurally via `BankrollOrigin`
(`engine.rs:413-482`):

- `PaperSeed(u64)` — paper/replay only, seeded from `cfg.bankroll_initial_lamports`
- `LiveReconciled(u64)` — live only, seeded from reconciled on-chain wallet balance
- `require_live_verified()` — fail-closed guard: a `PaperSeed` **always errors**
  when a live order path tries to size off it. A paper seed can never fund a live
  trade.

### Conclusion

**Paper trading is a RUN item, not a BUILD item and not a CREDENTIAL item.** The
binary already exists, the mode is built-in, and it requires no wallet key or
data-plane credentials to exercise. It needs:
1. A config file with `fill_mode`, `bankroll_initial_lamports`, and the strategy
   parameters.
2. The `paper` CLI argument.
3. An event feed (PumpPortal is already live; Enhanced-WS is the next lane to
   prove).

The stale `.env.example:PAPER_MODE=false` should not be used to provision
anything — it belongs to the legacy Python stack, not the Rust engine.
