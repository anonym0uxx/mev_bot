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

fn drive(cfg: Config) -> pump_quant_app::engine::Report {
    let mut eng = Engine::new(cfg, RunMode::Replay);
    // 512 mints against a capacity-64 watchlist => heavy full-path eviction.
    // Extended (ledger-refinement batch) with three §21.5/§21.6/§29.6 cohorts —
    // see the cohort blocks after the wave below.
    let n = 512u64;
    for round in 0..6u64 {
        // A deterministic pump-then-dump price wave (bps of a 1e9 base) so the
        // held-position lifecycle is actually exercised: positions open, take the
        // principal-recovery tranche near the top, and trail/hard-stop out on the
        // way down — freeing slots for later admits. Integer, fixed (§22).
        let round_mult_bp: u64 = [10_000, 12_000, 15_000, 13_000, 10_000, 8_500][round as usize];
        for m in 0..n {
            let mt = mint(m);
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
        // ---- §21.5 cohort: "zombie" markets — mature (age 200), deep pools,
        // active only in rounds 0-1, then a LATE on-chain confirm (round 3) for
        // tape nobody trades anymore. The universe screen must refuse them at
        // promotion; without it they would open and bleed round-trip costs.
        for z in 0..6u64 {
            let mt = mint(1_000 + z);
            if round <= 1 {
                for i in 0..3u64 {
                    eng.tick(AppEvent::MarketTrade {
                        mint: mt,
                        price_fp: 1_000_000_000 + (z as i128) * 5_000 + (i as i128) * 1_000,
                        quote_lamports: 900_000 + z * 1_000,
                        liquidity_lamports: 400_000_000 + z * 10_000,
                        signed_base: 800_000 + (z as i64) * 500,
                        buyer_entity: 200 + (z + i) % 9,
                        age_slots: 200,
                    });
                }
            }
            if round == 3 {
                eng.tick(AppEvent::OnchainConfirm {
                    mint: mt,
                    sellable_depth_lamports: 500_000_000,
                });
            }
        }
        // ---- §21.6 cohort: "dense" fresh launches — 24 zigzag trades per round
        // (three 8-trade bars on the trade-count clock), deep pools, a mostly-
        // pump wave with a sell-flow capitulation in the last round. Bars and
        // swing structure are computed on the live gate path for every entry;
        // deeper venues out-price the toy pools in §23 arbitration (§18: costs
        // decide), and the capitulation exercises VPIN/thesis exits.
        let dense_mult_bp: u64 = [10_000, 12_000, 15_000, 18_000, 20_000, 19_000][round as usize];
        for d in 0..4u64 {
            let mt = mint(2_000 + d);
            let base = 1_000_000_000i128 * dense_mult_bp as i128 / 10_000;
            let zig: [i128; 8] = [0, 15, -10, 20, -5, 25, 5, 30];
            for i in 0..24u64 {
                let zg = zig[(i % 8) as usize] + (i as i128 / 8) * 3;
                let selling = round == 5;
                eng.tick(AppEvent::MarketTrade {
                    mint: mt,
                    price_fp: base + zg * 1_000_000 + (d as i128) * 10_000,
                    quote_lamports: 700_000 + d * 2_000,
                    liquidity_lamports: 300_000_000 + d * 5_000,
                    signed_base: if selling {
                        -(900_000 + (d as i64) * 700)
                    } else {
                        900_000 + (d as i64) * 700 - (i as i64 * 100)
                    },
                    buyer_entity: 300 + (d + i) % 11,
                    age_slots: 10 + d as u32,
                });
            }
            if round == 0 {
                eng.tick(AppEvent::OnchainConfirm {
                    mint: mt,
                    sellable_depth_lamports: 400_000_000,
                });
            }
        }
        // ---- §29.6 cohort: "stale-narrative" mints — one blast in round 0 and
        // silence forever. The decay law fades their discovery rank continuously
        // toward the TTL cliff instead of letting a stale mention rank like
        // fresh evidence for 100 ticks.
        if round == 0 {
            for s in 0..8u64 {
                eng.tick(AppEvent::NarrativeSample {
                    mint: mint(3_000 + s),
                    prior_active: 5,
                    new_mentions: 5_000 + s * 100,
                });
            }
        }
        // ---- live-stream cohort: a coin WATCHED ON STREAM right now. Its
        // on-chain flow is balanced (below the numeric-lane discovery bar), so
        // WITHOUT the live-chat attention structure it is never discovered —
        // the §29.6 opportunity shape the Twitch lane exists to catch.
        let s_mult_bp: u64 = [10_000, 12_000, 15_000, 18_000, 20_000, 19_000][round as usize];
        let st = mint(4_000);
        let sbase = 1_000_000_000i128 * s_mult_bp as i128 / 10_000;
        for i in 0..8u64 {
            let selling = round == 5;
            eng.tick(AppEvent::MarketTrade {
                mint: st,
                price_fp: sbase + (i as i128) * 500_000,
                quote_lamports: 700_000,
                liquidity_lamports: 500_000_000,
                signed_base: if selling {
                    -800_000
                } else if i % 2 == 0 {
                    500_000
                } else {
                    -480_000
                },
                buyer_entity: 400 + i % 7,
                age_slots: 10,
            });
        }
        if round == 0 {
            eng.tick(AppEvent::OnchainConfirm {
                mint: st,
                sellable_depth_lamports: 500_000_000,
            });
        }
        // The stream chat (deterministic batch per round): the broadcaster names
        // ticker + mint, distinct chatters spam the ticker.
        {
            use pump_quant_ingest::social_source::{MockSocialSource, RawSocialPayload};
            let mint_b58 = "BmoVsKix7SdPJwY9PRDsX3jDux3rr78RHEycUwWod4qM";
            let ts0 = 1_000_000_000u64 + round * 60_000_000_000;
            let mut batch = vec![RawSocialPayload::new(
                format!("{{\"platform\":\"twitch\",\"author\":\"streamer\",\"community\":\"streamer\",\"text\":\"$LIVE {mint_b58} full send r{round}\",\"likes\":0,\"reposts\":0,\"replies\":0,\"echo\":false}}").into_bytes(),
                ts0,
            )];
            // The chat SNOWBALLS as the coin pumps (4, 8, 12, 16, ... distinct
            // chatters per round): rising live attention = positive velocity.
            let n_chat = (4 + round * 4).min(16);
            for c in 0..n_chat {
                batch.push(RawSocialPayload::new(
                    format!("{{\"platform\":\"twitch\",\"author\":\"chat{c}\",\"community\":\"streamer\",\"text\":\"$LIVE lfg {c} r{round}\",\"likes\":0,\"reposts\":0,\"replies\":0,\"echo\":false}}").into_bytes(),
                    ts0 + (c + 1) * 1_000_000,
                ));
            }
            let mut src = MockSocialSource::new().with_batch(batch);
            eng.ingest_social(&mut src);
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
// Re-pin #3 (ledger-closure batch): config-hash-seeded digest, §23 expected-net
// arbitration, §33 probe→confirm→scale, §32 thesis exits, §21.7 authenticity +
// phase exit-cost law, §27 creator credibility, §21.3 regime consumption. Three
// concentrated admits netted +6_443_936 on the original 512-mint tape.
// Re-pin #4 (ledger-refinement batch): the TAPE ITSELF was extended (as when the
// exit lifecycle landed) with three cohorts exercising the §21.5 universe screen
// (zombie markets — filtered, never entered), §21.6 trade-count bars + swing
// structure (dense fresh launches on cheaper venues — they out-price the toy
// pools in §23 arbitration, §18), and §29.6 attention decay (stale narrative
// blasts fade instead of squatting rank). Net on the extended tape:
// +8_785_954 (arc: 2_979_624 → 5_017_234 → 6_443_936 → 8_785_954). The per-law
// causal deltas are pinned separately in `batch_e_laws.rs` A/B tests — each law
// strictly out-earns its own absence on its hazard tape.
// Re-pin #6 (Twitch/live-stream batch): the tape gained a live-stream cohort —
// a coin whose on-chain flow sits BELOW the numeric discovery bar but which is
// being watched on stream (broadcaster call + snowballing distinct chat, fed
// through ingest_social with the capture lane's exact NDJSON) — and the §71
// union-preservation quota landed: building the Twitch lane exposed that raw
// rank let numeric scores (~10^5) monopolize every promotion slot over the
// fade-capped (§29, ≤10^3) corroboration lanes, a de-facto intersection. With
// 2 of 8 slots reserved for gate-viable corroboration evidence, the streamed
// coin is discovered, admitted, and rides: net 8_785_954 → **12_550_767**
// (arc: 2_979_624 → 5_017_234 → 6_443_936 → 8_785_954 → 12_550_767). The
// quota's causal delta is pinned by `corroboration_quota_earns_on_this_tape`
// below; the twitch-vs-x arms tie BY DESIGN (§29.8: no per-platform trust in
// the quality path) — the Twitch-specific laws are pinned in attention/e2e.
// Re-pin #5 (Phase-A alignment batch): SEED-ONLY re-pin — every decision-level
// constant below is UNCHANGED (same promoted/admitted/rejected/net on the same
// tape). The digest moved solely because the §19 config-identity seed gained
// the new integrity keys (scale_confirm_auth_min_bp, expectancy_min_lane_trades)
// while the fail-open holes they close (neutral-prior scale-in, stale numeric
// snapshots, unknown exit cost, uncross-checked confirm depth) were repaired
// without changing any golden decision — the tape never exercised the holes.
const GOLDEN_DIGEST: u64 = 16_905_668_354_419_895_265;
const GOLDEN_NET_LAMPORTS: i128 = 12_550_767;
const GOLDEN_PROMOTED: u64 = 504;
const GOLDEN_ADMITTED: u64 = 17;
const GOLDEN_REJECTED: u64 = 487;
/// Zombie-cohort promotions the §21.5 screen must remove (visible activity).
const GOLDEN_UNIVERSE_FILTERED: u64 = 72;

#[test]
fn golden_digest_is_stable() {
    let r = drive(Config::dev_portable());
    // Print for inspection (`cargo test -- --nocapture`).
    println!(
        "GOLDEN ticks={} promoted={} admitted={} rejected={} net={} digest={} per_lane={:?} weights={:?}",
        r.ticks, r.promoted, r.admitted, r.rejected, r.net_lamports, r.journal_digest,
        r.per_lane_net, r.final_weights
    );
    // Determinism: identical inputs reproduce the identical report.
    let r2 = drive(Config::dev_portable());
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
    assert_eq!(
        r.universe_filtered, GOLDEN_UNIVERSE_FILTERED,
        "§21.5 screen activity drifted"
    );
}

/// The §71 quota's causal lamports on THIS tape: identical events, quota 2 vs
/// quota 0 (the pre-quota engine). The streamed coin is only reachable through
/// the reserved corroboration slots, and it pays.
#[test]
fn corroboration_quota_earns_on_this_tape() {
    let with_quota = drive(Config::dev_portable());
    let mut cfg0 = Config::dev_portable();
    cfg0.promote_corroboration_quota = 0;
    let without = drive(cfg0);
    assert!(
        with_quota.net_lamports > without.net_lamports,
        "the union-preservation quota must strictly out-earn its absence ({} vs {})",
        with_quota.net_lamports,
        without.net_lamports
    );
}
