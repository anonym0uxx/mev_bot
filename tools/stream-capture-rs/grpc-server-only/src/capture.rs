//! Training-capture orchestrator — owns the LaserStream subscription, the
//! raw recorder, the normalizer, the events writer, and the manifest.
//!
//! In training mode we subscribe BROAD:
//! * Transactions: Pump.fun + PumpSwap programs, non-vote, all (no failed
//!   filter, no mayhem/cashback/complete filters).
//! * Account updates: both programs (for curve/pool state snapshots).
//! * Slot updates: for ordering + gap detection.
//! * Block-meta: for block timing + tx count.
//!
//! Commitment: CONFIRMED (not PROCESSED) — avoids capturing fork-rolled txs.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use helius_laserstream::grpc::{
    subscribe_update::UpdateOneof, CommitmentLevel, SubscribeRequest,
    SubscribeRequestFilterAccounts, SubscribeRequestFilterBlocksMeta,
    SubscribeRequestFilterSlots, SubscribeRequestFilterTransactions,
};
use helius_laserstream::{subscribe, LaserstreamConfig};

use crate::events_writer::EventsWriter;
use crate::manifest::{self, SlotGap};
use crate::normalizer::Normalizer;
use crate::raw_recorder::{
    build_account_payload, build_block_meta_payload, build_slot_payload,
    build_tx_payload, RawRecorder,
};

/// Pump.fun program ID (base58).
const PUMP_FUN_PROGRAM: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
/// PumpSwap program ID (base58).
const PUMP_SWAP_PROGRAM: &str = "pPEEEJ5r9sRFMks2oBq1qjhtBf8V4qyGSz8xbxqHEBu";

pub struct TrainingCapture {
    config: LaserstreamConfig,
    data_dir: PathBuf,
    session_id: String,
    repo_sha: String,
    endpoint_host: String,
    duration_minutes: u64,
    our_wallet: Option<String>,
}

