//! Tests for the §18.2 version-controlled protocol registry entries.
use pump_quant_protocol::decode::{decode_pump_curve, decode_pumpswap_pool};
use pump_quant_protocol::registry::{account_discriminator, entry, CurveModel, FeeKind, Venue};

#[test]
fn entries_carry_the_mandated_fields() {
    let pf = entry(Venue::PumpFun);
    assert_eq!(pf.venue, Venue::PumpFun);
    assert_eq!(pf.program_id, Venue::PumpFun.program_id());
    assert!(!pf.config_pda.is_empty());
    assert_eq!(pf.layout_version, 1);
    assert_eq!(pf.decoder_version, 1);
    assert_eq!(pf.fee_model.kind, FeeKind::FixedBondingCurve);
    assert_eq!(pf.curve_model, CurveModel::VirtualConstantProduct);
    // pump.fun positions migrate to the PumpSwap program.
    assert_eq!(pf.migration_target, Some(Venue::PumpSwap.program_id()));
    assert!(pf.effective_slot_end.is_none());

    let ps = entry(Venue::PumpSwap);
    assert_eq!(ps.venue, Venue::PumpSwap);
    assert_eq!(ps.program_id, Venue::PumpSwap.program_id());
    assert_eq!(ps.curve_model, CurveModel::ConstantProductAmm);
    // PumpSwap is terminal — nowhere to migrate to.
    assert_eq!(ps.migration_target, None);
}

#[test]
fn both_venues_quote_in_wsol() {
    assert_eq!(
        entry(Venue::PumpFun).quote_mint,
        entry(Venue::PumpSwap).quote_mint
    );
    assert_eq!(
        entry(Venue::PumpFun).quote_mint,
        "So11111111111111111111111111111111111111112"
    );
}

#[test]
fn account_discriminators_match_helper() {
    assert_eq!(
        entry(Venue::PumpFun).account_discriminator,
        account_discriminator(Venue::PumpFun)
    );
    assert_eq!(
        entry(Venue::PumpSwap).account_discriminator,
        account_discriminator(Venue::PumpSwap)
    );
    // The two venues have distinct account identities.
    assert_ne!(
        account_discriminator(Venue::PumpFun),
        account_discriminator(Venue::PumpSwap)
    );
}

#[test]
fn known_account_discriminator_values() {
    // sha256("account:BondingCurve")[..8] and sha256("account:Pool")[..8].
    assert_eq!(
        account_discriminator(Venue::PumpFun),
        [23, 183, 248, 55, 96, 216, 172, 96]
    );
    assert_eq!(
        account_discriminator(Venue::PumpSwap),
        [241, 154, 109, 4, 17, 177, 109, 188]
    );
}

#[test]
fn golden_fixtures_decode_cleanly() {
    let pf = entry(Venue::PumpFun);
    let c = decode_pump_curve(pf.golden_fixture).expect("golden curve decodes");
    assert_eq!(c.virtual_token, 1_072_000_000_000_000);
    assert_eq!(c.virtual_sol, 30_000_000_000);

    let ps = entry(Venue::PumpSwap);
    let p = decode_pumpswap_pool(ps.golden_fixture).expect("golden pool decodes");
    assert_eq!(p.base_reserve, 1_000_000_000_000);
    assert_eq!(p.quote_reserve, 40_000_000_000);
}

#[test]
fn content_digest_is_deterministic() {
    assert_eq!(
        entry(Venue::PumpFun).content_digest(),
        entry(Venue::PumpFun).content_digest()
    );
}

#[test]
fn content_digest_distinguishes_venues() {
    assert_ne!(
        entry(Venue::PumpFun).content_digest(),
        entry(Venue::PumpSwap).content_digest()
    );
}

#[test]
fn content_digest_is_not_all_zero() {
    assert!(entry(Venue::PumpFun)
        .content_digest()
        .iter()
        .any(|&b| b != 0));
}

#[test]
fn content_digest_detects_field_drift() {
    // Copy the entry and mutate a single recorded fact; the digest must move.
    let mut drifted = *entry(Venue::PumpFun);
    let base = drifted.content_digest();
    drifted.last_verified_slot += 1;
    assert_ne!(base, drifted.content_digest());

    let mut disc_drift = *entry(Venue::PumpFun);
    disc_drift.account_discriminator[0] ^= 0x01;
    assert_ne!(base, disc_drift.content_digest());
}

#[test]
fn effective_range_gating() {
    let pf = entry(Venue::PumpFun);
    assert!(!pf.is_effective_at(pf.effective_slot_start - 1));
    assert!(pf.is_effective_at(pf.effective_slot_start));
    // Open-ended entry is effective arbitrarily far in the future.
    assert!(pf.is_effective_at(u64::MAX));
}
