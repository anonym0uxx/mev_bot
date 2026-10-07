import json, subprocess, sys, os
P = "/training/mh_build/proc"
rec = json.load(open(P + "/cont_recv.json"))
def run(name, start, end, ckpt=None, resume=None, extra=()):
    cmd = ["python3", P + "/run_fixture.py", name, "--start", str(start), "--end", str(end), "--max-s", "600"] + list(extra)
    if ckpt: cmd += ["--ckpt-from", ckpt]
    if resume: cmd += ["--resume", str(resume)]
    out = subprocess.run(cmd, cwd=P, capture_output=True, text=True, timeout=900).stdout
    return out
def hdr(name):
    out = subprocess.run(["python3", P + "/ckpt_hdr.py", f"{P}/{name}/data/flow.ckpt"], capture_output=True, text=True).stdout.strip()
    return out
steps = sys.argv[1:]
log = open(P + "/recovery_matrix.log", "a")
def say(*a):
    s = " ".join(str(x) for x in a); print(s, flush=True); log.write(s + "\n"); log.flush()
if "A" in steps:   # crash AFTER checkpoint publication at cutoff line 80000
    say("A: run lines 1..80000, flush, SIGKILL"); say(run("rA", 1, 80000, extra=["--flush", "--sigkill-after-flush"])[-300:]); say(hdr("rA"))
if "B" in steps:   # restart from A's checkpoint, COMPLETE OVERLAP: replay 70001..159532 (cursor 80000 is inside the overlap)
    say("B: restart from rA ckpt, overlap replay lines 70001..159532, resume clock = recv of line 70001", rec[70000])
    say(run("rB", 70001, 159532, ckpt=P + "/rA/data/flow.ckpt", resume=rec[70000], extra=["--flush", "--sigkill-after-flush"])[-400:]); say(hdr("rB"))
if "C" in steps:   # crash BEFORE any publication: killed with no flush; restart replays the whole segment (the only complete overlap)
    say("C: run lines 1..60000, SIGKILL with NO flush (checkpoint published only by the periodic writer, if at all)")
    say(run("rC", 1, 60000, extra=["--sigkill-no-flush"])[-300:]); say(hdr("rC"))
if "C2" in steps:
    say("C2: restart from rC ckpt (whatever was published), replay lines 1..159532, resume = recv of line 1", rec[0])
    say(run("rC2", 1, 159532, ckpt=P + "/rC/data/flow.ckpt", resume=rec[0], extra=["--flush", "--sigkill-after-flush"])[-400:]); say(hdr("rC2"))
if "D" in steps:   # unavailable interval: checkpoint at line 20000, resume at line 150001 (hole > 60 s MAX_BRIDGE_MS)
    say("D1: run lines 1..20000, flush, SIGKILL"); say(run("rD1", 1, 20000, extra=["--flush", "--sigkill-after-flush"])[-300:]); say(hdr("rD1"))
    say("D2: restart from rD1 ckpt with NO overlap: replay 150001..159532, resume=recv of line 150001", rec[150000], "hole_ms", rec[150000] - rec[19999])
    say(run("rD2", 150001, 159532, ckpt=P + "/rD1/data/flow.ckpt", resume=rec[150000], extra=["--max-s", "200"])[-700:]); say(hdr("rD2"))
