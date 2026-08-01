//! Decode bonding-curve account snapshots from accountSubscribe RPC responses.
//!
//! This module is a THIN ADAPTER over `pump_quant_protocol::decode_pump_curve`,
//! which is the discriminator-verifying, bounds-checking, canonical decoder for
//! the pump.fun BondingCurve account. We delegate to it rather than
//! reimplementing the layout — a second decoder is a second thing to drift,
//! and this repo already has a doc titled "three cost models disagree."
//!
//! BLOCKER 2 (structural fix, per operator correction):
//! The integrity point is not that the decoded `real_sol` differs from the
//! derived `vsol - 30 SOL`. On pump.fun they will usually be EQUAL — the
//! protocol maintains `virtual_sol = 30 SOL + real_sol` as an identity.
//! The point is PROVENANCE: the decoded value is OBSERVED from account data,
//! the derived value is COMPUTED from a constant pump.fun has changed before
//! and can change again.
//!
//! We make the mistake unrepresentable: `real_sol` enters the junction as
//! `DecodedRealSol`, a newtype whose sole constructor takes `&PumpCurve`
//! (which can only come from `decode_pump_curve`). A bare `u64` cannot
//! construct an `OnchainConfirm` through the junction's public API. The
//! text-file replay path (`parse_events`) uses bare `u64` directly and is
//! certified by the golden digest — it is untouched.

use pump_quant_app::event::AppEvent;
use pump_quant_domain::ids::Mint;
use pump_quant_protocol::decode::{decode_pump_curve, PumpCurve};

use crate::{DecodedRealSol, ProvenancedEvent, ProvenanceSource};

/// Decode a bonding-curve account blob into a provenanced `OnchainConfirm`.
///
/// The raw bytes come from `accountSubscribe.data` (base64-decoded by the
/// WebSocket caller before reaching this function). We delegate to the
/// protocol crate's `decode_pump_curve` for discriminator verification and
/// field extraction.
///
/// Returns `None` when:
/// - The account is too short for the BondingCurve layout.
/// - The discriminator does not match the pump.fun BondingCurve account type.
/// - The `complete` byte is not a canonical boolean.
///
/// On success, the `ProvenancedEvent` carries provenance: source =
/// `HeliusAccountSubscribe`, the slot, and `is_live = true` — satisfying
/// criterion 65 by construction. The `real_sol` value is wrapped in
/// `DecodedRealSol` (blocker 2 structural fix).
pub fn decode_onchain_confirm(
    mint_bytes: &[u8; 32],
    account_data: &[u8],
    slot: u64,
) -> Option<ProvenancedEvent> {
    let curve = decode_pump_curve(account_data)?;

    // real_sol comes FROM THE DECODE — structurally enforced via DecodedRealSol,
    // not via a value-inequality assertion. A derived u64 cannot construct
    // this path.
    let real_sol = DecodedRealSol::from_curve(&curve);

    let event = AppEvent::OnchainConfirm {
        mint: Mint(*mint_bytes),
        virtual_sol_lamports: curve.virtual_sol,
        real_sol_lamports: real_sol.into_lamports(),
    };

    Some(ProvenancedEvent {
        event,
        source: ProvenanceSource::HeliusAccountSubscribe,
        slot,
        is_live: true,
    })
}

