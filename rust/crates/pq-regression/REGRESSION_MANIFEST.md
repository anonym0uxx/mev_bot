# REGRESSION MANIFEST — `pq-regression`

The single human-facing register of every regression invariant this crate guards,
what each protects, and the pinned value / expectation. The machine-readable twin
is [`src/baselines.rs`](src/baselines.rs); `tests/regression_manifest.rs` asserts
this file and that module never drift apart (a re-pin must edit BOTH).

The crate ADDS coverage only. It changes no behaviour, pins nothing new about the
engine, and never edits a `dossier_*` file, a golden digest, or a pinned constant.
Everything here is integer / deterministic, fast (< 30 s), and needs no network,
no wall-clock, and no RNG (§22).

At the pinned green HEAD: `cargo test --workspace` = **all passed / 0 failed**,
golden digest `16527720425687282225` (net `31465931`), **191** dossiers intact.

### Brain representation state (no baseline of its own — recorded so a future re-pin knows where it started)

The episodic fingerprint is at **schema 2**: `SIGNATURE_BITS = 104`
(`EPISODE_SCHEMA_VERSION = 2`), schema 1's 99 bits plus the 5-bit
`F_HOLDER_GROWTH_VELOCITY` thermometer — the first derivative of holder growth,
adopted against the pre-registered laws V1/V2/V3 in
`pump-quant-brain/tests/schema2_information_gain.rs`. 24 of the `u128`'s bits
remain free for a schema 3.

Holder **concentration** deliberately spends **none** of them. It rides as a
§21.7 parallel stream on `EpisodeContext` and enters recall only through the
`RecallFilter` key, which is not the signature — so the band costs zero signature
bits, forces no migration, and can carry a first-class `Unknown` that the filter
never matches on. Neither the stream nor the velocity field moves the golden
digest: both are report/representation plane, and
`golden_digest.rs::the_concentration_parallel_stream_is_decision_inert_on_this_tape`
is the exercised-vs-not A/B that proves it.

