#![allow(unused_imports)]
use pump_quant_protocol::registry::*;

#[test]
fn versions_are_reported() {
    let (v_pump, _) = registry_version(Venue::PumpFun);
    let (v_swap, _) = registry_version(Venue::PumpSwap);
    assert_eq!(v_pump, 1);
    assert_eq!(v_swap, 1);
}

#[test]
fn is_deterministic() {
    // Identical input => identical output, twice.
    assert_eq!(
        registry_version(Venue::PumpFun),
        registry_version(Venue::PumpFun)
    );
    assert_eq!(
        registry_version(Venue::PumpSwap),
        registry_version(Venue::PumpSwap)
    );
}

#[test]
fn venues_produce_distinct_hashes() {
    let (_, h_pump) = registry_version(Venue::PumpFun);
    let (_, h_swap) = registry_version(Venue::PumpSwap);
    assert_ne!(h_pump, h_swap);
}

#[test]
fn hash_is_not_all_zero() {
    let (_, h) = registry_version(Venue::PumpFun);
    assert!(h.iter().any(|&b| b != 0));
}

#[test]
fn matches_independent_fnv_reference() {
    // Recompute the placeholder hash independently for PumpFun's program id.
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let seed = Venue::PumpFun.program_id().as_bytes();

    let mut base = FNV_OFFSET;
    for &b in seed {
        base ^= b as u64;
        base = base.wrapping_mul(FNV_PRIME);
    }
    let mut want = [0u8; 32];
    for (i, slot) in want.iter_mut().enumerate() {
        let mut h = base ^ (i as u64).wrapping_mul(FNV_PRIME);
        h = h.wrapping_mul(FNV_PRIME);
        *slot = (h >> ((i % 8) * 8)) as u8;
    }

    let (_, got) = registry_version(Venue::PumpFun);
    assert_eq!(got, want);
}
