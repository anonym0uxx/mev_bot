/**
 * @module entry/engine
 * Final Entry Engine per spec section 9.
 *
 * QUANT REDESIGN based on:
 * - Power law return distribution (Gabaix 2009, Cont 2001)
 * - Information asymmetry in token launches (Easley/O'Hara)
 * - Alpha decay in high-frequency markets (Budish/Cramton/Shim 2015)
 * - Adverse selection framework for meme coins
 *
 * Entry doctrine: enter ONLY when EV_enter_now > 0 AND EntryEdge > 0
 * AND breadth confirms velocity AND execution quality acceptable.
 */

import { createLogger } from '../utils/logger';
import { nowMs, ageS } from '../utils/time';
import {
  CandidatePacket, EntryEVCalculation, PositionSizing,
  ProbabilityOutputs, Regime,
} from '../types/state';
import { FeatureSnapshot } from '../types/features';
import { PumpQuantConfig } from '../types/config';
import { estimateExitSlippage } from '../features/dynamic-slippage';
import { computeAdverseSelectionPenalty } from '../features/adverse-selection';
import { recordEval, getMinEdge, detectCeilingViolation } from '../threshold/manager';

const log = createLogger('entry-engine');

export interface EntryDecision {
  shouldEnter: boolean;
  reason: string;
  ev: EntryEVCalculation;
  sizing: PositionSizing | null;
  hardFilterRejection: string | null;
}

/**
 * Evaluate whether to enter a position for a candidate token.
 *
 * Non-negotiable: EV_enter_now > 0 AND EntryEdge > 0
 */
