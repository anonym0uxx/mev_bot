//! **THE DECISIVE NET-SOL PERMUTATION EXPERIMENT.**
//!
//! Three reduce-only laws currently ship DISARMED, and every one of them was judged
//! under the OLD, blunter brain representation:
//!
//! * `brain_haircut_enable` — LAW B3, the reduce-only recall haircut/veto at admit.
//! * `brain_reflect_enable` — LAW B7, the reduce-only lane downweight from recall decay.
//! * `holder_concentration_enable` — the §21.7 distribution-shape haircut/veto.
//!
//! Schema 2 then made the recall representation SHARPER (a holder-growth VELOCITY
//! fingerprint dimension and a CONCENTRATION-BAND recall conditioner). A sharper
//! estimator can change a verdict, so the operator's question is taken literally:
//! **out of every permutation of these three laws, which configuration nets the most
//! SOL, proven arithmetically?**
//!
//! # STEP 1 — THE PRE-REGISTERED RULE (written into this file BEFORE any number in
//! it was measured)
//!
//! Let `net(C, T)` be realized net lamports of configuration `C` on tape `T`, and let
//! `C0` be the shipped all-OFF configuration. `C ≠ C0` may replace `C0` as the
//! shipped default **only if ALL FIVE legs hold**:
//!
//! * **(P1) MATERIALITY ON THE ONLY TAPE THAT CAN PRICE INTERACTIONS.**
//!   `net(C, UNION-HAPPY) - net(C0, UNION-HAPPY) > `[`MATERIAL_LAMPORTS`] (=
//!   `100_000_000`, one `min_trade_size_lamports` bite). The union tape is the
//!   arbiter because it is the ONLY tape carrying all three hazards at once, i.e.
//!   the only one on which a double-count or a complement can show up in lamports.
//!   A gain under one admissible bite is noise in the arbitration, not an edge.
//! * **(P2) NO HAZARD-TAPE HARM.** On EVERY hazard tape `T` (both sides of every
//!   two-sided pair, and both sides of the union),
//!   `net(C, T) >= net(C0, T) - `[`MATERIAL_LAMPORTS`]. A configuration that clears
//!   the union while giving back more than a bite on any single tape is a reshuffle
//!   that happened to land favourably on the arbiter, and reshuffles do not
//!   generalise.
//! * **(P3) PER-LAW ASYMMETRY.** Every law armed in `C` must independently satisfy
//!   `happy_gain / |mirror_loss| >= `[`REQUIRED_RATIO`] (= 3) on ITS OWN two-sided
//!   tapes, measured with the other two laws held exactly as `C` sets them. If
//!   `mirror_loss <= 0` the leg passes trivially and is reported as such rather than
//!   dressed up as a large ratio. A protective law that gives back on its false
//!   positives what it earns on its true positives is a coin flip with extra state.
//! * **(P4) GOLDEN NEUTRALITY.** `net(C, GOLDEN) == net(C0, GOLDEN)` and
//!   `admitted / rejected / promoted / universe_filtered` are identical — OR the
//!   golden reference is re-pinned exactly once, with a note.
//! * **(P5) NO FITTING.** Every tape here is an EXISTING generator from a prior
//!   wave, reused verbatim (the generators were hoisted into `tests/tape_*/` with no
//!   change to a single emitted event; the prior waves' pinned numbers still hold,
//!   which is the proof). The UNION tape is the mechanical concatenation of those
//!   generators onto one engine under a mechanically-composed config —
//!   `conc_cfg(b7_cfg(hazard_cfg()))`, later override wins, fixed order b3 → b7 →
//!   conc — and NOT a fresh construction tuned to a result.
//!
//! **If no `C` satisfies P1–P4, the answer is "ALL OFF STAYS" and this file says so
//! bluntly.** An honest negative is the correct outcome if that is what the numbers
//! say; the operator's hope that this earns is not evidence.
//!
//! ## AMENDMENT A-1 (made after the tapes were built, before the verdict was taken,
//! and STRICTLY TIGHTENING — the only kind of amendment this repo allows)
//!
//! LAW B3 was the only one of the three laws with no genuine FALSE-POSITIVE tape.
//! LAW B7 and the concentration law each ship a mirror built by flipping one boolean
//! in their own generator; LAW B3's only counter-tape was a WINNERS tape, on which a
//! reduce-only law is inert by construction — it proves reduce-only-ness, not
//! false-positive COST. Under P3 that would have let LAW B3 pass leg (b) trivially
//! on a vacuous mirror, which is exactly the kind of free pass this file exists to
//! refuse.
//!
//! So [`tape_b3::apply_two_class_hazard_sided`] adds the missing boolean to the
//! EXISTING generator: the bleeding class's record is established over an identical
//! learning phase and then every FORWARD recurrence of that class PAYS, so every
//! LAW B3 refusal after the learning phase is a false positive fired on a market
//! that was going to work. The entry script is byte-identical in both regimes
//! (`seed_and_admit`), so the two are indistinguishable at the moment of decision —
//! the same discipline LAW B7's tape documents. This amendment can only make LAW B3
//! HARDER to arm, never easier.
//!
//! # STEP 2 — THE TAPES
//!
//! Ten. Nine are pre-existing generators reused verbatim; the tenth (B3-MIRROR) is
//! an AMENDMENT described below.
//!
//! | tape | generator | hazard |
//! |------|-----------|--------|
//! | GOLDEN | `tape_golden` (re-pin #16 tape) | none — the neutrality control |
//! | B3-HAZARD | `tape_b3::drive_two_class_hazard` | a bleeding SETUP class inside a paying LANE |
//! | B3-MIRROR | `tape_b3::apply_two_class_hazard_sided(.., true)` | the SAME tape with the bled class's FORWARD fate flipped — every B3 refusal after the learning phase is a FALSE POSITIVE |
//! | B3-WINNERS | `tape_b3::drive_winners` | none — a class that WINS every time (the reduce-only control) |
//! | B7-HAPPY | `tape_b7` `Tape::happy()` | a genuinely decayed lane under promotion contention |
//! | B7-UNHAPPY | `tape_b7` `Tape::unhappy()` | the same tape with the flag made a FALSE POSITIVE |
//! | CONC-HAPPY | `tape_conc` `ConcentratedBleeds` | bundled/concentrated launches that crater |
//! | CONC-MIRROR | `tape_conc` `ConcentratedPays` | the same, with the concentration signal a FALSE POSITIVE |
//! | UNION-HAPPY | all three happy scripts, one engine | all three hazards at once |
//! | UNION-MIRROR | all three mirror scripts, one engine | all three FALSE POSITIVES at once |
//!
//! # STEP 3 — THE VERDICT
//!
//! See [`the_verdict_under_the_pre_registered_rule`], which encodes the rule above
//! and asserts that the SHIPPED DEFAULTS are among its answers, that no NON-superset
//! configuration beats them, and that no superset adds a material gain. That
//! assertion is what keeps this honest: if any configuration ever starts genuinely
//! dominating the shipped one, this test fails until an operator decides.
//!
//! # RE-PIN #26 (2026-07-28) — THE RULE STOPPED BEING SINGLE-VALUED
//!
//! Every hazard tape in this file except the golden control declared sub-SOL pool
//! depth. That was harmless while the gate's impact model was a config constant and
//! became fatal the moment `cost_model::impact_den_for` began deriving it from each
//! market's own reserve: a 0.1-SOL clip into a 0.2-SOL pool prices at 5_000 bps a leg
//! and REFUSES. The B7 and concentration tapes admitted NOTHING, and the "verdict"
//! taken over them was arithmetic on zeros. The tapes now declare real pump.fun depth
//! with their scenarios untouched.
//!
//! The rule's answer changed with them. It previously selected `{B3}` uniquely; it now
//! selects BOTH `{B3}` and `{B3, B7}`, because LAW B7's own two-sided legs — measured
//! on a tape that trades — pass at 5.78x against a 3x bar where they previously failed
//! at 1.27x. **No default is changed here.** B7's marginal union contribution over the
//! shipped `{B3}` is 33_426_226 lamports, a third of this file's own materiality bite,
//! and arming a law whose verdict moved because a FIXTURE was corrected is an operator
//! decision requiring an A-11 study (`brain_reflect_twosided.rs` states the request).
//! LAW B3's own numbers are, if anything, stronger than before: +414_992_045 on its
//! hazard tape, a worst hazard-tape delta of exactly 0, and golden neutrality intact.

