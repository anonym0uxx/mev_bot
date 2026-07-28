//! The **holder-concentration two-sided tape** (happy / mirror), hoisted verbatim
//! out of `tests/holder_concentration.rs` so more than one test binary can drive it.
//!
//! Nothing here was rewritten for the law-permutation sweep. `holder_concentration.rs`
//! still owns the pre-registered rule, the metric proofs and the basis discipline;
//! this module owns only the event script.
#![allow(dead_code)]

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_app::holder_flow::SNIPER_SLOT_WINDOW;
use pump_quant_domain::ids::Mint;
use pump_quant_ingest::social_source::{MockSocialSource, RawSocialPayload};

/// Fixed-point price scale shared with `holder_concentration.rs`.
pub const PRICE_SCALE: i128 = 10_000_000;

/// Valid Solana pubkeys (the attention lane only sees mints a social post named,
/// and the parser demands a real base58 key).
pub const CONC_B58: [&str; 4] = [
    "29d2S8fphGNdxpkLtoYM42q9Q6h7bxNiopT6JBHXmGB2",
    "2DYKaS8EYv7c92Pu9NMQRnvJdNA5gtFhEaLtf6SFCjbE",
    "2HTcijaeQZraKE3TPwAToZ1Trdd3mp8ffLEh21axeD1S",
    "2MNus334GDbYVRh1eVyXBK6d5u61rk1e668VNvjg5gRe",
];
pub const BROAD_B58: [&str; 4] = [
    "2RJD1LVU7sLWfdLZu4naZ5BnKAYywftcWr2HjqtPX9qr",
    "2VDW9dwsyX5Uqpz89dbdvqGwYS1x2bmawbv66m36xdG4",
    "2Z8oHwQHqApT22dgQCQhJbN6mhUv7XeZNMotTgBpQ6gG",
    "2d46SErhgpZRCEHEemDkgMTFzxwtCTXXo7hgpbLXqa6U",
];

pub fn b58(s: &str) -> Mint {
    Mint::from_bytes(pump_quant_ingest::base58::decode_pubkey(s).expect("valid pubkey"))
}

/// A stable per-mint entity id space, so cohorts cannot share entities.
pub fn ent(addr: &str, k: u64) -> u64 {
    pump_quant_ingest::social_parse::fnv1a_64(addr.as_bytes()) % 1_000 * 100_000 + k + 1
}

#[allow(clippy::too_many_arguments)]
pub fn swap(
    eng: &mut Engine,
    m: Mint,
    price_bp: i128,
    signed_base: i64,
    entity: u64,
    quote_lamports: u64,
    age_slots: u32,
    liq: u64,
) {
    eng.tick(AppEvent::MarketTrade {
        mint: m,
        price_fp: PRICE_SCALE * price_bp / 10_000,
        quote_lamports,
        liquidity_lamports: liq,
        signed_base,
        buyer_entity: entity,
        age_slots,
    });
}

pub fn ticks(eng: &mut Engine, n: u64) {
    for _ in 0..n {
        eng.tick(AppEvent::Tick);
    }
}

pub fn post(eng: &mut Engine, author: &str, addr: &str, body: &str, ts_ns: u64) {
    let json = format!(
        "{{\"platform\":\"x\",\"author\":\"{author}\",\"community\":\"\",\
         \"text\":\"{body} {addr} send\",\"likes\":40,\"reposts\":40,\
         \"is_designated_caller\":false}}"
    );
    let mut src =
        MockSocialSource::new().with_batch(vec![RawSocialPayload::new(json.into_bytes(), ts_ns)]);
    eng.ingest_social(&mut src);
}

/// **Pool depth, identical for both cohorts (lamports): a real pump.fun curve a
/// little past launch, 32 SOL of virtual SOL reserve.**
///
/// **Re-pin #26 (2026-07-28).** This was `260_000_000` — 0.26 SOL — which made the
/// tape VACUOUS as soon as `gate::decide` began deriving its impact denominator from
/// the market's own reserve (`cost_model::impact_den_for` = `vsol / 10_000`). A
/// 0.1 SOL clip into a 0.26 SOL pool is 3_846 bps of own impact a LEG; every one of
/// the eight markets refused as `EconomicallyUnviable`, both arms admitted 0, and
/// `ab_tape_is_not_vacuous` — the guard written for exactly this — fired.
///
/// **The scenario is untouched.** This tape's entire variable is WHO ENDS UP HOLDING
/// THE BASE (`launch_base` / `launch_age`): the concentrated cohort hands 98.5% of
/// the float to five creation-slot entities, the broad cohort splits it evenly, and
/// both take the same [`LAUNCH_PRINTS`] prints at the same [`PRINT_QUOTE`] into the
/// same depth. Depth is a CONTROL here, held equal across cohorts — so raising it
/// changes the level at which the experiment runs and nothing about the experiment.
/// It remains identical for both cohorts, which is the property that matters.
pub const LIQ: u64 = 32_000_000_000;
/// Confirmed sellable depth, identical for both cohorts (lamports) — just under
/// [`LIQ`], the discipline `tape_golden` uses. Re-pin #26: was `300_000_000`.
pub const DEPTH: u64 = 30_000_000_000;
/// Accumulation prints per launch, identical for both cohorts.
pub const LAUNCH_PRINTS: u64 = 25;
/// Quote lamports per accumulation print, identical for both cohorts — this is
/// what keeps the §21.7 flow-authenticity screen blind to the difference.
pub const PRINT_QUOTE: u64 = 200_000;
/// Broad-cohort per-entity base clip. `25 * 40_000 == 1_000_000`, which is
/// exactly the concentrated cohort's total (`800_000 + 4*45_000 + 20*1_000`).
pub const BROAD_CLIP: i64 = 40_000;
/// Age in slots at which the "organic" phase trades — past the sniper window and
/// still inside the §21.5 fresh-launch exemption, so the UNIVERSE screen stays
/// inert here and the A/B isolates the GATE lever.
pub const ORGANIC_AGE: u32 = 40;

