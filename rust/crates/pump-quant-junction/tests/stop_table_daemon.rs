//! Daemon-side stop-table helpers (`model_lifecycle`): startup paper check, run-deadline config, the deadline
//! handoff sentinel, the RPC budget basis and the disk input. Each assertion is mutation-checked
//! (see docs/missing_history_causal/STOP_ACTION_TABLE.md).

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_junction::model_lifecycle::{
    bootstrap_budget, disk_floors_ok, disk_headroom_ok, mem_available_for_self, ram_bytes_ok,
    request_deadline_handoff, run_deadline_from, startup_paper_check,
};

fn tmp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("pq_stopd_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn startup_refuses_live_flag_keypair_and_non_paper_engine() {
    let paper = Engine::new(Config::dev_portable(), RunMode::Paper);
    assert_eq!(startup_paper_check(&paper, false, false), Ok(()));
    assert_eq!(
        startup_paper_check(&paper, true, false),
        Err("live_flag_present")
    );
    assert_eq!(
        startup_paper_check(&paper, false, true),
        Err("keypair_loaded")
    );
    let replay = Engine::new(Config::dev_portable(), RunMode::Replay);
    assert_eq!(
        startup_paper_check(&replay, false, false),
        Err("engine_not_paper_mode")
    );
}

#[test]
fn run_deadline_defaults_to_6h_plus_30min_and_an_override_may_only_shorten() {
    assert_eq!(run_deadline_from(None, None), (21_600_000, 1_800_000));
    assert_eq!(
        run_deadline_from(Some("60000"), Some("5000")),
        (60_000, 5_000)
    );
    assert_eq!(
        run_deadline_from(Some("99999999999"), Some("99999999999")),
        (21_600_000, 1_800_000)
    );
    assert_eq!(
        run_deadline_from(Some("junk"), Some("-1")),
        (21_600_000, 1_800_000)
    );
    assert_eq!(
        run_deadline_from(Some("0"), Some("0")),
        (21_600_000, 1_800_000)
    );
}

#[test]
fn deadline_handoff_raises_the_existing_stop_sentinel_once_and_never_overwrites() {
    let d = tmp("handoff");
    let f = d.join("data").join("DAEMON_STOP");
    assert!(request_deadline_handoff(&f), "raised now");
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        "run_deadline_drain_elapsed\n"
    );
    std::fs::write(&f, "operator").unwrap();
    assert!(!request_deadline_handoff(&f), "already raised: not again");
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        "operator",
        "an existing stop request is never overwritten"
    );
}

#[test]
fn bootstrap_budget_reserves_held_position_capacity() {
    let mut b = bootstrap_budget();
    assert_eq!(
        (b.rule.capacity, b.rule.per_held, b.window_ms),
        (3_600, 120, 3_600_000)
    );
    // 10 held positions reserve 1_200 pages: discovery gets 2_400 (= 120 twenty-page walks).
    for _ in 0..120 {
        assert!(b.try_discovery(0, 10, 20));
    }
    assert!(
        !b.try_discovery(0, 10, 20),
        "discovery stops at the held reserve"
    );
    assert!(b.discovery_exhausted(10));
    assert!(b.try_held(0, 1_200), "held support still has its reserve");
}

#[test]
fn disk_input_is_some_true_only_when_measured_at_or_above_the_floor() {
    let d = tmp("disk");
    let p = d.join("safety.json");
    assert_eq!(disk_headroom_ok(&p, 1), Some(true));
    assert_eq!(disk_headroom_ok(&p, u64::MAX), Some(false));
    assert_eq!(
        disk_headroom_ok(
            std::path::Path::new("/proc/pq_no_such_dir/x/safety.json"),
            1
        ),
        None
    );
}

#[test]
fn disk_floors_take_the_least_free_path_and_unmeasurable_never_latches() {
    let d = tmp("floors");
    let a = d.join("safety.json");
    let b = d.join("held.json");
    let free = pump_quant_junction::model_lifecycle::free_bytes(&a).expect("measurable tmp dir");
    // Soft above free, hard below free: restricted, not latched.
    assert_eq!(disk_floors_ok(&[&a, &b], free + (1 << 30), 1), (Some(false), Some(true)));
    // Both floors above free: restricted and hard-latched.
    assert_eq!(disk_floors_ok(&[&a, &b], u64::MAX, u64::MAX), (Some(false), Some(false)));
    // Both below: healthy.
    assert_eq!(disk_floors_ok(&[&a, &b], 1, 1), (Some(true), Some(true)));
    // Two filesystems with different free space: the LEAST free decides.
    let cands = [std::env::temp_dir(), "/dev/shm".into(), "/".into(), std::env::current_dir().unwrap()];
    let fs: Vec<(std::path::PathBuf, u64)> = cands
        .iter()
        .filter_map(|p| pump_quant_junction::model_lifecycle::free_bytes(p).map(|f| (p.clone(), f)))
        .collect();
    let lo = fs.iter().min_by_key(|x| x.1).unwrap();
    let hi = fs.iter().max_by_key(|x| x.1).unwrap();
    assert!(hi.1 > lo.1, "this host offers two filesystems with different free space: {fs:?}");
    let floor = lo.1 + (hi.1 - lo.1) / 2; // between the two
    assert_eq!(disk_floors_ok(&[&hi.0, &lo.0], floor, floor), (Some(false), Some(false)), "least free decides");
    assert_eq!(disk_floors_ok(&[&hi.0], floor, floor), (Some(true), Some(true)));
    // One unmeasurable path: soft unknown (restricts), hard unknown (does NOT latch).
    let bad = std::path::Path::new("/proc/pq_no_such_dir/x/held.json");
    assert_eq!(disk_floors_ok(&[&a, bad], 1, 1), (None, None));
}

#[test]
fn ram_input_reads_host_and_own_cgroup() {
    let a = mem_available_for_self().expect("measurable on this host");
    assert!(a > 0);
    assert_eq!(ram_bytes_ok(), Some(a >= pump_quant_app::stop_policy::RAM_FLOOR_BYTES));
}