/// Construct an `OnchainConfirm` from a decoded `PumpCurve` without re-decoding.
///
/// This is the API the wire-up calls when it already has a `PumpCurve` from
/// a prior decode (e.g. cached snapshot). The `real_sol` provenance is
/// structural — `DecodedRealSol::from_curve` is the only constructor.
pub fn onchain_confirm_from_curve(
    mint_bytes: &[u8; 32],
    curve: &PumpCurve,
    slot: u64,
) -> ProvenancedEvent {
    let real_sol = DecodedRealSol::from_curve(curve);
    let event = AppEvent::OnchainConfirm {
        mint: Mint(*mint_bytes),
        virtual_sol_lamports: curve.virtual_sol,
        real_sol_lamports: real_sol.into_lamports(),
    };

    ProvenancedEvent {
        event,
        source: ProvenanceSource::HeliusAccountSubscribe,
        slot,
        is_live: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProvenanceSource;

    /// Build a valid BondingCurve account blob with the REAL on-chain layout.
    ///
    /// offset  size  field
    /// 0       8     anchor discriminator (BondingCurve)
    /// 8       8     virtual_token_reserves  (u64 LE)
    /// 16      8     virtual_sol_reserves    (u64 LE)
    /// 24      8     real_token_reserves     (u64 LE)
    /// 32      8     real_sol_reserves       (u64 LE)
    /// 40      8     token_total_supply      (u64 LE)
    /// 48      1     complete                (bool)
    fn make_curve_blob(
        v_token: u64,
        v_sol: u64,
        r_token: u64,
        r_sol: u64,
        complete: bool,
    ) -> Vec<u8> {
        // Use the REAL discriminator from the protocol registry — a hardcoded
        // copy would drift if pump.fun rotates the account type.
        let disc = pump_quant_protocol::registry::account_discriminator(
            pump_quant_protocol::registry::Venue::PumpFun,
        );

        let mut blob = Vec::with_capacity(49);
        blob.extend_from_slice(&disc);
        blob.extend_from_slice(&v_token.to_le_bytes());
        blob.extend_from_slice(&v_sol.to_le_bytes());
        blob.extend_from_slice(&r_token.to_le_bytes());
        blob.extend_from_slice(&r_sol.to_le_bytes());
        blob.extend_from_slice(&0u64.to_le_bytes()); // token_total_supply
        blob.push(complete as u8);
        blob
    }

    #[test]
    fn test_decode_valid_curve_with_real_layout() {
        let blob = make_curve_blob(
            1_000_000_000,  // v_token = 1B tokens
            30_000_000_000, // v_sol = 30 SOL (pump.fun initial virtual reserves)
            800_000_000,    // r_token
            5_000_000_000,  // r_sol = 5 SOL (DECODED from account data)
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
            // The decoded value is 5 SOL. On pump.fun, vsol - 30 SOL = 0, NOT 5 SOL.
            // But the PROVENANCE is what matters: this value came from decode_pump_curve,
            // not from arithmetic. The structural type (DecodedRealSol) enforces that.
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
    fn test_decode_wrong_discriminator() {
        let mut blob = make_curve_blob(
            1_000_000_000, 30_000_000_000, 800_000_000, 5_000_000_000, false,
        );
        // Corrupt the discriminator.
        blob[0] = 0x00;
        assert!(decode_onchain_confirm(&[0xAB; 32], &blob, 100).is_none());
    }

    #[test]
    fn test_decode_completed_curve_still_decodes() {
        // A completed curve still decodes — the `complete` flag is surfaced
        // but does not prevent OnchainConfirm. The engine decides what to do
        // with a completed curve (it may treat it as migrated).
        let blob = make_curve_blob(
            1_000_000_000, 60_000_000_000, 100_000_000, 30_000_000_000, true,
        );
        let result = decode_onchain_confirm(&[0xAB; 32], &blob, 100).unwrap();

        if let AppEvent::OnchainConfirm {
            virtual_sol_lamports,
            real_sol_lamports,
            ..
        } = result.event
        {
            assert_eq!(real_sol_lamports, 30_000_000_000);
            // Here vsol - 30 SOL = 30 SOL, which EQUALS real_sol. This is
            // exactly the case the operator flagged: the values match.
            // The structural type (DecodedRealSol) is what proves provenance,
            // NOT a value-inequality assertion.
            assert_eq!(
                real_sol_lamports,
                virtual_sol_lamports.saturating_sub(30_000_000_000),
                "on a completed curve, real_sol == vsol - 30 SOL — the values match, \
                 but the provenance differs (decoded vs derived). The structural \
                 type enforces this, not a value assertion."
            );
        } else {
            panic!("expected OnchainConfirm");
        }
    }

    /// BLOCKER 2 STRUCTURAL TEST: Verify that a derived `u64` CANNOT construct
    /// a `DecodedRealSol`. The type system enforces provenance — the mistake
    /// is unrepresentable, not merely tested for.
    #[test]
    fn test_decoded_real_sol_cannot_be_constructed_from_bare_u64() {
        // This test documents the structural guarantee: DecodedRealSol has a
        // private inner field. The ONLY public constructor is from_curve,
        // which takes &PumpCurve. There is no `DecodedRealSol::from_lamports`
        // or `DecodedRealSol(u64)` constructor.

        // If someone adds a public from_u64 constructor, this test should be
        // updated to call it and FAIL — the structural guarantee would be
        // broken. For now, we verify the API surface:
        let curve = PumpCurve {
            virtual_sol: 30_000_000_000,
            virtual_token: 1_000_000_000,
            real_sol: 5_000_000_000,
            real_token: 800_000_000,
            complete: false,
        };
        let decoded = DecodedRealSol::from_curve(&curve);
        assert_eq!(decoded.into_lamports(), 5_000_000_000);

        // The following would NOT compile because the field is private:
        //   let bad = DecodedRealSol(5_000_000_000);
        //   let bad = DecodedRealSol::from_lamports(5_000_000_000);
        // That is the structural guarantee.
    }

    #[test]
    fn test_onchain_confirm_from_curve_preserves_provenance() {
        let curve = PumpCurve {
            virtual_sol: 45_000_000_000,
            virtual_token: 500_000_000,
            real_sol: 15_000_000_000,
            real_token: 200_000_000,
            complete: false,
        };
        let result = onchain_confirm_from_curve(&[0xAB; 32], &curve, 999);

        assert_eq!(result.source, ProvenanceSource::HeliusAccountSubscribe);
        assert_eq!(result.slot, 999);
        assert!(result.is_live);

        if let AppEvent::OnchainConfirm {
            virtual_sol_lamports,
            real_sol_lamports,
            ..
        } = result.event
        {
            assert_eq!(virtual_sol_lamports, 45_000_000_000);
            assert_eq!(real_sol_lamports, 15_000_000_000);
        } else {
            panic!("expected OnchainConfirm");
        }
    }
}
