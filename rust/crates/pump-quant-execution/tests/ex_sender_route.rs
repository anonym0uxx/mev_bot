//! Tests for `ex_sender_route`.
//!
//! The load-bearing tests here are the NEGATIVE controls: a budget rule that
//! cannot decline, and a tip-account selector that cannot reject a bad address,
//! are not controls at all.

use pump_quant_execution::ex_route_policy::{Route, RouteCtx};
use pump_quant_execution::ex_sender_route::*;

/// 0.1 SOL — the position size implied by this bot's own ATA-rent finding
/// (0.00203928 SOL of rent reading as 203 bps).
const SIZE_0P1_SOL: u64 = 100_000_000;
const SIZE_1_SOL: u64 = 1_000_000_000;

fn ctx(size: u64, edge_bps: u32, sends: u32) -> SenderCtx {
    SenderCtx {
        trade_size_lamports: size,
        entry_edge_bps: edge_bps,
        expected_sends: sends,
        current_slippage_bps: 0,
        tip_budget_bps: DEFAULT_TIP_BUDGET_BPS,
        swqos_min_tip_lamports: SWQOS_ONLY_MIN_TIP_LAMPORTS,
        max_min_tip_lamports: MAX_TIER_MIN_TIP_LAMPORTS_DOCS,
        mev_protect: false,
        congestion_bps: 0,
        urgency: 0,
        sender_healthy: true,
        sender_latency_ms: 0,
        sender_fail_bps: 0,
    }
}

fn route_ctx(size: u64, edge_bps: u32) -> RouteCtx {
    RouteCtx {
        entry_edge_bps: edge_bps,
        trade_size_lamports: size,
        opportunity_half_life_ms: 10_000,
        is_sell: false,
        is_forced_exit: false,
        current_slippage_bps: 0,
        promotion_enabled: true,
        half_life_threshold_ms: 2_000,
        min_edge_for_nozomi_bps: 500,
        min_edge_for_jito_bps: 500,
        exit_slippage_trigger_bps: 300,
        nozomi_healthy: true,
        jito_healthy: true,
        rpc_latency_ms: 0,
        nozomi_latency_ms: 0,
        jito_latency_ms: 0,
        rpc_fail_bps: 0,
        nozomi_fail_bps: 0,
        jito_fail_bps: 0,
        nozomi_tip_lamports: 1_000_000,
        jito_tip_lamports: 1_000_000,
    }
}

// ───────────────────────────── budget arithmetic ─────────────────────────────

#[test]
fn edge_and_budget_are_exact_integers() {
    // 0.1 SOL at 1000 bps (10%) = 0.01 SOL of edge = 10_000_000 lamports.
    let c = ctx(SIZE_0P1_SOL, 1_000, 1);
    assert_eq!(edge_lamports(&c), 10_000_000);
    // 10% of that edge is spendable on tips.
    assert_eq!(tip_budget_lamports(&c), 1_000_000);
}

#[test]
fn zero_sends_is_treated_as_one() {
    let mut c = ctx(SIZE_0P1_SOL, 1_000, 0);
    c.max_min_tip_lamports = MAX_TIER_MIN_TIP_LAMPORTS_DOCS;
    let d = decide(&c);
    assert_eq!(d.expected_sends, 1);
    // A zero send-count must not make a tier look free.
    assert!(d.total_tip_lamports > 0);
}

// ─────────────────────── the regressivity this exists for ────────────────────

#[test]
fn max_tier_is_affordable_at_one_send_and_not_at_three() {
    // 0.1 SOL, 10% edge -> 1_000_000 lamport budget.
    // Max floor (docs) is 1_000_000 per send.
    let one = decide(&ctx(SIZE_0P1_SOL, 1_000, 1));
    assert_eq!(one.tier, SenderTier::Max);
    assert!(one.economic);
    assert_eq!(one.total_tip_lamports, 1_000_000);

    // Three sends (buy + two ladder rungs) costs 3_000_000 against the same
    // 1_000_000 budget, so Max must be refused and SWQoS chosen instead.
    let three = decide(&ctx(SIZE_0P1_SOL, 1_000, 3));
    assert_eq!(three.tier, SenderTier::SwqosOnly);
    assert!(
        three.economic,
        "swqos at 15_000 still fits a 1_000_000 budget"
    );
    assert_eq!(three.total_tip_lamports, 15_000);
}

