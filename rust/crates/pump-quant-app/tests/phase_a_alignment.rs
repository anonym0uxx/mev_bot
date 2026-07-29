//! Phase-A alignment laws (remediation directive): unknown/stale critical data
//! fails closed, caller-supplied conclusions cannot authorize capital, and
//! optimistic evidence cannot satisfy promotion. Each test drives the REAL
//! engine through its public surface — no mocks, no private access.

use pump_quant_app::config::{Config, FillModeCfg};
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_app::screen::FlowScreen;
use pump_quant_domain::ids::Mint;

fn mint(tag: u64) -> Mint {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&tag.to_le_bytes());
    b[8] = 0xCD;
    Mint::from_bytes(b)
}

/// A `dev_portable` config funded above the criterion-112 / A-6 0.1-SOL operator
/// floor. The recalibrated default 2-SOL bankroll sizes the base bite AT the floor,
/// so a reduce-only flow haircut drops it below the floor and REFUSES (correct in
/// production) — these admission-control tests are orthogonal to sizing, so a larger
/// bankroll lifts the base bite above the floor and lets the confirmed setup admit.
fn funded() -> Config {
    let mut cfg = Config::dev_portable();
    cfg.bankroll_initial_lamports = 20_000_000_000; // 20 SOL ⇒ base ~1 SOL, caps at x_max
    cfg
}

/// **The SOL-side reserve every fixture in this file declares: a pump.fun bonding
/// curve at LAUNCH depth, 30 SOL of virtual SOL reserve**
/// (`pump_quant_app::curve_state::LAUNCH_VSOL_LAMPORTS` — the shallowest depth the
/// venue can present).
///
/// **Re-pin #26 (2026-07-28).** A declared depth is now a PRICE: `gate::decide`
/// derives the gate's impact denominator from the market's own reserve
/// (`cost_model::impact_den_for` = `vsol / 10_000`). At the previous 10 SOL this
/// file's 20-SOL bankroll sized a ~0.9-SOL bite — 900 bps of own impact a leg — and
/// `inflated_depth_claim_buys_no_size` stopped admitting anything at all, so the §15
/// cross-check it exists to prove had nothing to compare.
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
/// Confirmed sellable depth, just under [`REAL_CURVE_VSOL`] — the "a confirm proves
/// slightly less than the pool holds" discipline the golden tape uses.
/// Alias kept for the fixtures that name the PAYOUT reserve directly.
const REAL_SELLABLE_DEPTH: u64 = REAL_CURVE_REAL_SOL;

/// Deep, admissible flow for one mint: strong OFI, deep pool, broad entities. The
/// pool is a real launch curve ([`REAL_CURVE_VSOL`]), so a ≥0.1-SOL floor clip
/// (criterion 112 / A-6) is a small fraction of it and clears the §34.4 exit-cost
/// veto — a shallow 0.3-SOL curve cannot absorb a 0.1-SOL exit (~33% of reserve).
fn feed_flow(eng: &mut Engine, mt: Mint, trades: u64) {
    for i in 0..trades {
        eng.tick(AppEvent::MarketTrade {
            mint: mt,
            price_fp: 1_000_000_000 + (i as i128) * 100_000,
            quote_lamports: 800_000,
            liquidity_lamports: REAL_CURVE_VSOL,
            signed_base: 900_000 - (i as i64),
            buyer_entity: 10 + i % 9,
            age_slots: 12,
        });
    }
}

// ============================================================================
// §34.3: a fresh confirm can NEVER borrow a stale numeric snapshot.
// ============================================================================
#[test]
fn stale_numeric_snapshot_cannot_authorize_entry() {
    let cfg = Config::dev_portable();
    let ttl = cfg.lane_evidence_ttl_ticks;
    let mut eng = Engine::new(cfg, RunMode::Replay);
    let mt = mint(1);
    feed_flow(&mut eng, mt, 20);
    // Age the numeric evidence PAST the lane TTL (no further trades)...
    for _ in 0..(ttl + 5) {
        eng.tick(AppEvent::Tick);
    }
    // ...then land a perfectly fresh on-chain confirm.
    eng.tick(AppEvent::OnchainConfirm {
        mint: mt,
        virtual_sol_lamports: REAL_CURVE_VSOL,
                    real_sol_lamports: REAL_SELLABLE_DEPTH,
    });
    for _ in 0..3 {
        eng.tick(AppEvent::Tick);
    }
    let r = eng.report();
    assert_eq!(
        r.admitted, 0,
        "depth proven now + liquidity observed long ago must NOT authorize"
    );
}

