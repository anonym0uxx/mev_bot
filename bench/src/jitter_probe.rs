//! Jitter probe sampler — measures OS scheduling jitter on the current thread.
//!
//! Spins for N iterations reading a high-resolution counter, records inter-arrival
//! deltas, and reports p50/p99/p999/max via `jitter_stats` from pump-quant-core.
//!
//! This is the sampler that OSTUNE_BUILD_SPEC.md §7 says "does not exist".
//! `jitter_stats` aggregates samples nobody produces — this produces them.
//!
//! Run before and after OsTune pinning to measure the delta. The absolute numbers
//! are box-specific; the BEFORE/AFTER delta on one box is the evidence.
//!
//!   cargo run --release --manifest-path bench/Cargo.toml --bin jitter-probe [--samples 50000]
//!
//! Standalone binary, no external deps. Uses std::time::Instant (sub-microsecond
//! resolution on Windows via QueryPerformanceCounter).

use std::env;
use std::hint::black_box;
use std::time::Instant;

fn main() {
    let args: Vec<String> = env::args().collect();
    let mut n: usize = 50_000;
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--samples" => {
                if let Some(val) = iter.next() {
                    if let Ok(parsed) = val.parse::<usize>() {
                        n = parsed;
                    }
                }
            }
            s if !s.starts_with("--") => {
                if let Ok(parsed) = s.parse::<usize>() {
                    n = parsed;
                }
            }
            _ => {}
        }
    }

    // Warmup: 2000 iterations to stabilize the cache and branch predictor.
    let warm = 2_000.min(n / 10);
    for _ in 0..warm {
        black_box(Instant::now());
    }

    // Collect inter-arrival deltas. Each iteration: read clock, compute delta
    // from previous reading, store delta in nanoseconds.
    let mut deltas_ns: Vec<u64> = Vec::with_capacity(n);
    let mut prev = Instant::now();
    for _ in 0..n {
        let now = Instant::now();
        let delta = now.duration_since(prev).as_nanos() as u64;
        deltas_ns.push(delta);
        prev = now;
        // Prevent the compiler from eliminating the loop.
        black_box(&mut deltas_ns);
    }

    // Report via the same jitter_stats used by the pin plan evidence.
    use pump_quant_core::cpu_numa_tuning::jitter_stats;
    let stats = jitter_stats(&deltas_ns);

    println!("== jitter probe ({n} samples, {warm} warmup) ==");
    println!("  p50:   {:>10} ns", stats.p50_ns);
    println!("  p99:   {:>10} ns", stats.p99_ns);
    println!("  p999:  {:>10} ns", stats.p999_ns);
    println!("  max:   {:>10} ns", stats.max_ns);
    println!("  n:     {:>10}", stats.n);
    println!();
    println!("(run before and after OsTune pinning; the delta is the evidence)");
}