export function evaluateEntry(
  packet: CandidatePacket,
  probabilities: ProbabilityOutputs,
  features: FeatureSnapshot,
  config: PumpQuantConfig,
  currentPositionCount: number,
  dailyLossSol: number,
  dailyEntryCount: number,
  isPaper: boolean
): EntryDecision {
  // ====== 9.2 HARD ENTRY FILTERS ======
  const hardFilter = checkHardFilters(packet, features, config, currentPositionCount, dailyLossSol, dailyEntryCount, probabilities, isPaper);
  if (hardFilter) {
    if (features.breadth_topology.unique_buyers_total >= 2) {
      log.info(`Hard filter reject ${packet.symbol || packet.mint.slice(0,8)}: ${hardFilter} (buyers=${features.breadth_topology.unique_buyers_total})`);
    }
    // Structured rejection log: always emitted for signal analysis
    const horizon = config.entry.ev_enter_horizon_s;
    const P_cont = horizon <= 5 ? probabilities.P_continuation_5s : probabilities.P_continuation_15s;
    const bcd = features.bonding_curve_dynamics;
    log.info(`REJECT ${packet.symbol || packet.mint.slice(0,8)}: ${hardFilter} (p_cont=${P_cont?.toFixed(3) || 'n/a'}, bcd=${bcd.bcd_score.toFixed(3)}, ce=${bcd.capital_efficiency_raw.toFixed(4)}, buyers=${features.breadth_topology.unique_buyers_total}, swaps=${features.total_swap_count})`);
    return {
      shouldEnter: false,
      reason: `Hard filter: ${hardFilter}`,
      ev: emptyEV(),
      sizing: null,
      hardFilterRejection: hardFilter,
    };
  }

  // ====== 9.3 ENTRY FORMULAS (Quant Redesign) ======

  const horizon = config.entry.ev_enter_horizon_s;
  const P_continuation = horizon <= 5
    ? probabilities.P_continuation_5s
    : probabilities.P_continuation_15s;
  const P_reversal = horizon <= 5
    ? probabilities.P_reversal_5s
    : probabilities.P_reversal_15s;
  const P_manipulation = probabilities.P_manipulation_event;

  const positionSize = config.risk.quick_spend_sol;

  // ---- Round-trip friction (computed ONCE) ----
  const roundTripFriction = computeRoundTripFriction(features, config, packet.regime);
  const routeEvAdjustment = features.friction_execution.route_ev_adjustment;

  // CALIBRATED 2026-03-26 (Citadel quant analysis of 8 live trades):
  // Realized mean winner return: ~58% gross (range 10-123%)
  // Realized mean loser return: ~-2.25% gross (DOA exit fires before -40% stop)
  // P-space: P_cont + P_reversal + P_DOA = 1  (DOA = flatline, ~2% loss)
  //
  // Using conservative 58% for continuation (realized mean vs 80% assumption),
  // and splitting reversal into organic (-40% stop) vs DOA (-3% flat, caught by 15s exit).
  // P_DOA estimated at 15% (tokens that stall immediately with 0% MFE).
  const E_return_continuation = 0.58;            // Calibrated from realized trade data
  const E_return_organic_reversal = -config.risk.raw_stop_pct; // -40% (full stop)
  const E_return_manipulation_reversal = -(config.risk.raw_stop_pct * 1.5); // -60% (rugs)
  const E_return_doa = -0.03;                     // DOA: ~-3% (friction + small move before exit)

  // ---- Probability decomposition ----
  // P_manipulation is CONDITIONAL: given reversal, probability it's manipulation-driven
  // P_DOA is carved out from reversal probability (flatline before stop)
  // Total: P_cont + P_organic_rev + P_manip_rev + P_DOA ≈ 1
  const P_DOA = Math.min(0.20, P_reversal * 0.40);  // ~40% of reversals are DOA flatlines
  const P_reversal_nondoa = P_reversal - P_DOA;
  const P_organic_reversal = P_reversal_nondoa * (1 - P_manipulation);
  const P_manipulation_reversal = P_reversal_nondoa * P_manipulation;

  // FIX #4 & #5: Add dynamic slippage + adverse selection penalty
  const tokenAgeS = ageS(packet.created_at);
  const adverseSelectionCost = computeAdverseSelectionPenalty(features, packet.regime, tokenAgeS) * positionSize;
  
  // ---- EV_enter_now ----
  // Single clean formula: expected return across all scenarios minus round-trip friction
  const EV_enter_now =
    P_continuation * positionSize * E_return_continuation +
    P_organic_reversal * positionSize * E_return_organic_reversal +
    P_manipulation_reversal * positionSize * E_return_manipulation_reversal +
    P_DOA * positionSize * E_return_doa -  // DOA flatline: small loss, not full stop
    roundTripFriction -
    adverseSelectionCost +
    routeEvAdjustment;

  // ---- EV_wait ----
  // On Pump.fun, alpha decays fast. Waiting has negative expected value when
  // there's positive velocity (you miss the move).
  const EV_wait = computeEVWait(packet, features, probabilities, config);

  const EntryEdge = EV_enter_now - Math.max(0, EV_wait);

  const ev: EntryEVCalculation = {
    EV_enter_now,
    EV_wait,
    EntryEdge,
    upside_net: P_continuation * positionSize * E_return_continuation,
    downside_net: -(P_organic_reversal * positionSize * E_return_organic_reversal +
                    P_manipulation_reversal * positionSize * E_return_manipulation_reversal),
    manipulation_cost: -P_manipulation_reversal * positionSize * E_return_manipulation_reversal,
    friction_cost_now: roundTripFriction,
    route_ev_adjustment: routeEvAdjustment,
  };

  // Record eval in adaptive threshold manager (before any gate check)
  recordEval(EntryEdge, EV_enter_now, P_continuation);

  // Log all evaluations
  log.info(`Entry eval ${packet.symbol || packet.mint.slice(0,8)}: EV=${EV_enter_now.toFixed(6)} Edge=${EntryEdge.toFixed(6)} P_cont=${P_continuation.toFixed(3)} P_rev=${P_reversal.toFixed(3)} P_manip=${P_manipulation.toFixed(3)} friction=${roundTripFriction.toFixed(6)} breadth=${features.breadth_topology.breadth_score.toFixed(3)} buyers=${features.breadth_topology.unique_buyers_total}`);

  // ====== 9.4 OBSERVATION PREMIUM ======
  // Reduced window: alpha decays fast on Pump.fun (Budish/Cramton/Shim 2015)
  const tokenAge = ageS(packet.created_at);

  // For tokens already in the target mcap band (vSol > min_vsol_in_curve), the token has
  // already proven survival by reaching that market cap. Observation window is irrelevant —
  // evaluate on current momentum signals only. Collapse window to 0.
  const minVSolCfg = (config.entry as any).min_vsol_in_curve ?? 0;
  const currentVSolForObs = features.bonding_curve_dynamics.capital_efficiency_raw * features.total_swap_count;
  const isEstablishedToken = minVSolCfg > 0 && currentVSolForObs >= minVSolCfg;

  const dynamicObsWindow = isEstablishedToken
    ? 0  // Already proven survival by reaching target mcap — evaluate immediately
    : features.flow_momentum.buy_notional_velocity_5s > 0.2
      ? 3  // Fast-moving new token: 3s observation
      : config.entry.observation_window_s;

  // Adaptive edge threshold — uses rolling p50 of observed edge distribution.
  // Falls back to config.entry.min_entry_edge if window is too small (cold start).
  // This prevents "threshold above model ceiling" failures permanently.
  // In paper mode: skip adaptive floor entirely — purpose is data collection, not edge selectivity.
  const adaptiveMinEdge = isPaper ? 0 : getMinEdge(0.50);
  const effectiveMinEdge = isPaper ? 0 : Math.max(adaptiveMinEdge, config.entry.min_entry_edge);

  // Warn if config threshold is above empirical ceiling (would block all entries)
  if (detectCeilingViolation(config.entry.min_entry_edge)) {
    log.warn(`CEILING VIOLATION: config min_entry_edge=${config.entry.min_entry_edge.toFixed(6)} > observed max. Using adaptive p50=${adaptiveMinEdge.toFixed(6)}`);
  }

  if (tokenAge < dynamicObsWindow) {
    if (EV_enter_now > 0 && EntryEdge > effectiveMinEdge * 2) {
      log.info(`Observation override for ${packet.symbol || packet.mint.slice(0,8)}: age=${tokenAge.toFixed(1)}s Edge=${EntryEdge.toFixed(6)}`);
    } else {
      if (EV_enter_now > 0) {
        log.info(`Obs window block ${packet.symbol || packet.mint.slice(0,8)}: age=${tokenAge.toFixed(1)}s < ${dynamicObsWindow}s (EV=${EV_enter_now.toFixed(6)} but edge ${EntryEdge.toFixed(6)} < ${(effectiveMinEdge * 2).toFixed(6)})`);
      }
      return {
        shouldEnter: false,
        reason: `Observation: ${tokenAge.toFixed(1)}s < ${dynamicObsWindow}s`,
        ev,
        sizing: null,
        hardFilterRejection: null,
      };
    }
  }

  // Log when we pass observation
  if (EV_enter_now > 0) {
    log.info(`POST-OBS ${packet.symbol || packet.mint.slice(0,8)}: age=${tokenAge.toFixed(1)}s EV=${EV_enter_now.toFixed(6)} Edge=${EntryEdge.toFixed(6)} breadth=${features.breadth_topology.breadth_score.toFixed(3)}`);
  }

  // ====== CORE ENTRY DECISION ======
  if (EV_enter_now <= 0) {
    return {
      shouldEnter: false,
      reason: `EV_enter_now <= 0: ${EV_enter_now.toFixed(6)}`,
      ev,
      sizing: null,
      hardFilterRejection: null,
    };
  }

  if (EntryEdge <= effectiveMinEdge) {
    return {
      shouldEnter: false,
      reason: `EntryEdge ${EntryEdge.toFixed(6)} below adaptive threshold ${effectiveMinEdge.toFixed(6)}`,
      ev,
      sizing: null,
      hardFilterRejection: null,
    };
  }

  // Breadth confirms velocity
  if (features.breadth_topology.breadth_score < config.entry.min_breadth_for_entry) {
    return {
      shouldEnter: false,
      reason: `Breadth ${features.breadth_topology.breadth_score.toFixed(3)} below ${config.entry.min_breadth_for_entry}`,
      ev,
      sizing: null,
      hardFilterRejection: null,
    };
  }

  // Execution quality
  if (features.friction_execution.route_score < 0.3) {
    return {
      shouldEnter: false,
      reason: `Route score too low: ${features.friction_execution.route_score.toFixed(3)}`,
      ev,
      sizing: null,
      hardFilterRejection: null,
    };
  }

  // ====== 9.5 POSITION SIZING ======
  const sizing = computePositionSizing(config, features, roundTripFriction);

  if (sizing.position_size <= 0) {
    return {
      shouldEnter: false,
      reason: 'Position size computed to zero',
      ev,
      sizing,
      hardFilterRejection: null,
    };
  }

  log.info(`🟢 ENTRY APPROVED ${packet.symbol || packet.mint.slice(0,8)}: EV=${EV_enter_now.toFixed(6)} Edge=${EntryEdge.toFixed(6)} Size=${sizing.position_size.toFixed(4)} SOL buyers=${features.breadth_topology.unique_buyers_total}`);

  return {
    shouldEnter: true,
    reason: `Entry approved: EV=${EV_enter_now.toFixed(6)}, Edge=${EntryEdge.toFixed(6)}, Size=${sizing.position_size.toFixed(4)} SOL`,
    ev,
    sizing,
    hardFilterRejection: null,
  };
}

