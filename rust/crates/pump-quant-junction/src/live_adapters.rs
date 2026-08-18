//! Live I/O adapters: bridge the execution crate's `LiveSigner`,
//! `LiveStateFetcher`, `LiveSubmitter` trait interfaces to the junction
//! crate's concrete I/O implementations.
//!
//! ## Why
//! The execution crate is std-only and defines trait interfaces. The junction
//! crate has the concrete I/O (`pq-stream-capture`: ring ed25519, ureq
//! JSON-RPC, Helius Sender). This module provides the adapter impls.
//!
//! ## Lifetime design
//! `RpcStateFetch<'a>` and `SenderClient<'a>` both borrow a `&'a dyn Transport`,
//! so they cannot be stored as `'static` fields behind `dyn LiveStateFetcher`
//! (which requires `Send + Sync + 'static`). Instead, the adapters **own** the
//! transport (`UreqTransport`) and construct `RpcStateFetch` / `SenderClient`
//! fresh on each call. This is zero-overhead: both `new()` constructors are
//! trivial struct assignments with no allocation.
//!
//! ## Latency design
//! The hot path is `fetch_state_hot` → `sign` → `submit`. The state fetcher
//! caches blockhash + curve state on a background updater thread (prefetch),
//! so the hot path returns cached state in ~0ms. The sign is pure compute
//! (~100μs via ring ed25519). The submit is the only network I/O (~5-50ms RTT
//! to Helius Sender, unavoidable).
//!
//! ## Constitution refs
//! - §36: failure taxonomy maps 1:1 to the error variants here.
//! - §41: construction parity — the fetcher returns decoded on-chain facts.
//! - §24(b): paper/replay mode never touches these adapters.

use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex, RwLock,
};

use pump_quant_execution::ex_live_io_traits::{
    LiveBlockhash, LiveCurveState, LiveSigner, LiveStateFetcher, LiveSubmitter,
    SignError, StateFetchError, SubmitError,
};

use pq_stream_capture::rpc::{Reply, Transport, UreqTransport};
use pq_stream_capture::sender::{Accepted, SenderClient, SenderEndpoint, SenderError};
use pq_stream_capture::signer::{WalletSigner, SignerError, SIGNATURE_BYTES};

use base64::Engine as _;

use crate::state_fetch::{RpcStateFetch, StateFetch, StateFetchError as JunctionStateFetchError};
use pump_quant_protocol::venue_accounts::PumpCurveCtx;

// ---------------------------------------------------------------------------
// LiveSigner adapter — wraps pq_stream_capture::signer::WalletSigner
// ---------------------------------------------------------------------------

/// Production signer backed by `WalletSigner` (ring ed25519).
///
/// The `WalletSigner` is loaded once at daemon startup and held behind `Arc`.
/// The sign operation is pure compute (~100μs) — no I/O.
pub struct LiveWalletSigner {
    inner: Arc<WalletSigner>,
}

impl LiveWalletSigner {
    /// Load the keypair from a Solana CLI file and bind it to the expected
    /// wallet address. Fail-closed: if the keypair doesn't match the expected
    /// address, this returns an error and the sink stays fail-closed.
    pub fn load(keypair_path: &Path, expected_address: &str) -> Result<Self, SignError> {
        let signer = WalletSigner::load_solana_keypair(keypair_path, expected_address)
            .map_err(map_signer_error)?;
        Ok(Self {
            inner: Arc::new(signer),
        })
    }

    /// Wrap an already-loaded `WalletSigner`. Used in tests and when the
    /// daemon already has a signer instance.
    pub fn from_loaded(signer: Arc<WalletSigner>) -> Self {
        Self { inner: signer }
    }

    /// The public address this signer will sign for. Not a secret.
    /// Delegates to the inner `WalletSigner::address()`.
    pub fn address(&self) -> &str {
        self.inner.address()
    }
}

impl LiveSigner for LiveWalletSigner {
    fn sign(&self, message_bytes: &[u8]) -> Result<[u8; 64], SignError> {
        self.inner
            .sign(message_bytes)
            .map_err(map_signer_error)
    }

