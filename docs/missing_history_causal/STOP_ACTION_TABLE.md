# Stop trigger -> action table (paper model lane)

Code: `rust/crates/pump-quant-app/src/stop_policy.rs` (`action_for`, the only table) and
`src/engine/model_stop.rs` (`Engine::model_stop_evaluate[_with]`, where the engine applies it). Daemon wiring is in
`pump-quant-junction/src/bin/pq_daemon.rs` (every 20 ticks and at each replay barrier) and
`src/model_lifecycle.rs` (measurement and actions).
Commits: 7c08875bc879f6560f0a364d917d0bc2b69b14f6 (table, engine, daemon, per-row tests) and
669a538e95e9c1c34f7fcbb932d5d4d8df416ee6 (daemon helper tests). Branch task/slice-m1acc.

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
| EndpointHung (3 consecutive abandoned requests, existing rule) | entry_asks_stopped_management_continues | Block / Block (`mgmt:refuse:add_blocked_safety_off`) | IfValid / Always / Always | SAFETY_OFF `model_endpoint_hung` | ALERT_MODEL_ENDPOINT_HUNG | stop_endpoint_hung_stops_entry_asks_but_a_valid_management_verdict_still_reduces; stop_endpoint_hung_refuses_add_by_name |
| ReconciliationFault (unresolved entry recon fault or mgmt-sell fault) | risk_off_reconcile_continues | Block | the correct report still settles once | SAFETY_OFF `reconciliation_fault` | ALERT_RECONCILIATION_FAULT | stop_reconciliation_fault_latches_risk_off_and_reconciliation_still_settles |
| DurableWriteFailure (new held-ledger or safety-file persist failure) | risk_off_durable_write_failed | Block | unchanged; nothing closed | SAFETY_OFF `durable_write_failure` | ALERT_DURABLE_WRITE_FAILURE | stop_durable_write_failure_latches_risk_off_by_name |
| DiskHeadroomLow (free bytes < 1 GiB on the safety-file filesystem, or unmeasurable) | entry_restricted_disk_headroom | Block (`mgmt:refuse:add_blocked_stop:<name>`) | REDUCE fills; protection serves | while condition (clears) | ALERT_DISK_HEADROOM_LOW | stop_disk_headroom_low_restricts_entries_only_while_low; stop_disk_headroom_unmeasurable_restricts_like_low; stop_operational_warning_keeps_protection_serving |
| RamHeadroomLow (MemAvailable < 12% of MemTotal, or unmeasurable) | entry_restricted_ram_headroom | Block | as above | while condition | ALERT_RAM_HEADROOM_LOW | stop_ram_headroom_low_restricts_entries_only_while_low; stop_ram_headroom_from_meminfo |
| FeedGap (no slot progress > STALE_SECS) | entry_restricted_feed_gap | Block | as above | while condition | ALERT_FEED_GAP | stop_feed_gap_restricts_entries_only_while_gapped |
| RpcBudgetLow (discovery share of the bootstrap page budget exhausted) | entry_restricted_rpc_budget_held_reserve_kept | Block | as above; held reserve untouched | while condition | ALERT_RPC_BUDGET_LOW | stop_rpc_budget_low_restricts_entries_only_while_low; stop_rpc_budget_reserves_capacity_for_held_positions; bootstrap_budget_reserves_held_position_capacity |
| ShadowDivergence (hook; the shadow slice supplies it, `false` until then) | risk_off_shadow_divergence | Block | unchanged | SAFETY_OFF `shadow_divergence` | ALERT_SHADOW_DIVERGENCE | stop_shadow_divergence_hook_latches_risk_off |
| PaperLossStop (2 SOL start minus model-estimate equity >= 0.5 SOL) | risk_off_paper_loss_stop | Block | unchanged; closes nothing | SAFETY_OFF `paper_loss_stop` | ALERT_PAPER_LOSS_STOP | stop_paper_loss_stop_trips_at_exactly_half_a_sol_by_the_model_estimate_and_never_picks_the_better_valuation |
| UnknownLiquidationValue (any held model-managed position with no value) | risk_unknown_stop_new_exposure | Block | unchanged; books nothing | SAFETY_OFF `risk_unknown_liquidation_value`; no auto re-arm | ALERT_RISK_UNKNOWN_LIQUIDATION_VALUE | stop_unknown_liquidation_value_is_risk_unknown_never_zero_or_cost_and_never_auto_rearms; stop_value_book_never_substitutes_zero_or_cost_for_an_unknown_position; stop_shadow_estimate_missing_for_a_held_position_is_risk_unknown_not_zero |
| RunDeadline (6 h) | deadline_stop_entries_drain_then_handoff | Block for the rest of the run (sticky) | management and protection through the drain and after it | run phase (not SAFETY_OFF) | ALERT_RUN_DEADLINE_DRAIN | stop_run_deadline_stops_entries_drains_with_management_and_protection_then_requests_handoff_without_closing; stop_run_phase_boundaries_and_default_bounds; run_deadline_defaults_to_6h_plus_30min_and_an_override_may_only_shorten; deadline_handoff_raises_the_existing_stop_sentinel_once_and_never_overwrites |
| LiveCapabilityPresent (`--live`, keypair, non-paper sink, non-Paper engine) | refuse_start_live_capability | n/a | n/a | refuse start (exit 95) | FATAL_LIVE_CAPABILITY_PRESENT | stop_startup_refuses_any_live_signing_or_submission_capability; startup_refuses_live_flag_keypair_and_non_paper_engine |

