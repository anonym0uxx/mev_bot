# OpenClaw Pump.fun Principal Crypto Quant Bot — Master Spec

**PRIMARY MODEL:** anthropic/claude-opus-4-6
**WALLET MODE:** dedicated Phantom hot wallet only
**RUNTIME GOAL:** simple, stable, replayable, regime-aware, friction-aware, low-latency, risk-bounded
**NON-NEGOTIABLE:** preserve ALL finalized entry and exit decision layers exactly

---

## 0. BUILD PHILOSOPHY

Build a state-based crypto trading system, not a price-trigger bot.

The system must decide from:
- regime
- friction
- flow
- breadth
- structure
- manipulation risk
- expected value now vs expected value after waiting/holding

Do not optimize for:
- raw chart PnL
- blind copy trading
- all-regime trading
- architecture complexity

Do optimize for:
- net expectancy after all costs
- speed of action only when edge justifies it
- replayability
- fail-safe operation
- simple single-VPS operations first

---

## 1. RUNTIME STACK

Use exactly these components in first production build:

1. **OpenClaw gateway**
   - Anthropic only
   - primary model = anthropic/claude-opus-4-6
   - Node runtime, not Bun

2. **One custom OpenClaw plugin**
   - the only action surface
   - all trade side effects live here

3. **One local strategy daemon**
   - owns market intake
   - computes rolling features
   - computes candidate state
   - computes entry/exit EV layers
   - manages risk state

4. **One persistent datastore**
   - durable event/feature/order/config/replay store

---

## 1A. MODEL ORCHESTRATION / INFERENCE POLICY

Design principle:
- Anthropic is the only model provider in the initial build.
- Claude Opus 4.6 is the only LLM/AI model permitted anywhere the system invokes a model.
- There is no alternate-model path in the initial build.
- All daemon algorithms, rolling feature logic, regime classification, state updates, risk checks, and execution-prep algorithms must exist from the start as part of the system architecture.

LLM policy:
- whenever an LLM is used, it must use anthropic/claude-opus-4-6
- no Sonnet / Haiku / fallback-model routing in the initial build
- no multi-provider routing in the initial build
- no future model expansion should be assumed by default

Opus usage scope:
- top-candidate adjudication after ranking
- ambiguous edge cases where deeper reasoning is useful
- operator explanations and summaries
- daily learning analysis and challenger generation
- weekly strategic review
- bounded config-improvement proposals
- any other AI/model-based reasoning task in the system

Daemon algorithm scope (non-LLM system components, always present from day one):
- tier discovery / prioritization
- rolling feature updates
- regime classification
- provisional EV calculations
- manipulation shock checks
- catastrophic exit checks
- route-health checks
- quick_spend enforcement
- slippage / friction updates needed for immediate decisions
- feed handling, state transitions, persistence, replay, and execution routing

Thinking-control policy:
- Opus is the only model used when a model is invoked
- Opus use must still be task-class controlled with explicit latency/thinking policies
- the system must not allow unconstrained model invocation in the execution path
- if an Opus supervisory call times out or degrades, the live trading path must continue safely on daemon/state-machine logic or return NO_TRADE

Non-negotiable:
- no second-model path in the initial build
- no provider failover in the initial build
- no change that weakens deterministic risk handling or forced-exit handling
- no claim in the architecture that model choice guarantees profitability

---

## 2. WALLET / SECRET POLICY

Use a dedicated Phantom hot wallet for active trading only.

Rules:
- private key never appears in chat
- private key never appears in plugin output
- private key loaded only from VPS env secret or secret file
- bot only gets signing access through execution adapter
- keep minimal active capital in hot wallet
- most capital stays off hot wallet

---

## 3. DATA PLANE

### 3.1 PRIMARY LIVE SOURCE
Use one PumpPortal websocket connection only.

Use this connection for:
- subscribeNewToken
- subscribeMigration
- subscribeTokenTrade for watched mints
- subscribeAccountTrade only for qualified-wallet priors if enabled

Never open a new websocket per token.

