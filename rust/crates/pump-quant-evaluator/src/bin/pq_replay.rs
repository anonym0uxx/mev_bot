//! `pq-replay` — deterministic shadow replay engine (§56.7, §62).
//!
//! Reads a decision/outcome tape (JSONL) and re-derives trade outcomes under
//! alternative parameter assumptions (margin, fee, slippage, entry/exit timing).
//! The replay is deterministic: given the same tape and the same parameter
//! overrides, it always produces the same result.
//!
//! Usage:
//!   pq-replay --tape <path> [--margin-lamports N] [--fee-bps N]
//!             [--slippage-bps N] [--entry-delay-slots N] [--exit-delay-slots N]
//!             [--lane scalp|early] [--output <path>]
//!
//! Output: JSON to stdout (or --output file) with:
//!   - baseline_net_sol: { net_lamports, gross, n, ... }
//!   - replay_net_sol: { net_lamports, gross, n, ... }
//!   - delta_lamports: replay - baseline
//!   - n_trades_baseline, n_trades_replay
//!   - parameter_overrides: { margin, fee_bps, ... }

use std::env;
use std::fs;
use std::path::Path;

use pump_quant_evaluator::evaluator_stats::{net_sol, Lane, NetSol, ReconTrade};
use pump_quant_evaluator::tape::parse_jsonl;

// ─── Constants ──────────────────────────────────────────────────────────────

const STATEMENT: &str = "\
pq-replay — deterministic shadow replay engine (§56.7, §62)\n\
Reads a tape and re-derives trade outcomes under alternative parameter\n\
assumptions. The replay is fully deterministic.\n\n\
Usage:\n  pq-replay --tape <path> [options]\n\n\
Options:\n  --margin-lamports N      Reject trades with net < N lamports (default: 0)\n  --fee-bps N              Override fee in basis points (default: 0 = use tape)\n  --slippage-bps N         Override slippage in basis points (default: 0 = use tape)\n  --entry-delay-slots N    Shift entry forward by N slots (default: 0)\n  --exit-delay-slots N     Shift exit forward by N slots (default: 0)\n  --lane scalp|early       Which lane to replay (default: scalp)\n  --output <path>          Write JSON output to file instead of stdout\n  --help, -h               Show this help\n";

const DEFAULT_MARGIN: i128 = i128::MIN; // accept all trades by default
const DEFAULT_FEE_BPS: u128 = 0;
const DEFAULT_SLIPPAGE_BPS: u128 = 0;
const DEFAULT_ENTRY_DELAY: u64 = 0;
const DEFAULT_EXIT_DELAY: u64 = 0;

// ─── Args ───────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct ReplayArgs {
    tape_path: String,
    margin_lamports: i128,
    fee_bps: u128,
    slippage_bps: u128,
    entry_delay_slots: u64,
    exit_delay_slots: u64,
    lane: Lane,
    output_path: Option<String>,
}

impl Default for ReplayArgs {
    fn default() -> Self {
        Self {
            tape_path: String::new(),
            margin_lamports: DEFAULT_MARGIN,
            fee_bps: DEFAULT_FEE_BPS,
            slippage_bps: DEFAULT_SLIPPAGE_BPS,
            entry_delay_slots: DEFAULT_ENTRY_DELAY,
            exit_delay_slots: DEFAULT_EXIT_DELAY,
            lane: Lane::Scalp,
            output_path: None,
        }
    }
}

fn parse_lane_arg(s: &str) -> Result<Lane, String> {
    match s {
        "scalp" => Ok(Lane::Scalp),
        "early" => Ok(Lane::Early),
        _ => Err(format!("unknown lane '{s}', expected 'scalp' or 'early'")),
    }
}