### Re-arm
- An explicit operator re-arm (`model_safety_rearm`) re-derives the entry block from the table.
- Any active operational restriction keeps entries blocked after a re-arm (`stop_rearm_with_an_active_operational_restriction_keeps_entries_blocked`).

## Paper loss stop: valuation rules
- Starting equity: 2 SOL (bankroll seed). Equity = cash (seed + realized - committed entry spend) + the sum of the held model-managed positions' liquidation values.
- The loss stop reads only the model valuation, net of costs. Estimator in use:
  - Until the shadow slice lands: `exec_quote_size_specific_v1(placeholder_for_shadow_model)`. This is the size-specific executable sell quote for the whole inventory at the latest reserve state, minus one landed exit leg (network fee p50 + exit tip).
  - When `model_stop_evaluate_with(.., Some(shadow))` is given the shadow estimates, the label is `shadow_model_net_liquidation`. A held position missing from the shadow estimates is UNKNOWN (`shadow_estimate_missing`).
- The external valuation (zero-size spot from `model_open_exposure`, no costs) is computed and reported beside it, both in `StopEvaluation` and in the daemon alert line. It is never consulted to decide. The test trips at exactly 0.5 SOL by the model estimate while the external valuation shows less than 0.05 SOL loss.
- Any unvalued held position makes the whole valuation `Unknown{mint, reason}`. It is never filled with zero or with cost.

## Run deadline and DRAIN
- `RUN_DEADLINE_MS` = 21,600,000 (6 h). `DRAIN_BOUND_MS` = 1,800,000 (30 min).
- Env overrides `PQ_RUN_DEADLINE_MS` / `PQ_DRAIN_BOUND_MS` can only shorten these.
- At the deadline:
  - New entries and ADDs stop for the rest of the run (sticky, even if a clock reading goes backwards).
  - Queued unfilled certain entry orders and ADDs are invalidated.
  - Uncertain orders stay pending for reconciliation.
- During the drain, REDUCE/EXIT management and protection continue.
- After the drain bound, `handoff_due` is set and the daemon raises the existing `data/DAEMON_STOP` sentinel. It never overwrites an existing one.
- The existing stop gate then completes only when flat+reconciled or after a session/request/exposure-bound acknowledgement. Otherwise the daemon stays up, blocked and protecting.
- Nothing is force-closed. The test calls `report()` after HandoffDue and asserts the books did not change.

## Measured basis for thresholds
Values below are from run m1H2/m1R2 on 2026-10-09, binary fb784d6d…, and the host at the same time.

| Quantity | Measured | Threshold / use |
|---|---|---|
| held.json (3 positions, 3 pending orders) | 7,756 B (m1H2), 6,832 B (m1R2) | disk floor 1 GiB = about 138,000x the ledger |
| flow.ckpt | 8,376,695 B | 1 GiB floor = about 128x one checkpoint |
| event_stream.jsonl growth | 87.9 MB in about 60 s uptime (replay at accelerated rate) | **not covered by the floor**; see open items |
| /training free | 283.9 GB (86% used) | far above the floor |
| Host RAM | MemTotal 263.4 GB, MemAvailable 245.7 GB | floor 12% of total (operator rule) = 31.6 GB |
| Pending order queue at stop | 3 uncertain (1 EXIT + 2 protect) | reconciliation always continues; never cancelled |
| flow_upstream_drops / windows_incomplete | 0 / 0 | feed-gap rule uses STALE_SECS (existing) |
| Helius plan / measured standing load | 200 req/s ceiling, 0.133 req/s (docs/HELIUS_BUDGET_2026-07-29.md) | bootstrap budget 3,600 pages/h (0.5% of ceiling), 120 pages/h reserved per held position (6 twenty-page walks) |

## Mutation checks
Runner: `/training/mh_build/proc/mut_stop_m1acc.py` (34 mutants defined, one or more per row). Results:
`/training/mh_build/proc/m1acc_mut_stop.jsonl`.
- recon_raise: CAUGHT by stop_reconciliation_fault_latches_risk_off_and_reconciliation_still_settles.
- durable_raise: CAUGHT by stop_durable_write_failure_latches_risk_off_by_name.
- **The other 32 have not run.** The next batch command was rejected by the approval guard (proc/BLOCKED_m1acc.md). It was not retried. Until those mutants run, those rows are tested but not mutation-checked.

## Open items
- Event-stream growth versus the 1 GiB disk floor: a 6 h run at the replay rate would write far more than 1 GiB. The floor protects the durable state files, but event_stream growth should become its own budget. This needs a real-time (not accelerated) growth measurement.
- `ShadowDivergence` and the shadow liquidation estimates are hooks until the shadow slice lands.
- `RamHeadroomLow` uses the host-wide figure, not the 4 GiB worker memcg.
