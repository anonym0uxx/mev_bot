//! Golden determinism regression: a rich multi-lane scenario whose decision-journal
//! digest and report must never change under behaviour-preserving optimization.
//!
//! Exercises all four lanes, on-chain confirms, capacity eviction (mints ≫ capacity),
//! recency pruning, promotion, gating/scalping, and the reflection cadence — the full
//! `evaluate()` surface — over many ticks, then pins the byte-exact outcome.

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::AppEvent;

mod tape_golden;
use tape_golden::*;

/// The byte-exact outcome of [`drive`], frozen. **Deliberately re-pinned twice**:
/// first when the engine moved from a one-shot fill to the held-position exit
/// lifecycle (§24; prior pin 13612654632551201076, admitted 16, net 2_979_624),
/// then when Batch C landed dynamic bankroll sizing + risk budgets (§33 — sizes now
/// derive from deployable capital under a 3-position concurrency cap, and refused
/// admits are JOURNALED, hence rejected 570), evidence-staleness gates (§34.3),
/// VPIN-X toxicity + Roll-regime multipliers, and the exit-reason field in every
/// Filled journal record. Fewer, properly-sized positions now net MORE
/// (+5_017_234 vs +2_979_624). From here this value is again the frozen tripwire —
/// any future move that is not a deliberate re-pin is a regression (§22, §54).
// Re-pin #3 (ledger-closure batch): config-hash-seeded digest, §23 expected-net
// arbitration, §33 probe→confirm→scale, §32 thesis exits, §21.7 authenticity +
// phase exit-cost law, §27 creator credibility, §21.3 regime consumption. Three
// concentrated admits netted +6_443_936 on the original 512-mint tape.
// Re-pin #4 (ledger-refinement batch): the TAPE ITSELF was extended (as when the
// exit lifecycle landed) with three cohorts exercising the §21.5 universe screen
// (zombie markets — filtered, never entered), §21.6 trade-count bars + swing
// structure (dense fresh launches on cheaper venues — they out-price the toy
// pools in §23 arbitration, §18), and §29.6 attention decay (stale narrative
// blasts fade instead of squatting rank). Net on the extended tape:
// +8_785_954 (arc: 2_979_624 → 5_017_234 → 6_443_936 → 8_785_954). The per-law
// causal deltas are pinned separately in `batch_e_laws.rs` A/B tests — each law
// strictly out-earns its own absence on its hazard tape.
// Re-pin #6 (Twitch/live-stream batch): the tape gained a live-stream cohort —
// a coin whose on-chain flow sits BELOW the numeric discovery bar but which is
// being watched on stream (broadcaster call + snowballing distinct chat, fed
// through ingest_social with the capture lane's exact NDJSON) — and the §71
// union-preservation quota landed: building the Twitch lane exposed that raw
// rank let numeric scores (~10^5) monopolize every promotion slot over the
// fade-capped (§29, ≤10^3) corroboration lanes, a de-facto intersection. With
// 2 of 8 slots reserved for gate-viable corroboration evidence, the streamed
// coin is discovered, admitted, and rides: net 8_785_954 → **12_550_767**
// (arc: 2_979_624 → 5_017_234 → 6_443_936 → 8_785_954 → 12_550_767). The
// quota's causal delta is pinned by `corroboration_quota_earns_on_this_tape`
// below; the twitch-vs-x arms tie BY DESIGN (§29.8: no per-platform trust in
// the quality path) — the Twitch-specific laws are pinned in attention/e2e.
// Re-pin #5 (Phase-A alignment batch): SEED-ONLY re-pin — every decision-level
// constant below is UNCHANGED (same promoted/admitted/rejected/net on the same
// tape). The digest moved solely because the §19 config-identity seed gained
// the new integrity keys (scale_confirm_auth_min_bp, expectancy_min_lane_trades)
// while the fail-open holes they close (neutral-prior scale-in, stale numeric
// snapshots, unknown exit cost, uncross-checked confirm depth) were repaired
// without changing any golden decision — the tape never exercised the holes.
// Re-pin #7 (Wave-2 §26 confirmed-creator-dump reversal): SEED-ONLY re-pin —
// every decision-level constant below is UNCHANGED (same promoted/admitted/
// rejected/net/universe_filtered on the same tape). The digest moved solely
// because the §19 config-identity seed gained the operator-approved §26 keys
// (creator_dump_veto_enable / _bp / _strict_bp). The golden tape emits no
// CreatorAction events, so the confirmed-dump hazard the reversal targets is
// never present here and no golden decision changes; the §26 law's causal
// effect is proven on its own hazard tape in `audit_wave2_laws.rs`.
// Re-pin #8 (Batch-2a exit/sizing mechanics — §24 cost-derived profit targets,
// §24(d) exit-into-strength, §24 volatility-scaled stops/trail): SEED-ONLY re-pin —
// every decision-level constant below is UNCHANGED (same promoted 504 / admitted 17
// / rejected 487 / net 12_550_767 / universe_filtered 72 on the same tape). The
// digest moved solely because the §19 config-identity seed gained the eight
// Batch-2a keys (derived_targets_enable / target_margin_mult_bp / target_floor_bp /
// target_ceiling_bp / into_strength_exit_enable / into_strength_climax_bp /
// vol_stop_enable / vol_stop_scale_bp), all DEFAULT-OFF so no golden decision
// changes. Each law's causal effect is proven on its own hazard tape in
// audit_wave2_laws.rs; the shadow tournament gained two report-only challenger
// arms (exit-into-strength, vol-stop) that never touch capital or the journal.
// Re-pin #9 (Batch-2b — §71.2 discovery-lane attribution, §25 archetype
// classifier, §24 EntryMode leaves): SEED-ONLY re-pin — every decision-level
// constant below is UNCHANGED (same promoted 504 / admitted 17 / rejected 487 /
// net 12_550_767 / universe_filtered 72, same per-lane net on the same tape). The
// digest moved solely because the §19 config-identity seed gained two Batch-2b
// keys (setup_classifier_enable — default ON, and entry_mode_leaves_enable —
// default OFF). LAW 3 (discovery-lane attribution) adds NO config key and is a
// pure attribution correction — the disc_perf ledger is inert on this tape
// because no creation-sighting and social-caller (nor narrative and attention-
// velocity) both open on the golden tape, so the archetype-keyed and lane-keyed
// ledgers coincide here. LAW 4's classified archetype tags the thesis/MFE/reject
// samples but arbitration, the gate, and exits never read it, so no golden
// decision changes. LAW 11 is default-OFF (byte-identical to the 4-lane gate).
// Each law's causal effect is proven on its own hazard tape in audit_wave2_laws.rs.
// Re-pin #10 (Batch-2c — §70.1 money proxy, §70.6/§70.8 narrative class,
// §70.7 platform-lead, §70.9/§70.10 deployer/fee-floor): every decision-level
// constant below is UNCHANGED (same promoted 504 / admitted 17 / rejected 487 /
// net 12_550_767 / universe_filtered 72 on the same tape). LAW 7 (§70.1 composite
// money proxy) is DEFAULT ON — a legitimate lamports-moving law — but on THIS tape
// it is decision-neutral: the composite money level (smart-wallet entry + holder
// growth folded ahead of buy-pressure) changes the attention field's internal
// divergence scoring but crosses no promotion/admission/exit threshold here, so
// the realized net is byte-for-byte 12_550_767 (delta 0). Its causal lamports
// effect on a wallet-led market is pinned in audit_wave2_laws.rs. LAWs 8/9/10 are
// DEFAULT OFF (new scoring/sizing or protective behaviours, report-only until an
// operator flips them — Batch-2a precedent), so they touch no golden decision.
// The digest moved solely because the §19 config-identity seed gained the five
// Batch-2c keys (money_proxy_enable / narrative_class_enable / platform_lead_enable
// / deployer_screen_enable / fee_floor_enable) plus LAW 7's decision-neutral
// score change. Each law's causal effect is proven on its own hazard tape.
// Re-pin #11 (Batch-2d — records + report-plane, LAWs 12–21): every decision-level
// constant below is UNCHANGED (same promoted 504 / admitted 17 / rejected 487 /
// net 12_550_767 / universe_filtered 72 on the same tape, same per-lane net). The
// digest moved for TWO record/seed-level reasons, neither a decision change:
// (1) LAW 12 (§34.4 DecisionRecord completeness) extended the Admitted journal
// record with the size band (x_min/x_cost/x_max), the attempt/fail-rate multiplier,
// and the round-trip impact provenance — a REAL journal-encoding change folded over
// all 17 admits (record completeness, not a different decision); (2) LAW 13's new
// §19 config-identity key (probe_budget_enable, DEFAULT OFF) moved the seed. LAW 13
// opened no probe on this tape (the golden bankroll never sizes below x_min), so no
// count moved; its A/B is pinned in batch_2d_laws.rs. LAWs 14/16/19/20/21 are
// report-plane/additive with NO config key and are not on the golden Report path
// (promotion/baseline/feature-admission/ablation/live-status are separate report
// surfaces), so they touch no golden decision. LAWs 15/17/18 write only the
// report-only analytics rings (convexity enrichment, post-exit markouts, terminal-
// state reflections) which never enter the journal digest or the Report counts.
// Each law's effect is proven by its own test.
// Re-pin #12 (§24 defect-#3 reversal GOING LIVE — "constitution wins"): this is a
// REAL decision-level re-pin, NOT seed-only. The operator ruled that fixed global
// TP constants (13_500/25_000/50_000) are FORBIDDEN as the live default; cost-
// derived profit targets MUST be THE behaviour. Accordingly LAW 2
// (derived_targets_enable) is flipped to DEFAULT ON in Config::dev_portable, so all
// admits on the golden tape now exit on cost-derived rungs (per-market tp1/tp2/tp3
// from the gate's measured round_trip_cost_bps + margin, tranche count from
// exit_ladder::ladder_rungs) instead of the forbidden fixed ladder. Counts and net
// MOVE as a result: admitted 17 → 16, rejected 487 → 476, promoted 504 and
// universe_filtered 72 UNCHANGED, and net 12_550_767 → 3_831_945 (a SIGNED delta of
// −8_718_822 — the reversal net-moves DOWN on THIS tape). This is measured, honest
// lamports: the golden tape's grind-then-crater waves reward the fixed ladder's
// aggressive 13_500/25_000/50_000 rungs, so pricing exits off each market's true
// round-trip cost banks smaller-but-principled tranches here; the constitution
// forbids the fixed constants as the live default regardless of this tape's net, and
// LAW 2's causal out-performance on its OWN hazard tape (a low-cost grind that never
// reaches the fixed rungs) is still proven in audit_wave2_laws.rs. LAWs 5/6 and the
// other situational/protective laws remain DEFAULT OFF per golden-arc discipline —
// only this mandated reversal goes ON. (arc: 2_979_624 → 5_017_234 → 6_443_936 →
// 8_785_954 → 12_550_767 → 3_831_945.)
// Re-pin #13 (COST-REALISTIC TAPE — the golden tape made REPRESENTATIVE): a REAL
// decision-level re-pin, NOT seed-only. The operator kept the §24 reversal live
// (LAW 2 still DEFAULT ON) and ruled that the tape itself must be REPRESENTATIVE
// rather than pathologically rewarding the forbidden fixed ladder. The pathology
// (diagnosed): (a) `dev_portable` modelled a ~150–190 bps round-trip — far too
// cheap — which collapsed the cost-derived first rung to ~1.02× and never let it
// bank a real move; and (b) EVERY one of the 512 markets followed the SAME wave
// grinding to 1.5×–2× then cratering, so a single fixed 13_500/25_000/50_000
// ladder structurally out-earned any cost-priced exit. Fixed beat derived here for
// tape-shape reasons, not merit. Two corrections, both integer/deterministic (§22):
//   1. COSTS. `drive` now models a realistic low-cap Solana memecoin scalp
//      round-trip (docs/PUMPSWAP_DECODE.md dynamic tiered fees): protocol/fee/spread
//      450 bps + fixed priority/tip 200k lamports + impact — a realized ~650–760 bps
//      round-trip. Cost-derived rungs are now sensibly sized (tp1≈1.16×, tp2≈1.32×,
//      tp3≈1.49×).
//   2. DISTRIBUTION. Each market now follows its OWN deterministic trajectory
//      (`main_scalp`) drawn from a realistic outcome mix — ~42% quick losers, ~42%
//      small→mid winners straddling the derived (1.16×) and old fixed (1.35×) first
//      rung, ~16% moderate winners/runners (right tail BOUNDED at ~2.2× so the
//      reference stays stable) — with a generic early-launch phase (discovery cannot
//      front-run the outcome) and staggered launches (each coin discovered once, no
//      re-admission churn bleeding the 7% cost). The special-purpose cohorts are held
//      controlled so the BROAD distribution drives the net; the streamed coin stays
//      the one genuine runner (§71).
// Result on the now-representative tape: promoted 504 (UNCHANGED), universe_filtered
// 72 (UNCHANGED), admitted 16 → 14, rejected 476 → 467, net 3_831_945 → 1_406_102.
// This net is the HONEST cost-derived (§24-compliant) result on realistic economics.
// Crucially the tape NO LONGER favours the forbidden ladder: on this exact tape the
// cost-derived default nets 1_406_102 vs 1_393_482 for the fixed ladder — derived
// now marginally OUT-earns fixed (+12_620), the pathology inverted. Per-lane the
// ActiveMarketScalp lane is honestly slightly negative (−1_656_517: realistic 7%
// costs on many small clips) while CreationSniper (+3_062_619) carries the net.
// Reference comparison: 12_550_767 was fixed-ladder-on-the-UNREALISTIC-tape and
// 3_831_945 was derived-on-the-UNREALISTIC-tape; both were tape artifacts. 1_406_102
// is derived-on-the-REPRESENTATIVE-tape and is the trustworthy reference.
// (arc: 2_979_624 → 5_017_234 → 6_443_936 → 8_785_954 → 12_550_767 → 3_831_945 →
// 1_406_102.)
// Re-pin #14 (Wave-3 §29 Discord paid-alpha lane — LAWs D1–D5): a REAL
// decision-level re-pin, NOT seed-only. The TAPE gained a Discord paid-alpha
// cohort: a DESIGNATED caller in a PAID room calls a mint EARLY (the new
// `DiscoveryLane::AlphaCall`, index 5, discovers it — distinct from the open
// social-caller firehose, §71 reflection integrity), and the mint then earns an
// on-chain confirm + real near-balanced microstructure (numeric-lane quiet, so
// the AlphaCall corroboration provenance is KEPT) and PASSES the gate — alpha
// ACCELERATED a real setup. A second mint the same room calls has NO on-chain
// support and is correctly NEVER admitted (LAW D4: alpha alone can never admit).
// The alpha winner is a MODEST winner (peak ≈ +30%, then a round-5 sell-off) held
// deliberately BELOW the forbidden fixed +35% (13_500) first rung, so it does NOT
// re-introduce a big runner that would reward the forbidden fixed ladder — the
// re-pin #13 representativeness (derived out-earns fixed) is preserved (the
// pq-regression mirror still measures derived − fixed = +12_621 > 0). Counts/net
// MOVE: admitted 14 → 18 (the alpha winner opens via the §71 corroboration quota,
// plus a few re-admits as it rides), rejected 467 → 486 (the no-confirm alpha mint
// is promoted then gate-rejected for want of an on-chain confirm across the early
// rounds — LAW D4 exercised on the tape), promoted 504 and universe_filtered 72
// UNCHANGED, and net 1_406_102 → 1_864_780 (a SIGNED delta of +458_678 — the paid
// room's modest winner, admitted and ridden, is honest lamports). Its realized net
// attributes to the AlphaCall discovery lane (856_711) AND the room's §29.8 outcome
// ledger (LAW D5: per_alpha_source = [(Discord room, 856_711)]) — the CreationSniper
// setup lane is now the SUM of AlphaCall + SocialCaller + OnchainCreation, the exact
// §71.2 split the AlphaCall lane exists to make. LAWs D1/D2/D3 are DEFAULT ON (D1
// attribution changes no capital decision — the discovery lane never ranks; D2
// designated-caller weight helps the paid call rank; D3 bearish-alpha exit pressure
// is inert here — the golden cohort has no bearish held case). Each law's isolated
// causal effect is proven on its own hazard tape in alpha_laws.rs.
// (arc: … → 1_406_102 → 1_864_780.)
// Re-pin #15 (0.1-SOL OPERATOR FLOOR + small-bankroll Kelly recalibration —
// criterion 112 / Amendment A-6): a REAL decision-level re-pin, NOT seed-only. The
// operator directed an ABSOLUTE minimum trade size of 0.1 SOL on EVERY individual
// order (entry, each probe, each probe→confirm→scale-in add) and a Kelly/bankroll
// recalibration so a small 2-SOL bankroll actually trades at-or-above the floor
// instead of being blocked. Two coupled changes land here:
//   1. FLOOR. A new `min_trade_size_lamports` config (default 100_000_000 = 0.1 SOL)
//      lifts the economic band's `x_min` to `max(0.1 SOL, x_min)` (via the additive
//      `economic_gate::floor_size_band`, applied in `gate::decide` ABOVE the dossier-
//      locked `size_band` leaf). A risk/Kelly-arbitrated size below the floor is
//      CLAMPED UP to it when the hard caps allow, else REFUSED; a market too thin to
//      take a 0.1-SOL clip (`x_min > x_max`) refuses. Every emitted bite — probe and
//      scale-in add — is ≥ 0.1 SOL (`open_pending` folds a sub-floor scale remainder
//      into the initial bite). The sub-`x_min` paid-information probe (LAW 13) is a
//      SUB-FLOOR bet, so it is switched OFF while the floor is active.
//   2. RECALIBRATION. `Config::dev_portable` moves floor_fraction_bps 5_000→2_500
//      (survival floor max(0.5 SOL, 25%×2 SOL)=0.5 SOL ⇒ deployable 1.5 SOL),
//      f_base_bp 150→667 (base bite ≈0.1 SOL — the floor is the natural base bite),
//      total_risk_cap_bp 450→2_100 (fits 3 concurrent floor notionals + fees, ~0.303
//      SOL ≈ 15% of the 2-SOL bankroll on an all-positions rug), and
//      x_min_promote_cap_bp 400→800 (0.1 SOL = 6.67% of deployable, so the promote
//      cap MUST exceed that or the floor is unreachable — the key unblock).
// The 2-SOL bankroll now ADMITS (admitted 13 > 0) and NO order is below 0.1 SOL
// (pinned by the no-sub-floor invariant in sizing_floor_laws.rs). Counts/net MOVE:
// admitted 18 → 13 (fewer, ~5× larger 0.1-SOL clips under the same 3-slot cap),
// rejected 486 → 457, promoted 504 and universe_filtered 72 UNCHANGED, and net
// 1_864_780 → 8_124_568 (a SIGNED delta of +13_546_021 — realistic 0.1-SOL clips
// bank far more per admit, and the lower fixed-cost fraction at larger size lifts
// per-trade efficiency). The paid-alpha cohort's round-5 settle was also lifted from
// a +10% near-round-trip (11_000) to a +20% consolidation plateau (12_000): at tiny
// pre-A-6 sizes the deep give-back only bled a marginal amount, but at realistic
// 0.1-SOL clips the re-admits into that fade turned the AlphaCall lane NEGATIVE
// (incoherent with LAW D1/D5 "the paid room earns its keep"). The +20% plateau — a
// coherent "called winner consolidates and holds", consistent with the tape's own
// `main_scalp` winner-settle model and the streamed runner's partial give-back —
// keeps the alpha winner a MODEST positive contributor (AlphaCall 447_700, close to
// its prior 856_711 role) WITHOUT reshaping it into a second runner, and its +30%
// peak still sits BELOW the forbidden fixed +35% rung so cost-derived STILL out-earns
// fixed (derived 8_124_568 − fixed 15_055_700 = +355_101 > 0). LAWs unchanged; the
// §71 quota and §24 reversal wiring are re-measured, not altered.
// (arc: … → 1_406_102 → 1_864_780 → 8_124_568.)
// Re-pin #16 (EPISODIC RECALL MEMORY — LAWs B1–B5, `pump-quant-brain` wired in):
// a SEED-ONLY re-pin. The engine gained an episodic memory plane: every completed
// trade seals an immutable `Episode` whose 20-field integer fingerprint is
// quantized from the state captured AT ADMIT (LAW B1 — a fingerprint computed at
// exit would be a function of the price path it is supposed to predict, so the
// capture point is the gate and a test pins that mutating the entire post-entry
// path leaves it byte-identical); the reflection cadence re-queries recall over the
// setup classes the engine ACTUALLY traded, conditioned by venue phase × meta
// category × discovery lane, and feeds the meta-lifecycle timeline and the social
// call/markout ledger (LAW B2); a REDUCE-ONLY recall haircut/veto can fade or refuse
// a historically-bleeding class (LAW B3, config-gated, DEFAULT OFF — see
// `brain_haircut_is_exactly_neutral_on_this_tape`); an `Unknown` verdict is
// structurally incapable of changing a decision (LAW B4, pinned, no toggle); and the
// journal survives a restart (LAW B5).
//
// EVERY decision-plane number here is UNCHANGED: net 8_124_568, promoted 504,
// admitted 13, rejected 457, universe_filtered 72, and every per-lane and
// per-discovery-lane net identical to re-pin #15. Only the DIGEST moves, and it
// moves for exactly one reason: §19 folds the whole `Config`'s strategy identity
// into the journal seed (`fnv1a_64(format!("{cfg:?}"))`), so ADDING the nine
// `brain_*` config fields necessarily re-seeds it. That is the identity law working
// as designed, not a behaviour change — `brain_plane_is_decision_inert_on_this_tape`
// and `brain_laws::b4_the_brain_plane_itself_is_decision_inert` prove the DECISION
// STREAM the digest is computed over is byte-identical with the plane on and off.
// LAW B3's causal value is proven on its own hazard tape in `brain_laws.rs`
// (+391_932_566 lamports of loss avoided), not here.
// (arc: … → 1_406_102 → 1_864_780 → 8_124_568 [net unchanged at re-pin #16].)
// Re-pin #17 (BRAIN INTEGRATION WAVE — measured gap leaves, the social abstraction
// plane, the social→on-chain hardening proofs, the meta-taxonomy v1 fix and the
// recall-radius retune): a SEED-ONLY re-pin. EVERY decision-plane number is
// UNCHANGED — net 8_124_568, promoted 504, admitted 13, rejected 457,
// universe_filtered 72, AlphaCall 447_700, and every per-lane / per-discovery-lane
// net identical to re-pin #16. Only the DIGEST moves, and it moves for exactly one
// reason: §19 folds the whole `Config`'s strategy identity into the journal seed
// (`fnv1a_64(format!("{cfg:?}"))`), so the two CONFIG VALUES this wave changed
// necessarily re-seed it:
//
//   1. `meta_taxonomy_version` 0 → 1. v0's naive substring matching mis-assigned
//      ordinary English into the brain's RECALL FILTER KEY — "Fair Launch"→AI (via
//      "ai" in "fair"), "Catalyst"→Animal, "Bottom Signal"→AI, "Bullish
//      Chain"→Animal, "Starter Pack"→Celebrity, "Magazine"→Political — pooling
//      tokens with the wrong meta's episodes and silently corrupting every
//      conditioned estimate keyed on them. `meta::TAXONOMY_V1` adopts the
//      word-boundary `MatchMode` discipline. The fix ships FORWARD under a bumped
//      version; v0 stays frozen and its six mis-assignments are PINNED as the
//      historical record (criterion 81 — assignments are timestamped and never
//      retroactive). The golden tape feeds no `TokenMetadata`, so no assignment on
//      it changes; only the config identity does.
//   2. `brain_recall_max_distance` 12 → 8. At radius 12 a maximally net-BUYING
//      setup matched a maximally net-SELLING one (the OFI and CVD ladders span 6
//      buckets each), and the engine formed an opinion on 144 of 245 admit-time
//      recalls from a 13-episode memory. At 8 that pairing is structurally
//      unreachable and admit-time recall correctly refuses on all 245 — the only
//      honest answer a 13-episode index with a sample floor of 8 can give. The
//      reflection pass still surfaces 3 conditioned setup classes. See
//      `brain::BRAIN_RECALL_MAX_DISTANCE_DEFAULT` for the full sweep.
//
// Everything else this wave added is REPORT-PLANE and never journaled: the four
// measured gap-leaf estimators (holder-growth acceleration, creator track record,
// meta lifecycle phase, narrative family — `measured_fingerprint.rs` proves each
// reachable AND decision-inert) and the social abstraction plane (trust, support,
// follow recommendation, style-lens scoreboard — `social_hardening.rs` proves the
// whole social plane cannot admit without on-chain confirmation, numeric
// microstructure and a passing economic gate, at ANY social strength).
// (arc: … → 1_406_102 → 1_864_780 → 8_124_568 [net unchanged at re-pins #16, #17].)
// Re-pin #18 (BRAIN → STRATEGY ANALYSIS — LAWs B6–B9): a SEED-ONLY re-pin. The
// brain stopped merely observing and started feeding the strategy-analysis loop:
//
//   * LAW B6 — `brain_analysis_v1`, a bounded, deterministic, integer-only export
//     written alongside `live_status.json` (same info-time discipline, never
//     wall-clock) plus `Engine::brain_analysis_json()`. Every array carries a
//     named cap and a documented total sort key, and an `unknown` verdict emits
//     its refusal reason with `null` in EVERY estimate field — a consumer cannot
//     read a number the brain refused to give (§46).
//   * LAW B6/§56 — `retirement_flags`: conditioned-negative setup classes, style
//     lenses, discovery lanes and alpha sources nominated for the weekly
//     governance review. A nomination is NOT a retirement: §51 FDR/PBO and §52
//     baselines remain the authority, and `brain_strategy::retirement_flags_
//     retire_nothing` pins that the flags retire nothing.
//   * LAW B7 — an optional, reduce-only, envelope-bounded brain downweight in
//     `reflect`. DEFAULT OFF: the A/B is exactly neutral (delta 0 net) on the
//     golden tape AND on a purpose-built decayed-lane tape, at every step from
//     250 bp to the full envelope. It did not earn, so it is not armed. (Re-tested
//     since, under a PRE-REGISTERED two-sided rule, on a tape where the mechanism
//     genuinely does act — `tests/brain_reflect_twosided.rs`. It still does not
//     earn: +26_697_249 on the true-positive tape against −21_009_674 on its
//     false-positive mirror, a 1.27x asymmetry versus the pre-registered 3x bar,
//     with the sign inverting across neighbouring market shapes. Golden-tape
//     neutrality is now asserted directly by
//     `b7_armed_reflection_is_exactly_neutral_on_this_tape` below. Default
//     unchanged, so this digest is unchanged.)
//   * LAW B8 — brain-grounded exit-challenger PROPOSALS derived from the recall
//     distribution of the setups that paid (median winner hold → time stop, p75
//     MFE → target, median winner heat → trail). Report-only, single-axis,
//     envelope-clamped, fail-closed at small n, never auto-adopted.
//   * LAW B9 — recall as an ADDITIONAL promotion blocker, consulted LAST so it can
//     never mask a §38/§51/§64 label, and one-directional so it can only ever
//     remove eligibility.
//
// EVERY decision-plane number is UNCHANGED — net 8_124_568, promoted 504,
// admitted 13, rejected 457, universe_filtered 72, AlphaCall 447_700, and every
// per-lane / per-discovery-lane net and final weight identical to re-pin #17. Only
// the DIGEST moves, and it moves for exactly one reason: §19 folds the whole
// `Config`'s strategy identity into the journal seed
// (`fnv1a_64(format!("{cfg:?}"))`), so ADDING the five config fields this wave
// needs — `brain_analysis_enable`, `brain_analysis_path`, `brain_reflect_enable`,
// `brain_reflect_step_bp`, `brain_decay_min_sample` — necessarily re-seeds it.
// That is the identity law working as designed. The three golden-tape conditioned
// setup classes are all POSITIVE (median +301_400, win rate 7_500 bp, n = 8 each),
// so LAW B7's lane-decay flag set is EMPTY on this tape even when armed, and the
// armed arm's net/admitted/promoted/rejected are byte-identical to the neutral
// arm's — measured, not assumed.
// (arc: … → 1_406_102 → 1_864_780 → 8_124_568 [net unchanged at re-pins #16, #17, #18, #19].)
// Re-pin #19 (§70.1 CONTINUOUS HOLDER ACCOUNTING): a SEED-ONLY re-pin. Holder-growth
// capture stopped being a seam nobody called and became a STREAM:
//
//   * `holder_flow.rs` — a per-mint `entity -> net base position` ledger folded from
//     OUR OWN decoded swaps (`buyer_entity` + `signed_base`), so the holder count is
//     canonical §6.1 evidence with zero added latency and no third-party dependency.
//     Birdeye/DAS holder counts stay strictly corroboration-tier (§6.6); the old
//     `Engine::observe_holder_count` seam is demoted to exactly that in its docs.
//   * The OBSERVATION-WINDOW LAW, enforced in the type: a mint watched from its
//     creation event is `Exact`, a mint discovered mid-life is `DeltaOnly`, an
//     over-cap ledger is `Incomplete`. `HolderReading`'s count is private and its
//     accessors are basis-gated, so a LEVEL consumer structurally cannot read a
//     delta-only or truncated count, while a GROWTH consumer (§70.1 wants a second
//     derivative) legitimately reads `DeltaOnly`. The `Exact` claim is falsifiable
//     by evidence — a sell from an untracked position proves a pre-window holder —
//     and the basis lattice only ever moves toward less confidence.
//   * WATCH: folded on every `MarketTrade` for every mint, admitted or not, and
//     sampled into `pump_quant_features::holder_growth` on a 3-tick (1.2 s) cadence
//     — the smallest whole-tick cadence at or above the estimator's 1 s minimum
//     interval, asserted at compile time. ANALYZE: the fingerprint's
//     `holder_growth_accel_bps` now receives a REAL measured value where before it
//     took the neutral rung on literally every admit. ENTER/HOLD: the open book's
//     holder trajectory rides out on `Report::holder_trajectory`.
//   * §3 — the money-proxy holder term (`money_proxy_holder_flow_enable`) replaces
//     the `unique_buyers` bitset popcount (which saturates at 64, collides on
//     `entity % 64`, and is MONOTONE NON-DECREASING, so it cannot see distribution
//     at all). DEFAULT OFF: it measures EXACTLY ZERO lamports on this tape and on
//     both sides of a purpose-built two-sided A/B, and `tests/holder_flow.rs`
//     establishes that the zero is an UNREACHABILITY result — the whole §70.1
//     composite money proxy is inert on those tapes — rather than an efficacy
//     result. It did not earn, so it is not armed.
//
// EVERY decision-plane number is UNCHANGED — net 8_124_568, promoted 504,
// admitted 13, rejected 457, universe_filtered 72, AlphaCall 447_700, and every
// per-lane / per-discovery-lane net and final weight identical to re-pin #18. Only
// the DIGEST moves, and it moves for exactly one reason: §19 folds the whole
// `Config`'s strategy identity into the journal seed, so ADDING the single config
// field this wave needs — `money_proxy_holder_flow_enable` — necessarily re-seeds
// it. Measured, not assumed: ablating the holder fold while KEEPING the config
// field reproduces this digest byte for byte, which isolates the drift to the seed.
// (arc: … → 1_406_102 → 1_864_780 → 8_124_568 [net unchanged at re-pins #16, #17, #18, #19].)
//
// ---------------------------------------------------------------------------
// RE-PIN #20 — §21.7/§70.1 holder DISTRIBUTION-SHAPE law (digest only).
// ---------------------------------------------------------------------------
// This wave derives the holder ledger's distribution SHAPE (`holder_concentration`):
// cumulative top-1/top-10 share, Herfindahl and its normalization, the arXiv
// 2512.00377 whale-dominance product, the MemeTrans (arXiv 2602.13480) first-ten-
// buyer cohort, the arXiv 2601.08641 bundle/sniper first-buy classification, and the
// bump/wash flip ratio — and wires it, reduce-only, to three consumers:
//   * the §21.5 active-market screen's `top_holder_concentration_bps`, which had been
//     a hard-coded `0` against a `u32::MAX` bar since inception (a screen that could
//     never bind because nothing produced the number);
//   * a sizing fragility haircut plus a CONJUNCTIVE pre-entry refusal (reject code 17
//     — §21.7 forbids this family from vetoing alone, so the refusal also requires an
//     independent flow-authenticity signature computed on QUOTE flow);
//   * the bundle/flip legs of the existing §21.7 authenticity multiplier (the single
//     authenticity entry point — never a second multiplier).
//
// DEFAULT OFF. `tests/holder_concentration.rs::ab_holder_concentration_two_sided`
// measures, on a purpose-built tape with real promotion/bankroll contention:
// HAPPY +84_996_098 lamports (bar: > 100_000_000) and MIRROR −61_154_566, an
// asymmetry of 1.39× against a pre-registered 3× bar. It failed both legs of its own
// pre-registered rule and is therefore not armed.
//
// EVERY decision-plane number is UNCHANGED — net 8_124_568, promoted 504,
// admitted 13, rejected 457, universe_filtered 72, AlphaCall 447_700, and every
// per-lane / per-discovery-lane net and final weight identical to re-pin #19. Only
// the DIGEST moves, and again for exactly one reason: §19 folds the whole `Config`'s
// strategy identity into the journal seed, so ADDING the single config field this
// wave needs — `holder_concentration_enable` — necessarily re-seeds it. Measured, not
// assumed: with the field present and the law ARMED the tape is still byte-identical
// on every counter (see `holder_concentration_is_exactly_neutral_on_this_tape`), and
// reverting each wired consumer in turn while keeping the config field reproduces
// this digest exactly — which isolates the drift to the seed.
// (arc: … → 1_864_780 → 8_124_568 [net unchanged at re-pins #16, #17, #18, #19, #20].)
//
// ---------------------------------------------------------------------------
// RE-PIN #21 — LAW B3 (`brain_haircut_enable`) ARMED BY DEFAULT (digest only).
// ---------------------------------------------------------------------------
// This is the first DEFAULT FLIP in the brain programme, and it is the OUTPUT of a
// pre-registered decision rule rather than an opinion. `tests/law_permutation_sweep.rs`
// measures ALL EIGHT combinations of the three previously-disarmed reduce-only laws
// {LAW B3, LAW B7, §21.7 concentration} on TEN tapes — the golden tape, both sides of
// each law's own two-sided pair, LAW B3's reduce-only winners control, and both sides
// of a UNION tape that concatenates all three hazard generators onto one engine — and
// evaluates a rule written into that file before any number in it was read.
//
// B3-alone is the UNIQUE configuration that clears every leg:
//   * P1 materiality: +296_536_625 lamports on the union tape, against a
//     100_000_000 (one 0.1-SOL bite) bar.
//   * P2 no harm: its WORST delta across all nine hazard tapes is exactly 0. LAW B3
//     does not cost a single lamport on any tape measured — including both sides of
//     LAW B7's pair and both sides of the concentration law's pair.
//   * P3 asymmetry: +391_932_566 on its own hazard tape against a NEGATIVE loss
//     (+350_288_025) on a purpose-added MAXIMAL false-positive mirror, in which the
//     flagged class's forward recurrences walk the healthy class's own 2.5× payoff
//     ladder. The 3× bar passes without the ratio being needed.
//   * P4 golden neutrality: EXACTLY neutral here, and that is why this is a
//     DIGEST-ONLY re-pin.
// Every other configuration fails: LAW B7 fails P1/P2/P3 (a reshuffle — +26_697_249
// happy vs −21_009_674 unhappy, 1.27×), and the concentration law fails P1/P3
// (+84_996_098 happy vs −61_154_566 mirror, 1.39×). Both stay OFF.
//
// EVERY decision-plane number is UNCHANGED — net 8_124_568, promoted 504,
// admitted 13, rejected 457, universe_filtered 72, AlphaCall 447_700, and every
// per-lane / per-discovery-lane net and final weight identical to re-pin #20. Only
// the DIGEST moves, and again for exactly one reason: §19 folds the whole `Config`'s
// strategy identity into the journal seed, so CHANGING the value of
// `brain_haircut_enable` necessarily re-seeds it. Measured, not assumed: the golden
// tape seals 13 episodes against a §46 sample floor of 8 at radius 8, so every
// admit-time recall here is `Unknown`, LAW B4 makes an `Unknown` a structural no-op,
// and `brain_haircut_is_exactly_neutral_on_this_tape` drives BOTH arms of the flag on
// the real golden tape to prove the counters are byte-identical.
// (arc: … → 1_864_780 → 8_124_568 [net unchanged at re-pins #16, #17, #18, #19, #20, #21].)
// RE-PIN #25 (2026-07-28) — SEED-ONLY. Six `Config` fields were ADDED for the
// operator target band and the per-candidate expected-move estimator:
// `mcap_band_enable` / `mcap_band_lo_lamports` / `mcap_band_hi_lamports` /
// `expected_move_model_enable` / `expected_move_min_sample` /
// `expected_move_prior_weight`. §19 folds `fnv1a_64(format!("{cfg:?}"))` into the
// journal seed, so ADDING a field re-seeds the digest with zero decision change.
//
// Both laws ship DISARMED and the expected-move table ships EMPTY, so every estimate
// refuses and `gate::decide` prices on `gate_expected_move_bps` exactly as before.
// EVERY decision-plane number is UNCHANGED — net 8_124_568, promoted 504, admitted 13,
// rejected 457, universe_filtered 72, and every per-lane / per-discovery-lane net and
// final weight identical to re-pin #24. Verified by `mcap_band_laws.rs` (P1) and
// `expected_move::tests::the_shipped_table_refuses_everywhere`.
// Study: `docs/BAND_THESIS_2026-07-28.md`.
// (arc: … → 1_864_780 → 8_124_568 [net unchanged at re-pins #16–#21, #25].)
//
// ---------------------------------------------------------------------------
// RE-PIN #26 (2026-07-28) — COST-MODEL UNIFICATION + HAZARD-TAPE DEPTH REALISM.
// ---------------------------------------------------------------------------
// A REAL decision-level re-pin, operator-authorised. The engine carried TWO
// disagreeing round-trip cost models — the gate's (~538 bps) and the lifecycle's
// (~420 bps) — and used one to DECIDE and the other to BOOK. `cost_model.rs` is now
// the single authority, wired into `gate::decide` and `HeldPosition::realize`. Four
// substantive corrections land together, and they must land together (see that
// module's header: two of them were nearly equal and opposite, and either alone
// moves the gate 200 bps in the wrong direction):
//
//   1. IMPACT IS DERIVED PER CANDIDATE. `impact_den_for(vsol) = vsol / 10_000` makes
//      the gate's linear impact model EXACTLY `curve_fill::own_impact_bps` for the
//      market in front of it. The static `cfg.gate_impact_den` is off the decision
//      path; a single denominator can only ever be right for one pool depth.
//   2. THE PHANTOM SPREAD IS GONE. `gate_protocol_bps` is `2 x venue_fee_bps_per_leg`
//      — 250 bps on the curve — and nothing else. The retired 450 contained ~200 bps
//      of "bid/ask spread"; a constant-product AMM has ONE reserve ratio and ONE
//      price, and the cost of crossing size is own impact, already charged.
//   3. ATA RENT IS PRICED. 2_039_280 lamports of rent-exempt deposit was priced
//      NOWHERE in the workspace — 203 bps of a floor clip. Under the lazy-hold /
//      close-on-full-exit policy a completed round trip's cash cost is one closing
//      signature, so the gate carries `ATA_CLOSE_LAMPORTS` and the lifecycle credits
//      the reclaim.
//   4. THE FIRST-SELL PENALTY IS DELETED. `first_sell_penalty_bps` (150 bps of
//      notional, once) was own-impact under another name, now charged exactly once
//      by `curve_fill`. `FIXED_LAMPORTS_PER_LEG` = 150_000 replaces both the gate's
//      200_000-a-round-trip and the lifecycle's 10_000-a-tranche (a 10x disagreement
//      about the price of the same signature).
//
// Counts/net MOVE: admitted 13 -> 12, rejected 457 -> 447, net 8_124_568 ->
// 16_778_896 (a SIGNED delta of +8_654_328), promoted 504 and universe_filtered 72
// UNCHANGED. The book roughly DOUBLES, and the direction is not a discount: the gate
// stopped charging 200 bps that the venue cannot charge and started charging own
// impact against each market's real reserve, so it admits fewer trades and prices
// the ones it takes correctly. Twelve admits in FOUR distinct markets (was 13 in
// five) — every statistical claim on this tape is still a statement about four hash
// draws (`edge_provenance.rs`).
//
// THREE FINDINGS THIS RE-PIN OVERTURNS, each stated here because each contradicts a
// published claim and none of them may be discovered twice:
//
//   (a) **THE BOOK IS STILL, IN MAJORITY, A BOUNDARY ARTIFACT.** Re-pin #24 recorded
//       +34_884_254 earned on the strategy's own triggers against -26_759_686 of
//       end-of-tape force closure: 77% of the book erased by where the fixture
//       stops. The doubling does NOT come from fixing that. Naturally-closed rises to
//       +42_058_785 and the forced subtotal barely moves (-25_279_889), so the
//       fraction goes 77% -> 60% and the caveat stands: this net is not quotable
//       without it (`edge_provenance.rs`). It is also still statistically
//       indistinguishable from zero — twelve trades, four markets, |t| ~ 0.19.
//   (b) **THE PAID DISCORD ROOM IS NET-POSITIVE AGAIN (+593_348).** Re-pin #24
//       asserted `alphacall_net < 0` and wrote that calling the lane "proven
//       positive" was FALSE. Under honest costs it is positive again. Both readings
//       were produced by a cost model, not by evidence about the room: the number
//       has now been negative under one and positive under two, on 12 trades in 4
//       markets. The correct statement is that this tape CANNOT settle whether a
//       paid room earns its subscription, and the assertion below no longer pins a
//       SIGN — it pins the VALUE and says so.
//   (c) **COST-DERIVED NO LONGER OUT-EARNS THE FIXED LADDER HERE.** Re-pins #13-#15
//       recorded derived - fixed = +12_620, then +355_101, then +797_253 at re-pin
//       #24. It is now -191_450: the forbidden fixed 13_500/25_000/50_000 ladder
//       nets 16_970_346 against cost-derived's 16_778_896. The §24 reversal STANDS
//       — re-pin #12 already ruled that fixed global TP constants are forbidden as
//       the live default REGARDLESS of this tape's net, and that ruling anticipated
//       exactly this case — but the supporting claim "and it earns more anyway" is
//       now false and has been removed rather than re-argued. A 1.1%-of-book
//       difference on 12 trades is noise either way; the reason cost-derived ships
//       is the constitution, not this number.
//
// The digest also necessarily moves: §19 folds `fnv1a_64(format!("{cfg:?}"))` into
// the journal seed and the decision stream itself changed.
//
// TWO LAW VERDICTS ARE NOW OPEN, AND NEITHER LAW IS ARMED BY THIS RE-PIN:
//
//   * **LAW B7 (`brain_reflect_enable`).** Re-pin #21 recorded B3-alone as the UNIQUE
//     configuration clearing the 2^3 sweep's pre-registered rule, with LAW B7 failing
//     at a 1.27x asymmetry. That was measured on a tape declaring 0.2 SOL pools; under
//     the derived impact model it admitted NOTHING and both arms read zero. At real
//     depth LAW B7's asymmetry is 5.78x (bar: 3x), the rule PASSES outright at every
//     step size above the shipped 250 bp, the reshuffle sign-inversion is gone on all
//     five market shapes, and `law_permutation_sweep.rs` now finds TWO winners —
//     {B3} and {B3, B7}. B7's MARGINAL union contribution over the shipped {B3} is
//     33_426_226, a third of the materiality bite, and arming a law whose verdict
//     moved because a FIXTURE was corrected is an operator decision. An A-11 study is
//     owed (`brain_reflect_twosided.rs`). LAW B3's own case is stronger than at re-pin
//     #21 (+414_992_045 on its hazard tape, worst hazard delta still exactly 0).
//   * **§32 flow persistence (`thesis_persist_obs`).** `k = 5` no longer turns the
//     golden book negative — because the book doubled, not because `k` improved. The
//     HARM is 11_469_573, against 11_347_743 before: 1.1% WORSE. It also now loses on
//     its OWN two-sided tape. It stays disarmed and the published study is corrected
//     (`docs/STRATEGY_PERMUTATION_STUDY_2026-07-25.md`, Erratum #2).
//
// The §21.7 concentration law still fails its rule (1.52x against a 3x bar) and stays
// OFF.
//
// TAPE DEPTH. The same change forced a fixture correction across the HAZARD tapes.
// Because the gate now reads each market's declared reserve, the sub-SOL depths the
// B7 / concentration / flow tapes carried (0.2-0.26 SOL against a 0.1 SOL clip)
// priced every candidate at thousands of bps of own impact and refused all of them:
// those tapes went VACUOUS (`admitted = 0`, both arms, delta 0) and could no longer
// arbitrate any law. They now declare real pump.fun depth (30-110 SOL) with their
// scenarios — bleeding cohort, concentration hazard, shakeout-then-run — unchanged.
// The golden tape needed no change; it was given real depth at re-pin #24.
// (arc: ... -> 1_864_780 -> 8_124_568 -> 16_778_896.)
// ============================================================================
// RE-PIN #27 (2026-07-28) — DEPTH AND MOVE PROVENANCE. A REAL decision-level re-pin,
// and the honest reading of it is NOT the number.
//
// Two quantities on the decision path could come from more than one place, and both
// travelled as bare integers with no record of which place:
//
//   * DEPTH. `Confirmation::sellable_depth_lamports` had three producers with three
//     meanings — an external `OnchainConfirm` assertion, a straight copy of
//     `Features::liquidity_lamports` (the VIRTUAL reserve) on the EntryMode paths, and
//     a hardcoded 0.2 SOL in two report harnesses. pump.fun seeds a curve with 30 SOL
//     of virtual reserve and ZERO real SOL, and escrows `real_sol = virtual_sol - 30
//     SOL` (`curve_state::real_sol_for`; the identity reproduces the venue's published
//     85.005 SOL graduation raise from first principles). Every fixture in this repo
//     was declaring a payout capacity ABOVE what its curve could hold — 30x at
//     vsol 31 SOL, unbounded at the 30 SOL seed, where a curve nobody has bought into
//     was credited with 29 SOL of sellable depth. `CurveDepth` now carries the reserve
//     AND its basis, `x_max` is capped by the PAYOUT reserve, and an inconsistent
//     decode is REFUSED rather than clamped.
//   * THE EXPECTED MOVE. Admission priced every candidate off the global
//     `gate_expected_move_bps` constant while §23 arbitration priced the same trade off
//     the lane's realized expectancy; once a lane cleared `expectancy_min_lane_trades`
//     the two diverged permanently and nothing recorded which had spoken. `PricedMove`
//     is now computed once, carries its `MoveSource`, and is journalled on every admit.
//
// THE NUMBERS, AND WHAT ACTUALLY MOVED THEM. Net 16_778_896 -> 31_111_528, admitted
// 12 -> 11, rejected 447 -> 448; promoted 504 and universe_filtered 72 unchanged.
//
// **NONE of that +85% comes from either correction.** Both were measured against this
// tape in isolation and both are decision-inert here:
//
//   * Removing the payout cap entirely (passing `u64::MAX` as `sellable_max`) leaves
//     net at 31_111_528 and admitted at 11. The cap NEVER binds on this tape: the
//     impact budget bounds `x_max` at 4.2-9.4 SOL against payout reserves of 0.5-37
//     SOL, so the impact bound is always the smaller.
//   * Reverting admission to the old `cfg.gate_expected_move_bps` constant likewise
//     leaves net at 31_111_528. No lane on this tape accumulates enough realized fills
//     to leave the cold-start prior before the tape ends.
//
// The whole delta is the confirmed-set EVICTION KEY. The §99 bound holds 256 markets;
// this tape presents ~268 confirmations. Eviction drops the market with the least
// sellable depth, which was the fixture's arbitrary `29 SOL + m` spread (ordered by
// mint index) and is now the truth (`real_sol`, ordered by curve progress). Restoring
// the old ORDER, with both corrections still in place, reproduces 16_778_896 and 12
// admits EXACTLY. A ~12-trade book in a handful of markets is dominated by which
// markets survive a capacity bound, and `edge_provenance.rs` already establishes that
// this book is statistically indistinguishable from zero. **The +85% is a tie-break
// readout. No claim may be built on it in either direction.**
//
// FIXTURES. Every tape declaring a sellable depth above `vsol - 30 SOL` was declaring
// a market that cannot exist, and all of them were corrected: the golden tape's
// confirms now carry the (virtual, real) pair its own trades imply, the unit fixtures'
// `REAL_CURVE_VSOL` became a curve with 0.3 SOL genuinely raised (own-impact on a
// floor clip unchanged at 33 bps a leg), and the two report harnesses' 0.2 SOL
// "pools" became real curves escrowing 0.2 SOL.
// (arc: ... -> 8_124_568 -> 16_778_896 -> 31_111_528 -> 30_889_282 -> 31_465_931.)
// Re-pin #28: TP1 + Thesis Invalidation lever tuning (target_floor_bp 11000→10300,
// target_margin_mult_bp 15000→5000, lc_tp1_bps 13500→10_500, lc_cvd_hold_frac_bps
// 4500→3000, lc_stall_ticks 25→75). Exit ladder now fires on observed ±4% micro-moves
// instead of being dead code for the $9k-$20k mcap band.
// Re-pin #30 (2026-08-09): §27/§28 amendment — Config struct grew new fields
// (tracked_wallet_boost_*, smart_money_boost_*) which change the Debug-format
// seed even though all boosts are DISABLED by default and decision logic is
// byte-identical. All other pinned values unchanged.
// 10_190_407_336_939_000_110 → 16_527_720_425_687_282_225 → 2_392_030_750_322_148_229.
// Re-pin #32: config struct Debug changed (6-revision fields, all OFF by default).
// Decision vector identical: promoted=504 admitted=11 rejected=493 net=31_465_931.
// Re-pin #33: config struct Debug changed (Rev-7 re-entry cooldown fields, OFF by
// default). §19 folds fnv1a_64(format!("{cfg:?}")) into the journal seed, so adding
// two fields re-seeds the digest with zero decision change. The cooldown feature
// ships DISARMED (reentry_cooldown_enable=false) and no position closes in the
// golden tape, so the cooldown set is never populated — every decision-plane number
// is byte-identical to re-pin #32.
// Re-pin #34 (2026-08-12): Rev-13 entry quality filter — config struct Debug
// changed (3 new Features fields: buy_ratio_bp, max_trade_lamports, trades_observed,
// plus 4 new Config fields: entry_quality_filter_*, all OFF by default). §19 folds
// fnv1a_64(format!("{cfg:?}")) into the journal seed, so adding these fields re-seeds
// the digest with zero decision change. The entry quality filter ships DISARMED
// (entry_quality_filter_enable=false). Decision vector identical:
// promoted=504 admitted=11 rejected=493 net=31_465_931 universe_filtered=72.
// 10_342_339_453_238_494_935 → 16_223_569_033_580_072_469.
const GOLDEN_DIGEST: u64 = 16_223_569_033_580_072_469;
// Re-pin #28: net changed 31_111_528 → 30_889_282 (exit ladder fires earlier on
// micro-moves; TP1 tranche recovers principal at +5% rather than holding to thesis
// invalidation). promoted/admitted/rejected/universe_filtered unchanged.
const GOLDEN_NET_LAMPORTS: i128 = 31_465_931;
const GOLDEN_PROMOTED: u64 = 504;
const GOLDEN_ADMITTED: u64 = 11;
const GOLDEN_REJECTED: u64 = 493;
/// Zombie-cohort promotions the §21.5 screen must remove (visible activity).
const GOLDEN_UNIVERSE_FILTERED: u64 = 72;
/// LAW D1/D5: the paid Discord room's realized net attributed to the AlphaCall
/// discovery lane (the modest winner it surfaced, admitted and ridden), keyed
/// distinctly from the open social-caller firehose.
///
/// **This number has now been positive, negative, and positive again, and NOT ONE of
/// those changes came from evidence about the room.** It was +447_700 while the
/// tape's pools were 0.12-0.47 SOL and our own impact went uncharged, -2_721_835 once
/// re-pin #24 gave the tape real depth and armed the curve fill, and +593_348 once
/// re-pin #26 unified the cost model. Three cost models, three signs, the same
/// events. It is pinned as a VALUE — a tripwire on the §71.2 attribution split — and
/// no claim about paid alpha rooms may be built on its sign in either direction.
const GOLDEN_ALPHACALL_NET: i64 = 891_331;

