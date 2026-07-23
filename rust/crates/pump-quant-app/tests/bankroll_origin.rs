//! Bankroll ORIGIN provenance (§33 Layer 1 / delta-§1; SERVER_BUILD_MANIFEST §7).
//!
//! The operator's law: **live trading must ALWAYS source the bankroll from the
//! reconciled on-chain wallet balance; the config `bankroll_initial_lamports` is a
//! PAPER/REPLAY seed ONLY.** The engine makes that structural through
//! [`BankrollOrigin`]. These tests pin, exactly (§22 determinism):
//!
//! * **paper/replay use the seed** — a paper/replay engine's `bankroll_balance()`
//!   equals the pre-origin value to the lamport (a regression trips the concrete pin;
//!   the golden-digest test cross-checks the same invariant end-to-end);
//! * **fail-closed guard** — `PaperSeed::require_live_verified()` is `Err`,
//!   `LiveReconciled(b)::require_live_verified()` is `Ok(b)`;
//! * **live ignores the config seed** — `Engine::new_live_reconciled(cfg, real)` and
//!   `Engine::set_live_bankroll(real)` size off `real`, NOT
//!   `cfg.bankroll_initial_lamports`: a 2-SOL config seed with a 7-SOL reconciled
//!   balance makes the base, the survival floor, the deployable capital, the total
//!   risk budget, AND the actual admitted order sizes track 7 SOL — proving the
//!   config seed has zero influence on live sizing.

use pump_quant_app::config::Config;
use pump_quant_app::engine::{BankrollOrigin, BankrollOriginError, Engine, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_app::journal_log::Decision;
use pump_quant_domain::ids::Mint;
use pump_quant_strategy::probe_ladder::{deployable_capital, derive_survival_floor};

const SOL: u64 = 1_000_000_000;

/// The config-seed default the paper engine boots with (see `Config::dev_portable`).
const PAPER_SEED_DEFAULT: u64 = 2 * SOL;

fn mint(tag: u64) -> Mint {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&tag.to_le_bytes());
    b[8] = 0xAB;
    Mint::from_bytes(b)
}

/// The admitted (entry) order sizes recorded on the journal, in emission order —
/// the end-to-end observable of what the sizing chain actually deployed.
fn admitted_sizes(eng: &Engine) -> Vec<u64> {
    eng.journal()
        .recent()
        .filter_map(|d| match *d {
            Decision::Admitted { size_lamports, .. } => Some(size_lamports),
            _ => None,
        })
        .collect()
}

/// A golden-style multi-mint tape over `cfg` (mirror of the sizing-floor-law tape):
/// many launches on deep, confirmable low-cap markets with realistic round-trip
/// economics, so the sizing chain is exercised across a broad admitted set. The tape
/// is a pure function of `cfg`, so two engines under the same `cfg` and the same
/// bankroll base produce byte-identical admits (§22).
fn drive_golden_style(mut eng: Engine) -> Engine {
    for round in 0..4u64 {
        for m in 0..24u64 {
            for i in 0..8u64 {
                eng.tick(AppEvent::MarketTrade {
                    mint: mint(m),
                    price_fp: 1_000_000_000 + (round as i128) * 40_000_000 + (i as i128) * 500_000,
                    quote_lamports: 700_000,
                    liquidity_lamports: 2_000_000_000 + (m % 8) * 250_000_000,
                    signed_base: 900_000 - (i as i64 * 50),
                    buyer_entity: (m + i) % 31,
                    age_slots: 12 + (m as u32 % 20),
                });
            }
            if round == m % 4 {
                eng.tick(AppEvent::OnchainConfirm {
                    mint: mint(m),
                    sellable_depth_lamports: 2_000_000_000,
                });
            }
        }
        for _ in 0..8 {
            eng.tick(AppEvent::Tick);
        }
    }
    eng
}

/// The realistic low-cap round-trip cost overrides the golden/sizing-floor tape uses,
/// applied on top of `dev_portable` so admits actually clear the economic gate.
fn golden_style_cfg(bankroll_initial_lamports: u64) -> Config {
    let mut cfg = Config::dev_portable();
    cfg.bankroll_initial_lamports = bankroll_initial_lamports;
    cfg.gate_expected_move_bps = 1_800;
    cfg.gate_protocol_bps = 450;
    cfg.gate_margin_bps = 150;
    cfg.gate_base_fixed_lamports = 200_000;
    cfg.gate_impact_den = 250_000;
    cfg
}

