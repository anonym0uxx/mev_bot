#!/usr/bin/env python
"""check_deploy_notional - fail if any module disagrees about the trade size.

WHY: the cost floor is in bps but its dominant term is a FIXED lamport tx fee, so
its bp value scales as 1/notional (p99 congestion is 10.05 bp/leg at 1.0 SOL but
100.5 bp/leg at 0.1 SOL). Two components quietly picking different deploy sizes
mis-state cost by a large factor and nothing catches it. The canonical value lives
in exit_mechanics.DEPLOY_SOL_CANONICAL; this check makes drift or a duplicate
definition fail loudly.

HOW (and why not a regex): a text scan over source lines cannot tell code from a
docstring, and it cannot see the shape of the site it matched. It reported
economic_exam_c3.py:19 (a docstring sentence quoting the OLD 0.5) as a conflict
while a rewritten literal would pass silently. This check parses the AST and reads
real call sites: keyword arguments (deploy_sol=1.0 inside a call - where the real
sites are), module-level and local assignments, dict values, and function
defaults. Matches inside strings or comments cannot occur by construction.

FINDING CLASSES
  CONFLICT     a literal that disagrees with DEPLOY_SOL_CANONICAL  -> FAIL
  DUPLICATE    a module-level `DEPLOY_SOL = <literal>` outside the canonical
               module: not drift today, but the thing that drifts tomorrow
               (reward_engine.py used to carry `= 0.5` exactly this way) -> FAIL
  INFO         a literal equal to the canonical (redundant but harmless)

Exit codes: 0 = consistent, 2 = FAIL (conflict or duplicate). --self-test runs the
scanner against planted fixtures and asserts it catches a kwargs conflict and a
duplicate while ignoring a docstring - i.e. it proves the guard can fail.
"""
from __future__ import annotations

import argparse
import ast
import json
import os
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from exit_mechanics import DEPLOY_SOL_CANONICAL  # noqa: E402

# The notional package root: .../src/v2 (rl/ is a subdir). Scanning only the rl
# directory would let a copy live in a sibling package unexamined.
PKG_ROOT = os.path.dirname(HERE)
SKIP_DIRS = {"__pycache__", ".git", ".ipynb_checkpoints"}

NAMES = {"deploy_sol", "notional_sol", "DEPLOY_SOL", "DEPLOY"}
CANON_NAME = "DEPLOY_SOL_CANONICAL"
CANON_MODULE = "exit_mechanics.py"
SELF = os.path.basename(__file__)

# Deliberate non-canonical notional probes. exit_mechanics._self_check exists to
# PROVE the floor is size-dependent ("fixed_cost_is_size_dependent"), which is
# impossible without calling the floor at a different size. Keyed by
# (basename, enclosing function) so the exemption is exactly this harness - a
# literal anywhere else in that file still fails.
ALLOW = {
    ("exit_mechanics.py", "_self_check"):
        "size-dependence probe: the test's whole point is a non-canonical notional",
}


def _num(node) -> float | None:
    """Numeric literal value of a node, or None (a Name/Call is not a literal)."""
    if isinstance(node, ast.Constant) and isinstance(node.value, (int, float)) \
            and not isinstance(node.value, bool):
        return float(node.value)
    if isinstance(node, ast.UnaryOp) and isinstance(node.op, ast.USub):
        inner = _num(node.operand)
        return None if inner is None else -inner
    return None


def _target_names(node) -> list[str]:
    if isinstance(node, ast.Name):
        return [node.id]
    if isinstance(node, ast.Attribute):
        return [node.attr]
    if isinstance(node, (ast.Tuple, ast.List)):
        out = []
        for e in node.elts:
            out += _target_names(e)
        return out
    return []


