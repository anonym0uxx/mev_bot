# North Star — Stage 0: Receiver Acknowledgment & Source/Coverage Registry

> **Historical record, not current completion evidence.** Stage numbering and several
> status/semantic claims below were superseded by the master-DAG audit. Consult
> `00_HOLISTIC_CONTEXT.md`, `BUILD_PLAN.md`, and task receipts before implementation.
> In particular, pseudo-tests, heuristic GOLD/EX_ANTE labels, and old PASS reports
> do not certify North Star admission. Original content is retained for traceability.

Status: **Stage 0 complete (receiver verified; source/coverage registry frozen).**
Date: 2026-09-07. Author: Windows Hermes agent.

This document is the durable record that (a) the receiver (Windows side) verified
its inputs, and (b) the source inventory and coverage registry are known before any
aggregation/labeling. It is superseded only by the next-stage docs, never silently
rewritten.

## 1. Receiver verification (what was confirmed, not assumed)

- Repo present at `D:/repos/mev_bot`, git clean, all pipeline/training work pushed
  to `origin/main` (`2c57e53f`; unpushed = 0). Durability achieved.
- Artifact store `D:/mev_bot-artifacts/` built and hash-verified: 817 data files,
  126.81 GB, SHA256 + size in `MANIFEST.json` (818 entries incl. README).
- `.gitignore` now excludes `tools/data-pipeline/output/`, `.tmp/`, `rust/data/`,
  `training-data/`, and `*/target/` — git holds code + manifests, store holds data.
- Large data moved OUT of the git history (same-volume atomic renames, ledgered).
  See `.moves.jsonl`.

## 2. Source inventory (artifact store)

| Store path | Content | Nature |
|---|---|---|
| `gold/laserstream_gold_v3/` | L1 pump-state / L2 outcome / L3 counterfactual / L4 policy parquet + checkpoints | on-chain-derived gold |
| `gold/slinky_gold_v3/`, `slinky_gold_v3_compact/` | Slinky-derived gold | on-chain-derived gold |
| `gold/qwen_curriculum_v1/` | Prior SFT curriculum (the degenerate 22-BUY set lives here) | prior training export |
| `gold/rust_gold_v1/` | Rust quality track (superseded for Qwen per scope decision) | dev track |
| `raw/` | LaserStream NDJSON/ZST raw captures | immutable raw source |
| `rust-data/` | `full_trades.pkl`, `event_stream.jsonl`, `slinky21_data/`, `slippage.pkl`, `archive_pre_rev12/` | balance-delta/execution truth |

**NOT yet in store (still git-tracked):** `narrative_gold_v1`, `narrative_gold_v1.1`
raw+gold jsonl (small, but contain UUID data-IDs that also trip the pre-push guard).
Action: move to store + untrack, then re-verify guard.

## 3. Coverage registry (v5 14 categories → have / gap)

Legend: ● full, ◐ partial, ○ gap.

1. Source truth / rights / hashes — ◐ (manifest has hashes; licensing unresolved, see §4)
2. Identity/mechanics (chain/mint/extensions/curve) — ◐ (slinky/laserstream, needs re-audit)
3. Trades/execution (fills/fees/failed orders) — ◐ (balance-delta; modeled exits ≠ real fills)
4. Capacity (size-dependent quotes/reserves/latency) — ○
5. Charts/flow (momentum/exhaustion/liquidity divergence) — ◐
6. Wallet/holder risk (qualified holders/creator activity) — ○
7. Narrative meaning (catalysts/originality/OG-derivative) — ◐ (narrative_gold, ambiguous)
8. Propagation/crowding (attention vs flow timing) — ○
9. Meta/rotations (competing tokens/capital rotation) — ○
10. Portfolio/opportunities (cash/inventory/thesis expiry) — ○
11. Decisions (typed action-only episodes) — ○ **← the core gap**
12. Outcomes (realized PnL/equity/censoring) — ◐ (counterfactual economics)
13. Mechanics/numeracy (exact accounting) — ◐
14. Rust quality — ● (tracked separately; superseded for Qwen per scope)

**Dominant gaps: D11 (decisions), D04 (capacity), D06 (holders), D08/D09 (propagation/meta).**
These are the sources of the BUY/WATCH/SKIP/SELL labels that must be balanced (V6).

## 4. Findings that MUST NOT be ignored

- **V6 class-balance failure (BINDING):** prior SFT = 6,197 WATCH / 3,262 SKIP / **22 BUY**
  (0.2%). Model emitted 0 BUYs across 36 eval panels. North Star MUST produce ≥2–5%
  per decision class (≥100–300 examples/class) with no padding/duplication; upweight
  genuine BUY via task weights; mirror in eval split.
- **narrative trajectory file** (490 records) is temporally ambiguous; 299 lack a
  primary mint. Do NOT inherit its GOLD labels as genuine trade reasoning.
- **Slinky README** has conflicting MIT/CC BY 4.0 declarations — resolve before reuse.
- **LaserStream v3** manifest records migration coverage gaps; modeled exits are not
  actual fills.
- **No human decision history exists** (user-confirmed). BUY labels must derive from
  on-chain economics (feasible + robust positive net return at our size/cost), never
  from the 22 historical buys or from padding.

## 5. Scope decision (user-confirmed 2026-09-07)

- Qwen = **trading brain only** (market reasoning + executable BUY/SELL/SKIP/WATCH).
  Rust is dropped from Qwen's training (frontier model — Astra — owns Rust).
- Quality over speed: full 8-stage rigor, no "speed-to-data" shortcut.

## 6. Freeze-eval boundaries (to be finalized in Stage 1)

- Held-out eval partition must be frozen BEFORE feature/ranking/execution fitting.
- Eval mirrors the balanced class distribution so BUY quality is measurable
  (precision/recall/false-buy rate).
- Exact eval contract and held-out IDs: see Stage 1 doc (next).

## Next stage

Stage 1 — protect evaluation: freeze held-out eval partition, define leakage-safe
export contracts, canonicalize accounting. Then market/narrative capture (Stage 2-3),
episode labeling (Stage 4), balanced export (Stage 5) with the V6 class-balance gate.
