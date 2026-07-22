// GENERATED FROM DOSSIER — DO NOT EDIT.
// This property test is the correctness authority for the 'safety_integrity' component (leaf 'si_replay_provenance').
// It was materialized independently of the builder. Editing it is a build-integrity
// violation caught by `materialize_tests.py --verify` and denied by .claude/settings.json.
// To change a component's contract, change its dossier and re-materialize — never edit here.
// The glob import below brings the leaf's public items into scope; integration tests in
// tests/ are a separate crate, so the implementation must be `pub` and reachable here.
#![allow(
    unused_imports,
    clippy::manual_range_contains,
    clippy::bool_comparison,
    clippy::nonminimal_bool
)]
use pump_quant_strategy::safety_integrity::*;

#[test]
fn mode_maps_to_provenance_and_is_immutable() {
    let obs = Observation { seq: 1, payload: 7 };
    let live = tag_provenance(&obs, SourceMode::Live);
    assert_eq!(live.provenance(), Provenance::OriginalLive);
    let replay = tag_provenance(&obs, SourceMode::Replay);
    assert_eq!(replay.provenance(), Provenance::ProviderReplay);
    // immutable: still original after re-reading
    assert_eq!(live.provenance(), Provenance::OriginalLive);
    assert_ne!(live.provenance(), replay.provenance());
}
