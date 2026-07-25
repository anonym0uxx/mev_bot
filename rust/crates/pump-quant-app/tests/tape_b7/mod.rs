//! The **LAW B7 two-sided tape** (happy / unhappy mirror), hoisted verbatim out of
//! `tests/brain_reflect_twosided.rs` so more than one test binary can drive it.
//!
//! Nothing here was rewritten for the law-permutation sweep. `brain_reflect_twosided.rs`
//! still owns the pre-registered LAW B7 rule and its pins; this module owns only the
//! event script.
#![allow(dead_code)]
// Plain modulo, not `is_multiple_of`, to honour the workspace MSRV 1.85 (the
// helper stabilised in 1.87) - the same choice `engine.rs` documents.
#![allow(clippy::manual_is_multiple_of)]

use pump_quant_app::brain_analysis::lane_decay;
use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, Report, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_domain::ids::Mint;
use pump_quant_watchlist::candidate::Lane;

// ===========================================================================
// The tape.
// ===========================================================================

/// Tag-namespaced mint (the 0xB7 marker keeps this tape's mints disjoint from
/// every other test tape's).
pub fn mint(tag: u64) -> Mint {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&tag.to_le_bytes());
    b[8] = 0xB7;
    Mint::from_bytes(b)
}

/// Rounds of the learning phase — the byte-identical prefix both regimes share, in
/// which the SOCIAL lane trades, bleeds on most of its setups, and is carried to a
/// POSITIVE aggregate by its runners. That aggregate is what the incumbent
/// realized-net reflection reads, and it is why the incumbent RAISES the lane's
/// weight while the brain's sample-weighted conditioned median flags it.
pub const LEARN_ROUNDS: u64 = 10;
/// Markets in the learning cohort. Every fourth is a runner.
pub const LEARN_MARKETS: u64 = 28;
/// Rounds of the forward phase — where the promotion-slot contention between the
/// flagged lane and the healthy lane is actually paid for.
pub const FORWARD_ROUNDS: u64 = 30;
/// Ticks emitted per round. The reflection cadence is 50 ticks, so the full
/// 480-tick tape runs nine reflection passes.
pub const TICKS_PER_ROUND: u64 = 12;
/// How many rounds a market's CORROBORATION evidence keeps being refreshed. After
/// this the call / whale sighting goes stale (§29.6/§34.3) and the mint leaves the
/// candidate set, so the contended set TURNS OVER instead of accumulating.
pub const EVIDENCE_ROUNDS: u64 = 4;
/// Price rungs a market walks, one per round; the last rung is then held for the
/// rest of the tape so a held position can always mark and exit.
pub const LIFE_ROUNDS: u64 = 6;

/// The three outcome ladders share this byte-identical PREFIX (rungs 0-2) and
/// diverge only from rung 3. That is the whole point of the tape: at the moment a
/// candidate is promoted and gated, a decayed-lane market and a healthy-lane market
/// are INDISTINGUISHABLE — same price path, same order flow, same depth, same age.
/// The §18 economic gate and the §23 expected-net arbitration therefore cannot
/// separate them, and the only thing on the engine that can is the episodic record
/// of what setups LIKE THIS did last time.
pub const COMMON_PREFIX_BP: [i64; 3] = [10_000, 10_450, 11_000];
/// A market that pays: a move well clear of the ~700 bps realistic round-trip, then
/// a partial give-back to a plateau it holds.
pub const GOOD_TAIL_BP: [i64; 3] = [13_000, 14_200, 13_800];
/// A market that does not: it rolls over from the same prefix and stays under the
/// round-trip cost.
pub const BAD_TAIL_BP: [i64; 3] = [9_800, 9_200, 9_000];
/// The runners that keep the social lane's realized AGGREGATE positive while most
/// of its setups bleed — the "a few runners carrying twenty bleeders" shape a lane's
/// net-SOL SUM cannot see through and a sample-weighted median can.
pub const RUNNER_TAIL_BP: [i64; 3] = [34_000, 46_000, 43_000];
/// A milder bleeder, used only by the shape-robustness sweep.
pub const MILD_BAD_TAIL_BP: [i64; 3] = [10_500, 9_900, 9_700];
/// A smaller runner, used only by the shape-robustness sweep.
pub const SMALL_RUNNER_TAIL_BP: [i64; 3] = [18_000, 22_000, 21_000];
/// A larger runner, used only by the shape-robustness sweep.
pub const HUGE_RUNNER_TAIL_BP: [i64; 3] = [58_000, 82_000, 76_000];

