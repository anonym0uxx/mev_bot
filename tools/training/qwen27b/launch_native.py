"""Manifest-bound native launcher. Read-only unless explicitly authorized."""
import argparse
import json
import hashlib
from pathlib import Path
import platform
import re
import os
import sys
import subprocess
import shutil


def command_output(argv):
    return subprocess.run(argv, check=True, capture_output=True, text=True, timeout=60).stdout


def collect_host(c):
    host = {'system': platform.system(), 'release': platform.release(), 'dxg': Path('/dev/dxg').exists()}
    require(host['system'] == 'Linux' and not re.search('microsoft|wsl', host['release'], re.I)
            and not host['dxg'], 'native Linux required')
    lines = command_output(['nvidia-smi', '--query-gpu=uuid,memory.total', '--format=csv,noheader,nounits'])
    host['gpus'] = [{'uuid': p[0].strip(), 'memory_mib': int(p[1])}
                    for p in (line.split(',') for line in lines.strip().splitlines())]
    host['gpu_processes'] = command_output(['nvidia-smi', '--query-compute-apps=pid', '--format=csv,noheader']).strip().splitlines()
    host['trainer_processes'] = []
    for proc in Path('/proc').iterdir():
        if not proc.name.isdigit() or int(proc.name) == os.getpid():
            continue
        try:
            cmd = (proc / 'cmdline').read_bytes().split(b'\0')
        except FileNotFoundError:
            continue
        if any(Path(os.fsdecode(arg)).name == 'train_qwen27b.py' for arg in cmd):
            host['trainer_processes'].append(proc.name)
    info = dict(line.split(':', 1) for line in Path('/proc/meminfo').read_text().splitlines())
    host['available_ram_bytes'] = int(info['MemAvailable'].split()[0]) * 1024
    host['total_ram_bytes'] = int(info['MemTotal'].split()[0]) * 1024
    host['mounts'] = json.loads(command_output(['findmnt', '--json', '--submounts', '--mountpoint', c['training_mount'],
        '--output', 'TARGET,FSTYPE,OPTIONS,UUID,MAJ:MIN,FSROOT']))['filesystems']
    host['disks'] = json.loads(command_output(['lsblk', '--json', '--tree', '--output', 'TYPE,SERIAL,MAJ:MIN']))['blockdevices']
    host['free_disk_bytes'] = shutil.disk_usage(c['training_mount']).free
    return host


def inside(path, parent):
    path, parent = Path(path), Path(parent)
    require(path.is_absolute() and parent.is_absolute() and path.resolve().is_relative_to(parent.resolve())
            and path.resolve() != parent.resolve(), 'path must be inside training mount')
    for part in [path, *path.parents]:
        require(not part.is_symlink(), 'symlink paths forbidden')
    return path


def inventory(directory, bindings):
    directory = Path(directory)
    require(directory.is_absolute() and directory.is_dir(), 'absolute seed directory required')
    require(not any(p.is_symlink() for p in [directory, *directory.parents, *directory.rglob('*')]),
            'symlink seed inventory forbidden')
    directory = directory.resolve()
    require(bindings, 'seed file inventory required')
    files = {p.resolve() for p in directory.rglob('*') if p.is_file()}
    expected = [pinned(b).resolve() for b in bindings]
    require(len(expected) == len(set(expected)) and set(expected) == files
            and all(p.is_relative_to(directory) for p in expected), 'seed file inventory mismatch')
    return files


