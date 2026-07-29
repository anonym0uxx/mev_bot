//! # pump-quant-journal
//!
//! Append-only durability codec for the pump-quant hot journal.
//!
//! Constitution reference: **§12 (Windows storage and journals)** — "Hot journal
//! format: append-only binary frames, length prefix, frame CRC, segment checksum,
//! schema version, connection epoch, sequence tracking, ... atomic sealing,
//! crash-recovery scan." This crate implements the *pure byte / integer* core of
//! that format and its in-memory segment state; it performs **no file I/O** (that
//! is layered above, per §12's separation of "buffered append" from "durable seal").
//!
//! ## Design constraints (§22 — deterministic strategy core discipline, applied here)
//!
//! * **No floating point.** Every checksum, hash, length, sequence and offset is an
//!   integer. There is no `f32`/`f64` anywhere in this crate.
//! * **Explicit overflow contract.** Byte/length accounting uses `checked_*` and
//!   returns typed errors on overflow; the CRC32 and FNV-1a hashes use `wrapping_*`
//!   *by contract* (a hash is defined modulo 2^n).
//! * **Deterministic.** No wall-clock, no RNG, no network, no float. Identical bytes
//!   in produce identical frames, hashes and recovery reports out.
//! * **Memory-bounded.** [`segment::Segment`] enforces caller-supplied frame-count
//!   and byte-length limits so a single segment can never grow without bound.
//!
//! ## Modules (the four durability leaves)
//!
//! * [`checksum`] — CRC32 (frame integrity) and FNV-1a-64 (content hashing).
//! * [`frame`]    — length-prefixed + checksummed frame encode/decode with strict bounds.
//! * [`segment`]  — a bounded, sequence-tracked set of frames that seals to an index entry.
//! * [`manifest`] — the sealed-segment index with an overall content hash.
//! * [`recovery`] — [`recovery::recovery_scan`], locating the last valid frame after
//!   truncation or corruption.

// SAFETY POLICY (added 2026-07-29): this crate is the append-only record the whole system is audited against,
// and it contained zero `unsafe` when this was added. `forbid` makes that a
// property the compiler holds rather than one a reviewer has to re-verify —
// and unlike `deny` it cannot be locally overridden by an `#[allow]`.
// Constitution §24(b): an `unsafe` block requires a dossier-registered,
// property-tested safety argument. There is no such dossier entry for this
// crate, so there is no `unsafe` this attribute could legitimately block.
#![forbid(unsafe_code)]

pub mod checksum;
pub mod frame;
pub mod manifest;
pub mod recovery;
pub mod segment;

pub use checksum::{crc32, fnv1a64, Fnv1a64};
pub use frame::{DecodedFrame, Frame, FrameError};
pub use manifest::{Manifest, ManifestError, SealedSegment};
pub use recovery::{recovery_scan, RecoveryReport, StopReason};
pub use segment::{Segment, SegmentError, SegmentLimits};