**Measured caveat (re-pin #21, recorded so it is not rediscovered):** the schema-2
information gain does **not** currently reach the decision plane in lamports, and
that is measured rather than argued.
`pump-quant-app/tests/law_permutation_sweep.rs::law_b3_under_schema_one_versus_schema_two`
censuses the `F_HOLDER_GROWTH_VELOCITY` bucket over every episode the engine seals
on all ten tapes: on seven of the ten it takes a **single** value (the neutral
rung), so it contributes exactly `0` to every Hamming and every weighted distance
and the schema-2 index ranks **identically** to schema 1 there. Replaying LAW B3's
own hazard tape under both representations yields `0` differing verdicts out of 48
queries and the identical `+391932566` lamports. Separately, the concentration-band
conditioner is structurally **not on LAW B3's path** — `BrainPlane::recall`, the only
call `size_verdict` makes, is deliberately unconditioned; the band reaches a decision
only through `conditioned_classes` → `brain_analysis::lane_decay` → LAW B7, which is
OFF. So the schema-2 wave's IQR gain is real as an information claim and currently
worth **zero lamports** on every tape this repo can drive.

---

## Pinned baselines (mirror of `src/baselines.rs`)

### Golden determinism fingerprint + outcome vector

| Baseline | Value | Guards |
|---|---|---|
| `GOLDEN_DIGEST` | `3203929616839788134` (hex `0x2c76a502e916c666`) | byte-exact decision-journal digest of the golden tape under `Config::dev_portable` — the primary determinism fingerprint (§22/§54) |
| `GOLDEN_NET_LAMPORTS` | `31465931` | realized §24-compliant cost-derived net-SOL on the golden tape |
| `GOLDEN_PROMOTED` | `504` | candidates promoted to the gate |
| `GOLDEN_ADMITTED` | `11` | candidates admitted by the gate |
| `GOLDEN_REJECTED` | `493` | candidates rejected by the gate — re-pin #31 accounting-identity fix |
| `GOLDEN_UNIVERSE_FILTERED` | `72` | §21.5 universe-screen removals (zombie cohort) |
| `GOLDEN_NET_FIXED_LADDER` | `31390194` | net with the §24 cost-derived ladder DISABLED (forbidden fixed 13500/25000/50000 ladder). Re-measured at re-pin #26 under the unified cost model (was `7327315` at re-pin #24, `15055700` on the fictional tape) |
| `GOLDEN_DERIVED_MINUS_FIXED` | `75737` | **NEGATIVE since re-pin #26 — the fixed ladder now out-earns cost-derived on this tape by 191450 lamports (1.1% of the book, on 12 trades in 4 markets).** The §24 reversal STANDS regardless: re-pin #12 ruled that fixed global TP constants are FORBIDDEN as the live default *whatever this tape's net*, and it anticipated exactly this case. The retired claim that derived also earns more (`+12620` → `+355101` → `+797253`) is withdrawn, not re-argued |

**Golden net re-pin arc** (`GOLDEN_NET_ARC`, mirrored from `golden_digest.rs`):
`2979624 → 5017234 → 6443936 → 8785954 → 12550767 → 3831945 → 1406102 → 1864780 → 8124568 → 16778896 → 31111528 → 30889282 → 31465931`.
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
| `holder_concentration_enable` | `false` | §21.7/§70.1 holder distribution-shape law (top-10 / early-top-10 / whale-dominance concentration, bundle+sniper cohort, bump/wash flip ratio) feeding the formerly-dormant §21.5 concentration screen, a reduce-only sizing haircut, a CONJUNCTIVE pre-entry refusal (reject code 17) and the §21.7 authenticity multiplier — OFF, its pre-registered two-sided A/B earned `+84996098` on the happy tape against a `>100000000` bar and lost `-61154566` on the mirror (asymmetry `1.39x` against a `3x` bar): it failed both legs (re-pin #20) |
| `money_proxy_holder_flow_enable` | `false` | §70.1 holder term sourced from the continuous holder ledger (`holder_flow`) instead of the `unique_buyers` bitset popcount — OFF, its two-sided A/B measured exactly zero lamports (re-pin #19) |
| `narrative_class_enable` | `false` | §70.6/§70.8 narrative class + ceiling |
| `platform_lead_enable` | `false` | §70.7 platform-lead / crypto-social-lag |
| `deployer_screen_enable` | `false` | §70.9 deployer credibility screen |
| `fee_floor_enable` | `false` | §70.10 anti-bundle fee-floor veto |
| `probe_budget_enable` | `false` | §33 probe-budget sizing |
| `alpha_call_lane_enable` | `true` | §29 Discord paid-alpha lane (Wave-3 LAW D1) |
| `designated_caller_enable` | `true` | §29 designated-caller attention weight (Wave-3 LAW D2) |
| `alpha_exit_pressure_enable` | `true` | §29.5 bearish-alpha reduce-only exit pressure (Wave-3 LAW D3) |
| `brain_enable` | `true` | episodic recall memory plane (LAWs B1/B2/B5) — record + readout, decision-inert |
| `brain_haircut_enable` | `true` | §29.5/§46 reduce-only recall haircut/veto (LAW B3) — ARMED since re-pin #21. The 2^3 law sweep (`law_permutation_sweep.rs`, 8 configurations x 10 tapes) makes B3-alone the unique configuration clearing a pre-registered rule: `+296536625` on a three-hazard union tape (bar `100000000`), a WORST delta of exactly `0` across all nine hazard tapes, `+391932566` on its own hazard tape against a NEGATIVE loss on a maximal false-positive mirror, and EXACT neutrality on the golden tape (digest-only re-pin) |
| `brain_persist_enable` | `false` | LAW B5 durable episodic journal — operator opt-in (needs `brain_path`) |
| `brain_analysis_enable` | `true` | LAW B6 `brain_analysis_v1` strategy-analysis export — report-plane and decision-inert, so ON; `brain_analysis_path` is empty by default so nothing is written until an operator names a sink |
| `brain_reflect_enable` | `false` | LAW B7 reduce-only brain-informed lane downweight — A/B EXACTLY neutral (delta 0 net) on the golden tape and on a purpose-built decayed-lane tape at every step up to the full §56.2 envelope, so it ships OFF |

Integers (`LAW_INT_DEFAULTS`):

| Config key | Default | Law |
|---|---|---|
| `universe_age_exempt_slots` | `64` | §21.5 fresh-launch age exemption |
| `bar_trades_per_bar` | `8` | §21.6 trades per structural bar |
| `structure_downtrend_haircut_bp` | `7000` | §21.6 downtrend structure haircut |
| `narrative_decay_bp` | `9330` | §29.6 attention/narrative decay rate |
| `narrative_decay_floor` | `4` | §29.6 decay floor |
| `promote_corroboration_quota` | `2` | §71 union-preservation corroboration slots |
| `brain_min_sample` | `8` | §46 LAW B4 sample floor below which recall is structurally `Unknown` |
| `brain_recall_max_distance` | `8` | §102 LAW B3 similarity radius defining "a setup like this". Re-pin #17 narrowed this 12 → 8: the OFI and CVD ladders span 6 buckets each, so at 12 a maximally net-BUYING setup matched a maximally net-SELLING one |
| `meta_taxonomy_version` | `1` | §21.4 / criterion 81 taxonomy version stamped on category assignments. Re-pin #17 bumped this 0 → 1 (word-boundary needle matching fixes six proven v0 substring mis-assignments); v0 stays frozen as the historical record and a v0-stamped assignment is left UNKNOWN, never retroactively remapped |
| `brain_haircut_win_rate_bp` | `3500` | §29.5 LAW B3 haircut bar (decisive win rate) |
| `brain_veto_win_rate_bp` | `1500` | §29.5 LAW B3 veto bar (decisive win rate) |
| `brain_haircut_mult_bp` | `5000` | §29.5/§56.2 LAW B3 reduce-only size factor (≤ 10000 by validation) |

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
  default-ON produces the derived net (`31465931`); OFF falls back to the forbidden
  fixed ladder (`16970346`). Since re-pin #26 fixed is the LARGER of the two by
  `191450`; the test pins both nets and that they DIFFER (the wiring is live), and no
  longer asserts an ordering it cannot support.
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
