//! ONE entry scheduler in paper-model mode (`Engine::tick`): legacy watchlist rank/score/tick state cannot
//! dispatch, prioritise or exclude a Qwen entry request.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, TradeVenue};
use pump_quant_app::model_authority::ModelSource;
use pump_quant_domain::ids::Mint as DomainMint;
use pump_quant_inference::InferenceError;

const T0: i64 = 1_800_000_000_000;
const CREATOR: [u8; 32] = [0xCD; 32];
const VSOL: u64 = 37_900_000_000;
const VTOK: u64 = 849_000_000_000_000;
const BUY: &str =
    "DECISION: BUY\nSIZE: SMALL\nPRICE LIMIT: 0.5\nINVALIDATION: none\nEVIDENCE: flow sustained";

struct Stub(Arc<AtomicUsize>);
impl ModelSource for Stub {
    fn complete(&self, _s: &str, _u: &str) -> Result<String, InferenceError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(BUY.to_string())
    }
}

fn m(i: u8) -> [u8; 32] {
    [i; 32]
}
fn dm(b: [u8; 32]) -> DomainMint {
    DomainMint::from_bytes(b)
}
fn wallet(i: u32) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[0] = (i % 200) as u8 + 1;
    w[1] = (i / 200) as u8;
    w[31] = 1;
    w
}
fn cfg() -> Config {
    let mut c = Config::dev_portable();
    c.bankroll_initial_lamports = 2_000_000_000;
    c
}
fn armed() -> (Engine, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut e = Engine::new(cfg(), RunMode::Paper);
    e.enable_paper_model(Stub(Arc::clone(&calls)));
    (e, calls)
}
fn rep(e: &Engine, prefix: &str) -> u64 {
    e.model_lane_report()
        .iter()
        .filter(|(k, _)| k.starts_with(prefix))
        .map(|(_, v)| *v)
        .sum()
}

/// A full warm-up tape for `mint`: launch + `n` priced prints (2 s apart from `start`) + curve + confirm.
/// `wallets` = how many DISTINCT traders the prints cycle through (1 => single-entity tape).
fn feed(mint: [u8; 32], start: i64, n: u32, wallets: u32, slot0: u64) -> Vec<AppEvent> {
    let mut ev = vec![AppEvent::LaunchObserved {
        mint: dm(mint),
        creator: CREATOR,
        launch_unix_ms: start - 1_000,
    }];
    for i in 0..n {
        let buy = i % 3 != 0;
        ev.push(AppEvent::MarketTrade {
            mint: dm(mint),
            price_fp: 22_000 + i128::from(i),
            quote_lamports: 500_000_000 + u64::from(i),
            liquidity_lamports: VSOL,
            signed_base: if buy { 30_000_000_000 } else { -30_000_000_000 },
            buyer_entity: 1 + u64::from(i % wallets),
            age_slots: 30,
            recv_unix_ms: Some(start + i64::from(i) * 2_000),
            trader_pubkey: Some(wallet(i % wallets)),
            slot: Some(slot0 + u64::from(i)),
            fee_lamports: Some(60_000 + u64::from(i) * 100),
            cu_consumed: Some(90_000 + u64::from(i)),
            venue: Some(TradeVenue::PumpFun),
            event_id: None,
            feature: None,
        });
    }
    ev.push(AppEvent::CurveObserved {
        mint: dm(mint),
        v_sol_lamports: VSOL,
        v_tokens: VTOK,
        real_sol_lamports: 7_900_000_000,
        real_tokens: 569_000_000_000_000,
        recv_unix_ms: Some(start + i64::from(n - 1) * 2_000),
        slot: 2_000 + slot0,
    });
    ev.push(AppEvent::OnchainConfirm {
        mint: dm(mint),
        virtual_sol_lamports: VSOL,
        real_sol_lamports: 7_900_000_000,
    });
    ev
}

