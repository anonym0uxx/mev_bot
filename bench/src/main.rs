//! Dependency-free latency harness for the pump-quant hot paths.
//!
//! No criterion, no external deps: just `std::time::Instant` + `std::hint::black_box`,
//! warmup, and percentile reporting. Standalone crate (not a workspace member) so the
//! gated workspace never sees wall-clock code. Build with `--release`.
//!
//!   cargo run --release --manifest-path bench/Cargo.toml
//!
//! Absolute nanoseconds are box-specific; the point is the BEFORE/AFTER delta on one box.

use std::hint::black_box;
use std::time::Instant;

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_core::fixedpoint::mul_div_u128;
use pump_quant_domain::ids::Mint;
use pump_quant_protocol::registry::Venue;
use pump_quant_protocol::decode::decode_pump_curve;
use pump_quant_protocol::registry::account_discriminator;

fn pct(samples: &mut [u64], p: f64) -> u64 {
    samples.sort_unstable();
    let idx = (((p / 100.0) * samples.len() as f64).ceil() as usize).saturating_sub(1);
    samples[idx.min(samples.len() - 1)]
}

/// Time `f` once per sample, `n` samples, after `warm` warmup runs. ns per call.
fn bench_each<F: FnMut() -> u64>(name: &str, warm: usize, n: usize, mut f: F) {
    for _ in 0..warm {
        black_box(f());
    }
    let mut s = Vec::with_capacity(n);
    for _ in 0..n {
        let t = Instant::now();
        black_box(f());
        s.push(t.elapsed().as_nanos() as u64);
    }
    report(name, &mut s, 1);
}

/// Time `batch` calls of `f` per sample (for sub-clock-resolution ops), `n` samples.
fn bench_batched<F: FnMut()>(name: &str, batch: usize, warm: usize, n: usize, mut f: F) {
    for _ in 0..warm * batch {
        f();
    }
    let mut s = Vec::with_capacity(n);
    for _ in 0..n {
        let t = Instant::now();
        for _ in 0..batch {
            f();
        }
        s.push(t.elapsed().as_nanos() as u64);
    }
    report(name, &mut s, batch as u64);
}

fn report(name: &str, s: &mut [u64], div: u64) {
    let min = *s.iter().min().unwrap() / div;
    let p50 = pct(s, 50.0) / div;
    let p99 = pct(s, 99.0) / div;
    let p999 = pct(s, 99.9) / div;
    println!(
        "{name:<34} min {min:>8} ns   p50 {p50:>8} ns   p99 {p99:>8} ns   p99.9 {p999:>8} ns",
    );
}

fn mint(tag: u64) -> Mint {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&tag.to_le_bytes());
    b[8] = 0xAB; // keep it non-zero / realistic
    Mint(b)
}

/// Build a warmed engine + a stream of pure Tick events representing steady state.
fn engine_scenario(n_mints: u64) -> (Engine, Vec<AppEvent>) {
    let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    // Warm: many mints with numeric flow + on-chain confirm, so lanes + confirmed set
    // + watchlist are populated and the gate actually fires each tick.
    for m in 0..n_mints {
        let mt = mint(m);
        for i in 0..4u64 {
            eng.tick(AppEvent::MarketTrade {
                mint: mt,
                liquidity_lamports: 100_000_000,
                signed_base: 1_000_000,
                buyer_entity: i,
                age_slots: 20,
            });
        }
        eng.tick(AppEvent::OnchainConfirm {
            mint: mt,
            sellable_depth_lamports: 200_000_000,
        });
        if m % 3 == 0 {
            eng.tick(AppEvent::NarrativeSample {
                mint: mt,
                prior_active: 10,
                new_mentions: 200,
            });
        }
    }
    // A few ticks to settle the watchlist.
    for _ in 0..8 {
        eng.tick(AppEvent::Tick);
    }
    (eng, vec![AppEvent::Tick])
}

fn main() {
    println!("== pump-quant latency harness (release) ==\n");

    // 1) fixed-point AMM math — the on-chain-path integer kernel.
    let (a, b, c) = (black_box(1_000_000_009u128), black_box(2_500u128), black_box(7u128));
    bench_batched("fixedpoint::mul_div_u128 (fast)", 4096, 50, 2000, || {
        black_box(mul_div_u128(black_box(a), black_box(b), black_box(c)));
    });
    let big = black_box(u128::MAX / 2);
    bench_batched("fixedpoint::mul_div_u128 (256-bit)", 4096, 50, 2000, || {
        black_box(mul_div_u128(black_box(big), black_box(big), black_box(c)));
    });

    // 2) identity-verified AMM decode — first thing that touches a fresh account.
    let disc = account_discriminator(Venue::PumpFun);
    let mut acct = vec![0u8; 49];
    acct[..8].copy_from_slice(&disc);
    acct[8..16].copy_from_slice(&1_000_000_000u64.to_le_bytes());
    acct[16..24].copy_from_slice(&30_000_000_000u64.to_le_bytes());
    let acct = black_box(acct);
    bench_batched("protocol::decode_pump_curve", 4096, 50, 2000, || {
        black_box(decode_pump_curve(black_box(&acct)));
    });

    // 3) THE hot path: steady-state per-tick engine latency.
    for &n in &[64u64, 256, 1024] {
        let (mut eng, ticks) = engine_scenario(n);
        bench_each(&format!("engine tick  [{n} mints]"), 200, 20_000, || {
            eng.tick(black_box(ticks[0]));
            black_box(eng.now())
        });
    }

    println!("\n(absolute ns are box-specific; compare deltas across builds on the same box)");
}
