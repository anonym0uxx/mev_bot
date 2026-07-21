#!/usr/bin/env node
/**
 * Build whale wallet list using Helius API.
 * 
 * Strategy:
 * 1. Pull recent Pump.fun program transactions via Helius
 * 2. Extract unique trader addresses
 * 3. For each candidate, fetch their transaction history and compute win rate
 * 4. Rank by trade count + estimated PnL, take top 50
 * 5. Write data/whale-wallets.json
 * 
 * Usage: node scripts/build-whale-list-helius.js
 */
const https = require('https');
const fs = require('fs');
const path = require('path');

const API_KEY = process.env.HELIUS_API_KEY || (() => {
  try {
    const env = fs.readFileSync(path.join(__dirname, '../.env'), 'utf8');
    const match = env.match(/HELIUS_API_KEY=(.+)/);
    return match ? match[1].trim() : null;
  } catch { return null; }
})();

if (!API_KEY) { console.error('HELIUS_API_KEY not found'); process.exit(1); }

const OUT_PATH = path.join(__dirname, '../data/whale-wallets.json');
const PUMP_PROGRAM = '6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P';
const BASE_URL = `https://mainnet.helius-rpc.com/?api-key=${API_KEY}`;

function sleep(ms) { return new Promise(r => setTimeout(r, ms)); }

async function rpcCall(method, params) {
  return new Promise((resolve, reject) => {
    const body = JSON.stringify({ jsonrpc: '2.0', id: 1, method, params });
    const url = new URL(BASE_URL);
    const options = {
      hostname: url.hostname,
      path: url.pathname + url.search,
      method: 'POST',
      headers: { 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(body) },
    };
    const req = https.request(options, res => {
      let data = '';
      res.on('data', c => data += c);
      res.on('end', () => {
        try { resolve(JSON.parse(data)); }
        catch(e) { reject(new Error(`Parse error: ${data.slice(0, 200)}`)); }
      });
    });
    req.on('error', reject);
    req.write(body);
    req.end();
  });
}

async function heliusGet(endpoint) {
  return new Promise((resolve, reject) => {
    const url = `https://api.helius.xyz${endpoint}&api-key=${API_KEY}`;
    const parsed = new URL(url);
    const options = {
      hostname: parsed.hostname,
      path: parsed.pathname + parsed.search,
      method: 'GET',
    };
    const req = https.request(options, res => {
      let data = '';
      res.on('data', c => data += c);
      res.on('end', () => {
        try { resolve(JSON.parse(data)); }
        catch(e) { reject(new Error(`Parse error: ${data.slice(0, 200)}`)); }
      });
    });
    req.on('error', reject);
    req.end();
  });
}

async function getRecentPumpTxSignatures(limit = 1000) {
  console.log(`Fetching recent Pump.fun transactions (limit: ${limit})...`);
  // Get recent signatures for the Pump.fun program account
  const result = await rpcCall('getSignaturesForAddress', [
    PUMP_PROGRAM,
    { limit: Math.min(limit, 1000) }
  ]);
  if (result.error) throw new Error(`RPC error: ${JSON.stringify(result.error)}`);
  return (result.result || []).map(r => r.signature);
}

async function getTransactionTraders(signatures) {
  console.log(`Parsing ${signatures.length} transactions for trader addresses...`);
  const traderMap = {}; // address -> { buys, sells, totalBuySOL, totalSellSOL }
  
  const BATCH = 100;
  for (let i = 0; i < signatures.length; i += BATCH) {
    const batch = signatures.slice(i, i + BATCH);
    await sleep(200); // rate limit
    
    try {
      // Use Helius enhanced transactions API
      const result = await new Promise((resolve, reject) => {
        const body = JSON.stringify({ transactions: batch });
        const url = new URL(`https://api.helius.xyz/v0/transactions/?api-key=${API_KEY}`);
        const options = {
          hostname: url.hostname,
          path: url.pathname + url.search,
          method: 'POST',
          headers: { 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(body) },
        };
        const req = https.request(options, res => {
          let data = '';
          res.on('data', c => data += c);
          res.on('end', () => {
            try { resolve(JSON.parse(data)); }
            catch(e) { resolve([]); }
          });
        });
        req.on('error', () => resolve([]));
        req.write(body);
        req.end();
      });

      if (!Array.isArray(result)) continue;

      for (const tx of result) {
        if (!tx || tx.transactionError) continue;
        
        // Look for Pump.fun swap events in tokenTransfers or nativeTransfers
        const feePayer = tx.feePayer;
        if (!feePayer) continue;

        // Check if this is a pump.fun trade by looking at accountData
        const isPumpTx = tx.instructions?.some(ix => ix.programId === PUMP_PROGRAM) ||
                         tx.accountData?.some(a => a.account === PUMP_PROGRAM);
        if (!isPumpTx) continue;

        // Native SOL transfers indicate buy/sell amounts
        const nativeTransfers = tx.nativeTransfers || [];
        let solOut = 0, solIn = 0;
        for (const t of nativeTransfers) {
          if (t.fromUserAccount === feePayer) solOut += t.amount / 1e9;
          if (t.toUserAccount === feePayer) solIn += t.amount / 1e9;
        }

        if (!traderMap[feePayer]) {
          traderMap[feePayer] = { address: feePayer, buys: 0, sells: 0, totalBuySOL: 0, totalSellSOL: 0 };
        }
        if (solOut > 0.001) { traderMap[feePayer].buys++; traderMap[feePayer].totalBuySOL += solOut; }
        if (solIn > 0.001) { traderMap[feePayer].sells++; traderMap[feePayer].totalSellSOL += solIn; }
      }
      
      process.stdout.write(`  Processed ${Math.min(i + BATCH, signatures.length)}/${signatures.length}\r`);
    } catch(e) {
      // Non-fatal — skip this batch
    }
  }
  console.log('');
  return traderMap;
}

