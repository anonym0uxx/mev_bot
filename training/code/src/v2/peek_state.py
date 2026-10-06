import json
d = json.loads(open("/training/v2/canonical/ledger_v7/states.jsonl").readline())
for k, v in d.items():
    if isinstance(v, dict):
        print(f"{k}: {list(v.keys())}")
    else:
        print(f"{k}: {v!r}")
print("\n--- identity ---")
print(json.dumps(d["identity"], indent=1)[:600])
print("--- decision_clock ---")
print(json.dumps(d["decision_clock"], indent=1)[:600])
print("--- sample ---")
print(json.dumps(d["identity"], indent=1))
