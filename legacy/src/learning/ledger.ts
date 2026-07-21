/**
 * @module learning/ledger
 * Learning ledger: event-driven append on every material event.
 * Full attribution per spec section 22A.
 */

import { v4 as uuidv4 } from 'uuid';
import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';
import { PumpQuantDB } from '../persistence/database';
import { LearningLedgerRecord } from '../types/trade';
import { CandidatePacket, Regime } from '../types/state';
import { FeatureSnapshot } from '../types/features';
import { RouteMode } from '../types/config';

const log = createLogger('learning-ledger');

export class LearningLedger {
  private db: PumpQuantDB;

  constructor(db: PumpQuantDB) {
    this.db = db;
  }

  /**
   * Record an entry decision.
   */
  recordEntry(
    packet: CandidatePacket,
    routeMode: RouteMode,
    configVersion: number,
    fastLaneDecision: string,
    deepLaneDecision: string | null
  ): void {
    const record = this.buildRecord(
      'entry', packet, routeMode, configVersion,
      fastLaneDecision, deepLaneDecision
    );
    this.db.insertLearningRecord(record);
    log.debug(`Learning: entry recorded for ${packet.mint}`);
  }

  /**
   * Record an exit decision.
   */
  recordExit(
    packet: CandidatePacket,
    routeMode: RouteMode,
    configVersion: number,
    realizedPnl: number,
    mfe: number,
    mae: number,
    exitTimingQuality: number,
    fastLaneDecision: string,
    deepLaneDecision: string | null
  ): void {
    const record = this.buildRecord(
      'exit', packet, routeMode, configVersion,
      fastLaneDecision, deepLaneDecision
    );
    record.realized_pnl_sol = realizedPnl;
    record.mfe_sol = mfe;
    record.mae_sol = mae;
    record.exit_timing_quality = exitTimingQuality;
    this.db.insertLearningRecord(record);
    log.debug(`Learning: exit recorded for ${packet.mint}, PnL=${realizedPnl.toFixed(4)}`);
  }

  /**
   * Record a reduce decision.
   */
  recordReduce(
    packet: CandidatePacket,
    routeMode: RouteMode,
    configVersion: number,
    fastLaneDecision: string
  ): void {
    const record = this.buildRecord(
      'reduce', packet, routeMode, configVersion,
      fastLaneDecision, null
    );
    this.db.insertLearningRecord(record);
  }

  /**
   * Record a rejection (for reject-regret analysis).
   */
  recordReject(
    packet: CandidatePacket,
    configVersion: number,
    reason: string
  ): void {
    const record = this.buildRecord(
      'reject', packet, 'local', configVersion,
      reason, null
    );
    this.db.insertLearningRecord(record);
  }

  /**
   * Record a ban.
   */
  recordBan(
    packet: CandidatePacket,
    configVersion: number,
    reason: string
  ): void {
    const record = this.buildRecord(
      'ban', packet, 'local', configVersion,
      reason, null
    );
    this.db.insertLearningRecord(record);
  }

  /**
   * Record a forced exit.
   */
  recordForcedExit(
    packet: CandidatePacket,
    routeMode: RouteMode,
    configVersion: number,
    reason: string,
    realizedPnl: number
  ): void {
    const record = this.buildRecord(
      'forced_exit', packet, routeMode, configVersion,
      reason, null
    );
    record.realized_pnl_sol = realizedPnl;
    this.db.insertLearningRecord(record);
  }

  /**
   * Update reject-regret: after a rejection, update whether the trade would have been profitable.
   */
  updateRejectRegret(recordId: string, regretValue: number): void {
    // Update via raw DB query since we don't have a dedicated method
    const db = this.db.getDb();
    db.prepare('UPDATE learning_ledger SET reject_regret = ? WHERE id = ?').run(regretValue, recordId);
  }

  /**
   * Build a learning record from a candidate packet.
   */
  private buildRecord(
    eventType: LearningLedgerRecord['event_type'],
    packet: CandidatePacket,
    routeMode: RouteMode,
    configVersion: number,
    fastLaneDecision: string,
    deepLaneDecision: string | null
  ): LearningLedgerRecord {
    const features = packet.features;
    const attribution = this.computeAttribution(features);

    return {
      id: uuidv4(),
      timestamp: nowMs(),
      event_type: eventType,
      mint: packet.mint,
      regime: packet.regime,
      config_version: configVersion,
      route_mode: routeMode,
      feature_snapshot: JSON.stringify(features),
      candidate_packet_id: packet.id,
      realized_fill_quality: null,
      realized_pnl_sol: null,
      mfe_sol: null,
      mae_sol: null,
      fast_lane_decision: fastLaneDecision,
      deep_lane_decision: deepLaneDecision,
      lane_agreement: deepLaneDecision === null || fastLaneDecision === deepLaneDecision,
      exit_timing_quality: null,
      reject_regret: null,
      ...attribution,
    };
  }

  /**
   * Compute feature-family attribution decomposition.
   * Measures each family's contribution to the decision.
   */
  private computeAttribution(features: FeatureSnapshot): {
    attribution_flow_momentum: number;
    attribution_breadth_topology: number;
    attribution_creator_wallet_prior: number;
    attribution_multimodal_junk: number;
    attribution_manipulation_penalty: number;
    attribution_friction_route: number;
    attribution_regime_boundary: number;
    route_attribution: number;
    wallet_prior_attribution: number;
    multimodal_filter_attribution: number;
  } {
    // Flow momentum contribution: velocity + imbalance signal
    const flowSignal = Math.min(1, features.flow_momentum.buy_notional_velocity_5s / 0.5) * 0.5 +
      features.flow_momentum.buy_sell_imbalance_5s * 0.5;

    // Breadth contribution
    const breadthSignal = features.breadth_topology.breadth_score;

    // Wallet prior contribution (capped)
    const walletPriorSignal = features.creator_wallet_prior.composite_prior;

    // Multimodal contribution
    const multimodalSignal = features.multimodal_junk.is_stale ? 0 : features.multimodal_junk.junk_score - 0.5;

    // Manipulation penalty (negative contribution)
    const manipulationSignal = -features.manipulation_distribution.manipulation_penalty;

    // Friction/route contribution
    const frictionSignal = features.friction_execution.route_score - 0.5;

    // Regime/boundary (contextual)
    const regimeSignal = 0; // Neutral unless in boundary

    return {
      attribution_flow_momentum: flowSignal,
      attribution_breadth_topology: breadthSignal,
      attribution_creator_wallet_prior: walletPriorSignal,
      attribution_multimodal_junk: multimodalSignal,
      attribution_manipulation_penalty: manipulationSignal,
      attribution_friction_route: frictionSignal,
      attribution_regime_boundary: regimeSignal,
      route_attribution: features.friction_execution.route_ev_adjustment,
      wallet_prior_attribution: walletPriorSignal,
      multimodal_filter_attribution: multimodalSignal,
    };
  }
}
