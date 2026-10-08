//! Wave-2 law attribution: A/B proof that each newly wired Wave-2 decision-path
//! law changes the audited outcome in the mandated direction on a tape built to
//! contain exactly the hazard it targets. Mirrors `batch_e_laws.rs` exactly —
//! isolate the law with a config toggle, drive the SAME event tape twice (armed
//! vs neutralized), and assert the armed arm strictly wins in the law's own
//! axis. Determinism (§22) makes the comparison exact, not statistical.
//!
//! ## §26 confirmed-creator-dump HARD VETO + held-position exit
//! (operator-approved operator reversal of the prior "creator distribution
//! is fade-only, never a veto" behaviour). Two limbs, two isolated A/Bs:
//!   * PRE-ENTRY: a market whose deployer has already distributed past the veto
//!     threshold is refused before entry (a NEW reject code, 13) — avoiding the
//!     loss of buying into a confirmed dump.
//!   * HELD-POSITION: a deployer that dumps *after* the position is open forces
//!     the exit at the current mark — banking the position before the crater
//!     instead of riding it into the hard stop.
//!
//! For a veto/forced-exit law the axis is loss AVOIDED (§52 spirit): the armed
//! arm must keep strictly more lamports than the arm that ignores the dump.

#![allow(dead_code)] // test scaffolding: helper/fixture chains not every #[test] exercises (consolidation N2)

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, Report, RunMode};
use pump_quant_app::event::{AppEvent, CreatorActionKind};
use pump_quant_app::journal_log::Decision;
use pump_quant_domain::ids::Mint;
use pump_quant_watchlist::candidate::{DiscoveryLane, Lane as WlLane};

/// **DEPTH REALISM (re-pin #26).** The gate's price-impact model is now DERIVED from
/// the market's own SOL-side reserve (`cost_model::impact_den_for`), so a fixture's
/// declared depth is a decision input rather than decoration. Real pump.fun virtual
/// reserves START at 30 SOL; the sub-SOL depths these fixtures used to declare put the
/// operator's 0.1 SOL floor clip at 20-125% of the pool — a market in which no
/// strategy result means anything (Amendment A-13(1)).
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

/// Count journalled rejections carrying `reason` for one mint tag.
fn reject_count(eng: &Engine, tag: u64, reason: u8) -> usize {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&tag.to_le_bytes());
    b[8] = 0xAB;
    eng.journal()
        .recent()
        .filter(
            |d| matches!(**d, Decision::Rejected { mint, reason: r } if mint == b && r == reason),
        )
        .count()
}

const M: u64 = 9_500;
const PRICE_SCALE: i128 = 10_000_000;

/// Emit `n` net-buy trades at a rising price for mint `M`, opening a long thesis.
fn pump(eng: &mut Engine, base_mult: i128, n: u64) {
    for i in 0..n {
        eng.tick(AppEvent::MarketTrade {
            mint: mint(M),
            price_fp: (base_mult + i as i128) * PRICE_SCALE,
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
            feature: None,
        });
    }
}

/// Emit `n` net-sell trades at a crashing price for mint `M` (the dump plays out).
fn crater(eng: &mut Engine, top_mult: i128, n: u64) {
    for i in 0..n {
        eng.tick(AppEvent::MarketTrade {
            mint: mint(M),
            price_fp: (top_mult - (i as i128) * 4) * PRICE_SCALE,
            quote_lamports: 800_000,
            liquidity_lamports: REAL_CURVE_VSOL,
            signed_base: -900_000,
            buyer_entity: 40 + i % 7,
            age_slots: 12,
            recv_unix_ms: None,
            trader_pubkey: None,
            slot: None,
            fee_lamports: None,
            cu_consumed: None,
            venue: None,
            event_id: None,
            feature: None,
        });
    }
}

// ============================================================================
// §26 held-position limb: a deployer that dumps AFTER the position is open.
// ============================================================================

/// A market that pumps and opens a position, then the deployer distributes >60%
/// of peak at the top, then the price craters. Armed: the dump forces the exit
/// at the top; neutralized: the position rides the crater into the hard stop.
fn drive_held_dump(cfg: Config) -> (Report, Engine) {
    // A-6 sizing regime: exercise the §26 held-exit above the 0.1-SOL operator floor.
    // Realistic wide-window economics (x_max ≈ 0.3 SOL, like the golden tape) + a
    // bankroll that sizes the position well above the floor, so the held-exit vs
    // ride-the-crater A/B is not clipped by the narrow default sizing window. The §26
    // toggle under test is untouched — both arms share these overrides.
    let mut cfg = cfg;
    cfg.gate_expected_move_bps = 1_800;
    cfg.gate_margin_bps = 150;
    cfg.bankroll_initial_lamports = 10_000_000_000; // 10 SOL ⇒ deployable 7.5, base ~0.5 SOL
    let mut eng = Engine::new(cfg, RunMode::Replay);
    // Deployer initializes holding the full supply (no sells yet — not a dump).
    eng.tick(AppEvent::CreatorAction {
        mint: mint(M),
        kind: CreatorActionKind::Init {
            initial_tokens: 1_000_000_000,
            total_supply: 1_000_000_000,
        },
        slot: 1,
    });
    // Pump up over three 8-trade bars, confirm depth, open the position.
    pump(&mut eng, 100, 24);
    eng.tick(AppEvent::OnchainConfirm {
        mint: mint(M),
        virtual_sol_lamports: REAL_CURVE_VSOL,
        real_sol_lamports: REAL_CURVE_REAL_SOL,
    });
    for _ in 0..3 {
        eng.tick(AppEvent::Tick);
    }
    // The deployer DUMPS 70% of peak while the price is still at the top.
    eng.tick(AppEvent::CreatorAction {
        mint: mint(M),
        kind: CreatorActionKind::Sell {
            tokens: 700_000_000,
            quote_lamports: 500_000_000,
        },
        slot: 100,
    });
    // The dump plays out on-chain: price craters.
    crater(&mut eng, 123, 20);
    for _ in 0..6 {
        eng.tick(AppEvent::Tick);
    }
    let r = eng.report();
    (r, eng)
}

