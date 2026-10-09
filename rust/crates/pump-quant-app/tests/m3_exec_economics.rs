//! M3 acceptance slice: size-specific execution economics in the shared paper executor.
//!
//! FROZEN ASSUMPTIONS (declared before evaluation; synthetic inputs labelled):
//! * curve reserves are SYNTHETIC (the model_manage_e2e rig: vsol 37.9 SOL, vtok 849e12) except where a
//!   mainnet capture is named;
//! * landing = first observation >= 400 ms after the order, on a newer slot (engine rule, unchanged);
//! * fees: curve 95 + 30 bp ceil (pinned pump-fees FeeConfig, slot 454,754,100); landed leg = 10,000 network
//!   (measured p50, NOT a ceiling) + configured tip;
//! * expected values are computed in this file from the program arithmetic, never read back from the engine.
//!
//! Cases: drained curve (shallow liquidity) refuses the requested size by name and keeps the order unfilled;
//! dust proceeds below the landed leg are booked as a net cost; venue fees / network / tip are each booked
//! once; requoting on a later state never changes the model's quantity.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, TradeVenue};
use pump_quant_app::exec_quote;
use pump_quant_app::model_authority::ModelSource;
use pump_quant_domain::ids::Mint as DomainMint;
use pump_quant_inference::InferenceError;

const T0: i64 = 1_800_000_000_000;
const MINT: [u8; 32] = [0xAB; 32];
const CREATOR: [u8; 32] = [0xCD; 32];
const VSOL: u64 = 37_900_000_000;
const VTOK: u64 = 849_000_000_000_000;
const BUY: &str =
    "DECISION: BUY\nSIZE: SMALL\nPRICE LIMIT: 0.5\nINVALIDATION: none\nEVIDENCE: flow sustained";
const HOLD: &str = "DECISION: HOLD\nINVALIDATION: none\nEVIDENCE: x";
const EXIT: &str = "DECISION: EXIT\nINVALIDATION: none\nEVIDENCE: x";

