# North Star Master DAG Audit and End-to-End Implementation Plan

> **For Hermes:** Use subagent-driven-development if available; otherwise use isolated implementers followed by independent specification-compliance and code-quality reviews. Implement this plan task-by-task, with failing tests before production changes. No self-certification.

**Goal:** Produce the complete, provenance-backed, causally aligned, narrative-and-numerical North Star training dataset for Qwen to serve as the memecoin trading brain: selection, timing, sizing, position management, verification, abstention and executable actions under our real constraints.

**Architecture:** Immutable source/version and coverage evidence feeds canonical point-in-time market, narrative, opportunity, portfolio and execution tables. Separate observed actions, supported deterministic economic targets, human statements/judgments and future outcomes. Derive bounded sequential task exports only after evaluation protection and execution calibration; independently certify every admitted file and capability.

**Tech stack:** Windows-controlled Python, Arrow/Parquet, bounded DuckDB queries, versioned JSON contracts/manifests, pytest and existing Rust execution/capture components. The current capture binary runs in WSL; that does not make the project a native-Linux deployment. Model training, multimodal expansion, live orders, destructive storage operations and changes to operational risk limits have separate approvals.

**Deliverable status:** AUDIT + IMPLEMENTATION PLAN, not an implemented or certified dataset. Written 2026-09-09 PT. This review does not freeze evaluation, change capture processes, admit sources, run training, or certify legacy corpora. Code-level findings are read-only findings; future test commands below are acceptance targets, not reported passing results.

---

## 1. Authority, scope and exact source baseline

`M:Lx–Ly` below means lines in:
`F:/handoff-linux/northstar/2026-09-06-expert-v5/NORTH_STAR_WINDOWS_MASTER_V5.md`.
The entire 935-line file was read, including v5 and embedded v4/v3/v2. Its governing Build DAG is **M:L387–407**. Older appendix WP numbers are implementation packages, NOT stage numbers. The master remains authoritative; this plan is its implementation mapping.

Verified SHA256:
- Master v5: `ebee45d5b98a5daeed72edebf16e7ef2b8878d5cc96b191c451584a315cea57c`.
- Binding data-mix amendment V6: `6af4760ae8ae247214a4a4ea5c61eb3bd3be314ce85e4b09a274d7015a5dda5f`.
- Original G01–G12 coverage checklist: `965efd3978f78daaf888c8ce01748c222dc0919e6e523d566a35885b578f6caa`.
- All five files named by `NORTH_STAR_HANDOFF_MANIFEST_V5.json` matched their recorded hashes in this review. V6 is additional and was hashed separately.
- Inspected repo HEAD: `d17645623f764931b0b81866a0a2819103e0acdd`.

Later explicit operator decisions govern conflicts with historical model-role prose:
1. **Qwen = trading brain only. Astra = Rust developer.** Do not restore Rust SFT tasks from embedded v4. Preserve D14 engineering artifacts separately; retain the mandatory Rust/action semantic-conformance and runtime safety tests.
2. **Kelly sizing philosophy; maximize net returned SOL per trade.** Keep all-in portfolio equity, capital-time, failed costs, tail risk and comparison to cash/frozen baselines as required safeguards. Optimizing average return on selected closed winners is not this objective. Kelly is not a numeric risk envelope or proof that estimated edge is trustworthy.
3. **No existing human-labeled decision history.** A/B action/economics tracks can proceed on valid evidence; H1/H2 reasoning coverage cannot be invented. Human semantic review/rights capacity remains a real dependency for the complete relevant capabilities, not a task silently assigned to Alon.
4. Narrative D07/D08/D09 is required for the requested full release. A numerical-only LIMITED release is not an acceptable substitute for this target.
5. No duplicate padding, no Qwen/self-generated training prose, no silent evaluation contamination. Exact source/license/token disclosure and inclusion approval remain gates. Preserve operator-reported Slinky publisher permission, but record the evidence, exact applicable grant and obligations rather than treating a prose `CLEARED` flag as machine-verifiable rights provenance.
6. Full-parameter 27B training is a separate proposal/Go; no smoke-model-training stage. Parser, data and fault-injection tests are required engineering validation, not smoke training.

### Terminology that must never be conflated
- **Stages 0–8:** build order in M:L391–407.
- **D01–D14:** collection dimensions, M:L217–298.
- **G01–G12:** v5 expert-gap contracts, M:L27–147.
- **Operational gates G0–G6:** operating/source/truth/learning/offline/shadow/live gates, M:L785–793. Use `OP-G0`…`OP-G6` in implementation records to avoid collision with expert `G01`.
- **WP0–WP9:** appendix work packages, reordered by the master DAG.
- An implemented field, a captured file, a software-test pass and admitted corpus support are four different things.

## 2. Audit method, limits and current baseline

Read the complete master and V6; read all ten `tools/data-pipeline/north_star/` Markdown documents; inspect named source code, capture/context metadata, source Parquet footers, manifest hashes and repo state. Independent read-only code/capture reviews supplement this plan. This was not a full raw-byte rehash, a whole-corpus semantic audit, a sealed evaluation run or a reproduction of historical training.

At the review preflight the repo was **not clean**: 12 modified tracked entries, 185 tracked deletions, 34 untracked status entries. These predate this plan. The deletions include historical raw parts in the repository capture directory; do not restore, stage, delete more or reset them without reconciling the artifact-store copies. Read-only inventory found 369 raw-part filenames in the store but no `part0368`; counting filenames does not establish complete coverage of a particular manifest.

The expected new directories `tools/data-pipeline/src/north_star/`, `schemas/north_star/` and `tests/north_star/` do not exist. A filename search under `tools/data-pipeline/` found no `EVAL_FREEZE.json`, `heldout_mints.sha256`, `source_registry.jsonl` or `TEST_RESULTS.json`. This supports **NOT EVIDENCED HERE**, not a claim that no legacy split files exist anywhere.

Fresh Parquet footer checks found:
- Slinky trades: 18 shards, 33,581,765 rows.
- `tokens.parquet`: 798,430 rows.
- `snapshots.parquet`: 26,934,864 rows; bucket OHLCV, flow and end-of-bucket reserves, NOT a complete token-account snapshot stream.
- `postgard_snapshots.parquet`: 1,392,133 rows; liquidity/price/volume summaries, not automatically executable historical quote truth.
- `wallet_stats.parquet`: 1,016,374 rows; whole-window totals/first/last seen, not causal teacher/holder profiles at every historical cutoff.
- `migrations.parquet` was absent at the expected Slinky root. Search/mapping/reconstruction is required; this does not establish all migration evidence is absent.

Other earlier numerical counts (KOL trades, counterfactual class totals, narrative GOLD counts, full raw SHA verification) are **historical reports**, not re-certified training support. They remain useful leads with producer paths, not acceptance receipts.

### Stage baseline against the actual master

| Master stage | Existing useful work | Audit status / missing gate |
|---|---|---|
| 0 Receive/inventory | Artifact store, source manifests, footer inventories, receiver prose; source master package now hash-checked | **PARTIAL / re-open closure.** No current full v5 D/G/source-to-task registry; rights, producer, coverage and dirty-tree reconciliation need completion. Missing sources may be explicitly inventoried without blocking Stage 0 itself. |
| 1 Protect evaluation/contracts | Written leakage rules and legacy split strategies | **PARTIAL, NOT CLOSED.** Protected boundaries are not evidenced as a new enforced North Star artifact. Ancestry/exposure, forward untouched window, relative-only quarantine and multi-parent policy missing. |
| 2 Bounded canonicalization | Legacy L1–L4 builders/schemas, compression/Parquet helper, accounting-related Rust | **REUSABLE COMPONENTS; new North Star stage not implemented/certified.** New schema/availability/identity/reconciliation/resume contracts required. Do not claim no useful code exists. |
| 3 Parallel capture/context | Market captures, social acquisition, one VOD transcription spike, live Twitch/LaserStream attempt | **PARTIAL RAW/PROTOTYPES.** No complete causal joint context, full opportunity/portfolio/session capture, G01–G12 coverage or reviewed narrative support. |
| 4 Development execution calibration | Legacy simulators/scenarios and cost code | **NOT NORTH STAR CERTIFIED.** Refit only on protected development partitions; actual fill/failed-exit residual support and frozen operating regimes required. |
| 5 Episodes/labeling | KOL action extraction, legacy policy outputs, narrative heuristics, balance specification | **NOT CLOSED.** Action origin, sequential portfolio branches, state-dependent SELL/ADD/HOLD, null rationales and independent examples need construction. |
| 6 Split inheritance/exports | Legacy exporters, trainer token-accounting code | **NOT CLOSED.** New inherited split resolver, exact trainer-path masking/counts and wired balance/loader guards needed. |
| 7 Whole-corpus certification | Legacy certifier scripts/reports | **NOT CLOSED.** New scope requires independent semantic/economic/source/temporal review plus all-file structural checks. |
| 8 Handback | Plan documents | **NOT READY.** Final release bundle and separately approved training proposal pending. |

