import json,base64,glob
f='/training/mh_build/wire/s1/wire_0003.ndjson'
n=0
for l in open(f):
    if 'pAMMBay6' not in l: continue
    d=json.loads(l)
    for ix in d.get('instructions',[]):
        if ix['program_b58']!='pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA': continue
        b=base64.b64decode(ix['data_b64'])
        if b[:8]==bytes([0xe4,0x45,0xa5,0x2e,0x51,0xcb,0x9a,0x1d]):
            print(len(b), b[8:16].hex()); n+=1
            if n>=8: raise SystemExit
