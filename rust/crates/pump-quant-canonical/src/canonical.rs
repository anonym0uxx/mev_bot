//! The merged canonical output and the pure merge reducer (§15).
//!
//! [`canonicalize_group`] is a **pure, deterministic** function: given all
//! observations of one transaction, it produces a [`CanonicalTransaction`]. It
//! performs no I/O, uses no wall-clock or RNG, and contains no floating point
//! (§22). Feed disagreement is preserved; timing is never equated across source
//! classes or delivery modes (§15, §16, §18.6).

use std::collections::BTreeMap;

use crate::observation::{SourcedTime, TransactionObservation};
use crate::types::{
    Commitment, DeliveryMode, FieldName, ForkStatus, Provider, Signature, SourceClass,
};

/// A canonical field value resolved from possibly-many source claims, retaining
/// which authority class decided it and whether all asserting sources agreed
/// (§15, §18.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedField<T> {
    /// The canonical value, or `None` if no source asserted this field.
    pub value: Option<T>,
    /// The source authority class whose claim was adopted as canonical.
    pub authority: Option<SourceClass>,
    /// `true` when every asserting source agreed on a single value (or none did).
    /// `false` signals a preserved disagreement (see
    /// [`CanonicalTransaction::disagreements`]).
    pub agreed: bool,
    /// Number of source claims that asserted this field.
    pub contributing: u32,
}

impl<T> ResolvedField<T> {
    /// The empty resolution: no source asserted the field.
    const fn empty() -> Self {
        ResolvedField {
            value: None,
            authority: None,
            agreed: true,
            contributing: 0,
        }
    }
}

/// One source's attribution for a specific claimed field value (§15 provenance).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClaimSource {
    /// Authority class of the asserting source.
    pub source_class: SourceClass,
    /// Provider of the asserting source.
    pub provider: Provider,
    /// Observation id of the asserting source.
    pub observation_id: u64,
}

/// One distinct claimed value for a field, plus every source that asserted it
/// (§15 — disagreement is preserved with full attribution).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldClaim {
    /// Injective integer encoding of the claimed value (no float; §22). For
    /// booleans this is 0/1, for enums the variant rank, otherwise the value.
    pub value_repr: i128,
    /// Sources asserting this value, sorted by `observation_id`.
    pub sources: Vec<ClaimSource>,
}

/// A preserved cross-source disagreement on one canonical field (§15).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldDisagreement {
    /// Which field disagreed.
    pub field: FieldName,
    /// The distinct claimed values with their sources, sorted by `value_repr`.
    pub claims: Vec<FieldClaim>,
}

/// A single contributing observation recorded for provenance (§15). One entry
/// per merged observation; the vector is bounded by the per-signature cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProvenanceEntry {
    /// Observation id.
    pub observation_id: u64,
    /// Source authority class.
    pub source_class: SourceClass,
    /// Provider identity.
    pub provider: Provider,
    /// Delivery mode.
    pub delivery_mode: DeliveryMode,
    /// Local receive time (nanoseconds).
    pub receive_time_ns: u64,
    /// Source sequence, where provided.
    pub source_sequence: Option<u64>,
    /// Connection epoch.
    pub connection_epoch: u64,
    /// Raw payload content hash.
    pub payload_hash: [u8; 32],
}

/// Key into the observation timeline: a (source class, delivery mode) pair. Two
/// timings are only ever comparable within the same key; the canonicalizer never
/// equates timing across keys (§15, §16, §18.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimelineKey {
    /// Source authority class.
    pub source_class: SourceClass,
    /// Delivery mode.
    pub delivery_mode: DeliveryMode,
}

/// Observation truth (§15): the earliest local receive time this server saw for
/// the transaction, kept **separately per (source class, delivery mode)** so that
/// live, replay, and backfill timings — and different authority classes — are
/// never pooled or equated.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObservationTimeline {
    first_seen: BTreeMap<TimelineKey, SourcedTime>,
    reconstructed_earliest: Option<SourcedTime>,
}

