//! The in-memory, memory-bounded QuantMemoryStore (§29.9, §57).
//!
//! Responsibility: hold the typed rows of every research-memory table with an
//! explicit per-table capacity bound and a durability-first overflow contract,
//! and expose the deterministic operations governance needs — insert, lookup, and
//! the sealed-experiment lifecycle.
//!
//! Persistence is **in** scope and durable: this store is the authoritative
//! in-process representation, and [`crate::persist`] gives it a crash-safe local
//! journal + snapshot with zero third-party dependencies. [`PersistenceSink`] is
//! the boundary an operator plugs a destination into — it is really called (see
//! [`QuantMemoryStore::flush`]), and [`crate::persist::BlobSink`] is the shipped
//! local implementation. Per §57's precedence (durability first), when a table is
//! full the store **rejects** the insert with [`StoreError::CapacityExceeded`]
//! rather than evicting reconciled evidence — silent eviction of sealed research
//! data is prohibited, on insert and on restore alike.

use crate::experiment::ExperimentError;
use crate::persist::PersistError;
use crate::rows::{
    AmplificationEdge, CallMarkout, CategoryAssignment, Experiment, ExperimentId, ExperimentResult,
    Hypothesis, MetaCategory, MetaRotationSnapshot, SocialCall, SourceQualityEntry,
};

/// The store's persistence boundary (§29.9).
///
/// Unlike the declared-and-never-called stub this replaces, implementations of
/// this trait are actually invoked — by [`QuantMemoryStore::flush`] — so research
/// memory reaches a device instead of only a type signature. The receiver is
/// `&mut self` and the result is fallible because a sink that can neither advance
/// its own state nor fail is a fiction about I/O.
///
/// [`crate::persist::BlobSink`] is the in-crate implementation: a local,
/// dependency-free, atomically-written blob. A server may supply another.
pub trait PersistenceSink {
    /// Durably persist a fully-formed store snapshot.
    ///
    /// # Errors
    /// Whatever the underlying destination reports.
    fn flush_snapshot(&mut self, store: &QuantMemoryStore) -> Result<(), PersistError>;
}

/// Errors returned by store operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    /// The target table is at its configured capacity (§57 durability-first:
    /// reject rather than evict).
    CapacityExceeded,
    /// No row with the requested id exists.
    NotFound,
    /// The operation would mutate a sealed experiment (§56.1 immutability).
    SealedImmutable,
    /// The experiment is already sealed.
    AlreadySealed,
}

impl From<ExperimentError> for StoreError {
    fn from(e: ExperimentError) -> Self {
        match e {
            ExperimentError::AlreadySealed => StoreError::AlreadySealed,
            ExperimentError::SealedImmutable => StoreError::SealedImmutable,
        }
    }
}

/// The research-memory store. Every field is a bounded table; `capacity` is the
/// shared maximum row count enforced per table on insert.
#[derive(Debug, Clone)]
pub struct QuantMemoryStore {
    /// Per-table maximum row count (§57 memory bound).
    pub capacity: usize,
    /// `hypotheses` table (§56.10).
    pub hypotheses: Vec<Hypothesis>,
    /// `experiments` table (§56.1).
    pub experiments: Vec<Experiment>,
    /// `results` table (§56.10).
    pub results: Vec<ExperimentResult>,
    /// `social_calls` table (§29.8).
    pub social_calls: Vec<SocialCall>,
    /// `call_markouts` table (§29.8 D1).
    pub call_markouts: Vec<CallMarkout>,
    /// `source_quality_ledger` table (§29.8).
    pub source_quality_ledger: Vec<SourceQualityEntry>,
    /// `amplification_edges` table (§29.7).
    pub amplification_edges: Vec<AmplificationEdge>,
    /// `meta_categories` table (§21.4 / §29.9).
    pub meta_categories: Vec<MetaCategory>,
    /// `category_assignments` table (§29.9).
    pub category_assignments: Vec<CategoryAssignment>,
    /// `meta_rotation_snapshots` table (§29.9).
    pub meta_rotation_snapshots: Vec<MetaRotationSnapshot>,
}

/// Push `row` onto `table` unless the table is at `capacity` (§57).
fn bounded_push<T>(table: &mut Vec<T>, capacity: usize, row: T) -> Result<(), StoreError> {
    if table.len() >= capacity {
        return Err(StoreError::CapacityExceeded);
    }
    table.push(row);
    Ok(())
}

