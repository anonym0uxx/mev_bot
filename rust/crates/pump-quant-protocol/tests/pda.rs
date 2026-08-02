//! PDA derivation fixtures: every constant in `VENUE_TX_LAYOUTS.md` §2 must
//! re-derive from first principles, and the bump-253 cases prove the on-curve
//! rejection actually fires (bumps 255 and 254 must be *valid curve points*,
//! i.e. rejected, before 253 can be the answer).

use pump_quant_protocol::pda::{
    anchor_account_discriminator, anchor_instruction_discriminator, create_program_address,
    derive_ata, find_program_address, is_on_curve, PdaError,
};
use pump_quant_protocol::venue_accounts::{
    FEE_PROGRAM_ID, PUMPSWAP_EVENT_AUTHORITY, PUMPSWAP_FEE_CONFIG, PUMPSWAP_GLOBAL_CONFIG,
    PUMPSWAP_GLOBAL_VOLUME_ACCUMULATOR, PUMPSWAP_PROGRAM_ID, PUMP_EVENT_AUTHORITY, PUMP_FEE_CONFIG,
    PUMP_GLOBAL, PUMP_GLOBAL_VOLUME_ACCUMULATOR, PUMP_PROGRAM_ID, TOKEN_PROGRAM_ID, WSOL_MINT,
};

// -- §2 pump.fun table -------------------------------------------------------

#[test]
fn pump_global_rederives() {
    let (addr, bump) = find_program_address(&[b"global"], &PUMP_PROGRAM_ID).unwrap();
    assert_eq!(addr, PUMP_GLOBAL);
    assert_eq!(bump, 255);
}

#[test]
fn pump_event_authority_rederives() {
    let (addr, bump) = find_program_address(&[b"__event_authority"], &PUMP_PROGRAM_ID).unwrap();
    assert_eq!(addr, PUMP_EVENT_AUTHORITY);
    assert_eq!(bump, 255);
}

#[test]
fn pump_global_volume_accumulator_rederives() {
    let (addr, bump) =
        find_program_address(&[b"global_volume_accumulator"], &PUMP_PROGRAM_ID).unwrap();
    assert_eq!(addr, PUMP_GLOBAL_VOLUME_ACCUMULATOR);
    assert_eq!(bump, 255);
}

// -- §2 PumpSwap table -------------------------------------------------------

#[test]
fn pumpswap_global_config_rederives() {
    let (addr, bump) = find_program_address(&[b"global_config"], &PUMPSWAP_PROGRAM_ID).unwrap();
    assert_eq!(addr, PUMPSWAP_GLOBAL_CONFIG);
    assert_eq!(bump, 255);
}

#[test]
fn pumpswap_event_authority_rederives() {
    let (addr, bump) = find_program_address(&[b"__event_authority"], &PUMPSWAP_PROGRAM_ID).unwrap();
    assert_eq!(addr, PUMPSWAP_EVENT_AUTHORITY);
    assert_eq!(bump, 255);
}

#[test]
fn pumpswap_global_volume_accumulator_rederives() {
    let (addr, bump) =
        find_program_address(&[b"global_volume_accumulator"], &PUMPSWAP_PROGRAM_ID).unwrap();
    assert_eq!(addr, PUMPSWAP_GLOBAL_VOLUME_ACCUMULATOR);
    assert_eq!(bump, 255);
}

// -- fee_config: venue-specific, and the on-curve check's positive controls --

/// pump.fun `fee_config` sits at **bump 253**: candidates 255 and 254 are
/// valid curve points and must be rejected. A broken on-curve check cannot
/// pass this test — it would return bump 255 with a different address.
#[test]
fn pump_fee_config_rederives_at_bump_253() {
    let (addr, bump) =
        find_program_address(&[b"fee_config", &PUMP_PROGRAM_ID], &FEE_PROGRAM_ID).unwrap();
    assert_eq!(addr, PUMP_FEE_CONFIG);
    assert_eq!(bump, 253);
}

#[test]
fn pumpswap_fee_config_rederives() {
    let (addr, bump) =
        find_program_address(&[b"fee_config", &PUMPSWAP_PROGRAM_ID], &FEE_PROGRAM_ID).unwrap();
    assert_eq!(addr, PUMPSWAP_FEE_CONFIG);
    assert_eq!(bump, 255);
}

