//! Deterministic retry backoff — jitter-free by design (§22).
//!
//! The Python twins have no in-fetch retry at all: one failed poll is logged
//! and the next cadence tick tries again. The Rust lanes keep that outer
//! behavior (a fetch that exhausts its retries is logged and skipped, the
//! cadence loop continues) and add a small bounded retry INSIDE a fetch for
//! the transient classes — HTTP 429 / 5xx / transport errors — because a
//! poll-cadence lane that silently drops a whole tick to a one-second blip is
//! leaving capture latency on the table.
//!
//! Steps are the same fixed doubling ladder as the Twitch lane's reconnect
//! (1 s, 2 s, 4 s, ... capped at 60 s), with no jitter: replays and tests are
//! byte-stable, and a capture edge has no thundering-herd problem to solve —
//! it is one client per vendor. `Retry-After` (integer-seconds form) is
//! respected when the server's ask is LONGER than our ladder step, still
//! capped at [`MAX_STEP_SECS`].

/// First backoff step (seconds) — mirrors the Twitch lane's `BACKOFF_MIN_SECS`.
pub const MIN_STEP_SECS: u64 = 1;
/// Backoff cap (seconds) — mirrors the Twitch lane's `BACKOFF_MAX_SECS`.
pub const MAX_STEP_SECS: u64 = 60;
/// Total tries per fetch (1 initial + 2 retries). Small on purpose: the outer
/// poll cadence is the real retry loop, exactly as in the Python twins.
pub const MAX_TRIES: u32 = 3;

/// The fixed ladder: `step(0) = 1`, doubling, capped at [`MAX_STEP_SECS`].
/// Pure and total — no clock, no RNG (§22).
#[must_use]
pub fn step_secs(attempt: u32) -> u64 {
    MIN_STEP_SECS
        .checked_shl(attempt)
        .unwrap_or(MAX_STEP_SECS)
        .min(MAX_STEP_SECS)
}

/// Decide the delay before retry number `attempt + 1` (0-based `attempt` just
/// failed). `None` means: retries exhausted, surface the error to the caller
/// (who logs and lets the poll cadence carry on — Python behavior).
/// `retry_after_secs` is the parsed `Retry-After` header, if any.
#[must_use]
pub fn retry_delay_secs(attempt: u32, retry_after_secs: Option<u64>) -> Option<u64> {
    if attempt + 1 >= MAX_TRIES {
        return None;
    }
    let ladder = step_secs(attempt);
    Some(retry_after_secs.unwrap_or(0).max(ladder).min(MAX_STEP_SECS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_doubles_and_caps() {
        assert_eq!(step_secs(0), 1);
        assert_eq!(step_secs(1), 2);
        assert_eq!(step_secs(2), 4);
        assert_eq!(step_secs(5), 32);
        assert_eq!(step_secs(6), 60);
        assert_eq!(step_secs(63), 60);
        assert_eq!(step_secs(200), 60, "shift overflow must not panic");
    }

    #[test]
    fn retries_are_bounded() {
        assert_eq!(retry_delay_secs(0, None), Some(1));
        assert_eq!(retry_delay_secs(1, None), Some(2));
        assert_eq!(retry_delay_secs(2, None), None, "MAX_TRIES exhausted");
    }

    #[test]
    fn retry_after_wins_when_longer_and_is_capped() {
        assert_eq!(retry_delay_secs(0, Some(30)), Some(30));
        assert_eq!(
            retry_delay_secs(1, Some(1)),
            Some(2),
            "ladder wins when longer"
        );
        assert_eq!(retry_delay_secs(0, Some(600)), Some(60), "capped");
    }
}