def validate_contract(c, phase, release):
    require(c['schema_version'] == 'qwen27b_launch_v2' and c['phase'] == phase, 'launch schema/phase mismatch')
    require(re.fullmatch('[A-Za-z0-9][A-Za-z0-9_.-]{0,95}', c['run_id']), 'invalid run identity')
    manifest = load_json(release)
    require(manifest['schema_version'] == 'qwen27b_release_v2' and manifest['release_id'] == c['release_id'], 'release identity mismatch')
    def check_release_pins(value):
        if isinstance(value, dict):
            if 'path' in value and 'sha256' in value:
                binding = dict(value)
                binding['path'] = str((release.parent / value['path']).resolve())
                p = pinned(binding)
                if 'bytes' in value:
                    require(p.stat().st_size == value['bytes'], 'release file size mismatch')
            for child in value.values():
                check_release_pins(child)
        elif isinstance(value, list):
            for child in value:
                check_release_pins(child)
    check_release_pins(manifest)
    admission = manifest['admission']
    gates = ['semantic_review', 'economic_contract', 'clean_evaluation', 'distributed_loss_runtime', 'full_parameter_27b']
    require(manifest['status'] == 'APPROVED' and manifest['training_authorized'] is True
            and manifest['scope'] == 'north_star_trader' and admission['approved_by']
            and admission['clean_seed'] is True and all(admission['gates'].get(g) is True for g in gates),
            'release admission not approved')
    trainer = pinned(c['trainer'])
    require(trainer.name == 'train_qwen27b.py', 'trainer entrypoint mismatch')
    code = [pinned(b).resolve() for b in c['code_files']]
    require(len(code) == len(set(code)) and trainer.resolve() in code
            and Path(__file__).resolve() in code
            and set(trainer.parent.glob('*.py')).issubset(set(code)), 'all launcher/trainer/helper code must be pinned')
    ds = load_json(pinned(c['deepspeed']))
    zero = ds['zero_optimization']
    require(zero['stage'] == 3 and not zero.get('offload_param')
            and zero['stage3_gather_16bit_weights_on_model_save'] is False
            and zero['offload_optimizer']['device'] == 'nvme'
            and zero['offload_optimizer']['buffer_count'] >= 4 and ds['bf16']['enabled'] is True,
            'native DeepSpeed configuration unsafe')
    require((ds['train_batch_size'], ds['train_micro_batch_size_per_gpu'], ds['gradient_accumulation_steps']) == (24, 1, 8), 'batch geometry must be 24/1/8')
    mount = Path(c['training_mount'])
    run = inside(c['run_dir'], mount)
    require(run.name == c['run_id'], 'run identity/path mismatch')
    inside(zero['offload_optimizer']['nvme_path'], mount)
    if not c.get('resume'):
        require(not run.exists(), 'fresh run directory must not exist; preserve old checkpoints')
    seed = c['seed']
    if phase == 'cpt':
        require(seed['kind'] == 'clean_upstream' and seed['verified_clean'] is True
                and seed['repo'] and re.fullmatch('[0-9a-f]{40}', seed['revision']), 'CPT requires verified clean upstream seed')
    else:
        require(seed['kind'] == 'selected_new_cpt', 'SFT requires selected new CPT lineage')
    files = inventory(seed['path'], seed['files'])
    require(any(p.name == 'config.json' for p in files)
            and any(p.suffix == '.safetensors' or p.name == 'pytorch_model.bin' for p in files), 'consolidated seed weights/config required')
    receipt = load_json(pinned(seed['verification']))
    for key in ['kind', 'files'] + (['repo', 'revision', 'verified_clean'] if phase == 'cpt' else ['release_id', 'run_id', 'selection']):
        require(receipt[key] == seed[key], 'seed verification receipt mismatch')
    if phase == 'sft':
        require(seed['release_id'] == c['release_id'] and seed['release_sha256'] == c['release_manifest']['sha256']
                and seed['run_id'] != c['run_id'] and seed['selection'] == 'best_internal_validation',
                'SFT requires selected new CPT from this release')
        parent = load_json(pinned(seed['parent_run']))
        require(parent['phase'] == 'cpt' and parent['completed'] is True and parent['clean_upstream'] is True
                and parent['run_id'] == seed['run_id'] and parent['release_id'] == c['release_id']
                and parent['release_sha256'] == seed['release_sha256'], 'new CPT parent lineage mismatch')
    if c.get('resume'):
        resume = c['resume']
        checkpoint = inside(resume['path'], run / (c['release_id'] + '_' + phase))
        files = inventory(checkpoint, resume['files'])
        require(checkpoint.parent == (run / (c['release_id'] + '_' + phase)).resolve()
                and resume['complete'] is True and re.fullmatch('checkpoint-[1-9][0-9]*', checkpoint.name),
                'explicit complete checkpoint required')
        state = load_json(checkpoint / 'trainer_state.json')
        require(state['global_step'] == int(checkpoint.name.split('-')[1]), 'checkpoint step mismatch')
        names = [p.name for p in files]
        require(sum(n.endswith('_optim_states.pt') for n in names) == 3
                and any(n.endswith('_model_states.pt') for n in names)
                and 'scheduler.pt' in names
                and all('rng_state_' + str(i) + '.pth' in names for i in range(3)),
                'complete three-rank optimizer/model/scheduler/RNG checkpoint required')
        identity = load_json(pinned(resume['run_identity']))
        require(Path(resume['run_identity']['path']).resolve() == (run / 'run_identity.json').resolve(), 'resume identity location mismatch')
        for k, value in run_identity(c).items():
            require(identity[k] == value, 'resume identity mismatch: ' + k)
    return manifest