There is no defensible percentage-complete estimate from these observations. The durable asset base is substantial; the integrated North Star acceptance chain is missing.

## 3. Corrections required in the Astra work ledger

Implementation task S0.1 must update `00_HOLISTIC_CONTEXT.md`, preserving its prior versions through git, and add supersession notices to historical stage documents. Do not keep patching contradictory paragraphs while leaving old completion claims active.

1. **Stage 0 was over-closed.** The original receiver registry has D categories but no G01–G12 ledger. Master v5 requires every G-ID to carry producer, source eligibility, measured support and independent acceptance (M:L139–147).
2. **Stage 1 is neither merely a mint hash nor a completed framework.** `STAGE1b_SCHEMA_REGISTRY.md:L3–4` defers IDs to a later exporter. Master freezes boundaries BEFORE fitting/curation and materializes their inherited assignments in Stage 6 (M:L393,403). Freeze exact policy/available IDs now; future events can inherit preregistered boundaries as they arrive. No post-outcome holdout selection.
3. **The previous next-step ordering was over-restrictive.** Safe file inventory/parser tests are allowed with unresolved numeric capital. Ledger implementation belongs to Stage 2. Approved time-sensitive raw capture can run independently while protection/contracts are built; it is not permission to fit or curate on a future protected pool (M:L129,393–397).
4. **D10 is account AND opportunity state**, not tape/account balances alone (M:L274–277). Required: alternatives, capital reservations, inventory, watch triggers and correlated exposure.
5. **Counterfactual-positive states are not independent BUY demonstrations.** Overlapping scenarios, hindsight exits, cost assumptions and absent opportunity/account context cannot be turned into millions of reliable decisions by relabeling `economic_class` (M:L319,381–385,635–645,767–781). BAD/TOXIC entry states are not SELL instructions.
6. **The 22-BUY failure is not structurally prevented by a Markdown spec.** `STAGE5_LABEL_BALANCE_GATE.md` mixes proposed behavior with claims of implementation. Its 10,000 BUY/1,000-per-class floors are stricter derived proposals, not the text of V6. Treat these larger floors as an unapproved proposal unless explicit approval evidence is recorded; resolve the V6 policy without silently introducing extra quotas or fabricating support. V6 concerns genuine action counts/shares; trainer optimization also requires post-mask label-token accounting.
7. **Natural prevalence and balanced diagnostic evaluation are separate.** Keep untouched natural chronological sessions for primary economics and a separately labeled balanced challenge suite for V6 class diagnostics (M:L675,747; V6:L45–74). Do not rebalance primary economic prevalence using future labels.
8. **Narrative heuristics are not timing/semantic certification.** `build_creator_claim_v1.py:L277–286` calls text EX_ANTE by future-tense regex. That does not prove availability before decision/outcome. `build_strategy_card_v1.py:L28–40,43–78` groups unresolved claims and assigns setup types heuristically. Legacy GOLD/EX_ANTE counts need reclassification, not promotion to human teaching.
9. **One VOD is a feasibility spike.** Keyword yield is not expert-label accuracy, rights approval, diarization, complete sessions or on-chain attribution. The master specifically warns about edited clips, narrator identity, retrospective overlays and PnL claims (M:L6–22,134–138).
10. **The blanket YouTube dead end was not supported.** Master lists direct interview/video URLs for Prosper, Cented, Megga and Cupsey (M:L15–21). Its Megga channel identifier `UCOjxpeSVbHn75QoVizg7dDg` differs from the truncated `UCOjxpeSVbHn75QoVizg` in seeds. Preserve both as distinct evidence tokens; do not silently repair the source field. Resolve provenance using direct links; source access and rights are still separate gates.
11. **Same-time processes do not prove co-temporality.** `CAPTURE_CONTEXT.json` records launcher-context time, not an HLS media-clock-to-UTC map or broadcast delay uncertainty. Media start, buffering/ad/discontinuity intervals, speaker time and market receipt/availability need measured mapping. Name matching/nearby timing is a hypothesis, not verified action attribution.
12. **Withdraw universal bundle-blindness as a verified fact.** No matching transaction or owner delta is not proof Jito hides a trader from `getTransaction`, nor proof Geyser identifies beneficial owners. Reconcile the same signature, loaded account keys, outer/inner instructions, token-account ownership and state deltas; distinguish proxy wallets, CPI routing, missing records and uncertain owner attribution. Do not label unobserved people/intent.
13. **SDK change causality was overstated.** An empty-credentials issue occurred during the same debugging. Working SDK 0.6.4 is an observed configuration; do not attribute the original failure to a breaking SDK change without a controlled comparison.
14. **D13 has a critical unit error.** `STAGE2_D13_PROTOCOL_NUMERACY.md:L15–16` says 15 decimals while equating raw supply `1e15` with one billion display tokens. That implies `1e6` raw/token (6 decimals), not 15. Supply is not decimal precision. Read actual mint metadata and versioned venue configuration. Hardcoded fee/graduation constants are not universal historical protocol truth. Code-origin documents also need provenance review before SFT.
15. **Holder/program assumptions need temporal scope.** Query the actual mint owner/program/extensions at the relevant time; do not impose today's Token-2022 convention on all historical sources. Current holder snapshots cannot certify historical ownership.
16. **Mint-set containment does not prove row-level source redundancy.** `STAGE2_AGGREGATION_INVENTORY.md:L41–46` bases 'zero new data' on all legacy mints appearing in Slinky. Keep the legacy source until event/row/value lineage equivalence is demonstrated. August LaserStream cannot directly validate June/July transactions without matching identities/time.
17. **Raw integrity is not decoder/coverage completeness.** `validate_laserstream.py` checks one manifest and prints failure without an explicit nonzero process exit; missing tail must remain a coverage/censoring defect. A global directory count of 369 does not refute the reported 368/369 for that manifest.
18. **Supervision was not added by recording infra facts.** Three ledger facts exist, but describe architecture, not a process owner. A rejected `artifact:live_status` reference proves that reference failed, not that the entire evidence store is empty. A status JSON with tick=1 does not prove a bot is currently live. Two connections do not by themselves quantify billed credits. Keep certification registration distinct from restart supervision.
19. **Retention currently violates the intended safety model.** `capture_daemon.py:L42–52,58–82` skips any existing Parquet, then purges raw by mtime even after compaction failure. `compact_events.py:L120–147` writes the final path directly. Active-file reads, interrupted outputs, schema/config changes, missing archival account data and permanent source deletion are not guarded. Retention must be lease/receipt/dependency based; no auto-purge authority from an age threshold alone.
20. **No absolute completeness claims on D01/D02/D03/D05/D12/D13.** Known manifests/rows prove assets, not full task coverage, prior-time semantics, rights or calibrated economics.

## 4. Target dataset structure and training semantics

### 4.1 Physical truth tables

Implement all master canonical tables (M:L368–377) under a new namespace:
- `sources`, `raw_objects`, `coverage_intervals`, `events` with append-only revisions/finality correction.
- `token_versions`, `venue_states`, `content_versions`, `entity_links`.
- `opportunities`, `portfolio_snapshots`, `decision_contexts`.
- `annotations`, `intents`, `execution_events`, `inventory_lots`, `realizations`.
- `outcomes`, `narrative_states`, `episodes`, `splits`, `export_records`.

Common fields: schema/code/config/source version, immutable ID/foreign keys, content hash, exact units, event/received/available time plus uncertainty and clock domain, per-field null reason, evidence tier, rights/revocation lineage, split restrictions and parent dependencies. Monetary accounting uses integers/Decimal/rational arithmetic with explicit rounding, never binary-float truth. Source floats remain observed rounded estimates until reconciled; casting cannot restore lost precision.

Add all v5 objects (M:L118–127):
`expert_source_claim_v1`, `discovery_attention_event_v1`, `narrative_competition_snapshot_v1`, `thesis_revision_event_v1`, `execution_parent_child_v1`, `influence_incentive_context_v1`, `session_opportunity_audit_v1`.
Keep `rust_market_bridge_episode_v1` in the separate engineering evidence namespace per the operator override, excluded from Qwen SFT. Its contract IDs still link differential runtime tests. Appendix WP7's `code_quality.py`, `export` code-teaching records and `test_code_quality.py` are explicitly out of the Qwen dataset build under that override; they remain separate engineering-track deliverables, not silently completed or silently deleted requirements. Rust/action semantic-conformance tests remain mandatory for the trading dataset.

