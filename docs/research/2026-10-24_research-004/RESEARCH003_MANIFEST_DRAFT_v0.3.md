# research-003 — "liquidity absorption veto" bounded paper test — MANIFEST DRAFT v0.2 (for review; NOT armed)
Prepared 2026-10-24 ~03:40 PT. Supersedes EXPERIMENT_SPEC_ABSORPTION §5. Arms are built but no book is armed by this document.

## 0. Hypothesis (corrected wording)
R = sell_volume_lamports / quote_reserves_lamports as rendered in the entry prompt: cumulative SELL quote volume since the mint's
FIRST CAPTURED TRADE (curve phase included, i.e. selling that happened before this AMM pool existed) divided by the CURRENT raw
quote vault of the pool snapshot. R is historical sell turnover relative to current raw liquidity. It does NOT measure selling this
pool absorbed, nor remaining absorption capacity. Hypothesis under test: high R marks opportunities where our BUY tends to become
exit liquidity for earlier holders and executable returns are negative. Competing explanations kept open: token age (cumulative
volume grows with time), gross churn, and an already-depleted denominator (a vault that has fallen raises R after the decline).
R is unchanged from the panel definition. No new feature search.

## 1. Arms and the primary comparison
Common stream: ONE daemon, ONE feed, ONE discovery/eligibility path; every entry ask is formed BEFORE any arm-dependent
capital/held-mint veto and delivered to every arm (`research_arms::Arms::tick`, implemented, commit 9570c0c2). A's depleted
account cannot suppress B/C opportunities. Union of all arms' exposure feeds is protected within the declared reserve.
- A (baseline): unchanged compact Qwen entry (probe_sl_v1_compact, identity probe-mg3-base served=4, paper-engine-8004), Qwen SIZE
  tier, PROD_HOLD_V16, Rust protections 3500/3000 bp. The primary daemon engine.
- B (challenger, PRIMARY COMPARISON A vs B): identical to A except ONE declared admission rule evaluated on the valid BUY from the
  exact prompt bytes before sizing: refuse when R > R* (`notrade:absorption`, components journaled); R unavailable (missing field or
  zero vault) → `notrade:absorption_unavailable` (named refusal, not zero risk). B then evolves under its own cash/exposure.
  Shared inference with A: rendered entry prompts contain no arm state, so identical bytes are answered once (MemoSource; hit/miss
  counters journaled). Management prompts DO depend on arm state (position, basis): each arm pays its own management inference —
  serving load = 1× entry + (number of arms holding) × management; disclosed in the serving-capacity account.
