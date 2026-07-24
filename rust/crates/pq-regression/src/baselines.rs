//! REGRESSION MANIFEST — every pinned invariant in ONE auditable place.
//!
//! This module is the single source of truth for the regression baselines. If a
//! value here changes, it is a deliberate re-pin and must be justified (a HEAD at
//! green with these values passing is the contract). `BASELINES.md` narrates the
//! same numbers for humans; `tests/regression_manifest.rs` asserts the two never
//! drift apart. Nothing here is computed at runtime — these are frozen tripwires.
//!
//! Provenance: mirrored from `pump-quant-app/tests/golden_digest.rs` (digest, net,
//! counts, universe_filtered), `crates/pump-quant-app/src/config.rs`
//! (`Config::dev_portable` law-toggle defaults), and the workspace dossier count.

// ===========================================================================
// Golden decision-journal digest + outcome (the determinism tripwire).
// Mirror of golden_digest.rs's frozen constants.
// ===========================================================================

/// Byte-exact decision-journal digest of the golden tape under
/// `Config::dev_portable`. The primary determinism fingerprint (§22/§54).
///
/// Re-pin #18 (brain → strategy analysis, LAWs B6–B9) moved this and NOTHING
/// else: §19 folds the whole `Config`'s strategy identity into the journal seed,
/// and this wave ADDED five config fields — `brain_analysis_enable`,
/// `brain_analysis_path`, `brain_reflect_enable`, `brain_reflect_step_bp`,
/// `brain_decay_min_sample` — which necessarily re-seeds it. Every decision-plane
/// number below is unchanged from re-pin #17, which is the proof that the whole
/// wave (the `brain_analysis_v1` export, the §56 retirement-review nominations,
/// the brain-grounded exit proposals and recall-as-promotion-blocker) is
/// decision-inert, and that LAW B7's reduce-only lane downweight — which SHIPS OFF
/// because its A/B was exactly neutral — changes nothing while disarmed.
///
/// (Re-pin #17 moved it for two config VALUES: `meta_taxonomy_version` 0 → 1 and
/// `brain_recall_max_distance` 12 → 8.)
pub const GOLDEN_DIGEST: u64 = 14_149_586_802_844_500_794;
/// Realized net-SOL (lamports) on the golden tape (§24-compliant cost-derived).
pub const GOLDEN_NET_LAMPORTS: i128 = 15_410_801;
/// Candidates promoted to the gate.
pub const GOLDEN_PROMOTED: u64 = 504;
/// Candidates admitted by the gate.
pub const GOLDEN_ADMITTED: u64 = 13;
/// Candidates rejected by the gate.
pub const GOLDEN_REJECTED: u64 = 457;
/// Zombie-cohort promotions the §21.5 universe screen removes (visible activity).
pub const GOLDEN_UNIVERSE_FILTERED: u64 = 72;

/// Net-SOL on the golden tape with the §24 cost-derived ladder DISABLED — i.e.
/// the forbidden fixed 13_500/25_000/50_000 ladder. Pinned so the §24 reversal's
/// decision-level effect (derived out-earns fixed here by +355_101) can never be
/// silently dead-coded. Source: golden_digest.rs re-pin #15 (recalibrated sizing).
pub const GOLDEN_NET_FIXED_LADDER: i128 = 15_055_700;

/// The margin by which the §24 cost-derived default out-earns the forbidden fixed
/// ladder on the representative golden tape (re-pin #13: "derived now marginally
/// OUT-earns fixed", preserved through re-pin #14's Discord alpha cohort and re-pin
/// #15's 0.1-SOL operator floor + small-bankroll Kelly recalibration — the modest
/// alpha winner still peaks BELOW the fixed +35% rung so it never rewards the
/// forbidden ladder; the larger clips widen the margin to +355_101). Mirror of
/// `GOLDEN_NET_LAMPORTS - GOLDEN_NET_FIXED_LADDER`; the manifest test proves the
/// identity so a drift in either net is caught here.
pub const GOLDEN_DERIVED_MINUS_FIXED: i128 = 355_101;

/// The full net-SOL re-pin ARC narrated in `golden_digest.rs` (re-pins #1→#15).
/// The LAST element is the live [`GOLDEN_NET_LAMPORTS`]; the manifest test proves
/// that identity, so an undocumented golden re-pin that forgot to extend the arc,
/// or a net drift, fails against this mirror. Source: the "(arc: …)" annotations.
pub const GOLDEN_NET_ARC: &[i128] = &[
    2_979_624, 5_017_234, 6_443_936, 8_785_954, 12_550_767, 3_831_945, 1_406_102, 1_864_780,
    15_410_801,
];

/// The single REAL decision-level signed delta re-pin #12 recorded on the
/// (then unrealistic) tape: `3_831_945 - 12_550_767`. Pinned so the documented
/// "signed delta of −8_718_822" stays internally consistent with the arc.
pub const GOLDEN_ARC_REPIN12_DELTA: i128 = -8_718_822;

// ===========================================================================
// Law-toggle default pins (`Config::dev_portable`). Catches a silent flip of a
// protective/behavioural default — a real regression even when the digest test
// would also move. The name is the exact `Config` field / apply() key.
// ===========================================================================

