//! R5 D3: a mint -> curve binding validated by a pump.fun buy/sell instruction is recorded DURABLY (curve_binding log,
//! sealed with the capture under capture seal schema 2) and restored on restart, so a market seen before the restart
//! only through trade prints keeps its curve routing. Older (schema 1) seals stay readable and say NOT RECORDED.
//! Nothing after the restart instant, outside the sealed prefix, or failing curve == PDA(mint) is restored.

use std::path::{Path, PathBuf};

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::{AppEvent, TradeVenue};
use pump_quant_app::model_authority::ModelSource;
use pump_quant_domain::ids::Mint as DomainMint;
use pump_quant_inference::InferenceError;
use pump_quant_junction::capture_rehydrate::{
    rehydrate_from_capture, restore_curve_bindings, set_aside_unsealed_binding_tail, CaptureSealer,
    RehydrateOutcome, CAPTURE_SEAL_SCHEMA,
};
use pump_quant_junction::curve_binding::{
    curve_pda, encode, BindingRestore, BindingSource, BindingWriter, CurveBinding,
};
use pump_quant_junction::event_stream::EventStreamWriter;

struct Skip;
impl ModelSource for Skip {
    fn complete(&self, _s: &str, _u: &str) -> Result<String, InferenceError> {
        Ok("DECISION: SKIP\nSIZE: NONE\nINVALIDATION: none\nEVIDENCE: x".into())
    }
}

const LAUNCHED: [u8; 32] = [0x81; 32];
const PRINT_ONLY: [u8; 32] = [0x82; 32];
const LATE: [u8; 32] = [0x83; 32];
const CREATOR: [u8; 32] = [0x84; 32];
const T0: i64 = 1_800_000_000_000;

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pq_cbind_{tag}_{}", std::process::id()));
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

fn print(mint: [u8; 32], i: u32) -> AppEvent {
    let mut w = [0u8; 32];
    w[0] = (i % 40) as u8 + 1;
    AppEvent::MarketTrade {
        mint: DomainMint::from_bytes(mint),
        price_fp: 22_000 + i128::from(i),
        quote_lamports: 500_000_000,
        liquidity_lamports: 37_900_000_000,
        signed_base: 30_000_000_000,
        buyer_entity: 1 + u64::from(i),
        age_slots: 30,
        recv_unix_ms: Some(T0 + 1_000 + i64::from(i) * 2_000),
        trader_pubkey: Some(w),
        slot: Some(1_000 + u64::from(i)),
        fee_lamports: Some(60_000),
        cu_consumed: Some(90_000),
        venue: Some(TradeVenue::PumpFun),
        event_id: None,
        feature: None,
    }
}

fn binding(mint: [u8; 32], src: BindingSource, recv: i64) -> CurveBinding {
    let mut sig = [0u8; 64];
    sig[0] = mint[0];
    sig[63] = 7;
    CurveBinding::from_instruction(mint, src, sig, 5_000, Some(recv))
}

struct Files {
    held: PathBuf,
    cap: PathBuf,
    seal: PathBuf,
    binds: PathBuf,
}

