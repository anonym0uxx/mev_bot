import json, hashlib, os

CON = "/training/code/qwen27b/LAUNCH_CONTRACT_SFT_V2.json"


def sha(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


con = json.load(open(CON))
updated = []


def walk(node, where):
    if isinstance(node, dict):
        if isinstance(node.get("path"), str) and isinstance(node.get("sha256"), str):
            p = node["path"]
            if os.path.isfile(p):
                h = sha(p)
                if h != node["sha256"]:
                    updated.append((where, p))
                    node["sha256"] = h
        for k, v in node.items():
            walk(v, f"{where}.{k}" if where else k)
    elif isinstance(node, list):
        for i, v in enumerate(node):
            walk(v, f"{where}[{i}]")


walk(con, "")
json.dump(con, open(CON, "w"), indent=1)
print("updated pins:", len(updated))
for w, p in updated:
    print("  ", w, "->", p)
print("contract_sha256=" + sha(CON))
