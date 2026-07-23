//! `pq-evaluator` — the §44/§62 canonical evaluator artifact.
//!
//! Reads a recorded decision/outcome tape (JSONL, from a path argument or stdin),
//! runs the frozen evaluator verdict leaves in `pump-quant-evaluator`, and prints
//! a graded report as a single JSON object: reconciled net-SOL, whether the
//! challenger defeats the §52 baseline family, and the §51 FDR/PBO promotion
//! verdict. It also prints its own evaluator hash and the tape's strategy hash so
//! the supervisor's `artifacts.py` trust-on-first-use pin can bind.
//!
//! The binary is deliberately thin: parsing lives in `tape`, every statistic in
//! the library leaves. It is deterministic and integer-only in the outcome path —
//! no floats reach the report (§22). Exit code is `0` on success, `2` on a bad
//! tape or bad arguments.

use std::io::Read;
use std::process::ExitCode;

use pump_quant_evaluator::baseline_destruction::{
    baseline_destruction, Competitor, DestructionVerdict,
};
use pump_quant_evaluator::baseline_family::{as_competitors, run_family, FamilyParams, FeeModel};
use pump_quant_evaluator::evaluator_pin::fnv1a_64;
use pump_quant_evaluator::evaluator_stats::{net_sol, Lane};
use pump_quant_evaluator::promotion_verdict::{promotion_verdict, PromotionBlockReason};
use pump_quant_evaluator::tape::{parse_jsonl, Tape};

/// Evaluator-release identity string. Its hash is the `evaluator_hash` the
/// supervisor can pin — a change to the frozen evaluator's release bumps this.
const EVALUATOR_RELEASE_ID: &[u8] = b"pq-evaluator/frozen-leaves/v1";

/// Default BH-FDR level: 0.05 == 50_000 ppm (§51).
const DEFAULT_ALPHA_PPM: u32 = 50_000;
/// Default PBO block threshold: 50% == 5_000 bps (§51).
const DEFAULT_PBO_THRESHOLD_BPS: u32 = 5_000;

struct Args {
    path: Option<String>,
    alpha_ppm: u32,
    pbo_threshold_bps: u32,
    required_margin: i128,
    per_entry_fee: u128,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        path: None,
        alpha_ppm: DEFAULT_ALPHA_PPM,
        pbo_threshold_bps: DEFAULT_PBO_THRESHOLD_BPS,
        required_margin: 0,
        per_entry_fee: 0,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--alpha-ppm" => a.alpha_ppm = next_num(&mut it, "--alpha-ppm")?,
            "--pbo-threshold-bps" => {
                a.pbo_threshold_bps = next_num(&mut it, "--pbo-threshold-bps")?
            }
            "--required-margin" => a.required_margin = next_num(&mut it, "--required-margin")?,
            "--per-entry-fee" => a.per_entry_fee = next_num(&mut it, "--per-entry-fee")?,
            "-h" | "--help" => return Err(usage()),
            other if other.starts_with('-') => {
                return Err(format!("unknown flag '{other}'\n{}", usage()))
            }
            other => a.path = Some(other.to_string()),
        }
    }
    Ok(a)
}

fn next_num<T: std::str::FromStr>(
    it: &mut impl Iterator<Item = String>,
    flag: &str,
) -> Result<T, String> {
    let raw = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
    raw.parse::<T>()
        .map_err(|_| format!("{flag}: '{raw}' is not a valid number"))
}

fn usage() -> String {
    "usage: pq-evaluator [TAPE.jsonl] [--alpha-ppm N] [--pbo-threshold-bps N] \
     [--required-margin N] [--per-entry-fee N]\n\
     reads the tape from the path argument, or from stdin if omitted."
        .to_string()
}

fn read_input(path: &Option<String>) -> Result<String, String> {
    match path {
        Some(p) => std::fs::read_to_string(p).map_err(|e| format!("cannot read '{p}': {e}")),
        None => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| format!("cannot read stdin: {e}"))?;
            Ok(buf)
        }
    }
}

