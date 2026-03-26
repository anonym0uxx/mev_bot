/**
 * @module threshold/manager
 * Adaptive threshold manager — eliminates "threshold above model ceiling" failures.
 *
 * Instead of absolute thresholds (min_entry_edge: 0.003), maintains a rolling
 * distribution of model outputs and gates on percentiles. The p50 of any
 * distribution is always within the distribution — ceiling violations are
 * physically impossible.
 *
 * Persists state to data/threshold_state.json across daemon restarts.
 */

import * as fs from 'fs';
import * as path from 'path';
import { createLogger } from '../utils/logger';

const log = createLogger('threshold-manager');

const STATE_PATH = path.resolve(__dirname, '../../data/threshold_state.json');
const WINDOW_SIZE = 500;
const COLD_START_FLOOR = 0.0001;
const MIN_WINDOW_FOR_PERCENTILE = 50;

interface ThresholdState {
  edgeWindow: number[];
  evWindow: number[];
  pContWindow: number[];
  lastUpdated: number;
  totalRecorded: number;
}

let state: ThresholdState = {
  edgeWindow: [],
  evWindow: [],
  pContWindow: [],
  lastUpdated: 0,
  totalRecorded: 0,
};

/** Load persisted state from disk */
export function loadThresholdState(): void {
  try {
    if (fs.existsSync(STATE_PATH)) {
      const raw = JSON.parse(fs.readFileSync(STATE_PATH, 'utf8'));
      state = { ...state, ...raw };
      // Enforce window size
      state.edgeWindow = state.edgeWindow.slice(-WINDOW_SIZE);
      state.evWindow = state.evWindow.slice(-WINDOW_SIZE);
      state.pContWindow = state.pContWindow.slice(-WINDOW_SIZE);
      log.info(`Threshold state loaded: ${state.edgeWindow.length} edge samples, totalRecorded=${state.totalRecorded}`);
    }
  } catch (err) {
    log.warn(`Failed to load threshold state: ${(err as Error).message} — starting fresh`);
  }
}

/** Persist state to disk */
function saveState(): void {
  try {
    fs.writeFileSync(STATE_PATH, JSON.stringify(state, null, 2));
  } catch (err) {
    log.warn(`Failed to save threshold state: ${(err as Error).message}`);
  }
}

/** Record a signal evaluation (call before gate check, win or lose) */
export function recordEval(edge: number, ev: number, pCont: number): void {
  state.edgeWindow.push(edge);
  state.evWindow.push(ev);
  state.pContWindow.push(pCont);

  // Maintain rolling window
  if (state.edgeWindow.length > WINDOW_SIZE) state.edgeWindow.shift();
  if (state.evWindow.length > WINDOW_SIZE) state.evWindow.shift();
  if (state.pContWindow.length > WINDOW_SIZE) state.pContWindow.shift();

  state.totalRecorded++;
  state.lastUpdated = Date.now();

  // Persist every 50 evals to avoid excessive I/O
  if (state.totalRecorded % 50 === 0) saveState();
}

/** Compute percentile of a sorted array */
function percentile(sorted: number[], p: number): number {
  if (sorted.length === 0) return 0;
  const idx = Math.floor(sorted.length * p);
  return sorted[Math.min(idx, sorted.length - 1)];
}

/** Get adaptive min_entry_edge threshold */
export function getMinEdge(targetPercentile = 0.50): number {
  if (state.edgeWindow.length < MIN_WINDOW_FOR_PERCENTILE) {
    return COLD_START_FLOOR; // cold start
  }
  const sorted = [...state.edgeWindow].sort((a, b) => a - b);
  const p = percentile(sorted, targetPercentile);
  return Math.max(COLD_START_FLOOR, p);
}

/** Get edge ceiling (max observed) */
export function getEdgeCeiling(): number {
  if (state.edgeWindow.length === 0) return 0;
  return Math.max(...state.edgeWindow);
}

/** Get full stats snapshot for monitoring */
export function getStats(): {
  edgeSamples: number;
  edgeP50: number;
  edgeP75: number;
  edgeP95: number;
  edgeMax: number;
  pContP50: number;
  pContMax: number;
  evPositiveRate: number;
  totalRecorded: number;
} {
  const edgeSorted = [...state.edgeWindow].sort((a, b) => a - b);
  const evPos = state.evWindow.filter(v => v > 0).length;
  const pContSorted = [...state.pContWindow].sort((a, b) => a - b);

  return {
    edgeSamples: state.edgeWindow.length,
    edgeP50: percentile(edgeSorted, 0.50),
    edgeP75: percentile(edgeSorted, 0.75),
    edgeP95: percentile(edgeSorted, 0.95),
    edgeMax: edgeSorted[edgeSorted.length - 1] ?? 0,
    pContP50: percentile(pContSorted, 0.50),
    pContMax: pContSorted[pContSorted.length - 1] ?? 0,
    evPositiveRate: state.evWindow.length > 0 ? evPos / state.evWindow.length : 0,
    totalRecorded: state.totalRecorded,
  };
}

/** Detect ceiling violation: absolute threshold is above observed max */
export function detectCeilingViolation(absoluteThreshold: number): boolean {
  const ceiling = getEdgeCeiling();
  return ceiling > 0 && absoluteThreshold > ceiling;
}