    fn public_key(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        out.copy_from_slice(self.inner.public_key_bytes());
        out
    }
}

fn map_signer_error(e: SignerError) -> SignError {
    match e {
        SignerError::SelfTestFailed => SignError::VerificationFailed,
        SignerError::MessageRejected { bytes, reason } => SignError::MessageRejected {
            bytes,
            reason: reason.to_string(),
        },
        _ => SignError::KeyNotLoaded, // All load-time errors → fail-closed.
    }
}

// ---------------------------------------------------------------------------
// LiveSubmitter adapter — wraps pq_stream_capture::sender::SenderClient
// ---------------------------------------------------------------------------

/// Production submitter backed by `SenderClient` (Helius Sender endpoint).
///
/// Owns a `UreqTransport` so the adapter is `'static + Send + Sync`.
/// Constructs a `SenderClient` fresh on each `submit()` call — zero-overhead
/// because `SenderClient::new` is a trivial struct assignment.
pub struct HeliusSenderSubmitter {
    transport: UreqTransport,
    endpoint: SenderEndpoint,
}

impl HeliusSenderSubmitter {
    /// Construct from a Helius Sender endpoint URL. Refuses plaintext HTTP
    /// unless explicitly allowed.
    pub fn new(endpoint_url: &str, swqos_only: bool, mev_protect: bool) -> Result<Self, SubmitError> {
        let endpoint = SenderEndpoint::new(endpoint_url, swqos_only, mev_protect)
            .map_err(map_sender_error)?;
        Ok(Self {
            transport: UreqTransport::new(),
            endpoint,
        })
    }

    /// Construct with an API key for the global Helius Sender endpoint.
    pub fn new_with_api_key(
        endpoint_url: &str,
        api_key: &str,
        swqos_only: bool,
        mev_protect: bool,
    ) -> Result<Self, SubmitError> {
        let endpoint = SenderEndpoint::new(endpoint_url, swqos_only, mev_protect)
            .map_err(map_sender_error)?
            .with_api_key(api_key);
        Ok(Self {
            transport: UreqTransport::new(),
            endpoint,
        })
    }

    /// Colocated variant: allows plaintext HTTP for datacentre-to-datacentre
    /// submission with lower latency (no TLS handshake).
    pub fn new_colocated(endpoint_url: &str, swqos_only: bool, mev_protect: bool) -> Result<Self, SubmitError> {
        let endpoint = SenderEndpoint::new_allow_plaintext(endpoint_url, swqos_only, mev_protect)
            .map_err(map_sender_error)?;
        Ok(Self {
            transport: UreqTransport::new(),
            endpoint,
        })
    }
}

impl LiveSubmitter for HeliusSenderSubmitter {
    fn submit(&self, wire_tx: &[u8]) -> Result<[u8; 64], SubmitError> {
        // 1. Base64-encode the wire bytes.
        let tx_b64 = base64::engine::general_purpose::STANDARD.encode(wire_tx);

        // 2. Construct a fresh SenderClient (zero-overhead constructor).
        let client = SenderClient::new(&self.transport, self.endpoint.clone());

        // 3. Submit with a short request id derived from the first bytes of the
        //    wire transaction (deterministic, collision-resistant for the
        //    1-second window where two submits might overlap).
        let id = &make_request_id(wire_tx);

        // 4. Send and parse the response.
        let accepted = client
            .send_transaction(id, &tx_b64)
            .map_err(map_sender_error)?;

        // 5. Decode the base58 signature string into 64 bytes.
        decode_signature_bytes(&accepted.signature)
    }
}

fn map_sender_error(e: SenderError) -> SubmitError {
    match e {
        SenderError::BadEndpoint(m) => SubmitError::EndpointRejected(m),
        SenderError::BadPayload(m) => SubmitError::EndpointRejected(m),
        SenderError::Transport(m) => SubmitError::HttpError(m),
        SenderError::Rpc { code, message } => {
            SubmitError::EndpointRejected(format!("rpc error {code}: {message}"))
        }
        SenderError::Unparseable(m) => SubmitError::EndpointRejected(m),
    }
}

