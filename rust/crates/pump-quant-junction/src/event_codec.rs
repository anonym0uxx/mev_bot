//! Event-stream codec v2: FULL-FIDELITY, versioned encoding of every `AppEvent` variant.
//!
//! v1 (unversioned) lines dropped fields (AmmSwap pool/fees/virtual quote/trader, CorpusFlowRow trader/venue/
//! fee/CU, MarketTrade slot/fee/CU/venue/event_id/feature basis, LaunchObserved creator) and the v1 reader had
//! no arm for CurveObserved / AmmSwap / CorpusFlowRow / LaunchObserved / Model* events, so a v1 replay
//! silently skipped most market data. v2 writes every field; the checked reader classifies every line and a
//! replay with any rejected or lossy critical line is INCOMPLETE by name, never a quiet success.
//!
//! Encoding: `{"v":3,"slot":N,"kind":"<Kind>","mint":"<b58>"?,"fields":{...}}`. u128/i128 are decimal or hex
//! strings (JSON numbers cannot carry them losslessly); pubkeys base58; signatures hex; `None` = key absent.
#![allow(clippy::too_many_lines)]

use std::collections::BTreeMap;

use pump_quant_app::event::{AppEvent, CreatorActionKind, FeatureBasis, TradeVenue};
use pump_quant_domain::ids::Mint;
use pump_quant_protocol::pumpswap_event::CashbackField;
use serde_json::{json, Map, Value};

/// Schema version written by [`encode`].
///
/// SCHEMA HISTORY (provenance; every line states the version it was written under in `"v"`):
/// * v1 — unversioned, lossy (see module docs).
/// * v2 (a566cb0d) — full fidelity for every field `AppEvent` had then.
/// * v3 — adds the REQUIRED `AmmSwap.fields.cashback` object: the event's cashback pair with its layout
///   provenance, `{"state":"known","bps":B,"lamports":L,"layout_len":N}` | `{"state":"missing","layout_len":N}`
///   | `{"state":"unsupported","layout_len":N}` | `{"state":"not_recorded"}`. Every other kind is byte-identical
///   to v2.
/// * v3 (additive kind, 2026-10-09) — `CurveModeObserved` `{"mayhem":bool,"slot":N}` (operator decision: SKIP
///   Mayhem-mode coins). No existing kind changed, so the version is not bumped; a reader that predates the kind
///   REJECTS such a line by name (critical kind -> the replay is INCOMPLETE), it never drops it silently.
///
/// A v2 line still decodes (same fields); its `AmmSwap` carries `CashbackField::NotRecorded` and the checked
/// reader counts it under [`KindCount::schema_gap`], so the gap is NAMED, never read as a zero.
pub const SCHEMA_VERSION: u64 = 3;
/// Oldest versioned schema [`decode`] still reads.
pub const MIN_READ_SCHEMA_VERSION: u64 = 2;
/// First schema version whose `AmmSwap` lines carry the cashback field.
pub const CASHBACK_SCHEMA_VERSION: u64 = 3;

fn cashback_j(c: CashbackField) -> Value {
    match c {
        CashbackField::Known {
            bps,
            lamports,
            layout_len,
        } => json!({"state": "known", "bps": bps, "lamports": lamports, "layout_len": layout_len}),
        CashbackField::Missing { layout_len } => {
            json!({"state": "missing", "layout_len": layout_len})
        }
        CashbackField::Unsupported { layout_len } => {
            json!({"state": "unsupported", "layout_len": layout_len})
        }
        CashbackField::NotRecorded => json!({"state": "not_recorded"}),
    }
}

fn cashback_p(f: &F<'_>, ver: u64) -> Result<CashbackField, String> {
    if ver < CASHBACK_SCHEMA_VERSION {
        // The writer's schema had no such field: whatever the chain carried was never recorded.
        if f.raw("cashback").is_some() {
            return Err(format!("field cashback: not part of schema v{ver}"));
        }
        return Ok(CashbackField::NotRecorded);
    }
    let m = f
        .raw("cashback")
        .and_then(Value::as_object)
        .ok_or("field cashback: missing or not object")?;
    let c = F(m);
    let len = || -> Result<u16, String> {
        c.small("layout_len")
            .map_err(|e| format!("field cashback.{e}"))
    };
    Ok(
        match c.str("state").map_err(|e| format!("field cashback.{e}"))? {
            "known" => CashbackField::Known {
                bps: c.u64("bps").map_err(|e| format!("field cashback.{e}"))?,
                lamports: c
                    .u64("lamports")
                    .map_err(|e| format!("field cashback.{e}"))?,
                layout_len: len()?,
            },
            "missing" => CashbackField::Missing { layout_len: len()? },
            "unsupported" => CashbackField::Unsupported { layout_len: len()? },
            "not_recorded" => CashbackField::NotRecorded,
            o => return Err(format!("field cashback.state: unknown {o}")),
        },
    )
}

