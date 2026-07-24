//! Durable local storage for the episodic brain — pure `std`, no database, no
//! dependencies, crash-safe (constitution 22, 57).
//!
//! Everything else in this crate is a pure function of its inputs. This module is
//! the single, explicit, isolated exception where I/O happens, and it is fenced
//! behind the [`BlobStore`] trait so the rest of the crate stays testable without
//! touching a filesystem. [`MemBlobStore`] is a full in-memory implementation, so
//! every persistence property below is proven in a test that never opens a file.
//!
//! # Why this module exists
//!
//! A memory that dies on restart is not a memory. Before this, everything the bot
//! learned about what setups pay evaporated on every deploy — which meant the
//! system was permanently a beginner, and every restart silently reset the sample
//! counts that the fail-closed guards depend on.
//!
//! # Format
//!
//! Two files, both little-endian integers throughout — no text, no schema
//! negotiation, no parser to get wrong.
//!
//! ```text
//! header (20 bytes)   MAGIC[8] | format_version u32 | capacity u64
//! frame  (12 + N)     payload_len u32 | fnv1a_64(payload) u64 | payload[N]
//! ```
//!
//! * **Journal** (`*.jnl`) — append-only. One frame per episode, written at the
//!   moment the episode is sealed. Appending is O(1) and never rewrites or reorders
//!   history, which is the same immutability contract [`crate::episode`] enforces
//!   in memory.
//! * **Snapshot** (`*.snap`) — the whole bounded index, oldest-first, written
//!   temp-then-rename so a crash mid-write can never leave a half-snapshot in
//!   place. The temp file is `fsync`ed before the rename.
//!
//! Restore = read the snapshot, then replay the journal tail on top. Because the
//! index rejects non-monotone `episode_id`s, replaying a journal that overlaps the
//! snapshot is safe and idempotent: the overlapping records are simply refused and
//! counted in [`RestoreReport::rejected_stale`].
//!
//! # Crash recovery
//!
//! Every frame carries its own length and its own FNV-1a checksum, so a torn write
//! is *locally* detectable:
//!
//! * **Truncated tail** — fewer bytes remain than the frame declares. The tail is
//!   discarded and counted in [`RestoreReport::truncated_tail_bytes`]. This is the
//!   normal crash case: the process died mid-`write`.
//! * **Corrupt frame** — length is intact but the checksum fails, or the length is
//!   implausible. The reader resynchronises by skipping one full record stride and
//!   continues, counting [`RestoreReport::corrupt_records_skipped`]. One bad sector
//!   costs one episode, not the whole history.
//!
//! In neither case does a damaged record enter the index, and in neither case does
//! restore fail. A crash during a write must not poison the store — that is the
//! whole point.
//!
//! # Signature is derived, never trusted
//!
//! The packed signature is written to disk for auditability but is **recomputed**
//! from the bucket vector on read, and a mismatch rejects the record. That makes
//! silent encoding drift between builds impossible to load.

use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::episode::{
    DiscoveryLane, Episode, EpisodeContext, EpisodeOutcome, ExitReason, EPISODE_SCHEMA_VERSION,
};
use crate::fingerprint::{SetupFingerprint, VenuePhase, FIELD_COUNT};
use crate::hash::fnv1a_64;
use crate::recall::{EpisodicIndex, IndexError};

/// File magic identifying a Hermes brain store (constitution 102).
pub const MAGIC: [u8; 8] = *b"HRMBRAIN";

/// On-disk format version. Bumped on any framing or record-layout change.
pub const FORMAT_VERSION: u32 = 1;

/// Header length in bytes: magic + format version + capacity.
pub const HEADER_LEN: usize = 8 + 4 + 8;

/// Frame preamble length in bytes: payload length + checksum.
pub const FRAME_HEADER_LEN: usize = 4 + 8;

/// Fixed serialized length of one [`Episode`] payload (constitution 102).
pub const EPISODE_WIRE_LEN: usize = 118;

/// Largest payload the reader will believe. Anything larger is treated as
/// corruption rather than as an allocation instruction — a length field is
/// attacker-shaped even when there is no attacker.
pub const MAX_RECORD_LEN: usize = 4_096;

