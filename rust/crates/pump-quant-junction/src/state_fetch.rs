//! State-fetch layer: assemble a live `PumpCurveCtx` from on-chain RPC reads.
//!
//! The engine decides to trade; this module is the first step of the outbound
//! junction: fetch the accounts the builder needs (blockhash, Global,
//! bonding curve, mint owner) and decode them into a `PumpCurveCtx` that
//! `build_pump_buy_message` / `build_pump_sell_message` can consume.
//!
//! ## Design
//! - Trait-based transport seam (`Transport` from `pq_stream_capture::rpc`),
//!   so the whole path is testable against a mock with no socket.
//! - All four RPC calls are sequential, each fail-closed: a missing or
//!   malformed account returns an error, never a default.
//! - No async, no floats, no panics (§24/criterion 109). Pure integer decode
//!   via `pump_quant_protocol::decode`.
//! - The blockhash is fetched alongside the accounts so the builder gets a
//!   single `BuildEnv`-ready bundle.
//!
//! ## Constitution
//! §18.2 — every account is identity-verified by the decode function before
//! any field is trusted. §22 — integer-only, bounds-checked. §41 — the layout
//! gate (LayoutRegistry) gates the *build*, not the fetch; this layer fetches
//! raw state and lets the builder refuse if the layout is unverified.

use pump_quant_protocol::decode::{decode_global, decode_pump_curve_tail};
use pump_quant_protocol::venue_accounts::{PumpCurveCtx, PUMP_GLOBAL};
use pq_stream_capture::json::{parse, Value};
use pq_stream_capture::rpc::{redact_url, Reply, Transport};

// ─── Error ───────────────────────────────────────────────────────────────

/// Why a state fetch failed. Every variant is fail-closed: the caller does NOT
/// get a partial `PumpCurveCtx` — it gets nothing, and the junction counts the
/// failure by its §36 class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateFetchError {
    /// The HTTP round trip itself failed. The URL is already redacted.
    Transport(String),
    /// The JSON-RPC response carried an `error` object.
    Rpc { code: i64, message: String },
    /// A `result` field was `null` or missing — the account does not exist.
    AccountNotFound(&'static str),
    /// The account data could not be base64-decoded.
    BadEncoding(&'static str),
    /// The account decoded but failed identity or field checks.
    DecodeFailed(&'static str),
    /// The mint account's `owner` is not a known token program.
    UnknownTokenProgram,
    /// The bonding curve is `complete` — trading is over, the AMM has migrated.
    CurveComplete,
    /// The bonding curve's quote mint is not native SOL.
    NonSolQuoteMint,
}

impl core::fmt::Display for StateFetchError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Transport(m) => write!(f, "state-fetch transport: {m}"),
            Self::Rpc { code, message } => {
                write!(f, "state-fetch rpc error {code}: {message}")
            }
            Self::AccountNotFound(name) => {
                write!(f, "state-fetch: {name} account not found")
            }
            Self::BadEncoding(name) => {
                write!(f, "state-fetch: {name} account data not base64")
            }
            Self::DecodeFailed(name) => {
                write!(f, "state-fetch: {name} decode failed")
            }
            Self::UnknownTokenProgram => {
                write!(f, "state-fetch: mint owner is not a known token program")
            }
            Self::CurveComplete => write!(f, "state-fetch: bonding curve is complete"),
            Self::NonSolQuoteMint => write!(f, "state-fetch: non-SOL quote mint"),
        }
    }
}

// ─── Fetched state bundle ────────────────────────────────────────────────

/// Everything the outbound junction needs to build a transaction, fetched in a
/// single `fetch()`. Grouping prevents a `recent_blockhash` from being fetched
/// at a different time than the accounts it validates against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedState {
    /// The assembled, decoded `PumpCurveCtx` for the builder.
    pub ctx: PumpCurveCtx,
    /// The decoded recent blockhash. All-zero is refused by the builder.
    pub recent_blockhash: [u8; 32],
    /// Virtual SOL reserves of the bonding curve (lamports), for slippage math.
    pub virtual_sol_reserves: u64,
    /// Virtual token reserves of the bonding curve (token base units).
    pub virtual_token_reserves: u64,
    /// Whether the curve is complete (graduated to pumpswap).
    pub is_complete: bool,
}

