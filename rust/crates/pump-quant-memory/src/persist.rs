//! Durable local persistence for the [`QuantMemoryStore`] — pure `std`, no
//! database, no dependency, crash-safe (§22, §56.9, §57, §99).
//!
//! Everything else in this crate is a pure function of its inputs. This module is
//! the single, explicit, isolated exception where I/O happens, and it is fenced
//! behind the [`BlobStore`] trait so the rest of the crate stays testable without
//! touching a filesystem. [`MemBlobStore`] is a full in-memory implementation, so
//! every persistence property below is proven in a test that never opens a file.
//!
//! The design is deliberately **identical in shape** to
//! `pump_quant_brain::persist`: one persistence idiom across the workspace, not
//! two. Same header, same frame layout, same checksum, same
//! snapshot-plus-journal-tail restore, same "restore never fails on damage, it
//! reports it" contract.
//!
//! # Why this module exists
//!
//! Research memory that dies on restart is not memory. Before this, every sealed
//! experiment, every reconciled markout, and every source scorecard the bot had
//! earned evaporated on each deploy — which silently reset the sample counts the
//! fail-closed governance gates depend on, and un-sealed research that §56.9 says
//! is immutable forever.
//!
//! # Format
//!
//! Two blobs, little-endian fixed-width integers throughout — no text, no schema
//! negotiation, no parser to get wrong (§22).
//!
//! ```text
//! header (20 bytes)  MAGIC[8] | format_version u32 | capacity u64
//! frame  (12 + N)    payload_len u32 | fnv1a_64(payload) u64 | payload[N]
//! payload (5 + B)    table_tag u8 | schema_version u32 | row_body[B]
//! ```
//!
//! * **Journal** (`*.jnl`) — append-only. One frame per row, written at the moment
//!   the row is admitted. Appending is O(1) and never rewrites history.
//! * **Snapshot** (`*.snap`) — every table in [`crate::schema::ALL_TABLES`] order,
//!   rows in insertion order, written temp → `fsync` → atomic rename so a crash
//!   mid-write can never leave a half-snapshot in place.
//!
//! Restore = read the snapshot, then replay the journal tail on top. Replay is
//! idempotent because admission is an **upsert on the table's primary key**: a
//! journal record already covered by the snapshot replaces an identical row and is
//! counted in [`RestoreReport::replaced`] rather than duplicated.
//!
//! # Crash recovery
//!
//! Every frame carries its own length and its own FNV-1a checksum, so a torn write
//! is *locally* detectable:
//!
//! * **Truncated tail** — the trailing bytes do not form a whole frame and no valid
//!   frame follows them. The tail is discarded and counted in
//!   [`RestoreReport::truncated_tail_bytes`]. This is the normal crash case: the
//!   process died mid-`write`.
//! * **Corrupt frame** — a frame fails to verify but a well-formed frame does exist
//!   later in the blob. The reader resynchronises onto that frame and continues,
//!   counting [`RestoreReport::corrupt_records_skipped`]. One bad sector costs one
//!   row, not the whole history.
//!
//! In neither case does a damaged record enter the store, and in neither case does
//! restore fail. A crash during a write must not poison research memory.
//!
//! # Fail-closed schema versioning (§56.9)
//!
//! Every payload states the [`crate::schema::SCHEMA_VERSION`] it was written under.
//! A record whose version is **newer** than this build is refused outright and
//! counted in [`RestoreReport::refused_newer_schema`]: unknown bytes are never
//! reinterpreted under an older layout, because a silently mis-decoded lamport
//! field is worse than a missing row.
//!
//! # Sealing survives restart (§56.1 / §56.9)
//!
//! A sealed [`Experiment`] restores as sealed, and its recorded [`SealHash`] is
//! **recomputed** from the decoded content on read: a sealed record whose stored
//! hash disagrees with its own bytes is corruption, not evidence. Once a sealed
//! experiment is in the restored store, no later record may overwrite it with
//! different content — an attempt is refused and counted in
//! [`RestoreReport::refused_sealed_immutable`]. A restart cannot un-seal research.

use std::fmt;
use std::fs;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

use crate::hashing::{fnv1a_64, SealHash};
use crate::rows::{
    AmplificationEdge, AssignmentId, CallMarkout, CategoryAssignment, ContentHash, EdgeId,
    EdgeKind, Experiment, ExperimentId, ExperimentResult, Hypothesis, HypothesisId, InferenceState,
    LifecycleTiming, MarkoutHorizon, MarkoutId, MetaCategory, MetaCategoryId, MetaLifecycle,
    MetaRotationSnapshot, ResultId, SnapshotId, SocialCall, SocialCallId, SourceClassification,
    SourceId, SourceQualityEntry,
};
use crate::schema::SCHEMA_VERSION;
use crate::store::{PersistenceSink, QuantMemoryStore};

// ---------------------------------------------------------------------------
// Format constants
// ---------------------------------------------------------------------------

/// File magic identifying a Hermes quant-memory store (§56.9 manifested data).
pub const MAGIC: [u8; 8] = *b"HRMQMEMS";

/// On-disk framing version. Bumped on any framing or record-layout change; this
/// is independent of [`SCHEMA_VERSION`], which versions the *rows*.
pub const FORMAT_VERSION: u32 = 1;

/// Header length in bytes: magic + format version + capacity.
pub const HEADER_LEN: usize = 8 + 4 + 8;

/// Frame preamble length in bytes: payload length + checksum.
pub const FRAME_HEADER_LEN: usize = 4 + 8;

/// Per-record envelope length in bytes: table tag + schema version.
pub const RECORD_ENVELOPE_LEN: usize = 1 + 4;

/// Largest payload the reader will believe. Anything larger is treated as
/// corruption rather than as an allocation instruction — a length field is
/// attacker-shaped even when there is no attacker (§99 bounded work).
pub const MAX_RECORD_LEN: usize = 4_096;

/// Upper bound on the byte-wise resynchronisation scan after a damaged frame
/// (§99: recovery work is bounded, never quadratic in blob size). If no valid
/// frame is found within this window the remainder is treated as an unrecoverable
/// tail.
pub const RESYNC_SCAN_LIMIT: usize = 64 * 1_024;

/// Number of tables the store persists (§29.9).
pub const TABLE_COUNT: usize = 10;

/// Wire length of a `hypotheses` payload (§56.10).
pub const HYPOTHESIS_WIRE_LEN: usize = RECORD_ENVELOPE_LEN + 81;
/// Wire length of an `experiments` payload (§56.1).
pub const EXPERIMENT_WIRE_LEN: usize = RECORD_ENVELOPE_LEN + 138;
/// Wire length of a `results` payload (§56.10).
pub const RESULT_WIRE_LEN: usize = RECORD_ENVELOPE_LEN + 49;
/// Wire length of a `social_calls` payload (§29.8).
pub const SOCIAL_CALL_WIRE_LEN: usize = RECORD_ENVELOPE_LEN + 89;
/// Wire length of a `call_markouts` payload (§29.8 D1).
pub const CALL_MARKOUT_WIRE_LEN: usize = RECORD_ENVELOPE_LEN + 25;
/// Wire length of a `source_quality_ledger` payload (§29.8).
pub const SOURCE_QUALITY_WIRE_LEN: usize = RECORD_ENVELOPE_LEN + 37;
/// Wire length of an `amplification_edges` payload (§29.7).
pub const AMPLIFICATION_EDGE_WIRE_LEN: usize = RECORD_ENVELOPE_LEN + 65;
/// Wire length of a `meta_categories` payload (§21.4 / §29.9).
pub const META_CATEGORY_WIRE_LEN: usize = RECORD_ENVELOPE_LEN + 49;
/// Wire length of a `category_assignments` payload (§29.9).
pub const CATEGORY_ASSIGNMENT_WIRE_LEN: usize = RECORD_ENVELOPE_LEN + 64;
/// Wire length of a `meta_rotation_snapshots` payload (§29.9).
pub const META_ROTATION_SNAPSHOT_WIRE_LEN: usize = RECORD_ENVELOPE_LEN + 33;

// ---------------------------------------------------------------------------
// BlobStore
// ---------------------------------------------------------------------------

/// The crate's entire I/O surface.
///
/// Three operations, deliberately: read a whole blob, append to one, replace one
/// atomically. Nothing here seeks, and nothing rewrites in place.
pub trait BlobStore {
    /// Read a blob in full. Returns an empty vector for a path that does not exist.
    ///
    /// # Errors
    /// Propagates any underlying I/O failure other than "not found".
    fn read_all(&self, path: &Path) -> io::Result<Vec<u8>>;

    /// Append bytes to a blob, creating it if absent.
    ///
    /// # Errors
    /// Propagates any underlying I/O failure.
    fn append(&mut self, path: &Path, bytes: &[u8]) -> io::Result<()>;

    /// Replace a blob's contents atomically (write temp, fsync, rename).
    ///
    /// # Errors
    /// Propagates any underlying I/O failure.
    fn write_atomic(&mut self, path: &Path, bytes: &[u8]) -> io::Result<()>;

    /// Whether a blob exists.
    fn exists(&self, path: &Path) -> bool;
}

/// Real-filesystem [`BlobStore`].
#[derive(Debug, Clone, Copy, Default)]
pub struct FileBlobStore;

impl FileBlobStore {
    /// Suffix used for the temp file in [`BlobStore::write_atomic`].
    pub const TMP_SUFFIX: &'static str = ".tmp";

    /// Path of the temp file a [`BlobStore::write_atomic`] of `path` goes through.
    #[must_use]
    pub fn tmp_path(path: &Path) -> PathBuf {
        let mut s = path.as_os_str().to_os_string();
        s.push(Self::TMP_SUFFIX);
        PathBuf::from(s)
    }
}

impl BlobStore for FileBlobStore {
    fn read_all(&self, path: &Path) -> io::Result<Vec<u8>> {
        match fs::File::open(path) {
            Ok(mut f) => {
                let mut buf = Vec::new();
                f.read_to_end(&mut buf)?;
                Ok(buf)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    fn append(&mut self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        f.write_all(bytes)?;
        // Durability point: the journal is the crash-recovery source of truth, so
        // an append that has not reached the device is not an append.
        f.sync_data()
    }

    fn write_atomic(&mut self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let tmp = Self::tmp_path(path);
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(bytes)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, path)
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
}

/// In-memory [`BlobStore`] for pure tests and replay harnesses.
///
/// Backed by a `Vec` of `(path, bytes)` pairs rather than a hash map so iteration
/// order is insertion order — deterministic, like everything else here (§22).
#[derive(Debug, Clone, Default)]
pub struct MemBlobStore {
    blobs: Vec<(PathBuf, Vec<u8>)>,
}

impl MemBlobStore {
    /// An empty in-memory store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn slot(&mut self, path: &Path) -> &mut Vec<u8> {
        if let Some(i) = self.blobs.iter().position(|(p, _)| p == path) {
            return &mut self.blobs[i].1;
        }
        self.blobs.push((path.to_path_buf(), Vec::new()));
        let last = self.blobs.len() - 1;
        &mut self.blobs[last].1
    }

    /// Truncate a blob to `len` bytes — the test hook that simulates a crash in the
    /// middle of a write. Returns the resulting length.
    pub fn truncate(&mut self, path: &Path, len: usize) -> usize {
        let slot = self.slot(path);
        if len < slot.len() {
            slot.truncate(len);
        }
        slot.len()
    }

    /// Flip bits of one byte of a blob — the test hook that simulates bit rot.
    /// No-op if the offset is past the end.
    pub fn corrupt_byte(&mut self, path: &Path, offset: usize, mask: u8) {
        let slot = self.slot(path);
        if offset < slot.len() {
            slot[offset] ^= mask;
        }
    }

    /// Current length of a blob (`0` if absent).
    #[must_use]
    pub fn len_of(&self, path: &Path) -> usize {
        self.blobs
            .iter()
            .find(|(p, _)| p == path)
            .map_or(0, |(_, b)| b.len())
    }
}

impl BlobStore for MemBlobStore {
    fn read_all(&self, path: &Path) -> io::Result<Vec<u8>> {
        Ok(self
            .blobs
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, b)| b.clone())
            .unwrap_or_default())
    }