async function main() {
  console.log('=== Helius Whale Wallet Bootstrap ===\n');

  // Step 1: Get recent pump.fun tx signatures
  let signatures;
  try {
    signatures = await getRecentPumpTxSignatures(1000);
    console.log(`Got ${signatures.length} signatures`);
  } catch(e) {
    console.error('Failed to fetch signatures:', e.message);
    // Fallback: write empty list
    const fallback = { version: 1, updatedAt: new Date().toISOString(), wallets: [], note: `Helius fetch failed: ${e.message}` };
    fs.writeFileSync(OUT_PATH, JSON.stringify(fallback, null, 2));
    process.exit(0);
  }

  if (signatures.length === 0) {
    console.log('No signatures found. Writing empty list.');
    fs.writeFileSync(OUT_PATH, JSON.stringify({ version: 1, updatedAt: new Date().toISOString(), wallets: [] }, null, 2));
    process.exit(0);
  }

  // Step 2: Parse traders from transactions
  let traderMap;
  try {
    traderMap = await getTransactionTraders(signatures);
  } catch(e) {
    console.error('Failed to parse traders:', e.message);
    fs.writeFileSync(OUT_PATH, JSON.stringify({ version: 1, updatedAt: new Date().toISOString(), wallets: [], note: e.message }, null, 2));
    process.exit(0);
  }

  console.log(`Found ${Object.keys(traderMap).length} unique traders`);

  // Step 3: Score and rank
  // Score = estimated net PnL (sellSOL - buySOL) weighted by trade count
  // Filter: min 3 buys, has at least some sells (active trader not just holding)
  const candidates = Object.values(traderMap)
    .filter(w => w.buys >= 3 && w.sells >= 1)
    .map(w => ({
      ...w,
      estimatedPnl: w.totalSellSOL - w.totalBuySOL,
      tradeCount: w.buys + w.sells,
      sellBuyRatio: w.sells / Math.max(w.buys, 1),
    }))
    .filter(w => w.estimatedPnl > 0) // positive PnL only
    .sort((a, b) => b.estimatedPnl - a.estimatedPnl)
    .slice(0, 50);

  console.log(`\nQualified whale candidates: ${candidates.length}`);

  if (candidates.length > 0) {
    console.log('\nTop 10 whales by estimated PnL:');
    candidates.slice(0, 10).forEach((w, i) => {
      console.log(`  ${i+1}. ${w.address.slice(0,8)}... | buys=${w.buys} sells=${w.sells} | est.PnL=${w.estimatedPnl.toFixed(4)} SOL`);
    });
  }

  // Step 4: Write output
  const output = {
    version: 1,
    updatedAt: new Date().toISOString(),
    source: 'helius',
    wallets: candidates.map(w => ({
      address: w.address,
      tradeCount: w.tradeCount,
      estimatedPnlSol: parseFloat(w.estimatedPnl.toFixed(4)),
      buys: w.buys,
      sells: w.sells,
    })),
  };

  fs.writeFileSync(OUT_PATH, JSON.stringify(output, null, 2));
  console.log(`\n✅ Saved ${candidates.length} whale wallets to ${OUT_PATH}`);
  
  if (candidates.length >= 5) {
    console.log('\n🎯 Stream 5 will activate on next daemon restart (need >= 5 wallets)');
  } else {
    console.log(`\n⚠️  Only ${candidates.length} whales found — Stream 5 needs >= 5 to activate`);
    console.log('   Try running again during higher-volume hours (Asia/EU open)');
  }
}

main().catch(e => { console.error('Fatal:', e); process.exit(1); });