### 4.2 Four objects per teaching unit

1. **Decision context:** immutable, allowlisted IDs and bytes actually available by cutoff.
2. **Decision target:** typed observed action / human judgment / source-backed fact/span / deterministic economics / mechanics, with provenance and applicability.
3. **Execution observation:** actual intent-to-submission/fill/failure reconciliation; no phantom fills.
4. **Outcome review:** later payoff, censoring and retrospective diagnosis, never back-joined into context/rationale.

Provenance tiers remain H1 contemporaneous, H2 blind replay, H3 retrospective, A verified action-only, B deterministic/simulator, Q quarantined. Missing human rationale is null. ASR is a machine transformation of source audio, not invented rationale and not automatically verified human transcription; retain model/version, timing, speaker uncertainty and evidence needed to correct material errors. No generated summaries/paraphrases, weak heuristic explanations or this plan's prose enter training as source truth.

### 4.3 Action contract

Use typed operations `WAIT`, `WATCH`, `ENTER`, `ADD`, `REDUCE`, `EXIT_ALL`, `CANCEL`, `REPLACE`, `ABSTAIN_DATA_QUALITY`, and `HOLD` only with a position (M:L627–633). Preserve BUY/SELL/SKIP/WATCH as a reporting/compatibility mapping, not a lossy replacement for this vocabulary. Do not merge SKIP with SELL or WATCH with all position management. Freeze a versioned adapter to concrete Rust symbols and test equivalent effects.

Actions include explicit lamport/raw-token size, sale-fraction denominator, position/pending-reservation identity, min receive, expiry, state version, allowed branches, thesis/horizon/invalidation and risk-policy ID. Repeated partial exits act on a named denominator; no double-spend across candidates or oversell across child orders. Kelly-derived recommended size uses development-calibrated distributions, uncertainty shrinkage, capacity/correlation and approved fractional/risk caps; sparse unsupported edge produces abstention or constrained exposure, not full-Kelly certainty.

### 4.4 Learning tasks

Preserve continuous economics/path/censoring targets, not only four action words. Export supported task families:
- Direct action selection and low-latency structured outputs.
- Source-backed concise explanation/evidence/counterevidence with action, only when origin permits.
- All eight narrative/numerical synthesis tasks from M:L304–311.
- Tool query/result/next-action sequences, verification delays, unavailable-source handling and instruction-injection resistance.
- Full position lifecycle: wait, entry, hold/add/trim/runner/full exit, cancellation, re-entry, thesis expiry and session restraint.
- Numerical mechanics/accounting, uncertainty, quote/fee/latency/capacity and failure reasoning.
- Separate retrospective diagnostics, never disguised as predecision rationales.
- Supported state/outcome/relative ordering tasks from legacy gold after revalidation; optional CPT only for demonstrated knowledge gaps.

### 4.5 Split, token and distribution best practices

- Reserve outcome-blind chronological/session/source boundaries before fit/curation. Record old CPT/SFT/eval/retrieval exposure and seed ancestry. Treat already outcome-inspected legacy collections as development by default unless a defensible exclusion proves otherwise; do not rename them untouched.
- Maintain natural economic evaluation and a distinct balanced/challenge diagnostic suite. Future model/system freeze requires a further fresh shadow window; today's captured data is not automatically fresh for a future trained model.
- Every derivative inherits ALL parents: chosen/unchosen candidates, held positions, content versions, teacher histories, learned clusters, retrieval indexes. Resolve connected-component conflicts and disclose excluded/giant groups rather than weakening protection.
- `available_at <= decision_at` uses ordering/uncertainty semantics; same coarse timestamp does not prove order. Derived availability is no earlier than latest dependency plus compute delay. Unknown-time data may support relative-only tasks but not global narrative/portfolio claims.
- Deduplicate raw deliveries separately from legitimate repeated events; cap episode/mint/source/family contributions. One story, many views is not independent evidence.
- Use the actual selected trainer/tokenizer/chat-template/masking path to count raw, visible, causal-context and post-mask loss-bearing tokens. Count action occurrences at panel/candidate and detailed-operation levels; report independent sessions and effective contributions separately.
- No zero-loss examples, masked-away actions, empty causal panels, future-bearing rationales, mislabeled tool messages or silently dropped long records. Causal truncation must preserve balances, existing positions, pending orders, exit/risk state and evidence/counterevidence; log loss of optional context.
- No inherited old fixed task mixture or record-count weighting. Use supported capability needs, bounded task-only loss weights and exact global weighted-token normalization. Never weight by payoff magnitude, future winner status or moonshot size.
- V6 floors and the stricter derived proposal must be resolved in a versioned policy before export; absent support blocks the affected full-release claim. The 22-BUY regression must fail through both exporter and actual training loader, not only a standalone test.

## 5. Complete D-category gap-to-work mapping

Each row requires evidence through raw → canonical → admitted task example → independent acceptance, with counts null until measured. `PRESENT` below means legacy/raw assets, not new admission.

| ID | Current evidence | Build needed / stage | Required acceptance |
|---|---|---|---|
| D01 | Store/manifests/partial hash report PRESENT | S0 inventory; S1 rights/split policy; S2 coverage/corrections/lineage | Manifest exact membership/hash/count; source permissions separate; blind intervals/revisions/revocation inherited |
| D02 | Token metadata PRESENT | S2 mint/program/decimals/creator/venue/metadata versions and migration continuity | Literal chain/mint; alias collision; as-of ownership/config; no terminal metadata in past |
| D03 | Trade tape/raw transaction sources PRESENT | S2 exact deltas, failures, instructions, fees/transfers; S3 intent timings | Independently reconciled balances; no transfer-as-trade or intent-as-fill; unknown cost basis retained |
| D04 | Reserve/liquidity/scenario proxies PRESENT | S2 venue truth; S3 quotes/route history; S4 empirical calibration | Size/age/fee/latency/migration supported; tape volume not depth; unsupported exit remains null |
| D05 | Trade/bucket flow PRESENT | S2 ordering; S3 causal chart/flow producers | Partial candle, gap-aware paths, past-only windows; future perturbation leaves prior bytes unchanged |
| D06 | Wallet totals/trade proxies PRESENT | S2 owner/transfer truth; S3 account snapshots/causal cohorts/clusters | Actual vs trade-implied holdings distinguished; open losers; uncertain identity; prior-only teacher skill |
| D07 | Web text/transcript spike PRESENT, rights/semantics unadmitted | S3 versioned spans, catalysts, verification, semantic annotation | Correct mint, language/negation/satire, causal cutoff, provenance, missing modality; eight synthesis tasks |
| D08 | Some URLs/social records PRESENT, graph not certified | S3 original/repost/reply/version graph + coverage | Echo storms not independent breadth; missing feed not no attention; observed origin not global origin |
| D09 | Legacy heuristic states PRESENT | S3 eligible universe + rivals/themes/rotation snapshots | Same-time competition/capital/attention, failed/quiet controls; no hindsight theme winner or fixed favored list |
| D10 | Tape/account fragments PRESENT | S3 opportunities/cash/pending inventory/alternatives | Entire candidate set at arrival; shared cash constraint; watch/expiry; hidden candidate not deliberate skip |
| D11 | Claimed wallet action extraction; no supplied human labels | S3 provenance/capture; S5 A/B and qualified H tracks | H tiers never inferred from profit; stateful action labels; human-review capacity explicit |
| D12 | Legacy outcomes/scenarios PRESENT | S2 lots/realizations; S4 calibrated replay; S5 labeled futures | Realized + conservative open equity, fees/failures, per-source censoring, independent denominator |
| D13 | Rust-derived prose PRESENT with unit defect | S2 verified versioned mechanics; S5 permitted source-backed cases | Mint decimals/rounding/protocol fees proven; test fixtures not training; old AI code/prose not laundered |
| D14 | Engineering legacy track PRESENT | Separate engineering evidence; S7 Rust/action conformance retained | Excluded Qwen SFT by operator; no secret/code-permission crossover; real differential tests |

## 6. Complete expert-gap implementation matrix (G01–G12)

These are additional contracts, not substitutes for D categories. For each row, add schema/source map, temporal/null rules, rights, task/mask, dedup/conflict policy, split ancestry, producer and named failure fixtures before implementation (M:L147). Current status is **NOT CERTIFIED** for all G rows; original checklist has null measured counts. The following gives the complete required work, not invented coverage.