fn drive(e: &mut Engine, evs: &[AppEvent], ticks: usize) {
    for ev in evs {
        e.tick(*ev);
    }
    for _ in 0..ticks {
        e.tick(AppEvent::Tick);
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn legacy_promotion_never_dispatches_in_paper_model_mode() {
    let (mut e, calls) = armed();
    e.barrier_enable();
    // Drive prints only first (events, no scheduler tick): the legacy watchlist promotes on the EVALUATION tick, and
    // the stream scheduler runs on the same tick BEFORE it, so any legacy dispatch would be the SECOND ask of a mint
    // the stream just asked about. Count the requests per mint: exactly one, from the stream source.
    for ev in &feed(m(0xA1), T0 + 1_000, 40, 12, 1_000) {
        e.tick(*ev);
    }
    for _ in 0..3 {
        e.tick(AppEvent::Tick);
    }
    assert!(
        rep(&e, "legacy_promotion_observed_not_dispatched") > 0,
        "the legacy path must still be OBSERVED (counted): {:?}",
        e.model_lane_report()
    );
    assert_eq!(
        rep(&e, "admit_attempt|src=legacy_promoted"),
        0,
        "no admit attempt may come from legacy promotion: {:?}",
        e.model_lane_report()
    );
    // Within a tick the stream pass runs first, so a legacy admit call would be refused as held/pending (skip:*).

    assert_eq!(
        rep(&e, "skip:"),
        0,
        "legacy promotion must not call the admit path at all: {:?}",
        e.model_lane_report()
    );
    let entries = e
        .barrier_take_log()
        .iter()
        .filter(|d| d.kind == "entry")
        .count();
    assert_eq!(
        entries,
        1,
        "one market, one request: {:?}",
        e.model_lane_report()
    );
    assert_eq!(
        rep(&e, "admit_attempt|src=stream"),
        1,
        "the stream scheduler carries the market to the model: {:?}",
        e.model_lane_report()
    );
    assert!(calls.load(Ordering::SeqCst) >= 1 || entries == 1);
}

#[test]
fn a_market_the_old_strategy_filter_rejects_still_reaches_the_model() {
    // Fresh-launch exemption keeps young markets, so build a MATURE market (age_slots >= 64) with a single-entity
    // tape: the old universe screen's wash guard (trades/entity > 6) rejects it, and the legacy path never promotes it.
    let (mut e, calls) = armed();
    let mut ev = feed(m(0xB2), T0 + 1_000, 40, 1, 1_000);
    for x in &mut ev {
        if let AppEvent::MarketTrade { age_slots, .. } = x {
            *age_slots = 500;
        }
    }
    drive(&mut e, &ev, 10);
    let report = e.report();
    assert!(
        report.universe_filtered > 0,
        "precondition: the OLD strategy filter must have rejected this market (universe_filtered={}): {:?}",
        report.universe_filtered,
        e.model_lane_report()
    );
    assert_eq!(
        rep(&e, "uniq_legacy_promoted"),
        0,
        "legacy never promoted it"
    );
    assert!(
        calls.load(Ordering::SeqCst) >= 1 && rep(&e, "dispatched") >= 1,
        "the model must still be asked: {:?}",
        e.model_lane_report()
    );
}

fn order(e: &mut Engine) -> Vec<u8> {
    e.barrier_take_log()
        .iter()
        .filter(|d| d.kind == "entry")
        .map(|d| d.mint[0])
        .collect()
}

#[test]
fn ready_markets_are_dispatched_oldest_dirty_first_not_by_mint_order() {
    let (mut e, _calls) = armed();
    e.barrier_enable();
    // Mint bytes sort C < B < A... the DIRTY ORDER is A, B, C (A observed first).
    let mut evs = Vec::new();
    evs.extend(feed(m(0xC3), T0 + 1_000, 40, 12, 1_000));
    evs.extend(feed(m(0xB2), T0 + 1_200, 40, 12, 2_000));
    evs.extend(feed(m(0xA1), T0 + 1_400, 40, 12, 3_000));
    for ev in &evs {
        e.tick(*ev);
    }
    e.tick(AppEvent::Tick);
    let got = order(&mut e);
    assert_eq!(
        got,
        vec![0xC3, 0xB2, 0xA1],
        "dispatch must follow when each market FIRST became dirty (oldest first): {:?}",
        e.model_lane_report()
    );
}

#[test]
fn ineligible_older_markets_do_not_consume_the_budget_of_an_eligible_one() {
    let (mut e, _calls) = armed();
    e.barrier_enable();
    // 12 OLDER markets with prints only (no launch, no curve): each is refused by name. They outnumber the
    // per-tick budget, so a scheduler that charged refusals against it would never reach the eligible market.
    let mut evs = Vec::new();
    for i in 0..12u8 {
        let mut mkt = feed(
            m(0x10 + i),
            T0 + 1_000 + i64::from(i),
            3,
            3,
            100 * u64::from(i + 1),
        );
        mkt.retain(|x| matches!(x, AppEvent::MarketTrade { .. }));
        evs.extend(mkt);
    }
    evs.extend(feed(m(0xEE), T0 + 2_000, 40, 12, 9_000));
    for ev in &evs {
        e.tick(*ev);
    }
    e.tick(AppEvent::Tick);
    assert_eq!(
        order(&mut e),
        vec![0xEE],
        "the eligible market must be dispatched on the first tick: {:?}",
        e.model_lane_report()
    );
}

#[test]
fn lane_backpressure_keeps_the_queue_and_its_age_without_starving_management() {
    let (mut e, _calls) = armed();
    e.barrier_enable();
    // Six ready markets observed in this order: 0x50..0x54, then 0x10 (youngest, but its mint sorts FIRST).
    // The entry table holds 4, so 0x54 hits the wall and 0x10 is behind it.
    let order_in: [u8; 6] = [0x50, 0x51, 0x52, 0x53, 0x54, 0x10];
    for (i, b) in order_in.iter().enumerate() {
        for ev in feed(
            m(*b),
            T0 + 1_000 + 100 * i as i64,
            40,
            12,
            1_000 * (i as u64 + 1),
        ) {
            e.tick(ev);
        }
    }
    e.tick(AppEvent::Tick);
    assert_eq!(order(&mut e), vec![0x50, 0x51, 0x52, 0x53], "oldest first");
    assert!(
        rep(&e, "sched_stopped_backpressure") >= 1,
        "the stop is NAMED: {:?}",
        e.model_lane_report()
    );
    // Capacity frees as the four answers settle. The waiting markets keep their ORIGINAL dirty times: 0x54 (older)
    // goes before 0x10 (younger). A push-back that re-stamped 0x54 with the current clock would let 0x10 overtake it.
    let _ = e.barrier_settle(Duration::from_secs(3));
    for _ in 0..4 {
        e.tick(AppEvent::Tick);
    }
    let later = order(&mut e);
    assert_eq!(
        later.first(),
        Some(&0x54),
        "the waiting older market is served before a younger one: {later:?} / {:?}",
        e.model_lane_report()
    );
}
