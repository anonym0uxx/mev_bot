"""CPU contract tests: real tokenizer/loader, no model/optimizer/training."""
import copy
import hashlib
import json
from pathlib import Path
from types import SimpleNamespace
import pytest
import torch
from transformers import AutoTokenizer
import train_qwen27b as trainer

SNAP = Path('C:/Users/Alon/.cache/huggingface/hub/models--unsloth--Qwen3.8-27B/snapshots/3ea932cee0a432ae86e9c7826cbe8aef52323a28')

@pytest.fixture(scope='module')
def tok():
    return AutoTokenizer.from_pretrained(str(SNAP), local_files_only=True)

def row(content='SOURCE ATTRIBUTION: BUY_RECORDED', task='observed_action_attribution'):
    return {'messages':[{'role':'user','content':'Attribute this supplied source record.'}, {'role':'assistant','content':content}], 'meta':{'task':task,'policy_supervision':False,'target':'BUY_RECORDED','split':'train','mint':'mint-fixture','candidate_id':'fixture'}}

def test_task_identity_has_no_retention_fallback():
    assert trainer.task_bucket(row()) == 'observed_action_attribution'
    with pytest.raises(ValueError): trainer.task_bucket({'id':'sft_rust_1'})
    with pytest.raises(ValueError): trainer.task_bucket(row(task='rust'))

def test_full_render_mask_is_only_assistant_body(tok):
    r=row('SOURCE ATTRIBUTION: BUY_RECORDED\nEvidence: café 中文.'); ids,labels=trainer.build_example(r,tok)
    rendered=tok.apply_chat_template(r['messages'], tokenize=False, add_generation_prompt=False, enable_thinking=False)
    encoding=tok(rendered,add_special_tokens=False,return_offsets_mapping=True)
    body=r['messages'][-1]['content']; start=rendered.index(body); end=start+len(body)
    assert ids == encoding['input_ids']
    expected=[i if b>a and a>=start and b<=end else -100 for i,(a,b) in zip(ids,encoding['offset_mapping'])]
    assert labels == expected
    assert trainer.loss_tokens(labels) == sum(x!=-100 for x in expected[1:])
    assert '<think>' not in tok.decode([x for x in labels if x!=-100])

def test_overlength_and_empty_target_fail_closed(tok):
    with pytest.raises(ValueError): trainer.build_example(row(''),tok)
    with pytest.raises(ValueError): trainer.build_example(row('long '*15000),tok)

def test_token_mass_weights_and_frozen_validation(tok):
    tr=[row('A'),row('A much longer attributed source record with factual evidence.')]
    tr[1]['meta']['target']='SELL_RECORDED'
    weights,stats=trainer.derive_train_weights(tr,tok,{'observed_action_attribution':1.0})
    ds=trainer.SFTDataset(tr,tok,shuffle=False,task_weights=weights)
    mass=[float(ds[i]['task_weight'])*trainer.loss_tokens(ds[i]['labels'].tolist()) for i in range(len(ds))]
    assert mass[0] == pytest.approx(mass[1],rel=1e-6)
    va=copy.deepcopy(tr); va[0]['meta']['split']='val'
    with pytest.raises(ValueError): trainer.derive_train_weights(va,tok,{'observed_action_attribution':1.0})
    altered=copy.deepcopy(tr); altered[0]['meta'].update({'pnl_lamports':999999,'class_weight':300.0})
    assert trainer.derive_train_weights(altered,tok,{'observed_action_attribution':1.0})[0]==weights
    assert all(float(trainer.SFTDataset(tr,tok,shuffle=False)[i]['task_weight'])==1.0 for i in range(2))

def test_cpt_keeps_short_tail(tok):
    docs=[{'content':'short source document'}]
    ds=trainer.PackedCPTDataset(docs,tok,shuffle=False)
    assert ds.blocks == [tok(docs[0]['content'],add_special_tokens=False)['input_ids']+[tok.eos_token_id]]

