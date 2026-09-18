#!/usr/bin/env python3
"""Build admission gate receipts that contain evidence, not assertions.

The release manifest's `admission` block is what the launcher and trainer both
check before real training. Today it is a bare set of false booleans with no
supporting material, which is useless in both directions: it cannot show WHY a
gate is closed, and it offers nothing to check if someone flips one true.

This tool produces, per gate, exactly one of two things:

  * a PASS receipt, only when the gate's evidence can actually be computed from
    pinned artifacts and the check passes; or
  * a BLOCKED entry naming the specific missing artifact.

It never writes `true` for a gate it did not verify. It cannot invent an
`approved_by`, a calibrated economic model, or a clean evaluation holdout, so
those stay blocked with reasons instead of being quietly satisfied.
"""
import hashlib
import json
import re
import sys
from datetime import datetime, timezone
from pathlib import Path


def require(condition, message):
    if not condition:
        raise ValueError(message)

GATES = ('semantic_review', 'economic_contract', 'clean_evaluation',
         'distributed_loss_runtime', 'full_parameter_27b')

# The five gates above must all be true, plus scope/approved_by, before the
# launcher will accept a real run. Kept here so the counts cannot drift.


def sha256(path):
    h = hashlib.sha256()
    with Path(path).open('rb') as f:
        for block in iter(lambda: f.read(1 << 20), b''):
            h.update(block)
    return h.hexdigest()


def pin(path):
    path = Path(path)
    if not path.is_file():
        raise FileNotFoundError(str(path))
    return {'path': str(path), 'sha256': sha256(path), 'bytes': path.stat().st_size}


def parameter_count_from_index(index_path):
    """Sum tensor element counts from a safetensors index without loading weights.

    Reads only `weight_map` plus each shard's header, so a 55 GB seed costs
    kilobytes of I/O instead of 55 GB.
    """
    index_path = Path(index_path)
    index = json.loads(index_path.read_text(encoding='utf-8'))
    weight_map = index.get('weight_map')
    if not isinstance(weight_map, dict) or not weight_map:
        raise ValueError('index has no weight_map')
    shards = sorted(set(weight_map.values()))
    header_sizes = {}
    total = 0
    dtype_of = {}
    for shard in shards:
        header_bytes = None
        with (index_path.parent / shard).open('rb') as f:
            first = f.read(8)
            if len(first) != 8:
                raise ValueError('short safetensors header: ' + shard)
            header_len = int.from_bytes(first, 'little')
            if not 0 < header_len < 200_000_000:
                raise ValueError('implausible safetensors header: ' + shard)
            header_bytes = f.read(header_len)
        header = json.loads(header_bytes.decode('utf-8'))
        header_sizes[shard] = {k: v for k, v in header.items() if k != '__metadata__'}
    for name in weight_map:
        shard = weight_map[name]
        entry = header_sizes.get(shard, {}).get(name)
        if entry is None:
            raise ValueError('tensor missing from shard header: ' + name)
        shape = entry.get('shape')
        if not isinstance(shape, list) or not shape:
            raise ValueError('tensor without shape: ' + name)
        count = 1
        for dim in shape:
            if not isinstance(dim, int) or dim < 0:
                raise ValueError('bad shape dim for ' + name)
            count *= dim
        total += count
        dtype_of[entry.get('dtype')] = dtype_of.get(entry.get('dtype'), 0) + count
    return total, dtype_of, shards, weight_map


