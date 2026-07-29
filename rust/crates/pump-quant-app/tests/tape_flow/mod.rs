//! The **FLOW-PERSISTENCE two-sided tape** (§32 `thesis_persist_obs`).
//!
//! # What this tape is for
//!
//! The engine's binding exit is the §32 thesis force-exit, which fires the moment
//! windowed order-flow imbalance turns net-sell. The external literature says that
//! is the least informative possible read of the flow process:
//!
//! * **arXiv 2606.16269** (Lillo–Mike–Farmer, `γ = α − 1`): trade signs are
//!   long-memory because metaorder lengths are Pareto-distributed. The information
//!   lives in a persistent same-signed RUN, in EVENT time; an isolated sign flip
//!   is mostly noise.
//! * **Kaminski & Lo**, *J. Financial Markets* 18:234–254: a stop rule's "stopping
//!   premium" is negative unless the trigger predicts PERSISTENT adverse drift.
//!
//! So: does demanding a RUN of `k` adverse observations beat exiting on the first?
//! That is a two-sided question, and this tape is built to answer it honestly —
//! there is a path on which waiting is right and a path on which waiting is wrong.
//!
//! # The mirror discipline (why this is a fair test)
//!
//! Both sides are **byte-identical up to and including the first adverse
//! observation**. Same admission script, same rise, same peak price `R1`, same
//! shakeout burst at the same step. At the moment the engine must decide, the two
//! tapes are INDISTINGUISHABLE — exactly the discipline the B7 and concentration
//! tapes use. They diverge only in what happens NEXT:
//!
//! * [`Side::ShakeoutThenRun`] (**happy**): the burst was a mid-rise shakeout.
//!   Flow recovers, price runs `R1 → R2`, and only later does a genuine sustained
//!   reversal arrive. Exiting on the first flip sells the runner at `R1` and
//!   forfeits the whole right tail.
//! * [`Side::TrueTop`] (**unhappy/mirror**): the burst WAS the top. Flow stays
//!   adverse and price collapses immediately. Exiting on the first flip is exactly
//!   right, and every observation of patience is paid for in lamports.
//!
//! A persistence rule cannot be "tuned" to win here: whatever `k` buys on the
//! happy side it must give back on the mirror, and the pre-registered asymmetry
//! bar (≥ 3×) is what decides whether the trade-off is worth making.
#![allow(dead_code)]

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_domain::ids::Mint;

pub const PRICE_SCALE: i128 = 10_000_000;

/// Which forward regime follows the (identical) shakeout burst.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    /// HAPPY: the burst was noise; the market runs on afterwards.
    ShakeoutThenRun,
    /// MIRROR: the burst was the genuine top; the market collapses from here.
    TrueTop,
}

/// Markets on the tape. Enough to matter in lamports, few enough to stay fast.
pub const MARKETS: u64 = 6;

/// **Pool depth (lamports): a real pump.fun curve a little past launch, 32 SOL of
/// virtual SOL reserve** — deep enough that the exit is priceable at every rung.
///
/// **Re-pin #26 (2026-07-28).** This was `260_000_000` — 0.26 SOL — and "priceable
/// at every rung" stopped being true the moment `gate::decide` began deriving its
/// impact denominator from the market's own reserve (`cost_model::impact_den_for` =
/// `vsol / 10_000`). A 0.1 SOL clip into a 0.26 SOL pool is 3_846 bps of own impact
/// a LEG; all six markets refused, nothing opened, and both sides of a tape whose
/// entire purpose is to make the two sides DIFFER reported the same 0.
///
/// **The scenario is untouched.** Every lever this tape exercises is a FLOW lever:
/// the shakeout burst ([`SELL_BURST`]) and the recovery ([`RECOVER_BUY`]) act on
/// windowed order-flow imbalance, which is computed from signed BASE and does not
/// read depth at all; the price waypoints are unchanged. Real depth changes only
/// whether the positions the tape is about ever get opened.
pub const LIQ: u64 = 32_000_000_000;
/// The SOL this curve actually escrows: `LIQ - LAUNCH_VSOL_LAMPORTS`.
///
/// **Re-pin #27 (2026-07-28): was `30_000_000_000`** — 30 SOL of claimed payout
/// against a 32 SOL price reserve, on a venue where a curve escrows `virtual_sol - 30
/// SOL` and this one therefore holds 2. The claim was 15x the money in the pool. It
/// never bound `x_max` at a 0.1 SOL clip, which is exactly why it survived.
pub const DEPTH: u64 = LIQ - 30_000_000_000;
/// Market age in slots: past the sniper window, still inside the §21.5
/// fresh-launch exemption, so the universe screen stays inert and this tape
/// isolates the EXIT lever only.
pub const AGE: u32 = 40;

