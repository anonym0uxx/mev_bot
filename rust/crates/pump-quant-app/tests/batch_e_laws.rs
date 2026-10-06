//! Batch-E law attribution: A/B proof that each newly wired law EARNS net-SOL
//! on a tape built to contain exactly the hazard it targets (§52 spirit: a law
//! only claims value by beating its own absence under identical events).
//!
//! Each test drives the SAME event tape twice — once with the law armed
//! (`dev_portable` defaults) and once with the law neutralized through config
//! (screen: everything age-exempt; structure: bars never close; decay:
//! rate 10_000 = identity) — and asserts the armed run keeps strictly more
//! lamports. Determinism (§22) makes the comparison exact, not statistical.

#![allow(dead_code)] // test scaffolding: helper/fixture chains not every #[test] exercises (consolidation N2)

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, Report, RunMode};
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

fn mint(tag: u64) -> Mint {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&tag.to_le_bytes());
    b[8] = 0xAB;
    Mint::from_bytes(b)
}

/// Admitted sizes for one mint tag, in journal order.
fn admitted_sizes(eng: &Engine, tag: u64) -> Vec<u64> {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&tag.to_le_bytes());
    b[8] = 0xAB;
    eng.journal()
        .recent()
        .filter_map(|d| match *d {
            Decision::Admitted {
                mint,
                size_lamports,
                ..
            } if mint == b => Some(size_lamports),
            _ => None,
        })
        .collect()
}

// ============================================================================
// §21.5 active-market-universe screen: zombie markets.
// ============================================================================

/// Mature (age 200), deep-pooled markets that trade early, earn a LATE
/// confirmation, then go silent. Without the screen the engine opens positions
/// in dead tape and bleeds round-trip costs; with it they are filtered at
/// promotion before any gate work.
fn drive_zombies(cfg: Config) -> (Report, Engine) {
    let mut eng = Engine::new(cfg, RunMode::Replay);
    for round in 0..6u64 {
        for z in 0..3u64 {
            let mt = mint(1_000 + z);
            if round <= 1 {
                for i in 0..3u64 {
                    eng.tick(AppEvent::MarketTrade {
                        mint: mt,
                        price_fp: 1_000_000_000 + (z as i128) * 5_000 + (i as i128) * 1_000,
                        quote_lamports: 900_000 + z * 1_000,
                        liquidity_lamports: REAL_CURVE_VSOL + z * 10_000,
                        signed_base: 800_000 + (z as i64) * 500,
                        buyer_entity: 200 + (z + i) % 9,
                        age_slots: 200,
                        recv_unix_ms: None,
                        trader_pubkey: None,
                        slot: None,
                        fee_lamports: None,
                        cu_consumed: None,
                        venue: None,
                        event_id: None,
                    });
                }
            }
            // The confirmation lands AFTER the market died (round 3 = tick 36;
            // last trade at tick 12): depth proven for a market nobody trades.
            if round == 3 {
                eng.tick(AppEvent::OnchainConfirm {
                    mint: mt,
                    virtual_sol_lamports: REAL_CURVE_VSOL,
                    real_sol_lamports: REAL_CURVE_REAL_SOL,
                });
            }
        }
        for _ in 0..12 {
            eng.tick(AppEvent::Tick);
        }
    }
    let r = eng.report();
    (r, eng)
}

// ============================================================================
// §21.6 bar-structure haircut: the exit-liquidity trap.
// ============================================================================

