//! OFFSET slice (operator 15:40 PT): Mayhem and mode-unknown curves are UNSUPPORTED FOR TRADING. They must not
//! inherit the ordinary constant-offset identity (confirm / depth) OR the curve quote formulas (fill, sell quote,
//! liquidation value, mark). Their raw reserves remain recorded as evidence. Reserves come from REAL captured
//! account bytes (fixtures/offset_mode_accounts.rs), decoded by the production decoder.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pump_quant_app::config::Config;
use pump_quant_app::curve_depth::{CurveDepth, CurveDepthMode, DepthBasis, DepthRefusal};
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, TradeVenue};
use pump_quant_app::model_authority::ModelSource;
use pump_quant_domain::ids::Mint as DomainMint;
use pump_quant_inference::InferenceError;
use pump_quant_protocol::decode::{classify_pump_curve, decode_pump_curve, PumpCurve};

include!("fixtures/offset_mode_accounts.rs");

fn curve(h: &str) -> PumpCurve {
    decode_pump_curve(&hex_bytes(h)).unwrap()
}

fn mode_of(h: &str) -> CurveDepthMode {
    CurveDepthMode::from_class(classify_pump_curve(&hex_bytes(h)))
}

// ---------------------------------------------------------------- depth (pure)

#[test]
fn ordinary_real_account_gets_the_cross_checked_decoded_depth() {
    let c = curve(CANON_60);
    let d =
        CurveDepth::from_classified_curve(classify_pump_curve(&hex_bytes(CANON_60)), &c).unwrap();
    assert_eq!(
        d.basis(),
        DepthBasis::CurveDecoded {
            virtual_sol: 45_024_964_690,
            real_sol: 15_024_964_690
        }
    );
    assert_eq!(d.payout_reserve(), Some(15_024_964_690));
}

#[test]
fn mayhem_real_accounts_are_refused_by_name_never_reclassified_or_overstated() {
    // vsol 144.6 SOL, real 5.09 SOL. Mode-blind: MigratedPool, payout = vsol (28x the decoded real SOL).
    let hi = curve(MAYHEM_HI_640);
    assert_eq!(
        (hi.virtual_sol, hi.real_sol),
        (144_604_657_461, 5_087_973_981)
    );
    assert_eq!(
        CurveDepth::decoded(hi.virtual_sol, hi.real_sol).payout_reserve(),
        Some(144_604_657_461),
        "the OLD mode-blind evidence, preserved: a live Mayhem curve read as a migrated pool"
    );
    // The coincidence account passes the ordinary cross-check numerically (offset = 30 SOL exactly).
    let co = curve(MAYHEM_COINCIDE_130489);
    assert!(!CurveDepth::decoded(co.virtual_sol, co.real_sol).is_unknown());
    for h in [
        MAYHEM_HI_640,
        MAYHEM_COINCIDE_130489,
        MAYHEM_CB_5158,
        MAYHEM_NONSOL_50841,
        MAYHEM_CB_NONSOL_719,
        FX804_32067,
        FX804_32626,
    ] {
        let c = curve(h);
        assert_eq!(
            CurveDepth::from_classified_curve(classify_pump_curve(&hex_bytes(h)), &c),
            Err(DepthRefusal::MayhemModeUnsupported)
        );
        assert_eq!(
            CurveDepth::decoded_for_mode(mode_of(h), c.virtual_sol, c.real_sol),
            Err(DepthRefusal::MayhemModeUnsupported)
        );
    }
}

#[test]
fn unknown_mode_is_never_ordinary() {
    let c = curve(CANON_60);
    // Same ordinary reserves, mode not decoded (e.g. 49-byte layout): refused, not cross-checked.
    assert_eq!(
        CurveDepth::decoded_for_mode(CurveDepthMode::Unknown, c.virtual_sol, c.real_sol),
        Err(DepthRefusal::CurveModeUnknown)
    );
    let short = &hex_bytes(CANON_60)[..49];
    assert_eq!(
        CurveDepth::from_classified_curve(
            classify_pump_curve(short),
            &decode_pump_curve(short).unwrap()
        ),
        Err(DepthRefusal::CurveModeUnknown)
    );
    assert_eq!(
        CurveDepthMode::from_decoded_mayhem(None),
        CurveDepthMode::Unknown
    );
}

#[test]
fn ordinary_cross_check_still_refuses_a_contradictory_pair() {
    let c = curve(CANON_60);
    assert_eq!(
        CurveDepth::decoded_for_mode(CurveDepthMode::Ordinary, c.virtual_sol, c.virtual_sol),
        Err(DepthRefusal::OrdinaryCrossCheckRefused)
    );
}

// ---------------------------------------------------------------- engine (armed model lane)

