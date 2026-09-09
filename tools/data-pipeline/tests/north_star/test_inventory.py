import importlib.util
from pathlib import Path
import hashlib
import pytest


def load_inventory():
    spec=importlib.util.spec_from_file_location('ns_inventory_test', MODULE)
    mod=importlib.util.module_from_spec(spec);spec.loader.exec_module(mod)
    return mod


@pytest.mark.parametrize('name', ['../escape','/absolute','C:/escape','folder\\\\escape'])
def test_rejects_unsafe_paths(tmp_path, name):
    with pytest.raises(ValueError):
        load_inventory().verify_manifest(tmp_path,[{'path':name,'size':0,'sha256':'0'*64}])


def test_duplicate_paths_and_mismatch(tmp_path):
    row={'path':'file','size':1,'sha256':'0'*64}
    (tmp_path/'file').write_bytes(b'x')
    with pytest.raises(ValueError):
        load_inventory().verify_manifest(tmp_path,[row,row])
    result=load_inventory().verify_manifest(tmp_path,[row])
    assert result['verified_count']==0 and len(result['mismatched'])==1

MODULE = Path(__file__).parents[2] / 'src/north_star/inventory.py'

def test_inventory_verifies_and_does_not_decode(tmp_path):
    assert MODULE.exists(), 'inventory implementation missing'
    spec=importlib.util.spec_from_file_location('ns_inventory_test', MODULE)
    mod=importlib.util.module_from_spec(spec);spec.loader.exec_module(mod)
    (tmp_path/'one.bin').write_bytes(b'fixture')
    rows=[{'path':'one.bin','size':7,'sha256':hashlib.sha256(b'fixture').hexdigest()}, {'path':'absent','size':1,'sha256':'0'*64}]
    result=mod.verify_manifest(tmp_path, rows)
    assert result['verified_count']==1
    assert result['missing']==['absent']
    assert result['mismatched']==[]
    assert result['complete'] is False
