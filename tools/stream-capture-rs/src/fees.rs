//! `fee-sampler` subcommand — priority-fee calibration records.
//!
//! Every [`FEE_SAMPLE_INTERVAL_SECS`] it calls BOTH:
//! * `getPriorityFeeEstimate` — Helius-only JSON-RPC extension (1 credit per
//!   call), with `includeAllPriorityFeeLevels`, `lookbackSlots: 150` and
//!   `evaluateEmptySlotAsZero` over the watched account keys (default: the
//!   PumpSwap AMM program and the pump.fun bonding-curve program);
//! * standard `getRecentPrioritizationFees` — any provider.
//!
//! and emits ONE versioned NDJSON calibration record
//! (`"record":"fee_calibration_v1"`): the Helius level ladder
//! (min/low/medium/high/veryHigh/unsafeMax) plus integer-EXACT p50/p90 over
//! the recent-fee samples (nearest-rank percentile, pure integer arithmetic —
//! §102: no floats anywhere in the math; level values are truncated to
//! integer micro-lamports at the parse edge).
//!
//! CONSUMER CONTRACT (SERVER_BUILD_MANIFEST §8): these records feed
//! `pump-quant-execution::ex_tip_compute`'s CalibrationStore at Phase-B. A
//! record older than 60 s MUST be treated as STALE by the consumer — the
//! sampler stamps `unix_ms` for exactly that check and deliberately does not
//! smooth or interpolate (raw ladder in, §6.3 discipline).
//!
//! Fail-open-as-absence: if one of the two methods fails (e.g. a non-Helius
//! provider answering `getPriorityFeeEstimate` with method-not-found), the
//! record carries `null` for that section and the loss is logged; if both
//! fail, nothing is emitted and the cadence continues. Fail-closed arming:
//! no provider configuration at all is exit [`EXIT_ARMING`].

use std::time::{Duration, Instant};

use crate::emit;
use crate::json::{self, Value};
use crate::rpc::{redact_url, RpcPool, Transport, UreqTransport};

/// Sampling cadence (seconds).
pub const FEE_SAMPLE_INTERVAL_SECS: u64 = 15;

/// PumpSwap AMM program id (default fee-context account).
pub const PUMPSWAP_PROGRAM: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";

/// pump.fun bonding-curve program id (default fee-context account).
pub const PUMP_PROGRAM: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

/// `lookbackSlots` for the Helius estimate (150 slots ≈ 60 s of mainnet).
pub const FEE_LOOKBACK_SLOTS: u64 = 150;

/// Fail-closed arming exit code (§18.8).
pub const EXIT_ARMING: u8 = 3;

// ---------------------------------------------------------- pure builders

fn push_key_array(out: &mut String, accounts: &[String]) {
    out.push('[');
    for (n, a) in accounts.iter().enumerate() {
        if n > 0 {
            out.push(',');
        }
        out.push('"');
        emit::escape_json_into(a, out);
        out.push('"');
    }
    out.push(']');
}

/// Params for `getPriorityFeeEstimate`. Pure.
#[must_use]
pub fn priority_fee_params(accounts: &[String]) -> String {
    let mut out = String::with_capacity(160 + accounts.len() * 48);
    out.push_str("[{\"accountKeys\":");
    push_key_array(&mut out, accounts);
    out.push_str(&format!(
        ",\"options\":{{\"includeAllPriorityFeeLevels\":true,\
         \"lookbackSlots\":{FEE_LOOKBACK_SLOTS},\"evaluateEmptySlotAsZero\":true}}}}]"
    ));
    out
}

/// Params for standard `getRecentPrioritizationFees`. Pure.
#[must_use]
pub fn recent_fees_params(accounts: &[String]) -> String {
    let mut out = String::with_capacity(8 + accounts.len() * 48);
    out.push('[');
    push_key_array(&mut out, accounts);
    out.push(']');
    out
}

// ----------------------------------------------------------- pure parsing

/// The Helius priority-fee level ladder, integer micro-lamports.
#[derive(Debug, PartialEq, Eq)]
pub struct FeeLevels {
    /// `min` level.
    pub min: u64,
    /// `low` level.
    pub low: u64,
    /// `medium` level.
    pub medium: u64,
    /// `high` level.
    pub high: u64,
    /// `veryHigh` level.
    pub very_high: u64,
    /// `unsafeMax` level.
    pub unsafe_max: u64,
}