def run_identity(c):
    return {'release_sha256': c['release_manifest']['sha256'], 'release_id': c['release_id'],
            'run_id': c['run_id'], 'phase': c['phase'], 'seed': c['seed'],
            'deepspeed': c['deepspeed'], 'code_files': c['code_files']}


def validate_host(c, host):
    require(host['system'] == 'Linux' and not re.search('microsoft|wsl', host['release'], re.I)
            and not host['dxg'], 'native Linux required')
    uuids = c['gpu_uuids']
    require(len(uuids) == 3 and len(set(uuids)) == 3 and len(host['gpus']) == 3
            and {g['uuid'] for g in host['gpus']} == set(uuids), 'exactly three pinned GPUs required')
    require(c['min_gpu_memory_mib'] >= 95000
            and all(g['memory_mib'] >= c['min_gpu_memory_mib'] for g in host['gpus']), 'GPU memory insufficient')
    require(c['min_available_ram_bytes'] >= 128000000000 and c['min_free_disk_bytes'] >= 1500000000000,
            'RAM/disk budget floor cannot be reduced')
    require(host['available_ram_bytes'] >= c['min_available_ram_bytes'], 'RAM headroom insufficient')
    require(0 < host['available_ram_bytes'] <= host['total_ram_bytes'], 'invalid RAM telemetry')
    reserve = (host['total_ram_bytes'] * 12 + 99) // 100
    require(host['available_ram_bytes'] - c['min_available_ram_bytes'] >= reserve,
            '12% total RAM reserve must remain after declared training RAM budget')
    require(host['free_disk_bytes'] >= c['min_free_disk_bytes'], 'disk headroom insufficient')
    require(not host['trainer_processes'] and not host['gpu_processes'], 'trainer/GPU workload already running')
    mounts = host['mounts']
    require(len(mounts) == 1, 'verified ext4 mount required')
    m = mounts[0]
    require(m['target'] == c['training_mount'] and m['fstype'] == 'ext4' and m['fsroot'] == '/'
            and m['uuid'] == c['mount_uuid'] and c['mount_uuid']
            and 'rw' in m['options'].split(',') and 'ro' not in m['options'].split(','), 'verified ext4 mount required')
    require(not m.get('children'), 'unexpected nested mounts forbidden')
    require(c['disk_serial'] == '25375338A05C',
            'operator-pinned Windows/Linux disk serial required; Data disk 253753246786 forbidden')
    ancestry = []
    def visit(device, parents):
        chain = parents + [device]
        if device.get('maj:min') == m['maj:min']:
            ancestry.append(chain)
        for child in device.get('children', []):
            visit(child, chain)
    for device in host['disks']:
        visit(device, [])
    require(len(ancestry) == 1 and len(ancestry[0]) == 2
            and [d['type'] for d in ancestry[0]] == ['disk', 'part'],
            'unambiguous physical disk/partition ancestry required')
    require(ancestry[0][0].get('serial') == c['disk_serial']
            and sum(d.get('serial') == c['disk_serial'] for d in host['disks']) == 1,
            'verified disk serial required')


def validate_shared(c, phase, release):
    # Same dependency-free admission and seed contract as the actual trainer.
    module_path = Path(__file__).resolve().with_name('release_data.py')
    require(module_path in {pinned(b).resolve() for b in c['code_files']},
            'shared release validator must be code-pinned')
    import release_data
    require(Path(release_data.__file__).resolve() == module_path, 'shared validator import mismatch')
    manifest = load_json(release)
    release_data.verify_admission(manifest, release.parent)
    require(hasattr(release_data, 'verify_seed'), 'shared seed validator not implemented')
    release_data.verify_seed(manifest, phase, c['seed'], release.parent, sha256(release))
    require(hasattr(release_data, 'verify_training_request'), 'shared training request validator not implemented')
    release_data.verify_training_request(
        {'manifest': manifest, 'path': str(release), 'sha256': sha256(release)}, phase,
        c['seed']['path'], c['run_dir'], c.get('resume', {}).get('path'),
        c['_contract_path'], c['_contract_sha256'])


