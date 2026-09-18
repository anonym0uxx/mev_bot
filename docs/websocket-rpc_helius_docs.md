---
url: https://www.helius.dev/docs/api-reference/rpc/websocket-methods
last_updated: 2026-05-18
---

# LaserStream WebSocket API

Helius's WebSocket streaming product. Serves the standard Solana RPC WebSocket subscriptions plus Helius extensions (`transactionSubscribe`, enhanced `accountSubscribe`) on a single unified endpoint.

## Getting Started

1. Get your API key at https://dashboard.helius.dev (For Agents: use the [Helius CLI](https://www.helius.dev/docs/api-reference/helius-cli.md) to programmatically create accounts and generate an API key)
2. Connect to the WebSocket endpoint
3. Send a subscription request
4. Receive real-time updates

### Fastest Setup

```javascript
const WebSocket = require('ws');

const ws = new WebSocket('wss://mainnet.helius-rpc.com/?api-key=YOUR_API_KEY');

ws.on('open', () => {
  ws.send(JSON.stringify({
    jsonrpc: '2.0',
    id: 1,
    method: 'accountSubscribe',
    params: [
      'ACCOUNT_ADDRESS',
      { encoding: 'jsonParsed', commitment: 'confirmed' }
    ]
  }));
});

ws.on('message', (data) => {
  const response = JSON.parse(data);
  if (response.method === 'accountNotification') {
    console.log('Account updated:', response.params.result);
  }
});
```

## Use Cases
- Monitor wallet balance changes
- Track token account updates
- Watch program account changes
- Confirm transaction signatures
- Stream new blocks and slots
- Monitor logs for specific programs

## Why LaserStream WebSocket
- Available on all plans (including free) for the standard Solana subscription methods; Helius extensions (`transactionSubscribe`, enhanced `accountSubscribe`) require Developer+
- Runs on the same backend as LaserStream gRPC
- Up to ~200 ms faster than standard Agave RPC-based WebSockets
- Full Solana WebSocket API compatibility
- Up to 1,000 concurrent connections (plan dependent)

## Quick Reference
- Mainnet: wss://mainnet.helius-rpc.com/?api-key=YOUR_API_KEY
- Devnet: wss://devnet.helius-rpc.com/?api-key=YOUR_API_KEY
- Latency: ~400ms

## Credits

All WebSocket streaming is metered at 2 credits per 0.1 MB (uncompressed). Opening a connection costs 1 credit.

## Connections and Rate Limits

| Plan | Concurrent Connections | Requests/sec |
|------|-------------|--------------|
| Free | 5 | 10 req/s |
| Developer | 150 | 50 req/s |
| Business | 250 | 200 req/s |
| Professional | 1,000 | 500 req/s |

## Methods

Stable, Helius-supported Solana WebSockets methods.

### accountSubscribe

Subscribe to account data changes.

#### Parameters
- `account` (required): Account public key (base-58)
- `options` (optional):
  - `encoding`: "base58" | "base64" | "jsonParsed"
  - `commitment`: "processed" | "confirmed" | "finalized"

#### Request
```javascript
ws.send(JSON.stringify({
  jsonrpc: '2.0',
  id: 1,
  method: 'accountSubscribe',
  params: [
    'ACCOUNT_ADDRESS',
    { encoding: 'jsonParsed', commitment: 'confirmed' }
  ]
}));
```

#### Notification
```json
{
  "method": "accountNotification",
  "params": {
    "subscription": 12345,
    "result": {
      "value": {
        "lamports": 1000000,
        "data": { "parsed": { ... } },
        "owner": "PROGRAM_ID",
        "executable": false
      }
    }
  }
}
```

### programSubscribe

Subscribe to all accounts owned by a program.

#### Parameters
- `programId` (required): Program public key
- `options` (optional):
  - `encoding`: "base58" | "base64" | "jsonParsed"
  - `commitment`: "processed" | "confirmed" | "finalized"
  - `filters`: Array of filter objects (dataSize, memcmp)

#### Request
```javascript
ws.send(JSON.stringify({
  jsonrpc: '2.0',
  id: 1,
  method: 'programSubscribe',
  params: [
    'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA',
    {
      encoding: 'jsonParsed',
      commitment: 'confirmed',
      filters: [{ dataSize: 165 }]
    }
  ]
}));
```

#### Notification
```json
{
  "method": "programNotification",
  "params": {
    "subscription": 12345,
    "result": {
      "value": {
        "pubkey": "ACCOUNT_ADDRESS",
        "account": {
          "lamports": 1000000,
          "data": { "parsed": { ... } },
          "owner": "PROGRAM_ID"
        }
      }
    }
  }
}
```