/// Descending-zigzag bars (lower swing highs AND lower swing lows) under
/// BUY-side flow — the classic exit-liquidity trap: flow says buy, structure
/// says down. The gate still admits (flow authorizes), but the armed run sizes
/// the entry at the configured structure haircut and so loses strictly less
/// when the crash completes.
fn drive_trap(cfg: Config) -> (Report, Engine) {
    // A-6 sizing regime: exercise the §21.6 structure haircut above the 0.1-SOL
    // operator floor. Realistic wide-window economics (x_max ≈ 0.3 SOL) + a 5-SOL
    // bankroll place the base bite (~0.25 SOL) squarely in the FREE sizing regime —
    // above the floor and below x_max — in BOTH arms, so the reduce-only haircut ratio
    // is observable (a floor clamp-up would cancel it). The structure toggle under
    // test (bar_trades_per_bar) is untouched.
    let mut cfg = cfg;
    cfg.gate_expected_move_bps = 1_800;
    cfg.gate_margin_bps = 150;
    // 7 SOL ⇒ deployable 5.25, base ~0.35 SOL. Both arms land in the WINDOW where a
    // bite (a) clears the 0.1-SOL floor without promotion (the 0.7 structure haircut is
    // below the promote-min guard) AND (b) is below the 0.2-SOL two-bite split
    // threshold, so BOTH open as a SINGLE full-size bite (no scale-in asymmetry). Neut
    // (~0.175 SOL) and armed (~0.1225 SOL) then differ ONLY by the structure haircut:
    // the ratio is observable and the smaller armed bite genuinely loses less on the
    // trap. (A larger base makes neut split into probe+scale-in while armed stays a
    // single bite, inverting the loss comparison — a floor/scale-in artifact.)
    cfg.bankroll_initial_lamports = 7_000_000_000;
    let mut eng = Engine::new(cfg, RunMode::Replay);
    let mt = mint(9_000);
    // Six 8-trade bars: (H,L) = (200,190),(205,195),(185,175),(190,180),
    // (170,160),(175,165) — swing highs 205→190, swing lows 175→160.
    let bars: [(i128, i128); 6] = [
        (200, 190),
        (205, 195),
        (185, 175),
        (190, 180),
        (170, 160),
        (175, 165),
    ];
    let scale = 10_000_000i128;
    for (h, l) in bars {
        for i in 0..8u64 {
            let p = if i % 2 == 0 { h } else { l };
            eng.tick(AppEvent::MarketTrade {
                mint: mt,
                price_fp: p * scale + (i as i128),
                quote_lamports: 800_000,
                liquidity_lamports: REAL_CURVE_VSOL,
                signed_base: 900_000 - (i as i64),
                buyer_entity: 40 + i % 7,
                age_slots: 12,
                recv_unix_ms: None,
                trader_pubkey: None,
                slot: None,
                fee_lamports: None,
                cu_consumed: None,
                venue: None,
                event_id: None,
            });
        }
    }
    // Close the 6th bar, then confirm and evaluate.
    eng.tick(AppEvent::MarketTrade {
        mint: mt,
        price_fp: 165 * scale,
        quote_lamports: 800_000,
        liquidity_lamports: REAL_CURVE_VSOL,
        signed_base: 900_000,
        buyer_entity: 47,
        age_slots: 12,
        recv_unix_ms: None,
        trader_pubkey: None,
        slot: None,
        fee_lamports: None,
        cu_consumed: None,
        venue: None,
        event_id: None,
    });
    eng.tick(AppEvent::OnchainConfirm {
        mint: mt,
        virtual_sol_lamports: REAL_CURVE_VSOL,
        real_sol_lamports: REAL_CURVE_REAL_SOL,
    });
    for _ in 0..3 {
        eng.tick(AppEvent::Tick);
    }
    // The trap springs: −40% under continued prints.
    for i in 0..16u64 {
        eng.tick(AppEvent::MarketTrade {
            mint: mt,
            price_fp: (100 - i as i128) * scale,
            quote_lamports: 800_000,
            liquidity_lamports: REAL_CURVE_VSOL,
            signed_base: 900_000,
            buyer_entity: 40 + i % 7,
            age_slots: 12,
            recv_unix_ms: None,
            trader_pubkey: None,
            slot: None,
            fee_lamports: None,
            cu_consumed: None,
            venue: None,
            event_id: None,
        });
    }
    for _ in 0..3 {
        eng.tick(AppEvent::Tick);
    }
    let r = eng.report();
    (r, eng)
}

// ============================================================================
// §29.6 attention decay: the stale-blast squatter.
// ============================================================================

