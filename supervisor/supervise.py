"""
Hermes Supervisor entrypoint.

Commands:
    health                  check the llama.cpp endpoint
    build --from M0         run the build loop from a milestone
    research                run one standing research cycle (post-build)
    status                  milestone / criteria / escalation dashboard
    pin-evaluator           HUMAN-ONLY: (re)pin the frozen evaluator hash to the current binary

Usage:
    python -m supervisor.supervise <command> [--config supervisor/config/supervisor.yaml]
"""
from __future__ import annotations

import argparse
import sys
import time
import uuid

from .core.config import SupervisorConfig
from .core.constitution import load_constitution
from .core.model_client import ModelClient, ModelUnavailable
from .store.evidence import EvidenceStore
from .console.escalate import EscalationChannel


def _load(args) -> SupervisorConfig:
    return SupervisorConfig.load(args.config)


def cmd_health(args) -> int:
    cfg = _load(args)
    client = ModelClient(cfg.model)
    try:
        h = client.health()
        print(f"[health] endpoint OK: {h}")
        return 0
    except ModelUnavailable as e:
        print(f"[health] endpoint UNAVAILABLE: {e}", file=sys.stderr)
        return 1


def cmd_build(args) -> int:
    cfg = _load(args)
    cons = load_constitution(cfg.constitution_path)
    store = EvidenceStore(cfg.evidence_db)
    run_id = f"build-{int(time.time())}-{uuid.uuid4().hex[:6]}"
    store.start_run(run_id, cons.git_hash, "supervisor-0.1", note=f"build from {args.from_milestone}")
    print(f"[build] run {run_id} | constitution {cons.content_hash} (git {cons.git_hash})")
    print(f"[build] milestones parsed: {cons.milestone_order()}")
    print(f"[build] criteria parsed: {len(cons.criteria)}")
    order = cons.milestone_order()
    if args.from_milestone in order:
        order = order[order.index(args.from_milestone):]
    print(f"[build] would execute: {order}")
    print("[build] NOTE: wiring the live VCS/exec adapter to your repo is the final integration step; "
          "the FSM, gates, reinforcement, safety, and evidence layers are complete and unit-tested.")
    store.close()
    return 0


def cmd_research(args) -> int:
    print("[research] standing research loop is scaffolded; it activates post-build once the "
          "bot's research-runner, frozen-evaluator, and QuantMemoryStore exist (TODO(live) bindings).")
    return 0


def cmd_status(args) -> int:
    cfg = _load(args)
    store = EvidenceStore(cfg.evidence_db)
    # latest run
    row = store._db.execute("SELECT run_id,started_at,constitution_hash FROM runs ORDER BY started_at DESC LIMIT 1").fetchone()
    if not row:
        print("[status] no runs yet")
        return 0
    run_id, started, chash = row
    print(f"[status] latest run {run_id} | constitution {chash}")
    esc = store.open_escalations(run_id)
    print(f"[status] open escalations: {len(esc)}")
    for e in esc:
        print(f"   - {e['milestone']}/{e['task_id']} [{e['domain']}]: {e['context'][:80]}")
    store.close()
    return 0



def cmd_pin_evaluator(args) -> int:
    """HUMAN-ONLY. Re-pin the frozen evaluator (§44) to the binary's current hash.
    Never callable via MCP; the agent cannot re-pin itself."""
    from .core.config import SupervisorConfig
    from .core import artifacts
    from .store.evidence import EvidenceStore
    import yaml
    from pathlib import Path
    cfg = SupervisorConfig.load(args.config)
    binp = cfg.evaluator_bin or (str(artifacts.discover_evaluator(cfg.repo_path) or ""))
    if not binp or not Path(binp).is_file():
        print("[pin-evaluator] no evaluator binary found; build it first", file=sys.stderr)
        return 1
    sha = artifacts.sha256_of(binp)
    confirm = input(f"Re-pin evaluator to {binp}\n  sha256={sha}\nType 'PIN' to confirm: ")
    if confirm.strip() != "PIN":
        print("[pin-evaluator] aborted")
        return 1
    data = yaml.safe_load(Path(args.config).read_text(encoding="utf-8")) or {}
    data["evaluator_bin"] = binp
    data["evaluator_pinned_sha256"] = sha
    Path(args.config).write_text(yaml.safe_dump(data, sort_keys=False), encoding="utf-8")
    store = EvidenceStore(cfg.evidence_db)
    store.register_artifact("evaluator", binp, sha, "human-repin", "")
    store.pin_evaluator(sha)
    print(f"[pin-evaluator] pinned {sha}")
    return 0