// ====== HARD ENTRY FILTERS ======

function checkHardFilters(
  packet: CandidatePacket,
  features: FeatureSnapshot,
  config: PumpQuantConfig,
  currentPositionCount: number,
  dailyLossSol: number,
  dailyEntryCount: number,
  probabilities: ProbabilityOutputs,
  isPaper: boolean = false
): string | null {
  // EARLY_CURVE, MID_CURVE, and LATE_CURVE are all tradeable
  // EXCLUDED (mayhem/tokenized-agent) and POST_MIGRATION are not
  if (packet.regime === Regime.EXCLUDED || packet.regime === Regime.POST_MIGRATION) {
    return 'excluded_regime';
  }
  if (features.creator_wallet_prior.creator_sell_flag) return 'creator_sold';

  // Hard gate: creator prior floor — blocks creators with KNOWN negative history.
  // IMPORTANT: composite_prior = 0.000 means UNKNOWN creator (no enrichment data), NOT bad creator.
  // The gate only fires when prior is demonstrably negative AND creator data exists.
  //
  // Config: entry.min_creator_prior (number | null)
  //   null  = gate disabled entirely
  //   -0.05 = block creators with clearly negative history (default)
  //   0.0   = block any non-positive known creator (stricter)
  //
  // BUG FIX: Old condition `minCreatorPrior > 0` meant setting 0.0 or any negative value
  // silently disabled the gate. Condition is now `minCreatorPrior !== null`.
  const minCreatorPrior = (config.entry as any).min_creator_prior ?? -0.05;
  const creatorPrior = features.creator_wallet_prior.composite_prior;
  const hasCreatorData = creatorPrior !== 0 || features.creator_wallet_prior.creator_history_score > 0;
  if (minCreatorPrior !== null && hasCreatorData && creatorPrior < minCreatorPrior) {
    return `creator_prior_low (${creatorPrior.toFixed(3)} < ${minCreatorPrior})`;
  }

  if (features.manipulation_distribution.hard_shock) return 'manipulation_hard_shock';

  // DOA pre-filter: block tokens where velocity is already dying and buyer flow has stalled.
  // Eliminates flatline trades that waste capital and slot capacity.
  // Two-condition filter (both must be true):
  //   1. buy_velocity_5s/buy_velocity_15s < threshold (velocity decaying fast)
  //   2. new_unique_buyers_3s < min_buyers (no new buyers sustaining the move)
  //
  // If new_unique_buyers_3s is not populated by the feature engine (null/undefined),
  // we STILL apply the filter using velocity alone with a stricter threshold (0.20)
  // to avoid a silent no-op when buyer flow data is unavailable.
  const doaVelocityThreshold = (config.entry as any).doa_velocity_decay_threshold ?? 0.35;
  const doaVelocityOnlyThreshold = doaVelocityThreshold * 0.6; // Stricter fallback (e.g. 0.21)
  const doaMinNewBuyers = (config.entry as any).doa_min_new_buyers_3s ?? 1;
  const vel5s = features.flow_momentum.buy_notional_velocity_5s;
  const vel15s = features.flow_momentum.buy_notional_velocity_15s;
  // vel15s ≈ 0 AND vel5s ≈ 0 → both windows dead. Don't use ratio (0/0 = undefined).
  // vel15s ≈ 0 AND vel5s > 0 → token just launched, velocity building → ratio 1.0 (pass).
  const bothVelDead = vel15s < 0.001 && vel5s < 0.001;
  const velocityDecayRatio = vel15s > 0.001 ? vel5s / vel15s : (vel5s > 0.001 ? 1.0 : 0.0);
  const recentBuyers = (features.flow_momentum as any).new_unique_buyers_3s ?? null;
  if (recentBuyers !== null) {
    // Full filter: velocity decay AND no recent buyers
    if (velocityDecayRatio < doaVelocityThreshold && recentBuyers < doaMinNewBuyers) {
      return `doa_signal (vel_ratio=${velocityDecayRatio.toFixed(2)}, new_buyers_3s=${recentBuyers})`;
    }
  } else if (bothVelDead) {
    // Both velocity windows are zero — token is in consolidation at t=60s observation window.
    // Only block if capital efficiency is also poor (ce < 0.5 SOL/swap = truly dead token).
    // Tokens with good CE (arXiv signal) that are momentarily quiet should still be evaluated.
    const ce = features.bonding_curve_dynamics.capital_efficiency_raw;
    if (ce < 0.5) {
      return `doa_signal_velocity_only (vel_ratio=0.00, ce=${ce.toFixed(4)} < 0.5)`;
    }
    // Good CE + both vels dead = consolidation pause, not DOA. Fall through to probability gate.
  } else {
    // Fallback: velocity-only filter with stricter threshold (buyer data not available)
    if (velocityDecayRatio < doaVelocityOnlyThreshold) {
      return `doa_signal_velocity_only (vel_ratio=${velocityDecayRatio.toFixed(2)} < ${doaVelocityOnlyThreshold.toFixed(2)})`;
    }
  }
  if (features.friction_execution.execution_freshness_s > config.friction.stale_threshold_s) return 'stale_friction';
  if (ageS(packet.last_updated) > config.health.market_feed_stale_s) return 'stale_feed';
  if (features.manipulation_distribution.manipulation_penalty > config.manipulation.hard_threshold) return 'manipulation_risk';
  if (features.friction_execution.expected_entry_slippage > config.entry.max_slippage_pct) return 'slippage_high';
  if (!isPaper && currentPositionCount >= config.risk.max_positions) return 'max_positions';
  if (!isPaper && Math.abs(dailyLossSol) >= config.risk.max_daily_loss_sol) return 'daily_loss_limit';
  if (features.breadth_topology.unique_buyers_total < config.entry.min_unique_buyers) return 'insufficient_buyers';

  // Market cap gate: filter by vSolInBondingCurve at entry time.
  // Paper trade analysis shows $5k–$31k mcap band has 44.7% WR vs 3–13% below $5k.
  // At $90/SOL: $5k mcap ≈ 55 SOL in curve, $31k ≈ 344 SOL in curve.
  // Config: min_vsol_in_curve (default 0 = disabled), max_vsol_in_curve (default 0 = disabled)
  // Derive current vSol: capital_efficiency_raw = vSol / totalSwapCount
  const currentVSol = features.bonding_curve_dynamics.capital_efficiency_raw * features.total_swap_count;
  const minVSol = (config.entry as any).min_vsol_in_curve ?? 0;
  const maxVSol = (config.entry as any).max_vsol_in_curve ?? 0;
  if (minVSol > 0 && currentVSol < minVSol) return `mcap_too_low (vSol=${currentVSol.toFixed(1)} < ${minVSol})`;
  if (maxVSol > 0 && currentVSol > maxVSol) return `mcap_too_high (vSol=${currentVSol.toFixed(1)} > ${maxVSol})`;

  // Daily entry cap — hard stop once max_daily_entries trades have been executed today.
  // Prevents runaway trading when the bot is trigger-happy on a volatile session.
  // PAPER MODE: skip daily cap entirely — we want maximum learning data, and
  // live trade history from earlier sessions should not constrain paper evaluation.
  if (!isPaper) {
    const maxDailyEntries = config.risk.max_daily_entries;
    if (maxDailyEntries > 0 && dailyEntryCount >= maxDailyEntries) {
      return `daily_entry_limit (${dailyEntryCount}/${maxDailyEntries})`;
    }
  }

  // Minimum P_continuation gate — reject weak signals regardless of EV.
  // Uses the same horizon as the EV calculation.
  const horizon = config.entry.ev_enter_horizon_s;
  const P_cont = horizon <= 5 ? probabilities.P_continuation_5s : probabilities.P_continuation_15s;
  const minPCont = config.entry.min_p_continuation ?? 0;
  if (minPCont > 0 && P_cont < minPCont) {
    const bcd = features.bonding_curve_dynamics;
    log.info(`REJECT ${(packet as any).symbol || 'token'}: p_continuation_low (p_cont=${P_cont.toFixed(3)} < ${minPCont}, bcd=${bcd.bcd_score.toFixed(3)}, ce=${bcd.capital_efficiency_raw.toFixed(4)}, ice=${bcd.initial_capital_efficiency.toFixed(3)}, shape=${bcd.accumulation_shape.toFixed(3)}, swaps=${features.total_swap_count})`);
    return `p_continuation_low (${P_cont.toFixed(3)} < ${minPCont})`;
  }

  // Concentration: scale by buyer count (natural high concentration with few buyers)
  const buyers = features.breadth_topology.unique_buyers_total;
  const concThreshold = buyers < 15 ? 1.0 : buyers < 30 ? 0.90 : config.entry.max_concentration_top10;
  if (features.breadth_topology.top_10_concentration > concThreshold) return 'concentration_high';

  return null;
}

