//! EventJoiner — merges PumpPortal, Helius, ShredStream (optional), and timer
//! events into a single ordered stream for the engine hot-path thread.

use crossbeam_channel::{Receiver, Sender, select};
use tracing::debug;

use crate::feeds::FeedEvent;

pub struct EventJoiner {
    pumpportal_rx: Receiver<FeedEvent>,
    helius_rx: Receiver<FeedEvent>,
    shredstream_rx: Option<Receiver<FeedEvent>>,
    tick_rx: Receiver<FeedEvent>,
    engine_tx: Sender<FeedEvent>,
}

impl EventJoiner {
    pub fn new(
        pumpportal_rx: Receiver<FeedEvent>,
        helius_rx: Receiver<FeedEvent>,
        shredstream_rx: Option<Receiver<FeedEvent>>,
        engine_tx: Sender<FeedEvent>,
    ) -> Self {
        // Create internal tick channel — joiner owns the sender, runtime spawns the generator
        let (tick_tx, tick_rx) = crossbeam_channel::bounded::<FeedEvent>(8);

        // Spawn tick generator as a std thread (avoids tokio dependency here)
        std::thread::spawn(move || {
            let interval = std::time::Duration::from_millis(50);
            loop {
                std::thread::sleep(interval);
                let ts_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                if tick_tx.send(FeedEvent::Tick { ts_ms }).is_err() {
                    break; // engine shut down
                }
            }
        });

        Self {
            pumpportal_rx,
            helius_rx,
            shredstream_rx,
            tick_rx,
            engine_tx,
        }
    }

    /// Run the event joining loop. Blocking — call from a dedicated thread.
    /// Forwards events from all channels to the engine in arrival order.
    /// Exits when the engine channel is closed (sender dropped).
    ///
    /// ShredStream channel is optional — if `None`, only PumpPortal, Helius,
    /// and tick events are processed.
    pub fn run(mut self) {
        // Take the optional shredstream channel out of self so we can move self freely.
        let shred_rx = self.shredstream_rx.take();
        // Destructure self into locals to avoid partial-move issues.
        let pp_rx = self.pumpportal_rx;
        let h_rx = self.helius_rx;
        let t_rx = self.tick_rx;
        let e_tx = self.engine_tx;

        match shred_rx {
            Some(s_rx) => run_4way(pp_rx, h_rx, s_rx, t_rx, e_tx),
            None => run_3way(pp_rx, h_rx, t_rx, e_tx),
        }
    }
}

/// 4-way select: PumpPortal + Helius + ShredStream + Tick.
/// If ShredStream disconnects, falls back to 3-way.
fn run_4way(
    pp_rx: Receiver<FeedEvent>,
    h_rx: Receiver<FeedEvent>,
    s_rx: Receiver<FeedEvent>,
    t_rx: Receiver<FeedEvent>,
    e_tx: Sender<FeedEvent>,
) {
    loop {
        select! {
            recv(pp_rx) -> msg => {
                match msg {
                    Ok(event) => {
                        if e_tx.send(event).is_err() {
                            debug!("[joiner] engine channel closed — exiting");
                            return;
                        }
                    }
                    Err(_) => {
                        debug!("[joiner] pumpportal channel closed");
                        return;
                    }
                }
            }
            recv(h_rx) -> msg => {
                match msg {
                    Ok(event) => {
                        if e_tx.send(event).is_err() {
                            return;
                        }
                    }
                    Err(_) => {
                        debug!("[joiner] helius channel closed");
                        return;
                    }
                }
            }
            recv(s_rx) -> msg => {
                match msg {
                    Ok(event) => {
                        if e_tx.send(event).is_err() {
                            debug!("[joiner] engine channel closed — exiting");
                            return;
                        }
                    }
                    Err(_) => {
                        debug!("[joiner] shredstream channel closed — falling back to 3-way");
                        return run_3way(pp_rx, h_rx, t_rx, e_tx);
                    }
                }
            }
            recv(t_rx) -> msg => {
                match msg {
                    Ok(event) => {
                        if e_tx.send(event).is_err() {
                            return;
                        }
                    }
                    Err(_) => return,
                }
            }
        }
    }
}

/// 3-way select: PumpPortal + Helius + Tick (no ShredStream).
fn run_3way(
    pp_rx: Receiver<FeedEvent>,
    h_rx: Receiver<FeedEvent>,
    t_rx: Receiver<FeedEvent>,
    e_tx: Sender<FeedEvent>,
) {
    loop {
        select! {
            recv(pp_rx) -> msg => {
                match msg {
                    Ok(event) => {
                        if e_tx.send(event).is_err() {
                            debug!("[joiner] engine channel closed — exiting");
                            return;
                        }
                    }
                    Err(_) => {
                        debug!("[joiner] pumpportal channel closed");
                        return;
                    }
                }
            }
            recv(h_rx) -> msg => {
                match msg {
                    Ok(event) => {
                        if e_tx.send(event).is_err() {
                            return;
                        }
                    }
                    Err(_) => {
                        debug!("[joiner] helius channel closed");
                        return;
                    }
                }
            }
            recv(t_rx) -> msg => {
                match msg {
                    Ok(event) => {
                        if e_tx.send(event).is_err() {
                            return;
                        }
                    }
                    Err(_) => return,
                }
            }
        }
    }
}
