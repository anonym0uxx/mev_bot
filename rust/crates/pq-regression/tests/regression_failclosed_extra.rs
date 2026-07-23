//! REGRESSION CLASS 3/4 — additional fail-closed + money-arithmetic invariants.
//!
//!   * QUOTE-UNDECODED REJECT — a swap whose quote/pool bytes do not decode must
//!     yield `None` (a refusal), never a fabricated pool (§18.2 fail-closed).
//!   * NO-COPY-TRADE — an [`Order`] has no public constructor other than `to_order`,
//!     and an external signal carries no source-wallet field, so mirroring a
//!     specific wallet's trade is UNREPRESENTABLE (a construction-level guarantee);
//!     a blocked pipeline stage refuses to produce an order (fail-closed).
//!   * MONEY ARITHMETIC IS CHECKED — the bonding-curve / constant-product money
//!     math never panics and never wraps on adversarial (extreme) inputs: it either
//!     returns a valid widened result or `None`. A hash-driven property sweep (no
//!     RNG) plus explicit overflow/degenerate corners prove the widening discipline
//!     (§22 — silent money wrap is prohibited).

// ---------------------------------------------------------------------------
// Quote / pool undecoded ⇒ refusal, never a fabricated decode.
// ---------------------------------------------------------------------------

#[test]
fn undecoded_pool_bytes_are_refused_not_fabricated() {
    use pump_quant_protocol::pumpswap::{decode_pool_account, POOL_FIXED_LEN};
    use pump_quant_protocol::registry::PUMPSWAP_ACCOUNT_DISCRIMINATOR;

    // Wrong discriminator ⇒ None (identity checked first, §18.2).
    let mut wrong = vec![0u8; POOL_FIXED_LEN];
    wrong[0] = 0xFF;
    assert!(
        decode_pool_account(&wrong).is_none(),
        "a foreign discriminator must be refused, not fabricated into a pool"
    );
    // Right discriminator but truncated below the fixed layout ⇒ None.
    let mut short = vec![0u8; POOL_FIXED_LEN - 1];
    if short.len() >= 8 {
        short[0..8].copy_from_slice(&PUMPSWAP_ACCOUNT_DISCRIMINATOR);
    }
    assert!(
        decode_pool_account(&short).is_none(),
        "a truncated pool account must be refused (fail-closed)"
    );
    // Empty buffer ⇒ None.
    assert!(
        decode_pool_account(&[]).is_none(),
        "empty bytes must be refused"
    );
}

#[test]
fn unknown_instruction_discriminator_is_refused() {
    use pump_quant_protocol::pumpswap_ix::decode_pumpswap_ix;
    // A well-sized buffer under an UNKNOWN discriminator must not decode to any
    // known instruction — an unrecognized op is refused, never guessed.
    let mut data = vec![0u8; 24];
    for (i, b) in data.iter_mut().take(8).enumerate() {
        *b = 0xA0 + i as u8; // a discriminator matching none of the known ops
    }
    assert!(
        decode_pumpswap_ix(&data).is_none(),
        "an unknown instruction discriminator must fail closed"
    );
}

// ---------------------------------------------------------------------------
// No copy-trade: Order is un-forgeable and mirrors no wallet.
// ---------------------------------------------------------------------------

#[test]
fn order_has_no_bypass_constructor_and_mirrors_no_wallet() {
    use pump_quant_strategy::safety_integrity::{
        to_order, DeterministicPipeline, ExternalSignal, SignalKind, Stage,
    };

    let all_ok = DeterministicPipeline {
        feature_ok: true,
        liquidity_ok: true,
        risk_ok: true,
        economic_ok: true,
        sellability_ok: true,
        signing_ok: true,
    };
    // The ONLY path to an Order runs every deterministic stage; the produced order
    // carries just the candidate token — there is no source-wallet field to mirror.
    let sig = ExternalSignal {
        kind: SignalKind::Wallet,
        token_mint: 4242,
    };
    let order = to_order(sig, &all_ok).expect("a fully-cleared pipeline yields an order");
    assert_eq!(
        order.token_mint(),
        4242,
        "the order targets only the candidate token"
    );

    // Fail-closed: if ANY stage is not ok, no order is produced — it is Blocked at
    // the first failing stage, never bypassed.
    let mut gated = all_ok.clone();
    gated.risk_ok = false;
    match to_order(
        ExternalSignal {
            kind: SignalKind::Wallet,
            token_mint: 1,
        },
        &gated,
    ) {
        Err(blocked) => assert_eq!(
            blocked.stage,
            Stage::Risk,
            "a blocked pipeline must refuse at the failing stage, not emit an order"
        ),
        Ok(_) => panic!("a gated pipeline must NOT produce an order (no bypass ctor)"),
    }
}

