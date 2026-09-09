"""Pure advisory predicate over reviewed attestations, never deletion authority.

No I/O, clock, age inference, lease expiry inference, or capture control. Receipt
validation is the caller's responsibility; filename presence is not evidence.
"""
import re as _re


def _sha(value):
    return isinstance(value, str) and _re.fullmatch('[0-9a-f]{64}', value) is not None


def _receipt(value):
    return isinstance(value, str) and bool(value.strip())


def retention_eligibility(evidence):
    """Return eligibility and stable blockers; missing/unknown values block.

    ``lease_state=cleared`` attests an authoritative review of all writer,
    compaction, audit and holdout leases. ``full_account_dependencies`` covers
    complete accounts, balances, instructions and fees, not scalar projections.
    No result permits an automatic purge, even if ``eligible`` is true.
    """
    if not isinstance(evidence, dict):
        return {'eligible': False, 'blockers': ['unknown_evidence']}
    blockers = []
    for key, expected in (
        ('session_state', 'closed'), ('integrity', 'verified'),
        ('compaction', 'succeeded'), ('lease_state', 'cleared'),
        ('dependency_inventory', 'reviewed_complete'),
        ('full_account_dependencies', 'durable_verified'),
        ('retention_policy', 'review_approved'),
    ):
        if evidence.get(key) != expected:
            blockers.append(key)
    for key, expected in (('active', False), ('frozen', False), ('retention_elapsed', True)):
        if evidence.get(key) is not expected:
            blockers.append(key)
    if not _sha(evidence.get('raw_sha256')):
        blockers.append('raw_sha256')
    if not _receipt(evidence.get('review_receipt')):
        blockers.append('review_receipt')
    derivatives = evidence.get('derivatives')
    if not isinstance(derivatives, list) or not derivatives:
        blockers.append('derivatives')
    else:
        for index, derivative in enumerate(derivatives):
            valid = isinstance(derivative, dict)
            if valid:
                valid = (
                    derivative.get('integrity') == 'verified'
                    and derivative.get('durable') is True
                    and derivative.get('complete') is True
                    and _receipt(derivative.get('receipt'))
                    and derivative.get('input_sha256') == evidence.get('raw_sha256')
                    and all(_sha(derivative.get(key)) for key in
                            ('input_sha256', 'output_sha256', 'schema_sha256',
                             'code_sha256', 'config_sha256'))
                )
            if not valid:
                blockers.append(f'derivatives[{index}]')
    return {'eligible': not blockers, 'blockers': blockers}
