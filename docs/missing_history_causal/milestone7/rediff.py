import json,glob,subprocess,os
d=json.load(open('/tmp/mh_recon2/slice/sample.json'))
n=0
for s,lst in d.items():
    for it in lst:
        m=it['mint'] if isinstance(it,dict) else it[0]
        pre=m[:8]
        f=f'/tmp/mh_recon2/slice/samp_{s}_{pre}'
        if not os.path.exists(f+'.rust.jsonl'): continue
        r=subprocess.run(['python3','/tmp/mh_recon2/q/fielddiff.py',f+'.rust.jsonl',m,s,f+'.diff.json'],capture_output=True,text=True)
        n+=1
print('rediffed',n)
