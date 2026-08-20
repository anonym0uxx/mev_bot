//! Leaf `ex_creator_history_veto`: R-3 daemon-level creator on-chain history
//! veto. Wraps an inner `OutboundSink` and, before forwarding a BUY admit,
//! queries `getSignaturesForAddress` on the creator's Solana wallet to count
//! recent on-chain signatures. If the count ≥ the configured threshold the buy
//! is vetoed (returned as `OutboundOutcome::Construction`), preventing the
//! serial-rugger pattern where a creator launches 50+ coins rapidly.
//!
//! ## Architecture
//! The engine is pure (§22 — no network I/O). The `AdmitRecord` carries the
//! mint bytes but NOT the creator pubkey. The daemon maintains a
//! `mint → creator_pubkey` map (populated from PumpPortal create events) and
//! passes it to this wrapper via `CreatorPubkeyLookup`. The wrapper queries
//! the RPC endpoint (Helius) for the creator's recent signatures, counts them,
//! and vetoes if above threshold.
//!
//! ## Fail-open (§6.4)
//! Any RPC error, missing creator pubkey, or cache miss results in NO veto —
//! the buy proceeds. The R-3 veto only fires when we have POSITIVE evidence
//! of serial launching. Unknown stays unknown; we never reject on missing data.
//!
//! ## Caching
//! Results per creator pubkey are cached with a 5-minute TTL to avoid
//! re-querying the same wallet on every buy. The cache is a simple
//! `Mutex<HashMap<[u8;32], (Instant, u32)>>` — the count and the time it was
//! queried. A re-query is only issued when the cached entry is older than the
//! TTL or absent.
//!
//! Constitution refs: §22 (engine purity preserved — all I/O in this wrapper,
//!   not in the engine), §6.4 (fail-open on unknown), §99 (bounded cache).

use crate::ex_outbound_sink::{AdmitRecord, OutboundOutcome, OutboundSink};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// TTL for the creator-history cache (5 minutes). A creator's recent
/// signature count doesn't change dramatically in this window, and re-querying
/// on every buy would waste RPC quota.
const CACHE_TTL_SECS: u64 = 300;

/// Trait for looking up the creator pubkey for a given mint. The daemon
/// implements this with its `mint → creator_pubkey` map (populated from
/// PumpPortal create events where `traderPublicKey` is the creator wallet).
pub trait CreatorPubkeyLookup: Send + Sync {
    /// Return the raw 32-byte creator pubkey for the given mint, or `None`
    /// when unknown (fail-open).
    fn lookup_creator_pubkey(&self, mint: &[u8; 32]) -> Option<[u8; 32]>;
}

/// Trait for querying the RPC node's `getSignaturesForAddress` endpoint.
/// The daemon implements this with its Helius HTTP client. Returns the number
/// of recent signatures for the given wallet address, or `None` on any RPC
/// failure (fail-open — the veto is skipped).
pub trait CreatorHistoryRpc: Send + Sync {
    /// Query the number of recent on-chain signatures for `creator_pubkey`.
    /// Returns `Some(count)` on success, `None` on any RPC failure (fail-open).
    fn query_signature_count(&self, creator_pubkey: &[u8; 32]) -> Option<u32>;
}

/// The R-3 wrapper sink. Wraps an inner `OutboundSink` (the real
/// `LiveOutboundSink`) and intercepts BUY admits. SELL admits are passed
/// through directly — R-3 only gates entries, never exits (a sell must always
/// execute to free capital).
pub struct CreatorHistoryVetoSink {
    inner: &'static dyn OutboundSink,
    max_launches: u32,
    lookup: &'static dyn CreatorPubkeyLookup,
    rpc: &'static dyn CreatorHistoryRpc,
    cache: Mutex<HashMap<[u8; 32], (Instant, u32)>>,
}

impl CreatorHistoryVetoSink {
    /// Construct the R-3 wrapper around `inner`. The `max_launches` threshold
    /// is the minimum signature count required to trigger a veto.
    pub fn new(
        inner: &'static dyn OutboundSink,
        lookup: &'static dyn CreatorPubkeyLookup,
        rpc: &'static dyn CreatorHistoryRpc,
        max_launches: u32,
    ) -> Self {
        Self {
            inner,
            max_launches,
            lookup,
            rpc,
            cache: Mutex::new(HashMap::new()),
        }
    }
}

