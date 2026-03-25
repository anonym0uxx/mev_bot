/**
 * Regime classifier tests — spec section 5
 */
import { describe, it, expect } from 'vitest';
import {
  classifyRegime, computeBondingCurveProgress,
  isTradeableRegime, detectMayhem, detectTokenizedAgent,
} from '../../src/regime/classifier';
import { Regime } from '../../src/types/state';

const defaultRegimeConfig = {
  early_curve_max_progress: 0.15,
  mid_curve_max_progress: 0.50,
  late_curve_max_progress: 0.85,
  graduation_boundary_start: 0.85,
  graduation_boundary_end: 1.0,
  max_token_age_s: 600,
  exclude_mayhem: true,
  exclude_tokenized_agent: true,
};

describe('classifyRegime', () => {
  it('classifies EARLY_CURVE for low progress', () => {
    const regime = classifyRegime({
      bondingCurveProgress: 0.05,
      migrated: false,
      tokenAgeMs: 5000,
      isMayhem: false,
      isTokenizedAgent: false,
      createdAt: Date.now() - 5000,
    }, defaultRegimeConfig);
    expect(regime).toBe(Regime.EARLY_CURVE);
  });

  it('classifies MID_CURVE', () => {
    const regime = classifyRegime({
      bondingCurveProgress: 0.30,
      migrated: false,
      tokenAgeMs: 10000,
      isMayhem: false,
      isTokenizedAgent: false,
      createdAt: Date.now() - 10000,
    }, defaultRegimeConfig);
    expect(regime).toBe(Regime.MID_CURVE);
  });

  it('classifies LATE_CURVE', () => {
    const regime = classifyRegime({
      bondingCurveProgress: 0.70,
      migrated: false,
      tokenAgeMs: 10000,
      isMayhem: false,
      isTokenizedAgent: false,
      createdAt: Date.now() - 10000,
    }, defaultRegimeConfig);
    expect(regime).toBe(Regime.LATE_CURVE);
  });

  it('classifies GRADUATION_BOUNDARY', () => {
    const regime = classifyRegime({
      bondingCurveProgress: 0.90,
      migrated: false,
      tokenAgeMs: 10000,
      isMayhem: false,
      isTokenizedAgent: false,
      createdAt: Date.now() - 10000,
    }, defaultRegimeConfig);
    expect(regime).toBe(Regime.GRADUATION_BOUNDARY);
  });

  it('classifies POST_MIGRATION when migrated', () => {
    const regime = classifyRegime({
      bondingCurveProgress: 1.0,
      migrated: true,
      tokenAgeMs: 10000,
      isMayhem: false,
      isTokenizedAgent: false,
      createdAt: Date.now() - 10000,
    }, defaultRegimeConfig);
    expect(regime).toBe(Regime.POST_MIGRATION);
  });

  it('excludes Mayhem tokens when configured', () => {
    const regime = classifyRegime({
      bondingCurveProgress: 0.10,
      migrated: false,
      tokenAgeMs: 1000,
      isMayhem: true,
      isTokenizedAgent: false,
      createdAt: Date.now() - 1000,
    }, defaultRegimeConfig);
    expect(regime).toBe(Regime.EXCLUDED);
  });

  it('excludes Tokenized-Agent tokens when configured', () => {
    const regime = classifyRegime({
      bondingCurveProgress: 0.10,
      migrated: false,
      tokenAgeMs: 1000,
      isMayhem: false,
      isTokenizedAgent: true,
      createdAt: Date.now() - 1000,
    }, defaultRegimeConfig);
    expect(regime).toBe(Regime.EXCLUDED);
  });

  it('excludes tokens older than max age', () => {
    const regime = classifyRegime({
      bondingCurveProgress: 0.10,
      migrated: false,
      tokenAgeMs: 700_000,
      isMayhem: false,
      isTokenizedAgent: false,
      createdAt: Date.now() - 700_000,
    }, defaultRegimeConfig);
    expect(regime).toBe(Regime.EXCLUDED);
  });
});

describe('computeBondingCurveProgress', () => {
  it('returns 0 for initial supply', () => {
    // Initial = ~1_073_000_000_000_000, progress = 0
    expect(computeBondingCurveProgress(1_073_000_000_000_000)).toBeCloseTo(0, 1);
  });

  it('returns ~1 for near-empty curve', () => {
    expect(computeBondingCurveProgress(1000)).toBeCloseTo(1, 1);
  });
});

describe('isTradeableRegime', () => {
  it('rejects EARLY_CURVE (too risky per QUANT_STRATEGY)', () => {
    expect(isTradeableRegime(Regime.EARLY_CURVE)).toBe(false);
  });
  it('allows MID_CURVE', () => {
    expect(isTradeableRegime(Regime.MID_CURVE)).toBe(true);
  });
  it('rejects EXCLUDED', () => {
    expect(isTradeableRegime(Regime.EXCLUDED)).toBe(false);
  });
  it('rejects POST_MIGRATION', () => {
    expect(isTradeableRegime(Regime.POST_MIGRATION)).toBe(false);
  });
});

describe('detectMayhem', () => {
  it('detects mayhem keywords', () => {
    expect(detectMayhem('Mayhem Token', 'MAYHEM', '')).toBe(true);
  });
  it('returns false for normal tokens', () => {
    expect(detectMayhem('Cool Dog', 'CDOG', '')).toBe(false);
  });
});

describe('detectTokenizedAgent', () => {
  it('detects agent keywords', () => {
    expect(detectTokenizedAgent('AI Agent Token', 'AGENT', '')).toBe(true);
  });
  it('returns false for normal tokens', () => {
    expect(detectTokenizedAgent('Cool Cat', 'CCAT', '')).toBe(false);
  });
});