#[test]
fn golden_digest_is_stable() {
    let r = drive(Config::dev_portable());
    // Print for inspection (`cargo test -- --nocapture`).
    println!(
        "GOLDEN ticks={} promoted={} admitted={} rejected={} universe_filtered={} net={} digest={} per_lane={:?} weights={:?}",
        r.ticks, r.promoted, r.admitted, r.rejected, r.universe_filtered, r.net_lamports, r.journal_digest,
        r.per_lane_net, r.final_weights
    );
    println!(
        "GOLDEN per_discovery_lane={:?} per_alpha_source={:?}",
        r.per_discovery_lane_net, r.per_alpha_source_net
    );
    // Determinism: identical inputs reproduce the identical report.
    let r2 = drive(Config::dev_portable());
    assert_eq!(r, r2, "same events -> identical report");
    // Frozen golden outcome: optimizations must be behaviour-preserving.
    assert_eq!(
        r.journal_digest, GOLDEN_DIGEST,
        "decision-journal digest drifted"
    );
    assert_eq!(
        r.net_lamports, GOLDEN_NET_LAMPORTS,
        "realized net-SOL drifted"
    );
    assert_eq!(r.promoted, GOLDEN_PROMOTED, "promotion count drifted");
    assert_eq!(r.admitted, GOLDEN_ADMITTED, "admission count drifted");
    assert_eq!(r.rejected, GOLDEN_REJECTED, "rejection count drifted");
    assert_eq!(
        r.universe_filtered, GOLDEN_UNIVERSE_FILTERED,
        "§21.5 screen activity drifted"
    );
    // LAW D1/D5: the paid Discord room's runner is attributed to the AlphaCall
    // discovery lane, distinct from the open social-caller firehose (§71.2), and
    // the CreationSniper setup lane is the SUM of AlphaCall + SocialCaller (the
    // split the AlphaCall lane exists to make).
    let alphacall_net = r
        .per_discovery_lane_net
        .iter()
        .find(|(l, _)| *l == pump_quant_watchlist::candidate::DiscoveryLane::AlphaCall)
        .map(|(_, n)| *n)
        .unwrap();
    let social_net = r
        .per_discovery_lane_net
        .iter()
        .find(|(l, _)| *l == pump_quant_watchlist::candidate::DiscoveryLane::SocialCaller)
        .map(|(_, n)| *n)
        .unwrap();
    let onchain_net = r
        .per_discovery_lane_net
        .iter()
        .find(|(l, _)| *l == pump_quant_watchlist::candidate::DiscoveryLane::OnchainCreation)
        .map(|(_, n)| *n)
        .unwrap();
    let creationsniper_net = r
        .per_lane_net
        .iter()
        .find(|(l, _)| *l == pump_quant_watchlist::candidate::Lane::CreationSniper)
        .map(|(_, n)| *n)
        .unwrap();
    assert_eq!(
        alphacall_net, GOLDEN_ALPHACALL_NET,
        "AlphaCall discovery-lane net drifted"
    );
    // FINDING (re-pin #26, superseding re-pin #24). Re-pin #24 asserted this lane was
    // net NEGATIVE and recorded that calling it "a proven positive discovery lane" was
    // FALSE. Under the unified cost model it is positive again. The honest conclusion
    // is not "the lane is good after all" — it is that a 12-trade, 4-market book
    // cannot settle the question at all, and that the sign here is a readout of
    // whichever cost model is installed. What IS pinned is the §71.2 SPLIT: the
    // AlphaCall lane is tracked separately from the open social-caller firehose and
    // sums correctly into its setup lane. No subscription decision may cite this.
    assert!(
        alphacall_net.unsigned_abs() < GOLDEN_NET_LAMPORTS.unsigned_abs() as u64,
        "the paid room is a MINORITY of the book ({alphacall_net}) — it has never been \
         the thing carrying this tape, under any of the three cost models that have \
         priced it"
    );
    assert_eq!(
        creationsniper_net,
        alphacall_net + social_net + onchain_net,
        "CreationSniper must be the sum of its independent discovery lanes (§71.2)"
    );
    // LAW D5: the room's realized net accrues in the §29.8 per-source ledger,
    // exposed on the Report — the seam reflection uses to grade the paid room.
    assert_eq!(
        r.per_alpha_source_net.len(),
        1,
        "exactly the one paid Discord room that led the winner is tracked"
    );
    let (room, room_net) = r.per_alpha_source_net[0];
    assert_eq!(
        room.kind,
        pump_quant_social::types::SourceKind::Discord,
        "the tracked alpha source is a Discord room"
    );
    assert_eq!(
        room_net, GOLDEN_ALPHACALL_NET,
        "the room's realized net matches its AlphaCall attribution"
    );
    // Whether a paid room is worth its subscription is an OPEN question for live
    // data, and this tape has now answered it three different ways under three
    // different cost models (+447_700, -2_721_835, +593_348) on the same events. The
    // room's net is pinned as a value by `GOLDEN_ALPHACALL_NET` above; its SIGN is not
    // evidence and is deliberately not asserted here in either direction.
}

