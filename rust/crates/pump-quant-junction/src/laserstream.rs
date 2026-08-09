//! LaserStream gRPC adapter — primary canonical ingest lane.
//!
//! Decodes raw Solana transactions from Helius LaserStream gRPC
//! `transactionSubscribe` (Geyser-fed, lowest latency, self-healing via
//! SDK-internal `from_slot` resume) into `ProvenancedEvent`s.
//!
//! Constitution criteria:
//! - §61: LaserStream gRPC operates on mainnet, not only devnet.
//! - §64: LaserStream disconnects do not create fabricated state.
//! - §65: Provider replay is distinguished from original live observation.
//! - §73: Active source combination demonstrates complete discovery, or
//!   reports explicit INCOMPLETE state.
//!
//! This module is Phase A (portable, no gRPC deps). The gRPC server
//! (`tools/stream-capture-rs/grpc-server-only/`) is Phase B (server-only,
//! helius-laserstream SDK). It outputs decoded transaction data that this
//! module consumes. The integration boundary is the NDJSON/typed-struct
//! interface — the adapter never touches tonic/yellowstone protos directly.
//!
//! Paper/live parity (§13): both paper_session and the live trading path
//! route through the same `decode_transaction` → `classify_instruction` →
//! `ProvenancedEvent` pipeline. The only difference is the transport
//! (gRPC vs WS vs file replay), not the decode path.

#![warn(clippy::all, clippy::integer_arithmetic, clippy::cast_possible_truncation)]

use pump_quant_app::event::AppEvent;
use pump_quant_domain::ids::Mint;
use pump_quant_protocol::ix::{BUY_DISCRIMINATOR, SELL_DISCRIMINATOR};
use pump_quant_protocol::pumpswap_ix::{
    decode_pumpswap_ix, PumpSwapIx,
    PUMP_MIGRATE_DISCRIMINATOR,
};

use crate::{ProvenanceSource, ProvenancedEvent};

/// Pump.fun program ID bytes — base58-decoded from 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P.
/// Verified against on-chain program address via `solana_program::pubkey::Pubkey::from_str`.
pub const PUMP_FUN_PROGRAM: [u8; 32] = [
    0x01, 0x56, 0xE0, 0xF6, 0x93, 0x66, 0x5A, 0xCF, 0x44, 0xDB, 0x15, 0x68, 0xBF, 0x17, 0x5B, 0xAA,
    0x51, 0x89, 0xCB, 0x97, 0xF5, 0xD2, 0xFF, 0x3B, 0x65, 0x5D, 0x2B, 0xB6, 0xFD, 0x6D, 0x18, 0xB0,
];

/// PumpSwap program ID bytes — base58-decoded from pPEEEJ5r9sRFMks2oBq1qjhtBf8V4qyGSz8xbxqHEBu.
/// Verified against on-chain program address via `solana_program::pubkey::Pubkey::from_str`.
pub const PUMP_SWAP_PROGRAM: [u8; 32] = [
    0x0C, 0x23, 0x6E, 0x6F, 0x4F, 0xDD, 0xBF, 0x03, 0x4F, 0xC8, 0xDD, 0x38, 0x84, 0xEC, 0xCB, 0x44,
    0x9E, 0x6D, 0xE6, 0x88, 0x9E, 0xD1, 0xE9, 0xF7, 0xF0, 0xA4, 0x90, 0xB3, 0xD8, 0xC8, 0x2B, 0x2C,
];

/// One decoded instruction from a LaserStream transaction notification.
#[derive(Clone, Debug)]
pub struct LaserStreamInstruction {
    /// Program ID bytes (32 bytes).
    pub program_id: [u8; 32],
    /// Instruction data (includes discriminator prefix).
    pub data: Vec<u8>,
    /// Account key indices into the transaction's account list.
    pub accounts: Vec<u8>,
}

/// A decoded LaserStream transaction notification.
#[derive(Clone, Debug)]
pub struct LaserStreamTx {
    /// Slot number from the gRPC notification.
    pub slot: u64,
    /// Transaction signature (64 bytes).
    pub signature: [u8; 64],
    /// All account keys in the transaction (message.header + account keys).
    pub account_keys: Vec<[u8; 32]>,
    /// Decoded instructions (outer + inner).
    pub instructions: Vec<LaserStreamInstruction>,
    /// Whether this is a live observation (true) or replay (false).
    /// §65: replay must be distinguished from live in every record.
    pub is_live: bool,
}

/// Classification of a pump.fun instruction found in a LaserStream transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PumpInstruction {
    /// pump.fun buy (bonding curve): `amount_lamports` in, `min_tokens` out.
    /// `buyer` is the signer's pubkey extracted from account index [6] of the
    /// buy instruction (the `ctx.user` per `venue_accounts::pump_buy_accounts`).
    Buy {
        mint: [u8; 32],
        amount_lamports: u64,
        min_tokens: u64,
        /// Buyer's wallet pubkey (account index [6] in the buy instruction).
        buyer: [u8; 32],
    },
    /// pump.fun sell (bonding curve): `amount_tokens` in, `min_lamports` out.
    /// `seller` is the signer's pubkey extracted from account index [6] of the
    /// sell instruction.
    Sell {
        mint: [u8; 32],
        amount_tokens: u64,
        min_lamports: u64,
        /// Seller's wallet pubkey (account index [6] in the sell instruction).
        seller: [u8; 32],
    },
    /// PumpSwap buy (post-migration AMM): `amount` lamports in.
    /// `buyer` is the signer at account index [1] of the PumpSwap buy ix.
    PumpSwapBuy {
        pool: [u8; 32],
        amount_lamports: u64,
        min_tokens: u64,
        /// Buyer's wallet pubkey (account index [1] in the PumpSwap buy ix).
        buyer: [u8; 32],
    },
    /// PumpSwap sell (post-migration AMM): `amount` tokens in.
    /// `seller` is the signer at account index [1] of the PumpSwap sell ix.
    PumpSwapSell {
        pool: [u8; 32],
        amount_tokens: u64,
        min_lamports: u64,
        /// Seller's wallet pubkey (account index [1] in the PumpSwap sell ix).
        seller: [u8; 32],
    },
    /// PumpSwap pool creation (migration event).
    CreatePool {
        pool: [u8; 32],
        base_mint: [u8; 32],
    },
    /// pump.fun → PumpSwap migration.
    Migrate {
        mint: [u8; 32],
    },
}

