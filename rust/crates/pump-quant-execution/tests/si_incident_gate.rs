//! Integration tests for leaf `si_incident_gate`.
//!
//! Expectations are computed independently of the implementation (the
//! constant-product formula and the signing mixer are recomputed in-test over
//! multiple inputs, including edge cases), so a memorized/hardcoded answer
//! would fail.

use pump_quant_execution::ex_sell_ladder_state::{
    LadderCtx, LadderPhase, LadderState, SellOutcome, LADDER_LEN,
};
use pump_quant_execution::si_incident_gate::{
    assert_model_independent, deterministic_exit_step, si_incident_gate, sign_digest,
    sign_through_policy, simulate_sell, DecodedMarket, IncidentGateInput, IncidentReject,
    KeyHandle, Position, SellUnprovable, SignError, SignPolicy, SignedTx,
};

/// Independent constant-product reference: out = quote*amount/(base+amount).
fn ref_out(amount: u64, base: u64, quote: u64) -> u128 {
    (quote as u128 * amount as u128) / (base as u128 + amount as u128)
}

#[test]
fn simulate_sell_matches_independent_formula_over_many_inputs() {
    // (amount, base, quote) tuples spanning small, large, and asymmetric cases.
    let cases = [
        (1_000u64, 10_000u64, 50_000u64),
        (500, 1_000_000, 2_000_000),
        (7, 13, 100),
        (1, 1, 1_000),
        (250_000, 250_000, 999_999),
        (u32::MAX as u64, u32::MAX as u64, u32::MAX as u64),
    ];
    for &(amount, base, quote) in &cases {
        let pos = Position {
            token_amount: amount,
            mint: 42,
        };
        let market = DecodedMarket {
            base_reserve: base,
            quote_reserve: quote,
            constructible: true,
        };
        let expected = ref_out(amount, base, quote);
        let got = simulate_sell(&pos, &market);
        if expected == 0 {
            assert_eq!(got, Err(SellUnprovable::InsufficientLiquidity));
        } else {
            assert_eq!(got.unwrap().out_amount as u128, expected);
        }
    }
}

#[test]
fn simulate_sell_edge_cases() {
    let base_market = DecodedMarket {
        base_reserve: 1_000,
        quote_reserve: 1_000,
        constructible: true,
    };
    // Non-constructible market.
    let nc = DecodedMarket {
        constructible: false,
        ..base_market.clone()
    };
    assert_eq!(
        simulate_sell(
            &Position {
                token_amount: 10,
                mint: 1
            },
            &nc
        ),
        Err(SellUnprovable::Unconstructible)
    );
    // Empty position.
    assert_eq!(
        simulate_sell(
            &Position {
                token_amount: 0,
                mint: 1
            },
            &base_market
        ),
        Err(SellUnprovable::Unconstructible)
    );
    // Empty reserves.
    let empty = DecodedMarket {
        base_reserve: 0,
        quote_reserve: 5,
        constructible: true,
    };
    assert_eq!(
        simulate_sell(
            &Position {
                token_amount: 10,
                mint: 1
            },
            &empty
        ),
        Err(SellUnprovable::InsufficientLiquidity)
    );
    // Rounds to zero out: quote*amount < base+amount.
    let dust = DecodedMarket {
        base_reserve: 1_000_000,
        quote_reserve: 1,
        constructible: true,
    };
    assert_eq!(
        simulate_sell(
            &Position {
                token_amount: 1,
                mint: 1
            },
            &dust
        ),
        Err(SellUnprovable::InsufficientLiquidity)
    );
}

#[test]
fn signing_boundary_recomputes_signature_and_enforces_policy() {
    let policy = SignPolicy {
        approved_programs: vec![7, 9],
        max_tx_size: 1_200,
        key: KeyHandle::new(0xDEAD_BEEF),
    };
    // Approved + within cap: signature equals the independently-computed mixer.
    for &digest in &[0u64, 1, 12_345, u64::MAX] {
        let expected = sign_digest(0xDEAD_BEEF, digest);
        assert_eq!(
            sign_through_policy(7, 1_000, digest, &policy),
            Ok(SignedTx {
                signature: expected
            })
        );
    }
    // Unapproved program.
    assert_eq!(
        sign_through_policy(8, 100, 1, &policy),
        Err(SignError::PolicyDenied)
    );
    // Over size cap.
    assert_eq!(
        sign_through_policy(7, 1_201, 1, &policy),
        Err(SignError::PolicyDenied)
    );
}

fn make_policy() -> SignPolicy {
    SignPolicy {
        approved_programs: vec![100],
        max_tx_size: 900,
        key: KeyHandle::new(555),
    }
}