/// The §71 quota's causal lamports on THIS tape: identical events, quota 2 vs
/// quota 0 (the pre-quota engine). The streamed coin is only reachable through
/// the reserved corroboration slots, and it pays.
#[test]
fn corroboration_quota_earns_on_this_tape() {
    let with_quota = drive(Config::dev_portable());
    let mut cfg0 = Config::dev_portable();
    cfg0.promote_corroboration_quota = 0;
    let without = drive(cfg0);
    assert!(
        with_quota.net_lamports > without.net_lamports,
        "the union-preservation quota must strictly out-earn its absence ({} vs {})",
        with_quota.net_lamports,
        without.net_lamports
    );
}

/// LAW B3's measured effect on the representative golden tape — leg P4 of the
/// re-pin #21 decision rule, and the reason that re-pin is DIGEST-ONLY.
///
/// Recall DOES reach `Known` here — 144 admit-time verdicts clear the §46 sample
/// floor at the widened radius this test uses — but every class it can speak about
/// is PROFITABLE, so the reduce-only law correctly does nothing: zero haircuts, zero
/// vetoes, and a net delta of EXACTLY ZERO.
///
/// Since re-pin #21 the law ships ARMED, so this test is no longer the reason it is
/// off — it is the reason arming it was FREE on the representative path. The law's
/// causal value is demonstrated on tapes that actually contain the hazard it targets:
/// `brain_laws.rs::b3_armed_recall_haircut_strictly_out_earns_its_absence`
/// (+391_932_566 lamports of loss avoided) and the full ten-tape, eight-configuration
/// lattice in `law_permutation_sweep.rs`, where B3-alone is the unique configuration
/// clearing a pre-registered rule and its worst delta across every hazard tape is 0.
/// This test pins the golden-path neutrality, so a future change that made the law
/// active on the golden path would fail loudly.
#[test]
fn brain_haircut_is_exactly_neutral_on_this_tape() {
    // Both arms widen the recall radius to the PRE-#17 default of 12. Under the
    // shipped default of 8 admit-time recall correctly REFUSES on every query on
    // this 13-episode tape (see `BRAIN_RECALL_MAX_DISTANCE_DEFAULT`), which would
    // make this neutrality proof vacuous — a haircut that never fires because
    // recall never speaks proves nothing about the haircut. Widening to 12 gives
    // recall something to say so the neutrality claim has content. The radius is
    // the ONLY difference between the arms, so the comparison is still exact.
    let mut off_cfg = Config::dev_portable();
    off_cfg.brain_recall_max_distance = 12;
    // Since re-pin #21 the flag ships ARMED, so the neutral arm must disarm it
    // EXPLICITLY — otherwise both arms would be identical and this proof vacuous.
    off_cfg.brain_haircut_enable = false;
    let off = drive(off_cfg);
    let mut on = Config::dev_portable();
    on.brain_recall_max_distance = 12;
    on.brain_haircut_enable = true;
    let armed = drive(on);
    assert_ne!(
        off_cfg.brain_haircut_enable, on.brain_haircut_enable,
        "the two arms must actually differ in the flag under test"
    );
    println!(
        "B3-on-golden: off_net={} armed_net={} known={} unknown={} haircuts={} vetoes={}",
        off.net_lamports,
        armed.net_lamports,
        armed.brain_recall_known,
        armed.brain_recall_unknown,
        armed.brain_haircuts_applied,
        armed.brain_vetoes
    );
    assert!(
        armed.brain_recall_known > 0,
        "recall must actually reach Known here, else this neutrality proves nothing"
    );
    assert_eq!(
        armed.brain_haircuts_applied, 0,
        "every recalled class on this tape is profitable — nothing to fade"
    );
    assert_eq!(armed.brain_vetoes, 0, "and nothing to refuse");
    assert_eq!(
        armed.net_lamports, off.net_lamports,
        "LAW B3 is EXACTLY neutral on the golden tape — hence DEFAULT OFF"
    );
    assert_eq!(armed.admitted, off.admitted);
    assert_eq!(armed.rejected, off.rejected);
}

