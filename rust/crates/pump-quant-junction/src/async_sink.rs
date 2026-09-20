//! E3: get outbound submission OFF the decision thread — on per-mint lanes.
//!
//! ## Why this exists
//! `LiveOutboundSink::on_admit` fetches on-chain state, builds, signs, and submits
//! a transaction — a real network round trip (RTT ~5-50 ms, plus whatever the R-3
//! creator-history veto costs on top). The engine calls `on_admit` inline from
//! `tick()`, so every admit paid that latency twice: the decision loop stalled, and
//! so did every other market in the tick's watchlist.
//!
//! ## Lanes, not one queue
//! A single worker would make every position wait behind every other position's
//! submission: hold ten positions and their exits queue up one at a time. So the
//! work is spread over `LANES` workers, and each job is routed to a lane by its
//! MINT.
//!
//! That routing is the whole ordering argument:
//! - **same mint → same lane → FIFO**, so a sell can never overtake its own buy, and
//!   two exits of the same position stay in the order the engine decided them;
//! - **different mints → different lanes → parallel**, which is what makes ten
//!   positions ten independent streams instead of one queue.
//!
//! ## Handoff contract
//! - `on_admit` **enqueues** the record and returns immediately with
//!   [`OutboundOutcome::Queued`], carrying a ticket. The engine parks the bookkeeping
//!   it owes (see `Engine::note_inflight_outbound`) and keeps the position open — a
//!   queued submission is not a failed one.
//! - a lane worker runs the *same* inner pipeline and pushes the real outcome into a
//!   shared result channel (ordering across lanes is irrelevant: each verdict carries
//!   its ticket, and the engine looks up what it owes by ticket).
//! - the daemon drains that channel on its tick and reports each verdict back into
//!   the engine (`complete_async_outbound` / `fail_async_outbound`), which is where
//!   the accounting and the phantom-position rollback live.
//!
//! ## Bounds and failure direction
//! Each lane's queue is **bounded**. A full lane refuses the submission with
//! `OutboundOutcome::Sender` rather than blocking the decision thread: the engine
//! then treats it exactly as a synchronous sink failure (the position is reversed),
//! which is the safe direction — capital is not committed to a transaction that was
//! never handed off. Refusals are counted (`refused`), so a saturated lane is
//! visible rather than silent.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::Mutex;
use std::thread;
use std::time::Instant;

use pump_quant_execution::ex_outbound_sink::{AdmitRecord, OutboundOutcome, OutboundSink};

/// How many submission lanes the daemon runs. Four is enough to keep ten positions
/// independent without oversubscribing the RPC/Sender endpoint (the node rate-limits
/// per connection, and a burst of same-mint retries stays on one lane regardless).
pub const DEFAULT_LANES: usize = 4;

/// One completed submission, as reported by a lane worker.
#[derive(Debug, Clone)]
pub struct OutboundResult {
    /// The ticket the engine received when the record was queued.
    pub ticket: u64,
    /// The record that was submitted — carries the mint, side and sizes the
    /// engine needs to finish its bookkeeping.
    pub record: AdmitRecord,
    /// What the inner sink returned.
    pub outcome: OutboundOutcome,
    /// Microseconds the worker spent inside the inner sink. This is the time the
    /// decision thread did NOT pay.
    pub worker_us: u64,
    /// Which lane ran it. Diagnostic — route a repeated failure to its lane.
    pub lane: usize,
}

/// Wraps an inner [`OutboundSink`] and runs it on a pool of per-mint FIFO lanes.
pub struct AsyncOutboundSink {
    lanes: Vec<SyncSender<(u64, AdmitRecord)>>,
    results: Mutex<Receiver<OutboundResult>>,
    next_ticket: AtomicU64,
    in_flight: AtomicU64,
    refused: AtomicU64,
    delivered: AtomicU64,
    /// Per-lane refusals, so a saturated lane is identifiable.
    lane_refused: Vec<AtomicU64>,
}

