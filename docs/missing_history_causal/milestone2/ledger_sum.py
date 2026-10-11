import json,collections,sys
for sess in sys.argv[1:]:
    rows=[json.loads(l) for l in open(f'/tmp/mh_recon2/ledger/{sess}.jsonl')]
    print('==',sess,len(rows))
    c=collections.Counter(r['cls'] for r in rows)
    print(dict(c))
    k=collections.Counter(r.get('amt') or r.get('why') or '-' for r in rows)
    for a,n in k.most_common(14): print('  ',n,a)
    print(list(rows[0].keys()))
