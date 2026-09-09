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
