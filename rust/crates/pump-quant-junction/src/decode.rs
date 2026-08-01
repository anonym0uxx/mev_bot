//! Decode bonding-curve account snapshots from accountSubscribe RPC responses.
//!
//! This module is a THIN ADAPTER over `pump_quant_protocol::decode_pump_curve`,
//! which is the discriminator-verified, bounds-checked, integer-only decoder
//! that the protocol crate owns. We DO NOT reimplement the account layout here
//! — that would duplicate the on-chain field offsets into a second source of
//! truth that can drift (§18.2).
//!
//! The junction's job is: take a raw account blob from accountSubscribe,
//! call the protocol decoder, and wrap the result into an `AppEvent::OnchainConfirm`
//! with structural provenance. The protocol crate owns WHAT the fields mean;
//! the junction owns WHERE they go.
//!
//! CRITICAL (blocker 2): `real_sol_lamports` in the resulting `OnchainConfirm`
//! comes from the decoded account snapshot — the SAME blob, the SAME slot, the
//! SAME decode path as `virtual_sol_lamports`. It is NOT derived from
//! `virtual_sol - 30 SOL`. The gate compares two independently-decoded fields
//! from one snapshot, not a number to itself.

use pump_quant_app::event::AppEvent;
use pump_quant_domain::ids::Mint;
use pump_quant_protocol::decode::decode_pump_curve;