impl TrainingCapture {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: LaserstreamConfig,
        data_dir: PathBuf,
        session_id: String,
        repo_sha: String,
        endpoint_host: String,
        duration_minutes: u64,
        our_wallet: Option<String>,
    ) -> Self {
        Self {
            config,
            data_dir,
            session_id,
            repo_sha,
            endpoint_host,
            duration_minutes,
            our_wallet,
        }
    }

    pub async fn run(self, smoke_mode: bool) -> Result<(), Box<dyn std::error::Error>> {
        let start_unix_ms = crate::encoding::now_unix_ms();
        let start_instant = Instant::now();

        // ── Initialize recorders ──
        let raw_recorder = Arc::new(RawRecorder::new(self.data_dir.clone(), &self.session_id)?);
        let events_path = self.data_dir.join(format!(
            "pumpfun_laserstream_events_v1_{}.ndjson",
            self.session_id
        ));
        let events_writer = Arc::new(EventsWriter::new(&events_path)?);
        let normalizer = Arc::new(std::sync::Mutex::new(Normalizer::new()));

        // ── Counters ──
        let total_raw = Arc::new(AtomicU64::new(0));
        let total_events = Arc::new(AtomicU64::new(0));
        let reconnects = Arc::new(AtomicU64::new(0));
        let start_slot = Arc::new(AtomicU64::new(0));
        let end_slot = Arc::new(AtomicU64::new(0));
        let last_slot = Arc::new(AtomicU64::new(0));
        let gaps: Arc<std::sync::Mutex<Vec<SlotGap>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));

        // ── Shutdown flag (set by Ctrl+C handler) ──
        let shutdown = Arc::new(AtomicBool::new(false));

        // ── Build the BROAD subscription request ──
        let mut request = SubscribeRequest::default();

        // Transactions: Pump.fun + PumpSwap, non-vote, include failed (for learning).
        let mut tx_filter = SubscribeRequestFilterTransactions::default();
        tx_filter.vote = Some(false);
        // No failed filter — we want losers too.
        tx_filter.account_include = vec![
            PUMP_FUN_PROGRAM.to_string(),
            PUMP_SWAP_PROGRAM.to_string(),
        ];
        request.transactions = HashMap::from([("pump_broad".to_string(), tx_filter)]);

        // Account updates: both programs (curve + pool state).
        let mut acct_filter = SubscribeRequestFilterAccounts::default();
        acct_filter.owner = vec![
            PUMP_FUN_PROGRAM.to_string(),
            PUMP_SWAP_PROGRAM.to_string(),
        ];
        request.accounts = HashMap::from([("pump_accounts".to_string(), acct_filter)]);

        // Slot updates: for ordering + gap detection.
        request.slots = HashMap::from([(
            "slots".to_string(),
            SubscribeRequestFilterSlots::default(),
        )]);

        // Block-meta: for block timing + tx count.
        request.blocks_meta = HashMap::from([(
            "blocks_meta".to_string(),
            SubscribeRequestFilterBlocksMeta::default(),
        )]);

        // CONFIRMED commitment for training (not PROCESSED).
        request.commitment = Some(CommitmentLevel::Confirmed as i32);

        eprintln!("Subscribing (CONFIRMED, BROAD)...");
        let (stream, _handle) = subscribe(self.config.clone(), request);
        tokio::pin!(stream);

        // ── Set up Ctrl+C / SIGINT handler ──
        let shutdown_flag = shutdown.clone();
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            eprintln!("\n[SIGINT] Received Ctrl+C — initiating graceful shutdown...");
            shutdown_flag.store(true, Ordering::SeqCst);
        });

        // ── Main event loop ──
        let effective_duration = if smoke_mode { 1 } else { self.duration_minutes };
        let duration_ms = effective_duration * 60 * 1000;

        eprintln!("=== CAPTURE STARTED ===");
        eprintln!("Session: {}", self.session_id);
        eprintln!("PID: {}", std::process::id());
        eprintln!("Duration: {effective_duration} minutes ({duration_ms} ms)");

        // Print periodic stats every 30 seconds.
        let mut stats_interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
        stats_interval.tick().await; // skip first immediate tick

        loop {
            // Check shutdown conditions.
            if shutdown.load(Ordering::SeqCst) {
                break;
            }
            let elapsed = start_instant.elapsed().as_millis() as u128;
            if elapsed >= duration_ms as u128 {
                eprintln!("\n[TIMEOUT] Reached {effective_duration} min limit — stopping.");
                break;
            }

            // Use select to interleave stream reads with timeout/stats.
            tokio::select! {
                biased;
                _ = tokio::signal::ctrl_c() => {
                    eprintln!("\n[SIGINT] Ctrl+C — graceful shutdown...");
                    shutdown.store(true, Ordering::SeqCst);
                    break;
                }
                update_result = stream.next() => {
                    match update_result {
                        None => {
                            eprintln!("\n[STREAM-END] LaserStream stream ended.");
                            break;
                        }
                        Some(Ok(update)) => {
                            self.process_update(
                                &update,
                                &raw_recorder,
                                &events_writer,
                                &normalizer,
                                &total_raw,
                                &total_events,
                                &start_slot,
                                &end_slot,
                                &last_slot,
                                &gaps,
                                &self.our_wallet,
                            );
                        }
                        Some(Err(e)) => {
                            eprintln!("[STREAM-ERROR] {e}");
                            reconnects.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                }
                _ = stats_interval.tick() => {
                    let raw = total_raw.load(Ordering::Relaxed);
                    let events = total_events.load(Ordering::Relaxed);
                    let elapsed_min = start_instant.elapsed().as_secs() / 60;
                    let norm = normalizer.lock().unwrap();
                    eprintln!(
                        "[stats {elapsed_min}min] raw={raw} events={events} | creates={} buys={}/{} sells={}/{} completes={} migrations={} pumpswap_buys={} pumpswap_sells={} pools={} dups={}",
                        norm.creates,
                        norm.pump_buys, norm.pumpswap_buys,
                        norm.pump_sells, norm.pumpswap_sells,
                        norm.pump_completes,
                        norm.migrations,
                        norm.pumpswap_buys,
                        norm.pumpswap_sells,
                        norm.pumpswap_create_pools,
                        norm.duplicates(),
                    );
                    drop(norm);
                }
            }
        }

        // ── Finalize ──
        eprintln!("\n=== FINALIZING ===");
        raw_recorder.finalize()?;
        events_writer.flush()?;

        let end_unix_ms = crate::encoding::now_unix_ms();
        let actual_duration = (end_unix_ms - start_unix_ms) / 60_000;
        let s_slot = start_slot.load(Ordering::SeqCst);
        let e_slot = end_slot.load(Ordering::SeqCst);

        eprintln!("Hashing raw files for manifest...");
        let raw_files = self.collect_raw_file_info()?;
        let events_info = self.collect_events_file_info(&events_path)?;

        let total_raw_records = raw_recorder.total_records();
        let total_events_count = events_writer.count();

        let norm = normalizer.lock().unwrap();
        let gaps_vec = gaps.lock().unwrap().clone();
        drop(gaps);

        manifest::write_manifest(
            &self.data_dir,
            &self.session_id,
            &self.repo_sha,
            &self.endpoint_host,
            "CONFIRMED",
            &[PUMP_FUN_PROGRAM.to_string(), PUMP_SWAP_PROGRAM.to_string()],
            start_unix_ms,
            end_unix_ms,
            if s_slot > 0 { Some(s_slot) } else { None },
            if e_slot > 0 { Some(e_slot) } else { None },
            actual_duration.max(1),
            &norm,
            reconnects.load(Ordering::SeqCst),
            gaps_vec,
            raw_files,
            events_info,
            total_raw_records,
            total_events_count,
        )?;
        drop(norm);

        let manifest_path = self.data_dir.join(format!(
            "pumpfun_laserstream_manifest_v1_{}.json",
            self.session_id
        ));

        eprintln!("=== CAPTURE COMPLETE ===");
        eprintln!("Session: {}", self.session_id);
        eprintln!("Duration: {actual_duration} minutes");
        eprintln!("Raw records: {total_raw_records}");
        eprintln!("Events: {total_events_count}");
        eprintln!("Manifest: {}", manifest_path.display());
        Ok(())
    }

    /// Process a single LaserStream update — route to the appropriate recorder.
    #[allow(clippy::too_many_arguments)]
    fn process_update(
        &self,
        update: &helius_laserstream::grpc::SubscribeUpdate,
        raw_recorder: &Arc<RawRecorder>,
        events_writer: &Arc<EventsWriter>,
        normalizer: &Arc<std::sync::Mutex<Normalizer>>,
        total_raw: &Arc<AtomicU64>,
        total_events: &Arc<AtomicU64>,
        start_slot: &Arc<AtomicU64>,
        end_slot: &Arc<AtomicU64>,
        last_slot: &Arc<AtomicU64>,
        gaps: &Arc<std::sync::Mutex<Vec<SlotGap>>>,
        our_wallet: &Option<String>,
    ) {
        let update_oneof = match &update.update_oneof {
            Some(u) => u,
            None => return,
        };

        match update_oneof {
            UpdateOneof::Transaction(tx_update) => {
                if let Some(tx_info) = &tx_update.transaction {
                    let slot = tx_update.slot;
                    Self::track_slot(slot, start_slot, end_slot, last_slot, gaps);

                    if tx_info.is_vote {
                        return;
                    }

                    let payload = build_tx_payload(tx_info);
                    if raw_recorder.write("transaction", slot, payload).is_ok() {
                        total_raw.fetch_add(1, Ordering::Relaxed);
                    }

                    let mut norm = normalizer.lock().unwrap();
                    let events = norm.normalize_tx(slot, tx_info, true, our_wallet.as_deref());
                    drop(norm);

                    for ev in &events {
                        events_writer.write(ev).ok();
                        total_events.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            UpdateOneof::Account(acct_update) => {
                if let Some(acct_info) = &acct_update.account {
                    let slot = acct_update.slot;
                    Self::track_slot(slot, start_slot, end_slot, last_slot, gaps);

                    let payload = build_account_payload(acct_info);
                    if raw_recorder.write("account", slot, payload).is_ok() {
                        total_raw.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            UpdateOneof::Slot(slot_update) => {
                let slot = slot_update.slot;
                Self::track_slot(slot, start_slot, end_slot, last_slot, gaps);
                let payload = build_slot_payload(slot_update);
                if raw_recorder.write("slot", slot, payload).is_ok() {
                    total_raw.fetch_add(1, Ordering::Relaxed);
                }
            }
            UpdateOneof::BlockMeta(bm_update) => {
                let slot = bm_update.slot;
                Self::track_slot(slot, start_slot, end_slot, last_slot, gaps);
                let payload = build_block_meta_payload(bm_update);
                if raw_recorder.write("block_meta", slot, payload).is_ok() {
                    total_raw.fetch_add(1, Ordering::Relaxed);
                }
            }
            _ => {} // Ping/Pong/Entry/Block/TransactionStatus — not needed for training.
        }
    }

    /// Track slot range + detect gaps.
    fn track_slot(
        slot: u64,
        start_slot: &Arc<AtomicU64>,
        end_slot: &Arc<AtomicU64>,
        last_slot: &Arc<AtomicU64>,
        gaps: &Arc<std::sync::Mutex<Vec<SlotGap>>>,
    ) {
        if start_slot.load(Ordering::SeqCst) == 0 {
            start_slot.store(slot, Ordering::SeqCst);
        }

        let prev = last_slot.swap(slot, Ordering::SeqCst);
        // Only flag a gap when the skip is > 50 slots — small skips are
        // normal on Solana (especially CONFIRMED, where out-of-order
        // delivery is common). A real gap (disconnected stream, missing
        // block) typically spans hundreds of slots.
        if prev > 0 && slot > prev + 50 {
            let mut g = gaps.lock().unwrap();
            g.push(SlotGap {
                from_slot: prev + 1,
                to_slot: slot - 1,
            });
            drop(g);
        }

        end_slot.store(slot, Ordering::SeqCst);
    }

    /// Collect raw file info (filenames, sizes, hashes) for the manifest.
    fn collect_raw_file_info(&self) -> Result<Vec<manifest::RawFileInfo>, Box<dyn std::error::Error>> {
        let mut files = vec![];
        let prefix = format!("pumpfun_laserstream_raw_v1_{}_part", self.session_id);
        let entries = std::fs::read_dir(&self.data_dir)?;
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(&prefix) && name.ends_with(".ndjson.zst") {
                let path = entry.path();
                let bytes = manifest::file_bytes(&path)?;
                let hash = manifest::file_sha256(&path)?;
                files.push(manifest::RawFileInfo {
                    filename: name,
                    bytes,
                    sha256: hash,
                });
            }
        }
        files.sort_by(|a, b| a.filename.cmp(&b.filename));
        Ok(files)
    }

    /// Collect events file info for the manifest.
    fn collect_events_file_info(
        &self,
        path: &PathBuf,
    ) -> Result<Option<manifest::EventFileInfo>, Box<dyn std::error::Error>> {
        if !path.exists() {
            return Ok(None);
        }
        let bytes = manifest::file_bytes(path)?;
        let hash = manifest::file_sha256(path)?;
        let filename = path.file_name().unwrap().to_string_lossy().to_string();
        Ok(Some(manifest::EventFileInfo {
            filename,
            bytes,
            sha256: hash,
        }))
    }
}