#[test]
fn same_trade_at_ten_times_the_size_affords_max() {
    // Identical edge in bps, ten times the size: the fixed tip stops mattering.
    let d = decide(&ctx(SIZE_1_SOL, 1_000, 3));
    assert_eq!(d.tier, SenderTier::Max);
    assert!(d.economic);
    assert_eq!(d.total_tip_lamports, 3_000_000);
    assert_eq!(d.tip_budget_lamports, 10_000_000);
}

#[test]
fn dashboard_minimum_changes_the_answer() {
    // The unresolved 5x discrepancy is not cosmetic: at three sends on a 0.1 SOL
    // trade, the dashboard figure affords Max and the docs figure does not.
    let mut c = ctx(SIZE_0P1_SOL, 1_000, 3);
    c.max_min_tip_lamports = MAX_TIER_MIN_TIP_LAMPORTS_DASHBOARD;
    let d = decide(&c);
    assert_eq!(d.tier, SenderTier::Max);
    assert_eq!(d.total_tip_lamports, 600_000);
}

// ───────────────────────────── NEGATIVE CONTROLS ─────────────────────────────

#[test]
fn negative_control_uneconomic_trade_is_declined() {
    // A trade so small that even the SWQoS floor exceeds the edge budget.
    // 0.001 SOL at 100 bps = 10_000 lamports of edge; 10% budget = 1_000
    // lamports; the SWQoS floor is 5_000. It must report economic == false.
    let c = ctx(1_000_000, 100, 1);
    let d = decide(&c);
    assert_eq!(d.tier, SenderTier::SwqosOnly);
    assert!(
        !d.economic,
        "budget rule must be able to decline, or it is not a rule"
    );
    assert!(d.total_tip_lamports > d.tip_budget_lamports);
}

#[test]
fn negative_control_zero_edge_is_never_economic() {
    let d = decide(&ctx(SIZE_1_SOL, 0, 1));
    assert_eq!(d.tip_budget_lamports, 0);
    assert!(!d.economic);
}

#[test]
fn negative_control_uneconomic_sender_never_wins_the_plan() {
    // Sender declines on budget, so the plan must fall back to a legacy route
    // even though the legacy tip is far larger.
    let rc = route_ctx(1_000_000, 100);
    let sc = ctx(1_000_000, 100, 1);
    let out = choose_submit_plan(&rc, &sc);
    assert!(!out.sender.economic);
    assert!(matches!(out.plan, SubmitPlan::Legacy(_)));
    assert_eq!(out.sender_ev_lamports, i128::MIN);
}

#[test]
fn negative_control_unhealthy_sender_never_wins() {
    let rc = route_ctx(SIZE_1_SOL, 1_000);
    let mut sc = ctx(SIZE_1_SOL, 1_000, 1);
    sc.sender_healthy = false;
    let out = choose_submit_plan(&rc, &sc);
    assert!(matches!(out.plan, SubmitPlan::Legacy(_)));
}

#[test]
fn negative_control_tip_account_rejects_bad_input() {
    // Empty set.
    assert_eq!(tip_account_from(&[], 0), None);
    assert_eq!(tip_account_from(&["tooshort"], 0), None);
    // Non-base58 characters (0, O, I, l are not in the alphabet).
    assert_eq!(
        tip_account_from(&["0OIl0OIl0OIl0OIl0OIl0OIl0OIl0OIl"], 0),
        None
    );
    // One bad address poisons the whole set — fail closed.
    assert_eq!(
        tip_account_from(&[SENDER_TIP_ACCOUNTS[0], "tooshort"], 0),
        None
    );
    // Including when the bad one is a truncation that a length check would pass.
    assert_eq!(
        tip_account_from(
            &[
                SENDER_TIP_ACCOUNTS[0],
                "4ACfpUFoaSD9bfPdeu6DBt89gB6ENTeHBXCA"
            ],
            0
        ),
        None
    );
}

// ──────────────────────────── tip account handling ───────────────────────────

