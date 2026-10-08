import json,sys
d=json.loads(open('/tmp/mh_recon2/m1/one.ndjson').read())
A='123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'
def dec(s):
    n=0
    for c in s: n=n*58+A.index(c)
    b=n.to_bytes((n.bit_length()+7)//8,'big'); return b'\0'*(len(s)-len(s.lstrip('1')))+b
for i,k in enumerate(d['account_keys']):
    try: L=len(dec(k))
    except Exception as e: L='ERR'
    if L!=32: print(i,k,L)
print('n',len(d['account_keys']))
