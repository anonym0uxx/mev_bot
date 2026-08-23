//! Causal event normalizer — decodes `pump_event_v1` records from raw
//! LaserStream transaction + account updates.
//!
//! DESIGN PRINCIPLES:
//! * Hindsight-free: never labels GOOD/BAD/profit. Contains only causal truth
//!   observable at or before the event's slot/time.
//! * Integer/exact-rational: lamports and token base units as u64/i128. No
//!   canonical floats, NO USD market cap.
//! * Venue-by-program-ID: discriminators overlap between pump.fun and PumpSwap,
//!   so we route by the program ID of the instruction, not the discriminator.
//! * Reuses the pump-quant-protocol decoders where possible (ix discriminators,
//!   pumpswap_ix, curve math). Since this crate cannot depend on the workspace
//!   crate (server-only build), the constants are ported here byte-for-byte.
//! * Arrival-order preserved: each event gets a monotonic `event_index`.
//! * Deterministic dedupe: signatures are tracked; a duplicate signature within
//!   the same slot is suppressed and counted.

use std::collections::HashSet;

use serde::Serialize;

use crate::encoding::{b58_encode, sha256_hex};

// ─── Program IDs (base58) ───────────────────────────────────────────────

const PUMP_FUN_PROGRAM_ID: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
const PUMP_SWAP_PROGRAM_ID: &str = "pPEEEJ5r9sRFMks2oBq1qjhtBf8V4qyGSz8xbxqHEBu";

// ─── Instruction discriminators (sha256("global:<name>")[..8]) ──────────

const PUMP_BUY: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];
const PUMP_SELL: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];
const PUMP_CREATE: [u8; 8] = [24, 30, 200, 40, 5, 28, 7, 119];
const PUMP_COMPLETE: [u8; 8] = [0, 77, 224, 147, 136, 25, 88, 76];
const PUMP_MIGRATE: [u8; 8] = [155, 234, 231, 146, 236, 158, 162, 30];

const PUMPSWAP_BUY: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234]; // same as pump buy
const PUMPSWAP_SELL: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173]; // same
const PUMPSWAP_CREATE_POOL: [u8; 8] = [233, 146, 209, 142, 207, 104, 64, 188];
const PUMPSWAP_DEPOSIT: [u8; 8] = [242, 35, 198, 137, 82, 225, 242, 182];
const PUMPSWAP_WITHDRAW: [u8; 8] = [183, 18, 70, 156, 148, 109, 161, 34];

// ─── Output event types ─────────────────────────────────────────────────

/// The normalized pump_event_v1 record written to the events NDJSON file.
/// Hindsight-free — contains only causal truth for a future labeler.
#[derive(Serialize, Clone)]
pub struct PumpEventV1 {
    pub event_index: u64,
    pub event_type: String,
    pub venue: String,
    pub slot: u64,
    pub tx_index: Option<u64>,
    pub signature_b58: String,
    pub raw_hash: String,
    pub recv_unix_ms: u64,
    pub is_live: bool,

    // ── Common identity fields ──
    pub mint_b58: Option<String>,
    pub trader_b58: Option<String>,
    pub creator_b58: Option<String>,

    // ── Pump curve fields (bonding-curve events) ──
    pub curve_account_b58: Option<String>,
    pub virtual_sol: Option<u64>,
    pub virtual_token: Option<u64>,
    pub real_sol: Option<u64>,
    pub real_token: Option<u64>,
    pub curve_complete: Option<bool>,
    pub mayhem: Option<bool>,
    pub cashback: Option<bool>,

    // ── PumpSwap pool fields ──
    pub pool_account_b58: Option<String>,
    pub base_reserve: Option<u64>,
    pub quote_reserve: Option<u64>,
    pub lp_supply: Option<u64>,

    // ── Trade economics (integer-only) ──
    pub amount_in: Option<u64>,
    pub amount_out: Option<u64>,
    pub min_amount_out: Option<u64>,
    pub max_amount_in: Option<u64>,
    pub fee_bps: Option<u32>,
    pub trade_side: Option<String>, // "buy" | "sell"

    // ── Pump create specifics ──
    pub initial_supply: Option<u64>,
    pub initial_virtual_sol: Option<u64>,
    pub initial_virtual_token: Option<u64>,

    // ── Transaction meta ──
    pub fee_lamports: Option<u64>,
    pub cu_consumed: Option<u64>,
    pub tx_status: Option<String>, // "success" | "failed"
    pub err_hex: Option<String>,