def test_candidate_refused_before_tokenizer_or_model(tmp_path,monkeypatch):
    p=tmp_path/'candidate.json'; p.write_text(json.dumps({'schema_version':'qwen27b_release_v2','status':'CANDIDATE','training_authorized':False}))
    def forbidden(*a,**k): raise AssertionError('tokenizer/model loader must not run')
    monkeypatch.setattr(trainer.AutoTokenizer,'from_pretrained',forbidden)
    monkeypatch.setattr(trainer.AutoModelForCausalLM,'from_pretrained',forbidden)
    with pytest.raises(ValueError,match='approved|APPROVED|authorized'):
        trainer.main(['--phase','sft','--release_manifest',str(p),'--out_root',str(tmp_path/'run')])

def test_prepare_real_loader_and_report(tok):
    tr=[row()]; va=[row('A validation observation')]; va[0]['meta']['split']='val'
    release={'manifest':{'release_id':'fixture','phases':{'sft':{'task_targets':{'observed_action_attribution':1.0}}},
                         'tokenizer':{'effective_sha256':trainer.tokenizer_fingerprint(tok)}},
             'sha256':'f'*64,'phases':{'sft':{'train':tr,'validation':va}}}
    train,val,geometry=trainer.prepare_datasets(release,'sft',tok)
    report=trainer.dataset_report(release,'sft',train,val,geometry)
    assert report['train']['records']==1 and report['validation']['records']==1
    assert report['train']['shifted_loss_tokens']==trainer.loss_tokens(train[0]['labels'].tolist())
    assert report['training_authorized'] is False
    assert report['step_plan']['marks_steps'][-1]==report['step_plan']['total_steps_max']


def test_effective_tokenizer_mutation_rejected(tok):
    import release_data
    clone=copy.deepcopy(tok)
    pinned=release_data.tokenizer_fingerprint(clone)
    clone.chat_template += '\nMUTATED'
    with pytest.raises(ValueError,match='Effective tokenizer'):
        release_data.verify_tokenizer(clone,{'effective_sha256':pinned})


def test_self_asserted_policy_flags_never_admit():
    # Self-asserted booleans still cannot admit a policy row. The gate now demands
    # per-record evidence provenance instead of refusing every policy task outright.
    r=row(task='decision_action')
    r['meta'].update(policy_supervision=True,authenticated_policy_scope=True,target='BUY')
    with pytest.raises(ValueError,match='policy_evidence'):
        trainer.task_bucket(r)


def test_self_asserted_evidence_shaped_booleans_never_admit():
    # Claiming an evidence block without provenance is not evidence.
    r=row(task='decision_action')
    r['meta']['policy_evidence']={'action':True,'economic':True,'causal':True}
    with pytest.raises(ValueError,match='evidence'):
        trainer.task_bucket(r)


def test_policy_evidence_must_cite_an_artifact_for_derived_claims():
    r=row(task='decision_action')
    r['meta']['policy_evidence']={
        'action':{'status':'present','source':'labels.sqlite'},
        'economic':{'status':'derived'},                      # no source cited
        'causal':{'status':'present','source':'strict cutoff'}}
    with pytest.raises(ValueError,match='cites no source'):
        trainer.task_bucket(r)


def test_source_export_task_name_is_preserved():
    assert trainer.task_bucket(row(task='bounded_movement_summary'))=='bounded_movement_summary'


def test_arbitrary_evidence_boolean_cannot_approve(tmp_path):
    import release_data
    evidence=tmp_path/'evidence.json'; evidence.write_text(json.dumps({'verified':True}))
    m={'status':'APPROVED','training_authorized':True,'scope':'north_star_trader','release_id':'fixture',
       'admission':{'approved_by':'fixture','clean_seed':True,'gates':dict.fromkeys(release_data.GATES,True),
                    'evidence':{'path':str(evidence),'bytes':evidence.stat().st_size,'sha256':release_data.sha256(evidence)}}}
    with pytest.raises(ValueError,match='evidence|Evidence'):
        release_data.verify_admission(m,tmp_path)