fn rpc_result<'a>(v: &'a Value, body_tag: &str) -> Result<&'a Value, String> {
    if let Some(err) = v.get("error") {
        return Err(format!(
            "{body_tag} JSON-RPC error: {}",
            json::serialize(err)
        ));
    }
    v.get("result")
        .ok_or_else(|| format!("{body_tag}: no result member"))
}

/// Parse a `getPriorityFeeEstimate` response body (levels truncated to
/// integer micro-lamports at this edge — Helius returns them as floats).
/// Pure.
pub fn parse_fee_levels(body: &str) -> Result<FeeLevels, String> {
    let v = json::parse(body).map_err(|e| format!("getPriorityFeeEstimate: {e}"))?;
    let levels = rpc_result(&v, "getPriorityFeeEstimate")?
        .get("priorityFeeLevels")
        .ok_or("getPriorityFeeEstimate: no priorityFeeLevels (includeAllPriorityFeeLevels not honored?)")?;
    let take = |k: &str| -> Result<u64, String> {
        levels
            .get(k)
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("getPriorityFeeEstimate: missing level {k:?}"))
    };
    Ok(FeeLevels {
        min: take("min")?,
        low: take("low")?,
        medium: take("medium")?,
        high: take("high")?,
        very_high: take("veryHigh")?,
        unsafe_max: take("unsafeMax")?,
    })
}

/// Parse a `getRecentPrioritizationFees` response body into the raw fee
/// samples (integer micro-lamports, one per slot). Pure.
pub fn parse_recent_fees(body: &str) -> Result<Vec<u64>, String> {
    let v = json::parse(body).map_err(|e| format!("getRecentPrioritizationFees: {e}"))?;
    let arr = rpc_result(&v, "getRecentPrioritizationFees")?
        .as_array()
        .ok_or("getRecentPrioritizationFees: result is not an array")?;
    Ok(arr
        .iter()
        .filter_map(|item| item.get("prioritizationFee").and_then(Value::as_u64))
        .collect())
}

/// Nearest-rank percentile over UNSORTED samples — integer-exact (§102):
/// rank = ceil(p·n/100) via integer `div_ceil`, 1-clamped. `None`
/// for an empty sample set (absence, not zero). Pure.
#[must_use]
pub fn percentile_nearest_rank(samples: &[u64], p: u64) -> Option<u64> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let n = sorted.len() as u64;
    let rank = (p * n).div_ceil(100).max(1).min(n);
    Some(sorted[(rank - 1) as usize])
}

/// Build one `fee_calibration_v1` record line. Absent sections are `null`
/// (fail-open-as-absence — the consumer must not mistake loss for zero).
/// Pure.
#[must_use]
pub fn calibration_record(
    unix_ms: u64,
    provider_redacted: &str,
    levels: Option<&FeeLevels>,
    p50: Option<u64>,
    p90: Option<u64>,
) -> String {
    let mut out = String::with_capacity(256);
    out.push_str("{\"record\":\"fee_calibration_v1\",\"unix_ms\":");
    out.push_str(&unix_ms.to_string());
    out.push_str(",\"provider\":\"");
    emit::escape_json_into(provider_redacted, &mut out);
    out.push_str("\",\"levels\":");
    match levels {
        Some(l) => out.push_str(&format!(
            "{{\"min\":{},\"low\":{},\"medium\":{},\"high\":{},\"veryHigh\":{},\"unsafeMax\":{}}}",
            l.min, l.low, l.medium, l.high, l.very_high, l.unsafe_max
        )),
        None => out.push_str("null"),
    }
    let opt = |v: Option<u64>| v.map_or("null".to_string(), |n| n.to_string());
    out.push_str(",\"recent_fees_p50\":");
    out.push_str(&opt(p50));
    out.push_str(",\"recent_fees_p90\":");
    out.push_str(&opt(p90));
    out.push('}');
    out
}

// ----------------------------------------------------------------- runner

const USAGE: &str = "usage: pq-stream-capture fee-sampler [--accounts-file f] [--once]\n\
  env: RPC_URLS (comma-separated priority list) — or HELIUS_API_KEY alone to\n\
  derive the mainnet Helius URL. Neither set: exit 3 (fail-closed arming).\n\
  Default account keys: PumpSwap + pump.fun programs. Cadence: every\n\
  15s (FEE_SAMPLE_INTERVAL_SECS); getPriorityFeeEstimate costs 1 credit/call.";

