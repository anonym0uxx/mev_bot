//! Launch / creator bootstrap from authenticated historical RPC (Helius `getTransactionsForAddress`).
//!
//! WHAT IT ESTABLISHES, AND HOW (each fact is decoded, never inferred from ordering):
//! * **Launch** = a SUCCESSFUL transaction containing a pump.fun `create`/`create_v2` instruction
//!   (program id + Anchor discriminator checked, outer OR inner) whose instruction account `[0]` is
//!   the queried mint. The oldest returned signature is NOT assumed to be the creation: a mint's
//!   history can start with an ATA creation, a transfer, or a router swap. No create instruction for
//!   this mint => `CreateNotFound`, never a launch time.
//! * **Creator** = the trained definition: the transaction's fee payer / first signer (account key 0),
//!   which is what the launch table the corpus was built from records
//!   (`renormalize_raw.py`: `creator = keys[0]`). On every verified create we also require that key 0
//!   appear in the create instruction's `user` slot (account index 5 in `create_v2`); a mismatch is a
//!   named `CreatorAmbiguous`, not a silent pick.
//! * **Time**: three distinct clocks are kept. `block_time_s` (chain), `source_position`
//!   (slot, transaction index — the order the source returned it in) and `retrieved_unix_ms` (when we
//!   fetched it). Nothing fetched today is presented as available to a decision before
//!   `retrieved_unix_ms`.
//!
//! `creator_past_launches` (trained definition, `build_c9_enrichment_full.prior_launches`): the number of
//! launches IN THE LAUNCH TABLE by the same creator strictly before this mint's own launch time,
//! deduplicated by mint. The table is a capture window, not a lifetime on-chain count. This module
//! therefore reports a creator's prior launches only with an explicit [`CoverageWindow`]; a creator
//! history that was not paginated to exhaustion over that window is `Incomplete`, never a number, and an
//! empty page is never a confident zero.

#![forbid(unsafe_code)]

use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// pump.fun bonding-curve program.
pub const PUMP_FUN: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
/// `sha256("global:create")[..8]`.
pub const CREATE_DISC: [u8; 8] = [24, 30, 200, 40, 5, 28, 7, 119];
/// `sha256("global:create_v2")[..8]`.
pub const CREATE_V2_DISC: [u8; 8] = [214, 144, 76, 236, 95, 139, 49, 180];
/// `create_v2` instruction account index of `user` (the paying creator). Verified on 37/37 real
/// create_v2 transactions from the 2026-09-09 capture: key 0 sits here.
pub const CREATE_V2_USER_IX: usize = 5;
/// Full-detail page size Helius documents for `getTransactionsForAddress`.
pub const PAGE_LIMIT_FULL: u32 = 100;

/// One instruction (outer or CPI) with resolved account keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ix {
    pub program: [u8; 32],
    pub accounts: Vec<[u8; 32]>,
    pub data: Vec<u8>,
    pub inner: bool,
}

/// A fully decoded transaction plus where/when the SOURCE placed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxRecord {
    pub signature: String,
    pub slot: u64,
    pub tx_index: Option<u64>,
    pub block_time_s: Option<i64>,
    pub succeeded: bool,
    pub keys: Vec<[u8; 32]>,
    pub ixs: Vec<Ix>,
}

/// Why a response could not be decoded. Never defaulted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    Missing(&'static str),
    BadBase58(&'static str),
    IndexOutOfRange,
}

fn pk(s: &str) -> Option<[u8; 32]> {
    let v = bs58::decode(s).into_vec().ok()?;
    <[u8; 32]>::try_from(v.as_slice()).ok()
}

/// Base58 rendering of a key (public data; used in evidence records).
#[must_use]
pub fn b58(k: &[u8; 32]) -> String {
    bs58::encode(k).into_string()
}

