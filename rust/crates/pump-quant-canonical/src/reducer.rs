//! Stateful, memory-bounded grouping reducer (§15).
//!
//! [`Canonicalizer`] accumulates provenance-tagged observations, groups them by
//! signature, and emits [`CanonicalTransaction`]s via the pure
//! [`canonicalize_group`] reducer. It is deterministic and memory-bounded: it
//! caps both the number of tracked signatures and the number of observations
//! retained per signature, evicting deterministically when a cap is hit.

use std::collections::BTreeMap;

use crate::canonical::{canonicalize_group, CanonicalTransaction};
use crate::observation::TransactionObservation;
use crate::types::Signature;

/// A canonical transaction emitted because its signature was evicted to keep the
/// canonicalizer within its signature bound (§15 memory-bounded discipline).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evicted {
    /// The canonicalized view of the evicted signature at eviction time.
    pub canonical: CanonicalTransaction,
}

/// Per-signature accumulation. Bounded by `max_observations_per_signature`.
#[derive(Clone, Debug)]
struct Aggregation {
    observations: Vec<TransactionObservation>,
    /// Observations dropped because the per-signature cap was reached.
    dropped_observations: u64,
    /// Highest observation id seen for this signature — the recency key used for
    /// deterministic eviction.
    latest_observation_id: u64,
}

/// A deterministic, memory-bounded canonicalizer over provenance-tagged
/// observations (§15).
///
/// # Responsibility
/// Group observations by signature and produce [`CanonicalTransaction`]s that
/// preserve feed disagreement, dual timelines, fork status, and full provenance —
/// while never exceeding its configured memory bounds.
///
/// # Determinism
/// Grouping uses a [`BTreeMap`] keyed by signature; eviction picks the least
/// recently updated signature (lowest latest observation id, signature as
/// tie-break). Output never depends on ingest order beyond these explicit rules.
#[derive(Clone, Debug)]
pub struct Canonicalizer {
    groups: BTreeMap<Signature, Aggregation>,
    max_signatures: usize,
    max_observations_per_signature: usize,
    total_dropped_observations: u64,
}

impl Canonicalizer {
    /// Creates a canonicalizer bounded to at most `max_signatures` concurrently
    /// tracked signatures and `max_observations_per_signature` retained
    /// observations per signature. Both bounds are clamped to at least 1.
    pub fn new(max_signatures: usize, max_observations_per_signature: usize) -> Self {
        Canonicalizer {
            groups: BTreeMap::new(),
            max_signatures: max_signatures.max(1),
            max_observations_per_signature: max_observations_per_signature.max(1),
            total_dropped_observations: 0,
        }
    }

    /// Number of currently tracked signatures.
    pub fn len(&self) -> usize {
        self.groups.len()
    }

    /// Whether no signatures are currently tracked.
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// Total observations dropped across all signatures due to per-signature caps.
    pub fn total_dropped_observations(&self) -> u64 {
        self.total_dropped_observations
    }

    /// Ingests one observation.
    ///
    /// If accepting a **new** signature would exceed `max_signatures`, the least
    /// recently updated signature is first evicted and returned as an
    /// [`Evicted`] canonical transaction (§15 memory bound). Ingesting an
    /// observation for an already-tracked signature never evicts.
    ///
    /// When a signature already holds `max_observations_per_signature`
    /// observations, further observations for it are dropped (counted in
    /// [`Canonicalizer::total_dropped_observations`]) rather than growing memory.
    pub fn ingest(&mut self, obs: TransactionObservation) -> Option<Evicted> {
        let sig = obs.signature;

        if let Some(agg) = self.groups.get_mut(&sig) {
            Self::push_bounded(
                agg,
                obs,
                self.max_observations_per_signature,
                &mut self.total_dropped_observations,
            );
            return None;
        }

        // New signature: evict if at capacity.
        let mut evicted = None;
        if self.groups.len() >= self.max_signatures {
            if let Some(victim) = self.pick_eviction_victim() {
                if let Some(agg) = self.groups.remove(&victim) {
                    evicted = Some(Evicted {
                        canonical: canonicalize_group(victim, &agg.observations),
                    });
                }
            }
        }

        let latest = obs.observation_id;
        self.groups.insert(
            sig,
            Aggregation {
                observations: vec![obs],
                dropped_observations: 0,
                latest_observation_id: latest,
            },
        );
        evicted
    }

    /// Removes a signature and returns its canonicalized view, if tracked.
    pub fn finalize(&mut self, signature: &Signature) -> Option<CanonicalTransaction> {
        self.groups
            .remove(signature)
            .map(|agg| canonicalize_group(*signature, &agg.observations))
    }

    /// Canonicalizes a tracked signature **without** removing it.
    pub fn peek(&self, signature: &Signature) -> Option<CanonicalTransaction> {
        self.groups
            .get(signature)
            .map(|agg| canonicalize_group(*signature, &agg.observations))
    }

    /// Canonicalizes and removes every tracked signature, returning results in
    /// deterministic signature order.
    pub fn drain_all(&mut self) -> Vec<CanonicalTransaction> {
        let out: Vec<CanonicalTransaction> = self
            .groups
            .iter()
            .map(|(sig, agg)| canonicalize_group(*sig, &agg.observations))
            .collect();
        self.groups.clear();
        out
    }

    /// Appends an observation to a group, respecting the per-signature cap.
    fn push_bounded(
        agg: &mut Aggregation,
        obs: TransactionObservation,
        cap: usize,
        total_dropped: &mut u64,
    ) {
        if obs.observation_id > agg.latest_observation_id {
            agg.latest_observation_id = obs.observation_id;
        }
        if agg.observations.len() >= cap {
            agg.dropped_observations = agg.dropped_observations.saturating_add(1);
            *total_dropped = total_dropped.saturating_add(1);
            return;
        }
        agg.observations.push(obs);
    }

    /// Chooses the eviction victim: the signature with the lowest
    /// `latest_observation_id`, breaking ties by the smaller signature. Fully
    /// deterministic.
    fn pick_eviction_victim(&self) -> Option<Signature> {
        self.groups
            .iter()
            .min_by(|(sa, aa), (sb, ab)| {
                aa.latest_observation_id
                    .cmp(&ab.latest_observation_id)
                    .then_with(|| sa.cmp(sb))
            })
            .map(|(sig, _)| *sig)
    }
}