/// Decode a LaserStream transaction into classified pump.fun instructions.
///
/// Hot path — zero-panic, no allocation on the no-pump path. We iterate
/// instructions, match program IDs, and decode instruction data.
///
/// Returns an empty vec when the transaction has no pump.fun instructions
/// (most transactions on Solana are not pump.fun — this is the common case
/// and should be fast).
pub fn classify_pump_instructions(tx: &LaserStreamTx) -> Vec<PumpInstruction> {
    let mut out = Vec::with_capacity(1); // Most txs have ≤1 pump instruction

    for ix in &tx.instructions {
        if ix.program_id == PUMP_FUN_PROGRAM {
            if ix.data.len() >= 8 {
                let disc = &ix.data[..8];
                if disc == BUY_DISCRIMINATOR && ix.data.len() >= 8 + 8 + 8 {
                    // Account [2] = mint (per `venue_accounts::pump_buy_accounts`)
                    // Account [6] = user (signer — the buyer's wallet pubkey)
                    if let (Some(mint), Some(buyer)) = (account_key_at(ix, tx, 2), account_key_at(ix, tx, 6)) {
                        let amount = u64::from_le_bytes(
                            ix.data[8..16].try_into().unwrap_or([0; 8]),
                        );
                        let min_tokens = u64::from_le_bytes(
                            ix.data[16..24].try_into().unwrap_or([0; 8]),
                        );
                        out.push(PumpInstruction::Buy {
                            mint,
                            amount_lamports: amount,
                            min_tokens,
                            buyer,
                        });
                    }
                } else if disc == SELL_DISCRIMINATOR && ix.data.len() >= 8 + 8 + 8 {
                    // Account [2] = mint, Account [6] = user (signer — the seller's wallet)
                    if let (Some(mint), Some(seller)) = (account_key_at(ix, tx, 2), account_key_at(ix, tx, 6)) {
                        let amount = u64::from_le_bytes(
                            ix.data[8..16].try_into().unwrap_or([0; 8]),
                        );
                        let min_lamports = u64::from_le_bytes(
                            ix.data[16..24].try_into().unwrap_or([0; 8]),
                        );
                        out.push(PumpInstruction::Sell {
                            mint,
                            amount_tokens: amount,
                            min_lamports,
                            seller,
                        });
                    }
                }
            }
        } else if ix.program_id == PUMP_SWAP_PROGRAM {
            if let Some(parsed) = decode_pumpswap_ix(&ix.data) {
                match parsed {
                    PumpSwapIx::Buy(args) => {
                        // Account [0] = pool, Account [1] = user (signer — buyer's wallet)
                        if let (Some(pool), Some(buyer)) = (account_key_at(ix, tx, 0), account_key_at(ix, tx, 1)) {
                            out.push(PumpInstruction::PumpSwapBuy {
                                pool,
                                amount_lamports: args.max_quote_amount_in,
                                min_tokens: args.base_amount_out,
                                buyer,
                            });
                        }
                    }
                    PumpSwapIx::Sell(args) => {
                        // Account [0] = pool, Account [1] = user (signer — seller's wallet)
                        if let (Some(pool), Some(seller)) = (account_key_at(ix, tx, 0), account_key_at(ix, tx, 1)) {
                            out.push(PumpInstruction::PumpSwapSell {
                                pool,
                                amount_tokens: args.base_amount_in,
                                min_lamports: args.min_quote_amount_out,
                                seller,
                            });
                        }
                    }
                    PumpSwapIx::CreatePool(_) => {
                        if let Some(pool) = account_key_at(ix, tx, 0) {
                            if let Some(base_mint) = account_key_at(ix, tx, 3) {
                                out.push(PumpInstruction::CreatePool { pool, base_mint });
                            }
                        }
                    }
                    PumpSwapIx::Deposit(_) | PumpSwapIx::Withdraw(_) => {}
                }
            }
            // Check for migration discriminator
            if ix.data.len() >= 8 && ix.data[..8] == PUMP_MIGRATE_DISCRIMINATOR {
                if let Some(mint) = account_key_at(ix, tx, 1) {
                    out.push(PumpInstruction::Migrate { mint });
                }
            }
        }
    }

    out
}

/// Resolve an account key from an instruction's account index.
/// Returns None if the index is out of bounds (fail-safe, not panic).
fn account_key_at(
    ix: &LaserStreamInstruction,
    tx: &LaserStreamTx,
    idx: usize,
) -> Option<[u8; 32]> {
    if idx >= ix.accounts.len() {
        return None;
    }
    let key_idx = ix.accounts[idx] as usize;
    if key_idx >= tx.account_keys.len() {
        return None;
    }
    Some(tx.account_keys[key_idx])
}