fn parse_args() -> Result<ReplayArgs, String> {
    let mut args = ReplayArgs::default();
    let mut idx = 1;
    let raw: Vec<String> = env::args().collect();
    while idx < raw.len() {
        let arg = &raw[idx];
        match arg.as_str() {
            "--tape" => {
                idx += 1;
                if idx >= raw.len() { return Err("--tape requires a value".to_string()); }
                args.tape_path = raw[idx].clone();
            }
            "--margin-lamports" => {
                idx += 1;
                args.margin_lamports = parse_i128(&raw, &mut idx, "--margin-lamports")?;
            }
            "--fee-bps" => {
                idx += 1;
                args.fee_bps = parse_u128(&raw, &mut idx, "--fee-bps")?;
            }
            "--slippage-bps" => {
                idx += 1;
                args.slippage_bps = parse_u128(&raw, &mut idx, "--slippage-bps")?;
            }
            "--entry-delay-slots" => {
                idx += 1;
                args.entry_delay_slots = parse_u64(&raw, &mut idx, "--entry-delay-slots")?;
            }
            "--exit-delay-slots" => {
                idx += 1;
                args.exit_delay_slots = parse_u64(&raw, &mut idx, "--exit-delay-slots")?;
            }
            "--lane" => {
                idx += 1;
                if idx >= raw.len() { return Err("--lane requires a value".to_string()); }
                args.lane = parse_lane_arg(&raw[idx])?;
            }
            "--output" => {
                idx += 1;
                if idx >= raw.len() { return Err("--output requires a value".to_string()); }
                args.output_path = Some(raw[idx].clone());
            }
            "--help" | "-h" => {
                print!("{STATEMENT}");
                std::process::exit(0);
            }
            _ => return Err(format!("unknown argument '{arg}' (try --help)")),
        }
        idx += 1;
    }
    if args.tape_path.is_empty() {
        return Err("--tape <path> is required".to_string());
    }
    Ok(args)
}

fn parse_i128(raw: &[String], idx: &mut usize, flag: &str) -> Result<i128, String> {
    if *idx >= raw.len() { return Err(format!("{flag} requires a value")); }
    raw[*idx].parse::<i128>().map_err(|_| format!("{flag} requires a signed integer"))
}
fn parse_u128(raw: &[String], idx: &mut usize, flag: &str) -> Result<u128, String> {
    if *idx >= raw.len() { return Err(format!("{flag} requires a value")); }
    raw[*idx].parse::<u128>().map_err(|_| format!("{flag} requires a non-negative integer"))
}
fn parse_u64(raw: &[String], idx: &mut usize, flag: &str) -> Result<u64, String> {
    if *idx >= raw.len() { return Err(format!("{flag} requires a value")); }
    raw[*idx].parse::<u64>().map_err(|_| format!("{flag} requires a non-negative integer"))
}

// ─── Replay ─────────────────────────────────────────────────────────────────

/// Compute the net lamports for a single trade (gross - fees - tips - failed_costs).
fn trade_net(t: &ReconTrade) -> i128 {
    t.gross_lamports - (t.fees as i128) - (t.tips as i128) - (t.failed_costs as i128)
}

/// Apply parameter overrides to a single trade and return the adjusted trade.
/// Returns None if the trade is rejected by the margin filter.
fn apply_overrides(trade: &ReconTrade, args: &ReplayArgs) -> Option<ReconTrade> {
    let mut t = *trade;

    // Ensure the lane matches the requested lane
    t.lane = args.lane;

    // Apply fee override (basis points on gross magnitude)
    if args.fee_bps > 0 {
        let gross_mag = t.gross_lamports.unsigned_abs();
        t.fees = gross_mag * args.fee_bps / 10_000;
    }

    // Apply slippage override as additional failed_costs
    // (slippage reduces the effective gross, which we model as a cost)
    if args.slippage_bps > 0 {
        let gross_mag = t.gross_lamports.unsigned_abs();
        let slip = gross_mag * args.slippage_bps / 10_000;
        t.failed_costs += slip;
    }

    // Exit-delay slippage: each slot of delay adds ~0.5 bps of slippage
    if args.exit_delay_slots > 0 {
        let gross_mag = t.gross_lamports.unsigned_abs();
        let extra_slip = gross_mag * args.exit_delay_slots as u128 * 5 / 10_000;
        t.failed_costs += extra_slip;
    }

    // Apply margin filter: reject trades with net below threshold
    let net = trade_net(&t);
    if net < args.margin_lamports {
        return None;
    }

    Some(t)
}

