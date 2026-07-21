//! Leaf tests: cross-channel copy-echo detection (§29.8 D7/D8).

use pump_quant_social::copy_echo::{
    copy_echo_density_bps, detect_copy_echo, jaccard_bps, ChannelCall,
};

#[test]
fn jaccard_known_and_unsorted() {
    // {1,2,3} ∩ {2,3,4} = {2,3} (2), ∪ = {1,2,3,4} (4) → 5000.
    assert_eq!(jaccard_bps(&[1, 2, 3], &[2, 3, 4]), 5_000);
    assert_eq!(jaccard_bps(&[1, 2, 3], &[1, 2, 3]), 10_000); // identical
    assert_eq!(jaccard_bps(&[1, 2], &[3, 4]), 0); // disjoint
    assert_eq!(jaccard_bps(&[], &[]), 0); // empty
                                          // Unsorted / duplicated input is normalised internally.
    assert_eq!(jaccard_bps(&[3, 1, 2, 1], &[4, 3, 2]), 5_000);
}

fn call(source: u64, channel: u64, ts: u64, shingles: &[u32]) -> ChannelCall {
    ChannelCall {
        source_id: source,
        channel_id: channel,
        token_id: 42,
        timestamp_ns: ts,
        shingles: shingles.to_vec(),
    }
}

#[test]
fn detect_orients_earlier_to_later() {
    let calls = [
        call(1, 10, 0, &[7, 8, 9]),
        call(2, 20, 100, &[7, 8, 9]), // exact echo 100ns later
    ];
    let edges = detect_copy_echo(&calls, 5_000, 1_000);
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].originator, 1);
    assert_eq!(edges[0].echo, 2);
    assert_eq!(edges[0].similarity_bps, 10_000);
    assert_eq!(edges[0].lag_ns, 100);
}

#[test]
fn same_source_and_out_of_window_produce_no_edge() {
    // Same source reposting itself is not a cross-channel echo.
    let same = [call(1, 10, 0, &[1, 2, 3]), call(1, 11, 50, &[1, 2, 3])];
    assert!(detect_copy_echo(&same, 5_000, 1_000).is_empty());

    // Outside the time window.
    let late = [call(1, 10, 0, &[1, 2, 3]), call(2, 20, 5_000, &[1, 2, 3])];
    assert!(detect_copy_echo(&late, 5_000, 1_000).is_empty());

    // Below similarity threshold.
    let dissimilar = [call(1, 10, 0, &[1, 2, 3]), call(2, 20, 10, &[4, 5, 6])];
    assert!(detect_copy_echo(&dissimilar, 5_000, 1_000).is_empty());
}

#[test]
fn density_counts_echo_share() {
    // A originator @0; B similar echo @100; C dissimilar @100 → 1 of 3 is an echo.
    let calls = [
        call(1, 10, 0, &[1, 2, 3]),
        call(2, 20, 100, &[1, 2, 3]),
        call(3, 30, 100, &[9, 8, 7]),
    ];
    assert_eq!(copy_echo_density_bps(&calls, 5_000, 1_000), 3_333);
    assert_eq!(copy_echo_density_bps(&[], 5_000, 1_000), 0);
}

#[test]
fn multiple_echoes_all_detected() {
    // One originator, two echoes → two edges, both from source 1.
    let calls = [
        call(1, 10, 0, &[1, 2, 3, 4]),
        call(2, 20, 100, &[1, 2, 3, 4]),
        call(3, 30, 200, &[1, 2, 3, 4]),
    ];
    let edges = detect_copy_echo(&calls, 6_000, 1_000);
    // A->B, A->C, and B->C all qualify (all identical, all within window).
    assert_eq!(edges.len(), 3);
    assert!(edges.iter().all(|e| e.similarity_bps == 10_000));
    // Density: B and C are both echoes → 2/3.
    assert_eq!(copy_echo_density_bps(&calls, 6_000, 1_000), 6_666);
}
