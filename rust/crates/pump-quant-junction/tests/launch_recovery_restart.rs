//! RESTART launch recovery through the daemon's own library code (`capture_rehydrate`): a capture sealed to the held
//! ledger (lineage + generation + decision clock + sha256) rebuilds the per-market evidence a restarted engine needs;
//! every failure to prove the capture is a NAMED refusal that applies nothing and reports the lost coverage.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, TradeVenue};
use pump_quant_app::model_authority::ModelSource;
use pump_quant_domain::ids::Mint as DomainMint;
use pump_quant_inference::InferenceError;
use pump_quant_junction::capture_rehydrate::{
    rehydrate_from_capture, set_aside_unsealed_tail, CaptureSealer, RehydrateOutcome,
};
use pump_quant_junction::event_stream::EventStreamWriter;

struct Skip;
impl ModelSource for Skip {
    fn complete(&self, _s: &str, _u: &str) -> Result<String, InferenceError> {
        Ok("DECISION: SKIP\nSIZE: NONE\nINVALIDATION: none\nEVIDENCE: x".into())
    }
}

const MINT: [u8; 32] = [0x71; 32];
const CREATOR: [u8; 32] = [0x72; 32];
const T0: i64 = 1_800_000_000_000;

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pq_caprh_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn engine(held: &Path) -> Engine {
    let mut c = Config::dev_portable();
    c.bankroll_initial_lamports = 2_000_000_000;
    let mut e = Engine::new(c, RunMode::Paper);
    e.enable_paper_model(Skip);
    e.model_held_attach(held);
    e
}

fn flow_attach(e: &mut Engine, d: &Path) {
    let _ = e.model_flow_attach(
        &d.join("flow.ckpt"),
        pump_quant_market_state::flow_reducer::FlowParams::default(),
        pump_quant_app::flow_checkpoint::Provenance {
            seed_source: "cold_start:test".into(),
            seed_sha256: "none".into(),
            seed_before_ms: 0,
            producer: "t".into(),
        },
        T0,
    );
}

fn trade(i: u32) -> AppEvent {
    let mut w = [0u8; 32];
    w[0] = (i % 40) as u8 + 1;
    w[31] = 1;
    AppEvent::MarketTrade {
        mint: DomainMint::from_bytes(MINT),
        price_fp: 22_000 + i128::from(i),
        quote_lamports: 500_000_000 + u64::from(i),
        liquidity_lamports: 37_900_000_000,
        signed_base: if i % 3 != 0 {
            30_000_000_000
        } else {
            -30_000_000_000
        },
        buyer_entity: 1 + u64::from(i),
        age_slots: 30,
        recv_unix_ms: Some(T0 + 1_000 + i64::from(i) * 2_000),
        trader_pubkey: Some(w),
        slot: Some(1_000 + u64::from(i)),
        fee_lamports: Some(60_000 + u64::from(i) * 100),
        cu_consumed: Some(90_000 + u64::from(i)),
        venue: Some(TradeVenue::PumpFun),
        event_id: None,
        feature: None,
    }
}

fn curve(t: i64) -> AppEvent {
    AppEvent::CurveObserved {
        mint: DomainMint::from_bytes(MINT),
        v_sol_lamports: 37_900_000_000,
        v_tokens: 849_000_000_000_000,
        real_sol_lamports: 7_900_000_000,
        real_tokens: 569_000_000_000_000,
        recv_unix_ms: Some(t),
        slot: 2_000,
    }
}

fn launch() -> AppEvent {
    AppEvent::LaunchObserved {
        mint: DomainMint::from_bytes(MINT),
        creator: CREATOR,
        launch_unix_ms: T0,
    }
}

/// Run the first process: launch + 26 prints captured and sealed to a durable held ledger. Returns paths.
fn first_process(d: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let held = d.join("held.json");
    let cap = d.join("event_stream.jsonl");
    let seal = d.join("capture_seal.json");
    let mut e = engine(&held);
    flow_attach(&mut e, d);
    let mut w = EventStreamWriter::open(&cap).unwrap();
    let mut s = CaptureSealer::new(&cap, &seal);
    for ev in std::iter::once(launch()).chain((0..26).map(trade)) {
        e.tick(ev);
        w.write_event(&ev, 0).unwrap();
    }
    assert!(e.model_held_persist_now());
    assert!(e.model_flow_flush(std::time::Duration::from_secs(5)));
    w.flush().unwrap();
    s.seal(
        e.model_held_lineage(),
        e.model_held_generation(),
        e.model_held_through_ms_now(),
    )
    .unwrap();
    // An unsealed tail (written after the last seal, before the crash).
    w.write_event(&trade(26), 0).unwrap();
    w.flush().unwrap();
    (held, cap, seal)
}

