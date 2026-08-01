//! §4.5 jitter probe — measures scheduling jitter before/after OS tuning.
//!
//! Run modes:
//!   `jitter_probe baseline`   — measure jitter WITHOUT any tuning
//!   `jitter_probe tuned`      — apply OS tuning, then measure jitter
//!   `jitter_probe delta`      — run baseline, then tuned, report delta
//!
//! The probe samples inter-tick deltas in a tight loop, feeding them to
//! `jitter_stats` from `pump-quant-core`. The `tuned` mode constructs a
//! `WinOsTune` adapter, derives a pin plan for one hot thread on the
//! current box's group-0 cores, applies it, sets HIGH_PRIORITY_CLASS,
//! sets a 1ms timer resolution, then runs the probe.
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

fn run_probe() -> JitterStats {
    let mut deltas: Vec<u64> = Vec::with_capacity(SAMPLES);
    let mut prev = Instant::now();
    for _ in 0..SAMPLES {
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
    jitter_stats(&deltas)
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

    // Box the adapter so it lives on the heap and outlives this function.
    // The TuningGuard holds it, and the caller keeps the guard alive.
    let mut os = Box::new(WinOsTune::new(64 * 1024 * 1024) // 64 MiB lock budget
        .map_err(|e| format!("WinOsTune::new failed: {:?}", e))?);

    // Timer resolution: 1 ms (timeGetDevCaps + timeBeginPeriod)
    let timer_res = os.set_timer_res_ms(1)
        .map_err(|e| format!("set_timer_res_ms failed: {:?}", e))?;

    // Apply the pin plan with HIGH priority (NOT realtime per §66/109)
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

    // Lock a sentinel region to exercise VirtualLock (64 KiB).
    // The sentinel Vec stays alive in the TuningGuard.
    let sentinel = vec![0u8; 64 * 1024];
    let locked = unsafe {
        os.lock_region(sentinel.as_ptr(), sentinel.len())
    }.map_err(|e| format!("lock_region failed: {:?}", e))?;
    if locked == 0 {
        return Err("lock_region returned 0 bytes".to_string());
    }

    Ok(TuningGuard {
        _os: os,           // adapter alive → timer + lock persist
        _sentinel: sentinel, // locked region alive → VirtualLock valid
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

    match mode.as_str() {
        "baseline" => {
            let stats = run_probe();
            print_stats("baseline", &stats);
        }
        "tuned" => {
            let plan = build_topology()
                .expect("failed to build topology");
            match apply_tuning(&plan) {
                Ok(_guard) => {
                    // _guard holds the adapter alive for the probe duration.
                    let stats = run_probe();
                    print_stats("tuned", &stats);
                    // _guard drops here → timer resets, memory unlocks.
                }
                Err(e) => {
                    eprintln!("TUNING FAILED (fail-closed): {}", e);
                    eprintln!("Running baseline probe instead.");
                    let stats = run_probe();
                    print_stats("tuned-failed-fallback-baseline", &stats);
                }
            }
        }
        "delta" => {
            // Phase 1: baseline (no tuning, no adapter in scope)
            println!("=== §4.5 Jitter Probe — BEFORE (baseline) ===");
            let before = run_probe();
            print_stats("before", &before);

            // Phase 2: apply tuning, keep adapter alive, then measure
            println!("\n=== §4.5 Jitter Probe — AFTER (tuned) ===");
            let plan = build_topology()
                .expect("failed to build topology");
            match apply_tuning(&plan) {
                Ok(guard) => {
                    // guard holds WinOsTune + sentinel alive.
                    // Timer resolution and VirtualLock are ACTIVE.
                    println!("tuning: affinity=applied, priority=HIGH, timer={}ms, locked={}bytes",
                        guard.timer_res_ms, guard.locked_bytes);
                    let after = run_probe();
                    print_stats("after", &after);
                    // guard drops here → all tuning undone.

                    // Delta
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
            eprintln!("Usage: jitter_probe [baseline|tuned|delta]");
            std::process::exit(1);
        }
    }
}