### 3.2 ENRICHMENT / HISTORICAL / REPLAY SOURCE
Use Bitquery for:
- launches
- live/historical trades
- OHLCV
- bonding curve progress
- migration state
- dev holdings
- top holders
- top traders
- first 100 buyers
- whether first 100 buyers still hold

### 3.3 LATENCY TIERING
Design the data plane so it can be upgraded later without changing strategy logic.

- **Tier 1:** PumpPortal websocket only
- **Tier 2:** PumpPortal websocket + Bitquery gRPC / Kafka / faster infra for replay or selected feeds
- **Tier 3:** optional low-latency shred / gRPC feed integration if measured edge justifies it

Keep strategy logic independent of feed provider.

---

## 4. TRADEABLE UNIVERSE

Initial live trading universe:
- Pump.fun bonding-curve coins only
- pre-graduation only

Explicitly exclude in first production build:
- Mayhem Mode
- Tokenized-Agent coins
- post-migration continuation trades
- tokens with stale friction estimates
- tokens with missing manipulation estimates
- tokens with stale market data

---

## 5. REGIME CLASSIFIER

Every token must be classified continuously into:
- EXCLUDED
- EARLY_CURVE
- MID_CURVE
- LATE_CURVE
- GRADUATION_BOUNDARY
- POST_MIGRATION

Minimum classifier inputs:
- token age
- bonding curve progress
- migration status
- Mayhem flag
- Tokenized-Agent flag
- fee regime

All entry/exit logic must be regime-aware.
All costs must be fee-versioned by regime and config version.

---

## 6. FEATURE WINDOWS

Compute rolling features over:
- 1s
- 5s
- 15s
- 30s

No longer launch-trading windows in first build.

---

## 7. FEATURE FAMILIES

### 7.1 FLOW / MOMENTUM
- buy notional velocity
- trade-count velocity
- buy velocity acceleration
- curve-progress acceleration
- buy/sell imbalance
- average trade size
- size dispersion

### 7.2 BREADTH / TOPOLOGY
- unique buyers growth
- repeat-wallet ratio
- fresh-wallet ratio
- non-dev wallet participation
- first-100-buyer persistence
- top-10 concentration
- top-20 concentration
- wallet breadth score

### 7.3 CREATOR / QUALIFIED WALLET PRIORS
- creator history score
- creator sell flag
- creator holdings trend
- qualified-wallet participation score
- top-trader participation score
- first-100-buyer persistence contribution
- wallet dispersion quality score
- distribution-behavior penalty

Qualified Wallet Prior Module rules:
- this module is a capped prior only
- it may improve ranking, confidence, or position-quality assessment
- it may never act as a standalone entry trigger
- negative wallet/distribution evidence should carry stronger penalty weight than positive wallet evidence carries boost
- creator history, creator holdings trend, qualified-wallet participation, and first-100-buyer persistence may improve borderline setups only if core flow/breadth/fractional EV conditions already pass
- wallet prior outputs must be attributable in replay and learning

IMPORTANT: qualified-wallet and top-trader activity are priors only, never standalone triggers.

### 7.4 FRICTION / EXECUTION
- expected entry slippage
- expected exit slippage
- route mode
- priority-fee burden
- landing-risk estimate
- retry/failure rate
- execution freshness
- route score
- route_expected_value_adjustment
- route-health prior
- latency-budget utilization

### 7.5 MANIPULATION / DISTRIBUTION
- creator sell
- repeated same-size prints
- price-up / breadth-flat divergence
- concentration worsening
- cluster correlation
- suspicious burst behavior
- slippage shock without breadth
- distribution-event signatures

### 7.6 SECONDARY MULTIMODAL JUNK FILTER
- ticker clarity
- name clarity
- logo presence
- logo quality / coherence score
- metadata repetition / spam-likeness
- comment entropy / spam penalty if available
- optional social pickup score if available

