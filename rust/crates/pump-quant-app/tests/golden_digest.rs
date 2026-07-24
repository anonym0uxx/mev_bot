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

/// LAW D1/D4/D5 golden alpha cohort: two fresh, valid Solana pubkeys named by a
/// PAID Discord alpha room. `ALPHA_WIN_B58` earns an on-chain confirm + real
/// microstructure (admits, rides, profits — attributed to the `AlphaCall`
/// discovery lane and the room's §29.8 outcome ledger); `ALPHA_NOCONFIRM_B58`
/// has NO on-chain support (no trades, no confirm), so alpha alone can never
/// admit it (LAW D4). Both decode to full 32-byte keys distinct from every
/// `mint(tag)` (which are `tag_le ++ 0xAB ++ 0…`), so there is no collision.
const ALPHA_WIN_B58: &str = "7GCihgDB8fe6KNjn2MYtkzZcRjQy3t9GHdC8uHYmW2hr";
const ALPHA_NOCONFIRM_B58: &str = "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263";

/// Decode a golden-cohort base58 pubkey to a `Mint` (valid by construction).
fn b58_mint(s: &str) -> Mint {
    Mint::from_bytes(pump_quant_ingest::base58::decode_pubkey(s).expect("valid golden pubkey"))
}

/// Deterministic per-mint scalp **trajectory** over the six-round tape (no RNG —
/// §22). A multiplicative-hash avalanche of the mint tag spreads the 512 markets
/// across a REALISTIC pump.fun/PumpSwap low-cap outcome distribution instead of the
/// old pathological "every market grinds to 1.5×–2× then craters" wave (which
/// structurally rewarded ONE fixed 13_500/25_000/50_000 ladder — the forbidden
/// §24 constants). The mix, drawn from the memecoin scalp research (project docs,
/// arxiv MC): **~45% quick losers** (fade/rug, never clear the round-trip cost),
/// **~35% small→mid winners** straddling the cost-derived (~1.16×) and the old
/// fixed (1.35×) first rung, **~15% moderate winners**, **~5% runners** (2.5×–6×).
/// Returns `(price_fp, signed_base)` at `(round, i)`. Rises to a per-mint peak by
/// round 2–3 on net-buy flow, then fades on net-sell flow — the honest order-flow
/// shape the CVD/precursor/trailing exits actually read.
fn main_scalp(m: u64, round: u64, i: u64) -> (i128, i64) {
    // Avalanche mix (SplitMix64-style constants) — same tag ⇒ same trajectory.
    let h = m
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(0x1234_5678_9ABC_DEF0)
        .rotate_left(29)
        ^ m.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    let bucket = h % 1_000; // outcome class
    let spread = (h / 1_000) % 1_000; // within-class magnitude spread

    // Terminal peak multiple (bps of entry; 10_000 = break-even). Realistic
    // pump.fun scalp OUTCOME mix: ~42% quick losers, ~42% small→mid winners
    // straddling the cost-derived (~1.16×) and old fixed (1.35×) first rung, ~16%
    // moderate winners/runners. The right tail is BOUNDED at ~2.2× so the golden
    // reference stays stable and no admit chases a catastrophic 5× fade; an
    // unbounded tail is exactly where the fixed 2.5×/5× rungs would reclaim their
    // edge, so a bounded tail is the CONSERVATIVE representative choice.
    let peak_bp: u64 = if bucket < 420 {
        10_000 + spread % 300 // 1.00×..1.03× — never clears the round-trip cost
    } else if bucket < 840 {
        11_400 + spread * 2_600 / 1_000 // 1.14×..1.40× — the fat middle
    } else {
        14_000 + spread * 8_000 / 1_000 // 1.40×..2.20× — moderate winners/runners
    };
    // Settled (plateau) multiple the market holds from round 4 on: losers settle at
    // 0.80×..0.90× (a scalper's stop/precursor caps the downside — not a −45% rug);
    // winners give back ~half the excursion to a plateau (a realistic post-pump
    // consolidation, so late re-admits enter at a settled level, not a peak).
    let settle_bp: u64 = if bucket < 420 {
        8_000 + spread % 1_000
    } else {
        10_000 + (peak_bp - 10_000) / 2
    };

    // Rounds 0-1 are a GENERIC early-launch phase: every market drifts ~1.00×→1.03×
    // on mild buy flow, indistinguishable by outcome — discovery cannot front-run
    // which coin will pump (a real launch reveals nothing at t≈0), so the admitted
    // set samples the FULL distribution, losers included. Round 2-3 reveal the
    // per-mint outcome (peak then a partial give-back); round 4-5 hold the settled
    // plateau so a late re-admit enters at the consolidated level.
    let peak_round = 2u64;
    let cur_bp: u64 = match round {
        0 => 10_000,
        1 => 10_300,
        2 => peak_bp,
        3 => (peak_bp + settle_bp) / 2, // partial give-back
        _ => settle_bp,                 // rounds 4-5: settled plateau
    };
    // Intra-round micro-drift so the three prints per round differ (microstructure /
    // CVD / realized-vol inputs), plus a tag offset to keep every mint distinct.
    let micro = (i as i128) * (cur_bp as i128 / 100);
    let price_fp = 1_000_000_000i128 * cur_bp as i128 / 10_000 + micro + (m as i128) * 1_000;
    // Buy flow while rising (early phase + up to the peak), sell flow on the fade.
    let rising = round <= 1 || round <= peak_round;
    let signed_base: i64 = if rising {
        500_000 + (m as i64 % 13) * 1000 - (i as i64 * 100)
    } else {
        -(500_000 + (m as i64 % 13) * 1000) + (i as i64 * 100)
    };
    (price_fp, signed_base)
}

