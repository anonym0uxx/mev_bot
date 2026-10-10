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
    assert_eq!(
        r.rep("fee:executed_fill_fee_modelled_default"),
        0,
        "a paper fill is priced by the authority, never by the default"
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

/// F4: no unjustified default on a NEW simulated fill, yet an ALREADY EXECUTED fill still books. After graduation
/// a pool event whose rates are not a tier leaves the paper executor without an authoritative fee: it refuses by
/// name and sells nothing (no 95/30 fallback). The executor then reports the same order as executed, price-only:
/// it books (inventory leaves the book), and the modelled fee on it is disclosed by name.
#[test]
fn f4_simulated_fill_refuses_without_authoritative_fees_but_an_executed_fill_still_books() {
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
    let known0 = CashbackField::Known {
        bps: 0,
        lamports: 0,
        layout_len: 369,
    };
    r.clock += 1_000;
    r.slot += 5;
    pool_swap_cb(
        &mut r.e,
        POOL,
        r.clock,
        r.slot,
        (200_000_000_000_000, 85_000_000_000),
        (0, 95, 30),
        known0,
    );
    assert_eq!(
        r.rep("mgmt:quote_unavailable:fee_rates_not_in_schedule"),
        1,
        "{:?}",
        r.e.model_lane_report()
    );
    assert_eq!(
        r.e.model_inventory_tokens(&MINT),
        Some(inv),
        "no simulated sell"
    );
    assert!(
        r.e.model_mgmt_fills().is_empty(),
        "nothing booked by default"
    );
    assert_eq!(r.rep("fee:executed_fill_fee_modelled_default"), 0);
    let (id, _, intended, _) = r.e.model_mgmt_pending(&MINT).expect("EXIT still pending");
    r.e.model_mgmt_apply_reconciled_fill(MINT, id, intended, 22_000)
        .expect("an executed fill books");
    assert_eq!(
        r.e.model_mgmt_fills().len(),
        1,
        "{:?}",
        r.e.model_lane_report()
    );
    assert_ne!(
        r.e.model_inventory_tokens(&MINT),
        Some(inv),
        "inventory left the book"
    );
    assert_eq!(
        r.rep("fee:executed_fill_fee_modelled_default"),
        1,
        "{:?}",
        r.e.model_lane_report()
    );
}

// ==== F5 ====
// F5 (SCOPE_ENG_R5 W2): fixed before/after replay that CROSSES A PUMPSWAP FEE TIER, on the real engine.
//
// LABEL: SYNTHETIC. No available capture contains a model fill on PumpSwap (proc/FEES_TRACE.md §F5: 60 daemon run
// dirs, `ready|venue=pumpswap` = 0) and the curve has one tier, so no captured replay can exercise the tier
// difference. This fixture builds it: the same held curve position graduates and is exited on a canonical pool
// whose market cap sits in tier 10 (29,470-34,380 SOL: 20/5/50 = 75 bp/side) or tier 11 (34,380-39,300 SOL:
// 20/5/45 = 70 bp/side). Pool reserves are synthetic; the virtual quote and layout follow a captured event; the
// slot is inside the window both pool reads agree on (never today's config applied to a historical slot).
//
// For each order the test checks PROMPT (v1 statement, v2 statement from `exec_costs_v2`), QUOTE (fee stamp of the
// paper fill) and SETTLEMENT (booked venue fees) agree, and writes one JSONL row per order to `$PQ_F5_OUT` for the
// independent reconciliation script `proc/fees_f5_reconcile.py`.
use pump_quant_app::exec_costs_v2::{exec_costs, ExecMarket};
use pump_quant_app::exec_quote::FeeCtx;
use pump_quant_proposal::contract_v2::{cost_line_v2, decompose_v2};
use pump_quant_proposal::cost::{cost_line, Regime};
use pump_quant_protocol::pumpswap_event::CashbackField as F5Cashback;

const BRES: u64 = 200_000_000_000_000;

/// Pool market cap (lamports) the program computes: (quote + virtual quote) x 1e15 / base.
fn mcap(qres: u64) -> u128 {
    (u128::from(qres) + u128::from(VQ)) * 1_000_000_000_000_000 / u128::from(BRES)
}

/// One tier-specific exit: returns (stamp label, gross, venue fees, v1 per-side bp, v2 per-side bp, v2 round trip).
fn exit_on_pool(qres: u64, parts: (u32, u32, u32)) -> serde_json::Value {
    let mut r = rig(
        HIST,
        FeeSelection::BySlot,
        |s| if s == 0 { EXIT } else { HOLD },
    );
    r.run_to_order(VSOL + 200_000_000, VTOK - 4_000_000_000_000, 8_100_000_000);
    graduate(&mut r);
    ticks(&mut r.e, 4);
    let inv = r.e.model_inventory_tokens(&MINT).unwrap();
    r.clock += 1_000;
    r.slot += 5;
    let landing_slot = r.slot;
    let known0 = F5Cashback::Known {
        bps: 0,
        lamports: 0,
        layout_len: 369,
    };
    pool_swap_cb(&mut r.e, POOL, r.clock, r.slot, (BRES, qres), parts, known0);
    assert!(
        !r.e.model_position_open(&MINT),
        "{:?}",
        r.e.model_lane_report()
    );
    let stamp: Vec<String> =
        r.e.model_lane_report()
            .iter()
            .filter(|(k, _)| k.starts_with("fee_stamp:pumpswap@"))
            .map(|(k, _)| k.trim_start_matches("fee_stamp:").to_string())
            .collect();
    assert_eq!(stamp.len(), 1, "{:?}", r.e.model_lane_report());
    let gross = r.rep("econ:exit_gross_lamports");
    let fees = r.rep("econ:exit_venue_fees_lamports");
    // PROMPT after (v2), from the same authority at the same slot and pool state.
    let ec = exec_costs(
        // SYNTHETIC fixture: live supply stated as 1e15 (the fixture's pool is built at that supply).
        ExecMarket::Pool {
            effective_quote: u128::from(qres) + u128::from(VQ),
            base_reserve: u128::from(BRES),
            mint_supply: Some(1_000_000_000_000_000),
        },
        FeeCtx::by_slot(landing_slot),
        0,
        0,
    )
    .unwrap();
    let clip_sol = gross as f64 / 1e9;
    let v2_line = cost_line_v2(clip_sol, None, &ec);
    let v2_rt = decompose_v2(clip_sol, None, &ec).round_trip_bp;
    // PROMPT before (v1): the trained AMM statement.
    let v1_line = cost_line(clip_sol, None, Regime::Amm);
    serde_json::json!({
        "label": "SYNTHETIC_tier_crossing_fixture",
        "slot": landing_slot, "pool_base": BRES, "pool_quote": qres, "virtual_quote": VQ,
        "mcap_lamports": mcap(qres).to_string(), "event_parts": [parts.0, parts.1, parts.2],
        "inventory_tokens": inv, "quote_stamp": stamp[0], "settled_gross": gross, "settled_venue_fees": fees,
        "v1_prompt_bp_per_side": 30, "v1_line": v1_line,
        "v2_prompt_bp_per_side": ec.venue_bp_per_side, "v2_stamp": ec.fee_stamp, "v2_round_trip_bp": v2_rt,
        "v2_line": v2_line,
    })
}

#[test]
fn f5_synthetic_pool_tier_crossing_prompt_quote_settlement_agree() {
    // 31,900 SOL (tier 10, 75 bp) and 37,000 SOL (tier 11, 70 bp): q + vq = mcap x base / 1e15.
    let rows = [
        exit_on_pool(6_380_000_000_000 - VQ, (20, 5, 50)),
        exit_on_pool(7_400_000_000_000 - VQ, (20, 5, 45)),
    ];
    let mut out = String::new();
    for (row, (bp, tier)) in rows.iter().zip([(75u64, 10u16), (70, 11)]) {
        let stamp = row["quote_stamp"].as_str().unwrap();
        // QUOTE: the paper fill was priced by the tier for this pool state, at this slot, with provenance.
        assert_eq!(
            stamp,
            format!("pumpswap@445186127:between_identical_reads:tier{tier}:{bp}bp")
        );
        // PROMPT (after) states exactly the executor's tier; (before) understated it.
        assert_eq!(row["v2_prompt_bp_per_side"].as_u64().unwrap(), bp);
        assert_eq!(row["v2_stamp"].as_str().unwrap(), stamp);
        assert!(row["v1_prompt_bp_per_side"].as_u64().unwrap() < bp);
        // SETTLEMENT: booked venue fees are this tier's per-component ceil charge on the gross, once.
        let g = u128::from(row["settled_gross"].as_u64().unwrap());
        let c = |b: u128| (g * b).div_ceil(10_000);
        let parts: Vec<u128> = row["event_parts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| u128::from(v.as_u64().unwrap()))
            .collect();
        let want: u128 = parts.iter().map(|b| c(*b)).sum();
        assert_eq!(
            u128::from(row["settled_venue_fees"].as_u64().unwrap()),
            want,
            "{row}"
        );
        assert!(row["v2_round_trip_bp"].as_i64().unwrap() >= 2 * bp as i64);
        out.push_str(&row.to_string());
        out.push('\n');
    }
    // The tier difference is actually exercised: different stamps, different charged rate.
    assert_ne!(rows[0]["quote_stamp"], rows[1]["quote_stamp"]);
    if let Ok(p) = std::env::var("PQ_F5_OUT") {
        std::fs::write(p, out).unwrap();
    }
}

// ---- SCOPE_FEES_R6 T3: protocol tier at a REAL threshold of the pinned config, checked by an independent decoder ----
// Points: immediately below / exactly at / immediately above the tier-10/11 threshold (34,380 SOL), one point inside
// the old +-5% band where the band and the protocol differ (35,000 SOL: band 75, protocol 70), and the live-supply
// case that explains the measured "next-higher-rate" events (0.98e15 supply). Rows go to `$PQ_T3_OUT` for
// `proc/fees_r6_tier_decoder.py`, which parses the pinned bytes and selects the tier itself.
#[test]
fn t3_protocol_tier_at_a_real_threshold_rows_for_the_independent_decoder() {
    const SOL: u128 = 1_000_000_000;
    const FULL: u128 = 1_000_000_000_000_000;
    let th = 34_380 * SOL;
    // (label, effective quote, base reserve, live supply, expected tier, expected bp)
    let pts: [(&str, u128, u128, u128, u16, u64); 7] = [
        ("below", th - 1, FULL, FULL, 10, 75),
        ("at", th, FULL, FULL, 11, 70),
        ("above", th + 1, FULL, FULL, 11, 70),
        ("inside_old_band_35000", 35_000 * SOL, FULL, FULL, 11, 70),
        (
            "live_supply_0p98",
            35_000 * SOL,
            FULL,
            FULL / 100 * 98,
            10,
            75,
        ),
        ("first_threshold_below", 420 * SOL - 1, FULL, FULL, 0, 125),
        ("first_threshold_at", 420 * SOL, FULL, FULL, 1, 120),
    ];
    let mut out = String::new();
    for (label, q, b, s, tier, bp) in pts {
        let ec = exec_costs(
            ExecMarket::Pool {
                effective_quote: q,
                base_reserve: b,
                mint_supply: Some(s),
            },
            FeeCtx::by_slot(454_754_100),
            0,
            0,
        )
        .unwrap();
        assert_eq!(ec.venue_bp_per_side, bp, "{label}");
        assert_eq!(
            ec.fee_stamp,
            format!("pumpswap@454754100:read_at_slot:tier{tier}:{bp}bp"),
            "{label}"
        );
        let mcap = pump_quant_app::exec_costs_v2::pool_market_cap(q, b, Some(s)).unwrap();
        out.push_str(
            &serde_json::json!({
                "label": label, "effective_quote": q.to_string(), "base_reserve": b.to_string(),
                "mint_supply": s.to_string(), "rust_mcap": mcap.to_string(), "rust_tier": tier, "rust_bp": bp,
            })
            .to_string(),
        );
        out.push('\n');
    }
    if let Ok(p) = std::env::var("PQ_T3_OUT") {
        std::fs::write(p, out).unwrap();
    }
}