impl QuantMemoryStore {
    /// Create an empty store whose every table is bounded to `capacity` rows.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        QuantMemoryStore {
            capacity,
            hypotheses: Vec::new(),
            experiments: Vec::new(),
            results: Vec::new(),
            social_calls: Vec::new(),
            call_markouts: Vec::new(),
            source_quality_ledger: Vec::new(),
            amplification_edges: Vec::new(),
            meta_categories: Vec::new(),
            category_assignments: Vec::new(),
            meta_rotation_snapshots: Vec::new(),
        }
    }

    /// Insert a hypothesis.
    ///
    /// # Errors
    /// [`StoreError::CapacityExceeded`] when the table is full.
    pub fn insert_hypothesis(&mut self, row: Hypothesis) -> Result<(), StoreError> {
        bounded_push(&mut self.hypotheses, self.capacity, row)
    }

    /// Insert an experiment.
    ///
    /// # Errors
    /// [`StoreError::CapacityExceeded`] when the table is full.
    pub fn insert_experiment(&mut self, row: Experiment) -> Result<(), StoreError> {
        bounded_push(&mut self.experiments, self.capacity, row)
    }

    /// Insert an experiment result.
    ///
    /// # Errors
    /// [`StoreError::CapacityExceeded`] when the table is full.
    pub fn insert_result(&mut self, row: ExperimentResult) -> Result<(), StoreError> {
        bounded_push(&mut self.results, self.capacity, row)
    }

    /// Insert a social call.
    ///
    /// # Errors
    /// [`StoreError::CapacityExceeded`] when the table is full.
    pub fn insert_social_call(&mut self, row: SocialCall) -> Result<(), StoreError> {
        bounded_push(&mut self.social_calls, self.capacity, row)
    }

    /// Insert a call markout.
    ///
    /// # Errors
    /// [`StoreError::CapacityExceeded`] when the table is full.
    pub fn insert_call_markout(&mut self, row: CallMarkout) -> Result<(), StoreError> {
        bounded_push(&mut self.call_markouts, self.capacity, row)
    }

    /// Insert a source-quality ledger entry.
    ///
    /// # Errors
    /// [`StoreError::CapacityExceeded`] when the table is full.
    pub fn insert_source_quality(&mut self, row: SourceQualityEntry) -> Result<(), StoreError> {
        bounded_push(&mut self.source_quality_ledger, self.capacity, row)
    }

    /// Insert an amplification edge.
    ///
    /// # Errors
    /// [`StoreError::CapacityExceeded`] when the table is full.
    pub fn insert_amplification_edge(&mut self, row: AmplificationEdge) -> Result<(), StoreError> {
        bounded_push(&mut self.amplification_edges, self.capacity, row)
    }

    /// Insert a meta category.
    ///
    /// # Errors
    /// [`StoreError::CapacityExceeded`] when the table is full.
    pub fn insert_meta_category(&mut self, row: MetaCategory) -> Result<(), StoreError> {
        bounded_push(&mut self.meta_categories, self.capacity, row)
    }

    /// Insert a category assignment.
    ///
    /// # Errors
    /// [`StoreError::CapacityExceeded`] when the table is full.
    pub fn insert_category_assignment(
        &mut self,
        row: CategoryAssignment,
    ) -> Result<(), StoreError> {
        bounded_push(&mut self.category_assignments, self.capacity, row)
    }

    /// Insert a meta-rotation snapshot.
    ///
    /// # Errors
    /// [`StoreError::CapacityExceeded`] when the table is full.
    pub fn insert_meta_rotation_snapshot(
        &mut self,
        row: MetaRotationSnapshot,
    ) -> Result<(), StoreError> {
        bounded_push(&mut self.meta_rotation_snapshots, self.capacity, row)
    }

    /// Borrow an experiment by id.
    #[must_use]
    pub fn experiment(&self, id: ExperimentId) -> Option<&Experiment> {
        self.experiments.iter().find(|e| e.id == id)
    }

    /// Seal the experiment with the given id, returning its content hash (§56.1).
    ///
    /// # Errors
    /// [`StoreError::NotFound`] if no such experiment; [`StoreError::AlreadySealed`]
    /// if it is already sealed.
    pub fn seal_experiment(
        &mut self,
        id: ExperimentId,
    ) -> Result<crate::hashing::SealHash, StoreError> {
        let exp = self
            .experiments
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or(StoreError::NotFound)?;
        Ok(exp.seal()?)
    }

    /// Replace the dataset hash of an unsealed experiment (§56.4). A sealed
    /// experiment is immutable.
    ///
    /// # Errors
    /// [`StoreError::NotFound`] if no such experiment; [`StoreError::SealedImmutable`]
    /// if it is sealed.
    pub fn update_experiment_dataset(
        &mut self,
        id: ExperimentId,
        dataset_hash: crate::rows::ContentHash,
    ) -> Result<(), StoreError> {
        let exp = self
            .experiments
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or(StoreError::NotFound)?;
        exp.set_dataset_hash(dataset_hash)?;
        Ok(())
    }

    /// The deterministic VOI-ranked queue of open hypotheses (§56.10). Thin
    /// convenience over [`crate::voi::rank_open`].
    #[must_use]
    pub fn voi_queue(&self) -> Vec<crate::voi::RankedHypothesis> {
        crate::voi::rank_open(&self.hypotheses)
    }
}
