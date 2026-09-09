# North Star — Holistic Context & Work Ledger (ASTRA REVIEW ENTRY POINT)

Purpose: single authoritative entry point for a frontier model (Astra) to review the
full North Star lineage — vision, prior training, all Stage 0–2 capture work, the
capture-infra buildout, and every decision made. This supersedes all earlier versions.

Last updated: 2026-09-08. Updated by: Windows Hermes agent.

---

## 1. The bot vision (what we are building)

A **memecoin scalping bot** split into two components:

- **LLM = the trading brain** — market reasoning, inference, and *executable*
  decisions (**BUY / SELL / SKIP / WATCH**). This is what **Qwen** is trained for.
- **Rust = execution + real-time data** — buying/selling, data streaming/ingestion,
  and terminal-style viewing/filtering (market-cap filters like GMGN / Padre).
  Rust development is delegated to a **frontier model (Astra)**, NOT Qwen.

**Objective function (SS13, binding):** maximize **net returned SOL per trade**,
under *our* capital, latency, fee, and risk limits. Position sizing = **Kelly**.

## 2. Hardware & cross-OS architecture

- CPU EPYC 9655P, 3× RTX PRO 6000 Blackwell (96 GB each), 256 GB RAM.
- Two ~4 TB NVMe: `nvme0n1` (Windows C: + Linux root + `/training` 2 TB ext4),
  `nvme1n1` (D: Data — repo + artifact store).
- Dual-boot **Windows (DESKTOP-CP8N3IC)** / **Ubuntu (qwen27b)**, each with its own
  Hermes agent on the same Telegram bot; one OS booted at a time.
- Agent-initiated reboot is **hard-blocked**; BMC Redfish (192.168.4.112) is the
  sanctioned power path.

## 3. Training lineage (the road that led here)

- Qwen 27B **full-parameter** CPT then SFT (no smoke-test stage).
- The prior SFT export `qwen_sft_v2.jsonl` (2,162 records) carried a **catastrophic
  class imbalance**: WATCH 6,197 / SKIP 3,262 / **BUY 22 (0.2%)**.
- Eval (`qwen_eval_v1.2`) showed **0 BUY actions emitted** — the model was never
  taught what a BUY looks like. A supervised-signal absence, not a decode bug.
- The ranking backbone was NOT the failure (full_rank_aux spearman ≈ 0.49); the
  **action-label calibration** was. Keep ordering data; fix the action mix.

### Binding lesson (V6 amendment)
Every action class a model must emit needs **balanced, meaningful representation**
(≥2–5% share). **Never pad/duplicate to hit a target** — upweight genuine BUY via
task weights, mirror the balance in eval. This is now enforced by a hard gate (§10).

## 4. North Star pivot decisions (user, 2026-09-07) — SS13

1. **Qwen = trading brain only.** Rust removed from Qwen's training entirely.
2. **Rust → Astra.** Astra reviews the Rust codebase and the North Star data.
3. **No human decision history exists.** BUY labels must derive from on-chain
   counterfactual economics (feasible + robust positive net return at our
   size/latency/cost) — never from the 22 historical buys, never padded.
4. **Quality over speed.** Full 8-stage process; no "speed-to-data" shortcut.
5. **Capital/risk = Kelly sizing.** 6. **Objective = maximize net returned SOL/trade.**

## 5. Build-order status (master Build DAG — stages 0–8, source of truth)

Source of truth: `F:/handoff-linux/northstar/2026-09-06-expert-v5/NORTH_STAR_WINDOWS_MASTER_V5.md` §7 ("Build DAG … corrected order"). All stage numbering and ordering below follows that, not any earlier internal stage doc.

