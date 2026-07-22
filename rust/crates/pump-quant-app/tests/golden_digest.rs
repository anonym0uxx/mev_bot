//! Golden determinism regression: a rich multi-lane scenario whose decision-journal
//! digest and report must never change under behaviour-preserving optimization.
//!
//! Exercises all four lanes, on-chain confirms, capacity eviction (mints ≫ capacity),
//! recency pruning, promotion, gating/scalping, and the reflection cadence — the full
//! `evaluate()` surface — over many ticks, then pins the byte-exact outcome.

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_domain::ids::Mint;

fn mint(tag: u64) -> Mint {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&tag.to_le_bytes());
    b[8] = 0xAB;
    Mint::from_bytes(b)
}

fn drive() -> pump_quant_app::engine::Report {
    let mut eng = Engine::new(Config::dev_portable(), RunMode::Replay);
    // 512 mints against a capacity-64 watchlist => heavy full-path eviction.
    let n = 512u64;
    for round in 0..6u64 {
        for m in 0..n {
            let mt = mint(m);
            for i in 0..3u64 {
                eng.tick(AppEvent::MarketTrade {
                    mint: mt,
                    liquidity_lamports: 50_000_000 + m * 1000 + round * 7,
                    signed_base: 500_000 + (m as i64 % 13) * 1000 - (i as i64 * 100),
                    buyer_entity: (m + i) % 97,
                    age_slots: 10 + (m as u32 % 40),
                });
            }
            if m % 2 == 0 {
                eng.tick(AppEvent::OnchainConfirm {
                    mint: mt,
                    sellable_depth_lamports: 150_000_000 + m * 500,
                });
            }
            if m % 3 == 0 {
                eng.tick(AppEvent::NarrativeSample {
                    mint: mt,
                    prior_active: 5 + m % 11,
                    new_mentions: 100 + m * 3,
                });
            }
            if m % 5 == 0 {
                eng.tick(AppEvent::SocialCall {
                    mint: mt,
                    source_quality_bp: 2000 + (m as u32 % 500),
                });
            }
            if m % 7 == 0 {
                eng.tick(AppEvent::WalletAction {
                    mint: mt,
                    followable: m % 2 == 0,
                    size_lamports: 10_000_000 + m * 2000,
                });
            }
        }
        for _ in 0..12 {
            eng.tick(AppEvent::Tick);
        }
    }
    eng.report()
}

/// The byte-exact outcome of [`drive`], frozen. This value was produced by the
/// pre-optimization engine and re-verified, unchanged, after the latency work
/// (scratch-buffer discovery, alloc-free watchlist merge, memoized weakest-entry
/// eviction, `ilog10` decade). Any future change that moves it is a behavioural
/// change, not an optimization — this test is the tripwire (§22, §54).
const GOLDEN_DIGEST: u64 = 17_194_072_179_380_622_382;
const GOLDEN_NET_LAMPORTS: i128 = 3_766_464;
const GOLDEN_PROMOTED: u64 = 576;
const GOLDEN_ADMITTED: u64 = 288;
const GOLDEN_REJECTED: u64 = 288;

#[test]
fn golden_digest_is_stable() {
    let r = drive();
    // Print for inspection (`cargo test -- --nocapture`).
    println!(
        "GOLDEN ticks={} promoted={} admitted={} rejected={} net={} digest={} per_lane={:?} weights={:?}",
        r.ticks, r.promoted, r.admitted, r.rejected, r.net_lamports, r.journal_digest,
        r.per_lane_net, r.final_weights
    );
    // Determinism: identical inputs reproduce the identical report.
    let r2 = drive();
    assert_eq!(r, r2, "same events -> identical report");
    // Frozen golden outcome: optimizations must be behaviour-preserving.
    assert_eq!(
        r.journal_digest, GOLDEN_DIGEST,
        "decision-journal digest drifted"
    );
    assert_eq!(
        r.net_lamports, GOLDEN_NET_LAMPORTS,
        "realized net-SOL drifted"
    );
    assert_eq!(r.promoted, GOLDEN_PROMOTED, "promotion count drifted");
    assert_eq!(r.admitted, GOLDEN_ADMITTED, "admission count drifted");
    assert_eq!(r.rejected, GOLDEN_REJECTED, "rejection count drifted");
}
