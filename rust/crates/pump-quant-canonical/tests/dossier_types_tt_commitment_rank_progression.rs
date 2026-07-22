// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'types' component (leaf 'tt_commitment_rank_progression').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(
    unused_imports,
    dead_code,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_canonical::types::*;

#[test]
fn commitment_rank_is_monotone_chain_lifecycle() {
    // Concrete lifecycle ranks used to keep the highest observed commitment.
    assert_eq!(Commitment::Seen.rank(), 0);
    assert_eq!(Commitment::Processed.rank(), 1);
    assert_eq!(Commitment::Confirmed.rank(), 2);
    assert_eq!(Commitment::Finalized.rank(), 3);

    // Strictly increasing along the natural chain progression.
    assert!(Commitment::Seen.rank() < Commitment::Processed.rank());
    assert!(Commitment::Processed.rank() < Commitment::Confirmed.rank());
    assert!(Commitment::Confirmed.rank() < Commitment::Finalized.rank());

    // Finalized is the unique maximum; Seen the unique minimum.
    let all = [
        Commitment::Seen,
        Commitment::Processed,
        Commitment::Confirmed,
        Commitment::Finalized,
    ];
    let ranks: Vec<u8> = all.iter().map(|c| c.rank()).collect();
    assert_eq!(ranks, vec![0, 1, 2, 3]);
    assert_eq!(*ranks.iter().max().unwrap(), Commitment::Finalized.rank());

    // Derived Ord agrees with rank ordering (monotone, no inversion).
    assert!(Commitment::Seen < Commitment::Finalized);
    assert!(Commitment::Confirmed > Commitment::Processed);

    // Injective across the four levels.
    let mut r = ranks.clone();
    r.sort_unstable();
    r.dedup();
    assert_eq!(r.len(), 4);
}