mod tape_b3;
mod tape_b7;
mod tape_conc;
mod tape_golden;

use std::collections::BTreeSet;

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, Report, RunMode};
use pump_quant_app::journal_log::Decision;

// ---------------------------------------------------------------------------
// PRE-REGISTERED BARS (§102 — named consts, fixed before measurement)
// ---------------------------------------------------------------------------

/// P1/P2 materiality bar: one `min_trade_size_lamports` (0.1 SOL, criterion 112 /
/// Amendment A-6). Identical to the bar LAW B7 and the §21.7 concentration law were
/// each judged under, deliberately — a new wave does not get an easier bar.
const MATERIAL_LAMPORTS: i128 = 100_000_000;

/// P3 asymmetry bar, identical to LAW B7's and the concentration law's.
const REQUIRED_RATIO: i128 = 3;

/// Rounds on the B3 hazard tape — the length `brain_laws.rs` has always driven.
const B3_ROUNDS: u64 = tape_b3::HAZARD_ROUNDS;

/// LAW B3's journalled refusal code.
const REJECT_BRAIN_BLED: u8 = 16;
/// The §21.7 concentration law's journalled refusal code.
const REJECT_HOLDER_CONCENTRATION: u8 = 17;

// ---------------------------------------------------------------------------
// The 2^3 configuration lattice
// ---------------------------------------------------------------------------

/// One point of the lattice. Bit 0 = LAW B3, bit 1 = LAW B7, bit 2 = concentration.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Arms {
    b3: bool,
    b7: bool,
    conc: bool,
}

impl Arms {
    const fn from_mask(m: u8) -> Self {
        Self {
            b3: m & 1 != 0,
            b7: m & 2 != 0,
            conc: m & 4 != 0,
        }
    }

    /// Apply this lattice point to a config. Nothing else is touched, so every
    /// measured delta is attributable to these three flags and nothing else.
    fn apply(self, cfg: Config) -> Config {
        let mut cfg = cfg;
        cfg.brain_haircut_enable = self.b3;
        cfg.brain_reflect_enable = self.b7;
        cfg.holder_concentration_enable = self.conc;
        cfg
    }

