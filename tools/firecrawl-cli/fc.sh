#!/usr/bin/env bash
# fc.sh — Firecrawl CLI wrapper for mev_bot
# Usage: fc.sh scrape <url> | crawl <url> <limit> | map <url> | search <query>
# Requires: Firecrawl running at http://127.0.0.1:3002

set -euo pipefail

FIRECRAWL_URL="${FIRECRAWL_URL:-http://127.0.0.1:3002}"
API_KEY="${FC_API_KEY:-pq-local-test-key}"
TIMEOUT=30
TMPDIR_FC="${TMPDIR_FC:-/tmp/fc-cache}"
mkdir -p "$TMPDIR_FC"

usage() {
    echo "Usage: fc.sh scrape <url> [format]   — scrape single URL (md|json|html)"
    echo "       fc.sh crawl <url> <limit>     — crawl site up to N pages"
    echo "       fc.sh map <url>               — get site URL map"
    echo "       fc.sh search <query> [limit]  — web search + scrape results"
    exit 1
}

wait_for_firecrawl() {
    local tries=0
    while [ $tries -lt 10 ]; do
        if curl -sf "$FIRECRAWL_URL/v0/health" >/dev/null 2>&1 || \
           curl -sf "$FIRECRAWL_URL/health" >/dev/null 2>&1; then
            return 0
        fi
        sleep 2
        tries=$((tries + 1))
    done
    return 1
}

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

    local response
    response=$(curl -sf -X POST "$FIRECRAWL_URL/v0/scrape" \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer $API_KEY" \
        -d "$payload" \
        --connect-timeout 10 \
        --max-time $TIMEOUT 2>&1) || {
        echo '{"success":false,"error":"scrape_failed","url":"'$url'"}'
        return 1
    }

    echo "$response" > "$cache_file"
    echo "$response"
}

crawl() {
    local url="$1"
    local limit="${2:-10}"
    local payload
    payload=$(cat <<EOF
{"url":"$url","limit":$limit,"scrapeTypes":["markdown"],"maxDepth":2}
EOF
)

    curl -sf -X POST "$FIRECRAWL_URL/v0/crawl" \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer $API_KEY" \
        -d "$payload" \
        --connect-timeout 10 \
        --max-time 60 2>&1 || {
        echo '{"success":false,"error":"crawl_failed","url":"'$url'"}'
        return 1
    }
}

map_site() {
    local url="$1"
    curl -sf -X POST "$FIRECRAWL_URL/v0/map" \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer $API_KEY" \
        -d "{\"url\":\"$url\"}" \
        --connect-timeout 10 \
        --max-time $TIMEOUT 2>&1 || {
        echo '{"success":false,"error":"map_failed","url":"'$url'"}'
        return 1
    }
}

search() {
    local query="$1"
    local limit="${2:-5}"
    local payload
    payload=$(cat <<EOF
{"query":"$query","limit":$limit,"scrapeTypes":["markdown"]}
EOF
)

    curl -sf -X POST "$FIRECRAWL_URL/v0/search" \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer $API_KEY" \
        -d "$payload" \
        --connect-timeout 10 \
        --max-time 60 2>&1 || {
        echo '{"success":false,"error":"search_failed","query":"'$query'"}'
        return 1
    }
}

# Main
if [ $# -lt 1 ]; then
    usage
fi

if ! wait_for_firecrawl; then
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
        # alias for scrape — backward compat
        [ $# -lt 2 ] && usage
        scrape "$2" "${3:-markdown}"
        ;;
    map)
        [ $# -lt 2 ] && api_key="${API_KEY}"
        map_site "$2"
        ;;
    search)
        [ $# -lt 2 ] && usage
        search "$2" "${3:-5}"
        ;;
    health)
        curl -sf "$FIRECRAWL_URL/health" 2>&1 || echo "NOT_HEALTHY"
        ;;
    *)
        usage
        ;;
esac