const T0: i64 = 1_800_000_000_000;
const MINT: [u8; 32] = [0x5A; 32];
const BUY: &str =
    "DECISION: BUY\nSIZE: SMALL\nPRICE LIMIT: 0.5\nINVALIDATION: none\nEVIDENCE: flow sustained";
const HOLD: &str = "DECISION: HOLD\nINVALIDATION: none\nEVIDENCE: x";
const REDUCE: &str = "DECISION: REDUCE\nINVALIDATION: none\nEVIDENCE: x";

struct Stub {
    calls: Arc<AtomicUsize>,
    mgmt: &'static str,
}
impl ModelSource for Stub {
    fn complete(&self, _s: &str, user: &str) -> Result<String, InferenceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if user.starts_with("Decide the next action for a position you already hold") {
            return Ok(self.mgmt.to_string());
        }
        Ok(BUY.to_string())
    }
}

fn mint() -> DomainMint {
    DomainMint::from_bytes(MINT)
}

fn wallet(i: u32) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[0] = (i % 200) as u8 + 1;
    w[1] = (i / 200) as u8;
    w[31] = 1;
    w
}

fn armed(mgmt: &'static str) -> Engine {
    let mut c = Config::dev_portable();
    c.bankroll_initial_lamports = 2_000_000_000;
    let mut e = Engine::new(c, RunMode::Paper);
    e.enable_paper_model(Stub {
        calls: Arc::new(AtomicUsize::new(0)),
        mgmt,
    });
    e
}

fn t_last() -> i64 {
    T0 + 1_000 + 40 * 2_000
}

fn obs(e: &mut Engine, c: &PumpCurve, ts: i64, slot: u64) {
    e.tick(AppEvent::CurveObserved {
        mint: mint(),
        v_sol_lamports: c.virtual_sol,
        v_tokens: c.virtual_token,
        real_sol_lamports: c.real_sol,
        real_tokens: c.real_token,
        recv_unix_ms: Some(ts),
        slot,
    });
}

fn print(e: &mut Engine, i: u32, ts: i64, slot: u64, liq: u64) {
    e.tick(AppEvent::MarketTrade {
        mint: mint(),
        price_fp: 22_000 + i128::from(i % 7),
        quote_lamports: 500_000_000 + u64::from(i),
        liquidity_lamports: liq,
        signed_base: if i % 3 != 0 {
            30_000_000_000
        } else {
            -30_000_000_000
        },
        buyer_entity: 1 + u64::from(i),
        age_slots: 30,
        recv_unix_ms: Some(ts),
        trader_pubkey: Some(wallet(i)),
        slot: Some(slot),
        fee_lamports: Some(60_000 + u64::from(i) * 100),
        cu_consumed: Some(90_000 + u64::from(i)),
        venue: Some(TradeVenue::PumpFun),
        event_id: None,
        feature: None,
    });
}

