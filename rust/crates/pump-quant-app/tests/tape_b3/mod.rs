//! The **LAW B3 hazard tape** and the **winners (false-positive) tape**, hoisted
//! verbatim out of `tests/brain_laws.rs` so more than one test binary can drive the
//! same generators.
//!
//! Nothing here was rewritten for the law-permutation sweep. `brain_laws.rs` still
//! owns LAWs B1-B5 and their assertions; this module owns only the event script and
//! the `hazard_cfg()` the A/B has always used.
#![allow(dead_code)]

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, Report, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_app::journal_log::Decision;
use pump_quant_domain::ids::Mint;

pub const PRICE_SCALE: i128 = 10_000_000;

/// Deterministic mint tag → 32-byte id, distinct from every other test's cohort.
pub fn mint(tag: u64) -> Mint {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&tag.to_le_bytes());
    b[8] = 0xB1;
    Mint::from_bytes(b)
}

pub fn ticks(eng: &mut Engine, n: u64) {
    for _ in 0..n {
        eng.tick(AppEvent::Tick);
    }
}

/// Liquidity of the BLEEDING setup class: a curve at **LAUNCH depth**, 30 SOL of
/// virtual SOL reserve ([`pump_quant_app::curve_state::LAUNCH_VSOL_LAMPORTS`]) —
/// signed decade 10, `liquidity_decade` ladder bucket 3.
///
/// **Re-pin #26 (2026-07-28): this was 4 SOL and is now real.** The number is no
/// longer decorative. Since the cost-model unification `gate::decide` derives the
/// gate's impact denominator from the market's own reserve
/// ([`pump_quant_app::cost_model::impact_den_for`] — `vsol / 10_000`), so a declared
/// depth is now a PRICE. At 4 SOL a 0.02 SOL clip priced at 50 bps a leg; that was
/// survivable, but it is not a depth pump.fun has — the curve is seeded with 30 SOL
/// of virtual reserve and no market on it is ever thinner. The old figure made the
/// bleeding class expensive for a reason that does not exist on the venue.
///
/// The SCENARIO is unchanged, and is in fact sharper: the bleeding class is now a
/// FRESH LAUNCH at the shallowest depth the curve can present, which is exactly what
/// "thin, seconds-old, narrow-breadth, on net-sell flow" describes.
///
/// **Re-pin #27 (2026-07-28): 30.0 -> 30.3 SOL.** The seed reserve exactly is a curve
/// **nobody has bought into**, which escrows zero SOL and can pay a seller nothing;
/// the bleeding class would have been refused for having no capacity rather than for
/// bleeding. 0.3 SOL of raise is the shallowest curve that can still fund a 0.1 SOL
/// floor clip, keeps the same signed liquidity decade, and leaves own-impact on that
/// clip unchanged at 33 bps a leg.
pub const THIN_LIQUIDITY: u64 = 30_300_000_000;
/// Liquidity of the HEALTHY setup class: a curve **near graduation**, 110 SOL of
/// virtual SOL reserve — signed decade 11, `liquidity_decade` ladder bucket 4, one
/// ladder bucket away from [`THIN_LIQUIDITY`], so the two classes still never share
/// a `liquidity_decade`.
///
/// **Re-pin #26: this was 400 SOL, which is not a bonding curve at all.** The curve
/// is exhausted at [`pump_quant_app::curve_state::GRADUATION_VSOL_LAMPORTS`] =
/// 115.005 SOL, and `cost_model::venue_fee_bps_per_leg` charges 30 bps a leg at or
/// above it against 125 bps below. A 400 SOL "pool" therefore did not merely
/// exaggerate depth — it silently moved the HEALTHY class onto the POST-GRADUATION
/// fee tier while the BLEEDING class paid the curve rate, putting a 190 bps a leg
/// fee difference on the one axis the A/B is not supposed to vary. 110 SOL keeps
/// both classes on the curve, on the same fee schedule, differing only in depth.
///
/// The scenario survives intact: a mature market that has raised ~80 SOL and sits
/// one print from migrating is precisely the "deep, ~33 minutes old, broad
/// participation" healthy class this cohort was written to express.
pub const DEEP_LIQUIDITY: u64 = 110_000_000_000;
/// The SOL the BLEEDING curve actually escrows: `THIN_LIQUIDITY - 30 SOL`.
/// Re-pin #27: was 29 SOL claimed against a curve holding 0.
pub const THIN_SELLABLE: u64 = THIN_LIQUIDITY - 30_000_000_000;
/// The SOL the HEALTHY curve actually escrows: `DEEP_LIQUIDITY - 30 SOL` = 80 SOL,
/// i.e. a curve that has raised ~80 of the 85 it needs to graduate. Re-pin #27: was
/// 105 SOL, which is 25 SOL more than the pool can ever hold.
pub const DEEP_SELLABLE: u64 = DEEP_LIQUIDITY - 30_000_000_000;
/// Market age of the BLEEDING class: seconds old (`token_age` bucket 0).
pub const FRESH_AGE_SLOTS: u32 = 12;
/// Market age of the HEALTHY class: ~33 minutes of information time (bucket 3).
pub const MATURE_AGE_SLOTS: u32 = 5_000;

