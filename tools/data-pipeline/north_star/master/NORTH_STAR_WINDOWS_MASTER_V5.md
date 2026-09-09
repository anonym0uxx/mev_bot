# North Star v5 — Expert-informed reasoning and execution gaps

## Status, precedence and scope
This amendment extends unified v4: ONE model handles market reasoning, authorized execution and Rust development. v5 governs conflicts; existing licensing, causal, risk, evaluation, seed-selection and no-current-SFT-change rules remain. These are corpus requirements and research hypotheses, not authorization for live orders, manipulation, paid acquisition or training. No guarantee of named-trader profitability. The existing raw on-chain and Rust coverage stays, subject to admission checks.

We cannot observe private thoughts from a transaction. Separate (a) what a speaker publicly claims, (b) directly observed and independently linked actions, (c) inferred hypotheses and (d) objectively reconciled economics. This review retrieved public text/transcripts, not independently replayed named wallets or watched the visual trade sequences. No new interview content is admitted to training; permissive licenses remain unverified. Our plan prose is specification, not model-generated training examples.

## Evidence and identity limits
- **Prosper:** preserve user alias `pro6per (prosper)` literally in the source registry. The retrieved interview is published by Ethan Prosper/@pr6spr and links that handle; this supports a candidate identity, NOT automatic equivalence of every similarly named account or wallet. Do not silently normalize aliases. [S1]
- **Cented:** interview hosted by rasmr, with a transcript of a guest presented as Cented. Source supports reported views about narrative virality, speed, tracked wallets/streaming and market participation. Speaker turns lack reliable timecoded diarization; no claims about specific fills/PnL are independently verified. [S2]
- **Megga:** retrieved channel links Twitch megga, X megga and TikTok meggafaze; its linked trade-review video is uploaded by Megga. It mixes commentary over prior clips with purported live audio. Separate recording time, commentary time and publication time. Numerical totals/PnL in the narration are inconsistent and the speaker calls the UI PnL buggy. This is not audited profit evidence. [S3, S4]
- **Cupsey:** guest presented as Cupsey in a ThreadGuy/Jack Duval discussion. Supports investigation of participant replenishment, incentive regimes and creator-fee economics. Multi-speaker views must not all be attributed to Cupsey. Legacy wallet labels remain unverified identity candidates. [S5]
- Search also returned an entry-strategy video uploaded by **starwifpump**, not Megga. It was excluded from named-Megga evidence. A search result near someone's name does not establish authorship. [S6]

### Sources: research-only, not training admission
S1. https://www.youtube.com/watch?v=mK54UwtIHIk — Ethan Prosper, *How Ethan Prosper Makes $100K/mo Trading Memecoins (Full Interview)*; metadata upload 2025-10-05. Relevant transcript: new-pairs preference; adding Telegram discovery after missing opportunities; changing narrative response and bundle pressure. Title earnings are an unverified claim.
S2. https://www.youtube.com/watch?v=w0WtVwlR3cY — rasmr, *Cented Reveals the DARK TRUTH of Memecoins in 2026.. (INTERVIEW)*; metadata upload 2026-04-09. Relevant passages discuss continuous observation, tracked-wallet influence, narrative interpretation, volume and speed. Do not inherit speaker certainty that a coin is guaranteed to run.
S3. https://www.youtube.com/watch?v=hxXZTE9wWdA — Megga, *How I Made $20,000 Trading Memecoins In ONE DAY (FULL GUIDE)*; metadata upload 2026-09-01. Distinctive source locators: article check in the memecoiners example; competing image/ticker variants; waiting for a shrub-name variant; small exit clips versus price impact; keeping a residual position. Editorial hindsight and live speech require separation before annotation. Commentary's trademark/copyright and personal-relationship claims are not certified factual metadata.
S4. https://www.youtube.com/channel/UCOjxpeSVbHn75QoVizg7dDg — channel returned usable public links alongside an error banner. Use linked-video and profile provenance, not the page's error text as semantic content.
S5. https://www.youtube.com/watch?v=efq61vbmotM — threadguy, *Cupsey & Jack Duval on Memecoins, Crypto's Future, and More | TG Podcast*; metadata upload 2025-12-17. Publisher chapter locators 04:57 viral memes, 28:53 creator fees/incentives, 49:51 flows, 59:38 deploying versus trading. Chapter starts are not verified per-speaker quote timestamps. Fee/incentive descriptions are dated claims, not current protocol truth.
S6. https://www.youtube.com/watch?v=-czWNiE5Qmw — starwifpump; rejected as named-Megga attribution.
A linked Prosper website returned challenge/encoded content, so it was not used to establish wallet identity. No access barriers were bypassed.

## Delta register: already present versus now operationalized
v4 already includes narratives, rotation, liquidity, no-trades, sequential positions, multi-task Rust and independent evaluation. The additions below sharpen under-specified mechanics. They do not claim all were wholly absent or that each named trader uses all of them. Each G-ID must receive a measured coverage status, producer, source eligibility and acceptance result in the receiver report; zero evidence stays a gap.

### G01 — Discovery and attention allocation before BUY/SKIP
**Gap:** a candidate panel omits what the trader searched, hid, missed, revisited or never received. S1/S3 motivate this; profitable outcomes alone cannot label a missed discovery as negligence.
**Collect:** discovery_event_id, source/channel/filter version, first_observed_at, candidate eligibility at t, screen/list rank, inspected/hidden/watchlisted state with actor, alert visibility, inspection start/end, available time budget, refresh cadence, feed outage, open positions and queued alerts. For human attention use authorized interaction evidence, not invented gaze. Model analogue: query/tool calls, context truncation and queue delays.
**Tasks:** choose inspect/verify/watch/ignore within a measured budget; revisit after new evidence; keep held-position risk monitoring alive while searching.
**Test:** arrival-stream replay with unseen candidates absent until arrival; equal discovery sources and compute budgets across baselines; a hidden candidate is not automatically an informed SKIP. Report missed opportunities using hindsight only as evaluation labels.
**Additional contract:** record priority, maximum unobserved position age, preemption/coalescing policy, dropped-event reason and independently sampled discovery-coverage audits. Evaluate whole discovery-plus-selection alongside fixed-panel evaluation; no automatic label that every later pump was an executable missed trade. Bursty launches plus concurrent sandbox development must not starve open-position risk handling.
**Rust bridge:** priority queues, source heartbeat and bounded-latency watchlist scheduler.

### G02 — Narrative tournament, canonical identity and rival monitoring
**Gap:** strong theme does not identify the token that captures it. S3 describes image/ticker/name competition and a rival taking attention while the trader watches PnL.
**Collect:** catalyst_id, all linked mints, launch/event/availability times, exact name/ticker/Unicode, metadata version, image hash and rights, source text span, entity-link confidence, original/revival/derivative/impersonator relation, source-endorsed address evidence, prior as-of format conventions, pairwise attention/flow/reserve trajectories. `canonical` is a time-varying evidence claim, not the eventual winner. Reuse D07–D09 without collapsing competitors.
**Tasks:** compare variants, wait for a qualified representation, abstain on unresolved address, maintain competitor watch during holding, revise or rotate net of costs. Token-image cultural fit can matter, but no unsupported claim that an image is objectively better.
**Test:** late-arriving rival, same ticker/different mint, misleading Unicode/logo, multiple viable winners, and unchanged theme with changing leader. No final-winner identifier or future rank in input.
**Rust bridge:** entity graph, versioned rival-group joins, hot-path leader-change alerts.

### G03 — Source verification, ambiguity and information worth paying time for
**Gap:** checking an original article can resolve a rumor but consumes the entry window. S3 is a direct motivation; source claims about copyright or identity may themselves be wrong.
**Collect:** claim atomic text, reference URL/content hash/exact span, author and publication evidence, authenticity status, contradiction history, query requested/completed times, information unavailable reasons, age and estimated decision deadline, verification outcome observed at t. Separate factual confidence, narrative attractiveness and executable expectancy.
**Tasks:** answer 'what is this meme and why this token?', identify missing evidence, choose query/wait/abstain versus act only within allowed verified inputs. Probabilities require explicit event/horizon and calibration.
**Test:** fake article, edited tweet, sarcasm/mistranslation, right fact/wrong token, verification arriving after order expiry; latency-inclusive value-of-information comparison, not free omniscience.
**Rust bridge:** retrieval caches keyed by content version/as-of, timeout contracts and evidence-pointer validation.

### G04 — Local market memory and changing conventions
**Gap:** fixed rules like 'animal + coin name wins' can decay. S3 describes recent repeated patterns; S1 describes once-successful tweet narratives stopping working.
**Collect:** as-of comparable episodes, eligible denominator including failures, similarity features, age, regime/venue/catalyst family, observed time-to-response, source coverage and retrieval version. Only resolved historical outcomes available at t enter the memory; current unresolved inventory remains conservatively represented.
**Tasks:** retrieve analogues with disanalogies, distinguish novelty from repeated saturation, update belief after disconfirmation, decline analogy with weak support.
**Test:** regime reversal and spoofed resemblance; compare no-memory/causal-memory/stale-memory baselines. Fit retrieval and analogue selection only on development data, preserving sealed split rules. Live context updates are not online weight training.
**Rust bridge:** temporal feature store and snapshot-expiring analogue retrieval.

### G05 — Who can buy next? Flow quality and participant replenishment
**Gap:** raw activity may be repeated capital among the same traders rather than expanding demand. S2/S5 emphasize participation and flow; identity of a 'new human' is not derivable from a new wallet.
**Collect:** qualified active-wallet cohorts, first_seen-in-dataset confidence, funded-versus-recycled balance flows, turnover, concentration/overlap across rival tokens, fresh liquidity, fee drain, independent attention, catalyst-to-buy latency, cohort retention, launch rate versus demand and market-relative performance. Exchange funding/new address is a proxy, not proof of retail onboarding.
**Tasks:** distinguish viral-but-unbuyable, attention-without-inflows, liquid expansion and recycling; rank expected incremental participation with uncertainty, not force every narrative bullish.
**Test:** equal volume with different wallet churn/concentration, wash-like flow, shrinking cohort and source outage; no equating missing coverage with zero demand.
**Rust bridge:** causal cohort aggregates, reversible transfer classification, exact flow/cost reconciliation.

### G06 — Reflexivity, tracked-wallet influence and nontransferable edge
**Gap:** a public trader's action or broadcast can alter the market being imitated. Their fills may rely on audience response unavailable to us. S2 discusses this explicitly; S3 includes comms and wallet alerts.
**Collect:** broadcast/call time plus delay, publicly observable stream state (not inferred absence), observed owner/follower action lag distributions, follow-on flows, identity confidence, funding/shared-entity uncertainty, exposure before announcement and venue size. Do not infer collusion or private intent from correlated trades.
**Tasks:** separate information discovery from delayed copying; identify trades requiring unavailable audience/private information; refrain when expected residual edge after our arrival is uncertain.
**Test:** matched public/private-visibility strata only where observed, delayed-follower replay, removal of named identity, audience-feature ablations and self-impact stress. Observational correlations cannot identify what price would have done without a broadcast; mark that counterfactual unsupported.
**Boundary:** this is defensive market-understanding data, not instruction to mislead followers, coordinate pumping, fabricate volume or manipulate supply.
**Rust bridge:** timestamped provenance/lag analysis and anti-copy-chase expiry checks.

### G07 — Conviction updates, thesis clocks and falsification
**Gap:** confidence must change with observed evidence, not merely price/PnL; an unfulfilled expected response can matter before a hard stop. Existing invalidation fields need event-time semantics.
**Collect:** thesis_id/version, typed horizon, required and disconfirming signals, conditions expected by when, review triggers, time since catalyst/entry, attention-to-flow conversion, relative strength, volatility/liquidity context and evidence missingness. Human confidence only when expressed; model scores not retrofit as human feelings.
**Tasks:** test buy/hold/add/reduce/exit/wait after sequential evidence; separate failed thesis from temporary noise, price stop from time stop, and stale thesis from fresh re-entry setup.
**Test:** flat price with expiring catalyst, favorable price with worsening liquidity, delayed authentic confirmation, rival takeover, justified no-change. No posthoc redefinition to make every trade correct.
**Timer contract:** distinguish order expiry, thesis expiry, soft no-progress review, hard exit deadline and maximum cumulative hold. Define meaningful progress, clock domain, outage behavior and allowed reset evidence. Dust prints, a partial exit or a trivial new high must not silently extend the hard deadline.
**Rust bridge:** versioned thesis state machine, expiry timer and event-triggered reassessment.

### G08 — Endogenous impact, staged liquidation and residual risk
**Gap:** splitting exits is not automatically better; our prior trades change reserves and repeated clips add fees/delay. S3 explicitly discusses clipping rather than dumping all inventory and notes buggy marked PnL.
**Collect:** parent decision/child order IDs, confirmed pre/post reserves, aggregated owner inventory, quote age, clip units and denominator (initial versus remaining tokens), pending amounts, min-out, fee/tip/compute settings, realized proceeds, new demand between clips, price impact and conservative residual liquidation value.
**Tasks:** immediate liquidation versus paced clips versus residual position under risk/deadline constraints; handle adverse flow and partial fills, cancel remaining intents when invalidated. Do not hardcode any celebrity's percentages.
**Test:** recursive reserve updates and self-impact in replay; no sell above unreserved inventory; repeated percentage semantics; no future buyers assumed to arrive; no free slicing into the historical price path. For significant market impact, simulator uncertainty and off-policy support must bound claims, not a fictitious exact replay.
**Self-confirmation guard:** track features including/excluding verified own flow and own contribution to a volume/price trigger. Own buying is not independent external confirmation; actual reserve changes still count for tradability. Test repeated adds/exits, not only isolated trades.
**Marginal sizing contract:** compare next-unit ADD/HOLD/REDUCE/REPLACE/CASH under current/pending inventory, shared exit liquidity, exit-fee reserves and replacement costs. Do not double-count common liquidity or cash; define rebalancing hysteresis to avoid costly churn.
**Rust bridge:** parent/child state machine, reservation ledger, executable equity and idempotent retries.

