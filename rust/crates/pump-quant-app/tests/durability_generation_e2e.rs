//! M1 durability: the financial ledger and the flow history share a durable GENERATION boundary.
use pump_quant_app::config::Config;
use pump_quant_app::engine::model_admit::FlowAttach;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::flow_checkpoint::Provenance;
use pump_quant_market_state::flow_reducer::FlowParams;
use std::path::PathBuf;
use std::time::Duration;

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pq_gen_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}
fn prov() -> Provenance {
    Provenance {
        seed_source: "t".into(),
        seed_sha256: String::new(),
        seed_before_ms: 0,
        producer: "t".into(),
    }
}
fn engine(held: &std::path::Path) -> Engine {
    let mut e = Engine::new(Config::dev_portable(), RunMode::Paper);
    e.model_held_attach(held);
    e
}

/// Run an engine for a while, publishing books and history, return the dir and the final generation seen.
fn run_and_publish(tag: &str) -> (PathBuf, PathBuf, PathBuf, u64) {
    let d = dir(tag);
    let (held, flow) = (d.join("held.json"), d.join("flow.ckpt"));
    let mut e = engine(&held);
    assert!(matches!(
        e.model_flow_attach(&flow, FlowParams::default(), prov(), 0),
        FlowAttach::Fresh
    ));
    // Three durable book generations, with a history snapshot after each.
    for _ in 0..3 {
        assert!(e.model_held_persist_now());
        assert!(e.model_flow_flush(Duration::from_secs(5)));
    }
    let g = pump_quant_app::held_state::HeldLedger::read(&held)
        .unwrap()
        .generation;
    (d, held, flow, g)
}

#[test]
fn the_ledger_generation_advances_by_one_per_durable_write() {
    let (_d, _h, _f, g) = run_and_publish("adv");
    assert_eq!(g, 3);
}

#[test]
fn a_history_that_has_seen_newer_books_than_the_ledger_refuses_by_name_and_is_left_untouched() {
    let (d, held, flow, _g) = run_and_publish("ahead");
    let before = std::fs::read(&flow).unwrap();
    // The books were deleted: a fresh start with no ledger next to a history that saw generation 3.
    std::fs::remove_file(&held).unwrap();
    let mut e2 = engine(&held);
    assert!(matches!(e2.model_held_restore(), Ok(None)));
    match e2.model_flow_attach(&flow, FlowParams::default(), prov(), 0) {
        FlowAttach::Untrusted(w) => assert_eq!(w, "flow_ahead_of_books"),
        o => panic!("must refuse: {o:?}"),
    }
    assert_eq!(std::fs::read(&flow).unwrap(), before, "evidence untouched");
    assert!(e2
        .model_lane_report()
        .contains_key("flow_history:untrusted:flow_ahead_of_books"));
    let _ = d;
}

#[test]
fn an_older_copy_of_the_ledger_swapped_in_is_the_same_refusal() {
    let (d, held, flow, _g) = run_and_publish("older");
    // Capture generation 3, advance to 5 (history now saw 5), then roll the ledger back to a generation-3 copy.
    let old = std::fs::read(&held).unwrap();
    let mut e = engine(&held);
    assert!(e.model_held_restore().is_ok());
    assert!(matches!(
        e.model_flow_attach(&flow, FlowParams::default(), prov(), 0),
        FlowAttach::Restored { .. }
    ));
    for _ in 0..2 {
        assert!(e.model_held_persist_now());
        assert!(e.model_flow_flush(Duration::from_secs(5)));
    }
    drop(e);
    std::fs::write(&held, &old).unwrap();
    let mut e2 = engine(&held);
    let _ = e2.model_held_restore();
    assert!(matches!(
        e2.model_flow_attach(&flow, FlowParams::default(), prov(), 0),
        FlowAttach::Untrusted("flow_ahead_of_books")
    ));
    let _ = d;
}

#[test]
fn matching_generations_attach_normally_and_the_history_may_lag_the_books() {
    let (d, held, flow, g) = run_and_publish("match");
    let mut e = engine(&held);
    assert!(matches!(e.model_held_restore(), Ok(Some(_))));
    assert!(matches!(
        e.model_flow_attach(&flow, FlowParams::default(), prov(), 0),
        FlowAttach::Restored { .. }
    ));
    // Books advance past the history (a crash between the two publications): still valid, history is just older.
    for _ in 0..2 {
        assert!(e.model_held_persist_now());
    }
    drop(e);
    let mut e2 = engine(&held);
    assert!(matches!(e2.model_held_restore(), Ok(Some(_))));
    assert_eq!(e2.model_held_generation(), g + 2);
    assert!(matches!(
        e2.model_flow_attach(&flow, FlowParams::default(), prov(), 0),
        FlowAttach::Restored { .. }
    ));
    let _ = d;
}

#[test]
fn a_torn_temp_file_never_replaces_either_record() {
    let (d, held, flow, g) = run_and_publish("torn");
    let (hb, fb) = (std::fs::read(&held).unwrap(), std::fs::read(&flow).unwrap());
    // A crash mid-publication leaves a partial temp beside each record.
    std::fs::write(held.with_extension("tmp"), &hb[..hb.len() / 2]).unwrap();
    std::fs::write(flow.with_extension("tmp"), &fb[..fb.len() / 2]).unwrap();
    let mut e = engine(&held);
    let rep = e.model_held_restore();
    assert!(matches!(rep, Ok(Some(_))), "{rep:?}");
    assert_eq!(e.model_held_generation(), g);
    assert!(matches!(
        e.model_flow_attach(&flow, FlowParams::default(), prov(), 0),
        FlowAttach::Restored { .. }
    ));
    assert_eq!(std::fs::read(&held).unwrap(), hb);
    assert_eq!(std::fs::read(&flow).unwrap(), fb);
    let _ = d;
}

