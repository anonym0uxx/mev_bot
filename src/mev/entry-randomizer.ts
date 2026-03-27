import { MevConfig } from '../types/config';

export interface RandomizedEntry {
  delayMs: number;
  sizeSol: number;
}

export class EntryRandomizer {
  private cfg: MevConfig;

  constructor(cfg: MevConfig) {
    this.cfg = cfg;
  }

  /**
   * Returns a randomized delay and position size to avoid pattern fingerprinting.
   * Delay: uniform random between jitter_ms_min and jitter_ms_max
   * Size: uniform random between entry_size_sol * (1 - size_variance_pct) and entry_size_sol * (1 + size_variance_pct)
   */
  randomize(): RandomizedEntry {
    const minDelay = this.cfg.jitter_ms_min ?? 50;
    const maxDelay = this.cfg.jitter_ms_max ?? 200;
    const delayMs = Math.floor(Math.random() * (maxDelay - minDelay + 1)) + minDelay;

    const variance = this.cfg.size_variance_pct ?? 0.20;
    const base = this.cfg.entry_size_sol;
    const low = base * (1 - variance);
    const high = base * (1 + variance);
    const sizeSol = parseFloat((Math.random() * (high - low) + low).toFixed(4));

    return { delayMs, sizeSol };
  }
}
