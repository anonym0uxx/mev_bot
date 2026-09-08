# North Star — Holistic Context & Work Ledger

Purpose: single entry point for a frontier model (or any reviewer) to understand the
full lineage — the bot's vision, the training that preceded this, the infra/durability
work, and the North Star dataset direction. Written 2026-09-07, Windows Hermes agent.

---

## 1. The bot vision (what we are building)

A **memecoin scalping bot** where two components divide labor:

- **LLM = the trading brain** — market reasoning, inference, and *executable*
  decisions (BUY / SELL / SKIP / WATCH). This is what **Qwen** is trained for.
- **Rust = execution + real-time data** — buying/selling, data streaming/ingestion,
  and terminal-style viewing/filtering (market-cap filters like GMGN / Padre).
  Rust development is delegated to a **frontier model (Astra)**, NOT Qwen.

Target outcome (from the North Star spec): repeatable net profitability from memecoin
scalping at *our* capital, latency, fee, and risk limits. Narrative/meta and raw market
structure are complementary inputs; a dataset cannot guarantee any specific trader's
returns.

## 2. Hardware & cross-OS architecture

- CPU EPYC 9655P, 3× RTX PRO 6000, two ~4 TB NVMe:
  - `nvme0n1` (serial `…538A05C`): Windows C: + Linux root + `/training` (2 TB ext4)
  - `nvme1n1` (serial `…246786`): D: Data (repo + artifact store)
- Dual-boot **Windows (DESKTOP-CP8N3IC)** and **Ubuntu (qwen27b)**, each running its
  own Hermes agent sharing the same Telegram bot; only one OS booted at a time.
- Switching OS = reboot (agent-initiated reboot is hard-blocked; BMC Redfish power
  control is the sanctioned path).

## 3. Training lineage (the road that led here)

- Qwen 27B **full-parameter** CPT then SFT (no smoke-test stage) was the prior plan.
- The SFT export `qwen_sft_v2.jsonl` (2,162 records) carried a **catastrophic class
  imbalance**: WATCH 6,197 / SKIP 3,262 / **BUY 22 (0.2%)**.
- Eval (`qwen_eval_v1.2`, 36 panels) showed **0 BUY actions emitted** — the model was
  never taught what a BUY looks like. Not a decode/threshold bug; a supervised-signal
  absence. (Most pump.fun tokens ARE garbage, so caution was partly right, but genuine
  buy opportunities were suppressed entirely.)
- **Conclusion:** that lineage is superseded for buy-capable execution. The
  ranking/ordering backbone was NOT the failure (full_rank_aux spearman ≈ 0.49) — the
  action-label calibration was. Keep ordering data; fix the action mix.

### Binding lesson (V6 amendment)
A decision-class model must have meaningful, balanced representation of every action
it must emit. BUY/WATCH/SKIP/SELL each need ≥2–5% label share (≥100–300 examples).
**Never pad/duplicate to hit a target** — upweight genuine BUY via task weights, and
mirror the balance in the eval split.

## 4. Cross-OS auth stability (the infra problem we were solving)

Recurring failure: Linux Nous OAuth broke on every OS switch — the gateway fell back to
a **stale FAT-mounted shared store**, compounded by a **7-hour RTC clock skew** and an
inherited `ExecStopPost` cleanup hook. Fixes staged/verified:

- RTC `RealTimeIsUniversal=1` + boot-time clock resync before any OAuth use.
- `run_linux_cutover --apply` switches the live gateway to local `shared-local`.
- Cleared `ExecStopPost` via drop-in; fixed a group-writable `hermes-agent` dir
  (0o775 → 0o755) that blocked the cutover's `unsafe_path_permissions` gate.
- A **candidate credential** was independently minted and verified on native Linux
  (`candidate_verified_native`: refresh + models + inference + tools all 200).
- **Status at pivot:** live gateway cutover + Linux→Windows return test were still
  pending (interrupted by this North Star work). Windows auth passes live.

