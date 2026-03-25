/**
 * @module daemon/api
 * HTTP API for daemon, used by OpenClaw plugin for IPC.
 * Exposes endpoints for all 14 plugin tools.
 */

import express, { Request, Response, Router } from 'express';
import { createLogger } from '../utils/logger';
import { PumpQuantConfig } from '../types/config';

const log = createLogger('api');

export interface DaemonContext {
  getTopCandidates: (limit?: number) => any[];
  inspectCandidate: (mint: string) => any | null;
  buyToken: (mint: string, sizeSol: number, slippageBps: number, priorityFeeSol: number, routeMode: string) => Promise<any>;
  sellToken: (mint: string, amountPct: number, slippageBps: number, priorityFeeSol: number, routeMode: string, reason: string) => Promise<any>;
  getPositions: () => any[];
  pauseTrading: (reason: string) => void;
  resumeTrading: () => void;
  getBotHealth: () => any;
  getRiskSettings: () => any;
  updateRiskSettings: (settings: Record<string, unknown>) => any;
  getStrategyProfile: () => string;
  setStrategyProfile: (name: string) => void;
  getRuntimeConfig: () => PumpQuantConfig;
  updateRuntimeConfig: (patch: Record<string, unknown>) => any;
}

export function createApiRouter(ctx: DaemonContext): Router {
  const router = Router();

  // 1. get_top_candidates
  router.get('/candidates/top', (req: Request, res: Response) => {
    try {
      const limit = parseInt(String(req.query.limit || '10')) || 10;
      const candidates = ctx.getTopCandidates(limit);
      res.json({ success: true, data: candidates });
    } catch (err) {
      res.status(500).json({ success: false, error: (err as Error).message });
    }
  });

  // 2. inspect_candidate
  router.get('/candidates/:mint', (req: Request, res: Response) => {
    try {
      const candidate = ctx.inspectCandidate(String(req.params.mint));
      if (!candidate) {
        res.status(404).json({ success: false, error: 'Candidate not found' });
        return;
      }
      res.json({ success: true, data: candidate });
    } catch (err) {
      res.status(500).json({ success: false, error: (err as Error).message });
    }
  });

  // 3. buy_token
  router.post('/trade/buy', async (req: Request, res: Response) => {
    try {
      const { mint, size_sol, slippage_bps, priority_fee_sol, route_mode } = req.body;
      const result = await ctx.buyToken(mint, size_sol, slippage_bps, priority_fee_sol, route_mode);
      res.json({ success: true, data: result });
    } catch (err) {
      res.status(500).json({ success: false, error: (err as Error).message });
    }
  });

  // 4. sell_token
  router.post('/trade/sell', async (req: Request, res: Response) => {
    try {
      const { mint, amount_pct, slippage_bps, priority_fee_sol, route_mode, reason } = req.body;
      const result = await ctx.sellToken(mint, amount_pct, slippage_bps, priority_fee_sol, route_mode, reason);
      res.json({ success: true, data: result });
    } catch (err) {
      res.status(500).json({ success: false, error: (err as Error).message });
    }
  });

  // 5. get_positions
  router.get('/positions', (_req: Request, res: Response) => {
    try {
      const positions = ctx.getPositions();
      res.json({ success: true, data: positions });
    } catch (err) {
      res.status(500).json({ success: false, error: (err as Error).message });
    }
  });

  // 6. pause_trading
  router.post('/control/pause', (req: Request, res: Response) => {
    try {
      const { reason } = req.body;
      ctx.pauseTrading(reason || 'Operator requested');
      res.json({ success: true, message: 'Trading paused' });
    } catch (err) {
      res.status(500).json({ success: false, error: (err as Error).message });
    }
  });

  // 7. resume_trading
  router.post('/control/resume', (_req: Request, res: Response) => {
    try {
      ctx.resumeTrading();
      res.json({ success: true, message: 'Trading resumed' });
    } catch (err) {
      res.status(500).json({ success: false, error: (err as Error).message });
    }
  });

  // 8. get_bot_health
  router.get('/health', (_req: Request, res: Response) => {
    try {
      const health = ctx.getBotHealth();
      res.json({ success: true, data: health });
    } catch (err) {
      res.status(500).json({ success: false, error: (err as Error).message });
    }
  });

  // 9. get_risk_settings
  router.get('/risk', (_req: Request, res: Response) => {
    try {
      const settings = ctx.getRiskSettings();
      res.json({ success: true, data: settings });
    } catch (err) {
      res.status(500).json({ success: false, error: (err as Error).message });
    }
  });

  // 10. update_risk_settings
  router.patch('/risk', (req: Request, res: Response) => {
    try {
      const result = ctx.updateRiskSettings(req.body);
      res.json({ success: true, data: result });
    } catch (err) {
      res.status(500).json({ success: false, error: (err as Error).message });
    }
  });

  // 11. get_strategy_profile
  router.get('/profile', (_req: Request, res: Response) => {
    try {
      const profile = ctx.getStrategyProfile();
      res.json({ success: true, data: { profile } });
    } catch (err) {
      res.status(500).json({ success: false, error: (err as Error).message });
    }
  });

  // 12. set_strategy_profile
  router.put('/profile', (req: Request, res: Response) => {
    try {
      const { profile_name } = req.body;
      ctx.setStrategyProfile(profile_name);
      res.json({ success: true, message: `Profile set to ${profile_name}` });
    } catch (err) {
      res.status(500).json({ success: false, error: (err as Error).message });
    }
  });

  // 13. get_runtime_config
  router.get('/config', (_req: Request, res: Response) => {
    try {
      const config = ctx.getRuntimeConfig();
      res.json({ success: true, data: config });
    } catch (err) {
      res.status(500).json({ success: false, error: (err as Error).message });
    }
  });

  // 14. update_runtime_config
  router.patch('/config', (req: Request, res: Response) => {
    try {
      const result = ctx.updateRuntimeConfig(req.body);
      res.json({ success: true, data: result });
    } catch (err) {
      res.status(500).json({ success: false, error: (err as Error).message });
    }
  });

  return router;
}

/**
 * Create and start the daemon HTTP server.
 */
export function startApiServer(ctx: DaemonContext, port: number, host: string): void {
  const app = express();
  app.use(express.json());
  app.use('/api', createApiRouter(ctx));

  app.listen(port, host, () => {
    log.info(`Daemon API listening on ${host}:${port}`);
  });
}
