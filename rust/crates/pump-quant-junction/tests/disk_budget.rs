//! Item 2 (m1acc follow-up): disk budget over EVERY required write destination, grouped by filesystem; UNKNOWN is
//! never PASS; runtime monitoring with time-to-floor and the exit reserve. Mutation-checked
//! (proc/m1accR1/mutants_item2.out).

use pump_quant_junction::disk_budget::{
    disk_table, evaluate_runtime, required_destinations, Dest, DestKind, DiskMonitor, DiskVerdict,
    EXIT_RESERVE_BYTES, EXIT_RESERVE_SECS, NEED_CHECKPOINT, NEED_HANDOFF, NEED_HELD, NEED_JOURNALS,
    NEED_LOGS, NEED_SAFETY,
};
use std::path::{Path, PathBuf};

const GB: u64 = 1_000_000_000;

fn d(p: &str, need: u64) -> Dest {
    Dest {
        kind: DestKind::Journal,
        path: PathBuf::from(p),
        need,
    }
}

/// Fake probe: `/a/...` on dev 1 (100 GB free), `/b/...` on dev 2 (30 GB free), `/x/...` unmeasurable.
fn probe(p: &Path) -> Option<(u64, u64)> {
    let s = p.to_str().unwrap();
    if s.starts_with("/a/") {
        Some((1, 100 * GB))
    } else if s.starts_with("/b/") {
        Some((2, 30 * GB))
    } else {
        None
    }
}

#[test]
fn same_filesystem_is_counted_once_with_the_need_summed() {
    let t = disk_table(
        &[d("/a/es", 20 * GB), d("/a/held", 5 * GB), d("/b/log", GB)],
        &probe,
        10 * GB,
    );
    assert_eq!(t.rows.len(), 2, "two distinct st_dev -> two rows");
    let a = t.rows.iter().find(|r| r.dev == 1).unwrap();
    assert_eq!(a.free, 100 * GB, "free counted once, not per path");
    assert_eq!(a.need, 25 * GB, "need summed per filesystem");
    assert_eq!(a.margin, i128::from(100 * GB - 25 * GB - 10 * GB));
    assert_eq!(a.paths.len(), 2);
    assert_eq!(t.verdict, DiskVerdict::Pass);
}

#[test]
fn the_limiting_destination_is_the_smallest_margin_not_the_least_free() {
    // dev1: 100 free - 85 need - 10 = 5; dev2: 30 free - 1 need - 10 = 19 -> dev1 limits though it has more free.
    let t = disk_table(
        &[d("/a/es", 80 * GB), d("/a/ck", 5 * GB), d("/b/log", GB)],
        &probe,
        10 * GB,
    );
    let lim = &t.rows[t.limiting.unwrap()];
    assert_eq!(lim.dev, 1);
    assert_eq!(lim.margin, i128::from(5 * GB));
    assert_eq!(t.verdict, DiskVerdict::Pass);
    // Summed need pushes dev1 short: verdict Short.
    let t = disk_table(&[d("/a/es", 80 * GB), d("/a/ck", 11 * GB)], &probe, 10 * GB);
    assert_eq!(
        t.verdict,
        DiskVerdict::Short,
        "per-path need would pass, summed need does not"
    );
}

#[test]
fn an_unmeasurable_required_destination_is_unknown_never_pass_and_never_dropped() {
    let t = disk_table(&[d("/a/es", GB), d("/x/held", 1)], &probe, 1);
    assert_eq!(t.verdict, DiskVerdict::Unknown);
    assert_eq!(t.unmeasurable, vec![PathBuf::from("/x/held")]);
    let lines = pump_quant_junction::disk_budget::render_table(&t);
    assert!(lines
        .iter()
        .any(|l| l.contains("UNMEASURABLE") && l.contains("/x/held")));
    assert!(lines.iter().any(|l| l.contains("LIMITING")));
    assert!(lines.last().unwrap().contains("SCENARIO estimate"));
}

