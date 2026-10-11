//! The bounded inference worker: runs blocking `ModelSource` calls OFF the engine thread.
//!
//! The engine thread must keep processing the feed, protecting held positions and honouring
//! SAFETY_OFF while a completion is in flight, so the model call can never run inline. This module
//! owns the only threads in the paper-model lane and gives them these hard properties:
//!
//! * **Fixed thread ceiling.** `workers` threads are spawned ONCE at construction and reused. A
//!   timed-out request does not spawn a replacement thread; the worker is simply busy until the
//!   transport's own deadline fires and the call returns. Hung calls cannot accumulate beyond `workers`.
//! * **Bounded queues, management first.** Two FIFO queues (management, entry) under one lock, each bounded
//!   at `queue_depth` and bounded TOGETHER at `queue_depth`. `try_dispatch` NEVER blocks and refuses when full
//!   (a named local refusal, never an endpoint failure). A free worker takes MANAGEMENT first; anti-starvation:
//!   after [`MGMT_STREAK_MAX`] consecutive management takes while an entry job waits, the next take is the
//!   oldest entry job, so an entry job waits behind at most `MGMT_STREAK_MAX` management jobs per worker take.
//! * **Queued expiry (no send after the deadline).** Every job carries its engine-clock deadline fixed at
//!   SUBMIT. A worker that dequeues a job whose deadline has already passed on the ENGINE clock (published by
//!   the engine into [`InferencePool::clock`]) does NOT call the model: no HTTP, no GPU work. It returns a
//!   [`Verdict`] with `local_expired = true`. The deadline is never restarted at worker start.
//! * **Results come back as data.** Workers send a [`Verdict`] over a channel; the engine drains it
//!   with a non-blocking `try_recv` on its own thread and re-validates against FRESH state.
//!
//! The worker never decides anything: it returns the raw [`Completion`] (or the transport error)
//! tagged with the request identity and its engine-clock timeline stamps.

#![forbid(unsafe_code)]

use std::collections::{BTreeSet, VecDeque};
use std::sync::atomic::{AtomicI64, Ordering as AtOrd};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use pump_quant_inference::{Completion, InferenceError};

use crate::model_authority::ModelSource;
use crate::model_lane::RequestId;

/// Anti-starvation bound: consecutive management takes allowed while an entry job is waiting.
pub const MGMT_STREAK_MAX: u32 = 2;

/// Which lane a job belongs to (queue priority).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JobKind {
    /// A new-entry question.
    #[default]
    Entry,
    /// A held-position management question (taken first).
    Mgmt,
}

impl JobKind {
    /// Stable label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            JobKind::Entry => "entry",
            JobKind::Mgmt => "mgmt",
        }
    }
}

/// One unit of work: the two prompts plus the identity the answer must carry back.
#[derive(Debug, Clone)]
pub struct Job {
    /// The request this answers.
    pub id: RequestId,
    /// The mint the decision concerns (echoed back so acceptance can cross-check it).
    pub mint: [u8; 32],
    /// System prompt (byte-parity renderer output).
    pub system: String,
    /// User prompt (byte-parity renderer output).
    pub user: String,
    /// The engine process-session that issued this job. Request ids restart at 1 in every process, so the id alone
    /// cannot tell an answer to THIS process's request from a late answer to an abandoned process's request.
    pub session: u64,
    /// Queue lane.
    pub kind: JobKind,
    /// Absolute ENGINE-clock deadline, copied from the request table's submit (never restarted).
    /// `i64::MAX` = no deadline (unit tests only).
    pub deadline_ms: i64,
}

/// Engine-clock timeline of one request inside the pool (ms; `None` = did not happen).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stamps {
    /// Accepted into the local queue.
    pub enqueue_ms: Option<i64>,
    /// Taken by a worker.
    pub worker_start_ms: Option<i64>,
    /// The model call (HTTP send) started.
    pub http_send_ms: Option<i64>,
    /// The model call returned.
    pub completion_ms: Option<i64>,
}

/// A worker's answer. Raw: nothing here has been parsed or vetted.
#[derive(Debug)]
pub struct Verdict {
    /// The request this answers.
    pub id: RequestId,
    /// The mint echoed from the job.
    pub mint: [u8; 32],
    /// The session echoed from the job (see [`Job::session`]).
    pub session: u64,
    /// The completion, or the transport/inference failure. For a `local_expired` verdict this is an
    /// `Err(Transport("local_queue_expired"))` placeholder that must never be read as endpoint evidence.
    pub result: Result<Completion, InferenceError>,
    /// The job was dequeued after its deadline and NEVER sent (no HTTP, no GPU work).
    pub local_expired: bool,
    /// Engine-clock stamps.
    pub stamps: Stamps,
    /// Queue lane.
    pub kind: JobKind,
}

