//! Engine soak harness — criterion 99: sustained-load memory + latency stability.
//!
//! The removed soak proxy measured the CPython harness allocator, not the Rust
//! engine. This harness runs the REAL engine at sustained load for a fixed
//! duration, recording:
//!   - per-tick latency (p50/p95/p99/p99.9) over rolling windows
//!   - memory delta (RSS) before/after — via Windows GetProcessMemoryInfo
//!   - tick throughput (ticks/sec)
//!
//! The engine processes a steady stream of mixed events (MarketTrade,
//! OnchainConfirm, NarrativeSample, Tick) at high frequency for the configured
//! duration. Any memory leak or latency regression will manifest as either:
//!   (a) RSS growth over time, or
//!   (b) latency p99 degradation in later windows vs early windows.
//!
//!   cargo run --release --manifest-path bench/Cargo.toml --bin engine-soak \
//!       [--duration 60] [--mints 256] [--tick-rate 5000]
//!
//! Build: RUSTFLAGS="-C target-cpu=znver5" cargo build --release -j 16

use std::env;
use std::hint::black_box;
use std::time::{Duration, Instant};

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_domain::ids::Mint;

/// Read current process RSS in bytes.
///
/// On Windows: calls GetProcessMemoryInfo (psapi) via a minimal FFI binding
/// declared inline — no external crate dependency. Returns 0 if the call
/// fails (caller reports measurement unavailable).
///
/// On non-Windows: returns 0 (caller reports measurement unavailable).
#[cfg(windows)]
fn rss_bytes() -> u64 {
    use std::mem;

    #[repr(C)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    extern "system" {
        fn GetCurrentProcess() -> usize;
        fn K32GetProcessMemoryInfo(
            process: usize,
            counters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
    }

    unsafe {
        let mut counters: ProcessMemoryCounters = mem::zeroed();
        counters.cb = mem::size_of::<ProcessMemoryCounters>() as u32;
        let process = GetCurrentProcess();
        let ok = K32GetProcessMemoryInfo(
            process,
            &mut counters,
            mem::size_of::<ProcessMemoryCounters>() as u32,
        );
        if ok != 0 {
            counters.working_set_size as u64
        } else {
            0
        }
    }
}

#[cfg(not(windows))]
fn rss_bytes() -> u64 {
    0
}

fn pct(samples: &mut [u64], p: f64) -> u64 {
    samples.sort_unstable();
    let idx = (((p / 100.0) * samples.len() as f64).ceil() as usize).saturating_sub(1);
    samples[idx.min(samples.len() - 1)]
}

fn mint(tag: u64) -> Mint {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&tag.to_le_bytes());
    b[8] = 0xAB;
    Mint(b)
}

/// Generate a mixed event stream that exercises every lane:
/// MarketTrade (price discovery), OnchainConfirm (depth), NarrativeSample
/// (social), and periodic Tick (gate/settlement).
fn next_event(mint_idx: u64, tick_idx: u64) -> AppEvent {
    let mt = mint(mint_idx % 256);
    match tick_idx % 7 {
        0..=2 => AppEvent::MarketTrade {
            mint: mt,
            price_fp: 1_000_000_000 + (tick_idx as i128) * 1_000_000,
            quote_lamports: 500_000,
            liquidity_lamports: 100_000_000,
            signed_base: 1_000_000,
            buyer_entity: tick_idx as u64,
            age_slots: 20,
        },
        3 => AppEvent::OnchainConfirm {
            mint: mt,
            virtual_sol_lamports: 30_000_000_000,
            real_sol_lamports: 200_000_000,
        },
        4 => AppEvent::NarrativeSample {
            mint: mt,
            prior_active: 10,
            new_mentions: 200,
        },
        _ => AppEvent::Tick,
    }
}

