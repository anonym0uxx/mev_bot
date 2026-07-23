# REGRESSION MANIFEST — `pq-regression`

The single human-facing register of every regression invariant this crate guards,
what each protects, and the pinned value / expectation. The machine-readable twin
is [`src/baselines.rs`](src/baselines.rs); `tests/regression_manifest.rs` asserts
this file and that module never drift apart (a re-pin must edit BOTH).

The crate ADDS coverage only. It changes no behaviour, pins nothing new about the
engine, and never edits a `dossier_*` file, a golden digest, or a pinned constant.
Everything here is integer / deterministic, fast (< 30 s), and needs no network,
no wall-clock, and no RNG (§22).

At the pinned green HEAD: `cargo test --workspace` = **1908 passed / 0 failed**,
golden digest `9156528138145267483` (net `1864780`), **191** dossiers intact.

---

## Pinned baselines (mirror of `src/baselines.rs`)

### Golden determinism fingerprint + outcome vector

| Baseline | Value | Guards |
|---|---|---|
| `GOLDEN_DIGEST` | `9156528138145267483` (hex `7f1285d40a047f1b`) | byte-exact decision-journal digest of the golden tape under `Config::dev_portable` — the primary determinism fingerprint (§22/§54) |
| `GOLDEN_NET_LAMPORTS` | `1864780` | realized §24-compliant cost-derived net-SOL on the golden tape |
| `GOLDEN_PROMOTED` | `504` | candidates promoted to the gate |
| `GOLDEN_ADMITTED` | `18` | candidates admitted by the gate |
| `GOLDEN_REJECTED` | `486` | candidates rejected by the gate |
| `GOLDEN_UNIVERSE_FILTERED` | `72` | §21.5 universe-screen removals (zombie cohort) |
| `GOLDEN_NET_FIXED_LADDER` | `1852159` | net with the §24 cost-derived ladder DISABLED (forbidden fixed 13500/25000/50000 ladder) |
| `GOLDEN_DERIVED_MINUS_FIXED` | `12621` | margin by which cost-derived out-earns the fixed ladder on the representative tape (re-pin #13, preserved through the re-pin #14 Discord alpha cohort) |

**Golden net re-pin arc** (`GOLDEN_NET_ARC`, mirrored from `golden_digest.rs`):
`2979624 → 5017234 → 6443936 → 8785954 → 12550767 → 3831945 → 1406102 → 1864780`.
The arc must terminate at `GOLDEN_NET_LAMPORTS`; re-pin #12's documented signed
delta is `GOLDEN_ARC_REPIN12_DELTA` = `-8718822` (`3831945 - 12550767`).

### Bounded-state capacity pins (§99)

| Baseline | Value | Guards |
|---|---|---|
| `WATCHLIST_CAPACITY` | `64` | hard live-candidate cap; feeding 8× must never exceed it |
| `LIVE_CHATTER_CAP` | `16` | distinct-chatter breadth ceiling; past-cap flood must saturate |

### Structural / repository pins

| Baseline | Value | Guards |
|---|---|---|
| `DOSSIER_FILE_COUNT` | `191` | materialized `dossier_*.rs` property-test files across the workspace — a silent drop means a component lost its correctness authority |

### Law-toggle default pins (`Config::dev_portable`)

Booleans (`LAW_BOOL_DEFAULTS`): a silent default flip is itself a regression.

| Config key | Default | Law |
|---|---|---|
| `creator_dump_veto_enable` | `true` | §26 confirmed-creator-dump hard veto |
| `derived_targets_enable` | `true` | §24 cost-derived profit targets (mandated live default) |
| `into_strength_exit_enable` | `false` | §24(d) exit-into-strength |
| `vol_stop_enable` | `false` | §24 volatility-scaled stops/trail |
| `setup_classifier_enable` | `true` | §25 setup-archetype classifier |
| `entry_mode_leaves_enable` | `false` | §24 EntryMode pullback-continuation leaves |
| `money_proxy_enable` | `true` | §70.1 composite money proxy |
| `narrative_class_enable` | `false` | §70.6/§70.8 narrative class + ceiling |
| `platform_lead_enable` | `false` | §70.7 platform-lead / crypto-social-lag |
| `deployer_screen_enable` | `false` | §70.9 deployer credibility screen |
| `fee_floor_enable` | `false` | §70.10 anti-bundle fee-floor veto |
| `probe_budget_enable` | `false` | §33 probe-budget sizing |
| `alpha_call_lane_enable` | `true` | §29 Discord paid-alpha lane (Wave-3 LAW D1) |
| `designated_caller_enable` | `true` | §29 designated-caller attention weight (Wave-3 LAW D2) |
| `alpha_exit_pressure_enable` | `true` | §29.5 bearish-alpha reduce-only exit pressure (Wave-3 LAW D3) |

Integers (`LAW_INT_DEFAULTS`):

| Config key | Default | Law |
|---|---|---|
| `universe_age_exempt_slots` | `64` | §21.5 fresh-launch age exemption |
| `bar_trades_per_bar` | `8` | §21.6 trades per structural bar |
| `structure_downtrend_haircut_bp` | `7000` | §21.6 downtrend structure haircut |
| `narrative_decay_bp` | `9330` | §29.6 attention/narrative decay rate |
| `narrative_decay_floor` | `4` | §29.6 decay floor |
| `promote_corroboration_quota` | `2` | §71 union-preservation corroboration slots |

---

## Invariants by class (what each test guards)

### Class 1 — determinism / replay (`tests/regression_determinism.rs`)
- **golden_tape_digest_is_byte_stable_across_replays** — an independent driver
  (`src/golden_tape.rs`, a verbatim mirror of `golden_digest.rs::drive`) reproduces
  the pinned digest + full outcome vector on every replay. A SECOND witness beyond
  `golden_digest.rs`, so a tape edit that changes behaviour fails HERE too.
- **causally_independent_ingest_reordering_is_report_invariant** — permutation-
  invariance of causally-independent ingest ordering (digest included).

### Class 2 — law-presence invariants
Config-identity + defaults (`tests/regression_laws.rs`):
- **every_law_toggle_default_is_pinned** — every `LAW_*_DEFAULTS` key still names a
  real field and holds its pinned default.
- **every_law_toggle_is_in_the_strategy_identity_seed** — flipping ANY toggle moves
  the golden digest (the law is still part of the §19 strategy identity).
- **derived_targets_reversal_moves_the_golden_net_off_the_fixed_ladder** — §24
  default-ON produces the derived net (`1864780`); OFF falls back to the forbidden
  fixed ladder (`1852159`), derived > fixed.
- **corroboration_quota_changes_golden_admissions_and_net** — §71 quota strictly
  out-earns its absence and changes admissions.
- **universe_screen_toggle_changes_the_filtered_count** — §21.5 age screen moves
  `universe_filtered`.
- **creator_dump_veto_toggle_gates_a_real_reject** — §26 pre-entry veto fires
  (reject 13) and gates admission.
- **fee_floor_veto_toggle_gates_a_real_reject** — §70.10 fee-floor veto fires
  (reject 14) and gates admission.

Library-surface laws (`tests/regression_laws_lib.rs`):
- **narrative_class_conditions_conviction_and_ceiling** — §70.6/§70.8 class orders
  conviction sizing (reduce-only) and reach ceiling.
- **composite_money_proxy_outscores_buy_pressure_alone** — §70.1 wallet/holder-led
  market outscores flat buy-pressure.
- **platform_lead_gives_a_mainstream_led_mint_more_runway** — §70.7 runway only
  under the armed toggle.
- **signal_horizon_rejects_class_mismatch_and_too_slow** — a class/lane mismatch is
  ClassForbidden and a slow feature is TooSlow; fast on-chain flow is admissible.
- **burst_climax_into_strength_exit_reason_exists** — §24(d) `IntoStrength` exit
  reason exists (code 9), is terminal, and the exit taxonomy has distinct codes.
- **promotion_blocker_consults_fdr_and_pbo_and_fails_closed** — §51 FDR + PBO gates
  are both consulted; an unmeasurable matrix fails closed.
- **quote_mint_sol_default_identity** — WSOL is the pinned default quote for both
  venues and the pool decoder returns the on-chain quote identity.

Record / report-plane laws (`tests/regression_records.rs`):
- **admitted_record_carries_well_ordered_band_and_provenance** — §34.4 DecisionRecord
  band (x_min ≤ x_cost ≤ x_max, size inside) + fail-rate / rt-cost provenance.
- **vetoes_and_haircuts_record_nondegenerate_convexity** — §49 counterfactual != realized.
- **post_exit_markouts_and_foregone_upside_present** — §47/§54 markout cells + foregone.
- **dead_mint_gets_terminal_label_at_versioned_delta_t** — §47a terminal label at δT.
- **setup_classifier_tags_nonzero_archetype_vs_all_zero_stub** — §25 ON tags a
  non-stub archetype; OFF is the all-0 stub.
- **discovery_lane_attribution_keeps_lanes_distinct** — §71.2 independent discovery
  lanes carry distinct realized net (the setup slot is their sum).

Execution-plane laws (`tests/regression_execution.rs`):
- **construction_gate_clears_faithful_build_across_every_venue_side** — §77/§113
  parity + round-trip pass for a faithful build.
- **construction_gate_rejects_a_tampered_instruction** — a byte-tampered ix fails
  fixture parity.
- **construction_strikes_trip_the_builder_quarantine_and_stick** — §78 quarantine
  trips at the 3-strike threshold, is sticky over success, clears only on a
  registry-version bump.
- **only_construction_class_failures_trigger_quarantine** — market-noise failures
  never quarantine.

### Class 3 — fail-closed invariants (`tests/regression_failclosed.rs`, `..._extra.rs`)
- **run_mode_has_only_paper_and_replay** — no `RunMode::Live` variant (compile-time
  exhaustive match; a new variant stops the crate compiling).
- **promotion_readiness_never_live_eligible_on_paper_run** — a paper/replay run is
  never live-probe-eligible and reports a stable blocker.
- **empty_source_evidence_classifies_insufficient_sample** — absent/thin source
  evidence stays INSUFFICIENT_SAMPLE.
- **thin_creator_evidence_classifies_unknown** — sub-gate creator history stays UNKNOWN.
- **watchlist_stays_within_capacity_under_flood** — §99 watchlist ≤ `64` under 8×.
- **attention_field_tracks_at_most_track_cap_mints** — §99 attention track cap.
- **live_chatter_breadth_is_bounded_and_deterministic** — §99 `LIVE_CHATTER_CAP` = `16`
  saturates.
- **phase_b_live_sim_rung_is_fail_closed** — the deferred live-state (sell-sim) rung
  never reports validated-live.
- **undecoded_pool_bytes_are_refused_not_fabricated** / **unknown_instruction_
  discriminator_is_refused** — quote/pool/instruction that does not decode ⇒ refusal.
- **order_has_no_bypass_constructor_and_mirrors_no_wallet** — no-copy-trade: `Order`
  has no bypass ctor and carries no source wallet; a gated stage refuses.

### Class 4 — decoder property / fuzz + arithmetic (`tests/regression_decoder_fuzz.rs`, `..._failclosed_extra.rs`, `..._manifest.rs`)
- **decode_*_never_panics** (pool / global-config / spl-amount / curve-tail / ix /
  event) — exhaustive truncation-at-every-offset + wrong-discriminator + hash-random
  corpus; never panics, always fail-closed.
- **buy_and_sell_ix_round_trip_over_hash_driven_args** — encode→decode identity over
  256 hash-driven arg pairs; truncation and wrong-discriminator refused.
- **pumpswap_amount_out_is_checked_and_monotone** / **pump_amount_out_never_panics_
  on_extremes** — money math is checked/widened: never panics, never wraps, monotone
  in input, refuses >100% fee and empty pools.
- **golden_net_arc_is_internally_consistent** — the golden re-pin history constants
  are internally consistent (arc ends at the live net; derived-vs-fixed margin closes).

### Class 5 — cross-crate smoke (in the binaries' own crates)
- `crates/pq-evaluator/tests/smoke.rs` — the §44/§62 evaluator binary runs a tiny
  JSONL fixture over stdin, exits 0, emits the graded-report keys, is deterministic,
  and fails non-zero on a bad tape.
- `crates/pq-research-runner/tests/smoke.rs` — the §62 experiment-run binary runs a
  sealed-experiment fixture, exits 0, emits the ablation/baseline keys, is
  deterministic, and fails non-zero on a bad experiment.
  (These live with their binaries because only the defining crate's tests receive
  `CARGO_BIN_EXE_*`.)

### Class 6 — manifest consistency (`tests/regression_manifest.rs`)
- **manifest_mirrors_every_pinned_baseline** / **manifest_lists_every_law_toggle_key**
  — this file and `src/baselines.rs` never drift apart.
- **dossier_file_corpus_is_intact** — `191` `dossier_*.rs` files across the workspace.

---

## Updating a baseline on an intentional re-pin
A change here is only ever legitimate as a DELIBERATE, operator-approved re-pin
(a law reversal, a golden-tape re-pin). When that happens:
1. update the number in exactly one code place — `src/baselines.rs`;
2. mirror the same edit into this file;
3. re-sync the verbatim tape in `src/golden_tape.rs` if `golden_digest.rs::drive`
   changed;
4. let `tests/regression_manifest.rs` re-lock the new values.

Never edit a magic number inside a test body — the tests read `baselines`.
