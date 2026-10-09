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
    assert_eq!(
        disk_floors_ok(&[&a, &b], free + (1 << 30), 1),
        (Some(false), Some(true))
    );
    // Both floors above free: restricted and hard-latched.
    assert_eq!(
        disk_floors_ok(&[&a, &b], u64::MAX, u64::MAX),
        (Some(false), Some(false))
    );
    // Both below: healthy.
    assert_eq!(disk_floors_ok(&[&a, &b], 1, 1), (Some(true), Some(true)));
    // Two filesystems with different free space: the LEAST free decides.
    let cands = [
        std::env::temp_dir(),
        "/dev/shm".into(),
        "/".into(),
        std::env::current_dir().unwrap(),
    ];
    let fs: Vec<(std::path::PathBuf, u64)> = cands
        .iter()
        .filter_map(|p| pump_quant_junction::model_lifecycle::free_bytes(p).map(|f| (p.clone(), f)))
        .collect();
    let lo = fs.iter().min_by_key(|x| x.1).unwrap();
    let hi = fs.iter().max_by_key(|x| x.1).unwrap();
    assert!(
        hi.1 > lo.1,
        "this host offers two filesystems with different free space: {fs:?}"
    );
    let floor = lo.1 + (hi.1 - lo.1) / 2; // between the two
    assert_eq!(
        disk_floors_ok(&[&hi.0, &lo.0], floor, floor),
        (Some(false), Some(false)),
        "least free decides"
    );
    assert_eq!(
        disk_floors_ok(&[&hi.0], floor, floor),
        (Some(true), Some(true))
    );
    // One unmeasurable path: soft unknown (restricts), hard unknown (does NOT latch).
    let bad = std::path::Path::new("/proc/pq_no_such_dir/x/held.json");
    assert_eq!(disk_floors_ok(&[&a, bad], 1, 1), (None, None));
}

#[test]
fn ram_input_reads_host_and_own_cgroup() {
    let a = mem_available_for_self().expect("measurable on this host");
    assert!(a > 0);
    assert_eq!(
        ram_bytes_ok(),
        Some(a >= pump_quant_app::stop_policy::RAM_FLOOR_BYTES)
    );
}

/// Item 2: the 6 h + 30 min disk need is a RANGE from REAL-TIME stream rates (capture recv_unix_ms), pinned to the
/// RESOURCE_BUDGET.md section 6 derivation. The start verdict uses the STRESS bound plus the soft floor.
#[test]
fn run_disk_need_is_a_real_time_range_and_the_start_check_uses_the_stress_bound() {
    use pump_quant_junction::model_lifecycle::{
        run_budget, run_disk_need, MemCeiling, NON_STREAM_BYTES_6H, STREAM_RATE,
    };
    let (lo, mid, hi, st) = run_disk_need(STREAM_RATE, 23_400);
    assert_eq!(
        (lo, mid, hi, st),
        (
            450_750 * 23_400 + NON_STREAM_BYTES_6H,
            601_552 * 23_400 + NON_STREAM_BYTES_6H,
            685_258 * 23_400 + NON_STREAM_BYTES_6H,
            962_505 * 23_400 + NON_STREAM_BYTES_6H
        )
    );
    assert!(lo < mid && mid < hi && hi < st);
    // ~11.0 / 14.6 / 16.5 / 23.0 GB.
    assert_eq!(
        (
            lo / 100_000_000,
            mid / 100_000_000,
            hi / 100_000_000,
            st / 100_000_000
        ),
        (110, 145, 165, 230)
    );
    let soft = pump_quant_app::stop_policy::DISK_SOFT_FLOOR_BYTES;
    let gib12 = pump_quant_app::stop_policy::RAM_FLOOR_BYTES;
    assert_eq!(
        run_budget(
            Some(st + soft),
            Some(4096),
            MemCeiling::Unlimited,
            Some(gib12),
            23_400
        )
        .disk_ok,
        Some(true)
    );
    assert_eq!(
        run_budget(
            Some(st + soft - 1),
            Some(4096),
            MemCeiling::Unlimited,
            Some(gib12),
            23_400
        )
        .disk_ok,
        Some(false),
        "stress bound decides"
    );
    assert_eq!(
        run_budget(
            Some(hi + soft),
            Some(4096),
            MemCeiling::Unlimited,
            Some(gib12),
            23_400
        )
        .disk_ok,
        Some(false)
    );
    let unk = run_budget(None, None, MemCeiling::Unmeasured, None, 23_400);
    assert_eq!(
        (unk.disk_ok, unk.nofile_ok, unk.mem_ok),
        (None, None, None),
        "unmeasured is never fine"
    );
}

