# Helius WebSocket 401 Unauthorized — Frontier Model Escalation Brief

**Date:** 2026-08-02
**Context:** pump-quant paper trading session, Helius LaserStream WebSocket connection

---

## Problem

The Helius API key returns **HTTP 401 Unauthorized** when attempting a WebSocket
connection to `wss://mainnet.helius-rpc.com/?api-key=KEY`. The key was obtained
fresh from the Helius dashboard and confirmed by the operator.

## Key Details (snippets only — no complete keys)

- **API key format:** UUID v4, 36 chars (8-4-4-4-12)
- **Key snippet:** `2c32e05f...7fe1` (first 8 / last 4)
- **Key SHA-256[:8]:** `b84516c7`
- **Key serves double duty:** Helius WebSocket API key AND LaserStream gRPC token
  (same value for both — confirmed by operator)

## Endpoints Tested

### ✅ Works (key accepted)

| Endpoint | Protocol | Method | HTTP Status | Evidence |
|---|---|---|---|---|
| `wss://marielle-qe2lvr-fast-mainnet.helius-rpc.com/?api-key=KEY` | WebSocket | WS upgrade | **101** (connected) | tungstenite + Python websocket-client both succeed; slotSubscribe accepted (subscription ID returned) |
| `https://devnet.helius-rpc.com/?api-key=KEY` | HTTP RPC | POST | **200** | `{"id":1,"jsonrpc":"2.0","result":"ok"}` — getHealth returns ok |
| `https://mainnet.helius-rpc.com/?api-key=KEY` | HTTP RPC | GET | **404** + valid JSON-RPC body | `{"jsonrpc":"2.0","error":{"code":-32603,"message":"Method not found"}}` — **key was accepted**, 404 is just because GET is not a valid RPC method |

### ❌ Fails (key rejected)

| Endpoint | Protocol | Method | HTTP Status | Error Body |
|---|---|---|---|---|
| `wss://mainnet.helius-rpc.com/?api-key=KEY` | WebSocket | WS upgrade | **401** | `{"jsonrpc":"2.0","error":{"code":-32401,"message":"Unauthorized"}}` |
| `https://mainnet.helius-rpc.com/?api-key=KEY` | HTTP RPC | POST | **403** | `Forbidden` (Cloudflare WAF — `server: cloudflare`, `zid` header present) |
| `https://staked.helius-rpc.com/?api-key=KEY` | HTTP RPC | POST | **403** | `Forbidden` (Cloudflare WAF) |
| `wss://devnet.helius-rpc.com/?api-key=KEY` | WebSocket | WS upgrade | **401** | Same `-32401 Unauthorized` |
| `wss://beta.helius-rpc.com/?api-key=KEY` | WebSocket | WS upgrade | **403** | Cloudflare Forbidden |

### LaserStream gRPC Endpoints (TLS reachable, not yet tested with gRPC handshake)

| Endpoint | TLS | ALPN | Status |
|---|---|---|---|
| `laserstream-mainnet-lax.helius-rpc.com:443` | ✅ connected | `h2` (HTTP/2) | Reachable |
| `laserstream-mainnet-slc.helius-rpc.com:443` | ✅ connected | `h2` (HTTP/2) | Reachable |

## Code: How the WS URL Is Constructed

**File:** `rust/crates/pump-quant-core/src/config/creds.rs`

```rust
pub struct Creds {
    pub helius_api_key: Secret,      // UUID-format API key
    pub laserstream_endpoint: String, // gRPC endpoint hostname
    pub helius_ws_base: String,       // WebSocket base URL (e.g. "wss://mainnet.helius-rpc.com")
}

/// Build the real Helius WS URL at the call site.
pub fn ws_url(&self) -> Secret {
    Secret::new(format!(
        "{}/?api-key={}",
        self.helius_ws_base,
        self.helius_api_key.expose()
    ))
}
```

The resulting URL is: `wss://mainnet.helius-rpc.com/?api-key=2c32e05f-...-7fe1`

**File:** `rust/crates/pump-quant-junction/src/bin/paper_session.rs`

```rust
let helius_url = creds.ws_url().expose().to_string();
// ...
let mut helius_conn = match WsConn::connect(&helius_url) {
    Ok(c) => c,
    Err(e) => {
        eprintln!("FAIL-CLOSED: Helius WS connect failed: {e}");
        return ExitCode::from(4);   // fail-closed, no stub
    }
};
```

## Error Reproduction

### Rust binary (tungstenite client)

