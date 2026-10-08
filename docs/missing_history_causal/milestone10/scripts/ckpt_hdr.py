import json, sys
def hdr(p):
    h = json.loads(open(p, "rb").readline())
    cur = h["cursors"]
    hw = {k: v["high_water_ms"] for k, v in cur.items()} if isinstance(cur, dict) else [c.get("high_water_ms") for c in cur]
    return dict(payload_sha256=h["payload_sha256"][:16], payload_len=h["payload_len"], hw=hw, segments=h["coverage"]["segments"], gaps=h["coverage"]["gaps"],
                late=len(h["late"]), prov=h["provenance"]["seed_source"])
for p in sys.argv[1:]:
    try: print(p.split("/")[-3], hdr(p))
    except Exception as e: print(p, "ERR", e)
