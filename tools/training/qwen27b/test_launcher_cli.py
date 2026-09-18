"""Actual subprocess CLI safety tests. Fixtures are NOT native acceptance."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import shutil
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent
CLI = ROOT / 'launch_native.py'

class LauncherCLI(unittest.TestCase):
    def invoke(self, *args):
        self.assertTrue(CLI.is_file(), 'Manifest-bound launcher CLI is missing')
        return subprocess.run([sys.executable, '-B', str(CLI), *map(str, args)],
                              capture_output=True, text=True, timeout=30)

    def fixture(self, root, phase='cpt'):
        def put(name, value):
            p = root / name
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(json.dumps(value), encoding='utf-8')
            return p
        def pin(p):
            return {'path': str(p), 'sha256': hashlib.sha256(p.read_bytes()).hexdigest()}
        seed = root / 'clean_seed'
        seed.mkdir()
        put('clean_seed/config.json', {})
        put('clean_seed/model.safetensors', {'fixture': 'NOT REAL WEIGHTS'})
        release = put('release.json', {'schema_version': 'qwen27b_release_v2', 'release_id': 'new-release',
            'status': 'APPROVED', 'training_authorized': True, 'scope': 'north_star_trader',
            'admission': {'approved_by': 'TEST FIXTURE ONLY', 'clean_seed': True, 'gates': {
                k: True for k in ['semantic_review', 'economic_contract', 'clean_evaluation',
                                  'distributed_loss_runtime', 'full_parameter_27b']}}})
        trainer = put('train_qwen27b.py', {'fixture': True})
        ds = put('ds.json', {'bf16': {'enabled': True}, 'zero_optimization': {'stage': 3,
            'offload_optimizer': {'device': 'nvme', 'nvme_path': str(root / 'offload'), 'buffer_count': 4},
            'stage3_gather_16bit_weights_on_model_save': False},
            'train_batch_size': 24, 'train_micro_batch_size_per_gpu': 1, 'gradient_accumulation_steps': 8})
        contract = {'schema_version': 'qwen27b_launch_v2', 'phase': phase, 'run_id': 'new-run',
            'release_manifest': pin(release), 'release_id': 'new-release', 'trainer': pin(trainer),
            'code_files': [pin(trainer), pin(CLI)], 'deepspeed': pin(ds), 'run_dir': str(root / 'new-run'),
            'training_mount': str(root), 'mount_uuid': 'fixture-uuid', 'disk_serial': '25375338A05C',
            'gpu_uuids': ['GPU-a', 'GPU-b', 'GPU-c'],
            'min_gpu_memory_mib': 95000, 'min_available_ram_bytes': 128000000000,
            'min_free_disk_bytes': 1500000000000,
            'seed': {'path': str(seed), 'kind': 'clean_upstream', 'repo': 'fixture/upstream',
                'revision': 'a' * 40, 'files': [pin(p) for p in sorted(seed.iterdir())],
                'verified_clean': True, 'verification': pin(put('seed_receipt.json', {
                    'kind': 'clean_upstream', 'verified_clean': True, 'repo': 'fixture/upstream',
                    'revision': 'a' * 40, 'files': [pin(p) for p in sorted(seed.iterdir())]}))}}
        path = put('contract.json', contract)
        return contract, path, release

    def save_contract(self, path, contract):
        path.write_text(json.dumps(contract), encoding='utf-8')
        return hashlib.sha256(path.read_bytes()).hexdigest()

    def test_manifest_hash_and_native_platform_are_enforced(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            c, path, release = self.fixture(root)
            sha = self.save_contract(path, c)
            args = ('--phase', 'cpt', '--release_manifest', release,
                    '--launch-contract', path, '--contract-sha256', sha, '--dry-run')
            result = self.invoke(*args)
            self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
            self.assertIn('native Linux required', result.stdout)
            release.write_text('{}')
            result = self.invoke(*args)
            self.assertIn('SHA256 mismatch', result.stdout)
            self.assertFalse((root / 'new-run').exists())

    def simulated(self, c, path, release, host, *extra):
        sha = self.save_contract(path, c)
        # Dependency injection ONLY in test harness; production CLI has no bypass.
        harness = """import sys, json