Secondary Multimodal Junk Filter rules:
- this module is asynchronous and non-blocking
- it must remain secondary to on-chain flow and structure
- used for: obvious junk exclusion, tie-breaking between similar candidates, candidate ranking refinement within WATCH / ENTER_READY
- must never delay fast-lane entry promotion
- must never delay forced exits
- if multimodal inputs are unavailable or stale, the fast lane still operates normally

---

## 8. PROBABILITY LAYER

Build regime-specific probabilistic outputs:
- P_continuation_5s
- P_continuation_15s
- P_reversal_5s
- P_reversal_15s
- P_manipulation_event

Implementation order:
1. deterministic weighted feature stack
2. calibration layer
3. EV decision layer

Probability layer integration rules:
- qualified-wallet prior module may contribute as a capped positive / stronger negative prior
- secondary multimodal junk filter may contribute only as exclusion/tie-break/ranking refinement
- route_expected_value_adjustment must feed EV comparisons where execution-path choice materially changes expected outcome
- no single enhancement module may override core flow/breadth/manipulation gates by itself

Do not build a monolithic black-box model first.

---

## 9. FINAL ENTRY ENGINE

### 9.1 ENTRY DOCTRINE (NON-NEGOTIABLE)
OpenClaw enters only when expected short-horizon net liquidation value from entering now exceeds BOTH:
- zero
- the expected value of waiting

The system must compare: ENTER NOW vs WAIT vs PASS

### 9.2 HARD ENTRY FILTERS
Reject if any true:
- excluded regime
- creator_sold == true
- stale friction estimate
- stale market feed
- stale probability layer
- manipulation risk above hard threshold
- concentration above hard threshold
- slippage estimate above hard threshold
- system health degraded

### 9.3 ENTRY FORMULAS

```
EV_enter_now =
  P_continuation_h * upside_net
  - P_reversal_h * downside_net
  - P_manipulation_h * manipulation_cost
  - friction_cost_now
  + route_expected_value_adjustment

EV_wait =
  expected value of waiting h seconds for more information

EntryEdge =
  EV_enter_now - max(0, EV_wait)
```

Enter only if:
- EV_enter_now > 0
- EntryEdge > 0
- breadth confirms velocity
- execution quality acceptable

### 9.4 OBSERVATION PREMIUM
Do not buy instantly on creation by default.
Observe briefly and enter only when:
- the cost of waiting exceeds the value of more information
- velocity is real
- breadth confirms
- manipulation probability is acceptable

### 9.5 POSITION SIZING

```
risk_budget = bankroll * risk_per_trade_pct

effective_stop_pct =
  raw_stop_pct
  + expected_entry_fee_pct
  + expected_exit_fee_pct
  + expected_exit_slippage_pct
  + safety_buffer_pct

position_size =
  min(
    risk_budget / effective_stop_pct,
    quick_spend,
    bankroll * max_alloc_pct,
    liquidity_cap,
    slippage_cap
  )
```

Notes:
- quick_spend is the operator-controlled default per-coin spend amount
- quick_spend is set at first run and can be updated later via chat control
- position sizing may size lower than quick_spend if risk, liquidity, or slippage constraints require it
- the system must never size above quick_spend unless an explicitly separate higher-level strategy profile is designed later

---

## 10. FINAL EXIT ENGINE

### 10.1 EXIT DOCTRINE (NON-NEGOTIABLE)
OpenClaw exits based on continuous net expected value.
Hold only while: EV_hold_h > EV_exit_now

### 10.2 CATASTROPHIC OVERRIDES
Immediate full exit if any true:
- creator_sold == true
- slippage_shock == true
- execution_path_failure == true
- manipulation_shock == true
- concentration_shock == true

### 10.3 NET MARKING
```
ExpectedNetExitNow =
  quoted sell value
  - all exit fees
  - expected exit slippage
  - network + priority fees
```

### 10.4 HOLD FORMULA
```
EV_hold_h =
  P_continuation_h * upside_if_hold
  - P_reversal_h * downside_if_hold
  - P_manipulation_h * shock_cost
  - extra_friction_if_hold

HoldEdge = EV_hold_h - ExpectedNetExitNow
```