fn reason_str(r: PromotionBlockReason) -> &'static str {
    match r {
        PromotionBlockReason::Clear => "clear",
        PromotionBlockReason::FdrOnly => "fdr_only",
        PromotionBlockReason::PboOnly => "pbo_only",
        PromotionBlockReason::Both => "both",
    }
}

fn run(args: &Args, input: &str) -> Result<String, String> {
    let tape: Tape = parse_jsonl(input).map_err(|e| e.to_string())?;

    // Net-SOL per lane and total (integer lamports).
    let scalp = net_sol(&tape.trades, Lane::Scalp);
    let early = net_sol(&tape.trades, Lane::Early);
    let total_net = scalp.net_lamports + early.net_lamports;

    // §52 baseline family + destruction verdict.
    let fee = FeeModel::new(args.per_entry_fee);
    let family = run_family(&tape.baseline_events, &fee, &FamilyParams::default_params());
    let competitors: Vec<Competitor> = as_competitors(&family);
    let destruction = baseline_destruction(total_net, &competitors, args.required_margin);
    let baselines_defeated = destruction.defeats();
    let effective_margin = match destruction {
        DestructionVerdict::Defeats { effective_margin } => effective_margin,
        DestructionVerdict::Fails {
            effective_margin, ..
        } => effective_margin,
        DestructionVerdict::NoField => 0,
    };

    // §51 promotion statistical verdict (FDR + PBO). Candidate defaults to the
    // tape's declared candidate, else the lowest-id hypothesis, else 0.
    let candidate_id = tape
        .candidate_id
        .unwrap_or_else(|| tape.pvalues.iter().map(|h| h.id).min().unwrap_or(0));
    let verdict = promotion_verdict(
        &tape.pvalues,
        args.alpha_ppm,
        candidate_id,
        &tape.perf,
        args.pbo_threshold_bps,
    );

    // Grade: promotable only if it defeats the field AND clears both gates.
    let grade = if baselines_defeated && !verdict.blocks() {
        "promotable"
    } else if verdict.blocks() {
        "blocked_statistical"
    } else {
        "insufficient_baseline_margin"
    };

    let evaluator_hash = fnv1a_64(EVALUATOR_RELEASE_ID);
    let strategy_hash = fnv1a_64(input.as_bytes());

    // Hand-rolled JSON (integers/bools/fixed strings only — no float, no escape).
    let mut baselines_json = String::from("[");
    for (idx, r) in family.iter().enumerate() {
        if idx > 0 {
            baselines_json.push(',');
        }
        baselines_json.push_str(&format!(
            "{{\"kind\":\"{:?}\",\"entries\":{},\"net_lamports\":{}}}",
            r.kind, r.entries, r.net_lamports
        ));
    }
    baselines_json.push(']');

    let pbo_bps_json = match verdict.pbo_bps {
        Some(v) => v.to_string(),
        None => "null".to_string(),
    };

    let report = format!(
        "{{\
\"evaluator_hash\":\"{evaluator_hash:016x}\",\
\"strategy_hash\":\"{strategy_hash:016x}\",\
\"net_sol_lamports\":{total_net},\
\"net_sol_by_lane\":{{\"scalp\":{scalp_net},\"early\":{early_net}}},\
\"trades\":{n_trades},\
\"candidate_id\":{candidate_id},\
\"baselines_defeated\":{baselines_defeated},\
\"effective_margin\":{effective_margin},\
\"baselines\":{baselines_json},\
\"fdr_blocks\":{fdr_blocks},\
\"pbo_blocks\":{pbo_blocks},\
\"pbo_bps\":{pbo_bps_json},\
\"promotion_reason\":\"{reason}\",\
\"grade\":\"{grade}\"\
}}",
        scalp_net = scalp.net_lamports,
        early_net = early.net_lamports,
        n_trades = scalp.n + early.n,
        fdr_blocks = verdict.fdr_blocks,
        pbo_blocks = verdict.pbo_blocks,
        reason = reason_str(verdict.reason),
    );

    Ok(report)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let input = match read_input(&args.path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    match run(&args, &input) {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(2)
        }
    }
}