/// C1 per-request record (engine clock, ms). Keyed by `id` + `kind`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestRecord {
    /// Request id (management ids start at `1 << 40`).
    pub id: u64,
    /// "entry" or "mgmt".
    pub kind: &'static str,
    /// Market.
    pub mint: [u8; 32],
    /// Submit: the engine start point of the 3 s deadline.
    pub created_ms: i64,
    /// The engine deadline fixed at submit.
    pub deadline_ms: i64,
    /// Local enqueue.
    pub enqueue_ms: Option<i64>,
    /// Worker start (dequeue).
    pub worker_start_ms: Option<i64>,
    /// HTTP send (model call start). `None` for a request never sent.
    pub http_send_ms: Option<i64>,
    /// Model call returned.
    pub completion_ms: Option<i64>,
    /// Engine acceptance / abandonment / refusal time (first terminal outcome).
    pub resolved_ms: Option<i64>,
    /// First terminal outcome: `accepted`, `abandoned_deadline` (sent; endpoint wait), `local_queue_expired`,
    /// `local_capacity_overload`, or a `discard:*` / `mgmt:discard:*` label.
    pub outcome: Option<&'static str>,
    /// A later event on the same request (e.g. the late arrival after abandonment).
    pub late: Option<&'static str>,
    /// Whether the request was actually sent to the endpoint (known once a verdict came back).
    pub sent: bool,
    /// Parsed action of the completion (`BUY`, `HOLD`, ...), when it parsed.
    pub action: Option<&'static str>,
    /// Server finish reason.
    pub finish_reason: Option<String>,
}

/// Why a dispatch was refused. Never blocks, never queues without limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchRefusal {
    /// The bounded queue is full.
    QueueFull,
    /// The pool has shut down (all workers gone).
    PoolClosed,
}

/// Where a request is inside the pool, as seen by the engine at its deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalStatus {
    /// It was still queued: removed now; it will never be sent and no verdict will come.
    CancelledQueued,
    /// A worker dequeued it after its deadline and refused it unsent; a `local_expired` verdict is coming.
    ExpiredUnsent,
    /// A worker started it (sent, or about to send): a real endpoint wait.
    Started,
}

#[derive(Default)]
struct Queues {
    mgmt: VecDeque<(Job, i64)>,
    entry: VecDeque<(Job, i64)>,
    mgmt_streak: u32,
    expired_unsent: BTreeSet<RequestId>,
    closed: bool,
}

impl Queues {
    fn len(&self) -> usize {
        self.mgmt.len() + self.entry.len()
    }
    /// THE priority rule (C5): management first, bounded by the anti-starvation streak.
    fn take(&mut self) -> Option<(Job, i64)> {
        let entry_waits = !self.entry.is_empty();
        if !self.mgmt.is_empty() && !(entry_waits && self.mgmt_streak >= MGMT_STREAK_MAX) {
            self.mgmt_streak = if entry_waits { self.mgmt_streak + 1 } else { 0 };
            return self.mgmt.pop_front();
        }
        self.mgmt_streak = 0;
        self.entry.pop_front()
    }
}

struct Shared {
    q: Mutex<Queues>,
    cv: Condvar,
    depth: usize,
}

/// A fixed pool of inference workers.
pub struct InferencePool {
    shared: Arc<Shared>,
    rx: Receiver<Verdict>,
    handles: Vec<JoinHandle<()>>,
    workers: usize,
    clock: Arc<AtomicI64>,
}

fn stamp(c: &AtomicI64) -> Option<i64> {
    Some(c.load(AtOrd::SeqCst))
}

