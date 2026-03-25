/**
 * @module plugin/index
 * OpenClaw plugin: @alon/pump-quant
 * All 14 tools as specified in section 15.
 * Communicates with daemon via HTTP API.
 */

import fetch from 'node-fetch';
import { createLogger } from '../utils/logger';

const log = createLogger('plugin');

const DAEMON_URL = `http://${process.env.DAEMON_HOST || '127.0.0.1'}:${process.env.DAEMON_PORT || '9420'}`;

/** Make an API call to the daemon */
async function daemonCall(method: string, path: string, body?: Record<string, unknown>): Promise<any> {
  const url = `${DAEMON_URL}/api${path}`;
  const options: any = {
    method,
    headers: { 'Content-Type': 'application/json' },
  };
  if (body) {
    options.body = JSON.stringify(body);
  }

  const response = await fetch(url, options);
  const result = await response.json() as any;

  if (!result.success) {
    throw new Error(result.error || 'API call failed');
  }

  return result.data;
}

// ====== TOOL DEFINITIONS ======

/**
 * 1. get_top_candidates()
 * Returns the top-ranked candidate tokens currently being tracked.
 */
export async function get_top_candidates(limit: number = 10): Promise<any> {
  return daemonCall('GET', `/candidates/top?limit=${limit}`);
}

/**
 * 2. inspect_candidate(mint)
 * Returns detailed inspection of a specific token candidate.
 */
export async function inspect_candidate(mint: string): Promise<any> {
  return daemonCall('GET', `/candidates/${mint}`);
}

/**
 * 3. buy_token(mint, size_sol, slippage_bps, priority_fee_sol, route_mode)
 * Execute a buy trade on a token.
 */
export async function buy_token(
  mint: string,
  size_sol: number,
  slippage_bps: number = 500,
  priority_fee_sol: number = 0.0005,
  route_mode: string = 'local'
): Promise<any> {
  return daemonCall('POST', '/trade/buy', {
    mint, size_sol, slippage_bps, priority_fee_sol, route_mode,
  });
}

/**
 * 4. sell_token(mint, amount_pct, slippage_bps, priority_fee_sol, route_mode, reason)
 * Execute a sell trade on a token.
 */
export async function sell_token(
  mint: string,
  amount_pct: number = 100,
  slippage_bps: number = 500,
  priority_fee_sol: number = 0.0005,
  route_mode: string = 'local',
  reason: string = 'Manual sell'
): Promise<any> {
  return daemonCall('POST', '/trade/sell', {
    mint, amount_pct, slippage_bps, priority_fee_sol, route_mode, reason,
  });
}

/**
 * 5. get_positions()
 * Returns all open positions.
 */
export async function get_positions(): Promise<any> {
  return daemonCall('GET', '/positions');
}

/**
 * 6. pause_trading(reason)
 * Pause autonomous trading.
 */
export async function pause_trading(reason: string = 'Operator requested'): Promise<any> {
  return daemonCall('POST', '/control/pause', { reason });
}

/**
 * 7. resume_trading()
 * Resume autonomous trading.
 */
export async function resume_trading(): Promise<any> {
  return daemonCall('POST', '/control/resume');
}

/**
 * 8. get_bot_health()
 * Returns system health status.
 */
export async function get_bot_health(): Promise<any> {
  return daemonCall('GET', '/health');
}

/**
 * 9. get_risk_settings()
 * Returns current risk settings.
 */
export async function get_risk_settings(): Promise<any> {
  return daemonCall('GET', '/risk');
}

/**
 * 10. update_risk_settings(settings)
 * Update risk settings (quick_spend, max_alloc, risk_per_trade, slippage_cap, etc.)
 */
export async function update_risk_settings(settings: Record<string, unknown>): Promise<any> {
  return daemonCall('PATCH', '/risk', settings);
}

/**
 * 11. get_strategy_profile()
 * Returns current strategy profile name.
 */
export async function get_strategy_profile(): Promise<any> {
  return daemonCall('GET', '/profile');
}

/**
 * 12. set_strategy_profile(profile_name)
 * Switch to a different strategy profile.
 */
export async function set_strategy_profile(profile_name: string): Promise<any> {
  return daemonCall('PUT', '/profile', { profile_name });
}

/**
 * 13. get_runtime_config()
 * Returns the current runtime configuration.
 */
export async function get_runtime_config(): Promise<any> {
  return daemonCall('GET', '/config');
}

/**
 * 14. update_runtime_config(patch)
 * Apply a partial config update. Validated, persisted, versioned, auditable.
 */
export async function update_runtime_config(patch: Record<string, unknown>): Promise<any> {
  return daemonCall('PATCH', '/config', patch);
}

// ====== OPENCLAW PLUGIN FORMAT ======

/**
 * OpenClaw plugin manifest.
 * Defines tools available to the AI assistant.
 */