#[test]
fn truncated_address_is_now_rejected() {
    // Previously a documented LIMITATION; now a defence. A screenshot value
    // truncated to 36 base58 characters is still base58 and still 32..=44 long,
    // so a charset-and-length check accepted it. Decoding rejects it: the value
    // shrinks, producing leading zero bytes that the string has no leading '1's
    // to justify.
    let truncated = "4ACfpUFoaSD9bfPdeu6DBt89gB6ENTeHBXCA"; // 36 chars, valid charset
    assert!(!is_valid_tip_account(truncated));
    assert!(is_valid_tip_account(SENDER_TIP_ACCOUNTS[0]));
    // And the selector refuses a set containing it.
    assert_eq!(tip_account_from(&[truncated], 0), None);
}

#[test]
fn tip_accounts_are_valid_and_distinct() {
    // Re-proves at test time what was verified before committing: every entry
    // decodes to exactly 32 bytes, and no two are the same.
    for a in SENDER_TIP_ACCOUNTS {
        assert!(
            is_valid_tip_account(a),
            "{a} is not a valid 32-byte address"
        );
    }
    let mut seen: Vec<&str> = Vec::new();
    for a in SENDER_TIP_ACCOUNTS {
        assert!(!seen.contains(&a), "duplicate tip account {a}");
        seen.push(a);
    }
    assert_eq!(seen.len(), 10);
    // The 43-character entry is correct, not damaged: a small leading byte
    // encodes shorter. Length is not a validity test.
    assert_eq!(SENDER_TIP_ACCOUNTS[6].len(), 43);
    assert!(is_valid_tip_account(SENDER_TIP_ACCOUNTS[6]));
}

#[test]
fn address_validator_rejects_the_usual_damage() {
    assert!(!is_valid_tip_account(""));
    assert!(!is_valid_tip_account("tooshort"));
    // 0, O, I and l are absent from the base58 alphabet precisely because they
    // are the characters misread off a screen.
    assert!(!is_valid_tip_account("0OIl0OIl0OIl0OIl0OIl0OIl0OIl0OIl"));
    // Too long to fit in 32 bytes.
    assert!(!is_valid_tip_account(
        "4ACfpUFoaSD9bfPdeu6DBt89gB6ENTeHBXCAi87NhDEE4ACfpUFoaSD9bfPdeu6"
    ));
    // A single transposed character still decodes to 32 bytes, so it passes.
    // Stated so nobody mistakes this for protection against a typo that happens
    // to remain well-formed - only the copy button protects against that.
    let mut chars: Vec<char> = SENDER_TIP_ACCOUNTS[0].chars().collect();
    chars.swap(5, 6);
    let transposed: String = chars.into_iter().collect();
    assert_ne!(transposed, SENDER_TIP_ACCOUNTS[0]);
    assert!(is_valid_tip_account(&transposed));
}

#[test]
fn tip_account_selection_is_deterministic_and_spreads() {
    let set: Vec<&str> = SENDER_TIP_ACCOUNTS.to_vec();
    // Same seed, same answer — replay must reproduce the choice.
    assert_eq!(tip_account_from(&set, 7), tip_account_from(&set, 7));
    // Consecutive seeds land on different accounts.
    assert_ne!(tip_account_from(&set, 0), tip_account_from(&set, 1));
    // Wraps at the set size.
    assert_eq!(tip_account_from(&set, 0), tip_account_from(&set, 10));
    // All ten are reachable — a selector that only ever picks a few would
    // reintroduce the write-lock contention this exists to spread.
    let mut hit = std::collections::BTreeSet::new();
    for seed in 0..10u64 {
        hit.insert(tip_account_from(&set, seed).unwrap());
    }
    assert_eq!(hit.len(), 10);
}

// ────────────────────────────── endpoint queries ─────────────────────────────

#[test]
fn query_suffixes_match_the_documented_parameters() {
    assert_eq!(
        query_suffix(SenderTier::SwqosOnly, false),
        "?swqos_only=true"
    );
    assert_eq!(
        query_suffix(SenderTier::SwqosOnly, true),
        "?swqos_only=true&mev-protect=true"
    );
    assert_eq!(query_suffix(SenderTier::Max, false), "");
    assert_eq!(query_suffix(SenderTier::Max, true), "?mev-protect=true");
}

// ─────────────────────────────── EV behaviour ────────────────────────────────