/// Decode one `transactionDetails:"full", encoding:"json"` entry (Helius / standard getTransaction
/// shape): `transaction.message.accountKeys` (+ `meta.loadedAddresses.writable/readonly`), outer
/// `instructions[{programIdIndex, accounts, data(base58)}]`, `meta.innerInstructions[{instructions}]`.
pub fn parse_full_tx(e: &Value) -> Result<TxRecord, ParseError> {
    let tx = e
        .get("transaction")
        .ok_or(ParseError::Missing("transaction"))?;
    let meta = e.get("meta").ok_or(ParseError::Missing("meta"))?;
    let msg = tx.get("message").ok_or(ParseError::Missing("message"))?;
    let signature = tx
        .get("signatures")
        .and_then(|s| s.get(0))
        .and_then(Value::as_str)
        .ok_or(ParseError::Missing("signatures"))?
        .to_string();
    let slot = e
        .get("slot")
        .and_then(Value::as_u64)
        .ok_or(ParseError::Missing("slot"))?;
    let mut keys = Vec::new();
    let static_keys = msg
        .get("accountKeys")
        .and_then(Value::as_array)
        .ok_or(ParseError::Missing("accountKeys"))?;
    let loaded = meta.get("loadedAddresses");
    let lw = loaded
        .and_then(|l| l.get("writable"))
        .and_then(Value::as_array);
    let lr = loaded
        .and_then(|l| l.get("readonly"))
        .and_then(Value::as_array);
    for k in static_keys
        .iter()
        .chain(lw.into_iter().flatten())
        .chain(lr.into_iter().flatten())
    {
        let s = k
            .as_str()
            .or_else(|| k.get("pubkey").and_then(Value::as_str))
            .ok_or(ParseError::Missing("accountKey"))?;
        keys.push(pk(s).ok_or(ParseError::BadBase58("accountKey"))?);
    }
    let conv = |ix: &Value, inner: bool| -> Result<Ix, ParseError> {
        let pi = ix
            .get("programIdIndex")
            .and_then(Value::as_u64)
            .ok_or(ParseError::Missing("programIdIndex"))?;
        let pi = usize::try_from(pi).map_err(|_| ParseError::IndexOutOfRange)?;
        let program = *keys.get(pi).ok_or(ParseError::IndexOutOfRange)?;
        let mut accounts = Vec::new();
        for a in ix
            .get("accounts")
            .and_then(Value::as_array)
            .ok_or(ParseError::Missing("accounts"))?
        {
            let i = a.as_u64().ok_or(ParseError::Missing("account index"))?;
            let i = usize::try_from(i).map_err(|_| ParseError::IndexOutOfRange)?;
            accounts.push(*keys.get(i).ok_or(ParseError::IndexOutOfRange)?);
        }
        let d = ix
            .get("data")
            .and_then(Value::as_str)
            .ok_or(ParseError::Missing("data"))?;
        let data = bs58::decode(d)
            .into_vec()
            .map_err(|_| ParseError::BadBase58("data"))?;
        Ok(Ix {
            program,
            accounts,
            data,
            inner,
        })
    };
    let mut ixs = Vec::new();
    for ix in msg
        .get("instructions")
        .and_then(Value::as_array)
        .ok_or(ParseError::Missing("instructions"))?
    {
        ixs.push(conv(ix, false)?);
    }
    for g in meta
        .get("innerInstructions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for ix in g
            .get("instructions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            ixs.push(conv(ix, true)?);
        }
    }
    Ok(TxRecord {
        signature,
        slot,
        tx_index: e.get("transactionIndex").and_then(Value::as_u64),
        block_time_s: e.get("blockTime").and_then(Value::as_i64),
        succeeded: meta.get("err").is_some_and(Value::is_null),
        keys,
        ixs,
    })
}

/// A verified launch, with every clock kept apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchEvidence {
    pub mint: [u8; 32],
    pub creator: [u8; 32],
    pub signature: String,
    pub slot: u64,
    pub tx_index: Option<u64>,
    /// Chain time (seconds). `None` when the source omitted it: a launch with no chain time is
    /// still a verified create but cannot be ordered against a decision clock.
    pub block_time_s: Option<i64>,
    /// `create` or `create_v2`.
    pub instruction: &'static str,
    /// Whether the create was a CPI (e.g. launched through a launchpad wrapper).
    pub inner: bool,
    /// When WE retrieved it (unix ms). Facts fetched now are available from now, not before.
    pub retrieved_unix_ms: i64,
    /// Which source produced it (e.g. `helius:getTransactionsForAddress`, `fixture:capture`).
    pub source: String,
}

