import json,sys
sys.path.insert(0,'/home/alon/build/mev_bot_consol/training/code/src/v2')
import renormalize_raw as R
d=json.loads(open('/tmp/mh_recon2/m1/one.ndjson').read())
keys=d['account_keys']; m=d['meta']
def conv(v): return [{'mint':e['mint'],'owner':e['owner'],'ui_token_amount':{'amount':e['amount']}} for e in v]
pre,post=conv(m['pre_token_balances']),conv(m['post_token_balances'])
mints=[]
for e in post+pre:
    x=e['mint']
    if x not in R.NOT_A_LAUNCH and x not in mints: mints.append(x)
print('mints',[x[:6] for x in mints])
for ix in d['instructions']:
    pass
import base64
for i,ix in enumerate(d['instructions']):
    if i!=13: continue
    print('ix13 prog',ix['program_b58'][:6],ix['data_b64'][:12],ix['accounts'][:8])
    for mt in mints:
        r=R.resolve_trader(ix['accounts'],keys,'sell',mt,m['pre_balances'],m['post_balances'],pre,post,prefer_idx=None)
        print(' inst-scope',mt[:6],r and (r[0][:6],r[1]))
    for mt in mints:
        r=R.wide_resolve(keys,'sell',mt,m['pre_balances'],m['post_balances'],pre,post)
        print(' wide',mt[:6],r and (r[0][:6],r[1]))