/// One trade for `m`, fully parameterized so the two hazard classes can be made
/// to quantize far apart while sharing a discovery lane.
#[allow(clippy::too_many_arguments)]
pub fn one_at(
    eng: &mut Engine,
    m: Mint,
    price_mult: i128,
    signed_base: i64,
    entity: u64,
    liquidity_lamports: u64,
    age_slots: u32,
) {
    eng.tick(AppEvent::MarketTrade {
        mint: m,
        price_fp: price_mult * PRICE_SCALE,
        quote_lamports: 800_000,
        liquidity_lamports,
        signed_base,
        buyer_entity: entity,
        age_slots,
    });
}

pub fn one(eng: &mut Engine, m: Mint, price_mult: i128, signed_base: i64, entity: u64) {
    one_at(
        eng,
        m,
        price_mult,
        signed_base,
        entity,
        THIN_LIQUIDITY,
        FRESH_AGE_SLOTS,
    );
}

pub fn narrate(eng: &mut Engine, m: Mint) {
    eng.tick(AppEvent::NarrativeSample {
        mint: m,
        prior_active: 5,
        new_mentions: 9_000,
    });
}

/// Discover `m` through the narrative lane, prove on-chain depth, and admit it —
/// the BLEEDING class's opening script: a fresh, thin, narrow-breadth, flat market
/// on net-SELL flow. Every cohort member runs it verbatim, so every admit quantizes
/// to the SAME setup class; that identity is what makes the class RECUR, which is
/// what LAW B3 needs in order to have anything to recall.
pub fn seed_and_admit(eng: &mut Engine, m: Mint, entity_base: u64) {
    for _ in 0..4 {
        narrate(eng, m);
    }
    for i in 0..10u64 {
        // Only 3 distinct buyers ⇒ the narrow end of the breadth ladder.
        one(eng, m, 100, -500_000, entity_base + i % 3);
    }
    eng.tick(AppEvent::OnchainConfirm {
        mint: m,
        virtual_sol_lamports: THIN_LIQUIDITY,
        real_sol_lamports: THIN_SELLABLE,
    });
    narrate(eng, m);
    ticks(eng, 2);
}

/// The cohort's shared FATE: a −40% single-swap collapse (rug precursor) that books
/// the loss and closes the position.
pub fn crater(eng: &mut Engine, m: Mint, entity: u64) {
    one(eng, m, 60, -1_500_000, entity);
    ticks(eng, 4);
    // Let the position close and the discovery evidence go stale before the next
    // cohort member arrives, so each round is a clean sequential round trip.
    ticks(eng, 20);
}