fn restarted(held: &Path) -> Engine {
    let mut e = engine(held);
    assert!(matches!(e.model_held_restore(), Ok(Some(_))));
    flow_attach(&mut e, held.parent().unwrap());
    e
}

/// The uninterrupted reference prompt at the decision clock.
fn reference_prompt() -> String {
    let d = dir("ref");
    let mut e = engine(&d.join("held.json"));
    flow_attach(&mut e, &d);
    e.tick(launch());
    for i in 0..40 {
        e.tick(trade(i));
    }
    e.tick(curve(T0 + 90_000));
    e.model_entry_snapshot(&MINT, T0 + 90_000)
        .expect("reference snapshot")
        .user_prompt
}

fn continue_live(e: &mut Engine) {
    // The live feed resumes with an overlap from print 20 (re-delivered prints are deduplicated).
    e.tick(launch());
    for i in 20..40 {
        e.tick(trade(i));
    }
    e.tick(curve(T0 + 90_000));
}

#[test]
fn a_sealed_capture_restores_the_uninterrupted_evidence_after_restart() {
    let d = dir("ok");
    let (held, cap, seal) = first_process(&d);
    let mut e = restarted(&held);
    match rehydrate_from_capture(&mut e, &cap, &seal) {
        RehydrateOutcome::Applied { stats, lost, .. } => {
            assert_eq!(stats.launches, 1);
            assert_eq!(stats.prints, 26, "the unsealed tail is never replayed");
            assert!(lost.is_empty(), "covered from launch: {lost:?}");
        }
        o => panic!("must apply: {o:?}"),
    }
    // The unsealed tail is moved aside as evidence; the capture is the sealed prefix again.
    let sealed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&seal).unwrap()).unwrap();
    let side = set_aside_unsealed_tail(&cap, &seal).expect("tail set aside");
    assert!(std::fs::metadata(&side).unwrap().len() > 0);
    assert_eq!(
        std::fs::metadata(&cap).unwrap().len(),
        sealed["capture_bytes"].as_u64().unwrap()
    );
    continue_live(&mut e);
    let got = e.model_entry_snapshot(&MINT, T0 + 90_000).expect("served");
    assert_eq!(got.user_prompt, reference_prompt(), "restart equivalence");
    assert_eq!(
        e.model_creator_dev_history(&MINT).creator_past_launches,
        Some(0),
        "re-delivered launch never double-counts"
    );
}

fn refused(e: &mut Engine, cap: &Path, seal: &Path, want: &str) {
    match rehydrate_from_capture(e, cap, seal) {
        RehydrateOutcome::Refused { why, lost, .. } => {
            assert!(why.starts_with(want), "{why} vs {want}");
            let _ = lost;
        }
        o => panic!("must refuse {want}: {o:?}"),
    }
    continue_live(e);
    let r = e.model_entry_snapshot(&MINT, T0 + 90_000).unwrap_err();
    assert_eq!(r.as_str(), "join_tape_truncated_at_restart");
    assert_eq!(
        e.model_restart_lost_coverage(),
        vec![(MINT, T0)],
        "lost coverage reported"
    );
}

#[test]
fn a_tampered_capture_is_refused_by_name_and_the_market_reports_lost_coverage() {
    let d = dir("tamper");
    let (held, cap, seal) = first_process(&d);
    let mut b = std::fs::read(&cap).unwrap();
    b[10] ^= 1;
    std::fs::write(&cap, b).unwrap();
    let mut e = restarted(&held);
    refused(&mut e, &cap, &seal, "capture_sha256_mismatch");
}

#[test]
fn a_seal_of_another_ledger_lineage_is_refused_by_name() {
    let d = dir("lineage");
    let (held, cap, seal) = first_process(&d);
    let s = std::fs::read_to_string(&seal).unwrap();
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    let l = v["lineage"].as_str().unwrap().to_string();
    std::fs::write(&seal, s.replace(&l, "ffffffffffffffffffffffffffffffff")).unwrap();
    let mut e = restarted(&held);
    refused(&mut e, &cap, &seal, "capture_seal_lineage_mismatch");
}

