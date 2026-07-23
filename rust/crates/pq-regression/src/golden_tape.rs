//! Byte-faithful mirror of `pump-quant-app/tests/golden_digest.rs::drive`.
//!
//! # Why a mirror and not a shared function
//! The canonical golden tape lives as a *private* `fn drive` inside the
//! `golden_digest.rs` integration-test binary. A separate crate cannot call into
//! another crate's test binary, and moving the tape into the `pump-quant-app`
//! library would edit precious pinned code. So this module reproduces the tape
//! verbatim over the *public* engine API. Two independent constructions that
//! reach the SAME pinned digest ([`crate::baselines::GOLDEN_DIGEST`]) is a
//! stronger determinism tripwire than one — an accidental coupling that changed
//! the engine's behaviour would break this mirror as well.
//!
//! # Keeping the mirror honest
//! [`drive`] MUST stay byte-identical to `golden_digest.rs::drive`. The
//! determinism regression asserts the mirror reproduces the pinned constants, so
//! if the canonical tape is ever re-pinned this mirror fails loudly until it is
//! re-synced. See `REGRESSION_MANIFEST.md` ("Updating a baseline") for the procedure.

use pump_quant_app::config::Config;
use pump_quant_app::engine::{Engine, Report, RunMode};
use pump_quant_app::event::AppEvent;
use pump_quant_domain::ids::Mint;

/// Deterministic mint tag → 32-byte id (mirror of the golden helper).
pub fn mint(tag: u64) -> Mint {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&tag.to_le_bytes());
    b[8] = 0xAB;
    Mint::from_bytes(b)
}

/// Wave-3 Discord paid-alpha cohort pubkeys (mirror of the golden helper).
const ALPHA_WIN_B58: &str = "7GCihgDB8fe6KNjn2MYtkzZcRjQy3t9GHdC8uHYmW2hr";
const ALPHA_NOCONFIRM_B58: &str = "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263";

/// Decode a golden-cohort base58 pubkey to a `Mint` (valid by construction).
fn b58_mint(s: &str) -> Mint {
    Mint::from_bytes(pump_quant_ingest::base58::decode_pubkey(s).expect("valid golden pubkey"))
}

/// Deterministic per-mint scalp trajectory over the six-round tape (no RNG, §22).
/// Verbatim mirror of `golden_digest.rs::main_scalp`.
fn main_scalp(m: u64, round: u64, i: u64) -> (i128, i64) {
    let h = m
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(0x1234_5678_9ABC_DEF0)
        .rotate_left(29)
        ^ m.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    let bucket = h % 1_000;
    let spread = (h / 1_000) % 1_000;

    let peak_bp: u64 = if bucket < 420 {
        10_000 + spread % 300
    } else if bucket < 840 {
        11_400 + spread * 2_600 / 1_000
    } else {
        14_000 + spread * 8_000 / 1_000
    };
    let settle_bp: u64 = if bucket < 420 {
        8_000 + spread % 1_000
    } else {
        10_000 + (peak_bp - 10_000) / 2
    };

    let peak_round = 2u64;
    let cur_bp: u64 = match round {
        0 => 10_000,
        1 => 10_300,
        2 => peak_bp,
        3 => (peak_bp + settle_bp) / 2,
        _ => settle_bp,
    };
    let micro = (i as i128) * (cur_bp as i128 / 100);
    let price_fp = 1_000_000_000i128 * cur_bp as i128 / 10_000 + micro + (m as i128) * 1_000;
    let rising = round <= 1 || round <= peak_round;
    let signed_base: i64 = if rising {
        500_000 + (m as i64 % 13) * 1000 - (i as i64 * 100)
    } else {
        -(500_000 + (m as i64 % 13) * 1000) + (i as i64 * 100)
    };
    (price_fp, signed_base)
}

