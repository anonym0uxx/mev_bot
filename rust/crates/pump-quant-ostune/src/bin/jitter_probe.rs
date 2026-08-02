//! §4.5 jitter probe — measures scheduling jitter before/after OS tuning.
//!
//! Run modes:
//!   `jitter_probe baseline`           — measure jitter WITHOUT any tuning
//!   `jitter_probe tuned`              — apply OS tuning, then measure jitter
//!   `jitter_probe delta`              — run baseline, then tuned, report delta
//!   `jitter_probe positive-control`   — inject known 50µs jitter every 1000
//!                                        samples, confirm instrument detects it
//!   `jitter_probe qpf`                — report QueryPerformanceFrequency only
//!
//! CRITICAL: The WinOsTune adapter MUST outlive the probe run. The adapter
//! owns the TimerResGuard (timer resolution) and locked memory ranges
//! (VirtualLock). If the adapter is dropped before the probe, the timer
//! resets and memory unlocks — the probe measures an untuned system while
//! reporting a "tuned" result. Only thread affinity and process priority
//! persist after adapter drop; timer and lock do NOT.
//!
//! All four adapter calls FAIL CLOSED — any error aborts the tuning run
//! and the probe runs untuned (reported as such).

use pump_quant_core::cpu_numa_tuning::{
    derive_plan, parse_topology, ProcRecord, HotThreadSpec, Prio,
    jitter_stats, JitterStats, OsTune,
};
use std::time::Instant;

/// Number of samples per probe run.
const SAMPLES: usize = 50_000;

/// Tick interval target in nanoseconds. We busy-wait and measure the
/// ACTUAL interval — jitter is the deviation from this target.
const TICK_NS: u64 = 1_000; // 1 µs target tick

/// Injected jitter magnitude for positive control (50 µs).
const INJECT_NS: u64 = 50_000;

/// Inject every N samples.
const INJECT_INTERVAL: usize = 1_000;

// ─── QPF FFI ──────────────────────────────────────────────────────────
// QueryPerformanceFrequency returns counts/sec. If 10,000,000 then
// 1 count = 100 ns — the clock resolves to 100 ns, NOT 1000 ns.
// Rust's Instant is backed by QPC on Windows.

#[cfg(windows)]
extern "system" {
    fn QueryPerformanceFrequency(freq: *mut i64) -> i32;
    fn QueryPerformanceCounter(counter: *mut i64) -> i32;
}

#[cfg(windows)]
fn report_qpf() -> Option<i64> {
    let mut freq: i64 = 0;
    let ok = unsafe { QueryPerformanceFrequency(&mut freq) };
    if ok == 0 {
        return None;
    }
    // Also read the counter once to show resolution in action.
    let mut counter: i64 = 0;
    unsafe { QueryPerformanceCounter(&mut counter) };
    println!("QPF: frequency={} Hz (1 count = {} ns), counter={}",
        freq, 1_000_000_000 / freq, counter);
    Some(freq)
}

#[cfg(not(windows))]
fn report_qpf() -> Option<i64> {
    println!("QPF: not on windows — no QPC");
    None
}

// ─── Probe ────────────────────────────────────────────────────────────

/// Run the probe. If `inject` is true, stall for ~INJECT_NS every
/// INJECT_INTERVAL samples. The stall happens BETWEEN samples, so the
/// NEXT sample's delta includes the stall time. If the instrument can
/// detect jitter, p999 and max should jump by ~INJECT_NS.
fn run_probe(inject: bool) -> JitterStats {
    let (stats, _) = run_probe_raw(inject);
    stats
}

/// Run the probe and return both the stats AND the raw deltas for analysis.
fn run_probe_raw(inject: bool) -> (JitterStats, Vec<u64>) {
    let mut deltas: Vec<u64> = Vec::with_capacity(SAMPLES);
    let mut prev = Instant::now();

    for i in 0..SAMPLES {
        // Positive control: inject known jitter every INJECT_INTERVAL samples.
        // The stall goes BEFORE the next busy-wait, so the next delta
        // includes the stall. This tests whether the instrument can
        // detect a perturbation of known magnitude.
        if inject && i > 0 && i % INJECT_INTERVAL == 0 {
            let stall_start = Instant::now();
            loop {
                let stalled = stall_start.elapsed().as_nanos() as u64;
                if stalled >= INJECT_NS {
                    break;
                }
            }
            // prev is still the last sample's timestamp. The next
            // busy-wait will measure elapsed since prev, which now
            // includes the stall. So that sample's delta ≈ TICK_NS + INJECT_NS.
        }

        // Busy-wait: spin until at least TICK_NS have elapsed since prev.
        loop {
            let now = Instant::now();
            let elapsed = now.duration_since(prev).as_nanos() as u64;
            if elapsed >= TICK_NS {
                deltas.push(elapsed);
                prev = now;
                break;
            }
        }
    }
    let stats = jitter_stats(&deltas);
    (stats, deltas)
}

