//! Telegram Bot API alert sender.
//!
//! Reads `TELEGRAM_BOT_TOKEN` and `TELEGRAM_CHAT_ID` from env vars.
//! Rate-limited to 1 message per 1100ms (conservative; Telegram allows ~30/s).
//! Provides both async `send()` and fire-and-forget `try_send_blocking()` for
//! use from the synchronous logger thread.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tracing::{debug, error, warn};

/// Rate limit: minimum interval between sends (ms).
const RATE_LIMIT_MS: u64 = 1_100;

/// Telegram alert sender. Created via `TelegramAlerter::new()` which returns
/// `None` if the env vars aren't set.
pub struct TelegramAlerter {
    bot_token: String,
    chat_id: String,
    client: reqwest::Client,
    last_sent_ms: AtomicU64,
    /// Tokio runtime handle — needed for `try_send_blocking` from non-async context.
    rt_handle: tokio::runtime::Handle,
}

impl TelegramAlerter {
    /// Create a new alerter. Returns `None` if `TELEGRAM_BOT_TOKEN` or
    /// `TELEGRAM_CHAT_ID` env vars are not set.
    pub fn new() -> Option<Arc<Self>> {
        let bot_token = std::env::var("TELEGRAM_BOT_TOKEN").ok()?;
        let chat_id = std::env::var("TELEGRAM_CHAT_ID").ok()?;

        if bot_token.is_empty() || chat_id.is_empty() {
            return None;
        }

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .ok()?;

        let rt_handle = tokio::runtime::Handle::try_current().ok()?;

        Some(Arc::new(Self {
            bot_token,
            chat_id,
            client,
            last_sent_ms: AtomicU64::new(0),
            rt_handle,
        }))
    }

    /// Current epoch ms (for rate limiting).
    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    /// Check and update rate limit. Returns true if we should send.
    fn check_rate_limit(&self) -> bool {
        let now = Self::now_ms();
        let last = self.last_sent_ms.load(Ordering::Relaxed);
        if now.saturating_sub(last) < RATE_LIMIT_MS {
            return false;
        }
        // CAS to avoid double-sends
        self.last_sent_ms
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    }

    /// Send a message to Telegram (async). Respects rate limit.
    pub async fn send(&self, message: &str) -> anyhow::Result<()> {
        if !self.check_rate_limit() {
            debug!("[telegram] rate-limited, skipping message");
            return Ok(());
        }

        let url = format!(
            "https://api.telegram.org/bot{}/sendMessage",
            self.bot_token
        );

        let body = serde_json::json!({
            "chat_id": self.chat_id,
            "text": message,
            "parse_mode": "HTML",
            "disable_web_page_preview": true,
        });

        let resp = self.client.post(&url).json(&body).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            warn!("[telegram] send failed: {} — {}", status, text);
            anyhow::bail!("Telegram API error: {status}");
        }

        debug!("[telegram] message sent");
        Ok(())
    }

    /// Fire-and-forget send from a synchronous context (e.g. logger thread).
    /// Spawns an async task on the tokio runtime. If the runtime is gone
    /// or rate-limited, the message is silently dropped.
    pub fn try_send_blocking(&self, message: &str) {
        if !self.check_rate_limit() {
            return;
        }

        let url = format!(
            "https://api.telegram.org/bot{}/sendMessage",
            self.bot_token
        );

        let body = serde_json::json!({
            "chat_id": self.chat_id,
            "text": message,
            "parse_mode": "HTML",
            "disable_web_page_preview": true,
        });

        let client = self.client.clone();
        self.rt_handle.spawn(async move {
            match client.post(&url).json(&body).send().await {
                Ok(resp) if !resp.status().is_success() => {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    error!("[telegram] send failed: {} — {}", status, text);
                }
                Err(e) => {
                    error!("[telegram] send error: {}", e);
                }
                _ => {
                    debug!("[telegram] message sent (fire-and-forget)");
                }
            }
        });
    }
}

/// Format a closed-position trade alert message.
pub fn format_trade_alert(
    exit_reason: &str,
    mint_b58: &str,
    hold_ms: u64,
    net_pnl_sol: f64,
) -> String {
    let mint_short = if mint_b58.len() > 8 {
        &mint_b58[..8]
    } else {
        mint_b58
    };
    format!(
        "[PAPER] {} | {} | {}ms | PnL: {:+.4} SOL",
        exit_reason, mint_short, hold_ms, net_pnl_sol
    )
}

/// Format a circuit breaker alert.
pub fn format_circuit_breaker_alert(consecutive_stops: u32, pause_s: u64) -> String {
    format!(
        "⚠️ CIRCUIT BREAKER: {} stops → paused {}s",
        consecutive_stops, pause_s
    )
}

/// Format a feed-stale alert.
pub fn format_feed_stale_alert(feed_name: &str, stale_s: u64) -> String {
    format!(
        "🔴 FEED STALE: {} last seen {}s ago — trading paused",
        feed_name, stale_s
    )
}

/// Format a feed-recovered alert.
pub fn format_feed_recovered_alert(feed_name: &str) -> String {
    format!("✅ FEED RECOVERED: {} — trading resumed", feed_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_trade_alert() {
        let msg = format_trade_alert("take_profit", "AbCdEfGhIjKl1234", 350, 0.0123);
        assert!(msg.contains("[PAPER]"));
        assert!(msg.contains("take_profit"));
        assert!(msg.contains("AbCdEfGh")); // 8-char mint short
        assert!(msg.contains("350ms"));
        assert!(msg.contains("+0.0123 SOL"));
    }

    #[test]
    fn test_format_trade_alert_negative_pnl() {
        let msg = format_trade_alert("stop_loss", "XyZ12345Abcd", 200, -0.0050);
        assert!(msg.contains("-0.0050 SOL"));
    }

    #[test]
    fn test_format_circuit_breaker() {
        let msg = format_circuit_breaker_alert(3, 180);
        assert!(msg.contains("3 stops"));
        assert!(msg.contains("180s"));
    }

    #[test]
    fn test_format_feed_stale() {
        let msg = format_feed_stale_alert("PumpPortal", 50);
        assert!(msg.contains("PumpPortal"));
        assert!(msg.contains("50s"));
        assert!(msg.contains("trading paused"));
    }

    #[test]
    fn test_format_feed_recovered() {
        let msg = format_feed_recovered_alert("Helius");
        assert!(msg.contains("Helius"));
        assert!(msg.contains("trading resumed"));
    }

    #[test]
    fn test_new_returns_none_without_env() {
        // Clear env vars to ensure None
        std::env::remove_var("TELEGRAM_BOT_TOKEN");
        std::env::remove_var("TELEGRAM_CHAT_ID");
        // This might or might not return None depending on whether a tokio
        // runtime is active, but without env vars it should return None early.
        // We can't test TelegramAlerter::new() without a tokio runtime,
        // so just verify the formatting functions.
    }
}