def test_numeric_weighted_window_uses_actual_trainer_methods():
    obj=object.__new__(trainer.WeightedLossTrainer)
    obj.args=SimpleNamespace(average_tokens_across_devices=True,world_size=1,n_gpu=0)
    obj.accelerator=SimpleNamespace(gather=lambda x:x,num_processes=1)
    obj._denom_stats=[];obj._startup_denom_ok=False;obj._startup_numer_ok=False
    labels=torch.tensor([[-100,1,2,-100],[-100,2,1,0]])
    weights=torch.tensor([0.25,3.0])
    logits=torch.arange(24,dtype=torch.float32).reshape(2,4,3)%7
    samples=[{'labels':labels[i:i+1],'task_weight':weights[i:i+1]} for i in range(2)]
    denom=obj._get_num_items_in_batch(samples,torch.device('cpu'))
    class LogitFixture:
        training=False
        def __init__(self,x): self.x=x
        def __call__(self,**kw): return SimpleNamespace(logits=self.x)
    with torch.no_grad():
        loss=sum(obj.compute_loss(LogitFixture(logits[i:i+1]),dict(samples[i]),num_items_in_batch=denom) for i in range(2))
        ce=torch.nn.functional.cross_entropy(logits[:,:-1].transpose(1,2),labels[:,1:],ignore_index=-100,reduction='none')
        expected=(ce.sum(1)*weights).sum()/(labels[:,1:].ne(-100).sum(1)*weights).sum()
    assert float(loss)==pytest.approx(float(expected),rel=1e-6)
    assert float(denom)==pytest.approx(9.5)
    assert not torch.cuda.is_initialized()


@pytest.mark.parametrize('phase,resuming',[('cpt',False),('sft',False),('cpt',True)])
def test_explicit_seed_resume_pre_model_cli(tmp_path,monkeypatch,phase,resuming):
    import release_data as rd
    s=seed_fixture(tmp_path);m={'release_id':'fixture','phases':{phase:{}}}
    manifest=tmp_path/'manifest.json';manifest.write_text(json.dumps(m));mh=rd.sha256(manifest)
    if phase=='sft':
        s.update(kind='selected_new_cpt',release_id='fixture',release_sha256=mh,run_id='parent_cpt',selection='best_internal_validation')
        parent=tmp_path/'parent.json';parent.write_text(json.dumps(dict(phase='cpt',completed=True,clean_upstream=True,run_id='parent_cpt',release_id='fixture',release_sha256=mh)))
        s['parent_run']=pin(parent)
        receipt=tmp_path/'selected.json';receipt.write_text(json.dumps(s));s['verification']=pin(receipt)
    root=tmp_path/'fresh_run';ds=tmp_path/'ds.json';ds.write_text('{}')
    c={'schema_version':'qwen27b_launch_v2','release_id':'fixture','phase':phase,'run_id':'fresh_run','run_dir':str(root),
       'release_manifest':pin(manifest),'seed':s,'trainer':pin(Path(trainer.__file__)),
       'code_files':[pin(Path(trainer.__file__)),pin(Path(rd.__file__))],'deepspeed':pin(ds)}
    checkpoint=None
    if resuming:
        checkpoint=root/('fixture_'+phase)/'checkpoint-1';checkpoint.mkdir(parents=True)
        for name in ['scheduler.pt','zero_model_states.pt']+[f'{i}_optim_states.pt' for i in range(3)]+[f'rng_state_{i}.pth' for i in range(3)]:
            (checkpoint/name).write_bytes(b'fixture not loaded')
        (checkpoint/'trainer_state.json').write_text(json.dumps({'global_step':1}))
        identity={k:c[k] for k in ('release_id','run_id','phase','seed','deepspeed','code_files')};identity['release_sha256']=mh
        ip=root/'run_identity.json';ip.write_text(json.dumps(identity))
        c['resume']={'path':str(checkpoint),'files':[pin(p) for p in checkpoint.iterdir()],'complete':True,'run_identity':pin(ip)}
    cp=tmp_path/'launch.json';cp.write_text(json.dumps(c))
    release={'manifest':m,'path':str(manifest),'sha256':mh,'phases':{phase:{}}}
    monkeypatch.setattr(trainer,'load_release',lambda *a,**kw:release)
    def forbidden(*a,**kw):raise AssertionError('preflight must not load tokenizer/model')
    monkeypatch.setattr(trainer.AutoTokenizer,'from_pretrained',forbidden)
    monkeypatch.setattr(trainer.AutoModelForCausalLM,'from_pretrained',forbidden)
    argv=['--phase',phase,'--release_manifest',str(manifest),'--init_from',s['path'],'--out_root',str(root),
          '--launch_contract',str(cp),'--contract_sha256',rd.sha256(cp),'--preflight_only']
    if checkpoint:argv+=['--resume_from_checkpoint',str(checkpoint)]
    result=trainer.main(argv)
    assert result['model_src']==str(Path(s['path']).resolve())
    assert result['resume_from_checkpoint']==(str(checkpoint.resolve()) if checkpoint else None)
    assert result['out_dir']==str(root/('fixture_'+phase))
    bad=list(argv);bad[bad.index('--init_from')+1]=str(tmp_path/'wrong')
    with pytest.raises(ValueError,match='seed argv'):trainer.main(bad)
    if checkpoint:
        with pytest.raises(ValueError,match='resume'):trainer.main(argv[:-2])
    else:
        Path(result['out_dir']).mkdir(parents=True)
        with pytest.raises(ValueError,match='resume'):trainer.main(argv)