/// Full stride of one well-formed episode frame; used to resynchronise after a
/// corrupt frame.
pub const RECORD_STRIDE: usize = FRAME_HEADER_LEN + EPISODE_WIRE_LEN;

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

    fn tmp_path(path: &Path) -> PathBuf {
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

/// In-memory [`BlobStore`] for pure tests and for replay harnesses.
///
/// Backed by a `Vec` of `(path, bytes)` pairs rather than a hash map so iteration
/// order is insertion order — deterministic, like everything else here.
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

    /// Flip one byte of a blob — the test hook that simulates bit rot.
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
#[derive(Debug)]
pub enum PersistError {
    /// Underlying I/O failure.
    Io(io::Error),
    /// The file does not begin with [`MAGIC`] — wrong file, not a corrupt one.
    BadMagic,
    /// The file's [`FORMAT_VERSION`] is not one this build can read.
    UnsupportedFormat {
        /// Version found in the header.
        found: u32,
        /// Version this build writes.
        expected: u32,
    },
    /// A header was present but truncated.
    TruncatedHeader {
        /// Bytes actually present.
        found: usize,
        /// Bytes a header requires.
        expected: usize,
    },
    /// The index refused a decoded record for a reason that is not corruption.
    Index(IndexError),
}

impl fmt::Display for PersistError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "brain store io error: {e}"),
            Self::BadMagic => write!(f, "brain store magic mismatch"),
            Self::UnsupportedFormat { found, expected } => {
                write!(
                    f,
                    "brain store format {found} is not readable by build format {expected}"
                )
            }
            Self::TruncatedHeader { found, expected } => {
                write!(
                    f,
                    "brain store header truncated: {found} of {expected} bytes"
                )
            }
            Self::Index(e) => write!(f, "brain store index rejected a record: {e:?}"),
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
// Encoding
// ---------------------------------------------------------------------------

/// Serialize one episode into its fixed-width little-endian payload.
#[must_use]
pub fn encode_episode(e: &Episode) -> Vec<u8> {
    let mut b = Vec::with_capacity(EPISODE_WIRE_LEN);
    b.extend_from_slice(&e.schema_version().to_le_bytes());
    b.extend_from_slice(&e.episode_id().to_le_bytes());
    b.extend_from_slice(&e.fingerprint().signature().to_le_bytes());
    b.extend_from_slice(e.fingerprint().buckets());
    let ctx = e.context();
    b.extend_from_slice(&ctx.mint_id.to_le_bytes());
    b.push(ctx.venue_phase.ordinal());
    b.extend_from_slice(&ctx.meta_category_id.to_le_bytes());
    b.push(ctx.discovery_lane.ordinal());
    b.extend_from_slice(&ctx.info_time_ns.to_le_bytes());
    b.extend_from_slice(&ctx.slot.to_le_bytes());
    let out = e.outcome();
    b.extend_from_slice(&out.realized_net_lamports.to_le_bytes());
    b.extend_from_slice(&out.hold_duration_ns.to_le_bytes());
    b.push(out.exit_reason.ordinal());
    b.extend_from_slice(&out.mfe_bps.to_le_bytes());
    b.extend_from_slice(&out.mae_bps.to_le_bytes());
    b.push(u8::from(out.was_admitted));
    debug_assert_eq!(b.len(), EPISODE_WIRE_LEN);
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

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
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

    fn u128(&mut self) -> Option<u128> {
        Some(u128::from_le_bytes(self.take(16)?.try_into().ok()?))
    }

    fn i128(&mut self) -> Option<i128> {
        Some(i128::from_le_bytes(self.take(16)?.try_into().ok()?))
    }
}

/// Deserialize one episode payload.
///
/// Returns `None` for anything malformed: wrong length, unknown enum ordinal,
/// foreign schema version, or a stored signature that disagrees with the signature
/// recomputed from the bucket vector. A `None` here is counted as a corrupt record
/// and skipped — never promoted into the index.
#[must_use]
pub fn decode_episode(payload: &[u8]) -> Option<Episode> {
    if payload.len() != EPISODE_WIRE_LEN {
        return None;
    }
    let mut r = Reader::new(payload);
    let schema_version = r.u16()?;
    if schema_version != EPISODE_SCHEMA_VERSION {
        return None;
    }
    let episode_id = r.u64()?;
    let stored_signature = r.u128()?;
    let mut buckets = [0u8; FIELD_COUNT];
    buckets.copy_from_slice(r.take(FIELD_COUNT)?);
    let fingerprint = SetupFingerprint::from_buckets(buckets);
    // The signature is derived on read; a disagreement means encoding drift.
    if fingerprint.signature() != stored_signature {
        return None;
    }

    let mint_id = r.u64()?;
    let venue_phase = VenuePhase::from_ordinal(r.u8()?)?;
    let meta_category_id = r.u32()?;
    let discovery_lane = DiscoveryLane::from_ordinal(r.u8()?)?;
    let info_time_ns = r.u64()?;
    let slot = r.u64()?;

    let realized_net_lamports = r.i128()?;
    let hold_duration_ns = r.u64()?;
    let exit_reason = ExitReason::from_ordinal(r.u8()?)?;
    let mfe_bps = r.i64()?;
    let mae_bps = r.i64()?;
    let was_admitted = match r.u8()? {
        0 => false,
        1 => true,
        _ => return None,
    };

    Some(Episode::with_schema_version(
        schema_version,
        episode_id,
        fingerprint,
        EpisodeContext {
            mint_id,
            venue_phase,
            meta_category_id,
            discovery_lane,
            info_time_ns,
            slot,
        },
        EpisodeOutcome {
            realized_net_lamports,
            hold_duration_ns,
            exit_reason,
            mfe_bps,
            mae_bps,
            was_admitted,
        },
    ))
}