/// First process: a launched market (launch + prints) and a PRINT-ONLY market whose curve routing was installed by a
/// pump.fun SELL instruction (the D3 path: its print is filtered, only the event-path trade is captured). The binding
/// is recorded when it is installed. After the durable barrier a third market is bound and its record is written but
/// never sealed (the crash tail). `schema2 = false` reproduces an older daemon (capture-only seal, no binding log).
fn first_process(d: &Path, schema2: bool) -> Files {
    let f = Files {
        held: d.join("held.json"),
        cap: d.join("event_stream.jsonl"),
        seal: d.join("capture_seal.json"),
        binds: d.join("curve_bindings.jsonl"),
    };
    let mut e = engine(&f.held);
    flow_attach(&mut e, d);
    let mut w = EventStreamWriter::open(&f.cap).unwrap();
    let mut s = if schema2 {
        CaptureSealer::with_bindings(&f.cap, &f.seal, &f.binds)
    } else {
        CaptureSealer::new(&f.cap, &f.seal)
    };
    let mut bw = schema2.then(|| BindingWriter::open(&f.binds).unwrap());
    let launch = AppEvent::LaunchObserved {
        mint: DomainMint::from_bytes(LAUNCHED),
        creator: CREATOR,
        launch_unix_ms: T0,
    };
    e.tick(launch);
    w.write_event(&launch, 0).unwrap();
    for i in 0..6 {
        if i == 2 {
            if let Some(b) = bw.as_mut() {
                b.record(&binding(
                    PRINT_ONLY,
                    BindingSource::IxSell,
                    T0 + 1_000 + 2 * 2_000,
                ))
                .unwrap();
            }
        }
        let p = print(if i % 2 == 0 { LAUNCHED } else { PRINT_ONLY }, i);
        e.tick(p);
        w.write_event(&p, 0).unwrap();
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
    // Crash tail: bound after the last seal, never sealed.
    if let Some(b) = bw.as_mut() {
        b.record(&binding(LATE, BindingSource::IxBuy, T0 + 60_000))
            .unwrap();
    }
    f
}

fn restarted(held: &Path) -> Engine {
    let mut e = engine(held);
    assert!(matches!(e.model_held_restore(), Ok(Some(_))));
    flow_attach(&mut e, held.parent().unwrap());
    e
}

fn applied_curve_mints(e: &mut Engine, f: &Files) -> Vec<[u8; 32]> {
    match rehydrate_from_capture(e, &f.cap, &f.seal) {
        RehydrateOutcome::Applied { stats, .. } => stats.curve_mints.into_iter().collect(),
        o => panic!("capture must apply: {o:?}"),
    }
}

#[test]
fn an_instruction_bound_print_only_curve_is_restored_from_the_sealed_binding_log() {
    let d = dir("ok");
    let f = first_process(&d, true);
    let seal: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&f.seal).unwrap()).unwrap();
    assert_eq!(seal["schema"].as_u64(), Some(CAPTURE_SEAL_SCHEMA));
    assert_eq!(CAPTURE_SEAL_SCHEMA, 2);
    let mut e = restarted(&f.held);
    let from_capture = applied_curve_mints(&mut e, &f);
    assert!(from_capture.contains(&LAUNCHED));
    assert!(
        !from_capture.contains(&PRINT_ONLY),
        "a print alone never routes a curve (W1.1b R3b)"
    );
    match restore_curve_bindings(&e, &f.seal, &f.binds) {
        BindingRestore::Restored {
            bindings,
            after_restart_skipped,
            undated_skipped,
        } => {
            assert_eq!(bindings.len(), 1, "{bindings:?}");
            let b = &bindings[0];
            assert_eq!(b.mint, PRINT_ONLY);
            assert_eq!(b.curve, curve_pda(&PRINT_ONLY));
            assert_eq!(b.source, BindingSource::IxSell);
            assert_eq!(b.slot, 5_000);
            assert_eq!(b.signature[0], PRINT_ONLY[0]);
            assert_eq!(b.recv_ms, Some(T0 + 1_000 + 2 * 2_000));
            assert_eq!(after_restart_skipped, 0);
            assert_eq!(undated_skipped, 0);
        }
        o => panic!("must restore: {o:?}"),
    }
    // The unsealed record (bound after the last seal) is evidence, never restored; the log is the sealed prefix again.
    let side = set_aside_unsealed_binding_tail(&f.binds, &f.seal).expect("tail set aside");
    assert!(std::fs::read_to_string(&side)
        .unwrap()
        .contains(&bs58::encode(LATE).into_string()));
    assert_eq!(
        std::fs::metadata(&f.binds).unwrap().len(),
        seal["bindings_bytes"].as_u64().unwrap()
    );
}

#[test]
fn a_binding_received_after_the_restart_instant_is_never_restored() {
    let d = dir("future");
    let f = first_process(&d, true);
    // Rewrite the sealed log with a record dated after the restart instant (seal re-made over the same bytes).
    let mut e = restarted(&f.held);
    let restart = e.model_replay_through_ms();
    let _ = applied_curve_mints(&mut e, &f);
    let future = encode(&binding(LATE, BindingSource::IxBuy, restart + 1));
    let past = encode(&binding(
        PRINT_ONLY,
        BindingSource::IxSell,
        T0 + 1_000 + 2 * 2_000,
    ));
    std::fs::write(&f.binds, format!("{past}\n{future}\n")).unwrap();
    let mut s = CaptureSealer::with_bindings(&f.cap, &f.seal, &f.binds);
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&f.seal).unwrap()).unwrap();
    s.seal(
        v["lineage"].as_str().unwrap(),
        v["held_generation"].as_u64().unwrap(),
        v["through_ms"].as_i64().unwrap(),
    )
    .unwrap();
    match restore_curve_bindings(&e, &f.seal, &f.binds) {
        BindingRestore::Restored {
            bindings,
            after_restart_skipped,
            ..
        } => {
            assert_eq!(
                bindings.iter().map(|b| b.mint).collect::<Vec<_>>(),
                vec![PRINT_ONLY]
            );
            assert_eq!(after_restart_skipped, 1, "future binding skipped by count");
        }
        o => panic!("must restore: {o:?}"),
    }
}

