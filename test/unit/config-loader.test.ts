/**
 * Config loader tests — spec section 21
 */
import { describe, it, expect } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import { PumpQuantConfig } from '../../src/types/config';

describe('config/default.json', () => {
  const config: PumpQuantConfig = JSON.parse(
    fs.readFileSync(path.join(__dirname, '../../config/default.json'), 'utf-8')
  );

  it('has all required top-level sections', () => {
    expect(config.regime).toBeDefined();
    expect(config.manipulation).toBeDefined();
    expect(config.friction).toBeDefined();
    expect(config.entry).toBeDefined();
    expect(config.exit).toBeDefined();
    expect(config.risk).toBeDefined();
    expect(config.execution).toBeDefined();
    expect(config.fees).toBeDefined();
    expect(config.llm).toBeDefined();
    expect(config.corecast).toBeDefined();
    expect(config.features).toBeDefined();
    expect(config.learning).toBeDefined();
    expect(config.health).toBeDefined();
    expect(config.alerts).toBeDefined();
  });

  it('uses anthropic as only provider', () => {
    expect(config.llm.provider).toBe('anthropic');
  });

  it('default model is sonnet, escalation is opus', () => {
    expect(config.llm.default_model).toBe('anthropic/claude-sonnet-4-6');
    expect(config.llm.escalation_model).toBe('anthropic/claude-opus-4-6');
  });

  it('quick_spend is set and positive', () => {
    expect(config.risk.quick_spend_sol).toBeGreaterThan(0);
  });

  it('feature windows are 1s, 5s, 15s, 30s', () => {
    expect(config.features.windows_s).toEqual([1, 5, 15, 30]);
  });

  it('probability weights sum to ~1', () => {
    const w = config.entry.probability_weights;
    const sum = w.flow_momentum + w.breadth_topology + w.creator_wallet_prior +
                w.friction_execution + w.manipulation_distribution + w.multimodal_junk;
    expect(sum).toBeCloseTo(1.0, 2);
  });

  it('max_positions >= 1 for canary', () => {
    expect(config.risk.max_positions).toBeGreaterThanOrEqual(1);
  });

  it('corecast section exists', () => {
    expect(config.corecast).toBeDefined();
    expect(typeof config.corecast.enabled).toBe('boolean');
    expect(config.corecast.endpoint).toBeTruthy();
  });

  it('private_route and bundle_route exist in execution', () => {
    expect(config.execution.private_route).toBeDefined();
    expect(config.execution.bundle_route).toBeDefined();
    expect(typeof config.execution.private_route.enabled).toBe('boolean');
    expect(typeof config.execution.bundle_route.enabled).toBe('boolean');
  });

  it('route_promotion defaults to disabled', () => {
    expect(config.execution.route_promotion.enabled).toBe(false);
  });

  it('fail-closed defaults: auto_pause_on_degraded = true', () => {
    expect(config.health.auto_pause_on_degraded).toBe(true);
  });
});

describe('config/canary.json', () => {
  const config: PumpQuantConfig = JSON.parse(
    fs.readFileSync(path.join(__dirname, '../../config/canary.json'), 'utf-8')
  );

  it('has max_positions = 1', () => {
    expect(config.risk.max_positions).toBe(1);
  });

  it('has corecast section', () => {
    expect(config.corecast).toBeDefined();
  });

  it('has private_route disabled', () => {
    expect(config.execution.private_route.enabled).toBe(false);
  });
});