fn b58(k: &[u8; 32]) -> String {
    solana_program::pubkey::Pubkey::new_from_array(*k).to_string()
}
fn pk(s: &str) -> Result<[u8; 32], String> {
    s.parse::<solana_program::pubkey::Pubkey>()
        .map(|p| p.to_bytes())
        .map_err(|e| format!("bad pubkey: {e}"))
}
fn hex64(sig: &[u8; 64]) -> String {
    sig.iter().map(|b| format!("{b:02x}")).collect()
}
fn unhex64(s: &str) -> Result<[u8; 64], String> {
    if s.len() != 128 || !s.is_ascii() {
        return Err("signature hex length".into());
    }
    let mut o = [0u8; 64];
    for (i, b) in o.iter_mut().enumerate() {
        let j = i.checked_mul(2).ok_or("idx")?;
        *b = u8::from_str_radix(s.get(j..j.checked_add(2).ok_or("idx")?).ok_or("idx")?, 16)
            .map_err(|_| "signature hex digit")?;
    }
    Ok(o)
}
fn venue_s(v: TradeVenue) -> &'static str {
    match v {
        TradeVenue::PumpFun => "pumpfun",
        TradeVenue::PumpSwap => "pumpswap",
    }
}
fn venue_p(s: &str) -> Result<TradeVenue, String> {
    match s {
        "pumpfun" => Ok(TradeVenue::PumpFun),
        "pumpswap" => Ok(TradeVenue::PumpSwap),
        o => Err(format!("unknown venue {o}")),
    }
}
fn feat_j(f: &FeatureBasis) -> Value {
    json!({"sol_lamports": f.sol_lamports, "tokens_raw": f.tokens_raw, "trader": b58(&f.trader)})
}

/// Typed field access over one line's `fields` object. Every getter names the missing/invalid key.
struct F<'a>(&'a Map<String, Value>);
impl F<'_> {
    fn raw(&self, k: &str) -> Option<&Value> {
        self.0.get(k).filter(|v| !v.is_null())
    }
    fn u64(&self, k: &str) -> Result<u64, String> {
        self.raw(k)
            .and_then(Value::as_u64)
            .ok_or(format!("field {k}: missing or not u64"))
    }
    fn ou64(&self, k: &str) -> Result<Option<u64>, String> {
        self.raw(k)
            .map(|v| v.as_u64().ok_or(format!("field {k}: not u64")))
            .transpose()
    }
    fn i64(&self, k: &str) -> Result<i64, String> {
        self.raw(k)
            .and_then(Value::as_i64)
            .ok_or(format!("field {k}: missing or not i64"))
    }
    fn oi64(&self, k: &str) -> Result<Option<i64>, String> {
        self.raw(k)
            .map(|v| v.as_i64().ok_or(format!("field {k}: not i64")))
            .transpose()
    }
    fn bool(&self, k: &str) -> Result<bool, String> {
        self.raw(k)
            .and_then(Value::as_bool)
            .ok_or(format!("field {k}: missing or not bool"))
    }
    fn small<T: TryFrom<u64>>(&self, k: &str) -> Result<T, String> {
        T::try_from(self.u64(k)?).map_err(|_| format!("field {k}: out of range"))
    }
    fn str(&self, k: &str) -> Result<&str, String> {
        self.raw(k)
            .and_then(Value::as_str)
            .ok_or(format!("field {k}: missing or not string"))
    }
    fn pk(&self, k: &str) -> Result<[u8; 32], String> {
        pk(self.str(k)?)
    }
    fn opk(&self, k: &str) -> Result<Option<[u8; 32]>, String> {
        self.raw(k).map(|_| self.pk(k)).transpose()
    }
    fn i128(&self, k: &str) -> Result<i128, String> {
        self.str(k)?
            .parse()
            .map_err(|_| format!("field {k}: not i128"))
    }
    fn u128hex(&self, k: &str) -> Result<u128, String> {
        u128::from_str_radix(self.str(k)?, 16).map_err(|_| format!("field {k}: not hex u128"))
    }
    fn ou128hex(&self, k: &str) -> Result<Option<u128>, String> {
        self.raw(k).map(|_| self.u128hex(k)).transpose()
    }
    fn ovenue(&self, k: &str) -> Result<Option<TradeVenue>, String> {
        self.raw(k).map(|_| venue_p(self.str(k)?)).transpose()
    }
    fn ofeat(&self, k: &str) -> Result<Option<FeatureBasis>, String> {
        let Some(v) = self.raw(k) else {
            return Ok(None);
        };
        let m = v.as_object().ok_or(format!("field {k}: not object"))?;
        let f = F(m);
        Ok(Some(FeatureBasis {
            sol_lamports: f.i64("sol_lamports")?,
            tokens_raw: f.i64("tokens_raw")?,
            trader: f.pk("trader")?,
        }))
    }
    fn sig(&self, k: &str) -> Result<[u8; 64], String> {
        unhex64(self.str(k)?)
    }
}