### G09 — Inventory ownership, account aggregation and return decomposition
**Gap:** observed multi-wallet activity is not equivalent to multiple independent traders or extra capital. S3 describes using several wallets; seed list includes 'friend'/'dev' labels that do not prove common control.
**Collect:** wallet_owner_evidence, effective intervals, account role, transfer lineage, opening inventory uncertainty, all owned wallet exposure, side payments, creator fees, cashback/rebates, referral/sponsor income separated from trade PnL, fee-entitlement and claim times.
**Tasks:** consolidate only proven ownership; model uncertainty for suspected clusters; assess trading-only returns versus other income and reproducibility at our bankroll.
**Test:** cross-wallet transfer not income, no double-counted exposure, unresolved owner not pooled as fact, realized rebates not assumed eligibility, leftover unsellable positions retained. Exclude unverifiable headline screenshots as economic ground truth.
**Rust bridge:** multi-account reconciliation and separately typed income ledger. No wallet rotation for evasion or deceptive appearance.

### G10 — Microstructure race and reliable intent delivery
**Gap:** same signal and eventual landed timestamp can hide different decision-to-submit delays, fees, route selection and failed attempts. Pros claiming speed does not validate a transferable latency advantage.
**Collect:** source-arrival/decision-start/end/simulation/submit/ack/land/finality timestamps, clock uncertainty, route/RPC, priority settings, blockhash lifetime, retries and signatures, timeout versus confirmed failure, rejection reason and quote expiry; access level of data (historical block data is not advance visibility).
**Tasks:** abandon stale opportunity, resolve unknown transaction status before retry, choose feasible route under cost/latency cap, continue position safety while model is busy.
**Test:** duplicate submissions, delayed confirmations, reorg/finality changes where applicable, stale blockhash, slot/clock ambiguity and budget exhaustion. Executable sequence uses what our infrastructure sees, not block history revealed later.
**Rust bridge:** submission/reconciliation telemetry, retry deduplication, end-to-end deadline tests. Protected orders remain deterministic and independent of LLM availability.

### G11 — Venue incentive regimes and deployment-versus-trading scope
**Gap:** fee/cashback/creator incentives alter who supplies tokens and sells. S1/S2/S5 contain dated discussion, not reliable current fee schedules.
**Collect:** venue/program/config version effective at t, empirically verified fee/rebate recipient eligibility, ownership/mint/freeze authorities, fee claims, deployer inventory/distributions, migration support, bot/launch modes only when verifiable on-chain, emissions/buybacks and incentive changes. Similar mode names across dates are not interchangeable.
**Tasks:** judge incentives/overhang and risk-return of available trades without pretending creator revenue is trading alpha. Model a creator's incentive as a hypothesis conditional on verified economics.
**Test:** changed fee recipient, ineligible cashback, repeated new launches in same theme, migration-route outage and constant narrative/different fee regime. Never propagate an interview's 'zero risk' language as truth.
**Scope:** Rust engine development is NOT memecoin issuance. Studying deployer behavior does not authorize launching tokens, fake activity or extracting value from deceptive promotion. New chains are contextual observations until separately supported/approved execution mechanics exist.
**Rust bridge:** versioned fee adapters and protocol-condition fixtures.

### G12 — Session allocation, losses, restraint and sustainable edge
**Gap:** selected clips omit losing sessions, idle periods, repeated failed re-entries, attention drain and crowding. Human tiredness/tilt must not be diagnosed from a wallet.
**Collect:** full consenting sessions, capital-time, all eligible opportunities, losses/no-trades/failures, session duration, budgeted compute, queue load, observable strategy changes, daily concentration, catalyst count, turnover costs, planned cooldown/review rules and human self-reports separately typed.
**Tasks:** reduce activity in weak opportunity sets, reserve capital/attention for better candidates, change thesis after evidence, avoid revenge-trade loops; debrief good-process losses and bad-process wins without rewriting decision-time facts.
**Test:** selected-highlight versus whole-session evaluation, leave-largest-winners-out sensitivity, bad-regime abstention, sequential no-edge re-entry and inventory stranded across session boundaries. Do not use daily PnL alone as automatic size escalation or tuning signal.
**Lesson lifecycle:** distinguish signal, execution, sizing, missing-information and ordinary-variance outcomes using evidence available at decision time. Record review_available_at, applicability, counterexamples, approval, supersedes and retirement condition. A lesson may influence only later eligible contexts; neither weights nor live risk limits self-update.
**Rust bridge:** session resource budgeting and explicit risk governor/review telemetry.

## Corpus objects and production order
Add typed tables beside existing D01–D14; do not replace their canonical truth:
1. `expert_source_claim_v1`: identity candidate, exact source URL/locator, speaker certainty, verbatim-span pointer, live/retrospective status, authoring origin, rights gate, claim-versus-action-versus-economics type. Plans/hypotheses are never mislabeled human claims.
2. `discovery_attention_event_v1`: event queue, source/filter/search/visibility state, permissioned interaction evidence and causal time.
3. `narrative_competition_snapshot_v1`: catalyst, candidates and rivals known at t, semantic/visual evidence versions, attention/flow comparisons, unresolved identity reasons.
4. `thesis_revision_event_v1`: expected response/horizon, evidence diff, confidence event definition, invalidation and chosen action; future resolution stored separately.
5. `execution_parent_child_v1`: intent, reservations, quotes, signed submissions/status/fills, recursive inventory/reserve effect, final liquidation and exact cost components.
6. `influence_incentive_context_v1`: timestamped public attention/visibility, lagged cohort behavior, protocol incentives and nontrading income with uncertainty.
7. `session_opportunity_audit_v1`: complete opportunities/exposures/idle time/failed delivery, evaluation coverage and excluded-source reasons.
8. `rust_market_bridge_episode_v1`: source issue, pre-change code/log context, accepted patch and real compiler/test/benchmark results, linked feature/fee/thesis/execution contract IDs. Trade outcomes or final fixes never enter pre-fix input.

Dependency order: protect evaluation boundaries and rights -> identity/source-time and event adapters -> discovery + rival capture -> truth/accounting/execution linkage -> human adjudication and whole-session coverage -> unified task exports -> per-role AND combined economic/reliability certification. Prospective capture can collect eligible public events independently of expert interview access; expert-mind imitation cannot proceed by filling missing thoughts.

## Annotation, coverage and acceptance additions
- Each expert case asks: what could be seen at t; what alternatives/competitors existed; what claim was actually stated; what changed; what was ordered versus filled; what costs/inventory remained; which parts depended on unavailable advantages? Answers may be UNKNOWN. No reconstructing intention from later success.
- Dual codebooks: fact/evidence annotations versus evaluative human judgments. Cite spans for facts; track reviewer disagreement/qualification for judgments. Qualify source origin even for repository code and comments—human review does not launder AI-written material into human-origin training under the standing rule.
- Edited clips do not become full sessions. Require original live sequence, time alignment and actual account/transaction linkage for action-observation claims. Otherwise retrospective educational track only after rights approval. Obtain rights/consent before collecting private voice calls or screen interaction; public interview mentions do not authorize access to groups.
- Visual narrative identity is a real dependency: store image/video provenance and modality requirements, allow source-backed human descriptions, and mark visual tasks unsupported for a text-only recipe. Do not assume text tokenization trains visual perception. Multimodal input/training requires compatible checkpoint/toolchain and separate approval; no hidden OCR/model-generated descriptions in training under current rules.
- Keep G01–G12 coverage row counts, independent sessions, creators, regimes, task-specific temporal support, original/copy ratios, admitted useful tokens and unmatched/unknown rates. No fabricated quota or celebrity preference. Do not oversample one viral story as many independent wins.
- Pass fixtures: candidate appears late; alias collision; new rival displaces incumbent; original article disproves claim; stale information query; open position during search; audience-dependent edge removed; template fails next regime; owner transfer; ineligible fee rebate; duplicate child order; each clip changes reserves; partial unknown fills; deadline/finality change; observation-only new chain; no available human rationale.
- Numeric/point-in-time baseline, narrative-only, combined, rival-monitoring-off, fresh-memory-off, latency-matched and influence-blinded comparisons run on protected development/evaluation protocol. Do not tune against sealed outcomes. Assess incremental all-in economics and role-specific failures, not claim/style similarity to famous traders.
- Before declaring a gap closed, show real source rows -> canonical records -> admitted task examples -> independent acceptance result. A field in a schema or a passing synthetic software fixture does not demonstrate corpus coverage.

## Rust semantic-conformance release gate
Map decision fields to concrete Rust symbols, commit, toolchain, effective configuration, units, clock/rounding semantics and branch/override precedence. Differentially replay source-backed events through canonical reference and Rust feature -> intent -> order -> fill transitions in a no-keys sandbox. Timer-unit, partial-exit and config-override repairs require failing-before/passing-after evidence plus causal invariants and latency measurements. Software fixtures may be synthetic for testing only; they are not human training records. Passing trading and code tests separately is insufficient if the actual engine interprets model actions differently.

## Portable handback and measurable completion
The package includes `NORTH_STAR_GAP_COVERAGE_V5.json`: twelve gap rows with required data, tasks, tests, Rust bridge and deliberately null coverage/token counts. It is a requirements checklist, not a dataset. Windows must replace nulls only with measured provenance-backed values, preserve version history, and attach source/producer/admission/test evidence. `NORTH_STAR_RESEARCH_REGISTER_V5.json` records research URLs and retrieved-text hashes; it does not convey rights or ship transcript training data. Future captures may differ from these text hashes and require versioned evidence, not fabricated matches.

For every G-ID, specify schema fields/types/units/null reasons, source-to-field map, source rights, availability/ordering rules, task mask/target scope, conflict/dedup policy, split ancestry, producer and failure fixture IDs. Name each acceptance fixture and its expected result before implementation. A release matrix distinguishes IMPLEMENTED_AND_TESTED, DATA_PRESENT_UNADMITTED, ADMITTED_WITH_SUPPORT, MISSING_SOURCE and UNSUPPORTED; narrative/expert capability cannot be silently claimed through a lower-tier proxy. If a required capability lacks data, release other independently qualified tasks with that capability explicitly unsupported, or delay the full release—not fabricated completion.

## Receiver instructions
Read v5 before embedded v4/v3/v2. Preserve prior releases. Update source registry and coverage matrix with G01–G12 and the eight objects above, then implement only authorized stages with the existing bounded-memory/resume discipline. Explicitly report which named identities, sessions, trading actions and rights remain unverified. No invented historical verification, timestamps, psychology, fee schedules or profit targets. The goal remains a profitable, reality-grounded reasoning/execution/Rust model—not a celebrity simulator.

---
# Embedded unified v4 master — v5 above governs conflicts

# North Star v4 — One Model, Three Capabilities

## Governing operator clarification
North Star is ONE jointly capable model/checkpoint: (1) market reasoning over narratives/meta, on-chain flow, charts, mechanics and portfolio state; (2) executable trading decisions and authorized execution-tool use; and (3) Rust software development, debugging, testing, review and optimization for our trading system. Separate data tracks, evaluation suites and runtime permission profiles do NOT mean separate specialist models. This amendment takes precedence over inconsistent v3/older text in the attached full specification.

## Correct the earlier assistant claims
- Base-only initialization was an assistant recommendation, NOT an operator decision. No evidence establishes that continuing from the current CPT/SFT must fail or that a unified 27B necessarily outperforms specialists.
- Compare admissible pretrained original checkpoints, selected domain-CPT and completed current-SFT checkpoints using provenance, contamination, market-decision, execution-contract, Rust and general-capability evaluations. Any additional training comparisons require an approved budget/run plan, not an assumed cheap smoke stage. Select the starting checkpoint after data certification and evidence review; do not discard existing checkpoints.
- A pretrained Qwen base is not untrained/random weights. Record exact upstream model/revision and whether it is base or instruction-tuned before approving a seed.
- Current SFT already contains multiple task shards; cross-sectional policy supervision dominates but does not establish that the resulting model is purely deterministic or incapable of reasoning/code. Its maximum is step 119, not a guaranteed finish; existing early-stop, best-validation and stop-and-ask rules remain.

## One model, bounded execution
The model owns reasoning and action selection: wait/buy/hold/add/reduce/close, size, invalidation, order constraints and follow-up based on confirmed fills. It may call approved execution tools. Deterministic software still signs/submits transactions and enforces authorization, balances, inventory, idempotency, exposure and order expiry. These are execution machinery and safety checks, not a second decision model. No claim that an LLM becomes deterministic by producing a schema.

Use distinct permission profiles with the same weights: trading cannot edit/deploy its engine or change its risk limits; development works in a sandbox/worktree, without live keys/order access. Code changes pass independent review/tests and explicit deployment gates before serving trades. Protective order handling must not require an available LLM; failed exits remain possible and must be recorded. Development jobs cannot starve live inference; benchmark shared serving/concurrency and deadline behavior rather than assume resource isolation.

## Unified training-data changes
1. Preserve ALL retained Rust/on-chain/narrative source tracks subject to rights, quality, provenance and leakage admission. Build one coherent multi-task curriculum; track task IDs and useful post-mask supervision. Do not let bulk code or repetitive policy records crowd out narrative reasoning or execution judgment. Do not invent fixed mixture percentages before measured support and validation.
2. Include paired causal evidence -> concise grounded rationale -> executable action records. Include direct-action low-latency records, tool-call/result/next-action episodes, rejects, partial fills, stale observations, cancellations, no-trades and reconciliation. Do not pad every action with long explanations or fabricate private thoughts.
3. Rust is a DEVELOPER capability, not only code-quality classification: collect source-backed requirements/issues, pre-change repository context, dependency/toolchain locks, actual patches, failed tests/compiler diagnostics, debugging sequences, test additions, performance benchmarks and verified before/after results. Preserve failed approaches and bounded negative examples with their true origin. Missing development traces are coverage gaps, not permission to invent them.
4. Cross-domain bridge tasks use real evidence: explain the units/window/availability of a feature from its Rust implementation; diagnose a observed signal or execution mismatch from logs/code; repair accounting/serialization with tests; trace risk/latency changes through the engine; review whether a proposed feature leaks future data. Future patches/incident outcomes are targets, never pre-fix input. These bridges align reasoning with implementation but do not guarantee semantic understanding.
5. Maintain train/eval exclusions across issue/repair chains, duplicated code and temporal trading/narrative evidence. Independent metric gates cover trading economics, executable-action/tool correctness, Rust functionality/security/performance and general retention. A blended average cannot hide a failed capability.
6. Candidate promotion requires the SAME checkpoint to meet all role gates, plus switching/concurrency tests: social content must not authorize code execution; development instructions cannot place live trades; execution results cannot be invented. Evaluate reasoning/action consistency and changes after disconfirming evidence. Compare unified-task mixtures/seed candidates on development data only; sealed evaluation remains protected.

