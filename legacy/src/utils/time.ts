/**
 * @module utils/time
 * Time utilities for consistent timestamp handling
 */

/** Get current time in milliseconds */
export function nowMs(): number {
  return Date.now();
}

/** Get current time in seconds */
export function nowS(): number {
  return Date.now() / 1000;
}

/** Convert seconds to milliseconds */
export function sToMs(s: number): number {
  return s * 1000;
}

/** Convert milliseconds to seconds */
export function msToS(ms: number): number {
  return ms / 1000;
}

/** Check if a timestamp (ms) is stale by threshold (seconds) */
export function isStale(timestampMs: number, thresholdS: number): boolean {
  return (nowMs() - timestampMs) > sToMs(thresholdS);
}

/** Get age in seconds from a millisecond timestamp */
export function ageS(timestampMs: number): number {
  return msToS(nowMs() - timestampMs);
}

/** Sleep for given milliseconds */
export function sleep(ms: number): Promise<void> {
  return new Promise(resolve => setTimeout(resolve, ms));
}

/** Format duration in seconds to human readable */
export function formatDuration(seconds: number): string {
  if (seconds < 60) return `${seconds.toFixed(1)}s`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ${Math.floor(seconds % 60)}s`;
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  return `${h}h ${m}m`;
}