#[test]
fn ev_charges_every_send_not_just_one() {
    let one = sender_ev_lamports(SenderTier::Max, &ctx(SIZE_1_SOL, 1_000, 1));
    let three = sender_ev_lamports(SenderTier::Max, &ctx(SIZE_1_SOL, 1_000, 3));
    // Two extra sends at the 1_000_000 lamport floor.
    assert_eq!(one - three, 2_000_000);
}

#[test]
fn max_tier_earns_the_jito_slippage_credit_and_swqos_does_not() {
    let mut c = ctx(SIZE_1_SOL, 1_000, 1);
    c.current_slippage_bps = 200;
    let max_ev = sender_ev_lamports(SenderTier::Max, &c);
    let swqos_ev = sender_ev_lamports(SenderTier::SwqosOnly, &c);
    // Max pays 1_000_000 more tip but earns (200 bps of 1 SOL) / 2 = 10_000_000
    // of slippage credit, so it should still win by 9_000_000 - the 5_000 SWQoS
    // tip it no longer pays.
    assert_eq!(max_ev - swqos_ev, 10_000_000 - 1_000_000 + 5_000);
}

#[test]
fn mev_protect_earns_no_ev_credit() {
    let plain = ctx(SIZE_1_SOL, 1_000, 1);
    let mut protected = plain;
    protected.mev_protect = true;
    // Deliberate: an unmeasured benefit must not tilt the model toward the
    // option it was never tested on.
    assert_eq!(
        sender_ev_lamports(SenderTier::Max, &plain),
        sender_ev_lamports(SenderTier::Max, &protected)
    );
}

#[test]
fn sender_wins_only_on_strictly_greater_ev() {
    // Legacy Jito and Sender Max priced identically: same tip, same latency,
    // same fail rate, same slippage credit. A tie must preserve today's
    // behaviour rather than switch submitters.
    //
    // Health inputs are populated deliberately - without them the fix-2 gate
    // fires and no comparison happens at all, which would test nothing.
    let mut rc = route_ctx(SIZE_1_SOL, 1_000);
    rc.is_sell = true;
    rc.current_slippage_bps = 200;
    rc.rpc_latency_ms = 800;
    rc.rpc_fail_bps = 2_000;
    rc.jito_latency_ms = 400;
    rc.jito_fail_bps = 500;
    rc.jito_tip_lamports = 1_000_000;

    let mut sc = ctx(SIZE_1_SOL, 1_000, 1);
    sc.current_slippage_bps = 200;
    sc.max_min_tip_lamports = MAX_TIER_MIN_TIP_LAMPORTS_DOCS; // == the Jito tip
    sc.sender_latency_ms = 400;
    sc.sender_fail_bps = 500;

    let out = choose_submit_plan(&rc, &sc);
    assert!(out.health_measured);
    assert_eq!(out.legacy_route, Route::JitoBundle);
    assert_eq!(out.legacy_ev_lamports, out.sender_ev_lamports);
    assert!(
        matches!(out.plan, SubmitPlan::Legacy(Route::JitoBundle)),
        "a tie must not displace the incumbent route"
    );
}

#[test]
fn rpc_is_modelled_as_free_so_unmeasured_health_makes_sender_unreachable() {
    // Documented trap, asserted so it cannot regress silently.
    //
    // `route_ev_lamports` charges Route::Rpc a fee of ZERO. With every health
    // input left at its default of 0 — which is exactly what an unwired
    // route-health feed produces — RPC scores gross edge with nothing deducted
    // and is unbeatable by any tipped route, Sender included.
    //
    // The consequence is operational, not theoretical: if rpc_fail_bps and
    // rpc_latency_ms are never populated with real measurements, Sender is dead
    // code that the EV model will never select.
    let rc = route_ctx(SIZE_1_SOL, 1_000); // all health inputs zero
    let mut sc = ctx(SIZE_1_SOL, 1_000, 1);
    sc.max_min_tip_lamports = MAX_TIER_MIN_TIP_LAMPORTS_DASHBOARD;

    let out = choose_submit_plan(&rc, &sc);
    assert_eq!(out.legacy_route, Route::Rpc);
    assert!(out.sender.economic, "the tip fits the budget");
    assert!(
        out.legacy_ev_lamports > out.sender_ev_lamports,
        "free RPC beats any tipped route when health is unmeasured"
    );
    assert!(matches!(out.plan, SubmitPlan::Legacy(Route::Rpc)));
}

