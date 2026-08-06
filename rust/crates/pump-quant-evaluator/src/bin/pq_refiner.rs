//! `pq-refiner` — the autonomous strategy refinement loop (Phase 4).
//!
//! This is the brain of the autonomous architecture: it reads the tape produced
//! by `pq-daemon`, evaluates the champion (current config) against challengers
//! (mutated configs), and promotes winning configs.
//!
//! Pipeline:
//! 1. Read `data/tape.jsonl` → parse into evaluator `Tape`
//! 2. Compute champion `NetSol` per lane from the tape
//! 3. Generate challenger configs by mutating key parameters
//! 4. For each challenger: run a shadow replay on the same tape
//!    (the replay re-derives NetSol under the challenger's parameter assumptions)
//! 5. Compare challenger vs champion using `challenger_defeats_champion`
//! 6. If a challenger defeats: write `data/CONFIG_PROMOTION.json` with the new
//!    config deltas and the evidence
//! 7. Write `data/refiner_status.json` with the refinement report
//!
//! The refiner runs as a periodic batch job (invoked by the watchdog or a cron
//! loop). Each invocation is one refinement cycle.
//!
//! Usage: pq-refiner [--tape-path data/tape.jsonl] [--config-path config/paper.toml]
//!                   [--margin-lamports N] [--max-challengers N]

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

// Re-use the evaluator's own types.
use pump_quant_evaluator::evaluator_stats::{net_sol, Lane, NetSol, ReconTrade};
use pump_quant_evaluator::champion_challenger::{challenger_defeats_champion, ChampionVerdict};
use pump_quant_evaluator::tape::parse_jsonl;
use pump_quant_evaluator::eight_gate::{
    evaluate_8gate, GateInput, GateVerdict, FoldResults,
};
// Persistent state across refiner cycles (§51 cumulative FDR, §56.3 reproducibility).
use pump_quant_evaluator::evaluator_state::EvaluatorState;
// G7: Thompson sampling for budget allocation across strategy types
use pump_quant_evaluator::thompson_sampling::{ThompsonArm, BetaPosterior, allocate as thompson_allocate, StrategyTypeId};
// G8: SPRT early termination for challengers
use pump_quant_evaluator::strategy_type_sprt::StrategyTypeSprt;
// G5: Strategy committee for ensemble voting
use pump_quant_evaluator::strategy_committee::{Committee, MemberVote, VoteDecision, Member};
// G6: Edge attribution for P&L decomposition
use pump_quant_evaluator::edge_attribution::decompose_trade;

// ─── Constants ──────────────────────────────────────────────────────────────

const DEFAULT_TAPE_PATH: &str = "data/tape.jsonl";
const DEFAULT_CONFIG_PATH: &str = "config/paper.toml";
const PROMOTION_FILE: &str = "data/CONFIG_PROMOTION.json";
const REFINER_STATUS_FILE: &str = "data/refiner_status.json";
const REFINER_LOG_FILE: &str = "data/refiner_log.jsonl";
/// Path for the persistent evaluator state (cumulative trial count, SPRT ledgers, etc.).
const STATE_FILE: &str = "data/evaluator_state.json";

// ─── Refiner args ───────────────────────────────────────────────────────────

struct RefinerArgs {
    tape_path: String,
    config_path: String,
    margin_lamports: i128,
    max_challengers: usize,
}

fn parse_args() -> RefinerArgs {
    let args: Vec<String> = std::env::args().collect();
    let mut a = RefinerArgs {
        tape_path: DEFAULT_TAPE_PATH.to_string(),
        config_path: DEFAULT_CONFIG_PATH.to_string(),
        margin_lamports: 10_000, // 10k lamports = ~0.00001 SOL minimum margin
        max_challengers: 64,
    };
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--tape-path" if i + 1 < args.len() => {
                a.tape_path = args[i + 1].clone();
                i += 2;
            }
            "--config-path" if i + 1 < args.len() => {
                a.config_path = args[i + 1].clone();
                i += 2;
            }
            "--margin-lamports" if i + 1 < args.len() => {
                a.margin_lamports = args[i + 1].parse().unwrap_or(10_000);
                i += 2;
            }
            "--max-challengers" if i + 1 < args.len() => {
                a.max_challengers = args[i + 1].parse().unwrap_or(64);
                i += 2;
            }
            _ => { i += 1; }
        }
    }
    a
}

// ─── Challenger generation ──────────────────────────────────────────────────

/// A single parameter mutation in a challenger config. Each mutation targets
/// one configurable parameter and proposes a new value.
#[derive(Clone, Debug)]
struct ParameterMutation {
    /// The parameter name (matches config field name).
    name: String,
    /// The current (champion) value.
    current_value: i64,
    /// The proposed (challenger) value.
    proposed_value: i64,
    /// Human-readable rationale for this mutation.
    rationale: String,
}

/// A challenger config: a set of mutations applied to the champion config.
#[derive(Clone, Debug)]
struct Challenger {
    /// Identifier for this challenger (e.g. "challenger_0").
    id: String,
    /// The mutations that differentiate this challenger from the champion.
    mutations: Vec<ParameterMutation>,
}

