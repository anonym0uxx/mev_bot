#![allow(unused_imports)]
use pump_quant_execution::ex_construction_gate::*;

fn op(venue: GateVenue, side: GateSide) -> LogicalOp {
    LogicalOp {
        venue,
        side,
        arg0: 1_234_567,
        arg1: 9_876_543,
        primary: [0x11; 32],
    }
}

#[test]
fn build_data_layout_is_discriminator_then_two_u64_le() {
    let o = op(GateVenue::PumpFun, GateSide::Buy);
    let ix = build_ix(o);
    assert_eq!(ix.data.len(), IX_DATA_LEN);
    assert_eq!(&ix.data[0..8], &PUMPFUN_BUY_DISCRIMINATOR);
    assert_eq!(&ix.data[8..16], &o.arg0.to_le_bytes());
    assert_eq!(&ix.data[16..24], &o.arg1.to_le_bytes());
    assert_eq!(ix.program_id, PUMPFUN_PROGRAM_ID);
    // account ordering: signer/payer first, primary (writable, non-signer) second.
    assert!(ix.accounts[0].is_signer);
    assert_eq!(ix.accounts[1].pubkey, o.primary);
    assert!(!ix.accounts[1].is_signer);
    assert!(ix.accounts[1].is_writable);
}

#[test]
fn parity_and_roundtrip_pass_for_correctly_built_ix() {
    for (v, s) in [
        (GateVenue::PumpFun, GateSide::Buy),
        (GateVenue::PumpFun, GateSide::Sell),
        (GateVenue::PumpSwap, GateSide::Buy),
        (GateVenue::PumpSwap, GateSide::Sell),
    ] {
        let o = op(v, s);
        let ix = build_ix(o);
        let golden = golden_fixture(o);
        let status = ConstructionValidationGate::validate(&ix, o, &golden);
        assert_eq!(status, LiveValidatedStatus::ValidatedDeterministic);
        assert!(status.is_validated());
    }
}

#[test]
fn roundtrip_decodes_to_same_logical_op() {
    let o = op(GateVenue::PumpSwap, GateSide::Sell);
    let ix = build_ix(o);
    assert_eq!(decode_ix(&ix), Some(o));
}

#[test]
fn parity_mismatch_on_mutated_data_byte_is_caught() {
    let o = op(GateVenue::PumpFun, GateSide::Buy);
    let golden = golden_fixture(o);
    let mut ix = build_ix(o);
    // Flip one byte of an argument: bytes differ from golden.
    ix.data[9] ^= 0xFF;
    let status = ConstructionValidationGate::validate(&ix, o, &golden);
    assert_eq!(
        status,
        LiveValidatedStatus::Rejected(GateRejection::FixtureParityMismatch)
    );
    assert!(!status.is_validated());
}

#[test]
fn parity_mismatch_on_reordered_accounts_is_caught() {
    let o = op(GateVenue::PumpFun, GateSide::Sell);
    let golden = golden_fixture(o);
    let mut ix = build_ix(o);
    // Swap account-meta order: same set, wrong ordering -> parity must fail.
    ix.accounts.swap(0, 2);
    let status = ConstructionValidationGate::validate(&ix, o, &golden);
    assert_eq!(
        status,
        LiveValidatedStatus::Rejected(GateRejection::FixtureParityMismatch)
    );
}

#[test]
fn parity_mismatch_on_flipped_flag_is_caught() {
    let o = op(GateVenue::PumpSwap, GateSide::Buy);
    let golden = golden_fixture(o);
    let mut ix = build_ix(o);
    ix.accounts[1].is_writable = !ix.accounts[1].is_writable;
    let status = ConstructionValidationGate::validate(&ix, o, &golden);
    assert_eq!(
        status,
        LiveValidatedStatus::Rejected(GateRejection::FixtureParityMismatch)
    );
}

#[test]
fn roundtrip_mismatch_when_intended_op_differs() {
    // Built ix is internally consistent and matches its own golden, but the
    // caller's *intended* op claims a different side -> round-trip rung rejects.
    let built_op = op(GateVenue::PumpFun, GateSide::Buy);
    let ix = build_ix(built_op);
    let golden = serialize(&ix);
    let mut intended = built_op;
    intended.side = GateSide::Sell;
    let status = ConstructionValidationGate::validate(&ix, intended, &golden);
    assert_eq!(
        status,
        LiveValidatedStatus::Rejected(GateRejection::RoundTripMismatch)
    );
}

#[test]
fn decode_fails_closed_on_unknown_program_and_short_data() {
    let o = op(GateVenue::PumpFun, GateSide::Buy);
    let mut ix = build_ix(o);
    ix.program_id = [0xAB; 32];
    assert_eq!(decode_ix(&ix), None);

    let mut short = build_ix(o);
    short.data.truncate(10);
    assert_eq!(decode_ix(&short), None);
}

#[test]
fn phase_b_sim_path_validates_live_and_rejects() {
    // A passing simulator lifts deterministic validation to ValidatedLive.
    struct AcceptSim;
    impl LiveStateSimulator for AcceptSim {
        fn simulate(&self, _ix: &BuiltIx, _op: LogicalOp) -> bool {
            true
        }
    }
    let o = op(GateVenue::PumpSwap, GateSide::Sell);
    let ix = build_ix(o);
    let golden = golden_fixture(o);
    let status = ConstructionValidationGate::validate_with_sim(&ix, o, &golden, &AcceptSim);
    assert_eq!(status, LiveValidatedStatus::ValidatedLive);

    // The Phase-B deferred stub never passes the live rung.
    let deferred =
        ConstructionValidationGate::validate_with_sim(&ix, o, &golden, &PhaseBDeferredSim);
    assert_eq!(
        deferred,
        LiveValidatedStatus::Rejected(GateRejection::LiveStateRejected)
    );

    // A deterministic rejection short-circuits before the sim runs.
    let mut bad = build_ix(o);
    bad.data[8] ^= 1;
    let status2 = ConstructionValidationGate::validate_with_sim(&bad, o, &golden, &AcceptSim);
    assert_eq!(
        status2,
        LiveValidatedStatus::Rejected(GateRejection::FixtureParityMismatch)
    );
}

#[test]
fn venue_side_discriminators_are_distinct() {
    let all = [
        build_ix(op(GateVenue::PumpFun, GateSide::Buy)).data[0..8].to_vec(),
        build_ix(op(GateVenue::PumpFun, GateSide::Sell)).data[0..8].to_vec(),
        build_ix(op(GateVenue::PumpSwap, GateSide::Buy)).data[0..8].to_vec(),
        build_ix(op(GateVenue::PumpSwap, GateSide::Sell)).data[0..8].to_vec(),
    ];
    for i in 0..all.len() {
        for j in (i + 1)..all.len() {
            assert_ne!(all[i], all[j], "discriminators {i},{j} collide");
        }
    }
}
