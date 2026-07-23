//! Criterion 112 / Amendment A-6 — the operator-directed absolute minimum trade
//! size (0.1 SOL) end-to-end. Four laws, each an A/B or an invariant over the REAL
//! engine (public surface only) or the pure split helper:
//!
//! * **no-sub-floor invariant** (load-bearing): across a golden-style multi-mint run,
//!   EVERY emitted order/bite is ≥ the 0.1-SOL floor, and NO sub-`x_min` probe fires.
//! * **clamp-up unblocks**: a viable setup whose Kelly size lands below the floor is
//!   ADMITTED (clamped UP to 0.1) with the promote valve, where a defeated valve
//!   (cap 0 — the strict refuse-below-x_min) would block it.
//! * **refuse-if-unsafe**: a market too thin to take a 0.1 clip (x_max < floor) is
//!   REFUSED — never a sub-floor order, never an over-x_max order.
//! * **probe ≥ floor**: a target that cannot split into two ≥floor bites opens as a
//!   SINGLE ≥floor bite; a target ≥ 2×floor splits into two ≥floor bites.
//!
//! Determinism (§22) makes every comparison exact.

use pump_quant_app::config::{Config, MIN_TRADE_SIZE_LAMPORTS_DEFAULT};
use pump_quant_app::engine::{probe_scale_split, Engine, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_app::journal_log::Decision;
use pump_quant_domain::ids::Mint;

const FLOOR: u64 = MIN_TRADE_SIZE_LAMPORTS_DEFAULT; // 100_000_000 = 0.1 SOL

fn mint(tag: u64) -> Mint {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&tag.to_le_bytes());
    b[8] = 0xAB;
    Mint::from_bytes(b)
}

/// The admitted (entry) order sizes recorded on the journal, in emission order.
fn admitted_sizes(eng: &Engine) -> Vec<u64> {
    eng.journal()
        .recent()
        .filter_map(|d| match *d {
            Decision::Admitted { size_lamports, .. } => Some(size_lamports),
            _ => None,
        })
        .collect()
}

/// A golden-style multi-mint tape over `cfg`: many launches on deep, confirmable
/// low-cap markets with realistic (golden) round-trip economics, so the sizing floor
/// and clamp are exercised across a broad admitted set (not a single hand-picked
/// mint). Deep pools keep a 0.1-SOL floor clip cheap to exit (§34.4).
fn drive_golden_style(mut cfg: Config) -> Engine {
    // Realistic low-cap round-trip (mirror of the golden tape's cost overrides).
    cfg.gate_expected_move_bps = 1_800;
    cfg.gate_protocol_bps = 450;
    cfg.gate_margin_bps = 150;
    cfg.gate_base_fixed_lamports = 200_000;
    cfg.gate_impact_den = 250_000;
    let mut eng = Engine::new(cfg, RunMode::Replay);
    for round in 0..4u64 {
        for m in 0..24u64 {
            for i in 0..8u64 {
                eng.tick(AppEvent::MarketTrade {
                    mint: mint(m),
                    price_fp: 1_000_000_000 + (round as i128) * 40_000_000 + (i as i128) * 500_000,
                    quote_lamports: 700_000,
                    // Deep pool (2–4 SOL of reserve): a ≥0.1-SOL floor clip is a small
                    // fraction of the curve and clears the exit-cost veto.
                    liquidity_lamports: 2_000_000_000 + (m % 8) * 250_000_000,
                    signed_base: 900_000 - (i as i64 * 50),
                    buyer_entity: (m + i) % 31,
                    age_slots: 12 + (m as u32 % 20),
                });
            }
            if round == m % 4 {
                eng.tick(AppEvent::OnchainConfirm {
                    mint: mint(m),
                    sellable_depth_lamports: 2_000_000_000,
                });
            }
        }
        for _ in 0..8 {
            eng.tick(AppEvent::Tick);
        }
    }
    eng
}

