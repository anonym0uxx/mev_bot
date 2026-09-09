"""Synthetic attestations only; no production captures or deletion authority."""
import copy
import importlib.util
from pathlib import Path
import pytest

MODULE = Path(__file__).parents[2] / 'src/north_star/retention.py'


def load_retention():
    assert MODULE.exists(), 'retention implementation missing'
    spec = importlib.util.spec_from_file_location('capture_retention', MODULE)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def approved():
    return dict(session_state='closed', active=False, frozen=False,
                integrity='verified', raw_sha256='a' * 64,
                compaction='succeeded', lease_state='cleared',
                dependency_inventory='reviewed_complete',
                full_account_dependencies='durable_verified',
                retention_policy='review_approved', retention_elapsed=True,
                review_receipt='review-1',
                derivatives=[dict(integrity='verified', durable=True,
                                  complete=True, receipt='derivative-1',
                                  input_sha256='a' * 64, output_sha256='b' * 64,
                                  schema_sha256='c' * 64, code_sha256='d' * 64,
                                  config_sha256='e' * 64)])


def test_unknown_evidence_is_ineligible():
    assert load_retention().retention_eligibility({})['eligible'] is False


def test_complete_reviewed_attestations_are_eligible_without_mutation():
    evidence = approved()
    before = copy.deepcopy(evidence)
    assert load_retention().retention_eligibility(evidence) == {'eligible': True, 'blockers': []}
    assert evidence == before


@pytest.mark.parametrize('field', list(approved()))
def test_every_required_field_missing_blocks(field):
    evidence = approved()
    del evidence[field]
    assert not load_retention().retention_eligibility(evidence)['eligible']


@pytest.mark.parametrize('field,value', [
    ('session_state', 'active'), ('session_state', 'unknown'),
    ('active', True), ('active', 0), ('frozen', True), ('frozen', 'false'),
    ('integrity', 'corrupt'), ('compaction', 'failed'), ('compaction', 'unknown'),
    ('lease_state', 'audit'), ('lease_state', 'holdout'), ('lease_state', 'unknown'),
    ('dependency_inventory', 'unknown'), ('full_account_dependencies', 'scalar_only'),
    ('retention_policy', 'age_only'), ('retention_elapsed', False),
    ('retention_elapsed', 1), ('review_receipt', ''), ('raw_sha256', 'A' * 64),
    ('derivatives', []), ('derivatives', [None])])
def test_unsafe_or_malformed_evidence_blocks(field, value):
    evidence = approved()
    evidence[field] = value
    assert not load_retention().retention_eligibility(evidence)['eligible']


@pytest.mark.parametrize('field', list(approved()['derivatives'][0]))
def test_derivative_receipt_requires_every_attestation(field):
    evidence = approved()
    del evidence['derivatives'][0][field]
    assert not load_retention().retention_eligibility(evidence)['eligible']


@pytest.mark.parametrize('field,value', [('input_sha256', 'f' * 64),
    ('durable', 1), ('complete', 'yes'), ('integrity', 'unknown'), ('receipt', '')])
def test_partial_or_stale_receipts_block(field, value):
    evidence = approved()
    evidence['derivatives'][0][field] = value
    assert not load_retention().retention_eligibility(evidence)['eligible']


@pytest.mark.parametrize('value', [None, [], 'approved', True])
def test_non_mapping_fails_closed(value):
    assert not load_retention().retention_eligibility(value)['eligible']


def test_no_deletion_or_filesystem_api_exposed():
    module = load_retention()
    assert [k for k, v in vars(module).items() if callable(v) and not k.startswith('_')] == ['retention_eligibility']