/// **LAW B7 leg (c): the NEUTRAL path.** Arming the brain-informed, reduce-only
/// lane downweight on the golden tape must change EXACTLY nothing.
///
/// The golden tape's conditioned setup classes are all POSITIVE, so
/// `brain_analysis::lane_decay` flags no lane and `reflect_with_brain` degenerates
/// to `reflect`. This is leg (c) of the pre-registered LAW B7 decision rule (see
/// `tests/brain_reflect_twosided.rs`): a protective law that perturbed the golden
/// path in the ABSENCE of the hazard it targets would be disqualified whatever its
/// hazard-tape economics, so the neutrality is asserted here on the real golden
/// tape rather than on a copy of it.
///
/// Only the JOURNAL DIGEST is exempt, and for one reason that is not a behaviour
/// change: §19 folds the whole `Config`'s strategy identity into the journal seed,
/// so flipping any config field necessarily re-seeds it.
#[test]
fn b7_armed_reflection_is_exactly_neutral_on_this_tape() {
    let off = drive(Config::dev_portable());
    let mut on_cfg = Config::dev_portable();
    on_cfg.brain_reflect_enable = true;
    let armed = drive(on_cfg);
    println!(
        "B7-on-golden: off_net={} armed_net={} off_w={:?} armed_w={:?}",
        off.net_lamports, armed.net_lamports, off.final_weights, armed.final_weights
    );
    assert_eq!(
        armed.net_lamports, off.net_lamports,
        "LAW B7 must be EXACTLY neutral on the golden tape (leg (c))"
    );
    assert_eq!(armed.admitted, off.admitted, "admissions unchanged");
    assert_eq!(armed.rejected, off.rejected, "rejections unchanged");
    assert_eq!(armed.promoted, off.promoted, "promotions unchanged");
    assert_eq!(
        armed.universe_filtered, off.universe_filtered,
        "§21.5 screen unchanged"
    );
    assert_eq!(
        armed.per_lane_net, off.per_lane_net,
        "attribution unchanged"
    );
    assert_eq!(
        armed.final_weights, off.final_weights,
        "no lane weight may move: an empty decay flag set is byte-identical to the \
         pre-LAW-B7 reflection pass"
    );
    // Not vacuous: the tape really does exercise the reflection pass.
    assert_ne!(
        off.final_weights,
        [
            (
                pump_quant_watchlist::candidate::Lane::CreationSniper,
                pump_quant_watchlist::candidate::Lane::CreationSniper.default_weight_bp()
            ),
            (
                pump_quant_watchlist::candidate::Lane::EarlyConfirmation,
                pump_quant_watchlist::candidate::Lane::EarlyConfirmation.default_weight_bp()
            ),
            (
                pump_quant_watchlist::candidate::Lane::GraduationTransition,
                pump_quant_watchlist::candidate::Lane::GraduationTransition.default_weight_bp()
            ),
            (
                pump_quant_watchlist::candidate::Lane::ActiveMarketScalp,
                pump_quant_watchlist::candidate::Lane::ActiveMarketScalp.default_weight_bp()
            ),
        ],
        "the golden tape must actually run reflection, else this neutrality is vacuous"
    );
}