## Scope and handoff
This updates the data/model specification only. No current SFT edits, new training, live orders, code deployment, paid acquisition or reboot is authorized. Windows receiver should acknowledge v4 hashes and explicitly confirm ONE model/three capabilities and an UNDECIDED starting checkpoint. All v3 licensing, temporal, accounting, narrative, resource, approval and independent certification requirements remain unless this amendment explicitly corrects them.

---
# Embedded v3 specification — v4 amendment above governs conflicts

# North Star v3 — Windows Aggregation, Cleaning & Labeling Handoff

**Owner:** Alon. **Deliverable:** implementation-ready research/data specification, not a trained model or validated strategy. **Target:** repeatable executable net profitability from memecoin scalping, with narrative/meta judgment AND numerical microstructure. No claim of Jane Street affiliation or guaranteed Cupsey performance.

**Scope:** finish the next-data plan; Windows Hermes implements after current SFT is safely finished. Current Linux training, checkpoints and frozen datasets are not modified. This handoff supersedes inconsistent planning language in v2; the full v2 specification follows as an appendix in the master document. No new live trading, paid API spending, source admission, training or reboot is authorized by this document.

## 1. Windows receiver: start here

1. Confirm with Alon that the current Linux SFT is complete, its best checkpoint selected and output safely preserved. If Windows is the other OS on the SAME server, do not reboot into Windows while SFT runs. An independent Windows machine can plan/read separately without disrupting training; resource-intensive work needs its own budget.
2. Obtain this master Markdown through Telegram/file transfer. Linux `/home/alon/` and `/training/` do not automatically exist on Windows. Do not claim handoff delivered merely because a file is local; acknowledge received filename and SHA256.
3. Verify the actual Windows repo, drives, free RAM/disk, Python and git state. Expected repo is `D:/repos/mev_bot`, not an instruction to create or overwrite it. Inspected Linux-mounted snapshot HEAD was `ce04b63b817c3078d63587f77207b71546b81f6e`; Windows may legitimately differ. Record actual HEAD, working-tree changes and relevant file hashes before adapting code. Never reset the checkout to match this note.
4. Preserve frozen trees. Stage a new isolated worktree/branch through the repo's branching rules and a new output namespace, e.g. `tools/data-pipeline/output/north_star_v3/<run_id>/`. No overwriting v1/v1.1/v3 gold, old evaluation sets, training assets or historical certifications.
5. Load Windows-local skills for gold pipelines, narrative acquisition, branching and code review; reconcile them against this document. Historical `--sources all`, old cron IDs, known-wallet labels and past “CERTIFIED” flags are not new admission authority.
6. Implement the inventory and validators first; do not run all legacy builders. Commands in the appendix are proposed test targets, not existing working North Star commands. No fictitious `northstar build` CLI should be assumed.
7. Return a receiver acknowledgment: actual host/OS; writable repo/workspace; received plan hash; actual repo SHA; found/missing source paths; resource envelope; protected evaluation boundaries; blocked permissions; and next bounded task. Exclude secrets.

**Path translation:** `/mnt/data/repos/mev_bot/...` was inspected read-only on Linux and normally corresponds to `D:/repos/mev_bot/...` on the Windows Data volume. Verify volume and path, do not infer from drive letter alone. `/training/qwen27b-assets/...` is Linux ext4, NOT a Windows D: source path. Copy required small reports explicitly with hashes; never mount or modify training partitions as a convenience.

**Safe discovery example in Windows PowerShell, read-only:**
```powershell
$repo = 'D:/repos/mev_bot'
Get-Item $repo
 git -C $repo rev-parse HEAD
 git -C $repo status --short
Get-FileHash -Algorithm SHA256 'PATH_TO_RECEIVED_MASTER.md'
```
Replace the received filename placeholder and, if different, `$repo` with the verified actual checkout. Appendix `pytest tests/north_star/...` commands run from the isolated worktree's `tools/data-pipeline/` directory, after those proposed tests have been implemented. Do not paste Linux shell syntax into PowerShell; use native Python for portable pipeline stages and `pathlib` for paths. Review package locks before installing; create an isolated environment, never mutate a shared training environment. No secrets in command lines, manifests, examples or git.

## 2. Compact data-point inventory — the collection contract

Every record needs source IDs/hashes, schema and code versions, units, event/observed/available timestamps, timing uncertainty, rights, coverage and split lineage. `null` means unknown/not available and needs a reason; it is not zero/false. RAW existence is not training eligibility.

### D01 — Source and observation truth [P0]
- Source/provider/account, canonical URL or object ID, raw hash, decoder version, license identifier/version/evidence and permitted uses.
- Capture start/end, received timestamp, timestamp provenance/precision, feed cursor, sequence/slot gaps, outage/error/rate-limit intervals, retention policy and revision/tombstone history.
- Source status: discovered / local-unverified / rights-pending / raw-eligible / task-admissible / quarantined. Record bytes, rows and distinct identities only after counting.
- Purpose: distinguish absent opportunity from blind collector; defend every downstream number.

### D02 — Token, creator and venue identity [P0]
- Exact chain/mint, token program, decimals, supply semantics, mint/freeze authorities and Token-2022 extensions as of observation.
- Creation transaction/slot/time, external relative-clock origin and offset proof, creator wallet attribution/evidence, name/symbol/description/URL versions.
- Pool/curve addresses, venue/program/version, migration events and continuity, actual reserves/liquidity changes, route availability.
- No ticker-only joins, assumed celebrity addresses, or launch-time offsets without validation. Chain-mint identity and wallet-owner attribution are separate.

### D03 — Raw trades, transfers and execution [P0]
- Signature, transaction/instruction/event index, commitment/finality, reorg correction, signer/payer, token accounts and economic owner where known.
- Side, integer token/native deltas, actual fill quantities/prices, successful/failed status, route and slot/state used.
- Separately reconciled swap/platform fees, base/priority fee, tip, transfer fee, rent lock/refund, rebate and failed-attempt cost.
- Intent → quote → submit → landed → confirm → balance reconciliation timestamps; reason codes for rejection, expiry and duplicate intent.
- Never double-charge impact already embedded in actual fills; never treat transfers as trades or intended orders as filled inventory.

### D04 — Tradability and execution capacity [P0]
- Size-specific quotes and min-receive, reserves and AMM/curve model version, available routes, transfer restrictions, exit capacity after migration.
- State/quote age, confirmation/failure/expiry distributions, congestion and fee/tip regime, our measured decision and transport latency.
- Mark observed vs reconstructed vs modeled execution. A volume proxy is not a depth measurement.
- Training test: apparent profit that disappears at our size/cost/latency must become abstention, reduced size or an explicitly unsupported opportunity.

### D05 — Chart structure and flow [P0]
- Partial/completed trailing candles, returns, ranges, drawdowns, volatility, realized turnover and trades/second, signed flow, distinct active buyers/sellers, repeat buying/selling and flow persistence.
- Pullback/reclaim, breakout/failed breakout, acceleration/deceleration, grind-then-crater precursors, price/flow divergence and migration discontinuities.
- Past-only OHLCV/VWAP and event paths with gap flags. No centered indicators, future-confirmed pivots or terminal normalization.
- Provisional feature windows: trailing 5/15/30/60 seconds and 5/15 minutes, plus since-launch for young tokens; preserve raw events. These are engineering starting points, not approved trading horizons. Emit insufficient-history flags, not padded histories. Choose/freeze final windows on development data only.

### D06 — Holders, wallet behavior and connected risk [P1]
- Actual account/supply snapshots where available; holder concentration, changes, new participants, retention of buyers, exits by earlier cohorts.
- Creator/deployer history, allocation/transfer patterns, suspected linked-wallet or bundle activity, funding links, sell pressure, insider/unrepeatable access flags.
- Distinct traders and trade-implied inventory stay explicitly named proxies when transfers/opening balances are missing. Cluster confidence is not identity proof; no claim that a public wallet identifies a natural person.
- Expert skill is prior-data net equity/risk/capacity INCLUDING open losers and locked capital; no ranking on the period being taught/tested.

### D07 — Narrative meaning and catalyst evidence [P0 for narrative-capable release]
- Verbatim source span, language/slang, literal proposition, referenced event, event time/status, original vs copied/remixed meme, cultural/context explanation by a qualified human where needed.
- Narrative/theme and parent-child relation; OG/derivative/competitor ties with evidence; actual catalyst vs rumor vs parody/satire vs sponsored claim vs debunk.
- Narrative specificity, novelty relative to what was known then, relevance to this chain/mint, plausible attention mechanism, opposing evidence and explicit uncertainty.
- Keep factual event truth, author's opinion/prediction, and economic outcome in separate fields. Narrative importance is not identical to bullish sentiment.

### D08 — Propagation, attention quality and crowding [P0 for narrative-capable release]
- Original post/version and earliest OBSERVED origin, repost/quote/reply edges, platform and community, message-level timestamps; unknown global origin remains unknown.
- Independent-author breadth, platform/community diversity, arrival and decay of NEW independent participants, reach/engagement snapshots as of t, bot/promotional indicators, account/website reuse.
- Actor roles: originator, early interpreter, tracker, amplifier, paid promoter, skeptical countervoice, ordinary participant; uncertain labels retained.
- Lead/lag between catalyst, messages, wallets, price and volume; does attention precede flow, follow price, or disagree? Association is not causal proof.
- Saturation, repeated recycled posts, coordinated amplification and copy-trade crowding. Keep spam/manipulation evidence for risk tasks when lawfully usable; exclude it as authoritative expert teaching. One post copied a hundred times is not a hundred independent endorsements.

### D09 — Market meta, competition and rotations [P0 for narrative-capable release]
- Which themes attract new independent attention and deployable capital; theme breadth, crowding, relative strength, participation persistence and liquidity/capital rotation between themes/chains.
- Opportunity density, launch activity, graduation observations, failed launches, broad memecoin flow and SOL market/congestion context, defined on a measured eligible universe.
- Theme stage candidates: emerging / broadening / crowded / weakening / revival / unresolved. These are as-of hypotheses with evidence, never retrospective certainty labels.
- Compare simultaneous candidates: leader vs derivative, stronger story but late entry, weaker story with feasible near-term flow, no suitable candidate, crowded existing portfolio.
- Cross-chain news may inform context; execution scope remains Solana Pump.fun/PumpSwap unless separately approved. Never equate “meta” with a fixed list of favored tickers.

### D10 — Account and opportunity state [P0]
- Candidate-generator version and eligible universe, available balance, committed/pending cash, current positions, cost basis, conservative liquidation equity, narrative correlations and risk limits.
- Available alternatives and selection reason; watch triggers, current thesis/invalidation, prior actions and fills, max loss/exposure, next observation and action expiry.
- Existing-position management and opportunity cost; do not train independent BUYs that all spend the same SOL.

### D11 — Sequential human decisions and revisions [P1; gates expert-rationale release]
- H1 contemporaneous human, H2 blind replay human, H3 retrospective review, A action-only, B deterministic/simulator: immutable origin tags, author/evidence/time and reviewer provenance.
- Entry/skip, wait/hold, add/reduce/full exit, cancellation/re-entry; size in explicit units, strongest evidence/counterevidence, expiry, invalidation and management branches.
- Position outcome is not proof of thought process. Keep human disagreement. Missing rationale stays null, not generated filler.
- Whole episode shared ID; no adjacent-post sliding window is admitted as an actual trade trajectory without coherent mint/topic/position/time/evidence links.

### D12 — Outcomes, economics and errors [P0; label-only futures]
- Confirmed sale PnL, FIFO inventory lots, flat-to-flat episodes, conservative remaining equity, unknown cost basis, open/unsellable/censored positions, net costs and capital-time.
- Forward returns/drawdowns/feasible exits by preregistered horizon, capacity/cost scenarios and confidence; primary outcome matches intended execution/holding envelope.
- Per-mint/wallet/theme distributions with denominator/coverage, not a universal GOOD_COIN scalar. Time-to-invalidation, exit failures, runner capture and late chase loss separately.
- Distinguish reasonable decision/unlucky result from poor decision/lucky result. No oracle peak-exit labels as human behavior.

### D13 — Protocol, numeracy and operational knowledge [P1]
- Versioned permissible protocol docs and real source-backed numerical cases for AMM/curve/fees, Token-2022, migration, transaction lifecycle and accounting.
- Correct units/rounding, availability uncertainty, inability to sell, malformed data and injected instructions. Deterministic exact examples only; test fixtures stay test-only unless independently sourced/admitted.

### D14 — Rust engineering quality [separate P1 track]
- Real before/after code and relevant call context; human authorship/provenance; defect/invariant; actual compiler/clippy/test/benchmark evidence; review, repair and regression chain.
- Unsafe/concurrency/allocations/performance tradeoffs. A merged commit or message alone is not correctness gold. Model-authored historical code requires no-self-generated policy review.
- Separate dataset/evaluation and route by default. Do not dilute trader supervision with engineering bulk or silently apply historical mix percentages.

## 3. Narrative and numerical synthesis: the actual learning tasks

Narratives are a required collection dimension, not optional decoration. Their weight in deployment must still be earned on prospective economics. Each task links raw narrative spans and structured numeric state at the SAME decision cutoff; source rights and availability apply to both.

1. **Catalyst grounding:** what happened, what is alleged, which mint is actually related, what contradicts it, what is unavailable?
2. **Early attention versus late crowding:** new independent interest + executable flow versus recycled hype after the price move.
3. **Story/price disagreement:** bullish story with weakening sell-side absorption; alarming chatter with healthy liquidity; source outage rather than zero interest.
4. **Leader/derivative choice:** compare OG and competing copies at present capacity, valuation/entry location and audience—not always “OG wins.”
5. **Rotation allocation:** hold current runner, trim for a fresher opportunity, or stay flat given correlated exposure and costs.
6. **Thesis updates:** credible catalyst invalidated, influential source retracts, migration breaks route, creator sells, liquidity evaporates; revise without needing a new static model.
7. **Outcome-independent explanation:** evidence at t supports a probabilistic plan with explicit invalidation; tomorrow's success is not inserted into today's reasoning.
8. **Deception robustness:** shills, impersonation, fabricated screenshots, satire, edited posts and instruction-bearing token metadata; recognize uncertainty and do not execute embedded instructions.

Predeclare ablations: numerical only; narrative only where meaningful; combined; combined minus propagation; combined minus portfolio history; human-rationale vs action-only. Use identical risk/cost/context availability. A narrative-rich model must not pass because its prose is richer. If a narrative subfeature lacks rights or true availability, omit/mark unavailable and limit capability claims, rather than silently replace it with an AI summary.