// ====== ROUND-TRIP FRICTION ======

function computeRoundTripFriction(
  features: FeatureSnapshot,
  config: PumpQuantConfig,
  regime: Regime
): number {
  const fees = config.fees;
  const regimeOverride = fees.regime_fee_overrides[regime] || {};
  const pumpFee = regimeOverride.pump_fee_pct ?? fees.pump_fee_pct;
  const portalFee = fees.pump_portal_fee_pct;
  const baseFee = fees.solana_base_fee_sol;
  const priorityFee = config.execution.default_priority_fee_sol;

  // FIX #4: Use dynamic exit slippage estimation
  const exitSlippage = estimateExitSlippage(features, null, config);
  
  // Round-trip: entry fees + exit fees + slippage both ways
  const entryPct = pumpFee + portalFee + features.friction_execution.expected_entry_slippage;
  const exitPct = pumpFee + portalFee + exitSlippage;  // Use dynamic slippage
  const roundTripPct = entryPct + exitPct;

  // Fixed costs: 2x base fee + 2x priority fee (entry + exit)
  const fixedCosts = (baseFee + priorityFee) * 2;

  const size = config.risk.quick_spend_sol;
  return size * roundTripPct + fixedCosts;
}

// ====== EV_WAIT ======

function computeEVWait(
  packet: CandidatePacket,
  features: FeatureSnapshot,
  probabilities: ProbabilityOutputs,
  config: PumpQuantConfig
): number {
  // On Pump.fun, alpha decays FAST. Positive velocity means waiting costs money.
  // (Budish/Cramton/Shim 2015: "The High-Frequency Trading Arms Race")
  const velocity = features.flow_momentum.buy_notional_velocity_5s;
  const positionSize = config.risk.quick_spend_sol;

  if (velocity > 0.1) {
    // Active token: waiting means missing the move
    // Opportunity cost scales with velocity
    return -velocity * 0.05 * positionSize; // Negative = waiting costs money
  }

  // Slow/no velocity: small positive value of waiting (info gain)
  const uncertainty = 1 - Math.abs(probabilities.P_continuation_5s - 0.5) * 2;
  return uncertainty * 0.005 * positionSize;
}

