//! RPC rate limiting and endpoint routing.
//!
//! This module provides priority-aware token bucket rate limiting for Solana
//! RPC endpoints. See [`rate_limiter::RateLimiter`] for the core implementation,
//! and [`client::RpcClient`] for the multi-endpoint routing client.

pub mod rate_limiter;
pub mod client;

pub use rate_limiter::{AcquireError, AcquireResult, Priority, RateLimiter, RateLimiterConfig, RateLimiterStats};
pub use client::{RpcClient, RpcClientConfig, RpcClientConfigError, RpcMethod};