sys.path.insert(0, sys.argv[1])
import launch_native as m
m.collect_host = lambda c: json.loads(sys.argv[2])
m.validate_shared = lambda *args: None  # Fixture-only, separately tested below.
raise SystemExit(m.main(sys.argv[3:]))
"""
        return subprocess.run([sys.executable, '-B', '-c', harness, str(ROOT), json.dumps(host),
            '--phase', c['phase'], '--release_manifest', str(release), '--launch-contract', str(path),
            '--contract-sha256', sha, *extra], capture_output=True, text=True, timeout=30)

    def host(self, root):
        return {'system': 'Linux', 'release': 'native', 'dxg': False,
            'gpus': [{'uuid': 'GPU-' + x, 'memory_mib': 96000} for x in 'abc'], 'gpu_processes': [],
            'trainer_processes': [], 'available_ram_bytes': 200000000000, 'total_ram_bytes': 256000000000,
            'free_disk_bytes': 2000000000000,
            'mounts': [{'target': str(root), 'fstype': 'ext4', 'options': 'rw,relatime',
                       'uuid': 'fixture-uuid', 'maj:min': '259:1', 'fsroot': '/'}],
            'disks': [{'type': 'disk', 'serial': '25375338A05C', 'maj:min': '259:0',
                       'children': [{'type': 'part', 'maj:min': '259:1'}]}]}

    def test_full_cli_readonly_plan_uses_manifest_seed_and_three_ranks(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            c, path, release = self.fixture(root)
            self.save_contract(path, c)
            before = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in root.rglob('*') if p.is_file()}
            result = self.simulated(c, path, release, self.host(root), '--dry-run')
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            plan = json.loads(result.stdout)
            self.assertFalse(plan['launched'])
            self.assertEqual(plan['status'], 'preflight_passed')
            cmd = plan['command']
            self.assertEqual(cmd[cmd.index('--num_processes') + 1], '3')
            self.assertEqual(cmd[cmd.index('--release_manifest') + 1], str(release))
            self.assertEqual(cmd[cmd.index('--init_from') + 1], c['seed']['path'])
            self.assertEqual(cmd[cmd.index('--out_root') + 1], c['run_dir'])
            self.assertNotIn('--data_root', cmd)
            self.assertEqual(before, {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in root.rglob('*') if p.is_file()})
            self.assertFalse((root / 'new-run').exists())

    def test_native_mount_ancestry_and_reserve(self):
        cases = [
            ('arbitrary_serial', lambda c,h: (c.update(disk_serial='arbitrary'), h['disks'][0].update(serial='arbitrary')), 'operator-pinned'),
            ('data_disk', lambda c,h: (c.update(disk_serial='253753246786'), h['disks'][0].update(serial='253753246786')), 'operator-pinned'),
            ('nested_mount', lambda c,h: h['mounts'][0].update(children=[{'target':str(Path(c['training_mount'])/'offload')}]), 'nested'),
            ('ambiguous_ancestry', lambda c,h: h['disks'].append(dict(h['disks'][0],serial='other')), 'ancestry'),
            ('mapped_device', lambda c,h: h['disks'][0]['children'][0].update(type='crypt'), 'ancestry'),
            ('reserve', lambda c,h: h.update(available_ram_bytes=c['min_available_ram_bytes']), '12%'),
        ]
        for name, mutate, message in cases:
            with self.subTest(case=name), tempfile.TemporaryDirectory() as temp:
                root=Path(temp);c,path,release=self.fixture(root);h=self.host(root)
                mutate(c,h)
                result=self.simulated(c,path,release,h,'--dry-run')
                self.assertEqual(result.returncode,2,result.stdout+result.stderr)
                self.assertIn(message,result.stdout)

    def test_guard_matrix_cli(self):
        cases = [
            ('two_gpus', lambda c,h,r: h.update(gpus=h['gpus'][:2]), 'three pinned GPUs'),
            ('wrong_gpu', lambda c,h,r: h['gpus'][0].update(uuid='GPU-other'), 'three pinned GPUs'),
            ('gpu_ram', lambda c,h,r: h['gpus'][0].update(memory_mib=40000), 'GPU memory'),
            ('wsl', lambda c,h,r: h.update(release='microsoft-standard-WSL2'), 'native Linux'),
            ('dxg', lambda c,h,r: h.update(dxg=True), 'native Linux'),
            ('ram', lambda c,h,r: h.update(available_ram_bytes=1), 'RAM headroom'),
            ('space', lambda c,h,r: h.update(free_disk_bytes=1), 'disk headroom'),
            ('space_floor', lambda c,h,r: c.update(min_free_disk_bytes=1), 'budget floor'),
            ('ram_floor', lambda c,h,r: c.update(min_available_ram_bytes=1), 'budget floor'),
            ('missing_mount', lambda c,h,r: h.update(mounts=[]), 'verified ext4 mount'),
            ('wrong_uuid', lambda c,h,r: h['mounts'][0].update(uuid='wrong'), 'verified ext4 mount'),
            ('wrong_serial', lambda c,h,r: h['disks'][0].update(serial='wrong'), 'disk serial'),
            ('ro_mount', lambda c,h,r: h['mounts'][0].update(options='ro'), 'verified ext4 mount'),
            ('ntfs_mount', lambda c,h,r: h['mounts'][0].update(fstype='ntfs3'), 'verified ext4 mount'),
            ('bind_mount', lambda c,h,r: h['mounts'][0].update(fsroot='/other'), 'verified ext4 mount'),
            ('duplicate', lambda c,h,r: h.update(trainer_processes=['987']), 'already running'),
            ('busy_gpu', lambda c,h,r: h.update(gpu_processes=['987']), 'already running'),
            ('phase', lambda c,h,r: c.update(schema_version='old'), 'launch schema'),
            ('dirty_output', lambda c,h,r: (r/'new-run').mkdir(), 'fresh run directory'),
            ('old_seed', lambda c,h,r: c['seed'].update(kind='old_cpt'), 'clean upstream'),
            ('missing_pin', lambda c,h,r: c['seed'].update(files=[]), 'seed file inventory'),
            ('sft_old', lambda c,h,r: c.update(phase='sft'), 'selected new CPT'),
            ('escape_run', lambda c,h,r: c.update(run_dir=str(r.parent/'escape')), 'inside training mount'),
        ]
        for name, mutate, error in cases:
            with self.subTest(case=name), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                c, path, release = self.fixture(root)
                h = self.host(root)
                mutate(c,h,root)
                self.save_contract(path,c)
                before = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in root.rglob('*') if p.is_file()}
                result = self.simulated(c,path,release,h,'--dry-run')
                self.assertEqual(result.returncode,2,result.stdout + result.stderr)
                self.assertIn(error,result.stdout)
                self.assertEqual(before,{str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in root.rglob('*') if p.is_file()})

    def test_config_and_admission_fail_closed(self):
        for case in ['gather','offload_param','stage','batch','unapproved','missing_gate','seed_drift','code_drift','duplicate_json']:
            with self.subTest(case=case), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                c,path,release = self.fixture(root)
                if case in ['gather','offload_param','stage','batch']:
                    ds = json.loads(Path(c['deepspeed']['path']).read_text())
                    if case == 'gather': ds['zero_optimization']['stage3_gather_16bit_weights_on_model_save'] = True
                    if case == 'offload_param': ds['zero_optimization']['offload_param'] = {'device':'nvme'}
                    if case == 'stage': ds['zero_optimization']['stage'] = 2
                    if case == 'batch': ds['train_batch_size'] = 8
                    p = Path(c['deepspeed']['path']); p.write_text(json.dumps(ds))
                    c['deepspeed']['sha256'] = hashlib.sha256(p.read_bytes()).hexdigest()
                elif case in ['unapproved','missing_gate']:
                    rel = json.loads(release.read_text())
                    if case == 'unapproved': rel['training_authorized'] = False
                    else: del rel['admission']['gates']['clean_evaluation']
                    release.write_text(json.dumps(rel)); c['release_manifest']['sha256'] = hashlib.sha256(release.read_bytes()).hexdigest()
                elif case == 'seed_drift': (root/'clean_seed/model.safetensors').write_text('changed')
                elif case == 'code_drift': (root/'train_qwen27b.py').write_text('changed')
                else:
                    release.write_text('{"status":"APPROVED","status":"CANDIDATE"}')
                    c['release_manifest']['sha256'] = hashlib.sha256(release.read_bytes()).hexdigest()
                result = self.simulated(c,path,release,self.host(root),'--dry-run')
                self.assertEqual(result.returncode,2,result.stdout + result.stderr)

    def test_sft_lineage_and_resume_fail_closed(self):
        for case in ['sft_valid','sft_wrong_release','sft_wrong_selection','resume_missing','resume_valid','resume_changed']:
            with self.subTest(case=case), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                c,path,release = self.fixture(root)
                def pin(p): return {'path':str(p),'sha256':hashlib.sha256(p.read_bytes()).hexdigest()}
                def put(name,obj):
                    p=root/name; p.parent.mkdir(parents=True,exist_ok=True);p.write_text(json.dumps(obj));return p
                if case.startswith('sft'):
                    c['phase']='sft'
                    c['seed'].update(kind='selected_new_cpt', release_id=c['release_id'], run_id='parent-cpt',
                                     selection='best_internal_validation', release_sha256=c['release_manifest']['sha256'])
                    parent=put('parent.json', {'phase':'cpt','run_id':'parent-cpt','release_id':c['release_id'],
                        'release_sha256':c['release_manifest']['sha256'], 'clean_upstream':True, 'completed':True})
                    c['seed']['parent_run']=pin(parent)
                    if case=='sft_wrong_release': c['seed']['release_sha256']='f'*64
                    if case=='sft_wrong_selection': c['seed']['selection']='last'
                    receipt={k:v for k,v in c['seed'].items() if k != 'verification'}
                    c['seed']['verification']=pin(put('selected.json',receipt))
                else:
                    output=root/'new-run'/'new-release_cpt'
                    output.mkdir(parents=True)
                    ckpt=output/'checkpoint-1';ckpt.mkdir()
                    f=put('new-run/new-release_cpt/checkpoint-1/trainer_state.json',{'global_step':1})
                    state=put('new-run/run_identity.json',{'release_sha256':c['release_manifest']['sha256'],
                        'release_id':c['release_id'],'run_id':c['run_id'],'phase':c['phase'],
                        'seed':c['seed'], 'deepspeed':c['deepspeed'], 'code_files':c['code_files']})
                    c['resume']={'path':str(ckpt),'files':[pin(f)],'run_identity':pin(state),'complete':True}
                    if case=='resume_missing': del c['resume']['files']
                    if case=='resume_changed': f.write_text('{}')
                result=self.simulated(c,path,release,self.host(root),'--dry-run')
                # No implicit acceptance of one trainer_state file as a complete ZeRO checkpoint.
                expected=0 if case=='sft_valid' else 2
                self.assertEqual(result.returncode,expected,result.stdout+result.stderr)

    def test_explicit_execute_has_real_dispatch_and_no_dryrun_writes(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);c,path,release=self.fixture(root)
            sha=self.save_contract(path,c)
            harness="""import sys,json