fn parse_arg(args: &[String], flag: &str, default: u64) -> u64 {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == flag {
            if let Some(val) = iter.next() {
                if let Ok(n) = val.parse::<u64>() {
                    return n;
                }
            }
        }
    }
    default
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let duration_secs = parse_arg(&args, "--duration", 60);
    let n_mints = parse_arg(&args, "--mints", 256);
    let tick_rate = parse_arg(&args, "--tick-rate", 5000);

    println!("== engine soak harness (release) ==");
    println!(
        "  duration: {duration_secs}s, mints: {n_mints}, target tick-rate: {tick_rate}/s"
    );
    println!();

    // Build and warm the engine.
    let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    for m in 0..n_mints {
        let mt = mint(m);
        for i in 0..4u64 {
            eng.tick(AppEvent::MarketTrade {
                mint: mt,
                price_fp: 1_000_000_000 + (i as i128) * 1_000_000,
                quote_lamports: 500_000,
                liquidity_lamports: 100_000_000,
                signed_base: 1_000_000,
                buyer_entity: i,
                age_slots: 20,
            });
        }
        eng.tick(AppEvent::OnchainConfirm {
            mint: mt,
            virtual_sol_lamports: 30_000_000_000,
            real_sol_lamports: 200_000_000,
        });
    }
    for _ in 0..8 {
        eng.tick(AppEvent::Tick);
    }

    // Measure RSS before.
    let rss_before = rss_bytes();
    println!(
        "  RSS before: {rss_before} bytes ({:.1} MB)",
        rss_before as f64 / 1e6
    );

    // Run sustained load.
    let target = Duration::from_secs(duration_secs);
    let window_secs = 10;
    let tick_interval_us = 1_000_000u64 / tick_rate;

    let mut all_latencies: Vec<u64> = Vec::new();
    let mut window_latencies: Vec<u64> = Vec::new();
    let mut tick_count = 0u64;
    let mut mint_idx = 0u64;
    let mut window_idx = 0u64;
    let start = Instant::now();
    let mut next_window = start + Duration::from_secs(window_secs);

    print!("  window 0: ");
    loop {
        let now = Instant::now();
        if now >= next_window {
            // Report window stats.
            if !window_latencies.is_empty() {
                let mut s = window_latencies.clone();
                let w_p50 = pct(&mut s, 50.0);
                let w_p95 = pct(&mut s, 95.0);
                let w_p99 = pct(&mut s, 99.0);
                let w_p999 = pct(&mut s, 99.9);
                println!(
                    "p50 {w_p50:>6}ns p95 {w_p95:>6}ns p99 {w_p99:>6}ns p999 {w_p999:>6}ns [{} ticks]",
                    window_latencies.len()
                );
                all_latencies.extend(window_latencies.drain(..));
            }
            window_idx += 1;
            next_window = start + Duration::from_secs((window_idx + 1) * window_secs);
            if start.elapsed() >= target {
                break;
            }
            print!("  window {window_idx}: ");
        }

        let event = next_event(mint_idx, tick_count);
        let t = Instant::now();
        eng.tick(black_box(event));
        let lat = t.elapsed().as_nanos() as u64;
        window_latencies.push(lat);
        tick_count += 1;
        mint_idx += 1;

        // Pace to target tick-rate (best-effort; don't sleep below 1us).
        if tick_interval_us > 0 {
            let elapsed_us = t.elapsed().as_micros() as u64;
            if elapsed_us < tick_interval_us {
                std::thread::sleep(Duration::from_micros(tick_interval_us - elapsed_us));
            }
        }
    }

    let elapsed = start.elapsed();
    let rss_after = rss_bytes();

    // Aggregate stats.
    if all_latencies.is_empty() {
        all_latencies.extend(window_latencies.drain(..));
    }
    let mut s = all_latencies.clone();
    let p50 = pct(&mut s, 50.0);
    let p95 = pct(&mut s, 95.0);
    let p99 = pct(&mut s, 99.0);
    let p999 = pct(&mut s, 99.9);
    let max_lat = *s.iter().max().unwrap_or(&0);

    let tps = tick_count as f64 / elapsed.as_secs_f64();

    println!();
    println!(
        "== soak summary ({} ticks in {:.1}s, {tps:.0} ticks/s) ==",
        tick_count,
        elapsed.as_secs_f64()
    );
    println!(
        "  RSS before: {rss_before} bytes ({:.1} MB)",
        rss_before as f64 / 1e6
    );
    println!(
        "  RSS after:  {rss_after} bytes ({:.1} MB)",
        rss_after as f64 / 1e6
    );
    let rss_delta = rss_after as i64 - rss_before as i64;
    println!(
        "  RSS delta:  {rss_delta} bytes ({:.1} MB)",
        rss_delta as f64 / 1e6
    );
    println!();
    println!("  latency p50:  {p50:>8} ns");
    println!("  latency p95:  {p95:>8} ns");
    println!("  latency p99:  {p99:>8} ns");
    println!("  latency p99.9:{p999:>8} ns");
    println!("  latency max:  {max_lat:>8} ns");
    println!();
    if rss_bytes() == 0 {
        println!("  ⚠ RSS measurement unavailable on this platform — memory stability UNVERIFIED");
    } else if rss_delta > 0 {
        println!("  ⚠ RSS grew by {rss_delta} bytes — investigate for memory leak");
    } else {
        println!("  ✓ RSS stable or shrank — no memory leak detected");
    }
    println!();
    println!("(criterion 99 evidence: engine memory + latency under sustained load)");
}
