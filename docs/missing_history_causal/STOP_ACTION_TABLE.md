# Stop trigger -> action table (paper model lane)

Code: `rust/crates/pump-quant-app/src/stop_policy.rs` (`action_for`, the only table) and
`src/engine/model_stop.rs` (`Engine::model_stop_evaluate[_with]`, where the engine applies it). Daemon wiring is in
`pump-quant-junction/src/bin/pq_daemon.rs` (every 20 ticks and at each replay barrier) and
`src/model_lifecycle.rs` (measurement and actions).
Commits: 7c08875bc879f6560f0a364d917d0bc2b69b14f6 (table, engine, daemon, per-row tests) and
669a538e95e9c1c34f7fcbb932d5d4d8df416ee6 (daemon helper tests). Boundary items and resource floors:
34f5bac2b9aeddb73cf7060a9c325275d7875e37 (drain refuses ADD), 78eab34f305f093078dc152bc1b3a62f32eb68d7 (hung-endpoint
management bound), 879d9a60e070638b599f2eb71e8821120cb2c489 (provisional metric pin, disk soft/hard floors, RAM bytes
floor), 2dce403b4c6394ad5045cc7dc1553441a006b90a (test hardening). Branch task/slice-m1acc.

## Invariants (every row; pinned by `stop_table_every_trigger_has_one_named_row_and_no_row_disables_management_protection_or_reconciliation`)
- Every row blocks new BUY entries and ADD.
- REDUCE/EXIT management continues wherever a valid verdict path and execution identity exist (`Continue::IfValid`).
- Protection always continues (`Continue::Always`).
- Reconciliation always continues (`Continue::Always`).
- The `Continue` type has no "stop" variant, so no row can switch these off.
- Management asks run on their own request table (`model_mgmt.table`). No row blocks that table.
- Nothing in the table closes a book. The only end-of-run action is the existing protective handoff.
- Inference failure is kept separate from entry restriction:
  - A hung endpoint stops entry asks.
  - Management keeps asking and acts on any valid verdict.
  - Protection never depends on inference.

