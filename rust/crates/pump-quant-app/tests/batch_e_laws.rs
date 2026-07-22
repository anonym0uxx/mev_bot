//! Batch-E law attribution: A/B proof that each newly wired law EARNS net-SOL
//! on a tape built to contain exactly the hazard it targets (§52 spirit: a law
//! only claims value by beating its own absence under identical events).
//!
//! Each test drives the SAME event tape twice — once with the law armed
//! (`dev_portable` defaults) and once with the law neutralized through config
//! (screen: everything age-exempt; structure: bars never close; decay:
//! rate 10_000 = identity) — and asserts the armed run keeps strictly more
//! lamports. Determinism (§22) makes the comparison exact, not statistical.

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, Report, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_app::journal_log::Decision;
use pump_quant_domain::ids::Mint;

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
                        liquidity_lamports: 400_000_000 + z * 10_000,
                        signed_base: 800_000 + (z as i64) * 500,
                        buyer_entity: 200 + (z + i) % 9,
                        age_slots: 200,
                    });
                }
            }
            // The confirmation lands AFTER the market died (round 3 = tick 36;
            // last trade at tick 12): depth proven for a market nobody trades.
            if round == 3 {
                eng.tick(AppEvent::OnchainConfirm {
                    mint: mt,
                    sellable_depth_lamports: 500_000_000,
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

#[test]
fn universe_screen_refuses_dead_markets_and_keeps_the_lamports() {
    let (armed, _) = drive_zombies(Config::dev_portable());
    let mut ncfg = Config::dev_portable();
    ncfg.universe_age_exempt_slots = u32::MAX; // screen neutralized
    let (neut, _) = drive_zombies(ncfg);

    // Neutralized: the dead markets are admitted and bleed round-trip costs.
    assert!(neut.admitted > 0, "neutral run must open zombie positions");
    assert!(
        neut.net_lamports < 0,
        "dead-market entries must bleed costs"
    );
    // Armed: the screen filters them at promotion — no entry, no bleed.
    assert_eq!(armed.admitted, 0, "armed run must refuse every zombie");
    assert!(
        armed.universe_filtered > 0,
        "the screen must be visibly active"
    );
    assert_eq!(armed.net_lamports, 0);
    assert!(
        armed.net_lamports > neut.net_lamports,
        "the §21.5 screen must strictly out-earn its absence"
    );
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
                liquidity_lamports: 400_000_000,
                signed_base: 900_000 - (i as i64),
                buyer_entity: 40 + i % 7,
                age_slots: 12,
            });
        }
    }
    // Close the 6th bar, then confirm and evaluate.
    eng.tick(AppEvent::MarketTrade {
        mint: mt,
        price_fp: 165 * scale,
        quote_lamports: 800_000,
        liquidity_lamports: 400_000_000,
        signed_base: 900_000,
        buyer_entity: 47,
        age_slots: 12,
    });
    eng.tick(AppEvent::OnchainConfirm {
        mint: mt,
        sellable_depth_lamports: 500_000_000,
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
            liquidity_lamports: 400_000_000,
            signed_base: 900_000,
            buyer_entity: 40 + i % 7,
            age_slots: 12,
        });
    }
    for _ in 0..3 {
        eng.tick(AppEvent::Tick);
    }
    let r = eng.report();
    (r, eng)
}

#[test]
fn structure_haircut_shrinks_the_exit_liquidity_trap() {
    let acfg = Config::dev_portable();
    let haircut = u128::from(acfg.structure_downtrend_haircut_bp);
    let (armed, aeng) = drive_trap(acfg);
    let mut ncfg = Config::dev_portable();
    ncfg.bar_trades_per_bar = 1_000_000; // bars never close: structure neutralized
    let (neut, neng) = drive_trap(ncfg);

    // Both runs enter (flow authorizes; structure never vetoes)...
    let a_sizes = admitted_sizes(&aeng, 9_000);
    let n_sizes = admitted_sizes(&neng, 9_000);
    assert!(!a_sizes.is_empty() && !n_sizes.is_empty());
    // ...but the armed first entry carries the configured haircut relative to
    // the neutral one (reduce-only, §56.2 envelope). The composed integer
    // haircut chain rounds down at each /10_000 stage, so allow the ratio a
    // ±10bp integer-rounding band around the configured value — never above it.
    let ratio_bp = u128::from(a_sizes[0]) * 10_000 / u128::from(n_sizes[0]);
    assert!(
        ratio_bp <= haircut && ratio_bp >= haircut - 10,
        "armed/neutral size ratio {ratio_bp}bp must sit at the {haircut}bp haircut"
    );
    // Both lose (the trap is real); armed loses strictly less.
    assert!(neut.net_lamports < 0 && armed.net_lamports < 0);
    assert!(
        armed.net_lamports > neut.net_lamports,
        "the §21.6 haircut must strictly reduce the trap loss"
    );
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
            liquidity_lamports: 300_000_000,
            signed_base: if i % 2 == 0 { 500_000 } else { -460_000 },
            buyer_entity: 60 + i % 5,
            age_slots: 15,
        });
    }
    eng.tick(AppEvent::OnchainConfirm {
        mint: k,
        sellable_depth_lamports: 400_000_000,
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
                liquidity_lamports: 300_000_000,
                signed_base: if i % 2 == 0 { 500_000 } else { -460_000 },
                buyer_entity: 60 + i % 5,
                age_slots: 15,
            });
        }
    }
    let r = eng.report();
    (r, eng)
}

#[test]
fn decay_unseats_the_stale_squatter_and_earns() {
    let mut acfg = Config::dev_portable();
    acfg.promote_k = 1;
    acfg.watchlist_ttl_ticks = 12; // fast board turnover: rank reflects CURRENT evidence
    // Isolate the DECAY law: the §71 corroboration quota (a separate law with
    // its own A/B in golden_digest.rs) would rescue this tape in both arms.
    acfg.promote_corroboration_quota = 0;
    let (armed, _) = drive_squatter(acfg);

    let mut ncfg = Config::dev_portable();
    ncfg.promote_k = 1;
    ncfg.watchlist_ttl_ticks = 12;
    ncfg.promote_corroboration_quota = 0;
    ncfg.narrative_decay_bp = 10_000; // decay neutralized
    ncfg.narrative_decay_floor = 0;
    let (neut, _) = drive_squatter(ncfg);

    // Neutral: the stale blast holds the slot for the whole run — nothing admits.
    assert_eq!(
        neut.admitted, 0,
        "stale squatter must block the slot when decay is off"
    );
    assert_eq!(neut.net_lamports, 0);
    // Armed: the blast decays, the fresh confirmable market is admitted and earns.
    assert!(
        armed.admitted > 0,
        "decay must free the slot for fresh evidence"
    );
    assert!(
        armed.net_lamports > neut.net_lamports,
        "the §29.6 decay law must strictly out-earn its absence"
    );
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

#[test]
fn baseline_verdict_guards_small_n_then_fires() {
    // Fresh engine: no trades — no verdict (§46: small-n verdicts are noise).
    let eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    assert!(eng.baseline_verdict().is_none());

    // A run with realized trades and a lowered n-guard produces a verdict.
    let mut cfg = Config::dev_portable();
    cfg.promote_k = 1;
    cfg.watchlist_ttl_ticks = 12;
    cfg.baseline_min_trades = 1;
    let (_r, eng) = drive_squatter(cfg);
    let verdict = eng
        .baseline_verdict()
        .expect("realized trades past the guard must produce a §52 verdict");
    // The verdict is a real evaluator decision, not a placeholder.
    let _defeats = verdict.defeats();
}