/// Social-lane call qualities. The social lane's discovery score is the summed call
/// quality, so `q × 8_000 / 10_000` is its seeded rank; these interleave with the
/// wallet ladder below.
pub const SOCIAL_QUALITY: [u64; 7] = [1_200, 1_300, 1_400, 1_500, 1_600, 1_700, 1_800];
/// Wallet-lane followable sizes. The wallet score is `decade(size) × 100`, so these
/// five magnitudes give scores 900/1_000/1_100/1_200/1_300 — ranks that slot BETWEEN
/// the social ranks above, which is the precondition for a bounded weight step to be
/// able to reorder anything at all.
pub const WALLET_SIZE: [u64; 5] = [
    300_000_000,
    3_000_000_000,
    30_000_000_000,
    300_000_000_000,
    3_000_000_000_000,
];

/// A market's outcome shape.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
    Good,
    Bad,
    Runner,
}

/// The tape's knobs: which regime, and (for the robustness sweep only) which runner
/// and bleeder tails.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Tape {
    /// `false` — HAPPY PATH: the social lane's forward setups are genuinely bad and
    /// the wallet lane's are good, so the decay flag was RIGHT.
    ///
    /// `true` — UNHAPPY PATH: the two forward cohorts' shapes are SWAPPED, so the
    /// flag — built on the IDENTICAL learning-phase losses — is a FALSE POSITIVE:
    /// the lane's forward setups perform fine and the armed arm suppresses them.
    pub forward_social_good: bool,
    pub runner_tail: [i64; 3],
    pub bad_tail: [i64; 3],
}

impl Tape {
    pub const fn happy() -> Self {
        Self {
            forward_social_good: false,
            runner_tail: RUNNER_TAIL_BP,
            bad_tail: BAD_TAIL_BP,
        }
    }

    pub const fn unhappy() -> Self {
        Self {
            forward_social_good: true,
            runner_tail: RUNNER_TAIL_BP,
            bad_tail: BAD_TAIL_BP,
        }
    }

    pub const fn with_tails(self, runner_tail: [i64; 3], bad_tail: [i64; 3]) -> Self {
        Self {
            forward_social_good: self.forward_social_good,
            runner_tail,
            bad_tail,
        }
    }

    pub fn path(&self, shape: Shape) -> [i64; 6] {
        let tail = match shape {
            Shape::Good => GOOD_TAIL_BP,
            Shape::Bad => self.bad_tail,
            Shape::Runner => self.runner_tail,
        };
        [
            COMMON_PREFIX_BP[0],
            COMMON_PREFIX_BP[1],
            COMMON_PREFIX_BP[2],
            tail[0],
            tail[1],
            tail[2],
        ]
    }
}

/// One market on the tape.
#[derive(Clone, Copy)]
pub struct Market {
    pub tag: u64,
    pub birth: u64,
    pub shape: Shape,
    /// `true` = discovered by the SOCIAL lane (`Lane::CreationSniper` — the lane the
    /// brain flags); `false` = discovered by the WALLET / smart-money lane
    /// (`Lane::GraduationTransition` — the healthy competitor for the same slots).
    pub social: bool,
    /// The corroboration strength handed to the discovering lane.
    pub strength: u64,
}