    fn append(&mut self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.slot(path).extend_from_slice(bytes);
        Ok(())
    }

    fn write_atomic(&mut self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let slot = self.slot(path);
        slot.clear();
        slot.extend_from_slice(bytes);
        Ok(())
    }

    fn exists(&self, path: &Path) -> bool {
        self.blobs.iter().any(|(p, _)| p == path)
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Everything that can go wrong persisting or restoring.
///
/// Note what is **not** here: record damage. A torn or rotten record is reported
/// in a [`RestoreReport`], never raised as an error, because a restore that fails
/// on a bad sector loses the whole memory to save one row.
#[derive(Debug)]
pub enum PersistError {
    /// Underlying I/O failure.
    Io(io::Error),
    /// The blob does not begin with [`MAGIC`] — a foreign file, not a corrupt one.
    BadMagic,
    /// The blob's [`FORMAT_VERSION`] is not one this build can read.
    UnsupportedFormat {
        /// Framing version found in the header.
        found: u32,
        /// Framing version this build writes.
        expected: u32,
    },
    /// A header was present but truncated.
    TruncatedHeader {
        /// Bytes actually present.
        found: usize,
        /// Bytes a header requires.
        expected: usize,
    },
}

impl fmt::Display for PersistError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "quant memory store io error: {e}"),
            Self::BadMagic => write!(f, "quant memory store magic mismatch"),
            Self::UnsupportedFormat { found, expected } => write!(
                f,
                "quant memory store format {found} is not readable by build format {expected}"
            ),
            Self::TruncatedHeader { found, expected } => write!(
                f,
                "quant memory store header truncated: {found} of {expected} bytes"
            ),
        }
    }
}

impl std::error::Error for PersistError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for PersistError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

// ---------------------------------------------------------------------------
// Table tags and the record sum type
// ---------------------------------------------------------------------------

/// On-disk discriminator naming the table a payload belongs to.
///
/// Ordinals match the index of the table in [`crate::schema::ALL_TABLES`]; the
/// `table_tags_match_schema_order` test pins that, so the schema listing and the
/// wire format cannot drift apart (§29.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TableTag {
    /// `hypotheses` (§56.10).
    Hypotheses,
    /// `experiments` (§56.1).
    Experiments,
    /// `results` (§56.10).
    Results,
    /// `social_calls` (§29.8).
    SocialCalls,
    /// `call_markouts` (§29.8 D1).
    CallMarkouts,
    /// `source_quality_ledger` (§29.8).
    SourceQualityLedger,
    /// `amplification_edges` (§29.7).
    AmplificationEdges,
    /// `meta_categories` (§21.4 / §29.9).
    MetaCategories,
    /// `category_assignments` (§29.9).
    CategoryAssignments,
    /// `meta_rotation_snapshots` (§29.9).
    MetaRotationSnapshots,
}

impl TableTag {
    /// Every tag, in [`crate::schema::ALL_TABLES`] order.
    pub const ALL: [TableTag; TABLE_COUNT] = [
        TableTag::Hypotheses,
        TableTag::Experiments,
        TableTag::Results,
        TableTag::SocialCalls,
        TableTag::CallMarkouts,
        TableTag::SourceQualityLedger,
        TableTag::AmplificationEdges,
        TableTag::MetaCategories,
        TableTag::CategoryAssignments,
        TableTag::MetaRotationSnapshots,
    ];

    /// Stable on-disk ordinal. Never renumber (§56.9).
    #[must_use]
    pub const fn ordinal(self) -> u8 {
        match self {
            TableTag::Hypotheses => 0,
            TableTag::Experiments => 1,
            TableTag::Results => 2,
            TableTag::SocialCalls => 3,
            TableTag::CallMarkouts => 4,
            TableTag::SourceQualityLedger => 5,
            TableTag::AmplificationEdges => 6,
            TableTag::MetaCategories => 7,
            TableTag::CategoryAssignments => 8,
            TableTag::MetaRotationSnapshots => 9,
        }
    }

    /// Inverse of [`Self::ordinal`]; `None` for an unknown byte (fail-closed).
    #[must_use]
    pub const fn from_ordinal(ordinal: u8) -> Option<Self> {
        match ordinal {
            0 => Some(TableTag::Hypotheses),
            1 => Some(TableTag::Experiments),
            2 => Some(TableTag::Results),
            3 => Some(TableTag::SocialCalls),
            4 => Some(TableTag::CallMarkouts),
            5 => Some(TableTag::SourceQualityLedger),
            6 => Some(TableTag::AmplificationEdges),
            7 => Some(TableTag::MetaCategories),
            8 => Some(TableTag::CategoryAssignments),
            9 => Some(TableTag::MetaRotationSnapshots),
            _ => None,
        }
    }

    /// Exact wire length of one payload of this table, envelope included.
    #[must_use]
    pub const fn wire_len(self) -> usize {
        match self {
            TableTag::Hypotheses => HYPOTHESIS_WIRE_LEN,
            TableTag::Experiments => EXPERIMENT_WIRE_LEN,
            TableTag::Results => RESULT_WIRE_LEN,
            TableTag::SocialCalls => SOCIAL_CALL_WIRE_LEN,
            TableTag::CallMarkouts => CALL_MARKOUT_WIRE_LEN,
            TableTag::SourceQualityLedger => SOURCE_QUALITY_WIRE_LEN,
            TableTag::AmplificationEdges => AMPLIFICATION_EDGE_WIRE_LEN,
            TableTag::MetaCategories => META_CATEGORY_WIRE_LEN,
            TableTag::CategoryAssignments => CATEGORY_ASSIGNMENT_WIRE_LEN,
            TableTag::MetaRotationSnapshots => META_ROTATION_SNAPSHOT_WIRE_LEN,
        }
    }

    /// Index of this table's counters inside the [`RestoreReport`] arrays.
    #[must_use]
    pub const fn index(self) -> usize {
        self.ordinal() as usize
    }
}

/// One row of any research-memory table, tagged — the unit the journal stores and
/// the unit admission operates on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryRecord {
    /// A `hypotheses` row.
    Hypothesis(Hypothesis),
    /// An `experiments` row.
    Experiment(Experiment),
    /// A `results` row.
    Result(ExperimentResult),
    /// A `social_calls` row.
    SocialCall(SocialCall),
    /// A `call_markouts` row.
    CallMarkout(CallMarkout),
    /// A `source_quality_ledger` row.
    SourceQuality(SourceQualityEntry),
    /// An `amplification_edges` row.
    AmplificationEdge(AmplificationEdge),
    /// A `meta_categories` row.
    MetaCategory(MetaCategory),
    /// A `category_assignments` row.
    CategoryAssignment(CategoryAssignment),
    /// A `meta_rotation_snapshots` row.
    MetaRotationSnapshot(MetaRotationSnapshot),
}

impl MemoryRecord {
    /// Which table this record belongs to.
    #[must_use]
    pub const fn tag(&self) -> TableTag {
        match self {
            MemoryRecord::Hypothesis(_) => TableTag::Hypotheses,
            MemoryRecord::Experiment(_) => TableTag::Experiments,
            MemoryRecord::Result(_) => TableTag::Results,
            MemoryRecord::SocialCall(_) => TableTag::SocialCalls,
            MemoryRecord::CallMarkout(_) => TableTag::CallMarkouts,
            MemoryRecord::SourceQuality(_) => TableTag::SourceQualityLedger,
            MemoryRecord::AmplificationEdge(_) => TableTag::AmplificationEdges,
            MemoryRecord::MetaCategory(_) => TableTag::MetaCategories,
            MemoryRecord::CategoryAssignment(_) => TableTag::CategoryAssignments,
            MemoryRecord::MetaRotationSnapshot(_) => TableTag::MetaRotationSnapshots,
        }
    }

    /// The schema version this record will be written under (§56.9).
    ///
    /// [`Hypothesis`] and [`Experiment`] carry their own `schema_version` field and
    /// it is written verbatim, so a row authored under an older schema keeps its
    /// provenance across a round trip. The remaining tables have no per-row column,
    /// so the envelope carries the build's [`SCHEMA_VERSION`].
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        match self {
            MemoryRecord::Hypothesis(h) => h.schema_version,
            MemoryRecord::Experiment(e) => e.schema_version,
            _ => SCHEMA_VERSION,
        }
    }
}

macro_rules! record_from {
    ($ty:ty, $variant:ident) => {
        impl From<$ty> for MemoryRecord {
            fn from(row: $ty) -> Self {
                MemoryRecord::$variant(row)
            }
        }
    };
}

record_from!(Hypothesis, Hypothesis);
record_from!(Experiment, Experiment);
record_from!(ExperimentResult, Result);
record_from!(SocialCall, SocialCall);
record_from!(CallMarkout, CallMarkout);
record_from!(SourceQualityEntry, SourceQuality);
record_from!(AmplificationEdge, AmplificationEdge);
record_from!(MetaCategory, MetaCategory);
record_from!(CategoryAssignment, CategoryAssignment);
record_from!(MetaRotationSnapshot, MetaRotationSnapshot);

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

fn put_u8(b: &mut Vec<u8>, v: u8) {
    b.push(v);
}
fn put_u32(b: &mut Vec<u8>, v: u32) {
    b.extend_from_slice(&v.to_le_bytes());
}
fn put_u64(b: &mut Vec<u8>, v: u64) {
    b.extend_from_slice(&v.to_le_bytes());
}
fn put_i64(b: &mut Vec<u8>, v: i64) {
    b.extend_from_slice(&v.to_le_bytes());
}
fn put_i128(b: &mut Vec<u8>, v: i128) {
    b.extend_from_slice(&v.to_le_bytes());
}
fn put_hash(b: &mut Vec<u8>, v: &ContentHash) {
    b.extend_from_slice(v);
}