| Gap / master lines | Producer objects and work | Required failure/contrast fixtures | Stages |
|---|---|---|---|
| G01 discovery/attention, 27–33 | discovery_attention_event; eligible arrivals, source/filter/rank, inspection/queue budget, dropped reasons, preemption, oldest unattended position | late unseen candidate; hidden≠skip; risk handling during search; equal-source/compute baseline; sampled discovery-coverage audit | 1 policy; 2 schema; 3 capture; 5 episodes; 7 tests |
| G02 rivals/identity, 35–40 | narrative_competition_snapshot; all known mints, exact Unicode, metadata/image evidence versions, as-of canonical claim, pairwise flow/reserves | alias/logo collision; late rival; multiple winners; leader changes without theme change; visual unsupported explicit | 2/3/5/7 |
| G03 verification/value of information, 42–47 | expert_source_claim + retrieval request/result; atomic span, authenticity, contradiction, query latency, deadline | fake/edited article; satire; right fact/wrong mint; query completes after expiry; uncertainty≠bullishness | 2/3/5/7 |
| G04 local memory, 49–55 | causal episode retrieval; prior resolved outcomes, age/regime/disanalogies, failures denominator, index versions | stale memory; pattern reverses; spoofed resemblance; unresolved future lesson excluded; no live weight update | 1/3/5/6/7 |
| G05 participant replenishment, 56–61 | qualified cohort/flow snapshots; funded/recycled proxies, overlap, retention, fee drain, liquidity/attention | equal volume different participants; recycled/wash-like flow; source outage; new wallet≠new human | 2/3/5/7 |
| G06 influence/nontransferable edge, 63–69 | influence_incentive_context; broadcast delay, owner confidence, follower lags, prior exposure, venue/size | delayed copying; identity/audience ablation; unknown visibility; no causal no-broadcast counterfactual claim | 2/3/4/5/7 |
| G07 thesis updates/clocks, 71–77 | thesis_revision_event; evidence diffs, typed horizons, expected response, distinct expiry/review/hard-hold clocks and reset rules | flat expired catalyst; rising price/worse liquidity; rival takeover; dust/partial exit must not reset hard deadline | 2/3/5/7 |
| G08 impact/partial liquidation, 79–86 | execution_parent_child; inventory reservations, actual recursive reserves, clip denominator, conservative remainder, own-flow attribution; marginal add/hold/reduce/replace/cash | duplicate child/oversell; repeated percentage semantics; own buys not independent confirmation; no free historical slicing/future demand; shared liquidity | 2/3/4/5/7 |
| G09 ownership/return decomposition, 88–94 | owner-evidence intervals + transfer/income ledgers; fees/rebates/referrals separate from trading return | transfer not income; uncertain owners not pooled; rebates not assumed; unsellable residuals retained | 2/3/5/7 |
| G10 intent delivery/race, 95–100 | source→decision→simulation→submission→ack→landing→finality clocks, uncertainty, route/retry/signature/expiry | unknown timeout before retry; duplicate submit; stale hash; reorg; slot ambiguity; LLM busy while exit required | 2/3/4/5/7 |
| G11 venue incentives, 102–108 | effective protocol/config/fee entitlement/claim/migration snapshots, deployer allocation and verified modes | wrong fee recipient; ineligible cashback; incentive regime changes; unsupported new chain; deployment not issuance authority | 2/3/4/5/7 |
| G12 whole sessions/restraint, 110–116 | session_opportunity_audit; idle/loss/failed sessions, capital-time/compute/queue load, cooldown, lesson approval/availability/supersession | highlight-vs-session bias; largest-winner sensitivity; no-edge re-entry; stranded inventory; later lesson never affects earlier state | 1/3/5/6/7 |

The eight v5 corpus objects and D14 exception are enumerated in §4.1. None of these contracts can disappear because the acquisition source is inconvenient or a current schema lacks the field.

## 7. Implementation sequence and acceptance gates

### Rules for every code task

Use a new isolated worktree/branch after reconciling—not resetting—the dirty working tree. New data root: `D:/mev_bot-artifacts/north_star_v1/<run_id>/`. This is a proposed namespace, not existing output. Keep legacy corpora immutable. The run manifest pins master hash + V6 + operator-override record, repo/config/schema/dependency versions.

All paths below are relative to `D:/repos/mev_bot/tools/data-pipeline/` unless an absolute path is given. Each code task follows five independently verifiable steps: (1) write named failing fixtures, (2) execute targeted tests and retain RED result, (3) minimal implementation, (4) GREEN + regression + a bounded source-backed integration slice, (5) independent spec and code review before committing only named paths. Synthetic fixtures are test-only. No speculative expected pass counts.

Future test invocation, from the isolated worktree's `tools/data-pipeline/`:
`python -m pytest tests/north_star/<named_test>.py -q`
Tests and North Star CLI do not exist yet. Implement their entrypoints before documenting runnable build commands. Use a guarded Python main on Windows; no guessed `northstar build` tool.

### Stage 0 — Complete receiver/inventory closure (WP1 + receipt)

**S0.1 Authority and documentation reconciliation.**
- Create `north_star/MASTER_SCOPE_AND_OVERRIDES.md`, `north_star/ASTRA_REVIEW.md` and `north_star/REQUIREMENTS_MATRIX.json`.
- Modify `north_star/00_HOLISTIC_CONTEXT.md` to the evidence-qualified baseline above. Add top-of-file supersession warnings to legacy `STAGE1*`, `STAGE2*`, `STAGE5_LABEL_BALANCE_GATE.md`; do not rewrite historical measurements as new observations.
- Copy/hash-pin governing master/V6/checklist into the portable review bundle without altering originals.
- `tests/north_star/test_requirements_coverage.py`: exactly D01–D14, G01–G12, all eight objects and stages 0–8; every requirement has owner/module/source/status/evidence/gate and all links resolve. Distinguish operator exceptions from missing implementation. Gate: zero silently omitted requirements.

**S0.2 Source inventory and store reconciliation.**
- Create `src/north_star/inventory.py`, `schemas/north_star/source.schema.json`, `north_star/SOURCE_ADMISSION.md`.
- Enumerate full source directories/manifests/orphans/revisions, licensed scope, scripts/commits/configs, time support, existing frozen ancestry and old producer gaps. Footer metadata first, bounded hashes next; never deserialize an untrusted pickle.
- `test_source_admission.py`, `test_inventory.py`: manifest missing/orphan/changed file; stale absolute path; derivative restrictions; permission fields cannot imply later-stage permission; raw source failure not empty coverage.
- Outputs `RECEIVER_ACK.md`, `source_registry.jsonl`, `coverage_intervals.parquet`, `DEPENDENCY_MATRIX.md`, `CAPABILITY_GAPS.md`, source evidence index and dirty-tree preservation receipt.
- Gate: every input is accounted for with an eligibility state; missing rights/data are allowed as explicit gaps, not green capabilities.

### Stage 1 — Protect evaluation and contracts (WP0 + early WP5)

**S1.1 Exposure and protected-boundary registry.**
- Create `src/north_star/contracts.py`, `src/north_star/splits.py`, `schemas/north_star/split.schema.json`, `configs/north_star_v1.yaml`, `north_star/EVAL_PROTOCOL.md`.
- Trace old CPT/SFT/retention/eval/retrieval/teacher/simulator sources and checkpoint ancestry. Reserve development/calibration-validation/validation/sealed-economics and challenge policies outcome-blind. Record deterministic ID assignment, source/time/episode restrictions, allowed metadata inspection, custody/access log, horizon/episode/feature/retrieval embargo rules and multi-parent conflicts.
- Do not select a new trailing band after examining its economics. Treat today's inspected/capture-debug data as development unless demonstrably segregated. Schedule an untouched acquisition window with boundaries/stop/coverage rules before it starts; separate post-model-freeze shadow later.
- `test_splits.py`, `test_split_parent_conflicts.py`, `test_calibration_splits.py`: protected candidate/content/portfolio parent poisons training context; unknown absolute time restricted; changed freeze refused; fitting/retrieval/teacher code rejects held-out IDs; giant components reported.
- Output `EVAL_FREEZE.json`, protected registry + hashes, exposure/ancestry report and relative-only quarantine policy. Gate: enforceable, versioned reservation exists BEFORE fit/curation. Final future row membership can be materialized in S6 without reselection.