def full_parameter_gate(seed_dir, trainer_path, min_billions=20.0):
    """Prove the seed is a ~27B model AND the trainer leaves every param trainable."""
    seed_dir = Path(seed_dir)
    index = seed_dir / 'model.safetensors.index.json'
    if not index.is_file():
        return {'passed': False, 'reason': 'seed index not found at ' + str(index)}
    total, dtype_of, shards, weight_map = parameter_count_from_index(index)
    trainer = Path(trainer_path)
    if not trainer.is_file():
        return {'passed': False, 'reason': 'trainer not found'}
    source = trainer.read_text(encoding='utf-8', errors='replace')
    # The trainer must assert that nothing is frozen; a LoRA/partial-freeze
    # regression would otherwise silently pass a "full parameter" claim.
    grad_check = re.search(
        r"require\(\s*all\(\s*p\.requires_grad\s+for\s+p\s+in\s+model\.parameters\(\)\s*\)",
        source)
    billions = total / 1e9
    passed = billions >= min_billions and grad_check is not None
    return {
        'passed': passed,
        'reason': None if passed else (
            'seed parameter count %.3fB below %.1fB floor' % (billions, min_billions)
            if billions < min_billions else
            'trainer does not assert every parameter requires_grad'),
        'parameter_count': total,
        'parameter_billions': round(billions, 4),
        'dtypes': dtype_of,
        'shard_count': len(shards),
        'tensor_count': len(weight_map),
        'trainer_all_parameters_trainable_assertion': grad_check is not None,
        'method': 'tensor element counts summed from safetensors headers; no weights loaded',
    }


def distributed_loss_gate(runtime_evidence, require_native=False):
    """Accept only evidence that a real multi-rank weighted loss ran.

    `runtime_evidence` is a mapping loaded from a prior run report. A CPU/Gloo
    fixture proves the loss mathematics, not native DeepSpeed, so it is recorded
    as partial unless native execution is explicitly required and present.
    """
    if not isinstance(runtime_evidence, dict):
        return {'passed': False, 'reason': 'no distributed runtime evidence supplied'}
    world = runtime_evidence.get('world_size')
    ranks_ok = runtime_evidence.get('all_ranks_exit_zero') is True
    rel_err = runtime_evidence.get('weighted_loss_relative_error')
    mass_ok = isinstance(rel_err, (int, float)) and abs(rel_err) < 1e-4
    native = runtime_evidence.get('native_deepspeed_verified') is True
    passed = ranks_ok and mass_ok and world in (2, 3) and (native or not require_native)
    reasons = []
    if not ranks_ok:
        reasons.append('not all ranks exited zero')
    if not mass_ok:
        reasons.append('weighted loss mass not reconciled (relative error >= 1e-4)')
    if world not in (2, 3):
        reasons.append('world_size not a verified multi-rank run')
    if require_native and not native:
        reasons.append('native DeepSpeed execution not verified')
    return {
        'passed': passed,
        'reason': None if passed else '; '.join(reasons),
        'world_size': world,
        'weighted_loss_relative_error': rel_err,
        'native_deepspeed_verified': native,
        'scope': 'native' if native else 'cpu_fixture_only',
    }


def blocked(reason, needs):
    return {'passed': False, 'reason': reason, 'requires': needs}


def load_waivers(path):
    """Operator-authored gate waivers. Attributed, never generated here."""
    if not path or not Path(path).is_file():
        return {}
    doc = json.loads(Path(path).read_text(encoding='utf-8'))
    require(doc.get('schema') == 'north_star_gate_waivers_v1',
            'waiver document schema mismatch')
    require(bool(doc.get('authorized_by')) and bool(doc.get('directive')),
            'waiver document lacks operator authority')
    waived = {}
    for gate, entry in (doc.get('waived') or {}).items():
        require(entry.get('waived') is True, 'waiver entry not marked waived: ' + gate)
        require(bool(entry.get('authorized_by')) and bool(entry.get('rationale')),
                'waiver entry lacks authority or rationale: ' + gate)
        waived[gate] = entry
    return waived