### logsSubscribe

Subscribe to transaction logs.

#### Parameters
- `filter` (required): "all" | "allWithVotes" | { mentions: ["ADDRESS"] }
- `options` (optional):
  - `commitment`: "processed" | "confirmed" | "finalized"

#### Request
```javascript
ws.send(JSON.stringify({
  jsonrpc: '2.0',
  id: 1,
  method: 'logsSubscribe',
  params: [
    { mentions: ['PROGRAM_ADDRESS'] },
    { commitment: 'confirmed' }
  ]
}));
```

#### Notification
```json
{
  "method": "logsNotification",
  "params": {
    "result": {
      "value": {
        "signature": "5wJb...",
        "logs": [
          "Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA invoke [1]",
          "Program log: Instruction: Transfer",
          "Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA success"
        ]
      }
    }
  }
}
```

### signatureSubscribe

Subscribe to transaction confirmation. Automatically cancelled after notification.

#### Parameters
- `signature` (required): Transaction signature
- `options` (optional):
  - `commitment`: "processed" | "confirmed" | "finalized"

#### Request
```javascript
ws.send(JSON.stringify({
  jsonrpc: '2.0',
  id: 1,
  method: 'signatureSubscribe',
  params: [
    'TRANSACTION_SIGNATURE',
    { commitment: 'confirmed' }
  ]
}));
```

#### Notification
```json
{
  "method": "signatureNotification",
  "params": {
    "result": {
      "value": { "err": null }
    }
  }
}
```

### slotSubscribe

Subscribe to slot changes.

#### Request
```javascript
ws.send(JSON.stringify({
  jsonrpc: '2.0',
  id: 1,
  method: 'slotSubscribe'
}));
```

#### Notification
```json
{
  "method": "slotNotification",
  "params": {
    "result": {
      "slot": 123456789,
      "parent": 123456788,
      "root": 123456780
    }
  }
}
```

### rootSubscribe

Subscribe to new root updates.

#### Request
```javascript
ws.send(JSON.stringify({
  jsonrpc: '2.0',
  id: 1,
  method: 'rootSubscribe'
}));
```

#### Notification
```json
{
  "method": "rootNotification",
  "params": {
    "result": 123456780
  }
}
```

### slotsUpdatesSubscribe

Subscribe to detailed slot updates. High volume â€” use only when needed.

#### Request
```javascript
ws.send(JSON.stringify({
  jsonrpc: '2.0',
  id: 1,
  method: 'slotsUpdatesSubscribe'
}));
```

#### Notification
```json
{
  "method": "slotsUpdatesNotification",
  "params": {
    "result": {
      "slot": 123456789,
      "timestamp": 1704067200,
      "type": "completed"
    }
  }
}
```

### Unsubscribing

All subscribe methods have a corresponding unsubscribe method. Use the subscription ID returned from the initial subscription response.

```javascript
ws.send(JSON.stringify({
  jsonrpc: '2.0',
  id: 1,
  method: '<method>Unsubscribe',  // accountUnsubscribe, programUnsubscribe, etc.
  params: [subscriptionId]
}));
```

Response:
```json
{ "jsonrpc": "2.0", "result": true, "id": 1 }
```

#### Unsubscribe methods: 

- accountUnsubscribe
- programUnsubscribe
- logsUnsubscribe
- signatureUnsubscribe
- slotUnsubscribe
- blockUnsubscribe
- rootUnsubscribe
- slotsUpdatesUnsubscribe
- voteUnsubscribe

## Unstable Methods

Unstable methods that are not supported by Helius. This documentation is provided for reference only.

### blockSubscribe

Subscribe to new blocks. Requires validator flag `--rpc-pubsub-enable-vote-subscription` â€” not available on standard Helius endpoints.

#### Parameters
- `filter` (required): "all" | { mentionsAccountOrProgram: "ADDRESS" }
- `options` (optional):
  - `commitment`: "confirmed" | "finalized"
  - `encoding`: "base58" | "base64" | "jsonParsed"
  - `transactionDetails`: "full" | "signatures" | "none"

#### Request
```javascript
ws.send(JSON.stringify({
  jsonrpc: '2.0',
  id: 1,
  method: 'blockSubscribe',
  params: [
    'all',
    { commitment: 'confirmed', transactionDetails: 'signatures' }
  ]
}));
```

#### Notification
```json
{
  "method": "blockNotification",
  "params": {
    "result": {
      "value": {
        "slot": 123456789,
        "block": {
          "blockhash": "...",
          "previousBlockhash": "...",
          "parentSlot": 123456788,
          "signatures": ["sig1", "sig2"]
        }
      }
    }
  }
}
```