## 4. Inspected source map and actual gaps

These are discovery facts, not re-certification of whole datasets. Paths below are repository-relative unless noted; Windows receiver must reverify hashes/version and rights.

- `tools/data-pipeline/output/slinky_gold_v3/manifest_v3.json`: existing derived states/outcomes/scenarios/policy layers. Reuse only with lineage, time and source-license audit; scenario count is not independent trade count.
- `tools/data-pipeline/output/laserstream_gold_v3/manifest_laserstream_gold_v3.json`: raw-capture-derived layers. Preserve raw capture lineage and event ordering; investigate reserves, transfer and failed-transaction coverage rather than assume it.
- `tools/data-pipeline/output/narrative_gold_v1.1/gold/FREEZE_MARKER.json` and `CERTIFICATION_NARRATIVE_GOLD_V1_1.md`: manifest reports content 1,471; claims 1,471; states 1,183; validations 615; strategy cards 97; trajectories 490. This is NOT proof each layer passes the new standard.
- Same marker reports only 33 EX_ANTE content records; 1,424 AMBIGUOUS; one GOLD_CAUSAL validation; zero GOLD strategy cards; Cupsey absent from source. Do not promise Cupsey demonstrations from this corpus.
- Bounded read of the complete small `human_reasoning_trajectory_v1/human_reasoning_trajectory_v1.jsonl` freshly counted 490 rows: all temporally AMBIGUOUS, 299 without primary mint, 202 labeled GOLD/288 SILVER; 2,441 stage references reuse 761 distinct content IDs. Repetition count is not independent evidence. Sample previews contain rendered-link/date fragments, so semantic extraction needs repair/review.
- `tools/data-pipeline/src/build_narrative_gold_v1_1.py` inspected version ends with content-only `build_v11()` writing content. Its header mentions all layers, but does not establish the producer chain for all frozen layer files. Locate every producing script/commit/config or quarantine unreproducible layers; do not rerun this script into its hard-coded frozen destination.
- `tools/data-pipeline/schemas/narrative_gold_v1.py` provides useful legacy field candidates, not a binding new schema. `tools/data-pipeline/docs/narrative_gold_v1_design.md` is stale: it says gold builders not started although frozen outputs exist. “Resolved” celebrity seeds and old API/cron instructions need independent verification.
- `tools/data-pipeline/src/` contains `acquire_narrative_web_v1.py`, `acquire_youtube_transcripts.py`, `build_narrative_state_v1.py`, `build_creator_claim_v1.py`, `build_strategy_card_v1.py`: inspect adapters and builders, keep proven parsing only, do not assume generated prose or heuristic stages qualify as human supervision.
- `tools/data-pipeline/output/rust_gold_v1/manifest_rust_gold_v1.json`: existing engineering lineage to inspect; not new quality certification.
- `rust/data/full_trades.pkl`: file existence/size freshly verified on Linux (4,636,777,631 bytes); contents not deserialized. Windows candidate `D:/repos/mev_bot/rust/data/full_trades.pkl`. Previous row/schema statistics remain historical assertions. Receiver must verify canonical file/manifest, hash/trust origin and bound resource use. Do not unpickle unknown input; trusted pickle conversion belongs in an isolated, no-credential/offline process with memory budget, then Arrow/Parquet streaming. If missing, mark missing instead of fabricating round trips.

### Additional prerequisite findings: source evidence, not approval

- `rust/data/slinky21_data/` exists with `tokens.parquet`, `migrations.parquet`, snapshot/outcome/wallet tables and `trades/`; the latter has **18 Parquet shards**, rather than the monolithic `trades.parquet` named by README. Prefer bounded Parquet footer inspection over pickle when content lineage supports the task.
- `rust/data/slinky21_data/README.md` has **conflicting license declarations**: YAML says MIT, Licensing & Provenance says CC BY 4.0. Both are potentially usable permissive licenses under the policy, but the actual grant, revision and third-party holder-data rights must be resolved against publisher evidence. Do not choose the more convenient declaration.
- Same README claims uninterrupted history, whereas `KNOWN_ISSUES.md` documents a July 3 collection outage, SOL/price inconsistencies, missing price data, concentration/units defects and regime changes. Treat these as claims to validate, not an automatic correction script. Trace missingness into ledger/censoring and economic support; exclusion must not fabricate complete inventory or remove inconvenient losses.
- **Known-issues documents themselves need factual validation.** One calls a noncanonical address the System Program. Do not encode identity exclusions from prose alone: validate exact address, owner, executable flag, account type and role against appropriate source evidence. A program account, PDA, token account and beneficial owner are distinct; do not silently “correct” a source address or label.
- `tools/stream-capture-rs/grpc-server-only/training-data/` is the raw capture location to inventory. Receiver should check the manifest files `pumpfun_laserstream_manifest_v1_20260823_133256_000398.json` and `pumpfun_laserstream_manifest_v1_20260824_053543_000288.json`, then verify their listed files. This handoff does not claim raw hash verification. Normalized outputs are not substitutes for authoritative raw records when decoding is disputed.
- LaserStream v3 manifest freshly read: **29 migration coverage-gap mints and zero joined lifecycles**. Its path-invariance assumption is explicit; old fee/TP/SL/holding-time settings are simulator assumptions, not our approved North Star strategy or actual historical costs. Do not use unobserved post-migration exits as realized outcomes.
- Inventory the ENTIRE narrative raw directory, not just `raw_manifest_v1.json`; existing acquisition manifests may enumerate only an earlier subset. Record orphan files explicitly and establish their rights/producer lineage before use.
- Numerical and narrative futures must share protected evaluation exclusions, including the old gold layers already used by CPT/SFT. Previously trained-on data can support development but cannot be sold as fresh held-out evidence for a descended checkpoint.

**Admission consequence:** old GOLD/CERTIFIED means certified under an old scope. For North Star, track compatibility, source rights, temporal adequacy, episode coherence and authoring provenance independently. Preserve the originals; write new rejection/reclassification reports and clean derivatives only.

## 5. Acquisition design and source rights

### 5.1 Three lanes
- **Local revalidation:** bounded manifests/schema/sample inspection first, then streaming conversion/certification after SFT. No paid calls.
- **Prospective market/capture:** after resource and access approval, append actual opportunities, feed/account states and quote/fill/failure events. Keep every eligible event or deterministic recorded sampling; deliberately passing a trade needs a decision record.
- **Narrative/human acquisition:** approved permissively licensed sources and own/human-authored material with appropriate rights. Public access is not a license. No paid social APIs, SaaS credentials, login bypass or private-group collection absent a separate explicit directive. Reuse the local Firecrawl option only after Windows verifies service/version/port/access and terms.

### 5.2 Source registry decision table in prose
For every source, record `source_id`, owner/licensor, original/derivative status, exact permissive license and evidence hash, attribution obligations, train/transform/redistribution/collection rights, access method, quota/budget, historical availability, intended task, source/token report status and Alon approval. Unknown or restrictive licensing blocks training inclusion even if retrieval is technically possible. Approval for using a tool does not approve every fetched item. Own telemetry's embedded third-party content still needs review.

Public Telegram/X/YouTube/Twitch/web content: discovery leads until rights pass; platform display permission is not a training grant. Contracted humans must provide the approved permissive grant, not merely private training permission, under the present rule. If this limits coverage, present the gap/options; never relax the rule by relabeling a source as “facts,” “permissioned” or “public.” Lawfully retained nonadmitted evidence remains segregated and cannot leak through retrieval into training/evaluation contrary to the policy.

### Stage-specific authorization matrix

Record independent explicit fields with approval evidence/time/scope: `collection_access_authorized`, `local_transform_authorized`, `rights_admissible`, `technical_task_eligible`, `candidate_export_status`, `training_inclusion_approved`, `training_run_approved`, `live_orders_authorized`. An earlier gate cannot imply a later one. Unknown defaults to blocked for that stage. Read-only inventory and test-only parser fixtures do not need invented live capital approvals. Candidate exports are quarantined by default, use distinct paths/manifests, and training loaders must reject them until rights, exact source/license/token disclosure and explicit inclusion approval pass. A dataset approval never grants trading keys/orders or authority to reboot the training host.

### 5.3 Capture granularity and parser contracts
- Message, post, clip or article VERSION is the unit—not a whole channel preview page assigned one timestamp. Preserve native ID, parent/reply/repost links and exact source text spans. Strip navigation, dates and image URLs from semantic body without losing their raw provenance.
- Preserve original language, negation, uncertainty, numerical units, URLs/mints and slang. Translation/transcription provenance must be explicit; no generative paraphrase in training. Existing captions can contain errors; verify material claims against source where rights permit. If only an image/clip exists and no admissible verified text is available, mark missing modality—not a made-up transcript.
- Timestamp extraction precedence uses authenticated/native timestamps when available; inferred visible dates require timezone/year confidence. Retrieval time is never assigned as historical publication time. Reject implausible cross-message shared timestamps, ordering reversals and future edits.
- Exact hash dedup plus near-duplicate/echo graph; keep one canonical semantic item with all propagation edges. Separate duplicate deliveries from valid repeated actions. Deletion/revision tombstones preserve when corrections became knowable; honor legal deletion obligations through documented revocation/rebuild, not a universal “never delete raw” rule.
- Broad neutral collection: ordinary/failed themes, nonmentioned control mints, abandoned catalysts, quiet sessions, bearish/skeptical voices, multiple languages and small creators. Selection based on what was observable then, not future success or fame. Control matches must use predecision features; no causal-effect claim from observational matching alone.

## 6. Explicit schema and label contract

All new physical schemas use a new North Star namespace. Required types: nonnegative UTC-millisecond integer when known, native-unit integer amounts, bounded fraction with named denominator, categorical state with version, nullable values with reason, immutable identifiers and foreign keys. Prices derived with explicit quote/base decimals; monetary accounting must not use binary float. Observed commitment and later finality/correction travel separately.

Canonical tables, primary keys and join direction:
- `sources(source_id, version)` → `raw_objects(raw_id)` → `events(event_id, revision)`; immutable payload references and coverage intervals.
- `token_versions(chain, mint, available_at, version)`, `venue_states(venue_state_id)`, `content_versions(content_id, version)` and `entity_links(link_id)`; links have their OWN available time.
- `opportunities(opportunity_id)` + `portfolio_snapshots(portfolio_id)` → `decision_contexts(context_id)` containing allowlisted IDs and cutoff; no future fields.
- `annotations(annotation_id)` points to frozen context, author, provenance tier and reveal times; `intents(intent_id)` → `execution_events(execution_id)` → `inventory_lots(lot_id)` / `realizations(realization_id)`.
- `outcomes(outcome_id)` points to context/episode/horizon and cost model, observed end, censoring and label availability; outcome table cannot be joined into decision-context export.
- `narrative_states(narrative_state_id)` references content/version/entity/coverage dependencies plus as-of semantic hypotheses, not hindsight outcome classes.
- `episodes(episode_id)` references ordered decision/intention/fill states; `splits(group_id, split_version)` is inherited by EVERY derived object; `export_records(record_id)` traces source/label/model-visible bytes and mask.

**Multi-parent split conflicts:** freeze a versioned split-resolution policy before export. A context inherits all candidate, position-episode, content-version and dependency restrictions, not just the chosen mint's label. Any training example referencing protected economic evidence or a protected narrative version is quarantined/excluded; never relabel a protected parent to make it fit. Where strict entity-disjoint evaluation requires grouping, resolve connected dependency groups before partition assignment and disclose coverage loss. A giant component is a design limitation, not permission to silently weaken the split. Distinguish ordinary shared, timeless permissive reference documentation (explicit allowlist) from protected empirical episodes/labels. Chronological deployment and strict unseen-entity challenges have separate policies; materialized conflicts must be counted. Test a multi-candidate training context with one protected mint and a shared protected social item: both must fail admission.

**Target types:** observed action; human judgment; source-backed fact/span; deterministic economics; model-independent mechanics. Each names author/algorithm/version, dependencies, assumptions, confidence/uncertainty and applicability. No type is silently promoted to another. Build weak keyword/stage heuristics only as review suggestions, never expert gold. Claim outcome statuses require a falsifiable proposition and declared horizon; otherwise UNRESOLVED.

**Outcome horizon proposal:** preserve full observed episode and compute development-only candidate horizons of 5/15/30/60 seconds and 5/15 minutes where coverage permits; freeze final subset after the operating envelope is approved. This is not an entry/exit policy. Censor at true per-source boundaries; longer unavailable labels stay null. Future realized outcomes may supervise a prediction label but cannot become cited predecision evidence. For labels selected by hindsight optimization, name them oracle/simulator targets and exclude from human imitation.

**Semantic QA:** independent human label review sees blind contexts first; logs reveal order. Small calibration set spans profitable/unprofitable/no-trade, young/migrated, strong/weak narrative, conflict/outage. Measure disagreement by field; report adjudication and unresolved examples. Annotator qualification and workload are resource gates, not silently assigned to Alon. Deterministic action-only and mechanics tracks can proceed without waiting for human rationale capacity.

## 7. Build DAG, permission boundaries and resource controls

Reconcile the appendix work packages with this corrected order:

**Stage 0 — receive/inventory (read-only, no admission):** verify workspace and sources, current-run completion, license evidence and small metadata; produce receiver acknowledgment, source/coverage registry and gap report. Can finish without specifying live capital.

**Stage 1 — protect evaluation and contracts:** reserve protected time/entity/source boundaries BEFORE teacher selection, feature/cluster fitting, execution calibration or curation. Unknown time sources enter relative-only quarantine, not randomly blended chronological tests. Record planned forward untouched window and inherited-weight uncertainty. Final numeric risk/economic limits must be approved before strategy tuning or promotion; they do not block safe file inventory and parser unit tests.

**Stage 2 — bounded canonicalization:** write new schemas/tests; parse trusted raw to partitioned Parquet; event identity/finality corrections; source/rights and per-field availability; ledger balance reconciliation on small independently checked examples. Then scale ONLY successful transforms. No whole-frame all-data pickle loads or overnight blind runs.

**Stage 3 — parallel capture and context:** prospective opportunity/account/execution capture and approved narrative/message capture; deterministic numerical features and human/verified semantic annotations; entity and propagation graphs. Capture callbacks must be bounded/nonblocking; never add latency to a live executor without separate instrumentation review.