// ============================================================================
// BankrollOrigin — the type's methods in isolation.
// ============================================================================

#[test]
fn seed_lamports_reads_the_base_of_either_variant() {
    assert_eq!(BankrollOrigin::PaperSeed(2 * SOL).seed_lamports(), 2 * SOL);
    assert_eq!(
        BankrollOrigin::LiveReconciled(7 * SOL).seed_lamports(),
        7 * SOL
    );
}

#[test]
fn only_live_reconciled_is_live_verified() {
    assert!(!BankrollOrigin::PaperSeed(2 * SOL).is_live_verified());
    assert!(BankrollOrigin::LiveReconciled(7 * SOL).is_live_verified());
}

#[test]
fn require_live_verified_is_fail_closed_on_a_paper_seed() {
    // Fail-closed: a paper/replay seed can NEVER back a live trade.
    assert_eq!(
        BankrollOrigin::PaperSeed(2 * SOL).require_live_verified(),
        Err(BankrollOriginError::PaperSeedNotLive)
    );
    // A reconciled live balance returns exactly that balance.
    assert_eq!(
        BankrollOrigin::LiveReconciled(7 * SOL).require_live_verified(),
        Ok(7 * SOL)
    );
    // The paper seed's numeric value is irrelevant — even a huge seed is refused.
    assert!(BankrollOrigin::PaperSeed(u64::MAX)
        .require_live_verified()
        .is_err());
}

// ============================================================================
// Paper/Replay use the config SEED — and `bankroll_balance()` is byte-identical
// to the pre-origin value (concrete pin; the golden digest cross-checks it).
// ============================================================================

#[test]
fn paper_and_replay_size_off_the_config_seed() {
    for mode in [RunMode::Paper, RunMode::Replay] {
        let eng = Engine::new(Config::dev_portable(), mode);
        // The origin is the PAPER seed built from the config constant.
        assert_eq!(
            eng.bankroll_origin(),
            BankrollOrigin::PaperSeed(PAPER_SEED_DEFAULT),
            "{mode:?}: origin must be the config paper seed"
        );
        assert!(
            !eng.bankroll_origin().is_live_verified(),
            "{mode:?}: a paper/replay engine is never live-verified"
        );
        // The pre-origin value, pinned concretely so a regression is caught: at boot
        // (Σ realized == 0) the balance is exactly the 2-SOL config seed.
        assert_eq!(
            eng.bankroll_balance(),
            PAPER_SEED_DEFAULT,
            "{mode:?}: bankroll_balance() must equal the config seed at boot"
        );
        assert_eq!(eng.bankroll_balance(), 2_000_000_000, "{mode:?}: == 2 SOL");
    }
}

#[test]
fn paper_balance_tracks_the_configured_seed_for_any_amount() {
    // The seed is scale-invariant: whatever the operator sets, the paper base is it.
    for seed in [750_000_000u64, 2 * SOL, 10 * SOL, 100 * SOL] {
        let mut cfg = Config::dev_portable();
        cfg.bankroll_initial_lamports = seed;
        let eng = Engine::new(cfg, RunMode::Paper);
        assert_eq!(eng.bankroll_balance(), seed);
        assert_eq!(eng.bankroll_origin(), BankrollOrigin::PaperSeed(seed));
    }
}

// ============================================================================
// LIVE ignores the config seed — the load-bearing proof.
// ============================================================================