@pytest.mark.parametrize("phase,resume",[("cpt",False),("cpt",True),("sft",False)])
def test_real_path_reaches_exact_validated_model(monkeypatch,tmp_path,phase,resume):
    request={'model_src':str(tmp_path/'seed'),'out_dir':str(tmp_path/'existing'),'resume_from_checkpoint':'checkpoint' if resume else None}
    Path(request['out_dir']).mkdir()
    (Path(request['out_dir'])/'state').write_text('existing resume fixture')
    release={'phases':{phase:{}},'tokenizer_path':'fixture','manifest':{'release_id':'fixture'}}
    monkeypatch.setattr(trainer,'load_release',lambda *a,**k:release)
    monkeypatch.setattr(trainer,'verify_training_request',lambda *a,**k:request)
    monkeypatch.setattr(trainer.AutoTokenizer,'from_pretrained',lambda *a,**k:object())
    monkeypatch.setattr(trainer,'prepare_datasets',lambda *a:([],[],{}))
    class BoundaryReached(Exception):pass
    def boundary(path,**kwargs):
        assert path==request['model_src']
        raise BoundaryReached()
    monkeypatch.setattr(trainer.AutoModelForCausalLM,'from_pretrained',boundary)
    argv=['--phase',phase,'--release_manifest','fixture','--init_from',request['model_src'],'--out_root',str(tmp_path)]
    if resume:argv+=['--resume_from_checkpoint','checkpoint']
    with pytest.raises(BoundaryReached):trainer.main(argv)