/// Wrap a payload in a length-prefixed, checksummed frame.
#[must_use]
pub fn frame(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(FRAME_HEADER_LEN + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&fnv1a_64(payload).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// Build a file header.
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
    let version = r.u32().ok_or(PersistError::TruncatedHeader {
        found: bytes.len(),
        expected: HEADER_LEN,
    })?;
    if version != FORMAT_VERSION {
        return Err(PersistError::UnsupportedFormat {
            found: version,
            expected: FORMAT_VERSION,
        });
    }
    let capacity = r.u64().ok_or(PersistError::TruncatedHeader {
        found: bytes.len(),
        expected: HEADER_LEN,
    })?;
    Ok(capacity)
}

/// Outcome of scanning one file's frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScanReport {
    /// Frames whose length and checksum both verified.
    pub good_frames: u64,
    /// Frames skipped because the checksum failed or the length was implausible.
    pub corrupt_frames: u64,
    /// Bytes discarded at the end of the file because a frame was cut short — the
    /// signature of a crash mid-write.
    pub truncated_tail_bytes: u64,
}

/// Scan a file body (everything after the header) into verified payload slices.
///
/// See the module docs for the truncated-tail versus corrupt-frame distinction.
#[must_use]
pub fn scan_frames(body: &[u8]) -> (Vec<&[u8]>, ScanReport) {
    let mut out = Vec::new();
    let mut report = ScanReport::default();
    let mut pos = 0usize;

    while pos < body.len() {
        let remaining = body.len() - pos;
        if remaining < FRAME_HEADER_LEN {
            report.truncated_tail_bytes += remaining as u64;
            break;
        }
        let len =
            u32::from_le_bytes([body[pos], body[pos + 1], body[pos + 2], body[pos + 3]]) as usize;
        let checksum = u64::from_le_bytes([
            body[pos + 4],
            body[pos + 5],
            body[pos + 6],
            body[pos + 7],
            body[pos + 8],
            body[pos + 9],
            body[pos + 10],
            body[pos + 11],
        ]);

        if len == 0 || len > MAX_RECORD_LEN {
            // The length field itself is damaged; resynchronise on the fixed stride.
            report.corrupt_frames += 1;
            if remaining <= RECORD_STRIDE {
                report.truncated_tail_bytes += (remaining - FRAME_HEADER_LEN.min(remaining)) as u64;
                break;
            }
            pos += RECORD_STRIDE;
            continue;
        }
        if remaining < FRAME_HEADER_LEN + len {
            report.truncated_tail_bytes += remaining as u64;
            break;
        }
        let payload = &body[pos + FRAME_HEADER_LEN..pos + FRAME_HEADER_LEN + len];
        if fnv1a_64(payload) == checksum {
            report.good_frames += 1;
            out.push(payload);
        } else {
            report.corrupt_frames += 1;
        }
        pos += FRAME_HEADER_LEN + len;
    }
    (out, report)
}

/// What happened during a restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RestoreReport {
    /// Episodes admitted from the snapshot.
    pub snapshot_admitted: u64,
    /// Episodes admitted from the journal tail.
    pub journal_admitted: u64,
    /// Frames whose checksum failed or whose payload would not decode.
    pub corrupt_records_skipped: u64,
    /// Bytes discarded from truncated tails across both files.
    pub truncated_tail_bytes: u64,
    /// Journal records refused because their `episode_id` was already covered by
    /// the snapshot. Expected and harmless — this is what makes replay idempotent.
    pub rejected_stale: u64,
    /// Capacity the restored index was built with.
    pub capacity: u64,
}

impl RestoreReport {
    /// Total episodes admitted.
    #[must_use]
    pub const fn admitted(&self) -> u64 {
        self.snapshot_admitted + self.journal_admitted
    }

    /// Whether anything at all was damaged. Worth alerting on; never fatal.
    #[must_use]
    pub const fn saw_damage(&self) -> bool {
        self.corrupt_records_skipped > 0 || self.truncated_tail_bytes > 0
    }
}

/// Append one episode to the journal.
///
/// # Errors
/// Propagates I/O failures from the [`BlobStore`].
pub fn append_episode<S: BlobStore>(
    store: &mut S,
    journal_path: &Path,
    episode: &Episode,
) -> Result<(), PersistError> {
    let mut bytes = Vec::with_capacity(HEADER_LEN + RECORD_STRIDE);
    if !store.exists(journal_path) || store.read_all(journal_path)?.is_empty() {
        bytes.extend_from_slice(&header(0));
    }
    bytes.extend_from_slice(&frame(&encode_episode(episode)));
    store.append(journal_path, &bytes)?;
    Ok(())
}

/// Write the whole index as a snapshot, atomically.
///
/// # Errors
/// Propagates I/O failures from the [`BlobStore`].
pub fn snapshot<S: BlobStore>(
    store: &mut S,
    snapshot_path: &Path,
    index: &EpisodicIndex,
) -> Result<(), PersistError> {
    let mut bytes = Vec::with_capacity(HEADER_LEN + index.len() * RECORD_STRIDE);
    bytes.extend_from_slice(&header(index.capacity() as u64));
    for e in index.iter_oldest_first() {
        bytes.extend_from_slice(&frame(&encode_episode(e)));
    }
    store.write_atomic(snapshot_path, &bytes)?;
    Ok(())
}