/// Why no launch is established. Each is named and counted; none becomes a launch time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchOutcome {
    Verified(LaunchEvidence),
    /// History was walked to its documented end and no successful create for this mint exists in it.
    CreateNotFound {
        pages: u32,
        txs: u64,
    },
    /// A create instruction for this mint exists but the transaction failed.
    CreateFailedTx {
        signature: String,
    },
    /// key 0 is not the create instruction's `user`: the trained creator is not identifiable.
    CreatorAmbiguous {
        signature: String,
    },
    /// The walk stopped before the end of history (budget, error, truncated page).
    HistoryIncomplete {
        pages: u32,
        reason: String,
    },
    /// The response could not be decoded.
    Malformed(String),
}

impl fmt::Display for LaunchOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            LaunchOutcome::Verified(_) => "verified",
            LaunchOutcome::CreateNotFound { .. } => "create_not_found",
            LaunchOutcome::CreateFailedTx { .. } => "create_failed_tx",
            LaunchOutcome::CreatorAmbiguous { .. } => "creator_ambiguous",
            LaunchOutcome::HistoryIncomplete { .. } => "history_incomplete",
            LaunchOutcome::Malformed(_) => "malformed",
        };
        f.write_str(s)
    }
}

/// `(creator, instruction name, is_cpi)` for a verified create.
pub type CreateHit = ([u8; 32], &'static str, bool);

/// Inspect one decoded transaction for a pump.fun create of `mint`.
/// `Ok(None)` = this transaction is not the mint's creation (keep walking).
pub fn create_in_tx(tx: &TxRecord, mint: &[u8; 32]) -> Option<Result<CreateHit, LaunchOutcome>> {
    let pf = pk(PUMP_FUN)?;
    for ix in &tx.ixs {
        if ix.program != pf || ix.data.len() < 8 {
            continue;
        }
        let kind = match <[u8; 8]>::try_from(&ix.data[..8]).ok()? {
            CREATE_DISC => "create",
            CREATE_V2_DISC => "create_v2",
            _ => continue,
        };
        if ix.accounts.first() != Some(mint) {
            continue;
        }
        if !tx.succeeded {
            return Some(Err(LaunchOutcome::CreateFailedTx {
                signature: tx.signature.clone(),
            }));
        }
        let key0 = *tx.keys.first()?;
        // Trained creator = key 0. Require it to be the instruction's paying user where the layout is
        // verified (create_v2); legacy `create` has no verified index here, so it must at least sign
        // as an account of the instruction.
        let consistent = match kind {
            "create_v2" => ix.accounts.get(CREATE_V2_USER_IX) == Some(&key0),
            _ => ix.accounts.contains(&key0),
        };
        if !consistent {
            return Some(Err(LaunchOutcome::CreatorAmbiguous {
                signature: tx.signature.clone(),
            }));
        }
        return Some(Ok((key0, kind, ix.inner)));
    }
    None
}

/// One page as returned: decoded txs in SOURCE order, and the continuation token (None = end).
#[derive(Debug, Clone)]
pub struct Page {
    pub txs: Vec<Value>,
    pub next: Option<String>,
}

/// Transport failures, named. A failure is never read as "no history".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    Http(String),
    RateLimited,
    RpcError(String),
    BadShape(String),
}

/// The only thing that talks to the network. Tests use [`MockPages`]; production uses [`HeliusHttp`].
pub trait PageSource {
    /// `getTransactionsForAddress(address, {transactionDetails:"full", encoding:"json",
    /// maxSupportedTransactionVersion:0, sortOrder:"asc", limit, filters, paginationToken})`.
    fn page(&self, address: &str, filters: &Value, token: Option<&str>)
        -> Result<Page, FetchError>;
    /// Stable label for provenance (never contains a secret).
    fn label(&self) -> &str;
}

