//! Bounded junction queue with explicit backpressure (§6/§99).
//!
//! The queue sits between the ingest feeds (PumpPortal WS, Helius accountSubscribe)
//! and the engine. It is bounded — never unbounded (that is the leak we are
//! fixing) — and overflow is counted, not silent (§6 forbids silent drops).
//!
//! This is a single-producer-single-consumer ring buffer. The producer is the
//! WebSocket reader thread; the consumer is the engine tick loop. The queue
//! is allocation-free on the hot path: it pre-allocates a fixed-capacity
//! ring at construction and reuses slots.

use crate::{ProvenancedEvent, JUNCTION_QUEUE_CAP, OverflowStats};

/// Bounded ring buffer for the ingest→junction→engine pipeline.
///
/// SPSC: one producer (the feed reader), one consumer (the engine tick loop).
/// No locks — the ring uses atomic head/tail indices. This is the explicit
/// backpressure boundary.
pub struct BoundedJunctionQueue {
    /// Ring buffer storage, pre-allocated to capacity.
    /// Stored as Vec for stable allocation; accessed circularly.
    buf: std::cell::UnsafeCell<Vec<Option<ProvenancedEvent>>>,
    /// Enqueue timestamps, parallel ring for dwell-time measurement.
    enqueued_at: std::cell::UnsafeCell<Vec<Option<std::time::Instant>>>,
    /// Capacity (must be power-of-2 for mask-based indexing).
    cap_mask: usize,
    /// Head index (consumer). Atomic because the engine thread reads it.
    head: std::sync::atomic::AtomicUsize,
    /// Tail index (producer). Atomic because the feed thread writes it.
    tail: std::sync::atomic::AtomicUsize,
    /// Overflow counter — journalled and surfaced, never silent.
    dropped: std::sync::atomic::AtomicU64,
    /// Slot of the last drop.
    last_drop_slot: std::sync::atomic::AtomicU64,
}

// SAFETY: The queue is accessed from exactly two threads — one producer
// (push) and one consumer (pop). The atomic head/tail indices ensure
// the producer only writes to slots between tail and head, and the
// consumer only reads slots between head and tail. This is the classic
// SPSC ring buffer invariant.
unsafe impl Send for BoundedJunctionQueue {}
unsafe impl Sync for BoundedJunctionQueue {}

impl BoundedJunctionQueue {
    /// Create a new bounded queue with the default capacity.
    /// Pre-allocates the ring — no allocation on the hot path.
    pub fn new() -> Self {
        Self::with_capacity(JUNCTION_QUEUE_CAP)
    }