impl InferencePool {
    /// Spawn `workers` threads (min 1) sharing bounded queues of `queue_depth` (min 1) in total and per lane.
    ///
    /// `source` is shared by every worker, so it must be `Send + Sync` — the real
    /// `InferenceClient` (a `ureq::Agent`) is.
    #[must_use]
    pub fn new(
        source: Arc<dyn ModelSource + Send + Sync>,
        workers: usize,
        queue_depth: usize,
    ) -> Self {
        let workers = workers.max(1);
        let shared = Arc::new(Shared {
            q: Mutex::new(Queues::default()),
            cv: Condvar::new(),
            depth: queue_depth.max(1),
        });
        let clock = Arc::new(AtomicI64::new(i64::MIN));
        let (verdict_tx, verdict_rx) = mpsc::channel::<Verdict>();
        let mut handles = Vec::with_capacity(workers);
        for n in 0..workers {
            let shared = Arc::clone(&shared);
            let verdict_tx = verdict_tx.clone();
            let source = Arc::clone(&source);
            let clock = Arc::clone(&clock);
            let handle = std::thread::Builder::new()
                .name(format!("pq-infer-{n}"))
                .spawn(move || loop {
                    // Hold the lock only to take ONE job (and decide its expiry atomically with the engine's
                    // cancel); it is released before the (slow) call.
                    let (job, enq, expired, start) = {
                        let Ok(mut g) = shared.q.lock() else { return };
                        loop {
                            if let Some(x) = g.take() {
                                break Some(x);
                            }
                            if g.closed {
                                break None;
                            }
                            g = match shared.cv.wait(g) {
                                Ok(g) => g,
                                Err(_) => return,
                            };
                        }
                        .map(|(job, enq)| {
                            let now = clock.load(AtOrd::SeqCst);
                            // C3: past the deadline on the ENGINE clock -> refuse unsent.
                            let expired = now > job.deadline_ms;
                            if expired {
                                g.expired_unsent.insert(job.id);
                            }
                            (job, enq, expired, now)
                        })
                        .map_or((None, 0, false, 0), |(j, e, x, s)| (Some(j), e, x, s))
                    };
                    let Some(job) = job else { return };
                    let mut stamps = Stamps {
                        enqueue_ms: Some(enq),
                        worker_start_ms: Some(start),
                        ..Stamps::default()
                    };
                    let result = if expired {
                        Err(InferenceError::Transport("local_queue_expired".into()))
                    } else {
                        stamps.http_send_ms = stamp(&clock);
                        let r = source.complete_meta(&job.system, &job.user);
                        stamps.completion_ms = stamp(&clock);
                        r
                    };
                    if verdict_tx
                        .send(Verdict {
                            id: job.id,
                            mint: job.mint,
                            session: job.session,
                            result,
                            local_expired: expired,
                            stamps,
                            kind: job.kind,
                        })
                        .is_err()
                    {
                        return; // the engine side is gone
                    }
                })
                .expect("spawn inference worker");
            handles.push(handle);
        }
        Self {
            shared,
            rx: verdict_rx,
            handles,
            workers,
            clock,
        }
    }

    /// The engine clock the workers read for the dequeue deadline check. The engine publishes into it.
    #[must_use]
    pub fn clock(&self) -> Arc<AtomicI64> {
        Arc::clone(&self.clock)
    }

    /// Publish the engine clock (monotone use by the engine).
    pub fn publish_clock(&self, ms: i64) {
        self.clock.fetch_max(ms, AtOrd::SeqCst);
    }

    /// Hand a job to a worker WITHOUT blocking.
    ///
    /// # Errors
    /// [`DispatchRefusal`] — nothing was queued; the caller must also release the request slot it
    /// reserved in the [`crate::model_lane::RequestTable`].
    pub fn try_dispatch(&self, job: Job) -> Result<(), DispatchRefusal> {
        let Ok(mut g) = self.shared.q.lock() else {
            return Err(DispatchRefusal::PoolClosed);
        };
        if g.closed {
            return Err(DispatchRefusal::PoolClosed);
        }
        let lane_len = match job.kind {
            JobKind::Mgmt => g.mgmt.len(),
            JobKind::Entry => g.entry.len(),
        };
        if g.len() >= self.shared.depth || lane_len >= self.shared.depth {
            return Err(DispatchRefusal::QueueFull);
        }
        let enq = self.clock.load(AtOrd::SeqCst);
        match job.kind {
            JobKind::Mgmt => g.mgmt.push_back((job, enq)),
            JobKind::Entry => g.entry.push_back((job, enq)),
        }
        drop(g);
        self.shared.cv.notify_one();
        Ok(())
    }