/// A config with the operator floor relaxed enough that a small hazard tape can
/// open many sequential positions off a 2-SOL bankroll, and with the discovery
/// evidence going stale fast so a freed slot does not silently re-admit an old
/// cohort member (which would confound the A/B axis).
pub fn hazard_cfg() -> Config {
    let mut cfg = Config::dev_portable();
    // Pin the Phase 1 production defaults this tape was NOT calibrated against.
    cfg.gate_exit_tranches = 3;
    cfg.promote_min_haircut_bp = 8_000;
    cfg.expected_move_model_enable = false;
    cfg.expected_move_min_sample = 30;
    cfg.watchlist_ttl_ticks = 8;
    cfg.lane_evidence_ttl_ticks = 8;
    // 0.02 SOL clips: the hazard tape needs ~14 sequential round trips, which a
    // 0.1-SOL floor on a 2-SOL bankroll cannot fund once the losses start. The
    // floor is an operator policy knob (§102), not a law under test here.
    cfg.min_trade_size_lamports = 20_000_000;
    cfg.f_base_bp = 400;
    cfg.total_risk_cap_bp = 4_000;
    cfg.max_concurrent_positions = 8;
    cfg.x_min_promote_cap_bp = 500;
    // A cohort member that has already cratered must not squat a slot for the rest
    // of the tape: with only 3 concurrent slots a zombie position would starve the
    // recurrence the A/B is measuring (every later admit would be a MAX_CONCURRENT
    // reject in BOTH arms, making the comparison vacuous). Tight stall/time-stop
    // windows retire it. Both arms share these values, so the axis stays clean.
    cfg.lc_stall_ticks = 5;
    cfg.lc_max_hold_ticks = 18;
    // Tighten the similarity radius from the brain's default 12 to 6. The two
    // classes on the hazard tape sit many ordinal buckets apart (opposite ends of the
    // OFI ladder, plus liquidity/age/breadth), which the default radius would POOL —
    // and a pooled estimate over a profitable class and a bleeding one is exactly
    // the §100-style error the whole design exists to avoid. 6 keeps them separate
    // while still matching within-class variation (which is 1 bucket here).
    // Operator knob (§102); identical in both arms, so the axis stays clean.
    cfg.brain_recall_max_distance = 3;
    // Neutralize the §23 arbitration's minimum-expected-net floor in BOTH arms.
    // That floor is driven by the §24 EXPECTANCY_V1 estimator, which pools every
    // setup a LANE produces into one mean — a competing, lane-conditioned law. On
    // this tape it would refuse the whole lane once the bleeding class dragged the
    // pooled mean down, masking the axis under test and making the A/B vacuous.
    // Isolating one law by neutralizing its competitor IN BOTH ARMS is the same
    // discipline the §29.5 exit-pressure A/B uses; the measured delta is then
    // attributable to LAW B3 and nothing else.
    cfg.arb_min_expected_net_lamports = i64::MIN / 4;
    cfg
}
// ===========================================================================
// LAW B3 — the reduce-only recall haircut/veto, A/B on a bleeding-class tape.
// ===========================================================================

// ---------------------------------------------------------------------------
// The B3 hazard tape: TWO setup classes inside ONE discovery lane.
//
// This shape is the whole quant argument for episodic recall over the §24
// per-lane conditional expectancy the engine already runs. EXPECTANCY_V1 pools
// every setup a lane produces into one mean; on a tape where the lane's OTHER
// setups pay well, a lane-pooled estimator stays happily positive while one
// specific SETUP CLASS bleeds on every single occurrence. Recall conditions on the
// setup, so it can see what the pooled estimator structurally cannot.
//
//   * class HEALTHY — a near-graduation curve (110 SOL), mature, broad
//     participation, choppy pre-entry tape; price rips to 2.5x.
//   * class BLEEDER — a launch curve (30 SOL), seconds old, three buyers, flat
//     pre-entry tape, net-SELL flow; price craters 40%.
//
// Re-pin #26 gave both classes REAL pump.fun depth (the gate now derives its impact
// denominator from the declared reserve, so a stylized depth is a mispriced trade,
// not a harmless label). The two still quantize far apart — opposite ends of the OFI
// and CVD ladders, one liquidity decade, two token-age buckets and two breadth
// buckets — so recall never pools them — while the WATCHLIST lane
// and the §71.2 discovery lane are identical for both, so the lane-pooled
// expectancy sees one blended, profitable stream.
//
// Rounds alternate HEALTHY, BLEEDER, HEALTHY, BLEEDER…. Until the bleeding class
// clears the §46 sample floor both arms are byte-identical; after it does, the
// armed arm must refuse every further BLEEDER while still taking every HEALTHY.
// ---------------------------------------------------------------------------

