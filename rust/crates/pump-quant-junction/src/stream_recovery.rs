//! Extracted stream-recovery loops of `pq_daemon` (m1acc follow-up item 3), driven by an injected connector /
//! spawner and a controlled clock so their behaviour is TESTED, not source-inspected.
//!
//! * [`ws_reconnect_episode`]: the Helius WebSocket reconnect episode used by the force / closed / Err / stale paths.
//!   Close the old connection first, then up to [`RetryBound::max_attempts`] connects; after EVERY failed attempt
//!   (including the last) sleep `backoff_ms(n)` = 500 ms, 1 s, 2 s, 4 s, 8 s (cap 10 s) - exactly the ladder the
//!   inline loops ran. Each episode starts again at the base (the existing contract: the stale check retries later).
//!   Exhaustion is the named outcome [`WsOutcome::Exhausted`] (the daemon degrades, it does not crash). NEW: a sleep
//!   is cut short by `cancelled()` (shutdown / emergency stop) -> [`WsOutcome::Cancelled`], no further attempts.
//! * [`RespawnGovernor`] + [`respawn_step`]: the LaserStream child respawn on reader disconnect. At least
//!   [`RetryBound::base_backoff_ms`] (15 s) between attempts (cooldown = minimum spacing, not a sleep), at most
//!   [`RetryBound::max_attempts`] (5) respawns PER PROCESS (a successful respawn does NOT reset the count - existing
//!   contract), then the named terminal [`RespawnDecision::Exhausted`] (Helius WS is the permanent fallback) and no
//!   further attempts. The old child is killed (and reaped) BEFORE the new one is spawned, so at most one child is ever
//!   live (the inline code spawned first and killed second, briefly two live; same intent, now strict).
//! * [`spawn_line_reader`]: the child's stdout reader thread feeding a BOUNDED channel ([`LS_QUEUE_CAP`]). It exits
//!   on EOF (child killed) or when the receiver is gone, so respawns leak no threads; a full queue blocks the reader
//!   (pipe back-pressure to the child) instead of growing memory.

use std::sync::mpsc::SyncSender;
use std::thread::JoinHandle;

use crate::endpoint_retry::RetryBound;

/// Bound on queued LaserStream updates between the reader thread and the engine loop.
pub const LS_QUEUE_CAP: usize = 16_384;

/// A sleeper for backoff. Returns `false` when the sleep was cut short by cancellation.
pub trait Sleeper {
    /// Sleep `ms` (or less, if cancelled). `false` = cancelled.
    fn sleep_ms(&mut self, ms: u64) -> bool;
}

/// Real sleeper: sleeps in <= 100 ms slices, checking `cancelled` between slices.
pub struct RealSleeper<F: Fn() -> bool> {
    /// Shutdown / emergency-stop probe.
    pub cancelled: F,
}

impl<F: Fn() -> bool> Sleeper for RealSleeper<F> {
    fn sleep_ms(&mut self, ms: u64) -> bool {
        let mut left = ms;
        while left > 0 {
            if (self.cancelled)() {
                return false;
            }
            let s = left.min(100);
            std::thread::sleep(std::time::Duration::from_millis(s));
            left = left.saturating_sub(s);
        }
        !(self.cancelled)()
    }
}

/// Result of one reconnect episode.
#[derive(Debug, PartialEq, Eq)]
pub enum WsOutcome<C> {
    /// Connected on attempt `attempt` (1-based).
    Connected {
        /// The new connection.
        conn: C,
        /// 1-based attempt that succeeded.
        attempt: u32,
    },
    /// Every attempt failed (named terminal outcome of the episode; the caller degrades).
    Exhausted {
        /// Attempts made (= max_attempts).
        attempts: u32,
    },
    /// Shutdown during a backoff sleep: no further attempts.
    Cancelled {
        /// Attempts made before cancellation.
        attempts: u32,
    },
}