#[test]
fn sender_wins_when_route_health_is_actually_measured() {
    // The same comparison with realistic health inputs: RPC lands slower and
    // fails more often, Jito lands well but costs 5_000_000 in tip, Sender
    // matches Jito's reliability at the dashboard tip floor.
    let mut rc = route_ctx(SIZE_1_SOL, 1_000);
    rc.is_sell = true;
    rc.rpc_latency_ms = 800;
    rc.rpc_fail_bps = 2_000; // 20% of sends fail to land
    rc.jito_latency_ms = 400;
    rc.jito_fail_bps = 500;
    rc.jito_tip_lamports = 5_000_000;

    let mut sc = ctx(SIZE_1_SOL, 1_000, 1);
    sc.max_min_tip_lamports = MAX_TIER_MIN_TIP_LAMPORTS_DASHBOARD;
    sc.sender_latency_ms = 400;
    sc.sender_fail_bps = 500;

    let out = choose_submit_plan(&rc, &sc);
    // Jito is the legacy winner: 92_100_000 against RPC's 89_200_000.
    assert_eq!(out.legacy_route, Route::JitoBundle);
    assert_eq!(out.legacy_ev_lamports, 92_100_000);
    // Sender matches Jito's landing profile for 4_800_000 less tip.
    assert_eq!(out.sender_ev_lamports, 96_900_000);
    assert!(matches!(
        out.plan,
        SubmitPlan::Sender {
            tier: SenderTier::Max,
            mev_protect: false
        }
    ));
}

#[test]
fn congestion_and_urgency_can_price_max_out_of_budget() {
    // Affordable when calm.
    let calm = decide(&ctx(SIZE_1_SOL, 1_000, 3));
    assert_eq!(calm.tier, SenderTier::Max);

    // Under congestion and urgency the scaled tip breaches the same budget and
    // the decision must step down a tier rather than overspend.
    let mut hot = ctx(SIZE_1_SOL, 1_000, 3);
    hot.congestion_bps = 20_000; // 3x
    hot.urgency = 2; // 2x
    let d = decide(&hot);
    assert_eq!(d.tier, SenderTier::SwqosOnly);
    assert!(d.total_tip_lamports < calm.total_tip_lamports);
}

// ═══════════════════ DEFECT FIXES — regression tests ════════════════════════

#[test]
fn fix_1_legacy_ev_now_charges_every_send() {
    use pump_quant_execution::ex_route_policy::{route_ev_lamports, route_ev_lamports_with_sends};

    let mut rc = route_ctx(SIZE_1_SOL, 1_000);
    rc.jito_tip_lamports = 1_000_000;

    // One send matches the original function exactly — existing callers are
    // unaffected by the fix.
    assert_eq!(
        route_ev_lamports_with_sends(Route::JitoBundle, &rc, 1),
        route_ev_lamports(Route::JitoBundle, &rc)
    );
    // Zero is treated as one, so a missing send count cannot make a route free.
    assert_eq!(
        route_ev_lamports_with_sends(Route::JitoBundle, &rc, 0),
        route_ev_lamports(Route::JitoBundle, &rc)
    );
    // Three sends charge the tip three times: 2 extra x 1_000_000.
    assert_eq!(
        route_ev_lamports(Route::JitoBundle, &rc)
            - route_ev_lamports_with_sends(Route::JitoBundle, &rc, 3),
        2_000_000
    );
    // Rpc pays no tip, so its EV is unchanged by send count. That is the
    // fee-free modelling documented in fix 2, not an oversight here.
    assert_eq!(
        route_ev_lamports_with_sends(Route::Rpc, &rc, 5),
        route_ev_lamports(Route::Rpc, &rc)
    );
}

