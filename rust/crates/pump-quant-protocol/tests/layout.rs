//! The layout gate and the fixture differ.
//!
//! The controls that matter here are the refusals. A registry that cannot
//! refuse is not a gate, and a differ that cannot detect the 2026-08-02
//! finding is not a differ - so that exact finding is reproduced as a test.

use pump_quant_protocol::layout::{
    diff_layout, required_layouts, LayoutDelta, LayoutError, LayoutKey, LayoutRegistry,
    ObservedAccount, Side, Variant, Venue, VerifiedLayout,
};
use pump_quant_protocol::venue_accounts::{
    pump_buy_accounts, pump_buy_accounts_with_tail, pump_sell_accounts_with_tail, AccountMeta,
    FeeTail, PumpCurveCtx, FEE_PROGRAM_GLOBAL, PUMP_GLOBAL, TOKEN_PROGRAM_ID, WSOL_MINT,
};

fn pk(tag: u8) -> [u8; 32] {
    let mut k = [7u8; 32];
    k[0] = tag;
    k
}

fn ctx() -> PumpCurveCtx {
    PumpCurveCtx {
        mint: pk(1),
        user: pk(2),
        fee_recipient: pk(3),
        creator: pk(4),
        token_program: TOKEN_PROGRAM_ID,
        is_cashback_coin: false,
        quote_mint: WSOL_MINT,
    }
}

fn key(side: Side) -> LayoutKey {
    LayoutKey {
        venue: Venue::PumpFun,
        side,
        variant: Variant::plain(),
    }
}

fn sig(b: u8) -> [u8; 64] {
    [b; 64]
}

// -- the gate refuses by default ---------------------------------------------

/// The control the whole module exists for: an empty registry builds nothing.
#[test]
fn negative_control_empty_registry_refuses_everything() {
    let reg = LayoutRegistry::new();
    assert_eq!(
        reg.require(&key(Side::Buy), 18),
        Err(LayoutError::Unverified(key(Side::Buy)))
    );
    assert_eq!(
        reg.require(&key(Side::Sell), 16),
        Err(LayoutError::Unverified(key(Side::Sell)))
    );
    assert!(reg.verified().is_empty());
}

/// Provenance cannot be manufactured: an all-zero signature is a default, not
/// a transaction, and recording it must fail.
#[test]
fn negative_control_zero_signature_is_not_provenance() {
    let mut reg = LayoutRegistry::new();
    let bad = VerifiedLayout {
        key: key(Side::Buy),
        account_count: 18,
        verifying_slot: 436_828_370,
        verifying_signature: [0u8; 64],
    };
    assert_eq!(
        reg.record_verified(bad),
        Err(LayoutError::Unverified(key(Side::Buy)))
    );
    assert!(reg.get(&key(Side::Buy)).is_none());
}

/// A builder that drifts away from a verified layout is caught at build time.
#[test]
fn negative_control_count_drift_is_caught() {
    let mut reg = LayoutRegistry::new();
    reg.record_verified(VerifiedLayout {
        key: key(Side::Buy),
        account_count: 18,
        verifying_slot: 436_828_370,
        verifying_signature: sig(9),
    })
    .unwrap();
    // The falsified 17-account builder must not pass a fixture proving 18.
    assert_eq!(
        reg.require(&key(Side::Buy), 17),
        Err(LayoutError::CountDisagrees {
            key: key(Side::Buy),
            built: 17,
            verified: 18
        })
    );
    assert!(reg.require(&key(Side::Buy), 18).is_ok());
}

/// A layout verified once and trusted forever is a gate that cannot fail.
#[test]
fn negative_control_stale_verification_refuses() {
    let mut reg = LayoutRegistry::new();
    reg.record_verified(VerifiedLayout {
        key: key(Side::Buy),
        account_count: 18,
        verifying_slot: 1_000_000,
        verifying_signature: sig(3),
    })
    .unwrap();
    assert!(reg
        .require_fresh(&key(Side::Buy), 18, 1_050_000, 100_000)
        .is_ok());
    assert_eq!(
        reg.require_fresh(&key(Side::Buy), 18, 1_200_000, 100_000),
        Err(LayoutError::Stale {
            key: key(Side::Buy),
            verified_at: 1_000_000,
            now: 1_200_000,
            max_age_slots: 100_000
        })
    );
}

/// Re-verifying replaces rather than accumulates, so the slot moves forward.
#[test]
fn re_verification_replaces_the_record() {
    let mut reg = LayoutRegistry::new();
    for (slot, s) in [(1_000_000u64, 1u8), (2_000_000, 2)] {
        reg.record_verified(VerifiedLayout {
            key: key(Side::Buy),
            account_count: 18,
            verifying_slot: slot,
            verifying_signature: sig(s),
        })
        .unwrap();
    }
    assert_eq!(reg.verified().len(), 1);
    assert_eq!(reg.get(&key(Side::Buy)).unwrap().verifying_slot, 2_000_000);
}