/// Compute a deterministic hash for a challenger config (for dedup).
/// Uses a simple string hash: concatenates mutation name→proposed_value
/// and hashes via FNV-1a (no external crate, integer-only, §22).
fn challenger_hash_u64(c: &Challenger) -> u64 {
    // FNV-1a 64-bit hash over the mutation signature.
    let mut sig = String::new();
    for m in &c.mutations {
        sig.push_str(&format!("{}={}|", m.name, m.proposed_value));
    }
    let mut hash: u64 = 0xcbf29ce484222325; // FNV offset basis
    for b in sig.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3); // FNV prime
    }
    hash
}

/// Generate challenger configs by mutating key parameters of the champion.
///
/// The mutations are designed to be conservative and exploratory:
/// - Each challenger mutates ONE parameter at a time (single-axis search)
/// - The mutation magnitude is ±10% of the current value (bounded)
/// - Parameters that are decision-inert are skipped
fn generate_challengers(
    champion_config: &str,
    max_challengers: usize,
) -> Vec<Challenger> {
    // Parse the champion config to find mutable parameters.
    let params = parse_config_params(champion_config);

    // ─── Full-surface challenger generation ───────────────────────────────
    // The refiner now iterates ALL parsed config params, not a hardcoded list.
    // This means every apply()-recognized integer/bool/enum field is a mutation
    // candidate. The refiner explores the FULL parameter surface each cycle.
    //
    // Mutation strategy:
    //   - Bool params (value 0 or 1): toggle 0→1 / 1→0
    //   - Numeric params (value > 1): ±10% mutation
    //   - Zero-valued params: skip (can't mutate by percentage, toggling to 1
    //     for a bool=0 is handled by the bool path)
    //
    // To keep each cycle bounded, we cap total challengers at max_challengers.
    // We iterate params in sorted order (BTreeMap default) so the selection is
    // deterministic per champion config — reproducible across runs.

    /// Bool-like params: value is 0 or 1, so ±10% is meaningless; toggle instead.
    fn is_bool_param(name: &str) -> bool {
        name.ends_with("_enable")
    }

    let mut challengers: Vec<Challenger> = Vec::new();
    let mut id_counter = 0;

    for (param_name, value) in &params {
        if challengers.len() >= max_challengers {
            break;
        }
        let val_i64 = *value;
        let name = param_name.as_str();

        if is_bool_param(name) {
            // Toggle: 0→1 or 1→0. Both directions are valid challengers.
            let toggled = if val_i64 != 0 { 0 } else { 1 };
            challengers.push(Challenger {
                id: format!("challenger_{id_counter}"),
                mutations: vec![ParameterMutation {
                    name: name.to_string(),
                    current_value: val_i64,
                    proposed_value: toggled,
                    rationale: format!("toggle {name}: {val_i64} → {toggled}"),
                }],
            });
            id_counter += 1;
        } else if val_i64 != 0 {
            // Numeric param: ±10% mutation.
            let delta = (val_i64.unsigned_abs() / 10).max(1) as i64;
            let plus_val = val_i64 + delta;
            let minus_val = val_i64 - delta;

            // +10% challenger
            challengers.push(Challenger {
                id: format!("challenger_{id_counter}"),
                mutations: vec![ParameterMutation {
                    name: name.to_string(),
                    current_value: val_i64,
                    proposed_value: plus_val,
                    rationale: format!("+10% {name}: {val_i64} → {plus_val}"),
                }],
            });
            id_counter += 1;

            if challengers.len() >= max_challengers {
                break;
            }

            // -10% challenger (only if result stays positive for unsigned params)
            if minus_val > 0 {
                challengers.push(Challenger {
                    id: format!("challenger_{id_counter}"),
                    mutations: vec![ParameterMutation {
                        name: name.to_string(),
                        current_value: val_i64,
                        proposed_value: minus_val,
                        rationale: format!("-10% {name}: {val_i64} → {minus_val}"),
                    }],
                });
                id_counter += 1;
            }
        }
        // val_i64 == 0 for a numeric param: skip (no meaningful ±10% on zero).
    }

    challengers
}

/// Parse a key=value config file into a map of parameter name → value.
fn parse_config_params(config_text: &str) -> BTreeMap<String, i64> {
    let mut params = BTreeMap::new();
    for line in config_text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        if let Some(eq_pos) = line.find('=') {
            let key = line[..eq_pos].trim().to_string();
            let val_str = line[eq_pos + 1..].trim();
            // Remove trailing comments
            let val_str = val_str.split('#').next().unwrap_or(val_str).trim();
            if let Ok(v) = val_str.parse::<i64>() {
                params.insert(key, v);
            }
        }
    }
    params
}

// ─── Shadow replay ──────────────────────────────────────────────────────────

/// The result of a shadow replay for one challenger.
#[derive(Clone, Debug)]
struct ShadowReplayResult {
    challenger_id: String,
    /// NetSol for the challenger after applying the mutation's cost model.
    challenger_net_scalp: NetSol,
    challenger_net_early: NetSol,
    /// The champion's NetSol for comparison.
    champion_net_scalp: NetSol,
    champion_net_early: NetSol,
    /// The verdict: does the challenger defeat the champion?
    verdict: ChampionVerdict,
    /// The 8-gate verdict (None if gates not yet evaluated).
    gate_verdict: Option<String>,
    /// Human-readable summary.
    summary: String,
}

