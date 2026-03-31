//! Live Raydium vault price feed via accountSubscribe WebSocket.
//!
//! After opening a graduation arb position, we subscribe to the pool's
//! SOL vault account via `accountSubscribe`. Every time the vault balance
//! changes (any swap on the pool), we get updated reserves and can compute
//! the current token price for TP/SL decisions.
//!
//! ## Architecture
//!
//! ```text
//! GradArbEngine::on_migration()
//!     │ position opened
//!     └──► spawn_vault_monitor(pc_vault_pubkey, ...)
//!              │
//!              └── accountSubscribe(pc_vault, "confirmed")
//!                       │ on account update
//!                       ├── parse SPL amount at [64..72]
//!                       ├── compute price from reserves
//!                       └── send to grad_arb_engine for TP/SL check
//! ```
//!
//! ## Cost
//! Uses existing Helius WebSocket — $0 extra. One subscription per open position.
//! Positions last max 5 seconds, so subscription count is always tiny (1-3 max).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::sync::mpsc;

/// Price update from vault monitoring.
#[derive(Debug, Clone)]
pub struct VaultPriceUpdate {
    /// Token mint this update is for.
    pub mint: [u8; 32],
    /// Current SOL reserves in the pool vault (lamports).
    pub reserve_sol_lamports: u64,
    /// Current token reserves (if available, otherwise entry value).
    pub reserve_token_atoms: u64,
    /// Timestamp of update (epoch ms).
    pub ts_ms: u64,
}

/// Shared state for a single vault subscription.
pub struct VaultMonitor {
    /// Whether the monitor is still active (set to false when position closes).
    pub active: Arc<AtomicBool>,
    /// Latest SOL vault balance (lamports), atomically updated.
    pub latest_sol_lamports: Arc<AtomicU64>,
    /// Entry SOL reserves (for price change calculation).
    pub entry_sol_lamports: u64,
    /// Entry token reserves (assumed static for short hold periods).
    pub entry_token_atoms: u64,
    /// Token mint.
    pub mint: [u8; 32],
}

impl VaultMonitor {
    /// Create a new vault monitor for a position.
    pub fn new(
        mint: [u8; 32],
        entry_sol_lamports: u64,
        entry_token_atoms: u64,
    ) -> Self {
        Self {
            active: Arc::new(AtomicBool::new(true)),
            latest_sol_lamports: Arc::new(AtomicU64::new(entry_sol_lamports)),
            entry_sol_lamports,
            entry_token_atoms,
            mint,
        }
    }

    /// Signal this monitor to stop.
    #[inline(always)]
    pub fn stop(&self) {
        self.active.store(false, Ordering::Release);
    }

    /// Check if still active.
    #[inline(always)]
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    /// Get current price ratio (SOL per token) based on latest vault balance.
    ///
    /// Returns price as f64 for TP/SL comparison. Uses latest atomic SOL balance
    /// and entry token balance (tokens don't change without our own swap).
    #[inline(always)]
    pub fn current_price(&self) -> f64 {
        let sol = self.latest_sol_lamports.load(Ordering::Relaxed);
        if self.entry_token_atoms == 0 {
            return 0.0;
        }
        sol as f64 / self.entry_token_atoms as f64
    }

    /// Compute PnL ratio vs entry price (for TP/SL check).
    ///
    /// Returns fractional change: 0.03 = +3% (TP territory), -0.02 = -2% (SL territory).
    #[inline(always)]
    pub fn pnl_ratio(&self) -> f64 {
        if self.entry_token_atoms == 0 {
            return 0.0;
        }
        let current = self.current_price();
        let entry = self.entry_sol_lamports as f64 / self.entry_token_atoms as f64;
        if !entry.is_finite() || entry == 0.0 {
            return 0.0;
        }
        (current - entry) / entry
    }
}

