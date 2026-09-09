# Action semantics v1 — diagnostic contract, not executable authority

Qwen training must distinguish BUY (new position), ADD, HOLD, REDUCE (strict partial quantity), EXIT_ALL, SKIP and WATCH. Coarse SELL must be resolved from actual inventory/quantity evidence, never blindly mapped. Coarse reporting can aggregate subtypes later without discarding their semantics.

`src/north_star/actions.py` is an initial structural validator. Observed poor BUYs remain observed evidence, not erased or relabeled successful recommendations. Human statements are not verified actions. Deterministic recommendation support references must be present separately. All outputs remain `STRUCTURAL_ONLY_NOT_ADMITTED`, imitation_eligible=False; no referenced evidence is authenticated here.

Positions and reductions use native integers. Unknown position/quantity or outside-scope execution fails validation and must be retained in upstream quarantine/context records, not deleted. Execution scope Solana Pump.fun/PumpSwap. Other venues can be retained as context, not order targets.

TDD: eight RED failures for missing module, eight GREEN, then 16 passing regression tests including bad observed buys, distinct SKIP/WATCH, partial/full exits, unsupported venue and non-admission of apparent recommendations. Command: `python -m pytest tools/data-pipeline/tests/north_star/test_actions.py -q`.

Independent review found EXIT_ALL bool/float acceptance and contradictory recommendation origins. New failing regressions preceded fixes; the action suite now passes 19 tests. Exact integer full-exit quantities and consistent recommendation flags are enforced.

Remaining: full action/order schema, source-backed ownership/inventory reconciliation, evidence authenticity, deadlines/watch/thesis clocks, actual versus attempted execution, serialized target masks, policy calibration and independent episode admission. This module neither signs nor submits orders.
