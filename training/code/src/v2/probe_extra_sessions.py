import glob, os, subprocess, json, collections

def zcat_lines(p, cap=None):
    out = subprocess.run(["zstd", "-dc", p], capture_output=True)
    if out.returncode != 0:
        return None, out.stderr.decode()[:200]
    n = 0
    kinds = collections.Counter()
    for line in out.stdout.split(b"\n"):
        if not line.strip():
            continue
        n += 1
        if n <= 4000:
            try:
                r = json.loads(line)
                kinds[r.get("kind") or r.get("type") or "?"] += 1
            except Exception:
                kinds["unparsed"] += 1
        if cap and n >= cap:
            break
    return n, dict(kinds)

for sess in ("20260824_053543_000288", "20260910_020917_000592"):
    fs = sorted(glob.glob(f"/mnt/data/mev_bot-artifacts/raw/*{sess}*part*.zst")) or \
         sorted(glob.glob(f"/mnt/data/mev_bot-artifacts/north_star/aggregation/prospective_capture_120_v1/*{sess}*part*.zst"))
    if not fs:
        print(sess, "-> no parts found"); continue
    tot = sum(os.path.getsize(f) for f in fs)
    n, kinds = zcat_lines(fs[0], cap=20000)
    print(f"{sess}: parts={len(fs)} bytes={tot/1e9:.1f}GB  part0_lines(<=20k)={n} kinds={kinds}")
    print(f"    part0={os.path.basename(fs[0])} size={os.path.getsize(fs[0])/1e6:.1f}MB")