/// Request body for one page. Pure; unit-tested. Contains no credential (the key travels only in the
/// transport's URL, which is never logged or returned).
#[must_use]
pub fn request_body(address: &str, filters: &Value, token: Option<&str>, limit: u32) -> Value {
    let mut opts = json!({
        "transactionDetails": "full",
        "encoding": "json",
        "maxSupportedTransactionVersion": 0,
        "sortOrder": "asc",
        "commitment": "finalized",
        "limit": limit,
        "filters": filters,
    });
    if let Some(t) = token {
        opts["paginationToken"] = Value::String(t.to_string());
    }
    json!({"jsonrpc": "2.0", "id": "launch-bootstrap", "method": "getTransactionsForAddress",
           "params": [address, opts]})
}

/// Parse one JSON-RPC response into a [`Page`].
pub fn parse_page(resp: &Value) -> Result<Page, FetchError> {
    if let Some(e) = resp.get("error") {
        let code = e.get("code").and_then(Value::as_i64).unwrap_or(0);
        if code == 429 || code == -32429 {
            return Err(FetchError::RateLimited);
        }
        return Err(FetchError::RpcError(format!("code {code}")));
    }
    let r = resp
        .get("result")
        .ok_or_else(|| FetchError::BadShape("no result".into()))?;
    let txs = r
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| FetchError::BadShape("no result.data".into()))?
        .clone();
    let next = r
        .get("paginationToken")
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok(Page { txs, next })
}

/// Walk budget. Exhausting it is `HistoryIncomplete`, never "not found".
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    pub max_pages: u32,
}

/// Find the mint's creation by walking its history OLDEST-FIRST, decoding every transaction.
/// Filters: successful transactions only is NOT applied server-side, so a failed create is visible
/// and named. The first decoded successful create of THIS mint ends the walk.
pub fn find_launch(
    src: &dyn PageSource,
    mint_b58: &str,
    budget: Budget,
    retrieved_unix_ms: i64,
) -> LaunchOutcome {
    let Some(mint) = pk(mint_b58) else {
        return LaunchOutcome::Malformed("mint is not base58 32 bytes".into());
    };
    let filters = json!({ "status": "any" });
    let mut token: Option<String> = None;
    let mut pages = 0u32;
    let mut txs = 0u64;
    let mut failed_create: Option<String> = None;
    loop {
        if pages >= budget.max_pages {
            return LaunchOutcome::HistoryIncomplete {
                pages,
                reason: "page_budget".into(),
            };
        }
        let page = match src.page(mint_b58, &filters, token.as_deref()) {
            Ok(p) => p,
            Err(e) => {
                return LaunchOutcome::HistoryIncomplete {
                    pages,
                    reason: format!("{e:?}"),
                }
            }
        };
        pages = pages.saturating_add(1);
        for raw in &page.txs {
            txs = txs.saturating_add(1);
            let tx = match parse_full_tx(raw) {
                Ok(t) => t,
                Err(e) => return LaunchOutcome::Malformed(format!("{e:?}")),
            };
            match create_in_tx(&tx, &mint) {
                None => {}
                Some(Err(LaunchOutcome::CreateFailedTx { signature })) => {
                    failed_create.get_or_insert(signature);
                }
                Some(Err(other)) => return other,
                Some(Ok((creator, instruction, inner))) => {
                    return LaunchOutcome::Verified(LaunchEvidence {
                        mint,
                        creator,
                        signature: tx.signature,
                        slot: tx.slot,
                        tx_index: tx.tx_index,
                        block_time_s: tx.block_time_s,
                        instruction,
                        inner,
                        retrieved_unix_ms,
                        source: src.label().to_string(),
                    })
                }
            }
        }
        match page.next {
            Some(t) if !page.txs.is_empty() => token = Some(t),
            _ => break,
        }
    }
    match failed_create {
        Some(signature) => LaunchOutcome::CreateFailedTx { signature },
        None => LaunchOutcome::CreateNotFound { pages, txs },
    }
}