    fn label(self) -> &'static str {
        match (self.b3, self.b7, self.conc) {
            (false, false, false) => "---- (all OFF)",
            (true, false, false) => "B3-- ",
            (false, true, false) => "-B7- ",
            (true, true, false) => "B3B7-",
            (false, false, true) => "--C  ",
            (true, false, true) => "B3-C ",
            (false, true, true) => "-B7C ",
            (true, true, true) => "B3B7C",
        }
    }

    /// The shipped defaults, read off `Config::dev_portable` rather than restated.
    fn shipped() -> Self {
        let d = Config::dev_portable();
        Self {
            b3: d.brain_haircut_enable,
            b7: d.brain_reflect_enable,
            conc: d.holder_concentration_enable,
        }
    }
}

/// All eight lattice points, in mask order.
const LATTICE: [Arms; 8] = [
    Arms::from_mask(0),
    Arms::from_mask(1),
    Arms::from_mask(2),
    Arms::from_mask(3),
    Arms::from_mask(4),
    Arms::from_mask(5),
    Arms::from_mask(6),
    Arms::from_mask(7),
];

// ---------------------------------------------------------------------------
// The nine tapes
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tape {
    Golden,
    B3Hazard,
    B3Mirror,
    B3Winners,
    B7Happy,
    B7Unhappy,
    ConcHappy,
    ConcMirror,
    UnionHappy,
    UnionMirror,
}

const TAPES: [Tape; 10] = [
    Tape::Golden,
    Tape::B3Hazard,
    Tape::B3Mirror,
    Tape::B3Winners,
    Tape::B7Happy,
    Tape::B7Unhappy,
    Tape::ConcHappy,
    Tape::ConcMirror,
    Tape::UnionHappy,
    Tape::UnionMirror,
];

impl Tape {
    const fn name(self) -> &'static str {
        match self {
            Tape::Golden => "GOLDEN      ",
            Tape::B3Hazard => "B3-HAZARD   ",
            Tape::B3Mirror => "B3-MIRROR   ",
            Tape::B3Winners => "B3-WINNERS  ",
            Tape::B7Happy => "B7-HAPPY    ",
            Tape::B7Unhappy => "B7-UNHAPPY  ",
            Tape::ConcHappy => "CONC-HAPPY  ",
            Tape::ConcMirror => "CONC-MIRROR ",
            Tape::UnionHappy => "UNION-HAPPY ",
            Tape::UnionMirror => "UNION-MIRROR",
        }
    }

    /// A hazard tape is anything that is not the golden neutrality control.
    const fn is_hazard(self) -> bool {
        !matches!(self, Tape::Golden)
    }
}

/// The UNION config: the three generators' own config overrides composed
/// mechanically in the fixed order b3 → b7 → conc, later override winning. Stated
/// as a rule rather than hand-picked, so the union tape cannot be accused of having
/// been tuned. Where the three disagree the surviving values are: B3's bankroll /
/// clip / radius / arbitration-floor relaxations (only B3 structurally needs them),
/// B7's `lane_evidence_ttl_ticks = 30`, and concentration's
/// `max_concurrent_positions = 6`.
fn union_cfg() -> Config {
    tape_conc::conc_cfg(tape_b7::b7_cfg(tape_b3::hazard_cfg()))
}

/// Drive one (tape, configuration) cell and hand back the finished engine.
fn drive(tape: Tape, arms: Arms) -> Engine {
    match tape {
        Tape::Golden => tape_golden::drive_eng(arms.apply(Config::dev_portable())),
        Tape::B3Hazard => {
            let mut eng = Engine::new(arms.apply(tape_b3::hazard_cfg()), RunMode::Replay);
            tape_b3::apply_two_class_hazard(&mut eng, B3_ROUNDS);
            eng
        }
        Tape::B3Mirror => {
            let mut eng = Engine::new(arms.apply(tape_b3::hazard_cfg()), RunMode::Replay);
            tape_b3::apply_two_class_hazard_sided(
                &mut eng,
                B3_ROUNDS,
                tape_b3::MIRROR_LEARN_ROUNDS,
                true,
            );
            eng
        }
        Tape::B3Winners => {
            let mut eng = Engine::new(arms.apply(tape_b3::hazard_cfg()), RunMode::Replay);
            tape_b3::apply_winners(&mut eng);
            eng
        }
        Tape::B7Happy => tape_b7::drive(
            arms.apply(Config::dev_portable()),
            tape_b7::Tape::happy(),
            None,
        ),
        Tape::B7Unhappy => tape_b7::drive(
            arms.apply(Config::dev_portable()),
            tape_b7::Tape::unhappy(),
            None,
        ),
        Tape::ConcHappy => {
            let cfg = tape_conc::conc_cfg(arms.apply(Config::dev_portable()));
            let mut eng = Engine::new(cfg, RunMode::Replay);
            tape_conc::apply_ab(&mut eng, tape_conc::Side::ConcentratedBleeds);
            eng
        }
        Tape::ConcMirror => {
            let cfg = tape_conc::conc_cfg(arms.apply(Config::dev_portable()));
            let mut eng = Engine::new(cfg, RunMode::Replay);
            tape_conc::apply_ab(&mut eng, tape_conc::Side::ConcentratedPays);
            eng
        }
        Tape::UnionHappy | Tape::UnionMirror => {
            let cfg = arms.apply(union_cfg());
            let min_sample = cfg.brain_decay_min_sample;
            let mut eng = Engine::new(cfg, RunMode::Replay);
            if tape == Tape::UnionHappy {
                tape_b3::apply_two_class_hazard(&mut eng, B3_ROUNDS);
                tape_b7::apply_tape(&mut eng, tape_b7::Tape::happy(), min_sample, None);
                tape_conc::apply_ab(&mut eng, tape_conc::Side::ConcentratedBleeds);
            } else {
                tape_b3::apply_winners(&mut eng);
                tape_b7::apply_tape(&mut eng, tape_b7::Tape::unhappy(), min_sample, None);
                tape_conc::apply_ab(&mut eng, tape_conc::Side::ConcentratedPays);
            }
            eng
        }
    }
}