// -- the differ reproduces the 2026-08-02 finding ----------------------------

/// THE REGRESSION TEST FOR THE FALSIFICATION. The 17-account builder against
/// an 18-account observation must report exactly one missing tail account -
/// not a cascade, and not silence.
#[test]
fn differ_detects_the_missing_fee_program_tail() {
    let built = pump_buy_accounts(&ctx()).unwrap();
    assert_eq!(built.len(), 17, "the falsified shape");

    // The chain: the same 17, plus one writable fee-program account.
    let extra = FEE_PROGRAM_GLOBAL;
    let mut observed: Vec<ObservedAccount> = built
        .iter()
        .map(|m| ObservedAccount {
            pubkey: m.pubkey,
            is_signer: m.is_signer,
            is_writable: m.is_writable,
        })
        .collect();
    observed.push(ObservedAccount {
        pubkey: extra,
        is_signer: false,
        is_writable: true,
    });

    let deltas = diff_layout(&built, &observed);
    assert_eq!(deltas.len(), 2, "one count mismatch + one missing tail");
    assert!(deltas.contains(&LayoutDelta::CountMismatch {
        built: 17,
        observed: 18
    }));
    assert!(deltas.contains(&LayoutDelta::MissingTail {
        index: 17,
        observed: extra
    }));
}

/// With the tail supplied, the same comparison is clean. This is what
/// criterion 77(a) parity looks like when it passes.
#[test]
fn differ_is_empty_once_the_tail_is_added() {
    let built = pump_buy_accounts_with_tail(&ctx(), FeeTail::FeeProgramGlobal).unwrap();
    assert_eq!(built.len(), 18);
    let observed: Vec<ObservedAccount> = built
        .iter()
        .map(|m| ObservedAccount {
            pubkey: m.pubkey,
            is_signer: m.is_signer,
            is_writable: m.is_writable,
        })
        .collect();
    assert!(
        diff_layout(&built, &observed).is_empty(),
        "byte-level parity"
    );
}

/// A flag difference must be caught even when every pubkey matches. A writable
/// account built read-only fails on chain; the reverse silently widens the
/// write-lock set.
#[test]
fn differ_catches_flag_only_divergence() {
    let built = pump_buy_accounts(&ctx()).unwrap();
    let mut observed: Vec<ObservedAccount> = built
        .iter()
        .map(|m| ObservedAccount {
            pubkey: m.pubkey,
            is_signer: m.is_signer,
            is_writable: m.is_writable,
        })
        .collect();
    observed[9].is_writable = false; // creator_vault, writable on chain
    let deltas = diff_layout(&built, &observed);
    assert_eq!(deltas.len(), 1);
    assert!(matches!(
        deltas[0],
        LayoutDelta::FlagMismatch { index: 9, .. }
    ));
}

/// An account substituted at one position must not cascade into N deltas.
#[test]
fn differ_reports_one_delta_per_position() {
    let built = pump_buy_accounts(&ctx()).unwrap();
    let mut observed: Vec<ObservedAccount> = built
        .iter()
        .map(|m| ObservedAccount {
            pubkey: m.pubkey,
            is_signer: m.is_signer,
            is_writable: m.is_writable,
        })
        .collect();
    observed[0].pubkey = pk(99);
    let deltas = diff_layout(&built, &observed);
    assert_eq!(deltas.len(), 1);
    assert!(matches!(
        deltas[0],
        LayoutDelta::PubkeyMismatch {
            index: 0,
            built: PUMP_GLOBAL,
            ..
        }
    ));
}

/// The builder emitting an account the chain lacks is the opposite failure and
/// must also be caught.
#[test]
fn differ_catches_an_invented_tail() {
    let built = pump_buy_accounts_with_tail(&ctx(), FeeTail::SharingConfig).unwrap();
    let observed: Vec<ObservedAccount> = built[..17]
        .iter()
        .map(|m| ObservedAccount {
            pubkey: m.pubkey,
            is_signer: m.is_signer,
            is_writable: m.is_writable,
        })
        .collect();
    let deltas = diff_layout(&built, &observed);
    assert!(deltas.contains(&LayoutDelta::CountMismatch {
        built: 18,
        observed: 17
    }));
    assert!(deltas
        .iter()
        .any(|d| matches!(d, LayoutDelta::ExtraTail { index: 17, .. })));
}

// -- tail shapes -------------------------------------------------------------

