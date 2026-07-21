//! The sealed-segment index with an overall content hash (§12 "atomic sealing, ...
//! manifests").
//!
//! A [`Manifest`] is the ordered, append-only index of the segments a journal has
//! sealed. It enforces the invariant that segment sequence ranges are *contiguous
//! and non-overlapping* — segment N+1 begins exactly where segment N ended — which
//! is what makes the whole journal one gap-free sequence and what lets
//! [`Manifest::find_by_sequence`] locate any frame's segment by binary search.
//!
//! [`Manifest::content_hash`] folds every entry's fields into a single FNV-1a-64
//! value, so two journals are byte-equivalent iff their manifest hashes match — the
//! reproducibility anchor §12/§(determinism) relies on.

use crate::checksum::Fnv1a64;

pub use crate::segment::SealedSegment;

/// Reasons a [`SealedSegment`] was rejected on [`Manifest::add`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestError {
    /// `first_sequence > last_sequence`: the entry's own range is malformed.
    InvalidRange { first: u64, last: u64 },
    /// The entry's `first_sequence` is not exactly one past the previous entry's
    /// `last_sequence` (§12 contiguous sequence tracking).
    NonContiguous { expected: u64, found: u64 },
    /// The entry's `segment_id` is not strictly greater than the previous entry's.
    SegmentOutOfOrder { previous: u64, found: u64 },
}

/// Ordered index of sealed segments plus a content hash.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Manifest {
    entries: Vec<SealedSegment>,
}

impl Manifest {
    /// Create an empty manifest.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Number of sealed segments indexed.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the manifest indexes no segments.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The indexed entry at position `index`, if any.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&SealedSegment> {
        self.entries.get(index)
    }

    /// All indexed entries, in seal order.
    #[must_use]
    pub fn entries(&self) -> &[SealedSegment] {
        &self.entries
    }

    /// Append a sealed segment to the index, enforcing ordering invariants.
    ///
    /// Responsibility (§12): keep the journal a single contiguous sequence. The entry
    /// must have a valid range, a `segment_id` strictly greater than the previous
    /// entry's, and a `first_sequence` exactly one past the previous `last_sequence`.
    /// On any violation the manifest is left unchanged.
    pub fn add(&mut self, entry: SealedSegment) -> Result<(), ManifestError> {
        if entry.first_sequence > entry.last_sequence {
            return Err(ManifestError::InvalidRange {
                first: entry.first_sequence,
                last: entry.last_sequence,
            });
        }
        if let Some(prev) = self.entries.last() {
            if entry.segment_id <= prev.segment_id {
                return Err(ManifestError::SegmentOutOfOrder {
                    previous: prev.segment_id,
                    found: entry.segment_id,
                });
            }
            // Contiguity: next segment starts exactly one past the previous end.
            let expected =
                prev.last_sequence
                    .checked_add(1)
                    .ok_or(ManifestError::NonContiguous {
                        expected: u64::MAX,
                        found: entry.first_sequence,
                    })?;
            if entry.first_sequence != expected {
                return Err(ManifestError::NonContiguous {
                    expected,
                    found: entry.first_sequence,
                });
            }
        }
        self.entries.push(entry);
        Ok(())
    }

    /// Total frame count across all indexed segments, or [`None`] on `u64` overflow.
    #[must_use]
    pub fn total_frames(&self) -> Option<u64> {
        let mut sum: u64 = 0;
        for e in &self.entries {
            sum = sum.checked_add(u64::from(e.frame_count))?;
        }
        Some(sum)
    }

    /// Total encoded byte length across all indexed segments, or [`None`] on overflow.
    #[must_use]
    pub fn total_bytes(&self) -> Option<u64> {
        let mut sum: u64 = 0;
        for e in &self.entries {
            sum = sum.checked_add(e.byte_len)?;
        }
        Some(sum)
    }

    /// The `(first, last)` sequence span covered by the whole manifest, or [`None`]
    /// if empty.
    #[must_use]
    pub fn sequence_span(&self) -> Option<(u64, u64)> {
        let first = self.entries.first()?.first_sequence;
        let last = self.entries.last()?.last_sequence;
        Some((first, last))
    }

    /// Index of the segment whose sequence range contains `sequence`, or [`None`].
    ///
    /// Binary search over the contiguous, ordered ranges — O(log N).
    #[must_use]
    pub fn find_by_sequence(&self, sequence: u64) -> Option<usize> {
        let mut lo = 0usize;
        let mut hi = self.entries.len();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let e = &self.entries[mid];
            if sequence < e.first_sequence {
                hi = mid;
            } else if sequence > e.last_sequence {
                lo = mid + 1;
            } else {
                return Some(mid);
            }
        }
        None
    }

    /// FNV-1a-64 content hash over the canonical serialization of every entry.
    ///
    /// Responsibility (§12 "manifests" / reproducibility): produce a single value
    /// that changes iff any indexed field changes, so a journal is byte-identifiable
    /// by this hash. Fields are folded in a fixed order as little-endian integers.
    #[must_use]
    pub fn content_hash(&self) -> u64 {
        let mut h = Fnv1a64::new();
        // Domain-separate by entry count so [] and truncations are distinguishable.
        h.update(&(self.entries.len() as u64).to_le_bytes());
        for e in &self.entries {
            h.update(&e.segment_id.to_le_bytes());
            h.update(&e.schema_version.to_le_bytes());
            h.update(&e.connection_epoch.to_le_bytes());
            h.update(&e.first_sequence.to_le_bytes());
            h.update(&e.last_sequence.to_le_bytes());
            h.update(&e.frame_count.to_le_bytes());
            h.update(&e.byte_len.to_le_bytes());
            h.update(&e.content_hash.to_le_bytes());
        }
        h.finish()
    }
}