**Stage 4 — development-only execution calibration:** train/validate simulator on separate development slices, freeze error/cost/latency/capacity models and accepted operating regimes. Protected economic sessions cannot inform fitting, parameter search or residual-driven fixes.

**Stage 5 — episode reconstruction/labeling:** observed trades and human episodes; relative-only local accounting remains separate from globally aligned sequences. Natural-distribution pool plus recorded targeted challenge pool. Freeze teacher selection from earlier data, outcome/semantic definitions and sampling contributions.

**Stage 6 — split inheritance + exports:** materialize previously protected splits, purge horizon overlaps; CPT only if needed, SFT task exports; leave outcome reviews out of predecision inputs. No fixed old mix; exact post-mask label-token accounting, context-window/truncation report and all-stage/retrieval dedup.

**Stage 7 — independent whole-corpus certification:** schemas, IDs/foreign keys, rights, unit/time invariants, anti-joins, manifest reconciliation and every output file hash; independent semantic/ledger/narrative review. Publish admitted/rejected/quarantined totals and remaining capability limits. Source/license/token report and explicit operator inclusion approval precede training admission.

**Stage 8 — Windows handback:** manifest, evidence, unresolved decisions and training proposal. No new training until separately approved. No auto-extension, no live RL or automatic feedback ingestion. Final operational/economic tests remain appendix G0–G6.

### Resource and restart discipline
- Before each heavy stage record free disk, measured working-set estimate, safe worker limit and scratch/output/checkpoint coexistence. Preserve about 12% available-RAM headroom; no unchecked parallel full-file scans. Current SFT disk/offload reserve cannot be borrowed.
- Bound partitions, queues and retry budgets. Content-hash/partition checkpoints; write temp then atomic replace; mark a partition complete only after validation/hash; resume exact matching stage/config, refuse mixed versions. Disk/RAM/network quota trips pause cleanly, never delete source/checkpoints to make room.
- Track input rows = output accepted + rejected + quarantined + explicitly accounted duplicate/correction handling; many-to-one aggregate manifests record cardinality rather than falsely requiring equal row counts.
- Long Windows tasks must run under independent reviewed supervision, not Hermes gateway-owned terminal children. Verify owner, durable log, exit status and resumability. Gateway restart should not kill aggregation. No unattended retry loop on corrupt input or schema mismatch.
- Use native filesystem paths in code; Windows atomic replace and file locking must be tested. Python multiprocessing must use a guarded `if __name__ == "__main__":` entry point and spawn-safe workers; do not inherit Linux-fork memory assumptions. Test one worker first, record per-worker memory, then approve bounded concurrency. This is a data-engineering fixture test, not smoke model training. No destructive drive/partition/boot or service changes. Paid calls have a hard operator-approved budget; retry limits/backoff and coverage gaps are recorded.

## 8. Acceptance suite and task-level release states

Proposed additional tests beyond the appendix:
- `test_message_units.py`: channel-page fragmentation, duplicate dates/links, edit/delete versions, timestamp ambiguity and exact source spans.
- `test_entity_links.py`: same ticker/different mint, wrong chain, expired metadata and future-created link cannot affect past state.
- `test_narrative_causality.py`: add future post/edit/engagement/reliability; earlier context bytes unchanged.
- `test_propagation.py`: repost storms do not inflate independent breadth; incomplete coverage != no mentions; independent origins remain uncertain where appropriate.
- `test_episode_coherence.py`: unrelated same-creator posts, multiple mints, chronology conflicts or no actual position cannot become a GOLD trade episode by stage count.
- `test_semantic_labels.py`: negation/satire/catalyst rumor/debunk, evidence link integrity, unsupported auto-stage labels blocked; H1/H2/H3 isolation.
- `test_calibration_splits.py`: simulator, clusters, stress parameters, teacher ratings and retrieval indexes reject protected IDs; hashes pin frozen artifacts.
- `test_rights_revocation.py`: known non-permissive/unknown licenses blocked, source-specific restrictions inherited, revoked data traced through exports/indexes and release invalidation.
- `test_windows_spawn.py`: native-Windows small-shard single/multiple worker identity/count parity, import-safe initialization, bounded serialized inputs and interrupted-worker resume. No claim of portability without this actual receiver test.
- `test_split_parent_conflicts.py`: a training context referencing protected candidate, portfolio episode or content version fails; shared-reference exemptions require a frozen explicit allowlist.
- `test_authorization_stages.py`: collection permission cannot admit training, candidate exports are loader-blocked, dataset inclusion cannot launch training or orders.
- `test_stage_resume.py`: interrupted write, same input different config, disk-full/retry exhaustion, resume idempotence and no duplicate outputs.
- `test_context_budget.py`: critical account/exit/risk state retained, causally selected narrative spans, missing evidence explicitly flagged; no silent overlength loss or hindsight selection.
- `test_narrative_numeric_join.py`: coherent narrative + numeric cutoff, delayed feed, wrong-mint resolution, duplicated source, inherited split; all dependencies available at decision time.

Each dataset/task declares `READY`, `LIMITED`, `BLOCKED` or `QUARANTINED` with reason, evidence and allowed use. Structural PASS is separate from rights PASS, semantic PASS, temporal PASS and economic-support PASS. A numerical-only valid release may be LIMITED, but cannot be advertised as the full narrative/meta North Star. A dataset can be release-ready while training remains unauthorized.

## 9. Concrete Windows outputs / no-guess handback

Under the NEW output run root, produce:
1. `RECEIVER_ACK.md`, `BUILD_PLAN.md`, stage/config/git/version fingerprints.
2. `source_registry.jsonl`, `SOURCE_LICENSE_TOKEN_REPORT.md`, `coverage_intervals.parquet`, `CAPABILITY_GAPS.md`.
3. Canonical event, content-version, entity, portfolio, order/fill, inventory and causal-state tables with schemas/lineage.
4. Separate observed/estimated/counterfactual economics; episode and narrative task datasets with source/annotation provenance.
5. `EVAL_FREEZE.json`, protected split registry, overlap/contamination report including simulator/retrieval/initial-model ancestry.
6. `ANNOTATION_CALIBRATION.md`, source-span adjudications and unresolved queue; no invented reviewer outcomes.
7. `DATA_CARD.md`, `TASK_MIX_AND_TOKENS.json`, exact tokenizer/mask/template/config hashes, drop/coverage/support reports.
8. `TEST_RESULTS.json`, `SEMANTIC_AUDIT.md`, `ECONOMICS_RECONCILIATION.md`, `NARRATIVE_QA.md` with actual command outputs, not predicted success.
9. `MANIFEST.json` + SHA256 per release file; reproducibility instructions and failure-resume evidence.
10. `TRAINING_PROPOSAL.md`, `DECISIONS_REQUIRED.md`, and signed admission request. Current SFT outputs are baseline candidates only, not an automatically chosen North Star parent.

No mandatory token count or fixed persona quota. Set acquisition priorities by valid independent evidence and missing capability, not filling a word budget. Preserve raw history where lawful; do not inflate training with duplicated views. Cost, source access, human reviewers, risk limits, time windows and full-parameter training schedule remain explicit operator decisions if not independently supplied.

## 10. Stop conditions and final scope

STOP the affected stage on source-rights failure, unknown timestamp required by task, balance mismatch, critical contamination, broken producer chain, unsupported exit economics, disk/RAM reserve breach or failed deterministic tests. Continue independent safe inventory/tasks where possible, never silently downgrade a gate to green.

This round delivers a complete handoff SPECIFICATION and bounded source discovery. It does not claim all missing source permissions, historical timestamps, human judgments or execution observations now exist. Windows should implement the defined pipeline, measure those gaps and return the admission bundle; Alon then decides data inclusion and the next training run. The original SFT must finish independently, and taking this plan to Windows must not interrupt it.


---

# Appendix: Full North Star v2 Technical Specification

The v3 receiver instructions/build ordering above govern portability, source qualification and any conflicting earlier wording. The following is the preserved detailed design, not evidence of implemented code. Linux-only evidence paths are archival references; the receiver must use the provided repo mapping or request an explicit transfer. In particular, the old native-Linux workspace recommendation is superseded by the isolated verified Windows workspace in v3.

# North Star: Profitable Memecoin Scalping — Data & Validation Implementation Plan

> **For Hermes:** Use subagent-driven-development skill to implement this plan task-by-task. If that skill is unavailable, use isolated implementer and independent reviewer stages; never skip the review gate.

**Goal:** Develop a model-assisted memecoin scalping system that delivers repeatable, executable net profitability at our capital, latency, access and risk limits—not a model that merely sounds like Cupsey or predicts which tokens eventually pump.

**Architecture:** A point-in-time evidence pipeline supplies a trader-judgment model, which proposes selective entries, sizing and position-management plans. A deterministic risk/execution engine validates and executes those plans; a separate outcome ledger and sealed evaluation system measure what was actually achievable. Human-origin demonstrations teach judgment; observed market data grounds economics; fresh context is supplied at inference.

**Tech Stack:** Existing Python/Arrow/Parquet data pipeline, bounded-memory deterministic accounting, timestamped source archives, human review records, tokenizer-aware exports, and the existing Rust execution stack. Future full-parameter model training is a separate, explicitly approved stage.

**Owner:** Alon; Hermes coordinates engineering and audit. A qualified human trader owns adjudication of expert decision annotations.

**Binding admission rule:** External training data requires an approved **permissive license**, plus purpose-specific rights/provenance evidence. Restricted training permission, public availability or an API subscription alone does not qualify. Report source, exact license/version and token counts to Alon **before inclusion**. Reject unknown, noncommercial and otherwise non-permissive licenses. No Qwen/self-generated training examples, evaluation contamination or duplicate padding. This requirement governs every use of “permissioned” or “rights” below.

**Status:** Refined planning proposal. No new data admitted, no corpus rebuilt, no model trained, no live orders authorized. The current SFT run and its frozen assets are untouched.

**Supersession:** This document replaces the DESIGN recommendations in `/home/alon/north-star-trader-data-plan.md`, including its appended scrutiny audit. That original remains preserved. It does not supersede current-run training approval, operating limits or frozen evaluation artifacts.

**Critical user clarification:** “Cupsey-like” means **profitability through memecoin scalping**, not personality, vocabulary, celebrity imitation, or merely agreeing with a named wallet. No verified, comparable Cupsey performance benchmark was supplied in this review. We cannot honestly promise his profitability or claim to reproduce private information, execution advantages or thought processes.

---

## Executive decision brief

**What changes:** Make profitable decisions across a complete position lifecycle the learning target. Teach selection, timing, sizing, waiting, partial profit-taking, runner management and exit under our actual capital/latency/cost constraints. Retain chart reading, narrative/meta awareness, mechanics, third-party round trips, per-token outcomes and Rust-code quality, but separate each task's evidence and evaluation.

**Most valuable missing data:** decision-time candidate sets and account state; verified order/fill/failure histories; human decisions recorded before outcomes; complete losing/open/no-trade episodes; executable reserve/route history; point-in-time narrative versions and source gaps. More winner screenshots or hindsight prose will not fill those gaps.

**First build tranche:** operating/evaluation contract → source and rights inventory → small independently reconciled event/inventory ledger → prospective opportunity/decision capture → calibrated execution replay → frozen partitions → human episode annotation. Expand raw acquisition only after those dependencies are measured.

**What earns promotion:** positive all-in net results and an economically meaningful improvement over the strongest frozen baseline, with dependence-aware uncertainty, acceptable downside and viable execution. Follow offline evaluation with prospective shadow and separately approved limited live verification.

**What remains unknown:** admissible source coverage, human-review capacity, our approved capital/risk/latency limits, achievable execution-model accuracy and whether rationale/narrative supervision actually improves net results. These are gates, not reasons to invent labels or promise a return.

---

## 1. Principal verdict and corrections to last night's plan

The original correctly identified temporal integrity, real trader examples and live narrative context. It nevertheless overemphasized rationale prose and treated several incomplete proxies as ground truth. Those defects could yield convincing explanations without an executable edge.

1. **Profitability is the objective; reasoning prose is an instrument.** A rationale that sounds expert but produces inferior net results is not success. Keep compact evidence, uncertainty and invalidation; test whether rationale supervision improves actions rather than assume it does.
2. **Predictive token quality is not a scalar GOOD/BAD label.** Separate tradability now, asymmetric opportunity at our size, manipulation/authority risk, narrative durability and exit liquidity. A weak long-term coin can offer a feasible scalp; a strong narrative can be a terrible entry.
3. **Per-mint aggregate realized PnL is not coin-quality truth.** It depends on sampled wallets, observation boundaries, transfers, unknown inventory and when profits are realized. Keep it as a coverage-qualified distribution, alongside losses/open inventory—not a universal label.
4. **Closed winners are not an expert dataset.** Capture the eligible universe, passes, cancellations, failed orders, full position episodes and abandoned theses. Do not select teachers using profits from the period being taught or tested.
5. **Rationale provenance must be typed.** Contemporaneous human commentary, blind historical replay, retrospective explanation, observed action-only trades and bot outputs are different supervision classes. Human approval of model-written text does not make it human-origin.
6. **Observed fills and simulated execution must not be mixed.** Recorded fill prices already reflect the execution that occurred. Do not subtract an assumed extra slippage penalty from observed cash flows. Missing fees make estimated net results, not verified actual net results.
7. **Absolute launch time is not a universal first blocker.** Reliable relative ordering can support a relative-time inventory ledger and local price windows. Absolute verified event/availability times ARE required for cross-mint portfolio history, chronological splits, expert track records and narrative joins. Do not halt all useful work while backfilling every mint.
8. **Unique trading wallets are not true holders.** Transfers, initial allocations, accounts outside the tape, burns and missing history matter. Name incomplete estimates `observed_trading_wallet_count` and `trade_implied_inventory_concentration`; do not certify them as holder truth.
9. **AMM execution depth is not an order book.** Use venue-specific reserves, fee/transfer behavior, route and size-dependent executable quotes. Tape volume/imbalance is a separate signal, not a substitute for capacity.
10. **Gold market data is more than schema alignment.** Causal states, liquidity, paths, objective outcomes and execution scenarios can support the trader task after re-certification; do not discard them or reduce them to BUY/WATCH/SKIP labels.
11. **Freeze a new evaluation design before teacher selection and curation.** Building evaluation after outcome-selected exemplar collection creates leakage and selection bias.
12. **Training cannot fix unavailable information or slow execution.** Prove the complete decision path fits the opportunity lifetime. A correct signal received too late is a losing or unavailable trade.
13. **Public content is not automatically permissively licensed.** Rights to ingest, transform, train and redistribute need distinct evidence. Ownership of a local file does not establish rights to its underlying source.
14. **The original literature section is not a proof of this system's profitability.** Its universal dataset-absence claim and claims of proven named-trader cloning are not accepted as evidence. Use research as hypotheses for controlled tests, not launch authority.