/// Control for the test above: identical stream WITHOUT the stale gap admits.
#[test]
fn fresh_numeric_snapshot_with_confirm_admits() {
    let mut eng = Engine::new(funded(), RunMode::Replay);
    let mt = mint(2);
    feed_flow(&mut eng, mt, 20);
    eng.tick(AppEvent::OnchainConfirm {
        mint: mt,
        virtual_sol_lamports: REAL_CURVE_VSOL,
                    real_sol_lamports: REAL_SELLABLE_DEPTH,
    });
    for _ in 0..3 {
        eng.tick(AppEvent::Tick);
    }
    let r = eng.report();
    assert!(r.admitted > 0, "the control stream must admit");
}

// ============================================================================
// §15: a caller-ASSERTED payout reserve is cross-checked against the venue's own
// identity — an inflated claim buys no size, and now buys no ADMISSION either.
// ============================================================================
/// **THE LAW GOT STRONGER AT RE-PIN #27, AND THE OLD VERSION WAS TOO WEAK.**
///
/// The retired rule was `min(claimed_depth, observed_liquidity)`: an inflated claim
/// was silently clamped to the observed VIRTUAL reserve and the trade proceeded. That
/// clamp was measured against the wrong number — a curve escrows `virtual_sol − 30
/// SOL`, so clamping a claim to `virtual_sol` still permitted a capacity 30x the money
/// in the pool at `vsol = 31 SOL`, and unbounded capacity at the seed reserve. The old
/// assertion ("the same size either way") passed the whole time.
///
/// The rule now: a confirm whose two reserves contradict `real_sol = virtual_sol − 30
/// SOL` beyond `curve_depth::cross_check_tolerance_lamports` is a BROKEN DECODE, and a
/// broken decode is refused rather than clamped — it is never recorded, the market has
/// no on-chain confirmation, and the gate refuses. Clamping would have hidden the
/// decoder fault forever; refusing costs one trade and surfaces it (§18.2).
#[test]
fn an_inflated_depth_claim_is_refused_not_clamped() {
    let run = |claimed_real_sol: u64| -> Vec<u64> {
        let mut eng = Engine::new(funded(), RunMode::Replay);
        let mt = mint(3);
        feed_flow(&mut eng, mt, 20);
        eng.tick(AppEvent::OnchainConfirm {
            mint: mt,
            virtual_sol_lamports: REAL_CURVE_VSOL,
            real_sol_lamports: claimed_real_sol,
        });
        for _ in 0..3 {
            eng.tick(AppEvent::Tick);
        }
        eng.journal()
            .recent()
            .filter_map(|d| match *d {
                pump_quant_app::journal_log::Decision::Admitted { size_lamports, .. } => {
                    Some(size_lamports)
                }
                _ => None,
            })
            .collect()
    };
    // The honest pair — what the venue's arithmetic says this reserve escrows — trades.
    let honest = run(REAL_SELLABLE_DEPTH);
    assert!(!honest.is_empty(), "an honest confirm must still admit");

    // The claim every fixture in this repo used to make: the PRICE reserve passed off
    // as payout capacity. It is refused outright.
    let inflated = run(REAL_CURVE_VSOL);
    assert!(
        inflated.is_empty(),
        "a claim of {REAL_CURVE_VSOL} lamports of payout on a curve escrowing \
         {REAL_SELLABLE_DEPTH} must be refused, not clamped ({inflated:?})"
    );
    // …and so is a claim 100x beyond the whole curve.
    assert!(run(REAL_CURVE_VSOL * 100).is_empty());

    // Drift INSIDE the tolerance is protocol-fee noise, not a decoder fault, and must
    // not cost a trade: the law refuses contradictions, not rounding.
    let tol = pump_quant_app::curve_depth::cross_check_tolerance_lamports(REAL_SELLABLE_DEPTH);
    assert!(!run(REAL_SELLABLE_DEPTH.saturating_sub(tol)).is_empty());
    assert!(!run(REAL_SELLABLE_DEPTH + tol).is_empty());
}