- C (SECONDARY whole-strategy benchmark, no LLM): BUY iff pricing_eligible == true AND net_flow_sol_300s > 0 (300 s net SOL flow,
  the prompt's field) AND ret_30s_bp > 0 AND R <= R*; any missing/unparseable field or unavailable R → SKIP. SIZE is the fixed
  SMALL tier (not a model sizing output). Timing differs from Qwen (no inference latency) — part of the whole-strategy comparison.
  Because C's sizing/timing/selection differ from A/B jointly, C > B cannot show that arithmetic rather than the LLM caused an
  advantage; differences are reported, no extra arms are added to resolve them.
Re-entry after own stop: diagnostic only, all arms; no cooldown rule.

## 2. Calibration of R* (frozen rule; applied once at the panel endpoint; no sweep)
Unit = enrolled MINT EPISODE in the shadow panel (PREREG_SHADOW_PANEL): the FIRST ask on a mint after enrollment is the episode's
representative observation; later asks on the same mint (repeats, re-entries) are excluded from calibration so repeated requests
cannot dominate. Studied-12 mints excluded. Population for R*: episodes whose representative ask carried a valid BUY verdict
(before any capital veto) AND an available R. R* = median R of that population (upper median on even counts). Episodes with
unavailable R are counted and reported as excluded from calibration; they do not move R*. The median is an outcome-blind
rejection-rate heuristic (~50 % of BUY-intent episodes refused), not an economic optimum. If the population has < 10 episodes,
calibration is NOT performed and the test is not proposed.
Panel verdict "useful" (published before inspecting outcomes; = PREREG_SHADOW_PANEL §Decision rule, restated numerically): within
Qwen-BUY episodes, Kendall τ between R and the primary 120 s net markout is negative with a by-mint bootstrap 90 % interval
excluding 0; the partial rank association after rank-adjusting for age_s, pool age, gross turnover/vault and ret_30s_bp keeps the
sign; leave-one-mint-out keeps the sign in every fold. Any other result → NOT useful → R retired, no paper test proposed.
Once R* is set from the panel, the panel is development/calibration data; research-003 runs on later untouched opportunities only.

## 3. Window, enrollment and resolution
Fixed endpoint: 48 h from arming OR 200,000 entry INFERENCE DISPATCHES on the common stream (A's dispatch counter; one ask =
one dispatch, repeats included) OR the aggregate data limit, whichever first. No extension for any statistic.
Enrollment: every PumpSwap-regime entry ask on the common stream (BUY/SKIP/refused/vetoed labelled per arm); unique mint episodes,
repeats and re-entries reported separately. Eligible BUY intentions are counted BEFORE capital vetoes per arm.
Coverage floor for reading A vs B: ≥ 30 unique mint episodes with a valid A BUY verdict in the window — a coverage floor, not
power. Resolution is never a condition of enrollment: unavailable outcomes, open exposure and unexecutable exits are retained in
all denominators with reasons. B's actual EXECUTED-mint coverage (fills) is reported alongside.

## 4. Capital, limits, costs, terminal inventory
Per arm: declared simulated seed 2.0 SOL; loss-from-start budget 0.300 SOL (worst case at admission); survival floor 0.5 SOL;
max 2 open+pending; Qwen SIZE tier (A/B) / SMALL (C); venue fees by fee parts + 10,000 lamports/leg; failed attempts unmodeled
(stated). Each arm's budget/floor is its own; no transfers between arms or from research-001/cohort-003.
Terminal inventory at the endpoint: positions are NOT sold synthetically. Report realized P&L, open basis, and the dated
executable mark (size-feasible proceeds on the last valid state with verified vq) or UNKNOWN; management continues under the
existing protections until closed by rule or an administrative stop is recorded. Economic equity = free cash + Σ marks (UNKNOWN →
equity UNKNOWN, with the bound "cash + 0" also stated).

## 5. Economic evaluation and success criteria (units stated)
Primary metric per arm: net realized SOL over the window vs the 2.0 SOL no-trade cash reference (realized_end − 0).
Also: ending economic equity after costs (free cash + marks), unresolved exposure separate.
Success for B (ALL required; point estimates, necessary not sufficient):
 (i) B ending economic equity − 2.0 SOL > 0 AND B realized > 0;
 (ii) B − A ending economic equity > 0;
 (iii) B maximum drawdown of economic equity from the 2.0 start ≤ 0.150 SOL (= 7.5 % of seed);
 (iv) B unresolved exposure at endpoint (open basis with UNKNOWN mark) ≤ 0.050 SOL AND the pass in (i) survives valuing every open
      position at 0 (plausible total loss on unresolved inventory must not erase the profit);
 (v) concentration: the largest single mint's positive contribution ≤ 50 % of the SUM of positive mint contributions;
 (vi) operational gates: p95 entry latency within the lane deadline, protection trigger→intent < 1 s p95, 0 ledger identity
      failures, no safety latch.
Uncertainty: by-mint bootstrap of (ii) with mint episodes as the resampling unit and time-ordered block structure disclosed;
shared-capital effects mean per-trade differences are not independent — reported, not assumed away. A pass supports one further
independent paper confirmation window; it does not establish production profitability. Losing less than A is progress only.
Rejected winners / avoided losers: for every A BUY-intent episode, B's disposition and both arms' outcomes are tabulated.
Stress (assumptions, not probabilities): failed-landing scenarios 0 / 10 / 25 % applied as a deterministic hash of (mint, ask id)
so the SAME opportunities fail across arms; plus an adverse-condition scenario where failures hit the top quartile of |ret_30s_bp|
asks first (failures correlate with volatility). Applied post hoc to journaled fills as a sensitivity; declared here.
Operating cost: the shared feed is paid once by the experiment; deployability is assessed on each strategy's STANDALONE cost
(one daemon's feed ≈ 43k credits/h observed + its own GPU serving), never on a divided bill.

## 6. Spending and authority bounds
Experiment feed cap ≤ 2.5M credits charged within the existing 27.5M allocation; exposure reserve 500k protected and never
borrowed; PQ_FEED_WINDOW_ALLOC_CREDITS set accordingly; discovery contracts before any exposure feed. RPC ≤ 100 calls (dated marks).
GPU: existing paper-engine-8004 on GPU1/2 only; GPU0 untouched. No new paid service, firehose expansion or purchase.
Rollback: ENTRY_PAUSE (all arms), protective handoff; per-arm MODEL_HELD/MODEL_SAFETY/model_io under data/arms/<id>/; versions pinned.
No live capital, no live orders, no automatic promotion: arms exist only in RunMode::Paper (refused in live mode by construction).
Arming requires: (a) panel endpoint verdict "useful" and R* calibrated per §2; (b) this manifest frozen (v1) with the binary digest;
(c) explicit go from Alon for the new funded paper book. A "useful" panel result alone does not arm it.

## 7. Implementation status (evidence)
- Engine: `notrade:absorption` / `notrade:absorption_unavailable` before sizing from exact prompt bytes; `absorption::RuleSource`
  emits valid compact records; `MemoSource` shares identical prompts (unit tests 3/3).
- Daemon: `PQ_RESEARCH_ARMS="B:veto:<bps>;C:rule:<bps>"` builds isolated arms (own cash/orders/held/protection/safety/journal/
  ledger under data/arms/<id>/), fan-out on every feed path, quiet-pool snapshots, exposure UNION for feed protection, entries-
  blocked/pause fan-out, persist on close. Live mode refuses arms.
- e2e (research_arms_e2e, passing): A capital-vetoed while B fills the same opportunity; B2 refused by name; C never invalid;
  shared lane answered fewer calls than dispatches; conservation per book; duplicate delivery adds nothing; restart restores B
  exactly once (idempotent persist); A's entry block does not disable B's protection (B hard-stops on a 0.5× state, A/B2 untouched).
Remaining blockers before arming: desk/report aggregation over arms (per-arm economics lines), protective-handoff assessment to
include arms' exposure (currently union is used for feeds, handoff JSON reads the primary only), panel endpoint + R*, and the go.
Estimated remaining engineering: ~3–4 h. Total resource estimate for the test: ≤ 2.5M credits, 48 h wall, existing GPU serving.

## v0.3 amendments (2026-10-24 ~09:55 PT) — supersede the matching v0.2 text
A. Management authority (verified in code and journal): under `ManagementPolicy::PROD_HOLD_V16` the engine NEVER dispatches
   management inference (`model_mgmt_schedule`: "PROD_HOLD control: never ask; HOLD until the barrier, then EXIT" — exits are a
   standing intent through the ordinary landing path; Rust protections independent). research-001 journal: 44,298 dispatches, all
   `kind=entry`, zero `mgmt`. The v0.2 sentence "each arm pays its own management inference" is WRONG and withdrawn: physical
   serving load for A+B = ONE entry inference per common ask (memo), C = none. The serving process registers LoRA modules
   `probe-mg3-entry` (fixture), `probe-mg3-mgmt` (= mgmt_pilot_001/adapter_step0250) and `probe-mg3-blank`; the engines request
   model id `probe-mg3-base` only — step250 is never requested and remains excluded; fixture adapters never supply decisions.
B. Calibration unit (amendment documented before outcome inspection): representative observation = the mint's FIRST ask with a
   VALID BUY verdict before capital veto (order created OR capital-veto disposition), re-entry asks excluded, studied-12 excluded by
   full pubkey (STUDIED_12_MINTS.json). `calibrate_rstar.py` is the committed selector; population at 09:40 PT = 27 of 35 panel
   mints (8 without a valid BUY verdict; 0 with unavailable R). R* serialization: `r_star_bps` u64 = floor(sell×10⁴/vault);
   engine rule `r_bps > r_star_bps` → veto; equality passes; R*+1 bps vetoes; unavailable = named refusal (unit tests).
C. Dispatch cap scope: 200,000 = entry INFERENCE DISPATCHES of the primary (A) on the common stream (one ask = one dispatch;
   repeats included); B shares them physically; C's rule evaluations are not inference. The cap is a data/serving bound, not an
   opportunity count; unique mint episodes are reported separately.
D. Economic labels: the 90 % interval criterion is an EXPLORATORY SCREEN; ≥ 30 episodes is a coverage floor; neither is proof of
   profitability. Success (i)/(ii) use TRADING profit after execution costs (fees, impact, failed-landing stress) and EXCLUDE
   operating costs; "deployable profit" = trading profit − standalone operating cost (one daemon's feed ≈ 43k credits/h at the
   observed rate + GPU serving hours) is reported SEPARATELY and is not a pass condition for this pilot. The by-mint bootstrap does
   not account for shared-cash coupling or time dependence; it is reported with that caveat, not as full uncertainty.
E. Serving acceptance: the live burst test (serving_burst.py) was INVALID and HARMFUL (unrepresentative ~2k-token prompts, 64
   concurrent; tripped research-001's `model_endpoint_hung` latch 09:40:40–09:47:20) and is withdrawn. Evidence retained:
   (1) e2e `arms_are_isolated_books_sharing_one_feed_and_one_model_lane`: shared memo answered fewer physical calls than logical
   dispatches with hits > 0; (2) single-lane production latency p50 0.80 s / p95 1.4 s at ~6k dispatches/h; (3) because PROD_HOLD
   has no management inference, the arms' physical load equals A's. Acceptance GATE for the pilot's first hour (predeclared):
   `ARMS_STATUS.shared_lane.physical_calls` ≤ 1.05 × primary dispatches AND lane p95 ≤ 1.6 s AND 0 `model_endpoint_hung`; else
   pause arms and report. No further live load tests against the shared server.
F. Collection repair (research-001 v4, binary 4ef06eab…, commit 0d42ebde): state ledger MAX_TRACKED_MINTS refuse-not-evict
   starved admission after ~4 h uptime (dispatch 6.3k/h → 14/h; AMM asks 0 after 05:32). Idle (>30 min) exposure-free mints are now
   forgotten every 60 s; gauges `gauge:ledger_refused_mints/prints` published. Regression test fails without the sweep (300
   refused). Dispatch recovered to ~1.8k/h within 10 min of the restart (warming).
G. Remaining before arming: panel endpoint verdict + R* (calibrate_rstar.py --set); manifest v1 freeze with binary digest;
   per-arm desk lines are implemented (desk_report.py); handoff over the union is implemented and tested; Alon's go.