// ============================================================================
// §26 pre-entry limb: a deployer already distributing when the market is gated.
// ============================================================================

/// The deployer has ALREADY dumped >60% of peak before the market pumps. Armed:
/// the pre-entry gate refuses it (reject code 13) — no position, no loss.
/// Neutralized: it is admitted and then the crash books a loss.
fn drive_preentry_dump(cfg: Config) -> (Report, Engine) {
    let mut eng = Engine::new(cfg, RunMode::Replay);
    eng.tick(AppEvent::CreatorAction {
        mint: mint(M),
        kind: CreatorActionKind::Init {
            initial_tokens: 1_000_000_000,
            total_supply: 1_000_000_000,
        },
        slot: 1,
    });
    // Confirmed distribution BEFORE any entry decision.
    eng.tick(AppEvent::CreatorAction {
        mint: mint(M),
        kind: CreatorActionKind::Sell {
            tokens: 700_000_000,
            quote_lamports: 500_000_000,
        },
        slot: 2,
    });
    // The tape still shows buy pressure (retail bid into the dump), then crashes.
    pump(&mut eng, 100, 24);
    eng.tick(AppEvent::OnchainConfirm {
        mint: mint(M),
        virtual_sol_lamports: REAL_CURVE_VSOL,
        real_sol_lamports: REAL_CURVE_REAL_SOL,
    });
    for _ in 0..3 {
        eng.tick(AppEvent::Tick);
    }
    crater(&mut eng, 123, 20);
    for _ in 0..6 {
        eng.tick(AppEvent::Tick);
    }
    let r = eng.report();
    (r, eng)
}

// ============================================================================
// Batch-2a exit-mechanics hazards (§24 / §24(d)).
//
// The live engine force-exits a held scalp on the first *net-buy* print (its §32
// momentum-invalidation thesis, code 3), so these hazards DISCOVER the market via
// the narrative lane (the §71 corroboration quota promotes it) and drive a
// net-SELL numeric tape — where the §32 thesis stays quiet — so the position is
// genuinely HELD and the Batch-2a exit MECHANIC is the only thing that differs
// between the arms. Determinism (§22) makes each comparison exact.
// ============================================================================

/// Advance the logical clock by `n` ticks (each runs a full evaluate()).
fn ticks(eng: &mut Engine, n: u64) {
    for _ in 0..n {
        eng.tick(AppEvent::Tick);
    }
}

/// One trade at `price_mult × PRICE_SCALE` with the given signed base flow and a
/// rotating buyer entity (so the §21.5 wash guard never filters the tape).
fn one(eng: &mut Engine, price_mult: i128, signed_base: i64, entity: u64) {
    eng.tick(AppEvent::MarketTrade {
        mint: mint(M),
        price_fp: price_mult * PRICE_SCALE,
        quote_lamports: 800_000,
        liquidity_lamports: REAL_CURVE_VSOL,
        signed_base,
        buyer_entity: entity,
        age_slots: 12,
        recv_unix_ms: None,
        trader_pubkey: None,
        slot: None,
        fee_lamports: None,
        cu_consumed: None,
        venue: None,
        event_id: None,
        feature: None,
    });
}

/// A narrative blast for mint `M` (drives corroboration-lane discovery).
fn narrate(eng: &mut Engine) {
    eng.tick(AppEvent::NarrativeSample {
        mint: mint(M),
        prior_active: 5,
        new_mentions: 9_000,
    });
}

/// Count journalled realized exits carrying `reason` (the ExitReason code).
fn fill_count(eng: &Engine, reason: u8) -> usize {
    eng.journal()
        .recent()
        .filter(|d| matches!(**d, Decision::Filled { reason: r, .. } if r == reason))
        .count()
}

/// Total realized net across journalled exits carrying `reason`.
fn fill_net(eng: &Engine, reason: u8) -> i128 {
    eng.journal()
        .recent()
        .filter_map(|d| match *d {
            Decision::Filled {
                reason: r,
                net_pnl_lamports,
                ..
            } if r == reason => Some(net_pnl_lamports),
            _ => None,
        })
        .sum()
}

/// Discover mint `M` through the narrative lane and open a scalp on a net-SELL
/// numeric snapshot at `entry` (so §32 stays quiet). `disc_prices` is the discovery
/// tape (volatile → high realized vol for the vol-stop hazard; flat otherwise);
/// its last element is the entry mark. Leaves the position OPEN.
fn seed_open(eng: &mut Engine, disc_prices: &[i128]) {
    for _ in 0..4 {
        narrate(eng);
    }
    for (i, &p) in disc_prices.iter().enumerate() {
        one(eng, p, -500_000, 60 + (i as u64 % 7));
    }
    eng.tick(AppEvent::OnchainConfirm {
        mint: mint(M),
        virtual_sol_lamports: REAL_CURVE_VSOL,
        real_sol_lamports: REAL_CURVE_REAL_SOL,
    });
    narrate(eng);
    ticks(eng, 2); // admit
}