    // ── Token metadata (from create) ──
    pub token_name: Option<String>,
    pub token_symbol: Option<String>,
    pub token_uri: Option<String>,
    pub decimals: Option<u32>,

    // ── Balance deltas (SOL + token) for causal reconstruction ──
    pub pre_sol_balances: Option<Vec<u64>>,
    pub post_sol_balances: Option<Vec<u64>>,
    pub pre_token_balances_json: Option<serde_json::Value>,
    pub post_token_balances_json: Option<serde_json::Value>,

    // ── Inner instructions (CPI) for graduation/migration detection ──
    pub inner_instructions_json: Option<serde_json::Value>,
    pub log_messages: Option<Vec<String>>,

    // ── Account-key list (for PDA resolution + trajectory reconstruction) ──
    pub account_keys_b58: Option<Vec<String>>,

    // ── Our-wallet flag (safe, no key material) ──
    pub is_our_wallet: Option<bool>,
}

/// The normalizer — takes raw protobuf-derived data and produces causal
/// pump_event_v1 records. Tracks dedupe state.
pub struct Normalizer {
    event_counter: u64,
    seen_signatures: HashSet<String>,
    duplicates: u64,
    decode_failures: u64,
    unknown_events: u64,
    // Counters for manifest
    pub creates: u64,
    pub pump_buys: u64,
    pub pump_sells: u64,
    pub pump_completes: u64,
    pub migrations: u64,
    pub pumpswap_buys: u64,
    pub pumpswap_sells: u64,
    pub pumpswap_create_pools: u64,
    pub pumpswap_deposits: u64,
    pub pumpswap_withdraws: u64,
}

impl Normalizer {
    pub fn new() -> Self {
        Self {
            event_counter: 0,
            seen_signatures: HashSet::new(),
            duplicates: 0,
            decode_failures: 0,
            unknown_events: 0,
            creates: 0,
            pump_buys: 0,
            pump_sells: 0,
            pump_completes: 0,
            migrations: 0,
            pumpswap_buys: 0,
            pumpswap_sells: 0,
            pumpswap_create_pools: 0,
            pumpswap_deposits: 0,
            pumpswap_withdraws: 0,
        }
    }