// ============================================================================
// LAW 1 (load-bearing): no emitted order is ever below the 0.1-SOL floor.
// ============================================================================
#[test]
fn no_emitted_order_is_below_the_operator_floor() {
    // A funded bankroll so the broad admitted set is exercised (the floor invariant
    // holds for ANY bankroll; funding it just guarantees a non-empty admitted set).
    let mut cfg = Config::dev_portable();
    cfg.bankroll_initial_lamports = 20_000_000_000; // 20 SOL
    assert_eq!(
        cfg.min_trade_size_lamports, FLOOR,
        "the floor default is 0.1 SOL"
    );
    let mut eng = drive_golden_style(cfg);
    let r = eng.report();

    let sizes = admitted_sizes(&eng);
    assert!(
        !sizes.is_empty() && r.admitted > 0,
        "the tape must ADMIT (bankroll trades), got admitted={}",
        r.admitted
    );
    // The load-bearing pin: every admitted entry order is ≥ the floor, AND the
    // probe→scale-in split of each one emits only ≥floor bites.
    for size in &sizes {
        assert!(
            *size >= FLOOR,
            "an admitted order {size} is below the 0.1-SOL operator floor"
        );
        let (probe, scale_add) = probe_scale_split(*size, cfg.probe_frac_bp, FLOOR);
        assert!(
            probe >= FLOOR,
            "probe bite {probe} below floor for target {size}"
        );
        assert!(
            scale_add == 0 || scale_add >= FLOOR,
            "scale-in bite {scale_add} below floor for target {size}"
        );
        assert_eq!(probe + scale_add, *size, "the bites must sum to the target");
    }
    // The sub-x_min paid-information probe (LAW 13) is a SUB-FLOOR bet, so it never
    // fires while the floor is active — no budgeted probes on this tape.
    let (probe_spend, probes) = eng.probe_budget_report();
    assert_eq!(
        probes, 0,
        "no sub-floor probe fires under the 0.1-SOL floor"
    );
    assert_eq!(probe_spend, 0, "no sub-floor probe spend under the floor");
    assert!(
        !eng.journal()
            .recent()
            .any(|d| matches!(*d, Decision::Probe { .. })),
        "no labeled sub-x_min Probe record under the floor"
    );
}

// ============================================================================
// LAW 2: clamp-up-to-floor UNBLOCKS a viable setup whose Kelly size < floor.
// ============================================================================
#[test]
fn clamp_up_to_floor_unblocks_what_refuse_below_xmin_would_block() {
    // A small 2-SOL bankroll: deployable 1.5 SOL, a full base bite ≈ 0.1 SOL, so a
    // reduce-only haircut drops the Kelly size BELOW the floor. The promote valve
    // clamps it UP to the floor and admits; a defeated valve (cap 0) refuses.
    let base = || {
        let mut cfg = Config::dev_portable();
        cfg.bankroll_initial_lamports = 2_000_000_000; // 2 SOL (the operator's start)
                                                       // Neutralize the anti-fade guard so the A/B isolates the promote CAP (the
                                                       // "key unblock"), not the haircut guard.
        cfg.promote_min_haircut_bp = 0;
        cfg
    };
    // Armed: the recalibrated promote cap (0.1 SOL = 6.67% of deployable < the 800bp
    // cap) lets the sub-floor Kelly size clamp UP to the floor.
    let mut armed = drive_golden_style(base());
    let r_armed = armed.report();
    // Defeated: promote cap 0 ⇒ the sub-floor Kelly size can never clamp up (the old
    // strict refuse-below-x_min), so those candidates are blocked.
    let mut ncfg = base();
    ncfg.x_min_promote_cap_bp = 0;
    let mut without = drive_golden_style(ncfg);
    let r_without = without.report();

    assert!(
        r_armed.admitted > r_without.admitted,
        "the clamp-up-to-floor must ADMIT setups the strict refuse-below-x_min blocks \
         (with clamp {} vs without {})",
        r_armed.admitted,
        r_without.admitted
    );
    // Every clamp-admitted order is still ≥ the floor (clamped UP TO it, never below).
    let sizes = admitted_sizes(&armed);
    assert!(!sizes.is_empty());
    for size in sizes {
        assert!(
            size >= FLOOR,
            "a clamp-admitted order {size} is below the floor"
        );
    }
}