/// One sampling pass: both methods, one record. Returns true when a record
/// was emitted.
fn sample_once(
    pool: &mut RpcPool,
    transport: &dyn Transport,
    now_ms: fn() -> u64,
    accounts: &[String],
    out: &mut impl std::io::Write,
) -> bool {
    let mut provider: Option<String> = None;
    let levels = match pool.call(
        transport,
        now_ms(),
        "getPriorityFeeEstimate",
        &priority_fee_params(accounts),
    ) {
        Ok(outcome) => {
            provider = Some(redact_url(&pool.providers()[outcome.provider_index].url));
            match parse_fee_levels(&outcome.body) {
                Ok(l) => Some(l),
                Err(e) => {
                    eprintln!("[pq-stream-capture] fee-sampler: {e}");
                    None
                }
            }
        }
        Err(e) => {
            eprintln!("[pq-stream-capture] fee-sampler: getPriorityFeeEstimate failed: {e}");
            None
        }
    };
    let (p50, p90) = match pool.call(
        transport,
        now_ms(),
        "getRecentPrioritizationFees",
        &recent_fees_params(accounts),
    ) {
        Ok(outcome) => {
            if provider.is_none() {
                provider = Some(redact_url(&pool.providers()[outcome.provider_index].url));
            }
            match parse_recent_fees(&outcome.body) {
                Ok(fees) => (
                    percentile_nearest_rank(&fees, 50),
                    percentile_nearest_rank(&fees, 90),
                ),
                Err(e) => {
                    eprintln!("[pq-stream-capture] fee-sampler: {e}");
                    (None, None)
                }
            }
        }
        Err(e) => {
            eprintln!("[pq-stream-capture] fee-sampler: getRecentPrioritizationFees failed: {e}");
            (None, None)
        }
    };
    if levels.is_none() && p50.is_none() {
        eprintln!("[pq-stream-capture] fee-sampler: no data this cadence tick (absence, not zero)");
        return false;
    }
    let record = calibration_record(
        now_ms(),
        provider.as_deref().unwrap_or("none"),
        levels.as_ref(),
        p50,
        p90,
    );
    emit::write_line(out, &record).is_ok()
}

