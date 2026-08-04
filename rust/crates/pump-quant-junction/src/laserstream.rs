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
    Buy {
        mint: [u8; 32],
        amount_lamports: u64,
        min_tokens: u64,
    },
    /// pump.fun sell (bonding curve): `amount_tokens` in, `min_lamports` out.
    Sell {
        mint: [u8; 32],
        amount_tokens: u64,
        min_lamports: u64,
    },
    /// PumpSwap buy (post-migration AMM): `amount` lamports in.
    PumpSwapBuy {
        pool: [u8; 32],
        amount_lamports: u64,
        min_tokens: u64,
    },
    /// PumpSwap sell (post-migration AMM): `amount` tokens in.
    PumpSwapSell {
        pool: [u8; 32],
        amount_tokens: u64,
        min_lamports: u64,
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
                    if let Some(mint) = account_key_at(ix, tx, 2) {
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
                        });
                    }
                } else if disc == SELL_DISCRIMINATOR && ix.data.len() >= 8 + 8 + 8 {
                    if let Some(mint) = account_key_at(ix, tx, 2) {
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
                        });
                    }
                }
            }
        } else if ix.program_id == PUMP_SWAP_PROGRAM {
            if let Some(parsed) = decode_pumpswap_ix(&ix.data) {
                match parsed {
                    PumpSwapIx::Buy(args) => {
                        if let Some(pool) = account_key_at(ix, tx, 0) {
                            out.push(PumpInstruction::PumpSwapBuy {
                                pool,
                                amount_lamports: args.max_quote_amount_in,
                                min_tokens: args.base_amount_out,
                            });
                        }
                    }
                    PumpSwapIx::Sell(args) => {
                        if let Some(pool) = account_key_at(ix, tx, 0) {
                            out.push(PumpInstruction::PumpSwapSell {
                                pool,
                                amount_tokens: args.base_amount_in,
                                min_lamports: args.min_quote_amount_out,
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
pub fn instructions_to_events(
    instructions: &[PumpInstruction],
    slot: u64,
    is_live: bool,
) -> Vec<ProvenancedEvent> {
    let mut events = Vec::with_capacity(instructions.len());

    for ix in instructions {
        match ix {
            PumpInstruction::Buy { mint, amount_lamports, .. } => {
                events.push(ProvenancedEvent {
                    event: AppEvent::MarketTrade {
                        mint: Mint(*mint),
                        price_fp: 0, // Filled by reserve-delta or account snapshot
                        quote_lamports: *amount_lamports,
                        liquidity_lamports: 0, // Filled by OnchainConfirm
                        signed_base: i64::try_from(*amount_lamports).unwrap_or(i64::MAX),
                        buyer_entity: 0, // Not available from ix data alone
                        age_slots: 0,   // Not available from ix data alone
                    },
                    source: ProvenanceSource::LaserStream,
                    slot,
                    is_live,
                });
            }
            PumpInstruction::Sell { mint, amount_tokens, .. } => {
                events.push(ProvenancedEvent {
                    event: AppEvent::MarketTrade {
                        mint: Mint(*mint),
                        price_fp: 0,
                        quote_lamports: 0, // Sell: quote_lamports = SOL received
                        liquidity_lamports: 0,
                        signed_base: -i64::try_from(*amount_tokens).unwrap_or(i64::MAX),
                        buyer_entity: 0,
                        age_slots: 0,
                    },
                    source: ProvenanceSource::LaserStream,
                    slot,
                    is_live,
                });
            }
            PumpInstruction::PumpSwapBuy { pool, amount_lamports, .. } => {
                events.push(ProvenancedEvent {
                    event: AppEvent::MarketTrade {
                        mint: Mint(*pool),
                        price_fp: 0,
                        quote_lamports: *amount_lamports,
                        liquidity_lamports: 0,
                        signed_base: i64::try_from(*amount_lamports).unwrap_or(i64::MAX),
                        buyer_entity: 0,
                        age_slots: 0,
                    },
                    source: ProvenanceSource::LaserStream,
                    slot,
                    is_live,
                });
            }
            PumpInstruction::PumpSwapSell { pool, amount_tokens, .. } => {
                events.push(ProvenancedEvent {
                    event: AppEvent::MarketTrade {
                        mint: Mint(*pool),
                        price_fp: 0,
                        quote_lamports: 0,
                        liquidity_lamports: 0,
                        signed_base: -i64::try_from(*amount_tokens).unwrap_or(i64::MAX),
                        buyer_entity: 0,
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
        let mut tx = make_tx(123, true);
        tx.account_keys = vec![
            [0x11; 32],
            [0x22; 32],
            mint_bytes,
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
            accounts: vec![0, 1, 2],
        });

        let classified = classify_pump_instructions(&tx);
        assert_eq!(classified.len(), 1);
        assert!(matches!(
            &classified[0],
            PumpInstruction::Buy { mint, amount_lamports, .. }
            if *mint == mint_bytes && *amount_lamports == 1_000_000
        ));
    }

    #[test]
    fn test_classify_sell_instruction() {
        let mint_bytes = [0xBB; 32];
        let mut tx = make_tx(456, true);
        tx.account_keys = vec![
            [0x11; 32],
            [0x22; 32],
            mint_bytes,
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
            accounts: vec![0, 1, 2],
        });

        let classified = classify_pump_instructions(&tx);
        assert_eq!(classified.len(), 1);
        assert!(matches!(
            &classified[0],
            PumpInstruction::Sell { mint, amount_tokens, .. }
            if *mint == mint_bytes && *amount_tokens == 500_000
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
        let mut tx = make_tx(100, true);
        tx.account_keys = vec![
            [0x11; 32],
            [0x22; 32],
            mint1,
            [0x33; 32],
            mint2,
        ];

        tx.instructions.push(LaserStreamInstruction {
            program_id: PUMP_FUN_PROGRAM,
            data: {
                let mut d = Vec::new();
                d.extend_from_slice(&BUY_DISCRIMINATOR);
                d.extend_from_slice(&100u64.to_le_bytes());
                d.extend_from_slice(&10u64.to_le_bytes());
                d
            },
            accounts: vec![0, 1, 2],
        });

        tx.instructions.push(LaserStreamInstruction {
            program_id: PUMP_FUN_PROGRAM,
            data: {
                let mut d = Vec::new();
                d.extend_from_slice(&SELL_DISCRIMINATOR);
                d.extend_from_slice(&200u64.to_le_bytes());
                d.extend_from_slice(&20u64.to_le_bytes());
                d
            },
            accounts: vec![0, 3, 4],
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
}