/// A seal AHEAD of the restored barrier (the capture was sealed after the held ledger's last durable write): the
/// capture lines after the restart instant must NOT be applied at startup (a launch or reserve of the future would be
/// known before it was available); the live feed re-delivers them.
#[test]
fn a_seal_ahead_of_the_restored_barrier_applies_nothing_after_the_restart_instant() {
    let d = dir("ahead");
    let held = d.join("held.json");
    let cap = d.join("event_stream.jsonl");
    let seal = d.join("capture_seal.json");
    let other = [0x73; 32];
    {
        let mut e = engine(&held);
        flow_attach(&mut e, &d);
        let mut w = EventStreamWriter::open(&cap).unwrap();
        let mut s = CaptureSealer::new(&cap, &seal);
        for ev in std::iter::once(launch()).chain((0..21).map(trade)) {
            e.tick(ev);
            w.write_event(&ev, 0).unwrap();
        }
        assert!(e.model_held_persist_now());
        assert!(e.model_flow_flush(std::time::Duration::from_secs(5)));
        let later = AppEvent::LaunchObserved {
            mint: DomainMint::from_bytes(other),
            creator: CREATOR,
            launch_unix_ms: T0 + 1_000 + 23 * 2_000 + 500,
        };
        for ev in (21..23)
            .map(trade)
            .chain(std::iter::once(later))
            .chain((23..26).map(trade))
        {
            e.tick(ev);
            w.write_event(&ev, 0).unwrap();
        }
        w.flush().unwrap();
        s.seal(
            e.model_held_lineage(),
            e.model_held_generation(),
            e.model_held_through_ms_now(),
        )
        .unwrap();
    }
    let mut e = restarted(&held);
    let restart = e.model_replay_through_ms();
    assert!(
        restart < T0 + 1_000 + 21 * 2_000,
        "barrier at print 20: {restart}"
    );
    match rehydrate_from_capture(&mut e, &cap, &seal) {
        RehydrateOutcome::Applied { stats, lost, .. } => {
            assert_eq!(
                stats.launches, 1,
                "the later launch is not known before its time"
            );
            assert_eq!(stats.prints, 21);
            assert_eq!(stats.after_restart_skipped, 6);
            assert!(lost.is_empty(), "{lost:?}");
        }
        o => panic!("must apply: {o:?}"),
    }
    assert!(
        !e.model_launch_feed_observed(&other),
        "future launch applied at startup"
    );
    continue_live(&mut e);
    let got = e.model_entry_snapshot(&MINT, T0 + 90_000).expect("served");
    assert_eq!(got.user_prompt, reference_prompt(), "restart equivalence");
}

#[test]
fn a_seal_behind_the_restored_barrier_is_refused_by_name() {
    let d = dir("behind");
    let (held, cap, seal) = first_process(&d);
    let s = std::fs::read_to_string(&seal).unwrap();
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    let t = v["through_ms"].as_i64().unwrap();
    std::fs::write(
        &seal,
        s.replace(
            &format!("\"through_ms\":{t}"),
            &format!("\"through_ms\":{}", t - 1),
        ),
    )
    .unwrap();
    let mut e = restarted(&held);
    refused(&mut e, &cap, &seal, "capture_seal_behind_restart");
}

#[test]
fn an_absent_seal_is_refused_by_name() {
    let d = dir("absent");
    let (held, cap, seal) = first_process(&d);
    std::fs::remove_file(&seal).unwrap();
    let mut e = restarted(&held);
    refused(&mut e, &cap, &seal, "capture_seal_absent");
}

const POOL_MINT: [u8; 32] = [0x74; 32];
const LAUNCH_ONLY: [u8; 32] = [0x76; 32];
const CURVE_ONLY: [u8; 32] = [0x77; 32];
const PRINT_ONLY: [u8; 32] = [0x78; 32];

/// One pump.fun curve market per evidence kind: a launch alone, a curve reserve observation alone, a curve print alone.
fn per_kind_curve_evidence() -> Vec<AppEvent> {
    let mut p = trade(3);
    if let AppEvent::MarketTrade { mint, .. } = &mut p {
        *mint = DomainMint::from_bytes(PRINT_ONLY);
    }
    let mut c = curve(T0 + 20_000);
    if let AppEvent::CurveObserved { mint, .. } = &mut c {
        *mint = DomainMint::from_bytes(CURVE_ONLY);
    }
    vec![
        AppEvent::LaunchObserved {
            mint: DomainMint::from_bytes(LAUNCH_ONLY),
            creator: CREATOR,
            launch_unix_ms: T0 + 500,
        },
        p,
        c,
    ]
}

