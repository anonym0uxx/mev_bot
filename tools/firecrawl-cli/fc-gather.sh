#!/usr/bin/env bash
# fc-gather.sh — Parallel autonomous scrape for mev_bot trading intelligence
# Runs every 15 minutes via Hermes cron. Scrapes 6 sources IN PARALLEL.
# Each source is a background process with a 30s timeout — one slow source
# never blocks the others. Output is appended to data/social-gather.jsonl.

set -euo pipefail

FC_SCRIPT="D:/repos/mev_bot/tools/firecrawl-cli/fc.sh"
DATA_DIR="${PQ_DATA_DIR:-D:/repos/mev_bot/rust/data}"
OUTFILE="$DATA_DIR/social-gather.jsonl"
PIDS=()
RESULTS=()
TIMESTAMP=$(date -u +%Y-%m-%dT%H:%M:%SZ)

mkdir -p "$DATA_DIR"
touch "$OUTFILE"

# ─── Source 1: Pump.fun trending tokens ─────────────────────────────────────
scrape_pumpfun_trending() {
    local result
    result=$("$FC_SCRIPT" scrape "https://pump.fun/board/trending" 2>/dev/null || echo '{}')
    if [ "$result" != '{}' ]; then
        echo "{\"source\":\"pumpfun_trending\",\"ts\":\"$TIMESTAMP\",\"data\":$result}" >> "$OUTFILE"
        echo "pumpfun_trending: OK"
    else
        echo "pumpfun_trending: SKIP"
    fi
}

# ─── Source 2: DexScreener Solana trending ──────────────────────────────────
scrape_dexscreener_trending() {
    local result
    result=$("$FC_SCRIPT" scrape "https://dexscreener.com/solana?order=desc&sort=volume24h" 2>/dev/null || echo '{}')
    if [ "$result" != '{}' ]; then
        echo "{\"source\":\"dexscreener_trending\",\"ts\":\"$TIMESTAMP\",\"data\":$result}" >> "$OUTFILE"
        echo "dexscreener_trending: OK"
    else
        echo "dexscreener_trending: SKIP"
    fi
}

# ─── Source 3: CoinGecko trending Solana ────────────────────────────────────
scrape_coingecko() {
    local result
    result=$("$FC_SCRIPT" scrape "https://www.coingecko.com/en/categories/solana-cells" 2>/dev/null || echo '{}')
    if [ "$result" != '{}' ]; then
        echo "{\"source\":\"coingecko_solana\",\"ts\":\"$TIMESTAMP\",\"data\":$result}" >> "$OUTFILE"
        echo "coingecko_solana: OK"
    else
        echo "coingecko_solana: SKIP"
    fi
}

# ─── Source 4: Crypto news (Solana/memecoin) ────────────────────────────────
scrape_crypto_news() {
    local result
    result=$("$FC_SCRIPT" search "Solana memecoin pump.fun trading" 3 2>/dev/null || echo '{}')
    if [ "$result" != '{}' ]; then
        echo "{\"source\":\"crypto_news\",\"ts\":\"$TIMESTAMP\",\"data\":$result}" >> "$OUTFILE"
        echo "crypto_news: OK"
    else
        echo "crypto_news: SKIP"
    fi
}

# ─── Source 5: Twitter/X search for $SOL memecoins ──────────────────────────
scrape_twitter_sol() {
    local result
    result=$("$FC_SCRIPT" scrape "https://twitter.com/search?q=%24SOL%20memecoin&f=live" 2>/dev/null || echo '{}')
    if [ "$result" != '{}' ]; then
        echo "{\"source\":\"twitter_sol_memecoin\",\"ts\":\"$TIMESTAMP\",\"data\":$result}" >> "$OUTFILE"
        echo "twitter_sol_memecoin: OK"
    else
        echo "twitter_sol_memecoin: SKIP"
    fi
}

# ─── Source 6: Solana ecosystem news ────────────────────────────────────────
scrape_solana_eco() {
    local result
    result=$("$FC_SCRIPT" scrape "https://solana.com/news" 2>/dev/null || echo '{}')
    if [ "$result" != '{}' ]; then
        echo "{\"source\":\"solana_ecosystem\",\"ts\":\"$TIMESTAMP\",\"data\":$result}" >> "$OUTFILE"
        echo "solana_ecosystem: OK"
    else
        echo "solana_ecosystem: SKIP"
    fi
}

# ─── Launch all 6 sources in parallel ───────────────────────────────────────
echo "=== fc-gather START $TIMESTAMP ===" >&2

scrape_pumpfun_trending &
PIDS+=($!)
scrape_dexscreener_trending &
PIDS+=($!)
scrape_coingecko &
PIDS+=($!)
scrape_crypto_news &
PIDS+=($!)
scrape_twitter_sol &
PIDS+=($!)
scrape_solana_eco &
PIDS+=($!)

# ─── Wait for all with a 45s hard ceiling ────────────────────────────────────
TIMEOUT=45
ELAPSED=0
for pid in "${PIDS[@]}"; do
    # Non-blocking wait per process with timeout
    while kill -0 "$pid" 2>/dev/null; do
        sleep 1
        ELAPSED=$((ELAPSED + 1))
        if [ $ELAPSED -ge $TIMEOUT ]; then
            echo "fc-gather: timeout after ${TIMEOUT}s — killing remaining" >&2
            kill "$pid" 2>/dev/null || true
            break 1
        fi
    done
    wait "$pid" 2>/dev/null || true
done

echo "=== fc-gather DONE (elapsed: ${ELAPSED}s) ===" >&2