/// Accumulation prints at launch (establishes buy-side flow and buyer breadth).
const LAUNCH_PRINTS: u64 = 25;
/// Per-print base clip during accumulation and the rise.
const BUY_CLIP: i64 = 40_000;
/// The shakeout/reversal sell print. Large enough to swing the 64-trade windowed
/// OFI net-sell on its own — that is what makes it an ADVERSE OBSERVATION.
const SELL_BURST: i64 = -2_000_000;
/// The recovery buy that restores net-buy flow after a SHAKEOUT (happy side only).
/// Sized to outweigh the burst so the adverse run is genuinely BROKEN, not paused.
const RECOVER_BUY: i64 = 20_000_000;
/// Quote lamports per print.
const QUOTE: u64 = 200_000;

/// Price waypoints, in bps of the launch price (10_000 = 1.0x).
const P_LAUNCH: i128 = 10_000;
/// Price at the shakeout burst — identical on both sides (the decision point).
const P_R1: i128 = 14_000;
/// Peak reached only on the happy side after the shakeout.
const P_R2: i128 = 30_000;
/// Terminal collapse price, identical on both sides.
const P_LOW: i128 = 8_000;

pub fn mint(tag: u64) -> Mint {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&tag.to_le_bytes());
    // Distinct marker byte: cannot collide with the golden tape's `0xAB` mints.
    b[8] = 0xF1;
    Mint::from_bytes(b)
}

fn swap(eng: &mut Engine, m: Mint, price_bp: i128, signed_base: i64, entity: u64) {
    eng.tick(AppEvent::MarketTrade {
        mint: m,
        price_fp: PRICE_SCALE * price_bp / 10_000,
        quote_lamports: QUOTE,
        liquidity_lamports: LIQ,
        signed_base,
        buyer_entity: entity,
        age_slots: AGE,
    });
}

fn ticks(eng: &mut Engine, n: u64) {
    for _ in 0..n {
        eng.tick(AppEvent::Tick);
    }
}

fn confirm(eng: &mut Engine, m: Mint) {
    eng.tick(AppEvent::OnchainConfirm {
        mint: m,
        virtual_sol_lamports: LIQ,
        real_sol_lamports: DEPTH,
    });
}

/// The tape's config: the golden tape's cost realism (so lamports are comparable)
/// plus enough position slots to hold every market on the tape.
pub fn flow_cfg(cfg: Config) -> Config {
    let mut cfg = cfg;
    // COST-MODEL UNIFICATION (2026-07-28). The gate's three cost inputs —
    // protocol bps, base fixed lamports and the impact denominator — are no longer
    // config: `gate::decide` derives them per candidate from the market's own
    // SOL-side reserve via `cost_model`. The overrides that used to sit here
    // (450 / 200_000 / 250_000) are gone because they no longer decide anything;
    // what this tape must now declare honestly is its DEPTH, which is what the
    // derived impact model reads — see `LIQ` (re-pin #26).
    cfg.gate_expected_move_bps = 1_800;
    cfg.gate_margin_bps = 150;
    cfg.max_concurrent_positions = 6;
    // ---- ISOLATE THE LEVER UNDER TEST (§32 flow persistence) ----
    // The held-position lifecycle carries its OWN exits that also fire on a large
    // sell print — CVD rollover, the in-profit stall, the rug precursor. If any of
    // them binds first, this tape measures THEM and not the flow-persistence rule,
    // and every number it produced would be an artifact. They are therefore
    // neutralized here (not "tuned"): the CVD floor and precursor are pushed past
    // anything this script generates, and the stall/max-hold clocks past its
    // length. The HARD STOP and TRAILING STOP are deliberately LEFT AT THEIR
    // SHIPPED DEFAULTS — they are the honest protective backstop whose job is
    // exactly to bound the cost of patience, so the mirror side must be allowed to
    // pay through them.
    cfg.lc_cvd_hold_frac_bps = 100;
    cfg.lc_precursor_drop_bps = 9_000;
    cfg.lc_stall_ticks = 100_000;
    cfg.lc_max_hold_ticks = 100_000;
    cfg
}