/// Drive the golden multi-lane tape and return the byte-exact [`Report`].
///
/// Verbatim mirror of `golden_digest.rs::drive` — same cost-realism overrides,
/// same 512-mint distribution, same §21.5/§21.6/§29.6 + live-stream cohorts, same
/// social ingest. Any divergence here changes the digest and fails the mirror.
pub fn drive(cfg: Config) -> Report {
    let mut cfg = cfg;
    cfg.gate_expected_move_bps = 1_800;
    cfg.gate_protocol_bps = 450;
    cfg.gate_margin_bps = 150;
    cfg.gate_base_fixed_lamports = 200_000;
    cfg.gate_impact_den = 250_000;
    let mut eng = Engine::new(cfg, RunMode::Replay);
    let n = 512u64;
    for round in 0..6u64 {
        for m in 0..n {
            let mt = mint(m);
            for i in 0..3u64 {
                let (price_fp, signed_base) = main_scalp(m, round, i);
                eng.tick(AppEvent::MarketTrade {
                    mint: mt,
                    price_fp,
                    quote_lamports: 400_000 + (m % 13) * 1_000,
                    liquidity_lamports: 120_000_000 + (m % 350) * 1_000_000 + round * 7,
                    signed_base,
                    buyer_entity: (m + i) % 97,
                    age_slots: 10 + (m as u32 % 40),
                });
            }
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
        // §21.5 cohort: zombie markets.
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
        // §21.6 cohort: dense fresh launches.
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
        // §29.6 cohort: stale-narrative mints.
        if round == 0 {
            for s in 0..8u64 {
                eng.tick(AppEvent::NarrativeSample {
                    mint: mint(3_000 + s),
                    prior_active: 5,
                    new_mentions: 5_000 + s * 100,
                });
            }
        }
        // live-stream cohort: a coin watched on stream right now.
        let s_mult_bp: u64 = [10_000, 12_000, 14_000, 16_000, 18_000, 16_000][round as usize];
        let st = mint(4_000);
        let sbase = 1_000_000_000i128 * s_mult_bp as i128 / 10_000;
        for i in 0..8u64 {
            let selling = round == 5;
            eng.tick(AppEvent::MarketTrade {
                mint: st,
                price_fp: sbase + (i as i128) * 500_000,
                quote_lamports: 700_000,
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
        if round <= 2 {
            use pump_quant_ingest::social_source::{MockSocialSource, RawSocialPayload};
            let mint_b58 = "BmoVsKix7SdPJwY9PRDsX3jDux3rr78RHEycUwWod4qM";
            let ts0 = 1_000_000_000u64 + round * 60_000_000_000;
            let mut batch = vec![RawSocialPayload::new(
                format!("{{\"platform\":\"twitch\",\"author\":\"streamer\",\"community\":\"streamer\",\"text\":\"$LIVE {mint_b58} full send r{round}\",\"likes\":0,\"reposts\":0,\"replies\":0,\"echo\":false}}").into_bytes(),
                ts0,
            )];
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
        // §29 Discord paid-alpha cohort (Wave-3 LAWs D1/D2/D4/D5). Verbatim mirror
        // of golden_digest.rs: a designated caller in a paid room calls a mint early
        // (AlphaCall lane), which earns an on-chain confirm + microstructure and
        // admits (net attributes to the AlphaCall lane + the room's §29.8 ledger);
        // a second mint the same room calls has NO on-chain support and never admits.
        let alpha_win = b58_mint(ALPHA_WIN_B58);
        // Modest winner (peak ≈ +30% round 4, settling to a ≈ +20% consolidation
        // plateau round 5), below the forbidden fixed +35% rung — mirror of
        // golden_digest.rs (re-pin #15: round-5 settle lifted 11_000→12_000 so the
        // AlphaCall re-admits stay net-positive at realistic 0.1-SOL clips; keeps
        // derived out-earning fixed on the golden tape).
        let aw_mult_bp: u64 = [10_000, 11_000, 12_000, 12_500, 13_000, 12_000][round as usize];
        let aw_base = 1_000_000_000i128 * aw_mult_bp as i128 / 10_000;
        for i in 0..8u64 {
            let selling = round == 5;
            eng.tick(AppEvent::MarketTrade {
                mint: alpha_win,
                price_fp: aw_base + (i as i128) * 500_000,
                quote_lamports: 700_000,
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
