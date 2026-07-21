#!/usr/bin/env node
/**
 * Bootstrap whale wallet list from Bitquery GraphQL.
 * Finds top 50 Pump.fun traders by profit over last 7 days.
 * Outputs: data/whale-wallets.json
 */
const https = require('https');
const fs = require('fs');
const path = require('path');

const API_KEY = process.env.BITQUERY_API_KEY;
if (!API_KEY) { console.error('BITQUERY_API_KEY not set'); process.exit(1); }

const OUT_PATH = path.join(__dirname, '../data/whale-wallets.json');

// Bitquery v2 GraphQL endpoint
const ENDPOINT = 'streaming.bitquery.io';
const PATH = '/graphql';

const query = `{
  Solana {
    DEXTrades(
      where: {
        Trade: {
          Dex: {
            ProgramAddress: {
              is: "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"
            }
          }
        }
        Block: {
          Time: {
            since: "${new Date(Date.now() - 7 * 24 * 60 * 60 * 1000).toISOString()}"
          }
        }
      }
      limit: { count: 5000 }
      orderBy: { descendingByField: "volume_usd" }
    ) {
      Trade {
        Account {
          Address
        }
        Buy {
          Amount
          Currency {
            MintAddress
          }
        }
        Sell {
          Amount
        }
      }
      volume_usd: sum(of: Trade__Buy__AmountInUSD)
      trade_count: count
    }
  }
}`;

// Alternative simpler query if above fails:
const querySimple = `{
  Solana(dataset: realtime) {
    DEXTrades(
      where: {
        Trade: {
          Dex: {
            ProgramAddress: {
              is: "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P"
            }
          }
        }
      }
      limit: { count: 1000 }
    ) {
      Trade {
        Account {
          Address
        }
        Buy {
          Amount
        }
        Sell {
          Amount
        }
      }
      count
    }
  }
}`;

function doRequest(queryStr) {
  return new Promise((resolve, reject) => {
    const body = JSON.stringify({ query: queryStr });
    const options = {
      hostname: ENDPOINT,
      path: PATH,
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'Authorization': `Bearer ${API_KEY}`,
        'Content-Length': Buffer.byteLength(body),
      },
    };
    const req = https.request(options, (res) => {
      let data = '';
      res.on('data', chunk => data += chunk);
      res.on('end', () => {
        try { resolve({ status: res.statusCode, body: JSON.parse(data) }); }
        catch(e) { resolve({ status: res.statusCode, body: data }); }
      });
    });
    req.on('error', reject);
    req.write(body);
    req.end();
  });
}

async function main() {
  console.log('Querying Bitquery for top Pump.fun traders (last 7 days)...');
  
  let result = await doRequest(query);
  console.log('Status:', result.status);
  
  if (result.status !== 200 || result.body.errors) {
    console.log('Primary query failed:', JSON.stringify(result.body).slice(0, 500));
    console.log('Trying simpler query...');
    result = await doRequest(querySimple);
    console.log('Simple query status:', result.status);
    console.log('Simple query result:', JSON.stringify(result.body).slice(0, 500));
  }
  
  if (result.status !== 200) {
    console.error('Both queries failed. Check BITQUERY_API_KEY and plan.');
    // Create fallback empty list so daemon doesn't crash on missing file
    const fallback = {
      version: 1,
      updatedAt: new Date().toISOString(),
      wallets: [],
      note: 'Empty — both Bitquery queries failed (check API key / plan)',
    };
    fs.writeFileSync(OUT_PATH, JSON.stringify(fallback, null, 2));
    console.log('Created empty fallback whale-wallets.json');
    process.exit(1);
  }

  const trades = result.body?.data?.Solana?.DEXTrades || [];
  console.log(`Got ${trades.length} trade records`);

  if (trades.length === 0) {
    console.log('No trades returned. Creating fallback empty list.');
    const fallback = {
      version: 1,
      updatedAt: new Date().toISOString(),
      wallets: [],
      note: 'Empty — bootstrap query returned no data',
    };
    fs.writeFileSync(OUT_PATH, JSON.stringify(fallback, null, 2));
    return;
  }

  // Aggregate by trader address
  const traderMap = {};
  for (const t of trades) {
    const addr = t.Trade?.Account?.Address;
    if (!addr) continue;
    if (!traderMap[addr]) traderMap[addr] = { address: addr, tradeCount: 0, totalVolume: 0 };
    traderMap[addr].tradeCount += (t.trade_count || t.count || 1);
    traderMap[addr].totalVolume += parseFloat(t.volume_usd || 0);
  }

  // Filter: min 10 trades, sort by volume desc, take top 50
  const wallets = Object.values(traderMap)
    .filter(w => w.tradeCount >= 10)
    .sort((a, b) => b.totalVolume - a.totalVolume)
    .slice(0, 50)
    .map(w => ({ address: w.address, tradeCount: w.tradeCount, totalVolume: w.totalVolume.toFixed(2) }));

  const output = {
    version: 1,
    updatedAt: new Date().toISOString(),
    wallets,
  };

  fs.writeFileSync(OUT_PATH, JSON.stringify(output, null, 2));
  console.log(`\nSaved ${wallets.length} whale wallets to ${OUT_PATH}`);
  if (wallets.length > 0) {
    console.log('Top 5:');
    wallets.slice(0, 5).forEach((w, i) => console.log(`  ${i+1}. ${w.address.slice(0,8)}... trades=${w.tradeCount} vol=$${w.totalVolume}`));
  }
}

main().catch(e => { console.error(e); process.exit(1); });