impl ObservationTimeline {
    /// First-seen time for an exact (source class, delivery mode) key.
    pub fn first_seen(
        &self,
        source_class: SourceClass,
        delivery_mode: DeliveryMode,
    ) -> Option<SourcedTime> {
        self.first_seen
            .get(&TimelineKey {
                source_class,
                delivery_mode,
            })
            .copied()
    }

    /// First-seen **live** time for a source class (`DeliveryMode::Live` only).
    /// Replay / repair / backfill receipts are excluded (§18.6).
    pub fn first_seen_live(&self, source_class: SourceClass) -> Option<SourcedTime> {
        self.first_seen(source_class, DeliveryMode::Live)
    }

    /// `first_seen_earliest_ns` (§17): earliest live receipt from the
    /// earliest-signal (shred-class) source class.
    pub fn first_seen_earliest_live(&self) -> Option<SourcedTime> {
        self.first_seen_live(SourceClass::EarliestSignal)
    }

    /// `first_seen_helius_ns` (§17): earliest live receipt from the structured
    /// observation (Helius LaserStream) source class.
    pub fn first_seen_structured_live(&self) -> Option<SourcedTime> {
        self.first_seen_live(SourceClass::StructuredObservation)
    }

    /// `reconstructed_earliest_ns` (§17): earliest shred-reconstruction-complete
    /// time from the earliest-signal source class, where reported.
    pub fn reconstructed_earliest(&self) -> Option<SourcedTime> {
        self.reconstructed_earliest
    }

    /// Full per-key map for inspection (deterministic ordering).
    pub fn all(&self) -> &BTreeMap<TimelineKey, SourcedTime> {
        &self.first_seen
    }
}

/// Canonical chain-truth timeline (§15): the local times at which each canonical
/// commitment level was first observed **live**. Non-live deliveries never
/// populate these (their timing is not live truth; §16, §18.6). Combined with
/// [`CanonicalFields::slot`], [`CanonicalFields::tx_index`] and
/// [`CanonicalFields::commitment`], this expresses canonical chain truth.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CanonicalTimeline {
    /// First live time the transaction was seen at `Seen` commitment.
    pub seen_ns: Option<SourcedTime>,
    /// First live time observed at `Processed` commitment.
    pub processed_ns: Option<SourcedTime>,
    /// First live time observed at `Confirmed` commitment.
    pub confirmed_ns: Option<SourcedTime>,
    /// First live time observed at `Finalized` commitment.
    pub finalized_ns: Option<SourcedTime>,
}

/// The resolved canonical fields of a transaction (§17). Each is an authority-
/// resolved value that also records whether sources agreed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanonicalFields {
    /// Slot of inclusion.
    pub slot: ResolvedField<u64>,
    /// Transaction index within its block.
    pub tx_index: ResolvedField<u32>,
    /// Success / failure.
    pub success: ResolvedField<bool>,
    /// Base fee (lamports).
    pub base_fee_lamports: ResolvedField<u64>,
    /// Priority fee (lamports).
    pub priority_fee_lamports: ResolvedField<u64>,
    /// Jito tip (lamports).
    pub jito_tip_lamports: ResolvedField<u64>,
    /// Compute units consumed.
    pub compute_units: ResolvedField<u64>,
    /// Commitment / confirmation status (highest observed among top authority).
    pub commitment: ResolvedField<Commitment>,
}

/// The merged, provenance-preserving canonical view of one transaction (§15,
/// §17). This is the sole output of the canonicalizer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalTransaction {
    /// The transaction identity.
    pub signature: Signature,
    /// Observation truth timeline (per source class and delivery mode).
    pub observation_timeline: ObservationTimeline,
    /// Canonical chain-truth commitment timeline.
    pub canonical_timeline: CanonicalTimeline,
    /// Authority-resolved canonical fields.
    pub fields: CanonicalFields,
    /// Fork inclusion status (authority-resolved; dropped-fork preserved, §15).
    pub fork: ResolvedField<ForkStatus>,
    /// Every contributing observation, sorted by `observation_id`.
    pub provenance: Vec<ProvenanceEntry>,
    /// Preserved cross-source disagreements, sorted by [`FieldName`].
    pub disagreements: Vec<FieldDisagreement>,
    /// Number of observations merged into this transaction.
    pub observation_count: u32,
}