/// Topology for group 0: 8 physical cores, each with 2 SMT siblings.
/// 192 logical CPUs / 3 processor groups; group 0 has 32 physical cores.
/// We model 8 cores (enough for 1 hot thread + control).
#[cfg(windows)]
fn build_topology() -> Option<pump_quant_core::cpu_numa_tuning::PinPlan> {
    let records: Vec<ProcRecord> = (0..8)
        .map(|i| ProcRecord::core(0, (0b11u64) << (i * 2)))
        .collect();
    let topo = parse_topology(&records).ok()?;
    let hot = vec![HotThreadSpec::test("jitter-probe-thread")];
    let plan = derive_plan(&topo, &hot).ok()?;
    Some(plan)
}

#[cfg(not(windows))]
fn build_topology() -> Option<pump_quant_core::cpu_numa_tuning::PinPlan> {
    None
}

/// Tuning state that MUST outlive the probe. The WinOsTune adapter owns
/// the TimerResGuard (timer resolution) and locked memory (VirtualLock).
/// The sentinel Vec keeps the locked region alive. Both are held in scope
/// by the caller for the duration of the probe.
#[cfg(windows)]
struct TuningGuard {
    _os: Box<pump_quant_core::cpu_numa_tuning::win_adapter::WinOsTune>,
    _sentinel: Vec<u8>,
    /// Read-back values for reporting.
    affinity_applied: bool,
    priority_applied: bool,
    timer_res_ms: u32,
    locked_bytes: usize,
}

#[cfg(windows)]
fn apply_tuning(
    plan: &pump_quant_core::cpu_numa_tuning::PinPlan,
) -> Result<TuningGuard, String> {
    use pump_quant_core::cpu_numa_tuning::win_adapter::WinOsTune;

    let mut os = Box::new(WinOsTune::new(64 * 1024 * 1024)
        .map_err(|e| format!("WinOsTune::new failed: {:?}", e))?);

    let timer_res = os.set_timer_res_ms(1)
        .map_err(|e| format!("set_timer_res_ms failed: {:?}", e))?;

    let report = pump_quant_core::cpu_numa_tuning::apply_plan(
        &mut *os as &mut dyn OsTune,
        plan,
        Prio::High,
    );

    if !report.errors.is_empty() {
        return Err(format!("apply_plan errors: {:?}", report.errors));
    }
    if !report.mismatches.is_empty() {
        return Err(format!("apply_plan mismatches: {:?}", report.mismatches));
    }
    if report.applied.is_empty() {
        return Err("apply_plan: no threads applied".to_string());
    }

    let sentinel = vec![0u8; 64 * 1024];
    let locked = unsafe {
        os.lock_region(sentinel.as_ptr(), sentinel.len())
    }.map_err(|e| format!("lock_region failed: {:?}", e))?;
    if locked == 0 {
        return Err("lock_region returned 0 bytes".to_string());
    }

    Ok(TuningGuard {
        _os: os,
        _sentinel: sentinel,
        affinity_applied: true,
        priority_applied: true,
        timer_res_ms: timer_res,
        locked_bytes: locked,
    })
}

#[cfg(not(windows))]
struct TuningGuard {}

#[cfg(not(windows))]
fn apply_tuning(_plan: &pump_quant_core::cpu_numa_tuning::PinPlan) -> Result<TuningGuard, String> {
    Err("not on windows".to_string())
}