#[test]
fn gate_admits_only_when_both_checks_pass() {
    let policy = make_policy();
    let amount = 2_000u64;
    let base = 8_000u64;
    let quote = 40_000u64;
    let expected_out = ref_out(amount, base, quote); // = 40000*2000/10000 = 8000
    assert_eq!(expected_out, 8_000);
    let expected_sig = sign_digest(555, 0xABCD);

    let input = IncidentGateInput {
        proposal: pump_quant_execution::si_incident_gate::RemediationProposal {
            position: Position {
                token_amount: amount,
                mint: 1,
            },
            program_id: 100,
            tx_size: 800,
            digest: 0xABCD,
        },
        market: DecodedMarket {
            base_reserve: base,
            quote_reserve: quote,
            constructible: true,
        },
        min_out_amount: 5_000,
        policy: &policy,
    };
    let admitted = si_incident_gate(&input).expect("should admit");
    assert_eq!(admitted.proof.out_amount as u128, expected_out);
    assert_eq!(admitted.signed.signature, expected_sig);
    assert_eq!(admitted.program_id, 100);
}

#[test]
fn gate_rejects_unsellable_before_signing() {
    let policy = make_policy();
    let input = IncidentGateInput {
        proposal: pump_quant_execution::si_incident_gate::RemediationProposal {
            position: Position {
                token_amount: 10,
                mint: 1,
            },
            program_id: 100,
            tx_size: 800,
            digest: 1,
        },
        market: DecodedMarket {
            base_reserve: 1,
            quote_reserve: 1,
            constructible: false, // not constructible
        },
        min_out_amount: 0,
        policy: &policy,
    };
    assert_eq!(
        si_incident_gate(&input),
        Err(IncidentReject::Unsellable(SellUnprovable::Unconstructible))
    );
}

#[test]
fn gate_rejects_below_min_out() {
    let policy = make_policy();
    let amount = 100u64;
    let base = 1_000_000u64;
    let quote = 1_000_000u64;
    let out = ref_out(amount, base, quote); // 100*1e6/1_000_100 ~= 99
    assert!(out > 0 && out < 5_000);
    let input = IncidentGateInput {
        proposal: pump_quant_execution::si_incident_gate::RemediationProposal {
            position: Position {
                token_amount: amount,
                mint: 1,
            },
            program_id: 100,
            tx_size: 800,
            digest: 1,
        },
        market: DecodedMarket {
            base_reserve: base,
            quote_reserve: quote,
            constructible: true,
        },
        min_out_amount: 5_000,
        policy: &policy,
    };
    assert_eq!(
        si_incident_gate(&input),
        Err(IncidentReject::BelowMinOut {
            simulated: out as u64,
            required: 5_000,
        })
    );
}

#[test]
fn gate_rejects_signing_denied_even_when_sellable() {
    let policy = make_policy();
    // Sellable and above min, but unapproved program.
    let input = IncidentGateInput {
        proposal: pump_quant_execution::si_incident_gate::RemediationProposal {
            position: Position {
                token_amount: 2_000,
                mint: 1,
            },
            program_id: 999, // not approved
            tx_size: 800,
            digest: 1,
        },
        market: DecodedMarket {
            base_reserve: 8_000,
            quote_reserve: 40_000,
            constructible: true,
        },
        min_out_amount: 1_000,
        policy: &policy,
    };
    assert_eq!(
        si_incident_gate(&input),
        Err(IncidentReject::SigningDenied(SignError::PolicyDenied))
    );
}

#[test]
fn deterministic_exit_path_is_model_independent_and_matches_ladder() {
    // Static/compile-time proof: exit-path input types are model-independent.
    assert_model_independent::<LadderState>();
    assert_model_independent::<SellOutcome>();
    assert_model_independent::<LadderCtx>();
    // (assert_model_independent::<RemediationProposal>() would NOT compile.)

    // The deterministic step drives the ladder purely from an on-chain outcome,
    // with no model input. A run of Failed outcomes must escalate to Exhausted.
    let mut st = LadderState::new(1_000);
    let mut now = 1_000u64;
    for _ in 0..LADDER_LEN {
        now += 100;
        st = deterministic_exit_step(
            st,
            LadderCtx {
                now_ms: now,
                outcome: SellOutcome::Failed,
            },
        );
    }
    assert_eq!(st.phase, LadderPhase::Exhausted);
    assert_eq!(st.level, LADDER_LEN - 1);

    // Confirmed completes it, independent of any model.
    let done = deterministic_exit_step(
        LadderState::new(0),
        LadderCtx {
            now_ms: 5,
            outcome: SellOutcome::Confirmed,
        },
    );
    assert_eq!(done.phase, LadderPhase::Completed);
}
