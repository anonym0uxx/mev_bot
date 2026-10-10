//! FEES (SCOPE_FEES items 3/4/7): every model paper fill is priced by the ONE versioned fee authority
//! (`pump_quant_protocol::fee_schedule`) at the slot of the landing state, and stamped with the version that
//! priced it. Guards: wrong venue / wrong tier (event rates must be a tier of the PumpSwap table), historical
//! config (a slot before any captured or evidenced config is a NAMED refusal, never today's rate), and no
//! double subtraction (venue fees inside an exact-in clip are never charged again on the books).
//!
//! SYNTHETIC reserves (the m3_exec_economics rig). MAINNET slots are used ONLY to select the fee config:
//! 445,637,xxx sits inside the curve trade-evidence window and between the two identical pool reads.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, TradeVenue};
use pump_quant_app::model_authority::ModelSource;
use pump_quant_domain::ids::Mint as DomainMint;
use pump_quant_inference::InferenceError;
use pump_quant_protocol::fee_schedule::FeeSelection;

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
    let buy = !i.is_multiple_of(3);
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
    e.tick(AppEvent::CurveModeObserved {
        mint: mint(),
        mayhem: false,
        slot: 0,
    });
}

struct Rig {
    e: Engine,
    clock: i64,
    slot: u64,
    n: u32,
}