fn kind(e: &AppEvent) -> &'static str {
    match e {
        AppEvent::MarketTrade { .. } => "MarketTrade",
        AppEvent::CorpusFlowRow { .. } => "CorpusFlowRow",
        AppEvent::NarrativeSample { .. } => "NarrativeSample",
        AppEvent::SocialCall { .. } => "SocialCall",
        AppEvent::WalletAction { .. } => "WalletAction",
        AppEvent::OnchainConfirm { .. } => "OnchainConfirm",
        AppEvent::CurveObserved { .. } => "CurveObserved",
        AppEvent::CurveModeObserved { .. } => "CurveModeObserved",
        AppEvent::AmmSwap { .. } => "AmmSwap",
        AppEvent::LaunchObserved { .. } => "LaunchObserved",
        AppEvent::LaunchFromChain { .. } => "LaunchFromChain",
        AppEvent::TokenMetadata { .. } => "TokenMetadata",
        AppEvent::CreatorAction { .. } => "CreatorAction",
        AppEvent::Migration { .. } => "Migration",
        AppEvent::MarketAuxiliary { .. } => "MarketAuxiliary",
        AppEvent::NarrativeResolved { .. } => "NarrativeResolved",
        AppEvent::TimeSignal { .. } => "TimeSignal",
        AppEvent::ModelOrderEvidence { .. } => "ModelOrderEvidence",
        AppEvent::ModelMgmtReport { .. } => "ModelMgmtReport",
        AppEvent::OurBuyConfirmed { .. } => "OurBuyConfirmed",
        AppEvent::OurBuyFailed { .. } => "OurBuyFailed",
        AppEvent::OurSellConfirmed { .. } => "OurSellConfirmed",
        AppEvent::OurSellFailed { .. } => "OurSellFailed",
        AppEvent::Tick => "Tick",
    }
}

/// Every kind the codec knows. A writer change that adds a variant must add it here (pinned by test).
pub const KINDS: [&str; 24] = [
    "MarketTrade",
    "CorpusFlowRow",
    "NarrativeSample",
    "SocialCall",
    "WalletAction",
    "OnchainConfirm",
    "CurveObserved",
    "CurveModeObserved",
    "AmmSwap",
    "LaunchObserved",
    "LaunchFromChain",
    "TokenMetadata",
    "CreatorAction",
    "Migration",
    "MarketAuxiliary",
    "NarrativeResolved",
    "TimeSignal",
    "ModelOrderEvidence",
    "ModelMgmtReport",
    "OurBuyConfirmed",
    "OurBuyFailed",
    "OurSellConfirmed",
    "OurSellFailed",
    "Tick",
];

/// Kinds whose loss makes a replay of market state, readiness or recovery INCOMPLETE.
#[must_use]
pub fn is_critical(kind: &str) -> bool {
    !matches!(
        kind,
        "Tick" | "TimeSignal" | "NarrativeSample" | "SocialCall"
    )
}

fn put(m: &mut Map<String, Value>, k: &str, v: Option<Value>) {
    if let Some(v) = v {
        m.insert(k.to_string(), v);
    }
}

fn fields(e: &AppEvent) -> Map<String, Value> {
    let mut m = Map::new();
    match *e {
        AppEvent::MarketTrade {
            price_fp,
            quote_lamports,
            liquidity_lamports,
            signed_base,
            buyer_entity,
            age_slots,
            trader_pubkey,
            recv_unix_ms,
            slot,
            fee_lamports,
            cu_consumed,
            venue,
            event_id,
            feature,
            ..
        } => {
            m.insert("price_fp".into(), json!(price_fp.to_string()));
            m.insert("quote_lamports".into(), json!(quote_lamports));
            m.insert("liquidity_lamports".into(), json!(liquidity_lamports));
            m.insert("signed_base".into(), json!(signed_base));
            m.insert("buyer_entity".into(), json!(buyer_entity));
            m.insert("age_slots".into(), json!(age_slots));
            put(
                &mut m,
                "trader_pubkey",
                trader_pubkey.map(|k| json!(b58(&k))),
            );
            put(&mut m, "recv_unix_ms", recv_unix_ms.map(|v| json!(v)));
            put(&mut m, "slot", slot.map(|v| json!(v)));
            put(&mut m, "fee_lamports", fee_lamports.map(|v| json!(v)));
            put(&mut m, "cu_consumed", cu_consumed.map(|v| json!(v)));
            put(&mut m, "venue", venue.map(|v| json!(venue_s(v))));
            put(
                &mut m,
                "event_id",
                event_id.map(|v| json!(format!("{v:032x}"))),
            );
            put(&mut m, "feature", feature.as_ref().map(feat_j));
        }
        AppEvent::CorpusFlowRow {
            venue,
            feature,
            recv_unix_ms,
            slot,
            fee_lamports,
            cu_consumed,
            event_id,
            ..
        } => {
            m.insert("venue".into(), json!(venue_s(venue)));
            m.insert("feature".into(), feat_j(&feature));
            m.insert("event_id".into(), json!(format!("{event_id:032x}")));
            put(&mut m, "recv_unix_ms", recv_unix_ms.map(|v| json!(v)));
            put(&mut m, "slot", slot.map(|v| json!(v)));
            put(&mut m, "fee_lamports", fee_lamports.map(|v| json!(v)));
            put(&mut m, "cu_consumed", cu_consumed.map(|v| json!(v)));
        }
        AppEvent::CurveObserved {
            v_sol_lamports,
            v_tokens,
            real_sol_lamports,
            real_tokens,
            recv_unix_ms,
            slot,
            ..
        } => {
            m.insert("v_sol_lamports".into(), json!(v_sol_lamports));
            m.insert("v_tokens".into(), json!(v_tokens));
            m.insert("real_sol_lamports".into(), json!(real_sol_lamports));
            m.insert("real_tokens".into(), json!(real_tokens));
            m.insert("slot".into(), json!(slot));
            put(&mut m, "recv_unix_ms", recv_unix_ms.map(|v| json!(v)));
        }
        AppEvent::CurveModeObserved { mayhem, slot, .. } => {
            m.insert("mayhem".into(), json!(mayhem));
            m.insert("slot".into(), json!(slot));
        }
        _ => fields2(e, &mut m),
    }
    m
}

