//! EXPERIMENT_SPEC_ABSORPTION — the one declared pre-decision quantity and the two research
//! model sources built on it. Nothing here is consulted by arm A (unchanged Qwen).
//!
//! R = sell_volume_lamports / quote_reserves_lamports, read from the EXACT rendered prompt bytes
//! (the fields the model received), never recomputed from another state. `sell_volume_lamports`
//! is the corpus/renderer cumulative SELL quote volume since the mint's first captured trade
//! (curve + AMM); `quote_reserves_lamports` is the RAW quote vault of the AMM snapshot. R therefore
//! measures historical sell turnover relative to the current raw vault — it is NOT a measure of
//! what this pool absorbed or of remaining absorption capacity (see the spec's hypothesis note).

use pump_quant_inference::{Completion, InferenceError};

/// Parse `key=value` tokens of a rendered user prompt.
fn field<'a>(user: &'a str, key: &str) -> Option<&'a str> {
    user.split_whitespace()
        .filter_map(|tok| tok.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

/// R in basis points plus its components, or `None` when either component is missing, non-numeric
/// or the denominator is zero (UNAVAILABLE — never treated as zero risk).
pub fn ratio_bps(user: &str) -> Option<(u64, u64, u64)> {
    let sell: u64 = field(user, "sell_volume_lamports")?.parse().ok()?;
    let vault: u64 = field(user, "quote_reserves_lamports")?.parse().ok()?;
    if vault == 0 {
        return None;
    }
    let r = (u128::from(sell) * 10_000 / u128::from(vault)).min(u128::from(u64::MAX)) as u64;
    Some((r, sell, vault))
}

/// ARM C — transparent no-LLM whole-strategy benchmark as a [`ModelSource`]: emits the exact
/// `probe_sl_v1_compact` record so the unchanged authority/sizing/management path runs it.
/// Rule: BUY SMALL iff pricing_eligible=true AND net_flow_sol_300s > 0 AND ret_30s_bp > 0 AND
/// R <= R*; any missing/unparseable field or unavailable R → SKIP. SIZE is always SMALL (a
/// fixed tier, not a model sizing output). Its timing differs from Qwen's (no inference latency);
/// that difference is part of the whole-strategy comparison and is disclosed, not corrected.
pub struct RuleSource {
    pub r_star_bps: u64,
}

impl RuleSource {
    pub fn decide(&self, user: &str) -> &'static str {
        let eligible = field(user, "pricing_eligible") == Some("true");
        let flow_pos = field(user, "net_flow_sol_300s").and_then(|v| v.parse::<f64>().ok()).is_some_and(|x| x > 0.0);
        let ret_pos = field(user, "ret_30s_bp").and_then(|v| v.parse::<i64>().ok()).is_some_and(|x| x > 0);
        let r_ok = ratio_bps(user).is_some_and(|(r, _, _)| r <= self.r_star_bps);
        if eligible && flow_pos && ret_pos && r_ok {
            "DECISION: BUY\nSIZE: SMALL"
        } else {
            "DECISION: SKIP"
        }
    }
}

impl crate::model_authority::ModelSource for RuleSource {
    fn complete(&self, _system: &str, user: &str) -> Result<String, InferenceError> {
        Ok(self.decide(user).to_string())
    }
    fn complete_meta(&self, _system: &str, user: &str) -> Result<Completion, InferenceError> {
        let text = self.decide(user).to_string();
        Ok(Completion {
            completion_tokens: Some(text.split_whitespace().count() as u32),
            text,
            finish_reason: Some("stop".into()),
            stop_reason: Some(pump_quant_inference::probe_sl::PROBE_SL_V1_COMPACT_STOP.into()),
        })
    }
}

/// Shared inference for arms whose rendered prompt bytes are identical: the first caller pays the
/// inference; identical (system, user) pairs within `ttl_ms` of wall time receive the SAME completion.
/// Arm-dependent state (cash, positions) is not in the entry prompt, so A and B share every call;
/// hit/miss counters are exposed for the serving-load account. A miss is never served stale.
pub struct MemoSource<S: crate::model_authority::ModelSource + Send + Sync> {
    inner: S,
    ttl_ms: u128,
    memo: std::sync::Mutex<std::collections::HashMap<u64, (u128, Completion)>>,
    pub hits: std::sync::atomic::AtomicU64,
    pub misses: std::sync::atomic::AtomicU64,
}

