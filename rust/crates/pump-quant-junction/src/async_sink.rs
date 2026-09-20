//! E3: get outbound submission OFF the decision thread.
//!
//! ## Why this exists
//! `LiveOutboundSink::on_admit` fetches on-chain state, builds, signs, and submits
//! a transaction — a real network round trip (RTT ~5-50 ms, plus whatever the R-3
//! creator-history veto costs on top). The engine calls `on_admit` inline from
//! `tick()`, so every admit paid that latency twice: the decision loop stalled, and
//! so did every other market in the tick's watchlist.
//!
//! `AsyncOutboundSink` wraps any inner sink and moves the work to a single worker
//! thread:
//!
//! - `on_admit` **enqueues** the record and returns immediately with
//!   [`OutboundOutcome::Queued`], carrying a ticket. The engine parks the
//!   bookkeeping it owes (see `Engine::note_inflight_outbound`) and keeps the
//!   position open — a queued submission is not a failed one.
//! - the worker runs the *same* inner pipeline and pushes the real outcome into a
//!   result channel.
//! - the daemon drains that channel on its tick and reports each verdict back into
//!   the engine (`complete_async_outbound` / `fail_async_outbound`), which is where
//!   the accounting and the phantom-position rollback live.
//!
//! ## Bounds and failure direction
//! The job queue is **bounded**. A full queue refuses the submission with
//! `OutboundOutcome::Sender` rather than blocking the decision thread: the engine
//! then treats it exactly as a synchronous sink failure (the position is reversed),
//! which is the safe direction — capital is not committed to a transaction that was
//! never handed off. Refusals are counted (`refused`), so a saturated queue is
//! visible rather than silent.
//!
//! ## Ordering
//! One worker thread preserves submission order (FIFO), which matters for a sell
//! queued behind its own buy.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::Mutex;
use std::thread;
use std::time::Instant;

use pump_quant_execution::ex_outbound_sink::{AdmitRecord, OutboundOutcome, OutboundSink};

/// One completed submission, as reported by the worker.
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
}

/// Wraps an inner [`OutboundSink`] and runs it on a dedicated worker thread.
pub struct AsyncOutboundSink {
    jobs: SyncSender<(u64, AdmitRecord)>,
    results: Mutex<Receiver<OutboundResult>>,
    next_ticket: AtomicU64,
    in_flight: AtomicU64,
    refused: AtomicU64,
    delivered: AtomicU64,
}

impl AsyncOutboundSink {
    /// Wrap `inner`, spawning the worker. `queue_depth` bounds the backlog; a full
    /// queue refuses (never blocks) the caller.
    pub fn new(inner: &'static dyn OutboundSink, queue_depth: usize) -> Self {
        let (jobs_tx, jobs_rx) = mpsc::sync_channel::<(u64, AdmitRecord)>(queue_depth.max(1));
        let (results_tx, results_rx) = mpsc::channel::<OutboundResult>();
        // The worker owns the inner sink for the process lifetime. It exits only if
        // the job sender or the result receiver goes away — for a `'static` sink
        // installed in the daemon that means process exit, which is intended: a
        // submission in flight must never be abandoned by a shutdown race.
        thread::Builder::new()
            .name("outbound-worker".to_string())
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
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .expect("spawn outbound worker thread");
        Self {
            jobs: jobs_tx,
            results: Mutex::new(results_rx),
            next_ticket: AtomicU64::new(1),
            in_flight: AtomicU64::new(0),
            refused: AtomicU64::new(0),
            delivered: AtomicU64::new(0),
        }
    }

    /// Drain every verdict the worker has produced since the last call. Non-blocking.
    pub fn drain_results(&self) -> Vec<OutboundResult> {
        let rx = self.results.lock().unwrap();
        rx.try_iter().collect()
    }

    /// Submissions handed off and not yet reported back. Diagnostic.
    #[must_use]
    pub fn in_flight(&self) -> u64 {
        self.in_flight.load(Ordering::Relaxed)
    }

    /// Submissions refused because the queue was full. Diagnostic.
    #[must_use]
    pub fn refused(&self) -> u64 {
        self.refused.load(Ordering::Relaxed)
    }