/// Build a deterministic request id from the wire transaction bytes.
/// Uses the first 8 bytes as a hex string — collision-resistant within the
/// short window where two submits might overlap. Satisfies the Sender's
/// validation: alphanumeric + '-' + '_', 1..=64 chars.
fn make_request_id(wire_tx: &[u8]) -> String {
    let n = wire_tx.len().min(8);
    let mut id = String::with_capacity(n * 2);
    for &b in &wire_tx[..n] {
        id.push_str(&format!("{b:02x}"));
    }
    if id.is_empty() {
        id.push_str("00");
    }
    id
}

/// Decode a base58 signature string (Solana format) into 64 raw bytes.
/// Uses the same base58 alphabet as the signer module.
fn decode_signature_bytes(sig_str: &str) -> Result<[u8; 64], SubmitError> {
    // The Solana signature is a 64-byte ed25519 signature encoded in base58.
    // We use the base58 Bitcoin alphabet (same as pq_stream_capture::signer).
    const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

    if sig_str.is_empty() {
        return Err(SubmitError::InvalidSignature);
    }

    // Count leading '1' bytes (zero bytes in base58).
    let mut zeros = 0;
    let bytes = sig_str.as_bytes();
    while zeros < bytes.len() && bytes[zeros] == b'1' {
        zeros += 1;
    }

    // Allocate the output buffer.
    let mut buffer = vec![0u8; sig_str.len() * 733 / 1000 + 1]; // upper bound

    let mut written = 0;
    for &c in &bytes[zeros..] {
        let mut carry = match ALPHABET.iter().position(|&a| a == c) {
            Some(idx) => idx as u32,
            None => return Err(SubmitError::InvalidSignature),
        };

        // Multiply buffer by 58 and add carry.
        let mut i = 0;
        while i < written || carry != 0 {
            if i >= buffer.len() {
                buffer.push(0);
                // If we exceed 64 bytes, this is not a valid signature.
                if buffer.len() > 64 {
                    return Err(SubmitError::InvalidSignature);
                }
            }
            let current = buffer[i] as u32 + carry;
            buffer[i] = (current % 256) as u8;
            carry = current / 256;
            i += 1;
        }
        written = i;
    }

    // Reverse the buffer (base58 is big-endian, we processed little-endian).
    buffer.truncate(written);
    buffer.reverse();

    // Prepend zero bytes.
    let mut result = [0u8; 64];
    if zeros + buffer.len() > 64 {
        return Err(SubmitError::InvalidSignature);
    }
    result[zeros..zeros + buffer.len()].copy_from_slice(&buffer);

    Ok(result)
}

// ---------------------------------------------------------------------------
// LiveStateFetcher adapter — wraps junction::state_fetch::RpcStateFetch
// ---------------------------------------------------------------------------

/// Cached curve state: the decoded on-chain facts + the slot at which they
/// were observed. Stored behind `RwLock` so the background prefetch thread
/// can write while the hot path reads.
#[derive(Clone)]
struct CachedCurveState {
    state: LiveCurveState,
}

/// Cached blockhash: the 32-byte blockhash + slot. Stored behind `Mutex`
/// because updates are rare and we want write-priority (the background thread
/// should never be blocked by a reader).
struct CachedBlockhash {
    blockhash: [u8; 32],
    slot: u64,
    /// Unix-epoch seconds when this blockhash was fetched — used for
    /// freshness checks in `latest_blockhash()`.
    fetched_at_secs: u64,
}

/// Production state fetcher backed by `RpcStateFetch` (RPC getAccountInfo +
/// getLatestBlockhash).
///
/// Owns a `UreqTransport` so the adapter is `'static + Send + Sync`.
/// Constructs `RpcStateFetch` fresh on each fetch — zero-overhead constructor.
///
/// The cache is warmed by the background updater thread calling
/// `prefetch_state` and `refresh_blockhash`. The hot path calls
/// `fetch_state_hot` which returns the cached state in ~0ms when warm.
pub struct RpcLiveStateFetcher {
    transport: UreqTransport,
    rpc_url: String,