| Stage | Master definition (verbatim gist) | Status |
|---|---|---|
| 0 | receive/inventory — verify workspace, sources, licenses; produce receiver ack + source/coverage registry + gap report | ✅ DONE |
| 1 | protect evaluation + contracts — reserve protected time/entity/source boundaries; freeze held-out partition; forward untouched window; inherited-weight uncertainty | ◐ PARTIAL (framework written; held-out ID freeze DEFERRED) |
| 2 | bounded canonicalization — write new schemas/tests; parse trusted raw → Parquet; event identity/finality; source/rights + per-field availability; ledger reconciliation | ⬜ NOT STARTED |
| 3 | parallel capture — prospective opportunity/account/execution capture + **narrative/message capture**; features; annotations; entity + propagation graphs | ◐ PARTIAL (lanes built; narrative P0 incomplete) |
| 4 | development-only execution calibration | ⬜ |
| 5 | episode reconstruction / labeling | ⬜ (balance-gate SPEC only) |
| 6 | split inheritance + exports | ⬜ |
| 7 | independent whole-corpus certification | ⬜ |
| 8 | Windows handback (training proposal) | ⬜ |

**Corrected baseline.** Stage 0 is done; **Stage 1 is partial** (framework written, but
the core deliverable — the frozen held-out mint/time partition — was deferred and
never done, see below). **Stage 2 (canonicalization) has not started** — the prior
"Stage 1b schema registry" verified *existing* L1–L4 gold schemas (Stage 0
inventory), not the *new North Star namespace schemas* (§6 of the master:
`sources`/`raw_objects`/`events`/`token_versions`/`opportunities`/
`decision_contexts`/`episodes`/`splits`…), and the raw→Parquet parse, event
identity/finality and ledger reconciliation are all unbuilt. Stage 3 capture lanes
exist but are incomplete.

**Stage 1 residual (from `STAGE1_PROTECT_EVALUATION.md` §4, explicitly deferred):**
1. freeze the held-out mint/time partition (hash-pinned ID list) — the core deliverable;
2. write the export schema enforcing leakage rules programmatically (assert no post-`t`
   field, group-disjoint);
3. canonical accounting function (lamport-exact rounding).
Plus master-Stage-1 items the doc does not cover: relative-only quarantine for unknown
time sources, the planned forward untouched window, inherited-weight uncertainty, and
numeric risk/economic-limit approval (pre-strategy-tuning).

**Narrative is P0, not optional.** Master line 302: *"narratives are a required
collection dimension, not optional decoration."* D07/D08/D09 are each marked
"P0 for narrative-capable release"; line 434: a numerical-only release "may be
LIMITED, but cannot be advertised as the full narrative/meta North Star." §3 lists
eight learning tasks, all narrative-linked.

**Hard blocker:** narrative↔slinky overlap is **1/317** (narrative mints current,
slinky Jun–Jul). Master line 302 requires narrative + numeric state at the SAME
decision cutoff → co-temporal capture is mandatory, not a nice-to-have. Now running:
live Megga stream + LaserStream co-temporal capture (§8).

## 6. Data inventory (measured, not estimated)

**On-chain (the core):**
- **slinky21** `D:/mev_bot-artifacts/rust-data/slinky21_data/` — **64.5M rows**
  (798K tokens, 33.5M trades [17.6M buy / 16.0M sell], 27M flow snapshots,
  1.4M postgard, 1M wallets). Window **Jun 5 – Jul 14 2026** (39 days). License CLEARED.
- **LaserStream raw** `D:/mev_bot-artifacts/raw/` — **70 GB**, 369 `.zst` parts.
  Window Aug 23–24. **Integrity-validated 99.87%** (size+SHA256; only `part0368`,
  the 18 MB tail = 0.128%, missing). 18.4M raw records / 4.24M events.
- **Counterfactual** `gold/slinky_gold_v3_compact/counterfactual_trade_v3/` —
  **33.5M scenarios**: STRONG 4.64M · GOOD 590K · MARGINAL 175K · BAD 3.44M ·
  TOXIC 5.89M · SKIP 18.8M. Positive 5.2M vs negative 9.3M (~36:64).
  **4.54M size-feasible (`sz050_feasible`) with positive net return @ 0.5 SOL.**