    /// At a request's deadline: where is it? A still-queued job is REMOVED (never sent, no verdict will come).
    #[must_use]
    pub fn local_status(&self, id: RequestId) -> LocalStatus {
        let Ok(mut g) = self.shared.q.lock() else {
            return LocalStatus::Started;
        };
        for lane in [0, 1] {
            let q = if lane == 0 { &mut g.mgmt } else { &mut g.entry };
            if let Some(pos) = q.iter().position(|(j, _)| j.id == id) {
                q.remove(pos);
                return LocalStatus::CancelledQueued;
            }
        }
        if g.expired_unsent.remove(&id) {
            return LocalStatus::ExpiredUnsent;
        }
        LocalStatus::Started
    }

    /// Put back an expired-unsent mark consumed by [`Self::local_status`] (the verdict is still on its way).
    pub fn remark_expired(&self, id: RequestId) {
        if let Ok(mut g) = self.shared.q.lock() {
            g.expired_unsent.insert(id);
        }
    }

    /// Forget an expired-unsent mark once its verdict has been consumed (bounded bookkeeping).
    pub fn forget_expired(&self, id: RequestId) {
        if let Ok(mut g) = self.shared.q.lock() {
            g.expired_unsent.remove(&id);
        }
    }

    /// Jobs waiting in the local queues (management, entry).
    #[must_use]
    pub fn queued(&self) -> (usize, usize) {
        self.shared
            .q
            .lock()
            .map_or((0, 0), |g| (g.mgmt.len(), g.entry.len()))
    }

    /// The queue bound (total and per lane).
    #[must_use]
    pub fn queue_depth(&self) -> usize {
        self.shared.depth
    }

    /// Take one finished verdict if any is ready. NEVER blocks — the engine calls this on its own
    /// thread each tick.
    #[must_use]
    pub fn try_recv(&self) -> Option<Verdict> {
        self.rx.try_recv().ok()
    }

    /// Wait up to `d` for one verdict. OFFLINE BARRIER SETTLE ONLY: the production engine thread never blocks on the
    /// pool (it uses [`Self::try_recv`]).
    #[must_use]
    pub fn recv_timeout(&self, d: std::time::Duration) -> Option<Verdict> {
        self.rx.recv_timeout(d).ok()
    }

    /// The fixed number of worker threads. This, not the request count, bounds hung calls.
    #[must_use]
    pub fn workers(&self) -> usize {
        self.workers
    }

    /// Stop accepting jobs and wait for every worker to finish its current call. Blocks for at most
    /// one transport deadline per worker — used on orderly shutdown, never on the hot path.
    pub fn shutdown(mut self) {
        if let Ok(mut g) = self.shared.q.lock() {
            g.closed = true;
        }
        self.shared.cv.notify_all();
        for h in self.handles.drain(..) {
            let _ = h.join();
        }
    }
}