**S1.2 Risk/economic, action and admission contracts.**
- Create `north_star/OPERATING_CONTRACT.md`, `north_star/ACTION_CONTRACT.md`, `north_star/DECISIONS_REQUIRED.md`, `schemas/north_star/authorization.schema.json`.
- Freeze known execution scope: **Solana Pump.fun/PumpSwap**; other chains/venues are contextual-only pending separate approval. Add positive contextual-observation and negative unapproved-chain/venue action fixtures. Record known operator decisions and unresolved capital/fractional-Kelly caps, position/correlation/drawdown limits, holding/runner/deadline envelope, latency/freshness, economic uplift/uncertainty/evidence sufficiency, source/reviewer/capture budgets.
- `test_config_contract.py`, `test_authorization_stages.py`: unresolved numeric fields block tuning/promotion/launch, not safe parser fixtures; collection cannot admit training; inclusion cannot launch training/orders; no risk-limit self-update.
- Gate: evaluation/source/action policies frozen; unresolved numbers visibly block only their dependent stages. No invented economic thresholds.

### Stage 2 — Bounded canonicalization (WP2 + schema foundations of WP3/WP6)

**S2.1 Canonical schemas, IDs, units, lineage and time.**
- Create `schemas/north_star/{event,token,venue,content,entity,context,fill,outcome,episode,annotation,export}.schema.json` and `src/north_star/{schema,events,availability,provenance}.py`.
- Include all tables/objects in §4.1, nullable reasons, evidence origin, revision/finality, monotonic/event clocks, dependency availability and split/rights ancestry.
- `test_schema.py`, `test_event_order.py`, `test_availability.py`, `test_entity_links.py`: duplicate delivery vs distinct instruction, loaded keys, reorg corrections, same-time ambiguity, wrong chain/Unicode collisions, late link edits and decimal/overflow rejection.

**S2.2 Safe partition writer and source adapters.**
- Create `src/north_star/io.py`, `manifest.py`, `adapters/{slinky,laserstream,narrative}.py`; isolate legacy parser reuse behind adapters.
- Write temp → close/decode/count/hash → atomic replace → completion receipt keyed by input/config/code/schema. Existing filename alone is never success. Separate accepted/rejected/quarantined/duplicate/revision counts with aggregation cardinalities.
- `test_stage_resume.py`, `test_windows_spawn.py`: interrupted writes, truncated zstd, disk full, duplicate worker, config mismatch, read-locked file, single/multiworker parity and idempotent resume.
- Native Windows source-backed small shard first; measure peak working set, throughput and scratch coexistence; then bound workers and scale only passing transformations.

**S2.3 Exact ledger and venue semantics.**
- Create `src/north_star/{accounting,venues,ownership}.py` and independent reference fixtures.
- FIFO lots, partial exits/re-entry, opening inventory, transfers/airdrops, pending reservations, embedded/separate fees, failed attempts, rent/refunds, realized rebates, migration, unknown cost basis and conservative residual liquidation. Fee/config/decimals effective at event time. No double slippage.
- `test_accounting.py`, `test_execution.py`, `test_ownership.py`: independently hand-reconciled real receipts plus failure fixtures; actual versus modeled tables separate; unknown opening sell cannot create free profit.
- Gate S2: source-backed small-ledger agreement + schema/time/rights invariants + atomic resume before scaled conversion. Capture raw bytes does not wait for the entire historical conversion.

### Stage 3 — Parallel capture and context (WP3 + WP6 acquisition)

**S3.1 Real Windows-owned supervision and retention.**
- Create `src/north_star/{capture_supervisor,retention,health}.py`, `configs/north_star_capture.json`, `scripts/register_north_star_capture.ps1`, `north_star/CAPTURE_RUNBOOK.md`.
- Use a Windows Task Scheduler/service owner independent of the Hermes gateway, with explicit service/user identity, pinned Python/env/workdir, durable logs, named single-instance lock, child/session identity, stop semantics, bounded restarts/backoff and startup replay/checkpoints. Scheduled task installation is a reviewed deployment step; no Linux boot config needed.
- Review the actual capture binary's WSL boundary and child cleanup; native-Windows rebuild is optional, not the critical path. Do not combine production/training subscriptions without measuring commitment/filter/coverage/latency differences and billing semantics.
- Retain raw/full-fidelity evidence needed for canonical accounts/instructions/fees/content/media verification. A deletion candidate needs closed session, validated durable derivatives, dependency completeness, source-specific retention/revocation policy and no audit/holdout lease. Failed compaction blocks purge. Disk pressure pauses, never destroys evidence.
- `test_supervision.py`, `test_retention.py`, `test_capture_health.py`: wrapper/gateway death, duplicate launch, dead child, alive-but-stalled stream, expired budget, corrupt input, active-file purge refused, account evidence retained, restart from last committed cursor. Gate: actual Windows process-owner/gateway-survival rehearsal and exact-target readback after registration.

**S3.2 Market/opportunity/account/execution capture.**
- Create `src/north_star/capture_market.py`, `opportunities.py`, `portfolio.py`; instrument Rust only through independently reviewed bounded/nonblocking telemetry callbacks.
- Log full eligible universe and neutral/control candidates, filter/discovery versions, arrivals/ranks/queue/drop reasons, account/reserve snapshots, clock/coverage gaps, quotes, intents/submissions/unknown/failed/filled states, complete cash/inventory and open-position priorities. Capture actual token program/account updates; global wallet totals are not substitutes.
- `test_opportunities.py`, `test_portfolio.py`, `test_capture_backpressure.py`: unseen/hidden candidates, reused cash, missing source, position risk preemption, order lifecycle and queue overload.

**S3.3 Message and live-media acquisition.**
- Create `src/north_star/{capture_narrative,media_timeline,transcript_ingest}.py`, `schemas/north_star/media_segment.schema.json`.
- Message/post VERSION is unit; keep native IDs, spans, parent/repost links, edit/delete, language, publish/receive/available times and precision. Record platform access/rights separately.
- For Twitch retain stream/source identity and HLS segment sequence, program-date-time if provided, duration, PTS/timebase, first/last receive, discontinuities/ad/blackout/stall gaps, broadcast latency bounds, resampling map and source hashes. If original clock cannot be recovered, mark relative-only / timing-unknown. Do not manufacture an epoch from launcher time.
- ASR/diarization versions + material-number/mint/speaker QA; preserve media needed for source audit where lawful. One completed transcript ≠ verified action episode. Live segments and final VOD dedup by source/time evidence; VOD publication date is not speech date. Guest/private-call consent and visual modality restrictions explicit.
- `test_message_units.py`, `test_media_timeline.py`, `test_transcript_ingest.py`: repeated dates/channel pages, buffering, discontinuity, ad gap, uncertain speaker, late editorial voiceover, no absolute anchor and keyword-only EX_ANTE blocked.

**S3.4 Entity, narrative, propagation, rivals and causal features.**
- Create `src/north_star/{causal_features,narrative_context,entities,propagation,competition,memory,cohorts,thesis}.py`.
- Implement all G01–G12 input contexts and eight narrative tasks; atomic sourced propositions separate truth/opinion/prediction/outcome, rivalry groups known at t, independent breadth, role uncertainty, neutral controls, failed themes, whole-session context, as-of memory and prior-only source/teacher reliability.
- Fresh metadata search may discover a candidate link but cannot establish the past link/availability. Same signature/instruction/owner evidence must confirm on-chain action; voice/ticker/time proximity alone stays candidate attribution.
- `test_causal_features.py`, `test_narrative_causality.py`, `test_propagation.py`, `test_narrative_numeric_join.py`, `test_memory.py`, `test_thesis_clocks.py`: future-mutation invariance, partial candles, wrong mint, delayed link, source outage, rival takeover, own-flow exclusion, stale lesson, hard clock reset.

**S3.5 Annotation governance and measurable support.**
- Create `src/north_star/annotation_ingest.py`, `north_star/ANNOTATION_RUBRIC.md` and blind-context review workflow.
- Separate fact-span codebook from evaluative judgment. H1/H2/H3/A/B/Q rules, reveal timestamps, familiarity, qualification, rights and disagreement; two independent qualified reviewers for initial calibration and material disputes. LLM flags remain review suggestions, not human gold.
- `test_annotation_ingest.py`, `test_semantic_labels.py`, `test_episode_coherence.py`: generated rationale rejected; absent rationale null; H3 not H1; unrelated creator posts not episode; evidence cites correct immutable spans.
- Gate S3: measured independent sessions/creators/regimes and source rights/timing/semantic support for required tasks. No fixed celebrity or token quota. Missing human/visual support is explicit and blocks those full-release claims, not unrelated A/B mechanics.

### Stage 4 — Development-only execution calibration (WP4)

