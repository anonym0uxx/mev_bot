//! Hot-path concurrency primitives for the trading bot.
//!
//! This module provides four cooperating primitives:
//!
//! * [`MutexQueue`] — a verified-safe, boring, bounded FIFO queue built on a
//!   single mutex. It is the honest baseline that any lock-free structure must
//!   beat on the benchmark before it is allowed to ship (constitution §57).
//! * [`Spsc`] — a bounded single-producer/single-consumer ring buffer using
//!   acquire/release ordering and cache-line-padded indices.
//! * [`SeqCell`] — a seqlock snapshot cell: one writer publishes a fixed-size
//!   `Copy` state, many readers retry on a torn read.
//! * [`Backoff`] / [`backoff_step`] — deterministic bounded busy-wait backoff
//!   that never issues a syscall inside the hot window.
//!
//! Constitutional discipline observed here:
//! * No `f32`/`f64` anywhere in outcome-controlling logic — integer only.
//! * No allocation after construction in any primitive.
//! * All shared cross-thread indices are cache-line padded (`#[repr(align(64))]`).
//! * Memory orderings are the minimum proven correct.

use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::atomic::{fence, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

// ============================================================================
// lf_mutex_baseline — MutexQueue
// ============================================================================

/// The bounded FIFO queue behind a single mutex. Backing storage is a fixed
/// `[MaybeUninit<T>; N]` ring, so there is zero allocation after construction.
struct RingInner<T, const N: usize> {
    /// Backing storage; only slots in `[head, head+len)` (mod N) are initialized.
    buf: [MaybeUninit<T>; N],
    /// Index of the front element (mod N).
    head: usize,
    /// Number of initialized (live) elements currently stored.
    len: usize,
}

impl<T, const N: usize> RingInner<T, N> {
    fn new() -> Self {
        // SAFETY: an array of `MaybeUninit<T>` requires no initialization; each
        // element is itself allowed to be uninitialized.
        Self {
            buf: unsafe { MaybeUninit::uninit().assume_init() },
            head: 0,
            len: 0,
        }
    }

    fn push(&mut self, v: T) -> Result<(), T> {
        if self.len == N {
            return Err(v);
        }
        let idx = (self.head + self.len) % N;
        self.buf[idx].write(v);
        self.len += 1;
        Ok(())
    }

    fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        let idx = self.head;
        // SAFETY: slot `idx` is within the live range, hence initialized. We read
        // it exactly once and immediately shrink the live range so it is never
        // read again without being re-written.
        let v = unsafe { self.buf[idx].as_ptr().read() };
        self.head = (self.head + 1) % N;
        self.len -= 1;
        Some(v)
    }
}

impl<T, const N: usize> Drop for RingInner<T, N> {
    fn drop(&mut self) {
        // Drop any remaining live elements so we do not leak `T`.
        while self.pop().is_some() {}
    }
}

/// Verified-safe bounded FIFO queue on a single [`Mutex`].
///
/// This is intentionally boring: correctness under arbitrary thread counts is
/// trivial because the mutex serializes every operation. It exists so the
/// lock-free benchmark has an honest opponent (constitution §57). Bounded at
/// `N`; a `push` on a full queue returns `Err(v)` — the value is returned, never
/// dropped. FIFO order is preserved and there is no allocation after `new`.
pub struct MutexQueue<T, const N: usize> {
    inner: Mutex<RingInner<T, N>>,
}

impl<T, const N: usize> MutexQueue<T, N> {
    /// Create an empty queue with capacity `N`.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(RingInner::new()),
        }
    }

    /// Push `v` onto the back. Returns `Err(v)` (value returned) if full.
    pub fn push(&self, v: T) -> Result<(), T> {
        // Recover from a poisoned lock: our invariants are upheld even if a prior
        // holder panicked, since every mutation leaves `RingInner` consistent.
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.push(v)
    }

    /// Pop the front element, or `None` if empty.
    pub fn pop(&self) -> Option<T> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.pop()
    }
}