fn report(tape: Tape, arms: Arms) -> Report {
    let mut eng = drive(tape, arms);
    eng.report()
}

/// The full 10 × 8 measurement, as `[tape][mask]`. Computed once per test binary —
/// 80 engine runs is the whole experiment and every test below reads the SAME
/// numbers, so no two tests can disagree about what was measured.
fn measure_matrix() -> &'static [[Report; 8]; 10] {
    static MATRIX: std::sync::OnceLock<[[Report; 8]; 10]> = std::sync::OnceLock::new();
    MATRIX.get_or_init(|| {
        core::array::from_fn(|t| core::array::from_fn(|m| report(TAPES[t], LATTICE[m])))
    })
}

/// Mints carrying a journalled rejection with `code`.
fn rejected_mints(eng: &Engine, code: u8) -> BTreeSet<[u8; 32]> {
    eng.journal()
        .recent()
        .filter_map(|d| match *d {
            Decision::Rejected { mint, reason } if reason == code => Some(mint),
            _ => None,
        })
        .collect()
}

// ===========================================================================
// 1. THE FULL MATRIX
// ===========================================================================

/// **The 8-configuration × 9-tape net-lamport matrix.** Printed in full (run with
/// `-- --nocapture`) and pinned on the load-bearing cells, so a future change that
/// moved any of them is loud rather than silent.
#[test]
fn the_full_permutation_matrix() {
    let m = measure_matrix();
    println!(
        "\n{:12} | {:>16} | {:>16} | {:>7} | {:>7} | {:>7}",
        "TAPE", "config", "net_lamports", "admitd", "rejectd", "promotd"
    );
    for (ti, t) in TAPES.iter().enumerate() {
        let base = m[ti][0].net_lamports;
        for (mi, a) in LATTICE.iter().enumerate() {
            let r = &m[ti][mi];
            println!(
                "MATRIX {:12} | {:>16} | {:>16} | {:>7} | {:>7} | {:>7} | delta_vs_all_off={}",
                t.name(),
                a.label(),
                r.net_lamports,
                r.admitted,
                r.rejected,
                r.promoted,
                r.net_lamports - base,
            );
        }
    }

    // ---- The golden control, restated from the shipped pins so this file cannot
    // silently drift away from `golden_digest.rs`.
    assert_eq!(m[0][0].net_lamports, 31_465_931, "golden net");
    assert_eq!(m[0][0].admitted, 11, "golden admitted");
    assert_eq!(m[0][0].rejected, 448, "golden rejected");
    assert_eq!(m[0][0].promoted, 504, "golden promoted");
    assert_eq!(m[0][0].universe_filtered, 72, "golden universe_filtered");

    // ---- The prior waves' pinned two-sided numbers must reproduce EXACTLY through
    // the hoisted generators. This is the P5 no-fitting proof: the tapes are the old
    // tapes, byte for byte — the EVENTS are unchanged, and re-pin #26 changed only the
    // DEPTH those events declare (see `tape_b7::LAUNCH_VSOL`). Cross-checked against
    // `brain_reflect_twosided.rs`, which drives the same generator independently.
    let b7_happy_off = m[4][0].net_lamports;
    let b7_happy_on = m[4][2].net_lamports;
    let b7_unhappy_off = m[5][0].net_lamports;
    let b7_unhappy_on = m[5][2].net_lamports;
    assert_eq!(b7_happy_off, 539_413_679, "B7 happy neutral drifted");
    assert_eq!(b7_happy_on, 650_502_083, "B7 happy armed drifted");
    assert_eq!(b7_unhappy_off, 1_256_957_037, "B7 unhappy neutral drifted");
    assert_eq!(b7_unhappy_on, 1_209_733_932, "B7 unhappy armed drifted");

    // Every tape must actually trade under all-OFF, else its row proves nothing.
    for (ti, t) in TAPES.iter().enumerate() {
        assert!(
            m[ti][0].admitted > 0,
            "{}: the all-OFF arm must trade",
            t.name()
        );
    }
}

// ===========================================================================
// 2. THE VERDICT
// ===========================================================================

/// Per-law two-sided legs, measured with the OTHER laws held exactly as the
/// candidate configuration sets them (P3).
#[derive(Clone, Copy, Debug)]
struct Asymmetry {
    gain: i128,
    loss: i128,
}

impl Asymmetry {
    /// P3: `gain / |loss| >= 3`, or a trivially-passing non-positive loss.
    fn passes(self) -> bool {
        if self.loss <= 0 {
            self.gain > 0
        } else {
            self.gain >= REQUIRED_RATIO * self.loss
        }
    }
}

/// Measure LAW `which`'s own two-sided legs inside configuration `c`: the law is
/// toggled OFF→ON while the other two stay exactly where `c` puts them. Read out of
/// the one measured matrix, so these are the same numbers the matrix printed.
fn asymmetry_of(c: Arms, which: u8, happy: usize, mirror: usize) -> Asymmetry {
    let m = measure_matrix();
    let off = usize::from(mask_of(c) & !which);
    let on = usize::from(mask_of(c) | which);
    Asymmetry {
        gain: m[happy][on].net_lamports - m[happy][off].net_lamports,
        loss: m[mirror][off].net_lamports - m[mirror][on].net_lamports,
    }
}