**S4.1 Empirical support and calibration freeze.**
- Create `src/north_star/{execution,replay,calibration}.py`, `configs/north_star_execution_v1.json`.
- Fit on development; validate on separate development calibration slice. Reconcile actual confirmed costs/fills, failure/expiry/latency and exit feasibility by venue/size/congestion. Distinguish reconstructed estimates and unsupported states; freeze models/tolerances/source IDs before sealed evaluation.
- Build baseline/stress/uncertainty bounds, recursive reserve effects, own-flow features and allowed impact envelope. No imagined future buyers, invariant-tape large orders or universal TP teacher.
- `test_execution.py`, `test_portfolio_replay.py`, `test_calibration_splits.py`: fees embedded vs separate, migration outage, order retries, no double cash/inventory, model action divergence, clipping changes reserves, unknown support → INCONCLUSIVE.
- Gate: actual independent execution residuals fit approved tolerances and leave meaningful economic support. Simulator self-tests alone cannot pass. No real capital to obtain missing fills without separate live authorization.

### Stage 5 — Sequential episode reconstruction and labels (WP6 + WP7)

**S5.1 Episode builder and supported targets.**
- Create `src/north_star/{episodes,labels,teacher_selection,sampling}.py`.
- Reconstruct opportunities→plans→state updates→intents→fills/failures→realizations/postmortems. A is observed behavior, B is deterministic supported economics, H tiers retain true origin. Reserve relative-only local tracks; never globally align invented times.
- Decision-state labels include enter/skip/watch, hold/add/reduce/exit/cancel/replace, runner survival, invalidation and re-entry. For **recommended B-track targets**, BUY requires calibrated feasible economics, not just later positive return; recommended SELL requires current inventory/reservation and a supported exit comparison. **Observed A-track BUY/SELL actions remain observed actions even when poor, losing, or unsuitable for our bankroll.** Store observed action, recommended action, feasibility, process quality and imitation eligibility separately; do not filter bad observed actions into fictitious good demonstrations. Absence of trade ≠ deliberate SKIP. Preserve uncertainty bands and continuous labels alongside action categories.
- Teacher eligibility uses only prior eligible evidence INCLUDING open losers/cost uncertainty, not fame or current period profit. No fabricated thoughts or inferred emotion.
- `test_labels.py`, `test_episodes.py`, `test_teacher_selection.py`: lucky bad setup, sound losing setup, no-human-rationale, missing history, false SELL without inventory, future teacher ratings, boundary censoring and no-print vs no-exit distinction.

**S5.2 Sampling and contribution freeze.**
- Maintain natural distribution plus recorded targeted failure/rare-action challenges; eligible-universe denominators and sampling inclusion rules. Preserve true fat-tail winners while cap repeated source/episode views. Independent-session counts and insufficient support must be explicit.
- `test_sampling.py`, `test_class_balance.py`: no duplicated padding, no hidden episode overcontribution, no future-performance weighting, required detailed operations not silently collapsed.
- Gate S5: independently audited coherent examples for each supported required task; observed/model/human semantics unambiguous; configured class policy supported, not forced.

### Stage 6 — Split inheritance and tokenizer-aware exports (WP5 + WP7)

**S6.1 Materialize previously reserved splits.**
- Finish `src/north_star/splits.py` using S1 policy; no new outcome-based membership decisions.
- Materialize all-parent restrictions, horizon/episode/dependency purges and timeless-reference allowlist. Audit CPT/SFT/retention/retrieval and exact/near duplicates. Emit exclusions with reasons, not silently relabel protected parents.
- `test_splits.py`, `test_split_parent_conflicts.py`, `test_calibration_splits.py` across final on-disk dependencies.

**S6.2 Export and loader admission boundary.**
- Create `src/north_star/{export_sft,export_cpt,token_audit,balance_gate}.py`, `schemas/north_star/export.schema.json` and integrate reviewed adapters with the **repository-relative** `tools/training/qwen27b/train_qwen27b.py` (`D:/repos/mev_bot/tools/training/qwen27b/train_qwen27b.py`) only in a new approved recipe.
- Pin tokenizer/revision/template/trainer path. Loss masks distinguish contexts/tool results from authorized target spans. Action counts must survive filtering, segmentation, packing and masking. Task contribution measured in post-mask tokens, weighted denominator verified across ranks. No old Rust or generated retention data slipped into the trader mix.
- `test_exports.py`, `test_context_budget.py`, `test_token_masks.py`, `test_weighted_loss.py`, `test_class_balance.py`, `test_training_admission.py`: serialized 22-BUY/absent-SELL fails exporter AND loader; empty causal rows, overlength critical-state loss, zero target tokens, unsupported target task, post-freeze edits and candidate export all refused.
- Gate S6: re-read every serialized file and use actual trainer tokenization path for counts/hashes; decoded samples by task/tier + exact distribution/drop report. Still quarantined candidate output until S7 approval.

### Stage 7 — Independent whole-corpus certification (WP8 + v5 conformance)

**S7.1 Deterministic certifier and negative controls.**
- Create `src/north_star/{certify,manifest}.py`, `north_star/RELEASE_CHECKLIST.md`.
- Check all files: schema/FK/IDs, raw/manifests, accepted/reject/quarantine/duplicates/revisions/cardinality, rights/revocation, provenance/time/units, all-parent anti-joins, mask/token counts, per-task/class support and hashes. Broken/empty required capability cannot average into a pass.
- `test_certify.py`, `test_rights_revocation.py`: deliberate corruption, unknown license, missing output, empty required action, leakage, stale hash, revoked parent, false receipt all fail. Structural whole-corpus checks are exhaustive; human semantic sample scope/uncertainty disclosed.

**S7.2 Independent semantic/economic review and Rust conformance.**
- Independent principal data/ML reviewer checks exporters/certifier; qualified semantic/ledger reviewers check source-backed contexts and reconciliation. Report disagreement/unresolved rates and independent sessions, not model self-approval.
- Map every action/feature/clock/unit to concrete Rust symbols/commit/config/override precedence. Differential no-keys replay canonical reference vs actual Rust state→intent→order→fill semantics; raw violations separate from wrapper blocks. Test late/model-unavailable exits, partial fractions, reset timers and prompt-injection boundaries.
- `test_semantic_conformance.py`, `test_evaluation.py`: balanced narrative/class/protocol support plus all adversarial cases M:L137,419–432. No mean score hides a failed role.
- Publish task states READY/LIMITED/BLOCKED/QUARANTINED with separate structural/rights/semantic/temporal/economic PASS fields; the separate exact evidence states `IMPLEMENTED_AND_TESTED`, `DATA_PRESENT_UNADMITTED`, `ADMITTED_WITH_SUPPORT`, `MISSING_SOURCE`, `UNSUPPORTED`. User target full release remains blocked if required narrative coverage fails.
- Gate: complete independently signed/attributed evidence, exact source/license/token report and explicit operator inclusion approval. Code review is not source admission.

### Stage 8 — Portable handback, not automatic training (WP9)

**S8.1 Reproducible release and training proposal.**
- Create `src/north_star/handback.py`, `north_star/TRAINING_PROPOSAL.md`, `north_star/RUNTIME_CONTRACT.md`.
- Pin seed candidates/upstream revision/base-vs-instruct/ancestry, preserve old checkpoints, and reconcile V6 parent-lineage wording before seed approval. No silent reuse of old LR/epochs/task weights/retention percentages; preserve prior approvals without treating them as the new run proposal.
- Full-parameter resource plan: actual native target hardware/library/AIO tests, model/checkpoint/offload/scratch coexistence, no automatic boot/mount/format operations, loss/validation/boundary stopping and resume-not-restart. No model launch this stage.
- `test_handback.py`, `test_training_admission.py`: portable path resolution, every artifact hash, exact trainer input/mask parity, unsigned candidate rejected and no implicit training/live authority.

**S8.2 Evaluation harness and runtime measurement (WP9; foundations begin earlier).**
- Create `src/north_star/evaluate.py`, `runtime_metrics.py`, a versioned baseline registry and evaluation protocol. Engineering implementation belongs to the data/evaluation owner; independently review portfolio-accounting and statistical outputs. Use source-backed development sequential sessions to exercise cash, deterministic and simple causal baselines, divergence-specific inventory, costs, paired-session uncertainty and unsupported-support reporting before any training.
- `test_evaluation.py` and `test_runtime_metrics.py` must verify full sensor→queue→context→decision→order→landing timestamp boundaries, timeout/unknown outcomes, p50/p95/p99 summaries and concurrency/deadline behavior. No inference or landed-trade measurements are fabricated from simulated timing. Separate no-key engineering rehearsals from actual approved-model measurements and separately authorized live execution.
- Output reproducible baseline/report fixtures and instrumentation receipts before handback. Model-dependent economic/latency acceptance is exercised after separately approved training; OP-G4–G6 remain unpassed.