fn fields2(e: &AppEvent, m: &mut Map<String, Value>) {
    match *e {
        AppEvent::AmmSwap {
            pool,
            pool_is_canonical,
            quote_is_wsol,
            token_reserve_pre,
            quote_reserve_pre,
            fee_bps,
            fee_parts,
            virtual_quote,
            cashback,
            is_buy,
            token_amount,
            quote_lamports,
            trader,
            fee_lamports,
            cu_consumed,
            recv_unix_ms,
            slot,
            ..
        } => {
            m.insert("cashback".into(), cashback_j(cashback));
            m.insert("pool".into(), json!(b58(&pool)));
            m.insert("pool_is_canonical".into(), json!(pool_is_canonical));
            m.insert("quote_is_wsol".into(), json!(quote_is_wsol));
            m.insert("token_reserve_pre".into(), json!(token_reserve_pre));
            m.insert("quote_reserve_pre".into(), json!(quote_reserve_pre));
            m.insert("is_buy".into(), json!(is_buy));
            m.insert("token_amount".into(), json!(token_amount));
            m.insert("quote_lamports".into(), json!(quote_lamports));
            m.insert("trader".into(), json!(b58(&trader)));
            m.insert("slot".into(), json!(slot));
            put(m, "fee_bps", fee_bps.map(|v| json!(v)));
            put(m, "fee_parts", fee_parts.map(|(a, b, c)| json!([a, b, c])));
            put(m, "virtual_quote", virtual_quote.map(|v| json!(v)));
            put(m, "fee_lamports", fee_lamports.map(|v| json!(v)));
            put(m, "cu_consumed", cu_consumed.map(|v| json!(v)));
            put(m, "recv_unix_ms", recv_unix_ms.map(|v| json!(v)));
        }
        AppEvent::LaunchObserved {
            creator,
            launch_unix_ms,
            ..
        } => {
            m.insert("creator".into(), json!(b58(&creator)));
            m.insert("launch_unix_ms".into(), json!(launch_unix_ms));
        }
        AppEvent::LaunchFromChain {
            creator,
            slot,
            block_time_s,
            retrieved_unix_ms,
            ..
        } => {
            m.insert("creator".into(), json!(b58(&creator)));
            m.insert("chain_slot".into(), json!(slot));
            m.insert("retrieved_unix_ms".into(), json!(retrieved_unix_ms));
            put(m, "block_time_s", block_time_s.map(|v| json!(v)));
        }
        AppEvent::NarrativeSample {
            prior_active,
            new_mentions,
            ..
        } => {
            m.insert("prior_active".into(), json!(prior_active));
            m.insert("new_mentions".into(), json!(new_mentions));
        }
        AppEvent::SocialCall {
            source_quality_bp, ..
        } => {
            m.insert("source_quality_bp".into(), json!(source_quality_bp));
        }
        AppEvent::WalletAction {
            followable,
            size_lamports,
            ..
        } => {
            m.insert("followable".into(), json!(followable));
            m.insert("size_lamports".into(), json!(size_lamports));
        }
        AppEvent::OnchainConfirm {
            virtual_sol_lamports,
            real_sol_lamports,
            ..
        } => {
            m.insert("virtual_sol_lamports".into(), json!(virtual_sol_lamports));
            m.insert("real_sol_lamports".into(), json!(real_sol_lamports));
        }
        AppEvent::TokenMetadata {
            category_id,
            taxonomy_version,
            creator,
            slot,
            ..
        } => {
            m.insert("category_id".into(), json!(category_id));
            m.insert("taxonomy_version".into(), json!(taxonomy_version));
            m.insert("creator".into(), json!(creator));
            m.insert("slot".into(), json!(slot));
        }
        _ => fields3(e, m),
    }
}

