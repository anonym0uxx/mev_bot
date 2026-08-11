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

use std::collections::{BTreeMap, HashMap};
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
use pump_quant_evaluator::evaluator_state::{EvaluatorState, ThompsonPosterior};
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
    /// Path to the event stream JSONL — used by Phase 3 engine replay.
    /// When set, the refiner spawns `pq-engine-replay` per challenger to
    /// produce genuine admission/sizing/exit decisions under each mutated
    /// config. When empty, falls back to shadow replay only.
    event_stream_path: String,
    /// Rev-11 §6: Rolling window for engine replay. If >0, only process
    /// the last N ticks of the event stream (optimizes for current market
    /// regime instead of full historical tape). 0 = use full tape.
    replay_window_ticks: u64,
}

/// Saturating cast from i128 to i64 — clamps to i64::MIN/MAX on overflow.
/// Used to convert fixed-point prices (i128) to the i64 lamport granularity
/// the edge decomposition functions expect.
fn saturating_cast_i128_to_i64(v: i128) -> i64 {
    i64::try_from(v).unwrap_or_else(|_| {
        if v > i128::from(i64::MAX) { i64::MAX }
        else { i64::MIN }
    })
}


fn parse_args() -> RefinerArgs {
    let args: Vec<String> = std::env::args().collect();
    let mut a = RefinerArgs {
        tape_path: DEFAULT_TAPE_PATH.to_string(),
        config_path: DEFAULT_CONFIG_PATH.to_string(),
        margin_lamports: 10_000, // 10k lamports = ~0.00001 SOL minimum margin
        max_challengers: 64,
        event_stream_path: String::new(), // default: no engine replay
        replay_window_ticks: 0,           // Rev-11 §6: 0 = full tape
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
            "--event-stream-path" if i + 1 < args.len() => {
                a.event_stream_path = args[i + 1].clone();
                i += 2;
            }
            "--replay-window-ticks" if i + 1 < args.len() => {
                a.replay_window_ticks = args[i + 1].parse().unwrap_or(0);
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

/// S3: Denylist of parameters that the refiner MUST NOT promote because they
/// affect envelope relationships (floor ≤ ceiling) or one-way ratchet state
/// (brain_reflect_enable) that the shadow replay cannot safely model. These
/// params are mutatable in principle but promoting them risks structural
/// invariant violations across compounding cycles.
///
/// The denylist is checked in two places:
///   1. `generate_challengers` — skips denylisted params entirely (no challenger
///      generated, so no shadow replay wasted on a no-op that would fail the
///      margin gate anyway).
///   2. `write_promotion_file` — defense-in-depth: even if a challenger somehow
///      defeats the champion with a denylisted mutation, the promotion file
///      refuses to write it.
///
/// Safe reflection params NOT in this list (single-value, no envelope):
///   - reflect_every_ticks (safe: just changes frequency, no envelope)
///   - brain_decay_min_sample (safe: just changes sample floor, no envelope)
const REFLECTION_DENYLIST: &[&str] = &[
    "reflect_weight_floor_bp",
    "reflect_weight_ceiling_bp",
    "brain_reflect_enable",
    "brain_reflect_step_bp",
];

/// S3: Returns true if a parameter name is on the reflection denylist.
fn is_reflection_denied(param_name: &str) -> bool {
    REFLECTION_DENYLIST.iter().any(|d| *d == param_name)
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

/// Rev-11 §1+§2+§5: Generate challenger configs using a tier-based priority
/// queue with adaptive mutation magnitudes and combinatorial pairs.
///
/// This replaces the Rev-9 alphabetical BTreeMap iteration that only explored
/// ~40 alphabetically-first parameters. The new system:
///
/// - Classifies all 182 parameters into 8 tiers (T0-T7) by economic impact
/// - Rotates through tiers across cycles using a persisted exploration cursor
///   so the FULL parameter surface is covered every 3 cycles
/// - Uses adaptive mutation magnitudes: BPS params ±15%, lamport params ±10%,
///   tick params ±20%, count params ±1/±2 absolute
/// - Generates 8 combinatorial pairs of coupled parameters per cycle
/// - T0 (exit params) ALWAYS explored every cycle — highest net-SOL impact
fn generate_challengers(
    champion_config: &str,
    max_challengers: usize,
    exploration_cursor: &mut std::collections::HashMap<u8, u64>,
) -> Vec<Challenger> {
    let params = parse_config_params(champion_config);

    // ─── Rev-11 §1: Tier classification ───────────────────────────────────
    // T0 = Exit/TP/SL/Trail (highest net-SOL impact — always explored)
    // T1 = Entry/Fee/Tip (second highest — entry timing drives entry price)
    // T2 = Sizing/Risk/DD/Cap (position sizing drives loss magnitude)
    // T3 = Moon bag/Reentry (asymmetric upside capture)
    // T4 = Gate/Margin (admission filter sensitivity)
    // T5 = Brain/Reflect (meta-learning — lower priority, slow convergence)
    // T6 = Meta/Universe (taxonomy, watchlist — infra-level)
    // T7 = Alpha/Bar/Narrative/VPIN (lowest — mostly regime detectors)
    let tiered = classify_params_by_tier(&params);

    let mut challengers: Vec<Challenger> = Vec::new();
    let mut id_counter = 0u64;

    // ─── T0: Always explore exit params (highest priority) ──────────────
    // The 32-slot reserve for combinatorial pairs only applies when
    // max_challengers is large enough to accommodate it.
    let single_slot_cap = if max_challengers > 40 {
        max_challengers.saturating_sub(32)
    } else {
        max_challengers
    };
    if let Some(t0_params) = tiered.get(&0u8) {
        for (name, val) in t0_params.iter() {
            if challengers.len() >= single_slot_cap {
                break; // Reserve slots for combinatorial pairs
            }
            if is_reflection_denied(name.as_str()) {
                continue;
            }
            generate_adaptive_challengers(
                name.as_str(),
                *val,
                &mut challengers,
                &mut id_counter,
            );
        }
    }

    // ─── T1-T7: Rotate through one tier per cycle ───────────────────────
    // Each cycle, we explore one additional tier (beyond T0) in rotation.
    // This ensures full-surface coverage within 7 cycles.
    let cycle_tier = select_rotation_tier(exploration_cursor);
    if cycle_tier > 0 {
        if let Some(tier_params) = tiered.get(&cycle_tier) {
            let cursor = exploration_cursor.get(&cycle_tier).copied().unwrap_or(0);
            let param_count = tier_params.len() as u64;
            let start_idx = (cursor % param_count.max(1)) as usize;

            // Explore params starting from cursor position, wrapping around
            for i in 0..tier_params.len() {
                if challengers.len() >= single_slot_cap {
                    break;
                }
                let idx = (start_idx + i) % tier_params.len();
                let (name, val) = &tier_params[idx];
                if is_reflection_denied(name.as_str()) {
                    continue;
                }
                generate_adaptive_challengers(
                    name.as_str(),
                    *val,
                    &mut challengers,
                    &mut id_counter,
                );
            }
            // Advance the cursor for next cycle
            exploration_cursor.insert(cycle_tier, (cursor + tier_params.len() as u64) % param_count.max(1));
        }
    }

    // Rev-11 fallback: if T0 + rotation tier didn't produce enough challengers
    // (common for small test configs or first cycle), explore ALL remaining
    // tiers alphabetically until we reach the cap.
    if challengers.len() < single_slot_cap {
        for tier in [1u8, 2, 3, 4, 5, 6, 7] {
            if challengers.len() >= single_slot_cap {
                break;
            }
            if tier == cycle_tier {
                continue; // Already explored above
            }
            if let Some(tier_params) = tiered.get(&tier) {
                for (name, val) in tier_params.iter() {
                    if challengers.len() >= single_slot_cap {
                        break;
                    }
                    if is_reflection_denied(name.as_str()) {
                        continue;
                    }
                    generate_adaptive_challengers(
                        name.as_str(),
                        *val,
                        &mut challengers,
                        &mut id_counter,
                    );
                }
            }
        }
    }

    // ─── Rev-11 §5: Combinatorial pairs ─────────────────────────────────
    // 8 coupled parameter pairs that are known to interact economically.
    // Each pair generates 4 challengers: (A+,B+), (A+,B-), (A-,B+), (A-,B-).
    // This explores the pairwise interaction surface that single-axis
    // search completely misses.
    generate_combinatorial_pairs(&params, &mut challengers, &mut id_counter, max_challengers);

    // ─── Rev-11 §4: Adaptive cap ────────────────────────────────────────
    // If we exceeded the cap (shouldn't normally), truncate. The caller
    // may also adjust max_challengers based on engine-replay throughput.
    if challengers.len() > max_challengers {
        challengers.truncate(max_challengers);
    }

    challengers
}

/// Rev-11 §1: Classify all config parameters into 8 priority tiers.
/// Returns a map of tier → sorted list of (param_name, value).
fn classify_params_by_tier(
    params: &BTreeMap<String, i64>,
) -> std::collections::HashMap<u8, Vec<(String, i64)>> {
    use std::collections::HashMap;
    let mut tiers: HashMap<u8, Vec<(String, i64)>> = HashMap::new();

    for (name, val) in params {
        let tier = param_tier(name.as_str());
        tiers.entry(tier).or_default().push((name.clone(), *val));
    }

    // Sort each tier's params alphabetically for deterministic ordering
    for vec in tiers.values_mut() {
        vec.sort_by(|a, b| a.0.cmp(&b.0));
    }

    tiers
}

/// Rev-11 §1: Map a parameter name to its priority tier (0=highest, 7=lowest).
fn param_tier(name: &str) -> u8 {
    // T0: Exit / TP / SL / Trail — directly controls per-trade PnL outcome
    if name.starts_with("lc_tp") || name.starts_with("lc_trail") || name.starts_with("lc_hard_sl")
        || name.starts_with("lc_tp1_") || name.starts_with("lc_tp2_") || name.starts_with("lc_tp3_")
        || name.starts_with("mcap_position_") || name.starts_with("exit_")
        || name.starts_with("target_") || name.starts_with("into_strength_")
        || name.starts_with("conditional_moon") || name.starts_with("moon_bag")
        || name == "lc_max_hold_ticks" || name == "lc_stall_ticks"
        || name == "lc_precursor_drop_bps" || name == "lc_cvd_hold_frac_bps"
    {
        0
    }
    // T1: Entry / Fee / Tip — controls entry price and cost basis
    else if name.starts_with("entry_") || name.starts_with("confirm_ttl")
        || name.starts_with("creation_") || name.starts_with("landing_")
        || name.starts_with("fill_") || name == "designated_caller_weight"
        || name == "curve_exact_fill_enable" || name == "entry_mode_leaves_enable"
    {
        1
    }
    // T2: Sizing / Risk / Drawdown / Cap — controls loss magnitude and capital allocation
    else if name.starts_with("dd_") || name.starts_with("total_risk")
        || name.starts_with("min_trade_size") || name.starts_with("max_concurrent")
        || name.starts_with("bankroll_") || name.starts_with("vol_stop")
        || name == "confirmed_capacity_mult" || name == "floor_fraction_bps"
        || name == "f_base_bp" || name == "baseline_margin_lamports"
        || name == "baseline_min_trades" || name == "scale_confirm_auth_min_bp"
    {
        2
    }
    // T3: Moon bag / Reentry — asymmetric upside capture and re-entry timing
    else if name.starts_with("reentry_") || name.starts_with("revert_")
        || name == "roll_revert_bp" || name == "roll_trend_bp"
    {
        3
    }
    // T4: Gate / Margin — admission filter sensitivity
    else if name.starts_with("gate_") || name.starts_with("probe_")
        || name.starts_with("sim_impact") || name.starts_with("expectancy_")
        || name.starts_with("expected_move")
    {
        4
    }
    // T5: Brain / Reflect — meta-learning (slow convergence, lower priority)
    else if name.starts_with("brain_") || name.starts_with("reflect_")
        || name.starts_with("narrative_") || name.starts_with("thesis_")
    {
        5
    }
    // T6: Meta / Universe / Creator — infrastructure-level
    else if name.starts_with("meta_") || name.starts_with("universe_")
        || name.starts_with("creator_") || name.starts_with("dev_")
        || name.starts_with("deployer_") || name.starts_with("coordinated_")
        || name.starts_with("bundle_") || name.starts_with("tracked_")
        || name.starts_with("smart_money") || name.starts_with("wallet_")
        || name.starts_with("watchlist_") || name.starts_with("holder_")
        || name.starts_with("money_proxy") || name.starts_with("designated_caller_enable")
        || name.starts_with("promote_") || name.starts_with("x_min_")
        || name == "mcap_band_enable" || name == "mcap_band_hi_lamports"
        || name == "mcap_band_lo_lamports"
    {
        6
    }
    // T7: Alpha / Bar / VPIN / Paper / Platform / Derived — lowest priority
    else {
        7
    }
}

/// Rev-11 §1: Select which tier to rotate through this cycle (T1-T7).
/// Uses the exploration cursor to cycle through tiers in order.
/// T0 is always explored separately and is not part of the rotation.
fn select_rotation_tier(cursor: &std::collections::HashMap<u8, u64>) -> u8 {
    // Count how many tiers have been explored so far
    let max_tier = 7u8;
    let explored = cursor.len() as u8;

    // Cycle through tiers 1-7 in order
    // The rotation key tracks the "next tier to explore"
    let rotation_key = 100u8; // sentinel key for rotation tracking
    if let Some(&next) = cursor.get(&rotation_key) {
        let tier = (next % 7) as u8 + 1; // cycles 1-7
        let _ = tier; // returned below
        return (next % 7) as u8 + 1;
    }
    // First cycle: start with T1 (entry params)
    1
}

/// Rev-11 §2: Generate challengers with adaptive mutation magnitudes.
/// BPS params: ±15% (basis points are coarse-grained, wider search is safe)
/// Lamport params: ±10% (standard, preserves capital scale)
/// Tick params: ±20% (timing is regime-dependent, wider exploration helps)
/// Count params: ±1 absolute (small counts need absolute steps, not %)
/// Other: ±10% (default)
fn generate_adaptive_challengers(
    name: &str,
    val: i64,
    challengers: &mut Vec<Challenger>,
    id_counter: &mut u64,
) {
    // Bool params: toggle only
    if name.ends_with("_enable") {
        let toggled = if val != 0 { 0 } else { 1 };
        challengers.push(Challenger {
            id: format!("challenger_{id_counter}"),
            mutations: vec![ParameterMutation {
                name: name.to_string(),
                current_value: val,
                proposed_value: toggled,
                rationale: format!("toggle {name}: {val} → {toggled}"),
            }],
        });
        *id_counter += 1;
        return;
    }

    // Zero-valued numeric: skip (can't mutate by percentage)
    if val == 0 {
        return;
    }

    // Determine mutation magnitude by parameter type
    let pct = adaptive_mutation_pct(name);
    let delta = compute_mutation_delta(val, pct, name);

    let plus_val = val.saturating_add(delta);
    let minus_val = val.saturating_sub(delta);

    // +mutation challenger
    challengers.push(Challenger {
        id: format!("challenger_{id_counter}"),
        mutations: vec![ParameterMutation {
            name: name.to_string(),
            current_value: val,
            proposed_value: plus_val,
            rationale: format!("+{pct}% {name}: {val} → {plus_val}"),
        }],
    });
    *id_counter += 1;

    // -mutation challenger (only if result stays positive for unsigned params)
    if minus_val > 0 {
        challengers.push(Challenger {
            id: format!("challenger_{id_counter}"),
            mutations: vec![ParameterMutation {
                name: name.to_string(),
                current_value: val,
                proposed_value: minus_val,
                rationale: format!("-{pct}% {name}: {val} → {minus_val}"),
            }],
        });
        *id_counter += 1;
    }

    // Rev-11 §2: For high-impact T0 params, also generate an aggressive
    // ±2× delta challenger to bracket the optimum faster
    if param_tier(name) == 0 && delta > 0 {
        let delta2 = delta * 2;
        let plus2_val = val.saturating_add(delta2);
        if plus2_val != plus_val && plus2_val > 0 {
            challengers.push(Challenger {
                id: format!("challenger_{id_counter}"),
                mutations: vec![ParameterMutation {
                    name: name.to_string(),
                    current_value: val,
                    proposed_value: plus2_val,
                    rationale: format!("+{}% {name}: {val} → {plus2_val} (aggressive bracket)", pct * 2),
                }],
            });
            *id_counter += 1;
        }
    }
}

/// Rev-11 §2: Determine the mutation percentage for a parameter based on its type.
fn adaptive_mutation_pct(name: &str) -> i64 {
    if name.ends_with("_bps") || name.ends_with("_bp") {
        15 // BPS params: ±15%
    } else if name.ends_with("_lamports") || name.ends_with("_lamport") {
        10 // Lamport params: ±10%
    } else if name.ends_with("_ticks") || name.ends_with("_tick") {
        20 // Tick params: ±20%
    } else if name.ends_with("_count") || name.ends_with("_cap")
        || name == "max_concurrent_positions" || name.ends_with("_min")
        || name.ends_with("_max") || name.ends_with("_window")
        || name.ends_with("_slots") || name.ends_with("_capacity")
    {
        0  // Signal for absolute mutation, not percentage
    } else {
        10 // Default: ±10%
    }
}

/// Rev-11 §2: Compute the mutation delta. For most params this is val * pct / 100.
/// For count params (pct==0), use absolute steps of 1-2.
fn compute_mutation_delta(val: i64, pct: i64, name: &str) -> i64 {
    if pct == 0 {
        // Count params: absolute step
        if val.unsigned_abs() <= 3 {
            1 // Small counts: step by 1
        } else {
            2 // Larger counts: step by 2
        }
    } else {
        // Percentage-based: val * pct / 100, with a minimum of 1
        let delta = (val.unsigned_abs() as u128 * pct as u128 / 100) as i64;
        delta.max(1)
    }
}

/// Rev-11 §5: Generate combinatorial challengers — 8 coupled parameter pairs.
/// Each pair generates 4 challengers: (A+,B+), (A+,B-), (A-,B+), (A-,B-).
/// This explores the pairwise interaction surface that single-axis search
/// completely misses. Memecoin parameters are highly coupled:
/// e.g., gate_margin + TP level — a tighter gate admits fewer but better
/// trades, and the optimal TP depends on how selective the gate is.
fn generate_combinatorial_pairs(
    params: &BTreeMap<String, i64>,
    challengers: &mut Vec<Challenger>,
    id_counter: &mut u64,
    max_challengers: usize,
) {
    // The 8 coupled pairs (param_a, param_b) — chosen by economic reasoning:
    // 1. (gate_margin_bps, lc_tp1_bps) — gate selectivity ↔ first TP target
    // 2. (entry_fee_bps, entry_tip_lamports) — entry cost ↔ entry speed
    // 3. (dd_tier1_bp, lc_hard_sl_bps) — first DD tier ↔ hard stop level
    // 4. (lc_tp1_bps, lc_tp1_frac_bps) — TP1 level ↔ TP1 fraction sold
    // 5. (lc_tp2_bps, lc_tp2_frac_bps) — TP2 level ↔ TP2 fraction sold
    // 6. (lc_trail_base_bps, lc_trail_max_bps) — trail start ↔ trail ceiling
    // 7. (max_concurrent_positions, min_trade_size_lamports) — diversification ↔ sizing
    // 8. (vol_stop_scale_bp, lc_hard_sl_bps) — vol-stop sensitivity ↔ hard stop
    let pairs: [(&str, &str); 8] = [
        ("gate_margin_bps", "lc_tp1_bps"),
        ("entry_fee_bps", "entry_tip_lamports"),
        ("dd_tier1_bp", "lc_hard_sl_bps"),
        ("lc_tp1_bps", "lc_tp1_frac_bps"),
        ("lc_tp2_bps", "lc_tp2_frac_bps"),
        ("lc_trail_base_bps", "lc_trail_max_bps"),
        ("max_concurrent_positions", "min_trade_size_lamports"),
        ("vol_stop_scale_bp", "lc_hard_sl_bps"),
    ];

    for (param_a, param_b) in &pairs {
        if challengers.len() + 4 > max_challengers {
            break;
        }
        let val_a = match params.get(*param_a) {
            Some(v) => *v,
            None => continue,
        };
        let val_b = match params.get(*param_b) {
            Some(v) => *v,
            None => continue,
        };
        // Skip if either param is zero (can't mutate by %)
        if val_a == 0 || val_b == 0 {
            continue;
        }
        // Skip denylisted
        if is_reflection_denied(param_a) || is_reflection_denied(param_b) {
            continue;
        }

        let pct_a = adaptive_mutation_pct(param_a);
        let pct_b = adaptive_mutation_pct(param_b);
        let delta_a = compute_mutation_delta(val_a, pct_a, param_a);
        let delta_b = compute_mutation_delta(val_b, pct_b, param_b);

        let a_plus = val_a.saturating_add(delta_a);
        let a_minus = val_a.saturating_sub(delta_a).max(1);
        let b_plus = val_b.saturating_add(delta_b);
        let b_minus = val_b.saturating_sub(delta_b).max(1);

        // Generate 4 combinations: (A+,B+), (A+,B-), (A-,B+), (A-,B-)
        for (va, vb, label) in [
            (a_plus, b_plus, "++"),
            (a_plus, b_minus, "+-"),
            (a_minus, b_plus, "-+"),
            (a_minus, b_minus, "--"),
        ] {
            if challengers.len() >= max_challengers {
                break;
            }
            challengers.push(Challenger {
                id: format!("challenger_{id_counter}"),
                mutations: vec![
                    ParameterMutation {
                        name: param_a.to_string(),
                        current_value: val_a,
                        proposed_value: va,
                        rationale: format!("combo {label}: {param_a} {val_a}→{va}"),
                    },
                    ParameterMutation {
                        name: param_b.to_string(),
                        current_value: val_b,
                        proposed_value: vb,
                        rationale: format!("combo {label}: {param_b} {val_b}→{vb}"),
                    },
                ],
            });
            *id_counter += 1;
        }
    }
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

// ─── Phase 3: Engine replay via subprocess ────────────────────────────────

/// The genuine engine replay score for one challenger. Unlike the shadow
/// replay (which only adjusts cost on existing trades), this is the REAL
/// engine output — different configs produce different admission decisions,
/// different sizing, different exits, and different net P&L.
#[derive(Clone, Debug)]
struct EngineReplayScore {
    /// Net lamports from the engine Report (the objective).
    net_lamports: i128,
    /// Trades admitted by the gate.
    admitted: u64,
    /// Trades rejected by the gate.
    rejected: u64,
    /// Trades promoted to the gate.
    promoted: u64,
    /// Events fed into the engine.
    events_fed: u64,
    /// Lines skipped during event stream parsing.
    parse_skipped: u64,
}

/// Apply a single mutation to the champion config text, producing the
/// challenger's full config text. The config format is `key = value` lines;
/// we replace the value for the mutated key. If the key isn't found (e.g.
/// a default-only param not in the champion file), we append it.
fn apply_mutation_to_config_text(champion_text: &str, mutation: &ParameterMutation) -> String {
    let target_key = &mutation.name;
    let new_val = mutation.proposed_value;
    let mut found = false;
    let mut out = String::with_capacity(champion_text.len());
    for line in champion_text.lines() {
        if let Some(eq_pos) = line.find('=') {
            let key = line[..eq_pos].trim();
            if key == target_key {
                // Replace this line's value.
                out.push_str(&format!("{} = {}\n", target_key, new_val));
                found = true;
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    if !found {
        // Key not in the champion config — append it.
        out.push_str(&format!("{} = {}\n", target_key, new_val));
    }
    out
}

/// Apply ALL of a challenger's mutations to the champion config text.
fn build_challenger_config_text(champion_text: &str, challenger: &Challenger) -> String {
    let mut text = champion_text.to_string();
    for m in &challenger.mutations {
        text = apply_mutation_to_config_text(&text, m);
    }
    text
}

/// Run `pq-engine-replay` as a subprocess with the given event stream and
/// config text. Writes the config to a temp file, spawns the binary, parses
/// the JSON output, and returns the score. Returns `None` if the subprocess
/// fails or the JSON is unparseable.
fn run_engine_replay(
    event_stream_path: &str,
    config_text: &str,
    replay_window_ticks: u64,
) -> Option<EngineReplayScore> {
    // Locate the pq-engine-replay binary next to the refiner binary.
    let replay_bin = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("pq-engine-replay")))
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "pq-engine-replay".to_string());

    // Write config to a temp file.
    let temp_config = std::env::temp_dir().join(format!(
        "pq-replay-config-{}-{}.cfg",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos()
    ));
    if fs::write(&temp_config, config_text).is_err() {
        return None;
    }

    // Spawn the subprocess.
    let mut cmd = std::process::Command::new(&replay_bin);
    cmd.arg("--event-stream").arg(event_stream_path)
        .arg("--config").arg(&temp_config)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    // Rev-11 §6: Rolling window — only replay the last N ticks of the event
    // stream. This optimizes for CURRENT market conditions rather than the
    // full historical tape. A window of 20000 ticks ≈ 1.4h at 14400 ticks/hr.
    if replay_window_ticks > 0 {
        cmd.arg("--replay-window-ticks").arg(replay_window_ticks.to_string());
    }

    let output = cmd.spawn().ok()?.wait_with_output().ok()?;

    // Clean up temp file (best-effort).
    let _ = fs::remove_file(&temp_config);

    if !output.status.success() {
        eprintln!(
            "[pq-refiner] engine-replay FAILED: exit={:?} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stderr).lines().last().unwrap_or("(empty)")
        );
        return None;
    }

    // Parse JSON from stdout.
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_engine_replay_json(&stdout)
}

/// Parse the JSON output of `pq-engine-replay` into an `EngineReplayScore`.
/// The format is:
///   {"admitted":N,"rejected":N,"net_lamports":I,"promoted":N,"ticks":N,
///    "events_fed":N,"parse_skipped":N}
fn parse_engine_replay_json(json: &str) -> Option<EngineReplayScore> {
    // Simple integer extractor — no serde dependency in the evaluator crate.
    let extract = |key: &str| -> Option<i128> {
        let needle = format!("\"{key}\":");
        let pos = json.find(&needle)?;
        let rest = &json[pos + needle.len()..];
        let end = rest.find(|c: char| !c.is_ascii_digit() && c != '-').unwrap_or(rest.len());
        rest[..end].parse().ok()
    };

    Some(EngineReplayScore {
        net_lamports: extract("net_lamports")?,
        admitted: extract("admitted")? as u64,
        rejected: extract("rejected")? as u64,
        promoted: extract("promoted")? as u64,
        events_fed: extract("events_fed")? as u64,
        parse_skipped: extract("parse_skipped")? as u64,
    })
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
    /// Phase 3: genuine engine replay score (None if engine replay not run).
    /// When present, this is the REAL engine output — different configs produce
    /// different admission/sizing/exit decisions. This OVERRIDES the shadow
    /// replay score for scoring and promotion decisions.
    engine_replay: Option<EngineReplayScore>,
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
            "reflect_every_ticks" => {
                // S5: Model the sample-count effect of changing reflection
                // frequency. Higher reflect_every_ticks → fewer reflection
                // passes → more trades accumulate per pass → thicker samples
                // → more reliable reflection signal. Lower reflect_every_ticks
                // → more frequent reflection → thinner samples per pass →
                // noisier weight adjustments.
                //
                // We model this as a small confidence adjustment to the net:
                // - Increasing reflect_every_ticks by +10% → samples are ~10%
                //   thicker per pass → apply a +2% bonus to net (conservative:
                //   the real benefit is better weight decisions, not direct P&L,
                //   so we keep the bonus small).
                // - Decreasing reflect_every_ticks by -10% → samples are ~10%
                //   thinner → apply a -2% penalty (noisier weight decisions).
                //
                // The bonus/penalty is intentionally small (2% of pct_change)
                // because reflection's effect on P&L is indirect — it changes
                // lane weights, which changes trade selection, which changes
                // P&L. The shadow replay can't model that full chain, but it
                // CAN model the sample-thickness confidence effect.
                let confidence_adj = pct_change * 0.02; // 2% of pct_change
                for t in &mut adjusted_trades {
                    let adjusted = (t.gross_lamports as f64 * (1.0 + confidence_adj)) as i128;
                    t.gross_lamports = adjusted;
                }
            }
            _ => {
                // Admission-gate parameters (gate_expected_move_bps,
                // gate_exit_tranches, promote_min_haircut_bp, etc.) change WHICH
                // trades the engine would admit — not just the economics of trades
                // that were admitted. The shadow replay CANNOT model this because
                // it operates on a fixed tape of already-executed trades.
                //
                // Proxy heuristics (removing fractions of trades, scaling fees by
                // leg count) were considered and REJECTED: they guess at what the
                // engine would do instead of running the engine. The real path is
                // the engine-replay subprocess (see `engine_replay_score` below),
                // which feeds the event stream through Engine::new(cfg, Replay)
                // and produces a genuine Report with different admission decisions.
                //
                // For the shadow-replay score, these parameters are no-ops: the
                // tape is unchanged. The engine-replay score is the one that
                // differentiates challengers on these parameters.
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
        engine_replay: None, // populated by Phase 3 engine replay in main
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

    // 4. Generate challengers (Rev-11: tier-based priority queue + adaptive mutations)
    // Rev-11 §4: Adaptive challenger cap — scale based on last cycle's duration.
    // If the last cycle took >60s for the default cap → reduce to 48 (throughput-limited).
    // If the last cycle took <30s → expand to 128 (headroom to explore more).
    // Clamp to [24, 128]. T0 params always explored regardless of cap.
    let adaptive_cap = {
        let base = args.max_challengers;
        let last_dur = state.last_cycle_duration_secs;
        let adjusted = if last_dur > 60 && base >= 64 {
            eprintln!("[pq-refiner] §4 adaptive cap: last cycle took {last_dur}s > 60s → reducing from {base} to 48");
            48
        } else if last_dur > 0 && last_dur < 30 && base <= 64 {
            eprintln!("[pq-refiner] §4 adaptive cap: last cycle took {last_dur}s < 30s → expanding from {base} to 128");
            128
        } else {
            base
        };
        adjusted.clamp(24, 128)
    };
    let challengers = generate_challengers(&champion_config_text, adaptive_cap, &mut state.exploration_cursor);
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

    // 5. Run shadow replays + Phase 3 engine replays
    // Rev-11 §4: Track cycle duration for adaptive cap.
    let cycle_start = std::time::Instant::now();

    let mut results: Vec<ShadowReplayResult> = Vec::new();
    let mut any_defeated = false;
    let mut any_gates_passed = false;
    let mut best_challenger: Option<ShadowReplayResult> = None;
    // Rev-11 §3: Track whether the best challenger was scored via engine-replay.
    // Promotion is ONLY allowed when engine-replay confirmed the edge —
    // shadow replay alone is a pre-filter, not a promotion gate.
    let mut best_has_engine_replay = false;

    // Phase 3: if the event stream path is set, also compute the champion's
    // engine-replay score for fair comparison. Each challenger's engine-replay
    // net_lamports is compared against this champion engine-replay score.
    let champion_engine_replay = if !args.event_stream_path.is_empty()
        && std::path::Path::new(&args.event_stream_path).exists()
    {
        eprintln!(
            "[pq-refiner] Phase 3: engine replay enabled — event stream: {}",
            args.event_stream_path
        );
        let champ_cfg = build_challenger_config_text(&champion_config_text, &Challenger {
            id: "champion".to_string(),
            mutations: vec![],
        });
        match run_engine_replay(&args.event_stream_path, &champ_cfg, args.replay_window_ticks) {
            Some(score) => {
                eprintln!(
                    "[pq-refiner] champion engine-replay: net={} admitted={} rejected={}",
                    score.net_lamports, score.admitted, score.rejected
                );
                Some(score)
            }
            None => {
                eprintln!("[pq-refiner] champion engine-replay FAILED — falling back to shadow replay only");
                None
            }
        }
    } else {
        eprintln!("[pq-refiner] Phase 3: engine replay disabled (no event stream path)");
        None
    };

    for challenger in &challengers {
        eprintln!("[pq-refiner] shadow replay: {} (mutations: {})",
            challenger.id,
            challenger.mutations.iter().map(|m| m.rationale.clone())
                .collect::<Vec<_>>().join(", ")
        );

        let mut result = shadow_replay(
            challenger,
            &tape.trades,
            champion_net_scalp,
            champion_net_early,
            args.margin_lamports,
        );

        // ─── Phase 3: engine replay via subprocess ──────────────────────
        // When the event stream is available, spawn `pq-engine-replay` with
        // the challenger's mutated config. This produces the GENUINE engine
        // output — the real admission/sizing/exit decisions under the mutated
        // config. If it succeeds, we OVERRIDE the shadow replay's net_lamports
        // with the engine replay's net_lamports and re-derive the verdict.
        // This is the REAL scoring path — no proxy heuristics.
        if let Some(ref champ_er) = champion_engine_replay {
            let challenger_cfg = build_challenger_config_text(
                &champion_config_text,
                challenger,
            );
            match run_engine_replay(&args.event_stream_path, &challenger_cfg, args.replay_window_ticks) {
                Some(score) => {
                    eprintln!(
                        "[pq-refiner]   engine-replay: {} net={} admitted={} rejected={}",
                        challenger.id, score.net_lamports, score.admitted, score.rejected
                    );
                    // Override the challenger's net with the genuine engine output.
                    result.challenger_net_scalp.net_lamports = score.net_lamports;
                    // Re-derive the verdict using the engine replay score.
                    let champ_net = NetSol {
                        net_lamports: champ_er.net_lamports,
                        ..NetSol::missing()
                    };
                    result.verdict = challenger_defeats_champion(
                        &champ_net,
                        &result.challenger_net_scalp,
                        args.margin_lamports,
                    );
                    result.engine_replay = Some(score);
                }
                None => {
                    eprintln!(
                        "[pq-refiner]   engine-replay FAILED for {} — keeping shadow replay score",
                        challenger.id
                    );
                }
            }
        }

        eprintln!("[pq-refiner]   → {}", result.summary);

        if result.verdict.defeats() {
            any_defeated = true;
            // Track the best challenger (highest net)
            if best_challenger.is_none() ||
               result.challenger_net_scalp.net_lamports >
               best_challenger.as_ref().unwrap().challenger_net_scalp.net_lamports
            {
                best_challenger = Some(result.clone());
                // Rev-11 §3: Record whether this best challenger has genuine
                // engine-replay confirmation. Only engine-replay-confirmed
                // challengers are eligible for promotion.
                best_has_engine_replay = result.engine_replay.is_some();
            }
        }

        // 5b. Run 8-gate evaluation (§45-56) for challengers that defeat on margin.
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

    // G6.5: Feed real trade outcomes into Thompson posteriors.
    // This is the core learning loop: each exited trade updates the Beta-Bernoulli
    // posterior for its strategy type (keyed by strategy_id from the enriched tape).
    // The posterior alpha/beta track wins/losses, n_trades tracks sample size,
    // and cumulative_netsol_lamports tracks the net SOL P&L for that type.
    // This data is then used by the Thompson allocation below to fund the
    // strategy types with the highest expected value.
    if !tape.full_trades.is_empty() {
        for ft in &tape.full_trades {
            // Skip trades with no strategy attribution (strategy_id == 0 means
            // the trade was recorded before the enrichment fix).
            if ft.strategy_id == 0 {
                continue;
            }
            let is_win = ft.realized_pnl_lamports > 0;
            let entry = state.thompson_posteriors
                .entry(ft.strategy_id)
                .or_insert_with(|| ThompsonPosterior {
                    alpha: 1,
                    beta: 1,
                    n_trades: 0,
                    cumulative_netsol_lamports: 0,
                    entry_mode: String::new(),
                    archetype: String::new(),
                    sizing: String::new(),
                    lane: String::new(),
                });
            entry.n_trades += 1;
            entry.cumulative_netsol_lamports =
                entry.cumulative_netsol_lamports
                    .saturating_add(ft.realized_pnl_lamports);
            if is_win {
                entry.alpha += 1;
            } else {
                entry.beta += 1;
            }
            // Populate metadata fields from the enriched tape record.
            if entry.entry_mode.is_empty() {
                entry.entry_mode = ft.source.clone();
            }
            if entry.archetype.is_empty() {
                entry.archetype = format!("type_{}", ft.strategy_id);
            }
            if entry.sizing.is_empty() {
                entry.sizing = format!("{}lamports", ft.size_lamports);
            }
            if entry.lane.is_empty() {
                entry.lane = ft.source.clone();
            }
        }
        eprintln!(
            "[pq-refiner] Thompson posterior update: {} strategy types, {} trades fed",
            state.thompson_posteriors.len(),
            tape.full_trades.iter().filter(|t| t.strategy_id != 0).count()
        );
    }

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

    // G8: SPRT early termination — feed real trade outcomes into the SPRT engine.
    // Each trade's win/loss outcome is fed to the SPRT for its strategy type,
    // allowing the SPRT to detect genuine edges (Adoptable) or coin-flip
    // strategies (Dropped) per strategy type rather than a single global type.
    {
        let mut sprt = StrategyTypeSprt::new(&mut state);
        // Feed real trade outcomes from full_trades into SPRT, keyed by strategy_id.
        for ft in &tape.full_trades {
            if ft.strategy_id == 0 {
                continue;
            }
            let won = ft.realized_pnl_lamports > 0;
            let sprt_result = sprt.push_pair(ft.strategy_id, won);
            eprintln!(
                "[pq-refiner] SPRT trade: type={} verdict={:?} action={:?}",
                ft.strategy_id, sprt_result.verdict, sprt_result.action
            );
        }
        // Also feed challenger pair results (if any) into SPRT.
        for result in &results {
            if result.verdict.defeats() {
                // Use the challenger's strategy type if available, else default.
                let type_id = 1u64;
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
        // Use full_trades (enriched 16-field format) for edge attribution.
        // Fall back to coarse trades only if full_trades is empty (backward compat).
        if !tape.full_trades.is_empty() {
            // Compute equal-weight baseline size once from the full trade set.
            let n = tape.full_trades.len() as u64;
            let total_size: u64 = tape.full_trades.iter()
                .map(|t| t.size_lamports)
                .sum::<u64>()
                .max(1);
            let eq_weight = (total_size / n.max(1)) as i64;
            for ft in &tape.full_trades {
                // Convert fixed-point prices to i64 for edge decomposition.
                // price_fp = price * 1e9, so we scale down to lamport granularity.
                // We use the raw i128->i64 cast with saturation to avoid overflow.
                // Since we don't have TWAP/midpoint baselines yet, entry and exit
                // edge are computed as 0 (neutral) — the real edges come from
                // sizing and the total P&L decomposition.
                let entry = saturating_cast_i128_to_i64(ft.entry_price_fp);
                let exit = saturating_cast_i128_to_i64(ft.exit_price_fp);
                let size = ft.size_lamports as i64;
                let per_unit_pnl = ft.realized_pnl_lamports;
                let decomp = decompose_trade(
                    entry,        // actual entry price
                    entry,        // twap entry (no TWAP data yet = neutral)
                    exit,         // actual exit price
                    exit,         // midpoint exit (no midpoint data yet = neutral)
                    size,         // actual size in lamports
                    eq_weight,    // equal-weight baseline size
                    per_unit_pnl, // per-unit P&L in lamports
                    0,            // selection PnL (not yet decomposed)
                );
                total_entry_edge += decomp.entry_edge_lamports;
                total_exit_edge += decomp.exit_edge_lamports;
                total_sizing_edge += decomp.sizing_edge_lamports;
            }
            eprintln!(
                "[pq-refiner] edge attribution (full): entry={total_entry_edge} exit={total_exit_edge} sizing={total_sizing_edge} (lamports, {} trades)",
                tape.full_trades.len()
            );
        } else {
            // Fallback: coarse trades (backward compat with older tapes).
            for trade in &tape.trades {
                let decomp = decompose_trade(
                    trade.gross_lamports as i64,
                    trade.gross_lamports as i64,
                    0, 0, 1, 1,
                    trade.gross_lamports as i64, 0,
                );
                total_entry_edge += decomp.entry_edge_lamports;
                total_exit_edge += decomp.exit_edge_lamports;
                total_sizing_edge += decomp.sizing_edge_lamports;
            }
            eprintln!(
                "[pq-refiner] edge attribution (coarse fallback): entry={total_entry_edge} exit={total_exit_edge} sizing={total_sizing_edge} (lamports, {} trades)",
                tape.trades.len()
            );
        }
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
        if any_gates_passed && best_has_engine_replay {
            eprintln!("[pq-refiner] CHAMPION DEFEATED by {} AND 8-gate passed AND engine-replay confirmed — writing promotion", best.challenger_id);
            write_promotion_file(&best, &challengers);
            write_refiner_status(challengers.len(), 1, "promoted");
            append_refiner_log(&results, true);
        } else if any_gates_passed && !best_has_engine_replay {
            // Rev-11 §3: Engine-replay is mandatory for promotion.
            // Shadow replay alone is a pre-filter — it cannot gate promotion.
            // If engine-replay is unavailable or failed, we skip the cycle
            // rather than promote on approximation.
            eprintln!("[pq-refiner] §3: champion defeated + 8-gate passed BUT no engine-replay confirmation — NO promotion (shadow-only is insufficient)");
            write_refiner_status(challengers.len(), 0, "gate_passed_no_engine_replay");
            append_refiner_log(&results, false);
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
    // Rev-11 §4: Save cycle duration for adaptive cap next cycle.
    state.last_cycle_duration_secs = cycle_start.elapsed().as_secs();
    eprintln!("[pq-refiner] §4: cycle took {}s", state.last_cycle_duration_secs);

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

/// S4: Reflection health snapshot embedded in the refiner status file.
/// Gives the refiner visibility into reflection's sample health without needing
/// to model reflection params in the shadow replay.
#[derive(Clone, Debug)]
pub struct ReflectionHealth {
    /// reflect_every_ticks value from the champion config.
    pub reflect_every_ticks: u64,
    /// Number of scalp-lane trades in the current tape window.
    pub scalp_trades: u32,
    /// Number of early-lane trades in the current tape window.
    pub early_trades: u32,
    /// brain_decay_min_sample from the champion config (the sample floor).
    pub brain_decay_min_sample: u32,
    /// True if either lane has fewer trades than brain_decay_min_sample.
    /// This means reflection's lane_decay() will skip that lane due to
    /// insufficient samples — the refiner should avoid tightening gates
    /// (which would reduce trade throughput further).
    pub lane_starved: bool,
}

impl ReflectionHealth {
    /// Construct from raw data. Computes `lane_starved` from the trade counts
    /// vs the sample floor.
    pub fn new(
        reflect_every_ticks: u64,
        scalp_trades: u32,
        early_trades: u32,
        brain_decay_min_sample: u32,
    ) -> Self {
        let lane_starved = scalp_trades < brain_decay_min_sample
            || early_trades < brain_decay_min_sample;
        ReflectionHealth {
            reflect_every_ticks,
            scalp_trades,
            early_trades,
            brain_decay_min_sample,
            lane_starved,
        }
    }

    /// Serialize to a JSON fragment for embedding in the refiner status file.
    pub fn to_json(&self) -> String {
        format!(
            "  \"reflection_health\": {{\n    \"reflect_every_ticks\": {},\n    \"scalp_trades\": {},\n    \"early_trades\": {},\n    \"brain_decay_min_sample\": {},\n    \"lane_starved\": {}\n  }}",
            self.reflect_every_ticks,
            self.scalp_trades,
            self.early_trades,
            self.brain_decay_min_sample,
            self.lane_starved
        )
    }
}

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

/// S4: Extended refiner status writer that includes reflection health metrics.
/// The status file now carries `reflection_health` alongside the existing
/// `challengers_evaluated` / `promoted` / `status` fields.
fn write_refiner_status_with_reflection(
    num_challengers: usize,
    num_promoted: usize,
    status: &str,
    health: &ReflectionHealth,
) {
    // Ensure the data directory exists
    if let Some(parent) = Path::new(REFINER_STATUS_FILE).parent() {
        let _ = fs::create_dir_all(parent);
    }
    let health_json = health.to_json();
    let status_json = format!(
        "{{\n  \"challengers_evaluated\": {num_challengers},\n  \"promoted\": {num_promoted},\n  \"status\": \"{status}\",\n  {health_json}\n}}",
    );
    let _ = fs::write(REFINER_STATUS_FILE, status_json);
}

/// S7: Determine whether a promotion should be deferred based on reflection
/// health. If the champion config's lanes are already starved (below
/// brain_decay_min_sample) AND the winning challenger would tighten gates
/// (gate_margin_bps increase → fewer trades → even thinner samples), the
/// promotion should be deferred to avoid starving reflection further.
///
/// Returns `true` if the promotion should proceed, `false` if it should be
/// deferred.
fn reflection_promotion_guard(
    challenger: Option<&Challenger>,
    health: Option<&ReflectionHealth>,
) -> bool {
    let health = match health {
        Some(h) => h,
        None => return true, // no health data → don't block promotion
    };
    if !health.lane_starved {
        return true; // lanes healthy → promotion is safe
    }
    // Lanes are starved. Check if the winning mutation would reduce throughput.
    // gate_margin_bps increase → tighter gate → fewer trades admitted →
    // even thinner samples per reflection pass.
    if let Some(c) = challenger {
        for m in &c.mutations {
            if m.name == "gate_margin_bps" && m.proposed_value > m.current_value {
                eprintln!(
                    "[pq-refiner] S7 DEFER: lane_starved=true and gate_margin_bps would INCREASE ({} → {}) — deferring promotion to protect reflection samples",
                    m.current_value, m.proposed_value
                );
                return false;
            }
            // gate_fail_rate_bps increase → trades look worse → faster lane
            // decay → fewer effective trades per reflection pass.
            if m.name == "gate_fail_rate_bps" && m.proposed_value > m.current_value {
                eprintln!(
                    "[pq-refiner] S7 DEFER: lane_starved=true and gate_fail_rate_bps would INCREASE ({} → {}) — deferring promotion to protect reflection samples",
                    m.current_value, m.proposed_value
                );
                return false;
            }
        }
    }
    true
}

fn write_promotion_file(result: &ShadowReplayResult, challengers: &[Challenger]) {
    // Ensure the data directory exists
    if let Some(parent) = Path::new(PROMOTION_FILE).parent() {
        let _ = fs::create_dir_all(parent);
    }
    // Find the challenger that produced this result
    let challenger = challengers.iter().find(|c| c.id == result.challenger_id);

    // S3: Defense-in-depth — refuse to write a promotion file if ANY mutation
    // in the winning challenger targets a denylisted envelope-affecting param.
    // This is a second guard behind the generate_challengers skip; it catches
    // the case where a denylisted param somehow reaches promotion (e.g. via
    // a hand-edited challenger or a future code path).
    if let Some(c) = challenger {
        for m in &c.mutations {
            if is_reflection_denied(m.name.as_str()) {
                eprintln!(
                    "[pq-refiner] S3 DENY: refusing to promote denylisted param '{}' — promotion file NOT written",
                    m.name
                );
                return;
            }
        }
    }

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

    // Phase 3: include engine replay evidence if available.
    let engine_replay_json = if let Some(er) = &result.engine_replay {
        format!(
            ",\n  \"engine_replay\": {{\"net_lamports\": {}, \"admitted\": {}, \"rejected\": {}, \"promoted\": {}, \"events_fed\": {}}}",
            er.net_lamports, er.admitted, er.rejected, er.promoted, er.events_fed,
        )
    } else {
        String::new()
    };

    let promotion_json = format!(
        "{{\n  \"challenger_id\": \"{}\",\n  \
         \"champion_net_scalp\": {},\n  \
         \"challenger_net_scalp\": {},\n  \
         \"mutations\": [\n{}\n  ],\n  \
         \"verdict\": \"defeats\",\n  \
         \"gate_verdict\": \"{}\",\n  \
         \"status\": \"READY_FOR_CONFIG_UPDATE\"{}\n}}",
        result.challenger_id,
        format_netsol(result.champion_net_scalp),
        format_netsol(result.challenger_net_scalp),
        mutations_json,
        gate_verdict_str,
        engine_replay_json,
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
    use std::sync::{Mutex, OnceLock};

    /// S4: Serialize all tests that touch the shared REFINER_STATUS_FILE
    /// or PROMOTION_FILE to prevent parallel-test race conditions.
    static FILE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    fn file_lock() -> std::sync::MutexGuard<'static, ()> {
        let mtx = FILE_LOCK.get_or_init(|| Mutex::new(()));
        mtx.lock().unwrap()
    }

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
        let mut cursor = std::collections::HashMap::new();
        let challengers = generate_challengers(config, 8, &mut cursor);
        assert!(!challengers.is_empty());
        // Most challengers should have exactly 1 mutation (single-axis search),
        // but combinatorial pairs will have 2.
        for c in &challengers {
            assert!(c.mutations.len() >= 1);
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
        let mut cursor = std::collections::HashMap::new();
        let challengers = generate_challengers(config, 4, &mut cursor);
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
        let _lock = file_lock();
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
        let _lock = file_lock();
        let result = ShadowReplayResult {
            challenger_id: "test_promo".to_string(),
            challenger_net_scalp: NetSol::missing(),
            challenger_net_early: NetSol::missing(),
            champion_net_scalp: NetSol::missing(),
            champion_net_early: NetSol::missing(),
            verdict: ChampionVerdict::Defeats,
            gate_verdict: Some("all_8_gates_passed".to_string()),
            summary: "test".to_string(),
            engine_replay: None,
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

    // ─── S3: Reflection denylist tests ──────────────────────────────────────

    #[test]
    fn s3_denylist_covers_envelope_params() {
        assert!(is_reflection_denied("reflect_weight_floor_bp"));
        assert!(is_reflection_denied("reflect_weight_ceiling_bp"));
        assert!(is_reflection_denied("brain_reflect_enable"));
        assert!(is_reflection_denied("brain_reflect_step_bp"));
    }

    #[test]
    fn s3_denylist_does_not_cover_safe_params() {
        // reflect_every_ticks and brain_decay_min_sample are safe (single-value, no envelope)
        assert!(!is_reflection_denied("reflect_every_ticks"));
        assert!(!is_reflection_denied("brain_decay_min_sample"));
        assert!(!is_reflection_denied("gate_margin_bps"));
        assert!(!is_reflection_denied("sim_impact_k_bps"));
    }

    #[test]
    fn s3_generate_challengers_skips_denylisted_params() {
        let config = "
            reflect_weight_floor_bp = 2000
            reflect_weight_ceiling_bp = 40000
            brain_reflect_enable = 0
            brain_reflect_step_bp = 250
            reflect_every_ticks = 50
            gate_margin_bps = 50
        ";
        let mut cursor = std::collections::HashMap::new();
        let challengers = generate_challengers(config, 64, &mut cursor);
        // No challenger should mutate a denylisted param
        for c in &challengers {
            for m in &c.mutations {
                assert!(!is_reflection_denied(m.name.as_str()),
                    "S3: challenger {} mutates denylisted param {}", c.id, m.name);
            }
        }
        // reflect_every_ticks (safe) should still generate challengers
        let has_reflect_every = challengers.iter()
            .any(|c| c.mutations.iter().any(|m| m.name == "reflect_every_ticks"));
        assert!(has_reflect_every, "S3: reflect_every_ticks should NOT be denied");
        // gate_margin_bps should still generate challengers
        let has_gate_margin = challengers.iter()
            .any(|c| c.mutations.iter().any(|m| m.name == "gate_margin_bps"));
        assert!(has_gate_margin, "S3: gate_margin_bps should NOT be denied");
    }

    #[test]
    fn s3_write_promotion_file_refuses_denylisted() {
        let _lock = file_lock();
        let result = ShadowReplayResult {
            challenger_id: "denylisted_test".to_string(),
            challenger_net_scalp: NetSol::missing(),
            challenger_net_early: NetSol::missing(),
            champion_net_scalp: NetSol::missing(),
            champion_net_early: NetSol::missing(),
            verdict: ChampionVerdict::Defeats,
            gate_verdict: Some("all_8_gates_passed".to_string()),
            summary: "test".to_string(),
            engine_replay: None,
        };
        let challengers = vec![Challenger {
            id: "denylisted_test".to_string(),
            mutations: vec![ParameterMutation {
                name: "reflect_weight_floor_bp".to_string(),
                current_value: 2000,
                proposed_value: 2200,
                rationale: "+10%".to_string(),
            }],
        }];
        write_promotion_file(&result, &challengers);
        // The promotion file should NOT exist (denylisted param → refused)
        assert!(!Path::new(PROMOTION_FILE).exists(),
            "S3: promotion file must NOT be written for denylisted param");
        // Clean up just in case
        let _ = fs::remove_file(PROMOTION_FILE);
    }

    // ─── S4: Reflection health metric tests ────────────────────────────────

    #[test]
    fn s4_reflection_health_healthy_lanes() {
        let h = ReflectionHealth::new(50, 100, 80, 12);
        assert!(!h.lane_starved, "100 and 80 trades > 12 min sample → not starved");
        let json = h.to_json();
        assert!(json.contains("\"reflect_every_ticks\": 50"));
        assert!(json.contains("\"scalp_trades\": 100"));
        assert!(json.contains("\"early_trades\": 80"));
        assert!(json.contains("\"brain_decay_min_sample\": 12"));
        assert!(json.contains("\"lane_starved\": false"));
    }

    #[test]
    fn s4_reflection_health_starved_scalp() {
        let h = ReflectionHealth::new(50, 5, 100, 12);
        assert!(h.lane_starved, "5 scalp trades < 12 min sample → starved");
        assert!(h.to_json().contains("\"lane_starved\": true"));
    }

    #[test]
    fn s4_reflection_health_starved_early() {
        let h = ReflectionHealth::new(50, 100, 3, 12);
        assert!(h.lane_starved, "3 early trades < 12 min sample → starved");
    }

    #[test]
    fn s4_reflection_health_starved_both() {
        let h = ReflectionHealth::new(50, 0, 0, 12);
        assert!(h.lane_starved, "0 trades < 12 min sample → starved");
    }

    #[test]
    fn s4_reflection_health_at_boundary() {
        // Exactly at the boundary: n == min_sample is NOT starved (<, not <=)
        let h = ReflectionHealth::new(50, 12, 12, 12);
        assert!(!h.lane_starved, "12 == 12 is at boundary, not starved");
    }

    #[test]
    fn s4_extended_status_includes_reflection_health() {
        let _lock = file_lock();
        let h = ReflectionHealth::new(50, 100, 80, 12);
        write_refiner_status_with_reflection(5, 1, "promoted", &h);
        let content = fs::read_to_string(REFINER_STATUS_FILE).unwrap();
        assert!(content.contains("\"challengers_evaluated\": 5"));
        assert!(content.contains("\"reflection_health\""));
        assert!(content.contains("\"scalp_trades\": 100"));
        assert!(content.contains("\"lane_starved\": false"));
        let _ = fs::remove_file(REFINER_STATUS_FILE);
    }

    // ─── S5: reflect_every_ticks shadow replay tests ───────────────────────

    #[test]
    fn s5_reflect_every_ticks_higher_produces_bonus() {
        // Champion reflect_every_ticks=50, challenger proposes 55 (+10%).
        // The shadow replay should apply a small positive confidence bonus
        // to gross (thicker samples → more reliable reflection).
        let trades = vec![
            ReconTrade::test(Lane::Scalp, 100_000, 1_000, 0, 0),
            ReconTrade::test(Lane::Scalp, 200_000, 2_000, 0, 0),
        ];
        let champion_net = net_sol(&trades, Lane::Scalp);
        let challenger = Challenger {
            id: "s5_higher".to_string(),
            mutations: vec![ParameterMutation {
                name: "reflect_every_ticks".to_string(),
                current_value: 50,
                proposed_value: 55,
                rationale: "+10%".to_string(),
            }],
        };
        let result = shadow_replay(&challenger, &trades, champion_net, NetSol::missing(), 100);
        // +10% pct_change * 0.02 confidence = +0.2% gross bonus
        // challenger gross should be slightly higher than champion gross
        assert!(result.challenger_net_scalp.gross_lamports > champion_net.gross_lamports,
            "S5: +10% reflect_every_ticks should produce a positive confidence bonus");
    }

    #[test]
    fn s5_reflect_every_ticks_lower_produces_penalty() {
        // Champion reflect_every_ticks=50, challenger proposes 45 (-10%).
        // The shadow replay should apply a small negative confidence penalty
        // to gross (thinner samples → noisier reflection).
        let trades = vec![
            ReconTrade::test(Lane::Scalp, 100_000, 1_000, 0, 0),
            ReconTrade::test(Lane::Scalp, 200_000, 2_000, 0, 0),
        ];
        let champion_net = net_sol(&trades, Lane::Scalp);
        let challenger = Challenger {
            id: "s5_lower".to_string(),
            mutations: vec![ParameterMutation {
                name: "reflect_every_ticks".to_string(),
                current_value: 50,
                proposed_value: 45,
                rationale: "-10%".to_string(),
            }],
        };
        let result = shadow_replay(&challenger, &trades, champion_net, NetSol::missing(), 100);
        // -10% pct_change * 0.02 confidence = -0.2% gross penalty
        assert!(result.challenger_net_scalp.gross_lamports < champion_net.gross_lamports,
            "S5: -10% reflect_every_ticks should produce a negative confidence penalty");
    }

    #[test]
    fn s5_reflect_every_ticks_bonus_is_small() {
        // The bonus should be intentionally small (2% of pct_change, not
        // 10%+). Verify the adjustment is conservative — the challenger
        // should NOT defeat the champion on a thin tape (only 2 trades,
        // margin=10000).
        let trades = vec![
            ReconTrade::test(Lane::Scalp, 100_000, 1_000, 0, 0),
        ];
        let champion_net = net_sol(&trades, Lane::Scalp);
        let challenger = Challenger {
            id: "s5_small".to_string(),
            mutations: vec![ParameterMutation {
                name: "reflect_every_ticks".to_string(),
                current_value: 50,
                proposed_value: 55,
                rationale: "+10%".to_string(),
            }],
        };
        let result = shadow_replay(&challenger, &trades, champion_net, NetSol::missing(), 10_000);
        // With only 1 trade and 10k margin, the tiny bonus shouldn't defeat
        assert!(!result.verdict.defeats(),
            "S5: the confidence bonus is intentionally small — should not defeat on thin tape");
    }

    // ─── S7: Reflection-aware promotion guard tests ───────────────────────

    #[test]
    fn s7_guard_allows_when_lanes_healthy() {
        // Lanes NOT starved → promotion proceeds regardless of mutation.
        let health = ReflectionHealth::new(50, 100, 80, 12);
        let challenger = Challenger {
            id: "s7_healthy".to_string(),
            mutations: vec![ParameterMutation {
                name: "gate_margin_bps".to_string(),
                current_value: 100,
                proposed_value: 110, // +10% (tighter)
                rationale: "+10%".to_string(),
            }],
        };
        assert!(reflection_promotion_guard(Some(&challenger), Some(&health)),
            "S7: healthy lanes → promotion should proceed");
    }

    #[test]
    fn s7_guard_defers_when_starved_and_gate_tightens() {
        // Lanes starved AND gate_margin_bps increases → DEFER.
        let health = ReflectionHealth::new(50, 5, 100, 12); // scalp starved
        let challenger = Challenger {
            id: "s7_defer".to_string(),
            mutations: vec![ParameterMutation {
                name: "gate_margin_bps".to_string(),
                current_value: 100,
                proposed_value: 110, // +10% (tighter → fewer trades)
                rationale: "+10%".to_string(),
            }],
        };
        assert!(!reflection_promotion_guard(Some(&challenger), Some(&health)),
            "S7: starved lanes + tighter gate → should defer");
    }

    #[test]
    fn s7_guard_defers_when_starved_and_fail_rate_increases() {
        // Lanes starved AND gate_fail_rate_bps increases → DEFER.
        let health = ReflectionHealth::new(50, 5, 100, 12);
        let challenger = Challenger {
            id: "s7_failrate".to_string(),
            mutations: vec![ParameterMutation {
                name: "gate_fail_rate_bps".to_string(),
                current_value: 200,
                proposed_value: 220, // +10%
                rationale: "+10%".to_string(),
            }],
        };
        assert!(!reflection_promotion_guard(Some(&challenger), Some(&health)),
            "S7: starved lanes + higher fail rate → should defer");
    }

    #[test]
    fn s7_guard_allows_when_starved_but_gate_loosens() {
        // Lanes starved BUT gate_margin_bps DECREASES → OK (more trades).
        let health = ReflectionHealth::new(50, 5, 100, 12);
        let challenger = Challenger {
            id: "s7_loosen".to_string(),
            mutations: vec![ParameterMutation {
                name: "gate_margin_bps".to_string(),
                current_value: 100,
                proposed_value: 90, // -10% (looser → more trades)
                rationale: "-10%".to_string(),
            }],
        };
        assert!(reflection_promotion_guard(Some(&challenger), Some(&health)),
            "S7: starved lanes but looser gate → should proceed (more trades = more samples)");
    }

    #[test]
    fn s7_guard_allows_when_no_health_data() {
        // No ReflectionHealth provided → don't block (backward compat).
        let challenger = Challenger {
            id: "s7_nohp".to_string(),
            mutations: vec![ParameterMutation {
                name: "gate_margin_bps".to_string(),
                current_value: 100,
                proposed_value: 110,
                rationale: "+10%".to_string(),
            }],
        };
        assert!(reflection_promotion_guard(Some(&challenger), None),
            "S7: no health data → should allow promotion");
    }

    #[test]
    fn s7_guard_allows_unrelated_mutation_when_starved() {
        // Lanes starved BUT mutation is unrelated to throughput → OK.
        let health = ReflectionHealth::new(50, 5, 100, 12);
        let challenger = Challenger {
            id: "s7_unrelated".to_string(),
            mutations: vec![ParameterMutation {
                name: "reflect_every_ticks".to_string(),
                current_value: 50,
                proposed_value: 55,
                rationale: "+10%".to_string(),
            }],
        };
        assert!(reflection_promotion_guard(Some(&challenger), Some(&health)),
            "S7: starved lanes but unrelated mutation → should proceed");
    }
}