## 2. Define success before choosing more data

### 2.1 Economic target

Primary target: **incremental executable net PnL against a frozen, credible non-LLM baseline, using equal starting capital and identical opportunity feeds, venue access, latency assumptions and risk budgets**.

Also report absolute net profitability against staying in cash, capital-time efficiency, exposure, capacity and operational costs. A gain over a losing baseline is insufficient. Normalize comparison to the same SOL capital; report USD effects separately using contemporaneous exchange rates so SOL beta is not mistaken for trading skill.

Maintain two reconciled views:
- **Realized ledger:** confirmed proceeds less allocated confirmed acquisition costs and all attributable costs; includes failed-attempt expenses.
- **Portfolio equity:** cash plus conservative executable liquidation value of inventory, net of liquidation costs; adjust explicitly for deposits/withdrawals. Never make a profitable-looking report by leaving losses open or valuing unsellable tokens at a last print.

Report RPC/data/inference infrastructure costs separately and also in an all-in profitability view. Attribute setup costs transparently rather than silently assuming zero. Cost categories must not overlap.

Risk gates include maximum drawdown, tail loss, per-position loss, correlated narrative exposure, capital locked in unsellable inventory, operational failure rates and turnover. Win rate and gross PnL are diagnostics, not objectives.

**Do not optimize against hindsight maximum price.** Feasible pathwise best exit is an upper-bound diagnostic, not a teacher's action or an attainable profit target.

### 2.2 Operating envelope to preregister

Before acquisition is scaled, specify:
- Chain/venue scope: start with **Solana Pump.fun and PumpSwap**, with explicit migration continuity. Other chains are context-only until adapters, economics and separate evaluation exist.
- Deployment capital, allowed order sizes, max concurrent positions and correlation limits.
- Holding-time and decision-frequency envelope, with a distinct residual-runner policy. Derive the final envelope from intended strategy and coverage, not an arbitrary universal five-minute horizon.
- Entry types, event triggers, cancellation/expiry rules and current-position management authority.
- End-to-end freshness/latency budget, including model inference, and maximum acceptable latency under congestion.
- Maximum tolerable drawdown and loss concentration, cost-stress grid, and statistical uncertainty tolerance.

These numeric limits require operator approval before model/economic tuning or promotion. Leave them unresolved rather than invent Cupsey-comparable numbers.

### 2.3 Capability scorecard

- **Selection:** identify tradable opportunities and correctly reject untradeable or negative-edge states.
- **Timing:** buy before deterioration without paying for every momentum burst; avoid chasing after the edge expires.
- **Sizing:** size to capital, capacity, uncertainty and correlated inventory—not fixed confidence-to-size mapping.
- **Position management:** hold, reduce, fully exit, invalidate, cancel and re-enter based on new information.
- **Profit capture:** distinguish transient momentum, durable runner continuation and grind-then-crater traps. Preserve fat-tail upside when evidence supports it; do not train every profitable position to exit at one universal TP.
- **Execution:** understand when an apparent edge disappears after fees, tips, impact, delays, transaction failure and exit feasibility.
- **Adaptation:** consume changing narratives and react to disconfirming evidence without overtrading.
- **Abstention:** stay flat when evidence or data quality is inadequate. Opportunity cost and no-trade periods count in evaluation.

## 3. Evidence baseline: what was actually inspected

Read-only review anchors:
- **A:** `/home/alon/north-star-trader-data-plan.md`, especially §§3–6 and §12. Original SHA256: `78d3de7b6c5c7b240fe5fee64c5799c508555a17751c5a21a3de5b49e4be7008`.
- **B:** `/home/alon/north-star-scrutiny-audit.md`. This is a historical audit assertion, not a fresh scan of every raw source.
- **C:** `/mnt/data/repos/mev_bot/tools/data-pipeline/src/sft_cross_exporter_v2.py:9–21,78–90`. Confirms compact live-action/ranking outputs and a curated causal field subset. Existing output style does not demonstrate economic profitability.
- **D:** `/training/qwen27b-assets/data/qwen_curriculum_v1/TRAINING_APPROVED_MANIFEST_V2.json`. Records current SFT domain corpus of 2,162 records, 9,009,762 raw/model-visible tokens and 605,884 loss-bearing label tokens; these are manifest-reported counts, not a whole-corpus recount in this review.
- **E:** `/mnt/data/repos/mev_bot/tools/data-pipeline/output/slinky_gold_v3/manifest_v3.json`. Reports 33,581,765 rows in each of states/outcomes/counterfactual/policy layers.
- **F:** `/mnt/data/repos/mev_bot/tools/data-pipeline/output/laserstream_gold_v3/manifest_laserstream_gold_v3.json`. Reports 3,790,870 states and 94,771,750 counterfactual scenario rows. These are not independent executed trades.
- **G:** `/mnt/data/repos/mev_bot/tools/data-pipeline/output/rust_gold_v1/manifest_rust_gold_v1.json`. Engineering corpus exists but its counts alone do not prove repair correctness.
- **H:** `/mnt/data/repos/mev_bot/docs/BUILD_SPEC_LIVE_EXECUTION.md:10–20,53–100,104–134`. Historical spec identifies confirmation/account-resolution and paper-versus-actual-PnL issues. Its historical incident rates are NOT current measurements; its abbreviated transactions and conclusions need independent verification before becoming labels.

Consequences:
- The original blanket “counterfactual_trade_v3 = 2,428 our-policy trades” is too ambiguous. Those may describe a derived subset, not entire gold layers. Inventory each actual file/layer and its lineage; never substitute subset counts for source capacity.
- Availability of `full_trades.pkl`, its exact schema, licenses, event coverage and prior semantic checks must be re-certified from a source manifest and bounded inspection. Do not load a large/untrusted pickle into this training host just to inventory it.
- Existing narrative rights, genuinely causal human decision coverage, bot input-snapshot completeness and named-trader attribution remain **unverified for the next dataset**.
- Existing certified corpora are immutable. A new task needs new compatibility/rights/causal checks, not silent edits to old certifications.

## 4. Dataset architecture: store truth separately from teaching examples

### 4.1 Immutable source and coverage ledger

Every source chunk records origin, acquisition method, content hash, exact rights evidence, event span, capture/ingestion span, venue/program version, completeness and failure intervals. Preserve raw bytes where rights allow. Quarantine uncertain rights, broken timestamps and unverifiable identities.

Distinguish:
- **event time:** when the underlying transaction/publication happened;
- **observed/received time:** when our collector first obtained it;
- **available time:** when the computed feature or document could actually be used;
- **decision time:** when the agent committed its decision;
- **submission/landing/confirmation times:** execution stages.

Time precision and ordering matter for scalping. Record slot, transaction identity/index and instruction index when available; millisecond-looking timestamps are not proof of millisecond accuracy. For unknown order within a bucket, do not manufacture a precise ordering: reject microstructure labels that depend on it, or report bounded scenarios.

Temporal capability levels are `relative_ordered`, `absolute_event_anchored`, and `decision_available`. Preserve `clock_domain`, `relative_origin_definition`, `anchor_source`, offset uncertainty and ordering confidence. A mint creation timestamp is not automatically the zero point used by an external tape; validate the offset against independent events before adding it to relative seconds.

A historical publication date retrieved today does not prove historical system availability. Mark a replay's availability as observed, reconstructed-with-evidence, or unknown; evaluate idealized-information and deployment-achievable tracks separately.

### 4.2 Observed fills, inventory and actual-cost ledger

Partition by chain/venue and wallet/mint, with stable event identity and provenance. Use one primary accounting convention—FIFO by default—and keep an independently tested average-cost view only as a sensitivity analysis. Do not switch conventions per example.

Handle partial sells, multiple buys, re-entries, transfers in/out, opening inventory, airdrops, rebates, account rent/recovery, Token-2022 transfer behavior, failed transactions, duplicate delivery and cross-venue migration. A missing-history sell is an unresolved-cost-basis record, not a zero-cost windfall. Transfer is not a trade; suspected same-owner wallets remain separate unless attribution is supported.

Outputs:
- fill ledger with confirmed token/native deltas and transaction status;
- inventory-lot ledger with remaining quantity and allocated costs;
- realized-sale ledger with realized partial PnL;
- complete flat-to-flat episodes where known;
- open inventory and observation-boundary/censoring ledger;
- estimated economics with explicit cost bounds where actual components are absent.

Actual cash-flow accounting and counterfactual execution live in different tables. Fees embedded in recorded amounts must be identified before adding fee components. Recoverable rent is not automatically permanent cost; failed attempts are not silently dropped. Keep values in explicit native integer units with token decimals; derived floats are presentation, not primary balance accounting. Pin conversion: 1 SOL = 1,000,000,000 lamports.

### 4.3 Decision opportunity and position-state ledger — new P0

A trade-only dataset cannot teach selection or restraint. At every eligible decision event record:
- the full candidate set known then, why candidates entered the universe and what was unavailable;
- current capital, spendable balance, locked/pending amounts and existing positions;
- cost basis, position size, age, unrealized executable value and previous commitments;
- selected action plus considered alternatives, including staying flat;
- watch trigger, re-evaluation condition, expiry, invalidation and maximum slippage;
- pending/rejected/cancelled/confirmed status, with actual balance reconciliation;
- contemporaneous market/narrative context and feature versions.

Deduplicate repeated feed events without deleting legitimate repeated decisions. Keep trajectory/session identities so nearby observations are not mistaken for independent evidence. A failed buy never creates sellable inventory merely because an intent existed. Absence of a wallet trade is not proof of a deliberate SKIP; no-trade labels require an observed opportunity/decision or clearly typed policy/replay provenance. Record candidate-generator version, inclusion probability where sampling is used, eligible universe and collector outages so changing coverage cannot masquerade as alpha.

### 4.4 Action vocabulary and units

Use distinct actions: `WAIT`, `WATCH`, `ENTER`, `ADD`, `REDUCE`, `EXIT_ALL`, `CANCEL`, `REPLACE`, `ABSTAIN_DATA_QUALITY` where supported. Include `HOLD` only for an existing position.

Explicit fields: `order_size_lamports`, `position_fraction_to_sell`, `max_total_exposure_lamports`, `min_receive_raw`, `valid_until_utc_ms`, and a versioned `risk_policy_id`. `confidence` must state the event and horizon it estimates. Never overload price/SOL units, profit multiples or “pct capital out.” Price thresholds specify quote/base units; fractions specify denominator.

Record entry/exit **intent** separately from actual fill. A quoted entry price is not a guaranteed execution. Intent may be feasible, rejected or expired; execution logic never invents a fill.

### 4.5 Counterfactual execution and sequential replay

Build versioned venue-specific feasibility models calibrated to our actual confirmations and fills. Include reserves, fee placement, minimum amounts, transfer fees, route, tip policy, priority fees, failure/expiry probabilities, congestion, state change during latency and exit routing after migration.

Replay evaluates alternative decisions at **our** size and latency. Historical tape is not invariant to our order: impose a measured small-impact/capacity envelope; outside it, mark unsupported or run stress bounds. Do not label the difference between two simulated actions as an identified causal effect without the necessary assumptions.

Costs must distinguish spread/quote movement, our price impact, adverse movement, fees and failure costs without double counting. AMM lack of prints alone does not prove selling is impossible; executable reserves/route/quote and coverage determine feasibility. Conversely, a last traded price alone does not prove an exit exists.

Require held-out actual-execution calibration with preregistered error tolerances for fill probability, effective price/cost, latency and exit feasibility, stratified by venue/size/congestion. Calibration reporting alone is not a pass: the defensible uncertainty envelope must not erase the required net uplift. If tail/failed-exit coverage is insufficient, restrict the supported regime or mark evaluation INCONCLUSIVE.

Emit baseline, stressed and uncertainty-bounded economics. Preserve continuous return, downside, drawdown path, capital-time and feasible-exit evidence; categorical action labels are auxiliary. A maximal future move is never copied into decision-time evidence.

### 4.6 Token and wallet profiles

Token profile is multi-axis: feasible liquidity/capacity, flow persistence, concentration and uncertainty, creator/authority risk, migration continuity, narrative quality/crowding and price-path behavior. Include outcomes by horizon with clear censoring; do not equate graduation with profitability or every collapse with a proven rug.

Wallet expertise profile is **as of t**, built from all information available before t: earlier resolved outcomes AND contemporaneous conservative liquidation value of open inventory, locked capital and unresolved cost-basis uncertainty. Exclude future resolutions, not currently open losers; incomplete coverage constrains or disqualifies expertise claims. Assess repeated net performance, drawdown, capital employed, concentration in one winner, breadth across regimes, cost/capacity sensitivity and outside-sample persistence. Flag insider allocations, deployer links, copy loops, promotions and extraordinary access as separate strategy classes. Do not teach informational privilege as universally replicable skill.

Identity evidence uses public source records, attribution confidence and uncertainty. No unverified addresses are labeled Cupsey. Study named traders only under verified provenance/rights. Rank anonymized strategies on achievable economics rather than fame.

## 5. The high-value learning unit: a whole scalping episode

Use `opportunity → initial plan → wait/entry → new information → hold/add/reduce/exit/cancel → execution → postmortem`, not isolated winning buys.

Store raw trajectory once. Export multiple causally valid decision views only where needed, with a shared episode ID and contribution cap; do not duplicate/pad examples to increase token totals.

### Required episode coverage

- High-quality setup that fails; poor setup that wins by luck.
- Similar pre-entry states with different later outcomes; uncertainty is a legitimate answer.
- Momentum continuation versus exhausted/chased breakout; pullback/reclaim versus liquidity collapse.
- Early trim versus a justified runner hold; full exit versus a failed protective exit.
- Grind-then-crater flow, bot-dominated apparent demand and distributed selling.
- Migration interruption, route changes and delayed/post-migration exits.
- No trade, insufficient capacity, stale context, fee-dominated edge and too-late observation.
- Failed buy, failed sell, partial execution, duplicate intent and balance disagreement.
- Capital competing between simultaneous candidates, correlated narratives and new opportunity versus existing exposure.
- Re-entry after exit, cancelled thesis, anti-revenge-trading cooldown and stop-for-the-session conditions.
- New narrative, derivative/copy token, conflicting sources, promotion versus independent attention, failed meta and attention rotation.