- **Helius historical extract** `output/solana_historical_enrichment/` —
  5,701 graduated mints, 874 MB, mint-keyed tx timelines + creator (co-temporal w/ slinky21).

**Decisions (D11):**
- **KOL on-chain decisions (slinky21, authoritative):** 26,427 total —
  Cupsey 15,672 (997 coins), Cented 7,992 (1,888 coins), Megga 2,763 (800 coins);
  **156 consensus coins (≥2 KOLs)**. Insentos/Ansem excluded (obfuscated, see §8).

**Narrative (D07–D09):**
- 5-layer gold chain rebuilt: content 1,346 · claims 1,060 · states 1,060 ·
  validations 363 · strategy cards 65. EX_ANTE 253, GOLD 23. Live 2h cron running.

**D10 (account state):** hot wallet `7ZwrFiGVE8dsEknqx879C7oV31gtR95abk8SLDLTR9DC`,
`rust/data/tape.jsonl` (260 trades), 0 fills in slinky21 (live Aug 2026, post-window).

## 7. Key findings / pitfalls (binding — do not relearn)

1. **SOL↔lamports = 1e9** (not 1e6). A 1000× bug was caught by a verify script.
2. **pump.fun uses Token-2022** (`TokenzQd...`), NOT SPL Token. Holder queries must
   use Token-2022 + `memcmp(mint)`, no dataSize filter.
3. **Jito bundle-blindness (skill #25):** `getTransaction` cannot see a KOL's own
   bundled trades — SOL/token delta reads 0.0, no Buy/Sell instruction. Only raw
   Geyser (slinky21/LaserStream) sees full account state. **Applies to mint-centric
   buyer attribution too** — the RPC-visible signer is the bundler, not the trader.
4. **Insentos wallet `7SDs3PjT2…` = shill/airdrop receiver** (69K txs, zero pump.fun),
   not a trader; flows through router → rotating Jito-bundle wallets. Not backfillable
   via RPC. **Cented/Ansem: same bundled-blindness.**
5. **Temporal mismatch:** narrative mints (current) vs slinky21 (Jun–Jul) → only
   1/317 overlap; blocks claim↔outcome validation for that pairing.
6. **full_trades.pkl (4.6 GB, 27.5M rows) is 100% subsumed** by slinky21 → dropped.
7. **LaserStream SDK 0.5.0 → 0.6.4** (Helius gRPC update, Sep 1) was required; the
   0.5.0 client connected but silently received zero data. Rebuilt binary now streams.
8. **creds file is CRLF + must be read inside WSL** (inline `$(…)`/`source` from the
   Windows shell yields empty vars → the "empty credentials" false-alarm).

## 8. Capture-infra buildout (this session — new)

### Twitch/VOD spoken-reasoning lane (D07/D11 enrichment)
- `transcribe_vod.py` — yt-dlp (VOD audio) → faster-whisper large-v3 on GPU
  (20× real-time steady-state), with the CUDA DLL-path fix (cublas/cudnn/cudart
  pip wheels + PATH). Output: timestamped segments + text.
- Spike: Megga "MILLY PNL" VOD (6h01m) → 33,211 words / 6,153 segments in 27 min.
  **Reasoning yield = 21.0%** (entry 304 / exit 293 / mcap 172 / holders 124 /
  dev-rug 121 / narrative 82 / risk 53 segment counts). Quality high (correct jargon).
- Caveat: **forward-only** (14-day VOD retention; Cented's YouTube = Fortnite, Megga's
  = 404). Transcript archived; audio deleted (transcribe-then-delete).

### Live co-temporal capture (active 2026-09-09)
- Megga live on Twitch (stream `320254509148`) → two lanes running co-temporally:
  1. **LaserStream** on-chain capture (session `20260909_144906_000490`, 120-min bounded).
  2. **yt-dlp** live audio download → `megga_320254509148.mp4` (no VOD buffer, so from now).
- Context metadata: `D:/mev_bot-artifacts/narrative/twitch_live/CAPTURE_CONTEXT.json`
  (streamer, stream_id, capture start, wallets, join keys).
- Purpose: satisfy the master's co-temporality requirement (narrative + numeric state
  at the SAME decision cutoff, line 302) — the fix for the 1/317 mismatch.
