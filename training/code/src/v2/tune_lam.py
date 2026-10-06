import subprocess, json, sys
for lam in ["0.5", "2.0", "8.0", "32.0"]:
    r = subprocess.run(["/home/alon/qwen27b-venv/bin/python",
                        "/training/v2/code/src/v2/build_replay_v2.py",
                        "--out", f"/tmp/replay_lam_{lam}",
                        "--splits", "train",
                        "--limit", "2000",
                        "--lam", lam],
                       capture_output=True, text=True)
    print(f"--- lam={lam} ---")
    for line in (r.stdout or "").splitlines():
        if "actions" in line or "records=" in line:
            print(line[:400])
    if r.returncode != 0:
        print("ERR", (r.stderr or "")[-300:])
