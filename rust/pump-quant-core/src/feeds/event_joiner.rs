//! EventJoiner — merges PumpPortal, Helius, and timer events into a single
//! ordered stream for the engine hot-path thread.

use crossbeam_channel::{Receiver, Sender, select};
use tracing::debug;

use crate::feeds::FeedEvent;

pub struct EventJoiner {
    pumpportal_rx: Receiver<FeedEvent>,
    helius_rx: Receiver<FeedEvent>,
    tick_rx: Receiver<FeedEvent>,
    engine_tx: Sender<FeedEvent>,
}

impl EventJoiner {
    pub fn new(
        pumpportal_rx: Receiver<FeedEvent>,
        helius_rx: Receiver<FeedEvent>,
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
            tick_rx,
            engine_tx,
        }
    }

    /// Run the event joining loop. Blocking — call from a dedicated thread.
    /// Forwards events from all three channels to the engine in arrival order.
    /// Exits when the engine channel is closed (sender dropped).
    pub fn run(self) {
        loop {
            select! {
                recv(self.pumpportal_rx) -> msg => {
                    match msg {
                        Ok(event) => {
                            if self.engine_tx.send(event).is_err() {
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
                recv(self.helius_rx) -> msg => {
                    match msg {
                        Ok(event) => {
                            if self.engine_tx.send(event).is_err() {
                                return;
                            }
                        }
                        Err(_) => {
                            debug!("[joiner] helius channel closed");
                            return;
                        }
                    }
                }
                recv(self.tick_rx) -> msg => {
                    match msg {
                        Ok(event) => {
                            if self.engine_tx.send(event).is_err() {
                                return;
                            }
                        }
                        Err(_) => return,
                    }
                }
            }
        }
    }
}