// ---------------------------------------------------------------------------
// Money arithmetic is checked / widened — never panics, never wraps.
// ---------------------------------------------------------------------------

fn h(seed: u64, i: u64) -> u64 {
    let mut z = seed
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(i.wrapping_mul(0xD1B5_4A32_D192_ED03));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z ^ (z >> 31)
}

#[test]
fn pumpswap_amount_out_is_checked_and_monotone() {
    use pump_quant_protocol::curve::pumpswap_amount_out;

    // Extreme corners must be handled, not panic: max reserves & amounts.
    let _ = pumpswap_amount_out(u128::MAX, u128::MAX, u128::MAX, 0);
    let _ = pumpswap_amount_out(u128::MAX, u128::MAX, u128::MAX, 10_000);
    // A nonsensical fee (> 100%) is refused.
    assert!(
        pumpswap_amount_out(1_000, 1_000, 100, 10_001).is_none(),
        "fee_bps > 10_000 must be refused"
    );
    // An empty pool (zero reserves, zero net input) is refused, not a div-by-zero.
    assert!(
        pumpswap_amount_out(0, 0, 0, 0).is_none(),
        "an empty pool must be refused, never divide by zero"
    );

    // Property sweep (no RNG): across a hash-driven corpus the call never panics,
    // and for a fixed pool the output is monotone non-decreasing in input.
    for s in 0..2_000u64 {
        let reserve_in = u128::from(h(s, 1)) + 1;
        let reserve_out = u128::from(h(s, 2)) + 1;
        let fee_bps = (h(s, 3) % 12_000) as u32; // deliberately spans the > 100% region
        let a_in = u128::from(h(s, 4));
        let b_in = a_in.saturating_add(u128::from(h(s, 5)) + 1); // b_in > a_in

        // Never panics (a wrapping add/mul without a check would panic here in the
        // overflow-checked test profile).
        let out_a = pumpswap_amount_out(reserve_in, reserve_out, a_in, fee_bps);
        let out_b = pumpswap_amount_out(reserve_in, reserve_out, b_in, fee_bps);

        // Monotonicity holds only for a valid (fee ≤ 100%) call where both sides
        // produced a value.
        if fee_bps <= 10_000 {
            if let (Some(oa), Some(ob)) = (out_a, out_b) {
                assert!(
                    ob >= oa,
                    "more input must never yield less output (s={s}, {oa} -> {ob})"
                );
            }
        } else {
            assert!(
                out_a.is_none() && out_b.is_none(),
                "a >100% fee must always refuse"
            );
        }
    }
}

#[test]
fn pump_amount_out_never_panics_on_extremes() {
    use pump_quant_protocol::curve::pump_amount_out;
    use pump_quant_protocol::decode::PumpCurve;

    // A hash-driven sweep of curve reserves and buy sizes — the checked math must
    // return Some/None for every one, never panic or wrap.
    for s in 0..2_000u64 {
        let curve = PumpCurve {
            virtual_sol: h(s, 1),
            virtual_token: h(s, 2),
            real_sol: h(s, 3),
            real_token: h(s, 4),
            complete: s % 2 == 0,
        };
        let _ = pump_amount_out(&curve, h(s, 5));
    }
    // The degenerate empty curve with a zero net input would divide by zero
    // (v_sol == 0 and net == 0) — it must be refused, never panic.
    let empty = PumpCurve {
        virtual_sol: 0,
        virtual_token: 0,
        real_sol: 0,
        real_token: 0,
        complete: false,
    };
    assert!(
        pump_amount_out(&empty, 0).is_none(),
        "a zero-reserve curve with zero input must be refused (no divide by zero)"
    );
}