// ---- LAW 2 — §24 cost-derived profit targets ------------------------------
//
// The position grinds UP ~+18% on distribution (rising price, net-SELL flow), then
// craters. The fixed ladder's first rung is +35% (tp1 = 13_500) — never reached, so
// it banks nothing and rides the crater to a trailing stop deep below entry. The
// §24 cost-derived ladder prices tp1 from the market's own round-trip cost, clamped
// up to the +10% envelope floor — a rung the grind DOES clear — so it recovers
// principal near the top before the crater. The cost-derived ladder keeps more.

fn drive_derived(cfg: Config) -> Report {
    let mut eng = Engine::new(cfg, RunMode::Replay);
    seed_open(&mut eng, &[120; 10]); // entry ≈ 120
                                     // Grind UP to ~+18% on net-SELL flow: §32 quiet, the ladder decides the exit.
    for i in 0..22u64 {
        one(&mut eng, 121 + i as i128, -600_000, 50 + i % 7);
    }
    // Crater back down through entry on continued sells.
    for i in 0..30u64 {
        one(
            &mut eng,
            (142 - 2 * i as i128).max(70),
            -800_000,
            30 + i % 7,
        );
    }
    ticks(&mut eng, 6);
    eng.report()
}

// ---- LAW 5 — §24(d) exit-into-strength ------------------------------------
//
// The position is held through a slow SELL baseline at a rising price (§32 quiet),
// then a fast, plateaued BUY-arrival burst — a genuine climax — prints while in
// profit. Because the running OFI is deeply net-sell, that lone buy does NOT trip
// the §32 thesis, so the neutral arm rides PAST the climax and exhausts into the
// fade; the armed arm sells INTO the climax (ExitReason::IntoStrength, code 9) at
// the local top. The into-strength exit both changes the audited exit AND keeps
// strictly more lamports.

fn drive_climax(eng: &mut Engine) -> Report {
    seed_open(eng, &[120; 10]); // entry ≈ 120
                                // BASELINE: net-SELL prints at a slowly RISING price (in profit, new highs so no
                                // stall) spaced ~4 ticks apart — a slow baseline arrival rate.
    let mut p: i128 = 121;
    let mut ent: u64 = 50;
    for _ in 0..4 {
        one(eng, p, -400_000, ent);
        ticks(eng, 4);
        p += 1;
        ent += 1;
    }
    // Two fast SELLs 1 tick apart (drive the recent AND prior arrival gap to 1),
    // then the BUY CLIMAX at the same fast cadence: recent gap == prior gap == 1,
    // strongly elevated over the slow baseline → a plateaued climax.
    one(eng, p, -400_000, ent); // gap 4 (from baseline)
    ticks(eng, 1);
    p += 1;
    ent += 1;
    one(eng, p, -400_000, ent); // gap 1 → prior gap := 1
    ticks(eng, 1);
    p += 1;
    ent += 1;
    one(eng, p, 900_000, ent); // buy-side climax print (gap 1, in profit ≈ +6%)
    ticks(eng, 1);
    // FADE: the buyers exhaust; the price drifts back below entry on sells.
    for i in 0..24u64 {
        one(eng, (p - 3 - i as i128).max(100), -600_000, 80 + i % 7);
        ticks(eng, 1);
    }
    ticks(eng, 6);
    eng.report()
}

// ---- LAW 6 — §24 volatility-scaled stops/trail ----------------------------
//
// A HIGH-volatility market (volatile discovery bars → large realized vol) whose
// price then bleeds ~28% off the peak on SELL flow (§32 quiet), then recovers. The
// fixed 22% trail stops out at the bleed low; the vol-scaled trail — widened past
// 22% by the measured volatility, inside the envelope — survives the bleed, so the
// position rides the recovery and is marked out strictly higher.

fn drive_bleed(cfg: Config) -> Report {
    let mut eng = Engine::new(cfg, RunMode::Replay);
    // Volatile discovery (net-SELL flow) whose 8-trade-bar CLOSES rise across bars
    // (155, 185, 215) so realized_vol_bps is large; the last trade (215) is the
    // entry/peak mark.
    let disc: Vec<i128> = (0..24i128)
        .map(|i| 120 + (i / 8) * 30 + (i % 8) * 5)
        .collect();
    seed_open(&mut eng, &disc); // entry ≈ 215, high vol
                                // Gradual SELL bleed to ~0.72× peak (each step < the 30% precursor): 215 → 190 →
                                // 170 → 155. The fixed 22% trail stops at 155; the vol-scaled trail survives it.
    for &p in &[190i128, 170, 155] {
        one(&mut eng, p, -700_000, 60 + (p as u64 % 7));
    }
    // Recovery on continued sells (rising price keeps §32 quiet): the surviving
    // (armed) position is marked out here; the neutral one already trailed at 155.
    for &p in &[170i128, 190, 205] {
        one(&mut eng, p, -600_000, 40 + (p as u64 % 7));
    }
    ticks(&mut eng, 4);
    eng.report()
}