sys.path.insert(0,sys.argv[1]);import launch_native as m
m.collect_host=lambda c:json.loads(sys.argv[2])
m.validate_shared=lambda *args:None  # Fixture-only admission boundary.
m.runtime_probe=lambda c,release:None
m.spawn=lambda cmd,env,log,lock_fd:123456
m.verify_spawn=lambda pid:None
raise SystemExit(m.main(sys.argv[3:]))
"""
            args=[sys.executable,'-B','-c',harness,str(ROOT),json.dumps(self.host(root)),
                '--phase','cpt','--release_manifest',str(release),'--launch-contract',str(path),
                '--contract-sha256',sha,'--execute']
            # fcntl is native only; test portable lock abstraction independently.
            harness=harness.replace('m.collect_host=', 'm.acquire_lock=lambda p:open(p,"a+b")\nm.collect_host=')
            args[3]=harness
            result=subprocess.run(args,capture_output=True,text=True,timeout=30)
            self.assertEqual(result.returncode,0,result.stdout+result.stderr)
            payload=json.loads(result.stdout)
            self.assertEqual(payload['status'],'spawned_not_training_verified')
            self.assertTrue((root/'new-run'/'launch_receipt.json').is_file())
            self.assertTrue((root/'new-run'/'run_identity.json').is_file())
            result=subprocess.run(args,capture_output=True,text=True,timeout=30)
            self.assertEqual(result.returncode,2,result.stdout+result.stderr)

    def test_full_resume_plan_and_identity(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);c,path,release=self.fixture(root)
            ckpt=root/'new-run'/'new-release_cpt'/'checkpoint-1';ckpt.mkdir(parents=True)
            def pin(p):return {'path':str(p),'sha256':hashlib.sha256(p.read_bytes()).hexdigest()}
            (ckpt/'trainer_state.json').write_text('{"global_step":1}')
            for name in ['rank0_model_states.pt','scheduler.pt'] + [f'rank{i}_optim_states.pt' for i in range(3)] + [f'rng_state_{i}.pth' for i in range(3)]:
                (ckpt/name).write_text('FIXTURE NOT CHECKPOINT')
            identity={k:c[k] for k in ['release_id','run_id','phase','seed','deepspeed','code_files']}
            identity['release_sha256']=c['release_manifest']['sha256']
            ip=root/'new-run'/'run_identity.json';ip.write_text(json.dumps(identity))
            c['resume']={'path':str(ckpt),'files':[pin(p) for p in ckpt.iterdir()], 'run_identity':pin(ip),'complete':True}
            result=self.simulated(c,path,release,self.host(root),'--resume','--dry-run')
            self.assertEqual(result.returncode,0,result.stdout+result.stderr)
            command=json.loads(result.stdout)['command']
            self.assertEqual(command[command.index('--resume_from_checkpoint')+1],str(ckpt))
            identity['run_id']='old';ip.write_text(json.dumps(identity));c['resume']['run_identity']=pin(ip)
            result=self.simulated(c,path,release,self.host(root),'--resume','--dry-run')
            self.assertEqual(result.returncode,2,result.stdout+result.stderr)
            self.assertIn('resume identity mismatch',result.stdout)

    def test_wrappers_and_native_config_are_safe(self):
        for name in ['launch_cpt_detached.sh','launch_sft_detached.sh','resume_cpt.sh','resume_sft.sh','bootstrap_linux.sh']:
            with self.subTest(name=name):
                text=(ROOT/name).read_text(encoding='utf-8')
                self.assertIn('launch_native.py',text)
                self.assertNotIn('rm -',text)
                self.assertNotIn('apt-get',text)
                self.assertNotIn('setsid',text)
                self.assertNotIn('/mnt/d/',text)
                result=subprocess.run([shutil.which('bash'),'-n',(ROOT/name).as_posix()],capture_output=True,text=True)
                self.assertEqual(result.returncode,0,result.stderr)
                env=dict(os.environ,PYTHON=sys.executable)
                env.pop('MSYS_NO_PATHCONV',None)
                env.pop('MSYS2_ARG_CONV_EXCL',None)
                result=subprocess.run([shutil.which('bash'),(ROOT/name).as_posix(),'--dry-run'],env=env,capture_output=True,text=True,timeout=10)
                self.assertEqual(result.returncode,2,result.stdout+result.stderr)
                self.assertIn('explicit phase, release manifest and launch contract required',result.stdout)
        ds=json.loads((ROOT/'ds_zero3_native.json').read_text())
        self.assertIs(ds['zero_optimization']['stage3_gather_16bit_weights_on_model_save'],False)

    def test_probe_failures_are_json_blocked(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);c,path,release=self.fixture(root);sha=self.save_contract(path,c)
            harness="""import sys,subprocess