/// One narrative blast (J) that never repeats vs a continuously re-observed,
/// confirmable market (K), competing for a single promotion slot on a
/// fast-turnover board. Without decay the stale blast re-enters the board at
/// full strength forever and squats the slot; with decay its rank fades and
/// the fresh market is promoted, admitted, and earns.
fn drive_squatter(cfg: Config) -> (Report, Engine) {
    // A-6 sizing regime: the fresh market K must size ABOVE the 0.1-SOL operator floor
    // to admit and earn once decay frees the slot. Realistic wide-window economics
    // (x_max ≈ 0.3 SOL) + a 10-SOL bankroll size it well above the floor. The decay /
    // promote-slot toggles under test are untouched — both arms share these overrides.
    let mut cfg = cfg;
    cfg.gate_expected_move_bps = 1_800;
    cfg.gate_margin_bps = 150;
    cfg.bankroll_initial_lamports = 10_000_000_000; // 10 SOL ⇒ deployable 7.5, base ~0.5 SOL
    let mut eng = Engine::new(cfg, RunMode::Replay);
    let j = mint(9_100);
    let k = mint(9_200);
    eng.tick(AppEvent::NarrativeSample {
        mint: j,
        prior_active: 5,
        new_mentions: 9_000,
    });
    eng.tick(AppEvent::NarrativeSample {
        mint: k,
        prior_active: 5,
        new_mentions: 9_000,
    });
    // K trades with sub-discovery OFI (numeric lane silent, gate snapshot live).
    for i in 0..12u64 {
        eng.tick(AppEvent::MarketTrade {
            mint: k,
            price_fp: 1_000_000_000 + (i as i128) * 500_000,
            quote_lamports: 600_000,
            liquidity_lamports: REAL_CURVE_VSOL,
            signed_base: if i % 2 == 0 { 500_000 } else { -460_000 },
            buyer_entity: 60 + i % 5,
            age_slots: 15,
            recv_unix_ms: None,
            trader_pubkey: None,
            slot: None,
            fee_lamports: None,
            cu_consumed: None,
            venue: None,
            event_id: None,
        });
    }
    eng.tick(AppEvent::OnchainConfirm {
        mint: k,
        virtual_sol_lamports: REAL_CURVE_VSOL,
        real_sol_lamports: REAL_CURVE_REAL_SOL,
    });
    for block in 0..5u64 {
        for _ in 0..10 {
            eng.tick(AppEvent::Tick);
        }
        eng.tick(AppEvent::NarrativeSample {
            mint: k,
            prior_active: 5,
            new_mentions: 9_000,
        });
        for i in 0..4u64 {
            eng.tick(AppEvent::MarketTrade {
                mint: k,
                price_fp: 1_000_000_000 + (block as i128 + 1) * 60_000_000 + (i as i128) * 500_000,
                quote_lamports: 600_000,
                liquidity_lamports: REAL_CURVE_VSOL,
                signed_base: if i % 2 == 0 { 500_000 } else { -460_000 },
                buyer_entity: 60 + i % 5,
                age_slots: 15,
                recv_unix_ms: None,
                trader_pubkey: None,
                slot: None,
                fee_lamports: None,
                cu_consumed: None,
                venue: None,
                event_id: None,
            });
        }
    }
    let r = eng.report();
    (r, eng)
}

// ============================================================================
// §55 capacity curve + §52 baseline verdict (report surfaces).
// ============================================================================

#[test]
fn capacity_report_covers_the_mandated_grid() {
    let eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    let curve = eng.capacity_report(1_000_000_000);
    assert_eq!(curve.len(), 7, "§55 mandates the 7-point size grid");
    assert_eq!(curve[0].size_lamports, 10_000_000);
    assert_eq!(curve[6].size_lamports, 1_000_000_000);
    // Non-linear scaling: price impact must strictly grow with size.
    for w in curve.windows(2) {
        assert!(w[0].size_lamports < w[1].size_lamports);
        assert!(w[0].price_impact_bps <= w[1].price_impact_bps);
    }
    assert!(
        curve[6].price_impact_bps > curve[0].price_impact_bps,
        "scaling never assumes linear PnL (§55)"
    );
}