/// The window a creator count is claimed over. The trained table is a capture window
/// (renormalized_v7: 2026-08-23 13:33Z .. 2026-09-10 04:09Z), so a count is meaningful only with one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoverageWindow {
    pub from_slot: u64,
    /// Exclusive: the mint's own launch slot (launches strictly before it count).
    pub to_slot_excl: u64,
}

/// `creator_past_launches` as established from the creator's own history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PriorLaunches {
    /// Walked to exhaustion over `window`: exactly `n` verified successful creates by this creator, one per
    /// distinct mint (dedup by mint, as the table dedups), strictly before the window end.
    Exact {
        n: u64,
        window: CoverageWindow,
        mints: Vec<String>,
    },
    /// The walk did not reach the end of the window. A lower bound is kept; it is never shown as the count.
    Incomplete {
        at_least: u64,
        pages: u32,
        reason: String,
    },
}

/// Count a creator's verified pump.fun launches in `window` by walking the CREATOR's history oldest-first
/// with a server-side slot filter. Only successful creates whose key 0 is this creator count.
pub fn creator_prior_launches(
    src: &dyn PageSource,
    creator_b58: &str,
    window: CoverageWindow,
    budget: Budget,
) -> PriorLaunches {
    let Some(creator) = pk(creator_b58) else {
        return PriorLaunches::Incomplete {
            at_least: 0,
            pages: 0,
            reason: "bad creator".into(),
        };
    };
    let Some(pf) = pk(PUMP_FUN) else {
        return PriorLaunches::Incomplete {
            at_least: 0,
            pages: 0,
            reason: "const".into(),
        };
    };
    let filters = json!({
        "status": "succeeded",
        "slot": { "gte": window.from_slot, "lt": window.to_slot_excl },
    });
    let mut seen = std::collections::BTreeSet::new();
    let mut token: Option<String> = None;
    let mut pages = 0u32;
    loop {
        if pages >= budget.max_pages {
            return PriorLaunches::Incomplete {
                at_least: seen.len() as u64,
                pages,
                reason: "page_budget".into(),
            };
        }
        let page = match src.page(creator_b58, &filters, token.as_deref()) {
            Ok(p) => p,
            Err(e) => {
                return PriorLaunches::Incomplete {
                    at_least: seen.len() as u64,
                    pages,
                    reason: format!("{e:?}"),
                }
            }
        };
        pages = pages.saturating_add(1);
        for raw in &page.txs {
            let Ok(tx) = parse_full_tx(raw) else {
                return PriorLaunches::Incomplete {
                    at_least: seen.len() as u64,
                    pages,
                    reason: "malformed tx".into(),
                };
            };
            if !tx.succeeded || tx.keys.first() != Some(&creator) {
                continue;
            }
            if tx.slot < window.from_slot || tx.slot >= window.to_slot_excl {
                // The source ignored the filter: never trust the page as covering the window.
                return PriorLaunches::Incomplete {
                    at_least: seen.len() as u64,
                    pages,
                    reason: "out_of_window_tx".into(),
                };
            }
            for ix in &tx.ixs {
                let is_create = ix.program == pf
                    && ix.data.len() >= 8
                    && (ix.data[..8] == CREATE_DISC || ix.data[..8] == CREATE_V2_DISC);
                if is_create {
                    if let Some(m) = ix.accounts.first() {
                        seen.insert(b58(m));
                    }
                }
            }
        }
        match page.next {
            Some(t) if !page.txs.is_empty() => token = Some(t),
            _ => break,
        }
    }
    PriorLaunches::Exact {
        n: seen.len() as u64,
        window,
        mints: seen.into_iter().collect(),
    }
}

// ---------------------------------------------------------------------------------------------
// Durable evidence cache (public chain facts only; no secret is ever written here).
// ---------------------------------------------------------------------------------------------