/// Per-entity base allocation for a launch's accumulation phase.
///
/// **The two cohorts are NUMERICALLY INDISTINGUISHABLE by construction.** Both
/// take exactly [`LAUNCH_PRINTS`] buy prints, at exactly [`PRINT_QUOTE`] quote
/// lamports each, into exactly the same pool depth, from exactly the same number
/// of distinct entities, summing to exactly the same total base. Order-flow
/// imbalance, CVD, buyer breadth, realized volatility, quote volume, the §21.7
/// flow-authenticity screen (which is computed on QUOTE flow) and the §21.5
/// activity legs therefore carry the SAME information about both.
///
/// The ONLY thing that differs is WHO ends up holding the base: the broad cohort
/// splits it evenly, the concentrated cohort gives 98.5% of it to the first five
/// creation-slot/sniper entities. That is exactly the variable this law reads,
/// and nothing else moves — so a measured difference can only come from this law.
pub fn launch_base(concentrated: bool, k: u64) -> i64 {
    if !concentrated {
        return BROAD_CLIP;
    }
    match k {
        0 => 800_000,    // the bundler that takes the float
        1..=4 => 45_000, // the rest of the creation-slot/sniper cohort
        _ => 1_000,      // twenty organic latecomers, dust by comparison
    }
}

/// Market age in slots at the k-th accumulation print. The concentrated cohort's
/// first five land in the creation slot and the first blocks (arXiv 2601.08641
/// bundle/sniper definitions); everyone else arrives later. The broad cohort has
/// no creation-slot cohort at all.
pub fn launch_age(concentrated: bool, k: u64, age: u32) -> u32 {
    if concentrated && k <= 4 {
        u32::try_from(k).unwrap_or(0).min(SNIPER_SLOT_WINDOW)
    } else {
        age
    }
}

/// Seed one mint's launch: creation sighting (⇒ `Exact` basis), on-chain confirm,
/// then the accumulation phase described by [`launch_base`].
///
/// `wash` adds round-trip QUOTE churn — the corroborating leg the constitution
/// requires before concentration may refuse rather than merely shrink. It is the
/// one place the two cohorts' quote flow is allowed to differ, and it applies to
/// only half the concentrated cohort so that both the haircut-only path and the
/// conjunctive-veto path are exercised by the same tape.
pub fn seed_launch(eng: &mut Engine, addr: &str, concentrated: bool, wash: bool, age: u32) {
    let mt = b58(addr);
    eng.tick(AppEvent::TokenMetadata {
        mint: mt,
        category_id: 0,
        taxonomy_version: 1,
        creator: ent(addr, 7_777),
        slot: 1,
    });
    eng.tick(AppEvent::OnchainConfirm {
        mint: mt,
        sellable_depth_lamports: DEPTH,
    });
    for k in 0..LAUNCH_PRINTS {
        let e = ent(addr, k);
        let a = launch_age(concentrated, k, age);
        swap(
            eng,
            mt,
            10_000,
            launch_base(concentrated, k),
            e,
            PRINT_QUOTE,
            a,
            LIQ,
        );
        if wash && k <= 4 {
            // Bump/wash churn on the bundler cohort: quote flow that returns to
            // its origin entity, which is what the INDEPENDENT §21.7 screen sees.
            swap(eng, mt, 10_000, -1_000, e, PRINT_QUOTE * 4, a, LIQ);
            swap(eng, mt, 10_000, 1_000, e, PRINT_QUOTE * 4, age, LIQ);
        }
    }
}