fn ticks(e: &mut Engine, n: usize) {
    for _ in 0..n {
        e.tick(AppEvent::Tick);
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn rep(e: &Engine, prefix: &str) -> u64 {
    e.model_lane_report()
        .iter()
        .filter(|(k, _)| k.starts_with(prefix))
        .map(|(_, v)| *v)
        .sum()
}

/// Launch + 40 prints + curve obs + (optional) mode + confirm, all from one real account.
fn setup(e: &mut Engine, c: &PumpCurve, mode: Option<bool>) {
    e.tick(AppEvent::LaunchObserved {
        mint: mint(),
        creator: [0xCD; 32],
        launch_unix_ms: T0,
    });
    for i in 0..40u32 {
        print(
            e,
            i,
            T0 + 1_000 + i64::from(i) * 2_000,
            1_000 + u64::from(i),
            c.virtual_sol,
        );
    }
    obs(e, c, t_last(), 2_000);
    if let Some(m) = mode {
        e.tick(AppEvent::CurveModeObserved {
            mint: mint(),
            mayhem: m,
            slot: 2_000,
        });
    }
    e.tick(AppEvent::OnchainConfirm {
        mint: mint(),
        virtual_sol_lamports: c.virtual_sol,
        real_sol_lamports: c.real_sol,
    });
    ticks(e, 8);
}

#[test]
fn engine_confirm_records_ordinary_and_refuses_mayhem_or_unknown_by_name() {
    let mut e = armed(HOLD);
    setup(&mut e, &curve(CANON_60), Some(false));
    assert!(e.is_market_confirmed(&MINT));

    // The coincidence Mayhem account passes the ordinary arithmetic but is NOT recorded.
    let mut e = armed(HOLD);
    setup(&mut e, &curve(MAYHEM_COINCIDE_130489), Some(true));
    assert!(!e.is_market_confirmed(&MINT));
    assert!(
        rep(&e, "confirm:depth_unsupported:mayhem_mode") >= 1,
        "{:?}",
        e.model_lane_report()
    );

    // Ordinary reserves but the mode was never decoded: refused fail-closed.
    let mut e = armed(HOLD);
    setup(&mut e, &curve(CANON_60), None);
    assert!(!e.is_market_confirmed(&MINT));
    assert!(rep(&e, "confirm:depth_unsupported:curve_mode_unknown") >= 1);
}

/// Hold a position opened on an ordinary curve, then the curve is observed in Mayhem mode (sticky).
fn held_then_mayhem(mgmt: &'static str) -> Engine {
    let mut e = armed(mgmt);
    let c0 = curve(CANON_60);
    setup(&mut e, &c0, Some(false));
    let landing = PumpCurve {
        virtual_sol: c0.virtual_sol + 200_000_000,
        virtual_token: c0.virtual_token - 3_000_000_000_000,
        real_sol: c0.real_sol + 200_000_000,
        real_token: c0.real_token - 3_000_000_000_000,
        complete: false,
    };
    obs(&mut e, &landing, t_last() + 1_500, 2_100);
    ticks(&mut e, 6);
    assert!(e.model_position_open(&MINT), "{:?}", e.model_lane_report());
    // Ordinary control: a liquidation value exists before the flip.
    assert!(
        e.model_liquidation_estimate(&MINT).is_ok(),
        "{:?}",
        e.model_liquidation_estimate(&MINT)
    );
    e.tick(AppEvent::CurveModeObserved {
        mint: mint(),
        mayhem: true,
        slot: 2_101,
    });
    e
}

#[test]
fn a_mayhem_curve_gets_no_liquidation_value_and_no_mark_from_the_curve_formulas() {
    let mut e = held_then_mayhem(HOLD);
    obs(&mut e, &curve(FX804_32067), t_last() + 2_000, 2_102);
    assert_eq!(
        e.model_liquidation_estimate(&MINT),
        Err("curve_quote_unsupported:mayhem_mode")
    );
    let x = e.model_open_exposure();
    let h = x.iter().find(|h| h.mint == MINT).expect("held");
    assert_eq!(h.mark_price_fp, None);
    assert_eq!(
        h.valuation_unavailable,
        Some("curve_quote_unsupported:mayhem_mode")
    );
}

#[test]
fn a_reduce_on_a_mayhem_curve_is_placed_but_never_priced_from_curve_formulas() {
    let mut e = held_then_mayhem(REDUCE);
    let (mut clock, mut slot) = (t_last() + 1_500, 2_102u64);
    let mut c = curve(FX804_32067);
    for n in 0..20u32 {
        clock += 5_000;
        slot += 1;
        c.virtual_sol += 1_000_000;
        obs(&mut e, &c, clock, slot);
        print(&mut e, 41 + n, clock, slot, c.virtual_sol);
        ticks(&mut e, 2);
    }
    // Whichever risk-reducing order is pending (the model's REDUCE, or the protection exit the vsol drop
    // triggers first) is refused BY NAME on pricing; nothing is settled from Mayhem reserves.
    let named = rep(
        &e,
        "mgmt:quote_unavailable:curve_quote_unsupported:mayhem_mode",
    ) + rep(
        &e,
        "protect:quote_unavailable:curve_quote_unsupported:mayhem_mode",
    );
    assert!(named >= 1, "{:?}", e.model_lane_report());
    assert!(
        e.model_mgmt_fills().is_empty(),
        "no REDUCE settled from Mayhem reserves"
    );
    assert_eq!(rep(&e, "mgmt:fill_curve"), 0);
    assert_eq!(rep(&e, "protect:fill"), 0);
    assert!(
        e.model_position_open(&MINT),
        "exposure is preserved, not silently closed"
    );
}

#[test]
fn a_pending_entry_order_whose_curve_turns_mayhem_is_retired_unfilled_by_name() {
    let mut e = armed(HOLD);
    let c0 = curve(CANON_60);
    setup(&mut e, &c0, Some(false));
    assert_eq!(
        e.model_pending_orders(),
        1,
        "BUY verdict placed an order: {:?}",
        e.model_lane_report()
    );
    // The mode is learned AFTER the order was placed, BEFORE the landing state arrives.
    e.tick(AppEvent::CurveModeObserved {
        mint: mint(),
        mayhem: true,
        slot: 2_050,
    });
    let landing = PumpCurve {
        virtual_sol: c0.virtual_sol + 200_000_000,
        virtual_token: c0.virtual_token - 3_000_000_000_000,
        real_sol: c0.real_sol + 200_000_000,
        real_token: c0.real_token - 3_000_000_000_000,
        complete: false,
    };
    obs(&mut e, &landing, t_last() + 1_500, 2_100);
    ticks(&mut e, 6);
    assert_eq!(
        rep(&e, "fill_none:curve_quote_unsupported:mayhem_mode"),
        1,
        "{:?}",
        e.model_lane_report()
    );
    assert!(!e.model_position_open(&MINT));
    assert_eq!(e.model_pending_orders(), 0);
}