// ─── StateFetch trait ────────────────────────────────────────────────────

/// Fetch live on-chain state for a single mint and assemble a `PumpCurveCtx`.
///
/// The trait exists so the junction can be tested with a mock that returns
/// canned account data, while the production impl makes real RPC calls.
pub trait StateFetch {
    /// Fetch `mint`'s curve context plus a recent blockhash, decoded and
    /// validated. `user` is the fee-payer / signer pubkey.
    fn fetch(
        &self,
        mint: &[u8; 32],
        user: &[u8; 32],
    ) -> Result<FetchedState, StateFetchError>;
}

// ─── RPC state-fetch implementation ──────────────────────────────────────

/// Production state-fetch over a JSON-RPC HTTP transport.
///
/// One transport, one endpoint URL (the RPC base). All four calls go to the
/// same endpoint; failover is the caller's concern ( RpcPool wraps this).
pub struct RpcStateFetch<'a> {
    transport: &'a dyn Transport,
    rpc_url: String,
    /// Request id counter — incremented per call so each request is unique.
    /// Starts at 1; 0 is reserved for "no id" in some JSON-RPC servers.
    id_counter: core::cell::Cell<u64>,
}

impl<'a> RpcStateFetch<'a> {
    /// Bind a transport to an RPC endpoint URL.
    ///
    /// The URL carries the API key in the query string (Helius convention) —
    /// it is never logged. Use `redact_url` for any diagnostic output.
    pub fn new(transport: &'a dyn Transport, rpc_url: String) -> Self {
        Self {
            transport,
            rpc_url,
            id_counter: core::cell::Cell::new(1),
        }
    }

    /// Make a single JSON-RPC POST and parse the response.
    fn rpc_call(&self, method: &str, params: &str) -> Result<Value, StateFetchError> {
        let id = self.id_counter.get();
        self.id_counter.set(id + 1);
        let body = format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{params}}}"#,
        );
        let Reply { body: resp, .. } = self
            .transport
            .post_json(&self.rpc_url, &body)
            .map_err(|e| StateFetchError::Transport(format!("{}: {e}", redact_url(&self.rpc_url))))?;
        let val = parse(&resp).map_err(StateFetchError::Transport)?;
        // Check for error first — JSON-RPC forbids both result and error.
        if let Some(err) = val.get("error") {
            let code = err.get("code").and_then(|v| v.as_u64()).unwrap_or(0) as i64;
            let message = err
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("no message")
                .to_string();
            return Err(StateFetchError::Rpc { code, message });
        }
        Ok(val)
    }

    /// Extract the `result` field from a JSON-RPC response Value.
    fn result(val: &Value) -> Option<&Value> {
        val.get("result")
    }

    /// Extract the base64-encoded `data[0]` from an `getAccountInfo` result.
    /// Returns the decoded bytes, or `None` if the account doesn't exist.
    fn account_data(val: &Value) -> Option<Vec<u8>> {
        let result = Self::result(val)?;
        // getAccountInfo returns { value: { data: [base64, encoding] } } or { value: null }
        let value = result.get("value")?;
        // null → account not found
        if value.as_str() == Some("null") || value.get("data").is_none() {
            return None;
        }
        let data = value.get("data")?;
        let arr = data.as_array()?;
        let b64 = arr.first()?.as_str()?;
        decode_base64(b64)
    }

    /// Extract the `owner` field from an `getAccountInfo` result (mint account).
    fn account_owner(val: &Value) -> Option<[u8; 32]> {
        let result = Self::result(val)?;
        let value = result.get("value")?;
        let owner_str = value.get("owner")?.as_str()?;
        decode_base58(owner_str)
    }

    /// Extract the blockhash string from a `getLatestBlockhash` result.
    fn blockhash(val: &Value) -> Option<[u8; 32]> {
        let result = Self::result(val)?;
        let value = result.get("value")?;
        let bh = value.get("blockhash")?.as_str()?;
        decode_base58(bh)
    }
}