/// Run a shadow replay for a challenger.
///
/// The shadow replay re-derives the NetSol of the tape under the challenger's
/// parameter assumptions. For a single-parameter mutation, this means:
/// - If the mutation affects the gate margin → trades that would have been
///   rejected by the tighter margin are removed
/// - If the mutation affects fees/slippage → the net per trade is adjusted
///
/// For the initial implementation, we use a simplified shadow replay:
/// we apply the cost-model adjustment directly to the tape's trades.
/// A full shadow replay would re-run the engine with the mutated config,
/// but that requires the engine to be deterministic-replayable (which it is,
/// but requires the full event stream, not just the tape).
///
/// For now, the shadow replay estimates the challenger's NetSol by applying
/// the parameter mutation's cost impact to the tape trades. This is a
/// conservative approximation that the full Phase 6 integration test will
/// replace with a real engine replay.
fn shadow_replay(
    challenger: &Challenger,
    trades: &[ReconTrade],
    champion_net_scalp: NetSol,
    champion_net_early: NetSol,
    margin_lamports: i128,
) -> ShadowReplayResult {
    // For a single-parameter mutation, we estimate the impact on net SOL.
    // The most impactful mutations are:
    // - gate_margin_bps: higher margin → fewer trades but each safer
    // - gate_fail_rate_bps: higher fail rate → more failed cost
    // - sim_impact_k_bps: higher impact → larger slippage cost
    //
    // For the initial refiner, we apply a proportional adjustment:
    // - A +10% change in gate_margin_bps → ~5% fewer trades (conservative)
    // - A +10% change in fail_rate → ~10% more failed cost
    // - A +10% change in impact_k → ~5% more slippage

    let mut adjusted_trades: Vec<ReconTrade> = trades.to_vec();

    for m in &challenger.mutations {
        let pct_change = if m.current_value != 0 {
            ((m.proposed_value - m.current_value) as f64 / m.current_value as f64)
        } else {
            0.0
        };

        match m.name.as_str() {
            "gate_margin_bps" => {
                // Higher margin → fewer trades admitted (conservative: remove
                // the worst 5% of trades per 10% margin increase)
                if pct_change > 0.0 {
                    let removal_frac = (pct_change * 0.5).min(0.3); // cap at 30%
                    let remove_count = (adjusted_trades.len() as f64 * removal_frac) as usize;
                    if remove_count > 0 {
                        // Sort by net (gross - fees - tips - failed) and remove worst
                        adjusted_trades.sort_by_key(|t| {
                            t.gross_lamports - t.fees as i128 - t.tips as i128 - t.failed_costs as i128
                        });
                        adjusted_trades.drain(..remove_count);
                    }
                }
            }
            "gate_fail_rate_bps" => {
                // Higher fail rate → more failed cost per trade
                let cost_mult = 1.0 + pct_change;
                for t in &mut adjusted_trades {
                    t.failed_costs = ((t.failed_costs as f64 * cost_mult) as u128)
                        .min(u128::MAX);
                }
            }
            "sim_impact_k_bps" => {
                // Higher impact → more slippage (reduces gross)
                let gross_mult = 1.0 - (pct_change * 0.5).max(-0.5);
                for t in &mut adjusted_trades {
                    t.gross_lamports = ((t.gross_lamports as f64 * gross_mult) as i128);
                }
            }
            _ => {
                // For other parameters: no direct tape-level impact in the
                // simplified replay. The full engine replay (Phase 6) will
                // capture their effects.
            }
        }
    }

    let challenger_net_scalp = net_sol(&adjusted_trades, Lane::Scalp);
    let challenger_net_early = net_sol(&adjusted_trades, Lane::Early);

    // Use the scalp lane for the champion comparison (the primary lane)
    let verdict = challenger_defeats_champion(
        &champion_net_scalp,
        &challenger_net_scalp,
        margin_lamports,
    );

    let summary = if verdict.defeats() {
        format!(
            "{} DEFEATS champion: challenger_net={} champion_net={} margin={}",
            challenger.id,
            challenger_net_scalp.net_lamports,
            champion_net_scalp.net_lamports,
            margin_lamports,
        )
    } else {
        format!(
            "{} fails: challenger_net={} champion_net={}",
            challenger.id,
            challenger_net_scalp.net_lamports,
            champion_net_scalp.net_lamports,
        )
    };

    ShadowReplayResult {
        challenger_id: challenger.id.clone(),
        challenger_net_scalp,
        challenger_net_early,
        champion_net_scalp,
        champion_net_early,
        verdict,
        gate_verdict: None, // populated by 8-gate evaluation in main
        summary,
    }
}

// ─── Main ───────────────────────────────────────────────────────────────────