#[test]
fn every_required_destination_kind_is_enumerated_and_non_stream_needs_sum_to_the_allowance() {
    let p = Path::new;
    let v = required_destinations(
        p("/a/es"),
        &[p("/a/tape"), p("/a/hist"), p("/a/barrier")],
        p("/a/ck"),
        p("/a/held"),
        p("/a/safety"),
        &[PathBuf::from("/a/out.log"), PathBuf::from("/a/err.log")],
        &[p("/a/req"), p("/a/ack"), p("/a/status"), p("/a/report")],
        23_400,
    );
    for k in [
        DestKind::EventStream,
        DestKind::Journal,
        DestKind::Checkpoint,
        DestKind::HeldLedger,
        DestKind::SafetyFile,
        DestKind::Log,
        DestKind::Handoff,
    ] {
        assert!(v.iter().any(|x| x.kind == k), "{k:?} missing");
    }
    assert_eq!(v.len(), 1 + 3 + 1 + 1 + 1 + 2 + 4);
    assert_eq!(
        NEED_CHECKPOINT + NEED_HELD + NEED_JOURNALS + NEED_LOGS + NEED_SAFETY + NEED_HANDOFF,
        pump_quant_junction::model_lifecycle::NON_STREAM_BYTES_6H
    );
    let es = v.iter().find(|x| x.kind == DestKind::EventStream).unwrap();
    assert_eq!(es.need, 962_505 * 23_400, "stress-rate SCENARIO estimate");
    let total: u64 = v.iter().map(|x| x.need).sum();
    // 23.02 GB scenario total (stream stress + non-stream), up to the integer split remainder.
    assert!(
        total <= 962_505 * 23_400 + 500_000_000 && total + 10 >= 962_505 * 23_400 + 500_000_000
    );
}