- Join: transcript timestamps → mint via name extraction → cross-ref LaserStream
  events in the capture window (Stage 3 work).

### Mint-centric on-chain confirmation stage
- `confirm_stream_mint.py` — the corrected architecture (replaces wallet-history paging):
  transcript names → DexScreener name→mint (recency-filtered ±72h) → mint-buyer lookup
  (Helius) → KOL-wallet cross-ref. **Bounded queries** (0.4s/mint vs 2M-deep wallet
  paging that ran 6+ min without finishing).
- Finding: Megga's wallets are **bot/bundle-burst addresses** (1000 tx/2min) — wallet
  paging is intractable; mint-centric is correct AND fast, but **RPC buyer attribution
  is still bundle-blind** (§7.3). True confirmation needs Geyser co-temporal with streams.

### Storage optimization (the resident-capture plan)
Diagnosis: raw `.zst` ≈ 68 GB/day, but **events `.ndjson` uncompressed ≈ 290 GB/day**
(the real wall). **Measured field split: 90% of event volume is complex fields**
(`log_messages` 36%, token balances 28%, inner instructions 15%, account keys 8%) —
all redundant with the raw lossless capture. Scalar trading fields = **10%**.

Implemented + measured:
- **Step 1 (done):** events output zstd-compressed (`.ndjson.zst`) — 263 MB → 19.2 MB
  (13.7×). Built into `pq-laserstream-grpc` (rebuilt + smoke-tested).
- **Step 2 (done):** `compact_events.py` — daily compaction to **scalar-only Parquet**
  (42 typed columns; complex fields excluded, stay in raw). **2.24 MB vs 19.2 MB zst =
  8.6× smaller, ~100× vs uncompressed.** `--full` flag re-includes the JSON fields.
- **Projected resident rate:** events scalar Parquet ≈ **3 GB/day** (vs 290 GB/day
  uncompressed). Tiered: raw ring buffer (24h) + scalar Parquet (accumulating) +
  counterfactual features (daily).
- **Step 3 (done):** resident daemon `capture_daemon.py` (launch→compact→purge→restart)
  + creds-safe `run_capture.sh` (replaces the hardcoded-key `run_capture_300.sh` leak)
  + raw ring-buffer purge (24h, verified 185 stale parts purged; canonical 369 intact).
- **Open design item:** scalar-only Parquet drops token balances; D04/D06 holder data
  beyond 24h needs a separate account-snapshot stream (or `--full` compaction).

**Format decision:** **Parquet** for the intermediate events layer (matches slinky21,
zero new deps, DuckDB/pyarrow native). **Lance** flagged as a *future* option for the
final training-loading layer (append-friendly, ML-oriented) — not now, because we're
in Stage 2 and the consumer is DuckDB. **Vortex** = pre-1.0, revisit later. **FastLanes**
= an encoding technique, not a format.

## 9. Stage 5 label-balance gate (frozen — Astra to review)

`STAGE5_LABEL_BALANCE_GATE.md` — the exact guard that would have caught the 22-BUY
failure. Fail-closed exporter + pre-training audit:
- No empty class (BUY/SELL/SKIP/WATCH each ≥1,000).
- BUY ≥ 10,000 absolute AND ≥ 5% relative; SELL ≥ 5%; SKIP ≤ 80%.
- Audit re-reads the serialized file (not the generator), asserts `balance_ok`.
- Regression test feeds `{BUY:22, SELL:0, SKIP:2000, WATCH:0}` → must BLOCK.
- **Open for Astra:** SELL's source predicate is undefined (exit-side counterfactual,
  not entry `economic_class`) — Stage 5 must pin it before export.

