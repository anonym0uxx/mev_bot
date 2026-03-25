/**
 * Manipulation model tests — spec section 11
 */
import { describe, it, expect } from 'vitest';
import { assessManipulationRisk } from '../../src/manipulation/model';
import { PumpQuantConfig } from '../../src/types/config';
import * as fs from 'fs';
import * as path from 'path';

const config: PumpQuantConfig = JSON.parse(
  fs.readFileSync(path.join(__dirname, '../../config/default.json'), 'utf-8')
);

describe('assessManipulationRisk', () => {
  it('flags hard shock on creator_sell', () => {
    const features = {
      creator_sell: true,
      same_size_prints: 0,
      price_breadth_divergence: 0,
      concentration_worsening: 0,
      cluster_correlation: 0,
      suspicious_burst: 0,
      slippage_shock: 0,
      manipulation_penalty: 0,
      hard_shock: false,
      hard_shock_reason: null as string | null,
    };
    const result = assessManipulationRisk(features, config.manipulation);
    expect(result.hardShock).toBe(true);
    expect(result.hardShockReason).toBe('creator_sell');
  });

  it('reads continuous penalty from pre-computed features', () => {
    const features = {
      creator_sell: false,
      same_size_prints: 3,
      price_breadth_divergence: 0.2,
      concentration_worsening: 0.1,
      cluster_correlation: 0.5,
      suspicious_burst: 0.3,
      slippage_shock: 0.1,
      manipulation_penalty: 0.35, // Pre-computed by feature engine
      hard_shock: false,
      hard_shock_reason: null as string | null,
    };
    const result = assessManipulationRisk(features, config.manipulation);
    expect(result.hardShock).toBe(false);
    expect(result.penalty).toBe(0.35);
    expect(result.penalty).toBeLessThanOrEqual(1);
  });

  it('returns zero penalty for clean token', () => {
    const features = {
      creator_sell: false,
      same_size_prints: 0,
      price_breadth_divergence: 0,
      concentration_worsening: 0,
      cluster_correlation: 0,
      suspicious_burst: 0,
      slippage_shock: 0,
      manipulation_penalty: 0,
      hard_shock: false,
      hard_shock_reason: null as string | null,
    };
    const result = assessManipulationRisk(features, config.manipulation);
    expect(result.hardShock).toBe(false);
    expect(result.penalty).toBe(0);
  });
});