impl<S: crate::model_authority::ModelSource + Send + Sync> MemoSource<S> {
    pub fn new(inner: S, ttl_ms: u128) -> Self {
        Self { inner, ttl_ms, memo: std::sync::Mutex::new(Default::default()), hits: Default::default(), misses: Default::default() }
    }
    fn key(system: &str, user: &str) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        system.hash(&mut h);
        user.hash(&mut h);
        h.finish()
    }
    fn now_ms() -> u128 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
    }
    fn lookup(&self, k: u64) -> Option<Completion> {
        let mut m = self.memo.lock().ok()?;
        let now = Self::now_ms();
        m.retain(|_, (t, _)| now.saturating_sub(*t) <= self.ttl_ms);
        m.get(&k).map(|(_, c)| c.clone())
    }
    fn store(&self, k: u64, c: &Completion) {
        if let Ok(mut m) = self.memo.lock() {
            m.insert(k, (Self::now_ms(), c.clone()));
        }
    }
}

impl<S: crate::model_authority::ModelSource + Send + Sync> crate::model_authority::ModelSource for MemoSource<S> {
    fn complete(&self, system: &str, user: &str) -> Result<String, InferenceError> {
        self.complete_meta(system, user).map(|c| c.text)
    }
    fn complete_meta(&self, system: &str, user: &str) -> Result<Completion, InferenceError> {
        let k = Self::key(system, user);
        if let Some(c) = self.lookup(k) {
            self.hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Ok(c);
        }
        self.misses.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let c = self.inner.complete_meta(system, user)?;
        self.store(k, &c);
        Ok(c)
    }
    fn complete_meta_within(&self, system: &str, user: &str, budget: std::time::Duration, request_id: &str) -> Result<Completion, InferenceError> {
        let k = Self::key(system, user);
        if let Some(c) = self.lookup(k) {
            self.hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Ok(c);
        }
        self.misses.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let c = self.inner.complete_meta_within(system, user, budget, request_id)?;
        self.store(k, &c);
        Ok(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ratio_reads_exact_prompt_fields_and_names_unavailable() {
        assert_eq!(ratio_bps("x=1 sell_volume_lamports=1500000000 quote_reserves_lamports=1000000000"), Some((15_000, 1_500_000_000, 1_000_000_000)));
        assert_eq!(ratio_bps("sell_volume_lamports=5 quote_reserves_lamports=0"), None);
        assert_eq!(ratio_bps("sell_volume_lamports=5"), None);
        assert_eq!(ratio_bps("sell_volume_lamports=n/a quote_reserves_lamports=7"), None);
    }
    #[test]
    fn rule_source_is_explicit_and_skips_on_missing() {
        let r = RuleSource { r_star_bps: 10_000 };
        let ok = "pricing_eligible=true net_flow_sol_300s=3.5 ret_30s_bp=120 sell_volume_lamports=4 quote_reserves_lamports=10";
        assert_eq!(r.decide(ok), "DECISION: BUY\nSIZE: SMALL");
        assert_eq!(r.decide(&ok.replace("ret_30s_bp=120", "ret_30s_bp=0")), "DECISION: SKIP");
        assert_eq!(r.decide(&ok.replace("quote_reserves_lamports=10", "quote_reserves_lamports=0")), "DECISION: SKIP");
        assert_eq!(r.decide(&ok.replace("sell_volume_lamports=4", "sell_volume_lamports=20")), "DECISION: SKIP");
        assert_eq!(r.decide(&ok.replace(" net_flow_sol_300s=3.5", "")), "DECISION: SKIP");
        let c = crate::model_authority::ModelSource::complete_meta(&r, "", ok).unwrap();
        assert!(pump_quant_inference::probe_sl::validate_probe_sl_entry_compact(c.finish_reason.as_deref(), c.stop_reason.as_deref(), &c.text, c.completion_tokens).is_ok());
    }
    struct Counting(std::sync::atomic::AtomicU64);
    impl crate::model_authority::ModelSource for Counting {
        fn complete(&self, _s: &str, _u: &str) -> Result<String, InferenceError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok("DECISION: SKIP".into())
        }
    }
    #[test]
    fn memo_shares_identical_prompts_only() {
        let m = MemoSource::new(Counting(Default::default()), 5_000);
        let _ = crate::model_authority::ModelSource::complete_meta(&m, "s", "u1");
        let _ = crate::model_authority::ModelSource::complete_meta(&m, "s", "u1");
        let _ = crate::model_authority::ModelSource::complete_meta(&m, "s", "u2");
        assert_eq!(m.inner.0.load(std::sync::atomic::Ordering::Relaxed), 2);
        assert_eq!(m.hits.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(m.misses.load(std::sync::atomic::Ordering::Relaxed), 2);
    }
}
