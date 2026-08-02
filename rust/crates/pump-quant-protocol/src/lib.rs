//! `pump_quant_protocol` — pump.fun / PumpSwap decoders + tx-instruction data builders.
//!
//! This crate ports the *pure, deterministic* portion of the legacy TypeScript
//! MEV stack (`pump-tx-builder.ts`, `bonding-curve-sim.ts`) into idiomatic,
//! std-only Rust. It contains four responsibilities, one per module:
//!
//! * [`decode`]  — strict, bounds-checked little-endian account decoders.
//! * [`curve`]   — integer bonding-curve / constant-product output math.
//! * [`ix`]      — instruction **data** serialization (discriminator + args).
//! * [`registry`]— versioned protocol-registry identifiers.
//!
//! Plus the end-to-end PumpSwap (Pump AMM) on-chain decode plane, one module
//! per responsibility:
//!
//! * [`pumpswap`]       — full account decoders (`Pool`, `GlobalConfig`, SPL
//!   token amounts for pool reserves, `BondingCurve` appended tail) + venue
//!   constants.
//! * [`pumpswap_ix`]    — instruction decoders (`buy`/`sell`/`create_pool`/
//!   `deposit`/`withdraw`, pump `migrate` detection) + account-index map.
//! * [`pumpswap_event`] — Anchor self-CPI event decoders (`BuyEvent`/
//!   `SellEvent`/`CreatePoolEvent`), the normalized [`pumpswap_event::PumpSwapTrade`]
//!   summary, fixed-point price helpers, and the CP cross-check
//!   [`pumpswap_event::verify_buy_event`].
//!
//! Plus the transaction **construction** plane (2026-08-02) — the modules
//! that produce the bytes the signer signs and the sender submits:
//!
//! * [`sha256`]         — self-contained FIPS 180-4 digest (PDA primitive).
//! * [`pda`]            — `find_program_address` from first principles, with
//!   the ed25519 on-curve check implemented locally.
//! * [`venue_accounts`] — the real §4.1 / §4.2 account-meta lists
//!   (`VENUE_TX_LAYOUTS.md`), fail-closed on every non-derivable input.
//! * [`message`]        — Solana legacy-message compiler + compute-budget /
//!   system-transfer / SPL helper instructions.
//! * [`tx_build`]       — end-to-end: decoded venue state → signable bytes.
//!
//! # Constitution
//! * §22 — NO `f32`/`f64` on any outcome-controlling path. Every calculation
//!   in this crate is integer / fixed-point (lamports as `u64`/`u128`, ratios
//!   in basis points). There is not a single float in the crate.
//! * Overflow is always explicit (`checked_*` / `saturating_*`), never silent.
//! * Deterministic: identical inputs always yield identical outputs. No
//!   wall-clock, RNG, network or floating point participates in any result.
//! * Live I/O (RPC / submit / streams) is out of scope; only the data layout
//!   and math are ported here.

#![forbid(unsafe_code)]

pub mod curve;
pub mod decode;
pub mod errors;
pub mod ix;
pub mod message;
pub mod pda;
pub mod pumpswap;
pub mod pumpswap_event;
pub mod pumpswap_ix;
pub mod registry;
pub mod sha256;
pub mod tx_build;
pub mod venue_accounts;