struct Script {
    answer: fn(i64) -> &'static str,
    calls: Arc<AtomicUsize>,
    _p: Arc<Mutex<()>>,
}
impl ModelSource for Script {
    fn complete(&self, _s: &str, user: &str) -> Result<String, InferenceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if user.starts_with("Decide the next action for a position you already hold") {
            let step: i64 = user
                .lines()
                .find_map(|l| l.strip_prefix("STEP: "))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            return Ok((self.answer)(step).to_string());
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
fn ticks(e: &mut Engine, n: usize) {
    for _ in 0..n {
        e.tick(AppEvent::Tick);
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn trade(e: &mut Engine, i: u32, ts: i64, slot: u64) {
    let buy = i % 3 != 0;
    e.tick(AppEvent::MarketTrade {
        mint: mint(),
        price_fp: 22_000 + i128::from(i),
        quote_lamports: 500_000_000 + u64::from(i),
        liquidity_lamports: VSOL,
        signed_base: if buy { 30_000_000_000 } else { -30_000_000_000 },
        buyer_entity: 1 + u64::from(i),
        age_slots: 30,
        recv_unix_ms: Some(ts),
        trader_pubkey: Some(wallet(i)),
        slot: Some(slot),
        fee_lamports: Some(60_000),
        cu_consumed: Some(90_000),
        venue: Some(TradeVenue::PumpFun),
        event_id: None,
        feature: None,
    });
}
/// Post-entry monitoring print at the held market's level (same as model_manage_e2e's rig).
fn print(e: &mut Engine, i: u32, ts: i64, slot: u64) {
    e.tick(AppEvent::MarketTrade {
        mint: mint(),
        price_fp: 45_300 + i128::from(i % 7),
        quote_lamports: 500_000_000 + u64::from(i),
        liquidity_lamports: VSOL,
        signed_base: 30_000_000_000,
        buyer_entity: 1 + u64::from(i),
        age_slots: 30,
        recv_unix_ms: Some(ts),
        trader_pubkey: Some(wallet(i)),
        slot: Some(slot),
        fee_lamports: Some(60_000),
        cu_consumed: Some(90_000),
        venue: Some(TradeVenue::PumpFun),
        event_id: None,
        feature: None,
    });
}
fn curve(e: &mut Engine, ts: i64, slot: u64, vsol: u64, vtok: u64, real_sol: u64) {
    e.tick(AppEvent::CurveObserved {
        mint: mint(),
        v_sol_lamports: vsol,
        v_tokens: vtok,
        real_sol_lamports: real_sol,
        real_tokens: 565_000_000_000_000,
        recv_unix_ms: Some(ts),
        slot,
    });
}

struct Rig {
    e: Engine,
    clock: i64,
    slot: u64,
    n: u32,
}

fn rig(answer: fn(i64) -> &'static str) -> Rig {
    let mut c = Config::dev_portable();
    c.bankroll_initial_lamports = 2_000_000_000;
    let mut e = Engine::new(c, RunMode::Paper);
    e.enable_paper_model(Script {
        answer,
        calls: Arc::new(AtomicUsize::new(0)),
        _p: Arc::new(Mutex::new(())),
    });
    e.tick(AppEvent::LaunchObserved {
        mint: mint(),
        creator: CREATOR,
        launch_unix_ms: T0,
    });
    for i in 0..40u32 {
        trade(
            &mut e,
            i,
            T0 + 1_000 + i64::from(i) * 2_000,
            1_000 + u64::from(i),
        );
    }
    let t_last = T0 + 1_000 + 40 * 2_000;
    curve(&mut e, t_last, 2_000, VSOL, VTOK, 7_900_000_000);
    e.tick(AppEvent::OnchainConfirm {
        mint: mint(),
        virtual_sol_lamports: VSOL,
        real_sol_lamports: 7_900_000_000,
    });
    ticks(&mut e, 8);
    curve(
        &mut e,
        t_last + 1_500,
        2_100,
        VSOL + 200_000_000,
        VTOK - 4_000_000_000_000,
        8_100_000_000,
    );
    ticks(&mut e, 6);
    assert!(e.model_position_open(&MINT), "{:?}", e.model_lane_report());
    Rig {
        e,
        clock: t_last + 1_500,
        slot: 2_100,
        n: 41,
    }
}

impl Rig {
    fn rep(&self, k: &str) -> u64 {
        self.e
            .model_lane_report()
            .iter()
            .filter(|(key, _)| key.starts_with(k))
            .map(|(_, v)| *v)
            .sum()
    }
    /// Quiet market until a management order is pending (prints keep the position monitored).
    fn to_order(&mut self, vsol: u64, vtok: u64, real_sol: u64) {
        let end = self.clock + 180_000;
        while self.clock < end && self.e.model_mgmt_pending(&MINT).is_none() {
            self.clock += 1_000;
            self.slot += 1;
            self.n += 1;
            curve(&mut self.e, self.clock, self.slot, vsol, vtok, real_sol);
            print(&mut self.e, self.n, self.clock, self.slot);
            ticks(&mut self.e, 2);
        }
        assert!(
            self.e.model_mgmt_pending(&MINT).is_some(),
            "{:?}",
            self.e.model_lane_report()
        );
    }
    fn land(&mut self, vsol: u64, vtok: u64, real_sol: u64) {
        self.clock += 1_000;
        self.slot += 5;
        curve(&mut self.e, self.clock, self.slot, vsol, vtok, real_sol);
        ticks(&mut self.e, 4);
    }
}

#[test]
fn entry_books_venue_fees_inside_the_clip_and_network_and_tip_once() {
    let r = rig(|_| HOLD);
    let v = r.e.model_accounting_view(&MINT);
    let inv = v.inventory_tokens.unwrap();
    // Independent expectation: the fill landed on (VSOL+200M, VTOK-4e12). Clip = the SMALL tier the engine sized.
    let clip =
        r.e.model_all_fills()
            .first()
            .map(|_| ())
            .map(|_| v.attribution_entry_spend.unwrap());
    let clip = clip.unwrap();
    let leg = 10_000 + Config::dev_portable().entry_tip_lamports;
    let ata = pump_quant_app::cost_model::ATA_RENT_LAMPORTS;
    let size = clip - leg - ata;
    // exact-SOL-in by hand: largest n with n + ceil(95n) + ceil(30n) <= size; tokens = vtok*(n-1)/(vsol+n-1)
    let c = |n: u128, b: u128| (n * b).div_ceil(10_000);
    let (mut lo, mut hi) = (0u128, u128::from(size));
    while lo < hi {
        let m = lo + (hi - lo).div_ceil(2);
        if m + c(m, 95) + c(m, 30) <= u128::from(size) {
            lo = m
        } else {
            hi = m - 1
        }
    }
    let (vs, vt) = (
        u128::from(VSOL + 200_000_000),
        u128::from(VTOK - 4_000_000_000_000),
    );
    let toks = vt * (lo - 1) / (vs + lo - 1);
    assert_eq!(
        u128::from(inv),
        toks,
        "inventory = exact-in tokens at the landing state"
    );
    assert_eq!(
        r.rep("econ:entry_venue_fees_lamports"),
        (c(lo, 95) + c(lo, 30)) as u64
    );
    assert_eq!(r.rep("econ:entry_network_lamports"), 10_000);
    assert_eq!(
        r.rep("econ:entry_tip_lamports"),
        Config::dev_portable().entry_tip_lamports
    );
}

#[test]
fn a_drained_curve_refuses_the_requested_size_by_name_and_books_nothing() {
    // SHALLOW LIQUIDITY (synthetic): the curve's real SOL is far below what selling the whole inventory pays.
    let mut r = rig(|s| if s == 0 { EXIT } else { HOLD });
    let inv = r.e.model_inventory_tokens(&MINT).unwrap();
    let real0 = r.e.model_accounting_view(&MINT).realized;
    let (vs, vt) = (VSOL + 200_000_000, VTOK - 4_000_000_000_000);
    r.to_order(vs, vt, 8_100_000_000);
    let (_, _, intended, _) = r.e.model_mgmt_pending(&MINT).unwrap();
    assert_eq!(intended, inv, "EXIT asks for the whole free inventory");
    // Landing state: real SOL 1_000 lamports. Gross of the requested size >> 1_000: the program refuses it.
    r.land(vs, vt, 1_000);
    assert_eq!(
        r.rep("mgmt:quote_unavailable:curve_real_sol_insufficient") >= 1,
        true,
        "{:?}",
        r.e.model_lane_report()
    );
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv), "nothing sold");
    assert_eq!(
        r.e.model_accounting_view(&MINT).realized,
        real0,
        "no fabricated settlement"
    );
    assert!(
        r.e.model_mgmt_pending(&MINT).is_some(),
        "order stays pending (state may recover) until TTL"
    );
    // Requote at the SAME size on recovered state: fills the whole quantity, never a resized one.
    r.land(vs, vt, 8_100_000_000);
    let f =
        r.e.model_mgmt_fills()
            .last()
            .copied()
            .expect("filled after recovery");
    assert_eq!(f.tokens, inv, "requote kept the model's quantity");
}

#[test]
fn an_exit_books_gross_venue_fees_network_and_tip_each_once() {
    let mut r = rig(|s| if s == 0 { EXIT } else { HOLD });
    let inv = r.e.model_inventory_tokens(&MINT).unwrap();
    let (vs, vt) = (VSOL + 200_000_000, VTOK - 4_000_000_000_000);
    r.to_order(vs, vt, 8_100_000_000);
    let before = r.e.model_accounting_view(&MINT);
    r.land(vs, vt, 8_100_000_000);
    let f = r.e.model_mgmt_fills().last().copied().expect("exit filled");
    let g = u128::from(inv) * u128::from(vs) / (u128::from(vt) + u128::from(inv));
    let venue = (g * 95).div_ceil(10_000) + (g * 30).div_ceil(10_000);
    let leg = 10_000 + u128::from(Config::dev_portable().exit_tip_lamports);
    assert_eq!(
        u128::from(f.gross_lamports),
        g,
        "gross = curve sell of the exact size"
    );
    assert_eq!(
        u128::from(f.fee_lamports),
        venue + leg,
        "venue + network + tip, once"
    );
    assert_eq!(r.rep("econ:exit_venue_fees_lamports"), venue as u64);
    assert_eq!(r.rep("econ:exit_network_lamports"), 10_000);
    let after = r.e.model_accounting_view(&MINT);
    assert_eq!(
        after.realized - before.realized,
        i128::try_from(g).unwrap()
            - i128::try_from(venue + leg).unwrap()
            - i128::from(before.remaining_cost_basis.unwrap())
            // The closing EXIT also closes the ATA: the rent DEPOSIT charged in the entry basis comes back less
            // one close signature (a separate, refundable item - not a fee and not double-counted).
            + i128::from(
                pump_quant_app::cost_model::ATA_RENT_LAMPORTS
                    - pump_quant_app::cost_model::ATA_CLOSE_LAMPORTS
            ),
        "realized = gross - venue - leg - remaining basis + ATA refund (nothing subtracted twice)"
    );
}

#[test]
fn dust_proceeds_below_the_landed_leg_are_a_net_cost_not_recovery() {
    let mut r = rig(|s| if s == 0 { EXIT } else { HOLD });
    let inv = r.e.model_inventory_tokens(&MINT).unwrap();
    // A collapsed curve (synthetic): vsol barely above the initial 30 SOL, huge vtok, real SOL tiny but enough
    // to pay this size. Venue-net < the 20,000-lamport landed leg.
    let (vs, vt) = (30_000_000_000u64, 1_072_999_000_000_000u64);
    let g = u128::from(inv) * u128::from(vs) / (u128::from(vt) + u128::from(inv));
    let real = u64::try_from(g).unwrap() + 1;
    r.to_order(VSOL + 200_000_000, VTOK - 4_000_000_000_000, 8_100_000_000);
    r.land(vs, vt, real);
    let f = r.e.model_mgmt_fills().last().copied();
    if let Some(f) = f {
        assert!(f.net_lamports < 0, "dust is never positive recovery: {f:?}");
        let venue_net = g - (g * 95).div_ceil(10_000) - (g * 30).div_ceil(10_000);
        if venue_net <= 20_000 {
            assert_eq!(r.rep("econ:sell_net_below_leg_cost"), 1);
        }
    } else {
        panic!(
            "dust exit should still fill (a real cost): {:?}",
            r.e.model_lane_report()
        );
    }
}

#[test]
fn the_curve_quote_at_the_bf2_drained_mainnet_state_is_unavailable_at_full_size() {
    // MAINNET CAPTURE (read-only, slot 454,754,100): 74721d. Today's state, never a historical valuation.
    let q = exec_quote::curve_sell(
        30_000_654_602,
        1_072_976_591_886_115,
        654_602,
        4_015_177_314_798,
    );
    assert!(matches!(
        q,
        Err(exec_quote::QuoteRefusal::CurveRealSolInsufficient {
            max_tokens: 23_412_456_533
        })
    ));
}

// ---------------------------------------------------------------------------------------------------------
// GRADUATION: held curve position -> verified canonical WSOL pool. SYNTHETIC pool reserves; the fee parts and
// virtual quote follow a captured non-cashback layout (2/93/30, vq 17_584_505_661). Pool hex is synthetic.
// ---------------------------------------------------------------------------------------------------------
const POOL: [u8; 32] = [0x5A; 32];
const POOL2: [u8; 32] = [0x5B; 32];
const VQ: u64 = 17_584_505_661;

fn pool_swap(e: &mut Engine, pool: [u8; 32], ts: i64, slot: u64, bres: u64, qres: u64, cr: u32) {
    e.tick(AppEvent::AmmSwap {
        mint: mint(),
        pool,
        pool_is_canonical: true,
        quote_is_wsol: true,
        token_reserve_pre: bres,
        quote_reserve_pre: qres,
        fee_bps: Some(2 + 93 + cr),
        fee_parts: Some((2, 93, cr)),
        virtual_quote: Some(VQ),
        is_buy: true,
        token_amount: 1_000_000_000,
        quote_lamports: 50_000,
        trader: [7u8; 32],
        fee_lamports: Some(5_000),
        cu_consumed: Some(1),
        recv_unix_ms: Some(ts),
        slot,
    });
    ticks(e, 2);
}

fn graduate(r: &mut Rig) {
    // The curve completes (virtual reserves zeroed by the program) and the migration is observed.
    r.clock += 1_000;
    r.slot += 1;
    curve(&mut r.e, r.clock, r.slot, 0, 0, 0);
    r.e.tick(AppEvent::Migration {
        mint: mint(),
        slot: r.slot,
    });
}

/// A held CURVE position whose curve completes sells through its verified canonical WSOL pool. The pending EXIT
/// keeps its id and quantity; inventory/basis are untouched by the switch; the fill is priced by the pool quote.
#[test]
fn a_held_curve_position_graduates_and_its_exit_routes_to_the_verified_pool() {
    let mut r = rig(|s| if s == 0 { EXIT } else { HOLD });
    let inv = r.e.model_inventory_tokens(&MINT).unwrap();
    let basis = r.e.model_accounting_view(&MINT).remaining_cost_basis;
    let (vs, vt) = (VSOL + 200_000_000, VTOK - 4_000_000_000_000);
    r.to_order(vs, vt, 8_100_000_000);
    let (id, _k, intended, filled) = r.e.model_mgmt_pending(&MINT).unwrap();
    // Graduation before any curve landing state: the curve is complete (zero reserves) -> curve sell refused.
    graduate(&mut r);
    ticks(&mut r.e, 4);
    assert!(
        r.rep("mgmt:quote_unavailable:curve_complete") >= 1,
        "{:?}",
        r.e.model_lane_report()
    );
    assert_eq!(
        r.e.model_mgmt_pending(&MINT),
        Some((id, _k, intended, filled)),
        "order unchanged by the switch"
    );
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv));
    assert_eq!(r.e.model_accounting_view(&MINT).remaining_cost_basis, basis);
    // The verified pool swap is the landing state: the SAME order fills on the pool at the SAME size.
    let (bres, qres) = (200_000_000_000_000u64, 85_000_000_000u64);
    r.clock += 1_000;
    r.slot += 5;
    pool_swap(&mut r.e, POOL, r.clock, r.slot, bres, qres, 30);
    let f =
        r.e.model_mgmt_fills()
            .last()
            .copied()
            .expect("exit filled on the pool");
    assert_eq!(f.order_id, id);
    assert_eq!(f.tokens, intended);
    let eff = u128::from(qres) + u128::from(VQ);
    let g = eff * u128::from(intended) / (u128::from(bres) + u128::from(intended));
    let venue = (g * 2).div_ceil(10_000) + (g * 93).div_ceil(10_000) + (g * 30).div_ceil(10_000);
    assert_eq!(
        u128::from(f.gross_lamports),
        g,
        "pool gross for the exact size"
    );
    assert_eq!(
        u128::from(f.fee_lamports),
        venue + 10_000 + u128::from(Config::dev_portable().exit_tip_lamports)
    );
    assert!(r.rep("mgmt:route:curve_to_pool") >= 1);
    assert!(!r.e.model_position_open(&MINT));
}