/// Rebuild an index: read the snapshot, then replay the journal tail on top.
///
/// A missing snapshot is not an error — a first-ever boot has only a journal (or
/// neither), and restore returns an empty index with a report saying so.
///
/// # Errors
/// [`PersistError::BadMagic`] or [`PersistError::UnsupportedFormat`] if a present
/// file is not a readable brain store; I/O failures from the [`BlobStore`].
pub fn restore<S: BlobStore>(
    store: &S,
    snapshot_path: &Path,
    journal_path: &Path,
    default_capacity: usize,
) -> Result<(EpisodicIndex, RestoreReport), PersistError> {
    let mut report = RestoreReport::default();

    let snap_bytes = store.read_all(snapshot_path)?;
    let capacity = if snap_bytes.is_empty() {
        default_capacity.max(1)
    } else {
        let declared = parse_header(&snap_bytes)?;
        if declared == 0 {
            default_capacity.max(1)
        } else {
            usize::try_from(declared).unwrap_or(default_capacity).max(1)
        }
    };
    report.capacity = capacity as u64;
    let mut index = EpisodicIndex::with_capacity(capacity);

    if !snap_bytes.is_empty() {
        let (payloads, scan) = scan_frames(&snap_bytes[HEADER_LEN..]);
        report.corrupt_records_skipped += scan.corrupt_frames;
        report.truncated_tail_bytes += scan.truncated_tail_bytes;
        for payload in payloads {
            match decode_episode(payload) {
                Some(e) => match index.push(e) {
                    Ok(_) => report.snapshot_admitted += 1,
                    Err(IndexError::NonMonotonicEpisodeId { .. }) => report.rejected_stale += 1,
                    Err(e) => return Err(PersistError::Index(e)),
                },
                None => report.corrupt_records_skipped += 1,
            }
        }
    }

    let jnl_bytes = store.read_all(journal_path)?;
    if !jnl_bytes.is_empty() {
        parse_header(&jnl_bytes)?;
        let (payloads, scan) = scan_frames(&jnl_bytes[HEADER_LEN..]);
        report.corrupt_records_skipped += scan.corrupt_frames;
        report.truncated_tail_bytes += scan.truncated_tail_bytes;
        for payload in payloads {
            match decode_episode(payload) {
                Some(e) => match index.push(e) {
                    Ok(_) => report.journal_admitted += 1,
                    Err(IndexError::NonMonotonicEpisodeId { .. }) => report.rejected_stale += 1,
                    Err(e) => return Err(PersistError::Index(e)),
                },
                None => report.corrupt_records_skipped += 1,
            }
        }
    }

    Ok((index, report))
}

// ---------------------------------------------------------------------------
// BrainStore
// ---------------------------------------------------------------------------

/// An [`EpisodicIndex`] wired to durable storage: every sealed episode is journaled
/// as it is recorded, and the whole index can be snapshotted to collapse the tail.
///
/// This is the "survives restart" object. Recall itself is untouched by it — the
/// index inside is the same pure structure, so nothing in the hot path pays for
/// durability.
#[derive(Debug)]
pub struct BrainStore<S: BlobStore> {
    store: S,
    snapshot_path: PathBuf,
    journal_path: PathBuf,
    index: EpisodicIndex,
    journaled_since_snapshot: u64,
}

impl<S: BlobStore> BrainStore<S> {
    /// Open (or create) a store at the given paths, restoring whatever is there.
    ///
    /// # Errors
    /// Propagates [`restore`] failures.
    pub fn open(
        store: S,
        snapshot_path: impl Into<PathBuf>,
        journal_path: impl Into<PathBuf>,
        capacity: usize,
    ) -> Result<(Self, RestoreReport), PersistError> {
        let snapshot_path = snapshot_path.into();
        let journal_path = journal_path.into();
        let (index, report) = restore(&store, &snapshot_path, &journal_path, capacity)?;
        Ok((
            Self {
                store,
                snapshot_path,
                journal_path,
                index,
                journaled_since_snapshot: 0,
            },
            report,
        ))
    }

    /// The live index — recall reads through here.
    #[must_use]
    pub const fn index(&self) -> &EpisodicIndex {
        &self.index
    }

    /// Journal records written since the last snapshot; drives compaction policy.
    #[must_use]
    pub const fn journaled_since_snapshot(&self) -> u64 {
        self.journaled_since_snapshot
    }

    /// Seal an episode: admit it to the index **and** append it to the journal.
    ///
    /// The index push happens first, so a record the index refuses (non-monotone
    /// id, foreign schema) never reaches the journal and history stays clean.
    ///
    /// # Errors
    /// [`PersistError::Index`] if the index refuses the episode; I/O failures from
    /// the [`BlobStore`].
    pub fn record(&mut self, episode: Episode) -> Result<Option<Episode>, PersistError> {
        let evicted = self.index.push(episode).map_err(PersistError::Index)?;
        append_episode(&mut self.store, &self.journal_path, &episode)?;
        self.journaled_since_snapshot += 1;
        Ok(evicted)
    }