/// The two venues' `fee_config` addresses must differ — the cross-venue
/// constant mixup this suite exists to make impossible.
#[test]
fn fee_configs_are_venue_specific() {
    assert_ne!(PUMP_FEE_CONFIG, PUMPSWAP_FEE_CONFIG);
}

/// Negative control for `create_program_address`: the bumps the search
/// rejected must individually refuse (they are on-curve).
#[test]
fn negative_control_on_curve_bumps_refuse() {
    for bump in [255u8, 254u8] {
        let r = create_program_address(&[b"fee_config", &PUMP_PROGRAM_ID], bump, &FEE_PROGRAM_ID);
        assert_eq!(
            r,
            Err(PdaError::NoViableBump),
            "bump {bump} must be on-curve"
        );
    }
    // And the accepted bump succeeds.
    let ok = create_program_address(&[b"fee_config", &PUMP_PROGRAM_ID], 253, &FEE_PROGRAM_ID);
    assert_eq!(ok, Ok(PUMP_FEE_CONFIG));
}

// -- per-entity vectors (cross-derived by an independent implementation) -----

/// `["bonding-curve", WSOL_MINT-as-mint]` → `6PiyjiAPkp2KdZtqkyQYzVsD1Prv7t8v4TaYd8ip4YFd`,
/// bump 253 — a second bump-253 fixture, exercising per-mint seeds.
#[test]
fn bonding_curve_pda_vector() {
    let (addr, bump) =
        find_program_address(&[b"bonding-curve", &WSOL_MINT], &PUMP_PROGRAM_ID).unwrap();
    let expected: [u8; 32] = [
        80, 28, 173, 102, 183, 30, 149, 143, 189, 14, 126, 152, 146, 254, 146, 168, 3, 99, 105,
        105, 212, 87, 190, 60, 209, 14, 161, 148, 236, 136, 180, 116,
    ];
    assert_eq!(addr, expected);
    assert_eq!(bump, 253);
}

/// ATA derivation vector: `ata(PUMP_GLOBAL-as-wallet, spl-token, WSOL)` →
/// `38dZhCVonMKfqUzWTu9t9KvcoF7ejRZynzcB98DpRKp7`, cross-derived by the
/// Python reference implementation in `VENUE_TX_LAYOUTS.md` §6.
#[test]
fn ata_derivation_vector() {
    let ata = derive_ata(&PUMP_GLOBAL, &TOKEN_PROGRAM_ID, &WSOL_MINT).unwrap();
    let expected: [u8; 32] = [
        31, 171, 200, 61, 219, 39, 181, 114, 231, 135, 147, 29, 172, 100, 104, 154, 129, 60, 106,
        144, 206, 95, 206, 17, 253, 180, 82, 114, 198, 103, 24, 36,
    ];
    assert_eq!(ata, expected);
}

// -- discriminators re-derive through the local sha256 -----------------------

#[test]
fn instruction_discriminators_rederive() {
    assert_eq!(
        anchor_instruction_discriminator("buy"),
        pump_quant_protocol::ix::BUY_DISCRIMINATOR
    );
    assert_eq!(
        anchor_instruction_discriminator("sell"),
        pump_quant_protocol::ix::SELL_DISCRIMINATOR
    );
}

#[test]
fn global_account_discriminator_rederives() {
    assert_eq!(
        anchor_account_discriminator("Global"),
        pump_quant_protocol::decode::GLOBAL_ACCOUNT_DISCRIMINATOR
    );
}

// -- input bounds fail closed ------------------------------------------------

#[test]
fn negative_control_too_many_seeds_refuses() {
    let seed: &[u8] = b"s";
    let seeds = [seed; 17];
    assert_eq!(
        find_program_address(&seeds, &PUMP_PROGRAM_ID),
        Err(PdaError::TooManySeeds)
    );
}

#[test]
fn negative_control_oversized_seed_refuses() {
    let long = [0u8; 33];
    assert_eq!(
        find_program_address(&[&long], &PUMP_PROGRAM_ID),
        Err(PdaError::SeedTooLong)
    );
}

/// A real public key (the fee program's address is NOT a PDA — it is a keyed
/// account) must read as on-curve; PDAs must not.
#[test]
fn on_curve_classifies_known_points() {
    // Every derived PDA above is off-curve by construction.
    assert!(!is_on_curve(&PUMP_GLOBAL));
    assert!(!is_on_curve(&PUMP_FEE_CONFIG));
    // The system program's all-zero key decodes to a valid point (y = 0).
    assert!(is_on_curve(&[0u8; 32]));
}