    /// Verdicts drained and reported back to the engine. Diagnostic.
    #[must_use]
    pub fn delivered(&self) -> u64 {
        self.delivered.load(Ordering::Relaxed)
    }
}

impl OutboundSink for AsyncOutboundSink {
    fn on_admit(&self, record: &AdmitRecord) -> OutboundOutcome {
        let ticket = self.next_ticket.fetch_add(1, Ordering::Relaxed);
        match self.jobs.try_send((ticket, *record)) {
            Ok(()) => {
                self.in_flight.fetch_add(1, Ordering::Relaxed);
                OutboundOutcome::Queued { ticket }
            }
            // A full queue must never block the decision thread. Refusing makes the
            // engine treat this exactly as a synchronous sink failure — the position
            // is reversed instead of being committed to a record that has no worker.
            Err(TrySendError::Full(_)) => {
                self.refused.fetch_add(1, Ordering::Relaxed);
                OutboundOutcome::Sender(
                    "outbound queue full — submission refused without blocking the decision thread"
                        .to_string(),
                )
            }
            Err(TrySendError::Disconnected(_)) => {
                self.refused.fetch_add(1, Ordering::Relaxed);
                OutboundOutcome::Sender("outbound worker is gone".to_string())
            }
        }
    }
}

/// The engine decrements this when a verdict is reported back, so the in-flight
/// count tracks the worker's own bookkeeping.
impl AsyncOutboundSink {
    /// Called by the drainer once a verdict has been delivered to the engine.
    pub fn note_delivered(&self) {
        self.in_flight.fetch_sub(1, Ordering::Relaxed);
        self.delivered.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
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

    fn record(is_buy: bool) -> AdmitRecord {
        AdmitRecord {
            mint: [9u8; 32],
            user: [0u8; 32],
            is_buy,
            size_lamports: 1_000,
            entry_price: 42,
            max_slippage_bps: 500,
        }
    }

    #[test]
    fn on_admit_returns_before_the_work_happens() {
        let inner: &'static SlowSink = Box::leak(Box::new(SlowSink {
            delay: Duration::from_millis(120),
            seen: AtomicBool::new(false),
        }));
        let sink = AsyncOutboundSink::new(inner, 8);
        let started = Instant::now();
        let outcome = sink.on_admit(&record(true));
        let handoff_us = started.elapsed().as_micros();
        assert!(
            matches!(outcome, OutboundOutcome::Queued { ticket } if ticket == 1),
            "a queued record must report Queued, not an outcome it cannot know yet"
        );
        assert!(
            handoff_us < 60_000,
            "handoff must not wait for the 120 ms inner sink (took {handoff_us} µs)"
        );
        // The worker still runs the real pipeline and reports the real verdict.
        let mut results = Vec::new();
        for _ in 0..200 {
            results = sink.drain_results();
            if !results.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
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
    fn a_full_queue_refuses_instead_of_blocking() {
        // Depth 1, and the worker is parked behind a slow first job.
        let inner: &'static SlowSink = Box::leak(Box::new(SlowSink {
            delay: Duration::from_millis(300),
            seen: AtomicBool::new(false),
        }));
        let sink = AsyncOutboundSink::new(inner, 1);
        assert!(matches!(
            sink.on_admit(&record(true)),
            OutboundOutcome::Queued { .. }
        ));
        // The worker is busy, the channel holds one → this one must be refused.
        let started = Instant::now();
        match sink.on_admit(&record(true)) {
            OutboundOutcome::Sender(msg) => assert!(msg.contains("queue full"), "{msg}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(
            started.elapsed().as_millis() < 200,
            "refusal must be immediate — blocking the decision thread is the bug"
        );
        assert_eq!(sink.refused(), 1);
    }

    #[test]
    fn failures_are_reported_verbatim() {
        let inner: &'static RefusingSink = Box::leak(Box::new(RefusingSink));
        let sink = AsyncOutboundSink::new(inner, 4);
        assert!(matches!(
            sink.on_admit(&record(false)),
            OutboundOutcome::Queued { .. }
        ));
        let mut results = Vec::new();
        for _ in 0..200 {
            results = sink.drain_results();
            if !results.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(results.len(), 1);
        assert!(!results[0].record.is_buy, "side travels with the verdict");
        assert!(matches!(results[0].outcome, OutboundOutcome::Construction(_)));
    }
}