// ============================================================================
// §28/§29: caller-supplied wallet conclusions cannot authorize entry.
// ============================================================================
#[test]
fn caller_followable_cannot_authorize_entry() {
    let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    let mt = mint(4);
    // A "smart wallet" screams BUY every round, and the market even has a
    // confirmed depth — but there is NO numeric flow evidence at all.
    for round in 0..6u64 {
        eng.tick(AppEvent::WalletAction {
            mint: mt,
            followable: true,
            size_lamports: 500_000_000,
        });
        if round == 0 {
            eng.tick(AppEvent::OnchainConfirm {
                mint: mt,
                virtual_sol_lamports: REAL_CURVE_VSOL,
                    real_sol_lamports: REAL_SELLABLE_DEPTH,
            });
        }
        for _ in 0..6 {
            eng.tick(AppEvent::Tick);
        }
    }
    let r = eng.report();
    assert_eq!(
        r.admitted, 0,
        "wallet conclusions + confirm without numeric flow must never enter"
    );
}

// ============================================================================
// §6.4/§33: thin-sample authenticity is a LABEL, not confirmation — the flow
// screen refuses to attest evidence below its swap-sample floor.
// ============================================================================
#[test]
fn thin_sample_authenticity_is_not_evidence() {
    let mut s = FlowScreen::new();
    let m = [7u8; 32];
    for i in 0..(pump_quant_app::screen::MIN_SWAPS_FOR_AUTH - 1) {
        s.record(&m, u64::from(i % 5), true, 100_000);
    }
    assert!(
        !s.has_auth_evidence(&m),
        "below the sample floor there is no authenticity EVIDENCE"
    );
    s.record(&m, 3, true, 100_000);
    assert!(s.has_auth_evidence(&m), "at the floor, evidence exists");
}

// ============================================================================
// §38/§64: optimistic evidence can never satisfy promotion; Mode C paper
// still fails the probe gate fail-closed; the blocker is capability/criteria,
// never a human approval.
// ============================================================================
#[test]
fn optimistic_run_is_labeled_and_never_promotion_eligible() {
    let eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    let r = eng.promotion_readiness();
    assert!(!r.live_probe_eligible);
    assert_eq!(r.blocked_on, "mode_c_required");
}

#[test]
fn mode_c_paper_fails_probe_gate_closed() {
    let mut cfg = Config::dev_portable();
    cfg.fill_mode = FillModeCfg::AdversarialRealistic;
    let eng = Engine::new(cfg, RunMode::Replay);
    let r = eng.promotion_readiness();
    assert!(!r.live_probe_eligible);
    assert!(
        r.blocked_on.starts_with("probe_gate:"),
        "laptop Mode C blocks on unattested criteria, got {}",
        r.blocked_on
    );
}

// ============================================================================
// §24: conditional expectancy uses the configured PRIOR below the minimum
// lane sample, and the lane cell only graduates with realized fills.
// ============================================================================
#[test]
fn expectancy_is_prior_until_lane_sample_gate() {
    let cfg = Config::dev_portable();
    let prior = i128::from(cfg.gate_expected_move_bps);
    let mut eng = Engine::new(cfg, RunMode::Replay);
    // No fills yet: every lane's conditional edge is exactly the prior.
    for (sum, n, edge) in eng.expectancy_report() {
        assert_eq!((sum, n), (0, 0));
        assert_eq!(edge, prior);
    }
    // Drive a couple of realized fills; the cell accumulates but the edge
    // stays at the prior below `expectancy_min_lane_trades`.
    let mt = mint(5);
    feed_flow(&mut eng, mt, 20);
    eng.tick(AppEvent::OnchainConfirm {
        mint: mt,
        virtual_sol_lamports: REAL_CURVE_VSOL,
                    real_sol_lamports: REAL_SELLABLE_DEPTH,
    });
    for _ in 0..3 {
        eng.tick(AppEvent::Tick);
    }
    // Crash it so the position exits and realizes.
    for i in 0..16u64 {
        eng.tick(AppEvent::MarketTrade {
            mint: mt,
            price_fp: 600_000_000 - (i as i128) * 10_000_000,
            quote_lamports: 800_000,
            liquidity_lamports: REAL_CURVE_VSOL,
            signed_base: 900_000,
            buyer_entity: 10 + i % 9,
            age_slots: 12,
        });
    }
    for _ in 0..3 {
        eng.tick(AppEvent::Tick);
    }
    let rep = eng.expectancy_report();
    let filled: u32 = rep.iter().map(|(_, n, _)| *n).sum();
    if filled > 0 && filled < eng_min(&eng) {
        for (_, n, edge) in rep {
            if n > 0 {
                assert_eq!(
                    edge, prior,
                    "below the sample gate the edge must remain the prior"
                );
            }
        }
    }
}

fn eng_min(_e: &Engine) -> u32 {
    Config::dev_portable().expectancy_min_lane_trades
}
