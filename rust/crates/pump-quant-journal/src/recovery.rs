//! Crash-recovery scan (§12 "crash-recovery scan"; §12 "corruption behavior").
//!
//! After a crash, a segment file's tail may be a partially-written (truncated) frame
//! or a corrupted one. [`recovery_scan`] walks an encoded byte buffer frame by frame
//! from the start and reports the **safe truncation point**: the byte offset up to
//! which every frame decoded cleanly, verified its CRC, and (optionally) continued
//! the expected contiguous sequence. Everything after that offset is discarded on
//! recovery, restoring the append-only invariant.
//!
//! The scan is strict and never resynchronizes past a bad frame: for an append-only
//! log, once a frame fails to validate the remainder of the file is untrusted. All
//! logic is integer and deterministic (§22).

use crate::frame::{Frame, FrameError};

/// Why the recovery scan stopped advancing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The entire buffer decoded into valid frames with nothing left over.
    CleanEnd,
    /// A trailing fragment was smaller than a frame header.
    TooShortForHeader,
    /// A frame's magic prefix did not match — structural corruption.
    BadMagic,
    /// A frame declared a payload length beyond the allowed bound.
    PayloadTooLarge,
    /// A trailing frame was declared but the buffer ended before its full extent.
    Truncated,
    /// A frame's stored CRC did not match its contents.
    BadChecksum,
    /// A frame decoded cleanly but broke contiguous sequence order.
    SequenceGap { expected: u64, found: u64 },
}

/// Outcome of a [`recovery_scan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Number of valid frames recovered from the front of the buffer.
    pub frames_recovered: u64,
    /// Byte offset of the end of the last valid frame — the safe truncation point.
    /// Bytes in `[0, valid_len)` are trustworthy; bytes at/after it are discarded.
    pub valid_len: usize,
    /// Sequence number of the last valid frame, or [`None`] if none were recovered.
    pub last_sequence: Option<u64>,
    /// Why the scan stopped.
    pub stop_reason: StopReason,
}

/// Scan `buf` for the last valid frame after truncation or corruption.
///
/// Responsibility (§12 "crash-recovery scan"): given the raw bytes of a possibly
/// damaged segment, return where the trustworthy region ends. Starting at offset 0,
/// each frame is decoded and CRC-checked via [`Frame::decode`] under
/// `max_payload_len`. If `expected_first_sequence` is [`Some`], each frame's sequence
/// must equal the running expected value (a contiguous run starting there); a
/// mismatch stops the scan with [`StopReason::SequenceGap`] and does **not** count
/// the offending frame. The scan halts at the first fragment that fails any check,
/// reporting the recovered frame count, the safe `valid_len`, and the reason.
///
/// Deterministic and float-free; makes no allocation beyond decoded payloads.
#[must_use]
pub fn recovery_scan(
    buf: &[u8],
    expected_first_sequence: Option<u64>,
    max_payload_len: u32,
) -> RecoveryReport {
    let mut offset: usize = 0;
    let mut frames_recovered: u64 = 0;
    let mut last_sequence: Option<u64> = None;
    let mut expected = expected_first_sequence;

    loop {
        let remaining = &buf[offset..];
        if remaining.is_empty() {
            return RecoveryReport {
                frames_recovered,
                valid_len: offset,
                last_sequence,
                stop_reason: StopReason::CleanEnd,
            };
        }

        match Frame::decode(remaining, max_payload_len) {
            Ok(decoded) => {
                let seq = decoded.frame.sequence;
                if let Some(exp) = expected {
                    if seq != exp {
                        // A validly-encoded frame that breaks contiguity: stop before
                        // it, leaving valid_len at the prior boundary.
                        return RecoveryReport {
                            frames_recovered,
                            valid_len: offset,
                            last_sequence,
                            stop_reason: StopReason::SequenceGap {
                                expected: exp,
                                found: seq,
                            },
                        };
                    }
                }

                // Advance past this frame. consumed is bounded by remaining.len(), so
                // offset + consumed cannot overflow the buffer length.
                offset += decoded.consumed;
                frames_recovered += 1;
                last_sequence = Some(seq);
                // If sequence would overflow, stop tracking expectation (next decode
                // of a real frame is impossible beyond u64::MAX anyway).
                expected = expected.and_then(|_| seq.checked_add(1));
            }
            Err(err) => {
                let stop_reason = match err {
                    FrameError::TooShortForHeader { .. } => StopReason::TooShortForHeader,
                    FrameError::BadMagic { .. } => StopReason::BadMagic,
                    FrameError::PayloadTooLarge { .. } => StopReason::PayloadTooLarge,
                    FrameError::Truncated { .. } | FrameError::LengthOverflow => {
                        StopReason::Truncated
                    }
                    FrameError::BadChecksum { .. } => StopReason::BadChecksum,
                };
                return RecoveryReport {
                    frames_recovered,
                    valid_len: offset,
                    last_sequence,
                    stop_reason,
                };
            }
        }
    }
}