**Sampling:** maintain a natural-distribution chronological pool for evaluation plus a clearly marked targeted learning/challenge pool. Never claim a curated runner/rug-balanced pool represents real opportunity prevalence. Keep normal quiet periods, not just memorable volatility. Cap contribution by episode, mint, wallet cluster, narrative family and source.

### Human-origin supervision tiers

- **H1 — contemporaneous:** permissioned human rationale/decision recorded before action/outcome, linked to the exact evidence viewed. Highest confidence about actual decision process; still not proof the decision was correct.
- **H2 — blind replay:** qualified human sees a frozen pre-outcome state, not identity/outcome cues, and records decision/rationale before reveal. Valuable judgment supervision but not the original trader's thought process. Log prior familiarity and exclude contaminated judgments from blinded evaluation.
- **H3 — retrospective postmortem:** human reviews realized outcome. Teach diagnosis/revision only in a separately labeled task; never relabel as ex-ante rationale.
- **A — action-only observed:** verified trades without rationale. Useful for behavior/action and execution study; no invented intention.
- **B — deterministic bot/simulator:** useful baseline/outcome comparisons, not expert human demonstrations. Record origin clearly.
- **Q — quarantined:** unclear rights, timing, identity, provenance or fabricated/generated reasoning.

LLMs may assist audit/flagging, but their judgments are not economic truth and cannot create training rationales under the current rule. Generated summaries/paraphrases, including human-approved AI prose, stay out of the corpus. Deterministic numerical derivations and serialization are permitted only when their source facts, definitions and tests are preserved. Recheck retention provenance too; prior corpus approval is not blanket approval of an unrelated new model-generated source.

### Annotation rubric and workflow

Before outcome reveal, collect: thesis, evidence IDs, strongest opposing evidence, specific entry/skip reason, feasible size, invalidation, management branches, next observation, confidence event/horizon and data insufficiencies. Do not require long internal monologues.

Use two independent qualified reviewers on an initial calibration set and all materially disputed/high-risk cases. Track action, sizing and invalidation agreement separately; retain unresolved ambiguity rather than forcing consensus. Rubric grades distinguish **decision quality given information then** from **result quality afterward**.

Human capacity is a genuine project prerequisite: Alon supplies or approves qualified reviewers and rights. Hundreds of rationales are a pilot acquisition tranche, not proof enough signal exists for full-parameter 27B generalization. Gate on independent episodes, coverage and validation—not a magical record count.

## 6. Chart, narrative and mechanics data

### 6.1 Chart/order-flow representation

Text-first structured paths for this phase. Pin windows, sampling cadence, units, aggregation algorithm and feature availability. Include log/relative returns plus absolute scale/liquidity so shape is not confused with tradability; retain extrema and abrupt moves rather than smoothing away rug precursors.

A candle may include only data observed by the decision cutoff. Distinguish a completed candle from a partial candle. No future-confirmed pivots, centered averages, terminal-normalized charts or indicators fitted with future data. Record source gaps and quote staleness. Event-time representation is necessary where wall-time resolution is too coarse.

Vision is deferred until a separate value/latency/data-rights test demonstrates benefit. The current pipeline is text-oriented; the architecture's possible vision support is not proof our trained artifact can read charts. If images are later used, enforce visible right-edge cutoff and remove future overlays/labels.

### 6.2 Narrative/meta context

Capture source snippets and human annotations with publication, edit, first-seen and available times. Deduplicate originals/reposts; distinguish independent breadth from amplification; track incentives/promotions and attribution uncertainty. Missing source coverage is unknown, not zero mentions.

A current retrieval layer provides permitted, point-in-time evidence. Train reading and conflict resolution, not memorization of today's hot ticker. Link by verified chain/mint identity; a ticker match alone is insufficient. Historical creator reliability uses only earlier resolved claims. Cohorts/clusters, vocabularies and regime detector thresholds must be fit on prior data; a retrospectively named regime is metadata, not a causal feature.

Log each retrieval query, document/version IDs, scores, timestamp filters and digest provenance. Retrieval can leak through indexes, future popularity rankings, updated biographies or hindsight summaries even if a document's original publication date is old. At time t, historical episodes enter memory only after their outcome becomes available.

Untrusted social/token text cannot issue tool commands or override risk controls. Train/evaluate refusal to infer unsupported claims, sponsored certainty and missing-source recovery. Schema-first evidence snippets are preferable to an unlogged generated digest. No AI digest is silently promoted into human-origin training data.

### 6.3 Mechanism grounding

Versioned, permissioned material on bonding curves/AMMs, pool migration, token programs/decimals/authorities/extensions, fees, transaction status, account constraints and risk. Numeric worked cases derive deterministically from validated mechanics. Protocol revision date and source evidence travel with examples; current documentation is not necessarily historical transaction semantics.

Distinguish market manipulation detection/risk avoidance from participating in manipulation. No teacher preference for strategies whose returns depend on deceptive promotions, wash trading or unrepeatable privileged allocation.

## 7. Point-in-time contract and leakage defenses

For every live feature/document:
`available_at <= decision_at`, with declared clock precision and latency model. For a derived feature, availability cannot precede the latest dependency plus compute delay. Missing or unverifiable timestamps fail closed for the task that requires them.

Keep four separate objects:
1. `decision_context` — allowlisted, causally available information;
2. `decision_target` — action/rationale tier with evidence references and authoring time;
3. `execution_observation` — actual submitted/landed/rejected result;
4. `outcome_review` — future returns, postmortems, labels and censoring.

Output-label timestamps may be later; those labels must never leak back into evidence or an alleged pre-outcome human rationale. Both input and rationale text need inspection. Denylist words alone do not prove no leakage; lineage, time joins, allowed-field contracts and adversarial perturbations are required.

**Mandatory mutation tests:** change future returns, terminal market cap, later social popularity, later holder count, later expert win rate and outcome grade; decision input bytes must remain unchanged. A future-only event inserted into source data must not change any earlier causal feature, retrieved context or historical teacher ranking.

Protect labels that expose outcome by construction: hindsight entry minima/exit maxima, post-hoc “winner” identifiers, future-confirmed chart pivots, token descriptions edited after the event and last-row aggregation. Exclude arbitrary UUID/filename/order cues that encode class.

Censoring is per mint/venue/horizon/source coverage, not inferred from the global latest timestamp. Observed no-print, unavailable coverage and impossible execution are separate states. Closed-only reporting must not erase open losers.

## 8. Evaluation before export: prove economics rather than fluent explanations

### 8.1 Splits and contamination registry

Freeze a new `north_star_eval_v1` independently of the existing `qwen_eval_v1.2`; preserve the latter untouched. Register source boundaries, hashes and entity/episode exclusions **before** building training exports.

Primary economic test: contiguous future market sessions, with natural candidate prevalence and portfolio constraints. Purge training labels whose outcome horizons overlap the test start; define embargo from maximum label horizon, episode duration and feature/retrieval dependencies. Do not pretend all data can be used if entity-group exclusion crosses the cutoff.

Secondary challenge tests: held-out narratives/regimes, unfamiliar wallet/deployer clusters where attribution is reliable, new mints, venue transitions, source outages and low-liquidity stress. Keep strict unseen-entity and ordinary chronological deployment tests distinct; describe remaining overlap rather than claim impossible universal disjointness.

Track contamination through raw content, paraphrases/duplicates, entire trajectories, creator clips, token families, repair chains, retrieval indexes and all CPT/SFT/retention stages. A pretrained model may already know famous historical outcomes. Identity masking is a diagnostic, not a cure; genuinely forward-collected sessions after model/data freeze are the strongest practical final check. Known-cutoff uncertainty remains disclosed.

### 8.2 Baselines and ablations

Freeze and replay under identical execution/risk assumptions:
- no-trade/cash;
- current deterministic strategy;
- a simple predeclared causal statistical/rule model;
- current policy model once available, as a comparison—not forced initialization;
- base/CPT model with the same tools/context, where feasible after current training ends;
- proposed North Star model.

Ablate human rationale supervision, narrative inputs, wallet attribution, execution-aware context, position history and retrieved past episodes. Compare names masked versus visible. If rationale or social data does not help economic decisions, reduce its role rather than rewarding eloquence. A cheaper model/rule system that performs as well wins operationally.

### 8.3 Economic evaluation protocol

Replay full sequential portfolios, not independent overlapping candidate states as if all could be traded. Model missed/cancelled/failed entries, own-impact restrictions and scarce capital. Same cash cannot fund simultaneous incompatible counterfactuals.

Report net PnL, conservative terminal equity, return on deployable capital, drawdown/tail loss, exposure/capital-time, turnover, cost share, order/fill/failure/exit rates, and PnL concentration by mint/day/narrative. Report matched trade count and effective independent sessions. Preserve genuine tail wins; also run leave-largest-winners-out sensitivity to reveal dependence without rewriting training truth.

Use paired session/day-level comparisons and dependence-aware confidence intervals, not IID per-fill confidence intervals. Pre-register the uncertainty level, minimum economically meaningful uplift, power/sample sufficiency criterion and stopping rule. A default proposal is a 95% lower confidence bound above the preregistered minimum economically meaningful uplift margin for incremental results, and above zero for absolute all-in net profitability, subject to operator-approved risk limits. The margin must be approved before evaluation; zero alone is not an adequate economic hurdle for complexity and operational risk. This is not a guarantee of future returns or a universal sufficient sample size.

Run cost/latency/capacity stress using empirically calibrated distributions and approved adverse scenarios. Do not cherry-pick the fee surface that makes the model look best. Test regime-specific deterioration and expanding order size. Performance claims are limited to the capacity envelope tested.

### 8.4 Separate judgment and reliability evaluation

Blinded human scoring of decision evidence/invalidation is secondary to economics. Evaluate calibration for explicitly defined events/horizons; abstention quality, missing-data behavior, citation correctness, preference stability and response to disconfirming updates. Test sensible size reductions under worse liquidity or higher costs without imposing simplistic monotonicity on all profitable opportunities.

Reliability suite: no fabricated fills, no spend above confirmed available balance, no sell above confirmed token inventory, no expired action execution, no unauthorized risk override, no future evidence and no social-text command execution. Report raw model violations separately from those blocked by the deterministic wrapper; zero observed violations is not proof of zero risk, so publish support and an appropriate uncertainty bound. Structured-output/schema validity is necessary, never sufficient.

Evaluate policy decisions on the portfolio created by its own previous actions, not the demonstrator's inventory after the two policies diverge. Outside observed-action support, mark the required transition as simulator-dependent/unsupported; do not silently reset to the expert trajectory. Off-policy estimates require justified behavior-policy/support assumptions and cannot rescue arbitrary action branches.

### 8.5 Promotion gates

- **G0 — operating contract:** scope, capital/risk/latency envelope, rights policy and scoring preregistered.
- **G1 — source admissibility:** every admitted source has rights and provenance; unresolved sources quarantined; source/coverage inventory reconciled.
- **G2 — truth integrity:** ledger conservation and fees verified, causal contracts pass, no forbidden train/eval overlap, exact disk/manifest count parity. No unresolved critical defect in admitted task slices.
- **G3 — learning sufficiency:** independent episode and regime coverage demonstrated, human annotations calibrated, no excessive single-source/mint/teacher domination, label-token distribution and truncation audited. Sparse capabilities labeled NOT_READY.
- **G4 — offline value:** candidate beats the strongest preregistered credible baseline economically within approved uncertainty/risk bounds; passes stress, safety and calibration gates. Low validation CE alone cannot pass this gate.
- **G5 — prospective shadow:** frozen model on newly arriving market data with actual system latency, evidence capture and executable quotes; human decisions recorded before outcomes. Freeze the complete performance-affecting system: weights, prompts, candidate generation, feature code, retrieval/ranking, execution settings and discretionary risk policy. Only preregistered causal state updates are permitted; material discretionary changes invalidate the assessment window and require a new version/window. Shadow must pass preregistered absolute/incremental net economic, risk, execution-model tolerance and evidence-sufficiency criteria under measured latency before G6 review. Sparse or conflicting evidence is INCONCLUSIVE, not a pass. Shadow results remain simulated, not landed PnL.
- **G6 — tightly limited live validation:** only after separate operator GO; verify real confirmations, costs and balance conservation. Risk limits and rollback outside the LLM. Offline/shadow success alone does not authorize capital. A reconciled but losing live trial fails the economic gate. Require operator-approved landed all-in net profitability, risk and evidence-sufficiency criteria before declaring deployed profitability or increasing capital; any increase needs separate authorization and capacity evidence. Predeclare live loss/execution-error stop and rollback triggers, maximum trial capital and trial-completion criteria. Insufficient evidence stays INCONCLUSIVE; risk breaches stop the trial regardless of statistical significance.

If sample sufficiency or evidence fails: INCONCLUSIVE/NOT_READY, collect more *independent valid evidence*. Never recycle the test set into a new pass while continuing to call it a holdout. Maintain an experiment registry and limit repeated peeking/selection.

## 9. Model training strategy and budget discipline

Keep trader judgment separate from the current policy-schema run. Candidate initializations are the verified original model and provenance-verified CPT export; the current policy-SFT model is a baseline, not the presumed best parent. A new CPT stage is optional and must demonstrate a specific knowledge/domain gap; it is not automatically required.

Training paths, in order:
1. Freeze task definitions, data/rights/eval manifests and inference contract.
2. Export human-origin judgment/action supervision plus tested numerical/mechanics tasks. Use explicit provenance tiers; do not conceal simulator targets as expert preferences.
3. Train only after source quality/coverage and operator-approved full-parameter feasibility/schedule gates. No smoke-training stage; bounded unit tests and data audits are not model training.
4. Select checkpoints using a preregistered development/validation rule with anti-overfit/stop-and-ask controls; do not select on sealed economic test results. Evaluate economics separately from loss.
5. Preference optimization/RL is a later research option, not a promised shortcut. Needs an approved preference/reward source, high-fidelity simulator, support constraints and reward-hacking audits. No generated training text or rolling live self-training under current rules.

**Composition:** do not inherit the old cross70/post10/rust10/narrative5/hermes1 mix automatically. Allocate by validated capability needs and **post-mask loss-bearing label tokens**, not raw token volume. Keep expert episodes prominent without duplicating them; use contribution caps and bounded task-only weights. Retention share and any engineering mix require explicit next-run review; preserve the existing current-run 4% retention rule untouched. Never weight by realized PnL magnitude, moonshot status or hindsight utility.

