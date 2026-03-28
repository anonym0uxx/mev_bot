//! EventJoiner — merges PumpPortal, Helius, ShredStream (optional), and timer
//! events into a single ordered stream for the engine hot-path thread.
//!
//! Resilience rules:
//! - Helius closed → fall back to PumpPortal + tick only (Helius is optional pre-warmer)
//! - ShredStream closed → fall back to without ShredStream
//! - PumpPortal closed → exit (it's the primary trigger feed; it has its own reconnect loop)
//! - Tick closed → exit (tick generator died, something is very wrong)
//! - Engine channel closed → exit cleanly

use crossbeam_channel::{Receiver, Sender, select};
use tracing::{debug, info};

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
        let (tick_tx, tick_rx) = crossbeam_channel::bounded::<FeedEvent>(8);

        std::thread::spawn(move || {
            let interval = std::time::Duration::from_millis(50);
            loop {
                std::thread::sleep(interval);
                let ts_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                if tick_tx.send(FeedEvent::Tick { ts_ms }).is_err() {
                    break;
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

    pub fn run(mut self) {
        let shred_rx = self.shredstream_rx.take();
        let pp_rx = self.pumpportal_rx;
        let h_rx = self.helius_rx;
        let t_rx = self.tick_rx;
        let e_tx = self.engine_tx;

        match shred_rx {
            Some(s_rx) => run_with_shred(pp_rx, h_rx, s_rx, t_rx, e_tx),
            None => run_pp_helius_tick(pp_rx, h_rx, t_rx, e_tx),
        }
    }
}

// ── Forward helper — returns false if engine channel closed ──────────
#[inline(always)]
fn forward(event: FeedEvent, e_tx: &Sender<FeedEvent>) -> bool {
    e_tx.send(event).is_ok()
}

// ── 4-way: PP + Helius + ShredStream + Tick ─────────────────────────
fn run_with_shred(
    pp_rx: Receiver<FeedEvent>,
    h_rx: Receiver<FeedEvent>,
    s_rx: Receiver<FeedEvent>,
    t_rx: Receiver<FeedEvent>,
    e_tx: Sender<FeedEvent>,
) {
    loop {
        select! {
            recv(pp_rx) -> msg => match msg {
                Ok(ev) => { if !forward(ev, &e_tx) { return; } }
                Err(_) => { debug!("[joiner] pumpportal closed"); return; }
            },
            recv(h_rx) -> msg => match msg {
                Ok(ev) => { if !forward(ev, &e_tx) { return; } }
                Err(_) => {
                    info!("[joiner] helius closed — continuing without it");
                    return run_pp_shred_tick(pp_rx, s_rx, t_rx, e_tx);
                }
            },
            recv(s_rx) -> msg => match msg {
                Ok(ev) => { if !forward(ev, &e_tx) { return; } }
                Err(_) => {
                    info!("[joiner] shredstream closed — falling back to pp+helius+tick");
                    return run_pp_helius_tick(pp_rx, h_rx, t_rx, e_tx);
                }
            },
            recv(t_rx) -> msg => match msg {
                Ok(ev) => { if !forward(ev, &e_tx) { return; } }
                Err(_) => { debug!("[joiner] tick closed"); return; }
            },
        }
    }
}

// ── 3-way: PP + Helius + Tick ────────────────────────────────────────
fn run_pp_helius_tick(
    pp_rx: Receiver<FeedEvent>,
    h_rx: Receiver<FeedEvent>,
    t_rx: Receiver<FeedEvent>,
    e_tx: Sender<FeedEvent>,
) {
    loop {
        select! {
            recv(pp_rx) -> msg => match msg {
                Ok(ev) => { if !forward(ev, &e_tx) { return; } }
                Err(_) => { debug!("[joiner] pumpportal closed"); return; }
            },
            recv(h_rx) -> msg => match msg {
                Ok(ev) => { if !forward(ev, &e_tx) { return; } }
                Err(_) => {
                    info!("[joiner] helius closed — continuing pp+tick only");
                    return run_pp_tick(pp_rx, t_rx, e_tx);
                }
            },
            recv(t_rx) -> msg => match msg {
                Ok(ev) => { if !forward(ev, &e_tx) { return; } }
                Err(_) => { debug!("[joiner] tick closed"); return; }
            },
        }
    }
}

// ── 3-way: PP + ShredStream + Tick (Helius gone) ────────────────────
fn run_pp_shred_tick(
    pp_rx: Receiver<FeedEvent>,
    s_rx: Receiver<FeedEvent>,
    t_rx: Receiver<FeedEvent>,
    e_tx: Sender<FeedEvent>,
) {
    loop {
        select! {
            recv(pp_rx) -> msg => match msg {
                Ok(ev) => { if !forward(ev, &e_tx) { return; } }
                Err(_) => { debug!("[joiner] pumpportal closed"); return; }
            },
            recv(s_rx) -> msg => match msg {
                Ok(ev) => { if !forward(ev, &e_tx) { return; } }
                Err(_) => {
                    info!("[joiner] shredstream closed — pp+tick only");
                    return run_pp_tick(pp_rx, t_rx, e_tx);
                }
            },
            recv(t_rx) -> msg => match msg {
                Ok(ev) => { if !forward(ev, &e_tx) { return; } }
                Err(_) => { debug!("[joiner] tick closed"); return; }
            },
        }
    }
}

// ── 2-way: PP + Tick (everything else gone) ──────────────────────────
fn run_pp_tick(
    pp_rx: Receiver<FeedEvent>,
    t_rx: Receiver<FeedEvent>,
    e_tx: Sender<FeedEvent>,
) {
    loop {
        select! {
            recv(pp_rx) -> msg => match msg {
                Ok(ev) => { if !forward(ev, &e_tx) { return; } }
                Err(_) => { debug!("[joiner] pumpportal closed"); return; }
            },
            recv(t_rx) -> msg => match msg {
                Ok(ev) => { if !forward(ev, &e_tx) { return; } }
                Err(_) => { debug!("[joiner] tick closed"); return; }
            },
        }
    }
}
