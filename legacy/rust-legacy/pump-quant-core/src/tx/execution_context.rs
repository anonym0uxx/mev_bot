//! Shared transaction infrastructure for all trading engines.
//! Created once in main.rs, Arc'd to every engine.

use std::sync::Arc;
use parking_lot::Mutex;

use crate::momentum::rpc_sender::RpcSender;
use crate::tx::executor::BlockhashCache;
use crate::tx::jito_grpc::JitoGrpcClient;
use crate::tx::nozomi::NozomiClient;
use crate::tx::tip_engine::{TipEngine, TipRequest};

/// Pre-loaded wallet keypair bytes. Clone-safe (64-byte copy, no I/O).
/// Replaces the per-trade `fs::read` pattern that loaded the keypair from disk
/// on every buy and sell task.
pub struct WalletKeys {
    keypair_bytes: [u8; 64],
    pubkey: [u8; 32],
}

impl WalletKeys {
    /// Load wallet keypair from a JSON file at `path`.
    /// Returns `None` if the path is empty, file is missing, or invalid.
    pub fn load_from_path(path: &str) -> Option<Self> {
        if path.is_empty() {
            return None;
        }
        let bytes = std::fs::read(path).ok()?;
        let arr: Vec<u8> = serde_json::from_slice(&bytes).ok()?;
        if arr.len() != 64 {
            tracing::error!(len = arr.len(), "[WalletKeys] invalid keypair length");
            return None;
        }
        let mut keypair_bytes = [0u8; 64];
        keypair_bytes.copy_from_slice(&arr);
        let mut pubkey = [0u8; 32];
        pubkey.copy_from_slice(&arr[32..64]);
        Some(Self {
            keypair_bytes,
            pubkey,
        })
    }

    /// Raw 32-byte public key.
    pub fn pubkey(&self) -> [u8; 32] {
        self.pubkey
    }

    /// Raw 64-byte keypair.
    pub fn keypair_bytes(&self) -> [u8; 64] {
        self.keypair_bytes
    }

    /// Construct a `solana_sdk::signature::Keypair` from the pre-loaded bytes.
    pub fn to_keypair(&self) -> solana_sdk::signature::Keypair {
        solana_sdk::signature::Keypair::from_bytes(&self.keypair_bytes)
            .expect("WalletKeys: validated 64-byte keypair")
    }
}

impl Clone for WalletKeys {
    fn clone(&self) -> Self {
        Self {
            keypair_bytes: self.keypair_bytes,
            pubkey: self.pubkey,
        }
    }
}

/// Shared infrastructure for transaction submission.
/// Built once in `main.rs`, wrapped in `Arc`, passed to every engine.
pub struct ExecutionContext {
    // ── TX submission ───────────────────────────────────────────────
    pub jito_grpc: Option<Arc<JitoGrpcClient>>,
    pub nozomi_client: Option<Arc<NozomiClient>>,

    // ── Blockhash ───────────────────────────────────────────────────
    pub blockhash_cache: Arc<BlockhashCache>,

    // ── Wallet (pre-loaded once — replaces per-trade fs::read) ──────
    pub wallet: Option<WalletKeys>,

    // ── Tip engine ──────────────────────────────────────────────────
    pub tip_engine: Arc<Mutex<TipEngine>>,

    // ── RPC primary sender (Helius) with rate limiter + circuit breaker ──
    pub rpc_sender: Arc<RpcSender>,

    // ── Legacy RPC fallback (kept for Nozomi→Jito→RPC triple fallback) ──
    pub rpc_fallback_client: reqwest::Client,
    pub rpc_fallback_url: Arc<String>,

    // ── URLs ────────────────────────────────────────────────────────
    pub helius_rpc_url: Arc<String>,
    pub public_rpc_url: Arc<String>,
}

impl ExecutionContext {
    /// Get the current blockhash synchronously (no async).
    pub fn blockhash_sync(&self) -> Option<[u8; 32]> {
        self.blockhash_cache.get_sync()
    }

    /// Compute a dynamic tip via the tip engine.
    pub fn compute_tip(&self, req: &TipRequest) -> u64 {
        self.tip_engine.lock().compute_tip(req)
    }

    /// Get the wallet public key (None in paper mode / no wallet configured).
    pub fn wallet_pubkey(&self) -> Option<[u8; 32]> {
        self.wallet.as_ref().map(|w| w.pubkey())
    }

    /// Build a `Keypair` from the pre-loaded wallet.
    /// Panics if no wallet is configured (paper mode). Always gate on `!paper_mode`.
    pub fn keypair(&self) -> solana_sdk::signature::Keypair {
        self.wallet
            .as_ref()
            .expect("keypair() called without wallet configured")
            .to_keypair()
    }
}