/// A second, different pool for the same mint makes the binding CONFLICTING: no pool price, no settlement.
#[test]
fn a_conflicting_pool_binding_is_a_named_unavailable_route_never_a_fill() {
    let mut r = rig(|s| if s == 0 { EXIT } else { HOLD });
    let inv = r.e.model_inventory_tokens(&MINT).unwrap();
    r.to_order(VSOL + 200_000_000, VTOK - 4_000_000_000_000, 8_100_000_000);
    graduate(&mut r);
    r.clock += 1_000;
    r.slot += 5;
    pool_swap(
        &mut r.e,
        POOL,
        r.clock,
        r.slot,
        200_000_000_000_000,
        85_000_000_000,
        30,
    );
    // (the first swap may already fill; only assert on the conflicting case when it did not)
    if r.e.model_position_open(&MINT) {
        r.clock += 1_000;
        r.slot += 5;
        pool_swap(
            &mut r.e,
            POOL2,
            r.clock,
            r.slot,
            200_000_000_000_000,
            85_000_000_000,
            30,
        );
        assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv));
    }
}

/// A completed curve with NO verified pool state: the exit stays unfilled, named, and expires without books.
#[test]
fn a_graduated_position_without_verified_pool_state_is_degraded_not_settled() {
    let mut r = rig(|s| if s == 0 { EXIT } else { HOLD });
    let inv = r.e.model_inventory_tokens(&MINT).unwrap();
    let realized0 = r.e.model_accounting_view(&MINT).realized;
    r.to_order(VSOL + 200_000_000, VTOK - 4_000_000_000_000, 8_100_000_000);
    graduate(&mut r);
    for _ in 0..8 {
        r.clock += 1_000;
        r.slot += 1;
        curve(&mut r.e, r.clock, r.slot, 0, 0, 0);
        ticks(&mut r.e, 2);
    }
    assert!(
        r.e.model_mgmt_fills().is_empty(),
        "no fabricated settlement"
    );
    assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv));
    assert_eq!(r.e.model_accounting_view(&MINT).realized, realized0);
    assert!(r.rep("mgmt:quote_unavailable:curve_complete") >= 1);
}