/// Serialize one record into its fixed-width little-endian payload, envelope
/// included.
///
/// The encoding is total and allocation-bounded: the output length is exactly
/// `record.tag().wire_len()` for every input.
#[must_use]
pub fn encode_record(record: &MemoryRecord) -> Vec<u8> {
    let tag = record.tag();
    let mut b = Vec::with_capacity(tag.wire_len());
    put_u8(&mut b, tag.ordinal());
    put_u32(&mut b, record.schema_version());
    match record {
        MemoryRecord::Hypothesis(h) => {
            put_u64(&mut b, h.id.0);
            put_hash(&mut b, &h.statement_hash);
            put_i128(&mut b, h.expected_impact_lamports);
            put_i64(&mut b, h.prob_true_bps);
            put_u64(&mut b, h.cost_to_test_lamports);
            put_u64(&mut b, h.edge_half_life_secs);
            put_u8(&mut b, h.status.ordinal());
        }
        MemoryRecord::Experiment(e) => {
            put_u64(&mut b, e.id.0);
            put_u64(&mut b, e.hypothesis_id.0);
            put_hash(&mut b, &e.title_hash);
            put_hash(&mut b, &e.causal_mechanism_hash);
            put_hash(&mut b, &e.dataset_hash);
            put_u64(&mut b, e.config_hash);
            put_u64(&mut b, e.created_at_ns);
            put_u8(&mut b, u8::from(e.sealed));
            match e.seal_hash {
                Some(SealHash(h)) => {
                    put_u8(&mut b, 1);
                    put_u64(&mut b, h);
                }
                None => {
                    put_u8(&mut b, 0);
                    put_u64(&mut b, 0);
                }
            }
        }
        MemoryRecord::Result(r) => {
            put_u64(&mut b, r.id.0);
            put_u64(&mut b, r.experiment_id.0);
            put_i128(&mut b, r.net_sol_effect_lamports);
            put_i64(&mut b, r.significance_bps);
            put_u8(&mut b, r.outcome.ordinal());
            put_u64(&mut b, r.reconciled_at_ns);
        }
        MemoryRecord::SocialCall(c) => {
            put_u64(&mut b, c.id.0);
            put_u64(&mut b, c.source_id.0);
            put_hash(&mut b, &c.token_hash);
            put_u64(&mut b, c.captured_at_ns);
            put_hash(&mut b, &c.content_hash);
            put_u8(&mut b, c.timing.ordinal());
        }
        MemoryRecord::CallMarkout(m) => {
            put_u64(&mut b, m.id.0);
            put_u64(&mut b, m.call_id.0);
            put_u8(&mut b, m.horizon.ordinal());
            put_i64(&mut b, m.executable_return_bps);
        }
        MemoryRecord::SourceQuality(s) => {
            put_u64(&mut b, s.source_id.0);
            put_u8(&mut b, s.classification.ordinal());
            put_i64(&mut b, s.confidence_bps);
            put_u32(&mut b, s.sample_size);
            put_i64(&mut b, s.mean_markout_30m_bps);
            put_u64(&mut b, s.updated_at_ns);
        }
        MemoryRecord::AmplificationEdge(e) => {
            put_u64(&mut b, e.id.0);
            put_u64(&mut b, e.from_source.0);
            put_u64(&mut b, e.to_source.0);
            put_hash(&mut b, &e.token_hash);
            put_u64(&mut b, e.observed_at_ns);
            put_u8(&mut b, e.kind.ordinal());
        }
        MemoryRecord::MetaCategory(c) => {
            put_u64(&mut b, c.id.0);
            put_hash(&mut b, &c.name_hash);
            put_u8(&mut b, c.lifecycle.ordinal());
            put_u64(&mut b, c.updated_at_ns);
        }
        MemoryRecord::CategoryAssignment(a) => {
            put_u64(&mut b, a.id.0);
            put_u64(&mut b, a.category_id.0);
            put_hash(&mut b, &a.token_hash);
            put_i64(&mut b, a.confidence_bps);
            put_u64(&mut b, a.assigned_at_ns);
        }
        MemoryRecord::MetaRotationSnapshot(s) => {
            put_u64(&mut b, s.id.0);
            put_u64(&mut b, s.category_id.0);
            put_u64(&mut b, s.taken_at_ns);
            put_u8(&mut b, s.lifecycle.ordinal());
            put_i64(&mut b, s.launch_share_bps);
        }
    }
    debug_assert_eq!(b.len(), tag.wire_len());
    b
}

/// Little-endian cursor over a byte slice. Every read is bounds-checked and
/// returns `Option`, so a malformed record can only ever produce `None` — never a
/// panic and never a partial decode.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    const fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn i128(&mut self) -> Option<i128> {
        Some(i128::from_le_bytes(self.take(16)?.try_into().ok()?))
    }

    fn hash(&mut self) -> Option<ContentHash> {
        let mut h = [0u8; 32];
        h.copy_from_slice(self.take(32)?);
        Some(h)
    }
}

/// Why a payload did not become a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeOutcome {
    /// The payload is structurally invalid for this build: wrong length, unknown
    /// table tag, unknown enum ordinal, or a sealed experiment whose recorded seal
    /// hash disagrees with its own content. Counted as corruption.
    Malformed,
    /// The payload declares a [`SCHEMA_VERSION`] newer than this build understands.
    /// Refused fail-closed (§56.9) — never reinterpreted under the older layout.
    NewerSchema {
        /// Version the record declares.
        found: u32,
        /// Version this build can read up to.
        supported: u32,
    },
}

/// Deserialize one payload.
///
/// Returns `Err(DecodeOutcome::NewerSchema { .. })` for a forward-versioned record
/// and `Err(DecodeOutcome::Malformed)` for anything this build cannot make sense
/// of. Neither ever panics and neither ever produces a partially-populated row.
///
/// # Errors
/// See [`DecodeOutcome`].
#[allow(clippy::too_many_lines)]
pub fn decode_record(payload: &[u8]) -> Result<MemoryRecord, DecodeOutcome> {
    let mut r = Reader::new(payload);
    let tag = r
        .u8()
        .and_then(TableTag::from_ordinal)
        .ok_or(DecodeOutcome::Malformed)?;
    if payload.len() != tag.wire_len() {
        return Err(DecodeOutcome::Malformed);
    }
    let schema_version = r.u32().ok_or(DecodeOutcome::Malformed)?;
    if schema_version > SCHEMA_VERSION {
        return Err(DecodeOutcome::NewerSchema {
            found: schema_version,
            supported: SCHEMA_VERSION,
        });
    }

    let m = DecodeOutcome::Malformed;
    let record = match tag {
        TableTag::Hypotheses => MemoryRecord::Hypothesis(Hypothesis {
            id: HypothesisId(r.u64().ok_or(m)?),
            schema_version,
            statement_hash: r.hash().ok_or(m)?,
            expected_impact_lamports: r.i128().ok_or(m)?,
            prob_true_bps: r.i64().ok_or(m)?,
            cost_to_test_lamports: r.u64().ok_or(m)?,
            edge_half_life_secs: r.u64().ok_or(m)?,
            status: r
                .u8()
                .and_then(InferenceState::from_ordinal)
                .ok_or(DecodeOutcome::Malformed)?,
        }),
        TableTag::Experiments => {
            let e = Experiment {
                id: ExperimentId(r.u64().ok_or(m)?),
                hypothesis_id: HypothesisId(r.u64().ok_or(m)?),
                schema_version,
                title_hash: r.hash().ok_or(m)?,
                causal_mechanism_hash: r.hash().ok_or(m)?,
                dataset_hash: r.hash().ok_or(m)?,
                config_hash: r.u64().ok_or(m)?,
                created_at_ns: r.u64().ok_or(m)?,
                sealed: match r.u8().ok_or(m)? {
                    0 => false,
                    1 => true,
                    _ => return Err(m),
                },
                seal_hash: {
                    let present = r.u8().ok_or(m)?;
                    let value = r.u64().ok_or(m)?;
                    match present {
                        0 if value == 0 => None,
                        1 => Some(SealHash(value)),
                        _ => return Err(m),
                    }
                },
            };
            // The seal flag and the seal hash are one fact, and the hash is derived
            // from the content — so both are re-checked, never trusted (§56.1).
            if e.sealed != e.seal_hash.is_some() {
                return Err(m);
            }
            if e.sealed && !e.verify_integrity() {
                return Err(m);
            }
            MemoryRecord::Experiment(e)
        }
        TableTag::Results => MemoryRecord::Result(ExperimentResult {
            id: ResultId(r.u64().ok_or(m)?),
            experiment_id: ExperimentId(r.u64().ok_or(m)?),
            net_sol_effect_lamports: r.i128().ok_or(m)?,
            significance_bps: r.i64().ok_or(m)?,
            outcome: r.u8().and_then(InferenceState::from_ordinal).ok_or(m)?,
            reconciled_at_ns: r.u64().ok_or(m)?,
        }),
        TableTag::SocialCalls => MemoryRecord::SocialCall(SocialCall {
            id: SocialCallId(r.u64().ok_or(m)?),
            source_id: SourceId(r.u64().ok_or(m)?),
            token_hash: r.hash().ok_or(m)?,
            captured_at_ns: r.u64().ok_or(m)?,
            content_hash: r.hash().ok_or(m)?,
            timing: r.u8().and_then(LifecycleTiming::from_ordinal).ok_or(m)?,
        }),
        TableTag::CallMarkouts => MemoryRecord::CallMarkout(CallMarkout {
            id: MarkoutId(r.u64().ok_or(m)?),
            call_id: SocialCallId(r.u64().ok_or(m)?),
            horizon: r.u8().and_then(MarkoutHorizon::from_ordinal).ok_or(m)?,
            executable_return_bps: r.i64().ok_or(m)?,
        }),
        TableTag::SourceQualityLedger => MemoryRecord::SourceQuality(SourceQualityEntry {
            source_id: SourceId(r.u64().ok_or(m)?),
            classification: r
                .u8()
                .and_then(SourceClassification::from_ordinal)
                .ok_or(m)?,
            confidence_bps: r.i64().ok_or(m)?,
            sample_size: r.u32().ok_or(m)?,
            mean_markout_30m_bps: r.i64().ok_or(m)?,
            updated_at_ns: r.u64().ok_or(m)?,
        }),
        TableTag::AmplificationEdges => MemoryRecord::AmplificationEdge(AmplificationEdge {
            id: EdgeId(r.u64().ok_or(m)?),
            from_source: SourceId(r.u64().ok_or(m)?),
            to_source: SourceId(r.u64().ok_or(m)?),
            token_hash: r.hash().ok_or(m)?,
            observed_at_ns: r.u64().ok_or(m)?,
            kind: r.u8().and_then(EdgeKind::from_ordinal).ok_or(m)?,
        }),
        TableTag::MetaCategories => MemoryRecord::MetaCategory(MetaCategory {
            id: MetaCategoryId(r.u64().ok_or(m)?),
            name_hash: r.hash().ok_or(m)?,
            lifecycle: r.u8().and_then(MetaLifecycle::from_ordinal).ok_or(m)?,
            updated_at_ns: r.u64().ok_or(m)?,
        }),
        TableTag::CategoryAssignments => MemoryRecord::CategoryAssignment(CategoryAssignment {
            id: AssignmentId(r.u64().ok_or(m)?),
            category_id: MetaCategoryId(r.u64().ok_or(m)?),
            token_hash: r.hash().ok_or(m)?,
            confidence_bps: r.i64().ok_or(m)?,
            assigned_at_ns: r.u64().ok_or(m)?,
        }),
        TableTag::MetaRotationSnapshots => {
            MemoryRecord::MetaRotationSnapshot(MetaRotationSnapshot {
                id: SnapshotId(r.u64().ok_or(m)?),
                category_id: MetaCategoryId(r.u64().ok_or(m)?),
                taken_at_ns: r.u64().ok_or(m)?,
                lifecycle: r.u8().and_then(MetaLifecycle::from_ordinal).ok_or(m)?,
                launch_share_bps: r.i64().ok_or(m)?,
            })
        }
    };
    Ok(record)
}