#[test]
fn tail_changes_counts_on_every_pump_shape() {
    let mut c = ctx();
    assert_eq!(
        pump_buy_accounts_with_tail(&c, FeeTail::None)
            .unwrap()
            .len(),
        17
    );
    assert_eq!(
        pump_buy_accounts_with_tail(&c, FeeTail::SharingConfig)
            .unwrap()
            .len(),
        18
    );
    assert_eq!(
        pump_sell_accounts_with_tail(&c, FeeTail::None)
            .unwrap()
            .len(),
        15
    );
    assert_eq!(
        pump_sell_accounts_with_tail(&c, FeeTail::SharingConfig)
            .unwrap()
            .len(),
        16
    );
    c.is_cashback_coin = true;
    assert_eq!(
        pump_sell_accounts_with_tail(&c, FeeTail::None)
            .unwrap()
            .len(),
        16
    );
    assert_eq!(
        pump_sell_accounts_with_tail(&c, FeeTail::SharingConfig)
            .unwrap()
            .len(),
        17
    );
}

/// The discriminating test, encoded: the per-mint hypothesis must produce
/// DIFFERENT addresses for different mints; the constant one must not. This is
/// exactly the observation Hermes is asked to make on chain.
#[test]
fn sharing_config_is_per_mint_and_fee_program_global_is_not() {
    let a = ctx();
    let mut b = ctx();
    b.mint = pk(77);

    let sa = *pump_buy_accounts_with_tail(&a, FeeTail::SharingConfig)
        .unwrap()
        .last()
        .unwrap();
    let sb = *pump_buy_accounts_with_tail(&b, FeeTail::SharingConfig)
        .unwrap()
        .last()
        .unwrap();
    assert_ne!(
        sa.pubkey, sb.pubkey,
        "sharing-config must vary with the mint"
    );

    let ga = *pump_buy_accounts_with_tail(&a, FeeTail::FeeProgramGlobal)
        .unwrap()
        .last()
        .unwrap();
    let gb = *pump_buy_accounts_with_tail(&b, FeeTail::FeeProgramGlobal)
        .unwrap()
        .last()
        .unwrap();
    assert_eq!(ga.pubkey, gb.pubkey, "fee-program-global must NOT vary");
    assert_eq!(ga.pubkey, FEE_PROGRAM_GLOBAL);

    assert!(
        sa.is_writable && ga.is_writable,
        "observed writable in every sample"
    );
}

/// The observed escape hatch reproduces truth verbatim, and still refuses a
/// zero.
#[test]
fn observed_tail_is_verbatim_and_refuses_zero() {
    let seen = pk(123);
    let v = pump_buy_accounts_with_tail(&ctx(), FeeTail::Observed(seen)).unwrap();
    assert_eq!(v.last().unwrap().pubkey, seen);
    assert!(pump_buy_accounts_with_tail(&ctx(), FeeTail::Observed([0u8; 32])).is_err());
}

#[test]
fn buyback_vault_tail_is_writable_and_refuses_zero() {
    // Item 6: the trailing fee account is a BuybackVault PDA. Verify the
    // builder emits it as a writable trailing account and rejects zero.
    let vault = pk(42);
    let v = pump_buy_accounts_with_tail(&ctx(), FeeTail::BuybackVault(vault)).unwrap();
    assert_eq!(v.last().unwrap().pubkey, vault);
    assert!(v.last().unwrap().is_writable);
    assert!(pump_buy_accounts_with_tail(&ctx(), FeeTail::BuybackVault([0u8; 32])).is_err());
}

// -- coverage ----------------------------------------------------------------

/// The permutation matrix is the difference between "we verified a buy" and
/// "we verified the buy path". Nothing is covered until it is recorded.
#[test]
fn coverage_report_starts_at_zero_and_tracks_what_is_proven() {
    let req = required_layouts(Venue::PumpFun);
    assert_eq!(req.len(), 16, "2 sides x cashback x token2022 x quote");
    let ps = required_layouts(Venue::PumpSwap);
    assert_eq!(ps.len(), 32, "PumpSwap adds the reversed-pool dimension");

    let mut reg = LayoutRegistry::new();
    assert_eq!(reg.missing(&req).len(), 16, "nothing proven yet");

    reg.record_verified(VerifiedLayout {
        key: key(Side::Buy),
        account_count: 18,
        verifying_slot: 436_828_370,
        verifying_signature: sig(1),
    })
    .unwrap();
    assert_eq!(
        reg.missing(&req).len(),
        15,
        "one permutation down, fifteen to go"
    );
}

/// AccountMeta flags survive the round trip the differ depends on.
#[test]
fn account_meta_flags_are_faithful() {
    let ro = AccountMeta::ro(pk(1));
    let w = AccountMeta::w(pk(2));
    let ws = AccountMeta::ws(pk(3));
    assert_eq!((ro.is_signer, ro.is_writable), (false, false));
    assert_eq!((w.is_signer, w.is_writable), (false, true));
    assert_eq!((ws.is_signer, ws.is_writable), (true, true));
}
