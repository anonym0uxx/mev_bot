"""Synthetic declared dependencies and timing; not production admission."""
from pathlib import Path
import sys
sys.path.insert(0,str(Path(__file__).parents[2]/'src'))
from north_star.dependencies import parent_closure
from north_star.availability import compute_available_at,is_available_at


def test_late_rival_dependency_blocks_decision_input():
    graph={'context':['price','rival'],'price':['feed'],'rival':['social'],'feed':[],'social':[]}
    parents=parent_closure('context',graph)
    deps={k:{'available_at':100,'clock':'UTC_KNOWN','clock_domain':None} for k in parents}
    deps['social']['available_at']=201
    available=compute_available_at(parents,deps,compute_delay_ms=3)
    assert available['available_at']==204
    assert not is_available_at(available,200)


def test_unknown_parent_clock_never_becomes_zero_delay():
    deps={'feed':{'available_at':100,'clock':'UTC_KNOWN'},'social':{'available_at':None,'clock':'UNKNOWN'}}
    available=compute_available_at(['feed','social'],deps,compute_delay_ms=0)
    assert available['available_at'] is None
    assert not is_available_at(available,1000)