class _Scanner(ast.NodeVisitor):
    def __init__(self, fname: str):
        self.fname = fname                    # display path (relative to root)
        self.base = os.path.basename(fname)   # ALLOW / canonical-module keying
        self.funcs: list[str] = []
        self.sites: list[dict] = []      # every literal-valued notional site
        self.duplicates: list[dict] = []  # module-level literal definition

    # --- enclosing-function tracking so ALLOW can be scoped to a harness -------
    def _visit_func(self, node):
        self.funcs.append(node.name)

        defs = {}
        a = node.args
        # defaults bind to the LAST N positional params, not the first N - zipping
        # from the left silently invents values for the wrong parameters.
        pos = a.posonlyargs + a.args
        if a.defaults:
            for arg, dflt in zip(pos[len(pos) - len(a.defaults):], a.defaults):
                defs[arg.arg] = dflt
        for arg, dflt in zip(a.kwonlyargs, a.kw_defaults):
            if dflt is not None:
                defs[arg.arg] = dflt
        for pname, dflt in defs.items():
            if pname in NAMES:
                v = _num(dflt)
                if v is not None:
                    self._add(dflt, pname, v, "default")
        self.generic_visit(node)
        self.funcs.pop()

    visit_FunctionDef = visit_AsyncFunctionDef = _visit_func

    # --- call sites: where the real deploy notional lives ----------------------
    def visit_Call(self, node):
        for kw in node.keywords:
            if kw.arg in NAMES:
                v = _num(kw.value)
                if v is not None:
                    self._add(kw.value, kw.arg, v, "kwarg")
        self.generic_visit(node)

    def visit_Dict(self, node):
        for k, v in zip(node.keys, node.values):
            if isinstance(k, ast.Constant) and isinstance(k.value, str) and k.value in NAMES:
                nv = _num(v)
                if nv is not None:
                    self._add(v, k.value, nv, "dict")
        self.generic_visit(node)

    def visit_Assign(self, node):
        self._assign(node.targets, node.value, top=not self.funcs)
        self.generic_visit(node)

    def visit_AnnAssign(self, node):
        self._assign([node.target], node.value, top=not self.funcs)
        self.generic_visit(node)

    def _assign(self, targets, value, top: bool):
        for t in targets:
            for nm in _target_names(t):
                if nm not in NAMES:
                    continue
                v = _num(value)
                if v is None:
                    continue          # DEPLOY_SOL = DEPLOY_SOL_CANONICAL -> fine
                self._add(value, nm, v, "assign")
                if top:
                    self.duplicates.append({
                        "file": self.fname, "base": self.base,
                        "name": nm, "value": v, "line": value.lineno,
                    })

    def _add(self, node, name, val, kind):
        self.sites.append({
            "file": self.fname, "line": node.lineno, "name": name,
            "value": val, "kind": kind, "base": self.base,
            "func": self.funcs[-1] if self.funcs else None,
        })


def scan_py(path: str, fname: str) -> tuple[list[dict], list[dict]]:
    """fname is the path as it should be REPORTED (relative to the scan root);
    keying against the allow-list uses the basename."""
    try:
        src = open(path, encoding="utf-8").read()
        tree = ast.parse(src, filename=fname)
    except (OSError, SyntaxError):
        return [], []
    sc = _Scanner(fname)
    sc.visit(tree)
    if sc.base == CANON_MODULE:
        # the canonical definition itself is the one legitimate literal
        sc.duplicates = [d for d in sc.duplicates if d["name"] != CANON_NAME]
    return sc.sites, sc.duplicates


def scan_tree(root: str) -> tuple[list[dict], list[dict], list[str]]:
    sites, dups, scanned = [], [], []
    for dp, dns, fns in os.walk(root):
        dns[:] = [d for d in dns if d not in SKIP_DIRS]
        for fn in sorted(fns):
            if fn.endswith(".py") and fn != SELF:
                rel = os.path.relpath(os.path.join(dp, fn), root)
                scanned.append(rel)
                s, d = scan_py(os.path.join(dp, fn), rel)
                sites += s
                dups += d
    return sites, dups, scanned


def scan_json(root: str) -> list[dict]:
    """Numeric values under a deploy/notional KEY in a config. A note string that
    merely mentions deploy_sol=1.0 is text, not a size, and is ignored."""
    out = []
    for dp, dns, fns in os.walk(root):
        dns[:] = [d for d in dns if d not in SKIP_DIRS]
        for fn in sorted(fns):
            if not fn.endswith(".json"):
                continue
            path = os.path.join(dp, fn)
            try:
                data = json.load(open(path, encoding="utf-8"))
            except (OSError, ValueError):
                continue
            stack = [("", data)]
            while stack:
                prefix, cur = stack.pop()
                if isinstance(cur, dict):
                    for k, v in cur.items():
                        key = f"{prefix}.{k}" if prefix else str(k)
                        if str(k) in NAMES and isinstance(v, (int, float)) \
                                and not isinstance(v, bool):
                            out.append({"file": os.path.relpath(path, root),
                                        "key": key, "value": float(v)})
                        stack.append((key, v))
                elif isinstance(cur, list):
                    for i, v in enumerate(cur):
                        stack.append((f"{prefix}[{i}]", v))
    return out


def evaluate(sites, dups, jsons, canonical) -> tuple[list, list, list, list]:
    conflicts, infos, allowed = [], [], []
    for s in sites:
        base = s.get("base", s["file"])
        if abs(s["value"] - canonical) <= 1e-12:
            infos.append(s)
        elif (base, s["func"]) in ALLOW:
            allowed.append(s)
        else:
            conflicts.append(s)
    for j in jsons:
        (conflicts if abs(j["value"] - canonical) > 1e-12 else infos).append(
            {"file": j["file"], "line": 0, "name": j["key"], "value": j["value"],
             "kind": "json", "func": None})
    return conflicts, infos, allowed, dups