/// Build the full market list for a regime.
pub fn markets(tape: Tape) -> Vec<Market> {
    let mut out = Vec::with_capacity((LEARN_MARKETS + 2 * FORWARD_ROUNDS) as usize);
    // ---- Learning phase: the social lane trades, most of its setups bleed, and a
    // quarter of them run hard enough to leave the lane's SUM positive. Identical in
    // both regimes, so the decay flag both arms inherit is identical too.
    for k in 0..LEARN_MARKETS {
        out.push(Market {
            tag: k,
            birth: k % LEARN_ROUNDS,
            shape: if k % 4 == 0 {
                Shape::Runner
            } else {
                Shape::Bad
            },
            social: true,
            strength: SOCIAL_QUALITY[(k % 7) as usize],
        });
    }
    // ---- Forward phase: the two lanes contend for the same promotion slots.
    let (social_shape, wallet_shape) = if tape.forward_social_good {
        (Shape::Good, Shape::Bad)
    } else {
        (Shape::Bad, Shape::Good)
    };
    for k in 0..FORWARD_ROUNDS {
        out.push(Market {
            tag: 100 + k,
            birth: LEARN_ROUNDS + k,
            shape: social_shape,
            social: true,
            strength: SOCIAL_QUALITY[(k % 7) as usize],
        });
        out.push(Market {
            tag: 200 + k,
            birth: LEARN_ROUNDS + k,
            shape: wallet_shape,
            social: false,
            strength: WALLET_SIZE[(k % 5) as usize],
        });
    }
    out
}

/// How often, and on which lanes, the LAW B7 decay flag ACTUALLY fired while the
/// tape was running. Sampled once per round, because the flag set is a function of
/// the live episodic index and the end-of-run snapshot is NOT representative — a
/// lane can be flagged for most of a run and unflagged by the last round.
#[derive(Default, Clone, Copy, Debug)]
pub struct FlagTrace {
    pub samples: u32,
    pub social_rounds: u32,
    pub wallet_rounds: u32,
    pub max_flagged: u32,
}

/// Drive the tape and return the finished engine (so the post-run brain state can be
/// inspected as well as the report). With `trace` supplied, the lane-decay flag set
/// is sampled once per round — a pure read of already-realized state.
/// The LAW B7 tape's CONFIG overrides, split out of [`drive`] verbatim so the union
/// tape of `law_permutation_sweep.rs` can compose them with the other generators'.
pub fn b7_cfg(cfg: Config) -> Config {
    let mut cfg = cfg;
    // The same cost-realism overrides the golden tape uses, so the gate is the real
    // §18 economic gate rather than a permissive one.
    cfg.gate_expected_move_bps = 1_800;
    cfg.gate_protocol_bps = 450;
    cfg.gate_margin_bps = 150;
    cfg.gate_base_fixed_lamports = 200_000;
    cfg.gate_impact_den = 250_000;
    // Corroboration evidence goes stale in ~2.5 rounds rather than ~8, so the
    // contended candidate set TURNS OVER (fresh launches displace old ones) instead
    // of growing without bound. Numeric snapshots stay well inside it — every live
    // market prints every round — so the gate still reads fresh microstructure.
    cfg.lane_evidence_ttl_ticks = 30;
    cfg
}

pub fn drive(cfg: Config, tape: Tape, trace: Option<&mut FlagTrace>) -> Engine {
    let cfg = b7_cfg(cfg);
    let min_sample = cfg.brain_decay_min_sample;
    let mut eng = Engine::new(cfg, RunMode::Replay);
    apply_tape(&mut eng, tape, min_sample, trace);
    eng
}

