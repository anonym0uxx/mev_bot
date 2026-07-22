// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'manifest' component (leaf 'mf_totals').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(unused_imports, dead_code, clippy::all)]
use pump_quant_journal::manifest::*;

fn entry(id: u64, first: u64, last: u64, frames: u32, bytes: u64, hash: u64) -> SealedSegment {
    SealedSegment {
        segment_id: id,
        schema_version: 1,
        connection_epoch: 0,
        first_sequence: first,
        last_sequence: last,
        frame_count: frames,
        byte_len: bytes,
        content_hash: hash,
    }
}

#[test]
fn mf_totals_property() {
    // Empty: identities are the additive units, span undefined.
    let empty = Manifest::new();
    assert_eq!(empty.total_frames(), Some(0));
    assert_eq!(empty.total_bytes(), Some(0));
    assert_eq!(empty.sequence_span(), None);

    // Sums equal the independent hand-computed totals.
    let mut m = Manifest::new();
    m.add(entry(0, 0, 9, 10, 100, 1)).unwrap();
    m.add(entry(1, 10, 19, 10, 200, 2)).unwrap();
    m.add(entry(2, 20, 24, 5, 50, 3)).unwrap();
    assert_eq!(m.total_frames(), Some(25));
    assert_eq!(m.total_bytes(), Some(350));
    assert_eq!(m.sequence_span(), Some((0, 24)));

    // total_bytes overflow -> None (checked add). First entry carries u64::MAX bytes,
    // second adds more, contiguity preserved.
    let mut ov = Manifest::new();
    ov.add(entry(0, 0, 9, 10, u64::MAX, 1)).unwrap();
    ov.add(entry(1, 10, 19, 10, 5, 2)).unwrap();
    assert_eq!(ov.total_bytes(), None);
    // Frame totals are unaffected and still exact.
    assert_eq!(ov.total_frames(), Some(20));
    assert_eq!(ov.sequence_span(), Some((0, 19)));
}