// ---------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------

/// Wrap a payload in a length-prefixed, checksummed frame.
#[must_use]
pub fn frame(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(FRAME_HEADER_LEN + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&fnv1a_64(payload).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// Build a blob header declaring `capacity` (`0` means "not declared").
#[must_use]
pub fn header(capacity: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&capacity.to_le_bytes());
    out
}

/// Parse a header, returning the declared capacity.
///
/// # Errors
/// [`PersistError::TruncatedHeader`], [`PersistError::BadMagic`] or
/// [`PersistError::UnsupportedFormat`].
pub fn parse_header(bytes: &[u8]) -> Result<u64, PersistError> {
    if bytes.len() < HEADER_LEN {
        return Err(PersistError::TruncatedHeader {
            found: bytes.len(),
            expected: HEADER_LEN,
        });
    }
    if bytes[..8] != MAGIC {
        return Err(PersistError::BadMagic);
    }
    let mut r = Reader::new(&bytes[8..HEADER_LEN]);
    let truncated = PersistError::TruncatedHeader {
        found: bytes.len(),
        expected: HEADER_LEN,
    };
    let version = match r.u32() {
        Some(v) => v,
        None => return Err(truncated),
    };
    if version != FORMAT_VERSION {
        return Err(PersistError::UnsupportedFormat {
            found: version,
            expected: FORMAT_VERSION,
        });
    }
    match r.u64() {
        Some(c) => Ok(c),
        None => Err(truncated),
    }
}

/// Outcome of scanning one blob's frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScanReport {
    /// Frames whose length and checksum both verified.
    pub good_frames: u64,
    /// Frames skipped because the checksum failed or the length was implausible,
    /// where a well-formed frame still followed.
    pub corrupt_frames: u64,
    /// Bytes discarded at the end of the blob because no whole frame could be read
    /// and none followed — the signature of a crash mid-write.
    pub truncated_tail_bytes: u64,
}

/// Try to read a well-formed frame at `pos`, returning its payload and the offset
/// just past it.
fn try_frame(body: &[u8], pos: usize) -> Option<(&[u8], usize)> {
    let remaining = body.len().checked_sub(pos)?;
    if remaining < FRAME_HEADER_LEN {
        return None;
    }
    let mut r = Reader::new(&body[pos..]);
    let len = r.u32()? as usize;
    let checksum = r.u64()?;
    if len == 0 || len > MAX_RECORD_LEN || remaining < FRAME_HEADER_LEN + len {
        return None;
    }
    let payload = &body[pos + FRAME_HEADER_LEN..pos + FRAME_HEADER_LEN + len];
    if fnv1a_64(payload) != checksum {
        return None;
    }
    Some((payload, pos + FRAME_HEADER_LEN + len))
}

/// Find the next offset at or after `from` that begins a verifying frame.
///
/// Byte-wise rather than stride-wise because this store's records are
/// variable-width (ten tables, ten widths), so there is no single stride to jump.
/// A false positive would require a 64-bit checksum collision. The scan is bounded
/// by [`RESYNC_SCAN_LIMIT`] so recovery work stays linear (§99).
fn resync(body: &[u8], from: usize) -> Option<usize> {
    let end = body.len().min(from.saturating_add(RESYNC_SCAN_LIMIT));
    (from..end).find(|&p| try_frame(body, p).is_some())
}

/// Scan a blob body (everything after the header) into verified payload slices.
///
/// The distinction the reader draws is *"is there anything good after this?"*: a
/// failure with a well-formed frame somewhere later is mid-blob **corruption**
/// (skip it, resynchronise, keep the rest); a failure with nothing good after it
/// is a **truncated tail** (a crash mid-write). See the module docs.
#[must_use]
pub fn scan_frames(body: &[u8]) -> (Vec<&[u8]>, ScanReport) {
    let mut out = Vec::new();
    let mut report = ScanReport::default();
    let mut pos = 0usize;

    while pos < body.len() {
        if let Some((payload, next)) = try_frame(body, pos) {
            report.good_frames += 1;
            out.push(payload);
            pos = next;
            continue;
        }
        match resync(body, pos + 1) {
            Some(next) => {
                report.corrupt_frames += 1;
                pos = next;
            }
            None => {
                report.truncated_tail_bytes += (body.len() - pos) as u64;
                break;
            }
        }
    }
    (out, report)
}

// ---------------------------------------------------------------------------
// Admission
// ---------------------------------------------------------------------------

/// What the store did with a record offered to [`QuantMemoryStore::admit_record`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// A new primary key; the row was appended.
    Inserted,
    /// The primary key already existed; the row was replaced. This is what makes
    /// replaying a journal over a snapshot idempotent.
    Replaced,
    /// The table is at [`QuantMemoryStore::capacity`]. Per §57's durability-first
    /// precedence the row is **refused**, never traded for an eviction of sealed
    /// research evidence.
    RejectedCapacity,
    /// The record would have overwritten a sealed [`Experiment`] with different
    /// content. Refused (§56.1 / §56.9): a restart must not un-seal research.
    RejectedSealedImmutable,
}

/// Insert-or-replace `row` in `table`, keyed by `key`, honouring `capacity`.
fn upsert<T, K, F>(table: &mut Vec<T>, capacity: usize, row: T, key: F) -> Admission
where
    K: PartialEq,
    F: Fn(&T) -> K,
{
    let k = key(&row);
    if let Some(slot) = table.iter_mut().find(|existing| key(existing) == k) {
        *slot = row;
        return Admission::Replaced;
    }
    if table.len() >= capacity {
        return Admission::RejectedCapacity;
    }
    table.push(row);
    Admission::Inserted
}

impl QuantMemoryStore {
    /// Admit one restored or journaled record, upserting on the table's primary
    /// key and honouring the §57 capacity bound.
    ///
    /// This is the single admission path used by [`Self::restore`] and by
    /// [`DurableMemory`], so the durability layer cannot acquire a way to put a row
    /// into the store that the capacity and sealing contracts have not seen.
    pub fn admit_record(&mut self, record: MemoryRecord) -> Admission {
        let cap = self.capacity;
        match record {
            MemoryRecord::Hypothesis(row) => upsert(&mut self.hypotheses, cap, row, |r| r.id),
            MemoryRecord::Experiment(row) => {
                // §56.9: an already-sealed experiment is immutable. Replacing it
                // with anything but a byte-identical copy is refused, so no replay
                // order and no damaged tail can un-seal research.
                if let Some(existing) = self.experiments.iter().find(|e| e.id == row.id) {
                    if existing.sealed && *existing != row {
                        return Admission::RejectedSealedImmutable;
                    }
                }
                upsert(&mut self.experiments, cap, row, |r| r.id)
            }
            MemoryRecord::Result(row) => upsert(&mut self.results, cap, row, |r| r.id),
            MemoryRecord::SocialCall(row) => upsert(&mut self.social_calls, cap, row, |r| r.id),
            MemoryRecord::CallMarkout(row) => upsert(&mut self.call_markouts, cap, row, |r| r.id),
            MemoryRecord::SourceQuality(row) => {
                upsert(&mut self.source_quality_ledger, cap, row, |r| r.source_id)
            }
            MemoryRecord::AmplificationEdge(row) => {
                upsert(&mut self.amplification_edges, cap, row, |r| r.id)
            }
            MemoryRecord::MetaCategory(row) => {
                upsert(&mut self.meta_categories, cap, row, |r| r.id)
            }
            MemoryRecord::CategoryAssignment(row) => {
                upsert(&mut self.category_assignments, cap, row, |r| r.id)
            }
            MemoryRecord::MetaRotationSnapshot(row) => {
                upsert(&mut self.meta_rotation_snapshots, cap, row, |r| r.id)
            }
        }
    }

    /// Every row of every table, in [`crate::schema::ALL_TABLES`] order and, within
    /// a table, in insertion order. The snapshot serialisation order (§22 stable
    /// iteration).
    #[must_use]
    pub fn iter_records(&self) -> Vec<MemoryRecord> {
        let mut out = Vec::with_capacity(self.row_count());
        out.extend(self.hypotheses.iter().cloned().map(MemoryRecord::from));
        out.extend(self.experiments.iter().cloned().map(MemoryRecord::from));
        out.extend(self.results.iter().cloned().map(MemoryRecord::from));
        out.extend(self.social_calls.iter().cloned().map(MemoryRecord::from));
        out.extend(self.call_markouts.iter().cloned().map(MemoryRecord::from));
        out.extend(
            self.source_quality_ledger
                .iter()
                .cloned()
                .map(MemoryRecord::from),
        );
        out.extend(
            self.amplification_edges
                .iter()
                .cloned()
                .map(MemoryRecord::from),
        );
        out.extend(self.meta_categories.iter().cloned().map(MemoryRecord::from));
        out.extend(
            self.category_assignments
                .iter()
                .cloned()
                .map(MemoryRecord::from),
        );
        out.extend(
            self.meta_rotation_snapshots
                .iter()
                .cloned()
                .map(MemoryRecord::from),
        );
        out
    }

    /// Total rows held across all ten tables.
    #[must_use]
    pub fn row_count(&self) -> usize {
        self.hypotheses.len()
            + self.experiments.len()
            + self.results.len()
            + self.social_calls.len()
            + self.call_markouts.len()
            + self.source_quality_ledger.len()
            + self.amplification_edges.len()
            + self.meta_categories.len()
            + self.category_assignments.len()
            + self.meta_rotation_snapshots.len()
    }
}

// ---------------------------------------------------------------------------
// Restore report
// ---------------------------------------------------------------------------