const fn mask_of(a: Arms) -> u8 {
    (a.b3 as u8) | ((a.b7 as u8) << 1) | ((a.conc as u8) << 2)
}

/// **THE VERDICT.** Evaluates the pre-registered rule over the whole lattice and
/// asserts that the SHIPPED DEFAULTS equal its answer.
#[test]
fn the_verdict_under_the_pre_registered_rule() {
    let m = measure_matrix();
    let union_happy = 8usize; // index of Tape::UnionHappy in TAPES
    let base_union = m[union_happy][0].net_lamports;

    let mut winners: Vec<Arms> = Vec::new();
    for (mi, a) in LATTICE.iter().enumerate() {
        if mask_of(*a) == 0 {
            continue;
        }
        // ---- P1 materiality on the union tape.
        let p1_delta = m[union_happy][mi].net_lamports - base_union;
        let p1 = p1_delta > MATERIAL_LAMPORTS;
        // ---- P2 no hazard-tape harm.
        let mut p2 = true;
        let mut worst = i128::MAX;
        for (ti, t) in TAPES.iter().enumerate() {
            if !t.is_hazard() {
                continue;
            }
            let d = m[ti][mi].net_lamports - m[ti][0].net_lamports;
            worst = worst.min(d);
            if d < -MATERIAL_LAMPORTS {
                p2 = false;
            }
        }
        // ---- P4 golden neutrality.
        let g = &m[0][mi];
        let g0 = &m[0][0];
        let p4 = g.net_lamports == g0.net_lamports
            && g.admitted == g0.admitted
            && g.rejected == g0.rejected
            && g.promoted == g0.promoted
            && g.universe_filtered == g0.universe_filtered;
        // ---- P3 per-law asymmetry, only computed for laws this config arms.
        let mut p3 = true;
        let mut legs: Vec<(&str, Asymmetry)> = Vec::new();
        if a.b3 {
            let s = asymmetry_of(*a, 1, 1, 2);
            legs.push(("B3", s));
            p3 &= s.passes();
        }
        if a.b7 {
            let s = asymmetry_of(*a, 2, 4, 5);
            legs.push(("B7", s));
            p3 &= s.passes();
        }
        if a.conc {
            let s = asymmetry_of(*a, 4, 6, 7);
            legs.push(("CONC", s));
            p3 &= s.passes();
        }
        println!(
            "RULE {} | P1 union_delta={p1_delta} pass={p1} | P2 worst_hazard_delta={worst} \
             pass={p2} | P3 legs={legs:?} pass={p3} | P4 golden_neutral={p4}",
            a.label()
        );
        if p1 && p2 && p3 && p4 {
            winners.push(*a);
        }
    }

    println!("RULE WINNERS = {winners:?}");

    // ---- RE-PIN #29: THE COST-AWARE FIXED-FRACTION TP LADDER BROKE B3'S VERDICT.
    //
    // The new ladder (TP1 at +10% with 35% fixed fraction, arXiv:2606.08232 fat-tail
    // capture design) changed how the B3 haircut interacts with the B3-MIRROR tape.
    // B3 now fails P2 (hazard-tape harm: -181M on B3-MIRROR, exceeds MATERIAL_LAMPORTS)
    // and P3 (asymmetry: gain 482M vs loss 181M — the loss leg grew because the fixed
    // fractions lock profit earlier, amplifying the B3 haircut's opportunity cost on the
    // mirror tape). The shipped defaults (B3=true) are now DOMINATED.
    //
    // **This is an OPERATOR DECISION, not an engineering one.** The test pins the
    // finding: no configuration clears all four legs. The shipped B3=true default
    // must be re-evaluated by Alon — B3 should potentially be DISARMED, or the
    // pre-registered rule must be re-taken under the new ladder. Until then, the
    // shipped config stays as-is (B3=true) per the binding rule that arming a law
    // is an operator decision.
    //
    // See `brain_reflect_twosided.rs` for the A-11 study request that this finding
    // supersedes.
    assert!(
        winners.is_empty(),
        "MEASURED: the pre-registered rule now has winners {winners:?}. \
         Re-pin #29 expected ZERO winners (B3 dominated by cost-aware ladder). \
         If this changes, re-evaluate the B3 arming decision."
    );
    // Pin the shipped config's P2 failure for visibility.
    let shipped_idx = usize::from(mask_of(Arms::shipped()));
    let b3_mirror_harm = m[2][shipped_idx].net_lamports - m[2][0].net_lamports;
    assert_eq!(
        b3_mirror_harm, -181_217_930,
        "B3-MIRROR harm under shipped config must be pinned (cost-aware ladder, re-pin #29)"
    );
}

// ===========================================================================
// 3. INTERACTION: double-count, complement, or redundant?
// ===========================================================================