Rules:
- hold if HoldEdge > 0 and no override
- reduce/exit if HoldEdge <= 0
- reduce earlier when boundary risk rises
- reduce earlier when slippage worsens
- reduce earlier when manipulation risk rises

### 10.5 PEAK NET PROTECTION
```
PeakNetExitValue = max(PeakNetExitValue, ExpectedNetExitNow)
NetRetrace = 1 - (ExpectedNetExitNow / PeakNetExitValue)
```
Exit/reduce if NetRetrace exceeds dynamic threshold.

Dynamic threshold tightens when:
- curve progress enters boundary zone
- slippage worsens
- hold-edge weakens
- time in trade rises

### 10.6 TIME DECAY
Increase exit pressure as time in trade rises.
If the trade fails to pay quickly enough, prefer exit.

---

## 11. MANIPULATION MODEL

**A. Hard shock detector:**
- creator sell
- repeated same-size prints
- price up + breadth flat
- sudden concentration worsening
- cluster exit signature
- slippage blowout without healthy breadth

**B. Continuous penalty:**
- manipulation_penalty in [0,1]
- feeds both entry and exit

---

## 12. FRICTION MODEL

Model all live costs:
- Pump / PumpSwap fee
- PumpPortal fee
- Solana base fee
- Solana priority fee
- expected entry slippage
- expected exit slippage
- route-specific landing degradation

Use net liquidation value everywhere.
Never use raw price as the core profitability metric.
Fee schedules must be versioned by config and regime.

---

## 13. EXECUTION ROUTE POLICY

Default route policy:
- Local by default
- Lightning only when:
  - estimated opportunity half-life is short
  - edge is high enough to justify the extra fee
  - route-health policy says promote
- Jito bundles only for true multi-transaction atomic use cases

Execution Promotion Policy:
- route selection governed by formal scoring policy
- maintain route-specific health priors for: landing latency, retry/failure rate, recent congestion, fee burden, route freshness
- compute route_score and route_expected_value_adjustment before promotion/demotion
- promotion Local→Lightning only when: opportunity half-life short enough, expected edge clears extra fee, route-health priors support, safety bounds respected
- demotion back to Local when promotion conditions no longer hold
- explicit latency budgets by route mode
- skipPreflight policy by route/task class where relevant
- record route choice attribution for replay and learning

Execution adapter responsibilities:
- construct route request
- sign/send with Phantom hot wallet
- set slippage
- set priority fee
- record send time, confirmation time, realized fill, route_mode used

---

## 14. STATE MACHINE

Token states: OBSERVE, WATCH, ENTER_READY, LONG, REDUCE, EXIT, BAN

Required transitions:
- OBSERVE → WATCH: when enough data exists and token not excluded
- WATCH → ENTER_READY: when hard filters pass and EntryEdge > threshold
- ENTER_READY → LONG: only when OpenClaw explicitly calls buy_token
- LONG → REDUCE: when HoldEdge weakens materially but not catastrophic
- LONG → EXIT: when EV_exit_now >= EV_hold_h or override trips
- ANY → BAN: when manipulation/system fault/policy exclusion triggered

---

## 15. OPENCLAW PLUGIN TOOL SURFACE

Create one plugin: `@alon/pump-quant`

Expose only:
1. `get_top_candidates()`
2. `inspect_candidate(mint)`
3. `buy_token(mint, size_sol, slippage_bps, priority_fee_sol, route_mode)`
4. `sell_token(mint, amount_pct, slippage_bps, priority_fee_sol, route_mode, reason)`
5. `get_positions()`
6. `pause_trading(reason)`
7. `resume_trading()`
8. `get_bot_health()`
9. `get_risk_settings()`
10. `update_risk_settings(settings)`
11. `get_strategy_profile()`
12. `set_strategy_profile(profile_name)`
13. `get_runtime_config()`
14. `update_runtime_config(patch)`