// ============================================================================
// Batch-2b LAW 3 — §71.2 discovery-lane attribution (reflection integrity).
//
// Two markets that both present as the `CreationSniper` setup archetype but arrive
// through DIFFERENT independent discovery lanes — one an on-chain creation sighting
// (`TokenMetadata`), one a social caller (`SocialCall`) — open and close. The
// legacy archetype-keyed ledger lumps both into the single `CreationSniper` slot
// (the cross-contamination §71.2 targets); the discovery-lane ledger attributes
// each to its OWN lane (`OnchainCreation` vs `SocialCaller`). Both markets carry a
// net-SELL numeric snapshot, so the self-authorizing numeric lane never emits and
// overrides the corroboration lane's provenance. Determinism (§22) makes it exact.
// ============================================================================

const MA: u64 = 7_100; // creation-sighting mint
const MB: u64 = 7_200; // social-caller mint

fn disc_net(r: &Report, lane: DiscoveryLane) -> i64 {
    r.per_discovery_lane_net
        .iter()
        .find(|(l, _)| *l == lane)
        .map(|(_, n)| *n)
        .unwrap()
}
fn setup_net(r: &Report, lane: WlLane) -> i64 {
    r.per_lane_net
        .iter()
        .find(|(l, _)| *l == lane)
        .map(|(_, n)| *n)
        .unwrap()
}

/// Net-SELL trades at a falling price for `tag` (numeric features/liquidity present
/// for the gate, but the numeric lane's bullish emit gate stays quiet so it never
/// overrides the corroboration lane's provenance).
fn sell_flow(eng: &mut Engine, tag: u64, base: i128, n: u64) {
    for i in 0..n {
        eng.tick(AppEvent::MarketTrade {
            mint: mint(tag),
            price_fp: (base - i as i128) * PRICE_SCALE,
            quote_lamports: 800_000,
            liquidity_lamports: REAL_CURVE_VSOL,
            signed_base: -500_000,
            buyer_entity: 60 + i % 7,
            age_slots: 12,
            recv_unix_ms: None,
            trader_pubkey: None,
            slot: None,
            fee_lamports: None,
            cu_consumed: None,
            venue: None,
            event_id: None,
            feature: None,
        });
    }
}

fn drive_disc_attribution() -> Report {
    let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    // Discover MA as an on-chain creation sighting; MB as a social caller.
    eng.tick(AppEvent::TokenMetadata {
        mint: mint(MA),
        category_id: 1,
        taxonomy_version: 1,
        creator: 11,
        slot: 1,
    });
    for _ in 0..4 {
        eng.tick(AppEvent::SocialCall {
            mint: mint(MB),
            source_quality_bp: 3_000,
        });
    }
    // Numeric snapshot on both (features + confirm depth), net-SELL so the numeric
    // lane emits nothing and the provenance stays the corroboration lane.
    sell_flow(&mut eng, MA, 130, 12);
    sell_flow(&mut eng, MB, 130, 12);
    eng.tick(AppEvent::OnchainConfirm {
        mint: mint(MA),
        virtual_sol_lamports: REAL_CURVE_VSOL,
        real_sol_lamports: REAL_CURVE_REAL_SOL,
    });
    eng.tick(AppEvent::OnchainConfirm {
        mint: mint(MB),
        virtual_sol_lamports: REAL_CURVE_VSOL,
        real_sol_lamports: REAL_CURVE_REAL_SOL,
    });
    // Keep both discovery signals fresh across the admit ticks.
    eng.tick(AppEvent::TokenMetadata {
        mint: mint(MA),
        category_id: 1,
        taxonomy_version: 1,
        creator: 11,
        slot: 2,
    });
    for _ in 0..4 {
        eng.tick(AppEvent::SocialCall {
            mint: mint(MB),
            source_quality_bp: 3_000,
        });
    }
    ticks(&mut eng, 4);
    eng.report()
}

// ============================================================================
// Batch-2b LAW 4 — §25 setup-archetype classifier.
//
// Two net-BUY markets with DIFFERENT bar structures self-authorize (numeric lane)
// and open; at admit the classifier derives each one's §24 setup family from the
// reconstructed bar/flow state and tags the excursion sample. Classifier ON: the
// analytics ring carries ≥2 DISTINCT, non-stub archetype ids. Classifier OFF:
// every row is the all-0 stub (`archetypes == [0]`).
// ============================================================================

const MC1: u64 = 8_100;
const MC2: u64 = 8_200;

/// Emit one 8-trade net-BUY bar for `tag` from an explicit price sequence
/// (open = first, close = last, high/low = extremes) so the reconstructed
/// bar/flow state is fully controlled.
fn bar8(eng: &mut Engine, tag: u64, prices: [i128; 8], entity0: u64) {
    for (i, &p) in prices.iter().enumerate() {
        eng.tick(AppEvent::MarketTrade {
            mint: mint(tag),
            price_fp: p * PRICE_SCALE,
            quote_lamports: 800_000,
            liquidity_lamports: REAL_CURVE_VSOL,
            signed_base: 900_000,
            buyer_entity: entity0 + i as u64 % 7,
            age_slots: 12,
            recv_unix_ms: None,
            trader_pubkey: None,
            slot: None,
            fee_lamports: None,
            cu_consumed: None,
            venue: None,
            event_id: None,
            feature: None,
        });
    }
}