    /// Attempt to normalize a transaction into one or more pump_event_v1
    /// records. Returns a vector of events (possibly empty if the tx is not
    /// pump-related). Deduplicates by signature within the same slot.
    pub fn normalize_tx(
        &mut self,
        slot: u64,
        tx_info: &helius_laserstream::grpc::SubscribeUpdateTransactionInfo,
        is_live: bool,
        our_wallet_b58: Option<&str>,
    ) -> Vec<PumpEventV1> {
        let sig_b58 = b58_encode(&tx_info.signature);
        let raw_hash = sha256_hex(&tx_info.signature);

        // Dedupe: same signature in same slot = duplicate.
        let dedupe_key = format!("{slot}:{sig_b58}");
        if self.seen_signatures.contains(&dedupe_key) {
            self.duplicates += 1;
            return vec![];
        }
        self.seen_signatures.insert(dedupe_key);

        // Skip vote transactions.
        if tx_info.is_vote {
            return vec![];
        }

        // Extract account keys from the message.
        let account_keys_b58: Vec<String> = tx_info
            .transaction
            .as_ref()
            .and_then(|t| t.message.as_ref())
            .map(|m| m.account_keys.iter().map(|k| b58_encode(k)).collect())
            .unwrap_or_default();

        // Extract meta fields.
        let meta = tx_info.meta.as_ref();
        let fee_lamports = meta.map(|m| m.fee);
        let cu_consumed = meta.and_then(|m| m.compute_units_consumed);
        let tx_status = meta.map(|m| if m.err.is_none() { "success" } else { "failed" }).map(String::from);
        let err_hex = meta.and_then(|m| m.err.as_ref().map(|e| crate::encoding::hex_encode(&e.err)));

        let pre_sol = meta.map(|m| m.pre_balances.clone());
        let post_sol = meta.map(|m| m.post_balances.clone());

        let pre_token_json = meta.map(|m| token_balances_to_json(&m.pre_token_balances));
        let post_token_json = meta.map(|m| token_balances_to_json(&m.post_token_balances));

        let inner_json = meta.map(|m| inner_instructions_to_json(&m.inner_instructions));
        let logs = meta.map(|m| m.log_messages.clone());

        let recv_ms = crate::encoding::now_unix_ms();

        // Scan all instructions (outer + inner) for pump-related ones.
        let mut events = vec![];

        // Process outer instructions.
        if let Some(tx) = &tx_info.transaction {
            if let Some(msg) = &tx.message {
                for ix in &msg.instructions {
                    if let Some(event) = self.classify_instruction(
                        ix.program_id_index as usize,
                        &ix.data,
                        &ix.accounts,
                        &account_keys_b58,
                        slot,
                        tx_info.index,
                        &sig_b58,
                        &raw_hash,
                        recv_ms,
                        is_live,
                        fee_lamports,
                        cu_consumed,
                        tx_status.as_deref(),
                        err_hex.as_deref(),
                        pre_sol.as_deref(),
                        post_sol.as_deref(),
                        pre_token_json.as_ref(),
                        post_token_json.as_ref(),
                        inner_json.as_ref(),
                        logs.as_deref(),
                        &account_keys_b58,
                        our_wallet_b58,
                    ) {
                        self.count_event(&event);
                        events.push(event);
                    }
                }
            }
        }

        // Process inner instructions (CPI) — these carry graduation/migration.
        if let Some(meta) = meta {
            for group in &meta.inner_instructions {
                for ii in &group.instructions {
                    let prog_idx = ii.program_id_index as usize;
                    if let Some(event) = self.classify_instruction(
                        prog_idx,
                        &ii.data,
                        &ii.accounts,
                        &account_keys_b58,
                        slot,
                        tx_info.index,
                        &sig_b58,
                        &raw_hash,
                        recv_ms,
                        is_live,
                        fee_lamports,
                        cu_consumed,
                        tx_status.as_deref(),
                        err_hex.as_deref(),
                        pre_sol.as_deref(),
                        post_sol.as_deref(),
                        pre_token_json.as_ref(),
                        post_token_json.as_ref(),
                        inner_json.as_ref(),
                        logs.as_deref(),
                        &account_keys_b58,
                        our_wallet_b58,
                    ) {
                        self.count_event(&event);
                        events.push(event);
                    }
                }
            }
        }

        events
    }