def test_eval_loop_is_global_token_mean(tmp_path):
    from transformers import PretrainedConfig, TrainingArguments
    class FixedLogits(torch.nn.Module):
        def __init__(self):
            super().__init__(); self.anchor=torch.nn.Parameter(torch.zeros(())); self.config=PretrainedConfig()
        def forward(self,input_ids,attention_mask=None,**kw):
            logits=torch.stack([input_ids.float(),-input_ids.float()],dim=-1)+self.anchor
            return SimpleNamespace(logits=logits)
    recs=[{'input_ids':torch.tensor([1,1]),'labels':torch.tensor([-100,0]),'attention_mask':torch.ones(2,dtype=torch.long),'task_weight':torch.tensor(1.)},
          {'input_ids':torch.tensor([2,2,2,2,2]),'labels':torch.tensor([-100,1,1,1,1]),'attention_mask':torch.ones(5,dtype=torch.long),'task_weight':torch.tensor(1.)}]
    model=FixedLogits()
    args=TrainingArguments(output_dir=str(tmp_path),use_cpu=True,report_to=[],per_device_eval_batch_size=1,remove_unused_columns=False)
    obj=trainer.WeightedLossTrainer(model=model,args=args,eval_dataset=recs,data_collator=lambda b:trainer.collate(b,0))
    expected=(torch.nn.functional.softplus(torch.tensor(-2.))+4*torch.nn.functional.softplus(torch.tensor(4.)))/5
    result=obj.evaluate()
    assert result['eval_loss']==pytest.approx(float(expected),rel=1e-6)
    assert result['eval_loss_tokens']==5


def pin(p):
    import release_data as rd
    return {'path':str(p),'bytes':p.stat().st_size,'sha256':rd.sha256(p)}


def seed_fixture(tmp_path,phase='cpt'):
    import release_data as rd
    d=tmp_path/'seed';d.mkdir()
    (d/'config.json').write_text('{}');(d/'model.safetensors').write_bytes(b'fixture only, never model-loaded')
    s={'path':str(d),'kind':'clean_upstream','repo':rd.TOKENIZER_REPO,'revision':rd.TOKENIZER_REV,'verified_clean':True,
       'files':[pin(p) for p in d.iterdir()]}
    receipt=tmp_path/'seed-verification.json';receipt.write_text(json.dumps(s));s['verification']=pin(receipt)
    return s


def test_exact_seed_inventory_and_clean_identity(tmp_path):
    import release_data as rd
    s=seed_fixture(tmp_path)
    assert rd.verify_seed({'release_id':'fixture'},'cpt',s,tmp_path,'a'*64)==str((tmp_path/'seed').resolve())
    (tmp_path/'seed'/'unlisted.safetensors').write_bytes(b'unlisted')
    with pytest.raises(ValueError,match='inventory'): rd.verify_seed({'release_id':'fixture'},'cpt',s,tmp_path,'a'*64)


def test_seed_config_only_and_traversal_rejected(tmp_path):
    import release_data as rd
    s=seed_fixture(tmp_path);s['files']=s['files'][:1]
    with pytest.raises(ValueError): rd.verify_seed({'release_id':'fixture'},'cpt',s,tmp_path,'a'*64)


def test_seed_weight_index_all_shards_required(tmp_path):
    import release_data as rd
    s=seed_fixture(tmp_path);d=tmp_path/'seed'
    (d/'model.safetensors.index.json').write_text(json.dumps({'weight_map':{'x':'missing.safetensors'}}))
    s['files']=[pin(p) for p in d.iterdir()]
    with pytest.raises(ValueError,match='shard'):rd.verify_seed({'release_id':'fixture'},'cpt',s,tmp_path,'a'*64)


def test_selected_new_cpt_lineage_required(tmp_path):
    import release_data as rd
    s=seed_fixture(tmp_path);s['kind']='selected_new_cpt'
    with pytest.raises(ValueError): rd.verify_seed({'release_id':'fixture'},'sft',s,tmp_path,'a'*64)


def test_bounded_summary_exact_source_query_group():
    import release_data as rd
    r={'meta':{'kind':'bounded_movement_summary','mint':'mint','task':'bounded_movement_summary','source_associations':[{'file':'extract.sqlite','file_sha256':'a'*64,'mint':'mint','query':'successful, timed, exact-integer, internally consistent net wallet/mint/signature movements; nonzero','tables':['token_delta','tx_facts']}]}}
    assert rd.record_groups(r)
    r['meta']['source_associations'][0]['mint']='other'
    with pytest.raises(ValueError):rd.record_groups(r)


