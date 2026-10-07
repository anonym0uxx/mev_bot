import json, glob, datetime
files = sorted(glob.glob("/training/mh_build/wire/%s/wire_*.ndjson" % __import__("sys").argv[1]))
mx = 0; mxs = 0; prev = None; ps = None; n = 0; first = last = None; slots = set(); lanes = {}
for f in files:
    for l in open(f):
        v = json.loads(l); t = v["recv_unix_ms"]; s = v.get("slot")
        n += 1; first = first or t; last = t
        lanes[v.get("lane")] = lanes.get(v.get("lane"), 0) + 1
        if prev is not None: mx = max(mx, t - prev)
        prev = t
        if s is not None:
            if ps is not None and s >= ps: mxs = max(mxs, s - ps)
            ps = s; slots.add(s)
sl = sorted(slots)
print(__import__("sys").argv[1], "lines", n, "recv_span_s", (last - first) / 1000, "max_recv_gap_ms", mx, "max_slot_step", mxs,
      "slot_range", sl[0], sl[-1], "distinct_slots", len(sl), "lanes", lanes)
