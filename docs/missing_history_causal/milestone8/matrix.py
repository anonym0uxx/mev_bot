import json,glob,collections
CAUSE={
 'price_sol_per_whole_token':'implementation (curve snapshot/last-trade timing differs from corpus state)',
 'price_sol_per_raw_token':'implementation (curve snapshot/last-trade timing differs from corpus state)',
 'price_lamports_per_raw_token':'implementation (curve snapshot/last-trade timing differs from corpus state)',
 'last_trade_age_s':'implementation (curve snapshot/last-trade timing differs from corpus state)',
 'v_tokens_reserves':'implementation (curve snapshot/last-trade timing differs from corpus state)',
 'v_sol_reserves_sol':'implementation (curve snapshot/last-trade timing differs from corpus state)',
 'real_tokens_reserves':'implementation (curve snapshot/last-trade timing differs from corpus state)',
 'real_sol_reserves_sol':'implementation (curve snapshot/last-trade timing differs from corpus state)',
 'mcap_sol_at_t':'implementation (curve snapshot/last-trade timing differs from corpus state)',
 'curve_progress':'implementation (curve snapshot/last-trade timing differs from corpus state)',
 'curve_price_sol_per_raw_token':'implementation (curve snapshot/last-trade timing differs from corpus state)',
 'curve_k':'implementation (curve snapshot/last-trade timing differs from corpus state)',
 'creator_past_launches':'unavailable input (harness registers only this mint launch; creator prior launches not fed)',
 'creator_known':'unavailable input (as creator_past_launches)',
 'reason':'formatting (label text)',
 'smart_entrants_300s':'unavailable input / population (needs PumpSwap rows in history; seed is pre-capture only)',
 'smart_net_flow_sol_300s':'unavailable input / population (as above)',
 'coentry_wallets_300s':'unavailable input / population (as above)',
 'flow_lookback_d':'unavailable input (history prefix)',
 'buy_count':'future-dependent corpus rule (whole-run median band) + population','sell_count':'future-dependent corpus rule (whole-run median band) + population',
 'n_prior_trades':'future-dependent corpus rule (whole-run median band) + population','unique_traders':'future-dependent corpus rule (whole-run median band) + population',
 'buy_volume_lamports':'future-dependent corpus rule (whole-run median band) + population','sell_volume_lamports':'future-dependent corpus rule (whole-run median band) + population',
 'net_flow_lamports':'future-dependent corpus rule (whole-run median band) + population','buyer_seller_ratio':'future-dependent corpus rule (whole-run median band) + population',
 'top1_trader_share':'future-dependent corpus rule (whole-run median band) + population','top5_trader_share':'future-dependent corpus rule (whole-run median band) + population',
 'vol_30s_bp':'future-dependent corpus rule (whole-run median band) + population','ret_5s_bp':'future-dependent corpus rule (whole-run median band) + population','ret_30s_bp':'future-dependent corpus rule (whole-run median band) + population',
 'staleness_ms':'implementation (curve snapshot timing vs corpus curve state)',
}
tot=collections.Counter(); per=collections.defaultdict(collections.Counter); n=0
for f in sorted(glob.glob('/tmp/mh_recon2/slice/samp_*.diff.json')):
    d=json.load(open(f)); tag=f.split('/')[-1].replace('.diff.json','').replace('.json','')
    for x in d:
        if 'diffs' not in x: continue
        n+=1
        for g in x['diffs']: tot[g['field']]+=1; per[g['field']][tag]+=1
print('clocks compared',n)
for k,v in sorted(tot.items(),key=lambda kv:-kv[1]): print('%4d  %-30s %s'%(v,k,CAUSE.get(k,'UNCLASSIFIED')))