impl<T, const N: usize> Default for MutexQueue<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// lf_spsc_ring — Spsc / Producer / Consumer
// ============================================================================

/// Cache-line-padded atomic index. Keeping `head` and `tail` on distinct 64-byte
/// lines prevents false sharing between the producer and consumer threads.
#[repr(align(64))]
struct Pad(AtomicUsize);

/// The shared ring backing an SPSC channel.
struct Ring<T, const N: usize> {
    /// Fixed backing storage. Slots are published/consumed via `tail`/`head`.
    buf: [UnsafeCell<MaybeUninit<T>>; N],
    /// Consumer-owned read index (monotone, wraps naturally; masked to a slot).
    head: Pad,
    /// Producer-owned write index (monotone, wraps naturally; masked to a slot).
    tail: Pad,
}

// SAFETY: `T: Send` is required. The split `Producer`/`Consumer` types guarantee
// exactly one producer and one consumer, so the `UnsafeCell`s are never aliased
// mutably across threads for the same slot: the producer only writes a slot
// before publishing it via a Release store to `tail`, and the consumer only
// reads a slot after acquiring that publication.
unsafe impl<T: Send, const N: usize> Sync for Ring<T, N> {}
// SAFETY: sending the ring across threads moves `T` values that are themselves
// `Send`; the atomics are trivially `Send`.
unsafe impl<T: Send, const N: usize> Send for Ring<T, N> {}

impl<T, const N: usize> Ring<T, N> {
    fn new() -> Self {
        // Compile-time enforcement that N is a non-zero power of two, so that
        // `& (N - 1)` is a correct modulo and masking never divides.
        const {
            assert!(
                N.is_power_of_two(),
                "Spsc capacity N must be a power of two"
            );
        }
        // SAFETY: an array of `UnsafeCell<MaybeUninit<T>>` needs no init.
        let buf = unsafe { MaybeUninit::<[UnsafeCell<MaybeUninit<T>>; N]>::uninit().assume_init() };
        Self {
            buf,
            head: Pad(AtomicUsize::new(0)),
            tail: Pad(AtomicUsize::new(0)),
        }
    }
}

impl<T, const N: usize> Drop for Ring<T, N> {
    fn drop(&mut self) {
        // Drain any elements still live in the ring so we do not leak `T`.
        // `Drop` runs single-threaded, so relaxed loads of the owned indices are
        // sound and no synchronization is needed.
        let mut head = self.head.0.load(Ordering::Relaxed);
        let tail = self.tail.0.load(Ordering::Relaxed);
        while head != tail {
            let slot = head & (N - 1);
            // SAFETY: every slot in `[head, tail)` was published and not yet
            // consumed, hence initialized. We read each exactly once.
            unsafe {
                let cell = &mut *self.buf[slot].get();
                cell.as_mut_ptr().drop_in_place();
            }
            head = head.wrapping_add(1);
        }
    }
}

/// Handle used to construct an SPSC ring and split it into its two endpoints.
///
/// `N` must be a power of two (checked at compile time). The channel performs
/// zero allocation after construction.
pub struct Spsc<T, const N: usize> {
    ring: Ring<T, N>,
}

impl<T, const N: usize> Spsc<T, N> {
    /// Create a new, empty SPSC ring.
    pub fn new() -> Self {
        Self { ring: Ring::new() }
    }

    /// Split the ring into its unique [`Producer`] and [`Consumer`] endpoints.
    ///
    /// Neither endpoint is `Clone`, so the single-producer/single-consumer
    /// contract is enforced at the type level.
    pub fn split(self) -> (Producer<T, N>, Consumer<T, N>) {
        let arc = Arc::new(self.ring);
        (Producer(arc.clone()), Consumer(arc))
    }
}

impl<T, const N: usize> Default for Spsc<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

/// The unique producing endpoint of an [`Spsc`] ring. Deliberately not `Clone`.
pub struct Producer<T, const N: usize>(Arc<Ring<T, N>>);

/// The unique consuming endpoint of an [`Spsc`] ring. Deliberately not `Clone`.
pub struct Consumer<T, const N: usize>(Arc<Ring<T, N>>);