impl OutboundSink for CreatorHistoryVetoSink {
    fn on_admit(&self, record: &AdmitRecord) -> OutboundOutcome {
        // R-3 only gates BUYs. SELLs pass through unconditionally —
        // blocking a sell traps capital (§ live-arming: exits always execute).
        if !record.is_buy {
            return self.inner.on_admit(record);
        }

        let mint = record.mint;

        // Step 1: look up the creator pubkey for this mint.
        let creator_pubkey = match self.lookup.lookup_creator_pubkey(&mint) {
            Some(pk) => pk,
            None => {
                // Unknown creator → fail-open (§6.4). No reject on missing data.
                eprintln!(
                    "[R-3] mint {} — creator pubkey unknown, fail-open (no veto)",
                    hex_short(&mint)
                );
                return self.inner.on_admit(record);
            }
        };

        // Step 2: check the cache. If we have a recent result, use it.
        let now = Instant::now();
        {
            let cache = self.cache.lock().unwrap();
            if let Some((queried_at, count)) = cache.get(&creator_pubkey) {
                if now.duration_since(*queried_at) < Duration::from_secs(CACHE_TTL_SECS) {
                    if *count >= self.max_launches {
                        eprintln!(
                            "[R-3] VETO (cached): creator {} has {} recent signatures ≥ threshold {}. Buy vetoed for mint {}.",
                            hex_short(&creator_pubkey),
                            count,
                            self.max_launches,
                            hex_short(&mint)
                        );
                        return OutboundOutcome::Construction(format!(
                            "R-3 creator-history veto: creator {} has {} recent signatures (threshold {})",
                            hex_short(&creator_pubkey),
                            count,
                            self.max_launches
                        ));
                    }
                    // Cached and below threshold → proceed.
                    return self.inner.on_admit(record);
                }
            }
        }

        // Step 3: query the RPC. Fail-open on any error (None → pass through).
        let count = match self.rpc.query_signature_count(&creator_pubkey) {
            Some(c) => c,
            None => {
                eprintln!(
                    "[R-3] RPC error for creator {} — fail-open (no veto) for mint {}",
                    hex_short(&creator_pubkey),
                    hex_short(&mint)
                );
                return self.inner.on_admit(record);
            }
        };

        // Step 4: cache the result.
        {
            let mut cache = self.cache.lock().unwrap();
            cache.insert(creator_pubkey, (now, count));
            // Bound the cache (§99): evict oldest entry if > 10_000.
            if cache.len() > 10_000 {
                let oldest_key = cache
                    .iter()
                    .min_by_key(|(_, (t, _))| *t)
                    .map(|(k, _)| *k);
                if let Some(key) = oldest_key {
                    cache.remove(&key);
                }
            }
        }

        // Step 5: veto if above threshold.
        if count >= self.max_launches {
            eprintln!(
                "[R-3] VETO: creator {} has {} recent signatures ≥ threshold {}. Buy vetoed for mint {}.",
                hex_short(&creator_pubkey),
                count,
                self.max_launches,
                hex_short(&mint)
            );
            return OutboundOutcome::Construction(format!(
                "R-3 creator-history veto: creator {} has {} recent signatures (threshold {})",
                hex_short(&creator_pubkey),
                count,
                self.max_launches
            ));
        }

        // Below threshold → proceed to the inner sink.
        eprintln!(
            "[R-3] PASS: creator {} has {} recent signatures < threshold {}. Buy proceeding for mint {}.",
            hex_short(&creator_pubkey),
            count,
            self.max_launches,
            hex_short(&mint)
        );
        self.inner.on_admit(record)
    }
}