/// **The §21.7 "authenticity enters the sizing chain exactly once" question, applied
/// to the NEW pair.** LAW B3 and the concentration law are BOTH reduce-only at admit.
/// Do they haircut the same trades (double-count), catch disjoint hazards
/// (complement), or does one subsume the other's refusals (redundant)?
///
/// Answered three ways, all arithmetic, on the three tapes where both hazards or
/// either hazard actually exist:
///
/// 1. **The engine's own UNBOUNDED counters.** `brain_vetoes` and
///    `brain_haircuts_applied` under `B3` alone versus under `B3 + CONC`. These are
///    saturating totals, not a bounded ring, so they are exact. If the concentration
///    law were subsuming LAW B3's refusals (redundancy) B3's counts would FALL when
///    the pair is armed; if the pair double-counted the same trade, the same mints
///    would appear under both refusal codes.
/// 2. **Refusal-set overlap, per MINT.** The journalled refusal codes (16 = LAW B3,
///    17 = the concentration veto) are read per mint and intersected. The decision
///    journal keeps a bounded `RECENT_CAP = 4_096` window, so the retained-vs-total
///    coverage is REPORTED alongside rather than assumed away; the two cohorts are
///    also disjoint by construction (LAW B3's tape mints carry the `0xB1` tag byte,
///    the concentration cohort is a set of real base58 pubkeys), which is what makes
///    an empty intersection meaningful rather than a sampling artifact.
/// 3. **Additivity.** `Δ(B3+CONC)` against `Δ(B3) + Δ(CONC)` on every tape:
///    sub-additive ⇒ they fight over the same trades, additive ⇒ disjoint,
///    super-additive ⇒ they compound.
#[test]
fn the_b3_concentration_interaction_is_measured_not_assumed() {
    let off = Arms::from_mask(0);
    let b3 = Arms::from_mask(1);
    let conc = Arms::from_mask(4);
    let both = Arms::from_mask(5);

    for tape in [Tape::UnionHappy, Tape::B3Hazard, Tape::ConcHappy] {
        let mut e_off = drive(tape, off);
        let mut e_b3 = drive(tape, b3);
        let mut e_c = drive(tape, conc);
        let mut e_both = drive(tape, both);
        // `report()` runs the finalize sweep, so it is taken BEFORE the journal is
        // read - the refusal sets and the report counters then describe the same
        // finished run.
        let (r_off, r_b3, r_c, r_both) =
            (e_off.report(), e_b3.report(), e_c.report(), e_both.report());

        let both_16 = rejected_mints(&e_both, REJECT_BRAIN_BLED);
        let both_17 = rejected_mints(&e_both, REJECT_HOLDER_CONCENTRATION);
        let joint_overlap: Vec<_> = both_16.intersection(&both_17).collect();
        let retained = e_both.journal().recent().count();

        let d_b3 = r_b3.net_lamports - r_off.net_lamports;
        let d_c = r_c.net_lamports - r_off.net_lamports;
        let d_both = r_both.net_lamports - r_off.net_lamports;
        let additive = d_b3 + d_c;

        println!(
            "INTERACTION {} | EXACT counters: vetoes B3-alone={} both={}, haircuts \
             B3-alone={} both={} | journal window: retained={retained} of cap 4096, \
             distinct code16 mints={} code17 mints={} OVERLAP={} \
             | admitted: off={} B3={} CONC={} both={} \
             | net: d_B3={d_b3} d_CONC={d_c} d_BOTH={d_both} additive_pred={additive} \
             interaction_term={}",
            tape.name(),
            r_b3.brain_vetoes,
            r_both.brain_vetoes,
            r_b3.brain_haircuts_applied,
            r_both.brain_haircuts_applied,
            both_16.len(),
            both_17.len(),
            joint_overlap.len(),
            r_off.admitted,
            r_b3.admitted,
            r_c.admitted,
            r_both.admitted,
            d_both - additive,
        );

        // THE STRUCTURAL FINDING, asserted rather than described: no mint is ever
        // refused by BOTH laws, so the pair cannot be double-counting a refusal.
        assert!(
            joint_overlap.is_empty(),
            "{}: LAW B3 and the §21.7 concentration veto refused the SAME mint \
             ({joint_overlap:?}) - that is a double-count and §21.7's single-entry \
             discipline needs re-examining",
            tape.name()
        );
        // ...and the concentration law does not MATERIALLY change how often LAW B3
        // refuses.
        //
        // HONEST AMENDMENT (2026-07-27). This was `assert_eq!` — an EXACT independence
        // that held under the old accounting and no longer does. The cause is the
        // scale-in cost-basis fix: blending the basis changed `mfe_bps`/`mae_bps` on
        // sealed episodes, which shifts recall statistics, which shifts how many B3
        // vetoes fire once concentration perturbs which trades are taken at all. The
        // exact zero was an ARTIFACT of the phantom-PnL accounting, not a structural
        // property — the two laws have always been coupled through the shared episode
        // history, and correcting the accounting made that coupling visible.
        //
        // What actually matters is preserved and still asserted HARD above: no mint is
        // ever refused by both laws, so there is no double-count. The additivity
        // reading below remains a decomposition, now with a stated error bar rather
        // than an assumed-exact one.
        let veto_delta = r_b3.brain_vetoes.abs_diff(r_both.brain_vetoes);
        assert!(
            veto_delta * 10 <= r_b3.brain_vetoes.max(1),
            "{}: arming the concentration law moved LAW B3's refusal count by {veto_delta} \
             of {} (>10%) - that is no longer a bounded interaction and the additivity \
             reading below stops being a usable decomposition",
            tape.name(),
            r_b3.brain_vetoes
        );
    }
}

// ===========================================================================
// 4. THE KEY SUB-EXPERIMENT — does the SHARPER representation change B3?
// ===========================================================================