#[test]
fn live_redirect_targets_and_probe_are_measurable_here() {
    let tmp = std::env::temp_dir().join(format!("pq_disk_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let (dev, free) =
        pump_quant_junction::disk_budget::probe_fs(&tmp.join("not_yet.json")).unwrap();
    assert!(
        free > 0 && dev > 0,
        "a not-yet-created file is measured on its parent"
    );
    assert!(
        pump_quant_junction::disk_budget::probe_fs(Path::new("/proc/pq_no_such/x/y")).is_none()
    );
}

// ---- runtime ----

const SOFT: u64 = 20 * GB;
const HARD: u64 = 4 * GB;

fn rt(
    mon: &mut DiskMonitor,
    free: u64,
    size: u64,
    t: f64,
) -> pump_quant_junction::disk_budget::DiskRuntime {
    evaluate_runtime(
        mon,
        &[(PathBuf::from("/a/es"), Some((1, free)), size)],
        t,
        SOFT,
        HARD,
        EXIT_RESERVE_BYTES,
        EXIT_RESERVE_SECS,
    )
}

#[test]
fn exit_reserve_is_the_drain_at_stress_plus_the_non_stream_allowance() {
    assert!(EXIT_RESERVE_BYTES >= 962_505 * 1_800 + 500_000_000);
    assert!(EXIT_RESERVE_BYTES - (962_505 * 1_800 + 500_000_000) < 50_000_000);
    assert_eq!(EXIT_RESERVE_SECS, 1_800 + 600);
}

#[test]
fn runtime_samples_written_bytes_and_projects_time_to_floor() {
    let mut m = DiskMonitor::default();
    let r0 = rt(&mut m, 100 * GB, 1_000, 0.0);
    assert_eq!(r0.soft_ok, Some(true));
    assert_eq!(r0.fs[0].time_to_floor_s, None, "no growth measured yet");
    // 100 s later: we wrote 1 GB, free dropped 1 GB -> 10 MB/s; (99 - 4) GB / 10 MB/s = 9500 s.
    let r1 = rt(&mut m, 99 * GB, 1_000 + GB, 100.0);
    assert_eq!(r1.fs[0].written, GB);
    assert!((r1.fs[0].rate_bps - 1e7).abs() < 1.0);
    assert!((r1.fs[0].time_to_floor_s.unwrap() - 9_500.0).abs() < 1.0);
    assert_eq!(r1.soft_ok, Some(true));
    // Another writer on the same fs eats free space faster than we write: the larger rate decides.
    let r2 = rt(&mut m, 49 * GB, 1_000 + GB, 100.0);
    assert!((r2.fs[0].rate_bps - 51e7).abs() < 1.0);
}

#[test]
fn new_exposure_stops_inside_the_byte_reserve_before_the_hard_floor() {
    let mut m = DiskMonitor::default();
    // Above soft floor: fine. Soft floor (20 GB) > hard + reserve (6.25 GB), so the soft floor fires first on
    // a slow fill; the reserve is what holds when the time projection is short.
    let r = rt(&mut m, HARD + EXIT_RESERVE_BYTES, 0, 0.0);
    assert!(
        !r.fs[0].in_reserve,
        "exactly at hard + reserve is outside it"
    );
    let mut m = DiskMonitor::default();
    let r = rt(&mut m, HARD + EXIT_RESERVE_BYTES - 1, 0, 0.0);
    assert!(r.fs[0].in_reserve);
    assert_eq!(r.soft_ok, Some(false), "BUY/ADD refused");
    assert_eq!(
        r.hard_ok,
        Some(true),
        "not latched: reduce/protect/durable writes continue"
    );
}

#[test]
fn new_exposure_stops_when_time_to_floor_is_inside_the_time_reserve_even_with_free_above_the_soft_floor(
) {
    let mut m = DiskMonitor::default();
    let _ = rt(&mut m, 100 * GB, 0, 0.0);
    // 60 s later free dropped 40 GB (667 MB/s): (60 - 4) GB / 667 MB/s = 84 s < 2400 s.
    let r = rt(&mut m, 60 * GB, 0, 60.0);
    assert!(r.fs[0].free >= SOFT);
    assert!(r.fs[0].time_to_floor_s.unwrap() < EXIT_RESERVE_SECS as f64);
    assert!(r.fs[0].in_reserve);
    assert_eq!(r.soft_ok, Some(false));
    assert_eq!(r.hard_ok, Some(true));
}

#[test]
fn below_soft_restricts_and_below_hard_latches() {
    let mut m = DiskMonitor::default();
    let r = rt(&mut m, SOFT - 1, 0, 0.0);
    assert_eq!((r.soft_ok, r.hard_ok), (Some(false), Some(true)));
    let mut m = DiskMonitor::default();
    let r = rt(&mut m, HARD - 1, 0, 0.0);
    assert_eq!((r.soft_ok, r.hard_ok), (Some(false), Some(false)));
}

#[test]
fn runtime_unknown_is_a_breach_for_new_exposure_and_is_never_dropped() {
    let mut m = DiskMonitor::default();
    let r = evaluate_runtime(
        &mut m,
        &[
            (PathBuf::from("/a/es"), Some((1, 100 * GB)), 0),
            (PathBuf::from("/x/held"), None, 0),
        ],
        0.0,
        SOFT,
        HARD,
        EXIT_RESERVE_BYTES,
        EXIT_RESERVE_SECS,
    );
    assert_eq!(r.soft_ok, None, "UNKNOWN, not the measured fs's PASS");
    assert_eq!(r.hard_ok, None, "unknown does not latch");
    assert_eq!(r.unmeasurable, vec![PathBuf::from("/x/held")]);
    // The engine treats soft_ok != Some(true) as the disk restriction (BUY + ADD blocked).
    let mut e = pump_quant_app::engine::Engine::new(
        pump_quant_app::config::Config::dev_portable(),
        pump_quant_app::engine::RunMode::Paper,
    );
    let ops = pump_quant_app::stop_policy::OpsInputs {
        disk_ok: r.soft_ok,
        disk_hard_ok: r.hard_ok,
        ..pump_quant_app::stop_policy::OpsInputs::healthy()
    };
    let ev = e.model_stop_evaluate(0, ops);
    assert!(ev.new_risk_blocked);
    assert!(
        !e.model_safety_blocked(),
        "unknown never latches SAFETY_OFF"
    );
}

#[test]
fn simulated_pressure_is_harness_only_and_maps_to_named_levels() {
    use pump_quant_junction::model_lifecycle::{parse_simulated_pressure, simulated_pressure};
    let p = parse_simulated_pressure("ram disk_reserve");
    assert!(p.ram);
    assert_eq!(p.disk_free, Some(HARD_FLOOR() + EXIT_RESERVE_BYTES - 1));
    assert_eq!(
        parse_simulated_pressure("disk_hard").disk_free,
        Some(HARD_FLOOR() - 1)
    );
    assert!(parse_simulated_pressure("disk_unknown").disk_unknown);
    assert!(!parse_simulated_pressure("").any());
    assert!(
        !simulated_pressure(false).any(),
        "never outside the replay harness"
    );
}

#[allow(non_snake_case)]
fn HARD_FLOOR() -> u64 {
    pump_quant_app::stop_policy::DISK_HARD_FLOOR_BYTES
}

#[test]
fn startup_gate_unknown_is_not_armed_with_a_named_alert() {
    use pump_quant_junction::disk_budget::startup_disk_gate;
    assert_eq!(startup_disk_gate(DiskVerdict::Pass), (Some(true), None));
    assert_eq!(
        startup_disk_gate(DiskVerdict::Short),
        (Some(false), Some("ALERT_DISK_BUDGET_SHORT"))
    );
    assert_eq!(
        startup_disk_gate(DiskVerdict::Unknown),
        (None, Some("ALERT_DISK_BUDGET_UNKNOWN"))
    );
}

#[test]
fn simulated_pressure_file_is_read_only_inside_the_replay_harness() {
    use pump_quant_junction::model_lifecycle::simulated_pressure_from;
    let f = std::env::temp_dir().join(format!("pq_sim_{}", std::process::id()));
    std::fs::write(&f, "ram disk_unknown").unwrap();
    let on = simulated_pressure_from(true, &f);
    assert!(on.ram && on.disk_unknown);
    assert!(
        !simulated_pressure_from(false, &f).any(),
        "file present but not a replay harness: ignored"
    );
}
