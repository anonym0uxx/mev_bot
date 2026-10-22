//! ENTRY-BASELINE contract through the REAL engine entry point: with `EntryContract::ProbeSlV1` the
//! system prompt sent is the verbatim probe_sl_v1 instruction, the strict scorer validator gates the
//! answer (v16-shaped / truncated / BUY-without-SIZE answers are named no-trades, never executed), and
//! a valid `DECISION: BUY / SIZE: SMALL / REASON / RISK` answer reaches the unchanged authority and
//! creates an order intent (no price limit). Synthetic events: execution integration only.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, EntryContract, RunMode};
use pump_quant_app::event::{AppEvent, TradeVenue};
use pump_quant_app::model_authority::ModelSource;
use pump_quant_domain::ids::Mint as DomainMint;
use pump_quant_inference::{Completion, InferenceError};
use pump_quant_inference::probe_sl::PROBE_SL_V1_SYSTEM_PROMPT;


const T0: i64 = 1_800_000_000_000;
const MINT: [u8; 32] = [0xAB; 32];
const CREATOR: [u8; 32] = [0xCD; 32];
const VSOL: u64 = 37_900_000_000;
const VTOK: u64 = 849_000_000_000_000;

fn wallet(i: u32) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[0] = (i % 200) as u8 + 1;
    w[1] = (i / 200) as u8;
    w[31] = 1;
    w
}

fn mint() -> DomainMint {
    DomainMint::from_bytes(MINT)
}

fn cfg() -> Config {
    let mut c = Config::dev_portable();
    c.bankroll_initial_lamports = 2_000_000_000;
    c
}

/// Answers with a fixed completion + finish reason; records every system prompt it was sent.
struct Fixed {
    text: &'static str,
    finish: Option<&'static str>,
    tokens: Option<u32>,
    systems: Arc<Mutex<Vec<String>>>,
    calls: Arc<AtomicUsize>,
}
impl ModelSource for Fixed {
    fn complete(&self, _s: &str, _u: &str) -> Result<String, InferenceError> {
        Ok(self.text.to_string())
    }
    fn complete_meta(&self, s: &str, _u: &str) -> Result<Completion, InferenceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.systems.lock().unwrap().push(s.to_string());
        Ok(Completion { text: self.text.to_string(), finish_reason: self.finish.map(str::to_string), completion_tokens: self.tokens })
    }
}

fn events(n: u32) -> Vec<AppEvent> {
    let mut ev = vec![AppEvent::LaunchObserved {
        mint: mint(),
        creator: CREATOR,
        launch_unix_ms: T0,
    }];
    for i in 0..n {
        let buy = i % 3 != 0;
        ev.push(AppEvent::MarketTrade {
            mint: mint(),
            price_fp: 22_000 + i128::from(i),
            quote_lamports: 500_000_000 + u64::from(i),
            liquidity_lamports: VSOL,
            signed_base: if buy { 30_000_000_000 } else { -30_000_000_000 },
            buyer_entity: 1 + u64::from(i),
            age_slots: 30,
            recv_unix_ms: Some(T0 + 1_000 + i64::from(i) * 2_000),
            trader_pubkey: Some(wallet(i)),
            slot: Some(1_000 + u64::from(i)),
            fee_lamports: Some(60_000 + u64::from(i) * 100),
            cu_consumed: Some(90_000 + u64::from(i)),
            venue: Some(TradeVenue::PumpFun),
        });
    }
    let t_last = T0 + 1_000 + i64::from(n) * 2_000;
    ev.push(AppEvent::CurveObserved {
        mint: mint(),
        v_sol_lamports: VSOL,
        v_tokens: VTOK,
        real_sol_lamports: 7_900_000_000,
        real_tokens: 569_000_000_000_000,
        recv_unix_ms: Some(t_last),
        slot: 2_000,
    });
    ev.push(AppEvent::OnchainConfirm {
        mint: mint(),
        virtual_sol_lamports: VSOL,
        real_sol_lamports: 7_900_000_000,
    });
    ev
}