impl Drop for InferencePool {
    fn drop(&mut self) {
        if let Ok(mut g) = self.shared.q.lock() {
            g.closed = true;
        }
        self.shared.cv.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::channel;
    use std::time::{Duration, Instant};

    const A: [u8; 32] = [1u8; 32];
    const B: [u8; 32] = [2u8; 32];

    struct Fixed(&'static str);
    impl ModelSource for Fixed {
        fn complete(&self, _s: &str, _u: &str) -> Result<String, InferenceError> {
            Ok(self.0.to_string())
        }
    }

    /// Blocks until released — a model that hangs.
    struct Gate {
        release: Mutex<Receiver<()>>,
        entered: AtomicUsize,
    }
    impl ModelSource for Gate {
        fn complete(&self, _s: &str, _u: &str) -> Result<String, InferenceError> {
            self.entered.fetch_add(1, Ordering::SeqCst);
            let _ = self.release.lock().unwrap().recv();
            Ok("DECISION: SKIP\nSIZE: NONE".to_string())
        }
    }

    fn job(id: u64, mint: [u8; 32]) -> Job {
        Job {
            id: RequestId(id),
            mint,
            system: "S".into(),
            user: "U".into(),
            session: 0,
            kind: JobKind::Entry,
            deadline_ms: i64::MAX,
        }
    }

    fn wait_for(pool: &InferencePool, n: usize) -> Vec<Verdict> {
        let t0 = Instant::now();
        let mut out = Vec::new();
        while out.len() < n && t0.elapsed() < Duration::from_secs(5) {
            if let Some(v) = pool.try_recv() {
                out.push(v);
            } else {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        out
    }

    #[test]
    fn a_dispatched_job_returns_its_verdict_tagged_with_its_identity() {
        let pool = InferencePool::new(Arc::new(Fixed("DECISION: SKIP\nSIZE: NONE")), 1, 4);
        pool.try_dispatch(job(7, A)).unwrap();
        let v = wait_for(&pool, 1);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].id, RequestId(7));
        assert_eq!(v[0].mint, A);
        assert!(v[0].result.is_ok());
        pool.shutdown();
    }

    #[test]
    fn try_recv_never_blocks_when_nothing_is_ready() {
        let pool = InferencePool::new(Arc::new(Fixed("x")), 1, 1);
        let t0 = Instant::now();
        assert!(pool.try_recv().is_none());
        assert!(t0.elapsed() < Duration::from_millis(50), "must not block");
        pool.shutdown();
    }

    /// THE BOUND. With one worker hung inside the model, dispatch still returns immediately, the
    /// queue fills, and the next dispatch is REFUSED — not blocked, not a new thread.
    #[test]
    fn a_hung_worker_cannot_stall_the_caller_or_grow_the_thread_count() {
        let (release_tx, release_rx) = channel::<()>();
        let gate = Arc::new(Gate {
            release: Mutex::new(release_rx),
            entered: AtomicUsize::new(0),
        });
        let pool = InferencePool::new(gate.clone(), 1, 1);
        assert_eq!(pool.workers(), 1);

        let t0 = Instant::now();
        pool.try_dispatch(job(1, A)).unwrap(); // taken by the worker, which then hangs
        let t_wait = Instant::now();
        while gate.entered.load(Ordering::SeqCst) == 0 && t_wait.elapsed() < Duration::from_secs(5)
        {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            gate.entered.load(Ordering::SeqCst),
            1,
            "worker is inside the model"
        );

        pool.try_dispatch(job(2, B)).unwrap(); // fills the one queue slot
        assert_eq!(
            pool.try_dispatch(job(3, [3u8; 32])),
            Err(DispatchRefusal::QueueFull),
            "refused, not blocked"
        );
        assert!(
            t0.elapsed() < Duration::from_secs(2),
            "the caller was never stalled by the hung worker"
        );
        // Only ONE thread ever entered the model despite three dispatch attempts.
        assert_eq!(gate.entered.load(Ordering::SeqCst), 1);

        // Release both calls so shutdown can join.
        release_tx.send(()).unwrap();
        release_tx.send(()).unwrap();
        let v = wait_for(&pool, 2);
        assert_eq!(v.len(), 2);
        pool.shutdown();
    }

    fn kjob(id: u64, kind: JobKind, deadline_ms: i64) -> Job {
        Job {
            kind,
            deadline_ms,
            ..job(id, [id as u8; 32])
        }
    }

    /// Records the order in which prompts reach the model ("U<id>").
    struct Order {
        seen: Mutex<Vec<String>>,
        release: Mutex<Receiver<()>>,
        entered: AtomicUsize,
    }
    impl ModelSource for Order {
        fn complete(&self, _s: &str, u: &str) -> Result<String, InferenceError> {
            if self.entered.fetch_add(1, Ordering::SeqCst) == 0 {
                let _ = self.release.lock().unwrap().recv(); // the first call wedges the only worker
            }
            self.seen.lock().unwrap().push(u.to_string());
            Ok("DECISION: SKIP".to_string())
        }
    }

    /// C3: a job dequeued after its ENGINE-clock deadline is never sent (no model call); it comes back as a
    /// `local_expired` verdict with no http_send stamp. A job inside its deadline is sent.
    #[test]
    fn a_job_dequeued_past_its_engine_deadline_is_never_sent() {
        struct Count(AtomicUsize);
        impl ModelSource for Count {
            fn complete(&self, _s: &str, _u: &str) -> Result<String, InferenceError> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok("DECISION: SKIP".into())
            }
        }
        let src = Arc::new(Count(AtomicUsize::new(0)));
        let pool = InferencePool::new(src.clone(), 1, 4);
        pool.publish_clock(10_000);
        pool.try_dispatch(kjob(1, JobKind::Entry, 9_999)).unwrap();
        let v = wait_for(&pool, 1);
        assert_eq!(v.len(), 1);
        assert!(v[0].local_expired, "dequeued past its deadline");
        assert_eq!(v[0].stamps.http_send_ms, None, "never sent");
        assert_eq!(v[0].stamps.worker_start_ms, Some(10_000));
        assert_eq!(
            src.0.load(Ordering::SeqCst),
            0,
            "no model call / no GPU work"
        );
        // Exactly at the deadline is still in time (deadline is inclusive, like RequestTable::accept).
        pool.try_dispatch(kjob(2, JobKind::Entry, 10_000)).unwrap();
        let v = wait_for(&pool, 1);
        assert!(!v[0].local_expired);
        assert_eq!(v[0].stamps.http_send_ms, Some(10_000));
        assert_eq!(src.0.load(Ordering::SeqCst), 1);
        pool.shutdown();
    }

    /// C5: management is taken before entry; after MGMT_STREAK_MAX (2) consecutive management takes while an entry
    /// waits, the oldest entry goes next (bounded wait, no starvation).
    #[test]
    fn management_is_taken_first_and_entry_waits_at_most_two_management_jobs() {
        assert_eq!(MGMT_STREAK_MAX, 2);
        let (tx, rx) = channel::<()>();
        let src = Arc::new(Order {
            seen: Mutex::new(Vec::new()),
            release: Mutex::new(rx),
            entered: AtomicUsize::new(0),
        });
        let pool = InferencePool::new(src.clone(), 1, 8);
        let mut wedge = kjob(100, JobKind::Entry, i64::MAX);
        wedge.user = "U100".into();
        pool.try_dispatch(wedge).unwrap();
        let t = Instant::now();
        while src.entered.load(Ordering::SeqCst) == 0 && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(1));
        }
        for (id, k) in [
            (1, JobKind::Entry),
            (2, JobKind::Entry),
            (3, JobKind::Mgmt),
            (4, JobKind::Mgmt),
            (5, JobKind::Mgmt),
            (6, JobKind::Mgmt),
        ] {
            let mut j = kjob(id, k, i64::MAX);
            j.user = format!("U{id}");
            pool.try_dispatch(j).unwrap();
        }
        assert_eq!(pool.queued(), (4, 2));
        tx.send(()).unwrap();
        let v = wait_for(&pool, 7);
        assert_eq!(v.len(), 7);
        let seen = src.seen.lock().unwrap().clone();
        assert_eq!(seen, ["U100", "U3", "U4", "U1", "U5", "U6", "U2"]);
        pool.shutdown();
    }

    /// C2/C6 bound: total and per-lane queue depth; a full lane refuses by name, never blocks or grows.
    #[test]
    fn the_queue_is_bounded_in_total_and_per_lane() {
        let (tx, rx) = channel::<()>();
        let gate = Arc::new(Gate {
            release: Mutex::new(rx),
            entered: AtomicUsize::new(0),
        });
        let pool = InferencePool::new(gate.clone(), 1, 2);
        pool.try_dispatch(kjob(1, JobKind::Mgmt, i64::MAX)).unwrap();
        let t = Instant::now();
        while gate.entered.load(Ordering::SeqCst) == 0 && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(1));
        }
        pool.try_dispatch(kjob(2, JobKind::Mgmt, i64::MAX)).unwrap();
        pool.try_dispatch(kjob(3, JobKind::Entry, i64::MAX))
            .unwrap();
        assert_eq!(
            pool.try_dispatch(kjob(4, JobKind::Entry, i64::MAX)),
            Err(DispatchRefusal::QueueFull)
        );
        assert_eq!(
            pool.try_dispatch(kjob(5, JobKind::Mgmt, i64::MAX)),
            Err(DispatchRefusal::QueueFull)
        );
        assert_eq!(pool.queued(), (1, 1));
        // A queued job cancelled at its deadline is removed exactly once; a second lookup does not find it.
        assert_eq!(
            pool.local_status(RequestId(3)),
            LocalStatus::CancelledQueued
        );
        assert_eq!(pool.local_status(RequestId(3)), LocalStatus::Started);
        assert_eq!(pool.queued(), (1, 0));
        for _ in 0..2 {
            tx.send(()).unwrap();
        }
        assert_eq!(wait_for(&pool, 2).len(), 2, "job 3 was never run");
        pool.shutdown();
    }

    #[test]
    fn a_failing_model_comes_back_as_an_error_verdict_not_a_panic() {
        struct Dead;
        impl ModelSource for Dead {
            fn complete(&self, _s: &str, _u: &str) -> Result<String, InferenceError> {
                Err(InferenceError::Unparseable("down".into()))
            }
        }
        let pool = InferencePool::new(Arc::new(Dead), 1, 2);
        pool.try_dispatch(job(1, A)).unwrap();
        let v = wait_for(&pool, 1);
        assert!(v[0].result.is_err());
        pool.shutdown();
    }
}