// ============================================================================
// LAW 3: a market too thin to take a 0.1 clip (x_max < floor) is REFUSED.
// ============================================================================
#[test]
fn too_thin_market_refuses_rather_than_sizing_below_floor() {
    // A market whose impact-bounded x_max is BELOW the 0.1-SOL floor: a thin impact
    // curve (x_impact ≈ 0.075 SOL) but a DEEP reserve, so the ONLY reason to refuse is
    // that a floor-sized clip does not fit x_max — not the exit-cost veto.
    let run = |floor_on: bool| -> (u64, Vec<u64>) {
        let mut cfg = Config::dev_portable();
        cfg.bankroll_initial_lamports = 20_000_000_000; // funded: base ≫ x_max
        cfg.min_trade_size_lamports = if floor_on { FLOOR } else { 0 };
        // Default gate economics with a SHALLOW impact curve: x_impact = (300−100−50)
        // × 500_000 = 75_000_000 = 0.075 SOL < the 0.1-SOL floor.
        cfg.gate_impact_den = 500_000;
        let mut eng = Engine::new(cfg, RunMode::Replay);
        let m = mint(700);
        for round in 0..3u64 {
            for i in 0..8u64 {
                eng.tick(AppEvent::MarketTrade {
                    mint: m,
                    price_fp: 1_000_000_000 + (round as i128) * 20_000_000 + (i as i128) * 500_000,
                    quote_lamports: 700_000,
                    liquidity_lamports: 10_000_000_000, // deep reserve: exit cost is cheap
                    signed_base: 900_000 - (i as i64 * 40),
                    buyer_entity: i % 9,
                    age_slots: 14,
                });
            }
            if round == 0 {
                eng.tick(AppEvent::OnchainConfirm {
                    mint: m,
                    sellable_depth_lamports: 10_000_000_000,
                });
            }
            for _ in 0..6 {
                eng.tick(AppEvent::Tick);
            }
        }
        let admitted = eng.report().admitted;
        (admitted, admitted_sizes(&eng))
    };
    // Floor ON: x_max (0.075 SOL) < floor (0.1 SOL) ⇒ the band collapses ⇒ REFUSE. No
    // order is emitted (nothing sub-floor, nothing over x_max).
    let (admitted_floor, sizes_floor) = run(true);
    assert_eq!(
        admitted_floor, 0,
        "a market that cannot take a 0.1-SOL clip must be REFUSED, not sized below the floor"
    );
    assert!(
        sizes_floor.is_empty(),
        "no order is emitted for the too-thin market"
    );
    // Floor OFF: the same market DOES admit (at a sub-floor size ≤ x_max) — proving the
    // floor's refuse-if-unsafe is what blocks it, not some unrelated gate.
    let (admitted_nofloor, sizes_nofloor) = run(false);
    assert!(
        admitted_nofloor > 0,
        "with the floor OFF the thin market admits (at a sub-floor size)"
    );
    for size in sizes_nofloor {
        assert!(
            size < FLOOR,
            "without the floor the thin market sizes BELOW 0.1 SOL ({size}) — exactly the \
             sub-floor order the floor exists to forbid"
        );
    }
}

// ============================================================================
// LAW 4: the probe→scale-in split never emits a sub-floor bite.
// ============================================================================
#[test]
fn probe_and_scale_in_bites_are_never_below_the_floor() {
    let frac = Config::dev_portable().probe_frac_bp; // 4000 (40%)
                                                     // A target AT the floor cannot split into two ≥floor bites ⇒ a single ≥floor bite.
    assert_eq!(probe_scale_split(FLOOR, frac, FLOOR), (FLOOR, 0));
    // A target of 1.5×floor: probe clamps up to the floor, the 0.05-SOL remainder is
    // sub-floor ⇒ it folds ⇒ a single ≥floor bite of the whole target.
    let t = FLOOR + FLOOR / 2; // 0.15 SOL
    assert_eq!(probe_scale_split(t, frac, FLOOR), (t, 0));
    // A target of exactly 2×floor splits into two EXACTLY-floor bites.
    assert_eq!(probe_scale_split(2 * FLOOR, frac, FLOOR), (FLOOR, FLOOR));
    // A target of 2.5×floor: probe = max(floor, 40%×2.5floor = floor) = floor; the
    // 1.5×floor remainder is ≥ floor ⇒ two ≥floor bites.
    let big = 2 * FLOOR + FLOOR / 2; // 0.25 SOL
    assert_eq!(probe_scale_split(big, frac, FLOOR), (FLOOR, big - FLOOR));

    // Property sweep: for every target ≥ floor, BOTH emitted bites are ≥ floor (or the
    // scale-in is folded to zero), and they always sum to the target.
    for k in 1..=40u64 {
        let target = FLOOR + k * (FLOOR / 10); // 0.1, 0.11, … 0.5 SOL
        let (probe, scale_add) = probe_scale_split(target, frac, FLOOR);
        assert!(
            probe >= FLOOR,
            "probe {probe} below floor at target {target}"
        );
        assert!(
            scale_add == 0 || scale_add >= FLOOR,
            "scale-in {scale_add} below floor at target {target}"
        );
        assert_eq!(
            probe + scale_add,
            target,
            "bites must sum to target {target}"
        );
        // Below 2×floor a target CANNOT be two ≥floor bites, so it is a single bite.
        if target < 2 * FLOOR {
            assert_eq!(
                scale_add, 0,
                "target {target} < 2×floor must be a single bite"
            );
        }
    }

    // With the floor DISABLED (min_trade_size == 0) the legacy 1-lamport probe minimum
    // is preserved (no folding), so a sub-x_min probe path stays byte-identical.
    assert_eq!(
        probe_scale_split(50_000_000, frac, 0),
        (20_000_000, 30_000_000)
    );
}
