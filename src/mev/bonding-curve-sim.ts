import { createLogger } from '../utils/logger';

const log = createLogger('mev:bonding-curve-sim');

const LAMPORTS_PER_SOL = 1_000_000_000n;

export interface BuyResult {
  tokensOut: bigint;
  newVSol: bigint;
  newVTokens: bigint;
  priceImpactPct: number;
  minTokensOut: bigint; // with slippage applied
}

export interface SellResult {
  solOut: bigint;
  newVSol: bigint;
  newVTokens: bigint;
  priceImpactPct: number;
}

export class BondingCurveSimulator {
  /**
   * Simulate a buy on Pump.fun constant product curve.
   * @param vSolLamports current virtual SOL reserves (lamports)
   * @param vTokens current virtual token reserves
   * @param solInLamports SOL to spend (lamports)
   * @param slippageBps slippage tolerance in basis points (e.g. 100 = 1%)
   */
  simulateBuy(vSolLamports: bigint, vTokens: bigint, solInLamports: bigint, slippageBps = 100n): BuyResult {
    // Pump.fun charges 1% fee on buys
    const feeNumerator = 100n;
    const feeDenominator = 10000n;
    const fee = (solInLamports * feeNumerator) / feeDenominator;
    const solInAfterFee = solInLamports - fee;

    // Constant product: (vSol + solIn) * (vTokens - tokensOut) = vSol * vTokens
    const k = vSolLamports * vTokens;
    const newVSol = vSolLamports + solInAfterFee;
    const newVTokens = k / newVSol;
    const tokensOut = vTokens - newVTokens;

    const priceImpactPct = Number(solInAfterFee * 10000n / vSolLamports) / 100;

    // Apply slippage: minTokensOut = tokensOut * (10000 - slippageBps) / 10000
    const minTokensOut = (tokensOut * (10000n - slippageBps)) / 10000n;

    return { tokensOut, newVSol, newVTokens, priceImpactPct, minTokensOut };
  }

  /**
   * Simulate a sell on Pump.fun constant product curve.
   */
  simulateSell(vSolLamports: bigint, vTokens: bigint, tokensIn: bigint): SellResult {
    // Constant product
    const k = vSolLamports * vTokens;
    const newVTokens = vTokens + tokensIn;
    const newVSol = k / newVTokens;
    const solOut = vSolLamports - newVSol;

    // Pump.fun charges 1% fee on sells
    const fee = (solOut * 100n) / 10000n;
    const solOutAfterFee = solOut - fee;

    const priceImpactPct = Number(tokensIn * 10000n / vTokens) / 100;

    return { solOut: solOutAfterFee, newVSol, newVTokens, priceImpactPct };
  }

  /**
   * Compute minimum next buyer size for our position to be profitable.
   * Returns minimum solIn (lamports) the next buyer must spend for us to break even.
   */
  computeBreakevenNextBuy(
    vSolAfterOurEntry: bigint,
    vTokensAfterOurEntry: bigint,
    ourTokensHeld: bigint,
    costBasisLamports: bigint,
    tipAndFeesLamports: bigint
  ): bigint {
    // Binary search: find minimum nextBuyLamports where our sell proceeds > cost + fees
    let lo = 1_000_000n; // 0.001 SOL min
    let hi = 10_000_000_000n; // 10 SOL max

    for (let i = 0; i < 50; i++) {
      const mid = (lo + hi) / 2n;
      // Simulate next buyer buying mid lamports
      const afterNextBuy = this.simulateBuy(vSolAfterOurEntry, vTokensAfterOurEntry, mid);
      // Simulate us selling into the new reserves
      const ourSell = this.simulateSell(afterNextBuy.newVSol, afterNextBuy.newVTokens, ourTokensHeld);
      if (ourSell.solOut > costBasisLamports + tipAndFeesLamports) {
        hi = mid;
      } else {
        lo = mid;
      }
    }
    return hi;
  }
}
