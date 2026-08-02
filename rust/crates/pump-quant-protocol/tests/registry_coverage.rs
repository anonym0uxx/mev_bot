//! Coverage report: populate LayoutRegistry from the fixtures JSON and report
//! the gap between verified layouts and `required_layouts()`.
//!
//! This test reads `docs/fixtures/layouts.json` (committed by the operator's
//! extract_layout_fixtures.py), calls `LayoutRegistry::record_verified` for
//! every fixture that has an `example_signature` and `example_slot`, and then
//! runs `missing()` against `required_layouts()` for both venues.
//!
//! The coverage number printed is the honest status of the builder: how many
//! of the permutation matrix entries have been proven against a real mainnet
//! transaction. It starts at or near zero and only grows when a fixture is
//! recorded.

use pump_quant_protocol::layout::{
    required_layouts, LayoutKey, LayoutRegistry, Side, Variant, Venue, VerifiedLayout,
};

/// Minimal base58 decoder (alphabet order). Good enough for signatures — the
/// registry only stores the 64 bytes, and we never re-encode.
fn b58_decode(sig: &str) -> [u8; 64] {
    const ALPHABET: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut result = [0u8; 64];
    let bytes = sig.as_bytes();
    for &c in bytes {
        let mut val: u32 = 58;
        for (i, &a) in ALPHABET.iter().enumerate() {
            if c == a {
                val = i as u32;
                break;
            }
        }
        if val == 58 {
            continue; // skip non-alphabet chars
        }
        // Multiply accumulator by 58 and add
        let mut carry = val;
        for byte in result.iter_mut().rev() {
            let acc = (*byte as u32) * 58 + carry;
            *byte = (acc & 0xFF) as u8;
            carry = acc >> 8;
        }
        // carry should be zero for a valid 64-byte signature
    }
    result
}

fn venue_from_str(s: &str) -> Venue {
    match s.to_lowercase().as_str() {
        "pumpfun" => Venue::PumpFun,
        "pumpswap" => Venue::PumpSwap,
        _ => panic!("unknown venue: {s}"),
    }
}

fn side_from_str(s: &str) -> Side {
    match s.to_lowercase().as_str() {
        "buy" => Side::Buy,
        "sell" => Side::Sell,
        _ => panic!("unknown side: {s}"),
    }
}

#[test]
fn registry_coverage_report() {
    // Load the fixtures JSON
    let json_path = "../../../docs/fixtures/layouts.json";
    let json_str = std::fs::read_to_string(json_path)
        .expect("fixtures JSON not found — run extract_layout_fixtures.py first");
    let parsed: serde_json::Value =
        serde_json::from_str(&json_str).expect("fixtures JSON is not valid JSON");

    let layouts = parsed
        .get("layouts")
        .and_then(|v| v.as_array())
        .expect("fixtures JSON has no 'layouts' array");

    let mut registry = LayoutRegistry::new();
    let mut recorded = 0u32;
    let mut skipped = 0u32;

    for entry in layouts {
        let venue_s = entry
            .get("venue")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let side_s = entry
            .get("side")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let count = entry
            .get("account_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let sig = entry.get("example_signature").and_then(|v| v.as_str());
        let slot = entry.get("example_slot").and_then(|v| v.as_u64());

        let variant_obj = entry.get("variant").unwrap_or(&serde_json::Value::Null);
        let cashback = variant_obj
            .get("cashback")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let token_2022 = variant_obj
            .get("token_2022")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let non_sol_quote = variant_obj
            .get("non_sol_quote")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let reversed_pool = variant_obj
            .get("reversed_pool")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Skip entries without signature/slot (can't record provenance)
        let (sig_str, slot_val) = match (sig, slot) {
            (Some(s), Some(sl)) => (s.to_string(), sl),
            _ => {
                skipped += 1;
                continue;
            }
        };

        let key = LayoutKey {
            venue: venue_from_str(venue_s),
            side: side_from_str(side_s),
            variant: Variant {
                cashback,
                token_2022,
                non_sol_quote,
                reversed_pool,
            },
        };

        let verified = VerifiedLayout {
            key,
            account_count: count,
            verifying_slot: slot_val,
            verifying_signature: b58_decode(&sig_str),
        };

        registry
            .record_verified(verified)
            .expect("record_verified should succeed for a non-zero signature");
        recorded += 1;
    }

    eprintln!();
    eprintln!("=== LayoutRegistry Coverage Report ===");
    eprintln!("  fixtures loaded:     {}", layouts.len());
    eprintln!("  recorded:            {recorded}");
    eprintln!("  skipped (no sig/slot): {skipped}");
    eprintln!("  verified entries:    {}", registry.verified().len());
    eprintln!();

    // Run missing() for both venues
    for venue in [Venue::PumpFun, Venue::PumpSwap] {
        let required = required_layouts(venue);
        let missing = registry.missing(&required);
        let proven = required.len() - missing.len();
        let venue_s = match venue {
            Venue::PumpFun => "pump.fun",
            Venue::PumpSwap => "PumpSwap",
        };
        eprintln!(
            "  {venue_s}: {proven}/{total} layouts proven ({missing_count} missing)",
            venue_s = venue_s,
            proven = proven,
            total = required.len(),
            missing_count = missing.len()
        );
    }

    eprintln!();
    eprintln!("  Honest status: builder coverage starts at {} of 48 required layouts.",
        registry.verified().len());
}