## The table
| Trigger | Named action | BUY / ADD | Mgmt / Protect / Recon | Latch | Alert | Test |
|---|---|---|---|---|---|---|
| EndpointHung (3 consecutive abandoned requests, existing rule) | entry_asks_stopped_management_continues | Block / Block (`mgmt:refuse:add_blocked_safety_off`) | IfValid / Always / Always; management asks bounded (see below) | SAFETY_OFF `model_endpoint_hung` | ALERT_MODEL_ENDPOINT_HUNG | stop_endpoint_hung_stops_entry_asks_but_a_valid_management_verdict_still_reduces; stop_endpoint_hung_refuses_add_by_name; stop_hung_endpoint_management_asks_stay_bounded_and_protection_still_orders |
| ReconciliationFault (unresolved entry recon fault or mgmt-sell fault) | risk_off_reconcile_continues | Block | the correct report still settles once | SAFETY_OFF `reconciliation_fault` | ALERT_RECONCILIATION_FAULT | stop_reconciliation_fault_latches_risk_off_and_reconciliation_still_settles |
| DurableWriteFailure (new held-ledger or safety-file persist failure) | risk_off_durable_write_failed | Block | unchanged; nothing closed | SAFETY_OFF `durable_write_failure` | ALERT_DURABLE_WRITE_FAILURE | stop_durable_write_failure_latches_risk_off_by_name |
| DiskHeadroomLow (least free bytes over safety file, held journal and event stream < SOFT floor 20 GiB, or unmeasurable) | entry_restricted_disk_headroom | Block (`mgmt:refuse:add_blocked_stop:<name>`) | REDUCE fills; protection serves | while condition (clears) | ALERT_DISK_HEADROOM_LOW | stop_disk_headroom_low_restricts_entries_only_while_low; stop_disk_headroom_unmeasurable_restricts_like_low; stop_operational_warning_keeps_protection_serving |
| DiskHardFloor (MEASURED least free bytes < HARD floor 4 GiB; unmeasurable does not latch) | risk_off_disk_hard_floor_evidence_kept | Block | REDUCE fills; protection orders; journals keep being written; nothing deleted | SAFETY_OFF `disk_hard_floor`; no auto re-arm | ALERT_DISK_HARD_FLOOR | stop_disk_soft_floor_restricts_hard_floor_latches_and_evidence_keeps_being_written; disk_floors_take_the_least_free_path_and_unmeasurable_never_latches |
| RamHeadroomLow (bytes available to the daemon = min(host MemAvailable, own cgroup memory.max - memory.current) < 12 GiB workload floor, or unmeasurable) | entry_restricted_ram_headroom | Block | as above | while condition | ALERT_RAM_HEADROOM_LOW | stop_ram_headroom_low_restricts_entries_only_while_low; stop_ram_floor_is_workload_bytes_capped_by_the_cgroup; ram_input_reads_host_and_own_cgroup; stop_ram_headroom_from_meminfo |
| FeedGap (no slot progress > STALE_SECS) | entry_restricted_feed_gap | Block | as above | while condition | ALERT_FEED_GAP | stop_feed_gap_restricts_entries_only_while_gapped |
| RpcBudgetLow (discovery share of the bootstrap page budget exhausted) | entry_restricted_rpc_budget_held_reserve_kept | Block | as above; held reserve untouched | while condition | ALERT_RPC_BUDGET_LOW | stop_rpc_budget_low_restricts_entries_only_while_low; stop_rpc_budget_reserves_capacity_for_held_positions; bootstrap_budget_reserves_held_position_capacity |
| ShadowDivergence (hook; the shadow slice supplies it, `false` until then) | risk_off_shadow_divergence | Block | unchanged | SAFETY_OFF `shadow_divergence` | ALERT_SHADOW_DIVERGENCE | stop_shadow_divergence_hook_latches_risk_off |
| PaperLossStop (2 SOL start minus model-estimate equity >= 0.5 SOL, under the run's PINNED valuation metric) | risk_off_paper_loss_stop | Block | unchanged; closes nothing | SAFETY_OFF `paper_loss_stop` | ALERT_PAPER_LOSS_STOP | stop_paper_loss_stop_trips_at_exactly_half_a_sol_by_the_model_estimate_and_never_picks_the_better_valuation |
| UnknownLiquidationValue (any held model-managed position with no value) | risk_unknown_stop_new_exposure | Block | unchanged; books nothing | SAFETY_OFF `risk_unknown_liquidation_value`; no auto re-arm | ALERT_RISK_UNKNOWN_LIQUIDATION_VALUE | stop_unknown_liquidation_value_is_risk_unknown_never_zero_or_cost_and_never_auto_rearms; stop_value_book_never_substitutes_zero_or_cost_for_an_unknown_position; stop_shadow_estimate_missing_for_a_held_position_is_risk_unknown_not_zero |
| RunDeadline (6 h) | deadline_stop_entries_drain_then_handoff | Block for the rest of the run (sticky); ADD during the drain refused as `mgmt:refuse:add_blocked_stop:deadline_stop_entries_drain_then_handoff` | management and protection through the drain and after it | run phase (not SAFETY_OFF) | ALERT_RUN_DEADLINE_DRAIN | stop_run_deadline_stops_entries_drains_with_management_and_protection_then_requests_handoff_without_closing; stop_run_deadline_drain_refuses_add_by_name_while_reduce_executes; stop_run_phase_boundaries_and_default_bounds; run_deadline_defaults_to_6h_plus_30min_and_an_override_may_only_shorten; deadline_handoff_raises_the_existing_stop_sentinel_once_and_never_overwrites |
| LiveCapabilityPresent (`--live`, keypair, non-paper sink, non-Paper engine) | refuse_start_live_capability | n/a | n/a | refuse start (exit 95) | FATAL_LIVE_CAPABILITY_PRESENT | stop_startup_refuses_any_live_signing_or_submission_capability; startup_refuses_live_flag_keypair_and_non_paper_engine |

### Re-arm
- An explicit operator re-arm (`model_safety_rearm`) re-derives the entry block from the table.
- Any active operational restriction keeps entries blocked after a re-arm (`stop_rearm_with_an_active_operational_restriction_keeps_entries_blocked`).

## Paper loss stop: valuation rules
- Starting equity: 2 SOL (bankroll seed). Equity = cash (seed + realized - committed entry spend) + the sum of the held model-managed positions' liquidation values.
- The loss stop reads only the model valuation, net of costs, under ONE valuation metric id per run:
  - The metric id is recorded at run start (`Engine::model_stop_pin_valuation_metric`; the daemon pins it right after the
    start headroom check and logs `loss-stop valuation metric pinned for this run: <id>`; an unpinned engine pins at its
    first evaluation). It is immutable for the run: an evaluation offering the other metric is refused
    (`stop:valuation_metric_switch_refused`, `StopState::metric_switch_refused`) and the book is valued under the pinned
    metric - never the better of the two, never a fallback. A shadow-pinned run with no shadow estimate is risk-UNKNOWN.
    Test: `stop_valuation_metric_is_pinned_at_run_start_and_cannot_switch_mid_run`.
  - Until the shadow slice lands: `provisional:exec_quote_size_specific_v1(placeholder_for_shadow_model)` - PROVISIONAL
    (the `provisional:` prefix is part of the id and pinned by the test). This is the size-specific executable sell quote for the whole inventory at the latest reserve state, minus one landed exit leg (network fee p50 + exit tip).
  - When `model_stop_evaluate_with(.., Some(shadow))` is given the shadow estimates, the label is `shadow_model_net_liquidation`. A held position missing from the shadow estimates is UNKNOWN (`shadow_estimate_missing`).
- The external valuation (zero-size spot from `model_open_exposure`, no costs) is computed and reported beside it, both in `StopEvaluation` and in the daemon alert line. It is never consulted to decide. The test trips at exactly 0.5 SOL by the model estimate while the external valuation shows less than 0.05 SOL loss.
- Any unvalued held position makes the whole valuation `Unknown{mint, reason}`. It is never filled with zero or with cost.

## Hung model endpoint: bounded management asks
- Management asks for held positions go through `model_mgmt.table`, a `RequestTable` with capacity
  `MGMT_MAX_OUTSTANDING` = `model_lane::DEFAULT_MAX_OUTSTANDING` = 4 (live + abandoned), shared worker pool of
  `MODEL_WORKERS` = 2 threads behind a `sync_channel(MODEL_QUEUE = 4)`. Per mint at most one ask is live (dedupe), and a
  mint with a pending management/protective order is not asked.
- A hung ask is abandoned at its deadline (`CHAMPION_MAX_DECISION_AGE_MS` = 3 s of feed time;
  `mgmt:request_abandoned_deadline`). Abandoning frees the dedupe key but NOT the capacity slot (the worker is still
  occupied), so under a hung endpoint outstanding asks plateau at 4 and further asks are refused by name
  (`mgmt:refuse:submit:AtCapacity`) - no queue growth, no retry backlog. A late verdict for an abandoned ask is
  discarded (`mgmt:discard:abandoned`) and frees its slot; a stale one past its deadline is `mgmt:discard:late`; every
  accepted verdict is revalidated against current state.
- Protection is independent of the endpoint (print-driven), and data-readiness checks are unchanged.
- Test `stop_hung_endpoint_management_asks_stay_bounded_and_protection_still_orders`: 600 held-position ticks (1 s each,
  ~20 cadence slots) with every management call hung -> per tick: total and per-mint outstanding <= 4, live per mint
  <= 1, request bindings <= 4, submitted <= 4, no management order; at the end refused_capacity >= 10, the endpoint saw
  <= 4 calls, a collapse print still creates the protective order for the whole inventory; after release all 4 late
  REDUCE answers are discarded as abandoned, 0 accepted, no order, table and bindings empty.

## Run deadline and DRAIN
- `RUN_DEADLINE_MS` = 21,600,000 (6 h). `DRAIN_BOUND_MS` = 1,800,000 (30 min).
- Env overrides `PQ_RUN_DEADLINE_MS` / `PQ_DRAIN_BOUND_MS` can only shorten these.
- At the deadline:
  - New entries and ADDs stop for the rest of the run (sticky, even if a clock reading goes backwards).
  - Queued unfilled certain entry orders and ADDs are invalidated.
  - Uncertain orders stay pending for reconciliation.
- During the drain, REDUCE/EXIT management and protection continue; an ADD verdict is refused by the deadline row's
  name (`stop_run_deadline_drain_refuses_add_by_name_while_reduce_executes`: REDUCE fills, then ADD is refused, no order,
  inventory unchanged).
- After the drain bound, `handoff_due` is set and the daemon raises the existing `data/DAEMON_STOP` sentinel. It never overwrites an existing one.
- The existing stop gate then completes only when flat+reconciled or after a session/request/exposure-bound acknowledgement. Otherwise the daemon stays up, blocked and protecting.
- Nothing is force-closed. The test calls `report()` after HandoffDue and asserts the books did not change.

## Measured basis for thresholds
Values below are from run m1H2/m1R2 on 2026-10-09, binary fb784d6d…, and the host at the same time.

| Quantity | Measured | Threshold / use |
|---|---|---|
| Full resource budget | /training/mh_build/proc/RESOURCE_BUDGET.md | disk soft 20 GiB / hard 4 GiB; RAM 12 GiB |
| event_stream bytes/event (current v2 schema) | 436.6 B mean; AmmSwap 535, MarketTrade 420, CorpusFlowRow 373 | by-kind table in RESOURCE_BUDGET.md |
| event_stream rate by CAPTURE recv ms (674 s of tape s1) | median 35.53 MB/min, p99 40.52, max 41.12 MB/min | 6.5 h at max = 16.04 GB |
| flow.ckpt / held.json after 670 s capture | 16.78 MB / 206.7 kB | included in the 16.4 GB run need |
| /training free (statvfs) | 234.86 GB | need 16.4 GB + 21.5 GB soft floor = 37.9 GB -> 6.2x headroom |
| pq-daemon cgroup (started as the run starts it) | /system.slice/hermes-gateway.service, memory.max = max, memory.high = max | input = min(host avail, cgroup room) |
| pq-daemon VmHWM | 117.9 MB (196 s capture), 225.7 MB (670 s capture); projected 5.95 GB at 6.5 h | RAM floor 12 GiB = 2x projection |
| vLLM cgroups | sft-serve-8000.service: max/max; RL pilot in hermes-worker scope: memory.max 4 GiB, at limit (51,430 max events) | reported; not changed |
| Host RAM | MemTotal 263.4 GB, MemAvailable 245.9 GB | host-wide 12% rule superseded for the stop input |
| Pending order queue at stop | 3 uncertain (1 EXIT + 2 protect) | reconciliation always continues; never cancelled |
| flow_upstream_drops / windows_incomplete | 0 / 0 | feed-gap rule uses STALE_SECS (existing) |
| Helius plan / measured standing load | 200 req/s ceiling, 0.133 req/s (docs/HELIUS_BUDGET_2026-07-29.md) | bootstrap budget 3,600 pages/h (0.5% of ceiling), 120 pages/h reserved per held position (6 twenty-page walks) |

## Mutation checks
- Original rows: 34/34 CAUGHT (proc/STOP_MUTATION_BATCH.md, proc/m1acc_mut_stop_v2.jsonl).
- Boundary items + resource floors: runner `proc/mut_stop_m1acc_v3.py` (same scoped byte restore as v2; CAUGHT requires
  compiled AND a named test failing). Results `proc/m1acc_mut_stop_v3.jsonl`: **18/18 CAUGHT, 0 SURVIVED, 0
  COMPILE_ERROR**, every record `pre_restore_equals_mutant` and `restored_sha_ok` true, tree clean after.
  - add_not_refused_in_drain -> stop_run_deadline_drain_refuses_add_by_name_while_reduce_executes (+5 while-condition rows)
  - mgmt_bound_unbounded, mgmt_no_expire, abandon_frees_capacity, abandoned_verdict_accepted ->
    stop_hung_endpoint_management_asks_stay_bounded_and_protection_still_orders
  - metric_switches, metric_pinned_shadow_falls_back, metric_label_not_provisional ->
    stop_valuation_metric_is_pinned_at_run_start_and_cannot_switch_mid_run
  - hard_floor_never_raised, hard_floor_unknown_latches, hard_floor_value, hard_floor_not_latching ->
    stop_disk_soft_floor_restricts_hard_floor_latches_and_evidence_keeps_being_written (+ table invariant test)
  - ram_cgroup_ignored, ram_floor_ge_gt, ram_floor_value -> stop_ram_floor_is_workload_bytes_capped_by_the_cgroup
  - disk_jn_not_min, disk_jn_unknown_ok, disk_jn_hard_soft_swapped ->
    disk_floors_take_the_least_free_path_and_unmeasurable_never_latches

## Open items
- `ShadowDivergence` and the shadow liquidation estimates are hooks until the shadow slice lands; the loss stop runs
  on the PROVISIONAL exec-quote metric until then (a shadow-metric run is a different run id).
- Capture rates cover ~26 min of tape (two segments); RSS at 6 h is a linear projection from 11 min of capture.
- The legacy `MIN_FREE_BYTES` (1 GiB) start/periodic headroom ALERT lines in pq_daemon remain (log-only); the stop
  input now uses the 20 GiB / 4 GiB floors.