impl CanonicalTransaction {
    /// Fork status value, defaulting to [`ForkStatus::Unknown`] if unasserted.
    pub fn fork_status(&self) -> ForkStatus {
        self.fork.value.unwrap_or(ForkStatus::Unknown)
    }

    /// Whether any canonical field carries a preserved disagreement.
    pub fn has_disagreement(&self) -> bool {
        !self.disagreements.is_empty()
    }

    /// The preserved disagreement for a given field, if any.
    pub fn disagreement(&self, field: FieldName) -> Option<&FieldDisagreement> {
        self.disagreements.iter().find(|d| d.field == field)
    }
}

// ---------------------------------------------------------------------------
// Internal resolution machinery
// ---------------------------------------------------------------------------

/// One source's claim of a single field's value, for resolution.
struct FieldObs<T> {
    value: T,
    source_class: SourceClass,
    provider: Provider,
    observation_id: u64,
}

/// Returns `true` if `cand` should replace `best` as the canonical claim.
///
/// Ordering: higher [`SourceClass::rank`] always wins. On equal authority, when
/// `tie_prefer_max` is set the larger encoded value wins (used for commitment, so
/// the highest observed lifecycle level survives among equally-authoritative
/// sources); otherwise the lower `observation_id` wins. Every comparison is
/// integer-only and total, guaranteeing determinism (§22).
fn is_better<T, F>(cand: &FieldObs<T>, best: &FieldObs<T>, tie_prefer_max: bool, encode: &F) -> bool
where
    F: Fn(&T) -> i128,
{
    let cr = cand.source_class.rank();
    let br = best.source_class.rank();
    if cr != br {
        return cr > br;
    }
    if tie_prefer_max {
        let cv = encode(&cand.value);
        let bv = encode(&best.value);
        if cv != bv {
            return cv > bv;
        }
    }
    cand.observation_id < best.observation_id
}

/// Resolves one field across all source claims, producing the canonical value and
/// any preserved disagreement (§15).
fn resolve_field<T, F>(
    field: FieldName,
    obs: &[FieldObs<T>],
    encode: F,
    tie_prefer_max: bool,
) -> (ResolvedField<T>, Option<FieldDisagreement>)
where
    T: Copy,
    F: Fn(&T) -> i128,
{
    if obs.is_empty() {
        return (ResolvedField::empty(), None);
    }

    // Winner selection: deterministic, integer-ordered.
    let mut best = &obs[0];
    for c in &obs[1..] {
        if is_better(c, best, tie_prefer_max, &encode) {
            best = c;
        }
    }

    // Group by distinct encoded value, preserving every asserting source.
    let mut groups: BTreeMap<i128, Vec<ClaimSource>> = BTreeMap::new();
    for c in obs {
        groups
            .entry(encode(&c.value))
            .or_default()
            .push(ClaimSource {
                source_class: c.source_class,
                provider: c.provider,
                observation_id: c.observation_id,
            });
    }
    for sources in groups.values_mut() {
        sources.sort_by_key(|s| s.observation_id);
    }

    let agreed = groups.len() == 1;
    let resolved = ResolvedField {
        value: Some(best.value),
        authority: Some(best.source_class),
        agreed,
        contributing: obs.len() as u32,
    };

    let disagreement = if agreed {
        None
    } else {
        Some(FieldDisagreement {
            field,
            claims: groups
                .into_iter()
                .map(|(value_repr, sources)| FieldClaim {
                    value_repr,
                    sources,
                })
                .collect(),
        })
    };

    (resolved, disagreement)
}