/// Seed + admit one HEALTHY-class member.
///
/// SAME discovery lane and SAME net-SELL flow sign as the bleeding class — that is
/// deliberate and load-bearing: if the two classes fell in different watchlist
/// lanes, the engine's existing §24 per-lane conditional expectancy would already
/// separate them and LAW B3 would be measuring nothing. They are separated ONLY on
/// axes the lane-pooled estimator cannot see: pool depth (a launch curve against a
/// near-graduation one — one liquidity decade since re-pin #26 gave both classes real
/// depth), market age (fresh vs ~33 min), buyer breadth (3 vs 24 distinct entities)
/// and realized volatility (flat vs choppy pre-entry).
pub fn seed_healthy(eng: &mut Engine, m: Mint, entity_base: u64) {
    for _ in 0..4 {
        narrate(eng, m);
    }
    for i in 0..12u64 {
        // Choppy pre-entry tape (realized-vol ladder) and broad participation.
        let px = 100 + i128::from(i as i64 % 4) * 3;
        one_at(
            eng,
            m,
            px,
            -500_000,
            entity_base + i * 2,
            DEEP_LIQUIDITY,
            MATURE_AGE_SLOTS,
        );
    }
    eng.tick(AppEvent::OnchainConfirm {
        mint: m,
        virtual_sol_lamports: DEEP_LIQUIDITY,
        real_sol_lamports: DEEP_SELLABLE,
    });
    narrate(eng, m);
    ticks(eng, 2);
}

/// The HEALTHY class's fate: a clean rip that banks the take-profit ladder.
pub fn rip(eng: &mut Engine, m: Mint, entity: u64) {
    for px in [130i128, 170, 210, 250] {
        one_at(
            eng,
            m,
            px,
            -500_000,
            entity,
            DEEP_LIQUIDITY,
            MATURE_AGE_SLOTS,
        );
        ticks(eng, 1);
    }
    ticks(eng, 24);
}

/// Rounds on the hazard tape — the length `brain_laws.rs` has always driven.
pub const HAZARD_ROUNDS: u64 = 40;

/// Rounds of the MIRROR tape's learning phase: the prefix in which the bleeding
/// class establishes its record, byte-identical to the hazard tape. Half the tape,
/// which is well past the §46 sample floor, so the flag LAW B3 acts on in the
/// forward phase is built on exactly the same evidence in both regimes.
pub const MIRROR_LEARN_ROUNDS: u64 = 20;

/// The MIRROR of [`crater`]: the flagged class's forward members walk the HEALTHY
/// class's own payoff ladder (`rip`'s 130/170/210/250 rungs) on their own thin/fresh
/// class script, on net-BUY flow.
///
/// The ladder is deliberately the strongest false positive the tape can express —
/// the refused class now performs exactly as well as the very best alternative the
/// freed capital could be redeployed into. A weaker mirror (say a +40% sign-flip of
/// `crater`) would let LAW B3 win on opportunity cost alone, which would not be a
/// test of the flag being WRONG. It is `rip`'s existing constants, not a ladder
/// chosen to reach a number.
///
/// Nothing before the fill differs: the entry script is [`seed_and_admit`] in both
/// regimes, so the fingerprint sealed AT ADMIT is byte-identical and the two regimes
/// are indistinguishable at the moment the decision is taken. Only the fate differs
/// — which is precisely what makes the mirror a FALSE-POSITIVE test of LAW B3 rather
/// than a different test.
pub fn moon(eng: &mut Engine, m: Mint, entity: u64) {
    for px in [130i128, 170, 210, 250] {
        one(eng, m, px, 1_500_000, entity);
        ticks(eng, 1);
    }
    ticks(eng, 24);
}

