import json, re, sys, collections
def req(d): return [json.loads(l)["user"] for l in open(f"/training/mh_build/proc/{d}/model_requests.jsonl")]
def split(u):
    lines = u.split("\n"); out = {}
    for l in lines:
        if "t_dec_ms=" in l: out["t_dec"] = int(re.search(r"t_dec_ms=(\d+)", l).group(1))
    # market identity: curve reserves line is state not identity; use creator/mint-free key = sorted set of (v_tokens at t) -- not stable. Use first-seen order per mint via 'n_prior_trades' no.
    return out
# which fields vary between same-market requests? group by the 'CURVE STATE' k constant: curve_k identifies the curve instance within the run
def key(u):
    m = re.search(r"curve_k=(\d+)", u)
    return m.group(1) if m else None
def fields(u):
    d = {}
    for tok in re.findall(r"([a-z_0-9]+)=([^\s]+)", u): d[tok[0]] = tok[1]
    return d
a, b = req(sys.argv[1]), req(sys.argv[2])
# first request per market (by curve_k)
def firsts(r):
    f = {}
    for u in r:
        if u.startswith("DECISION CLOCK"):
            f.setdefault(key(u), u)
    return f
fa, fb = firsts(a), firsts(b)
print("markets asked: A", len(fa), "B", len(fb), "common", len(set(fa) & set(fb)))
volatile = collections.Counter(); n = 0
for k in set(fa) & set(fb):
    x, y = fields(fa[k]), fields(fb[k]); n += 1
    for f in set(x) | set(y):
        if x.get(f) != y.get(f): volatile[f] += 1
print("per-field difference count over", n, "common markets (first request each):"); print(volatile.most_common(25))