fn fields3(e: &AppEvent, m: &mut Map<String, Value>) {
    match *e {
        AppEvent::CreatorAction { kind, slot, .. } => {
            m.insert("slot".into(), json!(slot));
            let k = match kind {
                CreatorActionKind::Init {
                    initial_tokens,
                    total_supply,
                } => {
                    json!({"t": "init", "initial_tokens": initial_tokens, "total_supply": total_supply})
                }
                CreatorActionKind::Buy {
                    tokens,
                    quote_lamports,
                } => {
                    json!({"t": "buy", "tokens": tokens, "quote_lamports": quote_lamports})
                }
                CreatorActionKind::Sell {
                    tokens,
                    quote_lamports,
                } => {
                    json!({"t": "sell", "tokens": tokens, "quote_lamports": quote_lamports})
                }
                CreatorActionKind::LinkedBuy { cluster, tokens } => {
                    json!({"t": "linked_buy", "cluster": cluster, "tokens": tokens})
                }
            };
            m.insert("action".into(), k);
        }
        AppEvent::Migration { slot, .. } => {
            m.insert("slot".into(), json!(slot));
        }
        AppEvent::MarketAuxiliary {
            token_standard,
            symbol_len,
            ..
        } => {
            m.insert("token_standard".into(), json!(token_standard));
            m.insert("symbol_len".into(), json!(symbol_len));
        }
        AppEvent::NarrativeResolved {
            verdict,
            stage,
            family,
            lexicon_version,
            ..
        } => {
            m.insert("verdict".into(), json!(verdict));
            m.insert("stage".into(), json!(stage));
            m.insert("family".into(), json!(family));
            m.insert("lexicon_version".into(), json!(lexicon_version));
        }
        AppEvent::TimeSignal { dow, hour_utc } => {
            m.insert("dow".into(), json!(dow));
            m.insert("hour_utc".into(), json!(hour_utc));
        }
        AppEvent::ModelOrderEvidence {
            order_id,
            attempt,
            clip_lamports,
            filled,
            ..
        } => {
            m.insert("order_id".into(), json!(order_id));
            m.insert("attempt".into(), json!(attempt));
            m.insert("clip_lamports".into(), json!(clip_lamports));
            put(m, "filled", filled.map(|(a, b)| json!([a, b])));
        }
        AppEvent::ModelMgmtReport {
            order_id,
            action,
            intended,
            cumulative_tokens,
            cumulative_gross,
            cumulative_fees,
            terminal,
            ..
        } => {
            m.insert("order_id".into(), json!(order_id));
            m.insert("action".into(), json!(action));
            m.insert("intended".into(), json!(intended));
            m.insert("cumulative_tokens".into(), json!(cumulative_tokens));
            m.insert("cumulative_gross".into(), json!(cumulative_gross));
            m.insert("cumulative_fees".into(), json!(cumulative_fees));
            m.insert("terminal".into(), json!(terminal));
        }
        AppEvent::OurBuyConfirmed {
            signature, slot, ..
        }
        | AppEvent::OurSellConfirmed {
            signature, slot, ..
        } => {
            m.insert("signature".into(), json!(hex64(&signature)));
            m.insert("slot".into(), json!(slot));
        }
        AppEvent::OurBuyFailed {
            signature,
            err_code,
            slot,
            ..
        }
        | AppEvent::OurSellFailed {
            signature,
            err_code,
            slot,
            ..
        } => {
            m.insert("signature".into(), json!(hex64(&signature)));
            m.insert("err_code".into(), json!(err_code));
            m.insert("slot".into(), json!(slot));
        }
        _ => {}
    }
}

/// Encode one event as a current-schema ([`SCHEMA_VERSION`]) line (no trailing newline).
#[must_use]
pub fn encode(e: &AppEvent, slot: u64) -> String {
    let mut o = Map::new();
    o.insert("v".into(), json!(SCHEMA_VERSION));
    o.insert("slot".into(), json!(slot));
    o.insert("kind".into(), json!(kind(e)));
    if let Some(m) = e.mint() {
        o.insert("mint".into(), json!(b58(m.as_bytes())));
    }
    let f = fields(e);
    if !f.is_empty() {
        o.insert("fields".into(), Value::Object(f));
    }
    Value::Object(o).to_string()
}

