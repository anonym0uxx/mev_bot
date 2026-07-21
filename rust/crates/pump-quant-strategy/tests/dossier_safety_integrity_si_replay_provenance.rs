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