/// A pinned boolean law toggle: `(config key, dev_portable default)`.
pub const LAW_BOOL_DEFAULTS: &[(&str, bool)] = &[
    // §26 confirmed-creator-dump hard veto (operator-approved reversal).
    ("creator_dump_veto_enable", true),
    // §24 cost-derived profit targets — the mandated live default ("constitution wins").
    ("derived_targets_enable", true),
    // §24(d) exit-into-strength — situational, report-only until flipped.
    ("into_strength_exit_enable", false),
    // §24 volatility-scaled stops/trail — situational, report-only.
    ("vol_stop_enable", false),
    // §25 setup-archetype classifier — correctness wiring, ON.
    ("setup_classifier_enable", true),
    // §24 EntryMode leaves (pullback-continuation admission) — new path, OFF.
    ("entry_mode_leaves_enable", false),
    // §70.1 composite money proxy — legitimate lamports-moving law, ON.
    ("money_proxy_enable", true),
    // §70.6/§70.8 narrative class + ceiling — new scoring, OFF.
    ("narrative_class_enable", false),
    // §70.7 platform-lead / crypto-social-lag — new scoring, OFF.
    ("platform_lead_enable", false),
    // §70.9 deployer credibility screen — protective, OFF.
    ("deployer_screen_enable", false),
    // §70.10 anti-bundle fee-floor veto — protective, OFF.
    ("fee_floor_enable", false),
    // §33 probe-budget sizing — OFF.
    ("probe_budget_enable", false),
    // §29 Discord paid-alpha lane (Wave-3 LAW D1) — attribution/correctness, ON.
    ("alpha_call_lane_enable", true),
    // §29 designated-caller attention weight (Wave-3 LAW D2) — high-signal, ON.
    ("designated_caller_enable", true),
    // §29.5 bearish-alpha reduce-only exit pressure (Wave-3 LAW D3) — protective, ON.
    ("alpha_exit_pressure_enable", true),
    // Episodic recall memory plane (LAWs B1/B2/B5) — record + readout only, ON.
    ("brain_enable", true),
    // §29.5/§46 reduce-only recall haircut/veto (LAW B3) — EXACTLY neutral on the
    // golden tape (every recalled class there is profitable), so it ships OFF and
    // earns its keep only on its own hazard tape (+391_932_566 lamports there).
    ("brain_haircut_enable", false),
    // LAW B5 durable episodic journal — operator opt-in (needs a path), OFF.
    ("brain_persist_enable", false),
    // LAW B6 `brain_analysis_v1` strategy-analysis export — report-plane and
    // decision-inert, so ON; the PATH is empty by default, so nothing is written
    // until an operator names a sink.
    ("brain_analysis_enable", true),
    // LAW B7 reduce-only brain-informed lane downweight — the A/B was EXACTLY
    // neutral (delta 0 net) on the golden tape and on a purpose-built decayed-lane
    // tape at every step from 250 bp to the full §56.2 envelope, so it ships OFF.
    ("brain_reflect_enable", false),
];

/// A pinned integer law parameter that gates a batch-E law's behaviour:
/// `(config key, dev_portable default)`. Each is the toggle a `batch_e_laws.rs`
/// A/B neutralizes; pinning the default here guards against a silent change.
pub const LAW_INT_DEFAULTS: &[(&str, i64)] = &[
    // §21.5 active-market universe screen: fresh-launch age exemption (slots).
    ("universe_age_exempt_slots", 64),
    // §21.6 bar-structure clock: trades per bar.
    ("bar_trades_per_bar", 8),
    // §21.6 downtrend structure haircut (bps of size).
    ("structure_downtrend_haircut_bp", 7_000),
    // §29.6 attention/narrative decay rate (bps; 10_000 == identity/off).
    ("narrative_decay_bp", 9_330),
    // §29.6 decay floor.
    ("narrative_decay_floor", 4),
    // §71 union-preservation corroboration quota (reserved promotion slots).
    ("promote_corroboration_quota", 2),
    // §21.4 / criterion 81 meta-taxonomy version stamped on category assignments.
    // Re-pin #17 bumped this 0 → 1 (word-boundary needle matching); a v0-stamped
    // assignment no longer matches and is left UNKNOWN rather than remapped.
    ("meta_taxonomy_version", 1),
    // §46 LAW B4 sample floor below which recall is structurally Unknown.
    ("brain_min_sample", 8),
    // §102 LAW B3 similarity radius defining "a setup like this". Re-pin #17
    // narrowed this 12 → 8: at 12 the OFI and CVD ladders (6 buckets each) let a
    // maximally net-BUYING setup match a maximally net-SELLING one.
    ("brain_recall_max_distance", 8),
    // §29.5 LAW B3 haircut / veto win-rate bars and the reduce-only factor (bps).
    ("brain_haircut_win_rate_bp", 3_500),
    ("brain_veto_win_rate_bp", 1_500),
    ("brain_haircut_mult_bp", 5_000),
];

// ===========================================================================
// Bounded-state capacity pins (§99). Feeding > cap must never grow past cap.
// ===========================================================================

/// `Config::dev_portable.watchlist_capacity` — the hard live-candidate cap (§99).
pub const WATCHLIST_CAPACITY: usize = 64;
/// `LIVE_CHATTER_CAP` in `attention.rs` — distinct-chatter breadth ceiling (§99).
pub const LIVE_CHATTER_CAP: usize = 16;

// ===========================================================================
// Structural / repository invariants.
// ===========================================================================

/// Count of materialized dossier property-test files (`dossier_*.rs` under any
/// crate's `tests/`) across the workspace. A silent drop means a component lost
/// its correctness authority. Source: workspace scan at the pinned green HEAD.
pub const DOSSIER_FILE_COUNT: usize = 191;
