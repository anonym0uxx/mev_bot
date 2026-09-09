"""Diagnostic action semantics; no order submission or dataset admission.

Observed actions are evidence, not automatically imitation targets. Returned
imitation eligibility always remains False until independent episode admission.
"""
ACTIONS = frozenset({'BUY', 'ADD', 'HOLD', 'REDUCE', 'EXIT_ALL', 'SKIP', 'WATCH'})
ORIGINS = frozenset({'observed_action', 'human_statement', 'deterministic_recommendation'})


def validate_action(row):
    if not isinstance(row, dict):
        raise ValueError('action must be a mapping')
    action, origin = row.get('action'), row.get('origin')
    if not isinstance(action, str) or action not in ACTIONS:
        raise ValueError('unknown action; coarse SELL requires explicit reduction semantics')
    if not isinstance(origin, str) or origin not in ORIGINS:
        raise ValueError('unknown action origin')
    if row.get('chain') != 'solana' or row.get('venue') not in ('pumpfun', 'pumpswap'):
        raise ValueError('outside execution scope; retain separately as contextual observation')
    evidence = row.get('evidence_id')
    if not isinstance(evidence, str) or not evidence.strip():
        raise ValueError('source evidence required')
    position = row.get('position_raw')
    if type(position) is not int or not 0 <= position <= 2**64-1:
        raise ValueError('exact known inventory required')
    if action in ('ADD', 'HOLD', 'REDUCE', 'EXIT_ALL') and position == 0:
        raise ValueError('position management requires inventory')
    if action == 'BUY' and position != 0:
        raise ValueError('existing position requires ADD semantics')
    if action == 'REDUCE':
        quantity = row.get('sell_raw')
        if type(quantity) is not int or not 0 < quantity < position:
            raise ValueError('REDUCE requires strict partial inventory quantity')
    if action == 'EXIT_ALL' and (type(row.get('sell_raw')) is not int or row['sell_raw'] != position):
        raise ValueError('EXIT_ALL quantity must equal inventory')
    if type(row.get('recommended')) is not bool:
        raise ValueError('explicit recommendation classification required')
    if origin == 'deterministic_recommendation' and not row['recommended']:
        raise ValueError('recommendation origin contradicts recommendation flag')
    if row['recommended']:
        support = row.get('recommendation_support')
        if origin != 'deterministic_recommendation' or not isinstance(support, dict):
            raise ValueError('recommendation support required separately from observed action')
        if not all(isinstance(support.get(k), str) and support[k].strip() for k in
                   ('calibration_id', 'decision_context_id', 'economic_evidence_id', 'policy_id')):
            raise ValueError('incomplete recommendation support')
    return {'action': action, 'origin': origin, 'imitation_eligible': False,
            'status': 'STRUCTURAL_ONLY_NOT_ADMITTED'}
