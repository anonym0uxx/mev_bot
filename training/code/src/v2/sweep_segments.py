import numpy as np, pyarrow as pa, pyarrow.compute as pc, pyarrow.json as paj, json

T = "/training/v2/canonical/renormalized_v7/trades.jsonl"
tbl = paj.read_json(T)
need = ["mint", "trader", "side", "venue", "sol_lamports", "tokens_raw", "recv_unix_ms"]
tbl = tbl.select(need)
print("rows:", tbl.num_rows)

side = np.asarray(pc.equal(tbl.column("side").combine_chunks(), pa.scalar("sell")))
n_sell = int(side.sum()); n_buy = int((~side).sum())
print(f"BUY={n_buy:,}  SELL={n_sell:,}  sell_share={n_sell/tbl.num_rows:.3f}")

dm = pc.dictionary_encode(tbl.column("mint").combine_chunks())
dt = pc.dictionary_encode(tbl.column("trader").combine_chunks())
mc = np.asarray(dm.indices).astype(np.int64)
tc = np.asarray(dt.indices).astype(np.int64)
n_mint = len(dm.dictionary); n_trader = len(dt.dictionary)
print(f"distinct mints={n_mint:,}  distinct traders={n_trader:,}")

key = mc * n_trader + tc
uk, inv = np.unique(key, return_inverse=True)
nb = np.bincount(inv, weights=(~side).astype(np.float64))
ns = np.bincount(inv, weights=side.astype(np.float64))
roundtrip = (nb > 0) & (ns > 0)
print(f"distinct (mint,trader) pairs={uk.size:,}")
print(f"round-trip pairs (>=1 buy AND >=1 sell)={int(roundtrip.sum()):,}")
print(f"  sells inside round-trip pairs={int(ns[roundtrip].sum()):,}")
print(f"  buys  inside round-trip pairs={int(nb[roundtrip].sum()):,}")

# wallets appearing in >1 mint
uw, tw = np.unique(tc, return_inverse=True)
mints_per_trader = np.bincount(np.unique(tc * 0 + np.asarray(dt.indices), return_inverse=True)[1])
mt = {}
for m, t in zip(mc, tc):
    mt.setdefault(t, set()).add(m)
multi = sum(1 for v in mt.values() if len(v) > 1)
print(f"traders active in >1 mint: {multi:,}")

json.dump({
    "rows": int(tbl.num_rows), "buy": n_buy, "sell": n_sell,
    "sell_share": n_sell / tbl.num_rows,
    "distinct_mints": int(n_mint), "distinct_traders": int(n_trader),
    "mint_trader_pairs": int(uk.size),
    "roundtrip_pairs": int(roundtrip.sum()),
    "sells_in_roundtrip": int(ns[roundtrip].sum()),
    "buys_in_roundtrip": int(nb[roundtrip].sum()),
    "traders_multi_mint": int(multi),
}, open("/training/v2/reports/SWEEP_segment_counts.json", "w"), indent=1)