fn pool_swap(t: i64) -> AppEvent {
    AppEvent::AmmSwap {
        mint: DomainMint::from_bytes(POOL_MINT),
        pool: [0x75; 32],
        pool_is_canonical: true,
        quote_is_wsol: true,
        token_reserve_pre: 200_000_000_000_000,
        quote_reserve_pre: 85_000_000_000,
        fee_bps: Some(125),
        fee_parts: Some((2, 93, 30)),
        virtual_quote: Some(85_000_000_000),
        cashback: pump_quant_protocol::pumpswap_event::CashbackField::NotRecorded,
        is_buy: true,
        token_amount: 1_000_000_000,
        quote_lamports: 425_000,
        trader: [7u8; 32],
        fee_lamports: Some(5_000),
        cu_consumed: Some(90_000),
        recv_unix_ms: Some(t),
        slot: 3_000,
    }
}

fn market_lines(e: &Engine) -> Vec<String> {
    e.barrier_state_lines()
        .into_iter()
        .filter(|l| l.starts_with("market "))
        .collect()
}

/// W1.1b defects D1/D2 (daemon pair wR5r0 vs wR5b2 on binary 823b156f): a canonical WSOL pool market seen only through
/// `AmmSwap` before the restart must be REGISTERED after rehydration exactly as in the uninterrupted process, and the
/// rehydration must name the pump.fun CURVE markets it proved were tracked (the daemon re-installs their curve-account
/// routing from that set) and never a pool-only market.
#[test]
fn a_rehydrated_pool_market_is_registered_and_curve_markets_are_named_for_routing() {
    let d = dir("poolreg");
    let held = d.join("held.json");
    let cap = d.join("event_stream.jsonl");
    let seal = d.join("capture_seal.json");
    let evs: Vec<AppEvent> = std::iter::once(launch())
        .chain((0..10).map(trade))
        .chain(per_kind_curve_evidence())
        .chain(std::iter::once(pool_swap(T0 + 25_000)))
        // A scheduler pass examines the queued markets, so they are NOT dirty at the durable write (as the 179
        // pool markets of the daemon pair were): only the rehydration can re-register them.
        .chain(std::iter::once(AppEvent::Tick))
        .collect();
    let reference = {
        let r = dir("poolreg_ref");
        let mut e = engine(&r.join("held.json"));
        flow_attach(&mut e, &r);
        for ev in &evs {
            e.tick(*ev);
        }
        market_lines(&e)
    };
    assert_eq!(
        reference.len(),
        5,
        "uninterrupted registers the curve markets and the pool market: {reference:?}"
    );
    let pool_hex: String = POOL_MINT
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect();
    assert!(
        reference
            .iter()
            .any(|l| l.starts_with(&format!("market {pool_hex} ")) && l.ends_with("dirty=false")),
        "the pool market is registered but not dirty at the durable write: {reference:?}"
    );
    {
        let mut e = engine(&held);
        flow_attach(&mut e, &d);
        let mut w = EventStreamWriter::open(&cap).unwrap();
        let mut s = CaptureSealer::new(&cap, &seal);
        for ev in &evs {
            e.tick(*ev);
            w.write_event(ev, 0).unwrap();
        }
        assert!(e.model_held_persist_now());
        assert!(e.model_flow_flush(std::time::Duration::from_secs(5)));
        w.flush().unwrap();
        s.seal(
            e.model_held_lineage(),
            e.model_held_generation(),
            e.model_held_through_ms_now(),
        )
        .unwrap();
    }
    let mut e = restarted(&held);
    match rehydrate_from_capture(&mut e, &cap, &seal) {
        RehydrateOutcome::Applied { stats, .. } => {
            for (m, why) in [
                (MINT, "launch + prints"),
                (LAUNCH_ONLY, "launch only"),
                (CURVE_ONLY, "curve reserve only"),
            ] {
                assert!(
                    stats.curve_mints.contains(&m),
                    "curve market ({why}) named for routing"
                );
            }
            assert!(
                !stats.curve_mints.contains(&POOL_MINT),
                "a pool-only market is never routed as a curve"
            );
            assert!(
                !stats.curve_mints.contains(&PRINT_ONLY),
                "a curve print alone does not prove the previous process routed the curve account"
            );
            assert_eq!(stats.curve_mints.len(), 3);
        }
        o => panic!("must apply: {o:?}"),
    }
    assert_eq!(
        market_lines(&e),
        reference,
        "restart registry == uninterrupted registry"
    );
}

#[test]
fn a_clean_start_never_rehydrates() {
    let d = dir("clean");
    let (_, cap, seal) = first_process(&d);
    let mut e = engine(&d.join("other_held.json"));
    assert!(matches!(e.model_held_restore(), Ok(None)));
    assert!(matches!(
        rehydrate_from_capture(&mut e, &cap, &seal),
        RehydrateOutcome::NotARestart
    ));
    let _ = Arc::new(());
}