/// The LAW B7 tape's EVENT SCRIPT, applied to an engine the caller owns. Byte-for-
/// byte the loop [`drive`] has always run.
pub fn apply_tape(
    eng: &mut Engine,
    tape: Tape,
    min_sample: u32,
    mut trace: Option<&mut FlagTrace>,
) {
    let eng = &mut *eng;
    let mkts = markets(tape);
    for round in 0..(LEARN_ROUNDS + FORWARD_ROUNDS) {
        for m in &mkts {
            if round < m.birth {
                continue;
            }
            let age = round - m.birth;
            let rung = age.min(LIFE_ROUNDS - 1) as usize;
            let path = tape.path(m.shape);
            let base = 1_000_000_000i128 * i128::from(path[rung]) / 10_000;
            // Six near-balanced prints: |OFI| stays well under `numeric_ofi_min_bp`
            // (1_000 bp) so the SELF-AUTHORIZING numeric lane never emits for these
            // mints and each candidate's provenance stays with the corroboration lane
            // that discovered it — the same discipline the golden tape's paid-alpha
            // cohort uses.
            let rising = rung + 1 < path.len() && path[rung + 1] > path[rung];
            for i in 0..6u64 {
                let sgn: i64 = if i % 2 == 0 { 1 } else { -1 };
                let mag: i64 = if (sgn > 0) == rising {
                    520_000
                } else {
                    500_000
                };
                eng.tick(AppEvent::MarketTrade {
                    mint: mint(m.tag),
                    price_fp: base + (i as i128) * 400_000 + (m.tag as i128) * 3_000,
                    quote_lamports: 700_000 + (m.tag % 11) * 1_000,
                    liquidity_lamports: 200_000_000 + (m.tag % 40) * 1_000_000,
                    signed_base: sgn * mag,
                    buyer_entity: 1_000 + (m.tag * 7 + i) % 23,
                    age_slots: 10 + (m.tag as u32 % 20),
                });
            }
            if age == 0 {
                eng.tick(AppEvent::OnchainConfirm {
                    mint: mint(m.tag),
                    sellable_depth_lamports: 400_000_000,
                });
            }
            if age < EVIDENCE_ROUNDS {
                if m.social {
                    // The social lane ACCUMULATES call quality, so the full weight is
                    // paid once and later rounds only refresh the sighting's
                    // freshness — otherwise the lane's score would ramp with age and
                    // the two lanes' ranks would stop being comparable.
                    eng.tick(AppEvent::SocialCall {
                        mint: mint(m.tag),
                        source_quality_bp: if age == 0 {
                            u32::try_from(m.strength).unwrap()
                        } else {
                            1
                        },
                    });
                } else {
                    // Same discipline on the wallet side: the full size once, then a
                    // token refresh that cannot move the score's decade.
                    eng.tick(AppEvent::WalletAction {
                        mint: mint(m.tag),
                        followable: true,
                        size_lamports: if age == 0 { m.strength } else { 1_000 },
                    });
                }
            }
        }
        for _ in 0..TICKS_PER_ROUND {
            eng.tick(AppEvent::Tick);
        }
        if let Some(t) = trace.as_deref_mut() {
            let d = lane_decay(&eng.brain_conditioned_classes(), min_sample);
            t.samples += 1;
            t.max_flagged = t.max_flagged.max(d.count());
            if d.is_decayed(Lane::CreationSniper) {
                t.social_rounds += 1;
            }
            if d.is_decayed(Lane::GraduationTransition) {
                t.wallet_rounds += 1;
            }
        }
    }
}

/// One measured arm: the report plus the lane-decay flag set the run finished with.
pub struct Arm {
    pub report: Report,
    pub trace: FlagTrace,
}

pub fn run(base: Config, armed: bool, step_bp: u32, tape: Tape) -> Arm {
    run_traced(base, armed, step_bp, tape, false)
}

pub fn run_traced(base: Config, armed: bool, step_bp: u32, tape: Tape, trace_flags: bool) -> Arm {
    let mut cfg = base;
    cfg.brain_reflect_enable = armed;
    cfg.brain_reflect_step_bp = step_bp;
    let mut trace = FlagTrace::default();
    let mut eng = drive(cfg, tape, if trace_flags { Some(&mut trace) } else { None });
    let report = eng.report();
    Arm { report, trace }
}