fn rig(base: u64, sel: FeeSelection, answer: fn(i64) -> &'static str) -> Rig {
    let mut c = Config::dev_portable();
    c.bankroll_initial_lamports = 2_000_000_000;
    let mut e = Engine::new(c, RunMode::Paper);
    e.model_set_fee_selection(sel);
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
            base + 1_000 + u64::from(i),
        );
    }
    let t_last = T0 + 1_000 + 40 * 2_000;
    curve(&mut e, t_last, base + 2_000, VSOL, VTOK, 7_900_000_000);
    e.tick(AppEvent::OnchainConfirm {
        mint: mint(),
        virtual_sol_lamports: VSOL,
        real_sol_lamports: 7_900_000_000,
    });
    ticks(&mut e, 8);
    curve(
        &mut e,
        t_last + 1_500,
        base + 2_100,
        VSOL + 200_000_000,
        VTOK - 4_000_000_000_000,
        8_100_000_000,
    );
    ticks(&mut e, 6);
    Rig {
        e,
        clock: t_last + 1_500,
        slot: base + 2_100,
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
    fn run_to_order(&mut self, vsol: u64, vtok: u64, real_sol: u64) {
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

// ---------------------------------------------------------------------------------------------------------
// GRADUATION: held curve position -> verified canonical WSOL pool. SYNTHETIC pool reserves; the fee parts and
// virtual quote follow a captured non-cashback layout (2/93/30, vq 17_584_505_661). Pool hex is synthetic.
// ---------------------------------------------------------------------------------------------------------
const POOL: [u8; 32] = [0x5A; 32];
const VQ: u64 = 17_584_505_661;

fn pool_swap_cb(
    e: &mut Engine,
    pool: [u8; 32],
    ts: i64,
    slot: u64,
    (bres, qres): (u64, u64),
    parts: (u32, u32, u32),
    cashback: pump_quant_protocol::pumpswap_event::CashbackField,
) {
    e.tick(AppEvent::AmmSwap {
        mint: mint(),
        pool,
        pool_is_canonical: true,
        quote_is_wsol: true,
        token_reserve_pre: bres,
        quote_reserve_pre: qres,
        fee_bps: Some(parts.0 + parts.1 + parts.2),
        fee_parts: Some(parts),
        virtual_quote: Some(VQ),
        cashback,
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
/// Inside the curve trade-evidence window (>= 445,637,000) and between the two identical pool reads.
const HIST: u64 = 445_637_700;
/// Before the curve evidence window AND before the first pool read: no config is known there.
const PRE: u64 = 400_000_000;

fn sum_prefix(e: &Engine, k: &str) -> u64 {
    e.model_lane_report()
        .iter()
        .filter(|(key, _)| key.starts_with(k))
        .map(|(_, v)| *v)
        .sum()
}

/// ITEM 4 (historical): a by-slot entry inside the evidence window fills and is stamped with the
/// labelled assumption it used; nothing is stamped `pinned`.
#[test]
fn historical_by_slot_entry_is_stamped_with_the_evidenced_curve_config() {
    let r = rig(HIST, FeeSelection::BySlot, |_| HOLD);
    assert!(
        r.e.model_position_open(&MINT),
        "{:?}",
        r.e.model_lane_report()
    );
    assert_eq!(
        r.rep("fee_stamp:curve@454754100:assumed_from_trade_evidence:tier0:125bp"),
        1,
        "{:?}",
        r.e.model_lane_report()
    );
    assert_eq!(r.rep("fee_stamp:curve@454754100:pinned_slot_ignored"), 0);
}

/// ITEM 4 (historical): a by-slot entry at a slot before any known config is REFUSED by name; no fill,
/// no inventory, no default rate.
#[test]
fn historical_config_unknown_refuses_the_fill_by_name_and_never_uses_todays_rate() {
    let r = rig(PRE, FeeSelection::BySlot, |_| HOLD);
    assert!(
        !r.e.model_position_open(&MINT),
        "{:?}",
        r.e.model_lane_report()
    );
    assert!(
        r.rep("fill_none:quote_unavailable:fee_config_unknown_at_slot") >= 1,
        "{:?}",
        r.e.model_lane_report()
    );
    assert_eq!(sum_prefix(&r.e, "fee_stamp:"), 0);
    assert_eq!(r.rep("econ:entry_venue_fees_lamports"), 0);
}

/// The same synthetic replay under `Pinned` (the test-rig default) still fills and says so.
#[test]
fn pinned_selection_fills_and_is_labelled_pinned() {
    let r = rig(PRE, FeeSelection::Pinned, |_| HOLD);
    assert!(r.e.model_position_open(&MINT));
    assert_eq!(
        r.rep("fee_stamp:curve@454754100:pinned_slot_ignored:tier0:125bp"),
        1,
        "{:?}",
        r.e.model_lane_report()
    );
}

/// NO DOUBLE SUBTRACTION (item 3), under the by-slot authority: the curve venue fee is INSIDE the exact-in clip
/// (committed = clip + network + tip + ATA, never + venue fee again) and on the exit it comes out of gross once.
#[test]
fn venue_fees_inside_a_net_quote_are_never_subtracted_again_on_the_books() {
    let mut r = rig(
        HIST,
        FeeSelection::BySlot,
        |s| if s == 0 { EXIT } else { HOLD },
    );
    let v = r.e.model_accounting_view(&MINT);
    let leg = 10_000 + Config::dev_portable().entry_tip_lamports;
    let ata = pump_quant_app::cost_model::ATA_RENT_LAMPORTS;
    let size = v.attribution_entry_spend.unwrap() - leg - ata;
    // Independent: largest n with n + ceil(95n) + ceil(30n) <= size (exact-SOL-in at the 125 bp curve config).
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
    let venue_in = (c(lo, 95) + c(lo, 30)) as u64;
    assert_eq!(r.rep("econ:entry_venue_fees_lamports"), venue_in);
    assert!(venue_in > 0);
    // Non-circular anchor: the clip implied by the booked cost (cost - leg - ATA) must be EXACTLY the all-in
    // spend that delivers the inventory held. A venue fee charged again on top of the clip inflates the implied
    // clip, and its exact-in token output no longer equals the inventory.
    let (vs0, vt0) = (
        u128::from(VSOL + 200_000_000),
        u128::from(VTOK - 4_000_000_000_000),
    );
    assert_eq!(
        u128::from(v.inventory_tokens.unwrap()),
        vt0 * (lo - 1) / (vs0 + lo - 1),
        "booked cost - leg - ATA is exactly the clip that bought the inventory (no fee on top)"
    );
    assert_eq!(
        v.committed,
        size + leg + ata,
        "committed = clip + leg + ATA: the venue fee inside the clip is not added a second time"
    );
    let (vs, vt) = (VSOL + 200_000_000, VTOK - 4_000_000_000_000);
    r.run_to_order(vs, vt, 8_100_000_000);
    let before = r.e.model_accounting_view(&MINT);
    let inv = before.inventory_tokens.unwrap();
    r.land(vs, vt, 8_100_000_000);
    let f = r.e.model_mgmt_fills().last().copied().expect("exit filled");
    let g = u128::from(inv) * u128::from(vs) / (u128::from(vt) + u128::from(inv));
    let venue = (g * 95).div_ceil(10_000) + (g * 30).div_ceil(10_000);
    let xleg = 10_000 + u128::from(Config::dev_portable().exit_tip_lamports);
    assert_eq!(u128::from(f.gross_lamports), g);
    assert_eq!(
        u128::from(f.fee_lamports),
        venue + xleg,
        "venue once + leg once"
    );
    let after = r.e.model_accounting_view(&MINT);
    assert_eq!(
        after.realized - before.realized,
        i128::try_from(g - venue - xleg).unwrap()
            - i128::from(before.remaining_cost_basis.unwrap())
            + i128::from(ata - pump_quant_app::cost_model::ATA_CLOSE_LAMPORTS),
        "realized = gross - venue - leg - basis + ATA refund"
    );
    assert_eq!(
        r.rep("fee_stamp:curve@454754100:assumed_from_trade_evidence:tier0:125bp"),
        2,
        "entry + exit both priced by the by-slot curve config"
    );
}

/// WRONG VENUE / WRONG TIER (item 7): after graduation a pool landing event whose fee stack is not one tier of
/// the PumpSwap table in force (all-zero, or the CURVE's 95/30 stack reported on a pool) is refused by name;
/// the order stays unfilled and nothing is booked. A real tier stack then fills, stamped with its tier.
#[test]
fn a_pool_event_whose_rates_are_not_a_tier_is_refused_and_a_tier_stack_fills_stamped() {
    use pump_quant_protocol::pumpswap_event::CashbackField;
    let mut r = rig(
        HIST,
        FeeSelection::BySlot,
        |s| if s == 0 { EXIT } else { HOLD },
    );
    let inv = r.e.model_inventory_tokens(&MINT).unwrap();
    r.run_to_order(VSOL + 200_000_000, VTOK - 4_000_000_000_000, 8_100_000_000);
    graduate(&mut r);
    ticks(&mut r.e, 4);
    let (bres, qres) = (200_000_000_000_000u64, 85_000_000_000u64);
    let known0 = CashbackField::Known {
        bps: 0,
        lamports: 0,
        layout_len: 369,
    };
    for (i, parts) in [(0u32, 0u32, 30u32), (0, 95, 30), (20, 5, 51)]
        .into_iter()
        .enumerate()
    {
        r.clock += 1_000;
        r.slot += 5;
        pool_swap_cb(&mut r.e, POOL, r.clock, r.slot, (bres, qres), parts, known0);
        assert_eq!(
            r.rep("mgmt:quote_unavailable:fee_rates_not_in_schedule"),
            i as u64 + 1,
            "{parts:?}: {:?}",
            r.e.model_lane_report()
        );
        assert_eq!(r.e.model_inventory_tokens(&MINT), Some(inv), "nothing sold");
    }
    r.clock += 1_000;
    r.slot += 5;
    pool_swap_cb(
        &mut r.e,
        POOL,
        r.clock,
        r.slot,
        (bres, qres),
        (2, 93, 30),
        known0,
    );
    assert!(
        !r.e.model_position_open(&MINT),
        "{:?}",
        r.e.model_lane_report()
    );
    assert_eq!(
        r.rep("fee_stamp:pumpswap@445186127:between_identical_reads:tier0:125bp"),
        1,
        "{:?}",
        r.e.model_lane_report()
    );
}
