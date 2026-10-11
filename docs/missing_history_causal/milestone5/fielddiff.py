"""Field diff: Rust-rendered prompt vs frozen corpus prompt, same mint + t_dec. usage: fielddiff.py rust.jsonl MINT SESSKEY out.json"""
import json,re,sys,glob
rust=[json.loads(l) for l in open(sys.argv[1])]; M=sys.argv[2]; out=sys.argv[4]
corp={}
for fn in ('train','validation','examination'):
    for l in open(f'/training/v2/candidate_sft_c12_entry/{fn}.jsonl'):
        if M not in l[:400]: continue
        r=json.loads(l)
        if r['mint']!=M: continue
        u=[m for m in r['messages'] if m['role']=='user'][0]['content']
        mm=re.search(r't_dec_ms=(\d+)',u)
        if mm: corp[int(mm.group(1))]=(fn,u)
TOK=re.compile(r'([A-Za-z_0-9]+)=([^\s]+)')
def fields(u):
    d={}
    for ln in u.split('\n'):
        for k,v in TOK.findall(ln): d.setdefault(k,v)
    return d
def num(x):
    try: return float(x)
    except: return None
res=[]
for r in rust:
    t=r['t']
    if t not in corp: res.append({'t':t,'status':'no_corpus_row'}); continue
    if 'prompt' not in r: res.append({'t':t,'status':'rust_refusal','why':r.get('refusal')}); continue
    fn,cu=corp[t]; a=fields(r['prompt']); b=fields(cu)
    diffs=[]
    for k in sorted(set(a)|set(b)):
        va,vb=a.get(k),b.get(k)
        if va==vb: continue
        na,nb=num(va),num(vb)
        rel=None
        if na is not None and nb is not None and nb!=0: rel=(na-nb)/abs(nb)
        diffs.append({'field':k,'rust':va,'corpus':vb,'rel':rel})
    same_lines=sum(1 for x,y in zip(r['prompt'].split('\n'),cu.split('\n')) if x==y)
    res.append({'t':t,'split':fn,'n_fields':len(set(a)|set(b)),'n_diff':len(diffs),'identical_lines':same_lines,
                'total_lines':len(cu.split('\n')),'diffs':diffs})
json.dump(res,open(out,'w'),indent=1)
for x in res:
    print(x['t'],x.get('status') or (x['n_diff'],'/',x['n_fields'],'lines',x['identical_lines'],'/',x['total_lines']))
