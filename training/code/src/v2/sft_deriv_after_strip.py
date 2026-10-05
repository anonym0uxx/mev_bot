#!/usr/bin/env python
"""Re-run derivability on the SFT target corpus with the leak fields removed.

Leak fields identified in observed_sft_train.jsonl:
  (a) prompt line  'SOURCE ACTION LABEL: <label>'   (literal answer in the prompt)
  (b) meta.source_action                             (same value, structured)
  (c) the observed token movement itself (sign of anchor_movement.token_delta_raw)
      -- the label is DEFINED as a restatement of this movement.

We measure lexical leakage and conditional derivability on:
  A = original records
  B = prompt with the 'SOURCE ACTION LABEL:' line stripped
  C = B, additionally with the SOURCE EVIDENCE json stripped (so no movement value)
and separately quantify how much of the label the token-delta sign alone recovers.
"""
import json, re, sys
from collections import Counter, defaultdict

P="/mnt/data/mev_bot-artifacts/north_star/aggregation/north_star_training_dataset_v1/corpus_builder_v1/integrated_data_v1/full_history_closure_v1/economics_v1/astra_review_v1/revision_candidates/support_export/observed_sft_train.jsonl"
DECISION_RE=re.compile(r'\b(BUY|SELL|EXIT|ADD|REDUCE|HOLD|WATCH|SKIP|NO_?ACTION)(?:_RECORDED|_ACTION|_TAKEN)?\b',re.I)
LABEL_LINE=re.compile(r'^SOURCE ACTION LABEL:.*$',re.I|re.M)
SOURCE_EV=re.compile(r'SOURCE EVIDENCE:.*?(?=\n[A-Z][A-Z ]+:|\Z)',re.S)

def load(p):
    out=[]
    with open(p) as f:
        for line in f:
            line=line.strip()
            if line:
                try: out.append(json.loads(line))
                except Exception: pass
    return out
def split(rec):
    msgs=rec.get('messages') or []
    prompt='\n'.join(m.get('content','') for m in msgs if m.get('role') in ('system','user'))
    target='\n'.join(m.get('content','') for m in msgs if m.get('role')=='assistant')
    return prompt,target
def dec(t):
    m=DECISION_RE.search(t or ''); return m.group(1).upper() if m else None

recs=load(P)
print(f"[LOAD] records={len(recs):,}")
assert recs, "no records"

# movement sign
def delta(rec):
    try: return rec['evidence']['anchor_movement']['token_delta_raw']
    except Exception: return None

# ---- mapping: sign -> label (marginals conditioned) ----
sign_bucket=defaultdict(Counter)
for r in recs:
    d=dec(split(r)[1]); s=delta(r)
    if d is None or s is None: continue
    k='increase' if s>0 else ('decrease' if s<0 else 'zero')
    sign_bucket[k][d]+=1
print("\n[0] label vs observed token-movement SIGN")
for k,c in sign_bucket.items():
    n=sum(c.values()); top,tn=c.most_common(1)[0]
    print(f"    {k:9s} n={n:6,d}  {dict(c)}  top={top} {tn/n*100:.1f}%")

def run(name, xform):
    labels=Counter(); lex=0; tot=0
    buckets=defaultdict(Counter)
    pos_buckets=defaultdict(Counter)
    for r in recs:
        p,t=split(r); p=xform(p,r); d=dec(t)
        if not d:
            continue
        tot+=1; labels[d]+=1
        if re.search(rf'\b{re.escape(d)}\b',p or '',re.I): lex+=1
        m=re.search(r'SOURCE ACTION LABEL[:\s]+([A-Za-z_]+)',p or '',re.I)
        if m: buckets[('stated_source_label',m.group(1).lower())][d]+=1
        mv=re.search(r'"token_delta_raw":(-?\d+)',p or '')
        if mv:
            s=int(mv.group(1)); k='increase' if s>0 else 'decrease'
            pos_buckets[('movement_sign_in_prompt',k)][d]+=1
    lex_rate=lex/tot if tot else 0
    print(f"\n[{name}] labeled={tot:,}  lexical={lex_rate*100:.2f}%  marginals={dict(labels)}")
    for bk in (buckets,pos_buckets):
        for (feat,k),c in sorted(bk.items(),key=lambda kv:-sum(kv[1].values())):
            n=sum(c.values())
            if n<50: continue
            top,tn=c.most_common(1)[0]
            flag='  <-- DERIVABLE' if tn/n>=0.90 else ''
            print(f"    {feat}={k:12s} n={n:6,d}  top={top:7s} {tn/n*100:6.2f}%{flag}")
    return lex_rate

run('A original', lambda p,r: p)
run('B strip SOURCE ACTION LABEL line', lambda p,r: LABEL_LINE.sub('',p))
ACTLABEL=re.compile(r'"(action_label|exit_quality|action_basis|admission_reason)"\s*:\s*("[^"]*"|null|[0-9.]+)',re.I)
run('C strip label line + SOURCE EVIDENCE block', lambda p,r: SOURCE_EV.sub('SOURCE EVIDENCE: [REDACTED]\n',LABEL_LINE.sub('',p)))
run('D strip label line + scrub label keys inside evidence', lambda p,r: ACTLABEL.sub(r'"\1":"[SCRUBBED]"',LABEL_LINE.sub('',p)))