def report(sites, dups, jsons, canonical) -> int:
    conflicts, infos, allowed, live_dups = evaluate(sites, dups, jsons, canonical)
    print(f"canonical DEPLOY_SOL_CANONICAL = {canonical} SOL")
    print(f"scanned {len(sites)} code site(s), {len(jsons)} config value(s)")
    print(f"INFO - literal == canonical ({len(infos)}):")
    for s in sorted(infos, key=lambda x: (x["file"], x["line"])):
        print(f"    {s['file']}:{s['line']} {s['kind']} {s['name']}={s['value']}")
    if allowed:
        print(f"ALLOWED - deliberate non-canonical probe ({len(allowed)}):")
        for s in allowed:
            why = ALLOW[(s.get("base", s["file"]), s["func"])]
            print(f"    {s['file']}:{s['line']} {s['name']}={s['value']} "
                  f"[{s['func']}] - {why}")
    if conflicts:
        print(f"CONFLICTS ({len(conflicts)}) - these disagree with the canonical notional:")
        for s in sorted(conflicts, key=lambda x: (x["file"], x["line"])):
            loc = f"{s['file']}:{s['line']}" if s["line"] else f"{s['file']} ({s['name']})"
            print(f"    {loc} {s['kind']} {s['name']}={s['value']}")
    if live_dups:
        print(f"DUPLICATE DEFINITIONS ({len(live_dups)}) - import the canonical instead:")
        for d in sorted(live_dups, key=lambda x: (x["file"], x["line"])):
            print(f"    {d['file']}:{d['line']} {d['name']} = {d['value']}")
    if conflicts or live_dups:
        print("RESULT: FAIL - align them to DEPLOY_SOL_CANONICAL")
        return 2
    print("RESULT: PASS - no module disagrees about the trade size, "
          "and no duplicate definition can drift")
    return 0


def self_test() -> int:
    """Negative control: the guard must FAIL on a real kwargs site and must NOT
    fire on a docstring that mentions the value."""
    with tempfile.TemporaryDirectory() as td:
        src = {
            # the case the old regex-style reasoning was about
            "kwarg_conflict.py":
                "def f(**k):\n    return k\n\n"
                "def go(tape):\n    return f(deploy_sol=0.7)\n",
            # a mention in prose must not be a finding
            "docstring_only.py":
                '"""Old behaviour was deploy_sol=0.5 everywhere."""\n'
                "def g():\n    return 1\n",
            # dict + duplicate module-level definition
            "dict_and_dup.py":
                'D = {"notional_sol": 0.7}\n\n'
                "DEPLOY_SOL = 0.4\n",
            # matching literal: INFO, must pass
            "matching.py":
                "def h(tape):\n    return tape(deploy_sol=1.0)\n",
            # regression: positional defaults bind to the LAST N params, so a
            # numeric default on a LATER, unrelated param must not be read as a
            # deploy size (zip-from-left invented deploy_sol=30000.0 here).
            "default_align.py":
                "def g(tape, t, a, engine=None, deploy_sol=DEPLOY_SOL,\n"
                "      interval_ms=30_000, min_hold_ms=60_000):\n"
                "    return 1\n",
        }
        for fn, body in src.items():
            open(os.path.join(td, fn), "w", encoding="utf-8").write(body)

        sites, dups, _ = scan_tree(td)
        conflicts, infos, _allowed, _ = evaluate(
            sites, dups, [], DEPLOY_SOL_CANONICAL)
        got = {(c["file"], c["kind"], c["name"], c["value"]) for c in conflicts}
        want = {
            ("kwarg_conflict.py", "kwarg", "deploy_sol", 0.7),
            ("dict_and_dup.py", "dict", "notional_sol", 0.7),
            ("dict_and_dup.py", "assign", "DEPLOY_SOL", 0.4),
        }
        dup_want = {("dict_and_dup.py", "DEPLOY_SOL", 0.4)}
        dup_got = {(d["file"], d["name"], d["value"]) for d in dups}
        doc_leak = [s for s in sites if s["file"] == "docstring_only.py"]
        align_leak = [s for s in sites if s["file"] == "default_align.py"]
        info_got = {(s["file"], s["name"]) for s in infos}

        ok = True
        if got != want:
            print(f"SELF-TEST FAIL: conflicts {got} != {want}")
            ok = False
        if dup_got != dup_want:
            print(f"SELF-TEST FAIL: duplicates {dup_got} != {dup_want}")
            ok = False
        if doc_leak:
            print(f"SELF-TEST FAIL: docstring was scanned as code: {doc_leak}")
            ok = False
        if align_leak:
            print(f"SELF-TEST FAIL: default/arg mis-alignment: {align_leak}")
            ok = False
        if info_got != {("matching.py", "deploy_sol")}:
            print(f"SELF-TEST FAIL: INFO set {info_got}")
            ok = False
        if ok:
            print("SELF-TEST PASS: kwargs conflict, dict conflict and duplicate "
                  "definition all caught; docstring ignored; matching literal INFO")
            return 0
        return 1


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--root", default=PKG_ROOT,
                    help="package root to scan (default: .../src/v2)")
    ap.add_argument("--self-test", action="store_true",
                    help="run the planted-fixture negative control and exit")
    a = ap.parse_args()
    if a.self_test:
        return self_test()
    sites, dups, scanned = scan_tree(a.root)
    jsons = scan_json(a.root)
    print(f"root = {a.root}  ({len(scanned)} module(s) parsed)")
    return report(sites, dups, jsons, DEPLOY_SOL_CANONICAL)


if __name__ == "__main__":
    sys.exit(main())