def build(seed_dir, trainer_path, runtime_evidence_path, release_path, waivers_path=None):
    gates = {}
    waived = load_waivers(waivers_path)
    try:
        gates['full_parameter_27b'] = full_parameter_gate(seed_dir, trainer_path)
    except (OSError, ValueError) as exc:
        gates['full_parameter_27b'] = blocked('evidence computation failed: %s' % exc,
                                              ['readable seed index', 'readable trainer'])
    evidence = None
    if runtime_evidence_path and Path(runtime_evidence_path).is_file():
        evidence = json.loads(Path(runtime_evidence_path).read_text(encoding='utf-8'))
    gates['distributed_loss_runtime'] = distributed_loss_gate(evidence)

    # These three cannot be manufactured from anything on this disk. Saying so
    # explicitly is the point: a blocked gate with a named missing artifact is
    # actionable, a bare `false` is not.
    gates['clean_evaluation'] = blocked(
        'no materialised untouched evaluation holdout with verified ancestry',
        ['joint CPT/SFT/retrieval/teacher/checkpoint ancestry',
         'embargo exceeding the label horizon',
         'held-out natural class prevalence'])
    gates['semantic_review'] = blocked(
        'no completed model semantic review of the candidate corpus',
        ['frozen review contract', 'per-item dispositions', 'reviewer identity + hash pins'])
    gates['economic_contract'] = blocked(
        'no calibrated executable cost/fill/latency model',
        ['complete actual ledgers', 'measured fill residuals',
         'submit-to-land latency distribution', 'capacity/impact model'])

    # An operator waiver satisfies a gate WITHOUT claiming it passed. The receipt
    # records the operator's name and reasoning so the decision stays auditable
    # and no tool can manufacture it.
    for gate, entry in waived.items():
        if gate in gates:
            gates[gate] = {'passed': True, 'via': 'operator_waiver',
                           'authorized_by': entry['authorized_by'],
                           'rationale': entry['rationale'],
                           'granted_evidence': entry.get('granted_evidence'),
                           'not_granted': entry.get('not_granted')}

    verified = sorted(g for g in GATES if gates.get(g, {}).get('via') != 'operator_waiver'
                      and gates.get(g, {}).get('passed'))
    waived_gates = sorted(g for g in GATES if gates.get(g, {}).get('via') == 'operator_waiver')
    unresolved = sorted(g for g in GATES if not gates.get(g, {}).get('passed'))
    receipt = {
        'schema': 'north_star_admission_evidence_v1',
        'generated_utc': datetime.now(timezone.utc).isoformat(),
        'release_manifest': str(release_path),
        'release_manifest_sha256': sha256(release_path) if Path(release_path).is_file() else None,
        'gates': gates,
        'verified_gates': verified,
        'waived_gates': waived_gates,
        'unresolved_gates': unresolved,
        'all_gates_satisfied': not unresolved,
        # Deliberately absent, and naming it is not a formality: only the
        # operator can author an approval, so no tool may synthesise it.
        'approved_by': None,
        'approved_by_note': 'operator identity is not derivable from artifacts; '
                            'this tool never writes one',
        'scope_note': 'scope must be north_star_trader for real training; the '
                      'candidate release is source_support_only by construction',
        'training_authorized': False,
    }
    return receipt


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    import argparse
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--seed-dir', required=True)
    ap.add_argument('--trainer', required=True)
    ap.add_argument('--runtime-evidence', default=None)
    ap.add_argument('--release', required=True)
    ap.add_argument('--waivers', default=None)
    ap.add_argument('--out', required=True)
    args = ap.parse_args(argv)
    receipt = build(args.seed_dir, args.trainer, args.runtime_evidence, args.release,
                    args.waivers)
    Path(args.out).write_text(json.dumps(receipt, indent=2) + '\n', encoding='utf-8')
    print(json.dumps({k: receipt[k] for k in
                      ('verified_gates', 'waived_gates', 'unresolved_gates',
                       'all_gates_satisfied')}, indent=2))
    # Exit 2 when gates are unresolved so a caller cannot mistake it for ready.
    return 0 if receipt['all_gates_satisfied'] else 2


if __name__ == '__main__':
    raise SystemExit(main())