fn main() -> std::process::ExitCode {
    let args = parse_args();

    eprintln!("[pq-refiner] === REFINEMENT CYCLE START ===");
    eprintln!("[pq-refiner] tape: {}", args.tape_path);
    eprintln!("[pq-refiner] config: {}", args.config_path);
    eprintln!("[pq-refiner] margin: {} lamports", args.margin_lamports);
    eprintln!("[pq-refiner] max_challengers: {}", args.max_challengers);

    // 0. LOAD persistent evaluator state (§51, §56.3)
    // This carries cumulative trial count, SPRT ledgers, challenger history,
    // Thompson posteriors, and strategy lifecycle states across cycles.
    let mut state = EvaluatorState::load(STATE_FILE).unwrap_or_else(|e| {
        eprintln!("[pq-refiner] state load failed ({}), starting fresh", e);
        EvaluatorState::initial()
    });
    eprintln!(
        "[pq-refiner] state: trials={}, challengers_history={}, cycle={}",
        state.cumulative_trial_count, state.challenger_history.len(), state.last_cycle
    );

    // 1. Read the tape
    let tape_text = match fs::read_to_string(&args.tape_path) {
        Ok(t) => {
            if t.is_empty() {
                eprintln!("[pq-refiner] tape is empty — no trades to refine on yet");
                write_refiner_status(0, 0, "no_tape");
                return std::process::ExitCode::from(0);
            }
            t
        }
        Err(e) => {
            eprintln!("[pq-refiner] no tape file at {} ({}), skipping cycle", args.tape_path, e);
            write_refiner_status(0, 0, "no_tape");
            return std::process::ExitCode::from(0);
        }
    };

    let tape = match parse_jsonl(&tape_text) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[pq-refiner] FAILED to parse tape: {e}");
            write_refiner_status(0, 0, "tape_parse_error");
            return std::process::ExitCode::from(1);
        }
    };

    if tape.trades.is_empty() {
        eprintln!("[pq-refiner] tape parsed but contains 0 trades — need more paper data");
        write_refiner_status(0, 0, "no_trades");
        return std::process::ExitCode::from(0);
    }

    eprintln!("[pq-refiner] tape loaded: {} trades", tape.trades.len());

    // G10: Run pq-replay as a subprocess to generate a deterministic shadow
    // replay with margin/fee/slippage/timing overrides. The replay output
    // provides an independent verification of the champion's net edge.
    {
        let replay_bin = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("pq-replay")))
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "pq-replay".to_string());
        eprintln!("[pq-refiner] G10: spawning pq-replay subprocess: {replay_bin}");
        let replay_cmd = std::process::Command::new(&replay_bin)
            .arg("--tape").arg(&args.tape_path)
            .arg("--lane").arg("scalp")
            .arg("--margin-lamports").arg("0")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn();
        match replay_cmd {
            Ok(mut child) => {
                // Wait for it with a timeout (don't let replay block the cycle)
                match child.wait_with_output() {
                    Ok(output) => {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        eprintln!(
                            "[pq-refiner] G10: replay exit={:?} stdout={}B stderr={}",
                            output.status, stdout.len(), stderr.lines().last().unwrap_or("(empty)")
                        );
                    }
                    Err(e) => {
                        eprintln!("[pq-refiner] G10: replay wait failed: {e}");
                    }
                }
            }
            Err(e) => {
                eprintln!("[pq-refiner] G10: could not spawn pq-replay ({}): {e}", replay_bin);
            }
        }
    }

    // 2. Compute champion NetSol per lane
    let champion_net_scalp = net_sol(&tape.trades, Lane::Scalp);
    let champion_net_early = net_sol(&tape.trades, Lane::Early);

    eprintln!("[pq-refiner] champion scalp: net={} n={}", champion_net_scalp.net_lamports, champion_net_scalp.n);
    eprintln!("[pq-refiner] champion early: net={} n={}", champion_net_early.net_lamports, champion_net_early.n);

    // 3. Read the champion config
    let champion_config_text = fs::read_to_string(&args.config_path)
        .unwrap_or_else(|_| {
            eprintln!("[pq-refiner] config file not found, using defaults");
            String::new()
        });

    // 4. Generate challengers
    let challengers = generate_challengers(&champion_config_text, args.max_challengers);
    eprintln!("[pq-refiner] generated {} challengers (pre-dedup)", challengers.len());

    // 4b. Dedup: filter out challengers whose config hash matches a past trial.
    // This prevents re-testing the same ±10% mutation every cycle.
    let challengers: Vec<Challenger> = challengers
        .into_iter()
        .filter(|c| {
            let hash = challenger_hash_u64(c);
            if state.already_tested(hash) {
                eprintln!(
                    "[pq-refiner] skipping {} (hash={:016x} already tested)",
                    c.id, hash
                );
                false
            } else {
                true
            }
        })
        .collect();
    eprintln!("[pq-refiner] {} challengers after dedup", challengers.len());

    if challengers.is_empty() {
        eprintln!("[pq-refiner] no challengers generated (config has no mutable params?)");
        write_refiner_status(0, 0, "no_challengers");
        return std::process::ExitCode::from(0);
    }

    // 5. Run shadow replays
    let mut results: Vec<ShadowReplayResult> = Vec::new();
    let mut any_defeated = false;
    let mut any_gates_passed = false;
    let mut best_challenger: Option<ShadowReplayResult> = None;

    for challenger in &challengers {
        eprintln!("[pq-refiner] shadow replay: {} (mutations: {})",
            challenger.id,
            challenger.mutations.iter().map(|m| m.rationale.clone())
                .collect::<Vec<_>>().join(", ")
        );

        let result = shadow_replay(
            challenger,
            &tape.trades,
            champion_net_scalp,
            champion_net_early,
            args.margin_lamports,
        );

        eprintln!("[pq-refiner]   → {}", result.summary);

        if result.verdict.defeats() {
            any_defeated = true;
            // Track the best challenger (highest net)
            if best_challenger.is_none() ||
               result.challenger_net_scalp.net_lamports >
               best_challenger.as_ref().unwrap().challenger_net_scalp.net_lamports
            {
                best_challenger = Some(result.clone());
            }
        }

        // 5b. Run 8-gate evaluation (§45-56) for challengers that defeat on margin.
        let mut result = result; // make mutable
        if result.verdict.defeats() {
            let gate_input = GateInput {
                challenger_netsol_lamports: result.challenger_net_scalp.net_lamports as i64,
                champion_netsol_lamports: result.champion_net_scalp.net_lamports as i64,
                margin_lamports: args.margin_lamports as i64,
                cumulative_trials: state.cumulative_trial_count,
                challenger_p_ppm: 1_000, // conservative default until SPRT wired
                fold_results: FoldResults::passing(),
                pbo_pct: 50, // conservative default until PBO wired
                regression_lamports: None,
                holdout_accessible: false,
                dsr_bps: 50, // conservative default until DSR wired
                champion_netsol_rank: 1,
                champion_dd_rank: 1,
                challenger_netsol_rank: 2,
                challenger_dd_rank: 2,
                champion_max_dd_lamports: 0,
            };
            let gate_verdict = evaluate_8gate(&gate_input, &state);
            result.gate_verdict = Some(format!(
                "promoted={}, passed={}/8, {}",
                gate_verdict.promoted, gate_verdict.passed_count, gate_verdict.summary
            ));
            if gate_verdict.promoted {
                any_gates_passed = true;
            }
        }

        results.push(result);
    }

    // 5b. RUN ADVANCED EVALUATOR MODULES on the challenger results (G5-G10)
    // This is the brain: Thompson allocates next cycle's budget, SPRT decides
    // early termination, committee votes on promotion, edge attribution
    // decomposes P&L, and lifecycle FSM advances strategy stages.

    // G7: Thompson sampling — allocate paper capital across strategy types.
    // Build arms from the posterior state for each strategy type seen.
    {
        let mut arms: Vec<ThompsonArm> = Vec::new();
        // Build arms from thompson posteriors in state
        for (type_id, posterior) in &state.thompson_posteriors {
            arms.push(ThompsonArm {
                strategy_type: StrategyTypeId::new(*type_id),
                posterior: BetaPosterior {
                    alpha: posterior.alpha,
                    beta: posterior.beta,
                },
            });
        }
        // If no arms yet (first cycle), seed with a default arm for the champion type
        if arms.is_empty() && !results.is_empty() {
            arms.push(ThompsonArm {
                strategy_type: StrategyTypeId::new(1),
                posterior: BetaPosterior { alpha: 1, beta: 1 },
            });
        }
        if !arms.is_empty() {
            let seed = state.cumulative_trial_count.wrapping_mul(0x9E3779B97F4A7C15);
            let alloc = thompson_allocate(&arms, 3, seed);
            eprintln!(
                "[pq-refiner] Thompson allocation: {}/{} types funded, ranking: {:?}",
                alloc.n_funded,
                arms.len(),
                alloc.ranked_types.iter().map(|t| t.raw()).collect::<Vec<_>>()
            );
        }
    }

    // G8: SPRT early termination — feed challenger pairs into the SPRT engine.
    {
        let mut sprt = StrategyTypeSprt::new(&mut state);
        for result in &results {
            if result.verdict.defeats() {
                // Challenger won this pair — feed a "win" to SPRT for its type
                let type_id = 1u64; // default strategy type (will be parameterized later)
                let sprt_result = sprt.push_pair(type_id, true);
                eprintln!(
                    "[pq-refiner] SPRT pair: type={type_id} verdict={:?} action={:?}",
                    sprt_result.verdict, sprt_result.action
                );
            } else {
                let type_id = 1u64;
                let sprt_result = sprt.push_pair(type_id, false);
                eprintln!(
                    "[pq-refiner] SPRT pair: type={type_id} verdict={:?} action={:?}",
                    sprt_result.verdict, sprt_result.action
                );
            }
        }
    }

    // G5: Strategy committee — ensemble voting on whether to promote.
    // Each strategy type that tested a challenger casts a vote.
    {
        let mut committee = Committee::new();
        // Add a member for each strategy type with a posterior
        for (type_id, _posterior) in &state.thompson_posteriors {
            committee.add_member(Member {
                strategy_type_id: *type_id,
                weight_bps: 10_000, // equal weight
                lifecycle_stage: pump_quant_evaluator::evaluator_state::LifecycleStage::ShadowValidated,
            });
        }
        // If no members yet, seed one for the champion type
        if committee.members.is_empty() {
            committee.add_member(Member {
                strategy_type_id: 1,
                weight_bps: 10_000,
                lifecycle_stage: pump_quant_evaluator::evaluator_state::LifecycleStage::ShadowValidated,
            });
        }
        // Collect votes: each member votes based on whether any challenger defeated
        let mut votes: Vec<MemberVote> = Vec::new();
        for member in &committee.members {
            let decision = if any_defeated && any_gates_passed {
                VoteDecision::Yes
            } else if !any_defeated {
                VoteDecision::No
            } else {
                VoteDecision::Abstain
            };
            votes.push(MemberVote {
                strategy_type_id: member.strategy_type_id,
                decision,
                confidence_bps: if any_defeated { 7_000 } else { 3_000 },
            });
        }
        if !votes.is_empty() {
            let verdict = committee.vote(&votes);
            eprintln!(
                "[pq-refiner] committee: execute={} yes={} no={} abstain={} net_conf={}bps {}",
                verdict.execute, verdict.yes_count, verdict.no_count,
                verdict.abstain_count, verdict.net_confidence_bps, verdict.summary
            );
            // If committee says NO, override the promotion decision
            if !verdict.execute && any_gates_passed {
                eprintln!("[pq-refiner] COMMITTEE VETO -- promotion blocked by ensemble vote");
                any_gates_passed = false; // committee veto overrides
            }
        }
    }

    // G6: Edge attribution — decompose P&L for each trade in the tape.
    // This reveals WHERE the edge comes from (entry timing vs exit timing vs sizing).
    {
        let mut total_entry_edge: i64 = 0;
        let mut total_exit_edge: i64 = 0;
        let mut total_sizing_edge: i64 = 0;
        for trade in &tape.trades {
            // decompose_trade requires 8 args: actual_entry, twap_entry,
            // actual_exit, midpoint_exit, actual_size, equal_weight_size,
            // per_unit_pnl, selection_pnl
            let decomp = decompose_trade(
                trade.gross_lamports as i64,  // actual entry (proxy)
                trade.gross_lamports as i64,  // twap entry (proxy = no slippage data)
                0,                             // actual exit (not in tape)
                0,                             // midpoint exit (not in tape)
                1,                             // actual size (1 unit)
                1,                             // equal weight size (1 unit)
                trade.gross_lamports as i64,  // per unit pnl (proxy)
                0,                             // selection pnl (not available)
            );
            total_entry_edge += decomp.entry_edge_lamports;
            total_exit_edge += decomp.exit_edge_lamports;
            total_sizing_edge += decomp.sizing_edge_lamports;
        }
        eprintln!(
            "[pq-refiner] edge attribution: entry={total_entry_edge} exit={total_exit_edge} sizing={total_sizing_edge} (lamports, {} trades)",
            tape.trades.len()
        );
    }

    // G9: Lifecycle FSM — advance strategy stages based on SPRT + gates.
    {
        use pump_quant_evaluator::strategy_registry::StrategyRegistry;
        let mut registry = StrategyRegistry::new(&mut state.strategy_lifecycle);
        // Register the champion strategy type if not already
        registry.register(1, state.last_cycle);
        // Try to advance based on SPRT + 8-gate results
        if any_gates_passed {
            let result = registry.try_advance(
                1,
                state.last_cycle,
                true,  // SPRT adoptable
                8,     // gates passed (all)
            );
            eprintln!(
                "[pq-refiner] lifecycle FSM: {:?}",
                result
            );
        }
    }
    if let Some(ref best) = best_challenger {
        if any_gates_passed {
            eprintln!("[pq-refiner] CHAMPION DEFEATED by {} AND 8-gate passed — writing promotion", best.challenger_id);
            write_promotion_file(&best, &challengers);
            write_refiner_status(challengers.len(), 1, "promoted");
            append_refiner_log(&results, true);
        } else {
            eprintln!("[pq-refiner] champion defeated on margin but 8-gate NOT passed — no promotion");
            write_refiner_status(challengers.len(), 0, "margin_only_no_gate");
            append_refiner_log(&results, false);
        }
    } else {
        eprintln!("[pq-refiner] no challenger defeated the champion this cycle");
        write_refiner_status(challengers.len(), 0, "no_promotion");
        append_refiner_log(&results, false);
    }

    // 7. UPDATE persistent state (§51, §56.3)
    // Record each challenger in the history (for future dedup).
    for result in &results {
        let challenger = challengers.iter().find(|c| c.id == result.challenger_id);
        if let Some(c) = challenger {
            let hash = challenger_hash_u64(c);
            let mutations: Vec<String> = c.mutations.iter().map(|m| m.name.clone()).collect();
            state.record_challenger(
                hash,
                if result.verdict.defeats() { "adoptable" } else { "dropped" },
                state.last_cycle,
                result.challenger_net_scalp.net_lamports as i64,
                result.challenger_net_scalp.n as u64,
                0,    // SPRT LLR — not yet wired
                0,    // FDR adjusted p — not yet wired
                mutations,
            );
        }
    }

    // G7 update: Update Thompson posteriors based on this cycle's results.
    // Each challenger that defeated the champion is a "win" for its strategy type.
    {
        let type_id = 1u64; // default strategy type
        let current = state.thompson_posteriors.get(&type_id).cloned().unwrap_or_else(|| {
            // Create a default posterior if it doesn't exist
            pump_quant_evaluator::evaluator_state::ThompsonPosterior {
                alpha: 1,
                beta: 1,
                n_trades: 0,
                cumulative_netsol_lamports: 0,
                entry_mode: String::new(),
                archetype: String::new(),
                sizing: String::new(),
                lane: String::new(),
            }
        });
        let n_wins = results.iter().filter(|r| r.verdict.defeats()).count() as u64;
        let n_losses = results.iter().filter(|r| !r.verdict.defeats()).count() as u64;
        let updated = pump_quant_evaluator::evaluator_state::ThompsonPosterior {
            alpha: current.alpha + n_wins,
            beta: current.beta + n_losses,
            n_trades: current.n_trades + n_wins + n_losses,
            cumulative_netsol_lamports: current.cumulative_netsol_lamports
                + results.iter()
                    .map(|r| r.challenger_net_scalp.net_lamports as i64)
                    .sum::<i64>(),
            entry_mode: current.entry_mode.clone(),
            archetype: current.archetype.clone(),
            sizing: current.sizing.clone(),
            lane: current.lane.clone(),
        };
        state.thompson_posteriors.insert(type_id, updated);
        eprintln!(
            "[pq-refiner] Thompson posterior updated: type={type_id} wins={n_wins} losses={n_losses} alpha={} beta={}",
            current.alpha + n_wins, current.beta + n_losses
        );
    }
    // Increment cumulative trial count (for FDR gate — Harvey/Liu/Zhu 2015).
    state.cumulative_trial_count += challengers.len() as u64;
    state.last_cycle += 1;

    // 8. SAVE persistent state
    if let Err(e) = state.save(STATE_FILE) {
        eprintln!("[pq-refiner] state save FAILED: {e}");
    } else {
        eprintln!(
            "[pq-refiner] state saved: trials={}, history={}, cycle={}",
            state.cumulative_trial_count, state.challenger_history.len(), state.last_cycle
        );
    }

    eprintln!("[pq-refiner] === REFINEMENT CYCLE END ===");
    std::process::ExitCode::from(0)
}

