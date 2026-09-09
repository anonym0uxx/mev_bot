"""Action-contract fixtures, not recommended trades or training labels."""
from pathlib import Path
import importlib.util
import pytest
P=Path(__file__).parents[2]/'src/north_star/actions.py'
def module():
    assert P.exists(), 'action contract missing'
    s=importlib.util.spec_from_file_location('actions_test',P);m=importlib.util.module_from_spec(s);s.loader.exec_module(m);return m

def row(action='BUY',origin='observed_action'):
    return {'action':action,'origin':origin,'chain':'solana','venue':'pumpfun','position_raw':0,'evidence_id':'fixture-only','recommended':False}

def test_observed_bad_buy_is_not_erased_by_economic_label():
    r=row();r['economic_quality']='NEGATIVE'
    assert module().validate_action(r)['imitation_eligible'] is False

def test_deterministic_recommendation_cannot_claim_not_recommended():
    r=row(origin='deterministic_recommendation')
    with pytest.raises(ValueError):module().validate_action(r)


def test_recommended_buy_requires_economic_support():
    r=row(origin='deterministic_recommendation');r['recommended']=True
    with pytest.raises(ValueError):module().validate_action(r)

@pytest.mark.parametrize('action',['HOLD','REDUCE','EXIT_ALL'])
def test_management_requires_inventory(action):
    with pytest.raises(ValueError):module().validate_action(row(action))

def test_skip_is_not_sell_and_watch_is_not_hold():
    assert module().validate_action(row('SKIP'))['action']=='SKIP'
    assert module().validate_action(row('WATCH'))['action']=='WATCH'

@pytest.mark.parametrize('quantity',[True,1.0])
def test_full_exit_rejects_bool_or_float(quantity):
    r=row('EXIT_ALL');r.update(position_raw=1,sell_raw=quantity)
    with pytest.raises(ValueError):module().validate_action(r)

def test_explicit_partial_and_full_exit():
    r=row('REDUCE');r.update(position_raw=10,sell_raw=3)
    assert module().validate_action(r)['action']=='REDUCE'
    r.update(action='EXIT_ALL',sell_raw=10)
    assert module().validate_action(r)['action']=='EXIT_ALL'

@pytest.mark.parametrize('quantity',[True,0,10,11,1.5])
def test_partial_quantities_fail_closed(quantity):
    r=row('REDUCE');r.update(position_raw=10,sell_raw=quantity)
    with pytest.raises(ValueError):module().validate_action(r)

def test_coarse_sell_is_not_silently_mapped():
    with pytest.raises(ValueError):module().validate_action(row('SELL'))

def test_recommendation_support_is_not_admission():
    r=row(origin='deterministic_recommendation');r['recommended']=True
    r['recommendation_support']={k:'fixture' for k in ['calibration_id','decision_context_id','economic_evidence_id','policy_id']}
    assert module().validate_action(r)['imitation_eligible'] is False

@pytest.mark.parametrize('venue',['unapproved','raydium'])
def test_outside_scope_rejected_for_action(venue):
    r=row();r['venue']=venue
    with pytest.raises(ValueError):module().validate_action(r)