#[test]
fn live_reconciled_sizes_off_the_wallet_not_the_config_seed() {
    // Config seed says 2 SOL (paper); the reconciled on-chain wallet holds 7 SOL.
    let cfg = golden_style_cfg(2 * SOL);
    let reconciled = 7 * SOL;
    assert_ne!(
        cfg.bankroll_initial_lamports, reconciled,
        "the test is only meaningful when the seed and the wallet differ"
    );

    let live = Engine::new_live_reconciled(cfg, reconciled);

    // 1) The bankroll BASE is the reconciled wallet, NOT the config seed.
    assert_eq!(
        live.bankroll_origin(),
        BankrollOrigin::LiveReconciled(reconciled)
    );
    assert!(live.bankroll_origin().is_live_verified());
    assert_eq!(
        live.bankroll_origin().require_live_verified(),
        Ok(reconciled),
        "a live path's fail-closed guard returns the reconciled balance"
    );
    assert_eq!(
        live.bankroll_balance(),
        reconciled,
        "base tracks the wallet"
    );
    assert_ne!(
        live.bankroll_balance(),
        cfg.bankroll_initial_lamports,
        "the base must NOT be the 2-SOL config seed"
    );

    // 2) Every derived limit tracks 7 SOL, not 2 SOL. Reconstruct the exact figures
    //    the engine's sizing chain computes, using the SAME pure public functions it
    //    calls internally (floor basis = origin seed; deployable = balance − floor).
    let floor_live = derive_survival_floor(
        live.bankroll_origin().seed_lamports(),
        cfg.floor_fraction_bps,
    );
    let deployable_live = deployable_capital(live.bankroll_balance(), floor_live);
    // floor = max(0.5 SOL, 25% × 7 SOL = 1.75 SOL) = 1.75 SOL; deployable = 5.25 SOL.
    assert_eq!(
        floor_live, 1_750_000_000,
        "floor scales with the 7-SOL wallet"
    );
    assert_eq!(deployable_live, 5_250_000_000, "deployable tracks 7 SOL");

    // What the SAME limits would be if live (wrongly) sized off the 2-SOL seed —
    // strictly smaller, so "tracks 7 vs 2" is a real, observable difference.
    let floor_seed = derive_survival_floor(cfg.bankroll_initial_lamports, cfg.floor_fraction_bps);
    let deployable_seed = deployable_capital(cfg.bankroll_initial_lamports, floor_seed);
    assert_eq!(floor_seed, 500_000_000); // max(0.5 SOL, 25% × 2 SOL = 0.5 SOL)
    assert_eq!(deployable_seed, 1_500_000_000);
    assert!(
        deployable_live > deployable_seed,
        "live deployable (7 SOL) must exceed the config-seed deployable (2 SOL)"
    );

    // 3) End-to-end: the ACTUAL admitted order sizes over an identical tape prove the
    //    config seed has zero influence. A live engine (2-SOL seed, 7-SOL reconciled)
    //    admits byte-identically to a PAPER engine seeded at 7 SOL, and differently
    //    from a PAPER engine seeded at 2 SOL.
    let live_run = drive_golden_style(Engine::new_live_reconciled(
        golden_style_cfg(2 * SOL),
        7 * SOL,
    ));
    let paper_7 = drive_golden_style(Engine::new(golden_style_cfg(7 * SOL), RunMode::Replay));
    let paper_2 = drive_golden_style(Engine::new(golden_style_cfg(2 * SOL), RunMode::Replay));

    let live_sizes = admitted_sizes(&live_run);
    let paper_7_sizes = admitted_sizes(&paper_7);
    let paper_2_sizes = admitted_sizes(&paper_2);

    assert!(
        !live_sizes.is_empty(),
        "the tape must admit under a 7-SOL wallet"
    );
    assert_eq!(
        live_sizes, paper_7_sizes,
        "live (2-SOL seed, 7-SOL reconciled) sizes IDENTICALLY to paper seeded at 7 SOL — the config seed is ignored"
    );
    assert_ne!(
        paper_7_sizes, paper_2_sizes,
        "2-SOL and 7-SOL bankrolls must size differently, else the equivalence is vacuous"
    );
    // The reconciled wallet deploys strictly more capital than the config seed would.
    let sum_live: u128 = live_sizes.iter().map(|&x| u128::from(x)).sum();
    let sum_paper_2: u128 = paper_2_sizes.iter().map(|&x| u128::from(x)).sum();
    assert!(
        sum_live > sum_paper_2,
        "live (7 SOL) deploys more than the 2-SOL config seed would: {sum_live} vs {sum_paper_2}"
    );
}

#[test]
fn set_live_bankroll_moves_the_base_off_the_paper_seed() {
    // An already-built paper engine re-armed to a reconciled live wallet.
    let mut eng = Engine::new(golden_style_cfg(2 * SOL), RunMode::Paper);
    assert_eq!(eng.bankroll_origin(), BankrollOrigin::PaperSeed(2 * SOL));
    assert_eq!(eng.bankroll_balance(), 2 * SOL);

    eng.set_live_bankroll(7 * SOL);

    assert_eq!(
        eng.bankroll_origin(),
        BankrollOrigin::LiveReconciled(7 * SOL)
    );
    assert!(eng.bankroll_origin().is_live_verified());
    assert_eq!(eng.bankroll_origin().require_live_verified(), Ok(7 * SOL));
    assert_eq!(
        eng.bankroll_balance(),
        7 * SOL,
        "after re-arming, the base is the reconciled wallet, not the 2-SOL seed"
    );
}