def test_authoritative_source_groups_and_parent_ids():
    import release_data as rd
    a=row();a['meta'].update(group='g1',source_associations=[{'source':{'file_sha256':'a'*64,'table':'examples','row':1}}],trajectory_id='shared')
    b=copy.deepcopy(a);b['meta'].update(group='g2',mint='different',candidate_id='other')
    b['meta']['source_associations'][0]['source']['row']=2
    assert ('trajectory','shared') in rd.record_groups(a)&rd.record_groups(b)
    del a['meta']['group']
    with pytest.raises(ValueError,match='group'):rd.record_groups(a)


def admission_fixture(tmp_path):
    import release_data as rd
    subject=tmp_path/'subject';subject.write_text('fixture')
    m={'status':'APPROVED','training_authorized':True,'scope':'north_star_trader','release_id':'fixture',
       'runtime':{'torch':'fixture'},'phases':{'sft':{'train':[dict(pin(subject),rows=2)],'validation':[dict(pin(subject),rows=1)]}},
       'exclusion_union':pin(subject),'tokenizer':{'path':str(tmp_path),'files':{'subject':rd.sha256(subject)}},
       'verification_subjects':{role:pin(subject) for roles in rd.GATE_SUBJECT_ROLES.values() for role in roles},
       'admission':{'approved_by':'fixture','clean_seed':True,'gates':dict.fromkeys(rd.GATES,True)}}
    results={}
    for gate in rd.GATE_SUBJECT_ROLES:
        receipt={'schema':'qwen27b_gate_result_v1','verified':True,'gate':gate,'release_id':'fixture',
                 'release_payload_sha256':rd.release_payload_sha256(m),'runtime':m['runtime'],
                 'evidence_scope':'complete_required_population','population':rd.release_population(m),
                 'result':{'schema':gate+'_result_v1','status':'PASS','failures':0},
                 'subject_pins':rd.required_gate_subjects(m,tmp_path,gate)}
        p=tmp_path/(gate+'.json');p.write_text(json.dumps(receipt))
        results[gate]={'passed':True,'reason':'verified_evidence','receipt':str(p),'receipt_sha256':rd.sha256(p)}
    evidence=tmp_path/'acceptance.json'
    e={'schema':'north_star_release_acceptance_v2','training_approved':True,'operator_go':True,'release_id':'fixture',
       'release_payload_sha256':rd.release_payload_sha256(m),'failed_gates':[],'gate_results':results}
    evidence.write_text(json.dumps(e));m['admission']['evidence']=pin(evidence)
    return m,e,evidence


def test_admission_exact_gate_payload_subjects_and_external_authority(tmp_path):
    import release_data as rd
    m,e,p=admission_fixture(tmp_path)
    rd.verify_admission(m,tmp_path,acceptance_sha256=rd.sha256(p))
    with pytest.raises(ValueError,match='independent'):rd.verify_admission(m,tmp_path,acceptance_sha256='0'*64)
    for mutation in ['payload','subjects','runtime','result']:
        gate='token_masks';rp=Path(e['gate_results'][gate]['receipt']); original=json.loads(rp.read_text());r=copy.deepcopy(original)
        if mutation=='payload':r['release_payload_sha256']='0'*64
        elif mutation=='subjects':r['subject_pins']=[dict(pin(p),role='irrelevant')]
        elif mutation=='runtime':r['runtime']={}
        else:r['result']={'status':'PASS'}
        rp.write_text(json.dumps(r));e['gate_results'][gate]['receipt_sha256']=rd.sha256(rp)
        p.write_text(json.dumps(e));m['admission']['evidence']=pin(p)
        with pytest.raises(ValueError,match='gate|Gate'):rd.verify_admission(m,tmp_path,acceptance_sha256=rd.sha256(p))
        rp.write_text(json.dumps(original))