#[test]
fn an_undated_binding_is_skipped_by_count_never_assumed_before_the_restart() {
    let d = dir("undated");
    let f = first_process(&d, true);
    let mut e = restarted(&f.held);
    let _ = applied_curve_mints(&mut e, &f);
    let mut u = binding(PRINT_ONLY, BindingSource::IxSell, 0);
    u.recv_ms = None;
    std::fs::write(&f.binds, format!("{}\n", encode(&u))).unwrap();
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&f.seal).unwrap()).unwrap();
    let mut s = CaptureSealer::with_bindings(&f.cap, &f.seal, &f.binds);
    s.seal(
        v["lineage"].as_str().unwrap(),
        v["held_generation"].as_u64().unwrap(),
        v["through_ms"].as_i64().unwrap(),
    )
    .unwrap();
    match restore_curve_bindings(&e, &f.seal, &f.binds) {
        BindingRestore::Restored {
            bindings,
            undated_skipped,
            ..
        } => {
            assert!(bindings.is_empty(), "{bindings:?}");
            assert_eq!(undated_skipped, 1);
        }
        o => panic!("must restore: {o:?}"),
    }
}

#[test]
fn a_tampered_binding_log_is_refused_by_name_and_restores_nothing() {
    let d = dir("tamper");
    let f = first_process(&d, true);
    let mut b = std::fs::read(&f.binds).unwrap();
    b[12] ^= 1;
    std::fs::write(&f.binds, b).unwrap();
    let mut e = restarted(&f.held);
    let _ = applied_curve_mints(&mut e, &f);
    assert_eq!(
        restore_curve_bindings(&e, &f.seal, &f.binds),
        BindingRestore::Refused("curve_bindings_sha256_mismatch".into())
    );
}

#[test]
fn a_forged_curve_account_is_refused_even_under_a_valid_seal() {
    let d = dir("forged");
    let f = first_process(&d, true);
    let good = std::fs::read_to_string(&f.binds).unwrap();
    let first = good.lines().next().unwrap().to_string();
    let forged = first.replace(
        &bs58::encode(curve_pda(&PRINT_ONLY)).into_string(),
        &bs58::encode(curve_pda(&LAUNCHED)).into_string(),
    );
    assert_ne!(forged, first);
    std::fs::write(&f.binds, format!("{forged}\n")).unwrap();
    let mut e = restarted(&f.held);
    let _ = applied_curve_mints(&mut e, &f);
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&f.seal).unwrap()).unwrap();
    let mut s = CaptureSealer::with_bindings(&f.cap, &f.seal, &f.binds);
    s.seal(
        v["lineage"].as_str().unwrap(),
        v["held_generation"].as_u64().unwrap(),
        v["through_ms"].as_i64().unwrap(),
    )
    .unwrap();
    match restore_curve_bindings(&e, &f.seal, &f.binds) {
        BindingRestore::Refused(why) => {
            assert!(why.ends_with("curve_binding_pda_mismatch"), "{why}");
        }
        o => panic!("must refuse: {o:?}"),
    }
}

#[test]
fn an_older_schema_1_seal_stays_readable_and_reports_bindings_not_recorded() {
    let d = dir("schema1");
    let f = first_process(&d, false);
    let seal: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&f.seal).unwrap()).unwrap();
    assert_eq!(seal["schema"].as_u64(), Some(1));
    assert!(seal.get("bindings_bytes").is_none());
    let mut e = restarted(&f.held);
    let from_capture = applied_curve_mints(&mut e, &f);
    assert!(
        from_capture.contains(&LAUNCHED),
        "schema 1 capture restores as before"
    );
    assert_eq!(
        restore_curve_bindings(&e, &f.seal, &f.binds),
        BindingRestore::NotRecordedSchema1
    );
}

#[test]
fn an_unknown_future_seal_schema_is_refused_by_name() {
    let d = dir("schema9");
    let f = first_process(&d, true);
    let s = std::fs::read_to_string(&f.seal).unwrap();
    std::fs::write(&f.seal, s.replace("\"schema\":2", "\"schema\":9")).unwrap();
    let e = restarted(&f.held);
    match restore_curve_bindings(&e, &f.seal, &f.binds) {
        BindingRestore::Refused(why) => {
            assert!(why.starts_with("capture_seal_schema_unsupported"), "{why}")
        }
        o => panic!("must refuse: {o:?}"),
    }
}