## 10. Doc index (detailed stage artifacts)

- `STAGE0_RECEIVER_REGISTRY.md` — source/coverage registry
- `STAGE1_PROTECT_EVALUATION.md` — freeze-eval + leakage contracts
- `STAGE1b_SCHEMA_REGISTRY.md` — L1–L4 verified schemas + BUY derivation
- `STAGE2_CAPTURE.md` — gap→source map
- `STAGE2_AGGREGATION_INVENTORY.md` — full measured D01–D14 ledger
- `STAGE2_D13_PROTOCOL_NUMERACY.md` — bonding-curve math, fees, graduation
- `STAGE2_CAPTURE_VALIDATION.md` — D01 99.87% + Reddit fix + Twitch/YouTube lane
- `STAGE2_TWITCH_SPIKE_EVAL.md` — 21% yield + leverage design
- `STAGE5_LABEL_BALANCE_GATE.md` — balance gate + BUY-count audit

## 11. Supervisor registration (status)

- **Infra facts (registered, done):** `north_star_capture_daemon`,
  `north_star_storage_architecture`, `north_star_laserstream_modes` — 3 facts in the
  supervisor's facts ledger (facts_count 21). Provenance-stamped, Astra-visible.
- **Formal component (`propose_amendment` `new_component`): BLOCKED.** The amendment
  intake requires an `evidence_ref` that resolves to a real record in the evidence
  store (`gate:`/`experiment:`/`artifact:`/`benchmark:`/`criterion:`), and that store
  is **empty** — the certification battery (cargo build/clippy/tests) is
  Rust-trading-bot-scoped; the capture daemon is a Python + WSL-Rust build that does
  not map onto it. Tool rejected `artifact:live_status` ("does not resolve to a
  record").
- **Unblock path (parked):** extend the supervisor to certify the data pipeline (a
  North Star milestone + criteria in the constitution) or register a gate result via
  `gate_verify` — both are supervisor-extension work, not capture work.

## 12. Open data items → master-stage map

The D-category gaps map onto the master DAG as follows (D = capture/derivation dimension):

| Item | Work | Master stage |
|---|---|---|
| D04 capacity | size-specific exit depth | Stage 2 (canonicalize depth) → Stage 4 (calibrate) |
| D06 holders | connected-wallet linking + Token-2022 holder enrichment | Stage 2 (canonicalize) + Stage 3 (capture holders) |
| D07 narrative | lift EX_ANTE/GOLD (253/23) | Stage 3 (narrative capture — P0) |
| D08 propagation | repost/echo graph | Stage 3 (propagation graphs) |
| D09 rotation | theme/rotation model | Stage 3 (narrative/meta) |
| D10 account state | prospective account/opportunity capture | Stage 3 (prospective capture) |
| D11 decisions | BUY/SELL/SKIP/WATCH labels | Stage 5 (episode labeling) — counterfactual economics |

**Canonicalization (Stage 2) precedes capture (Stage 3)** in the master order. The
D07/D08/D09 narrative items are Stage 3 P0, currently blocked by the 1/317
co-temporality gap — fixed only by the live co-temporal capture (Megga + LaserStream,
now active).

## 13. Open threads & risks

1. Cross-disk backup to `/training` (Linux boot required).
2. `narrative_gold_v1` jsonl still git-tracked → move to store + untrack.
3. **D04/D06 account-snapshot stream** — scalar Parquet drops token balances; holder
   data beyond 24h needs a separate snapshot stream (or `--full` compaction).
4. **SELL-label predicate** undefined (Stage 5 input, flagged to Astra).
5. Twitch reasoning lane: **forward-only**; live confirmation needs Geyser co-temporal
   with streams (paid/burst, not free).
6. **Consolidation decision:** fold training-capture into pq-daemon's existing LS
   child (single connection + free supervision) vs keep `capture_daemon.py` separate
   (double LaserStream credits). Rust-side → Astra.