/// Decode one versioned line (schema [`MIN_READ_SCHEMA_VERSION`]..=[`SCHEMA_VERSION`]). `Err` names the reason (unknown kind, missing/invalid field, wrong version).
pub fn decode(line: &str) -> Result<(String, AppEvent), String> {
    let v: Value = serde_json::from_str(line).map_err(|_| "not json".to_string())?;
    let o = v.as_object().ok_or("not an object")?;
    let ver = o
        .get("v")
        .and_then(Value::as_u64)
        .ok_or("no schema version (v1 line)")?;
    if !(MIN_READ_SCHEMA_VERSION..=SCHEMA_VERSION).contains(&ver) {
        return Err(format!("unsupported schema version {ver}"));
    }
    let kind = o
        .get("kind")
        .and_then(Value::as_str)
        .ok_or("missing kind")?
        .to_string();
    let empty = Map::new();
    let f = F(o.get("fields").and_then(Value::as_object).unwrap_or(&empty));
    let mint = || -> Result<Mint, String> {
        Ok(Mint(pk(o
            .get("mint")
            .and_then(Value::as_str)
            .ok_or("missing mint")?)?))
    };
    let e = match kind.as_str() {
        "Tick" => AppEvent::Tick,
        "TimeSignal" => AppEvent::TimeSignal {
            dow: f.small("dow")?,
            hour_utc: f.small("hour_utc")?,
        },
        "MarketTrade" => AppEvent::MarketTrade {
            mint: mint()?,
            price_fp: f.i128("price_fp")?,
            quote_lamports: f.u64("quote_lamports")?,
            liquidity_lamports: f.u64("liquidity_lamports")?,
            signed_base: f.i64("signed_base")?,
            buyer_entity: f.u64("buyer_entity")?,
            age_slots: f.small("age_slots")?,
            trader_pubkey: f.opk("trader_pubkey")?,
            recv_unix_ms: f.oi64("recv_unix_ms")?,
            slot: f.ou64("slot")?,
            fee_lamports: f.ou64("fee_lamports")?,
            cu_consumed: f.ou64("cu_consumed")?,
            venue: f.ovenue("venue")?,
            event_id: f.ou128hex("event_id")?,
            feature: f.ofeat("feature")?,
        },
        "CorpusFlowRow" => AppEvent::CorpusFlowRow {
            mint: mint()?,
            venue: venue_p(f.str("venue")?)?,
            feature: f.ofeat("feature")?.ok_or("field feature: missing")?,
            recv_unix_ms: f.oi64("recv_unix_ms")?,
            slot: f.ou64("slot")?,
            fee_lamports: f.ou64("fee_lamports")?,
            cu_consumed: f.ou64("cu_consumed")?,
            event_id: f.u128hex("event_id")?,
        },
        "CurveObserved" => AppEvent::CurveObserved {
            mint: mint()?,
            v_sol_lamports: f.u64("v_sol_lamports")?,
            v_tokens: f.u64("v_tokens")?,
            real_sol_lamports: f.u64("real_sol_lamports")?,
            real_tokens: f.u64("real_tokens")?,
            recv_unix_ms: f.oi64("recv_unix_ms")?,
            slot: f.u64("slot")?,
        },
        "CurveModeObserved" => AppEvent::CurveModeObserved {
            mint: mint()?,
            mayhem: f.bool("mayhem")?,
            slot: f.u64("slot")?,
        },
        _ => decode2(&kind, &f, mint, ver)?,
    };
    Ok((kind, e))
}

fn fee_parts(f: &F<'_>) -> Result<Option<(u32, u32, u32)>, String> {
    let Some(v) = f.raw("fee_parts") else {
        return Ok(None);
    };
    let a = v
        .as_array()
        .filter(|a| a.len() == 3)
        .ok_or("field fee_parts: not [3]")?;
    let g = |i: usize| -> Result<u32, String> {
        a[i].as_u64()
            .and_then(|x| u32::try_from(x).ok())
            .ok_or("field fee_parts: element".to_string())
    };
    Ok(Some((g(0)?, g(1)?, g(2)?)))
}

fn decode2(
    kind: &str,
    f: &F<'_>,
    mint: impl Fn() -> Result<Mint, String>,
    ver: u64,
) -> Result<AppEvent, String> {
    Ok(match kind {
        "AmmSwap" => AppEvent::AmmSwap {
            mint: mint()?,
            pool: f.pk("pool")?,
            pool_is_canonical: f.bool("pool_is_canonical")?,
            quote_is_wsol: f.bool("quote_is_wsol")?,
            token_reserve_pre: f.u64("token_reserve_pre")?,
            quote_reserve_pre: f.u64("quote_reserve_pre")?,
            fee_bps: f
                .ou64("fee_bps")?
                .map(u32::try_from)
                .transpose()
                .map_err(|_| "fee_bps range")?,
            fee_parts: fee_parts(f)?,
            virtual_quote: f.ou64("virtual_quote")?,
            cashback: cashback_p(f, ver)?,
            is_buy: f.bool("is_buy")?,
            token_amount: f.u64("token_amount")?,
            quote_lamports: f.u64("quote_lamports")?,
            trader: f.pk("trader")?,
            fee_lamports: f.ou64("fee_lamports")?,
            cu_consumed: f.ou64("cu_consumed")?,
            recv_unix_ms: f.oi64("recv_unix_ms")?,
            slot: f.u64("slot")?,
        },
        "LaunchObserved" => AppEvent::LaunchObserved {
            mint: mint()?,
            creator: f.pk("creator")?,
            launch_unix_ms: f.i64("launch_unix_ms")?,
        },
        "LaunchFromChain" => AppEvent::LaunchFromChain {
            mint: mint()?,
            creator: f.pk("creator")?,
            slot: f.u64("chain_slot")?,
            block_time_s: f.oi64("block_time_s")?,
            retrieved_unix_ms: f.i64("retrieved_unix_ms")?,
        },
        "NarrativeSample" => AppEvent::NarrativeSample {
            mint: mint()?,
            prior_active: f.u64("prior_active")?,
            new_mentions: f.u64("new_mentions")?,
        },
        "SocialCall" => AppEvent::SocialCall {
            mint: mint()?,
            source_quality_bp: f.small("source_quality_bp")?,
        },
        "WalletAction" => AppEvent::WalletAction {
            mint: mint()?,
            followable: f.bool("followable")?,
            size_lamports: f.u64("size_lamports")?,
        },
        "OnchainConfirm" => AppEvent::OnchainConfirm {
            mint: mint()?,
            virtual_sol_lamports: f.u64("virtual_sol_lamports")?,
            real_sol_lamports: f.u64("real_sol_lamports")?,
        },
        "TokenMetadata" => AppEvent::TokenMetadata {
            mint: mint()?,
            category_id: f.u64("category_id")?,
            taxonomy_version: f.small("taxonomy_version")?,
            creator: f.u64("creator")?,
            slot: f.u64("slot")?,
        },
        "Migration" => AppEvent::Migration {
            mint: mint()?,
            slot: f.u64("slot")?,
        },
        "MarketAuxiliary" => AppEvent::MarketAuxiliary {
            mint: mint()?,
            token_standard: f.small("token_standard")?,
            symbol_len: f.small("symbol_len")?,
        },
        "NarrativeResolved" => AppEvent::NarrativeResolved {
            mint: mint()?,
            verdict: f.small("verdict")?,
            stage: f.small("stage")?,
            family: f.small("family")?,
            lexicon_version: f.small("lexicon_version")?,
        },
        _ => decode3(kind, f, mint)?,
    })
}