/// The hazard tape's EVENT SCRIPT, applied to an engine the caller owns.
///
/// Split out of [`drive_two_class_hazard`] with no change to the emitted events, so
/// the same script can also be concatenated onto a shared engine (the union tape of
/// `law_permutation_sweep.rs`). The two-argument `drive_*` wrapper below is the
/// original entry point and is still what `brain_laws.rs` calls.
pub fn apply_two_class_hazard(eng: &mut Engine, rounds: u64) {
    apply_two_class_hazard_sided(eng, rounds, rounds, false);
}

/// The two-class hazard tape with an explicit LEARN / FORWARD split — one generator,
/// one boolean, exactly the shape LAW B7's and the §21.7 concentration law's
/// two-sided tapes already use.
///
/// * `learn_rounds == rounds` (and `forward_bleeder_pays == false`) reproduces the
///   original hazard tape byte for byte; that is what [`apply_two_class_hazard`]
///   asks for and what `brain_laws.rs` has always driven.
/// * `forward_bleeder_pays == true` makes the tape LAW B3's honest MIRROR: the
///   bleeding class's record is established identically over the learning phase, and
///   then every forward recurrence of that class PAYS. Every refusal LAW B3 makes
///   after the learning phase is therefore a FALSE POSITIVE fired on a market that
///   was going to work, and the law's give-back is directly measurable.
pub fn apply_two_class_hazard_sided(
    eng: &mut Engine,
    rounds: u64,
    learn_rounds: u64,
    forward_bleeder_pays: bool,
) {
    for k in 0..rounds {
        let healthy = mint(5_000 + k);
        seed_healthy(eng, healthy, 1_000 + k * 20);
        rip(eng, healthy, 1_015 + k * 20);
        let bleeder = mint(6_000 + k);
        seed_and_admit(eng, bleeder, 40_000 + k * 20);
        if forward_bleeder_pays && k >= learn_rounds {
            moon(eng, bleeder, 40_015 + k * 20);
        } else {
            crater(eng, bleeder, 40_015 + k * 20);
        }
    }
}

pub fn drive_two_class_hazard(cfg: Config, rounds: u64) -> (Report, Engine) {
    let mut eng = Engine::new(cfg, RunMode::Replay);
    apply_two_class_hazard(&mut eng, rounds);
    let r = eng.report();
    (r, eng)
}

/// Rounds on the B3 **mirror** (false-positive) tape: a setup class that WINS every
/// time. Verbatim the cohort `brain_laws.rs::b3_is_reduce_only_and_never_enlarges_a_
/// winning_class` has always driven; hoisted so the sweep can use it as LAW B3's
/// two-sided mirror without building anything new.
pub const WINNERS_ROUNDS: u64 = 14;

/// The winners tape's EVENT SCRIPT, applied to an engine the caller owns.
pub fn apply_winners(eng: &mut Engine) {
    for k in 0..WINNERS_ROUNDS {
        let m = mint(7_500 + k);
        seed_healthy(eng, m, 2_000 + k * 20);
        rip(eng, m, 2_015 + k * 20);
    }
}

pub fn drive_winners(cfg: Config) -> (Report, Engine) {
    let mut eng = Engine::new(cfg, RunMode::Replay);
    apply_winners(&mut eng);
    let r = eng.report();
    (r, eng)
}

/// Journalled rejections carrying the LAW B3 reject code.
pub const REJECT_BRAIN_BLED: u8 = 16;

pub fn brain_bled_rejects(eng: &Engine) -> usize {
    eng.journal()
        .recent()
        .filter(|d| matches!(**d, Decision::Rejected { reason, .. } if reason == REJECT_BRAIN_BLED))
        .count()
}

/// Realized net attributed to the mints of one cohort (`tag_base..tag_base+n`).
pub fn cohort_net(eng: &Engine, tag_base: u64, n: u64) -> i128 {
    let tags: Vec<[u8; 32]> = (0..n).map(|k| *mint(tag_base + k).as_bytes()).collect();
    eng.journal()
        .recent()
        .filter_map(|d| match *d {
            Decision::Filled {
                mint: mb,
                net_pnl_lamports,
                ..
            } if tags.contains(&mb) => Some(net_pnl_lamports),
            _ => None,
        })
        .sum()
}
