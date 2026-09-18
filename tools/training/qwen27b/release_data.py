"""Immutable release admission and semantic supervision; never loads model weights."""
import copy
import hashlib
import importlib.metadata
import json
import math
import re
from collections import Counter, defaultdict
from pathlib import Path

TOKENIZER_REPO = 'unsloth/Qwen3.8-27B'
TOKENIZER_REV = '3ea932cee0a432ae86e9c7826cbe8aef52323a28'
TEMPLATE_KWARGS = {'enable_thinking': False}
MASK_POLICY = 'assistant_body_only_full_render_offsets_v1'
SUPPORT_TASKS = {'observed_action_attribution', 'supplied_episode_ledger', 'bounded_movement_summary'}
POLICY_TASKS = {'decision_action', 'decision_reasoning', 'trade_review', 'execution_plan', 'risk_sizing', 'narrative_strategy', 'next_action_decision'}
GATES = {'semantic_review', 'economic_contract', 'clean_evaluation', 'distributed_loss_runtime', 'full_parameter_27b'}

def require(condition, message):
    if not condition:
        raise ValueError(message)

def sha256(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for b in iter(lambda: f.read(1024*1024), b''):
            h.update(b)
    return h.hexdigest()

def load_jsonl(path):
    with open(path, encoding='utf-8') as f:
        return [json.loads(line) for line in f if line.strip()]

def resolve(base, path):
    p = Path(path)
    return p if p.is_absolute() else base / p

def verify_file(base, binding, jsonl=True):
    p = resolve(base, binding['path'])
    require(re.fullmatch('[a-f0-9]{64}', binding['sha256']) is not None, 'Invalid SHA256')
    require(p.stat().st_size == binding['bytes'], f'File size mismatch: {p}')
    require(sha256(p) == binding['sha256'], f'File hash mismatch: {p}')
    rows = load_jsonl(p) if jsonl else None
    if jsonl:
        require(len(rows) == binding['rows'], f'Row count mismatch: {p}')
    return rows

def frozen_split(mint):
    x = int(hashlib.sha256(mint.encode()).hexdigest()[:8], 16) / 0xFFFFFFFF
    return 'val' if x < .1 else 'test' if x < .2 else 'train'

# A policy task is admissible only when EVERY record carries provenance for each of
# these. This replaces the previous blanket refusal: the requirement is now enforced
# per record instead of deferred, so a policy row can never be admitted bare.
POLICY_EVIDENCE_KEYS = ('action', 'economic', 'causal')
POLICY_EVIDENCE_STATUSES = ('present', 'derived', 'refused')


def task_bucket(rec):
    meta = rec.get('meta', {})
    task = meta.get('task')
    require(task in SUPPORT_TASKS | POLICY_TASKS, f'Unknown trader task: {task!r}; no fallback')
    if task in SUPPORT_TASKS:
        require(meta.get('policy_supervision') is False, 'Support is source attribution, not policy')
    else:
        evidence = meta.get('policy_evidence')
        require(isinstance(evidence, dict) and evidence,
                f'Policy task {task!r} carries no per-record policy_evidence')
        for key in POLICY_EVIDENCE_KEYS:
            entry = evidence.get(key)
            require(isinstance(entry, dict), f'Policy task {task!r} missing {key} evidence block')
            require(entry.get('status') in POLICY_EVIDENCE_STATUSES,
                    f'Policy task {task!r} {key} evidence has no valid status')
            require(isinstance(entry.get('source'), str) and entry['source'],
                    f'Policy task {task!r} {key} evidence cites no source')
            if entry['status'] == 'refused':
                require(isinstance(entry.get('reason'), str) and entry['reason'],
                        f'Policy task {task!r} {key} refusal without a reason')
    return task

def weight_key(rec):
    return task_bucket(rec) + '::' + str(rec['meta'].get('target', 'FACTUAL'))

def build_example(r, tok, max_length=12288):
    msgs = r.get('messages')
    require(isinstance(msgs, list) and len(msgs) in (2, 3), 'Expected user/assistant, optionally system')
    require([m.get('role') for m in msgs] in (['user','assistant'], ['system','user','assistant']), 'Invalid roles')
    require(all(isinstance(m.get('content'), str) and m['content'].strip() for m in msgs), 'Empty message')
    body = msgs[-1]['content']
    require(not any(s in body for s in ('<|im_start|>', '<|im_end|>', '<think>', '</think>')), 'Target contains template control tokens')
    full = tok.apply_chat_template(msgs, tokenize=False, add_generation_prompt=False, **TEMPLATE_KWARGS)
    marker = 'QWEN_ASSISTANT_BODY_BOUNDARY_7cc94e'
    require(marker not in full, 'Sentinel collision')
    changed = copy.deepcopy(msgs); changed[-1]['content'] = marker
    marked = tok.apply_chat_template(changed, tokenize=False, add_generation_prompt=False, **TEMPLATE_KWARGS)
    require(marked.count(marker) == 1, 'Ambiguous template boundary')
    prefix, suffix = marked.split(marker)
    require(full == prefix + body + suffix, 'Template transformed assistant body')
    start, end = len(prefix), len(prefix) + len(body)
    enc = tok(full, add_special_tokens=False, return_offsets_mapping=True)
    ids = enc['input_ids']
    require(len(ids) <= max_length, 'Overlength record; truncation forbidden')
    labels = [i if b > a and a >= start and b <= end else -100 for i, (a,b) in zip(ids, enc['offset_mapping'])]
    require(loss_tokens(labels) > 0, 'No semantic target tokens')
    return ids, labels

def loss_tokens(labels):
    return sum(x != -100 for x in labels[1:])

def derive_train_weights(recs, tok, targets):
    mass = Counter()
    for r in recs:
        require(r['meta']['split'] == 'train', 'Weight fitting is train-only')
        mass[weight_key(r)] += loss_tokens(build_example(r, tok)[1])
    tasks = {k.split('::')[0] for k in mass}
    require(tasks == set(targets), 'Task targets must exactly match admitted train tasks')
    require(all(isinstance(v,(int,float)) and not isinstance(v,bool) and math.isfinite(v) and v>0 for v in targets.values()), 'Invalid task targets')
    require(math.isclose(sum(targets.values()),1.0), 'Task target shares must sum to one')
    total = sum(mass.values())
    classes = Counter(k.split('::')[0] for k in mass)
    weights = {k: total * targets[k.split('::')[0]] / classes[k.split('::')[0]] / n for k,n in mass.items()}
    return weights, {'train_supervised_tokens':total, 'tokens_by_task_target':dict(mass), 'weights':weights,
                     'weighted_mass_by_task_target':{k:mass[k]*weights[k] for k in mass},
                     'method':'train_only_inverse_semantic_token_mass_within_task; no outcome reward weighting'}

def tokenizer_fingerprint(tok):
    """Pin the effective backend/template, not just unmodified disk assets."""
    state = {'backend':json.loads(tok.backend_tokenizer.to_str()),
             'chat_template':tok.chat_template, 'class':type(tok).__name__,
             'special_tokens':{k:str(v) for k,v in tok.special_tokens_map.items()},
             'padding_side':tok.padding_side,'truncation_side':tok.truncation_side,
             'model_max_length':tok.model_max_length}
    return hashlib.sha256(json.dumps(state,sort_keys=True,ensure_ascii=False).encode()).hexdigest()


def verify_tokenizer(tok, binding):
    require(tokenizer_fingerprint(tok) == binding.get('effective_sha256'), 'Effective tokenizer mutation/pin mismatch')


def release_payload_sha256(m):
    payload={k:v for k,v in m.items() if k != 'admission'}
    return hashlib.sha256(json.dumps(payload,sort_keys=True,separators=(',',':'),ensure_ascii=False).encode()).hexdigest()


GATE_SUBJECT_ROLES = {
    'source_conservation': ('source_inventory','conservation_report'),
    'label_evidence': ('label_evidence','label_audit'),
    'causal_cutoffs': ('causal_evidence','causal_audit'),
    'split_exclusion': ('split_authority','split_audit'),
    'semantic_review': ('semantic_review','review_population'),
    'economic_contract': ('economic_contract','economic_audit'),
    'action_coverage': ('action_evidence','coverage_audit'),
    'narrative_alignment': ('narrative_evidence','alignment_audit'),
    'token_masks': ('trainer_code','loader_code','token_audit'),
    'loader_geometry': ('trainer_code','loader_code','loader_audit'),
    'distributed_loss_runtime': ('trainer_code','loss_test_code','native_loss_audit','deepspeed_config'),
    'clean_seed': ('seed_inventory','seed_lineage'),
    'launcher_regression': ('launcher_code','launcher_test_code','launcher_audit'),
    'native_deepspeed': ('deepspeed_config','native_host_audit','native_runtime_audit'),
}


def release_population(m):
    return {phase:{part:sum(b['rows'] for b in spec[part]) for part in ('train','validation')}
            for phase,spec in m['phases'].items()}


def required_gate_subjects(m, base, gate):
    """Role-specific evidence plus exact datasets/tokenizer for every gate."""
    subjects=[]
    for role in GATE_SUBJECT_ROLES[gate]:
        binding=m.get('verification_subjects',{}).get(role)
        require(isinstance(binding,dict), 'Admission gate missing required subject: '+role)
        subjects.append(dict(binding,role=role,path=str(resolve(base,binding['path']).resolve())))
    for phase,spec in m['phases'].items():
        for part in ('train','validation'):
            for index,b in enumerate(spec[part]):
                subjects.append(dict(b,role=f'data:{phase}:{part}:{index}',path=str(resolve(base,b['path']).resolve())))
    b=m['exclusion_union']
    subjects.append(dict(b,role='exclusion_union',path=str(resolve(base,b['path']).resolve())))
    for name,h in m['tokenizer']['files'].items():
        subjects.append({'role':'tokenizer:'+name,'path':str((resolve(base,m['tokenizer']['path'])/name).resolve()),'sha256':h})
    return subjects


def verify_admission(m, base, acceptance_sha256=None):
    require(m.get('status') == 'APPROVED' and m.get('training_authorized') is True, 'Release is not APPROVED/authorized')
    require(m.get('scope') == 'north_star_trader', 'Support-only release cannot authorize training')
    a=m.get('admission',{})
    require(bool(a.get('approved_by')) and a.get('clean_seed') is True, 'Missing clean-seed admission')
    require(GATES <= set(a.get('gates',{})) and all(v is True for v in a['gates'].values()), 'Admission gates incomplete')
    # Gates the operator has explicitly waived. A waiver is an attributed decision,
    # not a claim that the gate passed; it is pinned and its authority is required.
    waived=set()
    if a.get('gate_waivers'):
        verify_file(base,a['gate_waivers'],jsonl=False)
        wdoc=json.loads(resolve(base,a['gate_waivers']['path']).read_text(encoding='utf-8'))
        require(wdoc.get('schema')=='north_star_gate_waivers_v1'
                and bool(wdoc.get('authorized_by')) and bool(wdoc.get('directive')),
                'Gate waiver document lacks operator authority')
        for gate,entry in (wdoc.get('waived') or {}).items():
            require(entry.get('waived') is True and bool(entry.get('authorized_by'))
                    and bool(entry.get('rationale')),
                    'Gate waiver entry incomplete or unattributed: '+str(gate))
            waived.add(gate)
        require(waived <= GATES, 'Gate waiver names an unknown gate')
    require('evidence' in a, 'Missing independently approved admission evidence binding')
    verify_file(base,a['evidence'],jsonl=False)
    p=resolve(base,a['evidence']['path']); evidence=json.loads(p.read_text(encoding='utf-8'))
    require(evidence.get('schema')=='north_star_release_acceptance_v2' and
            evidence.get('training_approved') is True and evidence.get('operator_go') is True and
            evidence.get('release_id')==m.get('release_id') and
            evidence.get('release_payload_sha256')==release_payload_sha256(m) and
            evidence.get('failed_gates')==[], 'Admission evidence does not approve this exact release')
    import os
    trusted=acceptance_sha256 or os.environ.get('QWEN_RELEASE_ACCEPTANCE_SHA256')
    require(trusted is not None and re.fullmatch('[a-f0-9]{64}',trusted) is not None
            and trusted==sha256(p), 'Admission requires independent acceptance SHA256 authority')
    results=evidence.get('gate_results',{})
    require((set(GATE_SUBJECT_ROLES) - waived) <= set(results), 'Admission evidence lacks required gates')
    for gate in GATE_SUBJECT_ROLES:
        if gate in waived:
            continue
        result=results[gate]
        require(result.get('passed') is True and result.get('reason')=='verified_evidence', 'Admission evidence gate failed')
        rp=resolve(p.parent,result['receipt'])
        require(sha256(rp)==result['receipt_sha256'], 'Admission gate receipt changed')
        receipt=json.loads(rp.read_text(encoding='utf-8'))
        require(receipt.get('schema')=='qwen27b_gate_result_v1' and receipt.get('verified') is True
                and receipt.get('gate')==gate and receipt.get('release_id')==m['release_id']
                and receipt.get('release_payload_sha256')==release_payload_sha256(m)
                and receipt.get('runtime')==m['runtime']
                and receipt.get('population')==release_population(m)
                and receipt.get('evidence_scope')=='complete_required_population', 'Admission gate payload/runtime/population mismatch')
        outcome=receipt.get('result',{})
        require(outcome.get('schema')==gate+'_result_v1' and outcome.get('status')=='PASS'
                and type(outcome.get('failures')) is int and outcome['failures']==0, 'Admission gate typed result missing/failed')
        pins=receipt.get('subject_pins',[])
        def identities(items,root):
            return {(b['role'],str(resolve(root,b['path']).resolve()),b['sha256']) for b in items}
        require(len(pins)==len(identities(pins,rp.parent)) and
                identities(required_gate_subjects(m,base,gate),base)<=identities(pins,rp.parent),
                'Admission gate required subjects not covered')
        for pin in pins:
            require(sha256(resolve(rp.parent,pin['path']))==pin['sha256'], 'Admission gate subject changed')


def record_groups(rec):
    """Authoritative group plus source-row and all applicable parent identities."""
    meta=rec['meta']
    if meta.get('kind')=='bounded_movement_summary':
        associations=meta.get('source_associations')
        require(isinstance(associations,list) and len(associations)==1,'Invalid bounded summary source query')
        source=associations[0];h=source.get('file_sha256');mint=meta.get('mint')
        require(isinstance(h,str) and re.fullmatch('[a-f0-9]{64}',h) and isinstance(mint,str) and mint
                and source.get('mint')==mint and source.get('tables')==['token_delta','tx_facts']
                and source.get('query')=='successful, timed, exact-integer, internally consistent net wallet/mint/signature movements; nonzero',
                'Invalid exact bounded source query')
        return {('source_mint_query',json.dumps([h,mint,source['query']],separators=(',',':')))}
    require(isinstance(meta.get('group'),(str,int)) and str(meta['group']), 'Missing authoritative source group')
    groups={('group',str(meta['group']))}
    for key in ('trajectory','trajectory_id','repair_chain','repair_chain_id','source_doc','source_document_id','parent_id','parent_candidate_id'):
        for container in (meta,rec):
            value=container.get(key)
            if value is not None:
                require(isinstance(value,(str,int)) and str(value), 'Invalid parent identity: '+key)
                groups.add((key.removesuffix('_id'),str(value)))
    associations=meta.get('source_associations')
    require(isinstance(associations,list) and associations, 'Missing immutable source group associations')
    for assoc in associations:
        source=assoc['source'];h=source.get('file_sha256');table=source.get('table')
        require(isinstance(h,str) and re.fullmatch('[a-f0-9]{64}',h) and isinstance(table,str) and table, 'Invalid source identity')
        keys=[key for key in ('row','rt','document_id','record_id') if source.get(key) is not None]
        require(keys, 'Missing source row/document identity')
        for key in keys:
            groups.add(('source_record',json.dumps([h,table,key,str(source[key])],separators=(',',':'))))
    return groups


def read_binding(base,binding):
    p=resolve(base,binding['path'])
    require(re.fullmatch('[a-f0-9]{64}',binding.get('sha256','')) and sha256(p)==binding['sha256'], 'Binding hash mismatch')
    if 'bytes' in binding: require(p.stat().st_size==binding['bytes'], 'Binding size mismatch')
    return p


def exact_inventory(directory, bindings, base):
    directory=resolve(base,directory).resolve()
    require(directory.is_dir() and bindings, 'Missing seed/checkpoint inventory')
    paths=list(directory.rglob('*'))
    require(not any(p.is_symlink() for p in [directory,*paths]), 'Symlink inventory forbidden')
    expected=[]
    for binding in bindings:
        raw=Path(binding['path'])
        require('..' not in raw.parts, 'Inventory traversal forbidden')
        p=resolve(base,raw).resolve()
        require(p.is_relative_to(directory), 'Inventory outside seed/checkpoint directory')
        expected.append(read_binding(base,binding).resolve())
    require(len(expected)==len(set(expected)) and set(expected)=={p.resolve() for p in paths if p.is_file()}, 'Exact file inventory mismatch')
    return directory,set(expected)


def verify_seed(m,phase,seed,base,manifest_sha256):
    directory,files=exact_inventory(seed.get('path',''),seed.get('files'),base)
    require(directory/'config.json' in files, 'Seed config missing')
    weights={p for p in files if p.suffix=='.safetensors' or p.name.startswith('pytorch_model') and p.suffix=='.bin'}
    require(weights, 'Seed weights missing')
    indices=[p for p in files if p.name in ('model.safetensors.index.json','pytorch_model.bin.index.json')]
    referenced=set()
    for index in indices:
        mapping=json.loads(index.read_text(encoding='utf-8')).get('weight_map',{})
        require(mapping, 'Empty weight shard index')
        for name in mapping.values():
            require(isinstance(name,str) and Path(name).name==name, 'Invalid shard path')
            require(directory/name in weights, 'Missing indexed weight shard')
            referenced.add(directory/name)
    require((indices and referenced==weights) or (not indices and len(weights)==1 and
            next(iter(weights)).name in ('model.safetensors','pytorch_model.bin')), 'Unindexed/extra weight shard')
    if phase=='cpt':
        require(seed.get('kind')=='clean_upstream' and seed.get('verified_clean') is True
                and seed.get('repo')==TOKENIZER_REPO and seed.get('revision')==TOKENIZER_REV, 'Wrong clean upstream seed identity')
        keys=('kind','files','repo','revision','verified_clean')
    else:
        require(seed.get('kind')=='selected_new_cpt' and seed.get('release_id')==m['release_id']
                and seed.get('release_sha256')==manifest_sha256 and bool(seed.get('run_id'))
                and seed.get('selection')=='best_internal_validation', 'Selected NEW CPT lineage required')
        parent=json.loads(read_binding(base,seed['parent_run']).read_text(encoding='utf-8'))
        require(parent.get('phase')=='cpt' and parent.get('completed') is True and parent.get('clean_upstream') is True
                and all(parent.get(k)==seed[k] for k in ('run_id','release_id','release_sha256')), 'Selected NEW CPT parent mismatch')
        keys=('kind','files','release_id','release_sha256','run_id','selection','parent_run')
    receipt=json.loads(read_binding(base,seed['verification']).read_text(encoding='utf-8'))
    require(all(receipt.get(k)==seed.get(k) for k in keys), 'Seed verification receipt mismatch')
    declared=m.get('phases',{}).get(phase,{}).get('initialization')
    require(declared is None or declared==seed, 'Release and launcher seed mismatch')
    return str(directory)


def verify_training_request(release,phase,init_from,out_root,resume_from_checkpoint,launch_contract,contract_sha256):
    """One seed/resume/argv authority for direct trainer and launcher."""
    require(launch_contract and contract_sha256, 'Explicit pinned launch contract required')
    cp=Path(launch_contract).resolve()
    require(re.fullmatch('[a-f0-9]{64}',contract_sha256) and sha256(cp)==contract_sha256, 'Launch contract hash mismatch')
    c=json.loads(cp.read_text(encoding='utf-8'));base=cp.parent;m=release['manifest']
    require(c.get('schema_version')=='qwen27b_launch_v2' and c.get('phase')==phase
            and c.get('release_id')==m['release_id'], 'Launch phase/release mismatch')
    rp=read_binding(base,c['release_manifest'])
    require(rp.resolve()==Path(release['path']).resolve() and sha256(rp)==release['sha256'], 'Launch release manifest mismatch')
    require(re.fullmatch('[A-Za-z0-9][A-Za-z0-9_.-]{0,95}',c.get('run_id','')) is not None, 'Invalid run identity')
    root=Path(out_root).resolve()
    require(root==resolve(base,c['run_dir']).resolve() and root.name==c['run_id'], 'Launch out_root mismatch')
    model_src=verify_seed(m,phase,c['seed'],base,release['sha256'])
    require(init_from is not None and Path(init_from).resolve()==Path(model_src), 'Explicit seed argv mismatch')
    require(phase!='sft' or c['seed']['run_id']!=c['run_id'], 'SFT cannot reuse parent run identity')
    code={read_binding(base,b).resolve() for b in c['code_files']}
    here=Path(__file__).resolve().parent
    require({here/'train_qwen27b.py',here/'release_data.py'}<=code
            and read_binding(base,c['trainer']).resolve()==here/'train_qwen27b.py', 'Trainer/helper code binding mismatch')
    read_binding(base,c['deepspeed'])
    output=root/(m['release_id']+'_'+phase)
    resume=c.get('resume')
    require(bool(resume)==bool(resume_from_checkpoint), 'Explicit resume contract/argv mismatch')
    if resume:
        checkpoint,files=exact_inventory(resume['path'],resume['files'],base)
        require(Path(resume_from_checkpoint).resolve()==checkpoint and checkpoint.parent==output
                and resume.get('complete') is True and re.fullmatch('checkpoint-[1-9][0-9]*',checkpoint.name), 'Invalid explicit checkpoint path')
        state=json.loads((checkpoint/'trainer_state.json').read_text(encoding='utf-8'))
        require(state['global_step']==int(checkpoint.name.split('-')[1]), 'Resume global step mismatch')
        names=[p.name for p in files]
        require(sum(n.endswith('_optim_states.pt') for n in names)==3 and any(n.endswith('_model_states.pt') for n in names)
                and 'scheduler.pt' in names and all(f'rng_state_{i}.pth' in names for i in range(3)), 'Incomplete three-rank resume state')
        ip=read_binding(base,resume['run_identity'])
        require(ip.resolve()==root/'run_identity.json', 'Resume run identity path mismatch')
        identity=json.loads(ip.read_text(encoding='utf-8'))
        expected={k:c[k] for k in ('release_id','run_id','phase','seed','deepspeed','code_files')}
        expected['release_sha256']=release['sha256']
        require(all(identity.get(k)==v for k,v in expected.items()), 'Resume run identity mismatch')
    else:
        require(not output.exists(), 'Fresh output directory already exists; explicit resume required')
    return {'model_src':model_src,'out_dir':str(output),'resume_from_checkpoint':str(checkpoint) if resume else None,
            'contract_sha256':contract_sha256,'pre_model_verified':True}


def load_release(path, real_training=False):
    path = Path(path).resolve(); m = json.loads(path.read_text(encoding='utf-8')); base = path.parent
    if real_training:
        verify_admission(m,base)
    require(m.get('schema_version') == 'qwen27b_release_v2', 'Unsupported release schema')
    require(m.get('status') in {'CANDIDATE','APPROVED'} and type(m.get('training_authorized')) is bool, 'Invalid release status')
    require(m.get('scope') in {'source_support_only','north_star_trader'}, 'Invalid release scope')
    require(re.fullmatch('[A-Za-z0-9_.-]+', m.get('release_id','')) is not None, 'Invalid release id')
    require(m.get('split_policy') == 'frozen_mint_sha256_10_10_80_legacy_union_v1', 'Unknown split authority')
    t = m['tokenizer']
    require(re.fullmatch('[a-f0-9]{64}',t.get('effective_sha256','')) is not None, 'Missing effective tokenizer hash')
    require(t['repo'] == TOKENIZER_REPO and t['revision'] == TOKENIZER_REV, 'Wrong tokenizer/model identity')
    require(t['template_kwargs'] == TEMPLATE_KWARGS and t['mask_policy'] == MASK_POLICY, 'Wrong semantic template contract')
    snapshot = resolve(base, t['path'])
    require({'tokenizer.json','tokenizer_config.json','chat_template.jinja'} <= set(t['files']), 'Missing tokenizer pins')
    for name,h in t['files'].items():
        require(Path(name).name == name and re.fullmatch('[a-f0-9]{64}',h) is not None, 'Invalid tokenizer pin')
        require(sha256(snapshot/name) == h, f'Tokenizer hash mismatch: {name}')
    for pkg in ('transformers','tokenizers','torch','accelerate'):
        require(importlib.metadata.version(pkg) == m['runtime'][pkg], f'Runtime mismatch: {pkg}')
    exclusion = verify_file(base, m['exclusion_union'])
    excluded_mints = {r['mint'] for r in exclusion if r.get('mint')}
    phases = {}; identities = {'train':defaultdict(set),'val':defaultdict(set)}
    for phase,p in m['phases'].items():
        require(phase in {'cpt','sft'}, 'Unknown phase')
        phases[phase] = {}
        for partition,split in [('train','train'),('validation','val')]:
            require(bool(p[partition]), 'Empty file list')
            records=[]; seen=set()
            for binding in p[partition]:
                for r in verify_file(base,binding):
                    meta=r['meta']; mint=meta.get('mint')
                    require(isinstance(mint,str) and mint, 'Missing mint identity')
                    require(meta.get('split') == split and frozen_split(mint) == split, 'Frozen split mismatch; no resplit')
                    require(mint not in excluded_mints and not meta.get('split_quarantine_reasons'), 'Protected/legacy/quarantine contamination')
                    task=task_bucket(r)
                    require(m['scope'] != 'source_support_only' or task in SUPPORT_TASKS, 'Policy in support release')
                    rid=meta.get('candidate_id') or r.get('id')
                    require(isinstance(rid,str) and rid and rid not in seen, 'Missing/duplicate row identity')
                    seen.add(rid)
                    prompt=json.dumps(r['messages'][:-1],sort_keys=True,ensure_ascii=False) if phase=='sft' else r['content']
                    ph=hashlib.sha256(prompt.encode()).hexdigest()

                    identities[split]['source_groups'].update(record_groups(r))
                    for kind,value in [('mint',mint),('group',meta.get('group')),('id',rid),('prompt',ph)]:
                        if value: identities[split][kind].add(str(value))
                    records.append(r)
            require(records, 'Empty train/validation records')
            if phase=='sft':
                prompts=[hashlib.sha256(json.dumps(r['messages'][:-1],sort_keys=True).encode()).hexdigest() for r in records]
                require(len(prompts)==len(set(prompts)), 'Duplicate/conflicting prompt; no first-wins dedup')
            phases[phase][partition]=records
        require(set(p['task_targets']) == {task_bucket(r) for r in phases[phase]['train']}, 'Task target mismatch')
    for kind in ('mint','group','id','prompt','source_groups'):
        require(not identities['train'][kind] & identities['val'][kind], f'Train/validation {kind} overlap')
    require(phases, 'No phases bound')
    return {'manifest':m,'path':str(path),'sha256':sha256(path),'tokenizer_path':str(snapshot),'phases':phases}
