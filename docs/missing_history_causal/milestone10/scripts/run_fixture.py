import sys, os, time, json, shutil, argparse
sys.path.insert(0, "/training/mh_build/proc")
from harness import Run, free_port, BIN, health
ap = argparse.ArgumentParser()
ap.add_argument("name"); ap.add_argument("--wire", default="/training/mh_build/proc/cont_wire05.ndjson")
ap.add_argument("--start", default="1"); ap.add_argument("--end", default="159532")
ap.add_argument("--seed-source", default="cold_start:segment_09_09_14_49_07")
ap.add_argument("--ckpt-from", default=None); ap.add_argument("--resume", default=None)
ap.add_argument("--keep-from", default=None, help="copy held.json etc from another run dir")
ap.add_argument("--kill-after-ckpt", default=None, choices=[None, "before", "after"])
ap.add_argument("--max-s", type=int, default=900); ap.add_argument("--hard-kill-at-tx", type=int, default=0)
ap.add_argument("--flush", action="store_true"); ap.add_argument("--sigkill-after-flush", action="store_true"); ap.add_argument("--sigkill-no-flush", action="store_true")
ap.add_argument("--model", default="/training/mh_build/proc/stub_model.py")
a = ap.parse_args()
d = f"/training/mh_build/proc/{a.name}"
if not os.path.exists(d + "/KEEP"): os.system(f"rm -rf {d}")
os.makedirs(d + "/data", exist_ok=True)
if a.ckpt_from: shutil.copy(a.ckpt_from, d + "/data/flow.ckpt")
if a.keep_from:
    for f in ("held.json", "mh.json", "safety.json"):
        if os.path.exists(f"{a.keep_from}/data/{f}"): shutil.copy(f"{a.keep_from}/data/{f}", d + f"/data/{f}")
r = Run(d); mp, wp = free_port(), free_port()
reqlog = d + "/model_requests.jsonl"
try:
    r.spawn("model", ["python3", a.model, str(mp), reqlog])
    r.spawn("ws", ["python3", "/training/mh_build/proc/stub_ws.py", str(wp)]); time.sleep(1)
    env = dict(PQ_LASERSTREAM_BIN="/training/mh_build/proc/stub_ls.sh", PQ_STUB_WIRE=a.wire, PQ_STUB_START=a.start, PQ_STUB_END=a.end, PQ_STUB_LINGER="3600",
               HELIUS_WS_URL=f"ws://127.0.0.1:{wp}", HELIUS_API_KEY="x", LASERSTREAM_ENDPOINT="http://127.0.0.1:1", PUMPPORTAL_WS_URL=f"ws://127.0.0.1:{wp}",
               PQ_MODEL_ENDPOINT=f"http://127.0.0.1:{mp}", PQ_MODEL_SAFETY_FILE=d + "/data/safety.json", PQ_MODEL_HELD_FILE=d + "/data/held.json",
               PQ_MODEL_MISSING_HISTORY_FILE=d + "/data/mh.json", PQ_FLOW_HISTORY_FILE=d + "/data/flow.ckpt",
               PQ_FLOW_SEED_SOURCE=a.seed_source, PQ_FLOW_SEED_SHA256="none", PQ_FLOW_SEED_BEFORE_MS="0", PQ_OFFLINE_PAPER_REPLAY="1")
    if a.resume:
        env["PQ_FLOW_RESUME_MS"] = a.resume
    r.spawn("daemon", [BIN, "--status-every-ticks", "4"], env=env, stdout=open(d + "/out.log", "w"), stderr=open(d + "/err.log", "w"))
    t0 = time.time(); last = (-1, time.time()); drained = False
    while time.time() - t0 < a.max_s:
        time.sleep(2)
        if not r.alive("daemon"): print("daemon exited early"); break
        h = health(d); n = os.path.getsize(d + "/data/event_stream.jsonl") if os.path.exists(d + "/data/event_stream.jsonl") else 0
        if n != last[0]: last = (n, time.time())
        if a.hard_kill_at_tx and n >= a.hard_kill_at_tx * 1000:
            print("HARD KILL at tx", n); import signal; os.kill(r.procs["daemon"]["pid"], signal.SIGKILL); time.sleep(1); break
        if n > 0 and time.time() - last[1] > 25: drained = True; break
    h = health(d)
    print("drained", drained, "event_stream_bytes", last[0], "elapsed_s", round(time.time() - t0))
    if drained and a.flush:
        open(d + "/data/FLOW_FLUSH", "w").write("1")
        for _ in range(60):
            time.sleep(1)
            if os.path.exists(d + "/data/FLOW_FLUSHED"): print("flushed", open(d + "/data/FLOW_FLUSHED").read()); break
    if drained and (a.sigkill_after_flush or a.sigkill_no_flush):
        import signal; os.kill(r.procs["daemon"]["pid"], signal.SIGKILL); time.sleep(1); print("SIGKILL daemon (flush=%s)" % a.sigkill_after_flush)
    elif drained:
        open(d + "/data/DAEMON_STOP", "w").write("stop")
        for _ in range(60):
            time.sleep(1)
            if not r.alive("daemon"): print("graceful exit"); break
finally:
    r.teardown()
os.system(f"grep -E 'flow-history|ALERT|FATAL|OFFLINE' {d}/err.log | cut -c1-260 | head -8")
print("model requests:", sum(1 for _ in open(reqlog)) if os.path.exists(reqlog) else 0)