/// Serialize verified evidence. Every clock is a separate field.
#[must_use]
pub fn evidence_json(e: &LaunchEvidence) -> Value {
    json!({
        "schema": "launch_evidence_v1",
        "mint": b58(&e.mint),
        "creator": b58(&e.creator),
        "creator_definition": "tx_account_key_0 (renormalize_raw.py); verified == create ix user",
        "signature": e.signature,
        "slot": e.slot,
        "tx_index": e.tx_index,
        "block_time_s": e.block_time_s,
        "instruction": e.instruction,
        "inner": e.inner,
        "retrieved_unix_ms": e.retrieved_unix_ms,
        "source": e.source,
    })
}

/// Restore evidence, refusing anything that does not round-trip exactly.
pub fn evidence_from_json(v: &Value) -> Option<LaunchEvidence> {
    if v.get("schema")?.as_str()? != "launch_evidence_v1" {
        return None;
    }
    let instruction = match v.get("instruction")?.as_str()? {
        "create" => "create",
        "create_v2" => "create_v2",
        _ => return None,
    };
    Some(LaunchEvidence {
        mint: pk(v.get("mint")?.as_str()?)?,
        creator: pk(v.get("creator")?.as_str()?)?,
        signature: v.get("signature")?.as_str()?.to_string(),
        slot: v.get("slot")?.as_u64()?,
        tx_index: v.get("tx_index").and_then(Value::as_u64),
        block_time_s: v.get("block_time_s").and_then(Value::as_i64),
        instruction,
        inner: v.get("inner")?.as_bool()?,
        retrieved_unix_ms: v.get("retrieved_unix_ms")?.as_i64()?,
        source: v.get("source")?.as_str()?.to_string(),
    })
}

/// Directory of `<mint>.json` evidence files, written atomically (tmp + rename).
#[derive(Debug, Clone)]
pub struct EvidenceCache {
    dir: PathBuf,
}

impl EvidenceCache {
    #[must_use]
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
        }
    }
    fn path(&self, mint_b58: &str) -> Option<PathBuf> {
        // A mint is base58: reject anything that could escape the directory.
        pk(mint_b58)?;
        Some(self.dir.join(format!("{mint_b58}.json")))
    }
    /// Persist verified evidence. Only `Verified` outcomes are cached: a negative or incomplete result is
    /// never memoised as a fact.
    pub fn put(&self, e: &LaunchEvidence) -> std::io::Result<()> {
        let m = b58(&e.mint);
        let p = self
            .path(&m)
            .ok_or_else(|| std::io::Error::other("bad mint"))?;
        std::fs::create_dir_all(&self.dir)?;
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, evidence_json(e).to_string())?;
        std::fs::rename(tmp, p)
    }
    /// Restore; a corrupt or mismatched file is `None` (re-fetch), never a launch.
    #[must_use]
    pub fn get(&self, mint_b58: &str) -> Option<LaunchEvidence> {
        let p = self.path(mint_b58)?;
        let v: Value = serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()?;
        let e = evidence_from_json(&v)?;
        (b58(&e.mint) == mint_b58).then_some(e)
    }
}

/// Cache-first lookup.
pub fn launch_cached(
    cache: &EvidenceCache,
    src: &dyn PageSource,
    mint_b58: &str,
    budget: Budget,
    now_ms: i64,
) -> LaunchOutcome {
    if let Some(e) = cache.get(mint_b58) {
        return LaunchOutcome::Verified(e);
    }
    let out = find_launch(src, mint_b58, budget, now_ms);
    if let LaunchOutcome::Verified(e) = &out {
        let _ = cache.put(e);
    }
    out
}

/// Whether this evidence may be shown to a decision at `t_dec_ms` in a REPLAY. Facts retrieved after the
/// decision existed only in hindsight: a replay that uses them must label itself hindsight-bootstrapped.
#[must_use]
pub fn available_at(e: &LaunchEvidence, t_dec_ms: i64) -> bool {
    e.retrieved_unix_ms <= t_dec_ms
}