fn drive_classifier(cfg: Config) -> (Report, Vec<u16>) {
    let mut eng = Engine::new(cfg, RunMode::Replay);
    // Two priming bars per market establish the prior structural low (~100) and the
    // VWAP anchor, then the TARGET (third, most-recent-CLOSED) bar differs:
    //   M1 pulls back only to 110 (>= prior low) but below VWAP, reclaiming to
    //      130 with net buying          → Reclaim (id 3).
    //   M2 breaches the prior low to 90 (< prior low 100) then reclaims to 130
    //      with net buying              → FailedBreakdownReversal (id 2).
    // A trailing net-buy trade CLOSES the target bar so it is the last closed bar.
    bar8(&mut eng, MC1, [100, 104, 108, 112, 110, 108, 113, 115], 40);
    bar8(&mut eng, MC1, [116, 118, 121, 124, 120, 122, 124, 125], 41);
    bar8(&mut eng, MC1, [125, 118, 112, 110, 115, 122, 128, 130], 42); // target: reclaim
    bar8(&mut eng, MC2, [100, 104, 108, 112, 110, 108, 113, 115], 50);
    bar8(&mut eng, MC2, [116, 118, 121, 124, 120, 122, 124, 125], 51);
    bar8(&mut eng, MC2, [125, 110, 95, 90, 100, 115, 125, 130], 52); // target: failed-breakdown
                                                                     // ONE trailing trade closes each target bar (and opens a fresh, still-open
                                                                     // bar of just one trade — so the target stays the last CLOSED bar).
    for tag in [MC1, MC2] {
        eng.tick(AppEvent::MarketTrade {
            mint: mint(tag),
            price_fp: 131 * PRICE_SCALE,
            quote_lamports: 800_000,
            liquidity_lamports: REAL_CURVE_VSOL,
            signed_base: 900_000,
            buyer_entity: 44,
            age_slots: 12,
            recv_unix_ms: None,
            trader_pubkey: None,
            slot: None,
            fee_lamports: None,
            cu_consumed: None,
            venue: None,
            event_id: None,
            feature: None,
        });
    }
    eng.tick(AppEvent::OnchainConfirm {
        mint: mint(MC1),
        virtual_sol_lamports: REAL_CURVE_VSOL,
        real_sol_lamports: REAL_CURVE_REAL_SOL,
    });
    eng.tick(AppEvent::OnchainConfirm {
        mint: mint(MC2),
        virtual_sol_lamports: REAL_CURVE_VSOL,
        real_sol_lamports: REAL_CURVE_REAL_SOL,
    });
    ticks(&mut eng, 3);
    let r = eng.report();
    let arch = eng.analytics_report().archetypes;
    (r, arch)
}

// ============================================================================
// Batch-2b LAW 11 — §24 EntryMode leaves (pullback-continuation admission).
//
// A market that pumps into a confirmed uptrend then pulls back in a CONTROLLED
// way, holding a retest of the prior breakout level with net buying — but which
// NEVER receives a fresh `OnchainConfirm`. The 4-lane gate rejects it for want of
// on-chain confirmation (the setup it structurally misses); with the EntryMode
// leaves enabled, `detect_pullback_continuation` maps it onto active-market-scalp
// eligibility (its sellable depth is the observed pool liquidity) and it is
// admitted. The armed arm changes the audited decision from reject to admit.
// ============================================================================

const MP: u64 = 8_800;

/// Emit one 8-trade net-BUY bar for `MP` from explicit prices (see `bar8`).
fn pb_bar(eng: &mut Engine, prices: [i128; 8], entity0: u64) {
    bar8(eng, MP, prices, entity0);
}

/// A confirmed-uptrend zigzag (rising swing highs AND rising swing lows) that ends
/// in a controlled pullback holding the prior breakout level — but NO on-chain
/// confirm. Leaves the decision to the gate.
fn drive_pullback(cfg: Config) -> Report {
    // A-6 sizing regime: exercise the §24 EntryMode pullback admission above the
    // 0.1-SOL operator floor. Realistic wide-window economics (x_max ≈ 0.3 SOL) + a
    // bankroll that sizes the pullback well above the floor, so a lightly-haircut
    // active-market scalp still clears x_min rather than refusing below the floor. The
    // entry_mode toggle under test is untouched — both arms share these overrides.
    let mut cfg = cfg;
    cfg.gate_expected_move_bps = 1_800;
    cfg.gate_margin_bps = 150;
    cfg.bankroll_initial_lamports = 10_000_000_000; // 10 SOL ⇒ deployable 7.5, base ~0.5 SOL
    let mut eng = Engine::new(cfg, RunMode::Replay);
    pb_bar(&mut eng, [100, 90, 110, 95, 100, 105, 98, 100], 40); // low90 high110
    pb_bar(&mut eng, [105, 130, 120, 105, 125, 128, 122, 125], 41); // swing high 130
    pb_bar(&mut eng, [120, 100, 115, 100, 110, 118, 112, 110], 42); // swing low 100
    pb_bar(&mut eng, [115, 150, 130, 115, 145, 148, 140, 145], 43); // swing high 150
    pb_bar(&mut eng, [140, 110, 135, 110, 130, 138, 132, 130], 44); // swing low 110
    pb_bar(&mut eng, [160, 175, 165, 152, 170, 174, 168, 170], 45); // peak 175, holds >150
    pb_bar(&mut eng, [162, 165, 158, 155, 160, 163, 159, 160], 46); // pullback, holds >150
                                                                    // One trailing net-buy trade closes the pullback bar (it becomes the last
                                                                    // closed bar the detector reads). Deliberately NO OnchainConfirm.
    eng.tick(AppEvent::MarketTrade {
        mint: mint(MP),
        price_fp: 160 * PRICE_SCALE,
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
        feature: None,
    });
    ticks(&mut eng, 3);
    eng.report()
}