// ====== POSITION SIZING ======

export function computePositionSizing(
  config: PumpQuantConfig,
  features: FeatureSnapshot,
  frictionCost: number
): PositionSizing {
  const risk = config.risk;
  const risk_budget = risk.bankroll_sol * risk.risk_per_trade_pct;

  const effective_stop_pct =
    risk.raw_stop_pct +
    config.fees.pump_fee_pct * 2 + // Entry + exit
    features.friction_execution.expected_exit_slippage +
    config.friction.safety_buffer_pct;

  const rawFromRisk = effective_stop_pct > 0 ? risk_budget / effective_stop_pct : 0;

  const caps: { name: string; value: number }[] = [
    { name: 'risk_budget', value: rawFromRisk },
    { name: 'quick_spend', value: risk.quick_spend_sol },
    { name: 'max_alloc', value: risk.bankroll_sol * risk.max_alloc_pct },
    { name: 'liquidity_cap', value: risk.liquidity_cap_sol },
    { name: 'slippage_cap', value: risk.slippage_cap_sol },
  ];

  let minCap = caps[0];
  for (const cap of caps) {
    if (cap.value < minCap.value) minCap = cap;
  }

  return {
    risk_budget,
    effective_stop_pct,
    raw_position_size: rawFromRisk,
    position_size: Math.max(0, minCap.value),
    limiting_factor: minCap.name as PositionSizing['limiting_factor'],
  };
}

function emptyEV(): EntryEVCalculation {
  return { EV_enter_now: 0, EV_wait: 0, EntryEdge: 0, upside_net: 0, downside_net: 0, manipulation_cost: 0, friction_cost_now: 0, route_ev_adjustment: 0 };
}