export const pluginManifest = {
  name: '@alon/pump-quant',
  version: '0.1.0',
  description: 'Pump.fun Principal Crypto Quant Bot — regime-aware autonomous trading system',
  tools: [
    {
      name: 'get_top_candidates',
      description: 'Get top-ranked candidate tokens currently being tracked by the bot. Returns candidates in WATCH or ENTER_READY state, ranked by entry edge.',
      parameters: {
        type: 'object',
        properties: {
          limit: { type: 'number', description: 'Max candidates to return', default: 10 },
        },
      },
      handler: get_top_candidates,
    },
    {
      name: 'inspect_candidate',
      description: 'Get detailed inspection of a specific token candidate including features, probabilities, EV calculations, and regime.',
      parameters: {
        type: 'object',
        properties: {
          mint: { type: 'string', description: 'Token mint address' },
        },
        required: ['mint'],
      },
      handler: inspect_candidate,
    },
    {
      name: 'buy_token',
      description: 'Execute a buy trade on a token. The token must be in ENTER_READY or WATCH state.',
      parameters: {
        type: 'object',
        properties: {
          mint: { type: 'string', description: 'Token mint address' },
          size_sol: { type: 'number', description: 'Amount in SOL to spend' },
          slippage_bps: { type: 'number', description: 'Max slippage in basis points', default: 500 },
          priority_fee_sol: { type: 'number', description: 'Priority fee in SOL', default: 0.0005 },
          route_mode: { type: 'string', enum: ['local', 'lightning', 'jito'], default: 'local' },
        },
        required: ['mint', 'size_sol'],
      },
      handler: buy_token,
    },
    {
      name: 'sell_token',
      description: 'Execute a sell trade on a token position.',
      parameters: {
        type: 'object',
        properties: {
          mint: { type: 'string', description: 'Token mint address' },
          amount_pct: { type: 'number', description: 'Percentage of position to sell (1-100)', default: 100 },
          slippage_bps: { type: 'number', description: 'Max slippage in basis points', default: 500 },
          priority_fee_sol: { type: 'number', description: 'Priority fee in SOL', default: 0.0005 },
          route_mode: { type: 'string', enum: ['local', 'lightning', 'jito'], default: 'local' },
          reason: { type: 'string', description: 'Reason for selling' },
        },
        required: ['mint'],
      },
      handler: sell_token,
    },
    {
      name: 'get_positions',
      description: 'Get all open trading positions with current PnL and status.',
      parameters: { type: 'object', properties: {} },
      handler: get_positions,
    },
    {
      name: 'pause_trading',
      description: 'Pause autonomous trading. The bot will not enter new positions until resumed.',
      parameters: {
        type: 'object',
        properties: {
          reason: { type: 'string', description: 'Reason for pausing' },
        },
      },
      handler: pause_trading,
    },
    {
      name: 'resume_trading',
      description: 'Resume autonomous trading after a pause.',
      parameters: { type: 'object', properties: {} },
      handler: resume_trading,
    },
    {
      name: 'get_bot_health',
      description: 'Get system health status including all subsystem checks.',
      parameters: { type: 'object', properties: {} },
      handler: get_bot_health,
    },
    {
      name: 'get_risk_settings',
      description: 'Get current risk settings (bankroll, quick_spend, risk_per_trade, max_alloc, etc.)',
      parameters: { type: 'object', properties: {} },
      handler: get_risk_settings,
    },
    {
      name: 'update_risk_settings',
      description: 'Update risk settings. Supports: quick_spend_sol, risk_per_trade_pct, max_alloc_pct, max_positions, slippage_cap_sol, max_daily_loss_sol.',
      parameters: {
        type: 'object',
        properties: {
          settings: { type: 'object', description: 'Risk setting key-value pairs to update' },
        },
        required: ['settings'],
      },
      handler: update_risk_settings,
    },
    {
      name: 'get_strategy_profile',
      description: 'Get the name of the current strategy profile (default, canary, etc.)',
      parameters: { type: 'object', properties: {} },
      handler: get_strategy_profile,
    },
    {
      name: 'set_strategy_profile',
      description: 'Switch to a different strategy profile. Available: default, canary.',
      parameters: {
        type: 'object',
        properties: {
          profile_name: { type: 'string', description: 'Profile name to activate' },
        },
        required: ['profile_name'],
      },
      handler: set_strategy_profile,
    },
    {
      name: 'get_runtime_config',
      description: 'Get the full current runtime configuration.',
      parameters: { type: 'object', properties: {} },
      handler: get_runtime_config,
    },
    {
      name: 'update_runtime_config',
      description: 'Apply a partial runtime configuration update. Changes are validated, persisted, versioned, and auditable.',
      parameters: {
        type: 'object',
        properties: {
          patch: { type: 'object', description: 'Partial config to merge' },
        },
        required: ['patch'],
      },
      handler: update_runtime_config,
    },
  ],
};

// Export for OpenClaw plugin registration
export default pluginManifest;