fn drive(cfg: Config) -> pump_quant_app::engine::Report {
    // ---- Cost-realism: model a REALISTIC low-cap Solana memecoin scalp round-trip.
    // The default `dev_portable` economics (protocol 100 bps, fixed 50k lamports,
    // impact_den 1e6) yield a ~150–190 bps round-trip — far too cheap, which
    // collapsed the cost-derived exits to a ~1.02× target and let the forbidden
    // fixed 1.35× rung win. Real frictions on a ~0.008–0.015 SOL clip
    // (docs/PUMPSWAP_DECODE.md — dynamic market-cap-tiered fees via `pfeeUxB6…`):
    //   • swap fee (tiered, ~1%/side low-mcap)         → ~200 bps round trip
    //   • LP + protocol + coin-creator fee (~0.28%/side)→  ~55 bps round trip
    //   • bid/ask spread on a thin low-cap (~1%/side)   → ~200 bps round trip
    //     ⇒ size-invariant protocol/fee/spread ≈ 450 bps (gate_protocol_bps).
    //   • priority fee + Jito tip, both legs ≈ 0.0002 SOL fixed (gate_base_fixed).
    //   • constant-product price impact vs pool depth ≈ 40–60 bps (gate_impact_den).
    // Realized round-trip on this tape lands ~650–760 bps (6.5–7.6%) — consistent
    // with observed low-cap memecoin scalp costs. The credited favourable move
    // (cold-start prior) is a realistic lottery-like ~18%.
    let mut cfg = cfg;
    cfg.gate_expected_move_bps = 1_800;
    cfg.gate_protocol_bps = 450;
    cfg.gate_margin_bps = 150;
    cfg.gate_base_fixed_lamports = 200_000;
    cfg.gate_impact_den = 250_000;
    let mut eng = Engine::new(cfg, RunMode::Replay);
    // 512 mints against a capacity-64 watchlist => heavy full-path eviction.
    // Extended (ledger-refinement batch) with three §21.5/§21.6/§29.6 cohorts —
    // see the cohort blocks after the wave below.
    let n = 512u64;
    for round in 0..6u64 {
        // Each of the 512 markets follows its OWN deterministic scalp trajectory
        // (`main_scalp`) drawn from a realistic pump.fun outcome distribution — many
        // small winners, quick losers, occasional runners — so the held-position
        // lifecycle is exercised across the real shape mix, NOT a single grind-then-
        // crater wave that rewarded the forbidden fixed ladder. Integer, fixed (§22).
        for m in 0..n {
            let mt = mint(m);
            for i in 0..3u64 {
                let (price_fp, signed_base) = main_scalp(m, round, i);
                eng.tick(AppEvent::MarketTrade {
                    mint: mt,
                    price_fp,
                    quote_lamports: 400_000 + (m % 13) * 1_000,
                    // Competitive, varied pool depth (0.12–0.47 SOL) so the BROAD
                    // realistic distribution — not just the deep dense/live cohorts —
                    // wins position slots and drives the representative net.
                    liquidity_lamports: 120_000_000 + (m % 350) * 1_000_000 + round * 7,
                    signed_base,
                    buyer_entity: (m + i) % 97,
                    age_slots: 10 + (m as u32 % 40),
                });
            }
            // Each market "launches" (emits its discovery evidence) in ONE staggered
            // early round only — a fresh coin is discovered once, not re-promoted on
            // every round forever. With the confirmed-set recency pruning this means
            // a mint is admitted around its launch, then makes way for later launches
            // instead of the same handful churning slots (and bleeding the 7%
            // round-trip cost on every faded re-entry). The m%-typed conditions keep
            // the discovery-lane MIX (numeric / narrative / social / wallet).
            let launch = m % 5;
            if round == launch {
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
        // A CONTROLLED moderate pump-then-capitulate wave (peak ~1.3×) — this
        // special-purpose §21.6 cohort exercises the trade-count bars + swing
        // structure and the round-5 capitulation without injecting wild runner
        // variance; the BROAD main distribution (`main_scalp`) carries the tape's
        // representative outcome mix.
        let dense_mult_bp: u64 = [10_000, 10_500, 12_000, 13_000, 12_500, 11_000][round as usize];
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
                    // Depth comparable to the main distribution so this special-
                    // purpose §21.6 cohort does not monopolize the 3 position slots.
                    liquidity_lamports: 180_000_000 + d * 5_000,
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
        // The streamed coin is the tape's one genuine RUNNER (peak ~1.8×) — the
        // "occasional runner" of a realistic scalp distribution, and the §71 quota
        // A/B needs it clearly profitable when the reserved slot admits it.
        let s_mult_bp: u64 = [10_000, 12_000, 14_000, 16_000, 18_000, 16_000][round as usize];
        let st = mint(4_000);
        let sbase = 1_000_000_000i128 * s_mult_bp as i128 / 10_000;
        for i in 0..8u64 {
            let selling = round == 5;
            eng.tick(AppEvent::MarketTrade {
                mint: st,
                price_fp: sbase + (i as i128) * 500_000,
                quote_lamports: 700_000,
                // Depth comparable to the main distribution so the streamed coin
                // still admits (via the §71 corroboration quota) but does not
                // monopolize every slot as the single deepest repeat runner.
                liquidity_lamports: 170_000_000,
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
        // ticker + mint, distinct chatters spam the ticker. Only the early rounds
        // carry chat — the coin is discovered and admitted via the §71 quota while
        // the stream is hot, then the freed slots go to the broad main distribution
        // instead of the streamed runner re-admitting on every later slot.
        if round <= 2 {
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
        // ---- §29 Discord paid-alpha cohort (Wave-3 LAWs D1/D2/D4/D5). A
        // DESIGNATED caller in a PAID Discord room calls a mint EARLY — the
        // `AlphaCall` discovery lane (index 5) surfaces it, distinct from the open
        // social-caller firehose (§71 reflection integrity). The mint then earns an
        // on-chain confirm + real microstructure (near-balanced flow — numeric-lane
        // quiet, so the AlphaCall corroboration provenance is KEPT, not overridden
        // by a self-authorizing numeric candidate) and PASSES the gate: alpha
        // ACCELERATED a real setup, and its realized net attributes to the AlphaCall
        // lane AND the room's §29.8 outcome ledger (LAW D5). A SECOND mint the same
        // room calls has NO on-chain support — alpha alone can NEVER admit it (LAW
        // D4). Two distinct designated callers corroborate the winner (LAW D2
        // breadth — a lone caller is half-formation). Deterministic (§22): no RNG,
        // no wall-clock; reuses the existing deterministic wave shape.
        let alpha_win = b58_mint(ALPHA_WIN_B58);
        // A MODEST winner (peak ≈ +30% round 4, settling to a ≈ +20% consolidation
        // plateau round 5) — deliberately BELOW the forbidden fixed +35% (13_500)
        // first rung, so the cost-derived ladder banks its lower rung while the fixed
        // ladder misses entirely. Re-pin #15: the round-5 settle was lifted from a
        // +10% near-round-trip (11_000) to this +20% plateau (12_000) — at realistic
        // 0.1-SOL clips the deep give-back turned the AlphaCall re-admits net-negative
        // (incoherent with LAW D1/D5); the plateau keeps the paid room a genuine MODEST
        // profitable admit WITHOUT re-introducing a big runner that would reward the
        // forbidden fixed ladder (re-pin #13 representativeness: the streamed coin stays
        // the tape's one runner; derived still out-earns fixed).
        let aw_mult_bp: u64 = [10_000, 11_000, 12_000, 12_500, 13_000, 12_000][round as usize];
        let aw_base = 1_000_000_000i128 * aw_mult_bp as i128 / 10_000;
        for i in 0..8u64 {
            let selling = round == 5;
            eng.tick(AppEvent::MarketTrade {
                mint: alpha_win,
                price_fp: aw_base + (i as i128) * 500_000,
                quote_lamports: 700_000,
                // Deep pool so the paid room's genuine runner wins a corroboration
                // slot in §23 arbitration (a real deep-liquidity alpha call).
                liquidity_lamports: 300_000_000,
                signed_base: if selling {
                    -800_000
                } else if i % 2 == 0 {
                    500_000
                } else {
                    -480_000
                },
                buyer_entity: 500 + i % 7,
                age_slots: 10,
            });
        }
        if round == 0 {
            eng.tick(AppEvent::OnchainConfirm {
                mint: alpha_win,
                sellable_depth_lamports: 500_000_000,
            });
        }
        // The paid room's designated calls (rounds 0-2, while the call is hot).
        if round <= 2 {
            use pump_quant_ingest::social_source::{MockSocialSource, RawSocialPayload};
            let ts0 = 2_000_000_000u64 + round * 60_000_000_000;
            let batch = vec![
                RawSocialPayload::new(
                    format!("{{\"platform\":\"discord\",\"author\":\"alphalead\",\"community\":\"alpha-room-1\",\"text\":\"$AWIN {ALPHA_WIN_B58} early call full send r{round}\",\"likes\":0,\"is_designated_caller\":true}}").into_bytes(),
                    ts0,
                ),
                RawSocialPayload::new(
                    format!("{{\"platform\":\"discord\",\"author\":\"alphasecond\",\"community\":\"alpha-room-1\",\"text\":\"$AWIN {ALPHA_WIN_B58} confirming the call r{round}\",\"likes\":0,\"is_designated_caller\":true}}").into_bytes(),
                    ts0 + 1_000_000,
                ),
                RawSocialPayload::new(
                    format!("{{\"platform\":\"discord\",\"author\":\"alphalead\",\"community\":\"alpha-room-1\",\"text\":\"$ANOC {ALPHA_NOCONFIRM_B58} degen alpha no chart yet r{round}\",\"likes\":0,\"is_designated_caller\":true}}").into_bytes(),
                    ts0 + 2_000_000,
                ),
            ];
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
// Re-pin #7 (Wave-2 §26 confirmed-creator-dump reversal): SEED-ONLY re-pin —
// every decision-level constant below is UNCHANGED (same promoted/admitted/
// rejected/net/universe_filtered on the same tape). The digest moved solely
// because the §19 config-identity seed gained the operator-approved §26 keys
// (creator_dump_veto_enable / _bp / _strict_bp). The golden tape emits no
// CreatorAction events, so the confirmed-dump hazard the reversal targets is
// never present here and no golden decision changes; the §26 law's causal
// effect is proven on its own hazard tape in `audit_wave2_laws.rs`.
// Re-pin #8 (Batch-2a exit/sizing mechanics — §24 cost-derived profit targets,
// §24(d) exit-into-strength, §24 volatility-scaled stops/trail): SEED-ONLY re-pin —
// every decision-level constant below is UNCHANGED (same promoted 504 / admitted 17
// / rejected 487 / net 12_550_767 / universe_filtered 72 on the same tape). The
// digest moved solely because the §19 config-identity seed gained the eight
// Batch-2a keys (derived_targets_enable / target_margin_mult_bp / target_floor_bp /
// target_ceiling_bp / into_strength_exit_enable / into_strength_climax_bp /
// vol_stop_enable / vol_stop_scale_bp), all DEFAULT-OFF so no golden decision
// changes. Each law's causal effect is proven on its own hazard tape in
// audit_wave2_laws.rs; the shadow tournament gained two report-only challenger
// arms (exit-into-strength, vol-stop) that never touch capital or the journal.
// Re-pin #9 (Batch-2b — §71.2 discovery-lane attribution, §25 archetype
// classifier, §24 EntryMode leaves): SEED-ONLY re-pin — every decision-level
// constant below is UNCHANGED (same promoted 504 / admitted 17 / rejected 487 /
// net 12_550_767 / universe_filtered 72, same per-lane net on the same tape). The
// digest moved solely because the §19 config-identity seed gained two Batch-2b
// keys (setup_classifier_enable — default ON, and entry_mode_leaves_enable —
// default OFF). LAW 3 (discovery-lane attribution) adds NO config key and is a
// pure attribution correction — the disc_perf ledger is inert on this tape
// because no creation-sighting and social-caller (nor narrative and attention-
// velocity) both open on the golden tape, so the archetype-keyed and lane-keyed
// ledgers coincide here. LAW 4's classified archetype tags the thesis/MFE/reject
// samples but arbitration, the gate, and exits never read it, so no golden
// decision changes. LAW 11 is default-OFF (byte-identical to the 4-lane gate).
// Each law's causal effect is proven on its own hazard tape in audit_wave2_laws.rs.
// Re-pin #10 (Batch-2c — §70.1 money proxy, §70.6/§70.8 narrative class,
// §70.7 platform-lead, §70.9/§70.10 deployer/fee-floor): every decision-level
// constant below is UNCHANGED (same promoted 504 / admitted 17 / rejected 487 /
// net 12_550_767 / universe_filtered 72 on the same tape). LAW 7 (§70.1 composite
// money proxy) is DEFAULT ON — a legitimate lamports-moving law — but on THIS tape
// it is decision-neutral: the composite money level (smart-wallet entry + holder
// growth folded ahead of buy-pressure) changes the attention field's internal
// divergence scoring but crosses no promotion/admission/exit threshold here, so
// the realized net is byte-for-byte 12_550_767 (delta 0). Its causal lamports
// effect on a wallet-led market is pinned in audit_wave2_laws.rs. LAWs 8/9/10 are
// DEFAULT OFF (new scoring/sizing or protective behaviours, report-only until an
// operator flips them — Batch-2a precedent), so they touch no golden decision.
// The digest moved solely because the §19 config-identity seed gained the five
// Batch-2c keys (money_proxy_enable / narrative_class_enable / platform_lead_enable
// / deployer_screen_enable / fee_floor_enable) plus LAW 7's decision-neutral
// score change. Each law's causal effect is proven on its own hazard tape.
// Re-pin #11 (Batch-2d — records + report-plane, LAWs 12–21): every decision-level
// constant below is UNCHANGED (same promoted 504 / admitted 17 / rejected 487 /
// net 12_550_767 / universe_filtered 72 on the same tape, same per-lane net). The
// digest moved for TWO record/seed-level reasons, neither a decision change:
// (1) LAW 12 (§34.4 DecisionRecord completeness) extended the Admitted journal
// record with the size band (x_min/x_cost/x_max), the attempt/fail-rate multiplier,
// and the round-trip impact provenance — a REAL journal-encoding change folded over
// all 17 admits (record completeness, not a different decision); (2) LAW 13's new
// §19 config-identity key (probe_budget_enable, DEFAULT OFF) moved the seed. LAW 13
// opened no probe on this tape (the golden bankroll never sizes below x_min), so no
// count moved; its A/B is pinned in batch_2d_laws.rs. LAWs 14/16/19/20/21 are
// report-plane/additive with NO config key and are not on the golden Report path
// (promotion/baseline/feature-admission/ablation/live-status are separate report
// surfaces), so they touch no golden decision. LAWs 15/17/18 write only the
// report-only analytics rings (convexity enrichment, post-exit markouts, terminal-
// state reflections) which never enter the journal digest or the Report counts.
// Each law's effect is proven by its own test.
// Re-pin #12 (§24 defect-#3 reversal GOING LIVE — "constitution wins"): this is a
// REAL decision-level re-pin, NOT seed-only. The operator ruled that fixed global
// TP constants (13_500/25_000/50_000) are FORBIDDEN as the live default; cost-
// derived profit targets MUST be THE behaviour. Accordingly LAW 2
// (derived_targets_enable) is flipped to DEFAULT ON in Config::dev_portable, so all
// admits on the golden tape now exit on cost-derived rungs (per-market tp1/tp2/tp3
// from the gate's measured round_trip_cost_bps + margin, tranche count from
// exit_ladder::ladder_rungs) instead of the forbidden fixed ladder. Counts and net
// MOVE as a result: admitted 17 → 16, rejected 487 → 476, promoted 504 and
// universe_filtered 72 UNCHANGED, and net 12_550_767 → 3_831_945 (a SIGNED delta of
// −8_718_822 — the reversal net-moves DOWN on THIS tape). This is measured, honest
// lamports: the golden tape's grind-then-crater waves reward the fixed ladder's
// aggressive 13_500/25_000/50_000 rungs, so pricing exits off each market's true
// round-trip cost banks smaller-but-principled tranches here; the constitution
// forbids the fixed constants as the live default regardless of this tape's net, and
// LAW 2's causal out-performance on its OWN hazard tape (a low-cost grind that never
// reaches the fixed rungs) is still proven in audit_wave2_laws.rs. LAWs 5/6 and the
// other situational/protective laws remain DEFAULT OFF per golden-arc discipline —
// only this mandated reversal goes ON. (arc: 2_979_624 → 5_017_234 → 6_443_936 →
// 8_785_954 → 12_550_767 → 3_831_945.)
// Re-pin #13 (COST-REALISTIC TAPE — the golden tape made REPRESENTATIVE): a REAL
// decision-level re-pin, NOT seed-only. The operator kept the §24 reversal live
// (LAW 2 still DEFAULT ON) and ruled that the tape itself must be REPRESENTATIVE
// rather than pathologically rewarding the forbidden fixed ladder. The pathology
// (diagnosed): (a) `dev_portable` modelled a ~150–190 bps round-trip — far too
// cheap — which collapsed the cost-derived first rung to ~1.02× and never let it
// bank a real move; and (b) EVERY one of the 512 markets followed the SAME wave
// grinding to 1.5×–2× then cratering, so a single fixed 13_500/25_000/50_000
// ladder structurally out-earned any cost-priced exit. Fixed beat derived here for
// tape-shape reasons, not merit. Two corrections, both integer/deterministic (§22):
//   1. COSTS. `drive` now models a realistic low-cap Solana memecoin scalp
//      round-trip (docs/PUMPSWAP_DECODE.md dynamic tiered fees): protocol/fee/spread
//      450 bps + fixed priority/tip 200k lamports + impact — a realized ~650–760 bps
//      round-trip. Cost-derived rungs are now sensibly sized (tp1≈1.16×, tp2≈1.32×,
//      tp3≈1.49×).
//   2. DISTRIBUTION. Each market now follows its OWN deterministic trajectory
//      (`main_scalp`) drawn from a realistic outcome mix — ~42% quick losers, ~42%
//      small→mid winners straddling the derived (1.16×) and old fixed (1.35×) first
//      rung, ~16% moderate winners/runners (right tail BOUNDED at ~2.2× so the
//      reference stays stable) — with a generic early-launch phase (discovery cannot
//      front-run the outcome) and staggered launches (each coin discovered once, no
//      re-admission churn bleeding the 7% cost). The special-purpose cohorts are held
//      controlled so the BROAD distribution drives the net; the streamed coin stays
//      the one genuine runner (§71).
// Result on the now-representative tape: promoted 504 (UNCHANGED), universe_filtered
// 72 (UNCHANGED), admitted 16 → 14, rejected 476 → 467, net 3_831_945 → 1_406_102.
// This net is the HONEST cost-derived (§24-compliant) result on realistic economics.
// Crucially the tape NO LONGER favours the forbidden ladder: on this exact tape the
// cost-derived default nets 1_406_102 vs 1_393_482 for the fixed ladder — derived
// now marginally OUT-earns fixed (+12_620), the pathology inverted. Per-lane the
// ActiveMarketScalp lane is honestly slightly negative (−1_656_517: realistic 7%
// costs on many small clips) while CreationSniper (+3_062_619) carries the net.
// Reference comparison: 12_550_767 was fixed-ladder-on-the-UNREALISTIC-tape and
// 3_831_945 was derived-on-the-UNREALISTIC-tape; both were tape artifacts. 1_406_102
// is derived-on-the-REPRESENTATIVE-tape and is the trustworthy reference.
// (arc: 2_979_624 → 5_017_234 → 6_443_936 → 8_785_954 → 12_550_767 → 3_831_945 →
// 1_406_102.)
// Re-pin #14 (Wave-3 §29 Discord paid-alpha lane — LAWs D1–D5): a REAL
// decision-level re-pin, NOT seed-only. The TAPE gained a Discord paid-alpha
// cohort: a DESIGNATED caller in a PAID room calls a mint EARLY (the new
// `DiscoveryLane::AlphaCall`, index 5, discovers it — distinct from the open
// social-caller firehose, §71 reflection integrity), and the mint then earns an
// on-chain confirm + real near-balanced microstructure (numeric-lane quiet, so
// the AlphaCall corroboration provenance is KEPT) and PASSES the gate — alpha
// ACCELERATED a real setup. A second mint the same room calls has NO on-chain
// support and is correctly NEVER admitted (LAW D4: alpha alone can never admit).
// The alpha winner is a MODEST winner (peak ≈ +30%, then a round-5 sell-off) held
// deliberately BELOW the forbidden fixed +35% (13_500) first rung, so it does NOT
// re-introduce a big runner that would reward the forbidden fixed ladder — the
// re-pin #13 representativeness (derived out-earns fixed) is preserved (the
// pq-regression mirror still measures derived − fixed = +12_621 > 0). Counts/net
// MOVE: admitted 14 → 18 (the alpha winner opens via the §71 corroboration quota,
// plus a few re-admits as it rides), rejected 467 → 486 (the no-confirm alpha mint
// is promoted then gate-rejected for want of an on-chain confirm across the early
// rounds — LAW D4 exercised on the tape), promoted 504 and universe_filtered 72
// UNCHANGED, and net 1_406_102 → 1_864_780 (a SIGNED delta of +458_678 — the paid
// room's modest winner, admitted and ridden, is honest lamports). Its realized net
// attributes to the AlphaCall discovery lane (856_711) AND the room's §29.8 outcome
// ledger (LAW D5: per_alpha_source = [(Discord room, 856_711)]) — the CreationSniper
// setup lane is now the SUM of AlphaCall + SocialCaller + OnchainCreation, the exact
// §71.2 split the AlphaCall lane exists to make. LAWs D1/D2/D3 are DEFAULT ON (D1
// attribution changes no capital decision — the discovery lane never ranks; D2
// designated-caller weight helps the paid call rank; D3 bearish-alpha exit pressure
// is inert here — the golden cohort has no bearish held case). Each law's isolated
// causal effect is proven on its own hazard tape in alpha_laws.rs.
// (arc: … → 1_406_102 → 1_864_780.)
// Re-pin #15 (0.1-SOL OPERATOR FLOOR + small-bankroll Kelly recalibration —
// criterion 112 / Amendment A-6): a REAL decision-level re-pin, NOT seed-only. The
// operator directed an ABSOLUTE minimum trade size of 0.1 SOL on EVERY individual
// order (entry, each probe, each probe→confirm→scale-in add) and a Kelly/bankroll
// recalibration so a small 2-SOL bankroll actually trades at-or-above the floor
// instead of being blocked. Two coupled changes land here:
//   1. FLOOR. A new `min_trade_size_lamports` config (default 100_000_000 = 0.1 SOL)
//      lifts the economic band's `x_min` to `max(0.1 SOL, x_min)` (via the additive
//      `economic_gate::floor_size_band`, applied in `gate::decide` ABOVE the dossier-
//      locked `size_band` leaf). A risk/Kelly-arbitrated size below the floor is
//      CLAMPED UP to it when the hard caps allow, else REFUSED; a market too thin to
//      take a 0.1-SOL clip (`x_min > x_max`) refuses. Every emitted bite — probe and
//      scale-in add — is ≥ 0.1 SOL (`open_pending` folds a sub-floor scale remainder
//      into the initial bite). The sub-`x_min` paid-information probe (LAW 13) is a
//      SUB-FLOOR bet, so it is switched OFF while the floor is active.
//   2. RECALIBRATION. `Config::dev_portable` moves floor_fraction_bps 5_000→2_500
//      (survival floor max(0.5 SOL, 25%×2 SOL)=0.5 SOL ⇒ deployable 1.5 SOL),
//      f_base_bp 150→667 (base bite ≈0.1 SOL — the floor is the natural base bite),
//      total_risk_cap_bp 450→2_100 (fits 3 concurrent floor notionals + fees, ~0.303
//      SOL ≈ 15% of the 2-SOL bankroll on an all-positions rug), and
//      x_min_promote_cap_bp 400→800 (0.1 SOL = 6.67% of deployable, so the promote
//      cap MUST exceed that or the floor is unreachable — the key unblock).
// The 2-SOL bankroll now ADMITS (admitted 13 > 0) and NO order is below 0.1 SOL
// (pinned by the no-sub-floor invariant in sizing_floor_laws.rs). Counts/net MOVE:
// admitted 18 → 13 (fewer, ~5× larger 0.1-SOL clips under the same 3-slot cap),
// rejected 486 → 457, promoted 504 and universe_filtered 72 UNCHANGED, and net
// 1_864_780 → 15_410_801 (a SIGNED delta of +13_546_021 — realistic 0.1-SOL clips
// bank far more per admit, and the lower fixed-cost fraction at larger size lifts
// per-trade efficiency). The paid-alpha cohort's round-5 settle was also lifted from
// a +10% near-round-trip (11_000) to a +20% consolidation plateau (12_000): at tiny
// pre-A-6 sizes the deep give-back only bled a marginal amount, but at realistic
// 0.1-SOL clips the re-admits into that fade turned the AlphaCall lane NEGATIVE
// (incoherent with LAW D1/D5 "the paid room earns its keep"). The +20% plateau — a
// coherent "called winner consolidates and holds", consistent with the tape's own
// `main_scalp` winner-settle model and the streamed runner's partial give-back —
// keeps the alpha winner a MODEST positive contributor (AlphaCall 447_700, close to
// its prior 856_711 role) WITHOUT reshaping it into a second runner, and its +30%
// peak still sits BELOW the forbidden fixed +35% rung so cost-derived STILL out-earns
// fixed (derived 15_410_801 − fixed 15_055_700 = +355_101 > 0). LAWs unchanged; the
// §71 quota and §24 reversal wiring are re-measured, not altered.
// (arc: … → 1_406_102 → 1_864_780 → 15_410_801.)
// Re-pin #16 (EPISODIC RECALL MEMORY — LAWs B1–B5, `pump-quant-brain` wired in):
// a SEED-ONLY re-pin. The engine gained an episodic memory plane: every completed
// trade seals an immutable `Episode` whose 20-field integer fingerprint is
// quantized from the state captured AT ADMIT (LAW B1 — a fingerprint computed at
// exit would be a function of the price path it is supposed to predict, so the
// capture point is the gate and a test pins that mutating the entire post-entry
// path leaves it byte-identical); the reflection cadence re-queries recall over the
// setup classes the engine ACTUALLY traded, conditioned by venue phase × meta
// category × discovery lane, and feeds the meta-lifecycle timeline and the social
// call/markout ledger (LAW B2); a REDUCE-ONLY recall haircut/veto can fade or refuse
// a historically-bleeding class (LAW B3, config-gated, DEFAULT OFF — see
// `brain_haircut_is_exactly_neutral_on_this_tape`); an `Unknown` verdict is
// structurally incapable of changing a decision (LAW B4, pinned, no toggle); and the
// journal survives a restart (LAW B5).
//
// EVERY decision-plane number here is UNCHANGED: net 15_410_801, promoted 504,
// admitted 13, rejected 457, universe_filtered 72, and every per-lane and
// per-discovery-lane net identical to re-pin #15. Only the DIGEST moves, and it
// moves for exactly one reason: §19 folds the whole `Config`'s strategy identity
// into the journal seed (`fnv1a_64(format!("{cfg:?}"))`), so ADDING the nine
// `brain_*` config fields necessarily re-seeds it. That is the identity law working
// as designed, not a behaviour change — `brain_plane_is_decision_inert_on_this_tape`
// and `brain_laws::b4_the_brain_plane_itself_is_decision_inert` prove the DECISION
// STREAM the digest is computed over is byte-identical with the plane on and off.
// LAW B3's causal value is proven on its own hazard tape in `brain_laws.rs`
// (+391_932_566 lamports of loss avoided), not here.
// (arc: … → 1_406_102 → 1_864_780 → 15_410_801 [net unchanged at re-pin #16].)
// Re-pin #17 (BRAIN INTEGRATION WAVE — measured gap leaves, the social abstraction
// plane, the social→on-chain hardening proofs, the meta-taxonomy v1 fix and the
// recall-radius retune): a SEED-ONLY re-pin. EVERY decision-plane number is
// UNCHANGED — net 15_410_801, promoted 504, admitted 13, rejected 457,
// universe_filtered 72, AlphaCall 447_700, and every per-lane / per-discovery-lane
// net identical to re-pin #16. Only the DIGEST moves, and it moves for exactly one
// reason: §19 folds the whole `Config`'s strategy identity into the journal seed
// (`fnv1a_64(format!("{cfg:?}"))`), so the two CONFIG VALUES this wave changed
// necessarily re-seed it:
//
//   1. `meta_taxonomy_version` 0 → 1. v0's naive substring matching mis-assigned
//      ordinary English into the brain's RECALL FILTER KEY — "Fair Launch"→AI (via
//      "ai" in "fair"), "Catalyst"→Animal, "Bottom Signal"→AI, "Bullish
//      Chain"→Animal, "Starter Pack"→Celebrity, "Magazine"→Political — pooling
//      tokens with the wrong meta's episodes and silently corrupting every
//      conditioned estimate keyed on them. `meta::TAXONOMY_V1` adopts the
//      word-boundary `MatchMode` discipline. The fix ships FORWARD under a bumped
//      version; v0 stays frozen and its six mis-assignments are PINNED as the
//      historical record (criterion 81 — assignments are timestamped and never
//      retroactive). The golden tape feeds no `TokenMetadata`, so no assignment on
//      it changes; only the config identity does.
//   2. `brain_recall_max_distance` 12 → 8. At radius 12 a maximally net-BUYING
//      setup matched a maximally net-SELLING one (the OFI and CVD ladders span 6
//      buckets each), and the engine formed an opinion on 144 of 245 admit-time
//      recalls from a 13-episode memory. At 8 that pairing is structurally
//      unreachable and admit-time recall correctly refuses on all 245 — the only
//      honest answer a 13-episode index with a sample floor of 8 can give. The
//      reflection pass still surfaces 3 conditioned setup classes. See
//      `brain::BRAIN_RECALL_MAX_DISTANCE_DEFAULT` for the full sweep.
//
// Everything else this wave added is REPORT-PLANE and never journaled: the four
// measured gap-leaf estimators (holder-growth acceleration, creator track record,
// meta lifecycle phase, narrative family — `measured_fingerprint.rs` proves each
// reachable AND decision-inert) and the social abstraction plane (trust, support,
// follow recommendation, style-lens scoreboard — `social_hardening.rs` proves the
// whole social plane cannot admit without on-chain confirmation, numeric
// microstructure and a passing economic gate, at ANY social strength).
// (arc: … → 1_406_102 → 1_864_780 → 15_410_801 [net unchanged at re-pins #16, #17].)
// Re-pin #18 (BRAIN → STRATEGY ANALYSIS — LAWs B6–B9): a SEED-ONLY re-pin. The
// brain stopped merely observing and started feeding the strategy-analysis loop:
//
//   * LAW B6 — `brain_analysis_v1`, a bounded, deterministic, integer-only export
//     written alongside `live_status.json` (same info-time discipline, never
//     wall-clock) plus `Engine::brain_analysis_json()`. Every array carries a
//     named cap and a documented total sort key, and an `unknown` verdict emits
//     its refusal reason with `null` in EVERY estimate field — a consumer cannot
//     read a number the brain refused to give (§46).
//   * LAW B6/§56 — `retirement_flags`: conditioned-negative setup classes, style
//     lenses, discovery lanes and alpha sources nominated for the weekly
//     governance review. A nomination is NOT a retirement: §51 FDR/PBO and §52
//     baselines remain the authority, and `brain_strategy::retirement_flags_
//     retire_nothing` pins that the flags retire nothing.
//   * LAW B7 — an optional, reduce-only, envelope-bounded brain downweight in
//     `reflect`. DEFAULT OFF: the A/B is exactly neutral (delta 0 net) on the
//     golden tape AND on a purpose-built decayed-lane tape, at every step from
//     250 bp to the full envelope. It did not earn, so it is not armed. (Re-tested
//     since, under a PRE-REGISTERED two-sided rule, on a tape where the mechanism
//     genuinely does act — `tests/brain_reflect_twosided.rs`. It still does not
//     earn: +26_697_249 on the true-positive tape against −21_009_674 on its
//     false-positive mirror, a 1.27x asymmetry versus the pre-registered 3x bar,
//     with the sign inverting across neighbouring market shapes. Golden-tape
//     neutrality is now asserted directly by
//     `b7_armed_reflection_is_exactly_neutral_on_this_tape` below. Default
//     unchanged, so this digest is unchanged.)
//   * LAW B8 — brain-grounded exit-challenger PROPOSALS derived from the recall
//     distribution of the setups that paid (median winner hold → time stop, p75
//     MFE → target, median winner heat → trail). Report-only, single-axis,
//     envelope-clamped, fail-closed at small n, never auto-adopted.
//   * LAW B9 — recall as an ADDITIONAL promotion blocker, consulted LAST so it can
//     never mask a §38/§51/§64 label, and one-directional so it can only ever
//     remove eligibility.
//
// EVERY decision-plane number is UNCHANGED — net 15_410_801, promoted 504,
// admitted 13, rejected 457, universe_filtered 72, AlphaCall 447_700, and every
// per-lane / per-discovery-lane net and final weight identical to re-pin #17. Only
// the DIGEST moves, and it moves for exactly one reason: §19 folds the whole
// `Config`'s strategy identity into the journal seed
// (`fnv1a_64(format!("{cfg:?}"))`), so ADDING the five config fields this wave
// needs — `brain_analysis_enable`, `brain_analysis_path`, `brain_reflect_enable`,
// `brain_reflect_step_bp`, `brain_decay_min_sample` — necessarily re-seeds it.
// That is the identity law working as designed. The three golden-tape conditioned
// setup classes are all POSITIVE (median +301_400, win rate 7_500 bp, n = 8 each),
// so LAW B7's lane-decay flag set is EMPTY on this tape even when armed, and the
// armed arm's net/admitted/promoted/rejected are byte-identical to the neutral
// arm's — measured, not assumed.
// (arc: … → 1_406_102 → 1_864_780 → 15_410_801 [net unchanged at re-pins #16, #17, #18, #19].)
// Re-pin #19 (§70.1 CONTINUOUS HOLDER ACCOUNTING): a SEED-ONLY re-pin. Holder-growth
// capture stopped being a seam nobody called and became a STREAM:
//
//   * `holder_flow.rs` — a per-mint `entity -> net base position` ledger folded from
//     OUR OWN decoded swaps (`buyer_entity` + `signed_base`), so the holder count is
//     canonical §6.1 evidence with zero added latency and no third-party dependency.
//     Birdeye/DAS holder counts stay strictly corroboration-tier (§6.6); the old
//     `Engine::observe_holder_count` seam is demoted to exactly that in its docs.
//   * The OBSERVATION-WINDOW LAW, enforced in the type: a mint watched from its
//     creation event is `Exact`, a mint discovered mid-life is `DeltaOnly`, an
//     over-cap ledger is `Incomplete`. `HolderReading`'s count is private and its
//     accessors are basis-gated, so a LEVEL consumer structurally cannot read a
//     delta-only or truncated count, while a GROWTH consumer (§70.1 wants a second
//     derivative) legitimately reads `DeltaOnly`. The `Exact` claim is falsifiable
//     by evidence — a sell from an untracked position proves a pre-window holder —
//     and the basis lattice only ever moves toward less confidence.
//   * WATCH: folded on every `MarketTrade` for every mint, admitted or not, and
//     sampled into `pump_quant_features::holder_growth` on a 3-tick (1.2 s) cadence
//     — the smallest whole-tick cadence at or above the estimator's 1 s minimum
//     interval, asserted at compile time. ANALYZE: the fingerprint's
//     `holder_growth_accel_bps` now receives a REAL measured value where before it
//     took the neutral rung on literally every admit. ENTER/HOLD: the open book's
//     holder trajectory rides out on `Report::holder_trajectory`.
//   * §3 — the money-proxy holder term (`money_proxy_holder_flow_enable`) replaces
//     the `unique_buyers` bitset popcount (which saturates at 64, collides on
//     `entity % 64`, and is MONOTONE NON-DECREASING, so it cannot see distribution
//     at all). DEFAULT OFF: it measures EXACTLY ZERO lamports on this tape and on
//     both sides of a purpose-built two-sided A/B, and `tests/holder_flow.rs`
//     establishes that the zero is an UNREACHABILITY result — the whole §70.1
//     composite money proxy is inert on those tapes — rather than an efficacy
//     result. It did not earn, so it is not armed.
//
// EVERY decision-plane number is UNCHANGED — net 15_410_801, promoted 504,
// admitted 13, rejected 457, universe_filtered 72, AlphaCall 447_700, and every
// per-lane / per-discovery-lane net and final weight identical to re-pin #18. Only
// the DIGEST moves, and it moves for exactly one reason: §19 folds the whole
// `Config`'s strategy identity into the journal seed, so ADDING the single config
// field this wave needs — `money_proxy_holder_flow_enable` — necessarily re-seeds
// it. Measured, not assumed: ablating the holder fold while KEEPING the config
// field reproduces this digest byte for byte, which isolates the drift to the seed.
// (arc: … → 1_406_102 → 1_864_780 → 15_410_801 [net unchanged at re-pins #16, #17, #18, #19].)
const GOLDEN_DIGEST: u64 = 6_234_587_619_693_007_457;
const GOLDEN_NET_LAMPORTS: i128 = 15_410_801;
const GOLDEN_PROMOTED: u64 = 504;
const GOLDEN_ADMITTED: u64 = 13;
const GOLDEN_REJECTED: u64 = 457;
/// Zombie-cohort promotions the §21.5 screen must remove (visible activity).
const GOLDEN_UNIVERSE_FILTERED: u64 = 72;
/// LAW D1/D5: the paid Discord room's realized net attributed to the AlphaCall
/// discovery lane (the modest winner it surfaced, admitted and ridden). Positive —
/// the room earned its keep; keyed distinctly from the open social-caller firehose.
const GOLDEN_ALPHACALL_NET: i64 = 447_700;

#[test]
fn golden_digest_is_stable() {
    let r = drive(Config::dev_portable());
    // Print for inspection (`cargo test -- --nocapture`).
    println!(
        "GOLDEN ticks={} promoted={} admitted={} rejected={} universe_filtered={} net={} digest={} per_lane={:?} weights={:?}",
        r.ticks, r.promoted, r.admitted, r.rejected, r.universe_filtered, r.net_lamports, r.journal_digest,
        r.per_lane_net, r.final_weights
    );
    println!(
        "GOLDEN per_discovery_lane={:?} per_alpha_source={:?}",
        r.per_discovery_lane_net, r.per_alpha_source_net
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
    // LAW D1/D5: the paid Discord room's runner is attributed to the AlphaCall
    // discovery lane, distinct from the open social-caller firehose (§71.2), and
    // the CreationSniper setup lane is the SUM of AlphaCall + SocialCaller (the
    // split the AlphaCall lane exists to make).
    let alphacall_net = r
        .per_discovery_lane_net
        .iter()
        .find(|(l, _)| *l == pump_quant_watchlist::candidate::DiscoveryLane::AlphaCall)
        .map(|(_, n)| *n)
        .unwrap();
    let social_net = r
        .per_discovery_lane_net
        .iter()
        .find(|(l, _)| *l == pump_quant_watchlist::candidate::DiscoveryLane::SocialCaller)
        .map(|(_, n)| *n)
        .unwrap();
    let onchain_net = r
        .per_discovery_lane_net
        .iter()
        .find(|(l, _)| *l == pump_quant_watchlist::candidate::DiscoveryLane::OnchainCreation)
        .map(|(_, n)| *n)
        .unwrap();
    let creationsniper_net = r
        .per_lane_net
        .iter()
        .find(|(l, _)| *l == pump_quant_watchlist::candidate::Lane::CreationSniper)
        .map(|(_, n)| *n)
        .unwrap();
    assert_eq!(
        alphacall_net, GOLDEN_ALPHACALL_NET,
        "AlphaCall discovery-lane net drifted"
    );
    assert!(
        alphacall_net > 0,
        "the paid room's alpha runner must earn positive net ({alphacall_net})"
    );
    assert_eq!(
        creationsniper_net,
        alphacall_net + social_net + onchain_net,
        "CreationSniper must be the sum of its independent discovery lanes (§71.2)"
    );
    // LAW D5: the room's realized net accrues in the §29.8 per-source ledger,
    // exposed on the Report — the seam reflection uses to grade the paid room.
    assert_eq!(
        r.per_alpha_source_net.len(),
        1,
        "exactly the one paid Discord room that led the winner is tracked"
    );
    let (room, room_net) = r.per_alpha_source_net[0];
    assert_eq!(
        room.kind,
        pump_quant_social::types::SourceKind::Discord,
        "the tracked alpha source is a Discord room"
    );
    assert_eq!(
        room_net, GOLDEN_ALPHACALL_NET,
        "the room's realized net matches its AlphaCall attribution"
    );
    assert!(room_net > 0, "the paid room earned its keep ({room_net})");
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

/// LAW B3's measured effect on the representative golden tape, and the evidence
/// behind its DEFAULT-OFF setting.
///
/// Recall DOES reach `Known` here — 144 admit-time verdicts clear the §46 sample
/// floor — but every class it can speak about is PROFITABLE, so the reduce-only
/// law correctly does nothing: zero haircuts, zero vetoes, and a net delta of
/// EXACTLY ZERO. That is the right behaviour and it is also exactly why the law
/// ships OFF: a law whose measured effect on the representative tape is neutral
/// has not demonstrated that it earns, and §46 forbids arming it on the assumption
/// that it will. Its causal value IS demonstrated, on a tape that actually contains
/// the hazard it targets, in `brain_laws.rs`
/// (`b3_armed_recall_haircut_strictly_out_earns_its_absence`: +391_932_566
/// lamports of loss avoided). This test pins the neutrality, so a future change
/// that made the law silently active on the golden path would fail loudly.
#[test]
fn brain_haircut_is_exactly_neutral_on_this_tape() {
    // Both arms widen the recall radius to the PRE-#17 default of 12. Under the
    // shipped default of 8 admit-time recall correctly REFUSES on every query on
    // this 13-episode tape (see `BRAIN_RECALL_MAX_DISTANCE_DEFAULT`), which would
    // make this neutrality proof vacuous — a haircut that never fires because
    // recall never speaks proves nothing about the haircut. Widening to 12 gives
    // recall something to say so the neutrality claim has content. The radius is
    // the ONLY difference between the arms, so the comparison is still exact.
    let mut off_cfg = Config::dev_portable();
    off_cfg.brain_recall_max_distance = 12;
    let off = drive(off_cfg);
    let mut on = Config::dev_portable();
    on.brain_recall_max_distance = 12;
    on.brain_haircut_enable = true;
    let armed = drive(on);
    println!(
        "B3-on-golden: off_net={} armed_net={} known={} unknown={} haircuts={} vetoes={}",
        off.net_lamports,
        armed.net_lamports,
        armed.brain_recall_known,
        armed.brain_recall_unknown,
        armed.brain_haircuts_applied,
        armed.brain_vetoes
    );
    assert!(
        armed.brain_recall_known > 0,
        "recall must actually reach Known here, else this neutrality proves nothing"
    );
    assert_eq!(
        armed.brain_haircuts_applied, 0,
        "every recalled class on this tape is profitable — nothing to fade"
    );
    assert_eq!(armed.brain_vetoes, 0, "and nothing to refuse");
    assert_eq!(
        armed.net_lamports, off.net_lamports,
        "LAW B3 is EXACTLY neutral on the golden tape — hence DEFAULT OFF"
    );
    assert_eq!(armed.admitted, off.admitted);
    assert_eq!(armed.rejected, off.rejected);
}

/// **LAW B7 leg (c): the NEUTRAL path.** Arming the brain-informed, reduce-only
/// lane downweight on the golden tape must change EXACTLY nothing.
///
/// The golden tape's conditioned setup classes are all POSITIVE, so
/// `brain_analysis::lane_decay` flags no lane and `reflect_with_brain` degenerates
/// to `reflect`. This is leg (c) of the pre-registered LAW B7 decision rule (see
/// `tests/brain_reflect_twosided.rs`): a protective law that perturbed the golden
/// path in the ABSENCE of the hazard it targets would be disqualified whatever its
/// hazard-tape economics, so the neutrality is asserted here on the real golden
/// tape rather than on a copy of it.
///
/// Only the JOURNAL DIGEST is exempt, and for one reason that is not a behaviour
/// change: §19 folds the whole `Config`'s strategy identity into the journal seed,
/// so flipping any config field necessarily re-seeds it.
#[test]
fn b7_armed_reflection_is_exactly_neutral_on_this_tape() {
    let off = drive(Config::dev_portable());
    let mut on_cfg = Config::dev_portable();
    on_cfg.brain_reflect_enable = true;
    let armed = drive(on_cfg);
    println!(
        "B7-on-golden: off_net={} armed_net={} off_w={:?} armed_w={:?}",
        off.net_lamports, armed.net_lamports, off.final_weights, armed.final_weights
    );
    assert_eq!(
        armed.net_lamports, off.net_lamports,
        "LAW B7 must be EXACTLY neutral on the golden tape (leg (c))"
    );
    assert_eq!(armed.admitted, off.admitted, "admissions unchanged");
    assert_eq!(armed.rejected, off.rejected, "rejections unchanged");
    assert_eq!(armed.promoted, off.promoted, "promotions unchanged");
    assert_eq!(
        armed.universe_filtered, off.universe_filtered,
        "§21.5 screen unchanged"
    );
    assert_eq!(
        armed.per_lane_net, off.per_lane_net,
        "attribution unchanged"
    );
    assert_eq!(
        armed.final_weights, off.final_weights,
        "no lane weight may move: an empty decay flag set is byte-identical to the \
         pre-LAW-B7 reflection pass"
    );
    // Not vacuous: the tape really does exercise the reflection pass.
    assert_ne!(
        off.final_weights,
        [
            (
                pump_quant_watchlist::candidate::Lane::CreationSniper,
                pump_quant_watchlist::candidate::Lane::CreationSniper.default_weight_bp()
            ),
            (
                pump_quant_watchlist::candidate::Lane::EarlyConfirmation,
                pump_quant_watchlist::candidate::Lane::EarlyConfirmation.default_weight_bp()
            ),
            (
                pump_quant_watchlist::candidate::Lane::GraduationTransition,
                pump_quant_watchlist::candidate::Lane::GraduationTransition.default_weight_bp()
            ),
            (
                pump_quant_watchlist::candidate::Lane::ActiveMarketScalp,
                pump_quant_watchlist::candidate::Lane::ActiveMarketScalp.default_weight_bp()
            ),
        ],
        "the golden tape must actually run reflection, else this neutrality is vacuous"
    );
}

/// LAW B1/B2 are DEFAULT ON and must be decision-inert: enabling the whole memory
/// plane records episodes and produces readouts without moving a single count or
/// a single lamport on the golden tape.
#[test]
fn brain_plane_is_decision_inert_on_this_tape() {
    let on = drive(Config::dev_portable());
    let mut off_cfg = Config::dev_portable();
    off_cfg.brain_enable = false;
    let off = drive(off_cfg);
    println!(
        "B1/B2-on-golden: episodes={} known={} unknown={} classes={} authors={} metas={}",
        on.brain_episodes_recorded,
        on.brain_recall_known,
        on.brain_recall_unknown,
        on.brain_setup_classes.len(),
        on.brain_author_records.len(),
        on.brain_meta_state.len()
    );
    assert!(
        on.brain_episodes_recorded > 0,
        "the plane must actually record on this tape, else the test is vacuous"
    );
    assert_eq!(off.brain_episodes_recorded, 0);
    assert_eq!(on.net_lamports, off.net_lamports, "net-SOL unchanged");
    assert_eq!(on.admitted, off.admitted, "admissions unchanged");
    assert_eq!(on.rejected, off.rejected, "rejections unchanged");
    assert_eq!(on.promoted, off.promoted, "promotions unchanged");
    assert_eq!(
        on.universe_filtered, off.universe_filtered,
        "universe screen unchanged"
    );
    assert_eq!(on.per_lane_net, off.per_lane_net, "attribution unchanged");
    assert_eq!(on.final_weights, off.final_weights, "weights unchanged");
}

/// §70.1 re-pin #19: the armed holder-flow money term is EXACTLY neutral here.
///
/// The wave's one decision-affecting switch, measured on the representative tape
/// rather than argued about. The continuous holder stream itself runs in BOTH
/// arms (it is unconditional — that is what "constant stream" means); only the
/// §70.1 money-proxy TERM is toggled. Zero lamports of difference is why it ships
/// off. See `tests/holder_flow.rs::ab_*` for the two-sided purpose-built A/B and
/// for the proof that the composite money proxy is unreachable on those tapes.
#[test]
fn holder_flow_money_term_is_exactly_neutral_on_this_tape() {
    let off = drive(Config::dev_portable());
    let mut on_cfg = Config::dev_portable();
    on_cfg.money_proxy_holder_flow_enable = true;
    let armed = drive(on_cfg);
    println!(
        "HOLDER-FLOW-on-golden: off_net={} armed_net={} off_adm={} armed_adm={}",
        off.net_lamports, armed.net_lamports, off.admitted, armed.admitted
    );
    assert_eq!(
        armed.net_lamports, off.net_lamports,
        "the §70.1 holder term must be EXACTLY neutral on the golden tape"
    );
    assert_eq!(armed.admitted, off.admitted, "admissions unchanged");
    assert_eq!(armed.rejected, off.rejected, "rejections unchanged");
    assert_eq!(armed.promoted, off.promoted, "promotions unchanged");
    assert_eq!(
        armed.per_lane_net, off.per_lane_net,
        "attribution unchanged"
    );
    // NOT vacuous: the holder stream really is populated on this tape, so a term
    // that mattered would have had something to say.
    let mut probe = Engine::new(Config::dev_portable(), RunMode::Replay);
    probe.tick(AppEvent::MarketTrade {
        mint: mint(7),
        price_fp: 1_000_000_000,
        quote_lamports: 400_000,
        liquidity_lamports: 120_000_000,
        signed_base: 500_000,
        buyer_entity: 3,
        age_slots: 12,
    });
    assert_eq!(
        probe
            .holder_reading(mint(7).as_bytes())
            .and_then(|r| r.growth_level()),
        Some(1),
        "the watch-time holder fold must be live on the golden engine"
    );
}