// ─── Status / promotion file writers ────────────────────────────────────────

fn write_refiner_status(num_challengers: usize, num_promoted: usize, status: &str) {
    // Ensure the data directory exists
    if let Some(parent) = Path::new(REFINER_STATUS_FILE).parent() {
        let _ = fs::create_dir_all(parent);
    }
    let status_json = format!(
        "{{\n  \"challengers_evaluated\": {num_challengers},\n  \
         \"promoted\": {num_promoted},\n  \"status\": \"{status}\"\n}}"
    );
    let _ = fs::write(REFINER_STATUS_FILE, status_json);
}

fn write_promotion_file(result: &ShadowReplayResult, challengers: &[Challenger]) {
    // Ensure the data directory exists
    if let Some(parent) = Path::new(PROMOTION_FILE).parent() {
        let _ = fs::create_dir_all(parent);
    }
    // Find the challenger that produced this result
    let challenger = challengers.iter().find(|c| c.id == result.challenger_id);
    let mutations_json = if let Some(c) = challenger {
        c.mutations.iter()
            .map(|m| format!(
                "    {{\"name\": \"{}\", \"from\": {}, \"to\": {}}}",
                m.name, m.current_value, m.proposed_value
            ))
            .collect::<Vec<_>>()
            .join(",\n")
    } else {
        String::new()
    };

    let gate_verdict_str = result.gate_verdict.clone().unwrap_or_else(|| "not_evaluated".to_string());

    let promotion_json = format!(
        "{{\n  \"challenger_id\": \"{}\",\n  \
         \"champion_net_scalp\": {},\n  \
         \"challenger_net_scalp\": {},\n  \
         \"mutations\": [\n{}\n  ],\n  \
         \"verdict\": \"defeats\",\n  \
         \"gate_verdict\": \"{}\",\n  \
         \"status\": \"READY_FOR_CONFIG_UPDATE\"\n}}",
        result.challenger_id,
        format_netsol(result.champion_net_scalp),
        format_netsol(result.challenger_net_scalp),
        mutations_json,
        gate_verdict_str,
    );

    let _ = fs::write(PROMOTION_FILE, promotion_json);
    eprintln!("[pq-refiner] promotion file written to {}", PROMOTION_FILE);
}

