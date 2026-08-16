#!/usr/bin/env bash
# fc.sh — Firecrawl CLI wrapper for mev_bot
# Usage: fc.sh scrape <url> [format]   — scrape single URL (md|json|html)
#        fc.sh crawl <url> <limit>     — crawl site up to N pages
#        fc.sh map <url>               — get site URL map (v1 API)
#        fc.sh search <query> [limit]  — web search + scrape results
#        fc.sh health                  — check Firecrawl health
#
# FIX (2026-08-16): Updated to v1 API endpoints, added auto-start logic,
#   robust health check via v1/scrape probe, and retry-with-backoff.
#   Previous version used deprecated /v0/ endpoints and had no auto-start,
#   causing chronic "firecrawl_unavailable" errors.

set -euo pipefail

FIRECRAWL_URL="${FIRECRAWL_URL:-http://127.0.0.1:3102}"
FC_API_KEY="${FC_API_KEY:-pq-local-test-key}"
TIMEOUT=60
TMPDIR_FC="${TMPDIR_FC:-/tmp/fc-cache}"
FC_STACK_DIR="${FC_STACK_DIR:-D:/repos/firecrawl}"
mkdir -p "$TMPDIR_FC"

usage() {
    echo "Usage: fc.sh scrape <url> [format]   — scrape single URL (md|json|html)"
    echo "       fc.sh crawl <url> <limit>     — crawl site up to N pages"
    echo "       fc.sh map <url>               — get site URL map"
    echo "       fc.sh search <query> [limit]  — web search + scrape results"
    echo "       fc.sh health                  — check if Firecrawl is healthy"
    exit 1
}

# ─── Auto-start: bring up the Firecrawl stack if it's not running ──────────
ensure_firecrawl_running() {
    # Quick probe: is the API port even listening?
    if curl -sf -X POST "$FIRECRAWL_URL/v1/scrape" \
       -H "Content-Type: application/json" \
       -d '{"url":"https://example.com","formats":["markdown"]}' \
       --connect-timeout 5 --max-time 15 >/dev/null 2>&1; then
        return 0
    fi

    echo "[fc.sh] Firecrawl not responding — starting stack..." >&2
    pushd "$FC_STACK_DIR" >/dev/null 2>&1
    docker compose up -d 2>&1 | tail -3 >&2 || true
    popd >/dev/null 2>&1

    # Wait for API to become healthy (up to 120s)
    local tries=0
    while [ $tries -lt 24 ]; do
        if curl -sf -X POST "$FIRECRAWL_URL/v1/scrape" \
           -H "Content-Type: application/json" \
           -d '{"url":"https://example.com","formats":["markdown"]}' \
           --connect-timeout 5 --max-time 15 >/dev/null 2>&1; then
            echo "[fc.sh] Firecrawl is healthy (waited $((tries * 5))s)" >&2
            return 0
        fi
        sleep 5
        tries=$((tries + 1))
    done

    echo "[fc.sh] Firecrawl failed to start within 120s" >&2
    return 1
}

# ─── Health check ──────────────────────────────────────────────────────────
health() {
    if curl -sf -X POST "$FIRECRAWL_URL/v1/scrape" \
       -H "Content-Type: application/json" \
       -d '{"url":"https://example.com","formats":["markdown"]}' \
       --connect-timeout 5 --max-time 15 >/dev/null 2>&1; then
        echo "HEALTHY"
    else
        echo "NOT_HEALTHY"
    fi
}

