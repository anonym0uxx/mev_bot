//! The GOLDEN TAPE generator, hoisted verbatim out of `tests/golden_digest.rs`
//! so more than one test binary can drive the *same* representative tape.
//!
//! Nothing here was rewritten for the law-permutation sweep: this is the byte-for-
//! byte generator the golden reference has been pinned against since re-pin #16.
//! `golden_digest.rs` still owns the pins and the neutrality laws; this module owns
//! only the event script.
#![allow(dead_code)]

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_domain::ids::Mint;

pub fn mint(tag: u64) -> Mint {
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
pub const ALPHA_WIN_B58: &str = "7GCihgDB8fe6KNjn2MYtkzZcRjQy3t9GHdC8uHYmW2hr";
pub const ALPHA_NOCONFIRM_B58: &str = "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263";

/// Decode a golden-cohort base58 pubkey to a `Mint` (valid by construction).
pub fn b58_mint(s: &str) -> Mint {
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
pub fn main_scalp(m: u64, round: u64, i: u64) -> (i128, i64) {
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

/// Drive the golden tape and hand back the ENGINE, un-reported.
///
/// Split out from [`drive`] so a test can exercise the report-plane machinery
/// (the §21.7 parallel stream, the strategy export) against the same tape and
/// then check that the journal digest did not move.
pub fn drive_eng(cfg: Config) -> Engine {
    drive_eng_with_fill(cfg, true)
}

/// The SAME tape, driven with the curve-exact fill DISARMED — fills taken at the
/// observed print instead of walking the constant product.
///
/// The event script is not duplicated: [`drive_eng`] and this function are the same
/// function under one boolean, so "what our own impact costs" is measurable on ONE
/// cost model by toggling exactly the fill, rather than by comparing against a number
/// measured under a retired one (`curve_fill_wiring.rs`).
pub fn drive_at_print(cfg: Config) -> pump_quant_app::engine::Report {
    drive_eng_with_fill(cfg, false).report()
}

fn drive_eng_with_fill(cfg: Config, curve_exact_fill: bool) -> Engine {
    // ---- Cost-realism. The round-trip cost is no longer stated here at all: it is
    // DERIVED, per candidate, from the market's own SOL-side reserve by
    // `pump_quant_app::cost_model` — the single authority the gate and the P&L
    // lifecycle now share. What this tape used to assert in a comment (a ~450 bps
    // size-invariant fee containing ~200 bps of "bid/ask spread") is arithmetically
    // impossible on a constant-product AMM and is gone: the venue charges 125 bps a
    // leg on the whole bonding curve, and the cost of crossing size is own impact,
    // charged separately on both legs against the depth this tape declares below.
    //
    // What remains configurable is the BENEFIT side (a realistic lottery-like ~18%
    // cold-start prior) and the safety margin. Everything on the cost side is now a
    // property of the market, not of this fixture.
    let mut cfg = cfg;
    cfg.gate_expected_move_bps = 1_800;
    cfg.gate_margin_bps = 150;
    // The depths below are real, so our own curve impact is charged on both legs.
    cfg.curve_exact_fill_enable = curve_exact_fill;
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
                    // REAL pump.fun depth. Virtual reserves start at 30 SOL and deepen
                    // as net SOL flows in; graduation lands near 85 SOL. `round` is the
                    // tape's proxy for cumulative inflow, so depth grows 30 -> ~67 SOL
                    // across a mint's life. The previous 0.12-0.47 SOL put our 0.1 SOL
                    // minimum clip at 21-83% OF THE POOL — a market in which no strategy
                    // result means anything.
                    liquidity_lamports: 30_000_000_000
                        + round * 4_000_000_000
                        + (m % 350) * 50_000_000,
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
                    // ---- CURVE RESERVES, FROM ONE SNAPSHOT (corrected 2026-07-28).
                    // The tape used to declare a "sellable depth" of 29-30 SOL against
                    // a 30-34 SOL price reserve. On this venue a curve is SEEDED with
                    // 30 SOL of VIRTUAL reserve and escrows `virtual_sol - 30 SOL`, so
                    // those rows described markets that cannot exist — overstating
                    // extractable SOL by 30x at vsol 31 and without bound at vsol 30.
                    // The confirm now carries the pair the program actually stores.
                    let vsol = 30_000_000_000 + round * 4_000_000_000 + (m % 350) * 50_000_000;
                    eng.tick(AppEvent::OnchainConfirm {
                        mint: mt,
                        virtual_sol_lamports: vsol,
                        real_sol_lamports: vsol - 30_000_000_000,
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
                        liquidity_lamports: 34_000_000_000 + z * 1_000_000,
                        signed_base: 800_000 + (z as i64) * 500,
                        buyer_entity: 200 + (z + i) % 9,
                        age_slots: 200,
                    });
                }
            }
            if round == 3 {
                // The zombie's last observed snapshot is its round-1 reserve; the
                // confirm reports that decode, and the freshness law decides what to
                // do with an old one.
                let vsol = 34_000_000_000 + z * 1_000_000;
                eng.tick(AppEvent::OnchainConfirm {
                    mint: mt,
                    virtual_sol_lamports: vsol,
                    real_sol_lamports: vsol - 30_000_000_000,
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
                    liquidity_lamports: 31_000_000_000 + d * 500_000,
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
                // Previously declared 34 SOL of sellable depth on a 31 SOL reserve —
                // depth ABOVE the whole price curve, which the retired
                // `min(depth, liquidity)` cross-check silently laundered into 31 SOL.
                let vsol = 31_000_000_000 + d * 500_000;
                eng.tick(AppEvent::OnchainConfirm {
                    mint: mt,
                    virtual_sol_lamports: vsol,
                    real_sol_lamports: vsol - 30_000_000_000,
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
                liquidity_lamports: 30_500_000_000,
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
                virtual_sol_lamports: 30_500_000_000,
                real_sol_lamports: 500_000_000,
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
                liquidity_lamports: 32_000_000_000,
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
                virtual_sol_lamports: 32_000_000_000,
                real_sol_lamports: 2_000_000_000,
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
    eng
}

pub fn drive(cfg: Config) -> pump_quant_app::engine::Report {
    drive_eng(cfg).report()
}