/// Lane entry point. `now_ms` is the injected capture clock (§22).
pub fn run(args: &[String], now_ms: fn() -> u64) -> u8 {
    let mut accounts: Vec<String> = vec![PUMPSWAP_PROGRAM.to_string(), PUMP_PROGRAM.to_string()];
    let mut once = false;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--accounts-file" => {
                let Some(path) = it.next() else {
                    eprintln!("[pq-stream-capture] fee-sampler: --accounts-file needs a value");
                    eprintln!("{USAGE}");
                    return 2;
                };
                match crate::read_list_file(path) {
                    Ok(list) if !list.is_empty() => accounts = list,
                    Ok(_) => {
                        eprintln!("[pq-stream-capture] fee-sampler: empty accounts file");
                        return 2;
                    }
                    Err(e) => {
                        eprintln!("[pq-stream-capture] fee-sampler: {e}");
                        return 2;
                    }
                }
            }
            "--once" => once = true,
            other => {
                eprintln!("[pq-stream-capture] fee-sampler: unknown flag {other:?}");
                eprintln!("{USAGE}");
                return 2;
            }
        }
    }
    let urls = match std::env::var("RPC_URLS") {
        Ok(u) if !u.trim().is_empty() => u,
        _ => match std::env::var("HELIUS_API_KEY") {
            Ok(k) if !k.trim().is_empty() => {
                format!("https://mainnet.helius-rpc.com/?api-key={k}")
            }
            _ => {
                eprintln!(
                    "[pq-stream-capture] fee-sampler ARMING_FAILED: neither RPC_URLS nor \
                     HELIUS_API_KEY is set — refusing to start (fail-closed, exit {EXIT_ARMING})"
                );
                return EXIT_ARMING;
            }
        },
    };
    let mut pool = match RpcPool::from_urls_csv(&urls) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[pq-stream-capture] fee-sampler: {e}");
            return EXIT_ARMING;
        }
    };
    eprintln!(
        "[pq-stream-capture] fee-sampler: {} provider(s), {} account key(s), every {}s",
        pool.providers().len(),
        accounts.len(),
        FEE_SAMPLE_INTERVAL_SECS
    );
    let transport = UreqTransport::new();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    loop {
        let started = Instant::now();
        sample_once(&mut pool, &transport, now_ms, &accounts, &mut out);
        if once {
            return 0;
        }
        let elapsed = started.elapsed();
        let interval = Duration::from_secs(FEE_SAMPLE_INTERVAL_SECS);
        if elapsed < interval {
            std::thread::sleep(interval - elapsed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_fee_params_exact_shape() {
        let p = priority_fee_params(&[PUMPSWAP_PROGRAM.to_string()]);
        assert_eq!(
            p,
            "[{\"accountKeys\":[\"pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA\"],\
             \"options\":{\"includeAllPriorityFeeLevels\":true,\"lookbackSlots\":150,\
             \"evaluateEmptySlotAsZero\":true}}]"
        );
        assert!(json::parse(&p).is_ok());
    }

    #[test]
    fn recent_fees_params_shape() {
        assert_eq!(
            recent_fees_params(&["A".into(), "B".into()]),
            "[[\"A\",\"B\"]]"
        );
    }

    #[test]
    fn parses_fee_levels_truncating_floats_to_integer_micro_lamports() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"priorityFeeLevels":{"min":0.0,"low":1000.0,"medium":42007.5,"high":250000.0,"veryHigh":1500000.9,"unsafeMax":2000000000.0}}}"#;
        let l = parse_fee_levels(body).unwrap();
        assert_eq!(
            l,
            FeeLevels {
                min: 0,
                low: 1000,
                medium: 42007,
                high: 250_000,
                very_high: 1_500_000,
                unsafe_max: 2_000_000_000
            }
        );
    }

    #[test]
    fn fee_levels_missing_member_is_error_not_zero() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"priorityFeeLevels":{"min":1}}}"#;
        assert!(parse_fee_levels(body).unwrap_err().contains("low"));
        let no_levels = r#"{"jsonrpc":"2.0","id":1,"result":{"priorityFeeEstimate":100}}"#;
        assert!(parse_fee_levels(no_levels).is_err());
    }

    #[test]
    fn jsonrpc_error_surfaces_as_error() {
        let body =
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}"#;
        assert!(parse_fee_levels(body)
            .unwrap_err()
            .contains("Method not found"));
        assert!(parse_recent_fees(body).is_err());
    }

    #[test]
    fn parses_recent_fees() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":[{"slot":100,"prioritizationFee":0},{"slot":101,"prioritizationFee":5000},{"slot":102,"prioritizationFee":1000}]}"#;
        assert_eq!(parse_recent_fees(body).unwrap(), vec![0, 5000, 1000]);
    }

    #[test]
    fn percentile_is_nearest_rank_integer_exact() {
        let s = [10u64, 20, 30, 40];
        assert_eq!(percentile_nearest_rank(&s, 50), Some(20), "ceil(0.5*4)=2nd");
        assert_eq!(percentile_nearest_rank(&s, 90), Some(40), "ceil(0.9*4)=4th");
        assert_eq!(percentile_nearest_rank(&s, 100), Some(40));
        assert_eq!(
            percentile_nearest_rank(&s, 1),
            Some(10),
            "rank clamped to 1"
        );
        assert_eq!(percentile_nearest_rank(&[7], 50), Some(7));
        assert_eq!(percentile_nearest_rank(&[], 50), None, "absence, not zero");
        // Unsorted input is sorted internally.
        assert_eq!(percentile_nearest_rank(&[30, 10, 20], 50), Some(20));
    }

    #[test]
    fn calibration_record_exact_shape() {
        let l = FeeLevels {
            min: 0,
            low: 1,
            medium: 2,
            high: 3,
            very_high: 4,
            unsafe_max: 5,
        };
        assert_eq!(
            calibration_record(1000, "https://mainnet.helius-rpc.com", Some(&l), Some(7), Some(9)),
            "{\"record\":\"fee_calibration_v1\",\"unix_ms\":1000,\
             \"provider\":\"https://mainnet.helius-rpc.com\",\
             \"levels\":{\"min\":0,\"low\":1,\"medium\":2,\"high\":3,\"veryHigh\":4,\"unsafeMax\":5},\
             \"recent_fees_p50\":7,\"recent_fees_p90\":9}"
        );
    }

    #[test]
    fn calibration_record_absence_is_null_never_zero() {
        let rec = calibration_record(1, "none", None, None, None);
        assert!(rec.contains("\"levels\":null"));
        assert!(rec.contains("\"recent_fees_p50\":null"));
        assert!(json::parse(&rec).is_ok());
    }
}