    /// Write a snapshot and reset the journal counter.
    ///
    /// The journal is intentionally **not** truncated here: replaying it over a
    /// newer snapshot is idempotent (stale ids are refused), so leaving it is
    /// strictly safer than deleting it. Truncation is an operator decision, taken
    /// when a snapshot is known-good on disk.
    ///
    /// # Errors
    /// Propagates I/O failures from the [`BlobStore`].
    pub fn snapshot_now(&mut self) -> Result<(), PersistError> {
        snapshot(&mut self.store, &self.snapshot_path, &self.index)?;
        self.journaled_since_snapshot = 0;
        Ok(())
    }

    /// Consume the store and return the underlying blob store (test/ops hook).
    #[must_use]
    pub fn into_blob_store(self) -> S {
        self.store
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::episode::{DiscoveryLane, EpisodeContext, EpisodeOutcome, ExitReason};
    use crate::fingerprint::{SetupInputs, TrendStructure};
    use crate::recall::{RecallFilter, RecallParams, EPISODE_CAP};

    fn snap_path() -> PathBuf {
        PathBuf::from("brain.snap")
    }
    fn jnl_path() -> PathBuf {
        PathBuf::from("brain.jnl")
    }

    fn make_episode(i: u64) -> Episode {
        let inputs = SetupInputs {
            ofi_bps: 600 + (i as i64 % 5) * 200,
            buyer_breadth: 10 + (i as u32 % 7),
            trend_structure: TrendStructure::Up,
            venue_phase: if i.is_multiple_of(3) {
                VenuePhase::Pool
            } else {
                VenuePhase::Curve
            },
            meta_category_id: (i % 4) as u32,
            info_time_ns: i * 1_000_000_000,
            ..SetupInputs::default()
        };
        Episode::new(
            i,
            SetupFingerprint::from_inputs(&inputs),
            EpisodeContext {
                mint_id: i * 17,
                venue_phase: if i.is_multiple_of(3) {
                    VenuePhase::Pool
                } else {
                    VenuePhase::Curve
                },
                meta_category_id: (i % 4) as u32,
                discovery_lane: DiscoveryLane::from_ordinal((i % 6) as u8).expect("in range"),
                info_time_ns: i * 1_000_000_000,
                slot: i * 3,
            },
            EpisodeOutcome {
                realized_net_lamports: (i as i128 % 21 - 10) * 1_000_000,
                hold_duration_ns: i * 1_000,
                exit_reason: ExitReason::from_ordinal((i % 8) as u8).expect("in range"),
                mfe_bps: i as i64 * 3,
                mae_bps: -(i as i64),
                was_admitted: !i.is_multiple_of(5),
            },
        )
    }

    fn build_index(n: u64, capacity: usize) -> EpisodicIndex {
        let mut idx = EpisodicIndex::with_capacity(capacity);
        for i in 1..=n {
            idx.push(make_episode(i)).expect("monotone");
        }
        idx
    }

    // ------------------------------------------------------------- encoding

    #[test]
    fn episode_wire_length_is_the_declared_constant() {
        assert_eq!(encode_episode(&make_episode(1)).len(), EPISODE_WIRE_LEN);
    }

    #[test]
    fn episode_encode_decode_round_trips_exactly() {
        for i in 1..=200u64 {
            let e = make_episode(i);
            let decoded = decode_episode(&encode_episode(&e)).expect("valid record");
            assert_eq!(decoded, e, "round trip failed at i={i}");
        }
    }

    #[test]
    fn encoding_is_byte_stable_across_calls() {
        let e = make_episode(7);
        let a = encode_episode(&e);
        for _ in 0..32 {
            assert_eq!(encode_episode(&e), a);
        }
    }

    #[test]
    fn decode_rejects_wrong_length() {
        let e = encode_episode(&make_episode(1));
        assert!(decode_episode(&e[..e.len() - 1]).is_none());
        let mut long = e.clone();
        long.push(0);
        assert!(decode_episode(&long).is_none());
    }

    #[test]
    fn decode_rejects_a_signature_that_disagrees_with_its_buckets() {
        let mut bytes = encode_episode(&make_episode(3));
        // Offset 10 is the packed signature; flip a bit in it.
        bytes[10] ^= 0b0000_0001;
        assert!(
            decode_episode(&bytes).is_none(),
            "encoding drift must not load"
        );
    }

    #[test]
    fn decode_rejects_unknown_enum_ordinals() {
        let mut bytes = encode_episode(&make_episode(2));
        // Offset 59 is the discovery-lane ordinal.
        bytes[59] = 200;
        assert!(decode_episode(&bytes).is_none());
        let mut bytes = encode_episode(&make_episode(2));
        // Offset 117 is the was_admitted flag; only 0 and 1 are legal.
        bytes[117] = 7;
        assert!(decode_episode(&bytes).is_none());
    }

    #[test]
    fn header_round_trips_and_rejects_foreign_files() {
        assert_eq!(parse_header(&header(16_384)).expect("valid"), 16_384);
        assert!(matches!(
            parse_header(b"not-a-brain-file----"),
            Err(PersistError::BadMagic)
        ));
        assert!(matches!(
            parse_header(b"HRMB"),
            Err(PersistError::TruncatedHeader { .. })
        ));
        let mut bad_version = header(1);
        bad_version[8] = 99;
        assert!(matches!(
            parse_header(&bad_version),
            Err(PersistError::UnsupportedFormat {
                found: 99,
                expected: FORMAT_VERSION
            })
        ));
    }

    #[test]
    fn frame_carries_length_and_checksum() {
        let payload = b"hello-brain";
        let f = frame(payload);
        assert_eq!(f.len(), FRAME_HEADER_LEN + payload.len());
        let (payloads, report) = scan_frames(&f);
        assert_eq!(payloads, vec![&payload[..]]);
        assert_eq!(report.good_frames, 1);
        assert_eq!(report.corrupt_frames, 0);
        assert_eq!(report.truncated_tail_bytes, 0);
    }

    // ------------------------------------------------------- round trip proof

    #[test]
    fn snapshot_restore_reproduces_byte_identical_recall_verdicts() {
        let idx = build_index(400, 512);
        let mut store = MemBlobStore::new();
        snapshot(&mut store, &snap_path(), &idx).expect("snapshot");

        let (restored, report) =
            restore(&store, &snap_path(), &jnl_path(), EPISODE_CAP).expect("restore");
        assert_eq!(report.snapshot_admitted, 400);
        assert_eq!(report.journal_admitted, 0);
        assert!(!report.saw_damage());
        assert_eq!(restored.len(), idx.len());
        assert_eq!(restored.capacity(), idx.capacity());

        // The proof that matters: the brain answers identically after a restart.
        let params = RecallParams::default();
        for probe in [1u64, 37, 199, 400] {
            let q = *make_episode(probe).fingerprint();
            let before = idx.recall(&q, &params);
            let after = restored.recall(&q, &params);
            assert_eq!(
                before, after,
                "verdict drifted after restore for probe {probe}"
            );
            let f = RecallFilter::for_query(&q).with_meta_category(probe as u32 % 4);
            assert_eq!(
                idx.recall_conditioned(&q, &params, &f),
                restored.recall_conditioned(&q, &params, &f)
            );
        }
        // And the episodes themselves are identical, in order.
        let a: Vec<&Episode> = idx.iter_oldest_first().collect();
        let b: Vec<&Episode> = restored.iter_oldest_first().collect();
        assert_eq!(a, b);
    }

    #[test]
    fn journal_replay_alone_rebuilds_the_index() {
        let mut store = MemBlobStore::new();
        let mut idx = EpisodicIndex::with_capacity(256);
        for i in 1..=100u64 {
            let e = make_episode(i);
            idx.push(e).expect("monotone");
            append_episode(&mut store, &jnl_path(), &e).expect("append");
        }
        let (restored, report) = restore(&store, &snap_path(), &jnl_path(), 256).expect("restore");
        assert_eq!(report.journal_admitted, 100);
        assert_eq!(report.snapshot_admitted, 0);
        assert_eq!(restored.len(), idx.len());
        let a: Vec<&Episode> = idx.iter_oldest_first().collect();
        let b: Vec<&Episode> = restored.iter_oldest_first().collect();
        assert_eq!(a, b);
    }

    #[test]
    fn snapshot_plus_journal_tail_replays_idempotently() {
        let mut store = MemBlobStore::new();
        let mut idx = EpisodicIndex::with_capacity(256);
        // 50 episodes journaled, then snapshotted, then 20 more journaled.
        for i in 1..=50u64 {
            let e = make_episode(i);
            idx.push(e).expect("monotone");
            append_episode(&mut store, &jnl_path(), &e).expect("append");
        }
        snapshot(&mut store, &snap_path(), &idx).expect("snapshot");
        for i in 51..=70u64 {
            let e = make_episode(i);
            idx.push(e).expect("monotone");
            append_episode(&mut store, &jnl_path(), &e).expect("append");
        }

        let (restored, report) = restore(&store, &snap_path(), &jnl_path(), 256).expect("restore");
        assert_eq!(report.snapshot_admitted, 50);
        assert_eq!(report.journal_admitted, 20);
        // The 50 overlapping journal records were refused, not double-counted.
        assert_eq!(report.rejected_stale, 50);
        assert_eq!(restored.len(), 70);
        assert_eq!(restored.last_episode_id(), Some(70));
    }

    #[test]
    fn restoring_from_nothing_yields_an_empty_index_not_an_error() {
        let store = MemBlobStore::new();
        let (idx, report) = restore(&store, &snap_path(), &jnl_path(), 64).expect("restore");
        assert!(idx.is_empty());
        assert_eq!(idx.capacity(), 64);
        assert_eq!(report.admitted(), 0);
        assert!(!report.saw_damage());
    }

    #[test]
    fn snapshot_preserves_the_ring_capacity_and_eviction_state() {
        let idx = build_index(300, 64); // wrapped: only the newest 64 survive
        assert_eq!(idx.len(), 64);
        let mut store = MemBlobStore::new();
        snapshot(&mut store, &snap_path(), &idx).expect("snapshot");
        // Default capacity is deliberately different; the file's capacity must win.
        let (restored, report) =
            restore(&store, &snap_path(), &jnl_path(), EPISODE_CAP).expect("restore");
        assert_eq!(restored.capacity(), 64);
        assert_eq!(report.capacity, 64);
        assert_eq!(restored.len(), 64);
        assert_eq!(restored.last_episode_id(), Some(300));
    }

    // ----------------------------------------------------- crash recovery

    #[test]
    fn a_journal_truncated_mid_record_loses_only_the_torn_record() {
        let mut store = MemBlobStore::new();
        for i in 1..=20u64 {
            append_episode(&mut store, &jnl_path(), &make_episode(i)).expect("append");
        }
        let full_len = store.len_of(&jnl_path());
        assert_eq!(full_len, HEADER_LEN + 20 * RECORD_STRIDE);

        // Simulate a crash exactly half way through writing record 20.
        let torn = full_len - RECORD_STRIDE / 2;
        store.truncate(&jnl_path(), torn);

        let (idx, report) = restore(&store, &snap_path(), &jnl_path(), 256).expect("restore");
        assert_eq!(
            report.journal_admitted, 19,
            "only the torn record should be lost"
        );
        assert_eq!(report.truncated_tail_bytes, (RECORD_STRIDE / 2) as u64);
        assert_eq!(report.corrupt_records_skipped, 0);
        assert!(report.saw_damage());
        assert_eq!(idx.len(), 19);
        assert_eq!(idx.last_episode_id(), Some(19));
    }

    #[test]
    fn truncation_at_every_offset_of_the_last_record_is_survivable() {
        // The strong version of the crash test: every possible torn-write boundary.
        for cut in 1..RECORD_STRIDE {
            let mut store = MemBlobStore::new();
            for i in 1..=5u64 {
                append_episode(&mut store, &jnl_path(), &make_episode(i)).expect("append");
            }
            let full = store.len_of(&jnl_path());
            store.truncate(&jnl_path(), full - cut);
            let (idx, report) =
                restore(&store, &snap_path(), &jnl_path(), 64).expect("restore must not fail");
            assert_eq!(
                idx.len(),
                4,
                "cut={cut} should lose exactly the last record"
            );
            assert_eq!(report.journal_admitted, 4);
            assert_eq!(idx.last_episode_id(), Some(4));
        }
    }

    #[test]
    fn a_truncated_header_is_reported_not_panicked() {
        let mut store = MemBlobStore::new();
        append_episode(&mut store, &jnl_path(), &make_episode(1)).expect("append");
        store.truncate(&jnl_path(), 5);
        let err = restore(&store, &snap_path(), &jnl_path(), 64).expect_err("header is unusable");
        assert!(matches!(
            err,
            PersistError::TruncatedHeader { found: 5, .. }
        ));
    }

    #[test]
    fn a_corrupt_middle_record_is_skipped_and_the_reader_resynchronises() {
        let mut store = MemBlobStore::new();
        for i in 1..=10u64 {
            append_episode(&mut store, &jnl_path(), &make_episode(i)).expect("append");
        }
        // Flip a payload bit inside record 5 (0-indexed 4).
        let offset = HEADER_LEN + 4 * RECORD_STRIDE + FRAME_HEADER_LEN + 3;
        store.corrupt_byte(&jnl_path(), offset, 0b1000_0000);

        let (idx, report) = restore(&store, &snap_path(), &jnl_path(), 64).expect("restore");
        assert_eq!(report.corrupt_records_skipped, 1);
        assert_eq!(
            report.journal_admitted, 9,
            "records after the damage must survive"
        );
        assert_eq!(idx.len(), 9);
        assert_eq!(idx.last_episode_id(), Some(10));
        assert!(idx.get_by_episode_id(5).is_none());
        assert!(idx.get_by_episode_id(6).is_some());
    }

    #[test]
    fn a_damaged_length_field_resynchronises_on_the_record_stride() {
        let mut store = MemBlobStore::new();
        for i in 1..=6u64 {
            append_episode(&mut store, &jnl_path(), &make_episode(i)).expect("append");
        }
        // Corrupt the length prefix of record 3 into something implausible.
        let offset = HEADER_LEN + 2 * RECORD_STRIDE + 3;
        store.corrupt_byte(&jnl_path(), offset, 0xFF);
        let (idx, report) = restore(&store, &snap_path(), &jnl_path(), 64).expect("restore");
        assert_eq!(report.corrupt_records_skipped, 1);
        assert_eq!(report.journal_admitted, 5);
        assert!(idx.get_by_episode_id(3).is_none());
        assert!(idx.get_by_episode_id(6).is_some());
    }

    #[test]
    fn a_corrupt_snapshot_record_does_not_prevent_journal_replay() {
        let mut store = MemBlobStore::new();
        let idx = build_index(10, 64);
        snapshot(&mut store, &snap_path(), &idx).expect("snapshot");
        store.corrupt_byte(&snap_path(), HEADER_LEN + FRAME_HEADER_LEN + 2, 0b0000_0100);
        for i in 11..=15u64 {
            append_episode(&mut store, &jnl_path(), &make_episode(i)).expect("append");
        }
        let (restored, report) = restore(&store, &snap_path(), &jnl_path(), 64).expect("restore");
        assert_eq!(report.corrupt_records_skipped, 1);
        assert_eq!(report.snapshot_admitted, 9);
        assert_eq!(report.journal_admitted, 5);
        assert_eq!(restored.len(), 14);
    }

    #[test]
    fn scan_frames_handles_an_empty_body() {
        let (payloads, report) = scan_frames(&[]);
        assert!(payloads.is_empty());
        assert_eq!(report, ScanReport::default());
    }

    // --------------------------------------------------------- BrainStore

    #[test]
    fn brain_store_survives_a_simulated_restart() {
        let mut store = MemBlobStore::new();
        {
            let (mut brain, report) =
                BrainStore::open(store, snap_path(), jnl_path(), 256).expect("open");
            assert_eq!(report.admitted(), 0);
            for i in 1..=40u64 {
                brain.record(make_episode(i)).expect("record");
            }
            assert_eq!(brain.journaled_since_snapshot(), 40);
            brain.snapshot_now().expect("snapshot");
            assert_eq!(brain.journaled_since_snapshot(), 0);
            for i in 41..=60u64 {
                brain.record(make_episode(i)).expect("record");
            }
            store = brain.into_blob_store();
        }
        // "Restart": a brand-new BrainStore over the same bytes.
        let (brain, report) =
            BrainStore::open(store, snap_path(), jnl_path(), 256).expect("reopen");
        assert_eq!(report.snapshot_admitted, 40);
        assert_eq!(report.journal_admitted, 20);
        assert_eq!(report.rejected_stale, 40);
        assert_eq!(brain.index().len(), 60);
        assert_eq!(brain.index().last_episode_id(), Some(60));
    }

    #[test]
    fn brain_store_refuses_a_bad_episode_before_it_reaches_the_journal() {
        let store = MemBlobStore::new();
        let (mut brain, _) = BrainStore::open(store, snap_path(), jnl_path(), 64).expect("open");
        brain.record(make_episode(5)).expect("first");
        assert!(
            brain.record(make_episode(5)).is_err(),
            "duplicate id must be refused"
        );
        // The journal must contain exactly one record: the refused one never landed.
        let store = brain.into_blob_store();
        assert_eq!(store.len_of(&jnl_path()), HEADER_LEN + RECORD_STRIDE);
    }

    #[test]
    fn persist_error_displays_without_panicking() {
        let e = PersistError::UnsupportedFormat {
            found: 9,
            expected: FORMAT_VERSION,
        };
        assert!(format!("{e}").contains('9'));
        assert!(format!("{}", PersistError::BadMagic).contains("magic"));
        let io_err = PersistError::Io(io::Error::other("boom"));
        assert!(format!("{io_err}").contains("boom"));
        assert!(std::error::Error::source(&io_err).is_some());
    }

    // ------------------------------------------------- real filesystem path

    #[test]
    fn file_blob_store_round_trips_on_a_real_filesystem() {
        let dir = std::env::temp_dir().join("pump-quant-brain-persist-test");
        // Ignore the error if a previous run left it behind.
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create test dir");
        let snap = dir.join("brain.snap");
        let jnl = dir.join("brain.jnl");

        let idx = build_index(120, 256);
        let mut store = FileBlobStore;
        snapshot(&mut store, &snap, &idx).expect("snapshot");
        for i in 121..=130u64 {
            append_episode(&mut store, &jnl, &make_episode(i)).expect("append");
        }
        assert!(store.exists(&snap));
        // The temp file must not survive an atomic write.
        assert!(!FileBlobStore::tmp_path(&snap).exists());

        let (restored, report) = restore(&store, &snap, &jnl, 256).expect("restore");
        assert_eq!(report.snapshot_admitted, 120);
        assert_eq!(report.journal_admitted, 10);
        assert_eq!(restored.len(), 130);

        let q = *make_episode(77).fingerprint();
        let params = RecallParams::default();
        assert_eq!(idx.recall(&q, &params), {
            let mut only_snapshot = EpisodicIndex::with_capacity(256);
            for e in restored.iter_oldest_first().take(120) {
                only_snapshot.push(*e).expect("monotone");
            }
            only_snapshot.recall(&q, &params)
        });

        fs::remove_dir_all(&dir).expect("cleanup");
    }
}
