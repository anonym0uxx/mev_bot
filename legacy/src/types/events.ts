/**
 * @module types/events
 * Raw event types from PumpPortal and Bitquery feeds
 */

/** Raw event from PumpPortal WebSocket */
export interface RawEvent {
  id?: string;
  type: EventType;
  data: Record<string, unknown>;
  timestamp: number;
  received_at: number;
}

export type EventType =
  | 'new_token'
  | 'token_trade'
  | 'account_trade'
  | 'migration'
  | 'system';

/** PumpPortal new token creation event */
export interface NewTokenEvent {
  signature: string;
  mint: string;
  traderPublicKey: string;
  txType: 'create';
  initialBuy?: number;
  bondingCurveKey: string;
  vTokensInBondingCurve: number;
  vSolInBondingCurve: number;
  marketCapSol: number;
  name: string;
  symbol: string;
  uri: string;
  pool?: string;
  timestamp: number;
}

/** PumpPortal token trade event */
export interface TokenTradeEvent {
  signature: string;
  mint: string;
  traderPublicKey: string;
  txType: 'buy' | 'sell';
  tokenAmount: number;
  solAmount: number;
  newTokenBalance: number;
  bondingCurveKey: string;
  vTokensInBondingCurve: number;
  vSolInBondingCurve: number;
  marketCapSol: number;
  timestamp: number;
}

/** PumpPortal account trade event (for wallet tracking) */
export interface AccountTradeEvent {
  signature: string;
  mint: string;
  traderPublicKey: string;
  txType: 'buy' | 'sell';
  tokenAmount: number;
  solAmount: number;
  newTokenBalance: number;
  bondingCurveKey: string;
  vTokensInBondingCurve: number;
  vSolInBondingCurve: number;
  marketCapSol: number;
  timestamp: number;
}

/** PumpPortal migration event */
export interface MigrationEvent {
  signature: string;
  mint: string;
  pool?: string;
  timestamp: number;
}

/** Bitquery enrichment: holder data */
export interface HolderData {
  address: string;
  balance: number;
  percentage: number;
  isCreator: boolean;
  firstBuyTimestamp: number;
}

/** Bitquery enrichment: creator data */
export interface CreatorData {
  address: string;
  totalCreated: number;
  totalRugged: number;
  avgHoldTime: number;
  recentActivity: CreatorTokenHistory[];
}

export interface CreatorTokenHistory {
  mint: string;
  createdAt: number;
  soldAll: boolean;
  holdDurationS: number;
  maxMarketCapSol: number;
}

/** Bitquery enrichment: OHLCV candle */
export interface OHLCVCandle {
  open: number;
  high: number;
  low: number;
  close: number;
  volume: number;
  timestamp: number;
}

/** Alert event for operator notification */
export interface AlertEvent {
  id: string;
  type: string;
  severity: 'immediate_alert' | 'scheduled_summary' | 'log_only';
  message: string;
  data: Record<string, unknown>;
  timestamp: number;
  delivered: boolean;
}

/** Health event for system monitoring */
export interface HealthEvent {
  id: string;
  subsystem: HealthSubsystem;
  status: HealthStatus;
  message: string;
  timestamp: number;
}

export type HealthSubsystem =
  | 'market_feed'
  | 'friction_estimate'
  | 'probability_layer'
  | 'datastore'
  | 'execution_adapter'
  | 'config_integrity';

export type HealthStatus = 'healthy' | 'degraded' | 'failed';