sys.path.insert(0,sys.argv[1]);import launch_native as m
def failed(c):raise subprocess.CalledProcessError(1,['nvidia-smi'])
m.collect_host=failed
raise SystemExit(m.main(sys.argv[2:]))
"""
            result=subprocess.run([sys.executable,'-B','-c',harness,str(ROOT),'--phase','cpt',
                '--release_manifest',str(release),'--launch-contract',str(path),'--contract-sha256',sha,'--dry-run'],capture_output=True,text=True)
            self.assertEqual(result.returncode,2,result.stdout+result.stderr)
            self.assertEqual(json.loads(result.stdout)['status'],'blocked')

    def test_real_loader_probe_and_spawn_boundary_without_training(self):
        import importlib.util
        from unittest.mock import patch
        spec=importlib.util.spec_from_file_location('launcher_under_test',CLI)
        m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);c,path,release=self.fixture(root)
            trainer=root/'train_qwen27b.py'
            trainer.write_text('''import argparse,json
p=argparse.ArgumentParser()
p.add_argument('--phase',required=True)
p.add_argument('--release_manifest',required=True)
p.add_argument('--out_root',required=True)
p.add_argument('--init_from',required=True)
p.add_argument('--dry_run',action='store_true')
p.add_argument('--preflight_only',action='store_true')
p.add_argument('--launch_contract',required=True)
p.add_argument('--contract_sha256',required=True)
a=p.parse_args()
assert a.dry_run or a.preflight_only, 'Test fixture refuses all training'
print(json.dumps({'phase':a.phase,'loss_tokens':1,'fixture_only':True}))
''')
            c['trainer']['sha256']=hashlib.sha256(trainer.read_bytes()).hexdigest()
            receipt=root/'trainer_verification.json';receipt.write_text(json.dumps({'trainer_sha256':c['trainer']['sha256'],
                'explicit_clean_seed':True,'out_root_semantics':'append_release_phase','explicit_resume':False}))
            c['trainer_verification']={'path':str(receipt),'sha256':hashlib.sha256(receipt.read_bytes()).hexdigest()}
            c['_contract_path']=str(path);c['_contract_sha256']=self.save_contract(path,c)
            calls=[];real_run=subprocess.run
            def runner(command,**kwargs):
                calls.append(command)
                if '-c' in command:
                    return subprocess.CompletedProcess(command,0,stdout='fixture AIO boundary only',stderr='')
                return real_run(command,**kwargs)
            with patch.object(m.subprocess,'run',runner):m.runtime_probe(c,release)
            self.assertEqual(len(calls),3)
            self.assertIn('--preflight_only',calls[0])
            self.assertIn('--dry_run',calls[1])
            self.assertIn('--launch_contract',calls[0])
            # Exercise the actual Popen boundary with a harmless child, not a trainer.
            if os.name=='nt':
                captured={}
                def popen(command,**kwargs):
                    captured.update(kwargs)
                    kwargs.pop('pass_fds')  # Windows cannot inherit POSIX flock descriptors.
                    child=subprocess_original(command,**kwargs)
                    captured['child']=child
                    return child
                subprocess_original=subprocess.Popen
                with patch.object(m.subprocess,'Popen',popen):
                    pid=m.spawn([sys.executable,'-c','print("harmless launcher dispatch fixture")'],dict(os.environ),root/'child.log',999)
                # Reap before assertions: any failed assertion must not leave the
                # Windows child holding child.log while TemporaryDirectory cleans up.
                child=captured['child']
                try:
                    returncode=child.wait(timeout=10)
                finally:
                    if child.poll() is None:
                        child.terminate()
                    child.wait(timeout=10)
                self.assertEqual(returncode,0)
                self.assertEqual(captured['pass_fds'],(999,))
                self.assertIn('harmless launcher dispatch fixture',(root/'child.log').read_text())
                self.assertGreater(pid,0)

    def test_shared_seed_and_generated_argv_fresh_cpt_sft_resume(self):
        import launch_native as m
        import release_data as rd
        from unittest.mock import patch
        for phase,resume in [('cpt',False),('sft',False),('cpt',True),('sft',True)]:
            with self.subTest(phase=phase,resume=resume), tempfile.TemporaryDirectory() as temp:
                root=Path(temp);c,path,release=self.fixture(root,phase)
                def pin(p):return {'path':str(p),'sha256':m.sha256(p)}
                c['trainer']=pin(ROOT/'train_qwen27b.py')
                c['code_files']=[pin(p) for p in ROOT.glob('*.py')]
                seed=c['seed'];seed.update(repo=rd.TOKENIZER_REPO,revision=rd.TOKENIZER_REV)
                if phase=='sft':
                    seed.update(kind='selected_new_cpt',release_id=c['release_id'],
                        release_sha256=c['release_manifest']['sha256'],run_id='parent-cpt',selection='best_internal_validation')
                    parent=root/'parent.json';parent.write_text(json.dumps(dict(phase='cpt',completed=True,clean_upstream=True,
                        run_id=seed['run_id'],release_id=c['release_id'],release_sha256=seed['release_sha256'])))
                    seed['parent_run']=pin(parent)
                receipt=Path(seed['verification']['path'])
                receipt.write_text(json.dumps({k:v for k,v in seed.items() if k!='verification'}));seed['verification']=pin(receipt)
                if resume:
                    ckpt=Path(c['run_dir'])/(c['release_id']+'_'+phase)/'checkpoint-1';ckpt.mkdir(parents=True)
                    (ckpt/'trainer_state.json').write_text('{"global_step":1}')
                    for name in ['rank0_model_states.pt','scheduler.pt']+[f'rank{i}_optim_states.pt' for i in range(3)]+[f'rng_state_{i}.pth' for i in range(3)]:
                        (ckpt/name).write_text('FIXTURE ONLY')
                    ip=Path(c['run_dir'])/'run_identity.json';ip.write_text(json.dumps(m.run_identity(c)))
                    c['resume']={'path':str(ckpt),'files':[pin(p) for p in ckpt.iterdir()],'complete':True,'run_identity':pin(ip)}
                digest=self.save_contract(path,c);c['_contract_path']=str(path);c['_contract_sha256']=digest
                # Admission tested independently. No model or optimizer deserialization.
                with patch.object(rd,'verify_admission',lambda *a:None), patch.object(m,'collect_host',lambda c:self.host(root)):
                    command=m.preflight(c,phase,release)['command']
                def arg(name):return command[command.index(name)+1] if name in command else None
                actual=rd.verify_training_request({'manifest':m.load_json(release),'path':str(release),'sha256':m.sha256(release)},
                    phase,arg('--init_from'),arg('--out_root'),arg('--resume_from_checkpoint'),arg('--launch_contract'),arg('--contract_sha256'))
                self.assertTrue(actual['pre_model_verified'])
                self.assertEqual(bool(actual['resume_from_checkpoint']),resume)
                (Path(seed['path'])/'model.safetensors').write_text('tampered')
                with self.assertRaises(ValueError):rd.verify_seed(m.load_json(release),phase,seed,root,m.sha256(release))

    def test_shared_admission_is_not_replaced_by_fixture_booleans(self):
        import launch_native as m
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);c,path,release=self.fixture(root)
            c['code_files'] += [{'path':str(ROOT/'release_data.py'),
                'sha256':hashlib.sha256((ROOT/'release_data.py').read_bytes()).hexdigest()}]
            with self.assertRaisesRegex((ValueError,m.Blocked), 'evidence|Evidence|acceptance|Acceptance'):
                m.validate_shared(c,'cpt',release)

    def test_gate_receipts_bind_payload_population_runtime_and_subjects(self):
        import copy
        import launch_native as m
        import release_data as rd
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);c,path,release=self.fixture(root)
            def pin(p):return {'path':str(p),'bytes':p.stat().st_size,'sha256':m.sha256(p)}
            c['code_files'].append(pin(ROOT/'release_data.py'))
            subject=root/'subject.txt';subject.write_text('FIXTURE NOT ACCEPTANCE')
            manifest=m.load_json(release)
            manifest.update(runtime={'fixture':'not-native'},phases={'cpt':{'train':[dict(pin(subject),rows=1)],'validation':[dict(pin(subject),rows=1)]}},
                exclusion_union=pin(subject),tokenizer={'path':str(root),'files':{'subject.txt':m.sha256(subject)}},
                verification_subjects={role:pin(subject) for roles in rd.GATE_SUBJECT_ROLES.values() for role in roles})
            payload=rd.release_payload_sha256(manifest);results={}
            for gate in rd.GATE_SUBJECT_ROLES:
                receipt={'schema':'qwen27b_gate_result_v1','verified':True,'gate':gate,'release_id':c['release_id'],
                    'release_payload_sha256':payload,'runtime':manifest['runtime'],'population':rd.release_population(manifest),
                    'evidence_scope':'complete_required_population','result':{'schema':gate+'_result_v1','status':'PASS','failures':0},
                    'subject_pins':rd.required_gate_subjects(manifest,root,gate)}
                rp=root/(gate+'.json');rp.write_text(json.dumps(receipt))
                results[gate]={'passed':True,'reason':'verified_evidence','receipt':str(rp),'receipt_sha256':m.sha256(rp)}
            ep=root/'acceptance.json'
            evidence={'schema':'north_star_release_acceptance_v2','training_approved':True,'operator_go':True,
                'release_id':c['release_id'],'release_payload_sha256':payload,'failed_gates':[],'gate_results':results}
            ep.write_text(json.dumps(evidence));manifest['admission']['evidence']=pin(ep)
            rd.verify_admission(manifest,root,acceptance_sha256=m.sha256(ep))
            gate='token_masks';rp=Path(results[gate]['receipt']);original=m.load_json(rp)
            for case in ['payload','runtime','population','subjects','result','receipt_hash','authority']:
                with self.subTest(case=case):
                    receipt=copy.deepcopy(original)
                    if case=='payload':receipt['release_payload_sha256']='0'*64
                    if case=='runtime':receipt['runtime']={}
                    if case=='population':receipt['population']={}
                    if case=='subjects':receipt['subject_pins']=[dict(pin(subject),role='irrelevant')]
                    if case=='result':receipt['result']={'status':'PASS'}
                    rp.write_text(json.dumps(receipt));results[gate]['receipt_sha256']='0'*64 if case=='receipt_hash' else m.sha256(rp)
                    ep.write_text(json.dumps(evidence));manifest['admission']['evidence']=pin(ep)
                    release.write_text(json.dumps(manifest));c['release_manifest']=pin(release)
                    with patch.dict(os.environ,{'QWEN_RELEASE_ACCEPTANCE_SHA256':'0'*64 if case=='authority' else m.sha256(ep)}):
                        with self.assertRaises(ValueError):m.validate_shared(c,'cpt',release)

    def test_reserve_boundary_and_nested_collection(self):
        import launch_native as m
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);c,path,release=self.fixture(root);host=self.host(root)
            reserve=(host['total_ram_bytes']*12+99)//100
            host['available_ram_bytes']=c['min_available_ram_bytes']+reserve
            m.validate_host(c,host)
            host['available_ram_bytes']-=1
            with self.assertRaisesRegex(m.Blocked,'12%'):m.validate_host(c,host)
        self.assertIn("'--submounts'",CLI.read_text())

    def test_release_dataset_pins_checked_without_training_imports(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);c,path,release=self.fixture(root)
            data=root/'data.jsonl';data.write_text('{"text":"fixture"}\n')
            rel=json.loads(release.read_text());rel['phases']={'cpt':{'train':[{'path':str(data),'sha256':'0'*64,'bytes':data.stat().st_size,'rows':1}]}}
            release.write_text(json.dumps(rel));c['release_manifest']['sha256']=hashlib.sha256(release.read_bytes()).hexdigest()
            result=self.simulated(c,path,release,self.host(root),'--dry-run')
            self.assertEqual(result.returncode,2,result.stdout+result.stderr)
            self.assertIn('SHA256 mismatch',result.stdout)

    def test_default_and_dryrun_are_read_only_and_require_explicit_identity(self):
        with tempfile.TemporaryDirectory() as temp:
            before = sorted(Path(temp).rglob('*'))
            for args in [(), ('--dry-run',)]:
                result = self.invoke(*args)
                self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
                self.assertIn('explicit phase, release manifest and launch contract required', result.stdout)
            self.assertEqual(before, sorted(Path(temp).rglob('*')))

    def test_launcher_and_trainer_cannot_reach_legacy_training_data(self):
        """Property: neither the launcher nor the trainer can silently reach the
        legacy training datasets. Real subprocesses, no mocks."""
        forbidden = ['economics_v1/datasets', 'economics_v1\\datasets',
                     'sft_train.jsonl', 'cpt_train.jsonl', 'cpt_all.jsonl',
                     'sft_balanced_train.jsonl', 'qwen_sft_v2.jsonl',
                     'reasoning.jsonl']
        guarded = [CLI, ROOT / 'train_qwen27b.py'] + sorted(ROOT.glob('*.sh'))
        for path in guarded:
            self.assertTrue(path.is_file(), 'guarded file missing: ' + str(path))
            text = path.read_text(encoding='utf-8', errors='replace')
            for token in forbidden:
                with self.subTest(file=path.name, token=token):
                    self.assertNotIn(token, text,
                        'legacy training-dataset path hardcoded in ' + path.name)
        # Launcher with no arguments: non-zero, JSON status blocked, launch_ready false.
        result = self.invoke()
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('blocked', result.stdout)
        self.assertFalse(json.loads(result.stdout)['launch_ready'])
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            legacy = root / 'legacy_candidate_release.json'
            legacy.write_text(json.dumps({
                'schema_version': 'qwen27b_release_v2', 'release_id': 'astra_support_repair_v2',
                'status': 'CANDIDATE', 'training_authorized': False, 'scope': 'source_support_only',
                'phases': {'cpt': {'train': [{'path': (
                    'D:/mev_bot-artifacts/north_star/aggregation/north_star_training_dataset_v1/'
                    'corpus_builder_v1/integrated_data_v1/full_history_closure_v1/economics_v1/'
                    'datasets/cpt_all.jsonl')}]}}}), encoding='utf-8')
            # Phase + release manifest but NO launch contract/contract sha -> blocked.
            result = self.invoke('--phase', 'cpt', '--release_manifest', legacy)
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn('blocked', result.stdout)
            self.assertFalse(json.loads(result.stdout)['launch_ready'])
            # Trainer with no --release_manifest: argparse requires it.
            trainer = subprocess.run([sys.executable, '-B', str(ROOT / 'train_qwen27b.py'),
                '--phase', 'cpt'], capture_output=True, text=True, timeout=120)
            self.assertNotEqual(trainer.returncode, 0, trainer.stdout + trainer.stderr)
            self.assertIn('--release_manifest', trainer.stderr)
            # Trainer at the legacy CANDIDATE manifest cannot reach real training:
            # admission is refused before any dataset path is opened.
            trainer = subprocess.run([sys.executable, '-B', str(ROOT / 'train_qwen27b.py'),
                '--phase', 'cpt', '--release_manifest', str(legacy)],
                capture_output=True, text=True, timeout=180)
            self.assertNotEqual(trainer.returncode, 0, trainer.stdout + trainer.stderr)
            self.assertIn('not APPROVED', trainer.stdout + trainer.stderr)

if __name__ == '__main__':
    unittest.main(verbosity=2)