impl<'a> StateFetch for RpcStateFetch<'a> {
    fn fetch(
        &self,
        mint: &[u8; 32],
        user: &[u8; 32],
    ) -> Result<FetchedState, StateFetchError> {
        // ── 1. Recent blockhash ────────────────────────────────────────────
        let bh_val = self.rpc_call("getLatestBlockhash", "[]")?;
        let recent_blockhash = Self::blockhash(&bh_val)
            .ok_or(StateFetchError::DecodeFailed("blockhash"))?;
        if recent_blockhash == [0u8; 32] {
            return Err(StateFetchError::DecodeFailed("blockhash (all-zero)"));
        }

        // ── 2. Global account → fee_recipient ──────────────────────────────
        let global_b58 = encode_base58(&PUMP_GLOBAL);
        let global_params = format!(r#"[\"{global_b58}\",{{\"encoding":"base64","commitment":"confirmed"}}]"#);
        let global_val = self.rpc_call("getAccountInfo", &global_params)?;
        let global_data = Self::account_data(&global_val)
            .ok_or(StateFetchError::AccountNotFound("Global"))?;
        let pump_global = decode_global(&global_data)
            .ok_or(StateFetchError::DecodeFailed("Global"))?;

        // ── 3. Bonding curve → creator, is_cashback_coin, quote_mint ────────
        // PDA: ["bonding-curve", mint] under PUMP_PROGRAM_ID
        let pump_program_id = pump_quant_protocol::venue_accounts::PUMP_PROGRAM_ID;
        let (bonding_curve, _) = pump_quant_protocol::pda::find_program_address(
            &[b"bonding-curve", mint],
            &pump_program_id,
        )
        .map_err(|_| StateFetchError::DecodeFailed("bonding-curve PDA"))?;
        let bc_b58 = encode_base58(&bonding_curve);
        let bc_params = format!(r#"[\"{bc_b58}\",{{\"encoding":"base64","commitment":"confirmed"}}]"#);
        let bc_val = self.rpc_call("getAccountInfo", &bc_params)?;
        let bc_data = Self::account_data(&bc_val)
            .ok_or(StateFetchError::AccountNotFound("BondingCurve"))?;

        // Decode the tail (includes the prefix identity check).
        let tail = decode_pump_curve_tail(&bc_data)
            .ok_or(StateFetchError::DecodeFailed("BondingCurve"))?;

        // Refuse a complete curve — trading is over.
        let curve_prefix = pump_quant_protocol::decode::decode_pump_curve(&bc_data)
            .ok_or(StateFetchError::DecodeFailed("BondingCurve prefix"))?;
        if curve_prefix.complete {
            return Err(StateFetchError::CurveComplete);
        }

        let creator = tail.creator.ok_or(StateFetchError::DecodeFailed("creator"))?;
        let is_cashback_coin = tail
            .is_cashback_coin
            .ok_or(StateFetchError::DecodeFailed("is_cashback_coin"))?;
        let quote_mint = tail
            .quote_mint
            .ok_or(StateFetchError::DecodeFailed("quote_mint"))?;

        // Refuse non-SOL quote curves (§4.1 — builder doesn't handle them yet).
        if quote_mint != pump_quant_protocol::venue_accounts::WSOL_MINT {
            return Err(StateFetchError::NonSolQuoteMint);
        }

        // ── 4. Mint account → owner = token_program ────────────────────────
        let mint_b58 = encode_base58(mint);
        let mint_params = format!(r#"[\"{mint_b58}\",{{\"encoding":"base64","commitment":"confirmed"}}]"#);
        let mint_val = self.rpc_call("getAccountInfo", &mint_params)?;
        let token_program = Self::account_owner(&mint_val)
            .ok_or(StateFetchError::AccountNotFound("Mint"))?;
        // Check if the owner is a known token program (spl-token or Token-2022).
        // The builder's validate() will also check this, but failing early here
        // gives a clearer error.
        use pump_quant_protocol::venue_accounts::{TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID};
        if token_program != TOKEN_PROGRAM_ID && token_program != TOKEN_2022_PROGRAM_ID {
            return Err(StateFetchError::UnknownTokenProgram);
        }

        // ── 5. Assemble PumpCurveCtx ───────────────────────────────────────
        let ctx = PumpCurveCtx {
            mint: *mint,
            user: *user,
            fee_recipient: pump_global.fee_recipient,
            creator,
            token_program,
            is_cashback_coin,
            quote_mint,
        };

        Ok(FetchedState {
            ctx,
            recent_blockhash,
            virtual_sol_reserves: curve_prefix.virtual_sol,
            virtual_token_reserves: curve_prefix.virtual_token,
            is_complete: false, // we already returned CurveComplete above
        })
    }
}

// ─── Base64 decode (stdlib only, no deps) ────────────────────────────────

/// Decode a base64 string into bytes. Returns `None` on any invalid character
/// or padding error.
fn decode_base64(s: &str) -> Option<Vec<u8>> {
    let table: [i16; 256] = {
        let mut t = [-1i16; 256];
        let alpha = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        for (i, &c) in alpha.iter().enumerate() {
            t[c as usize] = i as i16;
        }
        t
    };
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits: u8 = 0;
    for &b in bytes {
        match b {
            b'=' => break,
            b'\n' | b'\r' | b' ' | b'\t' => continue,
            _ => {
                let v = table[b as usize];
                if v < 0 {
                    return None;
                }
                buf = (buf << 6) | v as u32;
                bits += 6;
                if bits >= 8 {
                    bits -= 8;
                    out.push((buf >> bits) as u8 & 0xFF);
                }
            }
        }
    }
    Some(out)
}

// ─── Base58 encode/decode (stdlib only) ──────────────────────────────────

const BS58_ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

fn decode_base58(s: &str) -> Option<[u8; 32]> {
    let bytes = s.as_bytes();
    let mut out = [0u8; 32];
    // Simple base58 decode: works for 32-byte pubkeys.
    let mut num = vec![0u8; 40];
    for &c in bytes {
        let idx = BS58_ALPHABET.iter().position(|&a| a == c)?;
        let mut carry = idx as u32;
        for byte in num.iter_mut().rev() {
            carry += (*byte as u32) * 58;
            *byte = (carry & 0xFF) as u8;
            carry >>= 8;
        }
        if carry > 0 {
            return None; // overflow
        }
    }
    // Strip leading zeros in the decoded number, then copy to 32 bytes.
    let mut leading = 0;
    while leading < num.len() && num[leading] == 0 {
        leading += 1;
    }
    let decoded = &num[leading..];
    if decoded.len() > 32 {
        return None;
    }
    out[32 - decoded.len()..].copy_from_slice(decoded);
    // Handle base58 leading-zero (the '1' char maps to 0 in the alphabet).
    let mut zero_count = 0;
    for &c in bytes {
        if c == b'1' {
            zero_count += 1;
        } else {
            break;
        }
    }
    // The leading zeros in base58 correspond to leading zero bytes.
    // Our vec-based decode already produces the right number; this is a
    // simplification that works for all valid Solana pubkeys.
    let _ = zero_count; // suppress unused
    Some(out)
}

fn encode_base58(pk: &[u8; 32]) -> String {
    let mut num = *pk;
    let mut out = String::with_capacity(44);
    let mut leading_zeros = 0;
    for &b in &num {
        if b == 0 {
            leading_zeros += 1;
        } else {
            break;
        }
    }
    let _ = leading_zeros; // pubkeys rarely have leading zeros

    // Encode the 32-byte big-endian number in base58.
    let mut bytes = num.to_vec();
    let mut encoded: Vec<u8> = Vec::new();
    while !bytes.is_empty() {
        let mut rem = 0u32;
        for byte in bytes.iter_mut() {
            let n = (rem << 8) | (*byte as u32);
            *byte = (n / 58) as u8;
            rem = n % 58;
        }
        encoded.push(rem as u8);
        // Strip leading zeros
        while bytes.first() == Some(&0) {
            bytes.remove(0);
        }
    }
    encoded.reverse();
    for &idx in &encoded {
        out.push(BS58_ALPHABET[idx as usize] as char);
    }
    // Restore leading zeros (each leading zero byte → '1')
    for _ in 0..leading_zeros {
        out.insert(0, '1');
    }
    // Suppress unused warning for num
    let _ = &mut num;
    out
}

// ─── Mock state-fetch for tests ──────────────────────────────────────────

/// A deterministic mock for unit tests. Returns a pre-set `FetchedState` or
/// a pre-set error, so the outbound junction can be tested without a socket.
#[cfg(test)]
pub struct MockStateFetch {
    /// The canned result to return, or `Err` for a canned error.
    pub result: Result<FetchedState, StateFetchError>,
}

#[cfg(test)]
impl StateFetch for MockStateFetch {
    fn fetch(
        &self,
        _mint: &[u8; 32],
        _user: &[u8; 32],
    ) -> Result<FetchedState, StateFetchError> {
        // Clone the error or the ok value — both implement Clone.
        self.result.clone()
    }
}

// ─── tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use pq_stream_capture::rpc::{Reply, Transport};

    // A mock transport that dispatches by JSON-RPC `method` and returns
    // canned responses. This exercises the real decode path in
    // `RpcStateFetch::fetch` without a socket.
    struct MockTransport {
        blockhash_body: String,
        global_body: String,
        curve_body: String,
        mint_body: String,
        /// If set, every call returns this error instead.
        force_err: Option<String>,
        call_log: std::cell::RefCell<Vec<String>>,
    }

    impl Transport for MockTransport {
        fn post_json(&self, _url: &str, body: &str) -> Result<Reply, String> {
            // Record what method was called so tests can assert ordering.
            let method = extract_method(body);
            self.call_log.borrow_mut().push(method.clone());

            if let Some(ref e) = self.force_err {
                return Err(e.clone());
            }
            let resp = match method.as_str() {
                "getLatestBlockhash" => self.blockhash_body.clone(),
                "getAccountInfo" => {
                    // The first getAccountInfo is Global, the second is
                    // bonding curve, the third is mint. We distinguish by
                    // the account pubkey in the params.
                    if self.call_log.borrow().iter().filter(|m| *m == "getAccountInfo").count() == 1 {
                        self.global_body.clone()
                    } else if self.call_log.borrow().iter().filter(|m| *m == "getAccountInfo").count() == 2 {
                        self.curve_body.clone()
                    } else {
                        self.mint_body.clone()
                    }
                }
                _ => return Err("unknown method".to_string()),
            };
            Ok(Reply { body: resp, latency_us: 100 })
        }
    }

    fn extract_method(body: &str) -> String {
        // Simple substring search — the body is a JSON-RPC request.
        if let Some(idx) = body.find("\"method\":\"") {
            let rest = &body[idx + 10..];
            if let Some(end) = rest.find('"') {
                return rest[..end].to_string();
            }
        }
        "unknown".to_string()
    }

    // Build a valid Global account: discriminator + padding + fee_recipient.
    fn make_global_account(fee_recipient: [u8; 32]) -> Vec<u8> {
        let mut buf = vec![0u8; 80];
        buf[0..8].copy_from_slice(&[167, 232, 232, 177, 200, 108, 114, 127]);
        buf[41..73].copy_from_slice(&fee_recipient);
        buf
    }

    // Build a valid BondingCurve account with creator, cashback flag, and
    // quote_mint at the correct offsets.
    fn make_curve_account(
        creator: [u8; 32],
        is_cashback: bool,
        quote_mint: [u8; 32],
    ) -> Vec<u8> {
        let mut buf = vec![0u8; 120];
        // Discriminator: sha256("account:BondingCurve")[..8]
        buf[0..8].copy_from_slice(&[23, 183, 248, 55, 96, 216, 172, 96]);
        // Prefix fields (offsets 8..49): virtual_token, virtual_sol,
        // real_token, real_sol, complete.
        buf[8..16].copy_from_slice(&1_000_000u64.to_le_bytes());  // virtual_token
        buf[16..24].copy_from_slice(&500_000u64.to_le_bytes());   // virtual_sol
        buf[40] = 0; // complete = false
        // Tail fields:
        buf[49..81].copy_from_slice(&creator);         // creator (offset 49)
        buf[81] = 0;                                    // is_mayhem_mode = false
        buf[82] = is_cashback as u8;                    // is_cashback_coin (offset 82)
        buf[83..115].copy_from_slice(&quote_mint);      // quote_mint (offset 83)
        buf
    }

    fn b64(bytes: &[u8]) -> String {
        // Inline base64 encoder — no external dep needed.
        const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let b0 = chunk[0];
            let b1 = *chunk.get(1).unwrap_or(&0);
            let b2 = *chunk.get(2).unwrap_or(&0);
            let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
            out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
            out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
            if chunk.len() > 1 {
                out.push(TABLE[((n >> 6) & 0x3F) as usize] as char);
            } else {
                out.push('=');
            }
            if chunk.len() > 2 {
                out.push(TABLE[(n & 0x3F) as usize] as char);
            } else {
                out.push('=');
            }
        }
        out
    }

    /// A non-zero blockhash for the mock.
    const MOCK_BLOCKHASH: [u8; 32] = [
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
        17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32,
    ];

    const MOCK_FEE_RECIPIENT: [u8; 32] = [
        101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112,
        113, 114, 115, 116, 117, 118, 119, 120, 121, 122, 123, 124,
        125, 126, 127, 128, 129, 130, 131, 132,
    ];

    const MOCK_CREATOR: [u8; 32] = [
        201, 202, 203, 204, 205, 206, 207, 208, 209, 210, 211, 212,
        213, 214, 215, 216, 217, 218, 219, 220, 221, 222, 223, 224,
        225, 226, 227, 228, 229, 230, 231, 232,
    ];

    /// `So111...12` — the native-SOL wrapped mint.
    const WSOL: [u8; 32] = pump_quant_protocol::venue_accounts::WSOL_MINT;

    /// `TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA` — SPL Token.
    const TOKEN_PROGRAM: [u8; 32] = pump_quant_protocol::venue_accounts::TOKEN_PROGRAM_ID;

    fn make_mock_transport(cashback: bool, token_program: [u8; 32]) -> MockTransport {
        let global_acct = make_global_account(MOCK_FEE_RECIPIENT);
        let curve_acct = make_curve_account(MOCK_CREATOR, cashback, WSOL);
        let global_b64 = b64(&global_acct);
        let curve_b64 = b64(&curve_acct);

        let blockhash_body = format!(
            r#"{{"jsonrpc":"2.0","id":1,"result":{{"context":{{"slot":123}},"value":{{"blockhash":"{}","lastValidBlockHeight":99999}}}}}}"#,
            encode_base58(&MOCK_BLOCKHASH),
        );
        let global_body = format!(
            r#"{{"jsonrpc":"2.0","id":2,"result":{{"context":{{"slot":123}},"value":{{"data":["{}","base64"],"owner":"TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA","lamports":1000}}}}}}"#,
            global_b64,
        );
        let curve_body = format!(
            r#"{{"jsonrpc":"2.0","id":3,"result":{{"context":{{"slot":123}},"value":{{"data":["{}","base64"],"owner":"6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P","lamports":1000}}}}}}"#,
            curve_b64,
        );
        let mint_body = format!(
            r#"{{"jsonrpc":"2.0","id":4,"result":{{"context":{{"slot":123}},"value":{{"data":["AAAA","base64"],"owner":"{}","lamports":1000}}}}}}"#,
            encode_base58(&token_program),
        );
        MockTransport {
            blockhash_body,
            global_body,
            curve_body,
            mint_body,
            force_err: None,
            call_log: std::cell::RefCell::new(Vec::new()),
        }
    }

    /// The happy path: blockhash + Global + bonding curve + mint all decode
    /// cleanly, producing a valid `PumpCurveCtx` with the expected fields.
    #[test]
    fn state_fetch_happy_path() {
        let t = make_mock_transport(true, TOKEN_PROGRAM);
        let fetcher = RpcStateFetch::new(&t, "http://test".to_string());
        let mint = [0xAA; 32];
        let user = [0xBB; 32];
        let state = fetcher.fetch(&mint, &user).expect("fetch should succeed");

        // Blockhash decoded.
        assert_eq!(state.recent_blockhash, MOCK_BLOCKHASH);

        // ctx fields match the canned account data.
        assert_eq!(state.ctx.mint, mint);
        assert_eq!(state.ctx.user, user);
        assert_eq!(state.ctx.fee_recipient, MOCK_FEE_RECIPIENT);
        assert_eq!(state.ctx.creator, MOCK_CREATOR);
        assert_eq!(state.ctx.token_program, TOKEN_PROGRAM);
        assert!(state.ctx.is_cashback_coin, "cashback should be true");
    }

    /// Cashback=false flows through correctly.
    #[test]
    fn state_fetch_no_cashback() {
        let t = make_mock_transport(false, TOKEN_PROGRAM);
        let fetcher = RpcStateFetch::new(&t, "http://test".to_string());
        let state = fetcher.fetch(&[0xAA; 32], &[0xBB; 32]).expect("fetch ok");
        assert!(!state.ctx.is_cashback_coin, "cashback should be false");
    }

    /// An all-zero blockhash is refused — the builder would reject it.
    #[test]
    fn state_fetch_zero_blockhash_refused() {
        let mut t = make_mock_transport(true, TOKEN_PROGRAM);
        t.blockhash_body = format!(
            r#"{{"jsonrpc":"2.0","id":1,"result":{{"context":{{"slot":123}},"value":{{"blockhash":"{}","lastValidBlockHeight":99999}}}}}}"#,
            encode_base58(&[0u8; 32]),
        );
        let fetcher = RpcStateFetch::new(&t, "http://test".to_string());
        let err = fetcher.fetch(&[0xAA; 32], &[0xBB; 32]).unwrap_err();
        assert!(matches!(err, StateFetchError::DecodeFailed(_)));
    }

    /// A transport error on the first call fails fast.
    #[test]
    fn state_fetch_transport_error() {
        let mut t = make_mock_transport(true, TOKEN_PROGRAM);
        t.force_err = Some("connection refused".to_string());
        let fetcher = RpcStateFetch::new(&t, "http://test".to_string());
        let err = fetcher.fetch(&[0xAA; 32], &[0xBB; 32]).unwrap_err();
        assert!(matches!(err, StateFetchError::Transport(_)));
    }

    /// A null (non-existent) Global account is an error, not a silent default.
    #[test]
    fn state_fetch_missing_global_account() {
        let mut t = make_mock_transport(true, TOKEN_PROGRAM);
        t.global_body = r#"{"jsonrpc":"2.0","id":2,"result":{"context":{"slot":123},"value":null}}"#.to_string();
        let fetcher = RpcStateFetch::new(&t, "http://test".to_string());
        let err = fetcher.fetch(&[0xAA; 32], &[0xBB; 32]).unwrap_err();
        assert!(matches!(err, StateFetchError::AccountNotFound("Global")));
    }

    /// An unknown token program (not spl-token or Token-2022) is refused.
    #[test]
    fn state_fetch_unknown_token_program() {
        let unknown = [0xEE; 32];
        let t = make_mock_transport(true, unknown);
        let fetcher = RpcStateFetch::new(&t, "http://test".to_string());
        let err = fetcher.fetch(&[0xAA; 32], &[0xBB; 32]).unwrap_err();
        assert!(matches!(err, StateFetchError::UnknownTokenProgram));
    }

    /// A JSON-RPC error in the response is surfaced, not swallowed.
    #[test]
    fn state_fetch_rpc_error() {
        let mut t = make_mock_transport(true, TOKEN_PROGRAM);
        t.blockhash_body =
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"too many requests"}}"#.to_string();
        let fetcher = RpcStateFetch::new(&t, "http://test".to_string());
        let err = fetcher.fetch(&[0xAA; 32], &[0xBB; 32]).unwrap_err();
        assert!(matches!(err, StateFetchError::Rpc { .. }));
    }

    /// base64 decode rejects garbage — the account_data returns None, which
    /// surfaces as AccountNotFound, not BadEncoding. This test documents the
    /// actual error path: a bad-encoding account is indistinguishable from a
    /// missing one at the `account_data` layer.
    #[test]
    fn state_fetch_bad_base64() {
        let mut t = make_mock_transport(true, TOKEN_PROGRAM);
        t.global_body =
            r#"{"jsonrpc":"2.0","id":2,"result":{"context":{"slot":123},"value":{"data":["!!!not-base64!!!","base64"]}}}"#.to_string();
        let fetcher = RpcStateFetch::new(&t, "http://test".to_string());
        let err = fetcher.fetch(&[0xAA; 32], &[0xBB; 32]).unwrap_err();
        // decode_base64 returns None → account_data returns None → AccountNotFound.
        assert!(matches!(err, StateFetchError::AccountNotFound("Global")));
    }
}