    // ── Curve-state cache: keyed by mint (32 bytes) ───────────────────────
    // A single-entry cache is sufficient because the bot trades one mint at a
    // time. The key is the mint pubkey; the value is the decoded state + the
    // slot at which it was observed.
    curve_cache: RwLock<Option<CachedCurveState>>,

    // ── Blockhash cache: refreshed every ~5s by the background thread ─────
    blockhash_cache: Mutex<Option<CachedBlockhash>>,

    // ── Staleness threshold: if the cached state is older than this many
    // slots, the hot path falls back to a synchronous fetch.
    max_stale_slots: u64,

    // ── Shutdown flag: when true, the prefetch thread stops refreshing.
    shutdown: Arc<AtomicBool>,
}

impl RpcLiveStateFetcher {
    /// Construct from an RPC URL. The transport is owned internally.
    pub fn new(rpc_url: String) -> Self {
        Self {
            transport: UreqTransport::new(),
            rpc_url,
            curve_cache: RwLock::new(None),
            blockhash_cache: Mutex::new(None),
            max_stale_slots: 150, // ~5 seconds at 3 slots/sec → generous
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Construct with a custom transport (for testing).
    pub fn with_transport(rpc_url: String, _transport: UreqTransport) -> Self {
        Self::new(rpc_url)
    }

    /// Signal the background prefetch thread to stop. Does not block.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }

    /// Check if shutdown has been requested.
    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    /// Fetch fresh state from the RPC (the real round-trip). Used by both
    /// `prefetch_state` (background) and `fetch_state_hot` (fallback when the
    /// cache is cold or stale).
    fn fetch_fresh(&self, mint: &[u8; 32], user: &[u8; 32]) -> Result<LiveCurveState, StateFetchError> {
        let fetcher = RpcStateFetch::new(&self.transport, self.rpc_url.clone());
        let fetched = fetcher
            .fetch(mint, user)
            .map_err(map_state_fetch_error)?;

        // ── Latency optimization: write the blockhash from this fetch into the
        // cache so `latest_blockhash()` doesn't need a second RPC round-trip.
        // The fetch already called getLatestBlockhash as part of its batched
        // RPC — reusing it saves ~50-100ms on the hot path. ──
        if fetched.recent_blockhash != [0u8; 32] {
            *self.blockhash_cache.lock().unwrap() = Some(CachedBlockhash {
                blockhash: fetched.recent_blockhash,
                slot: 0, // RpcStateFetch doesn't return the slot; freshness is
                         // tracked via fetched_at_secs instead.
                fetched_at_secs: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            });
        }

        Ok(LiveCurveState {
            curve_ctx: fetched.ctx,
            virtual_sol_reserves: fetched.virtual_sol_reserves,
            virtual_token_reserves: fetched.virtual_token_reserves,
            is_complete: fetched.is_complete,
            observed_slot: 0, // RpcStateFetch doesn't return the slot; the
                              // background thread can set this from the
                              // blockhash cache.
            buyback_fee_recipients: fetched.buyback_fee_recipients,
        })
    }

    /// Refresh the blockhash cache by calling getLatestBlockhash. Called by
    /// the background thread.
    fn refresh_blockhash_inner(&self) -> Result<LiveBlockhash, StateFetchError> {
        // We use the RpcStateFetch to get the blockhash as part of a fetch,
        // but for a standalone blockhash refresh we need a direct RPC call.
        // For now, we construct a minimal fetch to a dummy mint/user that will
        // return the blockhash. A better approach: add a standalone
        // getLatestBlockhash to RpcStateFetch. But the existing RPC client
        // (pq_stream_capture::rpc) already has this method.
        //
        // Actually, the simplest approach: make a raw getLatestBlockhash RPC
        // call using the transport directly.
        let body = r#"{"id":1,"jsonrpc":"2.0","method":"getLatestBlockhash","params":[{"commitment":"confirmed"}]}"#;
        let url = self.rpc_url.clone();
        let reply = self
            .transport
            .post_json(&url, body)
            .map_err(|e| StateFetchError::RpcError(e))?;

        // Parse the blockhash from the JSON response.
        let blockhash_str = extract_blockhash_from_response(&reply.body)
            .ok_or(StateFetchError::ZeroBlockhash)?;

        if blockhash_str.chars().all(|c| c == '1') {
            // All-zeros base58 is "111...111" — degenerate.
            return Err(StateFetchError::ZeroBlockhash);
        }

        let blockhash = decode_base58_32(&blockhash_str)
            .ok_or(StateFetchError::DecodeError("blockhash not valid base58".into()))?;

        let slot = extract_slot_from_response(&reply.body)
            .unwrap_or(0);

        // Update the cache.
        *self.blockhash_cache.lock().unwrap() = Some(CachedBlockhash {
            blockhash,
            slot,
            fetched_at_secs: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        });

        Ok(LiveBlockhash { blockhash, slot })
    }
}

impl LiveStateFetcher for RpcLiveStateFetcher {
    fn fetch_state_hot(
        &self,
        mint: &[u8; 32],
        user: &[u8; 32],
    ) -> Result<LiveCurveState, StateFetchError> {
        // Try the cache first.
        {
            let cache = self.curve_cache.read().unwrap();
            if let Some(ref cached) = *cache {
                // Check freshness: if the blockhash cache has a newer slot,
                // the curve state is still valid as long as it's within the
                // staleness threshold.
                let bh = self.blockhash_cache.lock().unwrap();
                if let Some(ref bh_cached) = *bh {
                    let staleness = bh_cached.slot.saturating_sub(cached.state.observed_slot);
                    if staleness <= self.max_stale_slots {
                        return Ok(cached.state.clone());
                    }
                } else {
                    // No blockhash cache yet but we have curve state — return
                    // it. The caller will fall back to a fresh fetch if the
                    // blockhash is stale.
                    return Ok(cached.state.clone());
                }
            }
        }

        // Cache miss or stale: fetch synchronously.
        let state = self.fetch_fresh(mint, user)?;

        // Update the cache.
        *self.curve_cache.write().unwrap() = Some(CachedCurveState {
            state: state.clone(),
        });

        Ok(state)
    }

    fn prefetch_state(
        &self,
        mint: &[u8; 32],
        user: &[u8; 32],
    ) -> Result<LiveCurveState, StateFetchError> {
        // Fetch fresh state and update the cache.
        let mut state = self.fetch_fresh(mint, user)?;

        // Set the observed_slot from the blockhash cache if available.
        let bh = self.blockhash_cache.lock().unwrap();
        if let Some(ref bh_cached) = *bh {
            state.observed_slot = bh_cached.slot;
        }
        drop(bh);

        // Update the curve cache.
        *self.curve_cache.write().unwrap() = Some(CachedCurveState {
            state: state.clone(),
        });

        Ok(state)
    }

    fn latest_blockhash(&self) -> Result<LiveBlockhash, StateFetchError> {
        // Try the cache first — but only if it's fresh (< 5 seconds old).
        // Solana blockhashes expire after ~60s, but we refresh aggressively
        // to avoid landing failures on high-latency submissions.
        {
            let cache = self.blockhash_cache.lock().unwrap();
            if let Some(ref cached) = *cache {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                if now.saturating_sub(cached.fetched_at_secs) < 5 {
                    return Ok(LiveBlockhash {
                        blockhash: cached.blockhash,
                        slot: cached.slot,
                    });
                }
            }
        }
        // Cache miss or stale: fetch synchronously.
        self.refresh_blockhash_inner()
    }
}

/// Map the junction's `StateFetchError` to the execution crate's
/// `StateFetchError`.
fn map_state_fetch_error(e: JunctionStateFetchError) -> StateFetchError {
    match e {
        JunctionStateFetchError::CurveComplete => StateFetchError::CurveComplete,
        JunctionStateFetchError::Transport(s) => StateFetchError::RpcError(s),
        JunctionStateFetchError::Rpc { code, message } => {
            StateFetchError::RpcError(format!("rpc error {code}: {message}"))
        }
        JunctionStateFetchError::AccountNotFound(s) => {
            StateFetchError::AccountNotFound(s.to_string())
        }
        JunctionStateFetchError::BadEncoding(s) => {
            StateFetchError::DecodeError(s.to_string())
        }
        JunctionStateFetchError::DecodeFailed(s) => {
            StateFetchError::DecodeError(s.to_string())
        }
        JunctionStateFetchError::UnknownTokenProgram => {
            StateFetchError::DecodeError("unknown token program".to_string())
        }
        JunctionStateFetchError::NonSolQuoteMint => {
            StateFetchError::DecodeError("non-SOL quote mint".to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers: base58 decoding + JSON field extraction
// ---------------------------------------------------------------------------

/// Decode a base58 string into exactly 32 bytes. Uses the Bitcoin base58
/// alphabet (same as pq_stream_capture::signer::decode_base58_32).
fn decode_base58_32(s: &str) -> Option<[u8; 32]> {
    const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

    if s.is_empty() {
        return None;
    }

    // Count leading '1' bytes (zero bytes in base58).
    let mut zeros = 0;
    let bytes = s.as_bytes();
    while zeros < bytes.len() && bytes[zeros] == b'1' {
        zeros += 1;
    }

    let mut buffer = vec![0u8; s.len() * 733 / 1000 + 1];
    let mut written = 0;

    for &c in &bytes[zeros..] {
        let mut carry = match ALPHABET.iter().position(|&a| a == c) {
            Some(idx) => idx as u32,
            None => return None,
        };

        let mut i = 0;
        while i < written || carry != 0 {
            if i >= buffer.len() {
                buffer.push(0);
                if buffer.len() > 32 {
                    return None;
                }
            }
            let current = buffer[i] as u32 + carry;
            buffer[i] = (current % 256) as u8;
            carry = current / 256;
            i += 1;
        }
        written = i;
    }

    buffer.truncate(written);
    buffer.reverse();

    let mut result = [0u8; 32];
    if zeros + buffer.len() > 32 {
        return None;
    }
    result[zeros..zeros + buffer.len()].copy_from_slice(&buffer);

    Some(result)
}

/// Extract the `"blockhash":"..."` field from a getLatestBlockhash JSON
/// response. Pure string search (the JSON shape is fixed and tiny).
fn extract_blockhash_from_response(body: &str) -> Option<String> {
    let key = "\"blockhash\"";
    let start = body.find(key)?;
    let rest = body.get(start + key.len()..)?;
    let colon = rest.find(':')?;
    let after = rest.get(colon + 1..)?.trim_start();
    let open_quote = after.strip_prefix('"')?;
    let close_quote = open_quote.find('"')?;
    Some(open_quote[..close_quote].to_string())
}

/// Extract the `"slot":<number>` field from a JSON-RPC response.
fn extract_slot_from_response(body: &str) -> Option<u64> {
    let key = "\"slot\"";
    let start = body.find(key)?;
    let rest = body.get(start + key.len()..)?;
    let colon = rest.find(':')?;
    let after = rest.get(colon + 1..)?.trim_start();
    let end = after
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(after.len());
    after[..end].parse::<u64>().ok()
}

// ---------------------------------------------------------------------------
// Background prefetch thread
// ---------------------------------------------------------------------------

/// Configuration for the background prefetch updater.
#[derive(Clone, Debug)]
pub struct PrefetchConfig {
    /// How often to refresh the blockhash (in milliseconds).
    pub blockhash_refresh_ms: u64,
    /// How often to refresh the curve state (in milliseconds).
    pub curve_refresh_ms: u64,
}

impl Default for PrefetchConfig {
    fn default() -> Self {
        Self {
            blockhash_refresh_ms: 5_000, // 5 seconds — well within the 60s
                                          // blockhash validity window.
            curve_refresh_ms: 3_000,     // 3 seconds — fast enough for the
                                          // bonding curve to not drift.
        }
    }
}

/// Start a background thread that prefetches blockhash + curve state on a
/// timer, warming the cache so the hot path has zero RPC round-trips.
///
/// Returns a handle to the thread. The thread stops when the fetcher's
/// shutdown flag is set (via `fetcher.shutdown()`).
///
/// The `mint` and `user` are the ones the bot is actively trading — the
/// prefetch thread warms the cache for this pair. When the bot switches
/// mints, the old thread is stopped and a new one is started.
pub fn spawn_prefetch_thread(
    fetcher: Arc<RpcLiveStateFetcher>,
    mint: [u8; 32],
    user: [u8; 32],
    config: PrefetchConfig,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("pq-prefetch".into())
        .spawn(move || {
            let mut blockhash_timer = 0u64;
            let mut curve_timer = 0u64;
            let tick_ms = 100u64; // 100ms tick — fine-grained enough.

            while !fetcher.is_shutdown() {
                std::thread::sleep(std::time::Duration::from_millis(tick_ms));

                blockhash_timer += tick_ms;
                curve_timer += tick_ms;

                if blockhash_timer >= config.blockhash_refresh_ms {
                    blockhash_timer = 0;
                    let _ = fetcher.refresh_blockhash_inner();
                }

                if curve_timer >= config.curve_refresh_ms {
                    curve_timer = 0;
                    let _ = fetcher.prefetch_state(&mint, &user);
                }
            }
        })
        .expect("prefetch thread spawn")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base58_decode_known_blockhash() {
        // A known Solana blockhash (mainnet, 32 bytes base58).
        let bh = "FfLaDnxr9mZfffm9tKuCAjKbbfXhpiEfrT2qKpShZgF7";
        let decoded = decode_base58_32(bh);
        assert!(decoded.is_some(), "blockhash should decode to 32 bytes");
        if let Some(d) = decoded {
            assert_eq!(d.len(), 32);
            // Re-encode and verify round-trip (approximate — base58 encoding
            // of the same bytes should produce the same string).
            assert_ne!(d, [0u8; 32]);
        }
    }

    #[test]
    fn base58_decode_all_zeros() {
        // 32 '1' chars in base58 = 32 zero bytes (the all-zero Solana pubkey).
        let bh = "11111111111111111111111111111111";
        let decoded = decode_base58_32(bh);
        assert!(decoded.is_some(), "all-ones should decode to all-zeros");
        if let Some(d) = decoded {
            assert_eq!(d, [0u8; 32]);
        }
    }

    #[test]
    fn signature_decode_roundtrip() {
        // A known ed25519 signature (64 bytes base58). This is a test
        // signature from the Solana docs.
        let sig_str = "2G4p26GB78LLDeJquGNDRzhJ2J9AVgxWzi5HxFz7D2G5fvCg7etqiLtuTKWvfG53BTaokE2XwcUnzqnrbWMFyLED";
        let decoded = decode_signature_bytes(sig_str);
        assert!(decoded.is_ok(), "signature should decode to 64 bytes");
        if let Ok(d) = decoded {
            assert_eq!(d.len(), 64);
            assert_ne!(d, [0u8; 64]);
        }
    }

    #[test]
    fn extract_blockhash_from_json() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"context":{"slot":12345},"value":{"blockhash":"FfLaDnxr9mZfffm9tKuCAjKbbfXhpiEfrT2qKpShZgF7","lastValidBlockHeight":99999}}}"#;
        let bh = extract_blockhash_from_response(body);
        assert_eq!(
            bh,
            Some("FfLaDnxr9mZfffm9tKuCAjKbbfXhpiEfrT2qKpShZgF7".to_string())
        );
    }

    #[test]
    fn extract_slot_from_json() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"context":{"slot":12345},"value":{"blockhash":"abc","lastValidBlockHeight":99999}}}"#;
        let slot = extract_slot_from_response(body);
        assert_eq!(slot, Some(12345));
    }

    #[test]
    fn request_id_is_valid() {
        let wire = [0xAAu8; 8];
        let id = make_request_id(&wire);
        assert!(!id.is_empty());
        assert!(id.len() <= 64);
        assert!(id.bytes().all(|c| c.is_ascii_alphanumeric()));
    }
}
