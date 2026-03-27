/**
 * @module mev/jito-failure-handler
 * JitoFailureHandler: paper mode = log-only.
 * Circuit breaker: 5 failures in 60 s → pause submissions for 30 s.
 */

import { createLogger } from '../utils/logger';
import { nowMs } from '../utils/time';

const log = createLogger('mev:jito-failure-handler');

const FAILURE_WINDOW_MS = 60_000;
const FAILURE_THRESHOLD = 5;
const PAUSE_DURATION_MS = 30_000;

export class JitoFailureHandler {
  private failures: number[] = [];
  private pauseUntilMs = 0;

  /**
   * Record a Jito submission failure.
   * In paper mode this only logs; in live mode it would trigger retry/fallback logic.
   */
  recordFailure(reason: string): void {
    const now = nowMs();

    // Prune old failures outside window
    this.failures = this.failures.filter(ts => now - ts < FAILURE_WINDOW_MS);
    this.failures.push(now);

    log.warn(`Jito failure recorded: ${reason} (${this.failures.length}/${FAILURE_THRESHOLD} in window)`);

    // Check circuit breaker
    if (this.failures.length >= FAILURE_THRESHOLD) {
      this.pauseUntilMs = now + PAUSE_DURATION_MS;
      this.failures = []; // Reset counter after tripping
      log.error(
        `Jito circuit breaker OPEN — ${FAILURE_THRESHOLD} failures in ${FAILURE_WINDOW_MS / 1000}s. ` +
        `Pausing submissions for ${PAUSE_DURATION_MS / 1000}s.`
      );
    }
  }

  /**
   * Returns true if submissions are currently paused by the circuit breaker.
   */
  isPaused(): boolean {
    const paused = nowMs() < this.pauseUntilMs;
    if (!paused && this.pauseUntilMs > 0) {
      log.info('Jito circuit breaker CLOSED — resuming submissions');
      this.pauseUntilMs = 0;
    }
    return paused;
  }

  /**
   * Record a successful submission (resets failure window).
   */
  recordSuccess(): void {
    this.failures = [];
  }
}
