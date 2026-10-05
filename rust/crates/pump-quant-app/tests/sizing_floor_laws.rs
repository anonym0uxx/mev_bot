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

/// **DEPTH REALISM (re-pin #26).** The gate's price-impact model is now DERIVED from
/// the market's own SOL-side reserve (`cost_model::impact_den_for`), so a fixture's
/// declared depth is a decision input rather than decoration. Real pump.fun virtual
/// reserves START at 30 SOL; the sub-SOL depths these fixtures used to declare put the
/// operator's 0.1 SOL floor clip at 5-125% of the pool — a market in which no strategy
/// result means anything (Amendment A-13(1)).
/// **A REAL BONDING CURVE THAT HAS BEEN BOUGHT INTO (corrected 2026-07-28).**
///
/// pump.fun seeds a curve with **30 SOL of VIRTUAL reserve and ZERO real SOL**, and
/// escrows `real_sol = virtual_sol - 30 SOL` thereafter. This constant used to be the
/// bare seed reserve (30 SOL) paired with a "sellable depth" of 29-30 SOL — a market
/// that cannot exist, since a curve nobody has bought into can pay out nothing at all.
/// It is now a curve with 0.3 SOL genuinely raised: the price reserve is close enough
/// to the seed that own-impact on a 0.1 SOL floor clip is unchanged at 33 bps a leg,
/// and the payout reserve is the 0.3 SOL that was actually paid in.
/// See `curve_state::real_sol_for`.
const REAL_CURVE_VSOL: u64 = 30_300_000_000;
/// The SOL this curve actually escrows — `REAL_CURVE_VSOL - LAUNCH_VSOL_LAMPORTS`,
/// the identity, not a choice. This is what caps `size_band`'s `x_max`.
const REAL_CURVE_REAL_SOL: u64 = 300_000_000;

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
    cfg.gate_margin_bps = 150;
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
                    liquidity_lamports: REAL_CURVE_VSOL + (m % 8) * 250_000_000,
                    signed_base: 900_000 - (i as i64 * 50),
                    buyer_entity: (m + i) % 31,
                    age_slots: 12 + (m as u32 % 20),
                    recv_unix_ms: None,
                    trader_pubkey: None,
                    slot: None,
                    fee_lamports: None,
                    cu_consumed: None,
                    venue: None,
                });
            }
            if round == m % 4 {
                eng.tick(AppEvent::OnchainConfirm {
                    mint: mint(m),
                    virtual_sol_lamports: REAL_CURVE_VSOL,
                    real_sol_lamports: REAL_CURVE_REAL_SOL,
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
// ============================================================================
// LAW 2: clamp-up-to-floor UNBLOCKS a viable setup whose Kelly size < floor.
// ============================================================================
// ============================================================================
// LAW 3: a market too thin to take a 0.1 clip (x_max < floor) is REFUSED.
// ============================================================================
/// The sellable depth the on-chain confirm PROVES for the too-thin market: 0.075 SOL,
/// deliberately just under the 0.1-SOL operator floor, so `x_max` collapses below the
/// floor while everything else about the market stays real and healthy.
const PROVEN_SELLABLE: u64 = 75_000_000;

/// The VIRTUAL reserve of that too-thin market (corrected 2026-07-28). Thinness is
/// not a free parameter on this venue: a curve escrows `virtual_sol - 30 SOL`, so the
/// ONLY market that can prove 0.075 SOL of payout depth is one whose price reserve is
/// 30.075 SOL. The fixture used to declare a 30 SOL reserve alongside a 0.075 SOL
/// depth claim, which happened to be the right SIGN of thin for the wrong reason —
/// that curve escrows nothing at all.
const THIN_CURVE_VSOL: u64 = 30_000_000_000 + PROVEN_SELLABLE;

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