/// One reconnect episode. `close_old` runs first (the old connection is released before any new one exists).
/// `on_fail(attempt, backoff_ms, &err)` is called for each failure before its sleep (logging / counters).
pub fn ws_reconnect_episode<C, E>(
    bound: &RetryBound,
    close_old: impl FnOnce(),
    mut connect: impl FnMut() -> Result<C, E>,
    mut on_fail: impl FnMut(u32, u64, &E),
    sleeper: &mut dyn Sleeper,
) -> WsOutcome<C> {
    close_old();
    for attempt in 1..=bound.max_attempts {
        match connect() {
            Ok(conn) => return WsOutcome::Connected { conn, attempt },
            Err(e) => {
                let b = bound.backoff_ms(attempt);
                on_fail(attempt, b, &e);
                if !sleeper.sleep_ms(b) {
                    return WsOutcome::Cancelled { attempts: attempt };
                }
            }
        }
    }
    WsOutcome::Exhausted {
        attempts: bound.max_attempts,
    }
}

/// What to do on a LaserStream reader disconnect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RespawnDecision {
    /// Inside the cooldown since the last attempt: do nothing this time.
    CooldownWait,
    /// Attempt respawn number `n` (1-based) now.
    Attempt(u32),
    /// The per-process limit is reached: give up (named terminal; Helius WS is the permanent fallback).
    Exhausted,
    /// Already exhausted earlier: never attempt again.
    GaveUp,
}

/// Respawn bookkeeping (per process; the count never resets).
#[derive(Debug, Clone, Default)]
pub struct RespawnGovernor {
    /// Respawn attempts made so far.
    pub count: u32,
    /// Clock (ms) of the last attempt.
    pub last_ms: Option<u64>,
    /// The limit was reached and reported.
    pub gave_up: bool,
}

impl RespawnGovernor {
    /// Decide at `now_ms`. Cooldown first, then the limit (the inline order). Records an attempt.
    pub fn on_disconnect(&mut self, bound: &RetryBound, now_ms: u64) -> RespawnDecision {
        if self.gave_up {
            return RespawnDecision::GaveUp;
        }
        if let Some(t) = self.last_ms {
            if now_ms.saturating_sub(t) < bound.base_backoff_ms {
                return RespawnDecision::CooldownWait;
            }
        }
        if self.count >= bound.max_attempts {
            self.gave_up = true;
            return RespawnDecision::Exhausted;
        }
        self.count = self.count.saturating_add(1);
        self.last_ms = Some(now_ms);
        RespawnDecision::Attempt(self.count)
    }
}

/// Apply one disconnect: decide, and on `Attempt` kill+reap the old child FIRST, then spawn. Returns the decision
/// and whether the spawn succeeded (`Some(true|false)` only for `Attempt`).
pub fn respawn_step<Ch>(
    gov: &mut RespawnGovernor,
    bound: &RetryBound,
    now_ms: u64,
    current: &mut Option<Ch>,
    mut kill_and_reap: impl FnMut(&mut Ch),
    spawn: impl FnOnce() -> Option<Ch>,
) -> (RespawnDecision, Option<bool>) {
    let d = gov.on_disconnect(bound, now_ms);
    if let RespawnDecision::Attempt(_) = d {
        if let Some(mut old) = current.take() {
            kill_and_reap(&mut old);
        }
        let ok = match spawn() {
            Some(c) => {
                *current = Some(c);
                true
            }
            None => false,
        };
        return (d, Some(ok));
    }
    (d, None)
}

/// Reader thread: each line of `r` parsed by `parse` and sent on the BOUNDED `tx`. Exits on EOF / read error, or
/// when the receiver is dropped. A full channel blocks the thread (back-pressure), never buffers more.
pub fn spawn_line_reader<R, T>(
    r: R,
    tx: SyncSender<T>,
    parse: fn(&str) -> Option<T>,
) -> JoinHandle<()>
where
    R: std::io::Read + Send + 'static,
    T: Send + 'static,
{
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(r).lines() {
            let Ok(text) = line else { break };
            if let Some(u) = parse(&text) {
                if tx.send(u).is_err() {
                    break;
                }
            }
        }
    })
}