    /// Create a new bounded queue with a custom capacity (rounded up to
    /// the next power-of-2 for mask-based indexing).
    pub fn with_capacity(capacity: usize) -> Self {
        // Round up to power-of-2.
        let cap = capacity.next_power_of_two();
        let cap_mask = cap - 1;

        let mut buf = Vec::with_capacity(cap);
        let mut ts_buf = Vec::with_capacity(cap);
        for _ in 0..cap {
            buf.push(None);
            ts_buf.push(None);
        }

        Self {
            buf: std::cell::UnsafeCell::new(buf),
            enqueued_at: std::cell::UnsafeCell::new(ts_buf),
            cap_mask,
            head: std::sync::atomic::AtomicUsize::new(0),
            tail: std::sync::atomic::AtomicUsize::new(0),
            dropped: std::sync::atomic::AtomicU64::new(0),
            last_drop_slot: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Push an event into the queue. If the queue is full, the event is
    /// DROPPED and the overflow counter is incremented. §6: never silent.
    ///
    /// Returns true if the event was enqueued, false if dropped.
    pub fn push(&self, event: ProvenancedEvent, slot: u64) -> bool {
        let tail = self.tail.load(std::sync::atomic::Ordering::Relaxed);
        let head = self.head.load(std::sync::atomic::Ordering::Acquire);
        let size = tail.wrapping_sub(head);

        if size > self.cap_mask {
            // Queue full — drop and count. §6: never silent.
            self.dropped
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.last_drop_slot
                .store(slot, std::sync::atomic::Ordering::Relaxed);
            return false;
        }

        let idx = tail & self.cap_mask;
        // SAFETY: The producer owns slots between head and tail. No other
        // thread can write to this slot while we hold the tail index.
        let buf = unsafe { &mut *self.buf.get() };
        buf[idx] = Some(event);
        let ts_buf = unsafe { &mut *self.enqueued_at.get() };
        ts_buf[idx] = Some(std::time::Instant::now());
        self.tail
            .fetch_add(1, std::sync::atomic::Ordering::Release);
        true
    }

    /// Pop an event from the queue. Returns None if empty.
    pub fn pop(&self) -> Option<ProvenancedEvent> {
        let head = self.head.load(std::sync::atomic::Ordering::Relaxed);
        let tail = self.tail.load(std::sync::atomic::Ordering::Acquire);

        if head == tail {
            return None; // empty
        }

        let idx = head & self.cap_mask;
        // SAFETY: The consumer owns slots between head and tail. No other
        // thread can read from this slot while we hold the head index.
        let buf = unsafe { &mut *self.buf.get() };
        let event = buf[idx].take();
        self.head
            .fetch_add(1, std::sync::atomic::Ordering::Release);
        event
    }

    /// Current queue depth (approximate — may be stale due to concurrency).
    pub fn depth(&self) -> usize {
        let tail = self.tail.load(std::sync::atomic::Ordering::Relaxed);
        let head = self.head.load(std::sync::atomic::Ordering::Relaxed);
        tail.wrapping_sub(head)
    }

    /// Overflow statistics since start. The caller is responsible for
    /// journalling and surfacing these — §6 requires visibility, not
    /// silence.
    pub fn overflow_stats(&self) -> OverflowStats {
        OverflowStats {
            dropped: self.dropped.load(std::sync::atomic::Ordering::Relaxed),
            last_drop_slot: self
                .last_drop_slot
                .load(std::sync::atomic::Ordering::Relaxed),
        }
    }

    /// Pop an event and return it together with its dwell time (time spent
    /// in the queue from enqueue to drain). Returns None if the queue is
    /// empty.
    pub fn pop_with_dwell(&self) -> Option<(ProvenancedEvent, std::time::Duration)> {
        let head = self.head.load(std::sync::atomic::Ordering::Relaxed);
        let tail = self.tail.load(std::sync::atomic::Ordering::Acquire);

        if head == tail {
            return None; // empty
        }

        let idx = head & self.cap_mask;
        // SAFETY: The consumer owns slots between head and tail.
        let buf = unsafe { &mut *self.buf.get() };
        let event = buf[idx].take();
        let ts_buf = unsafe { &mut *self.enqueued_at.get() };
        let enqueued = ts_buf[idx].take();
        self.head
            .fetch_add(1, std::sync::atomic::Ordering::Release);

        event.map(|e| {
            let dwell = enqueued
                .map(|t| t.elapsed())
                .unwrap_or_default();
            (e, dwell)
        })
    }
}

impl Default for BoundedJunctionQueue {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pump_quant_app::event::AppEvent;
    use crate::ProvenanceSource;

    fn make_event(slot: u64) -> ProvenancedEvent {
        ProvenancedEvent {
            event: AppEvent::Tick,
            source: ProvenanceSource::PumpPortalTrade,
            slot,
            is_live: true,
        }
    }

    #[test]
    fn test_push_pop_basic() {
        let q = BoundedJunctionQueue::with_capacity(4);
        assert!(q.push(make_event(1), 1));
        assert_eq!(q.depth(), 1);
        let e = q.pop().unwrap();
        assert_eq!(e.slot, 1);
        assert_eq!(q.depth(), 0);
    }

    #[test]
    fn test_push_until_full_then_drop() {
        let q = BoundedJunctionQueue::with_capacity(4);
        // cap=4 (next_power_of_two of 4 = 4, mask=3)
        for i in 0..4 {
            assert!(q.push(make_event(i), i));
        }
        // Queue should be full — next push drops.
        assert!(!q.push(make_event(99), 99));
        let stats = q.overflow_stats();
        assert_eq!(stats.dropped, 1);
        assert_eq!(stats.last_drop_slot, 99);
    }

    #[test]
    fn test_pop_empty() {
        let q = BoundedJunctionQueue::new();
        assert!(q.pop().is_none());
    }

    #[test]
    fn test_fifo_order() {
        let q = BoundedJunctionQueue::with_capacity(8);
        for i in 0..5 {
            q.push(make_event(i), i);
        }
        for i in 0..5 {
            let e = q.pop().unwrap();
            assert_eq!(e.slot, i);
        }
        assert!(q.pop().is_none());
    }

    /// Deliberate overrun (Task 2c): fill the queue past capacity, confirm
    /// the overflow counter increments on every excess push, confirm the
    /// counter is surfaced via overflow_stats(), and confirm the queue depth
    /// never exceeds capacity. A backpressure counter that has never been
    /// observed incrementing is a control that has never been tested.
    #[test]
    fn test_deliberate_overrun_counter_increments_and_surfaces() {
        let cap = 8;
        let q = BoundedJunctionQueue::with_capacity(cap);

        // Fill to capacity.
        for i in 0..cap as u64 {
            assert!(q.push(make_event(i), i), "push {} should succeed", i);
        }
        assert_eq!(q.depth(), cap, "queue should be at capacity");

        // Deliberate overrun: push 20 more past capacity.
        let overrun: u64 = 20;
        for i in 0..overrun {
            let slot = cap as u64 + i;
            assert!(!q.push(make_event(slot), slot), "push past cap should drop");
        }

        // Counter must reflect every drop.
        let stats = q.overflow_stats();
        assert_eq!(stats.dropped, overrun, "overflow counter must equal overrun");
        assert_eq!(
            stats.last_drop_slot,
            cap as u64 + overrun - 1,
            "last_drop_slot must be the final dropped slot"
        );

        // Queue depth must never exceed capacity despite the overrun.
        assert_eq!(q.depth(), cap, "depth must not exceed capacity");

        // Surfacing: the overflow counter is readable and non-zero — this is
        // the value the junction-run binary prints as junction_overflow.
        // A silent counter would be invisible; this one is surfaced.
        assert!(stats.dropped > 0, "overflow counter must be non-zero after overrun");
    }
}