/// Linear price interpolation between two waypoints, integer-only (§22).
fn lerp(from: i128, to: i128, step: u64, of: u64) -> i128 {
    let of = of.max(1) as i128;
    let s = (step as i128).min(of);
    from + (to - from) * s / of
}

/// The event script. Identical on both sides through the shakeout burst.
pub fn apply(eng: &mut Engine, side: Side) {
    // ---- Phase 0: launch + admission (IDENTICAL both sides) ----
    for t in 0..MARKETS {
        let m = mint(t);
        eng.tick(AppEvent::TokenMetadata {
            mint: m,
            category_id: 0,
            taxonomy_version: 1,
            creator: 900_000 + t,
            slot: 1,
        });
        confirm(eng, m);
        for k in 0..LAUNCH_PRINTS {
            swap(eng, m, P_LAUNCH, BUY_CLIP, t * 1_000 + k);
        }
    }
    ticks(eng, 6);

    // ---- Phase 1: the rise to R1 (IDENTICAL both sides) ----
    // Buy-dominated flow, price climbing 1.0x -> 1.4x. Positions open here.
    const RISE_STEPS: u64 = 8;
    for s in 0..RISE_STEPS {
        let px = lerp(P_LAUNCH, P_R1, s + 1, RISE_STEPS);
        for t in 0..MARKETS {
            let m = mint(t);
            for k in 0..3u64 {
                swap(eng, m, px, BUY_CLIP, t * 1_000 + 100 + s * 3 + k);
            }
            confirm(eng, m);
        }
        ticks(eng, 2);
    }

    // ---- Phase 2: THE SHAKEOUT BURST (IDENTICAL both sides) ----
    // One large sell print per market at price R1. This drives the 64-trade
    // windowed OFI net-sell => the engine registers an ADVERSE OBSERVATION and,
    // at k == 1, force-exits right here. At k > 1 the position survives, because
    // one observation is not yet a RUN. The two sides are indistinguishable at
    // this instant — which is the whole point.
    for t in 0..MARKETS {
        swap(eng, mint(t), P_R1, SELL_BURST, t * 1_000 + 500);
    }
    ticks(eng, 1);

    // ---- Phase 3: DIVERGENCE ----
    match side {
        Side::ShakeoutThenRun => {
            // The burst was noise. Flow recovers hard and the market runs to R2.
            for t in 0..MARKETS {
                swap(eng, mint(t), P_R1, RECOVER_BUY, t * 1_000 + 600);
            }
            ticks(eng, 1);
            const RUN_STEPS: u64 = 10;
            for s in 0..RUN_STEPS {
                let px = lerp(P_R1, P_R2, s + 1, RUN_STEPS);
                for t in 0..MARKETS {
                    let m = mint(t);
                    for k in 0..3u64 {
                        swap(eng, m, px, BUY_CLIP * 4, t * 1_000 + 700 + s * 3 + k);
                    }
                }
                ticks(eng, 2);
            }
            // ...and only THEN the genuine, sustained reversal.
            collapse(eng, P_R2);
        }
        Side::TrueTop => {
            // The burst WAS the top: sustained adverse flow, immediate collapse.
            collapse(eng, P_R1);
        }
    }

    ticks(eng, 4);
}

/// A genuine sustained reversal: repeated net-sell prints with price falling to
/// [`P_LOW`]. Every observation here is adverse, so ANY persistence setting exits
/// within `k` observations — the cost of patience is bounded and measurable.
fn collapse(eng: &mut Engine, from: i128) {
    const FALL_STEPS: u64 = 8;
    for s in 0..FALL_STEPS {
        let px = lerp(from, P_LOW, s + 1, FALL_STEPS);
        for t in 0..MARKETS {
            let m = mint(t);
            for k in 0..2u64 {
                swap(eng, m, px, SELL_BURST / 2, t * 1_000 + 900 + s * 2 + k);
            }
        }
        ticks(eng, 1);
    }
}

/// Drive one (side, config) cell and hand back the finished engine.
pub fn drive_eng(cfg: Config, side: Side) -> Engine {
    let mut eng = Engine::new(flow_cfg(cfg), RunMode::Replay);
    apply(&mut eng, side);
    eng
}

pub fn drive(cfg: Config, side: Side) -> pump_quant_app::engine::Report {
    drive_eng(cfg, side).report()
}