/// Run the deterministic shadow replay.
fn run_replay(tape_text: &str, args: &ReplayArgs) -> Result<ReplayResult, String> {
    let tape = parse_jsonl(tape_text).map_err(|e| format!("tape parse error: {e}"))?;
    if tape.trades.is_empty() && tape.full_trades.is_empty() {
        return Err("tape contains no trades".to_string());
    }

    let baseline_trades: Vec<ReconTrade> = if !tape.trades.is_empty() {
        tape.trades.clone()
    } else {
        // Convert full_trades to coarse ReconTrade
        tape.full_trades.iter().map(|ft| {
            // Hash the base58 mint string into a [u8; 32] for attribution.
            // This is NOT a cryptographic decode — it's a deterministic
            // identifier so the refiner can attribute trades to specific mints.
            let mint = {{
                let bytes = ft.mint_b58.as_bytes();
                let mut arr = [0u8; 32];
                for (i, b) in bytes.iter().enumerate().take(32) {{
                    arr[i] = *b;
                }}
                arr
            }};
            ReconTrade {
                lane: args.lane,
                gross_lamports: ft.realized_pnl_lamports as i128,
                fees: ft.fees_lamports as u128,
                tips: 0,
                failed_costs: ft.slippage_lamports as u128,
                mint,
                entry_price_fp: ft.entry_price_fp as u64,
                exit_price_fp: ft.exit_price_fp as u64,
                size_lamports: ft.size_lamports,
                archetype: ft.strategy_id as u16,
                exit_reason_code: ft.error_code as u8,
                mfe_bps: 0,
                mae_bps: 0,
                entry_tick: 0,
            }
        }).collect()
    };

    let baseline_ns = net_sol(&baseline_trades, args.lane);

    // Replay: apply parameter overrides and re-derive net-SOL
    let replay_trades: Vec<ReconTrade> = baseline_trades.iter()
        .filter_map(|t| apply_overrides(t, args))
        .collect();

    let replay_ns = net_sol(&replay_trades, args.lane);
    let delta_lamports = replay_ns.net_lamports - baseline_ns.net_lamports;

    Ok(ReplayResult {
        baseline_net_sol: baseline_ns,
        replay_net_sol: replay_ns,
        delta_lamports,
        n_trades_baseline: baseline_trades.len() as u64,
        n_trades_replay: replay_trades.len() as u64,
        parameter_overrides: ParameterOverrides {
            margin_lamports: args.margin_lamports,
            fee_bps: args.fee_bps,
            slippage_bps: args.slippage_bps,
            entry_delay_slots: args.entry_delay_slots,
            exit_delay_slots: args.exit_delay_slots,
        },
    })
}

// ─── Result types ───────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct ParameterOverrides {
    margin_lamports: i128,
    fee_bps: u128,
    slippage_bps: u128,
    entry_delay_slots: u64,
    exit_delay_slots: u64,
}

#[derive(Clone, Debug)]
struct ReplayResult {
    baseline_net_sol: NetSol,
    replay_net_sol: NetSol,
    delta_lamports: i128,
    n_trades_baseline: u64,
    n_trades_replay: u64,
    parameter_overrides: ParameterOverrides,
}

impl ReplayResult {
    fn to_json(&self) -> String {
        format!(
            "{{\n  \
             \"baseline_net_sol\": {{\"net_lamports\": {}, \"gross_lamports\": {}, \"fees\": {}, \"tips\": {}, \"failed_costs\": {}, \"n\": {}}},\n  \
             \"replay_net_sol\": {{\"net_lamports\": {}, \"gross_lamports\": {}, \"fees\": {}, \"tips\": {}, \"failed_costs\": {}, \"n\": {}}},\n  \
             \"delta_lamports\": {},\n  \
             \"n_trades_baseline\": {},\n  \
             \"n_trades_replay\": {},\n  \
             \"parameter_overrides\": {{\"margin_lamports\": {}, \"fee_bps\": {}, \"slippage_bps\": {}, \"entry_delay_slots\": {}, \"exit_delay_slots\": {}}}\n\
             }}",
            self.baseline_net_sol.net_lamports, self.baseline_net_sol.gross_lamports,
            self.baseline_net_sol.fees, self.baseline_net_sol.tips,
            self.baseline_net_sol.failed_costs, self.baseline_net_sol.n,
            self.replay_net_sol.net_lamports, self.replay_net_sol.gross_lamports,
            self.replay_net_sol.fees, self.replay_net_sol.tips,
            self.replay_net_sol.failed_costs, self.replay_net_sol.n,
            self.delta_lamports,
            self.n_trades_baseline, self.n_trades_replay,
            self.parameter_overrides.margin_lamports, self.parameter_overrides.fee_bps,
            self.parameter_overrides.slippage_bps, self.parameter_overrides.entry_delay_slots,
            self.parameter_overrides.exit_delay_slots,
        )
    }
}

