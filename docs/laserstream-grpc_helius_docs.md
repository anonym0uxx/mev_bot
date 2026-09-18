---
url: https://www.helius.dev/docs/api-reference/laserstream-grpc
last_updated: 2026-05-18
---

# LaserStream API

Next-generation Solana data streaming with ultra-low latency, historical replay, and multi-node reliability for high-performance applications.

## Getting Started

1. Get your API key at https://dashboard.helius.dev
2. Choose your plan (Devnet: Developer/Business, Mainnet: Professional)
3. Select the endpoint closest to your server location
4. Connect using the LaserStream SDK or existing Yellowstone gRPC code

### Fastest Setup

```javascript
import { subscribe, CommitmentLevel } from 'helius-laserstream';

const config = {
  apiKey: "your-helius-api-key",
  endpoint: "https://laserstream-mainnet-ewr.helius-rpc.com"
};

await subscribe(config, subscriptionRequest, handleData, handleError);
```

## Use Cases
- Real-time transaction monitoring and alerting
- High-frequency trading and arbitrage systems
- DEX trade tracking and market data aggregation
- NFT marketplace activity monitoring
- Wallet activity tracking across thousands of accounts

## Why Helius LaserStream
- 24-hour historical replay (216,000 slots) for data continuity after disconnections
- Multi-node failover with automatic load balancing across regions
- 40x faster than JavaScript Yellowstone clients (Rust core with zero-copy NAPI bindings)
- Drop-in replacement for existing Yellowstone gRPC setups

## Quick Reference
- Base URLs: See Endpoints section for regional options
- Auth: Helius API key (passed as token)
- Historical replay: Up to 216,000 slots (~24 hours)

## Rate Limits
| Plan | Monthly Price | Networks | Max Pubkeys | Active Connections |
|------|----------|-------------|--------------------|
| Free | $0 | â€” | â€” | â€” |
| Developer | $49 | Devnet | 10M | â€” |
| Business | $499 | Devnet, Mainnet | 10M | 10 |
| Professional | $999 | Devnet, Mainnet | 10M | 100 |

## Credits
- 2 credits per 0.1 MB of streamed data (uncompressed)

## Endpoints

### Mainnet Regions

| Region | Location | Endpoint |
|--------|----------|----------|
| ewr | Newark, NJ | `https://laserstream-mainnet-ewr.helius-rpc.com` |
| pitt | Pittsburgh | `https://laserstream-mainnet-pitt.helius-rpc.com` |
| slc | Salt Lake City | `https://laserstream-mainnet-slc.helius-rpc.com` |
| lax | Los Angeles | `https://laserstream-mainnet-lax.helius-rpc.com` |
| lon | London | `https://laserstream-mainnet-lon.helius-rpc.com` |
| ams | Amsterdam | `https://laserstream-mainnet-ams.helius-rpc.com` |
| fra | Frankfurt | `https://laserstream-mainnet-fra.helius-rpc.com` |
| tyo | Tokyo | `https://laserstream-mainnet-tyo.helius-rpc.com` |
| sgp | Singapore | `https://laserstream-mainnet-sgp.helius-rpc.com` |

### Devnet

| Network | Endpoint |
|---------|----------|
| Devnet | `https://laserstream-devnet-ewr.helius-rpc.com` |

## gRPC Methods

### Subscribe

Stream real-time updates for accounts, transactions, blocks, and slots.

#### Parameters
- `accounts` (optional): Account filter configuration
- `transactions` (optional): Transaction filter configuration
- `blocks` (optional): Block filter configuration
- `slots` (optional): Slot update configuration
- `commitment` (optional): processed | confirmed | finalized

#### Request
```javascript
import { subscribe, CommitmentLevel } from 'helius-laserstream';

const request = {
  accounts: {
    client: "my-app",
    account: ["TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"],
  },
  commitment: CommitmentLevel.CONFIRMED
};

await subscribe(config, request, (data) => {
  console.log("Update:", data);
}, (error) => {
  console.error("Error:", error);
});
```

### GetBlockHeight

Get the current block height.

#### Request
```javascript
const blockHeight = await client.getBlockHeight({
  commitment: CommitmentLevel.FINALIZED
});
```

#### Response
```json
{
  "blockHeight": 250000000
}
```

### GetLatestBlockhash

Get the latest blockhash for transaction signing.

#### Request
```javascript
const result = await client.getLatestBlockhash({
  commitment: CommitmentLevel.FINALIZED
});
```

#### Response
```json
{
  "blockhash": "EkSnNWid2cvwEVnVx9aBqawnmiCNiDgp3gUdkDPTKN1N",
  "lastValidBlockHeight": 250000100
}
```

