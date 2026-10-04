//! The bounded inference worker: runs blocking `ModelSource` calls OFF the engine thread.
//!
//! The engine thread must keep processing the feed, protecting held positions and honouring
//! SAFETY_OFF while a completion is in flight, so the model call can never run inline. This module
//! owns the only threads in the paper-model lane and gives them three hard properties:
//!
//! * **Fixed thread ceiling.** `workers` threads are spawned ONCE at construction and reused. A
//!   timed-out request does not spawn a replacement thread; the worker is simply busy until the
//!   transport's own deadline (`InferenceClient`'s `ureq` overall + connect timeouts) fires and the
//!   call returns. Hung calls therefore cannot accumulate beyond `workers` — the bound
//!   [`crate::model_lane::RequestTable`] mirrors with its `capacity` (set `capacity <= workers`).
//! * **Bounded queue.** The job queue is a `sync_channel` of `queue_depth`; `try_dispatch` NEVER
//!   blocks and refuses when full, so back-pressure surfaces as a named refusal, not a stall.
//! * **Results come back as data.** Workers send a [`Verdict`] over a channel; the engine drains it
//!   with a non-blocking `try_recv` on its own thread and re-validates against FRESH state before
//!   anything is accepted. Nothing in a worker touches engine state.
//!
//! The worker never decides anything: it returns the raw [`Completion`] (or the transport error)
//! tagged with the request identity. Parsing, sizing, vetoes and `decide_entry` stay on the engine
//! thread, where they run against current state.

#![forbid(unsafe_code)]

use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use pump_quant_inference::{Completion, InferenceError};

use crate::model_authority::ModelSource;
use crate::model_lane::RequestId;

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
}

/// A worker's answer. Raw: nothing here has been parsed or vetted.
#[derive(Debug)]
pub struct Verdict {
    /// The request this answers.
    pub id: RequestId,
    /// The mint echoed from the job.
    pub mint: [u8; 32],
    /// The completion, or the transport/inference failure.
    pub result: Result<Completion, InferenceError>,
}

/// Why a dispatch was refused. Never blocks, never queues without limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchRefusal {
    /// The bounded queue is full.
    QueueFull,
    /// The pool has shut down (all workers gone).
    PoolClosed,
}

/// A fixed pool of inference workers.
pub struct InferencePool {
    tx: Option<SyncSender<Job>>,
    rx: Receiver<Verdict>,
    handles: Vec<JoinHandle<()>>,
    workers: usize,
}

impl InferencePool {
    /// Spawn `workers` threads (min 1) sharing one bounded queue of `queue_depth` (min 1).
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
        let (job_tx, job_rx) = mpsc::sync_channel::<Job>(queue_depth.max(1));
        let (verdict_tx, verdict_rx) = mpsc::channel::<Verdict>();
        let job_rx = Arc::new(Mutex::new(job_rx));
        let mut handles = Vec::with_capacity(workers);
        for n in 0..workers {
            let job_rx = Arc::clone(&job_rx);
            let verdict_tx = verdict_tx.clone();
            let source = Arc::clone(&source);
            let handle = std::thread::Builder::new()
                .name(format!("pq-infer-{n}"))
                .spawn(move || loop {
                    // Hold the lock only to take ONE job; it is released before the (slow) call,
                    // so the other workers keep pulling while this one is busy or hung.
                    let job = match job_rx.lock() {
                        Ok(guard) => guard.recv(),
                        Err(_) => return,
                    };
                    let Ok(job) = job else { return };
                    let result = source.complete_meta(&job.system, &job.user);
                    if verdict_tx
                        .send(Verdict {
                            id: job.id,
                            mint: job.mint,
                            result,
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
            tx: Some(job_tx),
            rx: verdict_rx,
            handles,
            workers,
        }
    }

    /// Hand a job to a worker WITHOUT blocking.
    ///
    /// # Errors
    /// [`DispatchRefusal`] — nothing was queued; the caller must also release the request slot it
    /// reserved in the [`crate::model_lane::RequestTable`].
    pub fn try_dispatch(&self, job: Job) -> Result<(), DispatchRefusal> {
        let Some(tx) = &self.tx else {
            return Err(DispatchRefusal::PoolClosed);
        };
        match tx.try_send(job) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(DispatchRefusal::QueueFull),
            Err(TrySendError::Disconnected(_)) => Err(DispatchRefusal::PoolClosed),
        }
    }

    /// Take one finished verdict if any is ready. NEVER blocks — the engine calls this on its own
    /// thread each tick.
    #[must_use]
    pub fn try_recv(&self) -> Option<Verdict> {
        match self.rx.try_recv() {
            Ok(v) => Some(v),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }

    /// The fixed number of worker threads. This, not the request count, bounds hung calls.
    #[must_use]
    pub fn workers(&self) -> usize {
        self.workers
    }

    /// Stop accepting jobs and wait for every worker to finish its current call. Blocks for at most
    /// one transport deadline per worker — used on orderly shutdown, never on the hot path.
    pub fn shutdown(mut self) {
        self.tx = None; // closes the queue; idle workers exit on `recv` error
        for h in self.handles.drain(..) {
            let _ = h.join();
        }
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
