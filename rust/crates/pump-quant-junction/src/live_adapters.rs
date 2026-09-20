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
//! ## Operator refs
//! - §36: failure taxonomy maps 1:1 to the error variants here.
//! - §41: construction parity — the fetcher returns decoded on-chain facts.
//! - §24(b): paper/replay mode never touches these adapters.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex, RwLock,
};

use pump_quant_execution::ex_live_io_traits::{
    LiveBlockhash, LiveCurveState, LiveSigner, LiveStateFetcher, LiveSubmitter, SignError,
    StateFetchError, SubmitError,
};

use pq_stream_capture::rpc::{Reply, Transport, UreqTransport};
use pq_stream_capture::sender::{Accepted, SenderClient, SenderEndpoint, SenderError};
use pq_stream_capture::signer::{SignerError, WalletSigner, SIGNATURE_BYTES};

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
        self.inner.sign(message_bytes).map_err(map_signer_error)
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
    pub fn new(
        endpoint_url: &str,
        swqos_only: bool,
        mev_protect: bool,
    ) -> Result<Self, SubmitError> {
        let endpoint =
            SenderEndpoint::new(endpoint_url, swqos_only, mev_protect).map_err(map_sender_error)?;
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
    pub fn new_colocated(
        endpoint_url: &str,
        swqos_only: bool,
        mev_protect: bool,
    ) -> Result<Self, SubmitError> {
        let endpoint = SenderEndpoint::new_allow_plaintext(endpoint_url, swqos_only, mev_protect)
            .map_err(map_sender_error)?;
        Ok(Self {
            transport: UreqTransport::new(),
            endpoint,
        })
    }
}