// ============================================================================
// Batch-2c LAW 7 — §70.1 composite money proxy.
//
// The money proxy feeds the attention field's attention-vs-money DIVERGENCE: a
// rising money level (against a rising attention level) reads Confirmed (300
// pts) instead of AttentionLeads (200 pts) in nv_candidate_score. The composite
// (`money_proxy_enable`) folds distinct-smart-wallet-entry / net-inflow and
// holder-growth AHEAD of price momentum, so a market whose genuine wallet-entry /
// holder-growth LEADS its (flat) buy-pressure shows rising money — and scores
// strictly higher — where the buy-pressure-only proxy shows flat money and
// scores lower. The two `money_of` closures below ARE the two config arms
// (money_proxy_enable on vs off); the wiring that selects between them is in
// `engine::evaluate`'s attention emit. Determinism (§22) makes it exact.
// ============================================================================

#[test]
fn composite_money_proxy_outscores_buy_pressure_alone_on_a_wallet_led_market() {
    use pump_quant_app::attention::{AttentionField, AttentionParams};
    use pump_quant_narrative::attention_state::Mention;

    fn men(ts_ns: u64, source_id: u64, weight: u64) -> Mention {
        Mention {
            ts_ns,
            source_id,
            community_id: source_id,
            weight,
            copycat: false,
        }
    }

    // Identical rising-attention mention stream in both arms.
    let build = || {
        let mut f = AttentionField::new(AttentionParams::standard());
        let m = [7u8; 32];
        for i in 0..8u64 {
            f.observe(m, men(1_000 + i * 10, i, 400));
        }
        f
    };

    // Flat buy-pressure momentum in BOTH arms (a balanced OFI tape). The armed
    // arm's composite money ALSO rises across the two emits (wallet-entry +
    // holder growth = 500·decade(inflow) + 200·holders); the neutral arm reads
    // buy-pressure alone, which does not move → money velocity 0.
    const FLAT_BP: u64 = 5_000;
    let composite_later: u64 = FLAT_BP + 500 * 3 + 200 * 8; // rising money (§70.1 fold)

    // Arm A — composite proxy (money_proxy_enable = true).
    let mut a = build();
    let mut buf = Vec::new();
    a.emit_into(&mut buf, 1, |_| FLAT_BP, |_| true); // seed prev_money
    buf.clear();
    for i in 8..16u64 {
        a.observe([7u8; 32], men(2_000 + i * 10, i, 800));
    }
    a.emit_into(&mut buf, 2, |_| composite_later, |_| true);
    assert_eq!(buf.len(), 1, "one attention candidate");
    let composite_score = buf[0].discovery_score;

    // Arm B — buy-pressure alone (money_proxy_enable = false).
    let mut b = build();
    let mut buf2 = Vec::new();
    b.emit_into(&mut buf2, 1, |_| FLAT_BP, |_| true);
    buf2.clear();
    for i in 8..16u64 {
        b.observe([7u8; 32], men(2_000 + i * 10, i, 800));
    }
    b.emit_into(&mut buf2, 2, |_| FLAT_BP, |_| true);
    assert_eq!(buf2.len(), 1);
    let buy_pressure_score = buf2[0].discovery_score;

    // The composite (wallet-entry/holder-growth leading flat buy-pressure) scores
    // the market strictly higher — the Confirmed vs AttentionLeads divergence gap.
    assert!(
        composite_score > buy_pressure_score,
        "the §70.1 composite money proxy must score a wallet/holder-led market \
         above the buy-pressure-only proxy ({} vs {})",
        composite_score,
        buy_pressure_score
    );
}

// ============================================================================
// Batch-2c LAW 8 — §70.6/§70.8 narrative class + ceiling.
//
// The class law conditions BOTH the corroboration ceiling/decay (via
// nv_narrative_ceiling) and the §49 sizing conviction on the derived
// NarrativeClass. Two narratives of DIFFERENT class must decay/ceiling
// differently (a durable class projects a higher ceiling and retains more rank)
// and size differently (a fast, low-ceiling class sizes down), where the
// class-unconditioned path treats them alike. Determinism (§22) makes it exact.
// ============================================================================