/// LAW B1/B2 are DEFAULT ON and must be decision-inert: enabling the whole memory
/// plane records episodes and produces readouts without moving a single count or
/// a single lamport on the golden tape.
#[test]
fn brain_plane_is_decision_inert_on_this_tape() {
    let on = drive(Config::dev_portable());
    let mut off_cfg = Config::dev_portable();
    off_cfg.brain_enable = false;
    let off = drive(off_cfg);
    println!(
        "B1/B2-on-golden: episodes={} known={} unknown={} classes={} authors={} metas={}",
        on.brain_episodes_recorded,
        on.brain_recall_known,
        on.brain_recall_unknown,
        on.brain_setup_classes.len(),
        on.brain_author_records.len(),
        on.brain_meta_state.len()
    );
    assert!(
        on.brain_episodes_recorded > 0,
        "the plane must actually record on this tape, else the test is vacuous"
    );
    assert_eq!(off.brain_episodes_recorded, 0);
    assert_eq!(on.net_lamports, off.net_lamports, "net-SOL unchanged");
    assert_eq!(on.admitted, off.admitted, "admissions unchanged");
    assert_eq!(on.rejected, off.rejected, "rejections unchanged");
    assert_eq!(on.promoted, off.promoted, "promotions unchanged");
    assert_eq!(
        on.universe_filtered, off.universe_filtered,
        "universe screen unchanged"
    );
    assert_eq!(on.per_lane_net, off.per_lane_net, "attribution unchanged");
    assert_eq!(on.final_weights, off.final_weights, "weights unchanged");
}