**Required bundle (M:L438–448):**
1. `RECEIVER_ACK.md`, `BUILD_PLAN.md`, stage/config/git/version fingerprints.
2. `source_registry.jsonl`, `SOURCE_LICENSE_TOKEN_REPORT.md`, `coverage_intervals.parquet`, `CAPABILITY_GAPS.md`.
3. All canonical schemas/tables and complete lineage.
4. Observed/estimated/counterfactual economics separately; episodes and narrative tasks with origin/review evidence.
5. `EVAL_FREEZE.json`, split registry and contamination/ancestry report including simulator/retrieval.
6. `ANNOTATION_CALIBRATION.md`, source-span adjudications and unresolved queue.
7. `DATA_CARD.md`, `TASK_MIX_AND_TOKENS.json`, tokenizer/mask/template/config hashes and support/drop reports.
8. `TEST_RESULTS.json`, `SEMANTIC_AUDIT.md`, `ECONOMICS_RECONCILIATION.md`, `NARRATIVE_QA.md` containing real execution receipts.
9. `MANIFEST.json`, SHA256 every release file, reproducibility and interrupted-resume evidence.
10. `TRAINING_PROPOSAL.md`, `DECISIONS_REQUIRED.md`, signed admission request plus operator inclusion decision.

### After handback — separate operational gates, not dataset completion

Implement/test the evaluation harness before training, but actual OP-G4 model economics follows the separately approved training run. OP-G0–G3 are operating/source/truth/learning sufficiency; OP-G4 offline comparison, OP-G5 fresh frozen-system shadow, OP-G6 separately authorized limited landed trading. No capital escalation, live RL, automatic feedback ingestion or extended training by implication.

Freeze cash/current deterministic/simple causal statistical/base-or-CPT/current-policy/North-Star comparators with identical opportunities/capital/latency/risk. Predeclare numeric-only, narrative-only where meaningful, combined, minus-propagation/portfolio/history/rivals/fresh-memory/human-rationale, latency-matched and identity/influence-blinded comparisons. Preserve full-session natural prevalence, open losers, failures, stranded funds, paid-data/inference costs and self-impact support. Use paired session/day dependence-aware intervals and preregistered absolute/incremental uplift/risk/evidence-sufficiency thresholds. Inconclusive evidence stays INCONCLUSIVE. Different model actions carry their own inventory forward rather than resetting to the teacher path. Passing dataset checks cannot establish profitability (M:L745–793,812–821,933–935).

## 8. Critical path, parallelism, resources and decisions

### First implementation tranche (no additional training/order authority)
1. S0.1–S0.2: reconcile authority, preserve dirty sources, complete D/G/rights/producer registry and evidence ledger.
2. S1.1: protect outcome-blind evaluation + ancestry/relative-only policies; S1.2 records unresolved numeric gates without blocking safe tests.
3. S2.1–S2.3: schema/availability/atomic writer and a small independently reconciled ledger, then bounded scale-up.
4. In parallel, secure approved capture evidence through S3.1–S3.3. Treat the present stream as an acquisition experiment until media-clock/rights/join QA passes. Avoid losing an unrepeatable source while canonicalization is being implemented.
5. S3.4–S3.5 full narrative/context and reviewer support; S4 calibration only after its numeric/source/development gates.
6. S5 → S6 → S7 → S8, each requiring real receipts and independent review.

**This is not a skip-to-training plan.** Source acquisition and eligible local transformations may proceed in parallel, but teacher fitting, calibration, semantic curation and final exports cannot outrun their gates. Human rights/review and enough independent forward sessions may be critical-path dependencies. Do not promise a completion date from bytes/day or one VOD.

### Resource rules

Read-only resource check at 2026-09-09 08:41:57 PT observed D free bytes `377897562112`, Windows total physical RAM `274428248064` and available RAM `253739450368`; these are point-in-time readings, not reserved budgets. Recheck immediately before every heavy task. Keep about 12% RAM headroom and predeclare scratch/source/output/checkpoint coexistence, per-worker cap and disk floor. Start one worker, measure, then approve bounded concurrency. No uncontrolled full Parquet/Python-dict materialization, no all-data pickle load, no collecting forever without a budget. Data hash/config checkpointing and durable reviewed Windows process ownership are required (M:L409–414).

### Decisions that cannot be fabricated

- Source grants/evidence and permitted transform/train/redistribution scope, including Twitch/interviews and third-party embedded content; operator-reported Slinky approval recorded as supplied, exact grant lineage completed.
- Reviewer/annotation resources for human semantic/fact/economic adjudication. No supplied H1/H2 history; action-only is not a substitute for claimed expert rationale.
- Numeric capital/order sizes/fractional-Kelly/risk/correlation/holding/runner/deadline/latency and capture spend limits. Derive recommendations from development data only, then obtain required approval.
- Preregistered minimum economic uplift, downside/error tolerances, confidence/sufficiency/stopping rules and fresh protected window.
- Exact detailed action/reporting mapping and final balance policy; do not silently lower the stricter drafted floors.
- Text-only limitation versus separately approved multimodal extension for genuinely visual narrative tasks. Qualified source-backed descriptions may support text tasks; model-generated descriptions do not create missing evidence.
- Seed lineage/revision, optional CPT, full-parameter training recipe, general-retention provenance and explicit training Go only after data approval.

## 9. Documentation and completion discipline

For each completed task update the review ledger with requirement IDs, source inputs, code/config/schema hashes, exact command, exit status, receipt/artifact path, accepted/rejected/quarantined counts, reviewer identity, limitations and next dependency. Append old findings/corrections rather than erasing history. Source support and task status must be generated from evidence records where possible; no narrative status inflation.

Before reporting a stage complete, demonstrate:
`master requirement → source evidence → implemented producer → real tested output → independent acceptance → explicit stage permission`.
A task can have passing software tests and zero admitted rows. A million counterfactuals do not establish a thousand independent episodes. A source with unresolved timing/rights does not become eligible because other fields are rich. A ledger fact is not process supervision. A frozen spec is not a wired runtime gate.

**Full dataset definition of done:** all in-scope master capabilities have measured admitted support and independent gates; unresolved optional/overridden tracks are explicitly identified; required narrative/semantic/economic gaps are not disguised as LIMITED completion; all output hashes, splits, origins, token masks and permissions reconcile; operator inclusion approval is recorded. Training and deployed profitability remain separate outcomes.

## 10. Supplementary audit evidence

### Mechanical requirements-reference check

A Python parser checked this plan against the original master: all stages 0–8, D01–D14, expert G01–G12, eight v5 objects and all 29 named master pytest target filenames are represented. WP7 `test_code_quality.py` is explicitly separated by the trader-only override, not a Qwen admission test. All 14 added v3 acceptance filenames (M:L419–432) are present. This checks reference coverage only, not that requirements are implemented or semantically certified.

Sidecars beside this Markdown: `.coverage.json` records the structural coverage check; `.evidence.json` records source-footer schema/row observations, master hash, repo HEAD and namespace absence checks. These are audit artifacts, not new data-pipeline code or training releases.

### Appendix WP → governing stage mapping

- WP0: Stage 1 operating/evaluation contracts; required later numeric approvals block dependent tuning/promotion.
- WP1: Stage 0 discovery registry; Stage 2 machine source admission/lineage; Stage 7 final rights/source inclusion check.
- WP2: Stage 2 event/accounting truth.
- WP3: Stage 2 availability/schema foundations; Stage 3 causal features and narrative context.
- WP4: Stage 4 execution calibration; Stage 2 reference mechanics fixtures precede fitting.
- WP5: Stage 1 outcome-blind boundary reservation; Stage 6 inherited split materialization, never late selection.
- WP6: Stage 3 source/annotation acquisition and calibration; Stage 5 eligible human episode assembly.
- WP7: Stage 5 episode reconstruction, Stage 6 exports; code-teaching track separate by override.
- WP8: Stage 7 whole-corpus certification.
- WP9: Stage 8 handback/proposal; build evaluation harness before training, exercise model/economic/shadow/live gates only with their later approvals.

### First TDD unit: authority matrix completeness

This is an illustrative implementation task, not executed code and not a new CLI. Start after S0 dirty-workspace preservation. Other S-task packages must be decomposed into similarly small units before assigning an implementer; this document is the end-to-end program plan, not a claim that a whole stage takes minutes.

