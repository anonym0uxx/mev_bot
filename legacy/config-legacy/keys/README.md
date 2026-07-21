# config/keys — Keypair Store

## ⚠️ CRITICAL — DO NOT DELETE THESE FILES

These keypairs are registered with external services. Losing a private key means
losing access to that service permanently — there is no recovery path.

---

## shredstream-keypair.json

**Purpose:** Jito ShredStream whitelist authentication

**Public key:** `2HegzSo8YujghD4jxwLjAri5XsQmUTCVwmVqoZjs21Wq`

**Format:** Solana keypair as uint8array JSON (64 bytes — first 32 = private, last 32 = public)

**Registered with:** Jito Labs ShredStream whitelist
**Submitted:** 2026-03-28
**Form URL:** https://web.miniextensions.com/WV3gZjFwqNqITsMufIEp
**Status:** Pending approval (days to weeks turnaround)

**Usage (once approved):**
```bash
docker run -d \
  -e BLOCK_ENGINE_URL=mainnet.block-engine.jito.wtf \
  -e AUTH_KEYPAIR_PATH=/app/keypair.json \
  -e DESIRED_REGIONS=ny \
  -e DEST_IP_PORTS=127.0.0.1:20000 \
  -v $(pwd)/config/keys/shredstream-keypair.json:/app/keypair.json \
  jitolabs/jito-shredstream-proxy
```

**Base58 private key:** (REDACTED — see .env or password manager)

---

## General notes

- Never commit these files to a public repo
- Add `config/keys/*.json` to .gitignore if not already there
- Back up to a password manager or encrypted store separately