#### Use Instead
- `slotSubscribe` to get notified of new slots
- LaserStream for production block streaming. See https://www.helius.dev/docs/laserstream/grpc/llms.txt

### voteSubscribe

Subscribe to vote notifications. Requires validator flag `--rpc-pubsub-enable-vote-subscription` â€” not available on standard Helius endpoints.

#### Parameters
- None

#### Request
```javascript
ws.send(JSON.stringify({
  jsonrpc: '2.0',
  id: 1,
  method: 'voteSubscribe'
}));
```

#### Response
```json
{
  "jsonrpc": "2.0",
  "result": 0,
  "id": 1
}
```

#### Notification
```json
{
  "method": "voteNotification",
  "params": {
    "result": {
      "hash": "8Rshv2oMkPu5E4opXTRyuyBeZBqQ4S477VG26wUTFxUM",
      "slots": [1, 2],
      "timestamp": null,
      "signature": "...",
      "votePubkey": "..."
    },
    "subscription": 0
  }
}
```

#### Notification Fields
- `hash`: Vote hash
- `slots`: Array of slots covered by the vote (u64[])
- `timestamp`: Vote timestamp (i64 or null)
- `signature`: Transaction signature containing the vote
- `votePubkey`: Vote account public key (base-58)

#### Use Instead
- `signatureSubscribe` to track when specific transactions reach a commitment level
- `accountSubscribe` to monitor vote account state changes for specific validators

## Error Responses

| Code | Status | Message |
|------|--------|---------|
| -32600 | 400 | Invalid request |
| -32601 | 404 | Method not found |
| -32602 | 400 | Invalid params |
| -32603 | 500 | Internal error |

## Common Patterns

### Watch SPL Token Transfers
```javascript
ws.send(JSON.stringify({
  jsonrpc: '2.0',
  id: 1,
  method: 'programSubscribe',
  params: [
    'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA',
    { encoding: 'jsonParsed', commitment: 'confirmed' }
  ]
}));
```

### Monitor Specific Account
```javascript
ws.send(JSON.stringify({
  jsonrpc: '2.0',
  id: 1,
  method: 'accountSubscribe',
  params: [
    'YOUR_WALLET_ADDRESS',
    { encoding: 'jsonParsed', commitment: 'confirmed' }
  ]
}));
```

### Connection with Reconnection
```javascript
class WebSocketClient {
  constructor(apiKey) {
    this.url = `wss://mainnet.helius-rpc.com/?api-key=${apiKey}`;
    this.reconnectInterval = 5000;
    this.pingInterval = 30000;
    this.connect();
  }

  connect() {
    this.ws = new WebSocket(this.url);

    this.ws.on('open', () => {
      this.startPing();
      this.onOpen();
    });

    this.ws.on('message', (data) => this.onMessage(JSON.parse(data)));

    this.ws.on('close', () => {
      this.stopPing();
      setTimeout(() => this.connect(), this.reconnectInterval);
    });

    this.ws.on('error', (err) => console.error('Error:', err));
  }

  startPing() {
    this.pingTimer = setInterval(() => {
      if (this.ws.readyState === WebSocket.OPEN) this.ws.ping();
    }, this.pingInterval);
  }

  stopPing() {
    if (this.pingTimer) clearInterval(this.pingTimer);
  }

  onOpen() { /* Override */ }
  onMessage(data) { /* Override */ }
}
```

## Best Practices

1. Implement reconnection: WebSockets can disconnect; always auto-reconnect
2. Send pings every 30-60 seconds: Prevents 10-minute inactivity timeout
3. Handle subscription IDs: Track IDs to manage multiple subscriptions
4. Process messages async: Don't block the message handler
5. Use filters on programSubscribe: Reduce message volume with dataSize/memcmp filters

## Troubleshooting

Q: My WebSocket keeps disconnecting. Why?
A: Implement reconnection logic and send pings every 30-60 seconds. The server has a 10-minute inactivity timeout.

Q: Can I have multiple subscriptions on one connection?
A: Yes, send multiple subscription requests. Track subscription IDs to identify message sources.

Q: How do I filter programSubscribe results?
A: Use the `filters` parameter with `dataSize` or `memcmp` conditions.

All WebSockets FAQs: https://www.helius.dev/docs/faqs/websockets.md

## Related
- RPC: https://www.helius.dev/docs/api-reference/rpc/http/llms.txt
- Helius LaserStream gRPC: https://www.helius.dev/docs/laserstream/grpc/llms.txt

---
Full documentation index: https://www.helius.dev/docs/llms.txt