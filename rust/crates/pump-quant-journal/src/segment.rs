//! Bounded, sequence-tracked frame set that seals to a manifest index entry (§12
//! "segment checksum, ... sequence tracking, preallocated files, ... atomic
//! sealing").
//!
//! A [`Segment`] is the in-memory model of one canonical segment file: an ordered,
//! append-only run of frames sharing a schema version and connection epoch, with a
//! rolling content checksum. It is **memory-bounded** — construction fixes a maximum
//! frame count and maximum byte length, and [`Segment::append`] refuses any frame
//! that would breach either. Sealing ([`Segment::seal`]) freezes the segment and
//! emits a [`SealedSegment`] carrying the final content hash and sequence span for
//! the [`Manifest`](crate::manifest::Manifest).
//!
//! No I/O lives here (§12 separates "buffered append" from "durable seal"); the
//! encoded bytes are held in memory for a higher layer to flush.

use crate::checksum::Fnv1a64;
use crate::frame::{encoded_len, Frame, FrameError, DEFAULT_MAX_PAYLOAD_LEN};

/// Memory bounds for a [`Segment`] (§12 "preallocated files" / bounded growth).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentLimits {
    /// Maximum number of frames the segment may hold.
    pub max_frames: u32,
    /// Maximum total encoded byte length the segment may hold.
    pub max_bytes: u64,
}

impl SegmentLimits {
    /// Construct explicit limits.
    #[must_use]
    pub const fn new(max_frames: u32, max_bytes: u64) -> Self {
        Self {
            max_frames,
            max_bytes,
        }
    }
}

/// Reasons an append or seal was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentError {
    /// The segment is sealed; no further appends are allowed.
    Sealed,
    /// The frame would exceed [`SegmentLimits::max_frames`] or [`SegmentLimits::max_bytes`].
    Full,
    /// The payload is larger than the frame codec's byte bound.
    PayloadTooLarge,
    /// The next sequence number would overflow `u64`.
    SequenceOverflow,
    /// Refused to seal an empty segment (a manifest entry must cover ≥1 frame).
    EmptySegment,
    /// Underlying frame encoding failed.
    Frame(FrameError),
}

/// A bounded, append-only run of frames with a rolling content checksum.
#[derive(Debug, Clone)]
pub struct Segment {
    segment_id: u64,
    schema_version: u16,
    connection_epoch: u32,
    limits: SegmentLimits,
    buf: Vec<u8>,
    frame_count: u32,
    first_sequence: Option<u64>,
    next_sequence: u64,
    hasher: Fnv1a64,
    sealed: bool,
}

impl Segment {
    /// Create an empty segment.
    ///
    /// `start_sequence` is the sequence number the first appended frame will carry;
    /// subsequent frames increment by one, giving the contiguous sequence tracking
    /// §12 requires.
    #[must_use]
    pub fn new(
        segment_id: u64,
        schema_version: u16,
        connection_epoch: u32,
        start_sequence: u64,
        limits: SegmentLimits,
    ) -> Self {
        Self {
            segment_id,
            schema_version,
            connection_epoch,
            limits,
            buf: Vec::new(),
            frame_count: 0,
            first_sequence: None,
            next_sequence: start_sequence,
            hasher: Fnv1a64::new(),
            sealed: false,
        }
    }

    /// Segment identifier.
    #[must_use]
    pub const fn segment_id(&self) -> u64 {
        self.segment_id
    }