HUMAN_IDENTITIES = frozenset({"operator", "human", "owner", "alon", "me", "admin"})


def _human_attestation(who: str, skip_prompt: bool) -> "tuple[bool, str, str]":
    """Decide how an attestation may be RECORDED. Returns (ok, who_to_record, error).

    "HUMAN-ONLY" on these verbs is a SURFACE restriction, not an identity check: they are
    absent from the MCP tool surface (supervisor/mcp/server.py exposes 16 tools; pin_manifest,
    pin_evaluator and amendment approve/apply are not among them), so an agent working through
    its sanctioned tools cannot reach them by any prompt path. But nothing in this process can
    distinguish a human at a keyboard from an agent in a shell. Therefore the RECORD must not
    be permitted to claim more than was actually established:

      * --who has no default. A default of "operator" made a flawless-looking human
        attestation the path of least resistance for an unattended caller.
      * A human identity may only be claimed when a human was actually asked -- stdin is a TTY
        and --yes was not passed. Otherwise the identity is refused, and a non-human attester
        is recorded with an explicit [non-interactive] marker.

    The pin is not the security boundary; live machine measurement is, and that cannot be
    forged by editing a file. The pin's only job is to make a post-pin edit to the declaration
    fail closed as tampering -- a job that is worthless if whatever edits the block can also
    re-pin it. That, and not the species of the typist, is why these stay off the tool surface.
    """
    who = (who or "").strip()
    if not who:
        return (False, "", "--who is required and has no default. Record the attester "
                           "truthfully: 'operator' if you are a human answering the prompt "
                           "below, otherwise the agent name and who delegated it, e.g. "
                           "--who 'hermes-agent (delegated by Alon 2026-07-30)'.")
    if _interactive(skip_prompt):
        return (True, who, "")
    if who.lower() in HUMAN_IDENTITIES:
        return (False, who,
                f"REFUSED: --who '{who}' claims a human attestation, but no human was asked "
                f"(stdin is not a TTY, or --yes was passed). Either drop --yes and run this in "
                f"a real console, or record the caller truthfully.")
    return (True, f"{who} [non-interactive]", "")


def _interactive(skip_prompt: bool) -> bool:
    try:
        tty = bool(sys.stdin.isatty())
    except Exception:  # noqa: BLE001
        tty = False
    return tty and not skip_prompt