#[test]
fn a_ledger_written_before_generations_existed_reads_as_generation_zero_and_cannot_be_ahead() {
    let d = dir("legacy");
    let (held, flow) = (d.join("held.json"), d.join("flow.ckpt"));
    let mut e = engine(&held);
    e.model_flow_attach(&flow, FlowParams::default(), prov(), 0);
    assert!(e.model_flow_flush(Duration::from_secs(5)));
    let h = pump_quant_app::flow_checkpoint::load(FlowParams::default(), &flow);
    match h {
        pump_quant_app::flow_checkpoint::Load::Loaded(h) => {
            assert_eq!(h.meta.held_gen_seen, Some(0))
        }
        _ => panic!("loads"),
    }
}

/// LINEAGE: generations are counters. A history from an UNRELATED ledger lineage whose counter happens to be lower
/// must NOT be taken as compatible with the restored books.
#[test]
fn a_history_from_an_unrelated_ledger_lineage_is_refused_even_when_its_generation_is_lower() {
    // Lineage A: history snapshot after 1 book write.
    let da = dir("lin_a");
    let (ha, fa) = (da.join("held.json"), da.join("flow.ckpt"));
    let mut a = engine(&ha);
    a.model_flow_attach(&fa, FlowParams::default(), prov(), 0);
    assert!(a.model_held_persist_now());
    assert!(a.model_flow_flush(Duration::from_secs(5)));
    drop(a);
    // Lineage B: an independent ledger with 3 generations.
    let (_db, hb, _fb, gb) = run_and_publish("lin_b");
    assert_eq!(gb, 3);
    // B's books next to A's history (held_gen_seen 1 <= 3): unrelated, must refuse by name, file untouched.
    let before = std::fs::read(&fa).unwrap();
    let mut e = engine(&hb);
    assert!(matches!(e.model_held_restore(), Ok(Some(_))));
    let r = e.model_flow_attach(&fa, FlowParams::default(), prov(), 0);
    assert!(
        matches!(r, FlowAttach::Untrusted("flow_lineage_mismatch")),
        "{r:?}"
    );
    assert_eq!(std::fs::read(&fa).unwrap(), before, "untouched");
}

/// Same lineage, history behind the books: still attaches (the accepted overlap-replay case).
#[test]
fn the_same_lineage_with_a_lagging_history_still_attaches() {
    let (_d, held, flow, _g) = run_and_publish("lin_same");
    let mut e = engine(&held);
    assert!(matches!(e.model_held_restore(), Ok(Some(_))));
    for _ in 0..2 {
        assert!(e.model_held_persist_now());
    }
    drop(e);
    let mut e2 = engine(&held);
    assert!(matches!(e2.model_held_restore(), Ok(Some(_))));
    assert!(matches!(
        e2.model_flow_attach(&flow, FlowParams::default(), prov(), 0),
        FlowAttach::Restored { .. }
    ));
}

/// A history that saw books but names no lineage (written before lineages existed) is UNBOUND next to restored books.
#[test]
fn a_history_that_saw_books_without_a_lineage_is_refused_as_unbound() {
    let (_d, held, flow, _g) = run_and_publish("lin_unbound");
    let raw = std::fs::read(&flow).unwrap();
    let nl = raw.iter().position(|b| *b == b'\n').unwrap();
    let mut hdr: serde_json::Value = serde_json::from_slice(&raw[..nl]).unwrap();
    assert!(hdr["held_gen_seen"].as_u64().unwrap() > 0);
    assert!(hdr
        .as_object_mut()
        .unwrap()
        .remove("held_lineage_seen")
        .is_some());
    let mut out = serde_json::to_vec(&hdr).unwrap();
    out.push(b'\n');
    out.extend_from_slice(&raw[nl + 1..]);
    std::fs::write(&flow, &out).unwrap();
    let mut e = engine(&held);
    assert!(matches!(e.model_held_restore(), Ok(Some(_))));
    let r = e.model_flow_attach(&flow, FlowParams::default(), prov(), 0);
    assert!(
        matches!(r, FlowAttach::Untrusted("flow_lineage_unbound")),
        "{r:?}"
    );
    assert_eq!(std::fs::read(&flow).unwrap(), out);
}

/// The lineage is created once and inherited across restarts (never re-created by a restored engine).
#[test]
fn the_lineage_is_created_once_and_inherited_across_restarts() {
    let (_d, held, _flow, _g) = run_and_publish("lin_inh");
    let l0 = pump_quant_app::held_state::HeldLedger::read(&held)
        .unwrap()
        .lineage;
    assert_eq!(l0.len(), 32);
    let mut e = engine(&held);
    assert!(matches!(e.model_held_restore(), Ok(Some(_))));
    assert!(e.model_held_persist_now());
    let l1 = pump_quant_app::held_state::HeldLedger::read(&held)
        .unwrap()
        .lineage;
    assert_eq!(l0, l1);
}