    /// Schema version shared by all frames in this segment.
    #[must_use]
    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }

    /// Connection epoch shared by all frames in this segment.
    #[must_use]
    pub const fn connection_epoch(&self) -> u32 {
        self.connection_epoch
    }

    /// Number of frames appended so far.
    #[must_use]
    pub const fn frame_count(&self) -> u32 {
        self.frame_count
    }

    /// Total encoded byte length held so far.
    #[must_use]
    pub fn byte_len(&self) -> u64 {
        self.buf.len() as u64
    }

    /// Whether the segment holds no frames.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.frame_count == 0
    }

    /// Whether the segment has been sealed.
    #[must_use]
    pub const fn is_sealed(&self) -> bool {
        self.sealed
    }

    /// Sequence number of the first frame, or [`None`] if empty.
    #[must_use]
    pub const fn first_sequence(&self) -> Option<u64> {
        self.first_sequence
    }

    /// Sequence number of the last frame, or [`None`] if empty.
    #[must_use]
    pub fn last_sequence(&self) -> Option<u64> {
        if self.frame_count == 0 {
            None
        } else {
            // next_sequence advanced past the last written frame; step back one.
            Some(self.next_sequence - 1)
        }
    }

    /// The encoded bytes of every appended frame, in order.
    ///
    /// This is exactly what a higher layer would flush to disk, and what
    /// [`recovery_scan`](crate::recovery::recovery_scan) consumes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.buf
    }

    /// Whether a payload of `payload_len` bytes would currently fit within the limits.
    ///
    /// Pure predicate (no mutation); overflow is treated as "does not fit".
    #[must_use]
    pub fn would_fit(&self, payload_len: usize) -> bool {
        if self.sealed {
            return false;
        }
        let Some(frame_bytes) = encoded_len(payload_len) else {
            return false;
        };
        let Ok(frame_bytes) = u64::try_from(frame_bytes) else {
            return false;
        };
        let Some(new_total) = self.byte_len().checked_add(frame_bytes) else {
            return false;
        };
        let Some(new_count) = self.frame_count.checked_add(1) else {
            return false;
        };
        new_total <= self.limits.max_bytes && new_count <= self.limits.max_frames
    }

    /// Append a frame carrying `payload`, returning its assigned sequence number.
    ///
    /// Responsibility (§12 "buffered append"): assign the next contiguous sequence,
    /// encode the frame, enforce both memory bounds, and fold the encoded bytes into
    /// the rolling content checksum. Every failure mode is explicit — a sealed
    /// segment, a bound breach, a sequence overflow, or an encode error — and on any
    /// failure the segment is left unchanged.
    pub fn append(&mut self, payload: &[u8]) -> Result<u64, SegmentError> {
        if self.sealed {
            return Err(SegmentError::Sealed);
        }
        if payload.len() > DEFAULT_MAX_PAYLOAD_LEN as usize {
            return Err(SegmentError::PayloadTooLarge);
        }

        // Enforce bounds *before* mutating state, using the size of the frame we are
        // about to write. All arithmetic is checked (§22).
        let frame_bytes_usize = encoded_len(payload.len()).ok_or(SegmentError::Full)?;
        let frame_bytes = u64::try_from(frame_bytes_usize).map_err(|_| SegmentError::Full)?;
        let new_total = self
            .byte_len()
            .checked_add(frame_bytes)
            .ok_or(SegmentError::Full)?;
        let new_count = self.frame_count.checked_add(1).ok_or(SegmentError::Full)?;
        if new_total > self.limits.max_bytes || new_count > self.limits.max_frames {
            return Err(SegmentError::Full);
        }

        let sequence = self.next_sequence;
        let next = sequence
            .checked_add(1)
            .ok_or(SegmentError::SequenceOverflow)?;

        let frame = Frame::new(
            self.schema_version,
            self.connection_epoch,
            sequence,
            payload.to_vec(),
        );

        // Encode into a scratch buffer so a codec error cannot leave a half-written
        // frame in the segment buffer.
        let mut encoded = Vec::with_capacity(frame_bytes_usize);
        frame
            .encode_into(&mut encoded)
            .map_err(SegmentError::Frame)?;

        self.hasher.update(&encoded);
        self.buf.extend_from_slice(&encoded);
        self.frame_count = new_count;
        self.first_sequence.get_or_insert(sequence);
        self.next_sequence = next;

        Ok(sequence)
    }

    /// Atomically seal the segment, returning its index entry (§12 "atomic sealing").
    ///
    /// After sealing, further [`append`](Self::append) calls fail with
    /// [`SegmentError::Sealed`]. Sealing an empty segment is rejected. The returned
    /// [`SealedSegment`] carries the final FNV-1a-64 content hash over every encoded
    /// frame byte, plus the sequence span and counts the manifest needs.
    pub fn seal(&mut self) -> Result<SealedSegment, SegmentError> {
        if self.sealed {
            return Err(SegmentError::Sealed);
        }
        let first = self.first_sequence.ok_or(SegmentError::EmptySegment)?;
        let last = self.next_sequence - 1;

        self.sealed = true;
        Ok(SealedSegment {
            segment_id: self.segment_id,
            schema_version: self.schema_version,
            connection_epoch: self.connection_epoch,
            first_sequence: first,
            last_sequence: last,
            frame_count: self.frame_count,
            byte_len: self.byte_len(),
            content_hash: self.hasher.finish(),
        })
    }
}

/// Immutable index entry describing one sealed segment (§12 manifest input).
///
/// Re-exported from [`crate::manifest`] as the manifest's element type; defined here
/// because a segment produces it. All fields are integers (§22).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SealedSegment {
    /// Segment identifier.
    pub segment_id: u64,
    /// Schema version of every frame in the segment.
    pub schema_version: u16,
    /// Connection epoch of every frame in the segment.
    pub connection_epoch: u32,
    /// Sequence number of the first frame (inclusive).
    pub first_sequence: u64,
    /// Sequence number of the last frame (inclusive).
    pub last_sequence: u64,
    /// Number of frames in the segment.
    pub frame_count: u32,
    /// Total encoded byte length of the segment.
    pub byte_len: u64,
    /// FNV-1a-64 content hash over all encoded frame bytes (the segment checksum).
    pub content_hash: u64,
}
