import json, datetime

def d(ms):
    return datetime.datetime.utcfromtimestamp(ms / 1000).strftime("%Y-%m-%d %H:%M:%S UTC")

mn = mx = None; n = 0
for line in open("canonical/renormalized_v6/launches.jsonl"):
    r = json.loads(line); t = r["recv_unix_ms"]; n += 1
    mn = t if mn is None or t < mn else mn
    mx = t if mx is None or t > mx else mx
print("v6 launches:", n, "window", d(mn), "->", d(mx))

mn = mx = None; n = 0
for line in open("canonical/renormalized_v6/trades.jsonl"):
    r = json.loads(line); t = r["recv_unix_ms"]; n += 1
    mn = t if mn is None or t < mn else mn
    mx = t if mx is None or t > mx else mx
    if n >= 2000000:
        break
print("v6 trades(first 2M):", n, "window", d(mn), "->", d(mx))

for s in ("20260824_053543_000288", "20260909_144906_000490", "20260909_173928_000596", "20260910_020917_000592"):
    y, mo, rest = s.split("_")
    print("session", s, "->", f"{y[:4]}-{y[4:6]}-{y[6:8]} {mo[:2]}:{mo[2:4]}:{rest[:2]}")