def preflight(c, phase, release):
    validate_contract(c, phase, release)
    host = collect_host(c)
    validate_host(c, host)
    validate_shared(c, phase, release)
    require(host['system'] == 'Linux' and not re.search('microsoft|wsl', host['release'], re.I)
            and not host['dxg'], 'native Linux required')
    cmd = [sys.executable, '-B', '-m', 'accelerate.commands.launch', '--use_deepspeed',
           '--num_processes', '3', '--num_machines', '1', '--machine_rank', '0',
           '--deepspeed_config_file', c['deepspeed']['path'], c['trainer']['path'],
           '--phase', phase, '--release_manifest', str(release), '--init_from', c['seed']['path'],
           '--out_root', c['run_dir'], '--launch_contract', c['_contract_path'],
           '--contract_sha256', c['_contract_sha256']]
    if c.get('resume'):
        cmd += ['--resume_from_checkpoint', c['resume']['path']]
    return {'status': 'preflight_passed', 'launched': False, 'launch_ready': False,
            'trainer_output_dir': str(Path(c['run_dir']) / (c['release_id'] + '_' + phase)),
            'command': cmd, 'host': host}


def runtime_probe(c, release):
    # Actual dataset construction and pinned runtime/tokenizer checks, no model training.
    trainer = pinned(c['trainer'])
    import ast
    tree = ast.parse(trainer.read_text(encoding='utf-8'))
    options = {arg.value for node in ast.walk(tree) if isinstance(node, ast.Call)
               and isinstance(node.func, ast.Attribute) and node.func.attr == 'add_argument'
               for arg in node.args if isinstance(arg, ast.Constant) and isinstance(arg.value, str)}
    required = {'--phase', '--release_manifest', '--out_root', '--init_from', '--dry_run',
                '--launch_contract', '--contract_sha256', '--preflight_only'}
    if c.get('resume'):
        required.add('--resume_from_checkpoint')
    require(required <= options, 'trainer CLI contract not implemented')
    command = [sys.executable, '-B', str(trainer), '--phase', c['phase'],
        '--release_manifest', str(release), '--out_root', c['run_dir'], '--init_from', c['seed']['path'],
        '--launch_contract', c['_contract_path'], '--contract_sha256', c['_contract_sha256']]
    if c.get('resume'):
        command += ['--resume_from_checkpoint', c['resume']['path']]
    # Pre-model validation exercises exactly the launch argv, not a boolean receipt.
    for mode in ['--preflight_only', '--dry_run']:
        result = subprocess.run(command + [mode], cwd=trainer.parent, env=runtime_env(c),
            capture_output=True, text=True, timeout=14400, check=True)
        require(result.stdout.strip(), 'trainer probe emitted no report')
    # AIO loading may compile: execute-only, never part of --dry-run.
    subprocess.run([sys.executable, '-B', '-c',
        'from deepspeed.ops.op_builder import AsyncIOBuilder; AsyncIOBuilder().load()'],
        cwd=trainer.parent, env=runtime_env(c), capture_output=True, text=True, timeout=1200, check=True)


def runtime_env(c):
    env = dict(os.environ)
    env.update(CUDA_VISIBLE_DEVICES=','.join(c['gpu_uuids']), HF_HUB_OFFLINE='1',
               TRANSFORMERS_OFFLINE='1', PYTHONDONTWRITEBYTECODE='1', TQDM_MININTERVAL='30')
    for key in ['RANK', 'LOCAL_RANK', 'WORLD_SIZE', 'MASTER_ADDR', 'MASTER_PORT']:
        env.pop(key, None)
    return env


def acquire_lock(path):
    import fcntl
    handle = open(path, 'a+b')
    try:
        fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        handle.close()
        raise Blocked('launcher already running: global training lock')
    return handle


def spawn(cmd, env, log, lock_fd):
    with open(log, 'ab') as output:
        child = subprocess.Popen(cmd, stdin=subprocess.DEVNULL, stdout=output, stderr=subprocess.STDOUT,
            env=env, cwd=Path(__file__).resolve().parent, start_new_session=True, pass_fds=(lock_fd,))
    return child.pid