/// Whether a concentrated cohort member also carries the wash leg. The first two
/// are bundled but authentically traded (corroboration ABSENT ⇒ only the haircut
/// can fire); the last two add the wash signature (⇒ the conjunctive veto is
/// reachable). Both paths are exercised by the same A/B.
pub fn washed(i: usize) -> bool {
    i >= 2
}
/// Which side of the two-sided A/B this tape is.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// HAPPY PATH: the concentrated/bundled cohort is the one that craters and
    /// the broad cohort is the one that pays. Refusing (or shrinking) the
    /// concentrated markets should redirect scarce position slots and bankroll to
    /// the broad ones.
    ConcentratedBleeds,
    /// THE MIRROR — same generator, one boolean flipped. The concentrated cohort
    /// is the one that PAYS and the broad cohort craters, so every concentration
    /// veto and haircut is a FALSE POSITIVE fired on a market that was going to
    /// work. If the law is a real edge rather than a coincidence, its loss here
    /// must be far smaller than its gain above.
    ConcentratedPays,
}

/// Deterministic price path (§22 — no RNG). A cohort that `pays` runs to ~1.9×
/// and settles at ~1.5×; one that does not fades to ~0.85×.
pub fn price_path(round: u64, pays: bool, offset: i128) -> i128 {
    let bp: i128 = if pays {
        match round {
            0 => 10_000,
            1 => 11_500,
            2 => 13_500,
            3 => 16_000,
            4 => 19_000,
            5 => 18_000,
            6 => 17_000,
            _ => 15_000,
        }
    } else {
        match round {
            0 => 10_000,
            1 => 10_600,
            2 => 10_900,
            3 => 10_400,
            4 => 9_700,
            5 => 9_100,
            _ => 8_500,
        }
    };
    bp + offset
}

/// The concentration tape's CONFIG overrides, split out of [`ab_drive`] verbatim so
/// the union tape of `law_permutation_sweep.rs` can compose them with the other
/// generators'.
pub fn conc_cfg(cfg: Config) -> Config {
    let mut cfg = cfg;
    // Same cost realism as the golden tape, so the lamport numbers are comparable.
    // COST-MODEL UNIFICATION (2026-07-28). The gate's three cost inputs —
    // protocol bps, base fixed lamports and the impact denominator — are no longer
    // config: `gate::decide` derives them per candidate from the market's own
    // SOL-side reserve via `cost_model`. The overrides that used to sit here
    // (450 / 200_000 / 250_000) are gone because they no longer decide anything;
    // what this tape must now declare honestly is its DEPTH, which is what the
    // derived impact model reads — see `LIQ` (re-pin #26).
    cfg.gate_expected_move_bps = 1_800;
    cfg.gate_margin_bps = 150;
    // Position capacity is SCARCE, so a refusal actually redirects capital rather
    // than merely removing a trade: eight candidates compete for two slots.
    cfg.max_concurrent_positions = 6;
    cfg
}

pub fn ab_drive(cfg: Config, side: Side) -> pump_quant_app::engine::Report {
    let mut eng = Engine::new(conc_cfg(cfg), RunMode::Replay);
    apply_ab(&mut eng, side);
    eng.report()
}

/// The concentration tape's EVENT SCRIPT, applied to an engine the caller owns.
/// Byte-for-byte the loop [`ab_drive`] has always run.
pub fn apply_ab(eng: &mut Engine, side: Side) {
    let eng = &mut *eng;
    let conc_pays = side == Side::ConcentratedPays;
    for (i, a) in CONC_B58.iter().enumerate() {
        seed_launch(eng, a, true, washed(i), ORGANIC_AGE);
    }
    for a in BROAD_B58 {
        seed_launch(eng, a, false, false, ORGANIC_AGE);
    }
    for (i, a) in CONC_B58.iter().chain(BROAD_B58.iter()).enumerate() {
        post(eng, "seedcaller", a, "seed call", 900_000_000 + i as u64);
    }
    ticks(eng, 8);

    for round in 0..9u64 {
        for (i, a) in CONC_B58.iter().enumerate() {
            let mt = b58(a);
            let px = price_path(round, conc_pays, i as i128);
            for k in 0..3u64 {
                let e = ent(a, 500 + round * 3 + k);
                swap(eng, mt, px, 300, e, 40_000, ORGANIC_AGE, LIQ);
            }
            // The bundled cohort is the LOUDER one — manufactured hype is exactly
            // what a bundled launch buys (§21.7(d): purchased volume plus
            // purchased trending placement implies a sponsor paying for exit
            // liquidity). Two callers per round versus the broad cohort's one, so
            // the concentrated markets OUTRANK the healthy ones in promotion and
            // would take the scarce position slots if nothing refused them.
            post(
                eng,
                "conccaller",
                a,
                &format!("conc r{round} i{i}"),
                1_000_000_000 + round * 20_000_000 + 100 + i as u64,
            );
        }
        for (i, a) in BROAD_B58.iter().enumerate() {
            let mt = b58(a);
            let px = price_path(round, !conc_pays, i as i128);
            for k in 0..3u64 {
                let e = ent(a, 500 + round * 3 + k);
                swap(eng, mt, px, 300, e, 40_000, ORGANIC_AGE, LIQ);
            }
            post(
                eng,
                "broadcaller",
                a,
                &format!("broad r{round} i{i}"),
                1_000_000_000 + round * 20_000_000 + i as u64,
            );
        }
        ticks(eng, 8);
    }
}
