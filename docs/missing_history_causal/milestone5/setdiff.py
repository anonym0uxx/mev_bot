import json,sys,collections
M='51nHMcvh4z3e7YYPiSieYc6KrFE6qsv8zatf6mdqpump'; T=int(sys.argv[1])
fed=[l.rstrip('\n').split('\t') for l in open('/tmp/mh_recon2/slice/fed.tsv')]
fed=[(int(a),int(b),int(c),int(d),int(e)) for a,b,c,d,e,_ in fed if int(a)<=T]
inside=collections.Counter((r,s,t) for r,f,s,t,q in fed if f==1)
outside=[(r,q,t) for r,f,s,t,q in fed if f==0]
tape=[json.loads(l) for l in open('/tmp/mh_recon2/ref/20260909_144906_000490/pf.jsonl') if M in l]
tape=[r for r in tape if r['mint']==M and r['recv_unix_ms']<=T]
tc=collections.Counter((r['recv_unix_ms'],r['sol_lamports'],r['tokens_raw']) for r in tape)
print('rust inside',sum(inside.values()),'tape',sum(tc.values()))
print('tape-not-rust',list((tc-inside).items())[:8],sum((tc-inside).values()))
print('rust-not-tape',list((inside-tc).items())[:8],sum((inside-tc).values()))
print('outside_corpus rows (recv,swap_sol,signed_base)',outside[:6],len(outside))