/// Format a 32-byte key as a short hex string for logging.
fn hex_short(bytes: &[u8]) -> String {
    let mut s = String::new();
    for &b in bytes.iter().take(4) {
        s.push_str(&format!("{:02x}", b));
    }
    s.push_str("...");
    for &b in bytes.iter().rev().take(4) {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    // ---- Mock infrastructure ----

    struct MockLookup {
        map: HashMap<[u8; 32], [u8; 32]>,
    }

    impl CreatorPubkeyLookup for MockLookup {
        fn lookup_creator_pubkey(&self, mint: &[u8; 32]) -> Option<[u8; 32]> {
            self.map.get(mint).copied()
        }
    }

    struct MockRpc {
        count: u32,
        err: bool,
        calls: AtomicU32,
    }

    impl CreatorHistoryRpc for MockRpc {
        fn query_signature_count(&self, _creator_pubkey: &[u8; 32]) -> Option<u32> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.err {
                None
            } else {
                Some(self.count)
            }
        }
    }

    struct RecordingSink {
        calls: AtomicU32,
    }

    impl OutboundSink for RecordingSink {
        fn on_admit(&self, _record: &AdmitRecord) -> OutboundOutcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            OutboundOutcome::Accepted { signature: [0u8; 64] }
        }
    }

    fn dummy_mint(n: u8) -> [u8; 32] {
        let mut m = [0u8; 32];
        m[0] = n;
        m
    }

    fn dummy_creator(n: u8) -> [u8; 32] {
        let mut c = [0u8; 32];
        c[0] = n;
        c
    }

    fn buy_record(mint: [u8; 32]) -> AdmitRecord {
        AdmitRecord {
            mint,
            user: [0u8; 32],
            is_buy: true,
            size_lamports: 10_000_000,
            entry_price: 1_000_000,
            max_slippage_bps: 500,
        }
    }

    fn sell_record(mint: [u8; 32]) -> AdmitRecord {
        AdmitRecord {
            mint,
            user: [0u8; 32],
            is_buy: false,
            size_lamports: 10_000_000,
            entry_price: 1_000_000,
            max_slippage_bps: 500,
        }
    }

    // Leak mock objects to &'static for testing (tests are short-lived processes).
    fn leaked_lookup(map: HashMap<[u8; 32], [u8; 32]>) -> &'static dyn CreatorPubkeyLookup {
        Box::leak(Box::new(MockLookup { map }))
    }

    #[test]
    fn sell_always_passes_through() {
        let inner = Box::leak(Box::new(RecordingSink { calls: AtomicU32::new(0) }));
        let sink = CreatorHistoryVetoSink::new(
            inner as &'static dyn OutboundSink,
            leaked_lookup(HashMap::new()),
            Box::leak(Box::new(MockRpc { count: 9999, err: false, calls: AtomicU32::new(0) })),
            10, // low threshold
        );
        let outcome = sink.on_admit(&sell_record(dummy_mint(1)));
        assert!(matches!(outcome, OutboundOutcome::Accepted { .. }));
    }

    #[test]
    fn unknown_creator_fail_open() {
        let inner = Box::leak(Box::new(RecordingSink { calls: AtomicU32::new(0) }));
        let sink = CreatorHistoryVetoSink::new(
            inner as &'static dyn OutboundSink,
            leaked_lookup(HashMap::new()), // empty → no lookup
            Box::leak(Box::new(MockRpc { count: 9999, err: false, calls: AtomicU32::new(0) })),
            10,
        );
        let outcome = sink.on_admit(&buy_record(dummy_mint(1)));
        assert!(matches!(outcome, OutboundOutcome::Accepted { .. }));
    }

    #[test]
    fn rpc_error_fail_open() {
        let mut map = HashMap::new();
        map.insert(dummy_mint(1), dummy_creator(42));
        let inner = Box::leak(Box::new(RecordingSink { calls: AtomicU32::new(0) }));
        let sink = CreatorHistoryVetoSink::new(
            inner as &'static dyn OutboundSink,
            leaked_lookup(map),
            Box::leak(Box::new(MockRpc { count: 0, err: true, calls: AtomicU32::new(0) })),
            10,
        );
        let outcome = sink.on_admit(&buy_record(dummy_mint(1)));
        assert!(matches!(outcome, OutboundOutcome::Accepted { .. }));
    }

    #[test]
    fn veto_when_count_above_threshold() {
        let mut map = HashMap::new();
        map.insert(dummy_mint(1), dummy_creator(42));
        let rpc_tracker = Box::leak(Box::new(MockRpc { count: 100, err: false, calls: AtomicU32::new(0) }));
        let inner = Box::leak(Box::new(RecordingSink { calls: AtomicU32::new(0) }));
        let sink = CreatorHistoryVetoSink::new(
            inner as &'static dyn OutboundSink,
            leaked_lookup(map),
            rpc_tracker as &'static dyn CreatorHistoryRpc,
            50, // threshold
        );
        let outcome = sink.on_admit(&buy_record(dummy_mint(1)));
        assert!(matches!(outcome, OutboundOutcome::Construction(ref msg)
            if msg.contains("R-3 creator-history veto")));
        // RPC was called once
        assert_eq!(rpc_tracker.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn pass_when_count_below_threshold() {
        let mut map = HashMap::new();
        map.insert(dummy_mint(1), dummy_creator(42));
        let rpc_tracker = Box::leak(Box::new(MockRpc { count: 100, err: false, calls: AtomicU32::new(0) }));
        let inner = Box::leak(Box::new(RecordingSink { calls: AtomicU32::new(0) }));
        let sink = CreatorHistoryVetoSink::new(
            inner as &'static dyn OutboundSink,
            leaked_lookup(map),
            rpc_tracker as &'static dyn CreatorHistoryRpc,
            1000, // high threshold
        );
        let outcome = sink.on_admit(&buy_record(dummy_mint(1)));
        assert!(matches!(outcome, OutboundOutcome::Accepted { .. }));
        assert_eq!(rpc_tracker.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cache_prevents_duplicate_rpc_calls() {
        let mut map = HashMap::new();
        map.insert(dummy_mint(1), dummy_creator(42));
        let rpc_tracker = Box::leak(Box::new(MockRpc { count: 100, err: false, calls: AtomicU32::new(0) }));
        let inner = Box::leak(Box::new(RecordingSink { calls: AtomicU32::new(0) }));
        let sink = CreatorHistoryVetoSink::new(
            inner as &'static dyn OutboundSink,
            leaked_lookup(map),
            rpc_tracker as &'static dyn CreatorHistoryRpc,
            1000, // high threshold → passes
        );
        // First call — queries RPC
        let outcome1 = sink.on_admit(&buy_record(dummy_mint(1)));
        assert!(matches!(outcome1, OutboundOutcome::Accepted { .. }));
        assert_eq!(rpc_tracker.calls.load(Ordering::SeqCst), 1);
        // Second call — should use cache, NO new RPC call
        let outcome2 = sink.on_admit(&buy_record(dummy_mint(1)));
        assert!(matches!(outcome2, OutboundOutcome::Accepted { .. }));
        assert_eq!(rpc_tracker.calls.load(Ordering::SeqCst), 1, "cache should prevent 2nd RPC call");
    }
}