#[test]
fn fix_2_unmeasured_health_is_detected() {
    use pump_quant_execution::ex_route_policy::route_health_is_measured;

    // Every latency and failure input zero: an unwired feed.
    let blank = route_ctx(SIZE_1_SOL, 1_000);
    assert!(!route_health_is_measured(&blank));

    // Any single non-zero input counts as measured.
    for mutate in [
        |c: &mut RouteCtx| c.rpc_latency_ms = 1,
        |c: &mut RouteCtx| c.nozomi_latency_ms = 1,
        |c: &mut RouteCtx| c.jito_latency_ms = 1,
        |c: &mut RouteCtx| c.rpc_fail_bps = 1,
        |c: &mut RouteCtx| c.nozomi_fail_bps = 1,
        |c: &mut RouteCtx| c.jito_fail_bps = 1,
    ] {
        let mut c = blank;
        mutate(&mut c);
        assert!(route_health_is_measured(&c));
    }
}

#[test]
fn fix_2_negative_control_unmeasured_health_fails_closed() {
    // Sender is healthy, within budget, and would win on EV if the comparison
    // meant anything. It must still lose, because the comparison does not.
    let rc = route_ctx(SIZE_1_SOL, 1_000); // all health inputs zero
    let mut sc = ctx(SIZE_1_SOL, 1_000, 1);
    sc.max_min_tip_lamports = MAX_TIER_MIN_TIP_LAMPORTS_DASHBOARD;

    let out = choose_submit_plan(&rc, &sc);
    assert!(!out.health_measured, "the blank feed must be detected");
    assert!(out.sender.economic, "the tip fits the budget");
    assert!(sc.sender_healthy, "the endpoint is up");
    assert!(matches!(out.plan, SubmitPlan::Legacy(_)));
    assert_eq!(
        out.sender_ev_lamports,
        i128::MIN,
        "an uninformative comparison must not produce a score"
    );
}

#[test]
fn fix_2_measured_health_re_enables_sender() {
    // The same context with one real measurement present: Sender is comparable
    // again and wins on its own numbers.
    let mut rc = route_ctx(SIZE_1_SOL, 1_000);
    rc.is_sell = true;
    rc.rpc_latency_ms = 800;
    rc.rpc_fail_bps = 2_000;
    rc.jito_latency_ms = 400;
    rc.jito_fail_bps = 500;
    rc.jito_tip_lamports = 5_000_000;

    let mut sc = ctx(SIZE_1_SOL, 1_000, 1);
    sc.max_min_tip_lamports = MAX_TIER_MIN_TIP_LAMPORTS_DASHBOARD;
    sc.sender_latency_ms = 400;
    sc.sender_fail_bps = 500;

    let out = choose_submit_plan(&rc, &sc);
    assert!(out.health_measured);
    assert!(matches!(
        out.plan,
        SubmitPlan::Sender {
            tier: SenderTier::Max,
            ..
        }
    ));
}

#[test]
fn fix_1_plan_comparison_charges_both_sides_for_the_ladder() {
    // Three sends. Before the fix the legacy side was charged one tip and the
    // Sender side three, which made Sender look worse by exactly the ladder
    // depth. Both sides must now be charged three.
    use pump_quant_execution::ex_route_policy::route_ev_lamports_with_sends;

    let mut rc = route_ctx(SIZE_1_SOL, 1_000);
    rc.is_sell = true;
    rc.jito_latency_ms = 400;
    rc.jito_fail_bps = 500;
    rc.rpc_latency_ms = 800;
    rc.rpc_fail_bps = 2_000;
    rc.jito_tip_lamports = 1_000_000;

    let mut sc = ctx(SIZE_1_SOL, 1_000, 3);
    sc.max_min_tip_lamports = MAX_TIER_MIN_TIP_LAMPORTS_DASHBOARD;
    sc.sender_latency_ms = 400;
    sc.sender_fail_bps = 500;

    let out = choose_submit_plan(&rc, &sc);
    assert_eq!(out.legacy_route, Route::JitoBundle);
    assert_eq!(
        out.legacy_ev_lamports,
        route_ev_lamports_with_sends(Route::JitoBundle, &rc, 3),
        "the legacy side must be priced for all three sends"
    );
    // Jito: 3 x 1_000_000 tip. Sender Max: 3 x 200_000. Same landing profile.
    assert_eq!(out.sender_ev_lamports - out.legacy_ev_lamports, 2_400_000);
    assert!(matches!(out.plan, SubmitPlan::Sender { .. }));
}