/// Spawn a WebSocket accountSubscribe task to monitor a vault.
///
/// Sends price updates through the mpsc channel. Automatically unsubscribes
/// when `active` is set to false (position closed).
///
/// # Arguments
/// * `ws_url` — Helius WebSocket URL
/// * `vault_pubkey_b58` — Base58-encoded vault account pubkey
/// * `monitor` — shared VaultMonitor (active flag + atomic balance)
/// * `update_tx` — channel to send price updates for TP/SL processing
pub fn spawn_vault_subscriber(
    ws_url: String,
    vault_pubkey_b58: String,
    monitor: Arc<VaultMonitor>,
    update_tx: mpsc::UnboundedSender<VaultPriceUpdate>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::connect_async;

        let connect_result = connect_async(&ws_url).await;
        let (mut ws, _) = match connect_result {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!(
                    vault = %vault_pubkey_b58,
                    err = %e,
                    "[price_feed] WebSocket connect failed"
                );
                return;
            }
        };

        // Subscribe to account changes
        let subscribe_msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "accountSubscribe",
            "params": [
                vault_pubkey_b58,
                {
                    "encoding": "base64",
                    "commitment": "confirmed"
                }
            ]
        });

        if let Err(e) = ws.send(tokio_tungstenite::tungstenite::Message::Text(
            subscribe_msg.to_string().into(),
        )).await {
            tracing::warn!(err = %e, "[price_feed] subscribe send failed");
            return;
        }

        // Process updates until position closes
        while monitor.is_active() {
            let msg = tokio::select! {
                msg = ws.next() => msg,
                _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                    continue; // check active flag periodically
                }
            };

            let msg = match msg {
                Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => text,
                Some(Ok(_)) => continue,
                Some(Err(e)) => {
                    tracing::debug!(err = %e, "[price_feed] WebSocket error");
                    break;
                }
                None => break, // connection closed
            };

            // Parse account notification
            let json: serde_json::Value = match serde_json::from_str(&msg) {
                Ok(v) => v,
                Err(_) => continue,
            };

            // Extract base64 account data
            let data_b64 = match json
                .pointer("/params/result/value/data")
                .and_then(|d| d.as_array())
                .and_then(|arr| arr.first())
                .and_then(|v| v.as_str())
            {
                Some(d) => d,
                None => continue,
            };

            // Parse SPL token account amount at bytes [64..72]
            let amount = match parse_spl_amount(data_b64) {
                Some(a) => a,
                None => continue,
            };

            // Update atomic balance
            monitor.latest_sol_lamports.store(amount, Ordering::Release);

            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

            // Send price update
            let _ = update_tx.send(VaultPriceUpdate {
                mint: monitor.mint,
                reserve_sol_lamports: amount,
                reserve_token_atoms: monitor.entry_token_atoms,
                ts_ms: now_ms,
            });
        }

        // Unsubscribe
        let unsub = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "accountUnsubscribe",
            "params": [1]
        });
        let _ = ws.send(tokio_tungstenite::tungstenite::Message::Text(
            unsub.to_string().into(),
        )).await;
        let _ = ws.close(None).await;

        tracing::debug!(
            vault = %vault_pubkey_b58,
            "[price_feed] vault monitor stopped"
        );
    })
}

/// Parse SPL token account amount from base64-encoded account data.
/// Amount is a LE u64 at bytes [64..72].
#[inline(always)]
fn parse_spl_amount(data_b64: &str) -> Option<u64> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(data_b64).ok()?;
    if bytes.len() < 72 {
        return None;
    }
    Some(u64::from_le_bytes(bytes[64..72].try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vault_monitor_creation() {
        let mint = [42u8; 32];
        let monitor = VaultMonitor::new(mint, 85_000_000_000, 200_000_000_000_000);
        assert!(monitor.is_active());
        assert_eq!(monitor.entry_sol_lamports, 85_000_000_000);
        assert_eq!(monitor.entry_token_atoms, 200_000_000_000_000);
    }

    #[test]
    fn test_vault_monitor_stop() {
        let monitor = VaultMonitor::new([0u8; 32], 100, 100);
        assert!(monitor.is_active());
        monitor.stop();
        assert!(!monitor.is_active());
    }

    #[test]
    fn test_vault_monitor_price() {
        let monitor = VaultMonitor::new(
            [0u8; 32],
            85_000_000_000,      // 85 SOL
            200_000_000_000_000, // 200T token atoms
        );
        let price = monitor.current_price();
        // 85e9 / 200e12 = 4.25e-4
        assert!((price - 4.25e-4).abs() < 1e-6);
    }

    #[test]
    fn test_vault_monitor_pnl_ratio_no_change() {
        let monitor = VaultMonitor::new([0u8; 32], 85_000_000_000, 200_000_000_000_000);
        let pnl = monitor.pnl_ratio();
        assert!((pnl - 0.0).abs() < 1e-9);
    }

    #[test]
    fn test_vault_monitor_pnl_ratio_profit() {
        let monitor = VaultMonitor::new([0u8; 32], 85_000_000_000, 200_000_000_000_000);
        // Simulate 3% price increase: SOL reserves go from 85 to 87.55
        monitor.latest_sol_lamports.store(87_550_000_000, Ordering::Relaxed);
        let pnl = monitor.pnl_ratio();
        assert!((pnl - 0.03).abs() < 0.001, "expected ~3% profit, got {}", pnl);
    }

    #[test]
    fn test_vault_monitor_pnl_ratio_loss() {
        let monitor = VaultMonitor::new([0u8; 32], 85_000_000_000, 200_000_000_000_000);
        // Simulate 2% price drop: SOL reserves go from 85 to 83.3
        monitor.latest_sol_lamports.store(83_300_000_000, Ordering::Relaxed);
        let pnl = monitor.pnl_ratio();
        assert!((pnl - (-0.02)).abs() < 0.001, "expected ~-2% loss, got {}", pnl);
    }

    #[test]
    fn test_parse_spl_amount() {
        use base64::Engine;
        let mut account_data = vec![0u8; 165];
        let amount: u64 = 85_000_000_000;
        account_data[64..72].copy_from_slice(&amount.to_le_bytes());
        let encoded = base64::engine::general_purpose::STANDARD.encode(&account_data);
        assert_eq!(parse_spl_amount(&encoded), Some(amount));
    }

    #[test]
    fn test_parse_spl_amount_too_short() {
        use base64::Engine;
        let short = vec![0u8; 64];
        let encoded = base64::engine::general_purpose::STANDARD.encode(&short);
        assert_eq!(parse_spl_amount(&encoded), None);
    }

    #[test]
    fn test_vault_monitor_zero_tokens() {
        let monitor = VaultMonitor::new([0u8; 32], 85_000_000_000, 0);
        assert_eq!(monitor.current_price(), 0.0);
        assert_eq!(monitor.pnl_ratio(), 0.0);
    }
}
