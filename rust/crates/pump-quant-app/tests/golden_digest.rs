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
            // A deterministic pump-then-dump price wave (bps of a 1e9 base) so the
            // held-position lifecycle is actually exercised: positions open, take the
            // principal-recovery tranche near the top, and trail/hard-stop out on the
            // way down — freeing slots for later admits. Integer, fixed (§22).
            let round_mult_bp: u64 =
                [10_000, 12_000, 15_000, 13_000, 10_000, 8_500][round as usize];
            for i in 0..3u64 {
                eng.tick(AppEvent::MarketTrade {
                    mint: mt,
                    price_fp: (1_000_000_000i128 * round_mult_bp as i128 / 10_000)
                        + (i as i128) * 1_000_000
                        + (m as i128) * 1_000,
                    quote_lamports: 400_000 + (m % 13) * 1_000,
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

/// The byte-exact outcome of [`drive`], frozen. **Deliberately re-pinned twice**:
/// first when the engine moved from a one-shot fill to the held-position exit
/// lifecycle (§24; prior pin 13612654632551201076, admitted 16, net 2_979_624),
/// then when Batch C landed dynamic bankroll sizing + risk budgets (§33 — sizes now
/// derive from deployable capital under a 3-position concurrency cap, and refused
/// admits are JOURNALED, hence rejected 570), evidence-staleness gates (§34.3),
/// VPIN-X toxicity + Roll-regime multipliers, and the exit-reason field in every
/// Filled journal record. Fewer, properly-sized positions now net MORE
/// (+5_017_234 vs +2_979_624). From here this value is again the frozen tripwire —
/// any future move that is not a deliberate re-pin is a regression (§22, §54).
const GOLDEN_DIGEST: u64 = 6_031_070_496_308_012_732;
const GOLDEN_NET_LAMPORTS: i128 = 5_017_234;
const GOLDEN_PROMOTED: u64 = 576;
const GOLDEN_ADMITTED: u64 = 6;
const GOLDEN_REJECTED: u64 = 570;

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
