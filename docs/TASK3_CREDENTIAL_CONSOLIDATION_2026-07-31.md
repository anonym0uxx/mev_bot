# Task 3: Credential Consolidation

**Date:** 2026-07-31  
**Directive:** §3/§3.1 (HERMES_PHASE_B_ACTIVATION_ONESHOT.md)  
**Status:** Consolidation complete — missing credentials identified

## What Already Exists on This Box

### Searched and NOT found (all empty/absent):

| Source | Path/Location | Status |
|--------|--------------|--------|
| `data/wallets.enc` | `D:/repos/mev_bot/data/wallets.enc` | NOT FOUND |
| Keypair files (`*.keypair`, `id.json`, `*.key`, `*.pem`) | repo + `C:/Users/Alon/` (3 levels) | NOT FOUND |
| `.env` file | `D:/repos/mev_bot/.env` | NOT FOUND (only `.env.example` exists) |
| Solana CLI config | `C:/Users/Alon/.config/solana/` | NOT FOUND |
| Environment variables (HELIUS, BIRDEYE, RPC, LASERSTREAM, WALLET) | Windows env | NOT FOUND |
| Shell profiles (`.bashrc`, `.bash_profile`, `.profile`, `.zshrc`) | `C:/Users/Alon/` | NOT FOUND |
| `config/default.json` | `D:/repos/mev_bot/config/` | NOT FOUND |
| Supervisor secrets | `supervisor/config/supervisor.yaml` | Contains `api_key_env` refs but NO actual keys |

### What DOES exist:

- `.env.example` — template listing expected variables (WALLET_PRIVATE_KEY,
  PUMP_PORTAL_API_KEY, BITQUERY_API_KEY, SOLANA_RPC_URL, JITO_BLOCK_ENGINE_URL,
  etc.) — all blank. This is a legacy template; the current architecture uses
  different env var names (HELIUS_API_KEY, BIRDEYE_API_KEY, RPC_URLS,
  LASERSTREAM_ENDPOINT per the source code and directive §3).
- `data/onchain-audit.md` — non-credential file.
- PumpPortal WebSocket — confirmed working with NO credential (Task 2).

## Conclusion

**No existing credentials of any kind exist on this box.** The box is clean.
There is nothing to consolidate — the operator starts from zero.

## What Is Missing — Split by Phase

### (a) DATA PLANE — needed for shadow and paper trading (read-only)

| Credential | Env Var | Purpose | Notes |
|-----------|---------|---------|-------|
| Helius API key | `HELIUS_API_KEY` | LaserStream gRPC primary ingest + Enhanced-WS fallback | Requires Helius Business plan ($499/mo) for LaserStream gRPC; Developer plan for Enhanced-WS |
| RPC URLs (2 providers min) | `RPC_URLS` | Deterministic multi-provider failover for account/state reads | Comma-separated failover priority. At least two independent RPC providers |
| LaserStream endpoint | `LASERSTREAM_ENDPOINT` | Override for LaserStream gRPC endpoint | Default: `https://laserstream-mainnet-ewr.helius-rpc.com` (may be overridable with same Helius key) |
| Birdeye API key | `BIRDEYE_API_KEY` | Token-security fields corroboration (§6.7 required source) | Requires Birdeye Starter+ plan for security fields |

**These are all read-only.** None of these credentials can sign transactions
or move SOL. They are data-plane keys for observing the chain.

### (b) SIGNING PLANE — needed only for a live probe (NOT now)

| Credential | Env Var | Purpose | Notes |
|-----------|---------|---------|-------|
| Wallet private key | `WALLET_PRIVATE_KEY` | Signing live transactions | **NOT REQUESTED.** Will be GENERATED on this box at ProbeReadinessGate. Private key will never travel over chat. |

## Constitutional §41 Tier-0 Custody Decision

Per the operator's directive and constitutional §41:

- The signing plane key does NOT exist on this box today, and a key that does
  not exist cannot be exposed by it.
- Shadow and paper trading require NO signing key.
- When (b) is needed at ProbeReadinessGate, the keypair will be GENERATED
  on this box and only the PUBLIC address reported. The private key will
  never travel over a chat transport.
- **If the operator offers the private key early, decline and cite this
  instruction.** This is a constitutional §41 Tier-0 custody decision.
  It is not the agent's to shortcut.

## Metrics

- common_chat_peg_parse: 0 (delta: 0)
- Largest stop_processing n_tokens: 112,858 (carry-forward)
- compression_count: 6 (delta: 0)
