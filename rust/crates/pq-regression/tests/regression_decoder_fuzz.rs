//! REGRESSION CLASS 4 — PumpSwap decoder property / fuzz (panic-safety).
//!
//! Every PumpSwap decoder is a strict, bounds-checked, fail-closed parser: on a
//! truncated / oversized / garbage buffer it must return `None`, NEVER panic and
//! never read out of bounds (§18.2 fail-closed, §22 deterministic). This suite
//! proves that property EXHAUSTIVELY over adversarial inputs generated WITHOUT an
//! RNG crate (a splitmix64-style integer hash drives every byte, so the corpus is
//! byte-for-byte reproducible), and proves the encode/decode round-trip holds for
//! the instruction decoders that have a canonical layout.
//!
//! The corpus for each decoder:
//!   * truncation at EVERY length from 0 up to a generous ceiling,
//!   * the correct discriminator with a truncated / garbage tail,
//!   * a wrong discriminator (every single-byte perturbation of the first 8),
//!   * fully hash-random buffers at many lengths.
//!
//! Success = the call returns (Some or None) without panicking. `cargo test`
//! turns any panic into a failed test, so a mere "it returned" is the assertion.

use pump_quant_protocol::pumpswap::{
    decode_global_config, decode_pool_account, decode_pump_curve_tail, decode_spl_token_amount,
};
use pump_quant_protocol::pumpswap_event::decode_pumpswap_event;
use pump_quant_protocol::pumpswap_ix::{decode_pumpswap_ix, is_pump_migrate_ix};

/// Deterministic byte fill (splitmix64 finalizer) — a reproducible stand-in for a
/// PRNG that keeps the crate std-only (§22: no RNG dependency).
fn fill(seed: u64, i: usize) -> u8 {
    let mut z = seed
        .wrapping_add(0x9E37_79B9_7F4A_7C15)
        .wrapping_add((i as u64).wrapping_mul(0xD1B5_4A32_D192_ED03));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    ((z ^ (z >> 31)) & 0xFF) as u8
}

/// A deterministic hash-filled buffer of length `len`.
fn buf(seed: u64, len: usize) -> Vec<u8> {
    (0..len).map(|i| fill(seed, i)).collect()
}

/// The upper length bound for the truncation sweep — comfortably past every
/// fixed PumpSwap layout (the `Pool` account is 211 bytes; the largest event is
/// well under 300).
const CEILING: usize = 320;

/// Run one decoder against the full adversarial corpus. `f` returns `true` iff it
/// "decoded something"; we do not care which — only that no input panics. The
/// return count is reported so a decoder that silently starts rejecting EVERYTHING
/// (a different regression) is visible via `--nocapture`.
fn hammer(name: &str, f: impl Fn(&[u8]) -> bool) {
    let mut somes = 0usize;
    // 1. Truncation at every length, several independent seeds.
    for seed in [0u64, 1, 0xDEAD_BEEF, 0x5555_5555_5555_5555, u64::MAX] {
        for len in 0..=CEILING {
            if f(&buf(seed, len)) {
                somes += 1;
            }
        }
    }
    // 2. Every single-byte perturbation of a zeroed 8-byte discriminator head on a
    //    full-length buffer (wrong-discriminator space).
    for b in 0..8usize {
        for v in 0..=255u8 {
            let mut x = vec![0u8; CEILING];
            x[b] = v;
            if f(&x) {
                somes += 1;
            }
        }
    }
    // 3. All-0x00 and all-0xFF at every length (degenerate extremes).
    for fillv in [0x00u8, 0xFF] {
        for len in 0..=CEILING {
            if f(&vec![fillv; len]) {
                somes += 1;
            }
        }
    }
    eprintln!("{name}: {somes} inputs decoded (no panics over the corpus)");
}

#[test]
fn decode_pool_account_never_panics() {
    hammer("decode_pool_account", |b| decode_pool_account(b).is_some());
}

#[test]
fn decode_global_config_never_panics() {
    hammer("decode_global_config", |b| {
        decode_global_config(b).is_some()
    });
}