fn decode3(
    kind: &str,
    f: &F<'_>,
    mint: impl Fn() -> Result<Mint, String>,
) -> Result<AppEvent, String> {
    Ok(match kind {
        "CreatorAction" => {
            let a = f
                .raw("action")
                .and_then(Value::as_object)
                .ok_or("field action: missing")?;
            let g = F(a);
            let k = match g.str("t")? {
                "init" => CreatorActionKind::Init {
                    initial_tokens: g.u64("initial_tokens")?,
                    total_supply: g.u64("total_supply")?,
                },
                "buy" => CreatorActionKind::Buy {
                    tokens: g.u64("tokens")?,
                    quote_lamports: g.u64("quote_lamports")?,
                },
                "sell" => CreatorActionKind::Sell {
                    tokens: g.u64("tokens")?,
                    quote_lamports: g.u64("quote_lamports")?,
                },
                "linked_buy" => CreatorActionKind::LinkedBuy {
                    cluster: g.u64("cluster")?,
                    tokens: g.u64("tokens")?,
                },
                o => return Err(format!("unknown creator action {o}")),
            };
            AppEvent::CreatorAction {
                mint: mint()?,
                kind: k,
                slot: f.u64("slot")?,
            }
        }
        "ModelOrderEvidence" => {
            let filled = match f.raw("filled") {
                None => None,
                Some(v) => {
                    let a = v
                        .as_array()
                        .filter(|a| a.len() == 2)
                        .ok_or("field filled: not [2]")?;
                    let g = |i: usize| a[i].as_u64().ok_or("field filled: element".to_string());
                    Some((g(0)?, g(1)?))
                }
            };
            AppEvent::ModelOrderEvidence {
                mint: mint()?,
                order_id: f.u64("order_id")?,
                attempt: f.small("attempt")?,
                clip_lamports: f.u64("clip_lamports")?,
                filled,
            }
        }
        "ModelMgmtReport" => AppEvent::ModelMgmtReport {
            mint: mint()?,
            order_id: f.u64("order_id")?,
            action: f.small("action")?,
            intended: f.u64("intended")?,
            cumulative_tokens: f.u64("cumulative_tokens")?,
            cumulative_gross: f.u64("cumulative_gross")?,
            cumulative_fees: f.u64("cumulative_fees")?,
            terminal: f.bool("terminal")?,
        },
        "OurBuyConfirmed" => AppEvent::OurBuyConfirmed {
            mint: mint()?,
            signature: f.sig("signature")?,
            slot: f.u64("slot")?,
        },
        "OurSellConfirmed" => AppEvent::OurSellConfirmed {
            mint: mint()?,
            signature: f.sig("signature")?,
            slot: f.u64("slot")?,
        },
        "OurBuyFailed" => AppEvent::OurBuyFailed {
            mint: mint()?,
            signature: f.sig("signature")?,
            err_code: f.small("err_code")?,
            slot: f.u64("slot")?,
        },
        "OurSellFailed" => AppEvent::OurSellFailed {
            mint: mint()?,
            signature: f.sig("signature")?,
            err_code: f.small("err_code")?,
            slot: f.u64("slot")?,
        },
        o => return Err(format!("unknown event kind {o}")),
    })
}

