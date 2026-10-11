"""Owned-run harness for the real pq-daemon. Every child is started in the run's own session, recorded in
<run>/manifest.json with (pid, start_ticks, cmdline), ports are OS-assigned, and teardown (success, failure,
interrupt, atexit) signals only processes whose identity still matches the manifest. No pkill-by-name anywhere.
"""
import os, sys, json, time, signal, socket, subprocess, atexit, uuid

BIN = "/training/mh_build/target/debug/pq-daemon"
PROC = "/training/mh_build/proc"


def free_port():
    s = socket.socket(); s.bind(("127.0.0.1", 0)); p = s.getsockname()[1]; s.close(); return p


def start_ticks(pid):
    try:
        st = open(f"/proc/{pid}/stat").read()
        return int(st[st.rindex(")") + 2:].split()[19])
    except Exception:
        return None


class Run:
    def __init__(self, rundir, name=None):
        self.dir = os.path.abspath(rundir); os.makedirs(self.dir + "/data", exist_ok=True)
        self.id = (name or "run") + "-" + uuid.uuid4().hex[:8]
        self.procs = {}   # role -> dict(pid,start,cmd,pgid)
        self.ports = {}
        self._write()
        atexit.register(self.teardown)
        for sg in (signal.SIGINT, signal.SIGTERM):
            signal.signal(sg, lambda *_: (self.teardown(), sys.exit(130)))

    def _write(self):
        json.dump(dict(run_id=self.id, owner_pid=os.getpid(), dir=self.dir, ports=self.ports, procs=self.procs),
                  open(self.dir + "/manifest.json", "w"), indent=1)

    def spawn(self, role, argv, env=None, stdout=None, stderr=None):
        e = dict(os.environ); e.update(env or {}); e["PQ_RUN_ID"] = self.id
        p = subprocess.Popen(argv, env=e, cwd=self.dir, stdout=stdout, stderr=stderr, start_new_session=True)
        time.sleep(0.05)
        self.procs[role] = dict(pid=p.pid, start=start_ticks(p.pid), cmd=" ".join(argv)[:160], pgid=p.pid, popen=None)
        self._popens = getattr(self, "_popens", {}); self._popens[role] = p
        self._write()
        return p

    def alive(self, role):
        m = self.procs[role]
        return start_ticks(m["pid"]) == m["start"] and m["start"] is not None and self._popens[role].poll() is None

    def stop(self, role, grace=10.0):
        """graceful (caller-driven) is the caller's job; here: SIGTERM then bounded wait then SIGKILL, identity rechecked."""
        m = self.procs.get(role)
        if not m or start_ticks(m["pid"]) != m["start"]:
            return "gone"
        p = self._popens[role]
        os.killpg(m["pgid"], signal.SIGTERM) if self._group_ours(m) else os.kill(m["pid"], signal.SIGTERM)
        t0 = time.time()
        while time.time() - t0 < grace and p.poll() is None:
            time.sleep(0.1)
        if p.poll() is None and start_ticks(m["pid"]) == m["start"]:
            (os.killpg(m["pgid"], signal.SIGKILL) if self._group_ours(m) else os.kill(m["pid"], signal.SIGKILL))
            p.wait(timeout=5); return "killed"
        return "terminated"

    def _group_ours(self, m):
        # a process group is signalled only if every member carries this run's id in its environment
        mem = []
        for d in os.listdir("/proc"):
            if d.isdigit():
                try:
                    st = open(f"/proc/{d}/stat").read(); pg = int(st[st.rindex(")") + 2:].split()[2])
                    if pg == m["pgid"]:
                        mem.append(d)
                except Exception:
                    pass
        for d in mem:
            try:
                if ("PQ_RUN_ID=" + self.id).encode() not in open(f"/proc/{d}/environ", "rb").read().split(b"\0"):
                    return False
            except Exception:
                return False
        return True

    def kill_hard(self, role):
        """simulated crash of exactly this run's daemon (SIGKILL the daemon pid, identity-checked)."""
        m = self.procs[role]
        if start_ticks(m["pid"]) == m["start"]:
            os.kill(m["pid"], signal.SIGKILL); self._popens[role].wait(timeout=10)

    def teardown(self):
        for role in list(self.procs)[::-1]:
            try: self.stop(role, 5.0)
            except Exception: pass
        self._write()


def health(d):
    try: return json.load(open(f"{d}/data/daemon_health.json"))
    except Exception: return {}
