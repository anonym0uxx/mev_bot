/**
 * @module friction/model
 * Friction model per spec section 12.
 *
 * Models ALL live costs:
 * - Pump / PumpSwap fee
 * - PumpPortal fee
 * - Solana base fee
 * - Solana priority fee
 * - Expected entry slippage
 * - Expected exit slippage
 * - Route-specific landing degradation
 *
 * Uses net liquidation value everywhere.
 * Fee schedules versioned by config and regime.
 */

import { createLogger } from '../utils/logger';
import { PumpQuantConfig, RouteMode, RegimeFeeOverride } from '../types/config';
import { Regime } from '../types/state';

const log = createLogger('friction');

/** Complete friction breakdown for a trade */
export interface FrictionBreakdown {
  /** Pump.fun bonding curve fee (pre-graduation) */
  pumpFeePct: number;
  /** PumpSwap fee (post-graduation) */
  pumpSwapFeePct: number;
  /** PumpPortal platform fee */
  portalFeePct: number;
  /** Solana base transaction fee (SOL) */
  solanaBaseFee: number;
  /** Solana priority fee (SOL) */
  priorityFee: number;
  /** Expected entry slippage as pct */
  entrySlippagePct: number;
  /** Expected exit slippage as pct */
  exitSlippagePct: number;
  /** Route-specific landing degradation */
  landingDegradationPct: number;
  /** Total percentage-based cost */
  totalPctCost: number;
  /** Total fixed cost (SOL) */
  totalFixedCost: number;
  /** Net cost for given trade size (SOL) */
  netCostForSize: number;
}

/**
 * Compute comprehensive friction breakdown for an entry trade.
 */
export function computeEntryFriction(
  tradeSizeSol: number,
  regime: Regime,
  routeMode: RouteMode,
  entrySlippagePct: number,
  priorityFeeSol: number,
  config: PumpQuantConfig
): FrictionBreakdown {
  return computeFriction(
    tradeSizeSol, regime, routeMode,
    entrySlippagePct, 0, // No exit slippage for entry
    priorityFeeSol, config
  );
}

/**
 * Compute comprehensive friction breakdown for an exit trade.
 */
export function computeExitFriction(
  tradeSizeSol: number,
  regime: Regime,
  routeMode: RouteMode,
  exitSlippagePct: number,
  priorityFeeSol: number,
  config: PumpQuantConfig
): FrictionBreakdown {
  return computeFriction(
    tradeSizeSol, regime, routeMode,
    0, exitSlippagePct, // No entry slippage for exit
    priorityFeeSol, config
  );
}

/**
 * Compute net liquidation value: what you actually get after all costs.
 * This is the CORE profitability metric per spec — never use raw price.
 */
export function computeNetLiquidationValue(
  grossValue: number,
  regime: Regime,
  routeMode: RouteMode,
  exitSlippagePct: number,
  priorityFeeSol: number,
  config: PumpQuantConfig
): number {
  const friction = computeExitFriction(
    grossValue, regime, routeMode, exitSlippagePct, priorityFeeSol, config
  );
  return grossValue - friction.netCostForSize;
}

/**
 * Compute total round-trip friction (entry + exit) for trade sizing.
 */
export function computeRoundTripFriction(
  tradeSizeSol: number,
  regime: Regime,
  routeMode: RouteMode,
  entrySlippagePct: number,
  exitSlippagePct: number,
  priorityFeeSol: number,
  config: PumpQuantConfig
): { entryFriction: FrictionBreakdown; exitFriction: FrictionBreakdown; totalCost: number } {
  const entryFriction = computeEntryFriction(
    tradeSizeSol, regime, routeMode, entrySlippagePct, priorityFeeSol, config
  );
  // Estimate exit on the same size for simplicity
  const exitFriction = computeExitFriction(
    tradeSizeSol, regime, routeMode, exitSlippagePct, priorityFeeSol, config
  );
  return {
    entryFriction,
    exitFriction,
    totalCost: entryFriction.netCostForSize + exitFriction.netCostForSize,
  };
}

/**
 * Core friction computation.
 */
function computeFriction(
  tradeSizeSol: number,
  regime: Regime,
  routeMode: RouteMode,
  entrySlippagePct: number,
  exitSlippagePct: number,
  priorityFeeSol: number,
  config: PumpQuantConfig
): FrictionBreakdown {
  const fees = config.fees;
  const regimeOverride: RegimeFeeOverride = fees.regime_fee_overrides[regime] || {};

  // Fee selection based on regime
  const pumpFeePct = regimeOverride.pump_fee_pct ?? fees.pump_fee_pct;
  const pumpSwapFeePct = regimeOverride.pump_swap_fee_pct ?? fees.pump_swap_fee_pct;
  const portalFeePct = fees.pump_portal_fee_pct;
  const solanaBaseFee = fees.solana_base_fee_sol;

  // Route-specific landing degradation
  let landingDegradationPct: number;
  switch (routeMode) {
    case 'lightning':
      landingDegradationPct = config.friction.landing_degradation_lightning_pct;
      break;
    case 'local':
    default:
      landingDegradationPct = config.friction.landing_degradation_local_pct;
      break;
  }

  // Use pump fee for pre-graduation, pump_swap fee for post-migration
  const effectivePumpFee = regime === Regime.POST_MIGRATION ? pumpSwapFeePct : pumpFeePct;

  // Total percentage-based costs
  const totalPctCost = effectivePumpFee + portalFeePct + entrySlippagePct + exitSlippagePct + landingDegradationPct;

  // Total fixed costs (SOL)
  const totalFixedCost = solanaBaseFee + priorityFeeSol;

  // Net cost for the given trade size
  const netCostForSize = tradeSizeSol * totalPctCost + totalFixedCost;

  return {
    pumpFeePct: effectivePumpFee,
    pumpSwapFeePct,
    portalFeePct,
    solanaBaseFee,
    priorityFee: priorityFeeSol,
    entrySlippagePct,
    exitSlippagePct,
    landingDegradationPct,
    totalPctCost,
    totalFixedCost,
    netCostForSize,
  };
}
