"""Synthetic ancestry DAGs; no source records or model examples."""
from pathlib import Path
import importlib.util
import pytest

P=Path(__file__).parents[2]/'src/north_star/dependencies.py'

def module():
    assert P.exists(), 'dependency resolver missing'
    spec=importlib.util.spec_from_file_location('ns_deps',P)
    m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m);return m

def test_complete_transitive_closure_includes_alternatives():
    graph={'decision':['chosen','alternative','inventory'],'chosen':['raw1'],'alternative':['raw2'],'inventory':['raw3'],'raw1':[],'raw2':[],'raw3':[]}
    assert module().parent_closure('decision',graph)==('alternative','chosen','inventory','raw1','raw2','raw3')

@pytest.mark.parametrize('graph',[{'a':['missing']},{'a':['b'],'b':['a']}])
def test_missing_and_cyclic_fail_closed(graph):
    with pytest.raises(ValueError):module().parent_closure('a',graph)

def test_deep_graph_does_not_use_recursive_stack():
    graph={str(i):[str(i+1)] for i in range(2000)};graph['2000']=[]
    assert len(module().parent_closure('0',graph))==2000

def test_diamond_parent_is_not_a_cycle():
    assert module().parent_closure('r',{'r':['a','b'],'a':['c'],'b':['c'],'c':[]})==('a','b','c')

@pytest.mark.parametrize('parents',['abc',[1],['a','a'],[None]])
def test_malformed_parent_lists_are_rejected(parents):
    with pytest.raises(ValueError):module().parent_closure('r',{'r':parents})

def test_closure_carries_protected_alternative_into_split_gate():
    import sys
    sys.path.insert(0,str(P.parent.parent))
    from north_star.splits import EvaluationBoundary,canonical_sha256
    import json
    policy=json.loads((P.parents[2]/'north_star/EVAL_FREEZE.json').read_text())
    pin=policy.pop('policy_sha256')
    boundary=EvaluationBoundary(policy,expected_sha256=pin)
    graph={'context':['chosen','rival'],'chosen':['raw_dev'],'rival':['raw_holdout'],'raw_dev':[],'raw_holdout':[]}
    ancestry=module().parent_closure('context',graph)
    partitions={n:('SEALED_ECONOMICS' if n=='raw_holdout' else 'DEVELOPMENT') for n in ancestry}
    assert boundary.inherit(partitions)=='QUARANTINED'

def test_budget_checked_before_parent_iteration():
    class NoScan(list):
        def __iter__(self):raise AssertionError('scanned before budget')
    with pytest.raises(ValueError,match='budget'):
        module().parent_closure('a',{'a':NoScan(['x','y'])},max_edges=1)

def test_node_budget_is_enforced():
    with pytest.raises(ValueError,match='budget'):module().parent_closure('a',{'a':['b'],'b':[]},max_nodes=1)
