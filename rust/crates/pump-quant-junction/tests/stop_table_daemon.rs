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
        run_budget, run_disk_need, NON_STREAM_BYTES_6H, STREAM_RATE,
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
        run_budget(Some(st + soft), Some(4096), mv(Some(gib12)), 23_400).disk_ok,
        Some(true)
    );
    assert_eq!(
        run_budget(Some(st + soft - 1), Some(4096), mv(Some(gib12)), 23_400).disk_ok,
        Some(false),
        "stress bound decides"
    );
    assert_eq!(
        run_budget(Some(hi + soft), Some(4096), mv(Some(gib12)), 23_400).disk_ok,
        Some(false)
    );
    let unk = run_budget(None, None, mv(None), 23_400);
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
        nofile_soft_limit, parse_nofile_soft, run_budget, FD_NEED,
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
        run_budget(Some(u64::MAX), Some(FD_NEED), mv(None), 1).nofile_ok,
        Some(true)
    );
    assert_eq!(
        run_budget(Some(u64::MAX), Some(FD_NEED - 1), mv(None), 1).nofile_ok,
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

/// A memory verdict with this effective availability (no cgroup cap), for the run_budget tests.
fn mv(effective: Option<u64>) -> pump_quant_junction::model_lifecycle::MemVerdict {
    pump_quant_junction::model_lifecycle::MemVerdict {
        state: pump_quant_junction::model_lifecycle::MemState::NoCgroupCap,
        host_floor: Some(0),
        host_term: effective,
        effective,
        ok: pump_quant_app::stop_policy::bytes_ok(
            effective,
            pump_quant_app::stop_policy::RAM_FLOOR_BYTES,
        ),
    }
}

fn lvl(
    level: &str,
    max: Option<Option<u64>>,
    high: Option<Option<u64>>,
    current: Option<u64>,
) -> pump_quant_junction::model_lifecycle::CgLevel {
    pump_quant_junction::model_lifecycle::CgLevel {
        level: level.to_string(),
        max,
        high,
        current,
    }
}

/// Item 1 (follow-up scope): ONE memory rule. effective = min(MemAvailable - 12% MemTotal, tightest cgroup room
/// over the WHOLE ancestor chain); the host floor is subtracted once; three distinct states; unmeasurable = UNKNOWN.
#[test]
fn mem_rule_all_max_chain_is_no_cgroup_cap_and_the_host_term_still_applies() {
    use pump_quant_app::stop_policy::{RAM_FLOOR_BPS, RAM_FLOOR_BYTES};
    use pump_quant_junction::model_lifecycle::{mem_rule, MemState};
    let gib_kb = 1u64 << 20;
    let gib = 1u64 << 30;
    let chain = [
        lvl("/a/b", Some(None), Some(None), Some(u64::MAX)),
        lvl("/a", Some(None), Some(None), Some(u64::MAX)),
    ];
    let v = mem_rule(
        Some(100 * gib_kb),
        Some(50 * gib_kb),
        &chain,
        RAM_FLOOR_BPS,
        RAM_FLOOR_BYTES,
    );
    assert_eq!(v.state, MemState::NoCgroupCap);
    assert_eq!(v.host_floor, Some(12 * gib));
    assert_eq!(v.host_term, Some(38 * gib));
    assert_eq!(
        v.effective,
        Some(38 * gib),
        "no cap is NOT unlimited: host term applies"
    );
    assert_eq!(v.ok, Some(true));
    // Host below its floor: 0, not fine, even with no cgroup cap.
    let low = mem_rule(
        Some(100 * gib_kb),
        Some(10 * gib_kb),
        &chain,
        RAM_FLOOR_BPS,
        RAM_FLOOR_BYTES,
    );
    assert_eq!((low.effective, low.ok), (Some(0), Some(false)));
    // Empty chain (process in the root cgroup) = no cap.
    let root = mem_rule(
        Some(100 * gib_kb),
        Some(50 * gib_kb),
        &[],
        RAM_FLOOR_BPS,
        RAM_FLOOR_BYTES,
    );
    assert_eq!(root.state, MemState::NoCgroupCap);
}

#[test]
fn mem_rule_capped_ancestor_decides_even_when_the_leaf_is_max() {
    use pump_quant_app::stop_policy::{RAM_FLOOR_BPS, RAM_FLOOR_BYTES};
    use pump_quant_junction::model_lifecycle::{mem_rule, MemState};
    let gib_kb = 1u64 << 20;
    let gib = 1u64 << 30;
    let chain = [
        lvl("/slice/svc", Some(None), Some(None), Some(gib)),
        lvl("/slice", Some(Some(20 * gib)), Some(None), Some(5 * gib)),
    ];
    let v = mem_rule(
        Some(1000 * gib_kb),
        Some(500 * gib_kb),
        &chain,
        RAM_FLOOR_BPS,
        RAM_FLOOR_BYTES,
    );
    assert_eq!(
        v.state,
        MemState::CgroupCap {
            cap: 20 * gib,
            room: 15 * gib,
            level: "/slice".into()
        }
    );
    assert_eq!(v.effective, Some(15 * gib));
    assert_eq!(v.ok, Some(true));
    // memory.high at an ancestor caps too (lower of max/high).
    let chain_h = [
        lvl("/slice/svc", Some(None), Some(None), Some(gib)),
        lvl(
            "/slice",
            Some(Some(20 * gib)),
            Some(Some(14 * gib)),
            Some(5 * gib),
        ),
    ];
    let h = mem_rule(
        Some(1000 * gib_kb),
        Some(500 * gib_kb),
        &chain_h,
        RAM_FLOOR_BPS,
        RAM_FLOOR_BYTES,
    );
    assert_eq!((h.effective, h.ok), (Some(9 * gib), Some(false)));
}

#[test]
fn mem_rule_parent_tighter_than_child_wins_and_child_tighter_wins_too() {
    use pump_quant_app::stop_policy::{RAM_FLOOR_BPS, RAM_FLOOR_BYTES};
    use pump_quant_junction::model_lifecycle::{mem_rule, MemState};
    let gib_kb = 1u64 << 20;
    let gib = 1u64 << 30;
    // Child cap 64 GiB (room 60), parent cap 40 GiB with 30 used (room 10): parent decides.
    let chain = [
        lvl("/p/c", Some(Some(64 * gib)), Some(None), Some(4 * gib)),
        lvl("/p", Some(Some(40 * gib)), Some(None), Some(30 * gib)),
    ];
    let v = mem_rule(
        Some(1000 * gib_kb),
        Some(500 * gib_kb),
        &chain,
        RAM_FLOOR_BPS,
        RAM_FLOOR_BYTES,
    );
    assert_eq!(
        v.state,
        MemState::CgroupCap {
            cap: 40 * gib,
            room: 10 * gib,
            level: "/p".into()
        }
    );
    assert_eq!((v.effective, v.ok), (Some(10 * gib), Some(false)));
    // Order independent: parent listed first gives the same verdict.
    let rev = [chain[1].clone(), chain[0].clone()];
    assert_eq!(
        mem_rule(
            Some(1000 * gib_kb),
            Some(500 * gib_kb),
            &rev,
            RAM_FLOOR_BPS,
            RAM_FLOOR_BYTES
        )
        .effective,
        Some(10 * gib)
    );
    // The host term is tighter than every cgroup room: host decides.
    let tight_host = mem_rule(
        Some(100 * gib_kb),
        Some(20 * gib_kb),
        &chain,
        RAM_FLOOR_BPS,
        RAM_FLOOR_BYTES,
    );
    assert_eq!(tight_host.effective, Some(8 * gib));
}

#[test]
fn mem_rule_unreadable_files_are_unknown_never_pass() {
    use pump_quant_app::stop_policy::{RAM_FLOOR_BPS, RAM_FLOOR_BYTES};
    use pump_quant_junction::model_lifecycle::{mem_rule, MemState};
    let gib_kb = 1u64 << 20;
    let gib = 1u64 << 30;
    let big = (Some(1000 * gib_kb), Some(900 * gib_kb));
    for chain in [
        vec![lvl("/x", None, Some(None), Some(gib))],
        vec![lvl("/x", Some(None), None, Some(gib))],
        vec![
            lvl("/x/y", Some(None), Some(None), Some(gib)),
            lvl("/x", Some(Some(500 * gib)), Some(None), None),
        ],
        // An unreadable ancestor after a capped child still makes the verdict UNKNOWN.
        vec![
            lvl("/x/y", Some(Some(500 * gib)), Some(None), Some(gib)),
            lvl("/x", None, None, None),
        ],
    ] {
        let v = mem_rule(big.0, big.1, &chain, RAM_FLOOR_BPS, RAM_FLOOR_BYTES);
        assert!(
            matches!(v.state, MemState::Unmeasurable(_)),
            "{chain:?} -> {v:?}"
        );
        assert_eq!((v.effective, v.ok), (None, None), "UNKNOWN, never PASS");
    }
    // A capless 'max' level with unreadable memory.current is still measurable (current is irrelevant there).
    let ok = mem_rule(
        big.0,
        big.1,
        &[lvl("/x", Some(None), Some(None), None)],
        RAM_FLOOR_BPS,
        RAM_FLOOR_BYTES,
    );
    assert_eq!(ok.state, MemState::NoCgroupCap);
    // meminfo unreadable: UNKNOWN regardless of the cgroup.
    let nm = mem_rule(None, None, &[], RAM_FLOOR_BPS, RAM_FLOOR_BYTES);
    assert_eq!((nm.effective, nm.ok), (None, None));
}

/// FAILS if the 12% host floor is subtracted twice (or on the cgroup term): host term 88 GiB, cgroup room 30 GiB
/// -> effective must be exactly 30 GiB (not 30 - 12 = 18), and a 13 GiB room must pass the 12 GiB floor.
#[test]
fn mem_rule_subtracts_the_host_floor_exactly_once() {
    use pump_quant_app::stop_policy::{RAM_FLOOR_BPS, RAM_FLOOR_BYTES};
    use pump_quant_junction::model_lifecycle::mem_rule;
    let gib_kb = 1u64 << 20;
    let gib = 1u64 << 30;
    let room30 = [lvl("/s", Some(Some(40 * gib)), Some(None), Some(10 * gib))];
    let v = mem_rule(
        Some(100 * gib_kb),
        Some(100 * gib_kb),
        &room30,
        RAM_FLOOR_BPS,
        RAM_FLOOR_BYTES,
    );
    assert_eq!(v.host_term, Some(88 * gib), "100 - 12% of 100 = 88, once");
    assert_eq!(v.effective, Some(30 * gib), "no floor on the cgroup term");
    let room13 = [lvl("/s", Some(Some(23 * gib)), Some(None), Some(10 * gib))];
    let w = mem_rule(
        Some(100 * gib_kb),
        Some(100 * gib_kb),
        &room13,
        RAM_FLOOR_BPS,
        RAM_FLOOR_BYTES,
    );
    assert_eq!((w.effective, w.ok), (Some(13 * gib), Some(true)));
    // Host term exactly at the floor: 12% of 100 subtracted once from 24 -> 12 GiB -> passes; twice -> 0.
    let x = mem_rule(
        Some(100 * gib_kb),
        Some(24 * gib_kb),
        &[],
        RAM_FLOOR_BPS,
        RAM_FLOOR_BYTES,
    );
    assert_eq!((x.effective, x.ok), (Some(12 * gib), Some(true)));
}

/// The chain reader walks every ancestor up to (excluding) the root, and the RUNTIME RAM input and the STARTUP
/// budget are the same function over the same live readings.
#[test]
fn cgroup_chain_reader_walks_to_the_root_and_startup_equals_runtime() {
    use pump_quant_junction::model_lifecycle::{
        mem_available_for_self, mem_verdict_now, read_cgroup_chain, run_budget_now, MemState,
    };
    let d = tmp("cgchain");
    for (rel, max, high, cur) in [
        ("a", "max", "max", "100"),
        ("a/b", "5000", "max", "1000"),
        ("a/b/c", "max", "max", "10"),
    ] {
        let p = d.join(rel);
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("memory.max"), format!("{max}\n")).unwrap();
        std::fs::write(p.join("memory.high"), format!("{high}\n")).unwrap();
        std::fs::write(p.join("memory.current"), format!("{cur}\n")).unwrap();
    }
    let chain = read_cgroup_chain(&d, "/a/b/c\n");
    assert_eq!(
        chain.iter().map(|l| l.level.as_str()).collect::<Vec<_>>(),
        vec!["/a/b/c", "/a/b", "/a"]
    );
    assert_eq!(chain[1].max, Some(Some(5000)));
    assert_eq!(chain[0].max, Some(None));
    std::fs::remove_file(d.join("a").join("memory.high")).unwrap();
    assert_eq!(
        read_cgroup_chain(&d, "/a/b/c")[2].high,
        None,
        "unreadable recorded, not defaulted"
    );
    // Live: startup budget and runtime input agree on state and verdict (same function).
    let live = mem_verdict_now();
    assert!(!matches!(live.state, MemState::Unmeasurable(_)), "{live:?}");
    let b = run_budget_now(&[std::env::temp_dir().as_path()], 23_400);
    assert_eq!(b.mem.state, live.state);
    assert_eq!(b.mem_ok, b.mem.ok);
    assert_eq!(b.mem.host_floor, live.host_floor);
    assert!(mem_available_for_self().is_some());
    assert_eq!(ram_bytes_ok().is_some(), true);
    let bad = std::path::Path::new("/proc/pq_no_such_dir/x/event_stream.jsonl");
    let partial = run_budget_now(&[std::env::temp_dir().as_path(), bad], 23_400);
    assert_eq!((partial.disk_free, partial.disk_ok), (None, None));
}