// SAFETY: an endpoint may be moved to another thread when `T: Send`; each
// endpoint exposes only its own single-threaded operation.
unsafe impl<T: Send, const N: usize> Send for Producer<T, N> {}
unsafe impl<T: Send, const N: usize> Send for Consumer<T, N> {}

impl<T, const N: usize> Producer<T, N> {
    /// Push `v` into the ring. Returns `Err(v)` if the ring is full
    /// (backpressure — never overwrites an unconsumed slot). Does not spin.
    pub fn push(&mut self, v: T) -> Result<(), T> {
        let r = &*self.0;
        // Producer solely owns `tail`; a relaxed load of it is sound.
        let tail = r.tail.0.load(Ordering::Relaxed);
        // Observe the consumer's progress on `head`.
        let head = r.head.0.load(Ordering::Acquire);
        if tail.wrapping_sub(head) >= N {
            return Err(v); // full -> backpressure
        }
        let slot = tail & (N - 1);
        // SAFETY: this slot is exclusively ours until we publish `tail`; the
        // consumer cannot read it until it observes the Release store below.
        unsafe {
            (*r.buf[slot].get()).as_mut_ptr().write(v);
        }
        // Publish AFTER the slot write; pairs with the consumer's Acquire load.
        r.tail.0.store(tail.wrapping_add(1), Ordering::Release);
        Ok(())
    }
}

impl<T, const N: usize> Consumer<T, N> {
    /// Pop the next element, or `None` if the ring is empty. Does not block.
    pub fn pop(&mut self) -> Option<T> {
        let r = &*self.0;
        // Consumer solely owns `head`; a relaxed load of it is sound.
        let head = r.head.0.load(Ordering::Relaxed);
        // Observe the producer's publication of `tail`.
        let tail = r.tail.0.load(Ordering::Acquire);
        if head == tail {
            return None; // empty
        }
        let slot = head & (N - 1);
        // SAFETY: the producer published this slot (its Release store to `tail`
        // pairs with our Acquire load above). We read it exactly once and then
        // advance `head`, so the slot is never read again before being re-written.
        let v = unsafe { (*r.buf[slot].get()).as_ptr().read() };
        // Free the slot for the producer; pairs with the producer's Acquire load.
        r.head.0.store(head.wrapping_add(1), Ordering::Release);
        Some(v)
    }
}

// ============================================================================
// lf_seqlock — SeqCell
// ============================================================================

/// Bound on reader spins observing an odd sequence before it is treated as a
/// crashed-writer defect (surfaced as a `debug_assert`, not a silent hang).
pub const STUCK_ODD_LIMIT: u32 = 1_000_000_000;

/// A seqlock snapshot cell for single-writer / multi-reader fixed-size state.
///
/// The writer increments `seq` to odd before mutating and to even after
/// (Release). A reader accepts a value only when it observes the same, even
/// sequence both before and after its read (Acquire) — any tear forces a retry,
/// so a reader never returns a half-written snapshot.
///
/// `T: Copy` and fixed-size — no pointers are chased inside the protected region.
pub struct SeqCell<T: Copy> {
    /// Even = stable, odd = a write is in progress.
    seq: AtomicU32,
    /// The protected snapshot.
    data: UnsafeCell<T>,
}

// SAFETY: single-writer-by-contract. Readers only ever read and retry on any
// tear, so no data race can expose a half-written value.
unsafe impl<T: Copy + Send> Sync for SeqCell<T> {}
// SAFETY: moving the cell moves a `Send` `T`; the atomic is `Send`.
unsafe impl<T: Copy + Send> Send for SeqCell<T> {}

impl<T: Copy> SeqCell<T> {
    /// Create a cell holding an initial stable value.
    pub fn new(v: T) -> Self {
        Self {
            seq: AtomicU32::new(0),
            data: UnsafeCell::new(v),
        }
    }