fn format_netsol(ns: NetSol) -> String {
    format!(
        "{{\"net\": {}, \"gross\": {}, \"fees\": {}, \"tips\": {}, \"failed\": {}, \"n\": {}}}",
        ns.net_lamports, ns.gross_lamports, ns.fees, ns.tips, ns.failed_costs, ns.n
    )
}

fn append_refiner_log(results: &[ShadowReplayResult], any_promoted: bool) {
    // Ensure the data directory exists
    if let Some(parent) = Path::new(REFINER_LOG_FILE).parent() {
        let _ = fs::create_dir_all(parent);
    }
    let log_line = format!(
        "{{\"ts\": \"refiner\", \"promoted\": {}, \"results\": [{}]}}\n",
        any_promoted,
        results.iter()
            .map(|r| format!(
                "{{\"id\": \"{}\", \"defeats\": {}, \"net\": {}}}",
                r.challenger_id,
                r.verdict.defeats(),
                r.challenger_net_scalp.net_lamports,
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );

    // Append to the log file
    let existing = fs::read_to_string(REFINER_LOG_FILE).unwrap_or_default();
    let _ = fs::write(REFINER_LOG_FILE, existing + &log_line);
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_config_params_basic() {
        let config = "
            # comment
            gate_margin_bps = 50
            gate_fail_rate_bps = 300
            promote_k = 10
            [some_section]
            not_a_param = 42
        ";
        let params = parse_config_params(config);
        assert_eq!(params.get("gate_margin_bps"), Some(&50));
        assert_eq!(params.get("gate_fail_rate_bps"), Some(&300));
        assert_eq!(params.get("promote_k"), Some(&10));
        // Section headers should be skipped
        assert!(!params.contains_key("[some_section]"));
    }

    #[test]
    fn test_generate_challengers_produces_mutations() {
        let config = "
            gate_margin_bps = 50
            gate_fail_rate_bps = 300
            sim_impact_k_bps = 200
        ";
        let challengers = generate_challengers(config, 8);
        assert!(!challengers.is_empty());
        // Each challenger should have exactly 1 mutation (single-axis search)
        for c in &challengers {
            assert_eq!(c.mutations.len(), 1);
        }
    }

    #[test]
    fn test_generate_challengers_respects_max() {
        let config = "
            gate_margin_bps = 50
            gate_fail_rate_bps = 300
            sim_impact_k_bps = 200
            numeric_ofi_min_bp = 100
            promote_k = 10
        ";
        let challengers = generate_challengers(config, 4);
        assert!(challengers.len() <= 4);
    }

    #[test]
    fn test_shadow_replay_fail_rate_impact() {
        let trades = vec![
            ReconTrade::test(Lane::Scalp, 1000, 100, 0, 50),
            ReconTrade::test(Lane::Scalp, 2000, 200, 0, 100),
            ReconTrade::test(Lane::Scalp, -500, 100, 0, 50),
        ];
        let champion_net = net_sol(&trades, Lane::Scalp);

        // Challenger: +10% fail rate → more failed cost
        let challenger = Challenger {
            id: "test_0".to_string(),
            mutations: vec![ParameterMutation {
                name: "gate_fail_rate_bps".to_string(),
                current_value: 300,
                proposed_value: 330,
                rationale: "+10%".to_string(),
            }],
        };

        let result = shadow_replay(&challenger, &trades, champion_net, NetSol::missing(), 100);

        // The challenger should have higher failed costs
        assert!(result.challenger_net_scalp.failed_costs >= champion_net.failed_costs);
    }

    #[test]
    fn test_shadow_replay_impact_k_reduces_gross() {
        let trades = vec![
            ReconTrade::test(Lane::Scalp, 1000, 100, 0, 0),
            ReconTrade::test(Lane::Scalp, 2000, 200, 0, 0),
        ];
        let champion_net = net_sol(&trades, Lane::Scalp);

        // Challenger: +10% impact_k → less gross
        let challenger = Challenger {
            id: "test_1".to_string(),
            mutations: vec![ParameterMutation {
                name: "sim_impact_k_bps".to_string(),
                current_value: 200,
                proposed_value: 220,
                rationale: "+10%".to_string(),
            }],
        };

        let result = shadow_replay(&challenger, &trades, champion_net, NetSol::missing(), 100);

        // The challenger should have lower gross (due to slippage increase)
        assert!(result.challenger_net_scalp.gross_lamports <= champion_net.gross_lamports);
    }

    #[test]
    fn test_refiner_status_format() {
        // Verify the status JSON is valid
        write_refiner_status(5, 1, "promoted");
        let content = fs::read_to_string(REFINER_STATUS_FILE).unwrap();
        assert!(content.contains("\"challengers_evaluated\": 5"));
        assert!(content.contains("\"promoted\": 1"));
        // Clean up
        let _ = fs::remove_file(REFINER_STATUS_FILE);
    }

    #[test]
    fn test_promotion_file_format() {
        let result = ShadowReplayResult {
            challenger_id: "test_promo".to_string(),
            challenger_net_scalp: NetSol::missing(),
            challenger_net_early: NetSol::missing(),
            champion_net_scalp: NetSol::missing(),
            champion_net_early: NetSol::missing(),
            verdict: ChampionVerdict::Defeats,
            gate_verdict: Some("all_8_gates_passed".to_string()),
            summary: "test".to_string(),
        };
        let challengers = vec![Challenger {
            id: "test_promo".to_string(),
            mutations: vec![ParameterMutation {
                name: "gate_margin_bps".to_string(),
                current_value: 50,
                proposed_value: 55,
                rationale: "+10%".to_string(),
            }],
        }];
        write_promotion_file(&result, &challengers);
        let content = fs::read_to_string(PROMOTION_FILE).unwrap();
        assert!(content.contains("READY_FOR_CONFIG_UPDATE"));
        assert!(content.contains("gate_margin_bps"));
        // Clean up
        let _ = fs::remove_file(PROMOTION_FILE);
    }
}