/// Per-kind accounting of a read. `written` is what the file holds; every line is exactly one of
/// parsed / rejected; `lossy_v1` counts legacy lines that parsed only through the v1 reader (their
/// v1 encoding dropped fields, so they are never full-fidelity).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KindCount {
    pub written: u64,
    pub parsed: u64,
    pub rejected: u64,
    pub lossy_v1: u64,
    /// Lines that decoded under an OLDER versioned schema lacking a field the current schema carries
    /// (today: a v2 `AmmSwap`, whose cashback is `NotRecorded`). Full fidelity for what that schema had;
    /// incomplete for what it lacked. Named by [`CheckedStream::schema_gaps`], never a zero.
    pub schema_gap: u64,
}

/// The result of a checked read: events in file order plus a reconciliation the caller must honour.
#[derive(Debug, Clone, Default)]
pub struct CheckedStream {
    pub events: Vec<AppEvent>,
    pub by_kind: BTreeMap<String, KindCount>,
    /// First few rejection reasons per kind (bounded), for the report.
    pub reasons: BTreeMap<String, BTreeMap<String, u64>>,
    pub blank_lines: u64,
    /// Lines per declared schema version (`"v"`), including rejected ones. v1 lines are counted under 1.
    pub versions: BTreeMap<u64, u64>,
}

impl CheckedStream {
    /// Named incompleteness: every critical kind with a rejected or lossy line, then every schema gap
    /// ([`Self::schema_gaps`]). Empty = complete.
    #[must_use]
    pub fn incomplete(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .by_kind
            .iter()
            .filter(|(k, c)| is_critical(k) && (c.rejected > 0 || c.lossy_v1 > 0))
            .map(|(k, c)| format!("{k}:rejected={},lossy_v1={}", c.rejected, c.lossy_v1))
            .collect();
        v.extend(self.schema_gaps());
        v
    }
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.incomplete().is_empty()
    }
    /// Named schema gaps: every kind with lines written under a schema that predates one of its current
    /// fields, e.g. `AmmSwap:cashback_not_recorded(schema<3)=N`. Empty = every line carried every field.
    /// Included in [`Self::incomplete`]; NOT counted in `read_event_stream`'s `skipped` (every event of such a
    /// line is delivered, so the legacy replay input and its outputs do not move).
    #[must_use]
    pub fn schema_gaps(&self) -> Vec<String> {
        self.by_kind
            .iter()
            .filter(|(_, c)| c.schema_gap > 0)
            .map(|(k, c)| {
                format!(
                    "{k}:cashback_not_recorded(schema<{CASHBACK_SCHEMA_VERSION})={}",
                    c.schema_gap
                )
            })
            .collect()
    }
}

fn line_kind(line: &str) -> (String, Option<u64>) {
    let v = serde_json::from_str::<Value>(line).ok();
    let kind = v
        .as_ref()
        .and_then(|v| v.get("kind").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| "<unparseable>".to_string());
    let ver = v.as_ref().and_then(|v| v.get("v").and_then(Value::as_u64));
    (kind, ver)
}

/// Read every line. v2 lines decode through [`decode`]; unversioned v1 lines go through the legacy
/// reader and, when they parse, are counted `lossy_v1` (still incomplete for critical kinds).
#[must_use]
pub fn read_checked(
    text: &str,
    legacy: impl Fn(&str) -> Result<AppEvent, String>,
) -> CheckedStream {
    let mut s = CheckedStream::default();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            s.blank_lines = s.blank_lines.saturating_add(1);
            continue;
        }
        let (kind, version) = line_kind(line);
        // Version is read from the parsed object (serde_json orders keys, so never sniff a prefix).
        // Any versioned line goes to the v2 decoder, which rejects an unsupported version by name.
        let is_v2 = version.is_some();
        let vc = s.versions.entry(version.unwrap_or(1)).or_insert(0);
        *vc = vc.saturating_add(1);
        let c = s.by_kind.entry(kind.clone()).or_default();
        c.written = c.written.saturating_add(1);
        let res = if is_v2 {
            decode(line).map(|(_, e)| e)
        } else {
            legacy(line)
        };
        match res {
            Ok(e) => {
                c.parsed = c.parsed.saturating_add(1);
                if !is_v2 {
                    c.lossy_v1 = c.lossy_v1.saturating_add(1);
                }
                if matches!(
                    e,
                    AppEvent::AmmSwap {
                        cashback: CashbackField::NotRecorded,
                        ..
                    }
                ) && version.is_some_and(|v| v < CASHBACK_SCHEMA_VERSION)
                {
                    c.schema_gap = c.schema_gap.saturating_add(1);
                }
                s.events.push(e);
            }
            Err(why) => {
                c.rejected = c.rejected.saturating_add(1);
                let why = if is_v2 { why } else { format!("v1: {why}") };
                let r = s.reasons.entry(kind).or_default();
                if r.len() < 8 || r.contains_key(&why) {
                    let n = r.entry(why).or_insert(0);
                    *n = n.saturating_add(1);
                }
            }
        }
    }
    s
}