# ─── Scrape a single URL ──────────────────────────────────────────────────
scrape() {
    local url="$1"
    local format="${2:-markdown}"
    local cache_key
    cache_key=$(echo -n "$url" | md5sum | cut -c1-16)
    local cache_file="$TMPDIR_FC/scrape_${cache_key}.json"

    # Cache hit (valid for 5 minutes)
    if [ -f "$cache_file" ]; then
        local age
        age=$(( $(date +%s) - $(stat -c %Y "$cache_file" 2>/dev/null || stat -f %m "$cache_file" 2>/dev/null || echo 0) ))
        if [ $age -lt 300 ]; then
            cat "$cache_file"
            return 0
        fi
    fi

    local payload
    payload=$(cat <<EOF
{"url":"$url","formats":["$format"],"onlyIncludesLevel":0,"maxAge":300000}
EOF
)

    # Retry with backoff (3 attempts)
    local attempt=0
    local response
    while [ $attempt -lt 3 ]; do
        response=$(curl -sf -X POST "$FIRECRAWL_URL/v1/scrape" \
            -H "Content-Type: application/json" \
            -H "Authorization: Bearer $FC_API_KEY" \
            -d "$payload" \
            --connect-timeout 10 \
            --max-time $TIMEOUT 2>&1) && break
        attempt=$((attempt + 1))
        [ $attempt -lt 3 ] && sleep $((attempt * 3))
    done

    if [ -z "$response" ] || ! echo "$response" | grep -q '"success"'; then
        echo "{\"success\":false,\"error\":\"scrape_failed\",\"url\":\"$url\"}"
        return 1
    fi

    echo "$response" > "$cache_file"
    echo "$response"
}

# ─── Crawl a site ─────────────────────────────────────────────────────────
crawl() {
    local url="$1"
    local limit="${2:-10}"
    local payload
    payload=$(cat <<EOF
{"url":"$url","limit":$limit,"scrapeTypes":["markdown"],"maxDepth":2}
EOF
)

    curl -sf -X POST "$FIRECRAWL_URL/v1/crawl" \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer $FC_API_KEY" \
        -d "$payload" \
        --connect-timeout 10 \
        --max-time 120 2>&1 || {
        echo "{\"success\":false,\"error\":\"crawl_failed\",\"url\":\"$url\"}"
        return 1
    }
}

# ─── Map a site (v1 API — v0/map returns 404) ─────────────────────────────
map_site() {
    local url="$1"
    curl -sf -X POST "$FIRECRAWL_URL/v1/map" \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer $FC_API_KEY" \
        -d "{\"url\":\"$url\",\"limit\":100}" \
        --connect-timeout 10 \
        --max-time $TIMEOUT 2>&1 || {
        echo "{\"success\":false,\"error\":\"map_failed\",\"url\":\"$url\"}"
        return 1
    }
}

# ─── Web search + scrape ──────────────────────────────────────────────────
search() {
    local query="$1"
    local limit="${2:-5}"
    local payload
    payload=$(cat <<EOF
{"query":"$query","limit":$limit}
EOF
)

    curl -sf -X POST "$FIRECRAWL_URL/v1/search" \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer $FC_API_KEY" \
        -d "$payload" \
        --connect-timeout 10 \
        --max-time 90 2>&1 || {
        echo "{\"success\":false,\"error\":\"search_failed\",\"query\":\"$query\"}"
        return 1
    }
}

# ─── Main ──────────────────────────────────────────────────────────────────
if [ $# -lt 1 ]; then
    usage
fi

case "$1" in
    health)
        health
        exit 0
        ;;
esac

# Auto-start before any operation
if ! ensure_firecrawl_running; then
    echo '{"success":false,"error":"firecrawl_unavailable"}'
    exit 1
fi

case "$1" in
    scrape)
        [ $# -lt 2 ] && usage
        scrape "$2" "${3:-markdown}"
        ;;
    crawl)
        [ $# -lt 2 ] && usage
        crawl "$2" "${3:-10}"
        ;;
    manual)
        [ $# -lt 2 ] && usage
        scrape "$2" "${3:-markdown}"
        ;;
    map)
        [ $# -lt 2 ] && usage
        map_site "$2"
        ;;
    search)
        [ $# -lt 2 ] && usage
        search "$2" "${3:-5}"
        ;;
    *)
        usage
        ;;
esac
