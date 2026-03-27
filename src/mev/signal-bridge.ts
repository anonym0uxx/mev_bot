/**
 * QualifiedMintCache — scalper pre-qualification list for MEV engine.
 * When the scalper's quality gate passes for a token, it adds the mint here.
 * MEV engine checks this before entering any position.
 * TTL: 30 seconds per mint (token momentum is short-lived)
 */
export class QualifiedMintCache {
  private cache = new Map<string, number>(); // mint → expiry timestamp
  private ttlMs: number;

  constructor(ttlMs = 30_000) {
    this.ttlMs = ttlMs;
    // Evict expired entries every 10s
    setInterval(() => this.evict(), 10_000).unref();
  }

  add(mint: string): void {
    this.cache.set(mint, Date.now() + this.ttlMs);
  }

  has(mint: string): boolean {
    const expiry = this.cache.get(mint);
    if (!expiry) return false;
    if (Date.now() > expiry) { this.cache.delete(mint); return false; }
    return true;
  }

  private evict(): void {
    const now = Date.now();
    for (const [mint, expiry] of this.cache) {
      if (now > expiry) this.cache.delete(mint);
    }
  }

  get size(): number { return this.cache.size; }
}