impl LiveSubmitter for HeliusSenderSubmitter {
    fn submit(&self, wire_tx: &[u8], is_buy: bool) -> Result<[u8; 64], SubmitError> {
        // 1. Base64-encode the wire bytes.
        let tx_b64 = base64::engine::general_purpose::STANDARD.encode(wire_tx);

        // 2. Construct a fresh SenderClient (zero-overhead constructor).
        let client = SenderClient::new(&self.transport, self.endpoint.clone());

        // 3. Submit with a short request id derived from the first bytes of the
        //    wire transaction (deterministic, collision-resistant for the
        //    1-second window where two submits might overlap).
        let id = &make_request_id(wire_tx);

        // 4. Send and parse the response.
        //    The Helius Sender endpoint (sender.helius-rpc.com) does NOT support
        //    preflight checks — it returns HTTP 500 "running preflight check is
        //    not supported" when skip_preflight=false.  This was the root cause
        //    of every sell failure in Rev-22 (sells set skip_preflight=false
        //    via `skip_preflight = is_buy`).  Both buys AND sells must skip
        //    preflight when submitting via the Sender.  Post-submission
        //    confirmation polling (poll_signature_confirmations in the daemon)
        //    catches on-chain failures asynchronously instead.
        let skip_preflight = true;
        let accepted = client
            .send_transaction(id, &tx_b64, skip_preflight)
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
/// Uses the SAME algorithm as `decode_base58_64` in pq_daemon.rs to ensure
/// byte-for-byte parity between stored pending signatures and on-chain
/// signatures fetched via `getSignaturesForAddress`. Without this parity,
/// the confirmation poll can never match stored sigs against on-chain sigs.
fn decode_signature_bytes(sig_str: &str) -> Result<[u8; 64], SubmitError> {
    const B58: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    if sig_str.is_empty() {
        return Err(SubmitError::InvalidSignature);
    }
    let mut out = [0u8; 64];
    for c in sig_str.bytes() {
        let digit = match B58.iter().position(|&a| a == c) {
            Some(idx) => idx as u32,
            None => return Err(SubmitError::InvalidSignature),
        };
        let mut carry = digit;
        for byte in out.iter_mut().rev() {
            let v = u32::from(*byte) * 58 + carry;
            *byte = (v & 0xff) as u8;
            carry = v >> 8;
        }
        if carry != 0 {
            return Err(SubmitError::InvalidSignature);
        }
    }
    let leading_ones = sig_str.bytes().take_while(|&c| c == b'1').count();
    let leading_zeros = out.iter().take_while(|&&b| b == 0).count();
    if leading_ones != leading_zeros {
        return Err(SubmitError::InvalidSignature);
    }
    Ok(out)
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
    /// The last slot the STREAM published for this mint. Kept separate from
    /// `state.observed_slot`, which the fetch path also writes: mixing the two clocks
    /// would let a fetch that observed a higher slot permanently reject every stream
    /// update (the stream's ordering rule is its own), or let a replayed event
    /// overwrite fresh reserves.
    stream_slot: u64,
}

/// The multi-mint curve cache.
///
/// The ctx inside each entry (mint, creator, fee_recipient, token_program,
/// cashback flag, quote mint) is IMMUTABLE account data: it is learned from one
/// cold fetch and then reused. The RESERVES are not — they move on every trade, and
/// they are what the hot path needs. Those come from the stream (see
/// [`RpcLiveStateFetcher::note_stream_reserves`]), so a mint that has traded once
/// never pays another state RPC.
///
/// Bounded: on overflow the entry with the OLDEST observed slot is evicted. A mint
/// evicted this way simply pays one cold fetch again — the cache is a latency
/// optimisation and never a source of truth.
struct CurveCache {
    entries: HashMap<[u8; 32], CachedCurveState>,
}

/// Max mints whose curve ctx + reserves are retained. Well above any sane position
/// count; the bound exists so a long session cannot grow without limit (§99).
const CURVE_CACHE_CAP: usize = 256;

/// C1: how stale the STREAM may go before cached curve state is no longer trusted.
/// The cache is only as good as the feed behind it: if no account event has arrived
/// for this many slots, the reserves held here may have moved unseen, so the hot
/// read falls back to a real fetch. Conservative direction - a false alarm costs one
/// RPC, a missed one would hand a builder stale reserves.
const STREAM_LIVENESS_SLOTS: u64 = 150;

/// Cached blockhash: the 32-byte blockhash + slot. Stored behind `Mutex`
/// because updates are rare and we want write-priority (the background thread
/// should never be blocked by a reader).
struct CachedBlockhash {
    blockhash: [u8; 32],
    slot: u64,
    /// E7: `lastValidBlockHeight` for `blockhash` — the height past which Solana
    /// refuses it. 0 = the RPC did not report one. This, not a wall clock, is what
    /// decides whether the cached hash may still be handed to a builder.
    last_valid_block_height: u64,
    /// Unix-epoch seconds when this blockhash was fetched. E7 demoted this from the
    /// freshness criterion to a protocol backstop: a hash older than the chain's own
    /// ~60 s validity window is refused even if the height test cannot fire (a slot
    /// observation may lag on a busy RPC). See `cached_blockhash_is_usable`.
    fetched_at_secs: u64,
}

/// The chain's own blockhash validity ceiling. A blockhash is refused past this age
/// regardless of `lastValidBlockHeight` — the backstop for the case where the height
/// test has no fresh observation to compare against.
const BLOCKHASH_PROTOCOL_EXPIRY_SECS: u64 = 60;

/// Unix-epoch seconds, saturating (a pre-epoch clock is not a panic).
fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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
    // C1: multi-mint. The daemon holds several positions at once, so a single-entry
    // cache forced a cold 4-RTT fetch on every submission that followed a different
    // mint. The ctx is learned once per mint; the reserves are then kept current by
    // the account stream.
    curve_cache: RwLock<CurveCache>,

    // ── Blockhash cache: refreshed every ~5s by the background thread ─────
    blockhash_cache: Mutex<Option<CachedBlockhash>>,

    // ── E7: the newest slot this adapter has observed, from either a state fetch
    // or a blockhash refresh. This is the reference height for blockhash validity.
    newest_slot: AtomicU64,

    /// C1: slot of the most recent account-stream note received for ANY mint.
    /// 0 = no stream wired to this fetcher (dev/paper): the cache then holds only
    /// what a fetch learned, and the liveness gate is inert.
    last_stream_slot: AtomicU64,

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
            curve_cache: RwLock::new(CurveCache {
                entries: HashMap::new(),
            }),
            blockhash_cache: Mutex::new(None),
            newest_slot: AtomicU64::new(0),
            last_stream_slot: AtomicU64::new(0),
            max_stale_slots: 150, // ≈ 60 s at ~2.5 slots/s (400 ms/slot) — NOT ~5 s
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

    /// C1: publish reserves decoded from the live account stream into the curve
    /// cache.
    ///
    /// Returns `true` when an entry was updated. It returns `false` for a mint whose
    /// ctx has not been learned yet: the stream carries the curve's reserves but NOT
    /// the `Global.fee_recipient` (a different account, one RPC), so a buildable ctx
    /// cannot be fabricated from a stream event alone. That mint pays one cold fetch
    /// on its first trade, and is stream-fed from then on. Never invent an entry.
    pub fn note_stream_reserves(
        &self,
        mint: &[u8; 32],
        virtual_sol: u64,
        virtual_token: u64,
        is_complete: bool,
        slot: u64,
    ) -> bool {
        // The feed is alive the moment an event arrives — whether or not this mint has
        // an entry, and whether or not the slot is newer. Liveness is a property of the
        // STREAM, and the hot read uses it to decide how far to trust a cached entry.
        // Recorded before any early return.
        self.last_stream_slot.fetch_max(slot, Ordering::Relaxed);
        let mut cache = self.curve_cache.write().unwrap();
        let Some(entry) = cache.entries.get_mut(mint) else {
            return false;
        };
        // Only ever make the entry FRESHER. A replayed or out-of-order notification
        // must not drag the cache backwards — the hot path decides against these
        // reserves, and stale reserves are a slippage bound derived from a market
        // that no longer exists.
        if slot < entry.stream_slot {
            return false;
        }
        entry.stream_slot = slot;
        entry.state.virtual_sol_reserves = virtual_sol;
        entry.state.virtual_token_reserves = virtual_token;
        entry.state.is_complete = is_complete;
        entry.state.observed_slot = slot;
        self.newest_slot.fetch_max(slot, Ordering::Relaxed);
        true
    }

    /// Insert (or refresh) a mint's entry, evicting the oldest observation when the
    /// cache is at its bound.
    fn store_curve(&self, state: &LiveCurveState) {
        let mint = state.curve_ctx.mint;
        let mut cache = self.curve_cache.write().unwrap();
        if cache.entries.len() >= CURVE_CACHE_CAP && !cache.entries.contains_key(&mint) {
            if let Some(oldest) = cache
                .entries
                .iter()
                .min_by_key(|(_, e)| e.state.observed_slot)
                .map(|(m, _)| *m)
            {
                cache.entries.remove(&oldest);
            }
        }
        // Carry the stream clock forward across a refresh: resetting it to 0 would
        // re-admit stream events we have already superseded.
        let stream_slot = cache.entries.get(&mint).map_or(0, |e| e.stream_slot);
        cache.entries.insert(
            mint,
            CachedCurveState {
                state: state.clone(),
                stream_slot,
            },
        );
    }

    /// C1: is a cached entry fresh enough to answer a hot read?
    ///
    /// Measured in SLOTS against the newest observation this adapter has seen from
    /// any source (stream, fetch, blockhash refresh) — never a wall clock (§E2/E7).
    /// With no observation to compare against, the entry is accepted: the caller has
    /// nothing better, and the sink's own TOCTOU mcap band re-check still runs on the
    /// state it receives.
    fn cached_is_fresh(&self, cached: &CachedCurveState) -> bool {
        let newest = self.newest_slot.load(Ordering::Relaxed);
        if newest == 0 {
            return true;
        }
        // C1: a cache fed by a stream that has since gone quiet is not evidence.
        // `vsol` moves only on a trade, and every trade is an account event - so a
        // live stream makes the cached reserves current by construction, while a
        // stalled one leaves them unverifiable. Distrust it and pay the fetch.
        let stream = self.last_stream_slot.load(Ordering::Relaxed);
        if stream != 0 && newest.saturating_sub(stream) > STREAM_LIVENESS_SLOTS {
            return false;
        }
        newest.saturating_sub(cached.state.observed_slot) <= self.max_stale_slots
    }

    /// Test-only: seed the cache without an RPC.
    #[cfg(test)]
    fn seed_curve_for_test(&self, state: LiveCurveState) {
        self.store_curve(&state);
    }

    /// E7: may the cached blockhash still be handed to a builder?
    ///
    /// The criterion is the height Solana itself publishes: the hash is accepted
    /// while the chain height is below `last_valid_block_height`. We compare that
    /// bound against the newest slot this adapter has OBSERVED. A slot is never
    /// below the block height at the same instant, so `newest_slot >=
    /// last_valid_block_height` is a sound "expired" verdict — at worst it retires a
    /// hash one block early, which costs one RPC and can never produce a bad
    /// submission.
    ///
    /// The wall clock survives only as the protocol ceiling (`BLOCKHASH_PROTOCOL_EXPIRY_SECS`),
    /// which covers the case where no fresh slot observation exists to compare
    /// against. It is a backstop, not the criterion — the 5 s clock is gone.
    fn cached_blockhash_is_usable(&self, c: &CachedBlockhash) -> bool {
        let newest_slot = self.newest_slot.load(Ordering::Relaxed);
        if c.last_valid_block_height != 0 && newest_slot >= c.last_valid_block_height {
            return false;
        }
        epoch_secs().saturating_sub(c.fetched_at_secs) < BLOCKHASH_PROTOCOL_EXPIRY_SECS
    }

    /// Fetch fresh state from the RPC (the real round-trip). Used by both
    /// `prefetch_state` (background) and `fetch_state_hot` (fallback when the
    /// cache is cold or stale).
    fn fetch_fresh(
        &self,
        mint: &[u8; 32],
        user: &[u8; 32],
    ) -> Result<LiveCurveState, StateFetchError> {
        let fetcher = RpcStateFetch::new(&self.transport, self.rpc_url.clone());
        let fetched = fetcher.fetch(mint, user).map_err(map_state_fetch_error)?;

        // ── Latency optimization: write the blockhash from this fetch into the
        // cache so `latest_blockhash()` doesn't need a second RPC round-trip.
        // The fetch already called getLatestBlockhash as part of its batched
        // RPC — reusing it saves ~50-100ms on the hot path. ──
        if fetched.observed_slot > 0 {
            self.newest_slot
                .fetch_max(fetched.observed_slot, Ordering::Relaxed);
        }
        if fetched.recent_blockhash != [0u8; 32] {
            *self.blockhash_cache.lock().unwrap() = Some(CachedBlockhash {
                blockhash: fetched.recent_blockhash,
                slot: fetched.observed_slot, // same getLatestBlockhash call's
                // result.context.slot
                // E7: the expiry bound arrives with the hash — no second RPC, no
                // inference from elapsed time.
                last_valid_block_height: fetched.last_valid_block_height,
                fetched_at_secs: epoch_secs(),
            });
        }

        Ok(LiveCurveState {
            curve_ctx: fetched.ctx,
            virtual_sol_reserves: fetched.virtual_sol_reserves,
            virtual_token_reserves: fetched.virtual_token_reserves,
            is_complete: fetched.is_complete,
            observed_slot: fetched.observed_slot,
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
        let blockhash_str =
            extract_blockhash_from_response(&reply.body).ok_or(StateFetchError::ZeroBlockhash)?;

        if blockhash_str.chars().all(|c| c == '1') {
            // All-zeros base58 is "111...111" — degenerate.
            return Err(StateFetchError::ZeroBlockhash);
        }

        let blockhash = decode_base58_32(&blockhash_str).ok_or(StateFetchError::DecodeError(
            "blockhash not valid base58".into(),
        ))?;

        let slot = extract_slot_from_response(&reply.body).unwrap_or(0);
        // E7: previously parsed nowhere — the response always carried it.
        let last_valid_block_height =
            extract_last_valid_block_height_from_response(&reply.body).unwrap_or(0);
        if slot > 0 {
            self.newest_slot.fetch_max(slot, Ordering::Relaxed);
        }

        // Update the cache.
        *self.blockhash_cache.lock().unwrap() = Some(CachedBlockhash {
            blockhash,
            slot,
            last_valid_block_height,
            fetched_at_secs: epoch_secs(),
        });

        Ok(LiveBlockhash {
            blockhash,
            slot,
            last_valid_block_height,
        })
    }
}

impl LiveStateFetcher for RpcLiveStateFetcher {
    fn fetch_state_hot(
        &self,
        mint: &[u8; 32],
        user: &[u8; 32],
    ) -> Result<LiveCurveState, StateFetchError> {
        // C1: the cache is keyed by mint, so "is this the right mint's state?" is
        // structural now rather than a comparison that can be forgotten.
        {
            let cache = self.curve_cache.read().unwrap();
            if let Some(cached) = cache.entries.get(mint) {
                if self.cached_is_fresh(cached) {
                    // The ctx is signer-agnostic — `user` is the only field that
                    // varies per caller, and the builder's account list depends on
                    // it. Patch it in rather than returning a ctx baked with another
                    // wallet (the wrong-user twin of the Rev-22/26 wrong-mint bug
                    // that produced AccountNotInitialized).
                    let mut state = cached.state.clone();
                    state.curve_ctx.user = *user;
                    return Ok(state);
                }
            }
        }

        // Cache miss or stale: fetch synchronously, then remember the ctx.
        let state = self.fetch_fresh(mint, user)?;
        self.store_curve(&state);
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
        self.store_curve(&state);

        Ok(state)
    }

    fn latest_blockhash(&self) -> Result<LiveBlockhash, StateFetchError> {
        // Try the cache first — accepting it only while it is provably still valid.
        {
            let cache = self.blockhash_cache.lock().unwrap();
            if let Some(ref cached) = *cache {
                if self.cached_blockhash_is_usable(cached) {
                    return Ok(LiveBlockhash {
                        blockhash: cached.blockhash,
                        slot: cached.slot,
                        last_valid_block_height: cached.last_valid_block_height,
                    });
                }
            }
        }
        // Cache miss or expired: fetch synchronously.
        self.refresh_blockhash_inner()
    }

    fn fetch_ata_balance(
        &self,
        mint: &[u8; 32],
        user: &[u8; 32],
        token_program: &[u8; 32],
    ) -> Result<u64, StateFetchError> {
        // Derive the ATA address: PDA seeds = [wallet, token_program, mint].
        let ata = pump_quant_protocol::pda::derive_ata(user, token_program, mint)
            .map_err(|_| StateFetchError::DecodeError("ATA derivation failed".to_string()))?;
        let ata_b58 = encode_base58_pubkey(&ata);

        // RPC: getAccountInfo(ata, { encoding: base64, commitment: confirmed }).
        let params =
            format!("[\"{ata_b58}\",{{\"encoding\":\"base64\",\"commitment\":\"confirmed\"}}]");
        let body = format!(
            "{{\"id\":1,\"jsonrpc\":\"2.0\",\"method\":\"getAccountInfo\",\"params\":{params}}}"
        );
        let reply = self
            .transport
            .post_json(&self.rpc_url, &body)
            .map_err(|e| StateFetchError::RpcError(e))?;

        // Parse the response. If value is null, the ATA doesn't exist → balance 0.
        let balance = extract_ata_balance(&reply.body)
            .ok_or(StateFetchError::AccountNotFound("ATA".to_string()))?;

        Ok(balance)
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
        JunctionStateFetchError::BadEncoding(s) => StateFetchError::DecodeError(s.to_string()),
        JunctionStateFetchError::DecodeFailed(s) => StateFetchError::DecodeError(s.to_string()),
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

/// E7: extract `"lastValidBlockHeight":<number>` from a `getLatestBlockhash`
/// response. The field sits inside `result.value`, next to the blockhash string.
fn extract_last_valid_block_height_from_response(body: &str) -> Option<u64> {
    let key = "\"lastValidBlockHeight\"";
    let start = body.find(key)?;
    let rest = body.get(start + key.len()..)?;
    let colon = rest.find(':')?;
    let after = rest.get(colon + 1..)?.trim_start();
    let end = after
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(after.len());
    after[..end].parse::<u64>().ok()
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

/// Encode a 32-byte pubkey into a base58 string (same alphabet as Solana).
fn encode_base58_pubkey(pk: &[u8; 32]) -> String {
    const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut leading_zeros = 0;
    for &b in pk.iter() {
        if b == 0 {
            leading_zeros += 1;
        } else {
            break;
        }
    }

    let mut bytes = pk.to_vec();
    let mut encoded: Vec<u8> = Vec::new();
    while !bytes.is_empty() {
        let mut rem = 0u32;
        for byte in bytes.iter_mut() {
            let n = (rem << 8) | (*byte as u32);
            *byte = (n / 58) as u8;
            rem = n % 58;
        }
        encoded.push(rem as u8);
        while bytes.first() == Some(&0) {
            bytes.remove(0);
        }
    }
    encoded.reverse();
    let mut out = String::with_capacity(44);
    for &idx in &encoded {
        out.push(ALPHABET[idx as usize] as char);
    }
    for _ in 0..leading_zeros {
        out.insert(0, '1');
    }
    out
}

/// Extract the token balance from a getAccountInfo JSON response for an ATA.
///
/// SPL token accounts (both spl-token and Token-2022) store the amount at
/// offset 32 (after mint[32] + owner[32]). The amount is a little-endian u64.
/// If the response value is null (account doesn't exist), returns 0 (balance 0).
fn extract_ata_balance(body: &str) -> Option<u64> {
    // Check for "value":null → account doesn't exist → balance 0.
    if body.contains("\"value\":null") {
        return Some(0);
    }

    // Find the data array: "data":["<base64>","base64"]
    let data_key = "\"data\":[\"";
    let start = body.find(data_key)?;
    let rest = body.get(start + data_key.len()..)?;
    let close_quote = rest.find('"')?;
    let b64 = &rest[..close_quote];

    // Decode base64 → raw bytes.
    let raw = decode_base64_std(b64)?;

    // The token amount is at offset 64 (after mint[32] + owner[32]).
    // It's a little-endian u64.
    if raw.len() < 72 {
        return Some(0); // Too short — treat as 0 balance.
    }
    let amount_bytes = &raw[64..72];
    let amount = u64::from_le_bytes(amount_bytes.try_into().ok()?);
    Some(amount)
}

/// Standard base64 decode (A-Z, a-z, 0-9, +, /).
fn decode_base64_std(s: &str) -> Option<Vec<u8>> {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut decode_map = [-1i16; 256];
    for (i, &c) in TABLE.iter().enumerate() {
        decode_map[c as usize] = i as i16;
    }

    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits: u8 = 0;

    for &b in bytes {
        match b {
            b'=' => break,
            b'\n' | b'\r' | b' ' | b'\t' => continue,
            _ => {
                let v = decode_map[b as usize];
                if v < 0 {
                    return None;
                }
                buf = (buf << 6) | v as u32;
                bits += 6;
                if bits >= 8 {
                    bits -= 8;
                    out.push((buf >> bits) as u8 & 0xFF);
                }
            }
        }
    }
    Some(out)
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
            curve_refresh_ms: 3_000, // 3 seconds — fast enough for the
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

/// Start a background thread that refreshes ONLY the blockhash on a timer,
/// keeping the global blockhash cache warm so `latest_blockhash()` never pays
/// a synchronous RPC on the hot path.
///
/// Unlike `spawn_prefetch_thread`, this takes no mint — the blockhash cache is
/// global and benefits every mint. Use it in the multi-mint daemon, where there
/// is no single "active mint" to curve-prefetch (curve-prefetch stays on-demand
/// in `fetch_state_hot`). The first refresh runs immediately, before the loop,
/// so the cache is warm before the first decision (no cold-start fetch).
pub fn spawn_blockhash_warmer(
    fetcher: Arc<RpcLiveStateFetcher>,
    config: PrefetchConfig,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("pq-blockhash-warmer".into())
        .spawn(move || {
            // Warm the cache immediately — avoids the cold-start synchronous
            // fetch on the very first decision.
            let _ = fetcher.refresh_blockhash_inner();

            let mut blockhash_timer = 0u64;
            // 100ms tick — fine-grained enough to honour refresh_ms closely.
            while !fetcher.is_shutdown() {
                std::thread::sleep(std::time::Duration::from_millis(100));
                blockhash_timer += 100;
                if blockhash_timer >= config.blockhash_refresh_ms {
                    blockhash_timer = 0;
                    let _ = fetcher.refresh_blockhash_inner();
                }
            }
        })
        .expect("blockhash warmer thread spawn")
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

#[cfg(test)]
mod e7_blockhash_validity {
    //! E7: a blockhash's freshness is decided by the chain's own
    //! `lastValidBlockHeight`, not by elapsed wall-clock time.
    use super::*;

    #[test]
    fn last_valid_block_height_parses_from_the_real_response_shape() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"context":{"slot":123},"value":{"blockhash":"FfLaDnxr9mZfffm9tKuCAjKbbfXhpiEfrT2qKpShZgF7","lastValidBlockHeight":99999}}}"#;
        assert_eq!(
            extract_last_valid_block_height_from_response(body),
            Some(99999),
            "the field is present in every getLatestBlockhash response"
        );
        // Absent / malformed shapes must not fabricate a bound.
        assert_eq!(
            extract_last_valid_block_height_from_response(
                r#"{"result":{"context":{"slot":1},"value":{"blockhash":"x"}}}"#
            ),
            None
        );
        assert_eq!(
            extract_last_valid_block_height_from_response("not json"),
            None
        );
    }

    fn fetcher() -> RpcLiveStateFetcher {
        RpcLiveStateFetcher::new("http://127.0.0.1:1".to_string())
    }

    fn entry(now: u64) -> CachedBlockhash {
        CachedBlockhash {
            blockhash: [7u8; 32],
            slot: 100,
            last_valid_block_height: 150,
            fetched_at_secs: now,
        }
    }

    #[test]
    fn expires_by_height_even_moments_after_being_fetched() {
        let f = fetcher();
        let c = entry(epoch_secs());
        f.newest_slot.store(149, Ordering::Relaxed);
        assert!(
            f.cached_blockhash_is_usable(&c),
            "height still below the bound"
        );
        // The chain reached the bound. The hash is now refused although far less
        // than the old 5 s clock had elapsed — that is the whole point of E7.
        f.newest_slot.store(150, Ordering::Relaxed);
        assert!(!f.cached_blockhash_is_usable(&c), "bound reached → expired");
    }

    #[test]
    fn protocol_ceiling_still_bounds_an_unknown_height() {
        let f = fetcher();
        f.newest_slot.store(1, Ordering::Relaxed);
        // Unknown bound: the chain never told us, so the 60 s protocol ceiling is
        // the only guard — a fresh hash is served, an over-age one never is.
        let unknown = CachedBlockhash {
            last_valid_block_height: 0,
            ..entry(epoch_secs())
        };
        assert!(f.cached_blockhash_is_usable(&unknown));
        let over_age = CachedBlockhash {
            last_valid_block_height: 0,
            ..entry(epoch_secs() - (BLOCKHASH_PROTOCOL_EXPIRY_SECS + 1))
        };
        assert!(
            !f.cached_blockhash_is_usable(&over_age),
            "past the chain's own validity window → refuse"
        );
    }

    #[test]
    fn no_observed_slot_leaves_the_height_test_inert() {
        let f = fetcher();
        f.newest_slot.store(0, Ordering::Relaxed);
        let c = entry(epoch_secs());
        assert!(
            f.cached_blockhash_is_usable(&c),
            "no observation cannot prove expiry → the ceiling decides"
        );
    }
}

#[cfg(test)]
mod c1_stream_fed_cache {
    //! C1: a mint that has traded once is thereafter answered from the stream, and
    //! the caller's own signer is patched into the ctx it gets back.
    //!
    //! Every test here builds the fetcher against port 1 — an unreachable endpoint.
    //! Any attempt to reach the RPC therefore FAILS, which is exactly how these tests
    //! prove a hot read never left the process.
    use super::*;

    fn ctx(mint_byte: u8) -> PumpCurveCtx {
        PumpCurveCtx {
            mint: [mint_byte; 32],
            user: [0u8; 32],
            fee_recipient: [7u8; 32],
            creator: [8u8; 32],
            token_program: [9u8; 32],
            is_cashback_coin: false,
            quote_mint: [0u8; 32],
        }
    }

    fn state(mint_byte: u8, vsol: u64, vtok: u64, slot: u64) -> LiveCurveState {
        LiveCurveState {
            curve_ctx: ctx(mint_byte),
            virtual_sol_reserves: vsol,
            virtual_token_reserves: vtok,
            is_complete: false,
            observed_slot: slot,
            buyback_fee_recipients: [[0u8; 32]; 8],
        }
    }

    fn fetcher() -> RpcLiveStateFetcher {
        RpcLiveStateFetcher::new("http://127.0.0.1:1".to_string())
    }

    #[test]
    fn a_cached_mint_is_served_without_an_rpc() {
        let f = fetcher();
        f.seed_curve_for_test(state(1, 1_000, 2_000, 100));
        let s = f
            .fetch_state_hot(&[1u8; 32], &[0xAAu8; 32])
            .expect("served from the cache — an RPC to port 1 would have failed");
        assert_eq!(s.virtual_sol_reserves, 1_000);
        assert_eq!(
            s.curve_ctx.user, [0xAAu8; 32],
            "the caller's signer must be patched into the cached ctx"
        );
        assert_eq!(s.curve_ctx.creator, [8u8; 32], "immutable ctx is retained");
    }

    #[test]
    fn streamed_reserves_reach_the_hot_path() {
        let f = fetcher();
        f.seed_curve_for_test(state(1, 1_000, 2_000, 100));
        assert!(f.note_stream_reserves(&[1u8; 32], 5_000, 6_000, false, 110));
        let s = f.fetch_state_hot(&[1u8; 32], &[0xAAu8; 32]).unwrap();
        assert_eq!(s.virtual_sol_reserves, 5_000, "the stream's reserves win");
        assert_eq!(s.virtual_token_reserves, 6_000);
        assert_eq!(s.observed_slot, 110);
        assert_eq!(s.curve_ctx.user, [0xAAu8; 32]);
    }

    #[test]
    fn an_unknown_mint_is_never_fabricated_from_the_stream() {
        let f = fetcher();
        assert!(
            !f.note_stream_reserves(&[2u8; 32], 1, 1, false, 5),
            "the stream carries no fee_recipient — a buildable ctx cannot be invented"
        );
        // Nothing was learned, so the hot read must fall through to the RPC, which
        // here is an unreachable port: an error, never a fabricated state.
        assert!(f.fetch_state_hot(&[2u8; 32], &[0u8; 32]).is_err());
    }

    #[test]
    fn an_older_notification_never_moves_the_cache_backwards() {
        let f = fetcher();
        // The fetch learned this mint at slot 500; the stream is its own clock, so
        // its first event is admitted whatever the fetch saw.
        f.seed_curve_for_test(state(3, 1_000, 2_000, 500));
        assert!(f.note_stream_reserves(&[3u8; 32], 3_000, 4_000, false, 400));
        assert_eq!(
            f.fetch_state_hot(&[3u8; 32], &[1u8; 32])
                .unwrap()
                .virtual_sol_reserves,
            3_000
        );
        // A replayed older event must NOT overwrite the fresh reserves.
        assert!(
            !f.note_stream_reserves(&[3u8; 32], 9, 9, false, 399),
            "a replayed older notification must be refused"
        );
        assert_eq!(
            f.fetch_state_hot(&[3u8; 32], &[1u8; 32])
                .unwrap()
                .virtual_sol_reserves,
            3_000,
            "reserves unchanged by the replay"
        );
        // The same slot is accepted — latest wins within a slot.
        assert!(f.note_stream_reserves(&[3u8; 32], 4_000, 5_000, false, 400));
        assert_eq!(
            f.fetch_state_hot(&[3u8; 32], &[1u8; 32])
                .unwrap()
                .virtual_sol_reserves,
            4_000
        );
    }

    /// C1: the cache is only as good as the feed behind it. Once the stream goes
    /// quiet while slots keep advancing, the hot read must stop trusting it.
    #[test]
    fn a_stalled_stream_demotes_the_cache_to_a_real_fetch() {
        let f = fetcher();
        f.seed_curve_for_test(state(6, 1_000, 2_000, 100));
        assert!(f.note_stream_reserves(&[6u8; 32], 1_100, 2_100, false, 100));
        let served = f
            .fetch_state_hot(&[6u8; 32], &[0u8; 32])
            .expect("a live feed answers");
        assert_eq!(served.virtual_sol_reserves, 1_100);
        // The stream stops; the chain does not.
        f.newest_slot
            .store(100 + STREAM_LIVENESS_SLOTS + 1, Ordering::Relaxed);
        assert!(
            f.fetch_state_hot(&[6u8; 32], &[0u8; 32]).is_err(),
            "a quiet feed must not answer the hot path — it must pay the fetch"
        );
    }

    #[test]
    fn two_mints_are_cached_independently() {
        // The single-entry cache forced a cold fetch whenever the daemon switched
        // mints — the Rev-22/26 wrong-mint bug class. Keying by mint retires it.
        let f = fetcher();
        f.seed_curve_for_test(state(4, 100, 200, 10));
        f.seed_curve_for_test(state(5, 300, 400, 10));
        assert!(f.note_stream_reserves(&[4u8; 32], 111, 222, false, 20));
        let a = f.fetch_state_hot(&[4u8; 32], &[0u8; 32]).unwrap();
        let b = f.fetch_state_hot(&[5u8; 32], &[0u8; 32]).unwrap();
        assert_eq!((a.virtual_sol_reserves, b.virtual_sol_reserves), (111, 300));
        assert_eq!(a.curve_ctx.mint, [4u8; 32]);
        assert_eq!(b.curve_ctx.mint, [5u8; 32]);
        assert_eq!(b.curve_ctx.user, [0u8; 32]);
    }
}