/// §70.1 re-pin #19: the armed holder-flow money term is EXACTLY neutral here.
///
/// The wave's one decision-affecting switch, measured on the representative tape
/// rather than argued about. The continuous holder stream itself runs in BOTH
/// arms (it is unconditional — that is what "constant stream" means); only the
/// §70.1 money-proxy TERM is toggled. Zero lamports of difference is why it ships
/// off. See `tests/holder_flow.rs::ab_*` for the two-sided purpose-built A/B and
/// for the proof that the composite money proxy is unreachable on those tapes.
#[test]
fn holder_flow_money_term_is_exactly_neutral_on_this_tape() {
    let off = drive(Config::dev_portable());
    let mut on_cfg = Config::dev_portable();
    on_cfg.money_proxy_holder_flow_enable = true;
    let armed = drive(on_cfg);
    println!(
        "HOLDER-FLOW-on-golden: off_net={} armed_net={} off_adm={} armed_adm={}",
        off.net_lamports, armed.net_lamports, off.admitted, armed.admitted
    );
    assert_eq!(
        armed.net_lamports, off.net_lamports,
        "the §70.1 holder term must be EXACTLY neutral on the golden tape"
    );
    assert_eq!(armed.admitted, off.admitted, "admissions unchanged");
    assert_eq!(armed.rejected, off.rejected, "rejections unchanged");
    assert_eq!(armed.promoted, off.promoted, "promotions unchanged");
    assert_eq!(
        armed.per_lane_net, off.per_lane_net,
        "attribution unchanged"
    );
    // NOT vacuous: the holder stream really is populated on this tape, so a term
    // that mattered would have had something to say.
    let mut probe = Engine::new(Config::dev_portable(), RunMode::Replay);
    probe.tick(AppEvent::MarketTrade {
        mint: mint(7),
        price_fp: 1_000_000_000,
        quote_lamports: 400_000,
        liquidity_lamports: 120_000_000,
        signed_base: 500_000,
        buyer_entity: 3,
        age_slots: 12,
    });
    assert_eq!(
        probe
            .holder_reading(mint(7).as_bytes())
            .and_then(|r| r.growth_level()),
        Some(1),
        "the watch-time holder fold must be live on the golden engine"
    );
}