```
$ PQ_CREDS_FILE="C:/Users/Alon/.hermes/creds/pump-quant.env" \
  ./target/release/paper_session.exe --duration-secs 15

[paper-session] PumpPortal: wss://pumpportal.fun/api/data
[paper-session] Helius WS:  wss://mainnet.helius-rpc.com/?api-key=<redacted>
[paper-session] duration=15s cap=4096 commitment=processed
[paper-session] MAX_ACCOUNT_SUBS=64 (FIFO eviction)
FAIL-CLOSED: Helius WS connect failed: upgrade refused: "HTTP/1.1 401 Unauthorized"
Nothing was stubbed. accountSubscribe is genuinely unavailable.
EXIT=4
```

### Python websocket-client

```python
import websocket
ws_url = 'wss://mainnet.helius-rpc.com/?api-key=2c32e05f-...-7fe1'
ws = websocket.WebSocket()
ws.connect(ws_url)
# → Handshake status 401 Unauthorized
# → Body: {"jsonrpc":"2.0","error":{"code":-32401,"message":"Unauthorized"}}
```

### Raw curl WS upgrade attempt

```
$ curl -v --include \
  'https://mainnet.helius-rpc.com/?api-key=2c32e05f-...-7fe1' \
  -H 'Connection: Upgrade' \
  -H 'Upgrade: websocket' \
  -H 'Sec-WebSocket-Version: 13' \
  -H 'Sec-WebSocket-Key: dGVzdA==' \
  --http1.1

< HTTP/1.1 400 Bad Request
< Server: cloudflare
< CF-RAY: a24e2273bc432b7b-LAX
```

## Key Observations

1. **The key IS valid** — it works on `marielle-qe2lvr-fast-mainnet.helius-rpc.com` (WS)
   and `devnet.helius-rpc.com` (HTTP RPC).

2. **The 401 on WS is from Helius's auth layer** (JSON-RPC error code `-32401`),
   NOT from Cloudflare. The response body is a valid JSON-RPC error.

3. **The 403 on HTTP POST is from Cloudflare WAF** (`server: cloudflare`, `zid` header,
   plain `Forbidden` body). This is separate from the WS auth issue.

4. **The key works on the dedicated endpoint** (`marielle-qe2lvr-fast-mainnet`) but NOT
   on the shared `mainnet.helius-rpc.com` endpoint for WebSocket.

5. **The operator confirmed** the WS URL `wss://mainnet.helius-rpc.com/?api-key=KEY`
   was obtained from the Helius dashboard. The `marielle` endpoint is from an older
   configuration and the operator explicitly said NOT to use it.

6. **GET on mainnet HTTP returns a valid JSON-RPC response** (404 + method-not-found
   error body), which means the key IS accepted for HTTP GET but NOT for WS upgrade
   on the same `mainnet.helius-rpc.com` host.

## Questions for Frontier Model

1. **Why would a Helius API key work on a dedicated endpoint (`marielle-qe2lvr-fast-mainnet`)
   but return 401 on the shared `mainnet.helius-rpc.com` for WebSocket?** Is this a
   plan/tier restriction, or a key configuration issue?

2. **Does Helius require a different WebSocket URL format for LaserStream keys?** The
   operator confirmed `wss://mainnet.helius-rpc.com/?api-key=KEY` from the Helius
   dashboard, but this returns 401. Is there a LaserStream-specific WS endpoint
   distinct from the standard RPC WS endpoint?

3. **Could the key be a gRPC-only (LaserStream) key without WS access?** The operator
   said it serves double duty (WS key + gRPC token), but the 401 on WS suggests WS
   access may not be enabled for this key on the mainnet endpoint.

4. **Is the `marielle-qe2lvr-fast-mainnet.helius-rpc.com` dedicated endpoint the
   correct WS endpoint for this key?** It works, but the operator said it's old and
   shouldn't be used. If the dedicated endpoint IS the correct one, how do we find
   the current dedicated endpoint name for this key from the Helius dashboard?

5. **Could the 403 on HTTP POST be a Cloudflare WAF rule blocking the POST body?**
   The GET works (returns JSON-RPC), but POST returns `Forbidden` from Cloudflare.
   This is a separate issue from the WS 401 but may indicate the key's access is
   restricted to certain methods/protocols on `mainnet.helius-rpc.com`.

## Environment

- **OS:** Windows 10 (DESKTOP-CP8N3IC)
- **Rust client:** tungstenite (via `WsConn::connect`)
- **Python client:** websocket-client
- **Curl:** standard WebSocket upgrade headers
- **Credential file:** `C:\Users\Alon\.hermes\creds\pump-quant.env` (outside git tree)
- **Env var:** `PQ_CREDS_FILE` set at User scope (survives restart)