def cmd_amendments(args) -> int:
    """HUMAN-ONLY amendment review. Deliberately CLI-only: `approve` and `apply` are
    absent from the MCP tool surface, so no model can reach them by any prompt path."""
    from .core.config import SupervisorConfig
    from .store.evidence import EvidenceStore
    from .core.amendment import apply_amendment

    cfg = SupervisorConfig.load(args.config)
    store = EvidenceStore(cfg.evidence_db)

    if args.action in ("approve", "reject", "apply"):
        ok, who_rec, err = _human_attestation(args.who, args.yes)
        if not ok:
            print(err)
            store.close()
            return 1
        args.who = who_rec

    if args.action == "list":
        items = store.list_amendments(args.state or "")
        if not items:
            print("no amendments" + (f" in state '{args.state}'" if args.state else ""))
            return 0
        for a in items:
            print(f"#{a['id']:<4} [{a['state']:<8}] {a['kind']:<14} {a['title'][:60]}")
            print(f"      evidence: {a['evidence_ref']}   by: {a['proposed_by']}")
        return 0

    if args.action == "show":
        a = store.get_amendment(args.id)
        if not a:
            print(f"no amendment {args.id}")
            return 1
        for k in ("id", "state", "kind", "title", "rationale", "evidence_ref",
                  "proposed_by", "target_hint", "decided_by", "note"):
            print(f"{k:>14}: {a[k]}")
        print("\n--- drafted text ---\n")
        print(a["diff_text"] or "(not drafted yet)")
        return 0

    if args.action == "reject":
        print(store.reject_amendment(args.id, human=args.who, why=args.why or "rejected"))
        return 0

    if args.action == "approve":
        a = store.get_amendment(args.id)
        if not a:
            print(f"no amendment {args.id}")
            return 1
        if a["state"] != "drafted":
            print(f"amendment {args.id} is '{a['state']}'; only a drafted amendment can be "
                  "approved (run draft_amendment first)")
            return 1
        print("\n--- text you are approving ---\n")
        print(a["diff_text"])
        print("\n--- end ---\n")
        if not args.yes:
            resp = input(f"Approve amendment #{args.id} as '{args.who}'? [y/N] ").strip().lower()
            if resp != "y":
                print("not approved")
                return 1
        print(store.approve_amendment(args.id, human=args.who))
        print("Now apply it at a milestone boundary: "
              f"hermes-supervise amendments apply --id {args.id} --file <edited-constitution>")
        return 0

    if args.action == "apply":
        a = store.get_amendment(args.id)
        if not a:
            print(f"no amendment {args.id}")
            return 1
        if a["state"] != "approved":
            print(f"amendment {args.id} is '{a['state']}'; only an APPROVED amendment may be "
                  "applied")
            return 1
        if not args.file:
            print("--file is required: the full candidate constitution with the approved text "
                  "already merged in. Applying is validated, atomic, and backed up; it is not "
                  "a blind patch.")
            return 1
        candidate = Path(args.file).read_text(encoding="utf-8", errors="replace")
        rep = apply_amendment(cfg.constitution_path, candidate, dry_run=args.dry_run)
        print(("DRY RUN " if args.dry_run else "") + ("OK: " if rep.ok else "REFUSED: ")
              + rep.reason)
        for k, v in rep.checks.items():
            print(f"    {k}: {v}")
        if rep.ok and not args.dry_run:
            store.mark_amendment_applied(args.id, rep.new_hash)
            print(f"    backup: {rep.backup_path}")
            print(f"    new sha256: {rep.new_hash[:16]}...")
            print("\nCommit the constitution now so the hash re-pins for the next run:")
            print("    git add docs/HERMES_ONE_SHOT_PROMPT.md && git commit -m "
                  f'"constitution amendment #{args.id}: {a["title"]}"')
        return 0 if rep.ok else 1

    print("unknown action")
    return 1