/// Collects the present claims of one field into a `Vec<FieldObs<T>>`.
fn collect<T, G>(obs: &[TransactionObservation], get: G) -> Vec<FieldObs<T>>
where
    G: Fn(&TransactionObservation) -> Option<T>,
{
    obs.iter()
        .filter_map(|o| {
            get(o).map(|value| FieldObs {
                value,
                source_class: o.source_class,
                provider: o.provider,
                observation_id: o.observation_id,
            })
        })
        .collect()
}

/// Keeps the earlier of two candidate first-seen times, breaking exact ties by
/// the lower observation id (total, deterministic order).
fn keep_earliest(slot: &mut Option<SourcedTime>, cand: SourcedTime) {
    let replace = match slot {
        None => true,
        Some(cur) => (cand.time_ns, cand.observation_id) < (cur.time_ns, cur.observation_id),
    };
    if replace {
        *slot = Some(cand);
    }
}

/// Merge all observations of one transaction into a [`CanonicalTransaction`].
///
/// # Responsibility (§15)
/// Deterministic, pure reducer over provenance-tagged observations of a single
/// signature. Preserves feed disagreement, builds the dual timelines (never
/// equating timing across source classes or delivery modes), resolves fork
/// status, and records full provenance.
///
/// # Preconditions
/// All observations should concern the same `signature`; observations whose
/// signature differs from `signature` are ignored (defensive, keeps the function
/// total). Ordering of the input slice does **not** affect the output.
///
/// # Determinism / §22
/// No floating point, no wall-clock, no RNG. All ordering is by integer keys.
pub fn canonicalize_group(
    signature: Signature,
    observations: &[TransactionObservation],
) -> CanonicalTransaction {
    // Defensive: only merge observations of this signature.
    let obs: Vec<&TransactionObservation> = observations
        .iter()
        .filter(|o| o.signature == signature)
        .collect();

    // ---- Provenance (sorted, deterministic) ----
    let mut provenance: Vec<ProvenanceEntry> = obs
        .iter()
        .map(|o| ProvenanceEntry {
            observation_id: o.observation_id,
            source_class: o.source_class,
            provider: o.provider,
            delivery_mode: o.delivery_mode,
            receive_time_ns: o.receive_time_ns,
            source_sequence: o.source_sequence,
            connection_epoch: o.connection_epoch,
            payload_hash: o.payload_hash,
        })
        .collect();
    provenance.sort_by_key(|p| p.observation_id);

    // ---- Observation timeline (per class+mode; never pooled) ----
    let mut first_seen: BTreeMap<TimelineKey, SourcedTime> = BTreeMap::new();
    let mut reconstructed_earliest: Option<SourcedTime> = None;
    for o in &obs {
        let key = TimelineKey {
            source_class: o.source_class,
            delivery_mode: o.delivery_mode,
        };
        let cand = SourcedTime {
            time_ns: o.receive_time_ns,
            provider: o.provider,
            observation_id: o.observation_id,
        };
        let replace = match first_seen.get(&key) {
            None => true,
            Some(cur) => (cand.time_ns, cand.observation_id) < (cur.time_ns, cur.observation_id),
        };
        if replace {
            first_seen.insert(key, cand);
        }

        if o.source_class == SourceClass::EarliestSignal {
            if let Some(rt) = o.reconstructed_time_ns {
                keep_earliest(
                    &mut reconstructed_earliest,
                    SourcedTime {
                        time_ns: rt,
                        provider: o.provider,
                        observation_id: o.observation_id,
                    },
                );
            }
        }
    }
    let observation_timeline = ObservationTimeline {
        first_seen,
        reconstructed_earliest,
    };

    // ---- Canonical commitment timeline (live only) ----
    let mut canonical_timeline = CanonicalTimeline::default();
    for o in &obs {
        if o.delivery_mode != DeliveryMode::Live {
            continue;
        }
        let Some(level) = o.claim.commitment else {
            continue;
        };
        let st = SourcedTime {
            time_ns: o.receive_time_ns,
            provider: o.provider,
            observation_id: o.observation_id,
        };
        let slot = match level {
            Commitment::Seen => &mut canonical_timeline.seen_ns,
            Commitment::Processed => &mut canonical_timeline.processed_ns,
            Commitment::Confirmed => &mut canonical_timeline.confirmed_ns,
            Commitment::Finalized => &mut canonical_timeline.finalized_ns,
        };
        keep_earliest(slot, st);
    }

    // ---- Field resolution + preserved disagreement ----
    let materialized: Vec<TransactionObservation> = obs.iter().map(|o| (*o).clone()).collect();
    let mut disagreements: Vec<FieldDisagreement> = Vec::new();
    let mut push = |d: Option<FieldDisagreement>| {
        if let Some(d) = d {
            disagreements.push(d);
        }
    };

    let (slot, d) = resolve_field(
        FieldName::Slot,
        &collect(&materialized, |o| o.claim.slot),
        |v: &u64| *v as i128,
        false,
    );
    push(d);
    let (tx_index, d) = resolve_field(
        FieldName::TxIndex,
        &collect(&materialized, |o| o.claim.tx_index),
        |v: &u32| *v as i128,
        false,
    );
    push(d);
    let (success, d) = resolve_field(
        FieldName::Success,
        &collect(&materialized, |o| o.claim.success),
        |v: &bool| i128::from(*v),
        false,
    );
    push(d);
    let (base_fee_lamports, d) = resolve_field(
        FieldName::BaseFeeLamports,
        &collect(&materialized, |o| o.claim.base_fee_lamports),
        |v: &u64| *v as i128,
        false,
    );
    push(d);
    let (priority_fee_lamports, d) = resolve_field(
        FieldName::PriorityFeeLamports,
        &collect(&materialized, |o| o.claim.priority_fee_lamports),
        |v: &u64| *v as i128,
        false,
    );
    push(d);
    let (jito_tip_lamports, d) = resolve_field(
        FieldName::JitoTipLamports,
        &collect(&materialized, |o| o.claim.jito_tip_lamports),
        |v: &u64| *v as i128,
        false,
    );
    push(d);
    let (compute_units, d) = resolve_field(
        FieldName::ComputeUnits,
        &collect(&materialized, |o| o.claim.compute_units),
        |v: &u64| *v as i128,
        false,
    );
    push(d);
    let (commitment, d) = resolve_field(
        FieldName::Commitment,
        &collect(&materialized, |o| o.claim.commitment),
        |v: &Commitment| v.rank() as i128,
        true, // highest observed commitment wins among equal authority
    );
    push(d);
    let (fork, d) = resolve_field(
        FieldName::Fork,
        &collect(&materialized, |o| o.claim.fork),
        |v: &ForkStatus| fork_rank(*v),
        false,
    );
    push(d);

    // Deterministic disagreement ordering by field name.
    disagreements.sort_by_key(|d| field_order(d.field));

    CanonicalTransaction {
        signature,
        observation_timeline,
        canonical_timeline,
        fields: CanonicalFields {
            slot,
            tx_index,
            success,
            base_fee_lamports,
            priority_fee_lamports,
            jito_tip_lamports,
            compute_units,
            commitment,
        },
        fork,
        provenance,
        disagreements,
        observation_count: materialized.len() as u32,
    }
}

/// Injective integer encoding of a fork status (no float; §22).
const fn fork_rank(f: ForkStatus) -> i128 {
    match f {
        ForkStatus::Unknown => 0,
        ForkStatus::OnFork => 1,
        ForkStatus::Canonical => 2,
        ForkStatus::Dropped => 3,
    }
}

/// Stable ordering key for disagreement sorting.
const fn field_order(f: FieldName) -> u8 {
    match f {
        FieldName::Slot => 0,
        FieldName::TxIndex => 1,
        FieldName::Success => 2,
        FieldName::BaseFeeLamports => 3,
        FieldName::PriorityFeeLamports => 4,
        FieldName::JitoTipLamports => 5,
        FieldName::ComputeUnits => 6,
        FieldName::Commitment => 7,
        FieldName::Fork => 8,
    }
}