/// What happened during a restore. Damage is **reported**, never raised: a bad
/// sector must cost the rows it touched, not the whole research memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestoreReport {
    /// Rows admitted from the snapshot (new primary keys).
    pub snapshot_admitted: u64,
    /// Rows admitted from the journal tail (new primary keys).
    pub journal_admitted: u64,
    /// Rows whose primary key already existed and were replaced. Expected and
    /// harmless — this is what makes replaying a journal over a snapshot
    /// idempotent.
    pub replaced: u64,
    /// Rows refused because their table was at capacity (§57 durability-first).
    /// Reported, never silently dropped.
    pub rejected_capacity: u64,
    /// Per-table breakdown of [`Self::rejected_capacity`], indexed by
    /// [`TableTag::index`].
    pub rejected_capacity_by_table: [u64; TABLE_COUNT],
    /// Records refused because they declare a schema version newer than this build
    /// (§56.9 fail-closed).
    pub refused_newer_schema: u64,
    /// Records refused because they would have overwritten a sealed experiment
    /// (§56.1).
    pub refused_sealed_immutable: u64,
    /// Frames whose checksum failed or whose payload would not decode.
    pub corrupt_records_skipped: u64,
    /// Bytes discarded from truncated tails across both blobs.
    pub truncated_tail_bytes: u64,
    /// Capacity the restored store was built with.
    pub capacity: u64,
}

impl Default for RestoreReport {
    fn default() -> Self {
        Self {
            snapshot_admitted: 0,
            journal_admitted: 0,
            replaced: 0,
            rejected_capacity: 0,
            rejected_capacity_by_table: [0; TABLE_COUNT],
            refused_newer_schema: 0,
            refused_sealed_immutable: 0,
            corrupt_records_skipped: 0,
            truncated_tail_bytes: 0,
            capacity: 0,
        }
    }
}

impl RestoreReport {
    /// Total rows admitted as new primary keys.
    #[must_use]
    pub const fn admitted(&self) -> u64 {
        self.snapshot_admitted + self.journal_admitted
    }

    /// Whether any byte-level damage was seen. Worth alerting on; never fatal.
    #[must_use]
    pub const fn saw_damage(&self) -> bool {
        self.corrupt_records_skipped > 0 || self.truncated_tail_bytes > 0
    }

    /// Whether any record on disk failed to reach the store for any reason —
    /// damage, capacity, schema, or sealing. The operator-facing "you lost
    /// something" flag.
    #[must_use]
    pub const fn saw_loss(&self) -> bool {
        self.saw_damage()
            || self.rejected_capacity > 0
            || self.refused_newer_schema > 0
            || self.refused_sealed_immutable > 0
    }

    fn count(&mut self, admission: Admission, tag: TableTag, from_journal: bool) {
        match admission {
            Admission::Inserted => {
                if from_journal {
                    self.journal_admitted += 1;
                } else {
                    self.snapshot_admitted += 1;
                }
            }
            Admission::Replaced => self.replaced += 1,
            Admission::RejectedCapacity => {
                self.rejected_capacity += 1;
                self.rejected_capacity_by_table[tag.index()] += 1;
            }
            Admission::RejectedSealedImmutable => self.refused_sealed_immutable += 1,
        }
    }
}

// ---------------------------------------------------------------------------
// The durable API
// ---------------------------------------------------------------------------

/// Append one record to the journal, writing the blob header first if the journal
/// is empty.
///
/// # Errors
/// Propagates I/O failures from the [`BlobStore`].
pub fn append_record<S: BlobStore>(
    store: &mut S,
    journal_path: &Path,
    record: &MemoryRecord,
) -> Result<(), PersistError> {
    let payload = encode_record(record);
    let mut bytes = Vec::with_capacity(HEADER_LEN + FRAME_HEADER_LEN + payload.len());
    if !store.exists(journal_path) || store.read_all(journal_path)?.is_empty() {
        // Journals declare no capacity; the snapshot owns that fact.
        bytes.extend_from_slice(&header(0));
    }
    bytes.extend_from_slice(&frame(&payload));
    store.append(journal_path, &bytes)?;
    Ok(())
}

impl QuantMemoryStore {
    /// Write the whole store as a snapshot blob, atomically (temp → fsync →
    /// rename), declaring the store's capacity in the header.
    ///
    /// # Errors
    /// Propagates I/O failures from the [`BlobStore`].
    pub fn snapshot<S: BlobStore>(
        &self,
        store: &mut S,
        snapshot_path: &Path,
    ) -> Result<(), PersistError> {
        let records = self.iter_records();
        let mut bytes = Vec::with_capacity(HEADER_LEN + records.len() * EXPERIMENT_WIRE_LEN);
        bytes.extend_from_slice(&header(self.capacity as u64));
        for record in &records {
            bytes.extend_from_slice(&frame(&encode_record(record)));
        }
        store.write_atomic(snapshot_path, &bytes)?;
        Ok(())
    }

    /// Rebuild a store: read the snapshot, then replay the journal tail on top.
    ///
    /// A missing snapshot is not an error — a first-ever boot has only a journal
    /// (or neither), and restore returns an **empty** store with a report saying
    /// so. It never manufactures rows.
    ///
    /// `default_capacity` is used when the snapshot declares none (a
    /// journal-only boot); a snapshot's declared capacity wins, so the §57 bound
    /// the store was running under is the bound it comes back under.
    ///
    /// # Errors
    /// [`PersistError::BadMagic`] or [`PersistError::UnsupportedFormat`] if a
    /// present blob is not a readable quant-memory store; I/O failures from the
    /// [`BlobStore`].
    pub fn restore<S: BlobStore>(
        store: &S,
        snapshot_path: &Path,
        journal_path: &Path,
        default_capacity: usize,
    ) -> Result<(QuantMemoryStore, RestoreReport), PersistError> {
        let mut report = RestoreReport::default();

        let snap_bytes = store.read_all(snapshot_path)?;
        let capacity = if snap_bytes.is_empty() {
            default_capacity
        } else {
            let declared = parse_header(&snap_bytes)?;
            if declared == 0 {
                default_capacity
            } else {
                usize::try_from(declared).unwrap_or(default_capacity)
            }
        };
        report.capacity = capacity as u64;
        let mut memory = QuantMemoryStore::new(capacity);

        if !snap_bytes.is_empty() {
            replay(&mut memory, &snap_bytes[HEADER_LEN..], &mut report, false);
        }

        let jnl_bytes = store.read_all(journal_path)?;
        if !jnl_bytes.is_empty() {
            parse_header(&jnl_bytes)?;
            replay(&mut memory, &jnl_bytes[HEADER_LEN..], &mut report, true);
        }

        Ok((memory, report))
    }

    /// Flush a full snapshot through a [`PersistenceSink`].
    ///
    /// This is the wired seam the crate used to declare and never call. It exists
    /// so an operator can hand the crate any sink — a [`BlobSink`] over the local
    /// filesystem, or a server-side one — without the store learning about paths.
    ///
    /// # Errors
    /// Whatever the sink reports.
    pub fn flush<K: PersistenceSink>(&self, sink: &mut K) -> Result<(), PersistError> {
        sink.flush_snapshot(self)
    }
}

/// Scan `body` and admit every decodable record into `memory`, accumulating the
/// report.
fn replay(
    memory: &mut QuantMemoryStore,
    body: &[u8],
    report: &mut RestoreReport,
    from_journal: bool,
) {
    let (payloads, scan) = scan_frames(body);
    report.corrupt_records_skipped += scan.corrupt_frames;
    report.truncated_tail_bytes += scan.truncated_tail_bytes;
    for payload in payloads {
        match decode_record(payload) {
            Ok(record) => {
                let tag = record.tag();
                let admission = memory.admit_record(record);
                report.count(admission, tag, from_journal);
            }
            Err(DecodeOutcome::NewerSchema { .. }) => report.refused_newer_schema += 1,
            Err(DecodeOutcome::Malformed) => report.corrupt_records_skipped += 1,
        }
    }
}

// ---------------------------------------------------------------------------
// PersistenceSink implementations
// ---------------------------------------------------------------------------

/// A [`PersistenceSink`] that writes snapshots to a [`BlobStore`].
///
/// The concrete answer to "where does `flush_snapshot` put the bytes": a local
/// blob, atomically, with no third-party database anywhere in the path.
#[derive(Debug, Clone)]
pub struct BlobSink<S: BlobStore> {
    store: S,
    snapshot_path: PathBuf,
    flushes: u64,
}

impl<S: BlobStore> BlobSink<S> {
    /// A sink writing snapshots to `snapshot_path` inside `store`.
    pub fn new(store: S, snapshot_path: impl Into<PathBuf>) -> Self {
        Self {
            store,
            snapshot_path: snapshot_path.into(),
            flushes: 0,
        }
    }

    /// Number of successful flushes — the counter that proves the seam is wired.
    #[must_use]
    pub const fn flushes(&self) -> u64 {
        self.flushes
    }

    /// Consume the sink and return the underlying blob store (test/ops hook).
    #[must_use]
    pub fn into_blob_store(self) -> S {
        self.store
    }
}