impl AsyncOutboundSink {
    /// Wrap `inner`, spawning `lanes` workers. `queue_depth` bounds EACH lane; a
    /// full lane refuses (never blocks) the caller. `lanes` is clamped to ≥ 1.
    pub fn new(inner: &'static dyn OutboundSink, lanes: usize, queue_depth: usize) -> Self {
        let lanes = lanes.max(1);
        let (results_tx, results_rx) = mpsc::channel::<OutboundResult>();
        let mut senders = Vec::with_capacity(lanes);
        for lane in 0..lanes {
            let (jobs_tx, jobs_rx) = mpsc::sync_channel::<(u64, AdmitRecord)>(queue_depth.max(1));
            let results_tx = results_tx.clone();
            // The workers own the inner sink for the process lifetime. A worker exits
            // only if its queue or the result channel goes away — for a `'static` sink
            // installed in the daemon that means process exit, which is intended: a
            // submission in flight must never be abandoned by a shutdown race.
            thread::Builder::new()
                .name(format!("outbound-lane-{lane}"))
                .spawn(move || {
                    while let Ok((ticket, record)) = jobs_rx.recv() {
                        let started = Instant::now();
                        let outcome = inner.on_admit(&record);
                        let worker_us = started.elapsed().as_micros() as u64;
                        if results_tx
                            .send(OutboundResult {
                                ticket,
                                record,
                                outcome,
                                worker_us,
                                lane,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                })
                .expect("spawn outbound lane worker");
            senders.push(jobs_tx);
        }
        Self {
            lanes: senders,
            results: Mutex::new(results_rx),
            next_ticket: AtomicU64::new(1),
            in_flight: AtomicU64::new(0),
            refused: AtomicU64::new(0),
            delivered: AtomicU64::new(0),
            lane_refused: (0..lanes).map(|_| AtomicU64::new(0)).collect(),
        }
    }

    /// Which lane a mint is routed to. A stable hash — the same mint always lands on
    /// the same lane, in this process and the next.
    ///
    /// FNV-1a alone is not enough here: over 32 IDENTICAL bytes it multiplies out to a
    /// constant modulo the lane count (the prime is ≡ 1 in the low bits after an even
    /// number of steps), which would route a whole low-entropy address space to ONE
    /// worker — precisely the starvation the lanes exist to prevent. The splitmix64
    /// avalanche at the end fixes the low bits, so the lane depends on the whole hash
    /// rather than its trailing residue. Real mints are high-entropy; the router must
    /// not depend on that.
    fn lane_of(&self, mint: &[u8; 32]) -> usize {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a offset basis
        for b in mint {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        // splitmix64 finalizer.
        h ^= h >> 33;
        h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
        h ^= h >> 33;
        (h as usize) % self.lanes.len()
    }

    /// Drain every verdict the workers have produced since the last call. Non-blocking.
    pub fn drain_results(&self) -> Vec<OutboundResult> {
        let rx = self.results.lock().unwrap();
        rx.try_iter().collect()
    }

    /// Submissions handed off and not yet reported back. Diagnostic.
    #[must_use]
    pub fn in_flight(&self) -> u64 {
        self.in_flight.load(Ordering::Relaxed)
    }

    /// Submissions refused because a lane was full. Diagnostic.
    #[must_use]
    pub fn refused(&self) -> u64 {
        self.refused.load(Ordering::Relaxed)
    }

    /// Verdicts drained and reported back to the engine. Diagnostic.
    #[must_use]
    pub fn delivered(&self) -> u64 {
        self.delivered.load(Ordering::Relaxed)
    }

    /// How many lanes are running, and how many refusals each has taken.
    #[must_use]
    pub fn lane_stats(&self) -> Vec<u64> {
        self.lane_refused
            .iter()
            .map(|c| c.load(Ordering::Relaxed))
            .collect()
    }

    /// Called by the drainer once a verdict has been delivered to the engine.
    pub fn note_delivered(&self) {
        self.in_flight.fetch_sub(1, Ordering::Relaxed);
        self.delivered.fetch_add(1, Ordering::Relaxed);
    }
}

impl OutboundSink for AsyncOutboundSink {
    fn on_admit(&self, record: &AdmitRecord) -> OutboundOutcome {
        let ticket = self.next_ticket.fetch_add(1, Ordering::Relaxed);
        let lane = self.lane_of(&record.mint);
        match self.lanes[lane].try_send((ticket, *record)) {
            Ok(()) => {
                self.in_flight.fetch_add(1, Ordering::Relaxed);
                OutboundOutcome::Queued { ticket }
            }
            // A full lane must never block the decision thread. Refusing makes the
            // engine treat this exactly as a synchronous sink failure — the position
            // is reversed instead of being committed to a record that has no worker.
            Err(TrySendError::Full(_)) => {
                self.refused.fetch_add(1, Ordering::Relaxed);
                self.lane_refused[lane].fetch_add(1, Ordering::Relaxed);
                OutboundOutcome::Sender(format!(
                    "outbound lane {lane} full — submission refused without blocking the decision thread"
                ))
            }
            Err(TrySendError::Disconnected(_)) => {
                self.refused.fetch_add(1, Ordering::Relaxed);
                self.lane_refused[lane].fetch_add(1, Ordering::Relaxed);
                OutboundOutcome::Sender(format!("outbound lane {lane} worker is gone"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    struct SlowSink {
        delay: Duration,
        seen: AtomicBool,
    }

    impl OutboundSink for SlowSink {
        fn on_admit(&self, _record: &AdmitRecord) -> OutboundOutcome {
            std::thread::sleep(self.delay);
            self.seen.store(true, Ordering::SeqCst);
            OutboundOutcome::Accepted {
                signature: [0xAB; 64],
                submit_rpc_us: self.delay.as_micros() as u64,
            }
        }
    }

    struct RefusingSink;

    impl OutboundSink for RefusingSink {
        fn on_admit(&self, _record: &AdmitRecord) -> OutboundOutcome {
            OutboundOutcome::Construction("no verified layout fixture".to_string())
        }
    }

    /// Records the order in which records arrive, per mint.
    struct OrderSink {
        log: StdMutex<Vec<([u8; 32], u64)>>,
        delay: Duration,
    }

    impl OutboundSink for OrderSink {
        fn on_admit(&self, record: &AdmitRecord) -> OutboundOutcome {
            self.log
                .lock()
                .unwrap()
                .push((record.mint, record.size_lamports));
            std::thread::sleep(self.delay);
            OutboundOutcome::Accepted {
                signature: [0x11; 64],
                submit_rpc_us: 0,
            }
        }
    }

    /// A realistic mint — 32 varied bytes. (Note: an FNV lane hash over a
    /// REPEATED-byte mint collapses — 32 identical bytes multiply out to a constant
    /// mod the lane count. Real mints are never like that, and neither are these.)
    fn varied_mint(seed: u32) -> [u8; 32] {
        let mut m = [0u8; 32];
        let mut h = seed.wrapping_mul(0x9e37_79b9) ^ 0x5bf0_3635;
        for slot in m.iter_mut() {
            h = h.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *slot = (h >> 24) as u8;
        }
        m
    }

    /// A mint whose lane is `want`, found by search rather than assumption.
    fn mint_on_lane(sink: &AsyncOutboundSink, want: usize) -> [u8; 32] {
        for seed in 0..100_000u32 {
            let m = varied_mint(seed);
            if sink.lane_of(&m) == want {
                return m;
            }
        }
        panic!("no probe mint hashes to lane {want}");
    }

    /// A mint on any lane other than `not`.
    fn mint_not_on_lane(sink: &AsyncOutboundSink, not: usize) -> [u8; 32] {
        for seed in 0..100_000u32 {
            let m = varied_mint(seed);
            if sink.lane_of(&m) != not {
                return m;
            }
        }
        panic!("no probe mint avoids lane {not}");
    }

    fn record(mint: [u8; 32], is_buy: bool, size: u64) -> AdmitRecord {
        AdmitRecord {
            mint,
            user: [0u8; 32],
            is_buy,
            size_lamports: size,
            entry_price: 42,
            max_slippage_bps: 500,
            // The model's price limit is not plumbed to this call site yet:
            // when the Qwen wiring lands, the engine fills it in from the
            // parsed decision. Until then the slippage budget protects the order.
            price_limit_lamports_per_raw_token: None,
        }
    }

    fn drain_until(sink: &AsyncOutboundSink, want: usize) -> Vec<OutboundResult> {
        let mut out = Vec::new();
        for _ in 0..400 {
            out.extend(sink.drain_results());
            if out.len() >= want {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        out
    }

    #[test]
    fn on_admit_returns_before_the_work_happens() {
        let inner: &'static SlowSink = Box::leak(Box::new(SlowSink {
            delay: Duration::from_millis(120),
            seen: AtomicBool::new(false),
        }));
        let sink = AsyncOutboundSink::new(inner, 2, 8);
        let started = Instant::now();
        let outcome = sink.on_admit(&record(varied_mint(1), true, 1_000));
        let handoff_us = started.elapsed().as_micros();
        assert!(
            matches!(outcome, OutboundOutcome::Queued { ticket } if ticket == 1),
            "a queued record must report Queued, not an outcome it cannot know yet"
        );
        assert!(
            handoff_us < 60_000,
            "handoff must not wait for the 120 ms inner sink (took {handoff_us} µs)"
        );
        let results = drain_until(&sink, 1);
        assert_eq!(results.len(), 1, "one submission → one verdict");
        assert_eq!(results[0].ticket, 1);
        assert!(matches!(
            results[0].outcome,
            OutboundOutcome::Accepted { signature, .. } if signature == [0xAB; 64]
        ));
        assert!(
            results[0].worker_us >= 100_000,
            "the worker's own timing must show the real work it absorbed"
        );
        assert!(inner.seen.load(Ordering::SeqCst), "inner sink really ran");
    }

    #[test]
    fn different_mints_submit_in_parallel() {
        // Four lanes, four mints, one per lane, 200 ms each. Serialised this is
        // ≥ 800 ms; parallel means each position is its own stream, not one queue.
        let inner: &'static SlowSink = Box::leak(Box::new(SlowSink {
            delay: Duration::from_millis(200),
            seen: AtomicBool::new(false),
        }));
        let sink = AsyncOutboundSink::new(inner, 4, 8);
        let mints: Vec<[u8; 32]> = (0..4).map(|l| mint_on_lane(&sink, l)).collect();
        let started = Instant::now();
        for m in &mints {
            assert!(matches!(
                sink.on_admit(&record(*m, true, 1_000)),
                OutboundOutcome::Queued { .. }
            ));
        }
        let results = drain_until(&sink, 4);
        let elapsed = started.elapsed();
        assert_eq!(results.len(), 4);
        assert!(
            elapsed < Duration::from_millis(700),
            "four 200 ms submissions on four lanes must overlap (took {elapsed:?})"
        );
    }

    #[test]
    fn one_mints_records_keep_their_order() {
        // Same mint, several records, interleaved with other mints: the mint's own
        // submissions must stay FIFO so a sell can never overtake its buy.
        let inner: &'static OrderSink = Box::leak(Box::new(OrderSink {
            log: StdMutex::new(Vec::new()),
            delay: Duration::from_millis(20),
        }));
        let sink = AsyncOutboundSink::new(inner, 4, 16);
        let mine = varied_mint(7);
        let my_lane = sink.lane_of(&mine);
        // Two mints that are NOT this one, so the log filter below is unambiguous.
        let mut others: Vec<[u8; 32]> = Vec::new();
        for seed in 0..100_000u32 {
            let m = varied_mint(seed);
            if sink.lane_of(&m) != my_lane && !others.contains(&m) {
                others.push(m);
                if others.len() == 2 {
                    break;
                }
            }
        }
        assert_eq!(others.len(), 2, "need two other mints to interleave");
        // Buy 100, then sell 200, then sell 300 — with other mints interleaved.
        for (m, is_buy, size) in [
            (mine, true, 100u64),
            (others[0], true, 111),
            (mine, false, 200),
            (others[1], true, 222),
            (mine, false, 300),
        ] {
            assert!(matches!(
                sink.on_admit(&record(m, is_buy, size)),
                OutboundOutcome::Queued { .. }
            ));
        }
        let results = drain_until(&sink, 5);
        assert_eq!(results.len(), 5);
        let order: Vec<u64> = inner
            .log
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| *m == mine)
            .map(|(_, s)| *s)
            .collect();
        assert_eq!(
            order,
            vec![100, 200, 300],
            "one mint's submissions must execute in the order the engine decided them"
        );
    }

    #[test]
    fn a_full_lane_refuses_instead_of_blocking() {
        let inner: &'static SlowSink = Box::leak(Box::new(SlowSink {
            delay: Duration::from_millis(300),
            seen: AtomicBool::new(false),
        }));
        // ONE lane: the lane holds one job, the worker the next, and the third has
        // nowhere to go.
        let sink = AsyncOutboundSink::new(inner, 1, 1);
        let m = varied_mint(3);
        let mut refusals = 0;
        for _ in 0..4 {
            if let OutboundOutcome::Sender(msg) = sink.on_admit(&record(m, true, 1_000)) {
                assert!(msg.contains("full"), "{msg}");
                refusals += 1;
            }
        }
        assert!(
            refusals >= 1,
            "a single lane with a slow worker must refuse rather than block"
        );
        assert_eq!(sink.refused(), refusals);
        assert_eq!(sink.lane_stats().iter().sum::<u64>(), refusals);
    }

    #[test]
    fn a_full_lane_does_not_block_a_different_mint() {
        // The point of lanes: one hot mint must not starve the rest.
        let inner: &'static SlowSink = Box::leak(Box::new(SlowSink {
            delay: Duration::from_millis(250),
            seen: AtomicBool::new(false),
        }));
        let sink = AsyncOutboundSink::new(inner, 4, 1);
        let hot_lane = sink.lane_of(&varied_mint(9));
        let hot = mint_on_lane(&sink, hot_lane);
        let cold = mint_not_on_lane(&sink, hot_lane);
        assert_ne!(sink.lane_of(&hot), sink.lane_of(&cold));
        // Saturate the hot lane however it falls out (the worker may or may not have
        // picked up the first job yet) — at least one handoff to it must be refused.
        let mut hot_refused = false;
        for _ in 0..4 {
            if matches!(
                sink.on_admit(&record(hot, true, 1)),
                OutboundOutcome::Sender(_)
            ) {
                hot_refused = true;
            }
        }
        assert!(hot_refused, "the hot lane is saturated");
        assert!(
            matches!(
                sink.on_admit(&record(cold, true, 3)),
                OutboundOutcome::Queued { .. }
            ),
            "a mint on another lane has its own worker and must still be accepted"
        );
    }

    #[test]
    fn the_lane_hash_does_not_collapse_on_low_entropy_mints() {
        // Plain FNV-1a over 32 identical bytes lands on ONE lane for every byte value.
        // A real mint is high-entropy, but the router must not silently depend on that
        // — a collapsed router funnels every position through a single worker, which
        // is the exact failure the lanes exist to prevent.
        let inner: &'static RefusingSink = Box::leak(Box::new(RefusingSink));
        let sink = AsyncOutboundSink::new(inner, 4, 4);
        let lanes: std::collections::BTreeSet<usize> =
            (0..=255u8).map(|b| sink.lane_of(&[b; 32])).collect();
        assert!(
            lanes.len() > 1,
            "low-entropy mints must still spread across lanes, got {lanes:?}"
        );
    }

    #[test]
    fn failures_are_reported_verbatim() {
        let inner: &'static RefusingSink = Box::leak(Box::new(RefusingSink));
        let sink = AsyncOutboundSink::new(inner, 2, 4);
        assert!(matches!(
            sink.on_admit(&record(varied_mint(4), false, 7)),
            OutboundOutcome::Queued { .. }
        ));
        let results = drain_until(&sink, 1);
        assert_eq!(results.len(), 1);
        assert!(!results[0].record.is_buy, "side travels with the verdict");
        assert!(matches!(
            results[0].outcome,
            OutboundOutcome::Construction(_)
        ));
    }
}