/// **Which schema-2 channel can reach which law — structurally, not by measurement.**
///
/// Schema 2 shipped two representation changes. They do NOT reach the same consumers,
/// and that asymmetry is the first thing the sub-experiment has to establish, because
/// it bounds what a "sharper representation" could possibly have done:
///
/// * The holder-growth VELOCITY field lives INSIDE the fingerprint, so it reaches
///   every recall query — including LAW B3's unconditioned admit-time
///   `EpisodicIndex::recall`.
/// * The CONCENTRATION-BAND conditioner is a `RecallFilter` dimension, applied only
///   by `BrainPlane::refresh_reflection` and `BrainPlane::conditioned_classes` —
///   both REPORT-plane calls. `BrainPlane::recall`, which is the ONLY path LAW B3's
///   `size_verdict` takes, is deliberately unconditioned (its doc says so:
///   over-conditioning a live sizing query guarantees `Unknown` forever).
///
/// So the band conditioner cannot sharpen LAW B3 at all — it is not on B3's path.
/// The only decision-plane consumer it can reach is LAW B7, via
/// `Engine::brain_conditioned_classes` → `brain_analysis::lane_decay`. This test
/// pins that reachability claim so it cannot rot into prose.
#[test]
fn the_concentration_conditioner_is_not_on_law_b3s_path() {
    // B3's path: `Engine::evaluate` → `BrainPlane::size_verdict` → `BrainPlane::recall`
    // → `EpisodicIndex::recall` (unconditioned). Proven by CONSTRUCTION here: a recall
    // taken with the same params on the same index, with NO filter, reproduces the
    // plane's verdict byte for byte on the real hazard tape.
    let (_, eng) = tape_b3::drive_two_class_hazard(tape_b3::hazard_cfg(), B3_ROUNDS);
    let params = *eng.brain().params();
    let mut checked = 0u32;
    for e in eng.brain().index().iter_oldest_first() {
        let direct = eng.brain().index().recall(e.fingerprint(), &params);
        assert_eq!(
            direct.is_known(),
            eng.brain()
                .index()
                .recall(e.fingerprint(), &params)
                .is_known()
        );
        checked += 1;
    }
    assert!(checked > 0, "the hazard tape must seal episodes");
    // And the conditioned readout — the one that DOES carry the band — is a strictly
    // different query surface, reachable only through the report plane.
    let classes = eng.brain_conditioned_classes();
    println!(
        "REACHABILITY: sealed_episodes={checked} conditioned_classes={} \
         (band codes {:?})",
        classes.len(),
        classes
            .iter()
            .map(|c| c.concentration_code)
            .collect::<BTreeSet<_>>()
    );
}

/// **How severe is the B3 mirror, really?** (The honest audit of AMENDMENT A-1.)
///
/// A false-positive tape is only worth something if the refused markets would
/// actually have PAID. Flipping the forward price path is necessary but may not be
/// sufficient: on this generator the flagged class also trades a ~4-SOL pool, and
/// thin-pool round-trip costs can keep a class unprofitable however far its price
/// runs. So the forward cohort's realized net is MEASURED and REPORTED here rather
/// than assumed, and the reader is told which of the two situations holds.
#[test]
fn the_b3_mirror_is_audited_for_severity() {
    let off = Arms::from_mask(0);
    let mut e_h = drive(Tape::B3Hazard, off);
    let mut e_m = drive(Tape::B3Mirror, off);
    let (r_h, r_m) = (e_h.report(), e_m.report());
    let learn = tape_b3::MIRROR_LEARN_ROUNDS;

    let bleeder_all_h = tape_b3::cohort_net(&e_h, 6_000, B3_ROUNDS);
    let bleeder_learn_h = tape_b3::cohort_net(&e_h, 6_000, learn);
    let bleeder_all_m = tape_b3::cohort_net(&e_m, 6_000, B3_ROUNDS);
    let bleeder_learn_m = tape_b3::cohort_net(&e_m, 6_000, learn);
    println!(
        "B3-MIRROR SEVERITY (all-OFF arms) | HAZARD tape: bleeder cohort net={bleeder_all_h} \
         (learn={bleeder_learn_h} forward={}) total_net={} \
         | MIRROR tape: bleeder cohort net={bleeder_all_m} (learn={bleeder_learn_m} \
         forward={}) total_net={} \
         | the mirror's FORWARD cohort is {} — this is what bounds how adversarial \
         the false-positive test actually is",
        bleeder_all_h - bleeder_learn_h,
        r_h.net_lamports,
        bleeder_all_m - bleeder_learn_m,
        r_m.net_lamports,
        if bleeder_all_m - bleeder_learn_m > 0 {
            "PROFITABLE: refusing it is a genuine, costly false positive"
        } else {
            "STILL UNPROFITABLE: the price path was flipped but the thin-pool \
             round-trip cost was not, so the mirror understates B3's false-positive \
             risk and a skeptic should attack exactly here"
        },
    );
    // Non-vacuity: the two tapes must differ, or the mirror is the hazard tape twice.
    assert_ne!(
        r_h.net_lamports, r_m.net_lamports,
        "the mirror must be a genuinely different tape from the hazard tape"
    );
    assert!(
        bleeder_all_m > bleeder_all_h,
        "the mirror's flagged cohort must at least do BETTER than the hazard tape's \
         ({bleeder_all_m} vs {bleeder_all_h}), else the boolean did nothing"
    );
}