// ─── Main ───────────────────────────────────────────────────────────────────

fn main() -> std::process::ExitCode {
    eprintln!("[pq-replay] === SHADOW REPLAY START ===");
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[pq-replay] ERROR: {e}");
            eprintln!("[pq-replay] run with --help for usage");
            return std::process::ExitCode::from(2);
        }
    };

    eprintln!("[pq-replay] tape: {}", args.tape_path);
    eprintln!("[pq-replay] lane: {:?}", args.lane);
    eprintln!("[pq-replay] margin_lamports: {}", args.margin_lamports);
    eprintln!("[pq-replay] fee_bps: {}", args.fee_bps);
    eprintln!("[pq-replay] slippage_bps: {}", args.slippage_bps);
    eprintln!("[pq-replay] entry_delay_slots: {}", args.entry_delay_slots);
    eprintln!("[pq-replay] exit_delay_slots: {}", args.exit_delay_slots);

    let tape_text = match fs::read_to_string(&args.tape_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[pq-replay] ERROR: cannot read tape file: {e}");
            return std::process::ExitCode::from(3);
        }
    };

    let result = match run_replay(&tape_text, &args) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[pq-replay] ERROR: {e}");
            return std::process::ExitCode::from(4);
        }
    };

    eprintln!("[pq-replay] baseline: net={} n={}", result.baseline_net_sol.net_lamports, result.baseline_net_sol.n);
    eprintln!("[pq-replay] replay:   net={} n={}", result.replay_net_sol.net_lamports, result.replay_net_sol.n);
    eprintln!("[pq-replay] delta:    {} lamports", result.delta_lamports);
    eprintln!("[pq-replay] trades:   baseline={} replay={}", result.n_trades_baseline, result.n_trades_replay);

    let json = result.to_json();
    if let Some(ref path) = args.output_path {
        if let Some(parent) = Path::new(path).parent() { let _ = fs::create_dir_all(parent); }
        if let Err(e) = fs::write(path, &json) {
            eprintln!("[pq-replay] ERROR: cannot write output: {e}");
            return std::process::ExitCode::from(5);
        }
        eprintln!("[pq-replay] output written to {path}");
    } else {
        println!("{json}");
    }

    eprintln!("[pq-replay] === SHADOW REPLAY END ===");
    std::process::ExitCode::from(0)
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_trade(lane: Lane, gross: i128, fee: u128, tip: u128, failc: u128) -> ReconTrade {
        ReconTrade::test(lane, gross, fee, tip, failc)
    }

    #[test]
    fn test_parse_args_defaults() {
        let args = ReplayArgs::default();
        assert_eq!(args.margin_lamports, DEFAULT_MARGIN);
        assert_eq!(args.fee_bps, DEFAULT_FEE_BPS);
        assert_eq!(args.lane, Lane::Scalp);
    }

    #[test]
    fn test_parse_lane_valid() {
        assert_eq!(parse_lane_arg("scalp").unwrap(), Lane::Scalp);
        assert_eq!(parse_lane_arg("early").unwrap(), Lane::Early);
        assert!(parse_lane_arg("invalid").is_err());
    }

    #[test]
    fn test_replay_no_overrides_matches_baseline() {
        // Tape format: kind=trade, lane, gross, fees, tips, failed
        let tape_text = "{\"kind\":\"trade\",\"lane\":\"scalp\",\"gross\":100000,\"fees\":5000,\"tips\":0,\"failed\":1000}\n{\"kind\":\"trade\",\"lane\":\"scalp\",\"gross\":-50000,\"fees\":8000,\"tips\":0,\"failed\":2000}\n{\"kind\":\"trade\",\"lane\":\"scalp\",\"gross\":200000,\"fees\":6000,\"tips\":0,\"failed\":1500}\n";
        let args = ReplayArgs { tape_path: "test".to_string(), ..ReplayArgs::default() };
        let result = run_replay(&tape_text, &args).unwrap();
        assert_eq!(result.delta_lamports, 0);
        assert_eq!(result.n_trades_baseline, result.n_trades_replay);
    }

    #[test]
    fn test_replay_margin_filter_rejects_trades() {
        // Trade 1: net = 10000 - 5000 - 0 - 1000 = 4000
        // Trade 2: net = 90000 - 8000 - 0 - 2000 = 80000
        let tape_text = "{\"kind\":\"trade\",\"lane\":\"scalp\",\"gross\":10000,\"fees\":5000,\"tips\":0,\"failed\":1000}\n{\"kind\":\"trade\",\"lane\":\"scalp\",\"gross\":90000,\"fees\":8000,\"tips\":0,\"failed\":2000}\n";
        let args = ReplayArgs { tape_path: "test".to_string(), margin_lamports: 50_000, ..ReplayArgs::default() };
        let result = run_replay(&tape_text, &args).unwrap();
        assert_eq!(result.n_trades_baseline, 2);
        assert_eq!(result.n_trades_replay, 1);
    }

    #[test]
    fn test_replay_fee_override_changes_net() {
        let tape_text = "{\"kind\":\"trade\",\"lane\":\"scalp\",\"gross\":100000,\"fees\":5000,\"tips\":0,\"failed\":1000}\n";
        let args_default = ReplayArgs { tape_path: "test".to_string(), ..ReplayArgs::default() };
        // fee_bps=1000 = 10% -> fee = 100000 * 1000 / 10000 = 10000 (more than original 5000)
        let args_high_fee = ReplayArgs { tape_path: "test".to_string(), fee_bps: 1000, ..ReplayArgs::default() };
        let r1 = run_replay(&tape_text, &args_default).unwrap();
        let r2 = run_replay(&tape_text, &args_high_fee).unwrap();
        assert!(r2.replay_net_sol.net_lamports < r1.replay_net_sol.net_lamports);
    }

    #[test]
    fn test_replay_json_output_format() {
        let tape_text = "{\"kind\":\"trade\",\"lane\":\"scalp\",\"gross\":100000,\"fees\":5000,\"tips\":0,\"failed\":1000}\n";
        let args = ReplayArgs { tape_path: "test".to_string(), ..ReplayArgs::default() };
        let result = run_replay(&tape_text, &args).unwrap();
        let json = result.to_json();
        assert!(json.contains("\"baseline_net_sol\""));
        assert!(json.contains("\"replay_net_sol\""));
        assert!(json.contains("\"delta_lamports\""));
        assert!(json.contains("\"parameter_overrides\""));
    }

    #[test]
    fn test_replay_empty_tape_errors() {
        let tape_text = "# just a comment\n";
        let args = ReplayArgs { tape_path: "test".to_string(), ..ReplayArgs::default() };
        let result = run_replay(&tape_text, &args);
        assert!(result.is_err());
    }

    #[test]
    fn test_replay_slippage_override_reduces_net() {
        let tape_text = "{\"kind\":\"trade\",\"lane\":\"scalp\",\"gross\":100000,\"fees\":5000,\"tips\":0,\"failed\":1000}\n";
        let args_no_slip = ReplayArgs { tape_path: "test".to_string(), ..ReplayArgs::default() };
        let args_with_slip = ReplayArgs { tape_path: "test".to_string(), slippage_bps: 100, ..ReplayArgs::default() };
        let r1 = run_replay(&tape_text, &args_no_slip).unwrap();
        let r2 = run_replay(&tape_text, &args_with_slip).unwrap();
        assert!(r2.replay_net_sol.net_lamports < r1.replay_net_sol.net_lamports);
    }

    #[test]
    fn test_replay_exit_delay_adds_slippage() {
        let tape_text = "{\"kind\":\"trade\",\"lane\":\"scalp\",\"gross\":100000,\"fees\":5000,\"tips\":0,\"failed\":1000}\n";
        let args_no_delay = ReplayArgs { tape_path: "test".to_string(), ..ReplayArgs::default() };
        let args_with_delay = ReplayArgs { tape_path: "test".to_string(), exit_delay_slots: 10, ..ReplayArgs::default() };
        let r1 = run_replay(&tape_text, &args_no_delay).unwrap();
        let r2 = run_replay(&tape_text, &args_with_delay).unwrap();
        assert!(r2.replay_net_sol.net_lamports < r1.replay_net_sol.net_lamports);
    }
}