### GetSlot

Get the current slot number.

#### Request
```javascript
const slot = await client.getSlot({
  commitment: CommitmentLevel.PROCESSED
});
```

#### Response
```json
{
  "slot": 275000000
}
```

### GetVersion

Get API and node version information.

#### Request
```javascript
const version = await client.getVersion();
```

#### Response
```json
{
  "version": "1.0.0"
}
```

### IsBlockhashValid

Check if a blockhash is still valid.

#### Parameters
- `blockhash` (required): The blockhash to validate
- `commitment` (optional): Commitment level

#### Request
```javascript
const isValid = await client.isBlockhashValid({
  blockhash: "EkSnNWid2cvwEVnVx9aBqawnmiCNiDgp3gUdkDPTKN1N",
  commitment: CommitmentLevel.PROCESSED
});
```

#### Response
```json
{
  "valid": true
}
```

### Ping

Check API connectivity.

#### Request
```javascript
const pong = await client.ping();
```

## Common Patterns

### Migration from Yellowstone gRPC
```javascript
// Before: Standard Yellowstone gRPC
const connection = new GeyserConnection(
  "your-current-endpoint.com",
  { token: "your-current-token" }
);

// After: LaserStream (just change endpoint and token)
const connection = new GeyserConnection(
  "https://laserstream-mainnet-ewr.helius-rpc.com",
  { token: "your-helius-api-key" }
);
```

### Historical Replay After Disconnect
```javascript
const request = {
  accounts: {
    client: "my-app",
    account: ["..."],
  },
  // SDK automatically handles reconnection and replays missed data
  // from the last 24 hours (216,000 slots)
};
```

Guide: https://www.helius.dev/docs/laserstream/historical-replay.md

### Transaction Filtering
```javascript
const request = {
  transactions: {
    client: "my-app",
    accountInclude: ["TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"],
    accountExclude: [],
    accountRequired: [],
  },
  commitment: CommitmentLevel.CONFIRMED
};
```

## Best Practices

1. Choose the closest region: Select the endpoint nearest to your server for lowest latency
2. Use the LaserStream SDK: Automatic reconnection, historical replay, and optimized performance
3. Filter aggressively: Only subscribe to the accounts/transactions you need to minimize data transfer
4. Handle commitment levels appropriately: Use `processed` for speed, `finalized` for certainty

## Troubleshooting

Q: How do I handle disconnections?
A: The LaserStream SDK automatically reconnects and replays missed data. If using raw gRPC, implement reconnection logic with the historical replay feature.

Q: Why am I seeing high credit usage?
A: Check your filtersâ€”broad subscriptions consume more data. Consider LaserStream Plus add-ons for high-volume use cases (5TB-100TB+ monthly).

Q: Can I use my existing Yellowstone gRPC code?
A: Yes, LaserStream is a drop-in replacement. Just update your endpoint and API token.

All LaserStream FAQs: /docs/faqs/laserstream.md

## SDKs

### Node.js (helius-laserstream)
```javascript
import { subscribe, CommitmentLevel } from 'helius-laserstream';

const config = {
  apiKey: "your-api-key",
  endpoint: "https://laserstream-mainnet-ewr.helius-rpc.com"
};

await subscribe(config, request, handleData, handleError);
```

SDK repo: https://github.com/helius-labs/laserstream-sdk
LaserStream Clients: /docs/laserstream/clients.md

## LaserStream vs Alternatives

| Feature | LaserStream | Standard WebSocket | Yellowstone gRPC |
|---------|-------------|-------------------|------------------|
| Historical replay | 24 hours | No | Limited |
| Auto-reconnect | Built-in | Manual | Manual |
| Multi-node failover | Automatic | Manual | Manual |
| Shredstream enabled | Yes | No | Manual |

Compare: /docs/laserstream/laserstream-vs-dedicated-nodes.md

## Guides
- Overview: /docs/laserstream.md
- gRPC Connection: /docs/laserstream/grpc.md
- Raw Shreds (UDP): /docs/shred-delivery/raw-shreds.md
- Pre-processed Transactions: /docs/shred-delivery/preprocessed-transactions.md
- Account Subscription Guide: /docs/laserstream/guides/account-subscription.md
- Decoding Transaction Data: /docs/laserstream/guides/decoding-transaction-data.md

## Related
- Helius WebSockets: /docs/api-reference/rpc/websocket/llms.txt
- Raw Shreds (UDP): /docs/shred-delivery/raw-shreds.md
- Helius Shred Delivery: /docs/shred-delivery/llms.txt

---
Full documentation index: /docs/llms.txt