    /// Publish a new value. Single writer by contract.
    pub fn write(&self, v: T) {
        let s = self.seq.load(Ordering::Relaxed);
        // -> odd: signal a write is starting.
        self.seq.store(s.wrapping_add(1), Ordering::Release);
        // Ensure the odd-sequence store is ordered before the data write on all
        // architectures.
        fence(Ordering::Release);
        // SAFETY: the writer is unique by contract; any reader that observes the
        // odd sequence retries and never exposes this half-written value.
        unsafe {
            *self.data.get() = v;
        }
        // -> even: write complete, value is stable again.
        self.seq.store(s.wrapping_add(2), Ordering::Release);
    }

    /// Read a consistent snapshot, retrying until it observes no tear.
    pub fn read(&self) -> T {
        let mut spins: u32 = 0;
        loop {
            let s1 = self.seq.load(Ordering::Acquire);
            if s1 & 1 == 1 {
                // Writer in progress — spin and retry.
                std::hint::spin_loop();
                spins = spins.wrapping_add(1);
                debug_assert!(spins < STUCK_ODD_LIMIT, "writer crashed mid-write");
                continue;
            }
            // SAFETY: `T` is `Copy` and fixed-size; this bitwise read is validated
            // by the sequence re-check below, which rejects any concurrent write.
            let v = unsafe { std::ptr::read(self.data.get()) };
            fence(Ordering::Acquire);
            if self.seq.load(Ordering::Acquire) == s1 {
                // Sequence unchanged and even -> the snapshot is consistent.
                return v;
            }
            // A writer intervened; discard `v` and retry.
            std::hint::spin_loop();
        }
    }
}

// ============================================================================
// lf_backoff — Backoff / backoff_step / Waited
// ============================================================================

/// Spin escalation threshold: below this stage we only emit spin-loop hints.
const SPIN_CAP: u32 = 6;
/// Yield escalation threshold: between `SPIN_CAP` and this we yield the thread.
const YIELD_CAP: u32 = 10;

/// Outcome of a single [`backoff_step`].
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Waited {
    /// Emitted a `spin_loop` hint only — no syscall.
    Spun,
    /// Yielded the current thread to the scheduler.
    Yielded,
    /// Parked the thread (only ever off the hot window).
    Parked,
}

/// Deterministic bounded backoff state.
///
/// Escalation is a pure function of the number of off-hot-window steps taken
/// since construction or [`reset`](Backoff::reset), so a given call count always
/// produces the same outcome.
#[derive(Copy, Clone, Debug)]
pub struct Backoff {
    /// Count of consecutive off-hot-window steps taken (drives escalation).
    stage: u32,
    /// Count of spin-loop hints emitted (diagnostic; used inside the hot window).
    spins: u32,
}

impl Backoff {
    /// Create fresh backoff state at the lowest escalation stage.
    pub fn new() -> Self {
        Self { stage: 0, spins: 0 }
    }

    /// Reset escalation back to the beginning.
    pub fn reset(&mut self) {
        self.stage = 0;
        self.spins = 0;
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

/// Perform one bounded backoff step.
///
/// Inside the hot window (`hot_window == true`) this only ever emits a
/// `spin_loop` hint and returns [`Waited::Spun`] — never a syscall, never a park.
/// Outside the hot window it escalates deterministically: spin, then yield, then
/// park, with the spin stage capped so escalation always reaches a park.
pub fn backoff_step(state: &mut Backoff, hot_window: bool) -> Waited {
    if hot_window {
        std::hint::spin_loop();
        state.spins = state.spins.wrapping_add(1);
        return Waited::Spun;
    }

    let stage = state.stage;
    // Saturate so a very long-lived backoff never wraps back to the spin stage.
    state.stage = state.stage.saturating_add(1);

    if stage < SPIN_CAP {
        std::hint::spin_loop();
        Waited::Spun
    } else if stage < YIELD_CAP {
        std::thread::yield_now();
        Waited::Yielded
    } else {
        // Park briefly with a bounded timeout so a lost wakeup cannot hang the
        // consumer forever.
        std::thread::park_timeout(std::time::Duration::from_micros(50));
        Waited::Parked
    }
}