def verify_spawn(pid):
    import time
    time.sleep(2)
    os.kill(pid, 0)
    stat = Path('/proc') / str(pid) / 'stat'
    require(stat.exists() and stat.read_text().split(') ', 1)[1][0] != 'Z', 'launcher exited immediately; inspect train.log')


def execute(c, args, release):
    # Global lock covers CPT and SFT and survives detachment in the accelerate parent.
    lock_path = Path(c['training_mount']) / '.qwen27b.launch.lock'
    require(not lock_path.is_symlink(), 'symlink lock forbidden')
    with acquire_lock(lock_path) as lock:
        plan = preflight(c, args.phase, release)
        runtime_probe(c, release)
        # Re-hash identities and re-probe capacity after the potentially long loader/JIT checks.
        pinned({'path': args.launch_contract, 'sha256': args.contract_sha256})
        pinned(c['release_manifest'])
        plan = preflight(c, args.phase, release)
        run = Path(c['run_dir'])
        if not c.get('resume'):
            run.mkdir(parents=False, exist_ok=False)
            (run / 'run_identity.json').write_text(json.dumps(run_identity(c), indent=2), encoding='utf-8')
        pid = spawn(plan['command'], runtime_env(c), run / 'train.log', lock.fileno())
        verify_spawn(pid)
        plan.update(status='spawned_not_training_verified', launched=True, launcher_pid=pid,
                    contract_sha256=args.contract_sha256)
        receipt = run / ('resume_' + str(pid) + '.json' if c.get('resume') else 'launch_receipt.json')
        with receipt.open('x', encoding='utf-8') as f:
            json.dump(plan, f, indent=2)
        require(load_json(receipt)['launcher_pid'] == pid, 'launch receipt readback failed')
        return plan


def require(ok, message):
    if not ok:
        raise Blocked(message)


def sha256(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for chunk in iter(lambda: f.read(8 * 1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()


def pinned(binding):
    digest = binding['sha256']
    require(isinstance(digest, str) and re.fullmatch('[0-9a-f]{64}', digest), 'invalid SHA256')
    path = Path(binding['path'])
    require(path.is_absolute() and path.is_file() and '..' not in path.parts
            and not any(p.is_symlink() for p in [path, *path.parents]), 'absolute regular pinned file required')
    require(sha256(path) == digest, 'SHA256 mismatch: ' + str(path))
    return path


def load_json(path):
    def unique(pairs):
        result = {}
        for k, v in pairs:
            require(k not in result, 'duplicate JSON key: ' + k)
            result[k] = v
        return result
    return json.loads(Path(path).read_text(encoding='utf-8'), object_pairs_hook=unique)

class Blocked(RuntimeError):
    pass

def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--phase', choices=['cpt', 'sft'])
    p.add_argument('--release_manifest')
    p.add_argument('--launch-contract')
    p.add_argument('--contract-sha256')
    mode = p.add_mutually_exclusive_group()
    mode.add_argument('--dry-run', action='store_true')
    mode.add_argument('--execute', action='store_true')
    p.add_argument('--resume', action='store_true', help='Require explicit resume object in contract')
    args = p.parse_args(argv)
    try:
        if not all([args.phase, args.release_manifest, args.launch_contract, args.contract_sha256]):
            raise Blocked('explicit phase, release manifest and launch contract required')
        c = load_json(pinned({'path': args.launch_contract, 'sha256': args.contract_sha256}))
        c['_contract_path'] = args.launch_contract
        c['_contract_sha256'] = args.contract_sha256
        release = pinned(c['release_manifest'])
        require(release.resolve() == Path(args.release_manifest).resolve(), 'release manifest path mismatch')
        require(not args.resume or c.get('resume'), 'explicit resume contract required')
        plan = preflight(c, args.phase, release)
        if args.execute:
            plan = execute(c, args, release)
        print(json.dumps(plan))
        return 0
    except (Blocked, OSError, ValueError, TypeError, KeyError, IndexError, subprocess.SubprocessError) as exc:
        print(json.dumps({'status': 'blocked', 'launch_ready': False, 'error': str(exc)}))
        return 2

if __name__ == '__main__':
    raise SystemExit(main())
