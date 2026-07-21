//! Leaf: equal-timestamp tie-break comparator (§19 "Tie-breaking").

use pump_quant_clock::{stable_tie_break_sort, tie_break_cmp, EventKey};
use std::cmp::Ordering;

struct Lcg(u64);
impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed)
    }
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }
}

/// Independent oracle: order by the raw tuple `(ts, source, seq)`.
fn oracle_cmp(a: &EventKey, b: &EventKey) -> Ordering {
    (a.ts_ns, a.source.0, a.seq).cmp(&(b.ts_ns, b.source.0, b.seq))
}

#[test]
fn timestamp_is_primary_key() {
    let earlier = EventKey::new(10, 9, 9);
    let later = EventKey::new(11, 0, 0);
    // Smaller ts_ns orders first even though source/seq are larger.
    assert_eq!(tie_break_cmp(&earlier, &later), Ordering::Less);
}

#[test]
fn source_breaks_equal_timestamps() {
    let a = EventKey::new(100, 1, 999);
    let b = EventKey::new(100, 2, 0);
    // Same ts; smaller source wins despite larger seq on `a`.
    assert_eq!(tie_break_cmp(&a, &b), Ordering::Less);
}

#[test]
fn seq_breaks_equal_timestamp_and_source() {
    let a = EventKey::new(100, 5, 7);
    let b = EventKey::new(100, 5, 8);
    assert_eq!(tie_break_cmp(&a, &b), Ordering::Less);
    assert_eq!(tie_break_cmp(&b, &a), Ordering::Greater);
    assert_eq!(tie_break_cmp(&a, &a), Ordering::Equal);
}

#[test]
fn known_ordering_matches_hand_computed_expectation() {
    let mut evs = vec![
        EventKey::new(100, 2, 0), // 3rd
        EventKey::new(100, 1, 5), // 2nd
        EventKey::new(90, 9, 9),  // 1st (earliest ts)
        EventKey::new(100, 1, 4), // between-ish: same ts/source as idx1, smaller seq
        EventKey::new(101, 0, 0), // last (latest ts)
    ];
    stable_tie_break_sort(&mut evs);
    let expected = vec![
        EventKey::new(90, 9, 9),
        EventKey::new(100, 1, 4),
        EventKey::new(100, 1, 5),
        EventKey::new(100, 2, 0),
        EventKey::new(101, 0, 0),
    ];
    assert_eq!(evs, expected);
}

/// Stability: fully-equal keys retain their input order. We tag equal keys
/// with distinct payload indices carried alongside to observe the ordering.
#[test]
fn equal_keys_are_stably_ordered() {
    // Three genuinely identical keys, interleaved with distinct ones. Because
    // the keys are identical we track original positions via a parallel vec.
    let key = EventKey::new(50, 3, 3);
    let other = EventKey::new(40, 0, 0);
    let mut tagged: Vec<(EventKey, usize)> = vec![(key, 0), (other, 1), (key, 2), (key, 3)];
    tagged.sort_by(|x, y| tie_break_cmp(&x.0, &y.0));
    // `other` (ts=40) sorts first; the three identical keys keep tags 0,2,3.
    assert_eq!(tagged[0].1, 1);
    assert_eq!(tagged[1].1, 0);
    assert_eq!(tagged[2].1, 2);
    assert_eq!(tagged[3].1, 3);
}

/// Property: `tie_break_cmp` equals the independent tuple oracle for many
/// generated pairs, including deliberate timestamp collisions.
#[test]
fn matches_oracle_over_many_pairs() {
    let mut rng = Lcg::new(0xABCDEF);
    for _ in 0..5_000 {
        // Bias ts and source to small ranges to force frequent collisions.
        let a = EventKey::new(rng.next() % 4, (rng.next() % 3) as u16, rng.next() % 4);
        let b = EventKey::new(rng.next() % 4, (rng.next() % 3) as u16, rng.next() % 4);
        assert_eq!(tie_break_cmp(&a, &b), oracle_cmp(&a, &b));
    }
}

/// Property: the comparator induces a total order — sorting is idempotent and
/// agrees with the oracle sort, over many random collision-heavy inputs.
#[test]
fn stable_sort_is_total_and_idempotent() {
    let mut rng = Lcg::new(0x13579);
    for _ in 0..300 {
        let n = (rng.next() % 40) as usize;
        let mut evs: Vec<EventKey> = (0..n)
            .map(|_| EventKey::new(rng.next() % 5, (rng.next() % 4) as u16, rng.next() % 5))
            .collect();

        let mut oracle = evs.clone();
        oracle.sort_by(oracle_cmp);

        stable_tie_break_sort(&mut evs);
        assert_eq!(evs, oracle);

        // Idempotent: re-sorting a sorted slice changes nothing.
        let snapshot = evs.clone();
        stable_tie_break_sort(&mut evs);
        assert_eq!(evs, snapshot);

        // Antisymmetry / consistency spot check on adjacent pairs.
        for w in evs.windows(2) {
            assert!(tie_break_cmp(&w[0], &w[1]) != Ordering::Greater);
        }
    }
}