fn print_stats(label: &str, s: &JitterStats) {
    println!("{}: n={}, p50={}ns, p99={}ns, p999={}ns, max={}ns",
        label, s.n, s.p50_ns, s.p99_ns, s.p999_ns, s.max_ns);
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "delta".to_string());

    // Report QPF in ALL modes — this is the instrument characterization.
    let qpf = report_qpf();

    match mode.as_str() {
        "qpf" => {
            // Just report QPF and exit.
            if let Some(f) = qpf {
                let tick_ns = 1_000_000_000 / f;
                println!("\nQPF={} — clock tick resolution = {} ns", f, tick_ns);
                if tick_ns == 100 {
                    println!("10 MHz QPC confirmed. 1000 ns is NOT the clock floor.");
                    println!("The probe is the defect if p50=p99=p999=1000 ns.");
                }
            }
        }
        "histogram" => {
            let (stats, deltas) = run_probe_raw(false);
            print_stats("histogram-baseline", &stats);
            let mut sorted = deltas.clone();
            sorted.sort_unstable();
            let n = sorted.len();
            println!("\n=== TOP 20 DELTAS (sorted descending) ===");
            for i in 0..20 {
                let rank = n - 1 - i;
                println!("  rank[{}] = {} ns", rank, sorted[rank]);
            }
            println!("\n=== HISTOGRAM (deltas > 1000 ns) ===");
            let buckets = [
                (1001u64, 2000u64, "1-2 us"), (2001u64, 5000u64, "2-5 us"),
                (5001u64, 10000u64, "5-10 us"), (10001u64, 50000u64, "10-50 us"),
                (50001u64, 100000u64, "50-100 us"), (100001u64, u64::MAX, "100 us+"),
            ];
            for (lo, hi, label) in buckets {
                let count = sorted.iter().filter(|&&d| d >= lo && d <= hi).count();
                if count > 0 {
                    println!("  {} ({}-{} ns): {} samples", label, lo, hi, count);
                }
            }
            let p999_idx = ((999u64 * n as u64).div_ceil(1000).saturating_sub(1)) as usize;
            println!("\n=== p999 CONTEXT (index {} of {}) ===", p999_idx, n);
            for offset in [3i64, 2, 1, 0, -1, -2, -3] {
                let idx = p999_idx as i64 - offset;
                if idx >= 0 && idx < n as i64 {
                    let i = idx as usize;
                    println!("  sorted[{}] = {} ns {}", i, sorted[i],
                        if i == p999_idx { "<-- p999" } else { "" });
                }
            }
        }
        "baseline" => {
            let stats = run_probe(false);
            print_stats("baseline", &stats);
        }
        "positive-control" => {
            // Inject ~50µs stall every 1000 samples.
            // If the instrument works, p999 and max should jump by ~50µs.
            // If they stay pinned, the instrument cannot measure jitter.
            println!("\n=== POSITIVE CONTROL — injecting {} ns jitter every {} samples ===",
                INJECT_NS, INJECT_INTERVAL);
            println!("Expected: ~{} injected samples out of {}, each ~{} ns",
                SAMPLES / INJECT_INTERVAL, SAMPLES, INJECT_NS + TICK_NS);
            println!("If p999 and max move by ~{} ns, the instrument CAN measure jitter.",
                INJECT_NS);
            println!("If they stay pinned at {} ns, the instrument CANNOT measure jitter.\n",
                TICK_NS);
            let stats = run_probe(true);
            print_stats("positive-control", &stats);
        }
        "tuned" => {
            let plan = build_topology()
                .expect("failed to build topology");
            match apply_tuning(&plan) {
                Ok(_guard) => {
                    let stats = run_probe(false);
                    print_stats("tuned", &stats);
                }
                Err(e) => {
                    eprintln!("TUNING FAILED (fail-closed): {}", e);
                    eprintln!("Running baseline probe instead.");
                    let stats = run_probe(false);
                    print_stats("tuned-failed-fallback-baseline", &stats);
                }
            }
        }
        "delta" => {
            // Phase 1: baseline (no tuning, no adapter in scope)
            println!("=== §4.5 Jitter Probe — BEFORE (baseline) ===");
            let before = run_probe(false);
            print_stats("before", &before);

            // Phase 2: apply tuning, keep adapter alive, then measure
            println!("\n=== §4.5 Jitter Probe — AFTER (tuned) ===");
            let plan = build_topology()
                .expect("failed to build topology");
            match apply_tuning(&plan) {
                Ok(guard) => {
                    println!("tuning: affinity=applied, priority=HIGH, timer={}ms, locked={}bytes",
                        guard.timer_res_ms, guard.locked_bytes);
                    let after = run_probe(false);
                    print_stats("after", &after);

                    println!("\n=== DELTA ===");
                    println!("p50:  {}ns -> {}ns (delta {}ns)",
                        before.p50_ns, after.p50_ns,
                        after.p50_ns as i64 - before.p50_ns as i64);
                    println!("p99:  {}ns -> {}ns (delta {}ns)",
                        before.p99_ns, after.p99_ns,
                        after.p99_ns as i64 - before.p99_ns as i64);
                    println!("p999: {}ns -> {}ns (delta {}ns)",
                        before.p999_ns, after.p999_ns,
                        after.p999_ns as i64 - before.p999_ns as i64);
                    println!("max:  {}ns -> {}ns (delta {}ns)",
                        before.max_ns, after.max_ns,
                        after.max_ns as i64 - before.max_ns as i64);
                }
                Err(e) => {
                    eprintln!("TUNING FAILED (fail-closed): {}", e);
                    eprintln!("No delta computed — tuning did not apply.");
                    eprintln!("Baseline: p99={}ns, max={}ns",
                        before.p99_ns, before.max_ns);
                }
            }
        }
        _ => {
            eprintln!("Usage: jitter_probe [baseline|tuned|delta|positive-control|qpf]");
            std::process::exit(1);
        }
    }
}
