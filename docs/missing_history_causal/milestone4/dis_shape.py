import sys,collections,json
n=int(sys.argv[1]); rows=[l.rstrip('\n').split('\t') for l in open(f'/tmp/mh_recon2/m1/s{n}.pop.tsv')]
bysig=collections.defaultdict(list)
for r in rows: bysig[r[0]].append(r)
dis=[r for r in rows if r[2]=='SOL' and r[3]=='corpus_row_disagrees_with_event']
# structure of the transactions holding a disagreement: events per tx, outcomes in the same tx
c=collections.Counter()
for r in dis:
    sib=bysig[r[0]]
    c[(len(sib), tuple(sorted(x[3] for x in sib)))]+=1
for k,v in c.most_common(8): print(v,k)
# same-tx, same side? sides of siblings
c2=collections.Counter()
for sig,sib in bysig.items():
    if any(x[3]=='corpus_row_disagrees_with_event' for x in sib):
        c2[('n_events',len(sib),'sides',''.join(sorted(x[4][0] for x in sib)))]+=1
print(c2.most_common(8))
print([r for r in dis[:3]])