impl<S: BlobStore> PersistenceSink for BlobSink<S> {
    fn flush_snapshot(&mut self, store: &QuantMemoryStore) -> Result<(), PersistError> {
        store.snapshot(&mut self.store, &self.snapshot_path)?;
        self.flushes += 1;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// DurableMemory
// ---------------------------------------------------------------------------

/// A [`QuantMemoryStore`] wired to durable local storage: every admitted row is
/// journaled as it lands, and the whole store can be snapshotted to collapse the
/// journal tail.
///
/// This is the "survives restart" object. The store inside is the same pure
/// structure, so nothing that reads research memory pays for durability.
#[derive(Debug)]
pub struct DurableMemory<S: BlobStore> {
    store: S,
    snapshot_path: PathBuf,
    journal_path: PathBuf,
    memory: QuantMemoryStore,
    journaled_since_snapshot: u64,
}

impl<S: BlobStore> DurableMemory<S> {
    /// Open (or create) a durable store at the given paths, restoring whatever is
    /// there.
    ///
    /// # Errors
    /// Propagates [`QuantMemoryStore::restore`] failures.
    pub fn open(
        store: S,
        snapshot_path: impl Into<PathBuf>,
        journal_path: impl Into<PathBuf>,
        capacity: usize,
    ) -> Result<(Self, RestoreReport), PersistError> {
        let snapshot_path = snapshot_path.into();
        let journal_path = journal_path.into();
        let (memory, report) =
            QuantMemoryStore::restore(&store, &snapshot_path, &journal_path, capacity)?;
        Ok((
            Self {
                store,
                snapshot_path,
                journal_path,
                memory,
                journaled_since_snapshot: 0,
            },
            report,
        ))
    }

    /// The live research memory — every read goes through here.
    #[must_use]
    pub const fn memory(&self) -> &QuantMemoryStore {
        &self.memory
    }

    /// Journal records written since the last snapshot; drives compaction policy.
    #[must_use]
    pub const fn journaled_since_snapshot(&self) -> u64 {
        self.journaled_since_snapshot
    }

    /// Admit a row **and** append it to the journal.
    ///
    /// The in-memory admission happens first, so a row the store refuses (table
    /// full, sealed experiment) never reaches the journal and history stays a
    /// truthful replay of what the store actually holds.
    ///
    /// # Errors
    /// Propagates I/O failures from the [`BlobStore`].
    pub fn record(&mut self, record: impl Into<MemoryRecord>) -> Result<Admission, PersistError> {
        let record = record.into();
        let admission = self.memory.admit_record(record.clone());
        if matches!(admission, Admission::Inserted | Admission::Replaced) {
            append_record(&mut self.store, &self.journal_path, &record)?;
            self.journaled_since_snapshot += 1;
        }
        Ok(admission)
    }

    /// Seal an experiment in memory and journal the sealed row, so the seal itself
    /// is durable (§56.1 / §56.9).
    ///
    /// # Errors
    /// Propagates I/O failures from the [`BlobStore`].
    pub fn seal_experiment(
        &mut self,
        id: ExperimentId,
    ) -> Result<Result<SealHash, crate::store::StoreError>, PersistError> {
        let hash = match self.memory.seal_experiment(id) {
            Ok(h) => h,
            Err(e) => return Ok(Err(e)),
        };
        let sealed = self
            .memory
            .experiment(id)
            .cloned()
            .expect("seal_experiment succeeded, so the row exists");
        append_record(
            &mut self.store,
            &self.journal_path,
            &MemoryRecord::Experiment(sealed),
        )?;
        self.journaled_since_snapshot += 1;
        Ok(Ok(hash))
    }

    /// Write a snapshot and reset the journal counter.
    ///
    /// The journal is intentionally **not** truncated: replaying it over a newer
    /// snapshot is idempotent (upsert on the primary key), so leaving it is
    /// strictly safer than deleting it. Truncation is an operator decision taken
    /// once a snapshot is known-good on disk.
    ///
    /// # Errors
    /// Propagates I/O failures from the [`BlobStore`].
    pub fn snapshot_now(&mut self) -> Result<(), PersistError> {
        self.memory.snapshot(&mut self.store, &self.snapshot_path)?;
        self.journaled_since_snapshot = 0;
        Ok(())
    }

    /// Consume the durable store and return the underlying blob store (test/ops
    /// hook).
    #[must_use]
    pub fn into_blob_store(self) -> S {
        self.store
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::ALL_TABLES;
    use crate::store::StoreError;

    fn snap_path() -> PathBuf {
        PathBuf::from("quant-memory.snap")
    }
    fn jnl_path() -> PathBuf {
        PathBuf::from("quant-memory.jnl")
    }

    fn h32(seed: u8) -> ContentHash {
        let mut out = [0u8; 32];
        let mut i = 0usize;
        while i < 32 {
            out[i] = seed.wrapping_add(i as u8).wrapping_mul(31);
            i += 1;
        }
        out
    }

    fn hyp(i: u64) -> Hypothesis {
        Hypothesis {
            id: HypothesisId(i),
            schema_version: SCHEMA_VERSION,
            statement_hash: h32(i as u8),
            expected_impact_lamports: (i as i128 % 17 - 8) * 1_000_000_000,
            prob_true_bps: (i as i64 * 137) % 10_001,
            cost_to_test_lamports: i * 1_000_000,
            edge_half_life_secs: 3_600 + i * 7,
            status: InferenceState::from_ordinal((i % 7) as u8).expect("in range"),
        }
    }

    fn exp(i: u64) -> Experiment {
        Experiment {
            id: ExperimentId(i),
            hypothesis_id: HypothesisId(i * 2),
            schema_version: SCHEMA_VERSION,
            title_hash: h32(i as u8 ^ 0x11),
            causal_mechanism_hash: h32(i as u8 ^ 0x22),
            dataset_hash: h32(i as u8 ^ 0x33),
            config_hash: i.wrapping_mul(0x9E37_79B9_7F4A_7C15),
            created_at_ns: i * 1_000_000_007,
            sealed: false,
            seal_hash: None,
        }
    }

    fn res(i: u64) -> ExperimentResult {
        ExperimentResult {
            id: ResultId(i),
            experiment_id: ExperimentId(i),
            net_sol_effect_lamports: (i as i128 % 11 - 5) * 777_000_000,
            significance_bps: (i as i64 * 991) % 10_001,
            outcome: InferenceState::from_ordinal((i % 7) as u8).expect("in range"),
            reconciled_at_ns: i * 3,
        }
    }

    fn call(i: u64) -> SocialCall {
        SocialCall {
            id: SocialCallId(i),
            source_id: SourceId(i % 13),
            token_hash: h32(i as u8 ^ 0x44),
            captured_at_ns: i * 5,
            content_hash: h32(i as u8 ^ 0x55),
            timing: LifecycleTiming::from_ordinal((i % 4) as u8).expect("in range"),
        }
    }

    fn markout(i: u64) -> CallMarkout {
        CallMarkout {
            id: MarkoutId(i),
            call_id: SocialCallId(i / 4),
            horizon: MarkoutHorizon::from_ordinal((i % 4) as u8).expect("in range"),
            executable_return_bps: (i as i64 % 23 - 11) * 137,
        }
    }

    fn quality(i: u64) -> SourceQualityEntry {
        SourceQualityEntry {
            source_id: SourceId(i),
            classification: SourceClassification::from_ordinal((i % 8) as u8).expect("in range"),
            confidence_bps: (i as i64 * 313) % 10_001,
            sample_size: (i as u32).wrapping_mul(3),
            mean_markout_30m_bps: (i as i64 % 19 - 9) * 41,
            updated_at_ns: i * 11,
        }
    }

    fn edge(i: u64) -> AmplificationEdge {
        AmplificationEdge {
            id: EdgeId(i),
            from_source: SourceId(i % 7),
            to_source: SourceId(i % 5),
            token_hash: h32(i as u8 ^ 0x66),
            observed_at_ns: i * 13,
            kind: EdgeKind::from_ordinal((i % 4) as u8).expect("in range"),
        }
    }

    fn category(i: u64) -> MetaCategory {
        MetaCategory {
            id: MetaCategoryId(i),
            name_hash: h32(i as u8 ^ 0x77),
            lifecycle: MetaLifecycle::from_ordinal((i % 4) as u8).expect("in range"),
            updated_at_ns: i * 17,
        }
    }

    fn assignment(i: u64) -> CategoryAssignment {
        CategoryAssignment {
            id: AssignmentId(i),
            category_id: MetaCategoryId(i % 6),
            token_hash: h32(i as u8 ^ 0x88),
            confidence_bps: (i as i64 * 701) % 10_001,
            assigned_at_ns: i * 19,
        }
    }

    fn rotation(i: u64) -> MetaRotationSnapshot {
        MetaRotationSnapshot {
            id: SnapshotId(i),
            category_id: MetaCategoryId(i % 6),
            taken_at_ns: i * 23,
            lifecycle: MetaLifecycle::from_ordinal((i % 4) as u8).expect("in range"),
            launch_share_bps: (i as i64 * 449) % 10_001,
        }
    }

    /// One record of every table for index `i` — the fixture the round-trip tests
    /// sweep.
    fn every_record(i: u64) -> Vec<MemoryRecord> {
        vec![
            hyp(i).into(),
            exp(i).into(),
            res(i).into(),
            call(i).into(),
            markout(i).into(),
            quality(i).into(),
            edge(i).into(),
            category(i).into(),
            assignment(i).into(),
            rotation(i).into(),
        ]
    }

    fn build_store(n: u64, capacity: usize) -> QuantMemoryStore {
        let mut s = QuantMemoryStore::new(capacity);
        for i in 1..=n {
            for record in every_record(i) {
                assert_eq!(s.admit_record(record), Admission::Inserted);
            }
        }
        s
    }

    // ----------------------------------------------------------- encoding (1-4)

    #[test]
    fn every_row_type_round_trips_byte_identically() {
        for i in 0..200u64 {
            for record in every_record(i) {
                let bytes = encode_record(&record);
                assert_eq!(
                    bytes.len(),
                    record.tag().wire_len(),
                    "declared wire length wrong for {:?}",
                    record.tag()
                );
                let decoded = decode_record(&bytes).expect("valid record");
                assert_eq!(decoded, record, "round trip failed for i={i}");
                // And re-encoding the decoded value reproduces the same bytes.
                assert_eq!(encode_record(&decoded), bytes);
            }
        }
    }

    #[test]
    fn all_ten_tables_are_covered_and_tags_match_schema_order() {
        assert_eq!(TableTag::ALL.len(), TABLE_COUNT);
        assert_eq!(ALL_TABLES.len(), TABLE_COUNT);
        for (i, tag) in TableTag::ALL.iter().enumerate() {
            assert_eq!(tag.ordinal() as usize, i);
            assert_eq!(TableTag::from_ordinal(tag.ordinal()), Some(*tag));
            assert_eq!(tag.index(), i);
        }
        // Every record variant maps to a distinct tag, and all ten are hit.
        let mut seen = [false; TABLE_COUNT];
        for record in every_record(1) {
            seen[record.tag().index()] = true;
        }
        assert!(
            seen.iter().all(|s| *s),
            "a table has no MemoryRecord variant"
        );
        assert!(TableTag::from_ordinal(TABLE_COUNT as u8).is_none());
    }

    #[test]
    fn encoding_is_byte_stable_and_length_sensitive() {
        for record in every_record(9) {
            let bytes = encode_record(&record);
            for _ in 0..16 {
                assert_eq!(encode_record(&record), bytes);
            }
            assert_eq!(
                decode_record(&bytes[..bytes.len() - 1]),
                Err(DecodeOutcome::Malformed)
            );
            let mut long = bytes.clone();
            long.push(0);
            assert_eq!(decode_record(&long), Err(DecodeOutcome::Malformed));
        }
        assert_eq!(decode_record(&[]), Err(DecodeOutcome::Malformed));
    }

    #[test]
    fn decode_refuses_unknown_tags_and_unknown_enum_ordinals() {
        let mut bytes = encode_record(&hyp(3).into());
        bytes[0] = 200; // unknown table tag
        assert_eq!(decode_record(&bytes), Err(DecodeOutcome::Malformed));

        // The status ordinal is the final byte of a hypothesis payload.
        let mut bytes = encode_record(&hyp(3).into());
        let last = bytes.len() - 1;
        bytes[last] = 250;
        assert_eq!(decode_record(&bytes), Err(DecodeOutcome::Malformed));

        // The horizon ordinal of a markout.
        let mut bytes = encode_record(&markout(3).into());
        bytes[RECORD_ENVELOPE_LEN + 16] = 99;
        assert_eq!(decode_record(&bytes), Err(DecodeOutcome::Malformed));
    }

    // ------------------------------------------------------- schema fence (5-6)

    #[test]
    fn a_record_from_a_newer_schema_is_refused_fail_closed() {
        let mut bytes = encode_record(&hyp(1).into());
        bytes[1..5].copy_from_slice(&(SCHEMA_VERSION + 1).to_le_bytes());
        assert_eq!(
            decode_record(&bytes),
            Err(DecodeOutcome::NewerSchema {
                found: SCHEMA_VERSION + 1,
                supported: SCHEMA_VERSION,
            })
        );
    }

    #[test]
    fn a_newer_schema_record_is_counted_and_never_admitted() {
        let mut store = MemBlobStore::new();
        // Three good rows, one forward-versioned row, three more good rows.
        for i in 1..=3u64 {
            append_record(&mut store, &jnl_path(), &hyp(i).into()).expect("append");
        }
        let mut future = encode_record(&hyp(99).into());
        future[1..5].copy_from_slice(&(SCHEMA_VERSION + 7).to_le_bytes());
        store
            .append(&jnl_path(), &frame(&future))
            .expect("append raw");
        for i in 4..=6u64 {
            append_record(&mut store, &jnl_path(), &hyp(i).into()).expect("append");
        }

        let (memory, report) =
            QuantMemoryStore::restore(&store, &snap_path(), &jnl_path(), 64).expect("restore");
        assert_eq!(report.refused_newer_schema, 1);
        assert_eq!(report.journal_admitted, 6);
        assert_eq!(
            report.corrupt_records_skipped, 0,
            "not corruption — refusal"
        );
        assert!(report.saw_loss());
        assert!(!report.saw_damage());
        assert_eq!(memory.hypotheses.len(), 6);
        assert!(
            memory.hypotheses.iter().all(|h| h.id != HypothesisId(99)),
            "unknown bytes must never be reinterpreted"
        );
    }

    // ------------------------------------------------------ header/frame (7-8)

    #[test]
    fn header_round_trips_and_rejects_foreign_blobs() {
        assert_eq!(parse_header(&header(4_096)).expect("valid"), 4_096);
        assert!(matches!(
            parse_header(b"not-a-memory-store--"),
            Err(PersistError::BadMagic)
        ));
        assert!(matches!(
            parse_header(b"HRMQ"),
            Err(PersistError::TruncatedHeader { found: 4, .. })
        ));
        let mut bad = header(1);
        bad[8] = 99;
        assert!(matches!(
            parse_header(&bad),
            Err(PersistError::UnsupportedFormat {
                found: 99,
                expected: FORMAT_VERSION
            })
        ));
    }

    #[test]
    fn frame_carries_length_and_checksum_and_empty_bodies_scan_clean() {
        let payload = b"hermes-quant-memory";
        let f = frame(payload);
        assert_eq!(f.len(), FRAME_HEADER_LEN + payload.len());
        let (payloads, report) = scan_frames(&f);
        assert_eq!(payloads, vec![&payload[..]]);
        assert_eq!(report.good_frames, 1);
        assert_eq!(report.corrupt_frames, 0);
        assert_eq!(report.truncated_tail_bytes, 0);
        assert_eq!(scan_frames(&[]), (Vec::new(), ScanReport::default()));
    }

    // -------------------------------------------------- snapshot/restore (9-12)

    #[test]
    fn a_full_store_snapshots_and_restores_every_table_identically() {
        let original = build_store(40, 128);
        let mut blobs = MemBlobStore::new();
        original
            .snapshot(&mut blobs, &snap_path())
            .expect("snapshot");

        let (restored, report) =
            QuantMemoryStore::restore(&blobs, &snap_path(), &jnl_path(), 4).expect("restore");
        assert_eq!(report.snapshot_admitted, 400);
        assert_eq!(report.journal_admitted, 0);
        assert!(!report.saw_loss());
        assert_eq!(report.capacity, 128, "the snapshot's capacity must win");

        assert_eq!(restored.capacity, original.capacity);
        assert_eq!(restored.hypotheses, original.hypotheses);
        assert_eq!(restored.experiments, original.experiments);
        assert_eq!(restored.results, original.results);
        assert_eq!(restored.social_calls, original.social_calls);
        assert_eq!(restored.call_markouts, original.call_markouts);
        assert_eq!(
            restored.source_quality_ledger,
            original.source_quality_ledger
        );
        assert_eq!(restored.amplification_edges, original.amplification_edges);
        assert_eq!(restored.meta_categories, original.meta_categories);
        assert_eq!(restored.category_assignments, original.category_assignments);
        assert_eq!(
            restored.meta_rotation_snapshots,
            original.meta_rotation_snapshots
        );
        assert_eq!(restored.iter_records(), original.iter_records());
    }

    #[test]
    fn a_restored_store_behaves_identically_not_just_looks_identical() {
        let original = build_store(60, 256);
        let mut blobs = MemBlobStore::new();
        original
            .snapshot(&mut blobs, &snap_path())
            .expect("snapshot");
        let (restored, _) =
            QuantMemoryStore::restore(&blobs, &snap_path(), &jnl_path(), 256).expect("restore");

        // The behavioural proof: the VOI research queue is the same queue.
        let before = original.voi_queue();
        let after = restored.voi_queue();
        assert!(!before.is_empty(), "fixture must produce an open queue");
        assert_eq!(before, after, "the research queue drifted across a restart");

        // And lookups agree row for row.
        for i in 1..=60u64 {
            assert_eq!(
                original.experiment(ExperimentId(i)),
                restored.experiment(ExperimentId(i))
            );
        }
    }

    #[test]
    fn an_absent_store_restores_empty_and_never_manufactures_rows() {
        let blobs = MemBlobStore::new();
        let (memory, report) =
            QuantMemoryStore::restore(&blobs, &snap_path(), &jnl_path(), 64).expect("restore");
        assert_eq!(memory.row_count(), 0);
        assert_eq!(memory.capacity, 64);
        assert_eq!(report.admitted(), 0);
        assert_eq!(report.capacity, 64);
        assert!(!report.saw_loss());
        assert!(memory.voi_queue().is_empty());

        // An empty (zero-byte) snapshot blob is the same story, not an error.
        let mut blobs = MemBlobStore::new();
        blobs.write_atomic(&snap_path(), &[]).expect("write");
        let (memory, report) =
            QuantMemoryStore::restore(&blobs, &snap_path(), &jnl_path(), 8).expect("restore");
        assert_eq!(memory.row_count(), 0);
        assert!(!report.saw_loss());
    }

    #[test]
    fn journal_replay_over_a_snapshot_is_idempotent() {
        let mut blobs = MemBlobStore::new();
        let mut memory = QuantMemoryStore::new(64);
        for i in 1..=10u64 {
            for record in every_record(i) {
                memory.admit_record(record.clone());
                append_record(&mut blobs, &jnl_path(), &record).expect("append");
            }
        }
        memory.snapshot(&mut blobs, &snap_path()).expect("snapshot");
        for i in 11..=14u64 {
            for record in every_record(i) {
                memory.admit_record(record.clone());
                append_record(&mut blobs, &jnl_path(), &record).expect("append");
            }
        }

        let (restored, report) =
            QuantMemoryStore::restore(&blobs, &snap_path(), &jnl_path(), 64).expect("restore");
        assert_eq!(report.snapshot_admitted, 100);
        assert_eq!(report.journal_admitted, 40);
        // The 100 overlapping journal records replaced identical rows, not duplicated.
        assert_eq!(report.replaced, 100);
        assert_eq!(restored.row_count(), 140);
        assert_eq!(restored.iter_records(), memory.iter_records());
    }

    // ------------------------------------------------------------ sealing (13-14)

    #[test]
    fn a_sealed_experiment_restores_as_sealed() {
        let mut memory = QuantMemoryStore::new(16);
        for i in 1..=5u64 {
            memory.admit_record(exp(i).into());
        }
        let hash = memory.seal_experiment(ExperimentId(3)).expect("seal");
        assert!(memory.experiment(ExperimentId(3)).expect("row").sealed);

        let mut blobs = MemBlobStore::new();
        memory.snapshot(&mut blobs, &snap_path()).expect("snapshot");
        let (restored, report) =
            QuantMemoryStore::restore(&blobs, &snap_path(), &jnl_path(), 16).expect("restore");
        assert!(!report.saw_loss());

        let e = restored.experiment(ExperimentId(3)).expect("row survives");
        assert!(e.sealed, "a restart must not un-seal research (§56.9)");
        assert_eq!(e.seal_hash, Some(hash));
        assert!(e.verify_integrity(), "the seal must still verify");
        // The others are still unsealed, and still mutable.
        let mut restored = restored;
        assert_eq!(
            restored.update_experiment_dataset(ExperimentId(3), [9u8; 32]),
            Err(StoreError::SealedImmutable)
        );
        assert_eq!(
            restored.update_experiment_dataset(ExperimentId(4), [9u8; 32]),
            Ok(())
        );
    }

    #[test]
    fn a_replayed_unsealed_record_cannot_un_seal_a_sealed_experiment() {
        // The dangerous ordering: the snapshot holds the sealed row, and a stale
        // journal still holds the pre-seal version of it.
        let mut memory = QuantMemoryStore::new(16);
        memory.admit_record(exp(1).into());
        let mut blobs = MemBlobStore::new();
        append_record(&mut blobs, &jnl_path(), &exp(1).into()).expect("append");
        memory.seal_experiment(ExperimentId(1)).expect("seal");
        memory.snapshot(&mut blobs, &snap_path()).expect("snapshot");

        let (restored, report) =
            QuantMemoryStore::restore(&blobs, &snap_path(), &jnl_path(), 16).expect("restore");
        assert_eq!(report.refused_sealed_immutable, 1);
        assert!(restored.experiment(ExperimentId(1)).expect("row").sealed);
        assert!(report.saw_loss());
        assert!(!report.saw_damage(), "a refusal is not byte damage");

        // And a tampered sealed record does not decode at all.
        let sealed = restored.experiment(ExperimentId(1)).expect("row").clone();
        let mut bytes = encode_record(&MemoryRecord::Experiment(sealed));
        bytes[RECORD_ENVELOPE_LEN + 16] ^= 0b1000_0000; // a byte of title_hash
        assert_eq!(decode_record(&bytes), Err(DecodeOutcome::Malformed));
    }

    // ----------------------------------------------------------- capacity (15-16)

    #[test]
    fn the_capacity_bound_is_honoured_on_restore_and_rejects_are_reported() {
        // A journal declares no capacity, so the caller's bound applies (§57).
        let mut blobs = MemBlobStore::new();
        for i in 1..=10u64 {
            append_record(&mut blobs, &jnl_path(), &hyp(i).into()).expect("append");
            append_record(&mut blobs, &jnl_path(), &call(i).into()).expect("append");
        }
        let (memory, report) =
            QuantMemoryStore::restore(&blobs, &snap_path(), &jnl_path(), 4).expect("restore");

        assert_eq!(memory.capacity, 4);
        assert_eq!(memory.hypotheses.len(), 4, "never evict, refuse instead");
        assert_eq!(memory.social_calls.len(), 4);
        assert_eq!(report.journal_admitted, 8);
        assert_eq!(report.rejected_capacity, 12);
        assert_eq!(
            report.rejected_capacity_by_table[TableTag::Hypotheses.index()],
            6
        );
        assert_eq!(
            report.rejected_capacity_by_table[TableTag::SocialCalls.index()],
            6
        );
        assert!(report.saw_loss(), "refused rows must be visible");
        assert!(!report.saw_damage());
        // The rows that were admitted are the *first* ones — nothing was evicted.
        assert_eq!(memory.hypotheses[0], hyp(1));
        assert_eq!(memory.hypotheses[3], hyp(4));
    }

    #[test]
    fn admission_reports_capacity_rejection_rather_than_evicting() {
        let mut memory = QuantMemoryStore::new(2);
        assert_eq!(memory.admit_record(hyp(1).into()), Admission::Inserted);
        assert_eq!(memory.admit_record(hyp(2).into()), Admission::Inserted);
        assert_eq!(
            memory.admit_record(hyp(3).into()),
            Admission::RejectedCapacity
        );
        // An upsert of an existing key still succeeds at capacity — it adds no row.
        let mut updated = hyp(1);
        updated.prob_true_bps = 42;
        assert_eq!(
            memory.admit_record(updated.clone().into()),
            Admission::Replaced
        );
        assert_eq!(memory.hypotheses.len(), 2);
        assert_eq!(memory.hypotheses[0], updated);
        // Each table keeps its own budget.
        assert_eq!(memory.admit_record(exp(1).into()), Admission::Inserted);
    }

    // ------------------------------------------------------ crash recovery (17-20)

    #[test]
    fn truncation_at_every_byte_offset_of_the_last_record_is_survivable() {
        // The strong crash test: every possible torn-write boundary.
        let stride = FRAME_HEADER_LEN + HYPOTHESIS_WIRE_LEN;
        for cut in 1..stride {
            let mut blobs = MemBlobStore::new();
            for i in 1..=5u64 {
                append_record(&mut blobs, &jnl_path(), &hyp(i).into()).expect("append");
            }
            let full = blobs.len_of(&jnl_path());
            assert_eq!(full, HEADER_LEN + 5 * stride);
            blobs.truncate(&jnl_path(), full - cut);

            let (memory, report) = QuantMemoryStore::restore(&blobs, &snap_path(), &jnl_path(), 64)
                .expect("restore must never fail on a torn tail");
            assert_eq!(
                memory.hypotheses.len(),
                4,
                "cut={cut} must lose exactly the torn record"
            );
            assert_eq!(report.journal_admitted, 4);
            assert_eq!(report.corrupt_records_skipped, 0);
            assert_eq!(report.truncated_tail_bytes, (stride - cut) as u64);
            assert!(report.saw_damage());
            assert_eq!(memory.hypotheses[3], hyp(4));
        }
    }

    #[test]
    fn a_corrupt_middle_frame_is_skipped_and_later_records_survive() {
        let stride = FRAME_HEADER_LEN + SOCIAL_CALL_WIRE_LEN;
        let mut blobs = MemBlobStore::new();
        for i in 1..=10u64 {
            append_record(&mut blobs, &jnl_path(), &call(i).into()).expect("append");
        }
        // Flip a payload bit inside record 5 (0-indexed 4).
        let offset = HEADER_LEN + 4 * stride + FRAME_HEADER_LEN + 9;
        blobs.corrupt_byte(&jnl_path(), offset, 0b1000_0000);

        let (memory, report) =
            QuantMemoryStore::restore(&blobs, &snap_path(), &jnl_path(), 64).expect("restore");
        assert_eq!(report.corrupt_records_skipped, 1);
        assert_eq!(
            report.journal_admitted, 9,
            "the tail must survive the damage"
        );
        assert!(memory.social_calls.iter().all(|c| c.id != SocialCallId(5)));
        assert!(memory.social_calls.iter().any(|c| c.id == SocialCallId(6)));
        assert!(memory.social_calls.iter().any(|c| c.id == SocialCallId(10)));
    }

    #[test]
    fn a_damaged_length_field_resynchronises_onto_the_next_good_frame() {
        let stride = FRAME_HEADER_LEN + META_ROTATION_SNAPSHOT_WIRE_LEN;
        let mut blobs = MemBlobStore::new();
        for i in 1..=8u64 {
            append_record(&mut blobs, &jnl_path(), &rotation(i).into()).expect("append");
        }
        // Corrupt the length prefix of record 3 into something implausible.
        blobs.corrupt_byte(&jnl_path(), HEADER_LEN + 2 * stride + 3, 0xFF);
        let (memory, report) =
            QuantMemoryStore::restore(&blobs, &snap_path(), &jnl_path(), 64).expect("restore");
        assert_eq!(report.corrupt_records_skipped, 1);
        assert_eq!(report.journal_admitted, 7);
        assert!(memory
            .meta_rotation_snapshots
            .iter()
            .all(|s| s.id != SnapshotId(3)));
        assert!(memory
            .meta_rotation_snapshots
            .iter()
            .any(|s| s.id == SnapshotId(8)));
    }

    #[test]
    fn a_corrupt_snapshot_record_does_not_prevent_journal_replay() {
        let memory = build_store(6, 64);
        let mut blobs = MemBlobStore::new();
        memory.snapshot(&mut blobs, &snap_path()).expect("snapshot");
        blobs.corrupt_byte(&snap_path(), HEADER_LEN + FRAME_HEADER_LEN + 7, 0b0000_0100);
        for i in 7..=9u64 {
            append_record(&mut blobs, &jnl_path(), &hyp(i).into()).expect("append");
        }
        let (restored, report) =
            QuantMemoryStore::restore(&blobs, &snap_path(), &jnl_path(), 64).expect("restore");
        assert_eq!(report.corrupt_records_skipped, 1);
        assert_eq!(
            report.snapshot_admitted, 59,
            "60 rows minus the damaged one"
        );
        assert_eq!(report.journal_admitted, 3);
        assert_eq!(restored.row_count(), 62);
    }

    #[test]
    fn a_truncated_header_is_reported_not_panicked() {
        let mut blobs = MemBlobStore::new();
        append_record(&mut blobs, &jnl_path(), &hyp(1).into()).expect("append");
        blobs.truncate(&jnl_path(), 5);
        let err = QuantMemoryStore::restore(&blobs, &snap_path(), &jnl_path(), 64)
            .expect_err("an unusable header is a real error");
        assert!(matches!(
            err,
            PersistError::TruncatedHeader { found: 5, .. }
        ));
        assert!(format!("{err}").contains("truncated"));
    }

    // -------------------------------------------------------- wired sink (22-24)

    #[test]
    fn the_persistence_sink_is_actually_called_and_writes_bytes() {
        let memory = build_store(12, 64);
        let mut sink = BlobSink::new(MemBlobStore::new(), snap_path());
        assert_eq!(sink.flushes(), 0);
        memory.flush(&mut sink).expect("flush");
        memory.flush(&mut sink).expect("flush again");
        assert_eq!(sink.flushes(), 2, "the declared seam must be wired");

        let blobs = sink.into_blob_store();
        assert!(blobs.exists(&snap_path()));
        let (restored, report) =
            QuantMemoryStore::restore(&blobs, &snap_path(), &jnl_path(), 64).expect("restore");
        assert_eq!(report.snapshot_admitted, 120);
        assert_eq!(restored.iter_records(), memory.iter_records());
    }

    #[test]
    fn durable_memory_survives_a_simulated_restart() {
        let mut blobs = MemBlobStore::new();
        {
            let (mut durable, report) =
                DurableMemory::open(blobs, snap_path(), jnl_path(), 256).expect("open");
            assert_eq!(report.admitted(), 0);
            for i in 1..=20u64 {
                for record in every_record(i) {
                    assert_eq!(durable.record(record).expect("record"), Admission::Inserted);
                }
            }
            assert_eq!(durable.journaled_since_snapshot(), 200);
            durable.snapshot_now().expect("snapshot");
            assert_eq!(durable.journaled_since_snapshot(), 0);
            for i in 21..=25u64 {
                for record in every_record(i) {
                    durable.record(record).expect("record");
                }
            }
            durable
                .seal_experiment(ExperimentId(7))
                .expect("io")
                .expect("seal");
            blobs = durable.into_blob_store();
        }
        // "Restart": a brand-new DurableMemory over the same bytes.
        let (durable, report) =
            DurableMemory::open(blobs, snap_path(), jnl_path(), 256).expect("reopen");
        assert_eq!(report.snapshot_admitted, 200);
        assert_eq!(report.journal_admitted, 50);
        assert_eq!(durable.memory().row_count(), 250);
        let e = durable
            .memory()
            .experiment(ExperimentId(7))
            .expect("experiment 7");
        assert!(e.sealed, "the seal was journaled and replayed");
        assert!(e.verify_integrity());
        assert_eq!(report.refused_sealed_immutable, 0);
    }

    #[test]
    fn durable_memory_does_not_journal_a_row_the_store_refused() {
        let (mut durable, _) =
            DurableMemory::open(MemBlobStore::new(), snap_path(), jnl_path(), 1).expect("open");
        assert_eq!(durable.record(hyp(1)).expect("record"), Admission::Inserted);
        assert_eq!(
            durable.record(hyp(2)).expect("record"),
            Admission::RejectedCapacity
        );
        assert_eq!(durable.journaled_since_snapshot(), 1);
        let blobs = durable.into_blob_store();
        assert_eq!(
            blobs.len_of(&jnl_path()),
            HEADER_LEN + FRAME_HEADER_LEN + HYPOTHESIS_WIRE_LEN,
            "a refused row must not reach the journal"
        );
    }

    #[test]
    fn persist_error_displays_and_chains_without_panicking() {
        let e = PersistError::UnsupportedFormat {
            found: 9,
            expected: FORMAT_VERSION,
        };
        assert!(format!("{e}").contains('9'));
        assert!(format!("{}", PersistError::BadMagic).contains("magic"));
        let io_err = PersistError::from(io::Error::other("boom"));
        assert!(format!("{io_err}").contains("boom"));
        assert!(std::error::Error::source(&io_err).is_some());
        assert!(std::error::Error::source(&PersistError::BadMagic).is_none());
    }

    // ---------------------------------------------------- real filesystem (26)

    #[test]
    fn file_blob_store_round_trips_on_a_real_filesystem() {
        let dir = std::env::temp_dir().join("pump-quant-memory-persist-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create test dir");
        let snap = dir.join("quant-memory.snap");
        let jnl = dir.join("quant-memory.jnl");

        let mut memory = build_store(15, 64);
        memory.seal_experiment(ExperimentId(2)).expect("seal");
        let mut blobs = FileBlobStore;
        memory.snapshot(&mut blobs, &snap).expect("snapshot");
        for i in 16..=18u64 {
            append_record(&mut blobs, &jnl, &hyp(i).into()).expect("append");
        }
        assert!(blobs.exists(&snap));
        assert!(
            !FileBlobStore::tmp_path(&snap).exists(),
            "the temp file must not survive an atomic write"
        );

        let (restored, report) =
            QuantMemoryStore::restore(&blobs, &snap, &jnl, 64).expect("restore");
        assert_eq!(report.snapshot_admitted, 150);
        assert_eq!(report.journal_admitted, 3);
        assert!(!report.saw_loss());
        assert!(
            restored
                .experiment(ExperimentId(2))
                .expect("row")
                .verify_integrity(),
            "the seal survived a real filesystem round trip"
        );
        assert_eq!(restored.experiments, memory.experiments);

        fs::remove_dir_all(&dir).expect("cleanup");
    }
}
