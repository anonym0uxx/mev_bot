import json,base64,struct,collections
f='/training/mh_build/wire/s1/wire_0003.ndjson'
c=collections.Counter(); shown=0
for l in open(f):
    if 'pAMMBay6' not in l: continue
    d=json.loads(l)
    for ix in d.get('instructions',[]):
        if ix['program_b58']!='pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA': continue
        b=base64.b64decode(ix['data_b64'])
        if b[:8]!=bytes.fromhex('e445a52e51cb9a1d') or b[8:16]!=bytes([103,244,82,31,44,245,119,119]): continue
        p=b[16:]
        u=lambda o:struct.unpack_from('<Q',p,o)[0]
        if len(p)<352: continue
        qin,lpb,lpf,prb,prf,withlp,uq=u(56),u(64),u(72),u(80),u(88),u(96),u(104)
        crf=u(344)
        c['uq==qin+lp+pr+cr']+= uq==qin+lpf+prf+crf
        c['withlp==qin+lp']+= withlp==qin+lpf
        c['uq==withlp+pr+cr']+= uq==withlp+prf+crf
        c['n']+=1
        if shown<2: shown+=1; print(dict(qin=qin,lpf=lpf,prf=prf,crf=crf,withlp=withlp,uq=uq,out=u(8),maxq=u(16)))
print(dict(c))
