import os, subprocess, sys, shutil
sys.path.insert(0, "/training/mh_build/proc")
from harness import BIN, Run, free_port
res = {}
run0 = Run("/training/mh_build/proc/boundary_ws"); wp = free_port(); run0.spawn("ws", ["python3", "/training/mh_build/proc/stub_ws.py", str(wp)]); import time; time.sleep(1)
for name, extra in [("boundary_noflag", {}), ("boundary_badvalue", {"PQ_OFFLINE_PAPER_REPLAY": "1", "PQ_FLOW_RESUME_MS": "12abc"}),
                    ("boundary_future", {"PQ_OFFLINE_PAPER_REPLAY": "1", "PQ_FLOW_RESUME_MS": "99999999999999"}),
                    ("boundary_live", {"PQ_OFFLINE_PAPER_REPLAY": "1", "PQ_FLOW_RESUME_MS": "1788965347168"})]:
    d = f"/training/mh_build/proc/{name}"; shutil.rmtree(d, ignore_errors=True); os.makedirs(d + "/data")
    env = dict(os.environ, PQ_MODEL_ENDPOINT="http://127.0.0.1:1", PQ_MODEL_SAFETY_FILE=d + "/data/safety.json", PQ_MODEL_HELD_FILE=d + "/data/held.json",
               PQ_MODEL_MISSING_HISTORY_FILE=d + "/data/mh.json", PQ_FLOW_HISTORY_FILE=d + "/data/flow.ckpt", HELIUS_WS_URL=f"ws://127.0.0.1:{wp}", PUMPPORTAL_WS_URL=f"ws://127.0.0.1:{wp}", HELIUS_API_KEY="x",
               LASERSTREAM_ENDPOINT="http://127.0.0.1:1", PQ_LASERSTREAM_BIN="/bin/true")
    env.update(extra); args = [BIN]
    if name == "boundary_live": args += ["--live", "--wallet-address", "x"]; env.pop("PQ_MODEL_ENDPOINT")
    if name == "boundary_noflag": env["PQ_FLOW_RESUME_MS"] = "1788965347168"
    try:
        p = subprocess.run(args, cwd=d, env=env, capture_output=True, text=True, timeout=25)
        rc, so = p.returncode, p.stderr + p.stdout
    except subprocess.TimeoutExpired as e:
        rc, so = "RUNNING(timeout 25s: NOT refused)", (e.stderr or b"").decode() + (e.stdout or b"").decode()
    class P: pass
    p = P(); p.returncode = rc; p.stderr = so; p.stdout = ""
    msg = [l for l in (p.stderr + p.stdout).splitlines() if "FATAL" in l or "refused" in l][:1]
    res[name] = (p.returncode, msg[0][:200] if msg else "")
for k, v in res.items(): print(k, v)

run0.teardown()
