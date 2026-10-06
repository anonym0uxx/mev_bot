//! Paper-model lane request discipline: identity, de-duplication, bounds, deadlines.
//!
//! WHY THIS IS ITS OWN MODULE. The model call is slow and blocking; the engine's feed, held-position
//! protection and SAFETY_OFF must never wait on it. So an entry decision is split into three steps
//! that are separated in TIME: *submit* (non-blocking), *inference* (a worker, outside the engine),
//! and *accept* (back on the engine thread, against FRESH state). This table is the only thing that
//! lets the third step trust the first: a result is accepted only if it answers a request this table
//! still holds, once, in time.
//!
//! Pure bookkeeping — no clock, no threads, no I/O. Time is a parameter (`now_ms`), so the tests
//! are deterministic and the engine's no-wall-clock law is not weakened: the caller owns the clock.
//!
//! THE FOUR WAYS A RESULT MUST NOT CREATE AN ORDER
//! 1. **Unknown** — never submitted, already accepted (a duplicate), or superseded.
//! 2. **Late** — its deadline passed; the slot was abandoned. Consumed and discarded.
//! 3. **Blocked** — SAFETY_OFF / entries blocked while it was in flight. Consumed and discarded.
//! 4. **Duplicate request** — a second submit for a mint that already has a live request is refused
//!    at SUBMIT, so there is never a second answer to race the first.
//!
//! BOUNDED RESOURCES. `capacity` counts EVERY entry including abandoned ones. A request that times
//! out frees its dedupe key (the mint may be asked again) but NOT its capacity slot: the worker
//! thread behind it is still occupied until its transport deadline fires, and counting it is what
//! makes "N hung workers" a hard ceiling instead of an unbounded leak.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

/// Default ceiling on outstanding model requests (live + abandoned).
pub const DEFAULT_MAX_OUTSTANDING: usize = 4;

/// Identity of one model request. Monotonic per table; never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(pub u64);

/// Why a submit was refused. Named so each cause is countable and journalled separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitRefusal {
    /// This mint already has a live (non-abandoned) request.
    DuplicateForMint,
    /// `capacity` outstanding requests (live + abandoned) — the bound that caps hung workers.
    AtCapacity,
    /// Entries are blocked (SAFETY_OFF / operator halt).
    EntriesBlocked,
}

/// Why a returned result was not accepted. In every case the entry is CONSUMED (a later duplicate
/// of the same result is then `Unknown`), and no order may be created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptRefusal {
    /// No such live request: never issued, already accepted, or for a different mint.
    Unknown,
    /// The deadline passed. `age_ms` is how long the request had been outstanding.
    DeadlineExceeded {
        /// Milliseconds between issue and arrival.
        age_ms: i64,
    },
    /// The request was already abandoned by [`RequestTable::expire`]; its slot is released now.
    Abandoned,
    /// Entries were blocked while the request was in flight.
    EntriesBlocked,
}

/// One outstanding request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inflight {
    /// The mint the decision concerns.
    pub mint: [u8; 32],
    /// When it was issued, ms on the caller's clock.
    pub issued_ms: i64,
    /// After this instant a result is late.
    pub deadline_ms: i64,
    /// Set by [`RequestTable::expire`]: the deadline passed with no result.
    pub abandoned: bool,
}

/// Counters for the journal / report plane. Every refusal cause is separately visible.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LaneCounters {
    /// Requests issued.
    pub submitted: u64,
    /// Results accepted.
    pub accepted: u64,
    /// Submits refused: duplicate mint.
    pub refused_duplicate: u64,
    /// Submits refused: at capacity.
    pub refused_capacity: u64,
    /// Submits refused: entries blocked.
    pub refused_blocked: u64,
    /// Results discarded: unknown / duplicate.
    pub discarded_unknown: u64,
    /// Results discarded: late.
    pub discarded_late: u64,
    /// Results discarded: abandoned earlier.
    pub discarded_abandoned: u64,
    /// Results discarded: entries blocked in flight.
    pub discarded_blocked: u64,
    /// Requests abandoned by deadline expiry.
    pub abandoned: u64,
}