// ---------------------------------------------------------------------------------------------
// Credential + transport. The key is read from a 0600 file, held in memory only, and never formatted
// into any error, log, provenance label or return value.
// ---------------------------------------------------------------------------------------------

/// Default credential file on Linux (outside every git tree).
pub const DEFAULT_CREDS: &str = "/home/alon/.config/pump-quant/helius.env";

/// Why the credential is unusable. Messages name the PATH and the problem, never the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredError {
    Missing(PathBuf),
    InsecureMode { path: PathBuf, mode: u32 },
    NoKeyLine(PathBuf),
    BadShape(PathBuf),
}

/// A secret that cannot be printed by accident.
pub struct ApiKey(String);
impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

/// Load `HELIUS_API_KEY=` from `path`. Refuses group/world-readable files and non-ASCII / wrong-shape values
/// (Helius keys are 36-char UUIDs).
pub fn load_key(path: &Path) -> Result<ApiKey, CredError> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path).map_err(|_| CredError::Missing(path.to_path_buf()))?;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(CredError::InsecureMode {
            path: path.to_path_buf(),
            mode,
        });
    }
    let text = std::fs::read_to_string(path).map_err(|_| CredError::Missing(path.to_path_buf()))?;
    let v = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("HELIUS_API_KEY="))
        .map(|v| v.trim().trim_matches('"').trim_matches('\'').to_string())
        .ok_or_else(|| CredError::NoKeyLine(path.to_path_buf()))?;
    let uuid_like = v.len() == 36
        && v.chars().enumerate().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        });
    if !uuid_like {
        return Err(CredError::BadShape(path.to_path_buf()));
    }
    Ok(ApiKey(v))
}

/// Production transport (blocking, bounded timeout). Not exercised by tests (no network).
pub struct HeliusHttp {
    key: ApiKey,
    agent: ureq::Agent,
    limit: u32,
}

impl HeliusHttp {
    #[must_use]
    pub fn new(key: ApiKey, timeout: std::time::Duration) -> Self {
        Self {
            key,
            agent: ureq::AgentBuilder::new().timeout(timeout).build(),
            limit: PAGE_LIMIT_FULL,
        }
    }
}

impl PageSource for HeliusHttp {
    fn page(
        &self,
        address: &str,
        filters: &Value,
        token: Option<&str>,
    ) -> Result<Page, FetchError> {
        let url = format!("https://mainnet.helius-rpc.com/?api-key={}", self.key.0);
        let body = request_body(address, filters, token, self.limit);
        match self.agent.post(&url).send_json(body) {
            Ok(r) => {
                let v: Value = r
                    .into_json()
                    .map_err(|_| FetchError::BadShape("non-json".into()))?;
                parse_page(&v)
            }
            Err(ureq::Error::Status(429, _)) => Err(FetchError::RateLimited),
            // Status code only: the URL (which carries the key) is never formatted.
            Err(ureq::Error::Status(c, _)) => Err(FetchError::Http(format!("status {c}"))),
            Err(ureq::Error::Transport(t)) => {
                Err(FetchError::Http(format!("transport {:?}", t.kind())))
            }
        }
    }
    fn label(&self) -> &str {
        "helius:getTransactionsForAddress"
    }
}

/// Test/mock transport: serves pre-recorded pages keyed by (address, token). LABELLED AS MOCK in provenance.
pub struct MockPages {
    pub pages: std::collections::BTreeMap<(String, String), Result<Page, FetchError>>,
    pub calls: std::cell::RefCell<Vec<(String, Value, Option<String>)>>,
}

impl PageSource for MockPages {
    fn page(
        &self,
        address: &str,
        filters: &Value,
        token: Option<&str>,
    ) -> Result<Page, FetchError> {
        self.calls.borrow_mut().push((
            address.to_string(),
            filters.clone(),
            token.map(str::to_string),
        ));
        self.pages
            .get(&(address.to_string(), token.unwrap_or("").to_string()))
            .cloned()
            .unwrap_or(Err(FetchError::RpcError("mock: no page".into())))
    }
    fn label(&self) -> &str {
        "MOCK"
    }
}