Publish per-source/task/tier: unique records, independent episodes/mints/teachers/regimes, raw tokens, visible tokens, label tokens, weighted share, truncation/drop reasons and train/dev/test counts. Fit token budget by useful evidence; do not pad to a requested corpus size.

Audit exact tokenizer, masking, weighted global normalization and distributed behavior. Overlength samples must be reported and segmented causally, not silently dropped. Small high-quality datasets can still overfit full-parameter 27B; insufficient diverse supervision means collect or defer, not automatic extra epochs.

## 10. Runtime required to make a scalping brain useful

Proposed fast/slow separation:
- **Fast path:** deterministic feed/state maintenance, quote/risk checks, trigger execution, balance reconciliation and emergency exit protection.
- **Judgment path:** model selects/updates time-limited entry and management plans on relevant events. It proposes structured actions, supporting evidence and invalidation; executor enforces bounds.
- **Context path:** point-in-time narrative/market evidence retrieval with freshness checks and source health.

This is a hypothesis to validate, not an already built architecture. Measure complete sensor-to-decision-to-landing latency at p50/p95/p99 under realistic concurrency. Use a smaller decision mechanism or narrower invocation scope if the 27B cannot meet the approved envelope; do not issue stale scalp orders because the model is expensive or slow. Profitability, not model size, decides placement.

A plan has an expiry, market-state version, maximum size/slippage and allowed branch actions. Protective exits cannot depend on LLM availability and are attempts, not guaranteed fills. On stale state, feed outage, inventory mismatch or risk trip, reject new exposure and use preapproved position safety behavior. No autonomous code edits, key access or risk-limit changes by the trader model. Log model version, prompt/context hashes, plan and rejection/execution outcome for every decision.

## 11. Preserve the additional requested capabilities without diluting the core

### Third-party round trips and per-token outcomes

Required supporting products, retained explicitly. Realized-sale, flat-to-flat, open-inventory and per-token distribution views must agree under declared coverage/cost assumptions. No “all wallets are profitable” conclusion from closed winners alone.

### Rust code quality for our repositories

Keep a separately measured engineering track. Acquire real before/after code, full relevant context, compiler/clippy/test output and qualified review of correctness, unsafe invariants, concurrency, hot-path allocation and performance tradeoffs. A commit message or “merged” status is not a correct-code label. Agent-authored diffs need provenance review under the no-self-generated rule; existing generated code is not silently admitted as human gold.

Split by chronological commit and repair-chain/group dependencies; exclude later repairs from earlier test prompts. No credentials or private operational secrets in corpus. Engineering validation is code/test evidence, not trading PnL. Prefer a separate engineering model/task route; include it in trader training only if ablation demonstrates no material economic/regression harm and the operator approves the mix.

### Hermes/repo-operating knowledge

Retrieve current operating docs with versioned permissions rather than memorize changing paths/secrets in trading weights. Keep workflow knowledge separate from action authority. Relevant mechanics/numeracy can remain common prerequisites.

## 12. Implementation work packages and exact proposed deliverables

This section defines future work, not code already implemented. Repository-relative paths below are proposed under `tools/data-pipeline/`. The inspected checkout is `/mnt/data/repos/mev_bot` and remains read-only. First obtain a verified writable workspace on native Linux; do not remount Data writable or write into frozen training trees.

Use bounded streaming reads and new versioned output directories. Do not execute heavy raw-data scans, model inference, tokenizer-wide exports or builds competing with the ongoing SFT. Schedule resource-intensive work after SFT or on separately budgeted capacity; preserve at least the user's approximately 12% available-RAM buffer and checkpoint disk reserve.

For every code-producing task: write the named failing fixtures first, implement the smallest deterministic function, run its targeted test, then obtain independent review before integration. Commands below are **future acceptance commands**, not executed results. A qualified principal data/ML engineer must review the exporters and certifier before admission.

### WP0 — Freeze operating and evaluation contract
- Create `docs/north_star_v2/OPERATING_CONTRACT.md`, `docs/north_star_v2/EVAL_PROTOCOL.md`, `configs/north_star_v2.yaml` after operator resolves numeric gates.
- Include all unresolved fields, baseline hashes, capital/latency/risk limits, primary endpoint, uncertainty procedure, evaluation schedule and prohibited adaptation. Assign protected source/session evaluation boundaries NOW, before WP4 execution calibration, residual analysis, stress-distribution fitting or simulator selection. WP5 materializes and verifies these already-protected partitions; it must not pick them after seeing calibration results.
- Test `tests/north_star/test_config_contract.py`: missing risk/fee/version/baseline/rights policy refuses execution; placeholders cannot pass GO.
- Acceptance: `pytest tests/north_star/test_config_contract.py -q`; operator-signed contract, not default-filled permissions.

### WP1 — Inventory rights, source coverage and dependencies
- Create `src/north_star/inventory.py`, `schemas/north_star/source.schema.json`, `docs/north_star_v2/SOURCE_ADMISSION.md`.
- Outputs under a new run: `source_registry.jsonl`, `coverage_intervals.parquet`, `admission_report.json`, `DEPENDENCY_MATRIX.md`.
- Test `tests/north_star/test_source_admission.py`: unknown AND known non-permissive licenses blocked; training-only permission cannot override permissive-only policy; source/license/token report and operator admission approval required; derivative rights preserved, unavailable source never counted as empty coverage, file/manifest mismatch detected.
- Acceptance: `pytest tests/north_star/test_source_admission.py -q`; every requested feature mapped to measured/raw/derived/proxy/missing and its usable task scope.

### WP2 — Time/event identity and deterministic inventory accounting
- Create `src/north_star/events.py`, `accounting.py`, `schemas/north_star/fill.schema.json`.
- Tests `test_event_order.py`, `test_accounting.py`: duplicate delivery; same-time ambiguous order; partial sells; re-entry; transfers; opening inventory; fees embedded vs separate; failed transactions; unresolved oversell; conservation in native units.
- Acceptance: `pytest tests/north_star/test_event_order.py tests/north_star/test_accounting.py -q`; hand-verified small transaction fixtures with independently reconciled balances. Only source-backed rows may enter corpus; test fixtures are marked test-only.
- Relative-only inventory can proceed where chronology is reliable. Absolute launch/event backfill is a parallel dependency; no fabricated epoch dates or forced complete coverage.

### WP3 — Point-in-time snapshots and narrative availability
- Create `src/north_star/causal_features.py`, `availability.py`, `narrative_context.py`, `schemas/north_star/context.schema.json`.
- Tests `test_causal_features.py`, `test_availability.py`: future-only mutation invariance; partial candle; stale state; postdated edits; derived compute delay; unknown timestamps; retrospective expert rankings.
- Acceptance: `pytest tests/north_star/test_causal_features.py tests/north_star/test_availability.py -q`; exact feature lineage and causal failure report.

### WP4 — Validate execution/economics before deriving action labels
- Create `src/north_star/execution.py`, `replay.py`, `configs/north_star_execution_v1.json`. Fit/calibrate and select all execution/stress models on development sources only, including a separate calibration-validation slice within development. Protected acceptance sessions cannot guide parameters, tolerances, residual-driven fixes or model selection. Register artifact hashes and calibration-source IDs in the contamination registry; freeze before sealed assessment.
- Tests `test_execution.py`, `test_portfolio_replay.py`: integer fee/reserve correctness, no double slippage, failed-exit costs, post-migration route, capacity violation, no phantom inventory, shared-capital allocation and unsupported path counterfactuals.
- Acceptance: `pytest tests/north_star/test_execution.py tests/north_star/test_portfolio_replay.py -q`; reconciliation report against actual fills plus explicit residual errors/coverage, not just passing simulator self-tests.

### WP5 — Freeze prospective/forward evaluation partitions
- Create `src/north_star/splits.py`, `schemas/north_star/split.schema.json`, `docs/north_star_v2/EVAL_FREEZE.md`.
- Tests `test_splits.py`: horizon overlap, trajectory/mint-group leaks, duplicate content, future teacher selection and CPT/retrieval contamination.
- Acceptance: `pytest tests/north_star/test_splits.py -q`; immutable partition hashes, quarantine accounting and independent overlap audit. Freeze before curation/teacher-selection sees test outcomes.

### WP6 — Human demonstration collection and adjudication
- Create `docs/north_star_v2/ANNOTATION_RUBRIC.md`, `schemas/north_star/annotation.schema.json`, `src/north_star/annotation_ingest.py`.
- Tests `test_annotation_ingest.py`: H2 blind reveal ordering, H3 cannot masquerade as H1, evidence IDs exist, rights are valid, generated rationale rejected, action-only keeps rationale null.
- Acceptance: `pytest tests/north_star/test_annotation_ingest.py -q`; reviewer calibration report, disagreements retained, independent episode coverage and review throughput measured.
- Acquisition order: permissioned own human decisions/blind replay, then verified third-party contemporaneous material where rights permit. Never let unavailable celebrity material block a broader skilled-trader collection.

### WP7 — Assemble episode tasks, code-quality track and exports
- Create `src/north_star/episodes.py`, `export_cpt.py`, `export_sft.py`, `code_quality.py`, `schemas/north_star/episode.schema.json`.
- Tests `test_episodes.py`, `test_exports.py`, `test_code_quality.py`: causal truncation, no lost management state, no padding duplicates, effective-token accounting, no grade-to-input leakage, repair-chain split and missing-code rejection.
- Acceptance: `pytest tests/north_star/test_episodes.py tests/north_star/test_exports.py tests/north_star/test_code_quality.py -q`; decoded examples for every task and exact whole-export distribution/drop report. No exports admitted before expert review.

### WP8 — Whole-corpus certification and release manifest
- Create `src/north_star/certify.py`, `manifest.py`, `docs/north_star_v2/RELEASE_CHECKLIST.md`.
- Tests `test_certify.py`: silent empty task, manifest/disk count mismatch, duplicate episode IDs, illegal split, invalid time/units, post-freeze changes and missing license evidence fail closed.
- Acceptance: `pytest tests/north_star/test_certify.py -q`; exact whole-corpus counts, anti-joins, hashes, temporal/rights/split/token gates, independent semantic spot audit and signed admission report. Sampling can audit semantics but cannot replace whole-corpus structural checks.

### WP9 — Training proposal, runtime validation and economic comparison
- Create `docs/north_star_v2/TRAINING_PROPOSAL.md`, `docs/north_star_v2/RUNTIME_CONTRACT.md`, `src/north_star/evaluate.py`.
- Tests `test_evaluation.py`: same-capital baselines, fees/failures included, open losers included, correlated block sampling, insufficient evidence → INCONCLUSIVE, sealed test not used for selection.
- Acceptance: `pytest tests/north_star/test_evaluation.py -q`; preregistered experiment and operator GO before training. After training: development selection → sealed economic test → prospective shadow → separately authorized limited live trial.

## 13. What is blocked, what can proceed, and what needs Alon

**Can proceed after implementation authorization without promising a model:** source/rights inventory, event schema, small deterministic ledger tests, operational capture design, annotation rubric, evaluation contract and relative-time accounting on qualified sources.

**Blocks ex-ante human-judgment/rationale training:** permissioned human evidence, qualified reviewers, decision-input snapshots and timestamp/availability proof. Missing reasoning is not repaired by Hermes writing convincing prose. **Action-only learning and mechanics/outcome tasks can proceed without rationales** once their own source, timing, economics and approval gates pass; human prose is not a universal prerequisite to extracting an edge from valid observations.

**Blocks claimed net profitability:** validated cash-flow/cost semantics, execution/exit capacity, full sequential portfolio evaluation, operating risk budget and independent forward observations.

**Blocks globally joined historical examples:** verified absolute event anchors, source coverage, point-in-time metadata/social evidence and sufficient ordering precision. Backfill a representative admissible subset first; do not force all raw mints into a single quality tier.

**Decisions for Alon before build scaling/training:**
1. Starting capital/order-size/risk/holding-time envelope and acceptable inference latency.
2. Human reviewer/annotation budget and permissioned source access; Cupsey-specific examples only if verifiable and authorized.
3. Approval of the economic evaluation protocol and uncertainty/promotion thresholds.
4. Approval of separate trader-versus-engineering routing and next-run model/data/training proposal.

Recommended defaults pending approval: Solana Pump.fun/PumpSwap first; structured text before vision; separate trader capability from current policy SFT; deterministic fast execution; no live RL; natural-distribution forward evaluation; no automatic changes to the current run.

## 14. Definition of done

### Research claim corrections checked against primary sources

- [Thought Cloning — NeurIPS 2023](https://www.shengranhu.com/ThoughtCloning/): the project explicitly says its experiments used synthetically generated thinking/action data. Large-scale human think-aloud training is a proposed direction, not demonstrated named-trader cloning. Its synthetic corpus is not automatically admissible here.
- [Trading-R1, §3.4 and abstract](https://arxiv.org/html/2509.11420v1): uses synthetic reverse-reasoning distillation and reports equities/ETF experiments. Its generated rationale pipeline is incompatible with our human-origin training rule; its results do not establish memecoin scalping transfer.
- [FinDPO, §5.1](https://arxiv.org/html/2507.18417v1): the reported 11% improvement is financial-sentiment weighted-F1 against FinGPT v3.3, not an 11% improvement in trader judgment or memecoin profitability. The paper also reports portfolio simulations; those are a different market/execution setting and not evidence that our intended system will earn comparable returns.

No ready-made corpus meeting ALL our rights, timing, provenance, judgment and scalping requirements was verified by this review. That is a bounded finding, not proof none exists.

### Review completion criteria

A refined plan is done when its objective, correction log, schemas/contracts, source dependencies, sampling/annotation rules, implementation deliverables and acceptance gates are explicit. **That does not mean a profitable model exists.**

A dataset is done only after permissioned source admission, verified causal/accounting semantics, representative independent examples, sealed evaluation separation, exact corpus certification and operator approval.

An offline/shadow-qualified candidate is distinct from a verified deployed trading capability. A deployed trading capability is done only when the frozen system demonstrates incremental and absolute landed all-in net profitability with acceptable risk and evidence sufficiency under an explicitly authorized live protocol, alongside the offline and prospective gates. No capital increase follows automatically; it needs operator approval and new capacity evidence. Cupsey-like profitability remains an ambition until a comparable verified benchmark and repeatable results establish it.