/// §21.7/§70.1 holder-concentration law: EXACTLY neutral on the golden tape.
///
/// Pre-registered condition (c) of the wave's A/B rule. Every golden market is
/// discovered mid-life (the tape's mints get no creation sighting before their
/// first swap), so every ledger here is `DeltaOnly` and every concentration
/// verdict is `Unknown` — which is the fail-open path, and which is exactly the
/// property that makes the basis discipline load-bearing rather than decorative:
/// the golden markets are *precisely* the ones an unguarded concentration screen
/// would have vetoed on an overstated subset share.
#[test]
fn holder_concentration_is_exactly_neutral_on_this_tape() {
    let off = drive(Config::dev_portable());
    let mut on_cfg = Config::dev_portable();
    on_cfg.holder_concentration_enable = true;
    let armed = drive(on_cfg);
    println!(
        "HOLDER-CONCENTRATION-on-golden: off_net={} armed_net={} off_adm={} armed_adm={} \
         off_filtered={} armed_filtered={}",
        off.net_lamports,
        armed.net_lamports,
        off.admitted,
        armed.admitted,
        off.universe_filtered,
        armed.universe_filtered
    );
    assert_eq!(
        armed.net_lamports, off.net_lamports,
        "the §21.7 concentration law must be EXACTLY neutral on the golden tape"
    );
    assert_eq!(armed.admitted, off.admitted, "admissions unchanged");
    assert_eq!(armed.rejected, off.rejected, "rejections unchanged");
    assert_eq!(armed.promoted, off.promoted, "promotions unchanged");
    assert_eq!(
        armed.universe_filtered, off.universe_filtered,
        "§21.5 screen activity unchanged"
    );
    assert_eq!(
        armed.per_lane_net, off.per_lane_net,
        "attribution unchanged"
    );
    // NOT vacuous by accident: prove the neutrality is the BASIS gate refusing,
    // and that a creation-first ledger on the same engine does produce a reading.
    let mut probe = Engine::new(Config::dev_portable(), RunMode::Replay);
    let m = mint(9);
    for e in 0..25u64 {
        probe.tick(AppEvent::MarketTrade {
            mint: m,
            price_fp: 1_000_000_000,
            quote_lamports: 400_000,
            liquidity_lamports: 120_000_000,
            signed_base: 500_000,
            buyer_entity: e,
            age_slots: 12,
        });
    }
    assert_eq!(
        probe
            .holder_concentration(m.as_bytes())
            .unknown_reason()
            .map(|u| format!("{u:?}")),
        Some("DeltaOnlyBasis".to_string()),
        "a mid-life golden-style market must refuse a concentration reading"
    );
    let mut probe2 = Engine::new(Config::dev_portable(), RunMode::Replay);
    let m2 = mint(10);
    probe2.tick(AppEvent::TokenMetadata {
        mint: m2,
        category_id: 0,
        taxonomy_version: 1,
        creator: 42,
        slot: 1,
    });
    for e in 0..25u64 {
        probe2.tick(AppEvent::MarketTrade {
            mint: m2,
            price_fp: 1_000_000_000,
            quote_lamports: 400_000,
            liquidity_lamports: 120_000_000,
            signed_base: 500_000,
            buyer_entity: e,
            age_slots: 12,
        });
    }
    assert!(
        probe2.holder_concentration(m2.as_bytes()).is_known(),
        "and a creation-first ledger on the SAME engine must produce one"
    );
}

