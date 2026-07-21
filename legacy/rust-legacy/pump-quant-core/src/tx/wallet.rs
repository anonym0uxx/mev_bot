//! Wallet management: loads trading keypairs from environment variables.
//!
//! Supports key rotation for multi-wallet operation.
//! Primary key: `WALLET_PRIVATE_KEY` (base58-encoded secret key).
//! Additional keys: `WALLET_PRIVATE_KEY_2`, `WALLET_PRIVATE_KEY_3`, etc.

use std::sync::atomic::{AtomicUsize, Ordering};

use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;

/// Manages one or more trading keypairs loaded from environment variables.
///
/// Thread-safe: uses atomic index for round-robin rotation.
pub struct WalletManager {
    keypairs: Vec<Keypair>,
    current: AtomicUsize,
}

impl WalletManager {
    /// Load keypairs from environment.
    ///
    /// Reads `WALLET_PRIVATE_KEY` (required), then `WALLET_PRIVATE_KEY_2`,
    /// `WALLET_PRIVATE_KEY_3`, etc. until a missing env var is encountered.
    ///
    /// # Panics
    /// Panics if `WALLET_PRIVATE_KEY` is not set or contains invalid base58.
    pub fn from_env() -> Self {
        let mut keypairs = Vec::new();

        // Primary key (required)
        let primary = std::env::var("WALLET_PRIVATE_KEY")
            .expect("WALLET_PRIVATE_KEY env var not set");
        let primary_bytes = bs58::decode(primary.trim())
            .into_vec()
            .expect("WALLET_PRIVATE_KEY is not valid base58");
        let primary_kp = Keypair::from_bytes(&primary_bytes)
            .expect("WALLET_PRIVATE_KEY is not a valid keypair (expected 64 bytes)");
        keypairs.push(primary_kp);

        // Additional keys: _2, _3, ...
        let mut idx = 2u32;
        loop {
            let var_name = format!("WALLET_PRIVATE_KEY_{idx}");
            match std::env::var(&var_name) {
                Ok(val) => {
                    let bytes = bs58::decode(val.trim())
                        .into_vec()
                        .unwrap_or_else(|_| panic!("{var_name} is not valid base58"));
                    let kp = Keypair::from_bytes(&bytes)
                        .unwrap_or_else(|_| panic!("{var_name} is not a valid keypair"));
                    keypairs.push(kp);
                    idx += 1;
                }
                Err(_) => break,
            }
        }

        tracing::info!(
            "WalletManager loaded {} keypair(s), primary pubkey: {}",
            keypairs.len(),
            keypairs[0].pubkey()
        );

        Self {
            keypairs,
            current: AtomicUsize::new(0),
        }
    }

    /// Get the current active keypair.
    pub fn current_keypair(&self) -> &Keypair {
        let idx = self.current.load(Ordering::Relaxed) % self.keypairs.len();
        &self.keypairs[idx]
    }

    /// Rotate to the next keypair (round-robin).
    pub fn rotate(&self) {
        let _ = self.current.fetch_add(1, Ordering::Relaxed);
    }

    /// Get the current wallet's public key as `[u8; 32]`.
    pub fn pubkey_bytes(&self) -> [u8; 32] {
        self.current_keypair().pubkey().to_bytes()
    }

    /// Number of loaded keypairs.
    pub fn count(&self) -> usize {
        self.keypairs.len()
    }
}