#[test]
fn narrative_class_conditions_ceiling_decay_and_size() {
    use pump_quant_app::attention::{
        narrative_class_conviction_bp, AttentionField, AttentionParams,
    };
    use pump_quant_narrative::attention_state::Mention;
    use pump_quant_narrative::narrative::{nv_narrative_ceiling, NarrativeClass, FP_ONE};

    // (a) SIZE differently (§49 conviction, reduce-only): a fast, low-ceiling
    // class sizes strictly below a durable, high-ceiling one — never above 100%.
    assert!(
        narrative_class_conviction_bp(NarrativeClass::News)
            < narrative_class_conviction_bp(NarrativeClass::Tech),
        "News must size below Tech"
    );
    assert!(
        narrative_class_conviction_bp(NarrativeClass::Trend)
            < narrative_class_conviction_bp(NarrativeClass::Culture),
        "Trend must size below Culture"
    );
    assert!(
        narrative_class_conviction_bp(NarrativeClass::Culture) <= 10_000,
        "class conviction is reduce-only"
    );

    // (b) CEILING/decay differently: the same reach projects to a strictly higher
    // ceiling for a durable class than a fast one (durable narratives run longer).
    assert!(
        nv_narrative_ceiling(NarrativeClass::Trend, 1_000, FP_ONE)
            < nv_narrative_ceiling(NarrativeClass::Culture, 1_000, FP_ONE),
        "a durable class must project a higher reach ceiling"
    );

    // (c) The class-conditioned SCORE difference on real attention state: build a
    // DURABLE, broad narrative (many persisted windows + broad sources ⇒ Tech/
    // Culture) and a SHORT one (few windows ⇒ Trend), feed each identically into
    // a class-conditioned (armed) field and a class-unconditioned (neutral) one.
    fn men(ts_ns: u64, source_id: u64, weight: u64) -> Mention {
        Mention {
            ts_ns,
            source_id,
            community_id: source_id,
            weight,
            copycat: false,
        }
    }
    // Feed `windows` emit-cycles, each seeded by `sources` distinct mentions, and
    // return the final scored candidate's discovery score (money confirmed so the
    // fade cap does not mask the class conditioning).
    fn run(enable_class: bool, m: [u8; 32], sources: u64, windows: u64) -> (u64, u32) {
        let params = AttentionParams {
            narrative_class_enable: enable_class,
            ..AttentionParams::standard()
        };
        let mut f = AttentionField::new(params);
        let mut ts = 1_000u64;
        let mut last = 0u64;
        for win in 0..windows {
            for s in 0..sources {
                f.observe(m, men(ts, s, 300));
                ts += 10;
            }
            let mut buf = Vec::new();
            f.emit_into(&mut buf, win + 1, |_| 1_000, |_| true);
            last = buf.first().map(|c| c.discovery_score).unwrap_or(0);
        }
        // class code for inspection (0=Trend,1=News,2=Tech,3=Culture)
        let code = match f.narrative_class_of(&m) {
            Some(NarrativeClass::Trend) => 0,
            Some(NarrativeClass::News) => 1,
            Some(NarrativeClass::Tech) => 2,
            Some(NarrativeClass::Culture) => 3,
            None => 255,
        };
        (last, code)
    }

    let durable = [1u8; 32];
    let short = [2u8; 32];
    let (armed_durable, dclass) = run(true, durable, 6, 9);
    let (neut_durable, _) = run(false, durable, 6, 9);
    let (armed_short, sclass) = run(true, short, 6, 3);
    let (neut_short, _) = run(false, short, 6, 3);

    eprintln!(
        "LAW8 durable class={dclass} armed={armed_durable} neut={neut_durable} | \
         short class={sclass} armed={armed_short} neut={neut_short}"
    );
    // The two narratives derive DIFFERENT classes (durable vs fast).
    assert_ne!(
        dclass, sclass,
        "the two narratives must classify differently"
    );
    // Class-conditioned: the durable (higher-ceiling) narrative is boosted by a
    // STRICTLY LARGER ceiling multiple than the fast one — decay/ceiling differ by
    // class. Cross-multiplied to avoid division: armed_d/neut_d > armed_s/neut_s.
    assert!(neut_durable > 0 && neut_short > 0, "both arms score");
    assert!(
        u128::from(armed_durable) * u128::from(neut_short)
            > u128::from(armed_short) * u128::from(neut_durable),
        "the durable class must gain more ceiling conditioning than the fast one \
         (durable {armed_durable}/{neut_durable} vs fast {armed_short}/{neut_short})"
    );
}

// ============================================================================
// Batch-2c LAW 9 — §70.7 platform-lead / crypto-social-lag.
//
// A mint whose MAINSTREAM (TikTok/Web) attention PRECEDES crypto pickup has a
// front-runnable pre-legibility window (the mainstream→crypto lag); a mint that
// is already crypto-saturated (no mainstream front) has none. With the law
// armed, the lead mint earns a higher pre-legibility runway — and scores strictly
// higher — than the crypto-saturated one; unconditioned, the two are identical.
// ============================================================================

#[test]
fn platform_lead_gives_a_mainstream_led_mint_more_runway() {
    use pump_quant_app::attention::{AttentionField, AttentionParams, MentionProvenance};
    use pump_quant_narrative::attention_state::Mention;

    fn men(ts_ns: u64, source_id: u64, weight: u64) -> Mention {
        Mention {
            ts_ns,
            source_id,
            community_id: source_id,
            weight,
            copycat: false,
        }
    }

    // Identical attention dynamics in every arm; only the platform PROVENANCE of
    // the first-mention fronts differs. `lead` ⇒ a mainstream mention precedes the
    // crypto ones (MainstreamLeads); otherwise every mention is crypto-native
    // (mainstream front never set ⇒ NoData ⇒ no runway).
    fn run(enable: bool, lead: bool) -> u64 {
        let params = AttentionParams {
            platform_lead_enable: enable,
            platform_lead_tolerance_ns: 1, // tiny deadband so a 10ns gap is a lead
            ..AttentionParams::standard()
        };
        let mut f = AttentionField::new(params);
        let m = [3u8; 32];
        let crypto = MentionProvenance::default(); // mainstream = false
        let mainstream = MentionProvenance {
            mainstream: true,
            ..MentionProvenance::default()
        };
        // Round 1: 6 distinct mentions. The FIRST is mainstream when `lead`, so the
        // mainstream front (ts 1_000) precedes the crypto front (ts ≥ 1_010).
        for s in 0..6u64 {
            let ts = 1_000 + s * 10;
            let prov = if lead && s == 0 { &mainstream } else { &crypto };
            f.observe_tagged(m, men(ts, s, 400), prov);
        }
        let mut buf = Vec::new();
        f.emit_into(&mut buf, 1, |_| 1_000, |_| true); // seed
                                                       // Round 2: rising attention (all crypto), then the scored emit.
        for s in 0..6u64 {
            f.observe_tagged(m, men(2_000 + s * 10, s, 800), &crypto);
        }
        buf.clear();
        f.emit_into(&mut buf, 2, |_| 1_000, |_| true);
        buf.first().map(|c| c.discovery_score).unwrap_or(0)
    }

    let armed_lead = run(true, true);
    let armed_sat = run(true, false);
    let neut_lead = run(false, true);
    let neut_sat = run(false, false);

    eprintln!(
        "LAW9 armed_lead={armed_lead} armed_sat={armed_sat} neut_lead={neut_lead} neut_sat={neut_sat}"
    );
    // Unconditioned: platform provenance is inert — the two mints are identical.
    assert_eq!(
        neut_lead, neut_sat,
        "without the law the mainstream lead earns no runway"
    );
    // Armed: the mainstream-led mint out-scores the crypto-saturated one (runway).
    assert!(
        armed_lead > armed_sat,
        "the §70.7 platform-lead runway must lift a mainstream-led mint above a \
         crypto-saturated one ({armed_lead} vs {armed_sat})"
    );
}