/// Item 2: open-file and memory verdicts use the process's own limits. The soft RLIMIT_NOFILE parses from
/// /proc/<pid>/limits text; memory.high caps like memory.max (the lower one decides).
#[test]
fn nofile_and_cgroup_ceiling_use_the_actual_limits() {
    use pump_quant_app::stop_policy::cgroup_mem_ceiling;
    use pump_quant_junction::model_lifecycle::{
        nofile_soft_limit, parse_nofile_soft, run_budget, MemCeiling, FD_NEED,
    };
    let t = "Limit                     Soft Limit           Hard Limit           Units     \nMax open files            4096                 524288               files     \n";
    assert_eq!(parse_nofile_soft(t), Some(4096));
    assert_eq!(
        parse_nofile_soft("Max open files  unlimited unlimited files"),
        Some(u64::MAX)
    );
    assert_eq!(parse_nofile_soft("nothing"), None);
    assert!(nofile_soft_limit().is_some(), "readable on this host");
    assert_eq!(FD_NEED, 72);
    assert_eq!(
        run_budget(
            Some(u64::MAX),
            Some(FD_NEED),
            MemCeiling::Unlimited,
            None,
            1
        )
        .nofile_ok,
        Some(true)
    );
    assert_eq!(
        run_budget(
            Some(u64::MAX),
            Some(FD_NEED - 1),
            MemCeiling::Unlimited,
            None,
            1
        )
        .nofile_ok,
        Some(false)
    );
    assert_eq!(
        cgroup_mem_ceiling(Some(8), Some(4)),
        Some(4),
        "memory.high below max decides"
    );
    assert_eq!(
        cgroup_mem_ceiling(None, Some(4)),
        Some(4),
        "high alone caps"
    );
    assert_eq!(cgroup_mem_ceiling(Some(8), None), Some(8));
    assert_eq!(cgroup_mem_ceiling(None, None), None);
}

/// Item 2: memory budget. When the cgroup is UNLIMITED (memory.max = memory.high = "max", the measured state of
/// /system.slice/hermes-gateway.service) the budget is host MemAvailable minus the 12% free-RAM floor of MemTotal;
/// no cap is invented. A real ceiling caps by its remaining room; an unreadable ceiling is unmeasured (never fine).
#[test]
fn mem_budget_is_mem_available_minus_the_12pct_floor_and_no_cap_is_invented() {
    use pump_quant_app::stop_policy::{RAM_FLOOR_BPS, RAM_FLOOR_BYTES};
    use pump_quant_junction::model_lifecycle::{
        cgroup_mem_ceiling_now, mem_run_budget, run_budget, run_budget_now, MemCeiling,
    };
    assert_eq!(RAM_FLOOR_BPS, 1_200);
    // 100 GiB total, 50 GiB available: floor 12 GiB -> 38 GiB budget, even with a huge memory.current.
    let gib_kb = 1u64 << 20;
    let gib = 1u64 << 30;
    let t = 100 * gib_kb;
    let a = 50 * gib_kb;
    let unl = mem_run_budget(
        Some(t),
        Some(a),
        RAM_FLOOR_BPS,
        MemCeiling::Unlimited,
        Some(u64::MAX),
    );
    assert_eq!(
        unl,
        Some(50 * gib - 12 * gib),
        "MemAvailable minus 12% of MemTotal"
    );
    assert_eq!(
        mem_run_budget(Some(t), Some(a), RAM_FLOOR_BPS, MemCeiling::Unlimited, None),
        unl,
        "unlimited: memory.current is irrelevant, no cap invented"
    );
    // A real 4 GiB ceiling with 1 GiB used caps the budget at 3 GiB.
    assert_eq!(
        mem_run_budget(
            Some(t),
            Some(a),
            RAM_FLOOR_BPS,
            MemCeiling::Bytes(4 * gib),
            Some(gib)
        ),
        Some(3 * gib)
    );
    assert_eq!(
        mem_run_budget(
            Some(t),
            Some(a),
            RAM_FLOOR_BPS,
            MemCeiling::Bytes(4 * gib),
            None
        ),
        None,
        "a ceiling without memory.current is unmeasured"
    );
    assert_eq!(
        mem_run_budget(
            Some(t),
            Some(a),
            RAM_FLOOR_BPS,
            MemCeiling::Unmeasured,
            Some(0)
        ),
        None
    );
    assert_eq!(
        mem_run_budget(None, Some(a), RAM_FLOOR_BPS, MemCeiling::Unlimited, None),
        None
    );
    // Available below the floor saturates to 0 (never negative, never fine).
    assert_eq!(
        mem_run_budget(
            Some(t),
            Some(10 * gib_kb),
            RAM_FLOOR_BPS,
            MemCeiling::Unlimited,
            None
        ),
        Some(0)
    );
    let b = |m| run_budget(Some(u64::MAX), Some(4096), MemCeiling::Unlimited, m, 1).mem_ok;
    assert_eq!(b(Some(RAM_FLOOR_BYTES)), Some(true));
    assert_eq!(b(Some(RAM_FLOOR_BYTES - 1)), Some(false));
    assert_eq!(b(None), None);
    // Live: this test process reads its own cgroup; the budget equals the pure function over the same readings.
    let (c, _cur) = cgroup_mem_ceiling_now();
    assert_ne!(
        c,
        MemCeiling::Unmeasured,
        "cgroup v2 limits are readable on this host"
    );
    let live = run_budget_now(&[std::env::temp_dir().as_path()], 23_400);
    assert_eq!(live.mem_ceiling, c);
    assert!(live.mem_budget.is_some());
    assert!(live.nofile_soft.is_some() && live.disk_free.is_some());
}