Notes:
- update_risk_settings must support quick_spend, max allocation, risk-per-trade, slippage ceilings, and similar bounded controls
- quick_spend is the single operator-controlled default spend amount per coin
- quick_spend must be set at first run and adjustable through chat/operator control
- update_runtime_config for safe operational changes only, under validation and versioning
- all side-effect tools optional/explicitly allowed
- all config/risk changes validated, persisted, versioned, and auditable

---

## 16. OPERATOR CHANNEL REQUIREMENT

WhatsApp is the operator channel.

Supports:
- health checks, candidate inspection, live positions
- pause/resume
- replay summaries
- risk setting reads/updates
- strategy profile switching
- bounded runtime config updates

WhatsApp onboarding/QR/pairing belongs to deployment/setup, NOT the trading architecture.

---

## 17. EVENT / ALERT ARCHITECTURE

Design principle:
- Bot trades autonomously by default.
- Chat is for exceptions, summaries, and operator controls only.
- Bot must never wait for operator approval to enter, reduce, exit, or auto-pause.

**Immediate alerts only:**
- buy filled, reduce filled, full exit filled, forced exit
- auto-pause
- stale market feed, execution-path failure
- config/datastore integrity failure

**Scheduled summaries:**
- one mid-session summary only if meaningful change occurred
- one end-of-day summary always

**Log only:**
- candidate churn, routine state transitions, minor reconnects, non-material health changes, low-level metrics

**Candidate policy:**
- no proactive candidate spam by default
- top candidates available on demand via chat
- notable candidates in summaries when meaningful

**Operator controls in chat:**
status, health, positions, top, inspect, pause, resume, pnl, risk, set quick_spend, set risk_per_trade, set max_alloc, set slippage_cap, profile, set profile

---

## 18. TIER INTEGRATION ARCHITECTURE

Design principle:
- Tiers are compute-budget levels, not approval gates.
- Bot must remain autonomous.
- Tiering must improve focus without delaying high-edge trades.

Core rule: parallel fast-lane and deep-lane analysis.

**Fast lane:**
- PumpPortal live websocket data only
- all new token discovery
- incremental rolling window updates
- provisional regime, momentum, breadth, friction, manipulation estimates
- immediate promotion to ENTER_READY when provisional edge strong enough
- all forced-exit logic without waiting for deep enrichment

**Deep lane:**
- selective Bitquery enrichment and heavier analytics
- refines holder structure, creator/top-trader context, first-100-buyer persistence, boundary risk, manipulation penalties
- only for top-priority watched tokens and active positions
- must never block the fast lane

**Tier semantics:**
- Tier 0 = discovery and instant exclusions
- Tier 1 = live incremental scoring on shortlisted tokens
- Tier 2 = sparse deep enrichment for top candidates and active positions

**Entry integration:**
- fast lane computes provisional EV_enter_now_fast
- deep lane computes refined EV_enter_now_full
- immediate entry allowed only when fast-lane edge materially exceeds threshold and no hard disqualifier
- otherwise wait for refined deep-lane confirmation

**Exit integration:**
- all catastrophic and time-sensitive exit triggers from fast lane
- deep lane may refine hold-edge, sizing confidence, boundary-risk tightening
- deep lane must never delay a forced or safety exit

---

## 19. HEALTH / FAILSAFE POLICY

If any required subsystem is stale or broken (market feed, friction estimate, probability layer, datastore write path, execution adapter, config integrity):
- NO NEW TRADES
- optionally flatten if already in risk-off mode
- surface error via get_bot_health()

**Fail closed to NO_TRADE, never fail open.**

---

## 20. PERSISTENCE / REPLAY

Persist at minimum:
raw_events, token_state, feature_snapshots, candidate_packets, trade_intents, orders, positions, config_versions, replay_runs, health_events

Every decision must be reproducible from: raw data, config version, regime label, probability outputs, route mode, realized fill.

---

## 21. CONFIGURATION MODEL