    /// Classify a single instruction by its program ID + discriminator.
    /// Returns Some(event) if it's a pump-related instruction, else None.
    #[allow(clippy::too_many_arguments)]
    fn classify_instruction(
        &mut self,
        program_id_index: usize,
        data: &[u8],
        accounts: &[u8],
        account_keys_b58: &[String],
        slot: u64,
        tx_index: u64,
        sig_b58: &str,
        raw_hash: &str,
        recv_ms: u64,
        is_live: bool,
        fee: Option<u64>,
        cu: Option<u64>,
        status: Option<&str>,
        err_hex: Option<&str>,
        pre_sol: Option<&[u64]>,
        post_sol: Option<&[u64]>,
        pre_token: Option<&serde_json::Value>,
        post_token: Option<&serde_json::Value>,
        inner_json: Option<&serde_json::Value>,
        logs: Option<&[String]>,
        all_keys: &[String],
        our_wallet: Option<&str>,
    ) -> Option<PumpEventV1> {
        if data.len() < 8 {
            return None;
        }

        let program_id = account_keys_b58.get(program_id_index).map(|s| s.as_str()).unwrap_or("");

        let disc = &data[..8];

        // Route by program ID, then by discriminator.
        let (venue, event_type, trade_side) = if program_id == PUMP_FUN_PROGRAM_ID {
            if disc == PUMP_BUY {
                ("pumpfun", "buy", Some("buy"))
            } else if disc == PUMP_SELL {
                ("pumpfun", "sell", Some("sell"))
            } else if disc == PUMP_CREATE {
                ("pumpfun", "create", None)
            } else if disc == PUMP_COMPLETE {
                ("pumpfun", "complete", None)
            } else if disc == PUMP_MIGRATE {
                ("pumpfun", "migrate", None)
            } else {
                // Unknown pump.fun instruction — record as unknown for completeness.
                self.unknown_events += 1;
                return None;
            }
        } else if program_id == PUMP_SWAP_PROGRAM_ID {
            if disc == PUMPSWAP_BUY {
                ("pumpswap", "buy", Some("buy"))
            } else if disc == PUMPSWAP_SELL {
                ("pumpswap", "sell", Some("sell"))
            } else if disc == PUMPSWAP_CREATE_POOL {
                ("pumpswap", "create_pool", None)
            } else if disc == PUMPSWAP_DEPOSIT {
                ("pumpswap", "deposit", None)
            } else if disc == PUMPSWAP_WITHDRAW {
                ("pumpswap", "withdraw", None)
            } else {
                self.unknown_events += 1;
                return None;
            }
        } else {
            // Not a pump program.
            return None;
        };

        // ── Decode instruction args based on event type ──
        let mut ev = PumpEventV1 {
            event_index: {
                self.event_counter += 1;
                self.event_counter - 1
            },
            event_type: event_type.to_string(),
            venue: venue.to_string(),
            slot,
            tx_index: Some(tx_index),
            signature_b58: sig_b58.to_string(),
            raw_hash: raw_hash.to_string(),
            recv_unix_ms: recv_ms,
            is_live,
            mint_b58: None,
            trader_b58: None,
            creator_b58: None,
            curve_account_b58: None,
            virtual_sol: None,
            virtual_token: None,
            real_sol: None,
            real_token: None,
            curve_complete: None,
            mayhem: None,
            cashback: None,
            pool_account_b58: None,
            base_reserve: None,
            quote_reserve: None,
            lp_supply: None,
            amount_in: None,
            amount_out: None,
            min_amount_out: None,
            max_amount_in: None,
            fee_bps: None,
            trade_side: trade_side.map(String::from),
            initial_supply: None,
            initial_virtual_sol: None,
            initial_virtual_token: None,
            fee_lamports: fee,
            cu_consumed: cu,
            tx_status: status.map(String::from),
            err_hex: err_hex.map(String::from),
            token_name: None,
            token_symbol: None,
            token_uri: None,
            decimals: None,
            pre_sol_balances: pre_sol.map(|b| b.to_vec()),
            post_sol_balances: post_sol.map(|b| b.to_vec()),
            pre_token_balances_json: pre_token.cloned(),
            post_token_balances_json: post_token.cloned(),
            inner_instructions_json: inner_json.cloned(),
            log_messages: logs.map(|l| l.to_vec()),
            account_keys_b58: Some(all_keys.to_vec()),
            is_our_wallet: None,
        };

        // ── Extract account keys by index from the instruction's account list ──
        // For pump.fun buy/sell: accounts[0]=mint, accounts[2]=bonding_curve (usually),
        // accounts[6]=user/trader. The exact mapping varies by ix version, so we
        // extract what we can with bounds checks.
        let get_acct = |idx: usize| -> Option<String> {
            accounts.get(idx).and_then(|i| account_keys_b58.get(*i as usize).cloned())
        };

        match event_type {
            "buy" | "sell" => {
                // pump.fun buy: data = disc(8) + min_tokens_out(8) + max_sol_cost(8)
                // pump.fun sell: data = disc(8) + token_amount(8) + min_sol_out(8)
                // PumpSwap buy: data = disc(8) + base_amount_out(8) + max_quote_in(8) + optional track_volume
                // PumpSwap sell: data = disc(8) + base_amount_in(8) + min_quote_out(8)
                if data.len() >= 24 {
                    let arg0 = u64::from_le_bytes(data[8..16].try_into().ok()?);
                    let arg1 = u64::from_le_bytes(data[16..24].try_into().ok()?);
                    if venue == "pumpfun" {
                        if event_type == "buy" {
                            ev.min_amount_out = Some(arg0); // min tokens
                            ev.max_amount_in = Some(arg1); // max SOL
                            ev.fee_bps = Some(100); // 1% buy fee = 100 bps
                        } else {
                            ev.amount_in = Some(arg0); // tokens sold
                            ev.min_amount_out = Some(arg1); // min SOL out
                            // Sell fee is 0% on pump.fun (buyer pays the fee).
                            ev.fee_bps = Some(0);
                        }
                    } else {
                        // PumpSwap
                        if event_type == "buy" {
                            ev.amount_out = Some(arg0); // base_amount_out (exact-out)
                            ev.max_amount_in = Some(arg1); // max_quote_amount_in
                        } else {
                            ev.amount_in = Some(arg0); // base_amount_in (exact-in)
                            ev.min_amount_out = Some(arg1); // min_quote_amount_out
                        }
                    }
                }
                // Extract mint + trader from account indices.
                ev.mint_b58 = get_acct(0);
                ev.trader_b58 = get_acct(6);
                // For pump.fun, bonding curve is usually accounts[2].
                if venue == "pumpfun" {
                    ev.curve_account_b58 = get_acct(2);
                } else {
                    ev.pool_account_b58 = get_acct(2);
                }
            }
            "create" => {
                // pump.fun create: accounts[0]=mint, data has name/symbol/uri/etc.
                // The create ix data is complex; we extract what's safely parseable.
                ev.mint_b58 = get_acct(0);
                ev.creator_b58 = get_acct(1);
            }
            "complete" => {
                // pump.fun complete: accounts[0]=mint, accounts[2]=bonding_curve
                ev.mint_b58 = get_acct(0);
                ev.curve_account_b58 = get_acct(2);
            }
            "migrate" => {
                // pump.fun migrate: accounts[0]=mint, accounts[2]=bonding_curve
                ev.mint_b58 = get_acct(0);
                ev.curve_account_b58 = get_acct(2);
            }
            "create_pool" => {
                // PumpSwap create_pool: data = disc(8) + index(2) + base_in(8) + quote_in(8) + optional
                ev.mint_b58 = get_acct(0);
                ev.pool_account_b58 = get_acct(4);
                if data.len() >= 26 {
                    ev.initial_virtual_sol = Some(u64::from_le_bytes(data[18..26].try_into().ok()?));
                }
            }
            "deposit" | "withdraw" => {
                ev.pool_account_b58 = get_acct(2);
            }
            _ => {}
        }

        // Check if our wallet appears in the account keys.
        if let Some(our) = our_wallet {
            ev.is_our_wallet = Some(account_keys_b58.iter().any(|k| k == our));
        }

        Some(ev)
    }