fn ticks(e: &mut Engine, n: usize) {
    for _ in 0..n {
        e.tick(AppEvent::Tick);
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn curve(e: &mut Engine, ts: i64, slot: u64, dsol: u64) {
    e.tick(AppEvent::CurveObserved {
        mint: mint(),
        v_sol_lamports: VSOL + dsol,
        v_tokens: VTOK - 4_000_000_000_000,
        real_sol_lamports: 8_100_000_000,
        real_tokens: 565_000_000_000_000,
        recv_unix_ms: Some(ts),
        slot,
    });
    ticks(e, 6);
}


/// A curve observation at the feed clock, without extra ticks: the account subscription keeps the
/// reserves fresh in production, and management now refuses reserves older than PRICING_BUDGET_MS.
fn curve_quiet(e: &mut Engine, ts: i64, slot: u64) {
    e.tick(AppEvent::CurveObserved {
        mint: mint(),
        v_sol_lamports: VSOL + 200_000_000,
        v_tokens: VTOK - 4_000_000_000_000,
        real_sol_lamports: 8_100_000_000,
        real_tokens: 565_000_000_000_000,
        recv_unix_ms: Some(ts),
        slot,
    });
}

/// A trade print on the held mint at the feed clock `ts` (keeps decision state fresh and the clock moving).
fn print(e: &mut Engine, i: u32, ts: i64, slot: u64) {
    e.tick(AppEvent::MarketTrade {
        mint: mint(),
        // Near the curve fill's entry price (~45_085): the held position must not be underwater,
        // or the agreed HARD STOP (correctly) closes it before the model is ever asked.
        price_fp: 45_300 + i128::from(i % 7),
        quote_lamports: 500_000_000 + u64::from(i),
        liquidity_lamports: VSOL,
        signed_base: 30_000_000_000,
        buyer_entity: 1 + u64::from(i),
        age_slots: 30,
        recv_unix_ms: Some(ts),
        trader_pubkey: Some(wallet(i)),
        slot: Some(slot),
        fee_lamports: Some(60_000 + u64::from(i) * 100),
        cu_consumed: Some(90_000 + u64::from(i)),
        venue: Some(TradeVenue::PumpFun),
    });
}

struct Rig {
    e: Engine,
    prompts: Arc<Mutex<Vec<String>>>,
    calls: Arc<AtomicUsize>,
    clock: i64,
    slot: u64,
    n: u32,
}

fn run(text: &'static str, finish: Option<&'static str>, tokens: Option<u32>) -> (Engine, Arc<Mutex<Vec<String>>>, Arc<AtomicUsize>) {
    let systems = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut e = Engine::new(cfg(), RunMode::Paper);
    e.enable_paper_model(Fixed { text, finish, tokens, systems: Arc::clone(&systems), calls: Arc::clone(&calls) });
    e.set_entry_contract(EntryContract::ProbeSlV1);
    let evs = events(40);
    for ev in &evs {
        e.tick(ev.clone());
    }
    ticks(&mut e, 8);
    let t_last = T0 + 1_000 + 40 * 2_000;
    curve(&mut e, t_last + 1_500, 2_100, 200_000_000);
    (e, systems, calls)
}

fn rep(e: &Engine, k: &str) -> u64 {
    e.model_lane_report().iter().filter(|(key, _)| key.starts_with(k)).map(|(_, v)| *v).sum()
}

#[test]
fn probe_sl_system_prompt_is_sent_verbatim_and_a_valid_buy_creates_an_order_intent() {
    let (e, systems, calls) = run("DECISION: BUY\nSIZE: SMALL\nREASON: net flow positive with fresh entrants\nRISK: thin book", Some("stop"), Some(40));
    assert!(calls.load(Ordering::SeqCst) >= 1, "{:?}", e.model_lane_report());
    for s in systems.lock().unwrap().iter() {
        assert_eq!(s, PROBE_SL_V1_SYSTEM_PROMPT, "system instruction must be the artifact, byte for byte");
    }
    assert!(rep(&e, "probe_sl:valid:BUY") >= 1, "{:?}", e.model_lane_report());
    assert!(rep(&e, "verdict:buy") >= 1, "a valid probe BUY reaches the authority: {:?}", e.model_lane_report());
    assert_eq!(rep(&e, "probe_sl:invalid"), 0);
}

#[test]
fn v16_shaped_truncated_and_sizeless_answers_are_named_no_trades() {
    let cases: [(&'static str, Option<&'static str>, Option<u32>, &str); 4] = [
        ("DECISION: SKIP\nSIZE: NONE\nINVALIDATION: n\nEVIDENCE: e\nCOUNTEREVIDENCE: c\nEVIDENCE_STATUS: complete", Some("stop"), Some(60), "probe_sl:invalid:size_on_non_buy"),
        ("DECISION: BUY\nSIZE: FULL\nREASON: r\nRISK: cut mid", Some("length"), Some(128), "probe_sl:invalid:not_natural_stop"),
        ("DECISION: BUY\nREASON: r\nRISK: k", Some("stop"), Some(30), "probe_sl:invalid:buy_without_size"),
        ("DECISION: BUY\nSIZE: SMALL\nREASON: r\nRISK: k", None, None, "probe_sl:invalid:no_finish_reason"),
    ];
    for (text, finish, tokens, key) in cases {
        let (e, _, calls) = run(text, finish, tokens);
        assert!(calls.load(Ordering::SeqCst) >= 1);
        assert!(rep(&e, key) >= 1, "{key}: {:?}", e.model_lane_report());
        assert_eq!(rep(&e, "verdict:buy"), 0, "never executed: {:?}", e.model_lane_report());
        assert!(rep(&e, "notrade:probe_sl_off_contract") >= 1);
    }
}

#[test]
fn a_valid_skip_is_a_skip() {
    let (e, _, _) = run("DECISION: SKIP\nREASON: sells dominate\nRISK: none material", Some("stop"), Some(20));
    assert!(rep(&e, "probe_sl:valid:SKIP") >= 1, "{:?}", e.model_lane_report());
    assert_eq!(rep(&e, "verdict:buy"), 0);
}