/// Decode a bonding-curve account blob into a provenanced `OnchainConfirm`.
///
/// The raw bytes come from `accountSubscribe.data` (base64-decoded by the
/// WebSocket caller before reaching this function). We delegate to the
/// protocol crate's `decode_pump_curve`, which verifies the 8-byte Anchor
/// discriminator (§18.2) and bounds-checks every field.
///
/// Returns `None` when:
/// - The blob is too short (`< 49 bytes`).
/// - The discriminator does not match the pump.fun BondingCurve account type.
/// - The `complete` byte is not a canonical boolean.
///
/// On success, the `ProvenancedEvent` carries provenance: source = `HeliusAccountSubscribe`,
/// the slot, and `is_live = true` — satisfying criterion 65 by construction.
pub fn decode_onchain_confirm(
    mint_bytes: &[u8; 32],
    account_data: &[u8],
    slot: u64,
) -> Option<crate::ProvenancedEvent> {
    let curve = decode_pump_curve(account_data)?;

    // The protocol decoder already validated discriminator and bounds.
    // real_sol comes FROM THE DECODE — not from virtual_sol - 30 SOL.
    let event = AppEvent::OnchainConfirm {
        mint: Mint(*mint_bytes),
        virtual_sol_lamports: curve.virtual_sol,
        real_sol_lamports: curve.real_sol, // DECODED, NOT DERIVED (blocker 2)
    };

    Some(crate::ProvenancedEvent {
        event,
        source: crate::ProvenanceSource::HeliusAccountSubscribe,
        slot,
        is_live: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pump_quant_protocol::registry::{self, Venue};

    /// Build a structurally valid pump.fun bonding curve account blob using
    /// the REAL on-chain layout from the protocol crate:
    ///   offset 0:  8-byte discriminator (from registry)
    ///   offset 8:  virtual_token_reserves (u64 LE)
    ///   offset 16: virtual_sol_reserves (u64 LE)
    ///   offset 24: real_token_reserves (u64 LE)
    ///   offset 32: real_sol_reserves (u64 LE)
    ///   offset 40: token_total_supply (u64 LE, unused)
    ///   offset 48: complete (bool)
    fn make_real_curve_account(
        v_token: u64,
        v_sol: u64,
        r_token: u64,
        r_sol: u64,
        complete: bool,
    ) -> Vec<u8> {
        let mut data = vec![0u8; 49]; // CURVE_MIN_LEN
        let disc = registry::account_discriminator(Venue::PumpFun);
        data[0..8].copy_from_slice(&disc);
        data[8..16].copy_from_slice(&v_token.to_le_bytes());
        data[16..24].copy_from_slice(&v_sol.to_le_bytes());
        data[24..32].copy_from_slice(&r_token.to_le_bytes());
        data[32..40].copy_from_slice(&r_sol.to_le_bytes());
        // offset 40: token_total_supply — leave zero
        data[48] = if complete { 1 } else { 0 };
        data
    }

    #[test]
    fn test_decode_valid_curve_with_real_layout() {
        // Use the REAL on-chain layout (v_token at offset 8, v_sol at 16,
        // r_sol at 32) — NOT the wrong layout the first draft had.
        let blob = make_real_curve_account(
            1_000_000_000, // v_token
            30_000_000_000, // v_sol = 30 SOL
            500_000_000,   // r_token
            5_000_000_000,  // r_sol = 5 SOL (DECODED, not derived)
            false,
        );
        let result = decode_onchain_confirm(&[0xAB; 32], &blob, 12345).unwrap();

        assert_eq!(result.source, crate::ProvenanceSource::HeliusAccountSubscribe);
        assert_eq!(result.slot, 12345);
        assert!(result.is_live);

        if let AppEvent::OnchainConfirm {
            mint,
            virtual_sol_lamports,
            real_sol_lamports,
        } = result.event
        {
            assert_eq!(mint.0, [0xAB; 32]);
            assert_eq!(virtual_sol_lamports, 30_000_000_000);
            assert_eq!(real_sol_lamports, 5_000_000_000);
        } else {
            panic!("expected OnchainConfirm");
        }
    }

    #[test]
    fn test_decode_too_short() {
        let blob = vec![0u8; 10];
        assert!(decode_onchain_confirm(&[0xAB; 32], &blob, 100).is_none());
    }

    #[test]
    fn test_decode_wrong_discriminator_rejected() {
        // Foreign discriminator (PumpSwap Pool) on a curve-length buffer.
        let mut blob = make_real_curve_account(100, 200, 300, 400, false);
        let wrong_disc = registry::account_discriminator(Venue::PumpSwap);
        blob[0..8].copy_from_slice(&wrong_disc);
        assert!(decode_onchain_confirm(&[0xAB; 32], &blob, 100).is_none());
    }

    #[test]
    fn test_decode_real_sol_not_derived() {
        // BLOCKER 2 TEST: the decoded real_sol is 5 SOL, NOT
        // vsol - 30 SOL = 0 SOL. The gate compares the decoded value
        // against an independent decode — passing a derived value would
        // make the gate compare a number to itself.
        let blob = make_real_curve_account(
            1_000_000_000,
            30_000_000_000, // v_sol
            500_000_000,
            5_000_000_000,  // r_sol DECODED from account, NOT 30 SOL - 30 SOL
            false,
        );
        let result = decode_onchain_confirm(&[0xAB; 32], &blob, 100).unwrap();

        if let AppEvent::OnchainConfirm {
            virtual_sol_lamports,
            real_sol_lamports,
            ..
        } = result.event
        {
            assert_eq!(real_sol_lamports, 5_000_000_000);
            assert_ne!(
                real_sol_lamports,
                virtual_sol_lamports.saturating_sub(30_000_000_000),
                "real_sol must NOT equal vsol - 30 SOL (blocker 2)"
            );
        } else {
            panic!("expected OnchainConfirm");
        }
    }

    #[test]
    fn test_decode_completed_curve_still_decodes() {
        // A completed curve (complete=true) still has valid reserves.
        let blob = make_real_curve_account(
            100, 200, 300, 400, true,
        );
        assert!(decode_onchain_confirm(&[0xAB; 32], &blob, 100).is_some());
    }

    #[test]
    fn test_decode_zero_discriminator_rejected() {
        // All-zeros buffer must fail closed (§18.2).
        let blob = vec![0u8; 49];
        assert!(decode_onchain_confirm(&[0xAB; 32], &blob, 100).is_none());
    }
}