// ============================================================================
// Batch-2c LAW 10 — §70.9/§70.10 deployer credibility + anti-bundle fee-floor.
//
// A market that self-authorizes and admits on genuine net-buy flow, but whose
// FIRST-SLOT fee footprint is a fully-saturated bundle/wash signature (many txs,
// near-zero cumulative priority+tip). With the fee-floor law armed the pre-entry
// gate VETOES it (a new reject code, 14) — no position, no manufactured-flow
// loss; neutral admits it. The axis is loss AVOIDED (§52 spirit): the veto keeps
// strictly more lamports than admitting into the wash.
// ============================================================================

const MF: u64 = 8_400; // fee-floor / bundle-signature mint

/// Emit `n` net-buy trades at a rising price for mint `MF` (self-authorizing).
fn pump_mf(eng: &mut Engine, n: u64) {
    for i in 0..n {
        eng.tick(AppEvent::MarketTrade {
            mint: mint(MF),
            price_fp: (100 + i as i128) * PRICE_SCALE,
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
            feature: None,
        });
    }
}

fn drive_fee_floor(cfg: Config, feed_bundle: bool) -> (Report, Engine) {
    use pump_quant_signals::launch_trajectory::FirstSlotTx;
    let mut eng = Engine::new(cfg, RunMode::Replay);
    if feed_bundle {
        // A bundle/wash first-slot footprint: 20 txs paying 1 lamport combined
        // TOTAL — intensity far below the plausible fee floor ⇒ ImplausiblyLow,
        // fade saturates near 10_000 (a veto-grade signature).
        let mut txs: Vec<FirstSlotTx> = (0..19)
            .map(|_| FirstSlotTx {
                tipper_entity: 0,
                priority_fee_lamports: 0,
                tip_lamports: 0,
                is_bundle: true,
                is_known_sniper: false,
            })
            .collect();
        txs.push(FirstSlotTx {
            tipper_entity: 0,
            priority_fee_lamports: 1,
            tip_lamports: 0,
            is_bundle: true,
            is_known_sniper: false,
        });
        eng.observe_first_slot_fees(mint(MF).as_bytes(), &txs);
    }
    pump_mf(&mut eng, 24);
    eng.tick(AppEvent::OnchainConfirm {
        mint: mint(MF),
        virtual_sol_lamports: REAL_CURVE_VSOL,
        real_sol_lamports: REAL_CURVE_REAL_SOL,
    });
    ticks(&mut eng, 6);
    let r = eng.report();
    (r, eng)
}

#[test]
fn deployer_screen_fades_a_serial_extractor_reduce_only() {
    use pump_quant_app::screen::deployer_screen_haircut_bp;
    use pump_quant_wallet_graph::deployer_credibility::DeployerCredibility;

    // A clean deployer (no prior CAs, no serial burst) takes no haircut.
    let clean = DeployerCredibility {
        prior_ca_count: 0,
        serial_deploy_flag: false,
        max_launches_in_window: 0,
        key_follower_reach: 0,
        mutual_follower_reach: 0,
        verified_partnership_count: 0,
        self_claimed_partnership_count: 0,
    };
    assert_eq!(deployer_screen_haircut_bp(&clean, false), 10_000);

    // A serial deployer (burst flag) is faded; a serial deployer the §27 classifier
    // labels a known extractor is faded STRICTLY MORE (class-conditioned) — but
    // never below the credibility floor (reduce-only, never a veto).
    let serial = DeployerCredibility {
        serial_deploy_flag: true,
        ..clean
    };
    let plain = deployer_screen_haircut_bp(&serial, false);
    let extractor = deployer_screen_haircut_bp(&serial, true);
    assert!(plain < 10_000, "a serial deployer must be faded ({plain})");
    assert!(
        extractor < plain,
        "a known extractor must fade strictly more than a plain serial deployer \
         ({extractor} vs {plain})"
    );
    assert!(
        extractor >= 5_000,
        "the deployer screen is reduce-only, never a veto"
    );
}