Files: `tests/north_star/test_requirements_coverage.py`, `src/north_star/requirements.py`, then the reviewed `north_star/REQUIREMENTS_MATRIX.json`.

1. Write a test-only full ID fixture and remove `G08`; assert validation fails with the missing exact ID. Add a fixture with all IDs and an explicit D14 override; assert requirements are still enumerated rather than deleted.
2. Run `python -m pytest tests/north_star/test_requirements_coverage.py -q`; save its actual failing receipt.
3. Implement only ID-set validation first:

```python
EXPECTED = {f"D{i:02d}" for i in range(1, 15)} | {f"G{i:02d}" for i in range(1, 13)}

def validate_requirement_ids(rows):
    ids = [row["requirement_id"] for row in rows]
    if len(ids) != len(set(ids)):
        raise ValueError("duplicate requirement_id")
    found = set(ids)
    if found != EXPECTED:
        raise ValueError({"missing": sorted(EXPECTED - found),
                          "unexpected": sorted(found - EXPECTED)})
```

4. Re-run the targeted test, then add separate tests for unresolved producer/evidence, source-to-field mapping, all stage IDs, object inclusion and explicit override semantics. Do not make this toy ID validator a dataset-completion gate.
5. Obtain independent spec/code review; stage only the named files and commit with the real test receipt. Next unit is source/rights state validation, not a bulk data rebuild.

### Additional directly inspected attribution defect

`src/confirm_stream_mint.py:L128–151` retrieves at most the latest 1,000 signatures mentioning the mint, limits inspected transactions with `max_tx=6`, and treats string-form account keys as signers. Its result truncates signature identity to 16 characters and reports `CONFIRMED` from signer intersection without validating buy/sell/token-owner/balance semantics. This is a bounded discovery prototype, not a complete on-chain confirmation pipeline. S3.4 must preserve full signatures, prove supported search coverage, use message-header/parsed signer roles correctly, reconcile instructions and owner deltas, and distinguish unknown/missing/negative/confirmed outcomes. No sampled failure proves bundle blindness.

### KOL output granularity and hindsight fields

`src/kol_decision_extract.py:L75–104` writes one aggregate per `(kol, mint)`, not ordered decision episodes. Its min/max price, total buys/sells and cross-KOL touched-coin consensus span the entire observed window. Retain this as a discovery/index product only; these values cannot be causal features or independent observed judgments without time-specific reconstruction. The original trades may support A-tier action sequences after source/owner/time/ledger verification; the summary itself does not satisfy D11. File title `kol_decisions.jsonl` is not proof of decision granularity.

### Raw retention loss from known roots; verified recovery avenue

Both original master-named manifests were compared by exact filenames against the capture directory and artifact-store raw directory (not a whole-disk search):
- August 23 session `20260823_133256_000398`: 185 manifest parts; 1 in store, 0 in capture directory, **184 missing from both**.
- August 24 session `20260824_053543_000288`: 369 manifest parts; 368 in store, 0 in capture directory; **part0368 absent**.

Read-only `git cat-file --batch-check` verified that **all 184 missing August 23 parts have HEAD blob objects with matching manifest byte sizes**. One actual blob (`part0000`, 19,505,136 bytes) was streamed from Git and its SHA256 matched the capture manifest. The remaining 183 need content hashing, and all recovered files still need publication to a new immutable artifact-store location; the August 24 tail has no corresponding HEAD blob. Do not run git cleanup/reset/history rewrite or purge while recovery is outstanding. The `.hermes/plans/2026-09-09_083313-north-star-raw-presence-audit.json` ledger lists every expected file, known location and Git recovery metadata. No files were restored/deleted during this review.

Make S0.2 recovery/reconciliation a first-priority preservation task: stream each required blob to a unique temporary file in the new store, verify manifest SHA256 and bytes, atomically publish and register its provenance without checking deleted files back into the transient capture directory. A size match is evidence of a recovery avenue, not proof of content integrity. Investigate other protected copies for the missing tail; retain its coverage gap if unrecoverable. This invalidates the earlier all-raw-intact assurance while avoiding an unsupported permanent-loss claim.

### Independent review integration and parent-verified blockers

Four independent read-only reviews completed: legacy implementation, capture/narrative, master requirements, and the written plan. Their completion is evidenced in the delegation transcripts; their conclusions were treated as hypotheses until checked against source. Review transcripts reside under `C:/Users/Alon/AppData/Local/hermes/cache/delegation/live/` in `deleg_91d6ec7a`, `deleg_ad204084`, and `deleg_d47e827c`. These are not independent corpus-admission signoffs.

**Verified numerical defect:** `src/build_slinky_gold_v3.py:L1011–1058,1555–1559` passes a common benchmark exit token amount into every size scenario and computes net PnL without the supplied entry/exit tips. Parent executed only the three extracted pure functions (no builder import/run or corpus write) on explicitly synthetic test inputs. At identical initial/exit reserves, the 0.05 SOL case received the same 0.4838709677 SOL proceeds as the 0.5 SOL case and reported 85,706 basis points net return. Changing both tips from zero to 100,000,000 lamports left reported 0.5-SOL net PnL unchanged. This is an actual failing economic invariant, not a conjecture from filenames. Trace producer versions and affected columns/exports; quarantine dependent multi-size targets until repaired and independently reconciled. Do not declare every legacy counterfactual bad without that lineage trace, or rely on earlier positive-class counts as certified BUY support.

**Verified future-field risk:** `src/rebuild_v3_corrected.py:L663–681` puts `capture_end_ms`, future-horizon `right_censored_*` and `observed_no_trade_*` fields in L1. These may exist as label/audit metadata but MUST NOT enter causal inputs. `STAGE1b_SCHEMA_REGISTRY.md:L49–51` incorrectly treats observed-no-trade fields as causal without distinguishing past windows from future horizons. Require lineage-aware allowlists and mutation tests, not layer-name trust or field-name denylist alone.

**Verified decoder inconsistency:** `src/build_laserstream_gold_v3.py:L724–728` concatenates static keys + loaded-readonly + loaded-writable. The correct transaction-key ordering must be verified against authoritative SDK/transaction fixtures before reuse; current code contradicts the expected static + writable + readonly resolution. Add a real versioned transaction with both loaded groups to S2.1's independent decoder fixtures and trace affected decoded gold. Do not fix output addresses by guesswork.

**Verified loader mismatch:** `tools/training/qwen27b/train_qwen27b.py:L118–145` uses first-found group identity and hash-based train/validation assignment. This does not implement the master’s chronological all-parent inherited split. S6 must consume the sealed split registry rather than resplitting North Star records through this helper.

### Final scope refinements from plan review

- **Permissive-only admission is explicit:** unknown AND known restrictive/noncommercial licenses are blocked. Private training-only permission is insufficient under the current policy. Source-specific transform/train/redistribution limits and derivative rights are inherited; reviewer approval cannot convert a restrictive grant to an admissible one. Add negative fixtures for these exact cases in S0/S1/S7 (M:L351–357,856).
- **Long-job supervision applies to aggregation/export/certification as well as capture.** Extend S3.1's reviewed Windows job owner to bounded jobs with immutable stage/config identity, exit/log receipts and gateway-survival/resume tests. Do not satisfy M:L413 by monitoring only the collectors.
- **V6 hard-gate policy is unresolved, not silently overridden by the derived 10,000/1,000 proposal.** Freeze denominators and approve the final policy before export; retain original V6 and stricter proposal as distinct evidence. No fabricated labels or arbitrary quotas. Neither a rule-of-thumb percentage nor passing a count threshold proves 27B learning sufficiency.
- **Current-training preservation is Stage 0 evidence:** verify completion, selected checkpoint and storage preservation through an existing signed receipt or explicitly obtained report. Being booted into Windows is not proof of selected/preserved Linux SFT. No boot or training-tree edits to obtain it.
- **No service deployment permission implied:** proposed Windows task/service installation requires scoped review/authorization, even though the user has requested useful supervision. Plan and test ownership first; avoid unrelated service changes.
- **Full narrative collection does not mandate unapproved vision training:** preserve visual-dependency evidence, support eligible text tasks and mark visual tasks unsupported unless a separately approved compatible pipeline exists. Do not call a required missing semantic capability complete through an AI description.

The statements above describe the read-only review at plan creation. Implementation has since been authorized and started: consult `00_HOLISTIC_CONTEXT.md` and `BUILD_*_RECEIPT.md` for task completion, recovery, tests and branch status. This plan does not itself certify any stage.