## 5. The North Star pivot (user decisions, 2026-09-07)

1. **Qwen = trading brain only.** Drop Rust from Qwen's training entirely.
2. **Rust → frontier model (Astra).** Astra will review the Rust codebase and the
   North Star data.
3. **No human decision history exists.** BUY labels must derive from on-chain
   economics (feasible entries with robust positive net return at our size/latency/cost)
   — never from the 22 historical buys, never padded.
4. **Quality over speed.** Follow the full 8-stage North Star process; no
   "speed-to-data" shortcut.

## 6. Data landscape (as of Stage 0)

Artifact store **`D:/mev_bot-artifacts/`** — 817 files, **126.81 GB**, SHA256 manifest
(`MANIFEST.json`, 818 entries). Git holds code + manifests only.

| Store path | Content | Nature |
|---|---|---|
| `gold/laserstream_gold_v3/` | L1/L2/L3/L4 parquet + checkpoints | on-chain gold |
| `gold/slinky_gold_v3/` + `_compact/` | Slinky gold | on-chain gold |
| `gold/qwen_curriculum_v1/` | prior SFT curriculum (22-BUY set) | superseded export |
| `gold/rust_gold_v1/` | Rust quality track | superseded for Qwen |
| `raw/` | LaserStream NDJSON/ZST captures | immutable raw source |
| `rust-data/` | full_trades.pkl, event_stream.jsonl, slinky21_data, slippage.pkl | execution truth |

Coverage (v5 14 categories): ● full ◐ partial ○ gap.
- Source truth ●◐ · Identity/mechanics ◐ · Trades/execution ◐ · **Capacity ○** ·
  Charts/flow ◐ · **Holder risk ○** · Narrative meaning ◐ · **Propagation ○** ·
  **Meta/rotations ○** · **Portfolio ○** · **Decisions ○ (core gap)** · Outcomes ◐ ·
  Mechanics ◐ · Rust ●

**Findings that must not be ignored:**
- narrative trajectory (490 records) temporally ambiguous; 299 lack a primary mint.
- Slinky README has conflicting MIT/CC BY 4.0 declarations.
- LaserStream v3: modeled exits ≠ actual fills (migration gaps).
- narrative_gold_v1/v1.1 jsonl still git-tracked (move to store + untrack pending).

## 7. Durability & repo hygiene work (this session)

- 45 unpushed commits (entire pipeline/training body) + working changes → **pushed**.
- `filter-repo` mistake: used 50 MB threshold (should be GitHub's 100 MB), rewrote
  published SHAs and pruned granular history; recovered as a squash commit. Rule
  recorded: never strip below 100 MB.
- Pre-push guard: **UUID → warn-only** (data IDs, not secrets); Telegram-token +
  API-key remain hard-block.
- Backup target: cross-disk to `/training` (2 TB ext4 on the other SSD) — pending a
  Linux boot.

## 8. North Star build order (8 stages) & status

0. Receiver/source inventory + source/coverage registry — **DONE** (this session).
1. Protect evaluation (freeze held-out eval, leakage-safe export contracts) — **next**.
2. Market/narrative capture.
3. Canonicalization/accounting.
4. Episode labeling (coherent typed action-only episodes).
5. Leakage-safe balanced export (BUY/WATCH/SKIP/SELL) with the class-balance gate.
6. Independent corpus certification.
7. Operator admission / training proposal.

Stage 0 artifact: `tools/data-pipeline/north_star/STAGE0_RECEIVER_REGISTRY.md`.

## 9. Open threads & risks

1. Cross-disk backup to `/training` (Linux boot required).
2. narrative jsonl → store + untrack from git.
3. Live gateway auth cutover + return test (paused from prior work).
4. BUY-label sourcing: no human decisions — must be economically derived, capacity- and
   cost-adjusted, leakage-safe.
5. Licensing: resolve Slinky/narrative rights before aggregation/admission.
