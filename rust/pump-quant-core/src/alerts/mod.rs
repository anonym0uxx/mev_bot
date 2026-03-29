//! Alert delivery — Telegram notifications for trade events,
//! circuit breaker activations, and feed health changes.

pub mod telegram;

pub use telegram::TelegramAlerter;