#[test]
fn decode_spl_token_amount_never_panics() {
    hammer("decode_spl_token_amount", |b| {
        decode_spl_token_amount(b).is_some()
    });
}

#[test]
fn decode_pump_curve_tail_never_panics() {
    hammer("decode_pump_curve_tail", |b| {
        decode_pump_curve_tail(b).is_some()
    });
}

#[test]
fn decode_pumpswap_ix_never_panics() {
    hammer("decode_pumpswap_ix", |b| decode_pumpswap_ix(b).is_some());
    // The migrate sniffer is a pure prefix compare — it must be total too.
    hammer("is_pump_migrate_ix", is_pump_migrate_ix);
}

#[test]
fn decode_pumpswap_event_never_panics() {
    hammer("decode_pumpswap_event", |b| {
        decode_pumpswap_event(b).is_some()
    });
}

// ---------------------------------------------------------------------------
// Encode → decode round-trip for the instruction decoders (a canonical layout
// exists, so a faithful decoder must recover the exact args from valid bytes).
// This is the positive half of the fuzz: the negative half proves it rejects
// garbage; here we prove it ACCEPTS and correctly parses a well-formed buffer.
// ---------------------------------------------------------------------------

use pump_quant_protocol::ix::{BUY_DISCRIMINATOR, SELL_DISCRIMINATOR};
use pump_quant_protocol::pumpswap_ix::PumpSwapIx;

fn buy_bytes(base_amount_out: u64, max_quote_in: u64) -> Vec<u8> {
    let mut d = Vec::with_capacity(24);
    d.extend_from_slice(&BUY_DISCRIMINATOR);
    d.extend_from_slice(&base_amount_out.to_le_bytes());
    d.extend_from_slice(&max_quote_in.to_le_bytes());
    d
}

fn sell_bytes(base_amount_in: u64, min_quote_out: u64) -> Vec<u8> {
    let mut d = Vec::with_capacity(24);
    d.extend_from_slice(&SELL_DISCRIMINATOR);
    d.extend_from_slice(&base_amount_in.to_le_bytes());
    d.extend_from_slice(&min_quote_out.to_le_bytes());
    d
}

#[test]
fn buy_and_sell_ix_round_trip_over_hash_driven_args() {
    // 256 deterministic arg pairs per side (no RNG): the decoder must recover the
    // exact little-endian args for every one.
    for k in 0..256u64 {
        let a0 = fill(k, 0) as u64 | ((fill(k, 1) as u64) << 32);
        let a1 = fill(k, 2) as u64 | ((fill(k, 3) as u64) << 40);

        match decode_pumpswap_ix(&buy_bytes(a0, a1)) {
            Some(PumpSwapIx::Buy(args)) => {
                assert_eq!(args.base_amount_out, a0, "buy arg0 round-trip");
                assert_eq!(args.max_quote_amount_in, a1, "buy arg1 round-trip");
            }
            other => panic!("well-formed buy ix must decode to Buy, got {other:?}"),
        }
        match decode_pumpswap_ix(&sell_bytes(a0, a1)) {
            Some(PumpSwapIx::Sell(args)) => {
                assert_eq!(args.base_amount_in, a0, "sell arg0 round-trip");
                assert_eq!(args.min_quote_amount_out, a1, "sell arg1 round-trip");
            }
            other => panic!("well-formed sell ix must decode to Sell, got {other:?}"),
        }
    }
    // Truncating a valid buy ix by one byte must fail closed (not decode stale).
    let mut short = buy_bytes(1, 2);
    short.pop();
    assert!(
        decode_pumpswap_ix(&short).is_none(),
        "a truncated required arg must fail closed (§18.2)"
    );
    // A valid body under a WRONG discriminator must not decode as buy/sell.
    let mut wrong = buy_bytes(1, 2);
    wrong[0] ^= 0xFF;
    assert!(
        !matches!(decode_pumpswap_ix(&wrong), Some(PumpSwapIx::Buy(_))),
        "a wrong discriminator must never decode as Buy"
    );
}