/// **§21.7 — the concentration PARALLEL STREAM is decision-inert on this tape.**
///
/// The stream touches three planes this wave: it is maintained continuously on the
/// holder-sample cadence, it is recorded on every episode at admit, and it joins
/// the recall FILTER key that the reflection readout and the strategy export
/// condition on. None of those may move a decision, and this is the A/B that
/// proves it rather than asserting it.
///
/// The comparison is *exercised vs not*: one arm drives the tape and reports; the
/// other drives the same tape, exercises the whole parallel-stream surface —
/// trajectory queries, reflection refresh, the conditioned-class recall, the full
/// `brain_analysis_v1` render — and only then reports. Every journal counter,
/// including the digest, must be byte-identical.
///
/// It also pins the honest half of the coverage result: on a tape with no creation
/// sightings, NO mint reaches an `Exact` holder basis, so every episode records a
/// REFUSED band. A fingerprint field would have been forced to call all of them
/// "neutral"; the parallel stream calls them `unknown`, and the export says so.
#[test]
fn the_concentration_parallel_stream_is_decision_inert_on_this_tape() {
    // Arm A: nothing exercised.
    let plain = drive(Config::dev_portable());

    // Arm B: the whole parallel-stream + export surface exercised BEFORE the
    // report is taken, so any feedback into a decision would land in the digest.
    let mut e = drive_eng(Config::dev_portable());
    let json = e.brain_analysis_json();
    let analysis = e.brain_analysis();
    let exercised = e.report();

    assert_eq!(
        plain.journal_digest, exercised.journal_digest,
        "exercising the parallel stream moved the journal digest"
    );
    assert_eq!(plain.net_lamports, exercised.net_lamports);
    assert_eq!(plain.admitted, exercised.admitted);
    assert_eq!(plain.promoted, exercised.promoted);
    assert_eq!(plain.rejected, exercised.rejected);
    assert_eq!(plain.universe_filtered, exercised.universe_filtered);
    assert_eq!(plain.journal_digest, GOLDEN_DIGEST);

    // …and the coverage fact, reported rather than barred.
    let bands: std::collections::BTreeSet<&str> = analysis
        .setup_classes
        .iter()
        .map(|c| c.concentration_band)
        .collect();
    println!(
        "PARALLEL-STREAM-on-golden: classes={} bands={:?} report_rows={} \
         reflection_classes={}",
        analysis.setup_classes.len(),
        bands,
        exercised.holder_trajectory.len(),
        exercised.brain_setup_classes.len()
    );
    assert!(
        bands.iter().all(|b| *b == "unknown"),
        "no mint on this tape reaches an Exact holder basis, so every class must \
         REFUSE a band: {bands:?}"
    );
    assert!(json.contains("\"concentration_band\":\"unknown\""));
    for band in ["broad", "moderate", "concentrated", "extreme"] {
        assert!(
            !json.contains(&format!("\"concentration_band\":\"{band}\"")),
            "the artifact invented band {band} on a tape that measured none"
        );
    }
    // Every reflection row agrees: code 0 is the refusal, and it is what conditions
    // (i.e. does not condition) the estimate behind the row.
    assert!(exercised
        .brain_setup_classes
        .iter()
        .all(|c| c.concentration_code == 0));
}