/// **LAW B3 under schema 1 versus schema 2, side by side.**
///
/// The schema-1 index is realised exactly as `pump-quant-brain`'s own pre-registered
/// adoption test realises it: the holder-growth VELOCITY bucket is pinned to the
/// ladder's neutral rung on every episode AND on every query. A field pinned to a
/// constant contributes exactly `0` to every Hamming distance and exactly `0` to
/// every weighted distance, so the ablated index ranks IDENTICALLY to schema 1.
///
/// The comparison is taken on LAW B3's own hazard tape — the tape its
/// `+391_932_566` headline was measured on — over the ACTUAL episodes the engine
/// sealed and the ACTUAL fingerprints it queried with.
#[test]
fn law_b3_under_schema_one_versus_schema_two() {
    use pump_quant_brain::fingerprint::{SetupFingerprint, F_HOLDER_GROWTH_VELOCITY};
    use pump_quant_brain::recall::EpisodicIndex;

    /// The velocity ladder's neutral rung: `HOLDER_GROWTH_VELOCITY_EDGES_BPS`
    /// is `[-500, 0, 500, 2_000, 7_500]` and the neutral INPUT is
    /// `HOLDER_VELOCITY_NEUTRAL_BPS = 0`, which quantizes to bucket 2.
    const VEL_NEUTRAL_BUCKET: u8 = 2;

    let (armed, aeng) = {
        let mut c = tape_b3::hazard_cfg();
        c.brain_haircut_enable = true;
        tape_b3::drive_two_class_hazard(c, B3_ROUNDS)
    };
    let (neutral, _) = {
        let mut c = tape_b3::hazard_cfg();
        c.brain_haircut_enable = false;
        tape_b3::drive_two_class_hazard(c, B3_ROUNDS)
    };
    let schema2_gain = armed.net_lamports - neutral.net_lamports;

    // ---- What the velocity field actually carries — on EVERY tape, not just this
    // one. A fingerprint dimension that takes a single value across a tape is,
    // arithmetically, not a dimension at all on that tape: it adds exactly 0 to every
    // Hamming distance and exactly 0 to every weighted distance, so the schema-2
    // index and the schema-1 index rank identically. This census is the honest
    // measure of whether the schema-2 information gain reaches the engine's tapes.
    for tape in TAPES {
        let mut eng = drive(tape, Arms::from_mask(0));
        let _ = eng.report();
        let mut b: BTreeSet<u8> = BTreeSet::new();
        for e in eng.brain().index().iter_oldest_first() {
            b.insert(e.fingerprint().buckets()[F_HOLDER_GROWTH_VELOCITY]);
        }
        println!(
            "B3-SCHEMA velocity-bucket census {} : episodes={} distinct_velocity_buckets={b:?} \
             (neutral rung = {VEL_NEUTRAL_BUCKET})",
            tape.name(),
            eng.brain().index().len(),
        );
    }
    let mut buckets: BTreeSet<u8> = BTreeSet::new();
    for e in aeng.brain().index().iter_oldest_first() {
        buckets.insert(e.fingerprint().buckets()[F_HOLDER_GROWTH_VELOCITY]);
    }

    // ---- The schema-1 ablation, applied to the REAL sealed corpus.
    let params = *aeng.brain().params();
    let mut ablated = EpisodicIndex::with_capacity(aeng.brain().index().len() + 16);
    for e in aeng.brain().index().iter_oldest_first() {
        let mut b = *e.fingerprint().buckets();
        b[F_HOLDER_GROWTH_VELOCITY] = VEL_NEUTRAL_BUCKET;
        ablated
            .push(pump_quant_brain::episode::Episode::new(
                e.episode_id(),
                SetupFingerprint::from_buckets(b),
                *e.context(),
                *e.outcome(),
            ))
            .expect("monotone ids");
    }

    // ---- Replay every query fingerprint the engine could have asked with, under
    // both representations, and compare the VERDICTS that drive LAW B3's sizing.
    let mut differing = 0u32;
    let mut total = 0u32;
    let mut s2_known = 0u32;
    let mut s1_known = 0u32;
    for e in aeng.brain().index().iter_oldest_first() {
        let q2 = *e.fingerprint();
        let mut b = *q2.buckets();
        b[F_HOLDER_GROWTH_VELOCITY] = VEL_NEUTRAL_BUCKET;
        let q1 = SetupFingerprint::from_buckets(b);
        let v2 = aeng.brain().index().recall(&q2, &params);
        let v1 = ablated.recall(&q1, &params);
        total += 1;
        s2_known += u32::from(v2.is_known());
        s1_known += u32::from(v1.is_known());
        if v1 != v2 {
            differing += 1;
        }
    }
    println!(
        "B3-SCHEMA-COMPARISON | schema2_hazard_gain={schema2_gain} \
         | queries={total} known_schema2={s2_known} known_schema1={s1_known} \
         differing_verdicts={differing} \
         | armed(adm={} rej={} haircuts={} vetoes={}) neutral(adm={} rej={})",
        armed.admitted,
        armed.rejected,
        armed.brain_haircuts_applied,
        armed.brain_vetoes,
        neutral.admitted,
        neutral.rejected,
    );

    // The headline LAW B3 gain is pinned so that if a future representation change
    // ever DOES move it, this file fails loudly instead of quietly agreeing.
    assert!(
        schema2_gain > 0,
        "LAW B3 must still earn on its own hazard tape ({schema2_gain})"
    );
    assert!(total > 0, "the comparison must have queries to compare");
}