    /// Count an event for the manifest statistics.
    fn count_event(&mut self, ev: &PumpEventV1) {
        match (ev.venue.as_str(), ev.event_type.as_str()) {
            ("pumpfun", "buy") => self.pump_buys += 1,
            ("pumpfun", "sell") => self.pump_sells += 1,
            ("pumpfun", "create") => self.creates += 1,
            ("pumpfun", "complete") => self.pump_completes += 1,
            ("pumpfun", "migrate") => self.migrations += 1,
            ("pumpswap", "buy") => self.pumpswap_buys += 1,
            ("pumpswap", "sell") => self.pumpswap_sells += 1,
            ("pumpswap", "create_pool") => self.pumpswap_create_pools += 1,
            ("pumpswap", "deposit") => self.pumpswap_deposits += 1,
            ("pumpswap", "withdraw") => self.pumpswap_withdraws += 1,
            _ => {}
        }
    }

    /// Number of duplicate signatures suppressed.
    pub fn duplicates(&self) -> u64 {
        self.duplicates
    }
    pub fn decode_failures(&self) -> u64 {
        self.decode_failures
    }
    pub fn unknown_events(&self) -> u64 {
        self.unknown_events
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────

/// Convert protobuf token balances to a JSON value for storage.
fn token_balances_to_json(
    balances: &[helius_laserstream::solana::storage::confirmed_block::TokenBalance],
) -> serde_json::Value {
    serde_json::json!(balances.iter().map(|tb| {
        let ui = tb.ui_token_amount.as_ref().map(|u| serde_json::json!({
            "ui_amount": u.ui_amount,
            "decimals": u.decimals,
            "amount": u.amount,
            "ui_amount_string": u.ui_amount_string,
        }));
        serde_json::json!({
            "account_index": tb.account_index,
            "mint": tb.mint,
            "owner": tb.owner,
            "program_id": tb.program_id,
            "ui_token_amount": ui,
        })
    }).collect::<Vec<_>>())
}

/// Convert protobuf inner instructions to a JSON value.
fn inner_instructions_to_json(
    inner: &[helius_laserstream::solana::storage::confirmed_block::InnerInstructions],
) -> serde_json::Value {
    serde_json::json!(inner.iter().map(|group| {
        let insts: Vec<serde_json::Value> = group.instructions.iter().map(|ii| {
            serde_json::json!({
                "program_id_index": ii.program_id_index,
                "accounts_b64": crate::encoding::b64_encode(&ii.accounts),
                "data_b64": crate::encoding::b64_encode(&ii.data),
                "stack_height": ii.stack_height,
            })
        }).collect();
        serde_json::json!({"index": group.index, "instructions": insts})
    }).collect::<Vec<_>>())
}