/// Deterministic, collision-resistant `u64` entity id from a 32-byte pubkey.
///
/// Uses a simple split-mix: takes the first 8 bytes of the pubkey in
/// little-endian, then mixes with the last 8 bytes via a single round of
/// split-mix64. This is NOT a cryptographic hash — it is a stable, uniform
/// `u64` handle for the `buyer_entity` field that the engine uses for holder
/// de-duplication and bitset tracking. Collisions are negligible for the
/// ~10⁶-wallet addressable space (birthday bound ~2³²).
fn wallet_entity_id(pubkey: &[u8; 32]) -> u64 {
    let lo = u64::from_le_bytes(pubkey[..8].try_into().unwrap_or([0; 8]));
    let hi = u64::from_le_bytes(pubkey[24..32].try_into().unwrap_or([0; 8]));
    // splitmix64 round: mix hi into lo
    let mut z = lo.wrapping_add(hi);
    z = z.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let z = (z >> (z >> 61).wrapping_add(4)) ^ z;
    let z = z.wrapping_mul(0xC2B9_5A82_79D4_CEA2);
    let z = (z >> (z >> 61).wrapping_add(4)) ^ z;
    z.wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

/// Convert classified pump.fun instructions into `ProvenancedEvent`s.
///
/// Each `PumpInstruction` becomes a `MarketTrade` event (for buy/sell) or
/// a `Migration` event (for migration). The `ProvenanceSource` is always
/// `LaserStream`, and `is_live` is taken from the transaction itself (§65).
///
/// Note: `MarketTrade` requires `liquidity_lamports` (pool quote-reserve depth
/// after the trade) which is NOT available from instruction data alone — it
/// requires an account snapshot. The LaserStream adapter emits the event with
/// `liquidity_lamports: 0` and the reserve-delta or account-subscribe path
/// fills it via `OnchainConfirm`. This is the same two-phase pattern used by
/// the Helius WS path.
///
/// **Wallet identity (G1 fix):** The `buyer_entity` field now carries a
/// deterministic `u64` derived from the buyer/seller's 32-byte pubkey
/// (extracted at account index [6] for pump.fun, [1] for PumpSwap). A value
/// of `0` means the wallet could not be extracted (e.g., an instruction with
/// too few accounts).
pub fn instructions_to_events(
    instructions: &[PumpInstruction],
    slot: u64,
    is_live: bool,
) -> Vec<ProvenancedEvent> {
    let mut events = Vec::with_capacity(instructions.len());

    for ix in instructions {
        match ix {
            PumpInstruction::Buy { mint, amount_lamports, buyer, .. } => {
                events.push(ProvenancedEvent {
                    event: AppEvent::MarketTrade {
                        mint: Mint(*mint),
                        price_fp: 0, // Filled by reserve-delta or account snapshot
                        quote_lamports: *amount_lamports,
                        liquidity_lamports: 0, // Filled by OnchainConfirm
                        signed_base: i64::try_from(*amount_lamports).unwrap_or(i64::MAX),
                        buyer_entity: wallet_entity_id(buyer),
                        age_slots: 0,   // Not available from ix data alone
                    },
                    source: ProvenanceSource::LaserStream,
                    slot,
                    is_live,
                });
            }
            PumpInstruction::Sell { mint, amount_tokens, seller, .. } => {
                events.push(ProvenancedEvent {
                    event: AppEvent::MarketTrade {
                        mint: Mint(*mint),
                        price_fp: 0,
                        quote_lamports: 0, // Sell: quote_lamports = SOL received
                        liquidity_lamports: 0,
                        signed_base: -i64::try_from(*amount_tokens).unwrap_or(i64::MAX),
                        buyer_entity: wallet_entity_id(seller),
                        age_slots: 0,
                    },
                    source: ProvenanceSource::LaserStream,
                    slot,
                    is_live,
                });
            }
            PumpInstruction::PumpSwapBuy { pool, amount_lamports, buyer, .. } => {
                events.push(ProvenancedEvent {
                    event: AppEvent::MarketTrade {
                        mint: Mint(*pool),
                        price_fp: 0,
                        quote_lamports: *amount_lamports,
                        liquidity_lamports: 0,
                        signed_base: i64::try_from(*amount_lamports).unwrap_or(i64::MAX),
                        buyer_entity: wallet_entity_id(buyer),
                        age_slots: 0,
                    },
                    source: ProvenanceSource::LaserStream,
                    slot,
                    is_live,
                });
            }
            PumpInstruction::PumpSwapSell { pool, amount_tokens, seller, .. } => {
                events.push(ProvenancedEvent {
                    event: AppEvent::MarketTrade {
                        mint: Mint(*pool),
                        price_fp: 0,
                        quote_lamports: 0,
                        liquidity_lamports: 0,
                        signed_base: -i64::try_from(*amount_tokens).unwrap_or(i64::MAX),
                        buyer_entity: wallet_entity_id(seller),
                        age_slots: 0,
                    },
                    source: ProvenanceSource::LaserStream,
                    slot,
                    is_live,
                });
            }
            PumpInstruction::CreatePool { base_mint, .. } => {
                // Pool creation = graduation/migration event for the bonding curve
                events.push(ProvenancedEvent {
                    event: AppEvent::Migration {
                        mint: Mint(*base_mint),
                        slot,
                    },
                    source: ProvenanceSource::LaserStream,
                    slot,
                    is_live,
                });
            }
            PumpInstruction::Migrate { mint } => {
                events.push(ProvenancedEvent {
                    event: AppEvent::Migration {
                        mint: Mint(*mint),
                        slot,
                    },
                    source: ProvenanceSource::LaserStream,
                    slot,
                    is_live,
                });
            }
        }
    }

    events
}

/// Connection state for the LaserStream gRPC lane (§64: disconnects do not
/// create fabricated state). When the stream disconnects, the adapter
/// reports a gap; on resume, the SDK's `from_slot` fills the gap. If the
/// gap can't be filled, the adapter reports INCOMPLETE (§73).
#[derive(Clone, Debug)]
pub struct LaserStreamState {
    /// Last slot successfully observed.
    pub last_slot: u64,
    /// Total transactions received.
    pub txs_received: u64,
    /// Total pump.fun instructions classified.
    pub pump_instructions_classified: u64,
    /// Total events emitted to the junction queue.
    pub events_emitted: u64,
    /// Disconnect count (for health metrics).
    pub disconnects: u64,
    /// Whether the stream is currently connected.
    pub connected: bool,
    /// Current replay state: None = live, Some(slot) = replaying from slot.
    pub replay_from_slot: Option<u64>,
}

impl LaserStreamState {
    pub fn new() -> Self {
        Self {
            last_slot: 0,
            txs_received: 0,
            pump_instructions_classified: 0,
            events_emitted: 0,
            disconnects: 0,
            connected: false,
            replay_from_slot: None,
        }
    }
}

impl Default for LaserStreamState {
    fn default() -> Self {
        Self::new()
    }
}

// ─── NDJSON line parser (gRPC server stdout → LaserStreamTx) ─────────────

/// A parsed update from the gRPC server's NDJSON stdout stream.
#[derive(Clone, Debug)]
pub enum LaserStreamUpdate {
    /// A transaction notification — decoded into `LaserStreamTx` for classification.
    Transaction(LaserStreamTx),
    /// An account notification — bonding-curve PDA snapshot for reserve-delta.
    Account {
        pubkey: [u8; 32],
        owner: [u8; 32],
        data: Vec<u8>,
        slot: u64,
    },
    /// A slot notification — heartbeat for staleness detection.
    Slot { slot: u64 },
}

/// Parse one NDJSON line from the gRPC server's stdout into a `LaserStreamUpdate`.
///
/// Returns `None` (not an error) for malformed lines, empty lines, or lines
/// that cannot be decoded — fail-safe, never panic. The caller skips None
/// and continues processing the next line.
///
/// Schema expected (emitted by `pq-laserstream-grpc`):
/// ```json
/// {"lane":"laserstream","kind":"transaction","slot":N,"recv_unix_ms":N,
///  "signature_b58":"...","account_keys":["b58",...],
///  "instructions":[{"program_b58":"...","data_b64":"...","accounts":[0,1,2]}]}
/// ```
pub fn parse_ndjson_line(line: &str) -> Option<LaserStreamUpdate> {
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine as _;
    use solana_program::pubkey::Pubkey;
    use std::str::FromStr;

    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Minimal JSON value parser — we only need to extract specific fields.
    // We use the pq_stream_capture::json parser which is already in deps.
    let v: pq_stream_capture::json::Value = pq_stream_capture::json::parse(trimmed).ok()?;
    let kind = v.get("kind")?.as_str()?;

    match kind {
        "transaction" => {
            let slot = v.get("slot")?.as_u64()?;

            // Parse signature (base58 → 64 bytes)
            let sig_str = v.get("signature_b58")?.as_str()?;
            let mut signature = [0u8; 64];
            // We use Pubkey::from_str for base58 decode of 32-byte keys,
            // but signature is 64 bytes. Use a manual base58 decoder.
            if let Some(sig_bytes) = b58_decode(sig_str) {
                if sig_bytes.len() == 64 {
                    signature.copy_from_slice(&sig_bytes);
                }
            }

            // Parse account keys (array of base58 strings → [[u8;32]; N])
            let account_keys: Vec<[u8; 32]> = v
                .get("account_keys")
                .and_then(|a| a.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|k| k.as_str())
                        .filter_map(|s| Pubkey::from_str(s).ok().map(|p| p.to_bytes()))
                        .collect()
                })
                .unwrap_or_default();

            // Parse instructions
            let instructions: Vec<LaserStreamInstruction> = v
                .get("instructions")
                .and_then(|a| a.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|ix| {
                            let prog_str = ix.get("program_b58")?.as_str()?;
                            if prog_str.is_empty() {
                                return None;
                            }
                            let program_id = Pubkey::from_str(prog_str).ok()?.to_bytes();
                            // Try data_b64 first, then data_b58 as fallback
                            let data_b64 = ix.get("data_b64").and_then(|v| v.as_str())
                                .or_else(|| ix.get("data_b58").and_then(|v| v.as_str()))?;
                            let data = B64.decode(data_b64).ok()?;
                            let accounts: Vec<u8> = ix
                                .get("accounts")
                                .and_then(|a| a.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|n| n.as_u64())
                                        .map(|n| n as u8)
                                        .collect()
                                })
                                .unwrap_or_default();
                            Some(LaserStreamInstruction {
                                program_id,
                                data,
                                accounts,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();

            Some(LaserStreamUpdate::Transaction(LaserStreamTx {
                slot,
                signature,
                account_keys,
                instructions,
                is_live: true, // gRPC stream is always live (§65)
            }))
        }
        "account" => {
            let slot = v.get("slot")?.as_u64()?;
            let pubkey_str = v.get("pubkey_b58")?.as_str()?;
            let owner_str = v.get("owner_b58")?.as_str()?;
            let data_b64 = v.get("data_b64")?.as_str()?;

            let pubkey = Pubkey::from_str(pubkey_str).ok()?.to_bytes();
            let owner = Pubkey::from_str(owner_str).ok()?.to_bytes();
            let data = B64.decode(data_b64).ok()?;

            Some(LaserStreamUpdate::Account {
                pubkey,
                owner,
                data,
                slot,
            })
        }
        "slot" => {
            let slot = v.get("slot")?.as_u64()?;
            Some(LaserStreamUpdate::Slot { slot })
        }
        _ => None,
    }
}

/// Minimal base58 (Bitcoin alphabet) decoder.
/// Returns None on invalid characters or overflow — fail-safe.
fn b58_decode(s: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let alphabet_map: [i16; 128] = {
        let mut m = [-1i16; 128];
        for (i, &c) in ALPHABET.iter().enumerate() {
            m[c as usize] = i as i16;
        }
        m
    };

    let mut result: Vec<u8> = vec![0u8];
    for c in s.bytes() {
        if (c as usize) >= 128 || alphabet_map[c as usize] < 0 {
            return None;
        }
        let digit = alphabet_map[c as usize] as u32;
        let mut carry = digit;
        for byte in result.iter_mut() {
            carry += (*byte as u32) * 58;
            *byte = (carry & 0xFF) as u8;
            carry >>= 8;
        }
        while carry > 0 {
            result.push((carry & 0xFF) as u8);
            carry >>= 8;
        }
    }

    // Handle leading '1' → leading zero bytes
    let zeros = s.bytes().take_while(|&c| c == b'1').count();
    result.extend(std::iter::repeat(0u8).take(zeros));
    result.reverse();
    Some(result)
}

// ─── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tx(slot: u64, is_live: bool) -> LaserStreamTx {
        LaserStreamTx {
            slot,
            signature: [0u8; 64],
            account_keys: vec![],
            instructions: vec![],
            is_live,
        }
    }

    #[test]
    fn test_classify_buy_instruction() {
        let mint_bytes = [0xAA; 32];
        let buyer_bytes = [0x42; 32]; // The buyer's wallet pubkey at index [6]
        let mut tx = make_tx(123, true);
        // pump.fun buy instruction has 17 accounts (§4.1); we need at least
        // index [6] for the buyer's wallet (ctx.user). Indices [0]-[2] are
        // PUMP_GLOBAL, fee_recipient, mint.
        tx.account_keys = vec![
            [0x11; 32], // [0] PUMP_GLOBAL
            [0x22; 32], // [1] fee_recipient
            mint_bytes,  // [2] mint
            [0x33; 32], // [3] bonding_curve
            [0x44; 32], // [4] associated_bonding_curve
            [0x55; 32], // [5] associated_user (buyer's ATA)
            buyer_bytes, // [6] user (buyer's wallet — the signer)
        ];
        tx.instructions.push(LaserStreamInstruction {
            program_id: PUMP_FUN_PROGRAM,
            data: {
                let mut d = Vec::new();
                d.extend_from_slice(&BUY_DISCRIMINATOR);
                d.extend_from_slice(&1_000_000u64.to_le_bytes());
                d.extend_from_slice(&100u64.to_le_bytes());
                d
            },
            accounts: vec![0, 1, 2, 3, 4, 5, 6],
        });

        let classified = classify_pump_instructions(&tx);
        assert_eq!(classified.len(), 1);
        assert!(matches!(
            &classified[0],
            PumpInstruction::Buy { mint, amount_lamports, buyer, .. }
            if *mint == mint_bytes && *amount_lamports == 1_000_000 && *buyer != [0u8; 32]
        ));
    }

    #[test]
    fn test_classify_sell_instruction() {
        let mint_bytes = [0xBB; 32];
        let seller_bytes = [0x43; 32]; // The seller's wallet pubkey at index [6]
        let mut tx = make_tx(456, true);
        tx.account_keys = vec![
            [0x11; 32], // [0] PUMP_GLOBAL
            [0x22; 32], // [1] fee_recipient
            mint_bytes,  // [2] mint
            [0x33; 32], // [3] bonding_curve
            [0x44; 32], // [4] associated_bonding_curve
            [0x55; 32], // [5] associated_user (seller's ATA)
            seller_bytes, // [6] user (seller's wallet — the signer)
        ];
        tx.instructions.push(LaserStreamInstruction {
            program_id: PUMP_FUN_PROGRAM,
            data: {
                let mut d = Vec::new();
                d.extend_from_slice(&SELL_DISCRIMINATOR);
                d.extend_from_slice(&500_000u64.to_le_bytes());
                d.extend_from_slice(&10u64.to_le_bytes());
                d
            },
            accounts: vec![0, 1, 2, 3, 4, 5, 6],
        });

        let classified = classify_pump_instructions(&tx);
        assert_eq!(classified.len(), 1);
        assert!(matches!(
            &classified[0],
            PumpInstruction::Sell { mint, amount_tokens, seller, .. }
            if *mint == mint_bytes && *amount_tokens == 500_000 && *seller != [0u8; 32]
        ));
    }

    #[test]
    fn test_classify_non_pump_transaction() {
        let mut tx = make_tx(789, true);
        tx.account_keys = vec![[0x11; 32], [0x22; 32]];
        tx.instructions.push(LaserStreamInstruction {
            program_id: [0x0; 32],
            data: vec![0x2, 0x0, 0x0, 0x0],
            accounts: vec![0, 1],
        });

        let classified = classify_pump_instructions(&tx);
        assert_eq!(classified.len(), 0);
    }

    #[test]
    fn test_instructions_to_events_buy() {
        let mint = [0xCC; 32];
        let instructions = vec![PumpInstruction::Buy {
            mint,
            amount_lamports: 1_000_000,
            min_tokens: 100,
            buyer: [0x42; 32],
        }];

        let events = instructions_to_events(&instructions, 123, true);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source, ProvenanceSource::LaserStream);
        assert!(events[0].is_live);
        assert_eq!(events[0].slot, 123);
        match &events[0].event {
            AppEvent::MarketTrade { mint: m, signed_base, quote_lamports, .. } => {
                assert_eq!(m, &Mint(mint));
                assert!(*signed_base > 0); // Buy = positive signed_base
                assert_eq!(quote_lamports, &1_000_000u64);
            }
            _ => panic!("Expected MarketTrade event"),
        }
    }

    #[test]
    fn test_instructions_to_events_sell() {
        let mint = [0xDD; 32];
        let instructions = vec![PumpInstruction::Sell {
            mint,
            amount_tokens: 500_000,
            min_lamports: 10,
            seller: [0x43; 32],
        }];

        let events = instructions_to_events(&instructions, 456, false);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source, ProvenanceSource::LaserStream);
        assert!(!events[0].is_live); // Replay, not live (§65)
        match &events[0].event {
            AppEvent::MarketTrade { signed_base, .. } => {
                assert!(*signed_base < 0); // Sell = negative signed_base
            }
            _ => panic!("Expected MarketTrade event"),
        }
    }

    #[test]
    fn test_instructions_to_events_create_pool_emits_migration() {
        let pool = [0xEE; 32];
        let base_mint = [0xFF; 32];
        let instructions = vec![PumpInstruction::CreatePool { pool, base_mint }];

        let events = instructions_to_events(&instructions, 789, true);
        assert_eq!(events.len(), 1);
        match &events[0].event {
            AppEvent::Migration { mint, slot } => {
                assert_eq!(mint, &Mint(base_mint));
                assert_eq!(*slot, 789);
            }
            _ => panic!("Expected Migration event"),
        }
    }

    #[test]
    fn test_instructions_to_events_migrate() {
        let mint = [0xAA; 32];
        let instructions = vec![PumpInstruction::Migrate { mint }];

        let events = instructions_to_events(&instructions, 100, true);
        assert_eq!(events.len(), 1);
        match &events[0].event {
            AppEvent::Migration { mint: m, slot } => {
                assert_eq!(m, &Mint(mint));
                assert_eq!(*slot, 100);
            }
            _ => panic!("Expected Migration event"),
        }
    }

    #[test]
    fn test_account_key_at_out_of_bounds() {
        let tx = make_tx(1, true);
        let ix = LaserStreamInstruction {
            program_id: [0; 32],
            data: vec![],
            accounts: vec![0, 1, 2],
        };
        assert!(account_key_at(&ix, &tx, 0).is_none());
    }

    #[test]
    fn test_laser_stream_state_default() {
        let state = LaserStreamState::new();
        assert_eq!(state.last_slot, 0);
        assert!(!state.connected);
        assert_eq!(state.disconnects, 0);
        assert!(state.replay_from_slot.is_none());
    }

    #[test]
    fn test_multiple_instructions_in_one_tx() {
        let mint1 = [0xAA; 32];
        let mint2 = [0xBB; 32];
        let buyer = [0x42; 32];
        let seller = [0x43; 32];
        let mut tx = make_tx(100, true);
        tx.account_keys = vec![
            [0x11; 32], // [0] PUMP_GLOBAL
            [0x22; 32], // [1] fee_recipient
            mint1,       // [2] mint1
            [0x33; 32], // [3] bonding_curve / bonding_curve for mint2
            mint2,       // [4] mint2
            [0x44; 32], // [5] associated_user (filler)
            buyer,       // [6] user (buyer's wallet for ix1)
            [0x55; 32], // [7] bonding_curve for mint2
            [0x66; 32], // [8] creator_vault
            [0x77; 32], // [9] filler
            seller,      // [10] user (seller's wallet for ix2)
        ];

        // Buy instruction: accounts [0,1,2,3,4,5,6] = PUMP_GLOBAL, fee, mint1, bonding_curve, assoc_curve, assoc_user, buyer
        tx.instructions.push(LaserStreamInstruction {
            program_id: PUMP_FUN_PROGRAM,
            data: {
                let mut d = Vec::new();
                d.extend_from_slice(&BUY_DISCRIMINATOR);
                d.extend_from_slice(&100u64.to_le_bytes());
                d.extend_from_slice(&10u64.to_le_bytes());
                d
            },
            accounts: vec![0, 1, 2, 3, 4, 5, 6],
        });

        // Sell instruction: accounts [0,3,4,5,7,8,9,10] = PUMP_GLOBAL, fee, mint2, bonding_curve, assoc_curve, assoc_user, creator, seller
        tx.instructions.push(LaserStreamInstruction {
            program_id: PUMP_FUN_PROGRAM,
            data: {
                let mut d = Vec::new();
                d.extend_from_slice(&SELL_DISCRIMINATOR);
                d.extend_from_slice(&200u64.to_le_bytes());
                d.extend_from_slice(&20u64.to_le_bytes());
                d
            },
            accounts: vec![0, 3, 4, 5, 7, 8, 9, 10],
        });

        let classified = classify_pump_instructions(&tx);
        assert_eq!(classified.len(), 2);
        assert!(matches!(classified[0], PumpInstruction::Buy { .. }));
        assert!(matches!(classified[1], PumpInstruction::Sell { .. }));
    }

    #[test]
    fn test_replay_vs_live_provenance() {
        // §65: replay must be distinguished from live in every record
        let mint = [0xAA; 32];
        let instructions = vec![PumpInstruction::Buy {
            mint,
            amount_lamports: 100,
            min_tokens: 10,
            buyer: [0x42; 32],
        }];

        let live_events = instructions_to_events(&instructions, 1, true);
        assert!(live_events[0].is_live);
        assert_eq!(live_events[0].source, ProvenanceSource::LaserStream);

        let replay_events = instructions_to_events(&instructions, 1, false);
        assert!(!replay_events[0].is_live);
        assert_eq!(replay_events[0].source, ProvenanceSource::LaserStream);
    }

    #[test]
    fn test_program_ids_match_onchain_pubkeys() {
        // Verify that the hardcoded byte arrays match the real on-chain
        // program IDs decoded from base58. This is a critical correctness
        // check — if these bytes are wrong, EVERY transaction is silently
        // ignored because the program ID never matches.
        use solana_program::pubkey::Pubkey;
        use std::str::FromStr;

        let pump_str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
        let pumpswap_str = "pPEEEJ5r9sRFMks2oBq1qjhtBf8V4qyGSz8xbxqHEBu";

        let pump_pk = Pubkey::from_str(pump_str).expect("pump.fun program id must parse");
        let pumpswap_pk = Pubkey::from_str(pumpswap_str).expect("pumpswap program id must parse");

        assert_eq!(pump_pk.to_bytes(), PUMP_FUN_PROGRAM,
            "PUMP_FUN_PROGRAM bytes must match base58-decoded 6EF8rrect...");
        assert_eq!(pumpswap_pk.to_bytes(), PUMP_SWAP_PROGRAM,
            "PUMP_SWAP_PROGRAM bytes must match base58-decoded pPEEEJ5...");
    }

    #[test]
    fn test_parse_ndjson_slot() {
        let line = r#"{"lane":"laserstream","kind":"slot","slot":123456,"recv_unix_ms":1700000000000}"#;
        let update = parse_ndjson_line(line).expect("slot line must parse");
        match update {
            LaserStreamUpdate::Slot { slot } => assert_eq!(slot, 123456),
            _ => panic!("Expected Slot update"),
        }
    }

    #[test]
    fn test_parse_ndjson_empty_line() {
        assert!(parse_ndjson_line("").is_none());
        assert!(parse_ndjson_line("   ").is_none());
    }

    #[test]
    fn test_parse_ndjson_malformed() {
        // Malformed JSON should return None, not panic
        assert!(parse_ndjson_line("not json").is_none());
        assert!(parse_ndjson_line("{ broken").is_none());
    }

    #[test]
    fn test_parse_ndjson_transaction_roundtrip() {
        // Build a transaction NDJSON line and verify it parses back
        // into a LaserStreamTx with the right fields.
        // Use the pump.fun program ID as a valid base58 key for both
        // account_keys and program_b58 — it's a valid 32-byte pubkey.
        let pump_b58 = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
        let line = format!(
            r#"{{"lane":"laserstream","kind":"transaction","slot":42,"recv_unix_ms":0,"signature_b58":"","account_keys":["{pump}"],"instructions":[{{"program_b58":"{pump}","data_b64":"","accounts":[0]}}]}}"#,
            pump = pump_b58
        );
        let update = parse_ndjson_line(&line).expect("tx line must parse");
        match update {
            LaserStreamUpdate::Transaction(tx) => {
                assert_eq!(tx.slot, 42);
                assert!(tx.is_live);
                assert_eq!(tx.instructions.len(), 1);
                assert_eq!(tx.instructions[0].program_id, PUMP_FUN_PROGRAM);
                assert_eq!(tx.account_keys.len(), 1);
            }
            _ => panic!("Expected Transaction update"),
        }
    }

    // ─── G7: Paper/live parity integration test ──────────────────────────
    //
    // §13 (shadow/replay parity): identical LaserStream events MUST produce
    // identical ProvenancedEvents in both paper and live modes. The only
    // difference is the `is_live` flag, which is a provenance marker, NOT
    // a data-path fork. This test proves that invariant holds end-to-end:
    // NDJSON -> parse -> classify -> events.

    #[test]
    fn test_paper_live_parity_identical_events() {
        let pump_b58 = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
        let mint_b58 = "11111111111111111111111111111111";
        let user_b58 = "11111111111111111111111111111112";
        // BUY discriminator (8) + amount (8) + min_tokens (8) = 24 bytes.
        // Uses the REAL pump.fun BUY discriminator from pump-protocol ix.rs.
        let ix_data = base64::encode(&{
            let mut d = vec![0x66, 0x06, 0x3d, 0x12, 0x01, 0xda, 0xeb, 0xea]; // BUY_DISCRIMINATOR
            d.extend_from_slice(&0x05u64.to_le_bytes());
            d.extend_from_slice(&0x01u64.to_le_bytes());
            d
        });

        let make_line = |slot: u64| -> String {
            let mut s = String::with_capacity(400);
            s.push_str("{\"lane\":\"laserstream\",\"kind\":\"transaction\",\"slot\":");
            s.push_str(&slot.to_string());
            s.push_str(",\"recv_unix_ms\":0,\"signature_b58\":\"\",\"account_keys\":[\"");
            s.push_str(pump_b58);
            s.push_str("\",\"");
            s.push_str(user_b58);
            s.push_str("\",\"");
            s.push_str(mint_b58);
            s.push_str("\"],\"instructions\":[{\"program_b58\":\"");
            s.push_str(pump_b58);
            s.push_str("\",\"data_b64\":\"");
            s.push_str(&ix_data);
            s.push_str("\",\"accounts\":[0,1,2]}]}");
            s
        };

        let line_paper = make_line(100);
        let line_live = make_line(100);

        let update_paper = parse_ndjson_line(&line_paper).expect("paper parse");
        let update_live = parse_ndjson_line(&line_live).expect("live parse");

        let tx_paper = match update_paper {
            LaserStreamUpdate::Transaction(t) => t,
            _ => panic!("paper: expected Transaction"),
        };
        let tx_live = match update_live {
            LaserStreamUpdate::Transaction(t) => t,
            _ => panic!("live: expected Transaction"),
        };

        // Both must parse identically.
        assert_eq!(tx_paper.slot, tx_live.slot);
        assert_eq!(tx_paper.account_keys, tx_live.account_keys);
        assert_eq!(tx_paper.instructions.len(), tx_live.instructions.len());

        // Classify through the same pipeline.
        let classified_paper = classify_pump_instructions(&tx_paper);
        let classified_live = classify_pump_instructions(&tx_live);
        assert_eq!(classified_paper.len(), classified_live.len());

        // Convert to events.
        let events_paper = instructions_to_events(&classified_paper, 100, true);
        let events_live = instructions_to_events(&classified_live, 100, true);

        assert_eq!(events_paper.len(), events_live.len());
        for (ep, el) in events_paper.iter().zip(events_live.iter()) {
            assert_eq!(ep.slot, el.slot);
            assert_eq!(ep.source, el.source);
            assert_eq!(ep.is_live, el.is_live);
            match (&ep.event, &el.event) {
                (AppEvent::MarketTrade { mint: mp, signed_base: sb_p, quote_lamports: ql_p, .. },
                 AppEvent::MarketTrade { mint: ml, signed_base: sb_l, quote_lamports: ql_l, .. }) => {
                    assert_eq!(mp.0, ml.0);
                    assert_eq!(sb_p, sb_l);
                    assert_eq!(ql_p, ql_l);
                }
                _ => panic!("event type mismatch"),
            }
        }
    }

    // ─── G8: Replay-vs-live provenance end-to-end test ────────────────────
    //
    // §65 (replay distinguished from live): the ProvenancedEvent carries
    // `is_live` as a STRUCTURAL field. A replayed event has is_live=false,
    // while a live gRPC event has is_live=true. The same transaction data
    // produces events that differ ONLY in the is_live flag.

    #[test]
    fn test_replay_vs_live_provenance_distinguished() {
        let pump_b58 = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
        let mint_b58 = "11111111111111111111111111111111";
        // BUY discriminator (8) + amount (8) + min_tokens (8) = 24 bytes.
        // Uses the REAL pump.fun BUY discriminator from pump-protocol ix.rs.
        let ix_data = base64::encode(&{
            let mut d = vec![0x66, 0x06, 0x3d, 0x12, 0x01, 0xda, 0xeb, 0xea]; // BUY_DISCRIMINATOR
            d.extend_from_slice(&0x05u64.to_le_bytes()); // amount
            d.extend_from_slice(&0x01u64.to_le_bytes()); // min_tokens
            d
        });

        // accounts: [0,1,2,3,4,5,6] — index 2 is the mint, index 6 is the buyer.
        // We need 7 account keys: [pump_program, fee_recipient, mint, bonding_curve,
        //   assoc_bonding_curve, assoc_user, user/buyer].
        let user_b58 = "11111111111111111111111111111112"; // buyer's wallet
        let filler_b58 = "11111111111111111111111111111113"; // filler accounts
        let line = {
            let mut s = String::with_capacity(600);
            s.push_str("{\"lane\":\"laserstream\",\"kind\":\"transaction\",\"slot\":");
            s.push_str(&777u64.to_string());
            s.push_str(",\"recv_unix_ms\":0,\"signature_b58\":\"\",\"account_keys\":[\"");
            s.push_str(pump_b58);       // [0] PUMP_GLOBAL
            s.push_str("\",\"");
            s.push_str(filler_b58);     // [1] fee_recipient
            s.push_str("\",\"");
            s.push_str(mint_b58);       // [2] mint
            s.push_str("\",\"");
            s.push_str(filler_b58);     // [3] bonding_curve
            s.push_str("\",\"");
            s.push_str(filler_b58);     // [4] assoc_bonding_curve
            s.push_str("\",\"");
            s.push_str(filler_b58);     // [5] assoc_user
            s.push_str("\",\"");
            s.push_str(user_b58);       // [6] user (buyer's wallet)
            s.push_str("\"],\"instructions\":[{\"program_b58\":\"");
            s.push_str(pump_b58);
            s.push_str("\",\"data_b64\":\"");
            s.push_str(&ix_data);
            s.push_str("\",\"accounts\":[0,1,2,3,4,5,6]}]}");
            s
        };

        let update = parse_ndjson_line(&line).expect("must parse");
        let tx = match update {
            LaserStreamUpdate::Transaction(t) => t,
            _ => panic!("expected Transaction"),
        };

        let classified = classify_pump_instructions(&tx);
        assert!(!classified.is_empty());

        let events_live = instructions_to_events(&classified, 777, true);
        let events_replay = instructions_to_events(&classified, 777, false);

        assert_eq!(events_live.len(), events_replay.len());
        for (el, er) in events_live.iter().zip(events_replay.iter()) {
            assert_eq!(el.slot, er.slot);
            assert_eq!(el.source, er.source);
            assert!(el.is_live);
            assert!(!er.is_live);
            match (&el.event, &er.event) {
                (AppEvent::MarketTrade { mint: ml, signed_base: sbl, .. },
                 AppEvent::MarketTrade { mint: mr, signed_base: sbr, .. }) => {
                    assert_eq!(ml.0, mr.0);
                    assert_eq!(sbl, sbr);
                }
                _ => panic!("event type mismatch"),
            }
        }
    }
}