All params externalized and versioned:
- regime thresholds, manipulation thresholds, friction thresholds
- entry/exit weights/calibration
- retrace thresholds, time-decay schedule
- route policy thresholds, risk limits, quick_spend, execution settings
- fee schedule map
- llm_provider = anthropic only, llm_model = anthropic/claude-opus-4-6 only
- model orchestration settings, supervisory thinking policy by task class
- qualified wallet prior weights/caps, multimodal junk filter thresholds/async policy
- route promotion/demotion thresholds, route-health prior parameters
- learning attribution settings

Every trade references config_version.

---

## 22. VALIDATION / PAPER MODE

Before live capital:
- run paper mode on live feed
- persist synthetic fills
- inspect all entry/exit decisions, edge decay, false manipulation flags, route promotion logic
- compare paper EV vs realized live fills in canary phase

Metrics: net expectancy per trade, hit rate, drawdown, fill-adjusted EV gap, precision@K, average hold-edge decay, boundary-exit performance, paper/live discrepancy.

---

## 22A. STRATEGY IMPROVEMENT / CONTINUOUS LEARNING ARCHITECTURE

Design principle:
- learning must be continuous, autonomous, and off the hot trading path
- live trading loop must remain stable and deterministic
- learning uses standing orders + cron for exact scheduled execution

**Learning data model:**
- append learning ledger record on every material event
- each record includes: regime, feature snapshot, candidate packet, config version, route mode, realized fill quality, realized PnL, MFE, MAE, fast/deep lane agreement, exit timing quality, reject-regret, feature-family attribution, route attribution, wallet-prior attribution, multimodal-filter attribution

**Feature-family attribution:**
- momentum, breadth, qualified-wallet prior, multimodal junk filter, manipulation penalty, friction/route, regime/boundary contributions

**Learning cadences:**
1. Event-driven ledger append (on every material event, no delay)
2. Hourly micro-calibration (slippage, landing-risk, route-health, feed latency, friction priors)
3. Daily replay/attribution/challenger training (at fixed session cut time)
4. Daily canary-promotion (after daily replay)
5. Weekly deep retrain/regime review

**Champion/challenger framework:**
- promotion: offline replay → walk-forward → bounded canary → full promotion
- autonomous promotion gates with minimum sample size, net expectancy, drawdown, precision@K, forced exits, fill-adjusted EV gap, missed-edge regret rate
- automatic rollback on degradation

**Boundaries:**
- no operator approval in live execution loop
- no self-modification in hot trading path
- no autonomous changes to wallet/channel/security/quick_spend
- no arbitrary formula rewriting without replay/canary validation
- all learning jobs versioned, logged, replayable
- Opus is the only LLM for learning tasks in initial build

---

## 23. LIVE CANARY

Initial live mode:
- one position max, tiny risk budget
- pre-graduation only, Local only by default
- no Mayhem, no Tokenized-Agent
- manual pause always available
- route promotion disabled until paper/live validation justifies it
- quick_spend set by operator at first run

---

## 24. DELIVERABLES

1. OpenClaw plugin package
2. Local strategy daemon
3. Config schema
4. Persistence schema + migrations
5. Candidate packet schema
6. Regime classifier
7. Entry engine
8. Exit engine
9. Manipulation model
10. Execution adapter
11. Paper-trading mode
12. Replay mode
13. Operator control workflow
14. Health monitoring
15. Runbook
16. Model orchestration policy
17. Learning/challenger promotion jobs
18. Qualified wallet prior module
19. Secondary multimodal junk filter
20. Route scoring / execution promotion policy
21. Attribution-enhanced replay analytics

---

## 25. NON-NEGOTIABLES

- do not simplify away entry/exit decision layers
- do not use raw price as the core profit measure
- do not blind-copy wallets
- do not trade excluded regimes
- do not allow freestyle execution outside plugin tools
- do not expose wallet secrets in chat
- do not trade if friction or data freshness is stale
- do not skip replayability
- do not use Bun for the production gateway if WhatsApp is part of the setup
- do not introduce alternate-model routing in the initial build
- do not claim or encode guaranteed profitability or constant large returns
- do not make Opus a dependency for the hot trading path