/// The outstanding-request table.
#[derive(Debug, Clone)]
pub struct RequestTable {
    next_id: u64,
    capacity: usize,
    inflight: BTreeMap<RequestId, Inflight>,
    blocked: bool,
    counters: LaneCounters,
}

impl Default for RequestTable {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_OUTSTANDING)
    }
}

impl RequestTable {
    /// A table bounded at `capacity` outstanding requests (min 1).
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            next_id: 1,
            capacity: capacity.max(1),
            inflight: BTreeMap::new(),
            blocked: false,
            counters: LaneCounters::default(),
        }
    }

    /// A table whose ids start at `id_base`, so two tables sharing one worker pool can never issue
    /// the same [`RequestId`] (entry ids vs management ids).
    #[must_use]
    pub fn with_id_base(capacity: usize, id_base: u64) -> Self {
        let mut t = Self::new(capacity);
        t.next_id = id_base.max(1);
        t
    }

    /// Block or unblock new entries (SAFETY_OFF). Blocking does not drop in-flight entries — their
    /// results are consumed and discarded on arrival, so a worker's slot is always reclaimed.
    pub fn set_entries_blocked(&mut self, blocked: bool) {
        self.blocked = blocked;
    }

    /// Whether entries are blocked.
    #[must_use]
    pub fn entries_blocked(&self) -> bool {
        self.blocked
    }

    /// Outstanding requests, live and abandoned.
    #[must_use]
    pub fn outstanding(&self) -> usize {
        self.inflight.len()
    }

    /// Live (non-abandoned) outstanding requests.
    #[must_use]
    pub fn live(&self) -> usize {
        self.inflight.values().filter(|i| !i.abandoned).count()
    }

    /// The counters.
    #[must_use]
    pub fn counters(&self) -> LaneCounters {
        self.counters
    }

    /// Whether `mint` has a live request.
    #[must_use]
    pub fn has_live_for(&self, mint: &[u8; 32]) -> bool {
        self.inflight
            .values()
            .any(|i| !i.abandoned && &i.mint == mint)
    }

    /// Register a request. Refusals are checked cause-first: blocked, then duplicate, then capacity.
    ///
    /// # Errors
    /// [`SubmitRefusal`] — nothing is registered and no worker should be started.
    pub fn submit(
        &mut self,
        mint: [u8; 32],
        now_ms: i64,
        deadline_ms: i64,
    ) -> Result<RequestId, SubmitRefusal> {
        if self.blocked {
            self.counters.refused_blocked += 1;
            return Err(SubmitRefusal::EntriesBlocked);
        }
        if self.has_live_for(&mint) {
            self.counters.refused_duplicate += 1;
            return Err(SubmitRefusal::DuplicateForMint);
        }
        if self.inflight.len() >= self.capacity {
            self.counters.refused_capacity += 1;
            return Err(SubmitRefusal::AtCapacity);
        }
        let id = RequestId(self.next_id);
        self.next_id += 1;
        self.inflight.insert(
            id,
            Inflight {
                mint,
                issued_ms: now_ms,
                deadline_ms,
                abandoned: false,
            },
        );
        self.counters.submitted += 1;
        Ok(id)
    }

    /// A result for `id` (about `mint`) has arrived at `now_ms`. Consumes the entry whatever the
    /// verdict, so a second delivery of the same result is [`AcceptRefusal::Unknown`].
    ///
    /// # Errors
    /// [`AcceptRefusal`] — the caller must create NO order.
    pub fn accept(
        &mut self,
        id: RequestId,
        mint: &[u8; 32],
        now_ms: i64,
    ) -> Result<Inflight, AcceptRefusal> {
        // A result naming the wrong mint is not an answer to this request: leave the entry in
        // place (the real answer may still come) and refuse.
        match self.inflight.get(&id) {
            None => {
                self.counters.discarded_unknown += 1;
                return Err(AcceptRefusal::Unknown);
            }
            Some(e) if &e.mint != mint => {
                self.counters.discarded_unknown += 1;
                return Err(AcceptRefusal::Unknown);
            }
            Some(_) => {}
        }
        let entry = self.inflight.remove(&id).expect("present: checked above");
        if entry.abandoned {
            self.counters.discarded_abandoned += 1;
            return Err(AcceptRefusal::Abandoned);
        }
        if self.blocked {
            self.counters.discarded_blocked += 1;
            return Err(AcceptRefusal::EntriesBlocked);
        }
        if now_ms > entry.deadline_ms {
            self.counters.discarded_late += 1;
            return Err(AcceptRefusal::DeadlineExceeded {
                age_ms: now_ms.saturating_sub(entry.issued_ms),
            });
        }
        self.counters.accepted += 1;
        Ok(entry)
    }

    /// Release a request that never reached a worker (the dispatch was refused). Returns true if
    /// it was present. Unlike [`accept`](Self::accept) this counts nothing as accepted.
    pub fn release(&mut self, id: RequestId) -> bool {
        self.inflight.remove(&id).is_some()
    }

    /// Abandon every live request whose deadline has passed. Frees the dedupe key (the mint may be
    /// asked again) but NOT the capacity slot — the worker is still occupied. Returns the ids
    /// abandoned now.
    pub fn expire(&mut self, now_ms: i64) -> Vec<RequestId> {
        let mut out = Vec::new();
        for (id, e) in &mut self.inflight {
            if !e.abandoned && now_ms > e.deadline_ms {
                e.abandoned = true;
                self.counters.abandoned += 1;
                out.push(*id);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: [u8; 32] = [1u8; 32];
    const B: [u8; 32] = [2u8; 32];
    const C: [u8; 32] = [3u8; 32];

    #[test]
    fn a_released_request_frees_its_slot_without_counting_an_acceptance() {
        let mut t = RequestTable::new(1);
        let id = t.submit(A, 0, 3_000).unwrap();
        assert!(t.release(id));
        assert!(!t.release(id), "second release is a no-op");
        assert_eq!(t.outstanding(), 0);
        assert_eq!(t.counters().accepted, 0);
        assert!(
            t.submit(A, 1, 3_001).is_ok(),
            "the slot and the mint key are free again"
        );
    }

    #[test]
    fn a_submitted_request_is_accepted_once_in_time() {
        let mut t = RequestTable::new(4);
        let id = t.submit(A, 1_000, 4_000).unwrap();
        let got = t.accept(id, &A, 2_500).unwrap();
        assert_eq!(got.issued_ms, 1_000);
        assert_eq!(t.outstanding(), 0, "accept consumes the entry");
        assert_eq!(t.counters().accepted, 1);
    }

    #[test]
    fn a_second_submit_for_a_live_mint_is_refused_so_there_is_never_a_second_answer() {
        let mut t = RequestTable::new(4);
        t.submit(A, 0, 3_000).unwrap();
        assert_eq!(t.submit(A, 10, 3_010), Err(SubmitRefusal::DuplicateForMint));
        assert_eq!(t.outstanding(), 1, "the refused submit registered nothing");
        // A different mint is independent.
        assert!(t.submit(B, 10, 3_010).is_ok());
        assert_eq!(t.counters().refused_duplicate, 1);
    }

    #[test]
    fn a_duplicate_delivery_of_the_same_result_is_unknown_and_creates_nothing() {
        let mut t = RequestTable::new(4);
        let id = t.submit(A, 0, 3_000).unwrap();
        assert!(t.accept(id, &A, 100).is_ok());
        assert_eq!(t.accept(id, &A, 101), Err(AcceptRefusal::Unknown));
        assert_eq!(t.counters().accepted, 1, "exactly one acceptance");
        assert_eq!(t.counters().discarded_unknown, 1);
    }

    #[test]
    fn a_never_issued_id_or_wrong_mint_is_unknown() {
        let mut t = RequestTable::new(4);
        assert_eq!(t.accept(RequestId(99), &A, 0), Err(AcceptRefusal::Unknown));
        let id = t.submit(A, 0, 3_000).unwrap();
        // The right id naming the wrong mint is not an answer to this request…
        assert_eq!(t.accept(id, &B, 10), Err(AcceptRefusal::Unknown));
        // …and does NOT consume it: the genuine answer can still land.
        assert!(t.accept(id, &A, 20).is_ok());
    }

    #[test]
    fn a_result_after_its_deadline_is_late_and_consumed() {
        let mut t = RequestTable::new(4);
        let id = t.submit(A, 1_000, 4_000).unwrap();
        assert_eq!(
            t.accept(id, &A, 4_001),
            Err(AcceptRefusal::DeadlineExceeded { age_ms: 3_001 })
        );
        assert_eq!(t.outstanding(), 0);
        assert_eq!(t.accept(id, &A, 4_002), Err(AcceptRefusal::Unknown));
        assert_eq!(t.counters().discarded_late, 1);
    }

    #[test]
    fn the_capacity_bound_refuses_rather_than_queueing_without_limit() {
        let mut t = RequestTable::new(2);
        t.submit(A, 0, 3_000).unwrap();
        t.submit(B, 0, 3_000).unwrap();
        assert_eq!(t.submit(C, 0, 3_000), Err(SubmitRefusal::AtCapacity));
        assert_eq!(t.outstanding(), 2);
        assert_eq!(t.counters().refused_capacity, 1);
    }

    /// The hung-worker ceiling. An abandoned request frees its DEDUPE key (the mint can be asked
    /// again) but still holds its CAPACITY slot until the worker's result returns — otherwise
    /// every timeout would leak a thread's worth of capacity unaccounted.
    #[test]
    fn an_abandoned_request_frees_its_mint_but_holds_its_capacity_until_the_worker_returns() {
        let mut t = RequestTable::new(1);
        let hung = t.submit(A, 0, 3_000).unwrap();
        assert_eq!(t.expire(3_001), vec![hung]);
        assert_eq!(
            t.live(),
            0,
            "no live request: the mint's dedupe key is free"
        );
        assert!(!t.has_live_for(&A));
        // …but the one worker is still busy, so the table is still full:
        assert_eq!(t.submit(A, 3_002, 6_000), Err(SubmitRefusal::AtCapacity));
        // The hung worker finally returns: discarded, slot released.
        assert_eq!(t.accept(hung, &A, 9_000), Err(AcceptRefusal::Abandoned));
        assert_eq!(t.outstanding(), 0);
        assert!(t.submit(A, 9_001, 12_000).is_ok(), "capacity is back");
        assert_eq!(t.counters().abandoned, 1);
        assert_eq!(t.counters().discarded_abandoned, 1);
    }

    #[test]
    fn expire_only_abandons_requests_past_their_own_deadline() {
        let mut t = RequestTable::new(4);
        let early = t.submit(A, 0, 1_000).unwrap();
        let late = t.submit(B, 0, 5_000).unwrap();
        assert_eq!(t.expire(2_000), vec![early]);
        assert!(
            t.accept(late, &B, 2_500).is_ok(),
            "the later one is untouched"
        );
        assert!(t.expire(2_000).is_empty(), "expire is idempotent");
    }

    #[test]
    fn blocked_entries_refuse_new_submits_and_discard_in_flight_results() {
        let mut t = RequestTable::new(4);
        let id = t.submit(A, 0, 3_000).unwrap();
        t.set_entries_blocked(true);
        assert!(t.entries_blocked());
        assert_eq!(t.submit(B, 1, 3_001), Err(SubmitRefusal::EntriesBlocked));
        // The in-flight answer arrives while blocked: consumed, no order, slot reclaimed.
        assert_eq!(t.accept(id, &A, 100), Err(AcceptRefusal::EntriesBlocked));
        assert_eq!(t.outstanding(), 0);
        // Re-arming is explicit; nothing was queued behind the block.
        t.set_entries_blocked(false);
        assert!(t.submit(B, 200, 3_200).is_ok());
    }

    #[test]
    fn request_ids_are_monotonic_and_never_reused() {
        let mut t = RequestTable::new(4);
        let a = t.submit(A, 0, 100).unwrap();
        t.accept(a, &A, 1).unwrap();
        let b = t.submit(A, 2, 100).unwrap();
        assert!(b > a, "a consumed id is never handed out again");
    }
}