def cmd_pin_manifest(args) -> int:
    """HUMAN-ONLY. Pin the manifest's deployment_host declaration (§9.5, criterion 113).

    Never callable via MCP; agents can rewrite the file but cannot re-pin it, and the phase
    gate fails closed on any mismatch. Live machine measurement remains the decisive check.
    See _human_attestation() for what "HUMAN-ONLY" does and does not enforce.
    """
    from .core.config import SupervisorConfig
    from .store.evidence import EvidenceStore
    from .gates.build_phase import deployment_declaration, declaration_sha, measure_machine

    cfg = SupervisorConfig.load(args.config)
    dep = deployment_declaration(args.manifest)
    if dep is None:
        print(f"no deployment_host declaration in {args.manifest} — generate it ON THE SERVER "
              "with: python scripts/gen_manifest.py --declare-deployment-host")
        return 1

    ok, who_rec, err = _human_attestation(args.who, args.yes)
    if not ok:
        print(err)
        return 1

    sha = declaration_sha(dep)
    live = measure_machine()

    # Print the WHOLE block. declaration_sha hashes EVERY key, so the reviewer must see every
    # key: showing two fields while hashing five is how a review becomes a rubber stamp.
    print("\ndeployment_host declaration being pinned "
          "(every line below is an input to the hash):")
    for k in sorted(dep):
        print(f"    {k:<14} = {dep[k]}")
    print(f"\n  declaration sha : {sha}")
    print(f"  live machine    : {live['machine_id']} ({live['id_source']})")
    print(f"  live cpu        : {live['cpu_model'] or '?'}")

    prev = ""
    try:
        _s = EvidenceStore(cfg.evidence_db)
        prev = _s.get_pinned_manifest()
        _s.close()
    except Exception:  # noqa: BLE001
        pass
    if prev and prev != sha:
        print(f"  SUPERSEDES pin  : {prev[:16]}...  (this is a RE-pin)")
    elif prev == sha:
        print("  (this sha is already pinned; re-running re-records the attester)")

    if live["machine_id"] != dep.get("machine_id"):
        print("\nNOTE: you are pinning from a machine that is NOT the declared deployment host. "
              "That is allowed (you may pin from the laptop), but generate the declaration on "
              "the server so its machine_id is measured, not typed.")
    if dep.get("id_source") == "hostname_fallback":
        print("\nWARNING: id_source is hostname_fallback, which the phase gate treats as "
              "spoofable. Phase B stays locked even if this pin matches.")

    if _interactive(args.yes):
        if input(f"\nPin this declaration as '{who_rec}'? [y/N] ").strip().lower() != "y":
            print("not pinned")
            return 1

    store = EvidenceStore(cfg.evidence_db)
    store.pin_manifest(sha, who_rec)
    store.close()
    print(f"pinned {sha[:16]}... by '{who_rec}' — Phase-B gates now require this exact "
          "declaration plus live machine match.")
    return 0


def main(argv=None) -> int:
    p = argparse.ArgumentParser(prog="supervise")
    p.add_argument("--config", default="supervisor/config/supervisor.yaml")
    sub = p.add_subparsers(dest="cmd", required=True)

    sub.add_parser("health")
    b = sub.add_parser("build")
    b.add_argument("--from", dest="from_milestone", default="M0")
    sub.add_parser("research")
    sub.add_parser("status")
    sub.add_parser("pin-evaluator")
    pm = sub.add_parser("pin-manifest", help="HUMAN-ONLY: pin the deployment_host declaration")
    pm.add_argument("--manifest", default="infra_manifest.json")
    pm.add_argument("--who", required=True,
                    help="who is attesting. 'operator' requires answering "
                         "the prompt on a TTY; an agent must name itself and "
                         "its delegator.")
    pm.add_argument("--yes", action="store_true")
    am = sub.add_parser("amendments", help="HUMAN-ONLY constitution amendment review "
                                            "(approve/apply are not MCP tools by design)")
    am.add_argument("action", choices=["list", "show", "approve", "reject", "apply"])
    am.add_argument("--id", type=int, default=0)
    am.add_argument("--state", default="")
    am.add_argument("--who", default="",
                    help="who is attesting (required for approve/reject/apply)")
    am.add_argument("--why", default="")
    am.add_argument("--file", default="", help="candidate constitution file (for apply)")
    am.add_argument("--dry-run", action="store_true")
    am.add_argument("--yes", action="store_true", help="skip the interactive confirmation")

    args = p.parse_args(argv)
    return {
        "health": cmd_health,
        "build": cmd_build,
        "research": cmd_research,
        "status": cmd_status,
        "pin-evaluator": cmd_pin_evaluator,
        "pin-manifest": cmd_pin_manifest,
        "amendments": cmd_amendments,
    }[args.cmd](args)


if __name__ == "__main__":
    raise SystemExit(main())
